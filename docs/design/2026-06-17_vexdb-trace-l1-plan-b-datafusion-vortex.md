# yiTrace Level-1 设计 · 方案 B:DataFusion + Vortex(最接近 SmithDB)

> 单机、本地 NVMe、专用 trace 数据库引擎。Rust 自研引擎,复用 DataFusion(SQL/向量化执行)+ Vortex(列式不可变段)+ 自建 LSM / merge-on-read / 中文倒排 / 区间树 / 带过滤 ANN。
> 目标客户:中小客户,< 1 亿 span/天。
> 约束:抛弃信创、可抛弃 openGauss/yiTrace 行存底座,从零自研 Rust 引擎。
> 文档日期:2026-06-17。本文所有外部成熟度判断均带"诚实标注",见 §15。

---

## 0. 结论先行(TL;DR)

- **能做,且比 fork openGauss 干净得多。** 自建 Rust 引擎拥有完整查询执行(DataFusion `TableProvider`/`ExecutionPlan` 直供列给上层),openGauss 那两处 CRITICAL(amgettuple 只吐 TID 必须回堆判可见性、必须 fork PG9.2.4 内核+新 RM+bootstrap pg_am+信创回炉)**在自建引擎里根本不存在**(详见 §13)。
- **方案 B 的真实成本不在 Vortex 文件格式,而在 Vortex 的"库 API"和"我们要自己写的那一层倒排/merge/folding"。**
  - Vortex **文件格式**自 0.36.0 起向后兼容稳定;但 **forward-compat(老库读新文件)1.0 明确不做**,**Rust/DuckDB/DataFusion 等库绑定明确不打 semver 1.0、持续高频发布**(可能 breaking)。对"长寿命私有化部署数据"这是真实风险,需用"格式版本固化 + 自带读路径 + 可重写迁移"对冲(§15)。
  - SmithDB 的"段内嵌倒排"**是它自己在 Vortex 之上写的**(FST 词典 + 128-doc 分块 bitpacked-delta postings + zoned layout 做 pruning;Vortex 把 postings 当**不透明二进制 blob** 存,不解码成 Arrow)。**Vortex 不提供倒排索引**——它给的是"可插拔编码 + zone-map + 嵌套 layout"这套底座。所以"段内嵌倒排在 Vortex 上要自研多少" = **几乎全部自研**(约 8–11 人月,§14)。
  - SmithDB 公开博客**只讲了对象存储版倒排的 V2 layout**,**没讲 CJK 分词、merge-on-read、deletion vector、query-time folding**——这四块在本设计里**全是我们自己的工程量**,不能假装抄现成。
- **总工程量 18–26 人月**到达可私有化交付的 L1(单实例),团队 Rust 储备(bm25_benchmark Rust + DiskANN C/C++)够用但 Vortex/DataFusion 学习曲线 + 倒排自研是主风险(§14)。
- **单机容量天花板:** 单实例舒适区 **≤ 1 亿 span/天、热数据 7–30 天、NVMe 4–8 TB**;超过需多实例分片(L2,不在本设计)。量化 SKU 见 §11。

---

## 1. 系统总览

### 1.1 架构分层

```
┌──────────────────────────────────────────────────────────────────┐
│  接入层:OTLP gRPC/HTTP · LangSmith 兼容 REST · SQL(DataFusion)     │
├──────────────────────────────────────────────────────────────────┤
│  查询层:DataFusion(SQL 解析/逻辑计划/向量化执行)                  │
│     ├─ TraceTableProvider(自定义,直供列,不走 TID 回堆)           │
│     ├─ MergeOnReadExec(多版本 fold 多路归并算子,自定义 ExecPlan)  │
│     ├─ InvertedScanExec(中文倒排 → row selection 下推)            │
│     ├─ AnnScanExec(带过滤 ANN → row selection)                    │
│     └─ IntervalScanExec(区间树 → 活/重叠 span 选择)               │
├──────────────────────────────────────────────────────────────────┤
│  存储引擎(自建,本设计核心):                                      │
│     ├─ WAL(写前日志,group commit)                                │
│     ├─ MemTable(活跃可变,行→列 microbatch)                       │
│     ├─ ImmutableSegment(Vortex 段:列 + zone-map + 嵌倒排 + 向量)  │
│     ├─ Manifest(段元数据 + deletion/upgrade vector + LSM level)    │
│     ├─ IntervalTree(活 trace / 时间重叠,内存 + 段级摘要)          │
│     └─ Compactor(时间分层 merge,生成新段、合并 delete vector)     │
├──────────────────────────────────────────────────────────────────┤
│  底座:object_store(本地 NVMe / 可选 MinIO)· RocksDB(可选,存     │
│        Manifest/小元数据)· tokio                                    │
└──────────────────────────────────────────────────────────────────┘
```

### 1.2 设计原则

1. **不可变段 + merge-on-read**:写入永不原地改;UPDATE/DELETE 走 deletion/upgrade vector,query time fold。根治膨胀(§12.1)。
2. **同字节既压缩又可检索**:列用 Vortex 可插拔编码压缩;倒排/zone-map 与列共置于同一段文件,随机读不需全解压。根治"压缩 XOR 可检索"(§12.2)。
3. **fold 是读算子,不是后台 job**:把 OTLP/LangSmith 同 span 多次上报(start→end→补属性)的多版本,在读路径多路归并成单一逻辑事件(§12.3 / §6)。
4. **诚实单机**:不假装分布式,明确标 SKU 天花板;但段格式 + object_store 抽象为未来 L2 横向扩展留口子(不是本设计承诺)。

