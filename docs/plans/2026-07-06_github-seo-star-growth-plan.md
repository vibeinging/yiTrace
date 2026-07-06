# yiTrace GitHub SEO 和 Star 增长计划

> 日期：2026-07-06
> 范围：GitHub 仓库页、README、npm 包、示例、内容传播。
> 初始观察：公开仓库 `vibeinging/yiTrace` 约 3 stars；GitHub description 当时仍写 `single-node`，和当前 README 的“local-first, shardable TraceDB”不一致。

## 2026-07-06 执行进展

已完成：

- GitHub description 改为 `Local-first TraceDB for AI agents: replay runs, search traces, track cost/evals, and embed in Node/Electron or shard with a gateway.`
- GitHub homepage 设置为 `https://github.com/vibeinging/yiTrace#readme`
- GitHub topics 补充 `tracedb`、`trace-database`、`ai-agents`、`agent-memory`、`local-first`、`electron`、`nodejs`、`evals` 等搜索词。
- `README.md` / `README.zh-CN.md` 首屏改成更直接的 Agent TraceDB 定位，并提前加入 `@yitrace/db` 安装示例。
- `@yitrace/db` root 包、平台 optional packages 和 Node native crate 版本升到 `0.1.0`。
- 已用 `YITRACE_PACK_LABEL=0.1 npm run pack:verify` 生成并验证本地 tarball。

仍建议继续：

- 录制 20 到 30 秒 GIF demo。
- 创建 GitHub Release 页面。注意：当前已推送的 `0.1` tag 指向版本升级前的提交；如果要 release 包含 `0.1.0` npm 元数据，建议后续提交后新建 `v0.1.0` 或 `0.1.1`，不要强推移动已发布 tag。
- 补一篇面向传播的文章：`Agent Memory 需要 TraceDB，而不是聊天记录`。

## 核心判断

yiTrace 不能只按“数据库内核”传播。更容易让人 star 的说法是：

**给 AI Agent 用的本地优先 TraceDB：能回放运行、搜索 trace、分析成本、沉淀 eval 和 Agent Memory 证据。**

用户看到后要马上明白三件事：

1. 它解决什么痛点：Agent 跑错后，trace 数据不好查、不好复用、不想交给托管平台。
2. 它和 LangSmith / Langfuse / OTel Collector 不一样：yiTrace 是可嵌入、可私有、可分片的 TraceDB 底座。
3. 它今天能不能跑：能跑，有 demo，有截图，有 npm 包或 tarball，有离线测试。

## P0：先修 GitHub 仓库页

### 1. 更新 About description

执行前的公开 description 仍强调 `single-node`，容易让读者误解项目状态。2026-07-06 已修正。

建议改成：

```text
Local-first TraceDB for AI agents: replay runs, search traces, track cost/evals, and embed in Node/Electron or shard with a gateway.
```

中文传播时用：

```text
给 AI Agent 用的本地优先 TraceDB：回放运行、搜索 trace、分析成本/eval，可嵌入 Node/Electron，也可分片。
```

### 2. 设置 homepage

如果还没有文档站，先填：

```text
https://github.com/vibeinging/yiTrace#readme
```

后续最好做一个轻量文档站，例如：

```text
https://yitrace.dev
```

### 3. 调整 topics

GitHub topics 是站内搜索入口，不要只放底层技术词。建议控制在 20 个以内：

```text
ai-agents
agent-observability
ai-observability
llm-observability
tracedb
trace-database
agent-memory
opentelemetry
otlp
openinference
rust
database-engine
local-first
distributed-database
nodejs
electron
vector-search
bm25
chinese-search
evals
```

可以删掉或降级的词：

- `rag`：容易把项目带偏成 RAG 工具。
- `hnsw`：太底层，适合文档里讲，不适合作为主搜索入口。
- `tracing`：太泛，保留空间给 `tracedb` / `trace-database`。

## P0：README 首屏再压缩

当前 README 已经比旧版强很多，但还偏长。首屏目标是 30 秒内让人 star。

