# 附录 P:Level-1 红队复审(读码证伪 方案A 两处致命 + punts)

## P0 调研底稿(Vortex内幕 + openGauss自管存储AM可行模式)

# 调研:SmithDB Vortex+自研倒排+LSM 怎么根治膨胀与搜索

I now have everything: the research base, primary sources with exact numbers, the Vortex internals, and the actual openGauss code grounding the "custom-storage-backed index AM" pattern (vector_smgr, bulkbuf_smgr, BM25 DiskVector/FreeSpace/jieba/WAL). Here is the report.

---

# Level 1 参照调研：Vortex 列式不可变段 + 自研倒排 + LSM merge-on-read 内幕机制

> 调研日期 2026-06-17。SmithDB(2026-05 发布)与 Vortex 关键事实均经一手来源联网核实(LangChain 官方两篇技术博客 + Spiral/Vortex 文档 + 研究底稿对抗核查版),并落地到本仓 openGauss 真实代码(`vector_smgr` / `bulkbuf_smgr` / `bm25` 的 `DiskVector`/`FreeSpace`/jieba/WAL)。诚实标注:凡标【推测】的为我据机制推演,非官方原文;凡引数字处均为官方原文值。

---

## 0. 一句话结论:Level 1 要"反着抄"什么

SmithDB 用**一套列式不可变段格式(Vortex)同时承载 [数据列 + zone-map + 内嵌倒排],配 metastore 挂 deletion/upgrade 向量做 merge-on-read,配时间分层 compaction 回收**,从根上同时解掉了你列的三处:**膨胀**(不可变段 + 向量代替原地 UPDATE)、**压缩 XOR 可检索的二选一**(倒排内嵌进压缩列段,压缩与随机访问/检索共存)、**query-time fold 开销**(merge-on-read 把"折叠多版本 span 事件"从应用层下推到读路径并用 zone-map 大幅剪枝)。

它的复杂度几乎全花在"对象存储 + 无状态分布式"上;**Level 1 在单机本地 NVMe 上要保留它的数据范式,砍掉分布式包袱**。而本仓 openGauss 已经用 **DiskANN 的 `vector_smgr`(新 fork + 独立 buffer 池 `vector_buffers`)** 和 **BM25 的 `DiskVector`/`FreeSpace` 磁盘容器框架 + jieba 分词 + WAL** 证明了"**自定义存储 backed 的 Index-AM**"这条路在封闭 Table-AM 下走得通——这正是 Level 1 段引擎的落地载体。

---

## ① 列式不可变段:如何同时给到压缩 + 随机访问(Parquet 给不了)

**Parquet 为什么二选一**(官方/底稿核实):随机访问要先读 footer + 读 row-group/column 元数据,**多次对象存储往返**才能定位;且 Parquet 的解码以"整页(page)"为单位——要取某一行某列的一个值,必须把整个压缩页解开。压缩越狠,随机取单值越贵。这正是你 Level 0 的痛点②:**CStore 压缩了就不能随机检索,要可检索只能留不压缩的行存**。

**Vortex 怎么破(均一手核实):**

**(a) 逻辑/物理分离 + 可组合的 Layout 树。** Vortex 先有"逻辑数据类型 DType"(区别于 Arrow,把内存布局和逻辑类型解耦),物理侧用四种可嵌套 Layout 节点组成一棵树:
- `FlatLayout` —— 持有单个序列化的 Vortex array(叶子);
- `StructLayout` —— 按列拆分(**列裁剪**,只读被 project 的列);
- `ChunkedLayout` —— 按行分块(官方示例:顶层 ChunkedLayout 每块 10 万行 → StructLayout 拆列 → 叶子 ChunkedLayout 每块 64KB 压缩页);
- `ZonedLayout` —— **存一份 zone-map 统计做过滤剪枝**。

关键:**zone-map 工作在"逻辑 8k 行 chunk"上,而非对齐物理页大小**,因此能做"页内/块内剪枝"(intra-chunk pruning),不受物理页对齐拖累。

**(b) "可在压缩域上算/取"的级联编码——这是随机访问与压缩共存的根因。** Vortex 内置 **BtrBlocks / FastLanes(整数 bitpacking)/ FSST(字符串)/ ALP(浮点)** 等编码,核心性质是**算子下推进压缩域、解压被尽量推迟(defer decompression)**:
- 字典数组上的所有标量函数被**转发到唯一字典值**上计算,再用未触碰的(可能仍压缩的)codes 重建——绝大多数场景**根本不必解压**;
- ALP 把浮点比较**下推进整数域**,实测比较快 **80%**;
- FastLanes bitpacking 允许直接在压缩整数域比较。

**(c) 性能与往返(官方原文值):相比 Parquet——随机访问快 ~100x、扫描快 10–20x、写快 5x、压缩比相当;对象存储往返 1 次(最坏 2 次)vs Parquet 多次。** 100x 随机访问 = "从一列里只取单个 trace 的几个值,而不解压整列"。

**→ 它怎么解决了你的痛点:**
- **解膨胀(痛点①)**:段**不可变**——折叠/冻结不再是原地 UPDATE,不产死元组、无需 vacuum。冷数据从源头不再有 ASTORE 膨胀压力。
- **解"压缩 XOR 可检索"(痛点②)**:**一个压缩列段就能随机访问 + 算子下推**,不必为了"老 trace 可搜"额外留一份不压缩的行存。压缩与可检索在同一份字节上共存——这正是 CStore 给不了的。
- **额外红利**:Microsoft 在 Iceberg 用 Vortex 探索 **zero-copy LSM compaction**——compaction 时把压缩字节**原样搬进新段不解压**,compaction 的 CPU/写放大被进一步压低(直接喂给你④的回收设计)。

**落到本仓:** Vortex 的 Layout 树 + zone-map + 段不可变,与本仓 `vector_smgr` 的"独立 fork + 1GB 文件分块 + 独立 buffer 池"模式同构。`bulkbuf_smgr.h` 里 `BulkBuffer` 已是"按 2^x 个 elem 分 chunk、`load_one_chunk` 懒加载、`visit_count` 命中统计"的列式块缓存雏形——Level 1 段引擎的 mmap/块缓存/GC 可直接复用这套页面整理框架,而不必照搬 SmithDB 的对象存储读路径。

---

## ② 内嵌倒排:term 列 FST + postings 分块 delta,如何与列式数据共存于一个段

这是 SmithDB **最有工程含量**的部分,官方专文《Designing an Inverted Index for Object Storage》逐项给了数字:

**(a) 倒排不是独立文件,而是作为列内嵌进同一个 Vortex 段。** doc-id **直接就是该 Vortex 数据文件里的行位置(row index)**——"**no translation table, no second identity to reconcile at query time**"(没有 segment-local id → 行号的翻译表)。倒排列与数据列共处一段、共享 Layout 树和 I/O 调度,这就是"压缩 + 随机访问 + 检索三合一"的物理形态。

**(b) term 列用 FST(有限状态转换器),且"每 row group 一个 FST"。** 与 Tantivy 的 per-segment 不同,SmithDB **per-row-group** 建 FST。压缩数字(原文):
- **term_key(JSON 路径,几百条路径在数百万行里重复)**:88.8 MiB 原始 → FST **3.8 KiB**(比原始小四个数量级,比 zstd 小 ~4×;对照 FSST 34.7 MiB、zstd 16.3 KiB);
- **term_value(1.41M 唯一 term)**:FST 32.7 MiB(比 zstd 21.7 MiB 大 ~1.5×,但仍胜 FSST);
- 合并 2.79M term:FST 37.6 MiB vs zstd 31.3 MiB。
- **统一两种查询形态进一个 FST**:term_value 存成 `{token}\0{flattened_path}`,让"键值检索"和"全文检索"共用一棵 FST。
- FST 可在压缩字节上直接做精确查找/前缀扫描/自动机遍历。

**(c) postings / positions 列用"128 元素块 + delta + 变长 bitwidth"。**
- 每个 term 的 list 切成**固定 128 元素块** + `<128` 的尾块;
- 块内 doc-id 存 **delta 并 bitpack 到最小位宽**:高频词如 `agent` 低至 **3–4 bit/doc**;稀有词与尾块退回 **VInt(~1 byte/delta)**,长尾优雅降级;
- 分块的意义:**避免一次性大内存分配**,且让"读某词的部分 postings"成为可能。

**(d) 字节预算式 row group(解决"term 频率倾斜"):**
- **每 row group postings ≤ 32MB**——"bound 住一个查询读该 row group postings 时最坏的对象存储 GET";
- **term-string ≤ 64MB**——cap 住内存与 I/O;
- 为什么按字节而非 term 数:"term count 是 I/O 大小的差代理";单个高频词("agent")在 v1 的 row group 里能把它撑过 500MB 压缩。字节预算从根上防爆。

**(e) zone-level 剪枝 + I/O 合并:**
- term 列带 **per-row-group min/max/count**:"对前缀路径查询这是最大的省——大多数 row group 根本不含谓词范围内的东西",**先整段跳过再走 FST**;
- 借 Vortex I/O 调度器:**把 1MB 内的邻近读合并、扩到 16MB 窗口**,顺序访问 → 极少几次 GET。