---

## 2. 数据模型

### 2.1 逻辑模型(trace / span / event)

一条 span 是核心记录。字段对齐 OTLP + LangSmith 超集:

| 字段 | 类型 | 说明 | 检索方式 |
|---|---|---|---|
| `trace_id` | `FixedBinary(16)` | trace 标识 | 等值/zone-map/可做次级哈希 |
| `span_id` | `FixedBinary(8)` | span 标识 | 等值 |
| `parent_span_id` | `FixedBinary(8)` nullable | 父 span | 等值(组装调用树) |
| `start_time_unix_nano` | `Int64` | 排序键主分量 | zone-map / 区间树 |
| `end_time_unix_nano` | `Int64` | | 区间树 |
| `duration_nano` | `Int64`(派生) | | zone-map / 范围 |
| `service_name` | `Dict(Utf8)` | 低基数 | 倒排 / 等值 |
| `span_name` | `Dict(Utf8)` | | 倒排 |
| `span_kind` | `UInt8` | | 等值 |
| `status_code` | `UInt8` | | 等值 |
| `attributes` | 见 §2.2 | 半结构化 KV | 倒排(路径+值) |
| `resource` | 同上 | 资源属性 | 倒排 |
| `events[]` | `List<Struct>` | span 内事件(log) | 文本倒排 |
| `body_text` | `Utf8`(大文本,LLM prompt/输出) | LangSmith trace 重点 | 中文倒排(全文) |
| `embedding` | `FixedSizeList<Float32, D>` nullable | 语义召回向量 | 带过滤 ANN |
| `_version_ts` | `Int64`(摄入序) | merge-on-read 版本仲裁 | 内部 |
| `_seq` | `UInt64`(全局单调) | WAL/段内行序 | 内部 |

> **半结构化 attributes 的关键决策**:OTLP/LangSmith 的 attributes 是动态 KV,基数高、稀疏。**不**做"每 key 一物理列"(列爆炸→膨胀)。采用 **JSON-path 倒排 + 值列分类型存** 的混合(§5.2),对齐 SmithDB"546 个 JSON path → FST 仅 3.8 KiB"的做法。

### 2.2 attributes 物理化策略(防列爆炸)

- 高频固定字段(`service_name`/`span_name`/`status`/`http.method` 等白名单)→ 提升为**物化列**(Dict 编码,zone-map,可直接 DataFusion 谓词下推)。
- 其余动态 KV → 两路存:
  1. **倒排路**:把 `path` 与 `path=value` 作为 term 进倒排(存在性 + 值匹配)。
  2. **取值路**:按值类型分 3 个稀疏列(`attr_str`/`attr_f64`/`attr_i64`)+ 一列 `attr_path_ord`(指向 FST ordinal),用于投影出具体值。
- **代价诚实**:动态值的"范围查询"(如 `attr.latency > 100` 且 latency 不是白名单)只能 zone-map 粗筛 + 行级过滤,不如物化列快。提供"运行时把某 path 提升为物化列"的 DDL(下次 compaction 生效),把热 path 升级为一等列。

---

## 3. 段格式与磁盘布局

### 3.1 一个 Segment = 一个 Vortex 文件 + 一份段尾自定义 footer

我们**不 fork Vortex 文件格式**(否则丢掉它的稳定性承诺)。我们用 **Vortex 的合法扩展点**:
- 列数据 → Vortex `ChunkedLayout`(2 MB 未压缩分块)+ `BufferedLayout`(≤1 MB 压缩块本地化)。
- 每列统计 → Vortex `ZonedLayout`(每 8k 行一组 min/max/count zone-map,Vortex 原生)。
- **倒排 / 向量索引 / 区间摘要** → 作为**额外的 Vortex "binary blob 列 + 独立 layout"** 写在同一文件里。Vortex 把它们当不透明字节存(像 SmithDB 那样:"Vortex sees the encoded bytes as a single binary blob; it never decodes them into Arrow"),我们自己的读路径解码。

```
segment_000123.vx
┌────────────────────────────────────────────────────────────┐
│ [Vortex 文件主体]                                            │
│   列区:trace_id, span_id, start_time, ..., body_text,       │
│         attr_str/f64/i64, attr_path_ord, embedding           │
│     每列:ChunkedLayout(2MB) → BufferedLayout(≤1MB 压缩块)   │
│     每列:ZonedLayout(8k 行/zone min/max/count)             │
│   索引区(Vortex binary blob 列,我方编码):                │
│     ├─ fts_fst       : 中文倒排 FST 词典(term→ordinal)     │
│     ├─ fts_postings  : 128-doc 分块 bitpacked-delta postings │
│     ├─ fts_positions : 同编码(短语/邻近用,可选)          │
│     ├─ ann_index     : HNSW/IVF 序列化图(段内自包含)       │
│     └─ interval_sumy : 段级 [min_start, max_end] + 稀疏摘要  │
├────────────────────────────────────────────────────────────┤
│ [自定义 footer(段尾,4KB 对齐)]                            │
│   magic "VXTRACE\0" · format_version(u16) · vortex_ver(u16) │
│   row_count · time_range[min_start,max_end] · lsm_level     │
│   各索引 blob 的 (offset,len,codec_id,checksum)              │
│   schema_hash · created_seq · crc32(footer)                 │
└────────────────────────────────────────────────────────────┘
```

