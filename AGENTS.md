# AGENTS.md

> 本文件给 AI 编程助手（Cursor / Claude Code / ZCode / 其他 agentic 工具）读，也供人类开发者参考。
> 它说清：**这个项目是什么、怎么构建测试、改代码要注意什么、以及如何对接 yiTrace（灌 trace / 查询 / 检索）**。
> AI 助手在动手改代码前应先读本文，避免破坏既定约定。

---

## 1. 项目是什么

**yiTrace** 是一个本地优先、可分片演进的 **AI Agent TraceDB**（Rust 自研）。它可以作为单个私有服务运行，也可以嵌入 Node/Electron，还可以通过 gateway + shard route table 走分布式数据路径。把 Agent（多轮对话、调工具、多 agent 协作）跑出来的 trace 灌进来，提供：

- trace 还原（事件折叠成完整 span）
- 中文 BM25 检索 + 带过滤的向量 ANN 召回 + 混合检索（RRF）
- 成本归因（token / 费用，per-agent）
- 评测闭环（eval：打分、回归数据集、per-agent 看板）
- 多租户隔离（tenant_id 全流程贯穿）
- 分片读写底座（route table、gateway fanout、snapshot lease、follower read、WAL replication 原语）

**关键约束（改代码时必须守住）：**

- **引擎主体零外部依赖、只用 Rust 标准库**。`cargo test --offline` 必须离线可过。重依赖（Vortex 等）隔离在独立 crate（`yitrace-segstore-vortex` 等），**不要把外部 crate 拉进 `yt-engine`**。
- **确定性 `event_id`** = `hash(ext_span_id, seq, event_type)`，跨 Python / TypeScript / 引擎逐字节一致。改 event 编码 = 破坏跨语言去重，必须有跨语言对账测试。
- **shard 内单写，cluster 层多写**：不要让多个进程直接写同一个 data dir；分布式扩展通过 route table、gateway、replication 和 snapshot lease 做，单 shard 的 WAL/manifest 正确性不能被绕开。
- **接缝优先于实现**：分词 / 向量索引 / 段存储 / shard client / route table 都是接缝。换实现不动上层。

---

## 2. 仓库结构

```
yitrace-engine/              # 引擎（Rust workspace，std-only 零依赖）— 当前承重代码
│   └── crates/
│       ├── yt-core/            # 核心类型：ids、确定性 event_id、不可变 Manifest、折叠算法
│       ├── yt-manifest/        # 单写者-多读者：快照 pin 协议、回收水位（正确性脊梁）
│       ├── yt-wal/             # 写前日志：fsync、崩溃安全帧、二进制编码
│       ├── yt-memtable/        # 活内存表：双水位 + 受 gate 的 evict
│       └── yt-engine/          # 协调器、四源折叠读、检索、eval、HTTP/OTLP、gateway、控制台
│           └── examples/       # demo / server / server_durable / bench_qps / eval_harness
yitrace-segstore-vortex/     # Vortex 列式段（工作区外，隔离重依赖）
yitrace-tokenizer-jieba/     # cppjieba FFI（可选；引擎默认用纯 Rust ChineseTokenizer）
yitrace-vecindex-graph/      # graph_index FFI（可选；引擎默认用自研磁盘 HNSW）
yitrace-sdk/                 # 打点 SDK
│   ├── python/                  # yitrace 包（pyproject.toml 已配，纯标准库）
│   └── typescript/              # @yitrace/trace-sdk（tsconfig + build 已配）
yitrace-node/                # @yitrace/db：Node/Electron 嵌入式 DB（N-API，独立于 engine workspace）
│   └── npm/                     # NAPI-RS optional platform packages（darwin/linux/win）
yitrace-db-python/           # yitrace-db：Python 嵌入式 DB（PyO3/maturin，独立于 engine workspace）
yitrace-db-rs/               # yitrace-db：Rust 嵌入式 DB crate（EngineJsonApi 轻封装）
yitrace-console/             # 控制台前端（React + Vite + TS，构建产物内嵌进引擎单二进制）
docs/                        # 设计文档 / 现状索引 / 分析
```

