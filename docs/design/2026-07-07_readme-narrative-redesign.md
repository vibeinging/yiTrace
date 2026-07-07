# README 首屏叙事重设计（PR 候选）

> 日期：2026-07-07
> 类型：文案设计 / PR 候选
> 目的：把 README 首屏从"分布式 TraceDB"叙事切换到"Agent framework 嵌入式 TraceDB"叙事。
> 依据：[`research/2026-07-07_trace-landscape-research.md`](../research/2026-07-07_trace-landscape-research.md) §6/§7、[`research/2026-07-07_team-aggregation-research.md`](../research/2026-07-07_team-aggregation-research.md)
> 范围：只改 README 首屏（tagline、定位段、badges、Start Here、Run Modes 表）。中英双版同步。不动 API/SDK/对接章节。
> 落地：本文件是设计稿。确认后用 Edit 应用到 `README.md` 和 `README.zh-CN.md`。

---

## 一、叙事调整总览

| 维度 | 现状（降级） | 调整后（升级） |
|---|---|---|
| Tagline | "A local-first TraceDB for AI agents" | 保留，但首屏强支撑 |
| 首屏定位段 | 提到 shard gateway，把分布式当一等公民 | 嵌入式为主，shard gateway 降为"升级路径"脚注 |
| 核心场景 | 抽象的能力清单（search/replay/...） | **具体的 coding agent 痛点场景** |
| Run Modes 表 | Sharded gateway / Replicated shard 和嵌入式并列 | 嵌入式三件套在先、为"主推"；server 次之；**分布式合并为单行"升级路径"** |
| 首屏叙事重心 | "我们能做分布式" | **"agent 跑一晚上，明天能搜/回放/定位"** |

核心原则:
- **不删除**任何现有能力（gateway / replication / distributed 都保留），但**重新分层**:嵌入式 = 主打,server = 次选,分布式 = 升级路径(不在首屏喧宾夺主)。
- **不承诺**团队聚合能力(调研结论:当前阶段不做)。
- 诚实标注 alpha 边界,不自封"集群"。

---

## 二、英文版首屏文案（替换 `README.md` 前 ~120 行）

```markdown
# yiTrace

**The embedded TraceDB for AI agents.**

yiTrace is the trace database that lives inside your agent process — like SQLite
for application data, but for agent traces. Drop it into a coding agent, a
self-improving loop, or any LLM workflow, and every prompt, tool call, retry,
and multi-agent handoff becomes searchable, replayable evidence you can come
back to tomorrow.

[中文](README.zh-CN.md) · English

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.80%2B-orange?logo=rust)](https://www.rust-lang.org/)
[![status](https://img.shields.io/badge/status-alpha-3fb950)](#project-status)
[![engine](https://img.shields.io/badge/engine-std--only%20zero--dep-4b7fd1)](#how-it-works)
[![OTLP](https://img.shields.io/badge/ingest-OTLP%20%2F%20OpenInference-7c3aed)](#ingest-agent-runs)

![yiTrace console](docs/images/console-overview.png)

### Why an embedded TraceDB

Most LLM observability tools send your prompts and tool output to a hosted
service. That works for cloud-first teams, but not for agents that run on a
developer's laptop, inside an Electron app, or behind a company firewall — the
exact places where trace data is most sensitive and most useful.

yiTrace flips the default: **traces stay local by default**, queryable across
sessions, durable across crashes, and never leave your process unless you choose
to. The same Rust engine runs embedded in Node, Python, and Rust, or as a
private local server with a bundled console.

- **Local by default** — prompts, tool I/O, and logs stay in your data directory, not a SaaS
- **Searchable tomorrow** — Chinese & English BM25, attr filters, vector recall, hybrid RRF
- **Replayable** — fold raw events into spans, replay multi-turn runs and multi-agent handoffs
- **Crash-safe** — WAL + manifest + GC log; deterministic `event_id` survives kill -9 and replay
- **Cost & eval aware** — per-agent token/cost attribution, eval scores, annotations, dataset links

> Status: alpha, runnable today. The embedded engine, WAL recovery, SDK/OTLP
> ingest, Node/Electron package, read-model indexes, and retention are covered
> by offline tests. TLS/RBAC, persistent audit, team aggregation, and managed
> clustering are explicitly **not** in scope yet — see
> [Project Status](#project-status) and the
> [hardening plan](docs/plans/2026-07-07_next-phase-hardening-plan.md).

---

## Start Here

Embed yiTrace in Node.js or Electron (most common for coding agents):

\```bash
npm install @yitrace/db
\```

\```ts
import { YiTraceDB } from "@yitrace/db";

const db = await YiTraceDB.open({ dataDir: "./data", tenantId: 1 });
const hits = await db.search({ text: "card fraud", k: 10 });
\```

Embed yiTrace in Python:

\```bash
pip install yitrace-db
\```

\```python
from yitrace_db import YiTraceDB

db = YiTraceDB.open("./data", tenant_id=1)
hits = db.search(text="card fraud", k=10)
\```

Embed yiTrace in Rust:

\```toml
[dependencies]
yitrace-db = { path = "yitrace-db-rs" }
\```

\```rust
use yitrace_db::{OpenOptions, SearchQuery, YiTraceDb};

let db = YiTraceDb::open_with_options(OpenOptions::new("./data").tenant_id(1))?;
let hits = db.search(&SearchQuery::text("card fraud").k(10))?;
\```

Or run the local server and bundled console:

\```bash
./scripts/demo_all.sh
\```

Open `http://127.0.0.1:7878`, search for `盗刷`, and inspect the span input,
output, logs, token usage, cost, and eval evidence.

