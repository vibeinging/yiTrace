# yiTrace 引擎选型决策与设计（买底座 vs 自研）

> 日期：2026-06-17｜面向：决策层 + 引擎团队
> 输入：四份代码/调研核实（BM25 移植 · graph_index 移植 · chDB/ClickHouse · DataFusion+格式）+ 两路对抗红队
> 定调前提（用户已拍，本文不再辩）：单机 · 自有 IP（BM25 中文倒排 + graph_index 带过滤 ANN）当旗舰噱头 · 中小规模 < 1 亿 span/天 · 抛弃信创约束 · 可抛弃 openGauss 行存底座 · **从零自建 Rust 引擎（路线乙，已定）** · 向量统一 graph_index（不用 DiskANN）。
> 关联：`2026-06-17_yitrace-l1-plan-b-datafusion-vortex.md`（细化稿，其 §14 的 18-26 人月表**已被本文 §3.5 废止重算**）、`2026-06-17_yitrace-l1-datafusion-lance.md`（Lance 方案，已否决见 §4）。

---

## 0. 一句话结论

**不是「就差一个列存」。** 差的是一台肯让你把自有 BM25/graph_index 挂成一等公民的查询引擎 + 一整套你必须自己写的「表层」（merge-on-read 折叠、删除向量、WAL/恢复、时间分层 compaction、快照一致性）。列式格式是这五件里**唯一能直接买现成、也最不值钱**的一件。

**路线已定 = 自研 DataFusion + Vortex（Parquet 作长寿命保守备选），BM25 与 graph_index 一等公民。** 这是唯一能让两块自有 IP 同时当一等索引、从而兑现「旗舰噱头」产品命题的路线——买 chDB/ClickHouse（路线甲）会把两块 IP 挤成旁路 sidecar，命题当场塌。代码核实坐实了这点：ClickHouse 官方明确不做 BM25 评分、无用户可注册索引 AM。

**但要诚实记三笔账（本稿相对初稿的核心修订，见 §8）：**
1. **引擎真实成本 ≈ 40-60 人月**（初稿写的 18-26 与它自己 §2 的 21-35「仅两块 IP 移植」自相矛盾，已重算见 §3.5）。
2. **引擎只是底座 ≈ 占整个交付的 30-40%**；完整可观测平台（前端/eval/告警/RBAC/SDK/仪表盘）是另外 60-70%，不在本引擎预算内（§3.6）。
3. **路线的上游变量在市场不在工程**：「客户是否为『自有 IP 当一等索引』本身付费，而不是为『中文召回好+延迟低』付费」这条命题未被验证；它不推翻已定路线，但它是 #1 风险，必须在重投入前由 PoC/市场回答（§7）。

---

## 1. 正面回答：「是不是就差一个列存数据库」

把口语里的「列存」拆成五件独立的事，逐件标注「现成可买」还是「必须自研」。**结论：五件里只有 (a) 真能买，(b)~(e) 都要自研——而 (b)~(e) 才是工程大头。**

| # | 能力 | 它到底是什么 | 现成可买？ | 谁提供 / 谁自研 |
|---|---|---|---|---|
| **(a)** | **列式格式** | 列存编码 + 压缩 + zone-map(per-chunk min/max/null/distinct) + schema(DType) | ✅ **真能买** | **Vortex**（layouts + zone-map + 统计，随机读 ~100x 快于 Parquet，已入 Linux Foundation 中立治理，Spice.ai 生产验证）；保守备选自管 Parquet。格式本体就这一件，剩下四件它「明确 out-of-scope」 |
| **(b)** | **merge-on-read 折叠 + 删除向量** | 同一 span 多次上报(start→end→补属性)的多版本读时折叠；UPDATE/DELETE 走 tombstone 不原地改 | ❌ **必须自研** | Vortex/Parquet 都不送 deletion vector；DataFusion 无现成 fold 算子。**自研 `MergeOnReadExec` + per-段 RoaringBitmap deletion vector**。有 Spice.ai 蓝本 + 团队 graph_index 侧 `deleted_rids_` 读时折叠经验可复用心智 |
| **(c)** | **WAL + 崩溃恢复 + 单写者** | 写前日志、group commit、fsync、崩溃后 replay、单写者 commit 串行化 | ❌ **必须自研** | 团队现有 BM25/graph_index 的持久化 **100% 靠 PG xlog + 三路 RTO replay**，移出内核后这一整套全没了。**被低估最严重、对金融政企数据最危险的一条**（见 §2、§5、§7） |
| **(d)** | **时间分层 compaction + TTL** | L0→L1→L2 段合并、按时间分层、TTL 整段删、合并清 tombstone、重写段 | ❌ **必须自研** | 没有底座送这个。**自研 compactor + 段 GC**（trace 是时序数据，这是膨胀根治的真正手段；FINAL/读时折叠只保证读正确，不等于物理已折叠） |
| **(e)** | **快照一致性 / MVCC / 时间旅行** | 每次 commit 产生不可变新版本、读看到一致快照、跨 memtable+段+删除+upgrade 的一致视图 | 🟡 **半买半自研** | Lance 自带 manifest+MVCC（但绑死它的索引，§4 否决）；Vortex/Parquet 不送，**自研轻量 manifest**（Spice.ai 蓝本：嵌入式 KV/RocksDB 存段元数据 + 目录即 snapshot + 原子 compaction 单事务）。团队 disk_container/FreeSpace/manifest('XDMF') 经验同构 |