> **`docs/CURRENT_STATE.md` 是现状的唯一权威入口**，新读者从那里看，别被历史过程文档带偏。

---

## 3. 构建与测试

**引擎（主力）：**

```bash
cd yitrace-engine
cargo test --offline                    # 全测试（含并发压测 + HTTP 往返 + ANN 召回 + 重启不丢 + 分布式 eval）
cargo run -p yt-engine --example demo   # 可运行 demo：灌数据 → 折叠 → 中文搜 → 向量 → 混合召回
cargo run -p yt-engine --example server # 起 HTTP 服务（:7878，自带 eval 种子数据）
cargo run -p yt-engine --example server_durable -- ./data/yitrace  # 持久化服务样板
cargo run -p yt-engine --example bench_qps --release  # 真实 QPS 压测（务必 --release）
```

- **测试必须 `--offline` 能过**（守零依赖原则）。release 才跑 bench（debug 慢几十倍，数字无意义）。
- 改了 HTTP/控制台后，要重新构建前端并内嵌：`cd yitrace-console && VITE_API=http npm run build && rm -rf ../yitrace-engine/crates/yt-engine/console_dist && cp -r dist ../yitrace-engine/crates/yt-engine/console_dist`。
- 可选：`YT_TOKEN=secret cargo run ... --example server` 开 Bearer 鉴权；`cargo test -p yt-engine --features gzip` 含 gzip 解压。
- 改了 gateway / route table / replication / snapshot lease，至少跑相关真实 eval：`cargo test --offline -p yt-engine --test distributed_process_eval -- --nocapture`、`cargo test --offline -p yt-engine --test distributed_production_eval -- --nocapture`、`cargo test --offline -p yt-engine --test distributed_read_target_eval -- --nocapture`。

**外部 crate（隔离的重依赖，按需构建）：**

```bash
cd yitrace-segstore-vortex && cargo build     # Vortex（需 Rust 1.91+）
cd yitrace-tokenizer-jieba && cargo build     # jieba FFI（默认 mock，--features link 接真库）
cd yitrace-vecindex-graph && cargo build      # graph_index FFI（同上）
```

**SDK：**

```bash
# Python
cd yitrace-sdk/python && python -m build      # 出 wheel/sdist（pyproject.toml 已配）
python -m pytest                              # 跑测试
# TypeScript
cd yitrace-sdk/typescript && npm install && npm run build   # tsc 出 dist/
npm test                                      # tsx 跑测试
```

**Node / Electron 嵌入式 DB（`@yitrace/db`）：**

```bash
cd yitrace-node
npm install
npm run build       # 本机 N-API native binary，生成的 *.node 不入库
npm test            # Node 集成测试：ingest/search/traces/sessions/reopen/tenant/lock
```

对外 npm 发布必须走 NAPI-RS optional platform package 流程，不能把只包含当前机器 `.node` 的 root 包发出去：

```bash
cd yitrace-node
npm ci
npm run npm:dirs
npm run build:release -- --target x86_64-unknown-linux-gnu   # CI matrix 中按 target 分别跑
npm run release:artifacts                                    # 把 artifacts/*.node 拷进 npm/*/
npm run release:prepublish                                   # 只更新元信息；脚本禁止自动 publish optional 包
npm run pack:check
npm run pack:verify                                          # 生成带 commit/label 后缀的 tarball，并用干净 consumer 验证 ESM/CJS/native

# 先发布平台包，再发布 root 包
npm publish npm/darwin-x64 --access public
npm publish npm/darwin-arm64 --access public
npm publish npm/linux-x64-gnu --access public
npm publish npm/linux-arm64-gnu --access public
npm publish npm/win32-x64-msvc --access public
npm publish --access public
```

**Python 嵌入式 DB（`yitrace-db`）：**

```bash
cd yitrace-db-python
python -m pip install -e .      # maturin editable build，生成 PyO3 native extension
python -m pytest                # 打开 embedded DB、ingest/search/session/span/lock 测试
python -m pip install maturin
python -m maturin build --release --interpreter "$(command -v python)"  # 本机 wheel；正式发布需要多平台 wheel matrix
```