建议结构：

1. 一句话定位。
2. 一张动图或截图。
3. 3 条价值点。
4. 30 秒 Quick Start。
5. `npm install @yitrace/db` 嵌入式例子。

首屏推荐文案：

```md
# yiTrace

**A local-first TraceDB for AI agents.**

Replay agent runs, search traces, track cost and evals, and keep the evidence
inside your own process or private shard cluster.
```

中文：

```md
# yiTrace

**给 AI Agent 用的本地优先 TraceDB。**

回放 Agent 运行、搜索 trace、分析成本和 eval，把证据留在自己的进程、本地服务或私有分片集群里。
```

## P0：做一个真正能转化 star 的 demo

只靠 README 很难拿 star。需要一个“看一眼就懂”的 demo。

### 最小 demo 路径

```bash
git clone https://github.com/vibeinging/yiTrace
cd yiTrace
./scripts/demo_all.sh
```

然后 README 里明确说：

- 浏览器打开控制台。
- 搜 `盗刷`。
- 点开 span。
- 看 input/output/log/cost。

### 需要补的素材

1. 20 到 30 秒 GIF：启动、灌 trace、搜索、点 span。
2. 一张结构图：SDK/OTLP/@yitrace/db -> yiTrace -> replay/search/eval/memory。
3. 一个真实 Agent 例子：coding agent、客服 copilot、数据分析 agent 三选一。

## P0：发布 0.1 可安装产物

GitHub star 转化里，“能安装”很重要。

执行前已经打了 Git tag `0.1`，但 `yitrace-node/package.json` 还是 `0.0.1`。2026-07-06 已把 npm root 包和平台 optional packages 升到 `0.1.0`。正式对外前仍建议：

1. 先发 tarball 或 npm pre-release。
2. README 首屏放：

```bash
npm install @yitrace/db
```

```ts
import { YiTraceDB } from "@yitrace/db";

const db = await YiTraceDB.open({ dataDir: "./data", tenantId: 1 });
const hits = await db.search({ text: "盗刷" });
```

如果暂时不公开 npm，就放 tarball 安装方式，但要写清平台包：

```bash
npm install ./vendor/yitrace-db-0.1.0.tgz ./vendor/yitrace-db-darwin-arm64-0.1.0.tgz
```

## P1：关键词矩阵

README、npm description、docs title、文章标题要围绕这些词，不要每页换说法。

### 英文主关键词

- AI agent TraceDB
- local-first AI observability
- open-source AI observability
- agent trace database
- LangSmith alternative
- Langfuse alternative
- OpenTelemetry AI tracing
- OpenInference trace store
- Agent Memory trace evidence
- Node Electron embedded database for agents

### 中文主关键词

- AI Agent TraceDB
- Agent 可观测性数据库
- 本地 AI 可观测
- Agent trace 搜索
- Agent Memory 证据
- LangSmith 替代
- Langfuse 替代
- OpenTelemetry Agent 追踪
- Electron Agent 本地数据库
- 中文 trace 检索

注意：不要堆词。每个词都要出现在自然句子里。

## P1：补生态对比页

很多人点 star 前会问“为什么不用已有工具”。README 里要有短表，详细解释放 docs。

建议表：

| 你需要 | 更适合 |
|---|---|
| 托管平台、团队协作、prompt 管理 | LangSmith / Langfuse |
| OpenTelemetry 采集、路由、指标管道 | OTel Collector |
| 大规模通用 OLAP | ClickHouse / DuckDB |
| 私有 Agent TraceDB、嵌入式查询、中文检索、eval/Agent Memory 证据 | yiTrace |

这不是贬低竞品，而是帮用户快速判断。

## P1：补真实 Agent 案例

README 已有 Agent recipes，可以继续补三类可复制例子：

1. Coding Agent：一次任务失败多次，找出成功路径，生成 Golden Path。
2. 客服 Copilot：按 project/skill/mode 搜索异常回答，绑定 eval dataset。
3. Electron Desktop Agent：本地保存 trace，不出用户机器。