**真正的工程大头不是列存格式，是 (b)+(c)+(d)+(e) 这套「表层」，外加把 BM25/graph_index 接成一等公民下推的「检索适配层」（§3）。** 「就差一个列存」方向对（确实差一个能装索引的列式引擎），但严重低估了工作量：差的是一个**引擎 + 表层 + 检索下推层**，不是一个文件格式。

> 诚实补一刀：连 (a) 这件唯一能买的，Vortex 也带残余风险——**forward-compat（老库读新文件）1.0 前不做、库绑定不打 semver 1.0 持续高频发布**。对 30 年长寿命私有化数据，需「锁定 ≥0.36.0 edition + 自带读路径 + 可重写迁移」对冲；要求绝对可读零依赖的客户，退守自管 Parquet（牺牲随机读换极致可移植）。

---

## 2. 校准「我们有这两块 IP」到底意味着什么

**核心校准：算法在手 ≠ 能直接进独立引擎。两块 IP 的价值资产（可移植）与真正成本（必须重写）是错位的——值钱的算法可移植，但让它「是一等公民」的那部分恰恰焊死在内核里要重写。**

### 2.1 BM25：`algorithm-portable / glue-rewrite`，移植 **9-15 人月**（捷径 5-8，对齐功能 7-10）

| 可直接复用（一等资产） | 必须重写（焊死在内核） |
|---|---|
| BM25/TF-IDF 评分数学（`bm25_score.cpp` 12-103，纯函数零内核依赖） | 倒排整个焊在 PG buffer manager 上：每个 postings op 都是 ReadBuffer/LockBuffer/MarkBufferDirty（`bm25_inverted_list.cpp` 1535 行，几乎全 I/O 绑定） |
| 布尔查询语言 lexer/parser/AST（AND/OR/boost/min_should_match，纯逻辑） | `disk_container` 模板底座（DiskVector/FreeSpace/DiskHashTable，~3756 行）直接 include 内核 xlog/relcache/heapam，须换 Rust 段/blob store |
| cppjieba 中文分词器（header-only ~3275 行，MIT，非 openGauss 耦合，cxx FFI 桥接或换 jieba-rs） | **全套自定义 WAL `RM_BM25_ID` + 双 replay（serial + 并行 RTO）+ ~15 处内核恢复派发注册**——离开 PG xlog 零意义，**durability 从零重做（确定性内核固定表级工作量，非可折扣项）** |
| DAAT/block-max-WAND top-k、skip-pointer + min_should_match + 稀疏向量融合（作为**规格/算法**可复用） | PG IndexAmRoutine 回调契约 + bgworker 并行建索引，整套重写为 DataFusion TableProvider/ExecutionPlan |
| 倒排盘上 layout + skip-pointer 维护算法（作为 spec） | 词典生命周期焊死 pg_ts_template/pg_ts_dict/pg_ts_content（用户词典 `vexjieba_add_userdict` 写 pg_ts_content）；planner 集成（@~@/@-@ 是 pg_operator 行，`setrefs.cpp` 把 bm25_score() 推出 indexscan）；doc_id 焊死 ItemPointerData(heap TID) |

**最危险的认知错位**：真正差异化的 IP（block-max-WAND skip-list 倒排、min_should_match、稀疏+BM25 融合）**就活在 buffer-coupled 的 `inverted_list/scan` 代码里，不在可移植的 score/parse 文件里**。你不能「直接 lift 索引」——你是在重写存储 + 重新验证搜索算法。