`yitrace-db` 和 `@yitrace/db` 一样，只通过 Rust engine API 打开数据目录，不允许 Python 直接解析 WAL/manifest/segment 文件。它调用 `EngineJsonApi` 进程内边界，不启动本地 HTTP server、不走 TCP socket。`yitrace` 仍是纯 Python 打点 SDK；`yitrace-db` 是嵌入式数据库包。

**Rust 嵌入式 DB（`yitrace-db` crate）：**

```bash
cd yitrace-db-rs
cargo test --offline          # 打开 embedded DB、ingest/search/trace/span/lock 测试
```

Rust crate 是 `EngineJsonApi` 的轻封装：应用侧用 `YiTraceDb`、`SpanEventBuilder`、`SearchQuery`，不要直接依赖 `WriteCoordinator`。它不引入新的 engine 依赖，也不启动本地 HTTP server。

**前端：**

```bash
cd yitrace-console && npm install && npm run dev     # 开发服务 :5180（默认 mock 数据）
VITE_API=http npm run build                          # 构建对接真实引擎的版本
```

---

## 4. 代码约定（AI 改代码前必读）

- **Rust**：edition 2021，`#![allow(dead_code)]` 在骨架 crate 里是刻意的（接缝实现待替换）。模块用中文 doc-comment 解释"为什么这么设计"，改代码要延续这个习惯（写清意图，不只是 what）。
- **零依赖**：要在 `yt-engine` 引入外部 crate，先想清楚能不能放进独立的外部 crate。引擎本体只 std。
- **测试是承重的**：每个不变量都有"真会失败的测试"（崩溃重放、召回对标、确定性 event_id 跨语言对账）。改逻辑前先看相关测试，改完跑全量。
- **命名**：crate `yt-*`，Rust 标识符 `yt_`，Prometheus 指标 `yt_*`，环境变量 `YT_*`，Python 打点 SDK 包 `yitrace`，Python 嵌入式 DB 包 `yitrace-db`（import `yitrace_db`），Rust 嵌入式 DB crate `yitrace-db`（import `yitrace_db`），TS 包 `@yitrace/trace-sdk`，Node/Electron 嵌入式 DB 包 `@yitrace/db`。顶层目录 `yitrace-*`。**不要引入旧前缀。**
- **N-API 包隔离**：`yitrace-node/` 可以依赖 NAPI-RS；这些依赖不得进入 `yt-engine`。`@yitrace/db` 只通过 Rust engine API 打开数据目录，不允许 JS 直接解析 WAL/manifest/segment 文件。嵌入式查询走 `EngineJsonApi` 进程内调用，不启动本地 HTTP server、不走 TCP socket。
- **PyO3 包隔离**：`yitrace-db-python/` 可以依赖 PyO3/maturin；这些依赖不得进入 `yt-engine`。`yitrace-db` 只通过 Rust engine API 打开数据目录，不允许 Python 直接解析 WAL/manifest/segment 文件。嵌入式查询走 `EngineJsonApi` 进程内调用，不启动本地 HTTP server、不走 TCP socket。
- **Rust DB crate 边界**：`yitrace-db-rs/` 是对外 Rust 包，允许依赖 `yt-engine`，但对应用侧只暴露 `YiTraceDb` / builder / query helper / `route_json`，不要把 `WriteCoordinator` 作为公开使用入口。
- **npm 发布**：`@yitrace/db` root 包只放 JS 入口（ESM + CommonJS）和类型声明；native binary 放在 `npm/*` 平台 optional packages。正式发布前必须跑 `npm run release:artifacts` + `npm run release:prepublish` + `npm run pack:check`，并先发布平台包再发布 root 包。
- **提交信息**：纯净的中文/英文描述，首行简短，body 说清 what + why。不带 AI 工具名。

---

## 5. 对接 yiTrace（灌 trace / 查询 / 检索）

### 5.1 起服务