> **为什么 footer 自带 `format_version` 和 `vortex_ver`**:对冲 Vortex forward-compat 不做的风险(§15)。读时先看版本,版本不认识→走对应解码器或触发"重写迁移"。

### 3.2 一个 trace 库 = 段集合 + Manifest

```
data/
  wal/                 写前日志(分段轮转)
  segments/
    L0/ seg_*.vx        新刷出的小段(memtable flush,可能含同 span 多版本)
    L1/ seg_*.vx        小时级 compaction 结果
    L2/ seg_*.vx        天级 compaction 结果(冷,强压缩)
  manifest/            段元数据 + deletion vector + upgrade vector(RocksDB 或自写 MVCC manifest 文件)
  meta.db              可选 RocksDB:path 字典、物化列白名单、schema 演进
```

- **排序**:段内行按 `(trace_id, start_time, _version_ts)` 排序 → 同 trace 行物理聚集(folding 时归并友好);跨段按 `time_range` 时间分层。
- **deletion vector**:每段一个 roaring bitmap(段内行号→已删),存 Manifest;读时跳过。免原地删 → 根治删后空洞膨胀。
- **upgrade vector**:记录"段内某行被后续新版本替代",指向新版本所在段/行(或仅标记 obsolete,新版本另存)。merge-on-read 时用它做版本仲裁。

---

## 4. LSM 写路径

### 4.1 写入流水线

```
摄入(OTLP/LangSmith)→ 规范化为 SpanRecord
   → 1) 追加 WAL(group commit,fsync 批量)        [崩溃恢复点]
   → 2) 写入 MemTable(按 trace_id 分桶的可变行缓冲)
   → 3) 同步更新内存 IntervalTree(活 trace 用,§7)
   → 4) MemTable 达阈值(行数/字节/时间)→ flush:
          行 → Arrow RecordBatch → Vortex 编码 → 写 L0 段
          同时构建该段的:zone-map(Vortex)、中文倒排、ANN 子图、interval 摘要
   → 5) 原子提交段到 Manifest(bump manifest version)
   → 6) 截断已落段的 WAL
```

### 4.2 MemTable 设计

- 结构:`HashMap<trace_id, Vec<SpanRecord>>` + 全局 `_seq` 单调计数。同 trace 的多次上报落在同桶,flush 时天然聚集。
- 双 buffer:`active` 接新写,达阈值切 `immutable` 后台 flush,写入不阻塞。
- 阈值(可调):每段目标 **64–256 MB 未压缩 / 50–200 万行 / 或 30s 强制**(活 trace 可见性)。

### 4.3 flush 即建段内全部索引(关键:压缩与可检索同生)

flush 时**一次遍历**生成:列(Vortex 压缩)+ zone-map + 倒排 + 向量子图 + 区间摘要,全部写进同一 `.vx` 文件。**压缩和可检索在同一份字节里同时产生**(§12.2),不是"先压缩再另建索引"。

### 4.4 写放大与 L0 膨胀的诚实账

- L0 段小且可能含同 span 多版本(start 先到、end 后到分别 flush)。**L0 段数量会涨**,读路径 fan-out 变大 → 必须靠 compaction 收敛(§8)。
- 写放大主要来自 compaction 重写(§8.3 量化),不是写入本身;写入是 append-only,放大≈1×(WAL + 段)。

---

## 5. 内嵌中文倒排(自研,最大单点工程)

> **诚实前提**:Vortex 不带倒排。SmithDB 的倒排是它自己写的,且公开博客**没讲 CJK**。所以这一节是"借 SmithDB 的 layout 思路 + 自研 CJK 分词 + 自研 postings 编码"。

### 5.1 分词

- 复用 Rust 生态:`jieba-rs`(精确/搜索模式)或 `tantivy-jieba`/`cang-jie` 的分词逻辑(分词器逻辑可借,但**索引存储我们自己做进 Vortex 段**,不直接挂 tantivy 索引文件——否则就成了"两套存储",违背"同字节既压缩又可检索")。
- 中文按词 + bigram 兜底(召回未登录词);英文/标识符按 unicode + 小写归一;trace 特有:`http.path` 切片、`service.name` 按 `.`/`/` 切。

### 5.2 段内倒排结构(对齐 SmithDB,自研)

每段一份倒排,覆盖该段所有可检索文本/路径列:
- **FST 词典**:`term → ordinal`(段内序)。低基数路径压缩极好(SmithDB:546 path → 3.8 KiB)。FST 在稀疏/低基数上胜 zstd;**高基数文本列上 FST 比 zstd 大约 1.5×**(诚实:这是 SmithDB 实测的代价,我们继承)。
- **Postings**:每 term 一个 doc(段内行号)列表,**128-doc 一块 bitpacked-delta**,块内按自适应位宽,尾部 < 128 用 VInt。高频 term(如 `service=agent`)3–4 bit/doc,稀有 term 只占 VInt 尾。
- **Positions**(可选,短语/邻近查询):同 128-块 bitpacked 编码,仅文本值列建,存在性(path 存在)不建 position。
- **zone-map pruning**:倒排所在段在 footer 记 `(min_start,max_end)`;倒排内对 term 列再加 row-group min/max,使时间谓词能跳过整段/整 row-group(SmithDB:"Per-row-group min/max/count via zoned storage layout")。

