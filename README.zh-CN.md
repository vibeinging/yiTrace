# yiTrace

**给 AI Agent 用的可搜索执行轨迹。**

yiTrace 记录 Agent 真实走过的路径，并让这些路径可以被搜索：多轮对话、工具调用、模型调用、
token、错误、日志和 eval 信号。你可以用它排查 Agent run、回放决策过程、构建 eval 数据集，
也可以把它作为 Agent Memory 的执行历史底座。

中文 · [English](README.md)

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.80%2B-orange?logo=rust)](https://www.rust-lang.org/)
[![npm](https://img.shields.io/badge/npm-%40yitrace%2Fdb%200.1.1-cb3837?logo=npm)](https://www.npmjs.com/package/@yitrace/db)
[![PyPI](https://img.shields.io/badge/PyPI-yitrace--db%200.1.0-3775a9?logo=pypi)](https://pypi.org/project/yitrace-db/)

![yiTrace 控制台](docs/images/console-overview.png)

## 为什么做

Agent 需要的不只是聊天记录。它需要知道一次任务是怎么走的：调用了哪些工具，哪个 span
失败了，模型看到了什么、返回了什么，花了多少钱，上次哪条路径最有效。

yiTrace 给你一份本地可查的执行记录：

- 回放多轮对话、工具调用、复杂 agent run
- 中文 BM25 检索、向量召回、混合检索
- 按 tenant、agent、状态、时间、自定义 attrs 精确过滤
- 按 trace、session、agent 汇总 token 和成本
- 把失败或有价值的 span 收进 eval / dataset
- Python、Node、Electron、Rust 都能嵌入本地 DB

## 快速开始

按你的项目技术栈选安装方式。

### Python

Python / FastAPI 项目如果想在本地写入和搜索 trace，不需要单独起 yiTrace 服务：

```bash
python -m pip install yitrace yitrace-db
```

```python
from yitrace import DbExporter, Tracer, connect

with connect(path="./yitrace-data", tenant_id=1) as db:
    tracer = Tracer(exporter=DbExporter(db, tenant_id=1), node_id=1)

    with tracer.trace("风控复核", tenant_id=1) as trace:
        with trace.span("LLM 判断") as span:
            span.log("疑似盗刷")
            span.set_tokens(input_tokens=900, output_tokens=120)

    tracer.close()

    hits = db.search({"text": "盗刷", "k": 10})
    print(hits)
```

如果只想把 trace 发到一个运行中的 yiTrace server，只装轻量 SDK：

```bash
python -m pip install yitrace
```

```python
from yitrace import HttpExporter, Tracer

tracer = Tracer(
    exporter=HttpExporter("http://127.0.0.1:7878/v1/ingest", tenant_id=1),
    node_id=1,
)

with tracer.trace("风控复核", tenant_id=1) as trace:
    with trace.span("工具调用") as span:
        span.log("查询风控数据库")

tracer.close()
```

### Node / Electron

Node 后端或 Electron 主进程如果要本地写入和搜索 trace，用 `@yitrace/db`。它通过
Node-API 嵌入 Rust engine，不是在 JS 里直接解析数据库文件。

```bash
npm install @yitrace/db
```

```ts
import { YiTraceDB, createSpanEventBuilder } from "@yitrace/db";

const db = await YiTraceDB.open({ dataDir: "./yitrace-data", tenantId: 1 });

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

events.startSpan({ spanId: "span-uuid", name: "风控复核", agentName: "risk-agent" });
events.log({ spanId: "span-uuid", message: "possible card fraud" });
events.endSpan({ spanId: "span-uuid", status: 0, durationNs: 12_000_000 });

await events.ingest(db);

const hits = await db.search({
  text: "fraud",
  k: 10,
  filter: { attrs: { project_id: "agentic-data", skill: "review" } },
});

console.log(hits);
await db.close();
```

如果 TypeScript 应用只打点上报到 server，用更轻的 SDK：

```bash
npm install @yitrace/trace-sdk
```

```ts
import { HttpExporter, Tracer } from "@yitrace/trace-sdk";

const tracer = new Tracer(
  new HttpExporter({ url: "http://127.0.0.1:7878/v1/ingest", tenantId: 1 }),
  1,
);

tracer.trace("风控复核", (trace) => {
  trace.span("工具调用", (span) => {
    span.log("查询风控数据库");
  });
}, undefined, 1);

await tracer.close();
```

### 起一个本地 yiTrace server

多个进程或多台机器要写到同一个 TraceDB 时，用 server 模式。

```bash
python -m pip install "yitrace-db[server]"
yitrace-db serve --data-dir ./yitrace-data --bind 127.0.0.1:7878
```

然后通过 HTTP 搜索：

```bash
curl -XPOST http://127.0.0.1:7878/v1/search \
  -H 'Content-Type: application/json' \
  -H 'X-Tenant-Id: 1' \
  -d '{"text":"盗刷","k":10}'
```

完整 HTTP 契约见 [API Reference](docs/API_REFERENCE.md)。

## 应该安装哪个包

| 应用形态 | 安装 | 做什么 |
|---|---|---|
| Python 应用本地写入和搜索 trace | `pip install yitrace yitrace-db` | Python 进程内嵌入 DB |
| Python 应用只发 trace 到 server | `pip install yitrace` | 轻量打点 SDK |
| FastAPI 想挂 yiTrace route / server | `pip install "yitrace-db[server]"` | embedded DB 加可选 FastAPI/Uvicorn server |
| Node 后端或 Electron 本地写入和搜索 trace | `npm install @yitrace/db` | 通过 Node-API 嵌入 DB |
| TypeScript 应用只发 trace 到 server | `npm install @yitrace/trace-sdk` | 轻量打点 SDK |
| 已经有 OTel/OpenInference | 发 `POST /v1/traces` | 兼容 OTLP/OpenInference 摄入 |
| Rust 应用 | 源码依赖 | Rust SDK 和 embedded wrapper 都在本仓库 |

当前公开版本：

- npm：`@yitrace/db@0.1.1`、`@yitrace/trace-sdk@0.1.1`
- PyPI：`yitrace==0.1.0`、`yitrace-db==0.1.0`

## embedded 模式和 server 模式

**embedded 模式**是在你的应用进程里打开一个本地目录：

```python
db = connect(path="./yitrace-data", tenant_id=1)
```

```ts
const db = await YiTraceDB.open({ dataDir: "./yitrace-data", tenantId: 1 });
```

适合一个机器拥有这个 data dir 的场景。同一台机器上的多个 worker 进程可以同时打开；
yiTrace 会用 data-dir 锁串行化 open/write，并用 reader pin 保护快照清理。

**server 模式**是跑一个 yiTrace 进程，其他客户端通过 HTTP 写入：

```bash
yitrace-db serve --data-dir ./yitrace-data --bind 0.0.0.0:7878
```

适合多机器、多容器、网络文件系统不可靠的场景。不要跨机器共享同一个 embedded data dir。

## 控制台

yiTrace 自带 trace 回放控制台。使用包安装时，启动 Python server 后打开：

```text
http://127.0.0.1:7878/
```

从源码运行时，先构建 React 控制台并拷到 engine crate：

```bash
cd yitrace-console
npm install
VITE_API=http npm run build
rm -rf ../yitrace-engine/crates/yt-engine/console_dist
cp -r dist ../yitrace-engine/crates/yt-engine/console_dist
```

控制台调用的也是普通 `/v1/*` JSON API，和你自己写 UI 用的是同一套接口。

## 从源码构建

改 engine、console 或包封装时走源码路径。

```bash
git clone https://github.com/vibeinging/yiTrace.git
cd yiTrace

cd yitrace-engine
cargo test --offline
cargo run -p yt-engine --example server
```

Rust engine 主体刻意保持 std-only。较重的集成放在独立 crate 或包目录里。

包级检查：

```bash
# Python SDK
cd yitrace-sdk/python
python -m pytest

# TypeScript SDK
cd yitrace-sdk/typescript
npm install
npm test
npm run build

# Node embedded DB
cd yitrace-node
npm install
npm run build
npm test

# Python embedded DB
cd yitrace-db-python
python -m pytest
```

构建发版产物：

```bash
./scripts/package_release_artifacts.sh
```

GitHub Actions 只在推送 `v*` tag 时打包。可以用 `vX.Y.Z-only-python-sdk`
这类 tag 先跑单个包；用 `vX.Y.Z` 跑完整打包矩阵。

## 它怎么工作

```text
SDKs / OTLP / embedded package
    |
    v
EngineJsonApi or HTTP ingest
    |
    v
WAL + memtable --flush--> immutable segments
    |                         |
    v                         v
BM25 / vector / attrs indexes read-time fold
    |                         |
    +---------- search / replay / cost / eval
```

核心思路很简单：yiTrace 存的是事件，不是可变 span。一个 span 会被写成 start、log、end
和晚到属性事件。读取时再把这些事件折叠成完整 span。事件 ID 是确定性的：

```text
event_id = hash(ext_span_id, seq, event_type)
```

所以重试和崩溃重放不会重复计算同一个事件。

## 现在能用什么

yiTrace 面向本地开发、问题排查、eval 流程和产品集成，适合需要私有、可搜索 Agent Trace 的团队。

| 能力 | 你能得到什么 |
|---|---|
| 本地 TraceDB | WAL、重启恢复、快照读取 |
| Python | SDK 和 embedded DB 已发布到 PyPI |
| Node / Electron | embedded DB 已发布到 npm |
| TypeScript | 轻量打点 SDK 已发布到 npm |
| Rust | SDK 和 embedded wrapper 在本仓库 |
| 搜索 | 中文 BM25、向量召回、混合检索 |
| 过滤 | tenant、agent、状态、时间、attrs |
| 回放 | session、trace、span、log event 视图 |
| eval 数据 | annotation 和 dataset hooks |
| 控制台 | 本地回放 UI |

实现边界见 [Current State](docs/CURRENT_STATE.md)。

## 仓库结构

```text
yitrace-engine/              # Rust engine workspace
yitrace-console/             # React 回放控制台
yitrace-sdk/python/          # Python 打点 SDK: yitrace
yitrace-sdk/typescript/      # TypeScript 打点 SDK: @yitrace/trace-sdk
yitrace-sdk/rust/            # Rust 打点 SDK
yitrace-node/                # Node/Electron embedded DB: @yitrace/db
yitrace-db-python/           # Python embedded DB: yitrace-db
yitrace-db-rs/               # Rust embedded DB wrapper
yitrace-segstore-vortex/     # 可选 Vortex 段存储适配
yitrace-tokenizer-jieba/     # 可选分词适配
yitrace-vecindex-graph/      # 可选图索引适配
docs/                        # 公开 API 文档、当前态索引、截图
```

## License

MIT