```bash
cd yitrace-engine && cargo run -p yt-engine --example server
# → http://127.0.0.1:7878  （自带 eval 种子数据，开箱可看）
```

### 5.2 用 SDK 打点（推荐）

**Python**

```python
from yitrace import Tracer, HttpExporter

# 指向 yiTrace 服务；event_id 与引擎逐字节一致，重传/崩溃重放自动去重
tracer = Tracer(exporter=HttpExporter(url="http://localhost:7878/v1/ingest"), node_id=1)

with tracer.trace("反洗钱筛查", tenant_id=1) as t:
    with t.span("交易风控") as root:
        with root.span("LLM 研判") as child:
            child.log("研判结论 需人工复核")
            child.set_tokens(input_tokens=900, output_tokens=120)
            child.set_status(0)   # 0=ok, 非0=error
```

嵌套 `span` 自动建父子；每个 span 产出 `SPAN_START` + `LOG` + `SPAN_END`，引擎按 `(trace, span)` 折叠成一条完整 span。多轮会话用同一 `session_id` 串起。

**TypeScript**（同款语义，BigInt 处理大整数精度）：

```typescript
import { Tracer, HttpExporter } from "@yitrace/trace-sdk";
const tracer = new Tracer({ exporter: new HttpExporter("http://localhost:7878/v1/ingest"), nodeId: 1 });
```

### 5.3 用 HTTP 直接对接（OTLP 生态入口，零改动接入）

已埋点 OTLP/OpenInference 的应用**不改一行**即可灌入——`POST /v1/traces` 是标准 OTLP/HTTP 端点（OTel GenAI `gen_ai.*`、Arize `llm.*`）。

> **完整端点契约**（方法/路径/请求体/响应字段/curl 示例/鉴权/租户）见 [`docs/API_REFERENCE.md`](docs/API_REFERENCE.md)。写自己的前端或对接，照那份文档即可。⚠️ 注意：原始 API（`/v1/traces`、`/v1/search`）是 snake_case，控制台 API（`/v1/sessions`、`/v1/traces/:id` 等）是 camelCase，别混用。