### 5.3 倒排读路径

```
WHERE body_text MATCH '退款失败' AND service_name='payment'
  → 分词 ['退款','失败']
  → 段级 zone-map 按时间/段范围裁段
  → 每候选段:FST 查 term → ordinal → 解 postings(128块) → roaring bitmap
  → AND 交集 → 与 service_name 等值的 postings 再交
  → 输出 row selection(段内行号集合)→ 交给 MergeOnReadExec
```

倒排**只产出 row selection**,真正取列由 Vortex 列扫 + DataFusion 投影完成 → **不回堆、不取整行**(对比 openGauss CRITICAL ①,§13)。

### 5.4 跨段 merge-on-read 对倒排的影响

每段独立倒排;查询多段 → 各段出 bitmap → MergeOnReadExec 归并并应用 deletion vector + 版本仲裁(§6)。**倒排不跨段合并存储**(避免全局倒排重写的写放大),靠 compaction 时段合并自然收敛 fan-out。

---

## 6. merge-on-read 读路径算法(folding 多版本事件)

### 6.1 为什么需要 fold

OTLP/LangSmith 同一 span 常被**多次上报**:span start(无 end/duration)、span end(补 end + status)、后续补 attributes/反馈分。同 `(trace_id, span_id)` 因此有**多版本行**散落在多个段(尤其 L0)。读时必须 fold 成"最终逻辑 span"。

### 6.2 算法:基于排序的 k 路归并 + 版本仲裁

前提:每段内已按 `(trace_id, span_id, start_time, _version_ts)` 排序。

```
MergeOnReadExec(inputs = 候选段们的有序行流):
  1. 用最小堆按 (trace_id, span_id) 做 k 路归并(各段已序,O(N log k))
  2. 对同一 (trace_id, span_id) 的所有版本(可能跨段):
       a. 应用各段 deletion vector,丢弃已删行
       b. 按 _version_ts 升序 fold:
            - 标量字段:last-non-null wins(end/status 后到覆盖)
            - 可累加字段(若有):按语义合并
            - attributes:按 key 做 last-writer-wins 合并(map upsert)
       c. 应用 upgrade vector:若该行被标 obsolete 且有指向,跳过旧、采新
  3. 输出单一 folded 行 → 上抛给 DataFusion 上层算子
```

### 6.3 与 row selection 的协同

倒排/ANN/区间树先给 row selection(候选行号),但 **fold 必须在"同 span 的全部版本可见"前提下做**——否则可能命中旧版本、漏新版本。处理:
- row selection 命中某 `(trace_id,span_id)` 的任一版本 → **把该 span 的所有版本拉进归并**(用段内排序 + trace_id 聚集快速定位邻近版本),fold 后再对 folded 结果复评谓词(后置过滤)。
- 这避免"命中半成品版本"。代价:命中行的邻域版本要多读一点,但因同 trace 物理聚集,代价有界。

### 6.4 正确性边界(诚实)

- **跨段 fold 的版本完整性依赖"所有相关段都进了候选集"**。若用时间谓词裁段,而某 span 的 end 版本时间戳落在裁掉的段 → 可能漏。对策:fold 的版本仲裁按 `(trace_id,span_id)` 而非时间;裁段只裁"start_time 范围",同时段尾记录"本段含哪些 trace_id 的后续补写"(布隆过滤器),防误裁。这是**真实复杂度**,不是免费午餐。

---

## 7. 区间树(活 trace / 时间重叠)

### 7.1 用途

- "当前还在进行的 trace"(活 trace):`end_time IS NULL OR end_time > now-ε`。
- 时间重叠查询:"找与时间窗 [t1,t2] 重叠的 span"(甘特图、并发分析)。

### 7.2 两级结构

1. **内存区间树**(活跃数据):MemTable + L0 的 span 进内存 augmented interval tree(红黑树 + 子树 max-end),支持 stabbing/overlap 查询 O(log n + k)。仅覆盖"近窗"数据(活 trace 必然新),内存可控。
2. **段级摘要**(历史数据):每段 footer 存 `[min_start, max_end]` + 段内稀疏 interval 摘要(每 8k 行块的 max_end)。overlap 查询先用段范围裁段,再用块 max_end 裁块,块内行级判定。

### 7.3 活 trace 实现

- span start 到达 → 进 MemTable + 内存区间树(end=NULL)。
- "活 trace 列表" = 内存区间树里 end=NULL 的根集合(秒级新鲜)。
- span end 到达 → fold(§6)补 end,从"活"集合移除(逻辑上,物理仍 append)。
- 超时未收 end 的 → 后台扫描标记 `status=timeout`(写新版本,merge-on-read 生效)。

---

## 8. 时间分层 Compaction(写放大)

### 8.1 分层策略(时间为主键,非纯 size-tiered)

- **L0**:memtable flush 直出,小段、可能多版本、时间区间重叠大。
- **L0→L1(小时级)**:把同一小时窗的 L0 段 merge:k 路归并 + **fold 同 span 多版本(物化 fold,不再 read 时 fold)** + 合并 deletion/upgrade vector + 重建倒排/zone-map/ANN。输出按时间不重叠的 L1 段。
- **L1→L2(天级)**:把一天的 L1 段合并成大段,**强压缩编码**(Vortex 更激进的 codec)、冷数据 ANN 用 IVF(省内存)而非 HNSW。
- **保留/TTL**:超过保留期的 L2 段直接整段删除(物理回收),不需逐行删。

