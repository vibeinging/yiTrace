# yiTrace Level-1 架构设计（方案 A：DataFusion + Lance）

> 单机、本地 NVMe、专用 trace 数据库引擎。Rust 自研引擎，DataFusion 做 SQL/向量化执行，Lance 做列式不可变段 + 内置向量 ANN + 内置中文 FTS + 版本。自建 LSM（memtable → Lance 段 → 时间分层 compaction）+ merge-on-read（deletion/upgrade vector）+ 区间树。
>
> 目标客户：中小客户 < 1 亿 span/天。
> 撰写日期：2026-06-17。所有外部事实已用 WebSearch 核对，文末列「事实核对与不确定」。

---

## 0. 设计前提与一句话结论

**约束解除后的根本改变**：不再建在 openGauss/yiTrace 行存底座上，可从零自研 Rust 引擎。这让上一轮"建在 openGauss 扩展里"的两处 CRITICAL 直接消失（第 13 节逐条解释）。

**一句话**：用 DataFusion 当查询大脑、Lance 当列式段格式与向量/FTS 索引底座，我们只需自研三件事——(1) LSM 写路径 + WAL，(2) merge-on-read 折叠算子，(3) 区间树（活 trace / 时间分层）。Lance 把"列式压缩段 + 中文倒排 + 向量 ANN + 版本"几乎全包了，**最大复用、最快落地**；代价是把命运绑在 Lance 的高频小写/compaction 成熟度上（第 12 节风险，第 13 节给出自研补丁）。

---

## 1. 总体架构分层

```
                ┌─────────────────────────────────────────────┐
   摄入接口      │  OTLP gRPC/HTTP   │  LangSmith REST  │  SQL   │
                └───────┬───────────────────┬──────────────┬────┘
                        │ 归一化为内部 Span Arrow RecordBatch
                        ▼
   写路径        ┌──────────────────────────────────────────────┐
   (LSM)        │ WAL(顺序写, fsync 组提交) → MemTable(Arrow 行组) │
                │  └ 活 trace 索引(trace_id→span 缓冲, 区间树)     │
                └───────┬──────────────────────────────────────┘
                        │ flush (按大小/时间 触发)
                        ▼
   存储层        ┌──────────────────────────────────────────────┐
   (Lance)      │  L0 段  L1 段  L2 段 ...  (Lance fragment)      │
                │  每段: 列式压缩数据 + zone-map + 段内倒排 + 向量  │
                │  + _deletions/  (soft delete) + upgrade 旁路列   │
                │  区间树(段级 min/max ts, trace_id 路由)          │
                └───────┬──────────────────────────────────────┘
                        │ 自定义 TableProvider / ExecutionPlan
                        ▼
   查询层        ┌──────────────────────────────────────────────┐
   (DataFusion) │  SQL 解析 → 逻辑计划 → 物理计划                  │
                │  TraceScanExec(投影/谓词/zone-map 下推)          │
                │  → MergeOnReadExec(多版本折叠多路归并)           │
                │  → FTS/VectorTopK/聚合算子                       │
                └──────────────────────────────────────────────┘
   后台         compaction 线程(时间分层) · WAL checkpoint · 段 GC
```

两种形态共用同一引擎核心（第 8 节）：
- **嵌入式**：`yitrace` 作为 Rust crate / cdylib，进程内调用，无网络。
- **单机服务**：同一核心 + Tonic(gRPC) + Axum(HTTP) + DataFusion FlightSQL/PG-wire 前端。

---

## 2. 数据模型与内部 Schema

Trace 的核心是 **span 事件**，且具备 trace 关键特征：**同一 span 会被多次上报/修正**（活 trace 阶段先发 span-start，后发 span-end；采样器后置；属性补写）。这是"多版本折叠（query-time fold）"的根因。

内部统一 Span Schema（Arrow，列式落 Lance）：

| 列 | 类型 | 说明 |
|---|---|---|
| `trace_id` | FixedSizeBinary(16) | 主路由键 |
| `span_id` | FixedSizeBinary(8) | trace 内唯一 |
| `parent_span_id` | FixedSizeBinary(8) nullable | 构树 |
| `start_unix_nano` | UInt64 | 排序键之一（时间分层用） |
| `end_unix_nano` | UInt64 nullable | 活 trace 阶段可空 |
| `duration_nano` | UInt64 nullable | 物化派生列（end-start），加速 P99 查询 |
| `service_name` | Dictionary(UInt16, Utf8) | 低基数，字典编码 |
| `span_name` | Dictionary(UInt32, Utf8) | 中基数 |
| `span_kind` | UInt8 | server/client/... |
| `status_code` | UInt8 | ok/error/unset |
| `attributes` | Map<Utf8, Utf8> 或拆 typed 列 | 见下 |
| `events` | List<Struct> | span 内日志事件 |
| `resource_attrs` | Map<Utf8, Utf8> | 资源属性 |
| `body_text` | Utf8 nullable | 供中文 FTS（日志/异常栈/prompt） |
| `embedding` | FixedSizeList<Float32, D> nullable | 语义召回向量（LangSmith trace/LLM span） |
| `_seq` | UInt64 | **引擎内部全局递增序列号**，折叠时定版本新旧 |
| `_op` | UInt8 | 0=upsert, 1=delete-tombstone |

**attributes 落列策略（关键，影响膨胀与可检索）**：
- 高频已知键（http.status_code、http.method、db.system、llm.model 等）**提升为 typed 物化列**（Dictionary/数值），享 zone-map + 谓词下推。
- 长尾键值落 `Map<Utf8,Utf8>`，由 Lance 字典/RLE 压缩；可被 SQL `attributes['k']` 取值，需要倒排时把对应 value 拼进 `body_text` 或单独建倒排列。
- 这一步在摄入归一化层完成（第 9 节），是"压缩 XOR 可检索"问题里"可检索"那一半的工程抓手。

