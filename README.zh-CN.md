# yiTrace

**给 AI Agent 用的单机 trace 数据库。**

一个 Rust 单二进制，把 Agent 的多轮对话、工具调用、多 Agent 协作 trace 灌进去，
本地完成 trace 回放、中文检索、向量召回、成本归因和 eval，不把数据送出内网。

中文 · [English](README.md)

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.80%2B-orange?logo=rust)](https://www.rust-lang.org/)
[![status](https://img.shields.io/badge/status-alpha-3fb950)](#项目状态)
[![engine](https://img.shields.io/badge/engine-std--only%20zero--dep-4b7fd1)](#工作原理)
[![OTLP](https://img.shields.io/badge/ingest-OTLP%20%2F%20OpenInference-7c3aed)](#从-agent-摄入)

打开自带控制台，可以回放多轮会话、查看 span、搜索中文 trace 文本，并下钻模型和工具调用。

![yiTrace 控制台](docs/images/console-overview.png)

yiTrace 适合正在做私有化 Agent 的团队：

- 回放多轮对话、工具调用、多 Agent 移交
- 用中文 BM25 搜 trace，再和向量召回混合
- 按租户、agent、状态、trace、时间过滤
- 按 trace、session、agent 归因 token 成本
- 把失败 span 收进 eval 数据集，持续做回归评测
- 单目录本地运行，引擎主体只用 Rust 标准库

> 状态：alpha，可本地运行。存储、WAL 恢复、OTLP 摄入、SDK、中文检索、
> 向量召回和 eval 都有离线测试覆盖。RBAC/TLS/托管部署仍在路线图中。

---

## 快速开始

需要 Rust 1.80+。

一键本地 demo：

```bash
./scripts/demo_all.sh
```

这条命令会构建控制台、启动引擎、灌入一条样例 trace，并打印可直接复制的搜索命令。
设置 `YT_DEMO_OPEN=1` 会自动打开控制台。

Docker：

```bash
docker compose up --build
```

然后打开 `http://127.0.0.1:7878`。

手动路径：

```bash
cd yitrace-engine
cargo run -p yt-engine --example server
```

服务会监听 `http://127.0.0.1:7878`，并自带 eval 种子数据。

另开一个终端：

```bash
curl -XPOST localhost:7878/v1/ingest \
  -H 'Content-Type: application/json' \
  -d '[
    {"trace_id":7,"span_id":1,"ts":1,"seq":1,"event_type":1,"ext_span_id":"7-1","agent_name":"风控","input_text":"疑似盗刷","logs":["开始"]},
    {"trace_id":7,"span_id":1,"ts":2,"seq":2,"event_type":2,"ext_span_id":"7-1","status":0,"duration_ns":4200000,"output_text":"需要人工复核","logs":["结束"]}
  ]'

curl localhost:7878/v1/traces

curl -XPOST localhost:7878/v1/search \
  -H 'Content-Type: application/json' \
  -d '{"text":"盗刷","k":10}'
```

按 agent / 状态过滤：

```bash
curl -XPOST localhost:7878/v1/search \
  -H 'Content-Type: application/json' \
  -d '{"text":"盗刷","k":10,"filter":{"agent_name":"风控","status":1}}'
```

可选鉴权：

```bash
YT_TOKEN=secret cargo run -p yt-engine --example server

curl localhost:7878/v1/traces \
  -H 'Authorization: Bearer secret' \
  -H 'X-Tenant-Id: 1'
```

## 控制台

引擎可以把 React 控制台作为静态资源内嵌进二进制。从源码运行时，先构建前端并拷进引擎 crate：

```bash
cd yitrace-console
npm install
VITE_API=http npm run build
rm -rf ../yitrace-engine/crates/yt-engine/console_dist
cp -r dist ../yitrace-engine/crates/yt-engine/console_dist

cd ../yitrace-engine
cargo run -p yt-engine --example server
```

然后打开 `http://127.0.0.1:7878/`。

控制台没有私有接口，它和任何第三方前端一样调用 `/v1/*` JSON API。
完整端点见 [HTTP API 文档](docs/API_REFERENCE.md)。

---

## Node / Electron 嵌入式 DB

对 Node 后端和 Electron 应用，yiTrace 也可以作为进程内嵌入式数据库使用。这不是直接读数据文件；
它通过 Node-API 加载 Rust engine，仍然复用同一套 WAL、manifest、折叠、检索和租户过滤逻辑。

```bash
npm install @yitrace/db
```

```ts
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
  },
});

events.startSpan({ spanId: "span-uuid", name: "风控研判", agentName: "风控 Agent" });
events.log({ spanId: "span-uuid", message: "疑似盗刷" });
events.endSpan({ spanId: "span-uuid", status: 0, durationNs: 12_000_000 });

await events.ingest(db);

const hits = await db.search({
  text: "盗刷",
  k: 10,
  filter: { attrs: { project_id: "agentic-data", skill: "review" } },
});
const trace = await db.trace("run-uuid");
const logEvents = trace?.spans?.flatMap((span) => span.logEvents ?? []) ?? [];
await db.close();
```

包目录是 `yitrace-node/`。Electron 应用建议在 main process 持有 `YiTraceDB`，renderer 通过 IPC 调用。
ESM `import` 和 CommonJS `require` 都支持。
内部调用的是引擎的进程内 `EngineJsonApi`，不会启动本地 HTTP server，也不会走 TCP socket。
direct ingest 支持数字 ID，也支持 UUID 这类外部字符串 ID。字符串 ID 会稳定 hash 成内部
`u64` key 用于索引，原文会以 `external_*` 字段返回。`attrs` 会真实持久化，并在 search、trace
和 span detail 响应里返回。`project_id`、`skill`、`mode`、`call_site` 支持精确 attrs 过滤。
trace 和 span detail 也会返回 `logEvents`，业务侧可以直接渲染 span 内过程日志，不需要把日志复制到
`attrs.event_logs`。`OpenOptions.readOnly` 暂不暴露；等 engine 有真正只读打开路径后再加。

`@yitrace/db` 对外是一个很小的 JS 入口包，native 二进制按平台拆成 optional packages
（例如 `@yitrace/db-darwin-arm64`、`@yitrace/db-linux-x64-gnu`）。用户只需要安装
`@yitrace/db`，npm 会按 OS/CPU 拉取匹配的二进制包，不要求用户机器安装 Rust toolchain。
维护者打包和发布步骤见 [`yitrace-node/README.md`](yitrace-node/README.md)。
公开 npm 发布前，可在 `yitrace-node/` 运行 `npm run pack:local` 生成可锁版本的 tarball，
放进 AgenticData 仓库或内部 npm 源。

---

## 从 Agent 摄入

Python：

```python
from yitrace import Tracer, HttpExporter

tracer = Tracer(
    exporter=HttpExporter(
        "http://127.0.0.1:7878/v1/ingest",
        tenant_id=1,
    ),
    node_id=1,
)

with tracer.trace("反洗钱筛查", tenant_id=1) as t:
    with t.span("风控 Agent") as span:
        span.log("疑似盗刷")
        span.set_tokens(input_tokens=900, output_tokens=120)

tracer.close()
```

TypeScript：

```ts
import { HttpExporter, Tracer } from "@yitrace/trace-sdk";

const tracer = new Tracer(
  new HttpExporter({
    url: "http://127.0.0.1:7878/v1/ingest",
    tenantId: 1,
  }),
  1,
);

tracer.trace("反洗钱筛查", (t) => {
  t.span("风控 Agent", (span) => {
    span.log("疑似盗刷");
    span.setTokens(900, 120);
  });
}, undefined, 1);

await tracer.close();
```

已经接了 OpenTelemetry 或 OpenInference？把 OTLP/HTTP JSON POST 到 `/v1/traces` 即可。
yiTrace 会把 OTel GenAI `gen_ai.*` 和 OpenInference `llm.*` 属性映射到同一套 trace 存储。

---

## 为什么是 yiTrace

大部分可观测性工具都能存 trace。yiTrace 的定位是：把 agent trace 当作可以检索、评测、私有化保存的数据。

| 你需要 | 更适合 |
|---|---|
| 托管 trace、prompt run、团队协作流程 | LangSmith / Langfuse |
| OpenTelemetry 路由、指标和管道胶水 | OpenTelemetry Collector |
| 超大通用事件表的 SQL 分析 | ClickHouse / DuckDB |
| 本地/私有化 agent trace 存储 + 中文检索 + 向量召回 + eval | yiTrace |

不同点：

- **默认私有**：一个本地进程，一个数据目录，不依赖外部服务。
- **Agent 原生记录**：多轮 session、span 树、工具、模型、token、eval score 都是一等字段。
- **重试安全摄入**：Rust、Python、TypeScript 都用确定性 `event_id = hash(ext_span_id, seq, event_type)`。
- **内置检索**：中文 BM25、带过滤向量召回、RRF 混合召回。
- **租户安全边界清楚**：租户从 `X-Tenant-Id` 请求头取，不信任 body 里的 tenant 字段。

---

## 工作原理

```text
SDK / OTLP
    |
    v
HTTP 摄入网关
    |
    v
WAL + 内存表 --flush--> 不可变段
    |                    |
    v                    v
BM25 / 向量 / 属性索引   读时折叠
    |                    |
    +------ 检索 / 回放 / 成本 / eval
```

三个机制撑住设计：

- **事件，而不是可变 span**：一个 span 写成 `SpanStart`、`SpanEnd`、日志和晚到属性，读时折叠成完整 span。
- **内容决定身份**：event id 是确定性的，重传和崩溃重放不会让 token 或成本算两遍。
- **四源折叠读**：一个快照里合并内存表、不可变段、删除位图和晚到补写块，用 `event_id` 去重。

引擎主体是 std-only Rust。Vortex 列式段、jieba FFI、外部 graph_index 这类重依赖都隔离在独立 crate，通过 trait 接缝接入。

---

## 项目状态

| 模块 | 状态 | 说明 |
|---|---|---|
| 存储、WAL、快照、重启恢复 | 已实现，有测试 | `cargo test --offline` 覆盖崩溃重放、compaction、GC、备份和重启 |
| HTTP API 与 OTLP/OpenInference 摄入 | 已实现 | `/v1/ingest`、`/v1/traces`、`/v1/search`、`/v1/sessions`、`/v1/metrics` |
| Python / TypeScript SDK | 已实现 | event id 与 Rust 引擎逐字节一致 |
| 中文分词与 BM25 | 已实现，纯 Rust | 词典 DAG + 最大概率 DP，内嵌 jieba 词典，支持用户词典 |
| 向量召回 | 引擎内已实现 | 磁盘多层 HNSW、带过滤搜索、L2/Cosine/IP |
| 控制台 | 可用 | React 前端可内嵌进引擎二进制 |
| eval 闭环 | Alpha | 当前是规则 scorer，LLM judge 在路线图中 |
| 生产安全 | 路线图 | TLS、RBAC、落盘加密、限流、持久审计 |
| 查询引擎 | 路线图 | 当前是手写查询路径，DataFusion 待接 |

运行验证：

```bash
cd yitrace-engine
cargo test --offline
```

可选 crate：

```bash
cd yitrace-segstore-vortex && cargo build      # Vortex 列式段
cd yitrace-tokenizer-jieba && cargo test       # jieba FFI wrapper，默认 mock
cd yitrace-vecindex-graph && cargo test        # graph_index FFI wrapper，默认 mock
```

---

## 仓库结构

```text
yitrace-engine/              # Rust 引擎 workspace，主体 std-only
  crates/
    yt-core                  # ids、event_id、fold、manifest 类型
    yt-manifest              # reader pin 协议和回收水位
    yt-wal                   # 崩溃安全 WAL frame
    yt-memtable              # 活数据和 gated eviction
    yt-engine                # 协调器、检索、eval、HTTP、OTLP、控制台资源
yitrace-console/             # React 控制台
yitrace-sdk/
  python/                    # Python 打点 SDK
  typescript/                # TypeScript 打点 SDK
yitrace-segstore-vortex/     # 可选 Vortex 段存储
yitrace-tokenizer-jieba/     # 可选 jieba FFI 分词
yitrace-vecindex-graph/      # 可选 graph_index FFI 向量索引
docs/                        # 设计文档、API 文档、当前态索引
```

想看工程实情，先读 [Current State](docs/CURRENT_STATE.md)。那里明确写了哪些已验证、哪些是 alpha、哪些仍在路线图中。

## License

MIT
