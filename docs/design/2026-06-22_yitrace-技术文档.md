# yiTrace 技术文档

> 日期：2026-06-22
> 对象：要读懂引擎内部、参与开发或评审的工程师。
> 配套：产品定位与差异化见 [`docs/2026-06-22_yitrace-产品说明.md`]，本文只讲「里面怎么搭的」。
> 现状：能编译、能跑、86 个测试全绿的**验证级骨架**（Rust 工作区，约 7200 行）。刻意只用标准库、零外部依赖，`cargo check` 离线可过。三块要换成团队自有件的接口（列式段 / 中文倒排 / 图式向量）已用 trait 立好边界，详见 §10。

---

## 1. 它是什么

一台**单机、单目录、零外部依赖**的 AI Agent 可观测性数据库引擎。Agent（多轮对话、调工具、多 agent 协作）跑起来的 trace（谁调了谁、花了多少 token、慢在哪、改 prompt 后变好还是变差）灌进来，引擎把它存成审计级不重不漏的数据，并提供中文检索 + 带过滤语义召回 + 评测三类查询。

技术上它要同时立住三件别人结构上给不了的事：

1. **审计级写入**：同一条事件不管重传几次、崩溃重放几次，都只算一次（token/成本不翻倍）；崩了重启数据不丢；历史不可篡改。
2. **原生中文检索 + 带过滤语义召回**：中文倒排 BM25、把过滤条件下推进向量图搜索，作为引擎一等公民，而不是外包给 ClickHouse 后挂不进去。
3. **单机私有化**：一个目录装下全部状态，气隙机房可部署，数据不出境。

---

## 2. 整体架构

### 2.1 工作区与 crate 分层

引擎是一个 Rust workspace（`yitrace-engine/`），按「下层不依赖上层」自底向上分 5 个 crate：

```
yt-engine     ← 把各层串成一台引擎；摄入/读/检索/eval/HTTP 都在这
  ├─ yt-memtable   活内存表（四源里唯一可变的源，带上下界双水位）
  ├─ yt-wal        写前日志（内存 / 文件两后端，文件后端真 fsync）
  ├─ yt-manifest   单写者-多读者下的版本发布、快照 pin、回收水位（正确性脊梁）
  └─ yt-core       核心类型：标识、确定性 event_id、不可变 Manifest、deletion/upgrade 块、折叠算子
```

`yt-core` 不依赖任何其它 crate，是共享底座；越往上越靠近外部接口。`yt-engine` 内部再分模块：`wire.rs`（线格式 JSON 解析）、`otlp.rs`（OTLP 适配）、`http.rs`（HTTP 服务）、`bm25.rs`（中文倒排）、`graph.rs`（向量 ANN）、`segstore.rs`（段落盘）、`persist.rs`（manifest 落盘）、`vecstore.rs`（向量落盘）。

### 2.2 段五态生命周期

物理数据以**不可变段（segment）**为单位组织，段走五态：

```
building → sealed → live → compacting → dead
```

- `building`/`sealed`：尚未进 manifest，外部读者看不见。
- `live`：已发布，读者能读。
- `compacting`：被合并中，仍可读旧版本。
- `dead`：已从 manifest 移除，等所有 pin 它的读者走完后回收（见 §5）。

### 2.3 一条数据的生命周期（鸟瞰）

```
SDK/OTLP ──HTTP──▶ WireRecord ──▶ WriteCoordinator.ingest
                                      │  单写者串行
                                      ▼
                        WAL 追加 + fsync ──ack──▶ MemTable（活内存表）
                                      │
                       memtable 满 → flush_to_segment（sealed→live）
                                      │  manifest 写时复制 commit + 落盘
                                      ▼
        读：pin 快照 → 四源折叠归并(MergeOnRead) → 折叠后的 FoldedSpan
        检索：BM25 / graph ANN / 混合，过滤下推 → 命中 span
```

---

## 3. 数据模型

### 3.1 事件，而不是 span

外部看到的是 **span**（一次 LLM 调用、一次工具调用），但引擎内部存的是**事件（event）**。一个 span 至少拆成 `SpanStart` + `SpanEnd` 两个事件，晚到的属性补写是第三类事件。读的时候把同一身份的多个事件**折叠（fold）**成一条 `FoldedSpan`。这样设计的好处：写入永远是 append-only 的不可变事件，没有原地更新，崩溃重放天然幂等。