---

## 3. 段格式与磁盘布局

### 3.1 复用 Lance 段（fragment）作为"列式不可变段"

每个 flush / compaction 产出一个或多个 **Lance fragment**，对应 SmithDB 的"列式不可变段"。Lance 段本身提供：
- **列式压缩**：Lance 2.x 自适应结构编码（值编码 + 字典/RLE/bit-packing/FSST 等），随机读友好（论文宣称比 Parquet 快约 100x 随机访问）。**同一份压缩字节既压缩又可随机读列** → 命中"压缩 XOR 可检索"的"压缩+随机读"那半。
- **zone-map**：列级 min/max 统计，谓词下推时跳段/跳页。
- **scalar 索引**：BTREE（数值/时间范围）、BITMAP（低基数如 service_name/status）、**INVERTED 倒排（FTS）**、**NGRAM（子串）**。
- **vector 索引**：IVF-PQ / IVF-SQ / HNSW。
- **soft delete**：`_deletions/{fragment_id}-{read_version}.arrow`，删除不重写数据文件。
- **版本/manifest**：每次 commit 产生新 manifest，天然 time-travel。

### 3.2 单机本地盘目录布局

```
/data/yitrace/
├── wal/                          # 自研 WAL（不走 Lance）
│   ├── 000000123.wal             # 顺序写, 段式滚动
│   └── checkpoint.meta           # 已 flush 的 _seq 水位
├── tenant=<t>/table=spans/
│   ├── _versions/                # Lance manifest (版本链)
│   ├── _latest.manifest
│   ├── data/                     # Lance fragment 数据文件 (.lance)
│   │   ├── L0/                   # 新鲜小段, 来自 flush
│   │   ├── L1/  L2/  L3/         # 时间分层 compaction 产物
│   ├── _deletions/               # soft delete 向量 (Lance 原生)
│   ├── _indices/                 # Lance scalar/vector/inverted 索引
│   └── _upgrade/                 # 自研 upgrade 旁路列 (见 4.3/5)
├── meta.db                       # 自研元数据(RocksDB 或 redb):
│                                 #   段目录 / 区间树持久化 / 活trace水位
└── catalog/                      # DataFusion catalog 描述
```

**为什么 WAL 不复用 Lance**：Lance 的写入单位是 fragment（批量），其 commit 是"先写数据文件再提交 manifest"的乐观并发模型，**不是为单行高频持久化设计的**（且有 2025-2026 单写者也偶发 commit-conflict 卡死的报告，见第 12 节）。trace 引擎需要"单条 span 落地即不丢"，所以崩溃恢复语义必须自研 WAL，Lance 只当"已成批的段"的归档格式。这是方案 A 里最重要的一处"自研补 Lance"。

### 3.3 时间分层（按 start_unix_nano 分桶）

段不是纯 size-tiered 而是 **time-tiered + size-tiered 混合**：
- L0：flush 直出，时间跨度小、可能乱序重叠（活 trace 的迟到 span）。
- L1：把 L0 同一时间窗（如 1 小时）的段归并去重折叠 → 段内有序、去多版本。
- L2：天级窗口；L3：周/月级窗口（冷数据，重压缩 + 可选下采样）。
每段 manifest 记录 `[min_ts, max_ts]`，进区间树（第 6 节）做查询路由。

---

## 4. LSM 写路径

### 4.1 写入流程（单条 / 批）

```
ingest(span_batch) →
  1. 归一化为内部 Schema 的 Arrow RecordBatch，分配 _seq（全局原子递增）
  2. 追加 WAL：把 RecordBatch 序列化(Arrow IPC) 顺序写 wal 文件
  3. 组提交 fsync（group commit，攒 N 条或 T 微秒一次 fsync）
  4. 写入 MemTable（进程内, 见 4.2）
  5. ack 客户端
  6. 更新活 trace 索引（区间树 + trace_id→未完成 span 缓冲）
```

**单写者串行化**：所有写入经过一个 `WriteCoordinator`（单逻辑写线程 / mpsc channel），保证 `_seq` 严格递增、WAL 顺序、避免 Lance 多写者 commit 冲突。摄入并发在 channel 之前（gRPC handler 多线程归一化），落盘串行。

### 4.2 MemTable 结构

MemTable = **Arrow 列式行组缓冲 + 内存索引**：
- 主数据：`Vec<RecordBatch>`（追加式，列式，零拷贝可直接喂 DataFusion）。
- 内存有序索引：`BTreeMap<(trace_id, span_id), Vec<MemRowRef>>` —— 同一 (trace_id,span_id) 的多个版本按 `_seq` 链在一起，**MemTable 内即可做折叠**（活 trace 查询走这里，免落盘也能查最新态）。
- 这避免了"未 flush 的 span 查不到"的活 trace 盲区。

### 4.3 Flush：MemTable → Lance L0 段

触发条件：MemTable 字节数 > 阈值（如 128 MB）或 时间 > 阈值（如 30s，保证近实时可查 + WAL 可回收）。

```
flush:
  1. 冻结当前 MemTable，开新 MemTable 继续接收写（双缓冲）
  2. 对冻结 MemTable 内同 (trace_id,span_id) 做"flush-time 预折叠"
     —— 只保留 MemTable 内的最新版本(降低 L0 段重复量, 但不做跨段折叠)
  3. 按 start_ts 排序 → 写成 Lance fragment 到 L0（Lance write append 模式）
  4. 同步/异步建段内索引：INVERTED(body_text, jieba) / BTREE(start_ts,duration)
     / BITMAP(service,status) / 可选 IVF/HNSW(embedding)
  5. Lance commit manifest 成功后，推进 WAL checkpoint 水位，回收旧 WAL 段
  6. 段 [min_ts,max_ts] 入区间树；meta.db 持久化段目录
```