> ⚠️ **已知 bug 必须当工作量计入，不是白送的中性继承**：`inverted_list.cpp:341-344` 带一个自认注释「The long skip pointer doc_id is not set properly」——**并行建索引的正确性原团队就没解决**。移植时必须二选一并各自计入预算：**(i) 修它**（净新增并发正确性科研）或 **(ii) 放弃并行建索引接受串行**（建索引吞吐退化）。不准当「随移植白送」。

### 2.2 graph_index：`algorithm-portable / glue-rewrite`，移植 **12-20 人月**（FFI 复用算法）

| 可直接复用（一等资产） | 必须重写（焊死在内核） |
|---|---|
| 纯算法层 `graph_index_algorithm.h`（1033 行模板类，存储/分配器/距离器全模板注入，不直接调 PG buffer mgr） | `DiskStore` 运行时盘上后端整段焊死 PG buffer manager（`graph_index_storage.h:670-1447`，直接用 Relation/Buffer/LockBuffer/heap_getattr） |
| **★ 过滤 ANN 算法骨架已在**（`search_internal` 有 `filter_func` 模板参数，:626 `if(!filter(cur_point.id)) continue`） | 存储头直接 include `vector_buffer/vector_smgr.h`（团队自管存储管理器，PG-only 焊点）、`floatvector.h`、`vacuum.h` |
| SIMD 距离内核（`common/distance/` 9036 行，SSE/AVX2/AVX512/NEON + 运行时探测，纯计算零 PG 依赖）——**最值钱的可移植资产** | on-disk 元页/页格式焊死 PG 8KB page+opaque（MetaPage 全用 BlockNumber），独立引擎换段内偏移布局 |
| PQ + RaBitQ 量化（1906 行，DuckDB 侧已端到端用起来） | WAL/redo 走 PG xlog；PG IndexAmRoutine + fork worker/DSM/LWLock 并行建索引（8976 行），重写为引擎适配层 |
| **★ 双后端已验证（仅证只读搜索算法可移植）**：DuckDB 用 174 行 `duck_pg_shim.hpp` mock 掉 MemoryContext/palloc/elog 就跑通同一套 common 算法 | 节点身份焊死 ItemPointerData(TID)（DuckDB 已示范重定义为 `{row_t row_id}`）；MemStore 无锁内存池 + entry-lock 绑 PG 共享内存/DSM，Rust 重写并发建图（高风险） |
| **★ 删除/段式持久化原型现成**：DuckDB 侧 `deleted_rids_` tombstone 读时折叠 + 过采样补偿；`DiskManifest('XDMF') + segments[]` + SerializeToWAL | |

**最危险的认知错位（两条）**：
- **「带过滤 ANN」名不副实**：filter_func 模板能力存在，但 **PG 和 DuckDB 两个出厂产品都只用 DummyFilter，HYBRID_INDEX 多列过滤在 DuckDB 明确 disabled**。即团队「带过滤 ANN」目前是**半成品**——只有算法骨架，没有 SQL 谓词→filter 下推链路、没有进图过滤（ACORN 式）、没有属性约束索引。且现有过滤是 **post-filter-during-search**（filter 只裁结果、非通过点仍占 ef 预算导航），**低选择率谓词下召回/QPS 结构性塌方，不是调参能救**。trace 要的 service/time/status 乃至高基数 trace_id 精确过滤都需新建下推 + prefilter/ACORN 改造，是**净新增工作量（§3.5 已单列计入）**。
- **174 行 shim 只证明「只读搜索算法」解耦，不证明持久化解耦**：`DiskStore` 运行时盘上后端整段仍焊在 PG（storage.h 直接 include vector_smgr.h/floatvector.h/vacuum.h）。**可移植的是算法骨架，要重写的持久化后端才是工程大头。**

### 2.3 校准结论

- **「有 IP」= 省掉了算法研发与召回正确性这块最难的科研，不等于省掉工程。** 移出内核进 Rust 引擎，**主要代价在重写持久化/恢复/存储/下推，不在重写索引算法**。
- **两块 IP 移植合计 21-35 人月**（BM25 9-15 + graph_index 12-20，FFI 复用已折扣）。**这个数字是 §3.5 总账的锚——任何低于 21 的总盘子都自相矛盾。**
- **「就差列存」对算法层基本成立、对工程层严重不成立**：智能索引内核确与存储后端解耦（DuckDB 174 行 shim 是铁证），缺的是把它接成新引擎一等公民二级索引 + 段式存储 + 过滤下推——没有任何路线送现成。