### 8.2 为什么时间分层适配 trace

trace 数据**近写近查、按时间老化**。时间分层让"删旧"= 删整段(零写放大回收),让查询天然按时间裁段。

### 8.3 写放大量化(诚实)

设每天 1 亿 span,单 span ~1.5 KB 原始 → 压缩后段 ~150 GB/天前压、压缩比 ~6–10× → 落盘 ~15–25 GB/天。

| 阶段 | 重写次数 | 累计写放大 |
|---|---|---|
| 写入(WAL+L0) | 1× | 1× |
| L0→L1 | 每行约 1 次 | +1× |
| L1→L2 | 每行约 1 次 | +1× |
| **合计** | | **~3× 写放大** |

- 对比通用 size-tiered LSM(常 10–30× 写放大),时间分层把放大压到 ~3×,因为**只在固定的小时/天边界各合并一次,不反复 merge**。
- NVMe 写耐久:3× × 20 GB/天 ≈ 60 GB/天 写入,远低于企业 NVMe DWPD,无忧。

### 8.4 fold 双重位置(诚实权衡)

- **read 时 fold**(§6):活/新数据,版本散落,必须 read 时做。
- **compaction 时 fold(物化)**:冷数据在 L0→L1 已 fold 定型,read 时几乎单版本 → 读快。
- 代价:compaction 是 CPU/IO 密集后台任务,需限流(token bucket)避免抢占查询。

---

## 9. 向量带过滤 ANN

### 9.1 段内自包含 ANN 索引

- 每段为有 `embedding` 的行建段内 ANN 图,序列化为段内 blob 列(§3.1 `ann_index`)。
- **热段(L0/L1)用 HNSW**(高召回、增量友好);**冷段(L2)用 IVF/IVF-PQ**(省内存、批量构建)。可移植团队 DiskANN C/C++ IP 成 Rust(冷段磁盘驻留场景,L2 大段适配 DiskANN 的磁盘图)。

### 9.2 带过滤 ANN(filtered ANN)的真实难点

朴素"先 ANN 再过滤"在高选择性过滤下召回崩塌(top-k 全被过滤掉)。策略:
- **pre-filter + ANN**:谓词选择性高(命中行少)→ 先用倒排/zone-map 得 row selection,再在该子集上**暴力/小图** KNN(子集小,暴力可接受)。
- **post-filter 放大 k**:谓词选择性低 → ANN 取 `k' = k × 放大因子`,再过滤,直到够 k(有上限,避免退化)。
- **段内 ANN + 过滤位图下推**:HNSW 搜索时传入 allowed bitmap(来自倒排/zone-map),遍历时跳过不允许节点(类似 filtered-HNSW)。这是**自研工作量**,Rust HNSW 库不一定现成支持,需改造。
- 跨段:各段出 top-k' → 全局归并取 top-k → 对 folded 行复评。

### 9.3 诚实标注

- filtered ANN 在"过滤选择性中等(1%–20%)"区间最难,三策略都需调参;没有银弹。
- 段内 ANN 随段数增加 fan-out 上升;compaction 合并段同时合并向量图(HNSW 合并即重建,有成本)。

---

## 10. 崩溃恢复(WAL)

### 10.1 WAL 设计

- **写前**:每条/每批 SpanRecord 先序列化进 WAL,再进 MemTable。
- **格式**:`[len][crc32][seq][payload]` 帧;按大小轮转(如 128 MB/文件)。
- **group commit**:多写聚批 fsync,降低 fsync 次数(吞吐关键)。可配 `sync=always|batch|interval`,中小客户默认 batch(几 ms 窗口)。
- **截断**:段成功提交到 Manifest 后,截断对应 WAL 区间。

### 10.2 恢复流程

```
启动:
  1. 读 Manifest → 确定已持久化段集合 + 最后提交的 seq(checkpoint)
  2. 重放 WAL 中 seq > checkpoint 的记录 → 重建 MemTable + 内存区间树
  3. 校验最后一帧 crc;残帧(崩溃在写一半)直接丢弃(WAL 帧自描述长度+crc)
  4. 恢复完成,接受新写
```

### 10.3 一致性保证

- **段提交原子性**:段文件写完 fsync → 再原子更新 Manifest(Manifest 用 RocksDB WriteBatch 或"写新 manifest + rename"原子换)。崩溃在"段写完但 Manifest 未更新"→ 段成孤儿,启动时 GC(Manifest 无引用的段文件)。
- **不会丢已 ack 写**:ack 在 WAL fsync 之后(可配)。`sync=batch` 下,极端崩溃可能丢最后几 ms 未 fsync 的写——对 trace 可观测场景可接受,且明确文档化(可调成 always 换吞吐)。

---

## 11. 单机容量天花板(量化 SKU)

> 单实例。所有数字为工程估算,需以实测校准(诚实标注:未实测)。

设单 span 原始 ~1.5 KB,压缩比 6–10×,含倒排+向量开销。