**关键点**：flush 时做的是**段内**预折叠（同段内同 key 去旧），不是全局折叠。全局最新态由读路径 merge-on-read 跨段折叠保证（第 5 节）。这是"免原地 UPDATE"的核心——span 修正写成新版本追加，永不回改旧段。

### 4.4 写放大初步（详见第 7 节）

- WAL 1x（顺序写，可回收）。
- flush 1x（MemTable → L0）。
- 每层 compaction 把数据重写一次：L0→L1→L2→L3，约 3-4 次重写。
- 总写放大 ≈ 1(WAL) + 1(flush) + ~3(compaction) ≈ **5x**，时间分层使大部分重写只发生在新鲜窗口，老数据落 L3 后不再动。

---

## 5. Merge-on-read 读路径算法（多版本事件折叠）

这是 query-time fold 下沉为引擎读路径多路归并算子的核心。SmithDB 称之为 merge-on-read；我们用 DataFusion 自定义 `ExecutionPlan` 实现。

### 5.1 版本与删除/升级语义

同一逻辑 span 的标识键 = `(trace_id, span_id)`。它在不同段/MemTable 里可能有多条物理记录：
- **多版本 upsert**：span-start（end 为空）→ span-end（补 end/duration/status）→ 属性补写。各版本带 `_seq`。折叠规则：**按 _seq 取最新非空字段做列级合并（last-non-null wins per column）**，而非整行覆盖——因为不同上报只带部分列（OTLP 增量更新场景）。
- **deletion vector**：保留 / GDPR 删除 / 采样丢弃。用 Lance 原生 `_deletions/` soft delete 标记物理行失效；**不重写段**。
- **upgrade vector（自研旁路列）**：当需要对历史段批量改写某列（如：迟到的采样决策、span 重新打 service 标签、PII 脱敏替换），不重写整段，而在 `_upgrade/` 写"覆盖补丁段"——只含 `(trace_id, span_id, _seq, 被改列)`。读路径把 upgrade 补丁当作最高 `_seq` 的版本参与折叠。这就是"upgrade vector"在 Lance 之上的自研实现（Lance 本身只有 deletion，没有列级 upgrade vector）。

### 5.2 多路归并折叠算法

DataFusion 物理计划：

```
TraceScanExec(每个候选段一个 stream，已应用 zone-map/谓词/投影/deletion)
   ├── L0 段 stream (按 trace_id,span_id,_seq 排序输出)
   ├── L1 段 stream
   ├── ...
   ├── upgrade 补丁 stream
   └── MemTable stream
        │
        ▼
MergeOnReadExec  (k-way merge + fold)
```

算法（k 路归并，所有输入按 `(trace_id, span_id)` 全局有序）：

```
输入：k 个按 (trace_id, span_id, _seq) 排序的 RecordBatch 流
输出：每个 (trace_id, span_id) 一行（折叠后最新态），已剔除 tombstone

最小堆按 (trace_id, span_id) 取最小键：
  收集所有 stream 中 == 当前最小键 的全部版本行
  按 _seq 升序遍历这些版本：
     合并到 accumulator：对每列 last-non-null-wins
     若遇到 _op == delete-tombstone(_seq 最大处)：标记本 key 删除
  若被删除标记 → 跳过(不输出)
  否则输出 accumulator 一行
  堆推进到下一个键
```

要点：
1. **段内有序是前提**。flush/compaction 时按 `(trace_id, span_id, _seq)` 排序写段（注意：时间分层用 start_ts 分桶，但段内物理排序用 trace_id 主序——见 5.3 取舍）。
2. **谓词下推先于折叠**：DataFusion 把 `WHERE service='x' AND ts BETWEEN ...` 通过 `supports_filters_pushdown` 标为 Exact/Inexact 推进 `TraceScanExec`，先用 zone-map 跳段、用 scalar 索引/倒排过滤行，再归并。这样归并的输入量已大幅缩小。
3. **deletion 在 scan 层就应用**（Lance 原生读时跳过 `_deletions/` 标记行），归并层只处理逻辑折叠。
4. **聚合直接吃折叠后列**：上层 `AggregateExec`（P99、错误率）消费 `MergeOnReadExec` 输出，无需回堆——**这正是 openGauss 方案不可能做到的（见第 13 节）**。

### 5.3 两种排序键的取舍（诚实）

Trace 查询有两类主导模式：
- **(A) 按 trace_id 取整条 trace**（trace 详情页）。
- **(B) 按时间窗 + 过滤聚合**（service P99、错误 trace 列表）。

折叠算法要求段内按 trace_id 有序（A 友好、归并简单）；但时间分层 compaction 要求按 ts 分桶。解法：
- **分桶用 ts，桶内段物理排序用 trace_id**。同一时间桶内 (trace_id) 不跨桶重复（同一 trace 的 span 若跨时间桶——长 trace——则该 trace 的 span 散在相邻几个 ts 桶，区间树会同时命中这些桶，归并层仍能聚齐）。
- (B) 类查询靠 zone-map(ts) + BTREE 索引(ts) 跳到相关 ts 桶，再按 trace_id 归并折叠；因为只在命中桶内归并，代价可控。
- 真实代价：长 trace（span 时间跨度大）会让其 span 分布在多个 ts 桶，trace 详情查询需扫多桶。缓解：活 trace 阶段同一 trace 优先聚在同一 L0 段（按 trace_id 缓冲），且 compaction 时把"同 trace 跨桶碎片"在 L2/L3 重新按 trace_id 共置（locality compaction）。

---

## 6. 区间树（时间路由 + 活 trace）

区间树解决两件事：

### 6.1 段级时间路由
- 每个段 manifest 持 `[min_ts, max_ts]`。所有段的时间区间建成**内存区间树（centered interval tree / 或 segment-skiplist）**，持久化到 meta.db。
- 查询带 `ts BETWEEN a AND b` → 区间树 O(log N + k) 找出重叠段集合，只把这些段喂 `TraceScanExec`。比 Lance 自身列 zone-map 更早地在"段选择"阶段剪枝（zone-map 是段内/页内，区间树是段间）。