---

## 3. 干净的二选一（路线已定为乙，本节存证为何）

> 两路都要「自己拼存储 + 索引下推」，差别在底座给不给你挂自有索引的位置。

| 维度 | **路线甲：买底座（chDB/ClickHouse）** | **路线乙：自研（DataFusion + Vortex/Parquet）✅ 已定** |
|---|---|---|
| **两块 IP 能否当一等噱头** | ❌ **不能。** ClickHouse/chDB 无任何用户可注册的索引 AM / 外部索引插件点（比 PG 更封闭），只有内置三类（skip/text/vector）。**BM25**：相关性评分官方明确不做（FTS GA 博客原文「不实现 TF-IDF/BM25」，仅 token 过滤；BM25 是 OPEN issue #92097 无 roadmap；text index 还要 26.2 才 GA），bm25_score() 这块核心 IP 无落点；jieba 只能 ingest 期预分词喂 array tokenizer（非自定义分词器）。**graph_index**：只能编 .so 走 UDF/旁路，做不成随段增量 merge 的一等索引；要么改用 CK usearch HNSW（退回团队已超越的能力面）。**= 两块 IP 都退化为旁路 sidecar，噱头塌** | ✅ **能。** DataFusion 的 `TableProvider`/`ExecutionPlan`/`PhysicalOptimizerRule` 是开放 trait。自定义 ExecutionPlan 内部直查我方 BM25 倒排和 graph_index（LanceDB/InfluxDB 即此法），把 service/time/status 谓词作为 filter_func 真正下推进 `search_layer`，prefilter→ANN→精排已被 LanceDB 在 DataFusion 上验证。bm25_score() 变 ScalarUDF + 优化规则重写。**唯一能让两块 IP 同时当一等公民、跑一个计划内 hybrid/RRF 融合的路线** |
| **总人月（到可私有化交付 L1 单实例引擎）** | 表面省（CK 出厂即列存+FINAL+HNSW），**实则被吃回**：旁路 glue + id 回灌 join + 混合排序自写（≈ 自研同件事）；chDB 安全面**据 Tinybird 分析/官方文档为零 auth/RBAC/审计/多租户隔离**（⚠️ 此项 chDB 源码不在工作区，**属文档断言级，须 PoC A 一手核实**），若属实则 per-tenant 隔离+审计须自建 CK-server 级一层。**未独立逐项估 PM**（见 §7 诚实缺口） | **≈ 40-60 人月（重算，见 §3.5）。** 初稿的 18-26 已废止 |
| **关键风险** | ① 底座源码改不动、库 churn、C++ 大单体；② BM25 相关性=核心卖点而底座无等价物；③ FINAL 读放大须靠排序键选择性+PREWHERE；④ chDB runaway query 拖垮宿主进程。**致命：买了一个装不进自己索引的盒子** | ① ANN/BM25 **TopK 下推 DataFusion 无标准 API**（Discussion #16358 OPEN）须自写 physical 节点+优化规则；② **WAL/崩溃恢复从零重写**（最危险）；③ 过滤 ANN 半成品须 ACORN 改造；④ 并发建图 Rust 重写正确性；⑤ Vortex forward-compat 长寿命风险；⑥ **小团队自研引擎 bus-factor**（§7） |
| **金融政企长寿命私有化** | chDB 安全面结构性硬伤（若断言属实，须外造）；CK server 有安全面但索引仍封闭、C++ 大单体维护重；**但 CK 是全球部署的成熟引擎，30 年可维护性/bus-factor 优于小团队自研** | Vortex 已入 **Linux Foundation 中立治理 + 0.36.0+ 向后兼容承诺**（主权风险 << Lance 美国系）；本地 NVMe 低延迟、原生中文、数据不出境全成立；**残余：自研引擎长期靠小团队维护，bus-factor 是真实代价（§7 对称记账）** |

### 推荐与已定：**路线乙（自研 DataFusion + Vortex，Parquet 保守备选）**

**这是用户已拍的产品命题决定的，不是工程账算出来的——本节存证它在「保住自有 IP 一等公民」这条用户选定的首要权重下成立：**

