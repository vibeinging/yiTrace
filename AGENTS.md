# AGENTS.md

> 本文件给 AI 编程助手（Cursor / Claude Code / ZCode / 其他 agentic 工具）读，也供人类开发者参考。
> 它说清：**这个项目是什么、怎么构建测试、改代码要注意什么、以及如何对接 yiTrace（灌 trace / 查询 / 检索）**。
> AI 助手在动手改代码前应先读本文，避免破坏既定约定。

---

## 1. 项目是什么

**yiTrace** 是给 AI Agent 用的本地 trace SDK、回放控制台和嵌入式 TraceDB。Rust 引擎主体是自研的，可以作为独立 HTTP 服务运行，也可以被 Node / Electron / Python / Rust 应用嵌入到进程内。把 Agent（多轮对话、调工具、多 agent 协作）跑出来的 trace 灌进来，提供：

- trace 还原（事件折叠成完整 span）
- 中文 BM25 检索 + 带过滤的向量 ANN 召回 + 混合检索（RRF）
- 成本归因（token / 费用，per-agent）
- 评测闭环（eval：打分、回归数据集、per-agent 看板）
- 多租户隔离（tenant_id 全流程贯穿）

**关键约束（改代码时必须守住）：**

- **引擎主体零外部依赖、只用 Rust 标准库**。`cargo test --offline` 必须离线可过。重依赖（Vortex、N-API、PyO3 等）隔离在独立目录 / crate（`yitrace-segstore-vortex`、`yitrace-node`、`yitrace-db-python` 等），**不要把外部 crate 拉进 `yt-engine`**。
- **确定性 `event_id`** = `hash(ext_span_id, seq, event_type)`，跨 Python / TypeScript / 引擎逐字节一致。改 event 编码 = 破坏跨语言去重，必须有跨语言对账测试。
- **接缝优先于实现**：分词 / 向量索引 / 段存储都是 trait 接缝（`Tokenizer` / `GraphIndex` / `SegmentStore` / `Bm25Index`）。换实现不动上层。

---

## 2. 仓库结构

```
yitrace-engine/              # 引擎（Rust workspace，std-only 零依赖）— 当前承重代码
│   └── crates/
│       ├── yt-core/            # 核心类型：ids、确定性 event_id、不可变 Manifest、折叠算法
│       ├── yt-manifest/        # 单写者-多读者：快照 pin 协议、回收水位（正确性脊梁）
│       ├── yt-wal/             # 写前日志：fsync、崩溃安全帧、二进制编码
│       ├── yt-memtable/        # 活内存表：双水位 + 受 gate 的 evict
│       └── yt-engine/          # 协调器、四源折叠读、检索、eval、HTTP/OTLP、控制台
│           └── examples/       # demo / server / bench_qps / eval_harness
yitrace-segstore-vortex/     # Vortex 列式段（工作区外，隔离重依赖）
yitrace-tokenizer-jieba/     # cppjieba FFI（可选；引擎默认用纯 Rust ChineseTokenizer）
yitrace-vecindex-graph/      # graph_index FFI（可选；引擎默认用自研磁盘 HNSW）
yitrace-sdk/                 # 打点 SDK
│   ├── python/                  # yitrace 包（pyproject.toml 已配，纯标准库）
│   ├── typescript/              # @yitrace/trace-sdk（tsconfig + build 已配）
│   └── rust/                    # yitrace crate（纯 std Rust 打点 SDK）
yitrace-node/                # @yitrace/db：Node/Electron 嵌入式 DB（N-API，独立于 engine workspace）
│   └── npm/                     # NAPI-RS optional platform packages（darwin/linux/win）
yitrace-db-python/           # yitrace-db：Python 嵌入式 DB（PyO3/maturin，可选 FastAPI server）
yitrace-db-rs/               # yitrace-db：Rust 嵌入式 DB crate
yitrace-console/             # 控制台前端（React + Vite + TS，构建产物内嵌进引擎单二进制）
docs/                        # 公开 API 文档 / 现状索引 / 截图
```

> **`docs/CURRENT_STATE.md` 是现状的唯一权威入口**，新读者从那里看。

---

## 3. 构建与测试

**引擎（主力）：**

```bash
cd yitrace-engine
cargo test --offline                    # 全测试（含并发压测 + HTTP 往返 + ANN 召回 + 重启不丢）
cargo run -p yt-engine --example demo   # 可运行 demo：灌数据 → 折叠 → 中文搜 → 向量 → 混合召回
cargo run -p yt-engine --example server # 起 HTTP 服务（:7878，自带 eval 种子数据）
cargo run -p yt-engine --example bench_qps --release  # 真实 QPS 压测（务必 --release）
```