### 6.2 活 trace 区间
- 活 trace = 尚未收到 span-end / 仍在 MemTable 缓冲、未 flush 的 trace。
- 维护 `trace_id → [first_seen_ts, last_update_ts, open_span_count]` 的活 trace 表 + 一棵按 `last_update_ts` 的区间树。
- 用途：
  - **活 trace 查询**：直接命中 MemTable，返回"进行中"的部分 trace（gap span 标记 pending）。
  - **超时收尾**：扫描区间树找 `last_update_ts` 超过 trace timeout（如 10 min）仍未闭合的 trace，标记 `incomplete` 并允许 flush（否则永远占着 MemTable）。
  - **乱序/迟到 span**：迟到 span 的 trace 若已 flush，则作为新版本写入新段，读路径折叠时与历史段聚齐。

---

## 7. 时间分层 compaction 与写放大

### 7.1 分层策略

| 层 | 时间窗 | 触发 | 动作 |
|---|---|---|---|
| L0 | flush 直出(~30s 跨度) | 段数 > 8 或字节 > 阈值 | 同窗 L0 归并折叠 → L1 |
| L1 | ~1 小时 | L1 段数 > N | 按小时合并 → L2 |
| L2 | ~1 天 | 跨天 | 合并 + 重压缩 + 应用 upgrade/deletion 物化 → L3 |
| L3 | 周/月（冷） | 容量/TTL | 重压缩、可选下采样、locality 重排、TTL 删除 |

compaction 同时做：**折叠多版本（去重）+ 物化 deletion（真正删行而非软删，回收空间）+ 物化 upgrade 补丁（合进主列）+ 重建段内索引**。

### 7.2 写放大量化（< 1 亿 span/天 SKU）

设 1 亿 span/天、单 span 压缩后约 300 B → **30 GB/天 压缩落盘**。
- WAL：1x（顺序，flush 后回收，不计入长期占用）。
- flush(MemTable→L0)：1x。
- L0→L1：1x。L1→L2：1x。L2→L3：1x。
- **稳态写放大 ≈ 4x（落盘字节口径）**，即每天写 30 GB 数据触发约 120 GB 物理写。NVMe（典型 1-3 GB/s 顺序写、TBW 充足）轻松承受：120 GB/天 ÷ 86400s ≈ 1.4 MB/s 平均，峰值 compaction 时几百 MB/s。
- **空间放大**：旧版本段在 compaction 后、GC 前短暂双占；time-travel 保留窗口（如 24h）内旧 manifest/段不回收 → 峰值空间 ≈ 1.2-1.5x 稳态。
- **读放大**：merge-on-read 的代价 = 命中段数。时间分层 + 区间树把单次时间窗查询命中段数压到个位数～几十；折叠是 k 路归并 O(总行 log k)，可接受。

### 7.3 Lance compaction 的真实成本（诚实）
Lance 官方文档明确：**compaction 是最贵的写操作**，因为重写数据文件且默认 remap 所有索引。Lance 用 **Fragment Reuse Index (FRI)** 和**实验性 stable row id（`new_table_enable_stable_row_ids`，2025 roadmap）**来跳过索引 remap。我们的依赖与补丁见第 12/13 节——简言之：**我们不依赖 Lance 自带的"按 key upsert/merge insert"做折叠**（那条路在高频小写下会触发昂贵 remap），而是自管 LSM + 自己控制 compaction 节奏 + 在读路径折叠，把 Lance 当"只 append + 偶尔我们主动重写整桶"的段格式用。

---

## 8. 嵌入式 vs 单机服务形态

同一 `yitrace-core` crate，两种封装：

| 维度 | 嵌入式 | 单机服务 |
|---|---|---|
| 形态 | Rust crate / `cdylib`（C ABI）/ PyO3 | 独立进程 + 网络前端 |
| 写入 | 进程内 `engine.ingest(batch)` | OTLP gRPC / HTTP / LangSmith REST |
| 查询 | 进程内 DataFusion `SessionContext` | FlightSQL / PG-wire / HTTP+SQL |
| 并发写 | 调用方保证或内置 WriteCoordinator | WriteCoordinator 串行化 |
| 适用 | SDK 内嵌、边缘 agent 本地缓冲、单测 | 团队共享 trace 后端 |
| 进程模型 | 宿主进程 | tokio 多线程 runtime |

核心 API（两形态一致）：
```rust
engine.ingest(spans: RecordBatch) -> SeqNo            // 写
engine.session_ctx() -> SessionContext               // DataFusion 查询入口
ctx.sql("SELECT ... FROM spans WHERE ...").await      // SQL
engine.checkpoint() / engine.compact() / engine.gc()  // 运维
```

单机服务多出：连接管理、鉴权、限流、OTLP/LangSmith 编解码、metrics/health。**不引入** cluster manager、对象存储无状态层、分布式 compaction——这些是 SmithDB 有而我们砍掉的部分。

---

## 9. 对外接口

### 9.1 SQL（DataFusion）
- 注册 `TraceTableProvider`（实现 `TableProvider`）：
  - `scan(projection, filters, limit)` 返回 `TraceScanExec`。
  - `supports_filters_pushdown` 对 `ts`/`service`/`status`/`duration`/`trace_id` 等返回 **Exact**（我们用区间树+scalar 索引精确过滤），对复杂表达式返回 **Inexact**（DataFusion scan 后再过一遍保正确）。
  - 投影下推：只读查询涉及列（Lance 列式天然支持）。
- 之上 `MergeOnReadExec` 保证 SQL 看到的是折叠后逻辑视图（用户写普通 SQL，不感知多版本）。
- UDF：`fts_match(body_text, '中文查询')`、`vector_topk(embedding, $q, k)` 映射到 Lance 索引；或暴露为表函数 `trace_search(...)`。
- 前端：单机服务用 DataFusion 的 **FlightSQL** server（成熟）或 PG-wire（`pgwire` crate）。