| 方法 | 端点 | 用途 |
|---|---|---|
| POST | `/v1/ingest` | 灌入 SDK 线格式 JSON 批（自定义高效格式） |
| POST | `/v1/traces` | **OTLP/HTTP 标准端点**（生态入口，已埋点应用直接接） |
| GET  | `/v1/traces` | trace 列表 |
| POST | `/v1/search` | 中文检索 + 向量召回 + 混合，可带 `filter`（agent/状态/tenant/时间/attrs） |
| POST | `/v1/trace-search` | 跨 session 结构化 span 搜索（分页/排序/attrs/text contains） |
| POST | `/v1/trace-aggregate` | 对结构化搜索结果做 group-by 聚合（path mining / trace inbox stats） |
| POST | `/v1/trajectory-groups` | 按完整 trajectory signature 分桶，发现稳定路径候选证据 |
| POST | `/v1/trace-trajectories` | 按 traceSearch 过滤返回每条 trace 的物化 trajectory 摘要 |
| POST | `/v1/storage-stats` | 按 traceSearch 过滤统计 trace/span/event、估算字节和 metadata 引用 |
| POST | `/v1/retention-plan` | dry-run 生成 retention 计划，默认保护 annotation/dataset/active Golden Path/snapshot/eval/path memory 引用 |
| POST | `/v1/retention/apply` | 执行 segment-row 软删除；要求 `deleteBeforeTs`，跳过 MemTable/WAL tail 热 trace；可选 `compact:true` 压实并 reclaim |
| GET/POST | `/v1/retention-audits` | 查询 retention/apply 审计记录（策略、计数、trace id 样本） |
| POST/GET | `/v1/retention-policies` | 创建/查询持久化 retention TTL 策略 |
| POST | `/v1/retention-policies/run-due` | 显式执行到期 retention policies；不会自动后台删除 |
| POST | `/v1/traces/diff` | 比较两条 trace 的 route、step、trajectory、duration/token/cost/status 差异 |
| POST | `/v1/golden-paths` | 登记 Golden Path 候选资产（只存源 trace/snapshot 引用，不复制 trace payload） |
| GET  | `/v1/golden-paths` | 查询 Golden Path 候选/已确认路径（支持 task/status/attrs 过滤） |
| POST | `/v1/golden-paths/:id/status` | 确认、拒绝或废弃候选路径 |
| POST | `/v1/path-adherence` | 比较新 trace 是否遵循某个 Golden Path，只返回 trajectory 证据 |
| POST | `/v1/golden-path-evidence` | 导出 Golden Path 的 source/candidate 证据包 |
| POST | `/v1/golden-path-export` | 按稳定 JSONL schema 导出 confirmed Golden Path |
| POST | `/v1/golden-path-health` | 批量统计同 scope trace 对 Golden Path 的遵循分布 |
| GET  | `/v1/loops` | agent loop 摘要列表 |
| GET  | `/v1/loops/:loopId` | 单个 loop 的摘要、trace 列表和 span 列表 |
| GET  | `/v1/tasks/:fingerprint/traces` | 同类 task 的 trace 列表 |
| POST | `/v1/annotations` | 给 trace/span 追加业务 annotation |
| GET  | `/v1/annotations` | 查询业务 annotation |
| PATCH | `/v1/annotations/:id` | 更新 annotation 字段或 review 状态 |
| DELETE | `/v1/annotations/:id` | 软删除 annotation（状态变为 deleted） |
| POST | `/v1/dataset-associations` | 关联外部 dataset item 与 trace/span |
| GET  | `/v1/dataset-associations` | 查询外部 dataset item 关联 |
| GET  | `/v1/sessions` | 会话列表（游标分页） |
| GET  | `/v1/sessions/:id/turns` | 一个会话的各轮 |
| GET  | `/v1/traces/:id/snapshot` | 导出带 hash 的完整 trace snapshot |
| GET  | `/v1/traces/:id` | 一条 trace 的折叠 span（瀑布） |
| GET  | `/v1/traces/:id/spans` | span detail 分页列表 |
| POST | `/v1/traces/:id/spans/batch` | 批量 span detail |
| GET  | `/v1/traces/:id/spans/:spanId` | 单 span 大字段（晚物化）+ `logEvents` |

**检索示例：**

```bash
# 中文 BM25 + 按 agent/状态/attrs 过滤
curl -XPOST localhost:7878/v1/search \
  -d '{"text":"盗刷","k":10,"filter":{"agent_name":"风控","status":1,"attrs":{"project_id":"agentic-data","skill":"review"}}}'

# 纯向量找相似
curl -XPOST localhost:7878/v1/search -d '{"vector":[0.1,0.2,...],"k":10}'

# 关键词 + 语义混合（RRF 融合）
curl -XPOST localhost:7878/v1/search -d '{"text":"盗刷","vector":[0.1,0.2,...],"k":10}'
```

**多租户**：tenant 从 `X-Tenant-Id` 请求头取（非 body，客户端不能越权），`/v1/search` 与 `GET /v1/traces` 都按 tenant 隔离。

### 5.4 嵌入式 Node / Electron DB

Node 后端或 Electron 应用不一定要启动 HTTP server。发布到 npm 后，用户可以直接安装：

```bash
npm install @yitrace/db
```