| SKU | vCPU | 内存 | NVMe | span/天 | 热数据保留 | 备注 |
|---|---|---|---|---|---|---|
| **S(嵌入式/边缘)** | 4 | 16 GB | 1 TB | ≤ 500 万 | 7 天 | 嵌入式库形态,无向量或小向量 |
| **M(中小主力)** | 8–16 | 32–64 GB | 2–4 TB | 1000 万–5000 万 | 14–30 天 | HNSW 热段,倒排全开 |
| **L(单机上限)** | 32 | 128 GB | 4–8 TB | **~1 亿(目标上限)** | 7–14 天热 + 冷段 | 接近单实例天花板 |
| **超过 L** | — | — | — | > 1 亿持续 | — | **需多实例分片(L2,本设计不含)** |

### 11.1 天花板成因(诚实)

- **内存**:HNSW 图 + 内存区间树 + MemTable + Vortex 读缓存。1 亿 span/天、热 30 天 → 30 亿 span,若 30% 带 512d 向量 → HNSW 图内存即 ~数十 GB,128 GB 是现实压力点。冷段转 IVF/DiskANN 缓解。
- **单写入流水线吞吐**:WAL fsync + 编码 + 建索引。1 亿/天 ≈ 1157 span/s 均值,峰值 5–10×。group commit + 多核编码可达,但**单实例峰值是硬上限**。
- **compaction 与查询争 IO**:大段 compaction 时查询尾延迟升高。NVMe 带宽是共享瓶颈。
- **段数 fan-out**:保留期越长、L0/L1 越多,查询 fan-out 越大。compaction 收敛 + 时间裁段是关键,但极长保留(年级)单机不现实。

### 11.2 明确不承诺

- 不承诺 > 1 亿 span/天 持续单机。
- 不承诺多年热数据全在线随机检索(冷数据应降级/归档)。
- 不承诺跨实例 HA/分布式查询(L1 单实例;HA 靠 WAL + 段文件备份/复制,RPO≈WAL 窗口)。

---

## 12. 逐条根治(膨胀 / 压缩可检索 / fold)——不回避

### 12.1 膨胀(storage amplification)

| 膨胀来源 | 传统行存(openGauss)病灶 | 本设计根治 |
|---|---|---|
| 原地 UPDATE/MVCC 旧版本堆积 | HOT/旧元组+VACUUM 追不上 | **不可变段,无原地更新**;新版本 append,旧版本读时 fold 跳过 |
| DELETE 留空洞 | dead tuple 等 VACUUM | **deletion vector bitmap**,读跳过;compaction/TTL 整段回收 |
| 同 span 多次上报多版本 | 行级多行常驻 | **compaction 物化 fold**,冷数据收敛单版本 |
| 索引膨胀 | B-tree 随机插入碎片 | 段内倒排**批量构建一次不可变**,无随机插入碎片 |
| 半结构化列爆炸 | 每 key 一列/JSONB 膨胀 | **FST path 词典 + 分类型稀疏值列**(SmithDB:546 path→3.8KiB) |
| 写放大 | size-tiered 10–30× | **时间分层 ~3×**(§8.3) |

**净效果**:无 VACUUM、无原地写、删除即跳过、TTL 整段回收 → 膨胀从根上不产生,而非靠后台清理追赶。

### 12.2 压缩 XOR 可检索 → 同字节既压缩又可检索

- 列用 Vortex 可插拔编码压缩(FSST/bitpack/dict/...),**zone-map 与压缩列同生**(Vortex ZonedLayout),谓词在压缩态 pruning,不需全解压(DuckDB-Vortex:"compute expressions on compressed data")。
- 倒排 postings 自身 bitpacked-delta(压缩态)且**可直接做集合交**(不需解回明文),FST 词典本身就是压缩态可查询结构。
- 索引与列**共置同一 `.vx` 段文件**,随机读某行某列:zone-map 定位 row-group → 解压 1MB 块 → 取值。**随机读不触发全段解压**。
- **诚实代价**:高基数文本列 FST 比 zstd 大 ~1.5×(为可检索付的空间税);position 索引可选(短语查询才付)。

### 12.3 query-time fold 下沉为读算子

- `MergeOnReadExec`(§6)是 DataFusion 自定义 `ExecutionPlan`,把多版本归并 fold 做成执行计划里的一个**多路归并算子**,在扫描之上、聚合/投影之下。
- fold 既在 read 时(活/新数据)也在 compaction 时(冷数据物化),双层把读 fold 成本压到最低。
- 与倒排/ANN/区间树 row selection 协同(§6.3),保证"不命中半成品版本"。

### 12.4 为什么这三条在方案 B 能同时成立

因为**自建引擎拥有从段格式到执行算子的全栈控制权**:段格式决定"压缩即可检索",执行层决定"fold 是算子",写路径决定"不可变防膨胀"。openGauss 扩展拿不到这三层的任意一层完整控制权(见 §13)。

---

## 13. 为什么自建 Rust 让 openGauss 那两处 CRITICAL 消失

### CRITICAL ① "amgettuple 只吐 TID,必须回堆取列判可见性"

- **openGauss 病灶**:索引扫描接口 `amgettuple` 只能返回 TID(行物理位置),引擎必须拿 TID **回堆(heap)读整行**,再做 MVCC 可见性判断 → 列式优势全失(回行)、且强依赖堆可见性机制。
- **自建引擎为何消失**:我们**拥有查询执行层**。DataFusion 的 `TableProvider::scan` 直接返回我们的 `ExecutionPlan`,该计划**直接从 Vortex 段按列产出 Arrow 批**给上层。倒排/ANN/区间树**只产出 row selection(段内行号)**,我们用行号**直接列扫投影需要的列**(列式直供),**没有"回堆取整行"这一步**,也**没有堆可见性概念**——可见性由我们的 deletion/upgrade vector + merge-on-read 在算子里决定。**根本不存在 amgettuple/TID/回堆这条路径。**