### 9.2 OTLP 摄入
- gRPC：`opentelemetry-proto` 的 `TraceService/Export`（Tonic）。
- HTTP：`/v1/traces`（protobuf/JSON）。
- 归一化器：OTLP `ResourceSpans → ScopeSpans → Span` 拍平为内部 Span Schema；已知语义属性提列（第 2 节），其余进 Map；为 LLM/异常类 span 抽 `body_text`（中文 FTS 源）。

### 9.3 LangSmith 摄入
- 兼容 LangSmith run ingestion REST（`POST /runs/batch`、multipart）。
- LangSmith `run` → span 映射：`run.id→span_id`、`parent_run_id→parent_span_id`、`trace_id→trace_id`、inputs/outputs/prompt → `body_text`（中文 FTS）+ `embedding`（语义召回）。
- LLM run 的 token/cost/model 提为 typed 列。

---

## 10. 崩溃恢复（WAL）

### 10.1 WAL 设计
- 顺序追加、段式滚动（128 MB/段）。每条记录 = `(_seq, op, Arrow IPC batch bytes, crc32)`。
- **组提交**：攒 N 条或 T µs（如 1ms）一次 `fsync`，平衡吞吐与延迟；可配 `fsync=always`（强一致）/ `group`（默认）/ `async`（高吞吐弱保证）。
- `checkpoint.meta` 记录"已成功 flush 进 Lance 段并 commit manifest 的最高 `_seq`"。

### 10.2 恢复流程
```
启动:
  1. 读 Lance 最新 manifest → 确定持久段集合 + 已落盘 _seq 水位 (= checkpoint)
  2. 重放 WAL 中 _seq > checkpoint 的记录 → 重建 MemTable + 活trace索引
  3. 校验 crc，遇损坏尾部记录截断（最后一次未完成 fsync 的丢弃，已 ack 的必在 fsync 内）
  4. 区间树从 meta.db + 段 manifest 重建
  5. 开放读写
```

### 10.3 一致性边界（诚实）
- **WAL fsync 之前 ack 的写**在崩溃后可能丢（`async` 模式）；`group`/`always` 模式保证 ack=持久。
- **Lance commit 是另一处事务边界**：flush 时"WAL 已持久 + MemTable→段"，必须先 Lance commit manifest 成功、再推进 checkpoint。若 commit 后、checkpoint 前崩溃 → 重启会重放这批 WAL 造成段内重复版本，**由 merge-on-read 折叠天然幂等吸收**（同 (key,_seq) 取一份）。这是"自研 WAL + Lance 段"两层事务的接缝，靠折叠的幂等性弥合，是本设计的关键正确性论证点。

---

## 11. 内嵌中文倒排 · 区间树 · 向量带过滤 ANN（可检索性三件套）

### 11.1 中文倒排（Lance 内置 FTS，最大复用点）
- Lance scalar 索引支持 **INVERTED**（BM25）和 **NGRAM**（子串/`contains()`），分词器内置 **`jieba/default`（中文）**、`lindera/*`（日韩）、`icu`（Unicode 分词）、`simple`/`whitespace`/`raw`。
- 对 `body_text`（日志/异常栈/prompt/中文 span 名）建 `jieba` INVERTED 索引，支持短语查询（`with_position`）、布尔（AND/OR/NOT）、模糊（编辑距离）。
- **段内/共置**：索引随段建在 `_indices/`，与压缩列数据共置同一段版本——满足"同字节既压缩又可检索可随机读"（数据列压缩、倒排索引同段、随机命中行后从压缩列随机读取）。
- **FTS + 过滤**：Lance 支持 FTS 查询带 `filter` 表达式（先 scalar 索引过滤再 BM25 或反之），映射到 SQL `WHERE fts_match(...) AND service='x'`。
- **为什么这是杀手锏**：中文倒排（jieba）+ 列式压缩 + 同段共置 + BM25，是上一轮在 openGauss 里要从零做的最重模块，Lance 直接内置 → 省下数人月。

### 11.2 区间树
见第 6 节（段间时间路由 + 活 trace 收尾）。这是少数 Lance 不提供、必须自研的结构，但实现简单（成熟算法 + meta.db 持久化）。

### 11.3 向量带过滤 ANN（语义召回）
- 对 `embedding` 列建 Lance 向量索引：**IVF-PQ**（内存省、召回略损）/ **IVF-SQ** / **HNSW**（最准最快，内存大）。单机 SKU 优先 IVF-PQ（容量友好），高端 SKU 用 HNSW。
- **带过滤 ANN**：
  - **prefilter**（推荐）：在过滤列（service/ts/status）上建 scalar 索引，先用谓词缩小候选集再做向量 TopK → 召回正确（不会"先 TopK 再被过滤光"）。
  - **postfilter**：过滤太复杂或无 scalar 索引时，先 ANN 再过滤（可能召回不足，需放大 k）。
  - 映射到 DataFusion：`VectorTopKExec` 接受下推的 filter（prefilter）。
- **团队 IP 移植路径**：若 Lance 内置 IVF/HNSW 召回/延迟不达标，可把团队 DiskANN(C/C++) 包成 Rust lib，作为自定义 `VectorIndexProvider` 旁挂（向量索引建在我们的 `_indices/` 旁路，行定位用 (trace_id,span_id)，绕开 Lance row-address remap 问题）。这是"丢 yiTrace 内核资产复用"的对冲——向量 IP 仍可复用。

---

## 12. 单机容量天花板（量化 SKU）

> 口径：压缩后单 span ≈ 300 B（typed 列 + 字典 + map 长尾，实测 trace 数据普遍 200-500 B）。embedding 默认不全开（仅 LLM/采样 span 带），全开则另计。