```typescript
import { YiTraceDB, createSpanEventBuilder } from "@yitrace/db";

const db = await YiTraceDB.open({ dataDir: "./data", tenantId: 1 });
const events = createSpanEventBuilder({
  traceId: "run-uuid",
  sessionId: "session-uuid",
  attrs: {
    project_id: "agentic-data",
    skill: "review",
    mode: "auto",
    call_site: "worker.ts:10",
    task_fingerprint: "npm-native-packaging",
    validation_status: "pass"
  },
});
events.startSpan({ spanId: "span-uuid", name: "风控研判", agentName: "风控 Agent" });
events.log({ spanId: "span-uuid", message: "疑似盗刷" });
events.endSpan({ spanId: "span-uuid", status: 0, durationNs: 12_000_000 });
await events.ingest(db);

const traces = await db.search({ text: "盗刷", k: 10, filter: { attrs: { project_id: "agentic-data", skill: "review" } } });
await db.annotate({ traceId: "run-uuid", spanId: "span-uuid", label: "best_path", score: 920, projectId: "agentic-data" });
await db.linkDatasetItem({ datasetId: "best-path-regression", itemId: "case-1", traceId: "run-uuid", spanId: "span-uuid" });
const stats = await db.traceAggregate({ groupBy: ["taskFingerprint", "validationStatus", "toolName"], filter: { attrs: { project_id: "agentic-data" } } });
const candidates = await db.trajectoryGroups({ filter: { taskFingerprint: "npm-native-packaging" }, sort: "best" });
const trajectories = await db.traceTrajectories({ filter: { taskFingerprint: "npm-native-packaging", attrs: { project_id: "agentic-data" } } });
const storage = await db.storageStats({ filter: { taskFingerprint: "npm-native-packaging", attrs: { project_id: "agentic-data" } }, groupBy: ["projectId", "validationStatus"] });
const retention = await db.retentionPlan({ filter: { taskFingerprint: "npm-native-packaging" }, deleteBeforeTs: 1751540000000000000 });
const applyResult = await db.applyRetention({ filter: { taskFingerprint: "npm-native-packaging" }, deleteBeforeTs: 1751540000000000000, compact: true, requestedBy: "nightly-retention-policy", reason: "ttl cleanup" });
const retentionAudits = await db.retentionAudits({ filter: { source: "nightly-retention-policy" } });
const retentionPolicy = await db.createRetentionPolicy({ name: "nightly-retention-policy", intervalNs: 86_400_000_000_000, source: "nightly-retention-policy", reason: "ttl cleanup", query: { filter: { attrs: { project_id: "agentic-data" } }, olderThanNs: 30 * 86_400_000_000_000, compact: true } });
const retentionPolicyRun = await db.runRetentionPolicies({ nowNs: (BigInt(Date.now()) * 1_000_000n).toString(), limit: 10 });
const diff = await db.traceDiff("run-uuid", "candidate-run-uuid");
const golden = await db.createGoldenPath({ sourceTraceId: "run-uuid", taskFingerprint: "npm-native-packaging", score: 960, projectId: "agentic-data" });
await db.updateGoldenPathStatus(golden.goldenPathId, { status: "confirmed", reason: "manual accept", source: "human" });
const adherence = await db.pathAdherence(golden.goldenPathId, "candidate-run-uuid");
const evidence = await db.goldenPathEvidence({ goldenPathId: golden.goldenPathId, candidateTraceId: "candidate-run-uuid" });
const exportPage = await db.goldenPathExport({ filter: { taskFingerprint: "npm-native-packaging", projectId: "agentic-data" } });
const health = await db.goldenPathHealth(golden.goldenPathId, { filter: { projectId: "agentic-data" } });
const loops = await db.loops({ taskFingerprint: "npm-native-packaging" });
const loop = await db.loop("loop-builder");
const taskTraces = await db.taskTraces("npm-native-packaging", { validationStatus: "pass" });
await db.close();
```