- **测试必须 `--offline` 能过**（守零依赖原则）。release 才跑 bench（debug 慢几十倍，数字无意义）。
- 改了 HTTP/控制台后，要重新构建前端并内嵌：`cd yitrace-console && VITE_API=http npm run build && rm -rf ../yitrace-engine/crates/yt-engine/console_dist && cp -r dist ../yitrace-engine/crates/yt-engine/console_dist`。
- 可选：`YT_TOKEN=secret cargo run ... --example server` 开 Bearer 鉴权；`cargo test -p yt-engine --features gzip` 含 gzip 解压。

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
# Rust
cd yitrace-sdk/rust && cargo test --offline   # 纯 std 打点 SDK
```

**嵌入式 DB 包：**

```bash
cd yitrace-db-python && python -m pytest      # Python embedded DB + FastAPI/CLI + 真实多进程 worker 测试
cd yitrace-db-rs && cargo test --offline      # Rust embedded DB crate
./scripts/package_mode_eval.sh                # 跨 Python/TS/Node/Rust 包形态回归，含 Python SDK clean consumer
./scripts/package_release_artifacts.sh        # 创建 v* tag 前先本地打包；GitHub Action 只在 tag push 时重复这条路径
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
- **命名**：crate `yt-*`，Rust 标识符 `yt_`，Prometheus 指标 `yt_*`，环境变量 `YT_*`，Python 包 `yitrace`，TS 包 `@yitrace/trace-sdk`，嵌入式 DB 包 `@yitrace/db`。顶层目录 `yitrace-*`。**不要引入旧前缀。**
- **N-API 包隔离**：`yitrace-node/` 可以依赖 NAPI-RS；这些依赖不得进入 `yt-engine`。`@yitrace/db` 只通过 Rust engine API 打开数据目录，不允许 JS 直接解析 WAL/manifest/segment 文件。嵌入式查询走 `EngineJsonApi` 进程内调用，不启动本地 HTTP server、不走 TCP socket。
- **恢复路径不能 eager load 大索引**：持久库 clean reopen 只恢复 manifest、WAL checkpoint 和控制状态；`trace_rollup.dat`、`filter_attrs.dat`、`bm25.dat`、`segment_bloom.dat` 按第一次相关查询加载。`filter_attrs.dat` 和 `bm25.dat` 只加载目录，postings 按命中字段/词读取，默认各受 64 MB 缓存预算约束；第一次写入前统一补齐。改恢复逻辑后必须跑 `scale_bench --phase open`，不能让启动重新随 span 数线性增长。
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

**Rust**（只打点，不嵌入 DB）：

```rust
use yitrace::{HttpExporter, TraceOptions, Tracer};

let exporter = HttpExporter::new("http://127.0.0.1:7878/v1/ingest")?.with_tenant_id(1);
let mut tracer = Tracer::with_exporter(exporter, 1);
tracer.trace_with_result("风控研判", TraceOptions::default().tenant_id(1), |trace| {
    trace.span_result("LLM 检查", |span| {
        span.log("疑似盗刷")?;
        span.set_tokens(Some(900), Some(120));
        Ok(())
    })
})?;
tracer.close()?;
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
| GET  | `/v1/sessions` | 会话列表（游标分页） |
| GET  | `/v1/sessions/:id/turns` | 一个会话的各轮 |
| GET  | `/v1/traces/:id` | 一条 trace 的折叠 span（瀑布） |
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

### 5.4 Python 嵌入式 DB

Python 项目如果只想打点上报，用 `yitrace` SDK + `HttpExporter`。如果还要在本地搜索 trace，安装 `yitrace-db`，再通过 `yitrace.connect(path=...)` 打开本地目录：

```python
from yitrace import DbExporter, Tracer, connect

with connect(path="./data", tenant_id=1) as db:
    tracer = Tracer(exporter=DbExporter(db, tenant_id=1), node_id=1)
    with tracer.trace("反洗钱筛查", tenant_id=1) as t:
        with t.span("交易风控") as span:
            span.log("疑似盗刷")
    tracer.close()
    print(db.search(text="盗刷", k=10))