| SKU | CPU | RAM | NVMe | 写入峰值 | 保留期 | 容量天花板 | 说明 |
|---|---|---|---|---|---|---|---|
| **S（嵌入式/边缘）** | 4c | 8 GB | 256 GB | ~5k span/s | 3-7 天 | ~2 亿 span / ~60 GB | MemTable 小、IVF-PQ、单租户 |
| **M（中小默认）** | 8-16c | 32 GB | 2 TB | ~30k span/s | 30 天 | **~30 亿 span / ~1 TB**（=1 亿/天×30天×含放大） | 命中"<1亿 span/天"目标 |
| **L（单机上限）** | 32c | 128 GB | 8-16 TB | ~150k span/s | 30-90 天 | ~100-200 亿 span / ~6-10 TB | HNSW 可开；写盘 ~1.4 MB/s 均值无压力 |

**天花板由什么决定（诚实）**：
1. **NVMe 容量** = 主硬约束。`保留期 × 日量 × 300B × 空间放大(~1.3)`。M SKU 2TB 盘约 30 天 @1 亿/天到顶。
2. **单写者吞吐**：WriteCoordinator 串行 + group-commit fsync 是写上限。实测 Arrow IPC 顺序写 + 组提交可达 10-30 万行/s（取决于 fsync 策略与 batch 大小）；超此需多表/多分区分摊（仍单机）。
3. **compaction 跟得上**：稳态写放大 4x，compaction 吞吐须 ≥ 4× 摄入。L SKU 高摄入时 compaction 是隐形天花板——CPU 与 NVMe 写带宽竞争。
4. **向量索引内存**：HNSW 全量驻留，`向量数 × D × 4B × 1.5`。1 亿 ×768d HNSW ≈ 460 GB → 单机放不下；故 L SKU 也只对**部分** span 开向量，或用 IVF-PQ（PQ 压缩到 ~1/16）。**这是单机最硬的天花板之一，必须如实标注。**
5. **MemTable + 活 trace 索引内存**：活 trace 多（长尾未闭合）会顶内存；用活 trace 区间树超时收尾兜底。

**超出单机即 Level-2**（对象存储 + 多节点），不在本设计范围。

---

## 13. 逐条根治：膨胀 / 压缩可检索 / fold（不回避）

### 13.1 膨胀
- **根因**：trace span 高频上报 + 同 span 多次修正，传统行存原地 UPDATE/DELETE 产生死元组、需 VACUUM，且历史无限增长。
- **根治**：
  1. **列式不可变段（Lance fragment）+ 追加写**：span 修正写新版本，**永不原地改旧段**，无死元组、无 VACUUM。
  2. **merge-on-read（deletion/upgrade vector）**：删除走 Lance soft-delete 向量（不重写段）；列级覆盖走自研 upgrade 旁路补丁（不重写段）。
  3. **时间分层 compaction**：定期把多版本折叠成单版本、物化删除/升级回收空间、冷数据重压缩 + 下采样 + TTL。写放大稳态 4x（第 7 节量化）。
  4. **字典/RLE/bit-packing/FSST**：低基数列（service/status/name）字典化，长尾 map 列压缩，压缩后 ~300B/span。
- **诚实剩余风险**：Lance compaction 贵（重写 + 索引 remap）；靠自管 compaction 节奏 + FRI/stable-row-id（实验性）+ 不依赖 Lance 自带 upsert 来规避（见 13.4 风险）。

### 13.2 压缩 XOR 可检索（既要压缩又要可检索可随机读）
- **根因**：传统上"压缩好"（大块、整体解压）与"可随机检索"（细粒度定位）矛盾。
- **根治**：Lance 段做到**同字节既压缩又随机可读**——
  1. 列式自适应编码：压缩比好，且 Lance 2.x 结构编码支持**列级随机访问**（不必整块解压，论文宣称比 Parquet 快约 100x 随机读）。
  2. **同段共置索引**：zone-map（段间/页内跳过）+ scalar 索引（BTREE/BITMAP）+ **INVERTED 中文倒排（jieba/BM25）** + NGRAM + 向量索引，全部建在同一段版本的 `_indices/`。
  3. 检索路径：倒排/scalar 命中 → 拿到行定位 → 从同段压缩列**随机读取**所需列（投影下推只读用到的列）。
  → 压缩与可检索**不再互斥**，因为压缩在"列编码"维度、可检索在"同段索引 + 列级随机读"维度，正交共存。
- **诚实剩余风险**：中文分词质量取决于 jieba 词典（专有名词/新词需自定义词典）；NGRAM 索引体积大（子串场景才开）。

### 13.3 query-time fold（折叠下沉为读路径多路归并算子）
- **根因**：span 多版本必须在查询时合并成"当前逻辑态"，否则用户看到重复/半截 span。
- **根治**：`MergeOnReadExec`（第 5 节）—— DataFusion 物理算子，对所有候选段 + upgrade 补丁 + MemTable 做 **k 路归并 + 列级 last-non-null-wins 折叠 + tombstone 剔除**。谓词/zone-map/deletion 在 scan 层先剪枝，折叠层只处理逻辑合并。聚合算子直接消费折叠后列。用户写普通 SQL，看到的就是折叠视图。**幂等**（重放 WAL 造成的重复版本被自动吸收，弥合 WAL/Lance 两层事务接缝）。

### 13.4 为什么自建 Rust 让 openGauss 那两处 CRITICAL 消失（核心论证）

**CRITICAL ①（已消失）：openGauss IndexScan 的 `amgettuple` 只能吐 TID、必须回堆取列判可见性。**
- openGauss 里：索引扫描只返回行指针（TID），引擎必须回堆（heap）读整行、再判 MVCC 可见性、再取列——对列式 trace 查询是灾难（回堆 = 随机 I/O + 反列式）。
- **自建 Rust 引擎为何消失**：我们**拥有整条查询执行**（DataFusion 自定义 `TableProvider`/`ExecutionPlan`）。`TraceScanExec` **直接从 Lance 列式段供折叠后的列**给上层算子——**根本没有"堆"这个概念，也没有 TID 回表**。投影下推让 scan 只读需要的列，倒排/向量命中直接在同段列式随机读。可见性由我们自己的 `_seq`/deletion/折叠语义定义，不走 PG MVCC。→ 死结消失。