---

## Run Modes

| Mode | Use it when | What exists today |
|---|---|---|
| **Node / Electron embedded DB** | You want `import { YiTraceDB } from "@yitrace/db"` inside a coding agent or Electron app, no extra process | Node-API package, ESM/CJS, optional native packages, same Rust engine in-process |
| **Python embedded DB** | You want `from yitrace_db import YiTraceDB` inside a Python agent | PyO3/maturin package, same `EngineJsonApi` in-process boundary |
| **Rust embedded DB** | You want `use yitrace_db::YiTraceDb` in a Rust agent/backend | Thin Rust crate over `EngineJsonApi`, no N-API/PyO3 layer |
| **Local server + console** | You want private trace capture with a browser UI for replay | `cargo run -p yt-engine --example server`, HTTP JSON API, embedded console |
| **Durable single server** | You want one private data directory with restart recovery | `server_durable`, WAL, manifest, immutable segments, disk vector index |
| Distributed path *(upgrade route)* | You outgrow a single shard and need horizontal scale | route table, write routing, read fanout, partial/strict consistency, follower read targets, WAL replication primitives — **a verified data path, not a managed cluster** |

The default shape is **one process, one data directory, one writer** — the same
model SQLite made ubiquitous. Cluster-level scale (sharding, replication,
follower reads) is an upgrade route for when a single shard is no longer enough,
not the starting point.
```

---

## 三、中文版首屏文案（替换 `README.zh-CN.md` 前 ~120 行）

```markdown
# yiTrace

**Agent 的嵌入式 TraceDB。**

yiTrace 是一个跑在 agent 进程里的 trace 数据库——之于 agent trace，就像
SQLite 之于应用数据。把它接进 coding agent、自改进循环、或任何 LLM 工作流，
每一次 prompt、工具调用、重试、多 agent 协作，都会变成明天还能搜得到、回放得
出、查得清的证据。