1. **命题决定路线。** 用户产品命题 = 「两块智能索引都是自有 IP 旗舰」。这条只有路线乙能兑现；路线甲下两块 IP 都是旁路二等公民（bm25_score 在 CK 无落点、graph_index 拿不到 merge 钩子）。**在此命题下路线乙是唯一解。**
2. **解耦已被铁证证明。** DuckDB 174 行 shim 跑通同套 graph_index 算法、BM25 评分/查询/分词全是纯逻辑——移出内核技术上确定可行，代价在工程不在科研。
3. **单机 + < 1 亿 span/天是自研舒适区。** 不需要 CK 的分布式横扩（单机大部分用不上），本地 NVMe + 原生中文 + 数据不出境恰是自研栈能做到极致处。

> ⚠️ **诚实记录路线甲何时反超（这是 #1 风险，不是脚注，见 §7）**：若产品命题从「自有 IP 旗舰」退化为「trace 能查就行、语义召回可有可无」，则两块 IP 不再是必须的一等公民，买一个成熟 OLAP 反而更省。**这个上游变量在市场不在工程，必须先于重投入回答。** 路线已定不代表这条风险消失——它决定 40-60 人月会不会打水漂。
>
> ⚠️ **买侧未被钢人化（红队 2 实锤）**：本表「路线甲」是 chDB 裸用 + sidecar 的最弱形态，而真实买侧应是 **ClickStack/HyperDX（ClickHouse 2025 收购，单二进制自托管 OTel 原生 trace 栈）或自托管 ClickHouse-Langfuse（MIT，免费）+ 加一层中文召回**。PoC A 必须拿这个**强买侧**对标，否则 1.5 节的「买省是假象」结论不可证伪。路线乙的正当性应建立在赢过强买侧、而非赢过稻草人。

---

## 3.5 单一权威人月表（废止初稿 18-26 与 plan-b §14）

> 红队 1 实锤：初稿 §2 列「两块 IP 移植 21-35」，§3 却写总盘「18-26」——总数低于 IP 移植下限，且与 plan-b §14「BM25 倒排 3-4 / filtered ANN 3-4」三处数字互相打架。**以下为唯一权威表，按首次写崩溃一致 LSM 上浮、逐项独立相加，不再引用任何旧数字。**

| 工作块 | 人月 | 依据 |
|---|---|---|
| BM25 移植（含 cppjieba FFI、倒排重写、durability 从零） | 9-15 | §2.1 核实 |
| graph_index 移植（FFI 复用算法+距离+PQ，重写存储/段/并发建图/接入） | 12-20 | §2.2 核实 |
| 表层：MemTable/WAL+崩溃恢复 | 2-3 | 团队首次，最危险一条 |
| 表层：MergeOnReadExec 折叠 + 删除向量 | 2.5-3.5 | |
| 表层：LSM 写路径 + 段生命周期 | 2-3 | |
| 表层：时间分层 compaction + 段 GC | 2-3 | |
| 表层：轻量 manifest + 单 metastore 原子 commit | 含上/1-2 | Spice.ai 蓝本 |
| DataFusion 检索下推层（自写 physical 节点 + 优化规则，#16358 无标准 API） | 2.5-3.5 | §research |
| **过滤 ANN ACORN/进图过滤改造**（半成品→成品，初稿漏计） | 3-5 | §2.2，大概率触发 |
| 摄入/打包/压测/混沌测试 | 4-6 | |
| **合计（引擎底座，到可私有化 L1 单实例）** | **≈ 40-60 人月** | |

- **自然月而非满载月**：40-60 人月 / 5 人 ≠ 8-12 月顺风。按「团队首次写 LSM」的学习曲线 + 混沌测试发现数据正确性 bug 的长尾返工，**真实交付 ~7-9 自然月起**（红队 1 对强耦合 LSM/WAL/fold/compaction/恢复链建议额外 1.4-1.6x 缓冲，已部分含在上表区间）。
- **最大不确定项**（PoC B/C/E 专为挤水分）：倒排自研 + DataFusion 自定义下推 + 并发建图 Rust 重写正确性 + WAL 崩溃一致性。

## 3.6 高度校正：引擎只是底座（红队 2 实锤）

**40-60 人月只是「产物③引擎底座」。** 按本组织决策摘要 §4 与 appendix C/D，客户实际买单的**完整可观测平台**——trace 浏览器前端、eval 评测套件、告警、RBAC/多租户、SDK 矩阵（产物②）、仪表盘——是交付的 **60-70%，且大部分买不到、必须自建，不在本引擎预算内**。

> 含义：把所有人月压在「最该买、最便宜」的存储底座、0 人月给「买不到、最贵」的平台层，是高度错位。**程序级排期应让平台层并行领跑，引擎按「真正买不到的部分」量裁。** 这不改变「引擎自研」的已定结论，但纠正「40-60 就是全部成本」的错觉——总程序成本显著更高。