```

`connect(url=...)` 返回 HTTP client，`connect(path=...)` 返回本地 embedded DB handle。`yitrace-db` 也提供可选 FastAPI router 和 `yitrace-db serve` CLI。embedded 模式支持同机多个进程打开同一个本地 data dir；引擎内部用 data-dir 锁串行化 open/write，并用 reader pin 保护跨进程快照回收。服务端用 `init_yitrace(...)` 时，可用 `runtime.health()` 查看 `enabled`、`mode`、`data_dir`、队列、drop、`last_error` 和锁等待；直接用 `YiTraceDB` 时可用 `db.lock_metrics()` 排查是否在等 DB 锁。多机器、网络文件系统或跨主机容器共享同一个 data dir 时，改用一个 yiTrace server，其他 worker 走 HTTP。`SpoolDbExporter` 是削峰、隔离 native 包或避免请求路径等待 DB 锁的可选方案，不再是同机多 worker 的必需方案。

### 5.5 嵌入式 Node / Electron DB

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
  attrs: { project_id: "agentic-data", skill: "review", mode: "auto", call_site: "worker.ts:10" },
});
events.startSpan({ spanId: "span-uuid", name: "风控研判", agentName: "风控 Agent" });
events.log({ spanId: "span-uuid", message: "疑似盗刷" });
events.endSpan({ spanId: "span-uuid", status: 0, durationNs: 12_000_000 });
await events.ingest(db);

const traces = await db.search({ text: "盗刷", k: 10, filter: { attrs: { project_id: "agentic-data", skill: "review" } } });
await db.close();
```

这不是直接读文件；它通过 Node-API 把 Rust engine 嵌进 Node 进程，并调用 `EngineJsonApi` 这个进程内 API 边界，仍然使用同一套 WAL 恢复、manifest、折叠、BM25、向量召回和租户过滤逻辑。推荐用 `createSpanEventBuilder` 隐藏 `seq`、`event_type`、`ext_span_id` 和 start/end 双事件；已有 wire event 时仍可直接 `db.ingest(events)`。direct `db.ingest()` 支持数字 ID 和 UUID 等外部字符串 ID：内部稳定 hash 成 `u64` 用于索引，原始值保留在 `external_trace_id` / `external_span_id` / `external_parent_span_id` / `external_session_id`；`attrs` 会贯穿 wire、折叠、WAL/segment/manifest 和查询输出，`project_id` / `skill` / `mode` / `call_site` 支持 search 和 sessions 精确过滤，JSON value 会按 string/number/bool/null/array/object round-trip。Electron 应用推荐在 main process 持有 `YiTraceDB`，renderer 通过 IPC 调用；打包时 `.node` 必须 asar unpack，不能裁剪 `@yitrace/db-*` optional native packages，可用 `NAPI_RS_NATIVE_LIBRARY_PATH` 指向自定义 native 文件。同机多个 Node 进程可以打开同一个本地 data dir；引擎内部用 `.yitrace.open.lock.d/`、`.yitrace.write.lock.d/` 和 `.yitrace.readers/` 管理跨进程 open/write/read。多机器或网络文件系统共享同一个 data dir 仍不支持。`OpenOptions.readOnly` 目前不暴露，传入会报错，直到 engine 提供真正只读打开路径。公开 npm 发布前，可用 `npm run pack:local` 生成带 commit/label 后缀的 root + 平台 optional package tarball，交给 AgenticData 用 `file:` 或内部 npm 源锁版本；不要长期覆盖复用 `0.0.1.tgz`。AgenticData server 侧必须选定单一 native 架构：当前默认 x64 时 DuckDB、yiTrace、sqlite 都保持 x64；若切 arm64，先把 DuckDB/sqlite 也切到 arm64 或 optional per-platform 策略，不能混用架构。交付前跑 `npm run pack:verify` 用干净 consumer 验证 ESM/CJS/native。

### 5.6 控制台

服务起来后浏览器开 `http://127.0.0.1:7878/`——前端已内嵌进引擎单二进制。左栏会话列表、中栏多轮时间线 + 瀑布、右栏 Span 详情。

---

## 6. 给 AI 助手的工作守则

1. **改代码前先跑 `cargo test --offline`** 确认基线绿，改完再跑一遍。
2. **不要在 `yt-engine` 加外部依赖**；要加重依赖，放进独立外部 crate。
3. **改 event 编码 / 折叠逻辑 / 检索算子**，必须更新或新增对应测试（这些是承重不变量）。
4. **改了前端**，记得重新 build 并拷到 `console_dist/`（否则引擎内嵌的是旧版）。
5. **改了 `yitrace-node/`**，至少跑 `npm run build && npm test`；如果影响发布结构，同步更新 `yitrace-node/README.md` 和本文件。
6. **改了 `yitrace-sdk/rust/`、`yitrace-db-python/` 或 `yitrace-db-rs/`**，至少跑对应包测试；如果影响 embedded 多进程、`connect(path=...)`、spool、FastAPI/CLI 或包边界，再跑 `./scripts/package_mode_eval.sh`。Python embedded 多 worker 必须保留真实 `multiprocessing` 子进程测试，不要只用同进程多 handle 代替。
7. **不确定就先读 `docs/CURRENT_STATE.md`**，它是现状权威。
8. **提交信息不带 AI 工具名**，写清 what + why。