[English](README.md) · 中文

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.80%2B-orange?logo=rust)](https://www.rust-lang.org/)
[![status](https://img.shields.io/badge/status-alpha-3fb950)](#项目状态)
[![engine](https://img.shields.io/badge/engine-std--only%20zero--dep-4b7fd1)](#工作原理)
[![OTLP](https://img.shields.io/badge/ingest-OTLP%20%2F%20OpenInference-7c3aed)](#灌入-agent-run)

![yiTrace 控制台](docs/images/console-overview.png)

### 为什么需要"嵌入式"TraceDB

大多数 LLM 可观测工具会把 prompt 和工具输出送到云端服务。这对云原生团队够用，
但对那些跑在开发者本机、Electron 应用里、或公司防火墙后面的 agent 来说不行
——而这些恰恰是 trace 数据最敏感、也最有价值的地方。

yiTrace 反转了这个默认：**trace 默认留在本地**，可跨 session 检索、崩溃不丢、
你不主动送出去就不会离开进程。同一套 Rust 引擎可以嵌进 Node / Python / Rust，
也可以作为带控制台的私有本地服务运行。

- **默认本地** —— prompt、工具 I/O、日志留在你的 data 目录，不去 SaaS
- **明天能搜** —— 中英文 BM25、attrs 过滤、向量召回、RRF 混合检索
- **能回放** —— 把原始事件折叠成 span，回放多轮 run 和多 agent 协作
- **崩溃不丢** —— WAL + manifest + GC log；确定性 `event_id` 扛得住 kill -9 和重放
- **成本和评测感知** —— per-agent token/成本归因、eval 分数、annotation、数据集关联

> 状态：alpha，今天就能跑。嵌入式引擎、WAL 恢复、SDK/OTLP 灌入、Node/Electron
> 包、读模型索引、retention 由离线测试覆盖。TLS/RBAC、持久审计、团队聚合、托管
> 集群**明确暂不在范围内** —— 见[项目状态](#项目状态)与
> [硬化计划](docs/plans/2026-07-07_next-phase-hardening-plan.md)。

---

## 从这里开始

嵌入 Node.js / Electron（coding agent 最常用）：

\```bash
npm install @yitrace/db
\```

\```ts
import { YiTraceDB } from "@yitrace/db";

const db = await YiTraceDB.open({ dataDir: "./data", tenantId: 1 });
const hits = await db.search({ text: "盗刷", k: 10 });
\```

嵌入 Python：

\```bash
pip install yitrace-db
\```

\```python
from yitrace_db import YiTraceDB

db = YiTraceDB.open("./data", tenant_id=1)
hits = db.search(text="盗刷", k=10)
\```

嵌入 Rust：

\```toml
[dependencies]
yitrace-db = { path = "yitrace-db-rs" }
\```

\```rust
use yitrace_db::{OpenOptions, SearchQuery, YiTraceDb};

let db = YiTraceDb::open_with_options(OpenOptions::new("./data").tenant_id(1))?;
let hits = db.search(&SearchQuery::text("盗刷").k(10))?;
\```

或运行本地服务和自带控制台：

\```bash
./scripts/demo_all.sh
\```

浏览器打开 `http://127.0.0.1:7878`，搜 `盗刷`，看 span 的 input、output、
日志、token 用量、成本和 eval 证据。

---

## 运行模式

| 模式 | 什么时候用 | 现状 |
|---|---|---|
| **Node / Electron 嵌入式 DB** | 想在 coding agent 或 Electron 应用里 `import { YiTraceDB } from "@yitrace/db"`，不起额外进程 | Node-API 包，ESM/CJS，optional native packages，同一套 Rust 引擎进程内调用 |
| **Python 嵌入式 DB** | 想在 Python agent 里 `from yitrace_db import YiTraceDB` | PyO3/maturin 包，同一 `EngineJsonApi` 进程内边界 |
| **Rust 嵌入式 DB** | 想在 Rust agent/后端里 `use yitrace_db::YiTraceDb` | `EngineJsonApi` 的轻封装，无 N-API/PyO3 层 |
| **本地服务 + 控制台** | 想私有采集 trace，用浏览器 UI 回放 | `cargo run -p yt-engine --example server`，HTTP JSON API，内嵌控制台 |
| **持久化单服务** | 想要一个私有 data 目录，重启可恢复 | `server_durable`，WAL、manifest、不可变段、磁盘向量索引 |
| 分布式路径 *（升级路径）* | 单分片扛不住，需要水平扩展 | route table、写入路由、读 fanout、partial/strict 一致性、follower read、WAL replication 原语 —— **是可验证的数据路径，不是托管集群** |

默认形态是**一个进程、一个 data 目录、一个写者** —— SQLite 让这种模型普及到
应用数据库，yiTrace 把它带到 agent trace。集群级扩展（分片、复制、follower
读）是单分片不够用时的升级路径，不是起点。
```

---

## 四、改动的关键叙事点（落地核对清单）

| # | 改动 | 现状原文 | 新文案 | 理由 |
|---|---|---|---|---|
| 1 | Tagline | "A local-first TraceDB for AI agents" | "The embedded TraceDB for AI agents" + SQLite 类比段 | 把"local-first"(模糊)升级为"embedded"(具体且可对标 SQLite/DuckDB) |
| 2 | 首屏定位段 | 提到 "behind a shard gateway" | 删除 shard gateway，改成嵌入式为主 + 本地隐私叙事 | 分布式不在首屏 |
| 3 | 新增 "Why an embedded TraceDB" | 无 | 解释"为什么不是又一个 SaaS observability" | 切入差异化定位 |
| 4 | Run Modes 表顺序 | Sharded gateway / Replicated shard 和嵌入式并列 | 嵌入式三件套在先；分布式合并为单行"升级路径" | 分层:主打 vs 升级路径 |
| 5 | 分布式行文案 | "Sharded gateway" + "Replicated shard" 两行 | 单行 "Distributed path *(upgrade route)*" + "verified data path, not a managed cluster" | 诚实标注边界 |
| 6 | 结尾新增一段 | 无 | "默认形态是 one process / one data dir / one writer... SQLite 模型" | 强化 SQLite 类比 |
| 7 | Status block | 提到 "automatic failover, fencing, background replication scheduling" 是 roadmap | 改成 "team aggregation, managed clustering **explicitly not in scope yet**" + 链硬化计划 | 团队聚合明确不做(调研结论) |
| 8 | "card fraud" → "盗刷" | 中文版用英文示例 | 中文版示例改中文 | 本地化 |

## 五、什么不改（避免误删）

- 所有 API/端点表、SDK 用法、对接章节:**不动**。这些是已落地能力,首屏叙事调整不影响它们。
- `Run Modes` 以下的所有内容(Quick Start / Ingest / Search / 等):**不动**。
- 仓库里实际存在的分布式代码:**不删**。只是 README 不再把它当主打。
- 例子:demo / server / server_durable / gateway_server / bench_qps / scale_bench / eval_harness:**都保留**。

## 六、落地步骤

1. 确认本设计稿(Review)。
2. 用 Edit 把 §2 应用到 `README.md` 的前 ~120 行。
3. 用 Edit 把 §3 应用到 `README.zh-CN.md` 的前 ~120 行。
4. `cargo run -p yt-engine --example demo` 跑一遍,确认示例代码块仍能跑(demo_all.sh 没变)。
5. 对照 §4 清单逐条核对。
6. 提交信息:中文,不带 AI 工具名,例如 "README 首屏叙事收敛:主打嵌入式 TraceDB,分布式降为升级路径"。

## 七、后续配套（非本 PR 范围,记录待办）

- `docs/CURRENT_STATE.md` 第 1 行项目定义同步("可分片演进"→"本地优先、可聚合")。
- `AGENTS.md` §1 项目定义同步。
- 首页截图 `docs/images/console-overview.png` 若有过时,后续换。
- 如认可 OpenCode/Hermes 接入示例方向,单独立项(不在本 PR)。