### 3.2 确定性 event_id —— 不重不漏的根

```
event_id = hash(ext_span_id, seq, event_type)
```

id 是**内容的哈希**，不是引擎侧生成的雪花号。同一条事件，Python SDK、TypeScript SDK、引擎三方逐字节算出同一个 id。于是：

- **重传幂等**：网络重试、SDK 重发，同一事件还是同一 id，折叠去重后只算一次 → token/成本绝不翻倍。
- **崩溃重放幂等**：WAL 重放把同一批再喂一遍，id 不变，结果一致。

身份字段（`ext_span_id` / `seq` / `event_type`）一经写入**冻结**，upgrade 补写不能覆盖它们——否则去重键会漂移。

### 3.3 线格式记录 `WireRecord` → `SpanFields`

摄入的 `WireRecord` 携带身份字段 + 业务字段。业务字段进引擎后归到 `SpanFields`：

| 字段 | 含义 | 折叠语义 |
|---|---|---|
| `status` / `duration_ns` | 状态、耗时 | last-non-null-wins |
| `parent_span_id` | 父 span（trace 是棵调用树） | last-non-null |
| `input_tokens` / `output_tokens` | LLM token（核心成本指标） | last-non-null |
| `session_id` | 会话 id（多轮/多 agent 串联） | last-non-null |
| `agent_name` / `tool_name` / `model` | 成本与可观测的下钻维度 | last-non-null |
| `input_text` / `output_text` | prompt / 答案（eval 的上文与对象） | last-non-null |
| `eval_score` / `eval_label` | 评测分（千分制整数）、标签 | last-non-null，**由 scorer 事后经 upgrade 通道补写**，不从线上摄入 |
| `logs` | 日志行 | 保序并集（union） |

### 3.4 折叠语义（`fold_events`）

同一身份的事件按 `seq` 定序后叠加：标量字段 **last-non-null-wins**（后到的非空值覆盖），`logs` 取**保序去重并集**。这套合并逻辑由 `SpanFields::merge_from` 唯一实现——`upgrade` 补写、读路径归并都调它，不各写一份（历史上各写一份导致过「只覆盖部分字段、新字段被悄悄丢」的 bug）。

### 3.5 三类不可变标识

`SegmentId`（段身份）、`ChunkId`（deletion/upgrade 块身份）、`WalLsn`（WAL 提交点）。全部单调递增、单写者分配、**GC 后也永不复用**——复用会引入 ABA 问题。

---

## 4. 写路径与单写者模型

### 4.1 单写者协调器 `WriteCoordinator`

所有改 manifest 的提交（flush / compaction / delete / upgrade）都过同一个 `WriteCoordinator`，串行执行。这样**没有写-写竞争**，整个并发难题被收敛成「1 写者 vs N 读者」，由 `yt-manifest` 单独处理。

摄入入口三个，最终都汇到同一个 WAL → memtable 边界：

- `ingest(records)` — 引擎内部 `WalRecord`。
- `ingest_wire(records)` — SDK 线格式（HTTP `/v1/ingest`）。
- `ingest_otlp(body)` — OTLP/OpenInference（HTTP `/v1/traces`）。

### 4.2 写前日志（WAL）

每批一帧：`[first_lsn u64][payload_len u32][payload][crc32 u32][marker=1 u8]`。整帧含 crc + marker **写完并 fsync 之后**才回 ack。重放时遇到第一个撕裂/损坏帧（短读 / marker≠1 / crc 不符）即停——那批从未 ack，丢弃合法。于是 **不丢已 ack、不重放半截批**。payload 是自研二进制编码（定长字段 LE + 长度前缀字符串），零依赖。

WAL 有内存与文件两个后端：测试用内存，生产用文件（真落盘）。

### 4.3 活内存表（MemTable）与 flush

MemTable 是四源里**唯一可变**的源，因此最棘手。它带**上下界双水位**：

- **上界** `live_lsn`：读者 pin 瞬间的已提交尾。读 MemTable 只接受 `LSN ≤ live_lsn` 的行。
- **下界保留** `retained_watermark`：读者 pin 版本的 memtable 水位。读半开区间 `(retained_watermark, live_lsn]`，与段源不重叠、不重复。