### CRITICAL ② "必须 fork PG9.2.4 内核 + 新 RM + bootstrap pg_am + 信创回炉"

- **openGauss 病灶**:基线是 PG9.2.4,**无 pluggable Table-AM**(表存储接入框架),要做列式不可变段必须 fork 内核、写新的 Resource Manager(WAL 回放扩展)、在 bootstrap 阶段注册新的 `pg_am`(访问方法),且每次内核改动都要走信创认证回炉。
- **自建引擎为何消失**:**没有 openGauss 内核**,所以没有"fork PG9.2.4""无 Table-AM""bootstrap pg_am"的任何一项。WAL 是我们自己设计的(§10),不需要 PG 的 RM 框架。**信创约束已被决策者解除**,无认证回炉。我们直接用 Rust 生态(DataFusion + Vortex + tokio)搭引擎,**自由度从"在 9.2.4 老内核里螺蛳壳做道场"变成"白纸上用现代积木搭"**。

> 一句话:**openGauss 的两处 CRITICAL 本质是"在别人的旧内核里被接口和架构卡死";自建引擎把内核换成自己的代码,这两道墙不是被绕过,而是从地基里就不存在。** 这正是决策者"解除死结"的含义。

---

## 14. 工程量(人月,对照团队 Rust 储备)

> 团队储备:写过 `bm25_benchmark`(Rust,倒排/打分有手感)+ DiskANN(C/C++,向量图有 IP)。Rust 工程化(async/tokio/DataFusion trait 体系/Vortex layout API)需爬坡。

| 模块 | 人月 | 风险/依赖 | 团队储备匹配 |
|---|---|---|---|
| 段格式 + Vortex 集成(layout/footer/版本) | 2.5–3.5 | **Vortex 库 API 不稳(高频 breaking)** | 低(新栈) |
| LSM 写路径 + MemTable + flush | 2–3 | | 中 |
| WAL + 崩溃恢复 + Manifest 原子提交 | 2–3 | 正确性敏感 | 中 |
| **中文倒排(分词+FST+postings+读路径)** | **3–4** | **几乎全自研;SmithDB 未公开 CJK** | 中高(有 bm25 手感) |
| **merge-on-read fold 算子 + 版本仲裁** | **2.5–3.5** | **正确性边界复杂(§6.4)** | 低(新概念) |
| 区间树(内存 + 段摘要 + 活 trace) | 1.5–2 | | 中 |
| 带过滤 ANN(段内 HNSW/IVF + filter 下推 + DiskANN 移植) | 3–4 | **DiskANN C→Rust 移植 + filtered-HNSW 自研** | 中高(有 DiskANN IP) |
| 时间分层 compaction + 限流 | 2–3 | 调参 | 中 |
| DataFusion 接入(TableProvider/4 个自定义 ExecPlan/谓词下推) | 2.5–3.5 | trait 体系学习曲线 | 低(新栈) |
| OTLP/LangSmith 摄入 + SQL 对外 + 嵌入式/服务双形态 | 2–3 | | 中 |
| 集成/压测/私有化打包/校准 SKU | 2–3 | | 中 |
| **合计** | **~18–26 人月** | | |

- **关键路径(并行难)**:段格式 → LSM → merge-on-read fold → 倒排,这条链强耦合,难大规模并行。
- **团队最舒服的两块**:倒排(bm25 经验)+ 向量(DiskANN IP);**最吃力两块**:Vortex/DataFusion 新栈 + fold 正确性。
- **可裁剪**:filtered ANN 的 DiskANN 移植可后置(先 HNSW 段内);position/短语索引可后置。裁后 L1 MVP ~14–18 人月。

---

## 15. 风险(诚实,不回避)

### 15.1 Vortex 成熟度 / 格式稳定性(本方案最大风险)

| 风险 | 事实(2026 调研) | 影响 | 对冲 |
|---|---|---|---|
| 文件格式向后兼容 | **已稳定(0.36.0 起向后兼容)** | 低 | 直接受益 |
| **forward-compat 老库读新文件** | **1.0 明确不做** | 中:老版本引擎读不了新版本写的段;私有化现场多版本共存麻烦 | footer 自带 `format_version`;现场统一版本;升级走"重写迁移" compaction |
| **库 API(Rust/DataFusion/DuckDB 绑定)无 semver 1.0、高频发布** | **官方明确不打 1.0、持续高频** | **高**:升级 Vortex/DataFusion 可能 breaking,牵动段格式集成与 ExecPlan | **锁版本**;封装 Vortex 调用在内部 `seg` crate 单点适配;升级走回归测试闸 |
| Vortex 不提供倒排/ANN | 确认:Vortex 只给编码+zone-map+layout,**索引全自研** | 高(工程量) | 已计入 §14;借 SmithDB layout 思路 |
| SmithDB 公开内容覆盖面 | **只讲对象存储版倒排 V2 layout;未讲 CJK/merge/deletion/folding** | 中:不能假装抄现成 | 这四块本设计自研并已列工程量 |
| Vortex 随机点查/点更新 | DuckDB-Vortex 博客未提点查优化;Vortex 偏分析扫描 | 中:trace 点查(按 trace_id)需靠我们 zone-map+排序,不是 Vortex 原生点查 | 段内 `(trace_id,...)` 排序 + zone-map + 可选次级哈希 |