**(f) 三种查询模式**:`json_key`(文档是否含键 K)、`json_key_search`(键 K 的值是否匹配 V)、`search`(任意索引值是否匹配 Q)。

整套达成**全文 + JSON 过滤 P50 400ms**(对象存储上、面对深层大 JSON)。

**→ 它怎么解决了你的痛点:**
- **彻底解"压缩 XOR 可检索"(痛点②)**:倒排**内嵌进压缩列段**——老 trace 既被 Vortex 压着、又能 FST 精确/前缀查 + delta-postings 取行号 + 随机访问回原行。CStore "压了就不能建 GIN/BM25" 的死结被解开。
- **搜索加速**:doc-id=行号免翻译表、zone-map 先剪段、I/O 合并窗口、FST 在压缩域查——把"老 trace 可搜"做到 sub-second。

**落到本仓(强证据):** 本仓 BM25 **已经在封闭 Table-AM 下自建了一套内嵌倒排**:
- `bm25_inverted_list.cpp` 用自研 `DiskVector`/`FreeSpace`(`vtl/disk_container`)磁盘容器,**多级倒排表**(`short_store`/`long_store`、`il_threshold_levels`、`n_il_level`)、**版本号嵌进 offset 高位**(`get_version`/`inc_version`,merge-on-read 风格的版本管理)、**带 WAL**(`need_wal` 全程透传);
- `tokenizer/dict_jieba.cpp` 已内置 **jieba 中文分词**(SmithDB 的公开空白,我方现成);
- `bm25_token_index.cpp` 即 term→倒排的字典层,正对应 FST 的角色位。

也就是说,SmithDB 的"FST + 分块 delta postings + 字节预算 row group"在本仓有**直接对位的自研件**(`DiskVector`+`token_index`+jieba),Level 1 是把它们从"BM25 index AM"重整为"段内倒排列",而非从零造。term 字典若要对标 FST 的 3.8KiB,可引入 `fst`/`tantivy-fst` 思路或在 `token_index` 上加前缀压缩。

---

## ③ merge-on-read + deletion/upgrade vector:读路径如何折叠多版本 span 事件(免原地 UPDATE → 免膨胀)

**官方原文机制:** "**a run as a sequence of events, not a single immutable row**"。变更不同步重写文件,而是 **metastore 给段挂 deletion 向量与 upgrade(update)向量**——"query and compaction paths use those vectors to interpret the immutable file correctly"。读路径要做 **filter fanout + 在查询时高效 merge events**;真正的物化重写**延到 compaction**。

拆成读路径折叠逻辑【机制为官方,细节流程为我据其语义推演,标【推测】】:
1. 一个长 span 的 start / 中间 update / completion / tool_call / retry 是**多条 append 事件**,分散落在不同段(早上 start、下午 end);
2. 查某 run 时,按 `run_id`(/trace_id 排序键)把命中的多段事件拉成一个有序流;
3. **deletion 向量**标记"该行已被删除/被新版本取代"——读时跳过;**upgrade 向量**指向覆盖版本——读时以新版本折叠;
4. 归并出该 span 的**最终态**返回。这正是把"应用层 query-time fold"**下推到读引擎 + 用 zone-map/排序键剪枝**的过程。
5. 配套两招让读更快:**progressive time-window**(查"最新 N 条"时沿时间倒走、对最新候选段建有界时间窗,"read newest bounded slice → stream → merge → dedupe → 尽早停",而非"全排序再 limit");**读 ingestion 节点缓存**(每段记 writer 的 server id,writer 在线就直接扫它本地 SSD/内存,服务**活 trace**)。

**→ 它怎么解决了你的三处痛点(逐条对应):**
- **痛点①膨胀(根治)**:折叠/冻结**不再是 ASTORE 上的原地 UPDATE**,而是 append 事件 + 向量标记。**不产死元组、不触发 vacuum**。"早上出生下午死亡"的长 span 更新不引发写风暴。这是从存储模型层面拆掉膨胀来源。
- **痛点③ query-time fold 开销(根治)**:fold 从**应用层**移到**读引擎**,且 zone-map + 排序键 + progressive time-window 大幅缩小要 merge 的数据量;活 trace 直读热缓冲。活 trace 多/事件量大时不再线性变慢。
- **顺带**:deletion 向量也是**差异化 TTL/合规删除**的廉价实现——逻辑标记即删,物理回收延到 compaction(接④)。

**落到本仓:** `vector_smgr` 已有 `vec_invalidate_buffer_cache`(失效语义)、BM25 倒排 offset 已嵌**版本号**——deletion/upgrade 向量在单机可落成"**manifest/metastore 里 per-segment 的位图 + 版本向量**",读端持不可变段快照、归并时套用向量(本仓已是"单写者 + 独立 buffer 快照"模型)。metastore 角色直接用内嵌 PG/SQLite,与团队 catalog 能力对口。

---

## ④ 时间分层 compaction:写放大与回收

**官方原文:** "**Recent data is more likely to receive end events, so compacting it into huge files too early would create unnecessary write amplification. Older data is more stable and more likely to be scanned repeatedly, so it is worth collapsing into larger files.**" compaction 时"multiple segments are read and merged as a single ordered stream"。

要点:
- **新数据少压实**:近期段很可能还要收 end 事件(乱序、晚到),过早压成大文件 = 反复重写 = **写放大**。所以新段保持小、写优化。
- **老数据合大文件**:老数据稳定、常被反复扫(聚合/导出/训练),合成大段→**压缩比更高、索引更紧凑、zone-map 更有效、查询优化**;并在此刻**物化 deletion/upgrade 向量**(真正回收被删/被覆盖的旧版本行)。
- 这天然形成**时间维度冷热分层**:热=近期小段(待补 end、低写放大),冷=老大段(强压缩、利重复扫)。
- 与③的回收闭环:逻辑删除便宜(挂向量),物理回收**批量延到 compaction**,且借 Vortex **zero-copy compaction**(压缩字节原样搬运)进一步降 CPU/写放大。

**→ 它怎么解决了你的痛点:**
- **解膨胀(痛点①的回收侧)**:死版本不是靠 vacuum 逐元组清,而是**compaction 批量重写时一次性丢弃**——回收成本可控、可限速、与前台解耦。
- **匹配 trace 时序本质**:trace 几乎只追加、按时间查、老数据不再变;时间分层比为通用 KV 设计的 size-tiered/leveled 更简单高效,且"按时间范围裁剪"近乎免费。

**落到本仓:** 团队已有"页面整理框架";`bulkbuf_smgr` 的 chunk 化加载 + `BulkBufferManager` 的 load/release/auto_release 即段生命周期管理雏形。单机 compaction = 独立后台线程池 + **IO 限速(令牌桶)** 隔离前台(代替 SmithDB 的无状态 compaction service)——这是单机 P99 稳定的关键。

---

## ⑤ 大字段晚物化(late materialization)

**官方原文:** SmithDB "**separates core run fields from large fields. Core rows carry pointers to large-field files, and the query engine only fetches those large payloads when the query actually projects them.**"

机制:
- 段内(或分文件)把**核心 run 字段**(id/parent/start/end/status/cost/latency/token/tags)与**大字段**(input/output 自然语言全文、大 JSON、多模态 payload)**分列存储**;核心行只存**指向大字段文件的指针**;
- list / filter / 聚合 / 树加载这类高频查询**只读小核心列**,大 payload **只在用户真点开某条 trace(显式 project)时才去取**;
- 配合 Vortex 列裁剪(StructLayout)与 zone-map,**核心列扫描又快又不被大 payload 毒化**。

**→ 它怎么解决了你的痛点:**
- **解膨胀(痛点①)**:大 payload(MB 级、无上界)**不进核心列段**,核心列段小而密、压缩好、扫描快;膨胀压力被隔离在另一层。
- **搜索/列表加速(配合③)**:trace 列表、run 过滤、聚合、树加载(SmithDB 92ms)都只动小列;**②的倒排也是建在大字段子文件上**——全文索引与核心小列各管各,互不拖累。

**落到本仓:** 与底稿一致——ingestion 期就**抽离 base64/大文本 → 引用 token**(Langfuse `@@@langfuseMedia:...@@@` 范式)→ 落本地盘/MinIO,核心行只留指针 + SHA256 去重。`object_store` crate 一码通本地盘/MinIO,满足信创私有化。

---

## 总结映射表:Vortex/SmithDB 五机制 ↔ 三处痛点 ↔ 本仓落地件