这不是直接读文件；它通过 Node-API 把 Rust engine 嵌进 Node 进程，并调用 `EngineJsonApi` 这个进程内 API 边界，仍然使用同一套 WAL 恢复、manifest、折叠、BM25、向量召回和租户过滤逻辑。推荐用 `createSpanEventBuilder` 隐藏 `seq`、`event_type`、`ext_span_id` 和 start/end 双事件；已有 wire event 时仍可直接 `db.ingest(events)`。direct `db.ingest()` 支持数字 ID 和 UUID 等外部字符串 ID：内部稳定 hash 成 `u64` 用于索引，原始值保留在 `external_trace_id` / `external_span_id` / `external_parent_span_id` / `external_session_id`；`attrs` 会贯穿 wire、折叠、WAL/segment/manifest 和查询输出，`project_id` / `skill` / `mode` / `call_site` / `task_fingerprint` / `loop_id` / `harness_version` / `schema_fingerprint` / `intent_signature` / `validation_status` / `review_status` / `eval_status` / `path_memory_id` / `stop_reason` / `phase` / `validator` 已提升为一等字段，支持 search、sessions、traceSearch、traces、traceAggregate、trajectoryGroups、traceDiff、goldenPaths、loops 和 taskTraces 精确过滤/分组；`path_memory_id` 默认不进 postings，避免高基数字段撑大索引，但仍可通过折叠校验精确过滤。`pathAdherence` 复用同一租户和 trace id 边界做路径对比，JSON value 会按 string/number/bool/null/array/object round-trip。标准 usage/cost 字段也走同一链路：`provider`、`cached_input_tokens`、`reasoning_tokens`、`total_tokens`、`cost_usd` / `cost_usd_nanos`、`cost_currency` 可直接摄入；显式 cost 优先，缺失时按 provider/model 内置价格表估算，再回退默认 token 单价，`traceSearch` 支持 cost/token 范围过滤，查询输出保留旧 `cost` 的同时返回 `usage` / `costDetail.source`（`explicit` / `estimated_model_price` / `estimated_default` / `mixed`）。`db.annotate()`、`db.linkDatasetItem()` 和 `db.createGoldenPath()` 用独立 `metadata.dat` 保存后验标注、外部 dataset item source link 与 Golden Path 候选资产，tenant-scoped，随在线备份一起拷走；`db.updateAnnotation()` 可更新 `active/resolved/rejected/deleted` 状态和 review 字段，`db.deleteAnnotation()` 是软删除，默认查询、反向过滤和 retention 保护会忽略 deleted；`db.annotations()` / `db.datasetAssociations()` 支持 `cursor`/`limit` 分页，按 `createdAtNs`/id 倒序稳定返回；`db.traceSearch()` / `db.traceAggregate()` / `db.trajectoryGroups()` / `db.traceDiff()` / `db.goldenPaths()` / `db.pathAdherence()` / `db.goldenPathHealth()` / `db.loops()` / `db.loop()` / `db.taskTraces()` / `db.traces()` / `db.sessions()` 可用 `annotation` / `dataset` 反查被标注或已绑定数据集的 trace/span/session/loop/task；`db.storageStats()` 复用 traceSearch filter 统计 trace/span/event、payload/attrs/external id 估算字节和 metadata 引用；`db.retentionPlan()` 默认保护 annotation、dataset association、active Golden Path、snapshot、eval link 和 path memory 引用的 trace，`db.applyRetention()` 只软删除已 flush 的 segment rows 并跳过 MemTable/WAL tail 热 trace，传 `compact:true` 时会把 deletion vector 物化进新段并走 GC log 安全 reclaim，`db.retentionAudits()` 可查真实执行审计，`db.createRetentionPolicy()` / `db.retentionPolicies()` / `db.runRetentionPolicies()` 提供持久化 TTL 策略和显式 run-due 调度底座，不会在 embedded 进程里自动后台删除；`db.trajectoryGroups()` 按完整 trajectory signature 分桶并输出 success/eval/annotation/dataset/cost/duration 证据，`db.traceDiff()` 额外返回两条 trace 的 route、逐步变化和 duration/token/cost/status delta，`db.pathAdherence()` 比较新 trace 是否遵循某个 Golden Path 并返回 common/missing/extra steps，`db.goldenPathHealth()` 批量统计同 scope 后续 trace 的 followed/extended/partial/deviated 分布，供 golden path mining、回归对比和 challenger 策略使用；`db.createGoldenPath()` 只保存 canonical source trace/snapshot 引用和状态，重复命中/引用计数后续作为独立需求设计。Electron 应用应在 main process 持有 `YiTraceDB`，renderer 通过 IPC 调用；打包时 `.node` 必须 asar unpack，不能裁剪 `@yitrace/db-*` optional native packages，可用 `NAPI_RS_NATIVE_LIBRARY_PATH` 指向自定义 native 文件。一个 data dir 同时只允许一个写者，`.yitrace.lock` 会阻止多进程同时打开。`OpenOptions.readOnly` 目前不暴露，传入会报错，直到 engine 提供真正只读打开路径。公开 npm 发布前，可用 `npm run pack:local` 生成带 commit/label 后缀的 root + 平台 optional package tarball，交给 AgenticData 用 `file:` 或内部 npm 源锁版本；不要长期覆盖复用 `0.0.1.tgz`。AgenticData server 侧必须选定单一 native 架构：当前默认 x64 时 DuckDB、yiTrace、sqlite 都保持 x64；若切 arm64，先把 DuckDB/sqlite 也切到 arm64 或 optional per-platform 策略，不能混用架构。交付前跑 `npm run pack:verify` 用干净 consumer 验证 ESM/CJS/native。