memtable 满了就 flush 成段：被吸收的前缀写进 sealed 段、推进水位、段转 live。**物理 evict 受 gate**：只回收 `LSN ≤ 所有活跃读者 retained_watermark 最小值` 的行。这条 gate 修的是「flush 把前缀物理删了，旧读者那截行段里有了但内存没了、被读零次」的漏行 bug——没人读完就不删。

---

## 5. 并发与快照模型（正确性脊梁）

这是 `yt-manifest` 的职责，也是整个引擎最容易写错的地方。

### 5.1 Manifest 是「值」不是可变结构

Manifest 记录「有哪些段、各段的 deletion 位图 / upgrade 补写块、各水位、epoch、id 计数器」。提交（commit）= 在旧 manifest 值上**写时复制**生成新版本，再**原子换 current 指针**。读者拿到的永远是某个冻结的版本，写者发布新版本不影响在读的旧版本。

deletion 与 upgrade 两条补写通道**结构完全对称**：都是 `Arc<不可变块> + 单调 _seq`，新补写一律生成新块、绝不原地改旧块。

### 5.2 读者 pin 协议：先登记，再解引用，最后校验

读快照必须按 **announce-before-deref-then-validate** 次序：

1. 先在读者登记表登记「我要 pin」；
2. 再解引用 current 指针拿到 manifest 版本；
3. 最后校验登记期间版本没被回收。

初稿写反了（先解引用后登记），被红队用 use-after-free 打穿。**次序才是正确性**，锁实现只是性能。

### 5.3 回收水位

```
safe_version = 所有活跃读者 pinned_version 的最小值
```

低于 `safe_version` 的 dead 段/旧块才能回收。对「已登记但还没落定（Tentative）」的读者要**保守保护**，否则就是被打穿的那个残窗。快照释放走 RAII（`Drop`），保证「注销 slot」与「释放 manifest 引用」严格同生死。

### 5.4 骨架取舍

当前用 `RwLock<Arc<Manifest>>` 当原子指针 + `Mutex<Vec<slot>>` 当登记表。真实实现换 `arc-swap`（无锁换指针）+ `crossbeam-epoch`（无锁纪元回收）。**但 pin 次序在骨架里是忠实的**——换无锁实现不改次序。

---

## 6. 读路径：四源折叠归并

读一条 trace，数据可能散在四个源里，必须在**同一个冻结快照**上跨源归并去重：

```
MergeOnReadExec（骨架，真实实现是 DataFusion 的 ExecutionPlan）
  ① MemTable     活内存行，区间 (retained_watermark, live_lsn]
  ② 段（segment）  落盘的不可变事件
  ③ deletion     删除位图，盖掉被删的行
  ④ upgrade      晚到属性补写，叠到对应 span 上
去重键 = 确定性 event_id；折叠 = fold_events
```

四源在固定快照上归并，保证「读到的是某一时刻的一致视图」，不会读到写一半的状态。

### 6.1 列投影（Projection）

聚合/列表类查询用位掩码声明只读哪些**可折叠值列**。身份与分组列（trace_id/span_id/ts/seq/event_type/ext_span_id）**恒读**（折叠、定序、分组都要用），不在投影里。投影的价值是让**列式段跳过不读的列**——尤其两个大文本列 `input_text`/`output_text`，多数成本/会话/聚合查询根本不碰原文。行式/内存源忽略投影（数据本就在手边），只有列式段（Vortex）从中受益。

### 6.2 查询出口

`read_spans_query`（trace 详情）、`list_traces` / `list_sessions`（列表）、`cost_by_agent`（按 agent 归因成本）等，都接受一个 `TraceQuery` + 快照，走同一套折叠读。

---

## 7. 检索内核（产品差异化）

### 7.1 中文倒排 BM25（`bm25.rs`）

两件事是真的、不是占位：

1. **中文分词** = 无词典的 **CJK bigram**（相邻汉字两两成词，"疑似盗刷" → 疑似/似盗/盗刷）。这是 Elasticsearch CJK analyzer 同款、零词典、std-only。ASCII/数字按空白与标点切词、小写化。接 jieba 词级分词是**升级**不是前置。
2. **BM25 打分**：真倒排（token → 每文档词频）+ idf + 文档长度归一（K1=1.5, B=0.75）。按相关性排序，不是子串「有/无」。