每个例子都要有：

- 10 行以内代码。
- 一张结果截图。
- 一句“这个例子解决什么问题”。

## P1：做 release 页面

GitHub release 本身也会被搜索和转发。

`0.1` release notes 建议写：

```md
yiTrace 0.1 is the first public preview of a local-first TraceDB for AI agents.

Highlights:
- Rust storage engine with WAL recovery and immutable segments
- SDK, OTLP/OpenInference ingest, and @yitrace/db embedded package
- BM25, vector search, attrs filters, trace aggregate, and trajectory groups
- eval, annotations, dataset links, and Golden Path evidence APIs
- sharded gateway and replication primitives with offline eval coverage
```

## P2：内容传播路线

### 第一波：技术社区

标题不要写“我做了一个数据库”。要写用户问题：

- `Agent Memory 需要 TraceDB，而不是聊天记录`
- `Why AI agents need a TraceDB`
- `Building a local-first TraceDB for AI agents in Rust`
- `From traces to Agent Memory: why runs should be searchable evidence`

### 第二波：对比文章

- `yiTrace vs LangSmith vs Langfuse: local TraceDB vs hosted observability`
- `Why OpenTelemetry is not enough for Agent Memory`
- `Trace search for Chinese AI agents`

### 第三波：工程文章

- `How yiTrace folds raw events into replayable spans`
- `How we avoid trace data bloat with immutable segments`
- `How Golden Path evidence is mined from agent traces`

## P2：GitHub 可信度

需要补这些基础设施：

1. GitHub Actions badge：Rust offline tests、Node tests、SDK tests。
2. `SECURITY.md`：告诉用户怎么报安全问题。
3. `CONTRIBUTING.md` 已有，但 README 要给入口。
4. `ROADMAP.md` 或 README roadmap：明确 alpha 边界。
5. Issue templates：bug、feature、integration request。
6. Discussions：开 `Show and tell`，鼓励用户贴 Agent 接入方式。

## P2：npm SEO

`@yitrace/db` 的 description 可以更强：

```text
Embedded local-first TraceDB for AI agents, Node.js, and Electron apps.
```

keywords 建议：

```text
ai-agent
tracedb
trace-database
agent-observability
ai-observability
llm-observability
opentelemetry
openinference
electron
node-api
vector-search
bm25
agent-memory
evals
```

## P3：Star 增长节奏

### 第 1 周

- 更新 GitHub description/topics/homepage。
- 发布 `0.1` release notes。
- README 首屏压缩。
- 录 GIF。
- 发一篇“Agent Memory 需要 TraceDB”的文章。

### 第 2 周

- 做 npm/tarball 安装页。
- 补 coding agent example。
- 发 Hacker News / Reddit / X / 掘金 / V2EX。
- 找 5 个 Agent 项目做 issue 或 PR，提供 yiTrace 接入例子。

### 第 3 到 4 周

- 做文档站。
- 补 LangSmith/Langfuse/OpenTelemetry 对比页。
- 做 Electron demo。
- 收集用户问题，转成 README FAQ。

## 衡量指标

不要只看 stars，还要看漏斗：

| 指标 | 目标 |
|---|---|
| README 首屏停留 | 截图/GIF 能让人继续往下看 |
| clone 后成功启动 | 3 分钟内完成 demo |
| npm/tarball 安装 | 一个命令安装 root + 平台包 |
| issue/discussion | 有真实接入问题 |
| stars | 第一阶段先看 100，再看 1000 |

## 最重要的下一步

先做这 5 件：

1. 录制 20 到 30 秒 GIF demo。
2. 发布包含当前 `0.1.0` 包元数据的新 release。不要移动已推送的 `0.1` tag。
3. 发一篇“Agent Memory 需要 TraceDB”的传播文章。
4. 做一个最小 coding agent example，让用户能照着接入。
5. 后续公开 npm 时，先发布平台 optional packages，再发布 root 包。