`db.traceTrajectories()` 是逐 trace 的物化 trajectory read model：复用 `traceSearch` 过滤语义，返回 trace 摘要、scope fields、usage/cost 和稳定 trajectory signature，不读取 input/output/log 大字段。

Golden Path scope 已包含 `project_id` / `task_fingerprint` / `skill` / `mode` / `harness_version` / `schema_fingerprint` / `eval_profile` / `model` / `provider` / `tool_version`；`db.createGoldenPath()` 会保存轻量 `sourceTrajectory` 和 `evidenceSummary`，raw source trace 被 retention 清理后仍可做 path adherence/health 的底层对比。重复命中和引用计数后续仍作为独立需求处理。

`db.goldenPathEvidence()` 是只读证据包 API：默认返回 Golden Path source trace 的摘要、trajectory、annotation 和 dataset association；传 `candidateTraceId` 时附带 path adherence 与 trace diff。它不做最佳路径裁决，也不做重复 trace 压缩。

`db.goldenPathExport()` 是稳定导出 API：默认只导出 confirmed Golden Path，返回 `schemaVersion="yitrace.golden_path_export.v1"`、`items` 和 `jsonl`，供 Agent Memory / regression dataset 管线消费；显式传 `status` 才会导出 candidate/rejected/deprecated。

`db.goldenPathHealth()` 是只读持续校验证据 API：默认按 Golden Path 的 `taskFingerprint + attrs` 收窄并排除 source trace，返回后续 trace 的 followed/extended/partial/deviated 分布、coverage、轻量 examples 和 `governance.staleReasons`；Golden Path metadata 支持 `challengerOf`、`evalProfile`、`minSampleCount`、`marginScore`、`comparisonWindowNs` 等 Best/Challenger 证据字段，但它不做 BestPath 裁决，也不更新 Golden Path 状态。

### 5.5 控制台

服务起来后浏览器开 `http://127.0.0.1:7878/`——前端已内嵌进引擎单二进制。左栏会话列表、中栏多轮时间线 + 瀑布、右栏 Span 详情。

---

## 6. 给 AI 助手的工作守则

1. **改代码前先跑 `cargo test --offline`** 确认基线绿，改完再跑一遍。
2. **不要在 `yt-engine` 加外部依赖**；要加重依赖，放进独立外部 crate。
3. **改 event 编码 / 折叠逻辑 / 检索算子**，必须更新或新增对应测试（这些是承重不变量）。
4. **改了前端**，记得重新 build 并拷到 `console_dist/`（否则引擎内嵌的是旧版）。
5. **改了 `yitrace-node/`**，至少跑 `npm run build && npm test`；如果影响发布结构，同步更新 `yitrace-node/README.md` 和本文件。
6. **改了 `yitrace-db-python/`**，至少跑 `python -m pip install -e . && python -m pytest`；如果影响发布结构，同步更新 `yitrace-db-python/README.md` 和本文件。
7. **改了 `yitrace-db-rs/`**，至少跑 `cargo test --offline`；如果影响公开 API，同步更新 `yitrace-db-rs/README.md` 和本文件。
8. **不确定就先读 `docs/CURRENT_STATE.md`**，它是现状权威，别被 docs/ 下的历史过程文档误导。
9. **提交信息不带 AI 工具名**，写清 what + why。