**CRITICAL ②（已消失）：必须 fork openGauss 内核 + 写新 Resource Manager + bootstrap `pg_am` + 信创回炉。**
- openGauss 基线是 PG 9.2.4，**无 pluggable Table-AM**（PG12 才有），要塞列式存储引擎得 fork 内核、加 WAL Resource Manager、改 bootstrap 注册新 access method，且信创认证要重新回炉。工程量与认证风险巨大。
- **自建 Rust 引擎为何消失**：**没有 openGauss 内核**。WAL 是我们自研的简单顺序日志（第 10 节），不需要 PG 的 RM 框架；存储引擎就是 LSM + Lance 段，不需要 `pg_am` bootstrap；约束已解除**不需要信创回炉**。从零 Rust 引擎（DataFusion + Lance 现成积木）比 fork 一个 2013 年 PG 基线、无 Table-AM 的内核**简单、干净得多** —— 我们写的是"组装现代积木 + 三个自研件"，而不是"在十年前的 C 内核里动外科手术"。→ 死结消失。

---

## 14. 工程量（人月，对照团队 Rust 储备）

团队储备（已知）：写过 `bm25_benchmark`（Rust）、DiskANN（C/C++）。即**有 Rust 实战 + 向量/BM25 领域 IP**，但无"从零 Rust 存储引擎"经验。

| 模块 | 复用来源 | 自研量 | 人月 | 风险 |
|---|---|---|---|---|
| 段格式/磁盘布局 | Lance（几乎全包） | 目录约定 + meta.db | 0.5 | 低 |
| LSM 写路径 + MemTable + WriteCoordinator | 自研 | 全自研 | 2.0 | 中 |
| WAL + 崩溃恢复 + checkpoint | 自研 | 全自研 | 1.5 | 中（两层事务接缝） |
| MemTable 内多版本索引 | 自研 | 全自研 | 1.0 | 低 |
| **MergeOnReadExec 折叠算子** | DataFusion 框架 | 算子全自研 | 2.0 | **高（正确性核心）** |
| upgrade vector 旁路 | 自研（Lance 无） | 全自研 | 1.0 | 中 |
| 区间树（段路由 + 活 trace） | 成熟算法 | 实现 + 持久化 | 1.0 | 低 |
| 时间分层 compaction | 自研调度 + Lance 写段 | 调度/折叠/locality | 2.5 | 中高（节奏 vs Lance remap） |
| 中文倒排 FTS | **Lance 内置 jieba** | 接线 + 自定义词典 | 0.5 | 低（团队有 BM25 IP 兜底） |
| 向量带过滤 ANN | Lance 内置 / DiskANN 移植 | 接线（移植则 +2） | 1.0（移植 +2） | 中 |
| DataFusion TableProvider/ExecutionPlan/UDF | DataFusion | scan + pushdown 接线 | 1.5 | 中 |
| OTLP 摄入 | opentelemetry-proto + Tonic | 归一化器 | 1.0 | 低 |
| LangSmith 摄入 | 自研 | REST + 映射 | 1.0 | 低 |
| 单机服务（FlightSQL/PG-wire/鉴权/限流） | DataFusion FlightSQL + 生态 | 接线 | 1.5 | 低 |
| 嵌入式封装（crate/cdylib/PyO3） | 生态 | API + FFI | 0.5 | 低 |
| 运维（GC/metrics/health/备份） | 生态 | 接线 | 1.0 | 低 |
| 测试/基准/混沌（崩溃恢复正确性） | 自研 | 重点投入 | 2.0 | 高 |

**合计 ≈ 22 人月（不移植 DiskANN）/ ≈ 24 人月（移植）**。
- 关键路径（决定能不能用）：LSM 写路径 + WAL 恢复 + **MergeOnReadExec 折叠** + compaction 调度，约 8 人月，是风险与价值集中处。
- 可并行：摄入接口、服务前端、向量接线可与核心并行。
- **对照储备**：团队 Rust 实战足以做接线与算子；最大学习曲线在"LSM/WAL/compaction 调度的工程正确性"——这是团队没做过的，建议前 2 个月专攻 + 大量混沌测试。一个 4-5 人小队约 **5-6 个自然月**到可用 M SKU。

---

## 15. 风险登记（诚实，不回避）