为什么比子串强：查「盗刷风控」这种**非连续多概念**串，子串占位要求文档里出现连续「盗刷风控」才命中 → 一条都召不回；BM25 按 bigram 拆成 盗刷/刷风/风控，命中两概念的文档排第一。模块自带一个**会失败的测试**钉住这个差距。

### 7.2 图式向量 ANN + 带过滤召回（`graph.rs`）

这块验证整个自研路线里红队点名的最大翻车点：**带过滤的近邻搜索能不能把召回拉回来**。当前是一个**可测量**的 NSW（navigable small-world）图（不是生产级 HNSW，没分层/量化/SIMD），重点全在两种过滤策略对比：

- **post-filter（事后过滤）**：先按向量搜出 ef 个近邻、**再**用谓词筛。谓词选择性一高（命中点稀疏），近邻里能活下来的寥寥无几，召回崩。
- **in-graph（进图过滤）**：导航时**穿过**不满足谓词的点当路由跳板，只把满足谓词的点收进结果，停止条件只看「满足谓词的点收够没」。于是会一直往图深处探到命中点。这是 ACORN 思路的最小版。

模块自带会失败的测试：选择性谓词下实测 **in-graph 召回 ≫ post-filter 召回**。这是对「拉不回 → 3-5 人月变 8-10」风险的实证答复。

### 7.3 混合检索与属性过滤

- `search_hybrid*`：关键词 BM25 + 语义 ANN 两路用 **RRF（Reciprocal Rank Fusion）** 融合，同时命中的排更前。
- `search_*_attr`：在向量/文本召回之上叠 `SearchFilter`（按 agent / 状态 / 时间过滤），实现「找 agent『风控研判』报错的相似 span」这类按业务维度的语义召回。过滤条件下推进图搜索（§7.2），不是事后筛。

HTTP `/v1/search` 走 `search_text_attr`，是产品差异化的对外出口。

---

## 8. 评测与数据集

### 8.1 单 span / per-agent 评测

- `eval_and_writeback(scorer, q)`：对查询命中的 span 跑 `Scorer`（如 `KeywordScorer`），分数（千分制整数，保住 `Eq`）经 **upgrade 通道补写回** span 的 `eval_score`/`eval_label`——评测结果是 trace 的一部分，但走补写而非重摄入。
- `eval_summary(q, threshold)`：按通过阈值聚合，看一批 span 的通过率（整体一行 + 每 agent 一行，回归视图「哪个 agent 退步了」）。
- `dataset(name)` / `eval_dataset(name, scorer, threshold)`：把采集的 span 固化成数据集（`DatasetExample`，存 span 快照而非引用，底层 trace 被回收也不影响），在数据集上跑评测——支撑「改了 prompt 之后是变好还是变差」的回归判断。

### 8.2 会话与多轮对话

层级是 **event → span → trace → session**：一**轮**用户问答 = 一条 trace（一棵 span 树），**多轮** = 同一 `session_id` 串起的多条 trace。session 不是新实体，就是 `SpanFields` 上一个按 last-non-null 折叠的字段，由 SDK 从 trace 透传到下面所有 span。

两个查询出口：

- `list_sessions(snap, q)`：按 `session_id` 聚合（distinct trace 数 / span 数 / token 汇总），只投影 `SESSION_ID + token` 列、跳过文本。
- `load_session_timeline(snap, session_id)`：把一个会话的多条 trace 拼成**多轮对话流** `SessionTimeline`——每条 trace 抽成一个 `SessionTurn`（`user_input` 取最早带输入的 span、`agent_output` 取最末带输出的 span、加该轮 agent/token/出错数/eval 分）。**轮次按 `trace_id` 升序定序**：折叠后的 `FoldedSpan` 不保留 ts，而 trace id 单调下发，是对话时间序的可靠代理。当前没有 session→trace 倒排索引，按 session_id 扫全量过滤（会话视图低频，可接受；高频再加边车索引）。

### 8.3 会话级（多轮专属）评测

把评测从 per-span 推到 per-session（`evalkit::score_session`，规则版）。多轮专属指标：

- **是否最终解决**（resolved）：最后一轮成功（无坏词、无错）。
- **是否绕圈**（looped）：双路检测——连续 ≥2 轮失败 **或** 同一问题被重复问 ≥2 次。
- 综合分 / 标签：未解决=0、绕圈后解决=500、一次到位=1000。