| 机制(一手核实) | 解膨胀① | 解压缩XOR检索② | 解query-time fold③ | 本仓现成落地件 |
|---|---|---|---|---|
| **①列式不可变段**(Layout树+zone-map+压缩域算子下推,随机访问100x/段不可变) | ✅ 段不可变,无原地UPDATE/无vacuum | ✅ 压缩列段可随机访问 | — | `vector_smgr`(新fork+1GB块+独立buffer)、`bulkbuf`(chunk懒加载) |
| **②内嵌倒排**(per-rowgroup FST 3.8KiB + 128块delta postings 3-4bit + 32/64MB字节预算 + zone剪枝 + doc-id=行号) | — | ✅✅ 倒排内嵌进压缩段,老trace可搜 | — | `bm25`:`DiskVector`/`FreeSpace`多级倒排+版本号+WAL、`token_index`、`dict_jieba`(中文分词) |
| **③merge-on-read + deletion/upgrade向量**(run=事件序列,读路径折叠) | ✅✅ append+向量替代原地UPDATE,免死元组 | — | ✅✅ fold下推读引擎+zone剪枝+progressive window+活trace直读热缓冲 | offset内嵌版本号、`vec_invalidate_buffer_cache`、单写者+段快照、内嵌PG/SQLite当metastore |
| **④时间分层compaction**(新数据少压/老数据合大,向量物化回收,zero-copy搬字节) | ✅✅ 批量回收死版本替代vacuum | — | — | 页面整理框架、`BulkBufferManager`段生命周期、后台线程池+IO限速 |
| **⑤大字段晚物化**(核心列存指针,project才取大payload) | ✅ 大payload不入核心段 | — | ✅(配合)核心小列扫描不被毒化 | ingestion抽离引用token、`object_store`(本地盘/MinIO)、SHA256去重 |

**最关键的落地判断(诚实标注):** 本仓 openGauss 的 **Index-AM 开放、Table-AM 封闭**,而 SmithDB 这五项全部属于"存储格式 + 索引"层面——**恰好都能落进自定义存储 backed 的 Index-AM**,沿 DiskANN `vector_smgr`(独立 fork + `vector_buffers` 独立池)与 BM25 `DiskVector`/jieba/WAL 的现成模式实现,**完全绕开封闭 Table-AM**。Level 1 不是从零造,而是把"BM25 内嵌倒排 + DiskANN 段式存储管理 + 页面整理框架"重整为"trace 列式不可变段 + 段内倒排 + 向量 merge-on-read + 时间分层 compaction"。

**两点不确定/需 PoC 坐实(不编造):** (a) Vortex 是 2025-08 入 LF 的~1 年新格式,其在"高频乱序 span + 频繁 compaction"下的写入/compaction 稳定性需 PoC 验证(Lance 是 Apache-2.0 的格式层降级方案);(b) SmithDB 的 FST term 字典在本仓需新引入(`fst`/`tantivy-fst` 或在 `token_index` 上做前缀压缩),`DiskVector` 现有字典层能否压到对标 3.8KiB 的量级,需实测。