---

## 4. 明确的废弃与统一（不再反复）

- **删掉 Lance 当主干。** 去掉「内置 FTS + 内置向量」需求后（因我们自带 BM25 + graph_index），Lance 把 IVF_PQ/HNSW + INVERTED 全文**绑进格式**从优势变累赘、**与我方两块 IP 直接重复且冲突**。叠加红队两路点穿的最弱环：lancedb #2329 OPEN「BM25 不支持中文」+ INVERTED 数据集级增量重建（热段走 flat search 直到 reindex）；lancedb #2426 单写者 commit-conflict 卡 version 66；pre-1.0 churn + 美国系主权。**Lance 出局。** 其 manifest/soft-delete 模型仅作设计参考（已被 Vortex+嵌入式 KV 蓝本替代）。
- **向量统一 graph_index。** 不用 DiskANN（团队定调），不用 CK usearch HNSW（退回已超越能力面），不用 Lance 内置向量。所有 ANN 走 graph_index（FFI 复用 algorithm.h + distance + PQ，存储/页/WAL/AM 重写）。
- **倒排统一自有 BM25，不赌 Lance jieba。** 中文分词走团队 cppjieba（cxx FFI 保留精确字典/HMM/用户词典；或 jieba-rs 但**必须对现有客户语料 A/B 验证**分词一致，否则 token 不同→召回/评分漂移）。绝不依赖 Lance INVERTED（中文不支持）。

---

## 5. 红队 punts —— 升级为正文义务（六条不变量 + 设计前置）

以下六条是前几轮红队点过、被前稿 punt 掉的硬骨头，升级为引擎必须正面实现的不变量，每条给归属模块 + 验收口径。

> ⚠️ **红队 1 加注：其中第 1、2 条是首次写 LSM 团队最易翻车处，必须先各出一份 ≤2 页设计草案（段生命周期锁状态机协议 / 跨四源快照读规则），由 PoC 证伪——而不是让 PoC 从零发明。** 没有设计草案前，这六条只是「命名过的硬骨头」，不是「设计过的不变量」。

1. **并发段生命周期锁模型。** 段 building→sealed→live→compacting→dead 状态机须有显式锁协议：compaction 重写段时并发读仍看到旧段一致版本，旧段在引用全释放前不得 GC。**归属**：Manifest + 段引用计数（epoch-based reclamation 或 Arc + 延迟 GC）。**验收**：compaction 与高并发读同跑 24h 无 use-after-free、无读到半写段。（原内核 graph_index 的 entry-lock 协议绑 PG 共享内存，Rust 必须重写，高风险）**【需先出 ≤2 页设计草案】**
2. **跨 memtable + 段 + 删除向量 + upgrade 的快照一致性。** 一次查询在单一一致快照上跨四源读：活 memtable、不可变段、per-段 deletion bitmap、upgrade（属性补写）旁路列。**归属**：Manifest 版本号 + 读开始钉住 snapshot id，MergeOnReadExec 全程同一版本。**验收**：读进行中并发 flush/compaction/delete，结果等价于读开始瞬间的逻辑视图。**金融政企「审计可复现」准入项。【需先出 ≤2 页设计草案】**
3. **活 trace 读扇出延迟硬不变量。** 活 trace（span 陆续上报）读扇出到 memtable + 最近 L0 段 + 区间树，**有明确延迟 SLO**（trace 详情页 P99 < X ms），不能因段增多线性退化。**归属**：IntervalTree（段级 min/max ts + trace_id 路由）+ 活 trace 内存索引。**验收**：1 亿 span/天下活 trace 读触及段数有**明确上界 N**、P99 守 SLO。（附录 Q 已点名：活 trace 事件同 time bucket 同 trace_id，zone-map 几乎裁不掉——必须给出 L0/L1 段数上界，不能只验正确性）
4. **fold 去重键必须用确定性 event_id，禁 snowflake。** 去重键须是写入方可确定性复算的 event_id（trace_id + span_id + 上报序/内容哈希），**绝不用引擎侧生成的 snowflake/自增 id**——否则同 span 重传被当两事件，折叠失效，静默重复/丢数据。**归属**：内部 Span Schema 主键 + ingest 归一化。**验收**：同 span 重传 N 次（乱序、跨 flush/commit 边界），fold 后恰一条逻辑事件、内容为最后写入语义；并 **property-based + 崩溃注入证明已 ack 事件在任意崩溃点不丢不重复折叠**。**零容忍。**
5. **格式迁移停机窗口。** Vortex forward-compat 1.0 前不做（§1 风险）。升级导致段 edition 变化时：**自带读路径（新库读锁定旧 edition）+ 后台可重写迁移（compaction 顺带升 edition）**，目标零停机滚动；做不到则给停机窗口上界写进 SLA。**归属**：Manifest 记每段 edition + Compactor 迁移。**验收**：跨一个 breaking edition 升级，旧段可读 + 后台重写完成、读写不中断（或 ≤ 合同承诺）。**对 TB 级数据须量化全量重写时长。**
6. **metastore 真单体化。** Manifest/段元数据/deletion 元数据落**单一事务性 metastore**（RocksDB/SQLite，Spice.ai 蓝本），compaction/commit 是**单条原子事务**。不准元数据散落多文件靠「约定」一致（裂脑根源）。**归属**：单 metastore + 原子 commit。**验收**：commit 任意点 kill -9，重启后要么完整新版本、要么完整旧版本，无中间态、无孤儿段、无悬空 tombstone。