换 LLM-judge 做会话级评判时只换 `score_session` 函数体，其余不动。

### 8.4 evalkit —— eval 测试框架 / 场景模拟器

`evalkit.rs`：一套**自造数据、走真实摄入、跑完整闭环**的端到端 eval 框架（既当验证、也当可跑演示，`cargo run -p yt-engine --example eval_harness`）。

- **自产测试数据**：4 类内置 agent 场景（客服问答 / 风控研判多 agent / 代码助手 / 数据分析），每条 trace 拆成 root(编排)+tool(工具)+answer(作答) 三 span，带中文 input/output、token、状态；失败答案埋坏词留信号。用 splitmix64 确定性伪随机（同 seed 可复现、零依赖、不碰时钟）。
- **走真实摄入**：全部经 `ingest_wire`（与 SDK 线格式同一入口），确定性 event_id、折叠、落盘真实经过，不是塞内存表。
- **跑完整闭环**：单 span 评测（注入失败被 eval 精确还原、per-agent 差异可见）→ 数据集回归（更严 scorer 通过率下降即检出退步）；会话级 `run_session_harness` 造连贯多轮会话（一次到位 / 重试后成功 / 重复问后成功 / 始终失败四种弧线），逐会话装对话流再打分、分类对账。

---

## 9. 持久化与崩溃恢复

「重启不丢」要四块都落盘，缺一不可：

| 落盘件 | 模块 | 格式与保证 |
|---|---|---|
| **WAL** | `yt-wal` | 帧 + crc + marker，fsync 后才 ack；撕裂尾即停 |
| **段文件** | `segstore.rs` | `seg-<id>.dat = [crc32][payload]`；原子落盘（tmp+fsync+rename）；读时校验 crc，损坏当空段 |
| **manifest** | `persist.rs` | `[crc32][payload]`，原子写，带 magic+版本号；记有哪些段 + 各段删除/补写 + 水位 + epoch + id 计数器 |
| **向量** | `vecstore.rs` | 独立 append-only 文件 `[trace][span][dim][f32×dim][crc32]`；逐条校验，撕裂即停 |

**为什么 manifest 必须单独持久化**：段文件在盘上，但引擎重启后不知道有哪些段、各段删了哪些行/补了什么——那些只在 manifest 里。光有段没有 manifest，recover 找不到它们，flush 过（水位之前、WAL 不再重放）的数据就丢了。

**为什么向量要单独持久化**：embedding 不在 trace 数据里（外部 embedder 算的、旁路 `index_embedding` 进来），段里推不出来。BM25/属性边车能从段重建，向量不能，只能自己落盘，`recover` 时重载并喂回图索引重建。

恢复流程（`recover`）：读 manifest 重建段集合 → 重放 WAL 补水位之后的尾巴 → 重载向量喂回图索引 → 从段重建 BM25/属性边车索引。

---

## 10. 外部件接口边界（要换成团队自有件的三块）

骨架刻意零依赖，但三块在决策文档里就定了「FFI 复用算法 / 重写存储」。这里只立 trait，真实实现接进来不改上层：

| trait | 当前骨架实现 | 真实实现 |
|---|---|---|
| `SegmentStore` | `InMemorySegmentStore` / `FileSegmentStore`（行式 WAL 编码） | **Vortex** 列式段（layouts + zone-map + 统计），见 `yitrace-segstore-vortex/` |
| `Bm25Index` | `InMemoryBm25`（子串占位）/ 真 BM25（`bm25.rs`） | 团队自有 BM25（cppjieba 词级分词 + 倒排）的 C ABI |
| `GraphIndex` | `InMemoryGraphIndex`（暴力 L2）/ 真 NSW（`graph.rs`） | 团队自有 `graph_index` 的 C ABI（同一套 algorithm/distance/PQ） |

Vortex 段存储**刻意建在引擎工作区之外**（`yitrace-segstore-vortex/`，依赖 vortex 0.75 + arrow 58 + tokio）：那套大依赖只在这一个 crate 里，引擎骨架保持零依赖、离线可编译。它走 path 依赖实现引擎的 `SegmentStore` trait。落地计划见 [`docs/design/2026-06-22_列式段存储-vortex-选型与落地计划.md`]。

---