| # | 风险 | 等级 | 缓解 |
|---|---|---|---|
| R1 | **Lance 高频小写产生过多小 fragment**，且其按-key upsert/merge 在小写下触发昂贵索引 remap | 高 | 不用 Lance upsert；自建 LSM 批量 flush（≥128MB/30s），高频写吸在 MemTable/WAL；compaction 节奏自控 |
| R2 | **Lance compaction 重写 + 索引 remap 贵**；stable-row-id 仍实验性（2025 roadmap）、FRI 缓存期才生效 | 高 | 自管 compaction、错峰、时间分层让老数据不再动；向量索引走自研旁路（不依赖 Lance row-address）；持续盯 Lance 版本 |
| R3 | **Lance 单写者偶发 commit-conflict 卡死**（2025-2026 社区有报告，含单写者） | 中高 | WriteCoordinator 严格单写者 + 重试上限告警 + 定期 manifest 健康检查 + 备份；锁版本 + 充分压测后再升级 |
| R4 | **WAL/Lance 两层事务接缝**导致重复或丢失 | 高 | 折叠幂等吸收重复；commit→checkpoint 严格顺序；混沌测试覆盖各 crash 点 |
| R5 | **折叠正确性**（列级 last-non-null、tombstone、upgrade 优先级）边界多 | 高 | 形式化折叠规则 + property-based 测试 + 与"全量重算"对拍 |
| R6 | **长 trace 跨时间桶**导致 trace 详情查询扫多桶 | 中 | 活 trace 同 trace 共置 L0；L2/L3 locality 重排 |
| R7 | **向量索引内存天花板**（HNSW 全量驻留）单机放不下大规模 | 高（物理） | 仅部分 span 建向量；IVF-PQ 压缩；如实标 SKU 上限（第 12 节） |
| R8 | **Lance 格式/API 仍在演进**（2.x，stable-row-id 实验性）锁版本成本 | 中 | 锁定 Lance 版本、抽象段访问层（便于将来切 Vortex）、关注 LFAI 动态 |
| R9 | **中文分词质量**（jieba 新词/专名） | 中 | 自定义词典 + 团队 BM25 IP 兜底；必要时 NGRAM 补召回 |
| R10 | **丢 yiTrace 内核资产**（向量/BM25 重获） | 中 | 向量移植 DiskANN（IP 仍可复用）；BM25 用 Lance 内置 + 团队基准校准 |
| R11 | **从零 LSM/WAL 工程量被低估** | 中 | 前 2 月专攻 + 混沌测试预算（R4/R5） |

### Plan B：Lance 若在高频小写/compaction 上证明不可接受
段访问层抽象后，可把**段格式切换为 Vortex**（自 0.36.0 文件格式稳定、LFAI 孵化、已在 Spice.ai 生产）。代价：Vortex **不内置 FTS/向量索引**，中文倒排需自研（tantivy + tantivy-jieba）、向量需自研/移植（DiskANN），且 Vortex 更激进/底层。即"用更多自研换格式可控"。本设计默认走 Lance（最大复用、最快），Plan B 留作工程逃生通道。

---

## 16. 关键设计决策摘要（一页）

1. **DataFusion 当大脑，Lance 当段格式 + 索引底座** —— 最大复用，砍掉 SmithDB 的分布式/对象存储层。
2. **自研三件套**：LSM 写路径 + WAL（Lance 不胜任单条高频持久化）、MergeOnReadExec 折叠算子（query-time fold 核心）、区间树（段时间路由 + 活 trace）。
3. **WAL 自研、Lance 只当段归档** —— 绕开 Lance 单写者 commit 脆弱性，崩溃恢复语义自控。
4. **折叠下沉为读算子**：列级 last-non-null-wins + tombstone + upgrade 旁路，免原地 UPDATE，根治膨胀。
5. **中文倒排直接用 Lance 内置 jieba INVERTED/BM25** —— 上一轮最重模块，现成省下数人月。
6. **向量带过滤 ANN 用 Lance prefilter（scalar 索引）+ DiskANN 移植旁路** 兜底召回/延迟。
7. **openGauss 两处 CRITICAL 因"拥有整条执行 + 没有 PG 内核"而消失**（第 13.4 节）。
8. **如实标天花板**：M SKU ~30 亿 span/1TB/30 天命中目标；向量内存与 NVMe 容量是单机最硬天花板。
9. **工程量 ~22 人月 / 4-5 人 5-6 月到可用**，关键路径在 LSM/WAL/折叠/compaction（团队未做过，需混沌测试重投入）。

---

## 17. 事实核对与不确定（2026-06，WebSearch 核对）

**已核实（有官方/社区来源支撑）**：
- Lance FTS 内置分词器含 `jieba/default`（中文）、`lindera/ipadic|ko-dic|unidic`、`icu`、`simple`/`whitespace`/`raw`；INVERTED（BM25）+ NGRAM；短语（`with_position`）、布尔、模糊；FTS 可带 filter。来源：Lance 官方 full-text-search 文档。
- Lance soft delete = `_deletions/{fragment_id}-{read_version}.arrow`，不重写数据文件；compaction 重写 + 默认 remap 索引、是最贵写操作；FRI 跳过 remap；建议批量插入避免小 fragment、否则定期 compaction。来源：LanceDB 文档 / DeepWiki / lance.org 性能指南。
- Lance stable row id（`new_table_enable_stable_row_ids`）跨 compaction/delete/merge 稳定，但**实验性、属 2025 roadmap**。来源：lance GitHub issue #3730 / discussion #3694。
- Lance 向量：IVF-PQ/IVF-SQ/HNSW；prefilter（需 scalar 索引）/ postfilter。来源：LanceDB 索引文档。
- Lance 单写者乐观并发 + manifest commit；**2025-2026 有单写者也偶发 commit-conflict / 不一致态的报告**。来源：lance GitHub issue #951/#2426/#3086。
- DataFusion 自定义 TableProvider 支持 `supports_filters_pushdown`（Exact/Inexact，Inexact 会 scan 后复检）、投影下推、流式并行 ExecutionPlan。来源：DataFusion 官方 custom-table-providers 文档 + 2026 博客。
- Vortex 文件格式自 0.36.0 起稳定、LFAI 孵化、已在 Spice.ai 生产；但不内置 FTS/向量。来源：Vortex 文档 / Spice.ai 博客 / GitHub。

**不确定 / 需上手验证（不回避）**：
- Lance 在"30s flush 节奏 + 每天数十次 compaction"下的真实索引 remap 开销与 stable-row-id 实测稳定性 —— **必须自己压测**，是 R1/R2/R8 的决断依据。
- 单 span 压缩后 ~300B 是经验估计，依赖 attributes 落列策略与真实数据基数，需用客户样本实测校准容量 SKU。
- WriteCoordinator 单写者 10-30 万行/s 上限取决于 fsync 策略/batch/盘，需基准。
- Lance row-address 在我们自研 upgrade 旁路 + 自管 compaction 下的交互（避免 remap 同时保证向量/倒排行定位正确）是工程难点，需 PoC 验证。