**主要来源(一手):** [SmithDB 倒排索引设计(FST/postings/字节预算/zone/16MB窗口/三查询模式)](https://www.langchain.com/blog/full-text-search-in-smithdb-designing-an-inverted-index-for-object-storage)、[Introducing SmithDB(LSM/merge-on-read/deletion&upgrade向量/时间分层compaction/晚物化/progressive window/活trace)](https://www.langchain.com/blog/introducing-smithdb)、[ZenML LLMOps 倒排拆解](https://www.zenml.io/llmops-database/building-full-text-search-for-agent-traces-with-custom-inverted-index-on-object-storage)、[Vortex Layouts 文档(Flat/Struct/Chunked/Zoned)](https://docs.vortex.dev/concepts/layouts)、[Spiral:Vortex 随机访问/压缩域算子/zero-copy compaction/100x](https://spiraldb.com/post/vortex-a-linux-foundation-project)。本仓代码:`src/include/access/annvector/store/{vector_smgr.h,bulkbuf_smgr.h,smgr.md}`、`src/gausskernel/storage/access/bm25/{bm25_inverted_list.cpp,bm25_token_index.cpp,tokenizer/dict_jieba.cpp}`、`diskann/storage_interface/{disk_store.cpp,mem_store.cpp}`。

---

# 调研:openGauss 上"自定义存储 backed AM"的真实可行模式(读他们 DiskANN 代码)

I have all the evidence I need. The `disk_container` template library is exactly the reusable "mini storage engine on buffer-managed pages" toolkit: `BlockMgr` (reads/writes `MAIN_FORKNUM` pages via `ReadBufferExtended`, reserves pages via `smgrnblocks`/`reserve_new_pages`), `FreeSpace` (free-page list for delete/reuse), `DiskVector`/`DiskArray` (typed append/random-access containers with `AccessorLockType` lock modes and `need_wal` flag), and `disk_hashtable`. Note `DiskArray` even calls `Bm25XLogInitPage` for WAL — these containers are shared between DiskANN and BM25.

I now have complete file-level evidence for all four sub-questions. Let me write the final report.

---

# yiTrace 落地调研：在这版 openGauss 上自建「自定义存储 backed 的访问方法」的可行模式与边界

代码基线：`/Users/Four/JobProjects/yitrace/openGauss-vector-main`（openGauss 5.x，PG 9.2.4 内核谱系）。以下结论全部有文件级证据；不确定项已明确标注。

---

## 0. 一句话结论

他们**已经把「自定义存储 backed 的 index AM」这条路走通并产品化了**，而且不止一种存储后端，是**两套并存**：

- **存储路线 A（主 fork + 页式）**：用 `disk_container` 模板库（`BlockMgr`/`DiskVector`/`DiskArray`/`DiskHashTable`/`FreeSpace`）在 `MAIN_FORKNUM` 上自建「带页头、走 shared buffer、走标准 WAL」的类型化容器。**BM25 倒排、DiskANN 图元数据全部用这套。**
- **存储路线 B（独立 fork + 旁路缓存）**：新增 `VECTOR_FORKNUM`（一个全新的 fork 号），用 `vector_smgr` 自管段文件 + **独立 buffer 池 `vector_buffers`** + **独立后台刷写线程 `vec_writer`**，做大块原始向量的随机读。DiskANN 的原始向量走这条。

trace 列式段存储**完全可以照搬**，而且**应该两条都用**：列式段数据走路线 B（独立 fork + 自管缓存，压缩+随机访问），段内倒排/min-max/字典等索引结构走路线 A（页式 + 标准 WAL）。**但这条路不是「AM 框架内就够」，它必然要 fork 改内核源码**——下面逐条给边界。

---

## ① DiskANN 怎么自管段文件 + 独立 buffer 池 + 自定义页面格式（这套机制叫什么）

这套机制在代码里叫 **`vector_smgr`（向量存储管理器）+ VECTOR_FORKNUM（独立 fork）+ vector_buffers（独立缓冲池）**。它不是 PG 的某个公开扩展点，而是**他们魔改 SMGR/fork 枚举后自建的一层**。

**1. 新增了一个全新 fork 号（改内核枚举）**
`src/include/storage/smgr/relfilenode.h:49`
```c
#define VECTOR_FORKNUM 5
...
#define MAX_FORKNUM VECTOR_FORKNUM   // 第57行，把上界也顶上去了
```
`src/common/backend/catalog/catalog.cpp:90-96` 的 `forkNames[]` 数组尾部加了 `"vec"`，于是 `relpath()` 会生成 `<relfilenode>_vec` 的物理文件名。**这意味着这个 fork 是标准 md.c/smgr 机制的一等公民**——文件创建、unlink、segment 切分、relpath 全部复用内核。

**2. 段文件自管，但骑在标准 md.c 段链上**
`vector_smgr.cpp`（`src/gausskernel/storage/access/annvector/module/vector_smgr.cpp`）：
- 创建：`create_vec_data()` 第1037行 → `smgrcreate(rel->rd_smgr, VECTOR_FORKNUM, false)` + `log_smgrcreate(...)` + `smgrimmedsync(...)`。**复用 smgr 创建 + 复用 storage WAL。**
- 打开段：`vec_getseg/vec_openseg`（第1158-1195行）调 `_mdfd_openseg(reln, VECTOR_FORKNUM, segno, ...)` 和 `mdopen(reln, VECTOR_FORKNUM, ...)`，**复用 md.c 的 1GB 段链**（`max_file_size = RELSEG_SIZE * BLCKSZ`，第54行）。
- 读写：**自己的 `pread_file`/`pwrite_file`（第1073/1107行）按字节偏移直接 pread/pwrite**，不走 page-at-a-time、不经 shared buffer。这是它能「随机读任意长度向量、不受 8KB 页边界约束」的关键。`vec_read/vec_write`（第1278/1331行）是入口。
- 截断：`truncate_vector_file()` 第1148行 → `smgrtruncatefunc` + `XLogTruncateRelation(rel->rd_node, VECTOR_FORKNUM, 0)`。

**3. 独立 buffer 池 vector_buffers（不是 shared_buffers）**
`vector_smgr.h:84-89` + `vector_smgr.cpp` 头部：
- 独立 GUC：`vector_buffers`、`vector_buffer_thread_num`（`smgr.md:29` 文档说明，POSTMASTER 级）。
- 独立池结构：`VecBufferManager`（挂在 `g_instance.diskann_cxt.vec_buffer_mgr` 这个**内核全局实例结构体的新增字段**上，第53行），用 `boost::concurrent_flat_map<BufferSignature, ...>` 做 `<rel_id, offset>→slot` 定位（第91-116行）。
- 读接口：`VecBuffer vec_read_buffer(rel, loc, vec_size)`（`disk_store.cpp:187` 在用），返回带 pin 的 buffer，用完 `release()`。
- **还有第二层纯内存缓存 BulkBuffer**（`bulkbuf_smgr.h`），把整条索引的 PQ/RABITQ 码加载成「逻辑连续大内存」，定位退化为 `StartAddress + idx*dim` 的 O(1) 寻址（`PQ + bulkbuffer.md:35-38`，实测再快 30~40%），通过 admin SQL `index_memory_load/release` 手动触发。

**4. 自定义页面格式（这部分在主 fork）**
注意区分：**原始向量在 VECTOR_FORKNUM（无页头、纯字节流）**，但**图结构/元数据在 MAIN_FORKNUM（有标准 PageHeader + 自定义 opaque）**。
- `DiskAnnMetaPage`（`diskann.h:91-108`）：自定义元页，记 `nodeMetaBlkNo`/`graphMetaBlkNo`/`pqPivotsMetaBlkNo` 等各容器的起始块号。
- `DiskAnnPageOpaque`（`diskann.h:86`）：自定义页 opaque，注释明说「opaque has the same format with diskvector opaque」。
- 这些页通过 **`disk_container` 模板库**操作（见 ③）。

---

## ② BM25（OID 4429 fulltext）：自研倒排怎么落盘 + 注册 AM

BM25 是**纯路线 A**——完全建在 `MAIN_FORKNUM` 上，没用 VECTOR_FORKNUM，所以是「页式 + 标准 WAL」最干净的范本。

**1. AM 注册（pg_am 一行 DATA）**
`src/include/catalog/pg_am.h:182-184`
```
DATA(insert OID = 4429 (  fulltext  0 0 f t f f t t f f f f f 0
   bm25insert bm25beginscan bm25gettuple - bm25rescan bm25endscan - - -
   bm25build bm25buildempty bm25bulkdelete bm25vacuumcleanup - bm25costestimate bm25options));
#define bm25_AM_OID 4429
```
即一个标准 index AM 的 16 个 handler 全填齐（`aminsert/ambeginscan/amgettuple/ambuild/ambulkdelete/...`）。**注册 AM 本身就是改这个内核 catalog 头**（openGauss 用 `.h` 里的 `DATA()` 行，不是 PG13+ 的 `.dat`）。

**2. 倒排落盘结构**
`src/include/access/bm25/bm25_inverted_list.h`：
- `InvertedListPageData`（第53行）：每页一个倒排链片段，页头后是 `nentry` + `skip_pointer`（跳表指针）+ `entries[]`。`GetInvertedListPage(page) = page + SizeOfPageHeaderData`（第68行）——**标准 PG 页头 + 自定义页体**。
- 每页能放 `max_il_page_nentry = (BLCKSZ - page_entry_offset)/sizeof(InvertedListEntry)`（第88行）。
- 还有 `bm25_token_index`、`bm25_doc_store`、`bm25_statistics` 等多种页类型，靠元页里的块号互相串联（同 DiskANN 元页思路）。

**3. WAL：用标准 buffer-redo 机制（关键范本）**
`bm25xlog.cpp` 是「自研索引怎么做 WAL」的标准答案：
- 写侧（第214-295行）：`XLogBeginInsert()` → `XLogRegisterBuffer(0, buffer, REGBUF_STANDARD)` → `XLogRegisterBufData(0, page+offset, size)` → `XLogInsert(RM_BM25_ID, XLOG_BM25_INSERT_ENTRY)` → `PageSetLSN`。**全程 START/END_CRIT_SECTION 包裹。**
- 读侧（第27-212行 `bm25_redo`）：`XLogReadBufferForRedo(record, 0, &buffer)` → `memcpy` 进页 → `PageSetLSN` + `MarkBufferDirty`。
- **前提是它用了 `RM_BM25_ID` 这个新资源管理器**（见 ④，又一处改内核）。

---

## ③ trace 列式段存储能不能也做成「index AM + 自管 smgr 段文件」？能否被优化器/执行器接住？

**能，而且有现成的两层工具。**

### 3a. 现成的「页式 mini 存储引擎」：`disk_container` 模板库
位置：`src/include/templates/vtl/disk_container/`（DiskANN 和 BM25 **共用**这套）。它就是「在 buffer-managed 主 fork 页上自建类型化容器」的工具箱：

| 组件 | 文件 | 作用 |
|---|---|---|
| `BlockMgr` | `blockmgr.hpp:31` | 经 `ReadBufferExtended(_rel, MAIN_FORKNUM, blkno, ...)` 读页、`reserve_new_pages()`/`smgrnblocks` 扩页、按 `AccessorLockType` 上锁 |
| `DiskVector<T>` | `diskvector.hpp` | 类型化随机访问/追加容器，`get_n/set_n/push_back/extend`，`need_wal` 开关 |
| `DiskArray<T,N>` | `diskarray.hpp:58` | 定长块数组，建页时直接 `Bm25XLogInitPage(buf, page)` 落 WAL |
| `FreeSpace<T>` | `freespace.hpp:13` | 空闲页链表——**这正是 delete/复用页、避免膨胀的基建** |
| `DiskHashTable` | `disk_hashtable.hpp` | 落盘哈希表（BM25 token→postings 在用） |
| `AccessorLockType` | — | `ReadLock/WriteLock/NoLockRW/NoLockUnsafe/ExternalLock` 多种并发模式 |

→ **trace 段内的倒排（term→ctid 列表）、min-max/zone-map、字典、deletion-vector，直接用 DiskArray/DiskHashTable/DiskVector 建，自动获得 shared buffer 缓存 + 标准 WAL。** 这是路线 A，零内核改动即可用（只要你的代码能 include 这个模板库并链到现有 AM 模块里）。

### 3b. 现成的「独立 fork 大块存储」：vector_smgr 模式
→ **trace 列式段的列数据本体**（编码压缩后的列块），走 VECTOR_FORKNUM 同款思路：自管段文件 + 字节级随机读 + 独立缓存池。这是路线 B，**需要内核改动**（见 ④）。

### 3c. 优化器/执行器怎么接住（merge-on-read 的扫描怎么挂）

**这是最关键、也是最该照抄 DiskANN 的部分**：它们**没有发明新的扫描节点**，而是**复用 PG 标准的 IndexScan/amgettuple 契约**——索引内部读自管存储，对外只吐 **heap 的 ctid**：

`diskann_scan.cpp`：
```c
:18   ItemPointer tids;                          // 扫描状态里存一批 ctid
:116  so->tids = palloc(searchListSize * sizeof(ItemPointerData));
:134  scan->xs_ctup.t_self = so->tids[so->currIndex];   // amgettuple 把 ctid 塞回执行器
:150  bool diskanngettuple_internal(IndexScanDesc scan, ScanDirection dir)
```
执行器拿到 `xs_ctup.t_self` 后照常回堆表取行。**自定义存储完全藏在 AM 背后，执行器/优化器契约不变。**

对 **trace 的 merge-on-read** 落地有两种挂法：

1. **走 amgettuple（推荐起步）**：trace 段 AM 内部完成「读多个不可变段 + 合并 deletion-vector + upgrade-vector + query-time fold」，对外只吐通过过滤的 ctid（或直接吐折叠后的 trace 行——见下）。merge 逻辑 = `diskann_scan` 里那段图搜索的位置，换成「多段归并 + 删除位图过滤」。
   - 代价估计：`amcostestimate`（`diskanncostestimate_internal`，`diskann.cpp:107`）照填即可，优化器就会选它。
   - 位图扫描：注意 BM25/DiskANN 的 pg_am 行里 `amgetbitmap` 那一列是 `-`（未实现），**只支持 amgettuple，不支持 bitmap-scan**。trace 若要 bitmap-AND 多条件，要么自己实现 `amgetbitmap`，要么在单个 AM 内部做多列合并。

2. **不回堆表，AM 直接产出列值（晚物化/列裁剪的关键）**：若希望「段里就是全部 trace 数据、不要堆表」，amgettuple 这条路就不够了——因为 IndexScan 必然回堆表。这时有两个选择：(a) 让 trace 表本身是个**普通堆表只存 ctid 占位/极小列**，宽数据和列块都在 AM 段里，扫描时 AM 直接返回；(b) 走 **Custom Scan Provider**（PG 的 `CustomScanMethods`，openGauss 同样有）做一个不依赖堆表的扫描节点。**(b) 是 Vortex/列存的正解，但 openGauss 的 CustomScan 完整度需另行验证（标注：未在本次代码中确认 openGauss CustomScan 对列式投影的支持度，建议先用 (a) 起步）。**

---

## ④ 哪些必须改内核源码、哪些在 AM 框架内就行；WAL/恢复怎么处理

### 必须改内核源码（fork 不可避免的清单）

| # | 改动点 | 文件:行 | 性质 |
|---|---|---|---|
| 1 | **新增 fork 号**（若走路线 B） | `relfilenode.h:49,57` + `catalog.cpp:96` forkNames | 改枚举 + 数组，编译期 |
| 2 | **注册 index AM**（pg_am 行 + `#define xxx_AM_OID`） | `pg_am.h`（如 4429/4471） | 改内核 catalog 头 |
| 3 | **注册资源管理器 RM_xxx_ID**（WAL 的核心） | `rmgrlist.h:85,88`（`RM_DISKANN_ID`/`RM_BM25_ID`，绑 `diskann_redo`/`bm25_redo`/`*_desc`） | 改编译期 PG_RMGR 列表，**RM_MAX_ID 名额有限** |
| 4 | **并行/极限 RTO 恢复的二级派发表** | `redo_xlogutils.cpp:1316,1325`（`XLogBlockDataCommonRedo` 的 switch）+ `:1919,1922`（`{DiskannRedoParseToBlock, RM_DISKANN_ID}` 派发数组） | 改内核恢复路径 |
| 5 | **独立 buffer 池 + 后台刷写线程**（若走路线 B 的旁路缓存） | `g_instance.diskann_cxt`（新增内核全局实例字段）；`postmaster.cpp:14272 vec_writer_main()` + 线程计数 `:1043-1068` + latch `:7348/8391` | 改 postmaster + 全局实例结构体 |
| 6 | 注册 redo 回调（普通串行恢复路径） | `rmgr.cpp:65 RmgrTable[]`（由 #3 的 rmgrlist.h 宏自动展开，但仍是编译期内核） | 同 #3 |

> 结论：**「自定义存储 backed 的 AM」在这版 openGauss 上不是纯插件能力，它是「半 fork」**——AM 的 handler 函数本身可以全写在你的 access/ 子目录里（像 bm25/、diskann/ 那样独立编译单元），**但要让它的 WAL 能 redo、能在主备/极限RTO下恢复、要有独立缓存线程，就必须改 6 处内核固定表**。这正是题面「Table-AM 封闭、Index-AM 半开放」的真实含义：**Index-AM 的接口是开放的（pg_am 填函数指针），但配套的 WAL 资源管理器、恢复派发、buffer/线程基建是封闭枚举，得改源码。**

### 在 AM 框架内就行（不用动内核固定表）的部分
- 所有 16 个 AM handler 的实现（build/insert/scan/vacuum/cost/options）——独立 .cpp，像 `bm25.cpp`/`diskann.cpp` 那样。
- 用 `disk_container` 模板库（路线 A）建任意页式容器：倒排、字典、min-max、deletion-vector、freespace。**这一层完全复用，零内核改动。**
- 自定义页面格式（PageHeader + 自定义 opaque/页体）。
- merge-on-read 的归并扫描逻辑（藏在 amgettuple 内部）。

### WAL/恢复对自管段怎么处理（两种策略，照 DiskANN 抄）

DiskANN 同时示范了两种 WAL，trace 应**分而治之**：

**策略 1 —— 页式数据走标准 buffer-redo（用于段内倒排/元数据/deletion-vector）**
全套是 `bm25xlog.cpp` / `diskannxlog.cpp` 里的：
```
写: XLogBeginInsert → XLogRegisterBuffer(REGBUF_STANDARD) → XLogRegisterBufData → XLogInsert(RM_xxx_ID, op) → PageSetLSN
读: XLogReadBufferForRedo → memcpy 进页 → PageSetLSN + MarkBufferDirty
```
crash-safe、可走主备复制、可走极限 RTO 并行恢复（前提是 #4 派发表填了）。

**策略 2 —— 独立 fork 大块数据走「物理字节 redo」（用于列式段本体/原始向量）**
DiskANN 对 VECTOR_FORKNUM 的处理（`diskannxlog.cpp:111 DiskannAddVector`）：
```c
xl_ann_add_vector *xl_rec = ...;           // WAL 记录体里直接带向量字节
SMgrRelation smgr = smgropen(tmp_node, ...);
if (!smgr->md_fd[VECTOR_FORKNUM] && !smgrexists(smgr, VECTOR_FORKNUM)) return;  // 文件不在就跳过
vec_write(smgr, xl_rec->offset, xl_rec->nbytes, vec, false);   // redo 时直接按偏移重写
```
即：**把要写入自管 fork 的字节，整段记进 WAL 记录体，redo 时直接 `vec_write` 重放到 fork 文件的指定偏移**，绕开 page-redo。写入侧由 `LogManager`（`disk_store.cpp:325 logmgr.log_write_vector(...)`）在真正 `write_vector` 前先发这条 WAL。
- **代价/边界**：这种方式 WAL 量 = 数据量（不像 page-redo 有 FPI 折叠），对**大列块 + 高写入**会放大 WAL。**对 trace 列式段，应在 compaction（批量、不可变段一次成型）时用 `REGBUF_FORCE_IMAGE` 整页镜像或 `log_newpage` 批量记，而不是逐行记**——参照 `Bm25XLogAppendPage`（`bm25xlog.cpp:225` 用 `REGBUF_FORCE_IMAGE`）和 `DiskannRedoExtendFullPages`（`diskannxlog.cpp:46`，整页恢复）。
- **不可变段是优势**：段一旦封存只读，WAL 只在 compaction 写一次，之后纯读。这天然契合 SmithDB Vortex 的「不可变段」模型，WAL 压力远小于 ASTORE 原地 UPDATE。

---

## 对「trace 存储到底怎么建、要不要 fork」的承重判断

**要 fork，但是「轻 fork」——改的是 6 张固定表 + 复用一个 access/ 子模块编译单元，不是动 Table-AM。** 这条路他们已经趟过两遍（DiskANN、BM25），团队有完整先例代码可抄。具体建议：

1. **AM 形态**：trace 段存储做成一个 index AM（新 pg_am OID，比如挂在 trace 主表上的一个特殊 `trace_segs` 索引），16 个 handler 独立成 `access/tracevault/` 子目录，编译进内核。**不要试图做 Table-AM**（`TAM_HEAP/TAM_USTORE` 是硬编码枚举，确认封闭）。

2. **存储分层**：
   - 列式段本体（压缩列块）→ **新增 `TRACE_FORKNUM` + 自管 smgr**，照 `vector_smgr` 抄（字节级随机读 + 独立缓存池 + vec_writer 同款后台线程）。这解决「压缩 XOR 可检索二选一」——段内既压缩又随机访问。
   - 段内倒排/字典/min-max/deletion-vector/upgrade-vector → **`disk_container` 模板库**（DiskArray/DiskHashTable/FreeSpace），走 MAIN_FORKNUM + 标准 WAL。这是内嵌倒排（对应 Vortex 的 FST term 列）。

3. **膨胀根治**：不可变段 + compaction（时间分层）+ `FreeSpace` 页链复用，彻底避开 ASTORE 原地 UPDATE 产死元组的问题。折叠/冻结 = 写新段 + deletion-vector 标删，不 UPDATE。

4. **query-time fold 根治**：把折叠逻辑下沉进 amgettuple 的 merge-on-read（活段 + 冻结段归并），不在应用层折叠。冷段预折叠物化、热段读时合并。

5. **WAL**：段本体用「批量 FPI / log_newpage」式（compaction 一次性），段内索引用标准 buffer-redo。**必须同时填 `rmgrlist.h` + `redo_xlogutils.cpp` 两处**，否则主备/极限RTO 恢复会 PANIC（`redo_xlogutils.cpp:1328 default: PANIC unknown rmid`）。

6. **风险/待验证（诚实标注）**：
   - `RM_MAX_ID` 资源管理器名额是定长数组，需确认还有空位（他们已用到 RM_BM25_ID，得查 `RM_MAX_ID` 上限）。
   - 不回堆表的「纯列式投影扫描」需 CustomScan，**openGauss CustomScan 对列式晚物化的支持度本次未确认**，建议起步先用 amgettuple+回堆表，宽列暂放堆表，迭代再下沉。
   - `amgetbitmap` 在 BM25/DiskANN 都没实现，多条件 bitmap-AND 要自己写或在单 AM 内合并。

**关键文件清单（给后续设计/实现直接定位）**：
- 机制总入口：`src/include/access/annvector/store/vector_smgr.h`、`bulkbuf_smgr.h`、`smgr.md`
- 自管 fork 实现：`src/gausskernel/storage/access/annvector/module/vector_smgr.cpp`（create_vec_data:1037 / vec_read:1278 / vec_write:1331 / vec_writer_main:1466）
- 页式容器库：`src/include/templates/vtl/disk_container/{blockmgr,diskvector,diskarray,disk_hashtable,freespace}.hpp`
- fork 枚举：`src/include/storage/smgr/relfilenode.h:49,57` + `src/common/backend/catalog/catalog.cpp:90-96`
- AM 注册：`src/include/catalog/pg_am.h:166（diskann 4471）, 182（bm25 4429）`
- WAL 写/读：`src/gausskernel/storage/access/{bm25/bm25xlog.cpp, diskann/diskannxlog.cpp}`
- 资源管理器注册：`src/include/access/rmgrlist.h:83-88`
- 恢复二级派发：`src/gausskernel/storage/access/redo/redo_xlogutils.cpp:1316,1325,1919,1922`
- 后台线程/全局实例：`src/gausskernel/process/postmaster/postmaster.cpp:14272` + `g_instance.diskann_cxt.*`
- 扫描吐 ctid 范本：`src/gausskernel/storage/access/diskann/diskann_scan.cpp:18,116,134,150`

---

# 调研:列式段+merge-on-read+内嵌倒排+区间树 设计原理

Confirmed the final pieces: BM25's `bm25_doc_store` maps `doc_id ↔ ItemPointerData tid + part_id` (a doc-id ↔ row-position table, exactly the segment-local id translation Vortex avoids), `vacuum` returns a `doc_id_track` (the deletion set), and `try_upgrade`/`try_downgrade` migrate postings between storage tiers as cardinality changes (the upgrade-vector precedent). Jieba is wired through TS template `TSTemplateJiebaId` with a `Jieba` object exposing cut/lexize. I have full file-backed grounding. Here is the design.

---

# yiTrace Level 1：trace 专用段格式（TraceSeg）+ 索引 设计原理库

> 定位：Level 0（标准 openGauss 表 + 复用既有索引）扛不住的三处——**膨胀 / 压缩 XOR 可检索 / query-time fold 开销**——由 Level 1 从存储格式根上解决。对标 SmithDB 的 Vortex + 自研倒排 + LSM merge-on-read，落到 yiTrace/openGauss 内核。
> 落地路线（已查实，见文末"内核可行性"）：**走"自管段文件的 Index-AM"（DiskANN/`vector_smgr` 同构）绕开封闭 Table-AM**，复用既有 `disk_container`（`DiskVector`/`VarDiskVector`/`DiskHashTable`/`BlockMgr`/`InvertedList`）做段内容器，复用 `vector_smgr` 的独立 FORK + 独立 buffer 池做段文件与缓存。
> 每条标注【根治膨胀】或【加速搜索】（部分两者兼有）。诚实标注未验证项。

---

## 0. 全局形状：TraceSeg = 一个段 = 一个自描述文件区

一个 TraceSeg 段 = 内核一个 relation 上的一段连续 block（用既有 `BlockMgr::reserve_new_pages` 几何增长分配，见 `blockmgr.hpp:96`），内部布局：

```
[SegMeta]  magic/version/行数/时间边界[min_ts,max_ts]/列目录/索引目录/编码字典指针/deletion&upgrade 向量指针
[列区]     每逻辑列 → 一组 zone(默认 8192 行/zone)，每 zone 独立选编码 + zone-map(min/max/count/null_count)
[倒排区]   per-text-column：FST(term→postings_blkno) + 分块 postings(128/块, 变长 bitwidth delta)
[树区]     pre/post/lvl 三列(随段物化) + span_id↔行号 映射
[向量区]   可选：DiskANN 图(复用)或段内 flat 向量 + zone-map
[行号映射] seg-local rownum ↔ (span_id) ；doc-id 直接 = 段内行号(省翻译表，见 §3)
```

段**不可变**（immutable）。写不改段，删除/更新不改段——全部走 §2 的 deletion/upgrade 向量。这是三处根治的总开关：**段一旦封盖只读，就没有原地 UPDATE、没有死元组、没有 vacuum**。

> 复用事实：`DiskVector::get_disk_vector(rel, is_wal, fork_num)`（`diskvector.hpp:168`）已支持把一个容器开在**指定 FORK** 上并几何扩页；`vector_smgr` 已有独立 `VECTOR_FORKNUM`/`vector_buffers`（`vector_smgr.h:84-101`，`smgr.md`）。Level 1 段文件用同一手法开 `TRACESEG_FORKNUM`，与主 fork 物理隔离、独立缓存、独立刷盘线程。

---

## ① 列式不可变段 + 可插拔轻量编码 + zone-map：压缩与随机读如何兼得

### 原理

**逻辑/物理分离 + 按 zone 选编码 + zone-map 段剪枝**，是"压缩"和"随机读"不再二选一的关键（对标 Vortex 逻辑 schema 与物理 layout 解耦、级联编码，研究底稿 line 98、154）。

1. **列存 + 定宽 zone**【加速搜索 + 根治膨胀】
   一列切成定行数 zone（默认 8192）。zone 是"压缩单元"也是"随机读单元"：随机取第 N 行 = `zone = N/8192; off = N%8192`，O(1) 定位到 zone，再在 zone 内解一个块即得——**不是 Parquet 那种读 footer+rowgroup 多次往返**。这正面回答"压缩 XOR 随机读":随机读的粒度是 zone（几 KB），不是整列。
   - 复用：`DiskVector` 的 `navigate_blkno_offset`（`diskvector.hpp:512`）已实现"行号→(页组,块,页内偏移)"的 O(1) 定位（几何页组 + clz 位运算）。Level 1 在其上加一层"行号→zone→块内偏移"。

2. **可插拔轻量编码（每 zone 独立选）**【根治膨胀】
   每个 zone 头部存一个 `encoding_id`，按列数据特征选最省的：
   | 编码 | 适用 trace 列 | 说明 |
   |---|---|---|
   | **delta + bitpack** | `ts`/`start_time`/`end_time`/`event_id`(雪花单调)/`pre`/`post` | 单调或近单调列，delta 后值域极小，按 zone 最小 bitwidth 打包（对标研究 line 154 postings 的"每词变长 bitwidth delta"） |
   | **RLE** | `status`/`event_type`/`span_kind`/`tenant_id`(段内常同租户) | 低基数、长游程 |
   | **字典 (dict)** | `model`/`name`/`span_kind` 文本枚举 | 段内不同值少，存 dict + 小码宽索引；过滤可在码上比较，不解字符串 |
   | **FSST** | `name`/短 `input_text`/`dotted_order` 等短字符串 | 字符串子串压缩，**压缩域可前缀比较**（对标研究 line 98 FSST） |
   | **ALP** | `total_cost`/`latency_ms`/数值 attrs | 浮点无损轻量压缩（对标研究 line 98 ALP） |
   | **plain** | 兜底/高熵列 | 不压缩 |

   "可插拔"的工程兑现：`VarDiskVector<T>`（`diskvector.hpp:549`）已支持变长元素；编码器/解码器做成一组 `encode(zone_in)->bytes` / `decode_at(bytes, off)->value` 的函数表，按 `encoding_id` 分派。**自研三件套**（delta-bitpack / RLE / dict）团队的"页面整理框架"能力直接覆盖；FSST/ALP 可一期 plain 兜底、二期补（诚实标注：FSST/ALP 内核现无现成实现，需自研或移植，见文末未验证项）。

3. **zone-map（min/max/count/null_count）段剪枝**【加速搜索】
   每 zone 存 4 个统计量。查询带谓词（`tenant_id=? AND start_time>=? AND total_cost>?`）时，先用 zone-map 把整段/整 zone 剪掉再解码——对标研究 line 105/154 的"zone 级 min/max/count 先剪 row group 再走 FST"。SegMeta 里再存段级 `[min_ts,max_ts]`，让"最近 7 天"这类时间谓词**直接跳过整个段**（呼应 SmithDB metastore 的 time bounds，研究 line 86）。
   - 与既有索引协同：这是**段内剪枝**，与 Level 0 的分区裁剪正交——分区裁掉天级，zone-map 裁掉段内 zone 级。

### 为什么能兼得（一句话）
压缩单元 = zone（独立编码、级联压缩 → 高压缩比根治膨胀）；随机读单元 = zone（O(1) 定位 + 只解一个 zone → 不牺牲随机读）；剪枝单元 = zone-map（先剪后解 → 搜索加速）。三者同一粒度，互不打架。

---

## ② merge-on-read：多版本 span 折叠 + deletion/upgrade vector

### 这是【根治膨胀】的核心一招，同时去掉【query-time fold 的应用层开销】

**思路**：span 的"折叠/冻结/更新/删除"全部不改段。段是某时刻一批事件的不可变快照；逻辑当前态 = 多个段 + deletion/upgrade 向量在**读路径上**合并出来的。免原地更新 → 无死元组、无 vacuum、无写放大。

### 数据模型
- 写：同一 `span_id` 的多条事件，落进**不同段**（按到达时间分段，append-only）。
- deletion vector（每段一个 bitmap）：标记"本段第 i 行已被逻辑删除/被更高版本覆盖"。挂在 SegMeta，**不重写段**（对标研究 line 86/139/388）。
- upgrade vector（段间）：metastore（Level 0 的小元数据表即可）记录 `span_id → 最新版本所在 (seg_id, rownum)`；旧段对应行在其 deletion vector 置位。

### 读路径折叠算法（按 span_id）
查一个 trace / 一个 span 的当前态：

```
1. 时间裁剪：用各段 SegMeta 的 [min_ts,max_ts] + zone-map 选出相关段集 S（跳过无关段）。
2. 候选行收集：对 S 中每段，用段内 span_id 索引(或行号映射)取出该 span_id 的所有行，
   过滤掉 deletion vector 已置位的行。
3. 段内/段间归并：把候选行按 (seq, ts, event_id) 排序(雪花全局单调保序)，做与 Level 0
   折叠语义完全一致的合并：后写覆盖标量、token/cost 累加、attrs 深合并、end 补全、status 推断。
4. 投影 + 晚物化：只对最终投影列解码；大字段(input/output 全文、媒体)此刻才按 payload_ref
   去 CAS 取(见 §⑤大字段晚物化)。
```

关键：**折叠从 Level 0 的"SQL MERGE INTO + query_dop=1 自定义聚合 / 大 trace 退化到应用层 DFS"下沉为段读路径里的归并算子**——这正面消除了任务点 3"query-time fold 在应用层、活 trace 多/事件量大时慢"的根因。归并是段内有序 + 段间多路归并，是 O(n log k)（k=段数），不是应用层全表聚合。

### 正确性（乱序 / 晚到）
- **乱序**：归并键 `(seq, ts, event_id)`，`event_id` 用应用端雪花（全局单调，Level 0 §2 已定），即使事件物理乱序到达、落进不同段，归并排序后语义确定。
- **晚到（冻结后才到的 feedback/eval）**：不再需要 Level 0 的 `late_event_inbox + 重融化重写冷分区`。晚到事件**就是新写一个段**（append-only 永远成立），其 `span_id` 命中已存在的旧段；下次读时该 span 的候选行自然包含新段的行，归并即得正确当前态。upgrade vector 把 `span_id→最新版本`指到新段，旧段对应行 deletion 置位。**晚到 = 普通写，零特殊路径**——这是事件模型 + merge-on-read 相对 Level 0 物化表的结构性优势（研究 line 423）。
- **删除/TTL**：合规删 = deletion vector 置位（廉价逻辑标记，研究 line 407）；物理回收推迟到 §⑤ compaction。

### 与既有内核的同构证据（不是空想）
BM25 已经在做这件事的**同形**操作：
- `bm25_doc_store` 维护 `doc_id ↔ (ItemPointerData tid, part_id)` 映射并有 `erase`（`bm25_doc_store.h:58-66`）——即段-local id ↔ 行位置的翻译 + 逻辑删除。
- `InvertedList::vacuum(doc_id_track&, IndexBulkDeleteResult*, ...)`（`bm25_inverted_list.cpp:1024`）按"被删 doc 集合"在 compaction 时物理清理 postings——这就是 deletion vector 的物理回收路径。
- `InvertedListPageData::DELETE_FLAG`（`bm25_inverted_list.h:54`）= 页内条目删除标记。
→ deletion 向量 + 延迟物理回收，**团队已写过、内核已有**，Level 1 是把它从 BM25 内推广为段级通用机制。

---

## ③ 内嵌倒排（中文 jieba + FST term + 分块 postings）嵌进段、与列共存

### 【加速搜索】

**原理**：把倒排索引作为段内的"几列"，与数据列同段、同生命周期、同剪枝（对标研究 line 149-154 SmithDB 嵌入 Vortex 的倒排）。

### 结构（三件套，全部有内核先例）
1. **中文分词 = 复用既有 vex_jieba**【加速搜索】
   段构建时对文本列(`input_text`/`output_text`/`name`)调既有 jieba：`dict_jieba.cpp` 通过 TS 模板 `TSTemplateJiebaId`(=3884) 暴露 `Jieba` 对象做 cut/lexize（`dict_jieba.cpp:43/224`），`bm25_tokenize`(OID 4528) 已内置。**这是 SmithDB 的空白（无中文分词）、我们的差异化点**（研究 line 217/425）。领域词典走 `vexjieba_add_userdict`/`vexjieba_reload`（Level 0 §9 已用）。

2. **term 列用 FST**【加速搜索 + 根治膨胀】
   分词去重后的 term 集合，构建 **FST(有限状态转换器)**：term→postings 块号。FST 在压缩字节上直接做精确查找 / 前缀扫描 / 自动机遍历，实测压缩比极高（研究 line 154：88.8 MiB→3.8 KiB）。
   - 复用/演进：BM25 现用 `DiskHashTable<Token, TokenIndexEntry,...>`（`bm25_token_index.h:48-57`，按词长分 short/mid/long/full 四桶）做 term→entry。Level 1 段是**不可变**的——不可变正好让 FST 可行（FST 要求构建期一次性排序，不支持增量插入；段封盖即一次性建 FST）。诚实标注：FST 是新增组件（内核现为 hashtable 不是 FST），需自研，但"term-sorted + 一次性构建"在不可变段上是顺水推舟。一期可先用现成 `DiskHashTable` 顶上，二期换 FST 拿压缩比 + 前缀扫描。

3. **分块 postings（128 元素块 + 变长 bitwidth delta）**【加速搜索 + 根治膨胀】
   每个 term 的 postings(doc-id 列表) 按 128 个一块、块内 doc-id 做 delta + 每块最小 bitwidth 打包（高频词低至 3-4 bit/doc，研究 line 154）。块头存 skip 信息以跳块。
   - 复用：BM25 `InvertedListPageData` 已有 **skip pointer 多级结构**(`InvertedListSkipPointers`，`bm25_inverted_list.h:33-39`)、按 postings 长度分级(`il_threshold_levels={4,32,162}`，line 41)、`try_upgrade/try_downgrade` 在级间迁移(`bm25_inverted_list.cpp:1376/1498`)。分块 + skip + 分级 postings **团队已写过**。

### 与列共存的关键：doc-id 直接 = 段内行号【加速搜索】
postings 里的 doc-id **直接是段内物理行号**，省掉"segment-local id ↔ 行位置"翻译表（对标研究 line 154）。于是：FST 查到 term → postings 给出行号集 → 直接用行号 O(1) 回列区取任意列（§① 的随机读）→ 直接喂 §② 折叠。**全文命中 → 取列 → 折叠当前态，全在段内闭环，零跨结构翻译。** 倒排区与列区共享同一 zone-map：先 zone min/max/count 剪枝，再走 FST（研究 line 154）。

> 对比 BM25 现状：BM25 doc_store 需要 `doc_id↔tid` 翻译表（`bm25_doc_store.h:58`），因为它是建在可变堆表上的独立索引；Level 1 段是不可变 + 自描述，行号即 doc-id，这层翻译被消掉——这是"内嵌"相对"旁挂索引"的实质收益。

---

## ④ trace 树：区间编码 [pre,post] 物化进段、支持子树范围扫 + 线程重建

### 【加速搜索】

**原理**：段封盖时一次性把树结构物化成三列 `pre/post/lvl`，随段不可变存储；子树 = 一个区间范围扫，命中 zone-map（对标 Level 0 §5 双编码，但把"读侧区间"下沉进段）。

### 物化进段的布局
- 段构建期对段内 span 做**一次性 O(n) DFS**（Level 0 §5 已定的应用层 DFS，这里在段构建线程内做），产出每行 `(pre, post, lvl)`。
- `pre`/`post` 作为列存进段，用 **delta+bitpack 编码**（DFS 序天然近单调，压缩比高）。
- **关键设计：段内按 `pre` 物理排序行**（段不可变 → 可在封盖时自由重排行序，这是不可变段相对可变表的又一红利）。于是"子树扫" = 段内一段**连续行区间** `[pre_root, post_root]` = 连续 zone 扫，命中 zone-map 的 `pre` min/max 直接定位起止 zone。

### 子树范围扫算法
```
1. 由 span_id 定位根行，读其 (pre_root, post_root)。
2. 子树 = 所有满足 pre ∈ [pre_root, post_root] 的行(区间编码不变式)。
3. 因段内按 pre 排序：用 zone-map 的 pre min/max 二分定位起止 zone → 连续扫这些 zone。
4. 扫出的行直接是 DFS 先序(=树展示序)，无需再排序。
5. 每行过 deletion vector，喂 §② 折叠取当前态。
```
这比 Level 0 `ix_cur_subtree (tenant,trace,pre) BETWEEN` 的二级索引更省：段内连续行 + zone-map，**顺序 I/O，不走索引随机回表**。

### 线程重建
`thread_id` 作为列 + zone-map；线程视图 = `WHERE thread_id=? ORDER BY start_time`，段内 `thread_id` 用字典/RLE 编码（同线程行常聚集），zone-map 剪枝后顺序解码。与子树扫共用段内随机读能力。

### 跨段 / 晚到的树正确性
- 一个 trace 可能跨多个段（晚到子节点落新段）。**段内区间编码只在段内自洽**；跨段子树查询时，以"逻辑当前态"为准：先 §② 折叠出该 trace 的全部当前 span（合并各段候选 + deletion），再在内存里按 `parent_span_id`(写侧邻接，永远正确) 重算 pre/post 展示——即段内物化是**快路径**（trace 完整落单段、已封盖时直接区间扫），跨段/未稳定时退化到邻接重建（Level 0 §5 的 `dotted_order` 抗晚到全序仍作兜底）。诚实标注:跨段树物化的一致性规则需在 PoC 用真实乱序 trace 验证。

---

## ⑤ 向量：复用 DiskANN 还是嵌段

### 结论：**主路径复用 DiskANN（旁挂），段内只做 flat 兜底**【加速搜索】

理由（基于查实）：
- DiskANN 已是**成熟存量产品**，且自带"自管段文件 + 独立 buffer 池"模式（`vector_smgr.h`、`disk_store.cpp` 的 `DiskVector<AnnNeighbors>/<DiskAnnVamanaNode>` + PQ 压缩 + `idx_diskann_inplace_filter` 带过滤 ANN）。重写一个段内图索引是重复造轮子且召回风险高。
- DiskANN 的图结构**不适合频繁不可变小段**：HNSW/Vamana 图要全局连通性，每段一张小图会割裂图、毁召回。
- **采样降规模**(Level 0 §8：只对 root/LLM/error span 建 embedding)让向量集足够小，**单一全局 DiskANN 索引**即可，不需要按段切。

落地分工：
| 层 | 做法 | 标注 |
|---|---|---|
| 主向量索引 | 全局一个 DiskANN（复用 `USING diskann (embedding, tenant_id, span_kind)` inplace-filter），库随 trace 增量 insert | 复用既有 K，零新内核 |
| 段内 flat 兜底 | 段可选存"本段采样 span 的原始向量 + zone-map"，用于：① 高选择度过滤后候选 < 阈值时段内暴力精排(recall=100%，Level 0 §8 已定)；② 段自包含、可独立迁移/校验 | 用 `VarDiskVector<float>` 存，新增轻量 |
| merge-on-read 衔接 | DiskANN 命中 span_id → 经 upgrade vector 指到最新段行号 → §② 折叠取当前态 | 复用 §② |

→ **不嵌全图，复用 DiskANN；段内仅 flat 向量做精排兜底与自包含**。这避免了"为不可变小段重做 ANN 图"的召回灾难，又保留段自描述性。

---

## ⑥ LSM 写路径（memtable → 段 → 时间分层 compaction）+ 写放大控制

### 【根治膨胀】（写放大是膨胀的另一面）

对标研究 line 118-128/388-410 的"内存缓冲写 → flush 不可变 sorted batch → time-tiered compaction"。

### 写路径三级
```
L0  memtable(内存)：新事件先进内存有序结构(按 span_id, seq, ts)。
     → 满阈值/定时 flush，封成 L1 不可变小段(TraceSeg)，带 zone-map + 倒排 + 树。
L1  近期小段：高频乱序晚到都落这里(append-only)，不 compaction(还在等 end/feedback)。
L2+ 时间分层 compaction：老段(时间稳定、已无新事件)合并成大段，
     合并时应用 deletion vector(真删行)、upgrade vector(只留最新版本)、TTL 过期。
```

### 写放大控制（四个杠杆，全部有依据）
1. **时间分层（time-tiered），不是大小分层**【根治膨胀】
   近期段**不压实**（研究 line 83/389/410：新数据还会再收 end/feedback，过早压成大文件 = 重复重写 = 写放大）；只对"时间已稳定、不再变"的老段压实成大段。trace 数据"写一次后短期内补几次、之后永久只读"的特性，让时间分层天然低写放大。
2. **memtable 批量封段**【根治膨胀】
   不是来一条写一条到段，而是内存攒一批、排好序、一次性封成压缩好的不可变段——写放大 = 1 次顺序写，无 in-place 更新（对比 Level 0 ASTORE 折叠 UPDATE 产死元组要 vacuum，这是任务点 1 的根治）。
3. **zero-copy compaction（搬压缩字节不解压）**【根治膨胀】
   compaction 合并段时，对**未被 deletion 命中、编码相同**的 zone，直接搬运压缩字节、不解码再编码（对标研究 line 105 Microsoft 在 Iceberg 的 "zero-copy LSM compaction"）。只有被删行所在 zone 才需解开重写。大幅降 compaction 期写放大与 CPU。诚实标注：需 zone 编码兼容判定逻辑，二期优化。
4. **WAL 复用既有机制**【根治膨胀】
   段写入走既有 `LogManager`/`diskann_*xlog`（`diskvector.hpp` 的 `_logmgr.diskann_extend_newpages`/`diskann_xlog_add_elem`，line 179/244），不自造 WAL。memtable 用一条轻量 redo 保护，崩溃后从最后封盖段 + WAL 重放 memtable。

### compaction 与 deletion/upgrade 的闭环
compaction = §② 的 deletion/upgrade 向量的**物理兑现点**：读路径只逻辑标记（廉价），compaction 才真删、真合并、真回收空间——这与 BM25 `InvertedList::vacuum` 走 `doc_id_track` 物理清理(`bm25_inverted_list.cpp:1024`)完全同构，团队已实现过该模式。

---

## 三处根治 ↔ 设计点 对照（收口）

| Level 0 扛不住的 | Level 1 根治手段 | 对应节 |
|---|---|---|
| **膨胀**：折叠/冻结的原地 UPDATE 产死元组、要 vacuum | 不可变段 + merge-on-read（逻辑删/更不改段）+ memtable 批量封段 + 时间分层 compaction + zero-copy 搬字节 | ②⑥ |
| **压缩 XOR 可检索 二选一** | 一个段同时：列式可插拔编码(压缩) + zone O(1) 随机读 + 内嵌 FST 倒排 + zone-map 剪枝 + 段内向量 flat —— 压缩与检索同段共存 | ①③④⑤ |
| **query-time fold 应用层开销** | 折叠下沉为段读路径多路归并算子（O(n log k)），不再走 SQL MERGE/应用层全表聚合 | ② |

---

## 内核可行性（已查实，决定能不能建）

- **Table-AM 封闭**（`tableam.h` `GetTableAmRoutine` 硬编码 `TAM_HEAP/TAM_USTORE` 二选一，`tupdesc.h` `NUM_TABLE_AM=2`，无 `CREATE ACCESS METHOD`、无 `amtype/amhandler`，见 appendix-K）→ **不把 TraceSeg 做成可注册表引擎**（那是内核 fork 级重改）。
- **Index-AM 开放 + 自管段文件先例**：DiskANN 正是"自定义存储 backed 的 index AM"——`vector_smgr` 独立 FORK + `vector_buffers` 独立 buffer 池(`vector_smgr.h`)、`disk_container::DiskVector`/`DiskHashTable`/`BlockMgr` 块级容器(`diskvector.hpp`/`blockmgr.hpp`)、BM25 `InvertedList` 分块 postings + skip + vacuum + upgrade(`bm25_inverted_list.*`)。**Level 1 循 DiskANN 模式：把 TraceSeg 实现为一个 index AM（如 `USING traceseg`），用独立 FORK 自管段文件**，绕开封闭 Table-AM。这条路团队已走通两次（DiskANN/BM25）。
- **复用清单（已读代码确认存在）**：`DiskVector`/`VarDiskVector`（块级 WAL POD 容器，几何扩页，O(1) 行号定位）、`DiskHashTable`（term 索引）、`BlockMgr`（FORK 感知扩页）、`InvertedList`（分块 postings/skip/vacuum/upgrade/downgrade）、`vector_smgr`（独立 FORK + buffer 池 + 刷盘线程）、`LogManager`（WAL）、`vex_jieba`/`bm25_tokenize`（中文分词）、DiskANN（向量主索引）。
- **需自研/移植（诚实标注未验证项）**：① FSST/ALP 编码器（内核现无；一期 plain/delta/RLE/dict 兜底，二期补）；② FST term 容器（现为 `DiskHashTable`，不可变段上一次性建 FST 可行但是新组件）；③ 段读路径多路归并折叠算子；④ zone-map 与可插拔编码框架；⑤ zero-copy compaction 的 zone 兼容判定；⑥ 跨段树物化一致性规则。这些都在 Index-AM + `disk_container` 框架内，不触 Table-AM。

---

## 诚实标注（未亲自验证 / 推断项）
- 以上为**静态源码核实**（已读 `diskvector.hpp`/`blockmgr.hpp`/`vector_smgr.h`/`bulkbuf_smgr.h`/`disk_store.*`/`bm25_inverted_list.h`/`bm25_token_index.h`/`bm25_doc_store.h` 及 grep 确认 `vacuum`/`try_upgrade`/`DELETE_FLAG`/`doc_id_track`/jieba `TSTemplateJiebaId`），**未编译/未跑 PoC**。
- `doc_id_track` 结构体定义未在 `include/access/bm25/` grep 到（可能在 .cpp 或 struct 头），但其作为 `vacuum` 入参的"被删 doc 集合"语义由签名与调用点可确证；具体字段需读实现确认。
- FSST/ALP/FST 在本内核树**无现成实现**（grep 研究底稿确认是 Vortex/SmithDB 的算法，非本仓代码），列为自研/移植项，不可声称"复用既有"。
- SmithDB/Vortex 的性能宣称（100x 随机读、zero-copy compaction、FST 88.8MiB→3.8KiB）来自研究底稿引用的官方/二手来源（line 98-105/154），属对标参照，非本设计实测。
- 段内"按 pre 物理排序行"与"DFS 序近单调利于 delta 编码"是基于不可变段可自由重排的推断，需 PoC 验证排序成本 vs 子树扫收益。

**关键文件路径（均绝对路径）**
- 复用容器：`/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/include/templates/vtl/disk_container/{diskvector,blockmgr,diskarray,disk_hashtable,freespace}.hpp`
- 自管段文件先例：`/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/include/access/annvector/store/{vector_smgr.h,bulkbuf_smgr.h,smgr.md}`、`/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/gausskernel/storage/access/diskann/storage_interface/disk_store.cpp`、`/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/include/access/diskann/storage_interface/disk_store.h`
- 倒排/postings/deletion 先例：`/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/include/access/bm25/{bm25_inverted_list.h,bm25_token_index.h,bm25_doc_store.h}`、`/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/gausskernel/storage/access/bm25/bm25_inverted_list.cpp`
- 中文分词：`/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/gausskernel/storage/access/bm25/tokenizer/dict_jieba.cpp`
- 内核边界（Table-AM 封闭 / Index-AM 开放）：`/Users/Four/JobProjects/yitrace/vex-x/docs/design/appendix-K_kernel-boundary.md`
- Level 0 schema（被 Level 1 根治的对象）：`/Users/Four/JobProjects/yitrace/vex-x/docs/design/2026-06-16_tracevault-schema.md`
- SmithDB/Vortex 对标底稿：`/Users/Four/JobProjects/yitrace/vex-x/docs/research/2026-06-16_smithdb-and-landscape-research.md`（line 83-154 存储/倒排、388-410 LSM/compaction）