## 11. 摄入生态接口

- **自有 SDK**（`yitrace-sdk/`，Python + TypeScript）：嵌套父子 span、token 计数、session id、agent/工具/模型标注、输入输出文本。三方确定性 event_id 一致。SDK `to_wire()` 输出 JSON 批，POST 到 `/v1/ingest`。
- **OTLP / OpenInference**（`otlp.rs`）：任何已用 OpenTelemetry / OpenInference 埋点的应用，不改打点就能灌进来。认两套语义约定：OTel GenAI（`gen_ai.*`）与 OpenInference（`llm.*`/`input.value`）。一条 OTLP span（start/end 两时间戳）→ 拆成 SpanStart + SpanEnd 两事件（seq=1/2），确定性 event_id 自然成立。
- **极小 HTTP/1.1 服务**（`http.rs`，只用 `std::net`）：`POST /v1/ingest`、`POST /v1/traces`（OTLP）、`GET /v1/traces`（列表）、`POST /v1/search`（检索）。支持 `Authorization: Bearer <token>` 鉴权（None = 仅本机开发）。上量/上 TLS 时换 axum/hyper，路由逻辑不变。

**两个线格式坑都处理了**：① 大整数超 f64 精度（trace_id ~8.5e17、event_id ~1.2e19）→ 数字按原始字符串存、按需解析成 u64/i64，绝不过 f64；② Python 发数字、TS 发字符串（BigInt.toString 避免 JS 精度丢失）→ 整数字段两种都接。

---

## 12. 构建、测试与现状

- **构建**：`cd yitrace-engine && cargo check`（离线可过，零外部依赖）。Vortex 段存储单独 `cd yitrace-segstore-vortex && cargo build`。
- **测试**：引擎 82（76 单测 + 6 eval 框架集成），加列式段 7、Python/TS SDK 各 8，全绿。多数是**验证级、带会失败的测试**——刻意构造「占位实现会挂、真实现才过」的用例，用来证明技术前提成立（中文非连续多概念召回、in-graph 召回 ≫ post-filter、崩溃重放幂等、flush 后重启不丢、并发快照隔离、eval 精确还原注入失败、会话级绕圈检测）。

### 现状与诚实边界

| 已用代码验证 | 还没做（上量必需） |
|---|---|
| 确定性 event_id 三方一致、重传/重放幂等 | 列式段从行式 WAL 编码换成真 Vortex |
| 单写者 + 四源折叠归并 + 快照 pin 协议 | BM25/graph 从 Rust 骨架换成团队自有 C ABI |
| 中文 BM25 召回、in-graph 带过滤召回 | manifest 无锁化（arc-swap + crossbeam-epoch） |
| WAL/段/manifest/向量四件落盘、重启不丢 | compaction 真实执行、生产级 HNSW |
| OTLP/SDK/HTTP 全链路摄入 | TLS、多租户、运维面 |

一句话：**核心技术前提已经用代码验证通过，真正上量是把三块占位换成团队自有件 + 加 compaction/无锁/运维**，不是推倒重来。

---

## 附：关键源码索引

| 主题 | 位置 |
|---|---|
| 核心类型 / event_id / 折叠 / Manifest | `crates/yt-core/src/lib.rs` |
| 快照 pin / 回收水位 | `crates/yt-manifest/src/lib.rs` |
| WAL 帧格式 / 崩溃重放 | `crates/yt-wal/src/lib.rs` |
| 内存表双水位 / flush gate | `crates/yt-memtable/src/lib.rs` |
| 协调器 / 四源归并 / 读与检索 API / 会话时间线 | `crates/yt-engine/src/lib.rs` |
| 中文 BM25 / 图式 ANN | `crates/yt-engine/src/{bm25,graph}.rs` |
| eval 测试框架 / 场景模拟器 / 会话级评测 | `crates/yt-engine/src/evalkit.rs`、`examples/eval_harness.rs`、`tests/eval_harness.rs` |
| 段 / manifest / 向量落盘 | `crates/yt-engine/src/{segstore,persist,vecstore}.rs` |
| 线格式 / OTLP / HTTP | `crates/yt-engine/src/{wire,otlp,http}.rs` |
| Vortex 列式段 | `yitrace-segstore-vortex/src/lib.rs` |
| SDK | `yitrace-sdk/{python,typescript}/` |