---

## 6. Phase 0 生死 PoC 清单

> 原则：先用低成本 PoC 证伪关键不确定性，再决定 40-60 人月重投入。下列是**门禁**不是热身，任一失败即改路线或砍线。

### A. 对标赛（决定路线，最高优先）—— 自研栈 vs **强买侧**
- **同一份 trace 负载**（1000 万-1 亿 span，含中文文本列 + 1536 维向量列）：
  - **强买侧（钢人，红队 2 要求）**：ClickStack/HyperDX 或自托管 ClickHouse-Langfuse + jieba ingest 预分词 FTS + CK usearch HNSW + 一层中文召回 feature。**不是 chDB 裸用**。
  - 自研栈：DataFusion + Vortex 段 + graph_index(FFI) + BM25(FFI 评分) 最小骨架。
- **比四件**：① 中文检索**相关性质量**（BM25 评分排序 vs CK 仅 token 过滤）；② 带过滤 ANN **召回@k**（service/time 过滤下 graph_index filter 下推 vs CK prefilter 掉召回）；③ 折叠正确性 + 读放大；④ 点查/span 取回延迟。
- **附带一手核实**：chDB/ClickStack 嵌入态**实际暴露的 auth/RBAC/审计面**（把 §3 那条「零安全面」从文档断言升到 L2 核实——否则不作为否决决定性论据）。
- **门禁**：自研栈在「相关性 + 带过滤召回」上须显著优于强买侧，否则「自有 IP 当一等公民」的付费价值证伪 → 触发 §7 #1 风险、重审路线。

### B. BM25 移植可行性钉子
- **cppjieba cxx FFI 桥接**：编进 Rust 跑通 cut_mix/cut_query/highlight + 用户词典，**对现有客户语料 A/B 验证**分词一致（或量化漂移）。
- **postings 重写 + block-max-WAND**：Rust 最小倒排段（skip-pointer + block-max）跑通 bm25_score + min_should_match。**门禁**：top-k 与现内核同语料一致；**并明确决定修复 §2.1 并行建索引 skip-pointer bug 还是接受串行建索引**，把对应代价计入预算。

### C. graph_index 移植可行性钉子
- **algorithm.h/distance/PQ 经 C ABI FFI 复用**：extern "C" 封装 + 异常边界，在 Rust 段式 block store 跑通构图/搜索。**门禁**：召回@k 与 DuckDB 出厂版同语料一致。
- **过滤下推真接线**（半成品→真货关键）：service/time/status 谓词作 filter_func **真正下推进 search_layer**，测**低选择率谓词召回/QPS 退化曲线**。**门禁**：中等选择率召回守住，否则坐实 §3.5 的 ACORN 改造 3-5 人月触发。

### D. 活 trace 读延迟（§5-3 提前验证）
- 构造「持续上报中的活 trace」（span-start 先到、end 后到、属性补写、乱序），测 trace 详情页读扇出 P99 随段数增长曲线。**门禁**：扇出段数有上界、P99 守 SLO；同时验 §5-4 同 span 重传 N 次 fold 后恰一条。

### E. 表层最危险一条：WAL + 崩溃恢复（§1c）
- 最小 WAL + group commit + 单 metastore 原子 commit，**commit/flush/compaction 随机 kill -9 重启 replay**。**门禁**：无中间态、无孤儿段、无悬空 tombstone、无静默丢事件。

---