> **诚实结论**:Vortex 的**文件格式**适合长寿命私有化数据(向后兼容稳),但**库 API 不稳**是持续维护税;**索引层几乎全自研**。如果团队想降低自研倒排/向量风险,**方案 A(Lance)** 值得对比——Lance 自带 IVF/HNSW + BM25 FTS + jieba/lindera 分词 + time-travel,索引不用自研,但要接受 Lance 的格式与权衡(本设计是方案 B,只在此点出对比,不展开)。

### 15.2 其它风险

| 风险 | 影响 | 对冲 |
|---|---|---|
| merge-on-read fold 正确性(跨段版本完整性,§6.4) | 高:漏/错版本=数据错 | 布隆过滤器记"段含哪些 trace 后续补写";按 (trace_id,span_id) 仲裁;充分 fuzz/property 测试 |
| filtered ANN 中等选择性区间无银弹(§9.2) | 中:召回/延迟波动 | 三策略自适应切换 + 调参 + 文档化边界 |
| 单实例硬天花板(§11) | 中:超 1 亿/天或超长保留撞墙 | 明确 SKU;段格式+object_store 为 L2 留口(不承诺) |
| Rust 团队学习曲线(DataFusion trait/Vortex layout) | 中:进度风险 | 关键路径串行、早做 spike;DataFusion 文档/社区活跃(2026 仍活跃迭代) |
| 丢 yiTrace 内核资产复用 | 中:向量/BM25 需 Rust 生态重获或移植 IP | DiskANN C→Rust 移植已计入;bm25 经验可迁移 |
| WAL `sync=batch` 极端崩溃丢最后几 ms | 低(可观测场景可接受) | 可配 `sync=always` 换吞吐;文档化 |

---

## 16. 嵌入式 vs 单机服务形态

| 维度 | 嵌入式(库) | 单机服务(daemon) |
|---|---|---|
| 形态 | Rust crate / C-ABI / Python 绑定,进程内 | 独立进程,gRPC/HTTP/SQL 端口 |
| 接入 | 宿主直接调 API 写入/查询 | OTLP collector / LangSmith SDK 指过来 |
| WAL/段 | 同进程持有 | 独立数据目录,可多客户端连 |
| 并发 | 单进程内 tokio | 多连接,连接池 |
| 适用 SKU | S(边缘/单应用 trace) | M/L(团队级可观测平台) |
| 共享代码 | **同一存储引擎 core,仅接入层不同** | 同左 |

- **同一引擎 core 双形态**:存储引擎(WAL/段/LSM/索引/merge-on-read)做成独立 crate;嵌入式直接 link,服务态在其上包 OTLP/LangSmith/SQL server。降低维护成本。

---

## 17. 对外接口

### 17.1 SQL(via DataFusion)

- 注册 `TraceTableProvider`,暴露 trace/span 逻辑表;支持标准 SQL + 自定义 UDF(`MATCH` 全文、`ANN(embedding, query, k)`、`OVERLAPS(window)`)。
- 谓词下推:时间→zone-map/段裁;全文→倒排;向量→ANN;区间→区间树。下推后产 row selection 交 merge-on-read。

### 17.2 OTLP 摄入

- gRPC `ExportTraceServiceRequest` + HTTP/protobuf;解析 ResourceSpans→ScopeSpans→Span,规范化为 SpanRecord 入 WAL。
- 同 span 多次上报天然多版本,靠 merge-on-read fold(§6)。

### 17.3 LangSmith 兼容摄入

- 兼容 LangSmith run 上报 REST(create/patch run):run start = span start,run patch(补 outputs/feedback)= 后续版本 → 同 fold 路径。
- LLM trace 的大文本(prompt/输出)进 `body_text` 中文全文倒排(§5)。

---

## 18. 落地路线建议(里程碑,非承诺)

1. **Spike(1–2 人月)**:Vortex 写/读 + zone-map + 自定义 blob 列读通;DataFusion `TableProvider` 跑通最简扫描。**验证库 API 稳定性税**(§15.1)。
2. **MVP(≈14 人月)**:WAL+LSM+不可变段+merge-on-read(read 时)+中文倒排+时间裁段+OTLP 摄入+SQL。可演示防膨胀+全文+fold。
3. **完整 L1(≈18–26 人月)**:+ compaction 物化 fold + filtered ANN(DiskANN 移植)+ 区间树活 trace + LangSmith + 嵌入式/服务双形态 + SKU 校准。

---

### 附:与 SmithDB 的异同(单机化裁剪)

| SmithDB | 本方案(单机 L1) |
|---|---|
| Vortex 段 + 自研倒排 + merge-on-read + DataFusion | **相同栈** |
| 对象存储(S3)+ 无状态计算 + cluster manager | **砍掉**;本地 NVMe + object_store 抽象 |
| 倒排为对象存储优化(大块、少往返) | **倒排为 NVMe 优化**(随机读便宜,可更细 row-group) |
| 分布式 compaction | **单机后台 compaction + 限流** |
| 横向扩展 | **单实例,明确天花板;为 L2 留口不承诺** |