## 7. 诚实的不确定性与 #1 风险（不准糊）

**#1 风险（上游、决定 40-60 人月会不会打水漂）——市场，不是工程：**
- 决策摘要 §3/§4 的结论是「护城河收敛到 NVMe 延迟 + 原生中文 + 数据不出境 + 信创主体资格」，**明确不是「自有 IP 当一等索引」**，且「带过滤语义召回是 ClickHouse 系在位者一个 sprint 就能补的 feature」。本文路线乙把「自有 IP 当一等公民噱头」抬成首要权重——**这是用户已拍的产品命题，本文执行它；但必须诚实标注：没有证据表明客户为『自建的一等 BM25/graph_index 索引』本身付费，而非为『中文召回好 + 过滤召回 + 低延迟』（任何后端都能给）付费。** PoC C（语义召回付费意愿）须**先于** build/buy 重投入跑通；若验不出，默认翻回买侧。**本文与决策摘要的关系：路线乙的兑现 CONDITIONAL on 该市场门禁通过。**

**工程侧确定的不确定性：**
- **DataFusion ANN/BM25 TopK 下推无标准 API**（#16358 OPEN）必须自写 physical 节点 + 优化规则；且**无任何单一先例同时做「filter 下推进 search_layer + fold 算子 + bm25_score ScalarUDF + RRF 融合 + 跨四源快照」在一个计划里**——PoC A 自研栈骨架必须显式验这四件协同，不能用 LanceDB「单点 ANN 下推」先例暗示已解决。
- **「带过滤 ANN」是半成品**：下推链路、进图过滤、属性约束索引全无；ACORN 改造已单列 3-5 人月（§3.5）。
- **BM25 继承一个已知并行建索引 bug**（§2.1）；durability 从零重做是确定性工作量非可折扣项。
- **Vortex forward-compat 与库 semver 是真实长寿命风险**，§5-5 是对冲非消除；30 年绝对可读零依赖客户退守 Parquet。
- **bus-factor 对称记账（红队 2）**：30 年长寿命下，**小团队自研 Rust 引擎的可维护性/总线因子风险，对称地不低于** Vortex pre-1.0 风险。不能只拿 pre-1.0 打 Vortex 却不计自研引擎本身的长期维护负债。
- **chDB「零 auth/审计」是文档断言**（源码不在工作区），PoC A 一手核实前不作否决路线甲的决定性论据。

---

## 8. 修订记录（本稿相对首席初稿，红队对账）

| 项 | 初稿 | 本稿修正 | 触发 |
|---|---|---|---|
| 引擎人月 | §3 写 18-26（与 §2 的 21-35 自相矛盾，抄了 plan-b） | **§3.5 单一权威表，重算 ≈40-60**，废止 18-26 与 plan-b §14 | 红队1 killshot |
| 平台层 | 0 提及（全押引擎底座） | **§3.6 高度校正**：引擎只占交付 30-40%，平台 60-70% 在预算外 | 红队2 |
| 过滤 ANN 改造 | 未计入任何表 | §3.5 单列 **3-5 人月**（大概率触发） | 两路红队 |
| BM25 已知 bug | 「随移植继承」轻描淡写 | §2.1/§6B **必须决定修复 or 串行**，计入预算 | 红队1 |
| 买侧 | chDB 裸用最弱形态 | §3/§6A **钢人化**为 ClickStack/HyperDX/Langfuse 对标 | 红队2 |
| chDB 零安全面 | 当实锤、否决论据 | §3/§7 **降为文档断言，PoC A 一手核实** | 红队1 gap |
| 市场门禁 | §7 末尾脚注 | **§0/§7 升为 #1 风险**，明确路线 conditional on 它 | 红队2 |
| 六不变量 | PoC 门禁 | §5 **第1/2 条须先出 ≤2 页设计草案**再 PoC | 红队1 |
| bus-factor | 只打 Vortex pre-1.0 | §3/§7 **对称记自研引擎长期维护负债** | 红队2 |

> 一句话：**路线乙（自研）按用户产品命题已定且技术可行（DuckDB 174 行 shim 是解耦铁证、chDB 确实装不进自有索引）；但诚实的引擎成本是 40-60 人月而非 18-26、且只是占 30-40% 的底座、且其商业回报 conditional on「客户为自有 IP 一等公民付费」这条尚未验证的市场命题。Phase 0 的 A（钢人对标 + 付费意愿）与 B/C/E（移植/崩溃挤水分）是放行 40-60 人月前的生死门禁。**
