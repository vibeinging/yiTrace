# yiTrace README 万星级优化评审

> 日期：2026-07-02
> 范围：根目录 `README.md` / `README.zh-CN.md`，参考 `docs/CURRENT_STATE.md`、`yitrace-engine/README.md` 和 SDK README。
> 目标：如果 yiTrace 要争取 GitHub 1 万 star，README 应如何改。

## 总评

当前 README 是“工程能力说明书”，不是“开源传播首页”。

技术含量很强：自研 Rust 引擎、零依赖、OTLP/OpenInference、中文检索、带过滤 ANN、eval、SDK、控制台都有。但 README 第一屏把这些能力压成了内部验收语言，读者看到的是“validation skeleton”“trait seams”“待接”，不是一个让人立刻想 star、clone、转发的项目。

**当前 README 万星潜力评分：6.5/10。**

- 技术独特性：9/10
- 首页传播力：5/10
- 新用户 60 秒成功率：6/10
- 可信度：7/10
- 生态对比清晰度：4/10
- 状态诚实度：8/10，但表达方式过度自损

## 第一优先级问题

### [P1] 首屏没有一句“可传播定位”

证据：`README.md:3` 的定义很长，塞了 7 个概念：single-node、single-directory、zero-dependency、database engine、Rust、multi-turn、tool calls、BM25、ANN、cost、eval。

问题不是不准确，而是不够可转述。用户不会在 Twitter/朋友圈/群里转发“a single-node single-directory zero-external-dependency AI agent observability database engine”。他们需要一句能复述的话。

建议改成更有抓力的定位：

```md
# yiTrace

**A single-binary trace database for AI agents.**

Run it locally, point OTLP/OpenInference or the SDK at it, and get trace replay,
Chinese search, vector recall, cost attribution, and evals without sending data
to a hosted observability service.
```

中文版本：

```md
# yiTrace

**给 AI Agent 用的单机 trace 数据库。**

一个 Rust 单二进制，把 Agent 的多轮对话、工具调用、多 Agent 协作 trace 灌进去，
本地完成 trace 回放、中文检索、向量召回、成本归因和 eval，不把数据送出内网。
```

### [P1] “validation skeleton” 放在第一屏，会杀掉 star 动机

证据：`README.md:10` badge 是 `status-validation skeleton`；`README.md:19` 首屏状态块再次强调 “validation skeleton”。

这对工程诚实是加分，但对 GitHub 首页传播是减分。读者第一反应会是：“还没做完，那我等你做完再看。”

建议：

- 首页 badge 改为更中性的 `status-alpha` 或 `status-preview`。
- “Not production ready” 留在 `Project Status` 段，不要压在截图上方。
- 第一屏状态强调“能跑什么”，然后用一行诚实边界。

推荐文案：

```md
> Status: alpha, runnable today. The storage, search, vector index, OTLP ingest,
> SDKs, and console are covered by offline tests. Security hardening and hosted
> deployment features are still roadmap items.
```

中文：

```md
> 状态：alpha，可本地运行。存储、检索、向量索引、OTLP 摄入、SDK、控制台都有离线测试覆盖。
> 安全加固和生产托管能力仍在路线图中。
```

### [P1] Quick Start 先跑测试，不是先让用户看到产品

证据：`README.md:41-53` 的第一个命令是 `cargo test --offline`。

这对 reviewer 很好，对潜在 star 用户不好。万星项目的 README 应该先让用户 60 秒内看到“东西活了”。测试应该是第二步。

建议 Quick Start 改顺序：

```bash
git clone https://github.com/<org>/yitrace
cd yitrace/yitrace-engine
cargo run -p yt-engine --example server
open http://127.0.0.1:7878
```

然后给一个“一条 curl 看到 trace”的最短路径。`cargo test --offline` 放到 `Development`。

### [P1] 缺“为什么不用 Langfuse / LangSmith / OpenTelemetry Collector”

当前 README 没有竞品对比表。对 AI observability 赛道，这是首页必须回答的问题。

建议放在 Quick Start 后面：

| If you need... | Use... |
|---|---|
| hosted SaaS tracing with team workflows | LangSmith / Langfuse |
| OpenTelemetry routing and metrics pipelines | OTel Collector |
| local/private trace storage with Chinese search + vector recall + eval loop | yiTrace |
| arbitrary analytics SQL over huge datasets | ClickHouse / DuckDB |

再给一个“yiTrace is different because”：

- one directory, no external services
- agent trace folding is native, not a generic log table
- Chinese BM25 and vector recall are first-class
- deterministic event ids make retries safe
- designed for private/on-prem/air-gapped environments

## 第二优先级问题

### [P2] README 状态和现状文档有明显漂移

例子：

- `README.md:19` 仍写 engine `122` tests、Python/TS SDK `8 each`。本轮验证后 engine 是 `123 passed / 1 ignored`，Python 是 `9 passed`，TS 是 `8` 条业务断言。
- `README.zh-CN.md:27` 写“中文走无词典的 CJK bigram 分词”，但 `docs/CURRENT_STATE.md` 当前态写的是默认纯 Rust `ChineseTokenizer`，内嵌 34.9 万词 jieba dict。
- `README.md:161` “Chinese tokenization is production-grade” 放在 “Still validation-grade or pending” 小节下，读起来互相打架。

建议把“测试数字”尽量从第一屏移走，或者改成不易漂移的表达：

```md
Verified by offline tests across storage, HTTP, OTLP, SDKs, search, vector recall,
restart recovery, and eval.
```

详细数字放到 `docs/CURRENT_STATE.md` 或 CI badge。

### [P2] 截图有，但没有“图在证明什么”

证据：`README.md:17` 直接放 `console-overview.png`，下面马上接状态块。

截图应该承担产品说服，不只是装饰。建议截图前加一句：

```md
Open the bundled console to replay multi-turn sessions, inspect spans, search Chinese trace text,
and drill into model/tool calls.
```

更进一步，做一个 20-30 秒 GIF：

1. 启动 server
2. SDK/curl 灌 trace
3. 控制台出现 session
4. 搜“盗刷”
5. 点开 span input/output

静态截图可以保留，但 GIF 对 star 转化更强。

### [P2] SDK 示例没有展示“发到引擎”

证据：`README.md:133-145` 用的是 `ConsoleExporter`。这适合 SDK 开发，不适合产品首页。

首页应该直接展示真实价值链：

```python
from yitrace import Tracer, HttpExporter

tracer = Tracer(
    exporter=HttpExporter("http://127.0.0.1:7878/v1/ingest", tenant_id=1),
    node_id=1,
)

with tracer.trace("AML screening", tenant_id=1) as t:
    with t.span("risk agent") as span:
        span.log("possible card fraud")
        span.set_tokens(input_tokens=900, output_tokens=120)

tracer.close()
```

然后下一行：

```bash
curl -XPOST localhost:7878/v1/search -d '{"text":"fraud","k":10}'
```

### [P2] 功能列表太像内部 checklist

`README.md:23-33` 的功能列表技术密度高，但没有按用户任务组织。建议改成 4 个 “jobs”：

1. Replay agent behavior
2. Search traces
3. Attribute cost
4. Build eval loops

每个 job 下面放 2 个技术点。读者先看到用途，再看到机制。

### [P2] 缺架构可信度的“实现状态表”

现在 `Current Status` 是大段 bullet。建议用表格：

| Area | Status | Notes |
|---|---|---|
| Storage + WAL + restart recovery | Done, tested | offline tests |
| OTLP/OpenInference ingest | Done | OTel GenAI + OpenInference |
| Python / TypeScript SDK | Done | deterministic event_id parity |
| Chinese tokenizer | Done in pure Rust | FFI jieba optional |
| Hosted auth/RBAC/TLS | Roadmap | local/on-prem first |
| LLM judge eval | Roadmap | rule scorer today |

这个表比“validation skeleton”更诚实，也更不劝退。

## 第三优先级问题

### [P3] 语言入口有小 bug

`README.md:5` 是 `**中文文档** · [English (this file)](README.md)`，中文文档不是链接。应该是：

```md
[中文](README.zh-CN.md) · English
```

中文 README 也类似，建议：

```md
中文 · [English](README.md)
```

### [P3] badge 可信度不足

当前 `crates` badge 是静态 badge，不是 crates.io 包状态。开源前建议换成真实 CI / release / docs badge：

- CI: GitHub Actions
- License
- MSRV
- `cargo test --offline`
- Python package / npm package，发布后再加

静态 “crates yt-*” 对新读者价值不大。

### [P3] 缺路线图和贡献入口

万星项目不只要“能用”，还要让人知道怎么参与。建议新增：

- `Roadmap`
- `Contributing`
- `Good first issues`
- `Security / Production notes`

## 推荐 README 结构

建议根 README 控制在 140-180 行，保持现在长度，但重排信息。

```md
# yiTrace

One-line positioning
3-line value prop
badges

Screenshot/GIF

## Why yiTrace
5 bullets, user-facing

## Quick Start
60-second server + console path

## Ingest From Your Agent
Python + TypeScript minimal examples

## Search / Replay / Eval
3 curl examples + screenshot

## How It Works
small architecture diagram
3 core mechanisms

## When To Use It
comparison table

## Project Status
done / alpha / roadmap table

## Repository Layout

## Docs

## License
```

## 可以直接替换的首屏草案

英文：

```md
# yiTrace

**A single-binary trace database for AI agents.**

Run it locally, point OTLP/OpenInference or the SDK at it, and get trace replay,
Chinese search, vector recall, cost attribution, and evals without sending agent
data to a hosted service.

[中文](README.zh-CN.md) · English

[badges...]

![yiTrace console](docs/images/console-overview.png)

yiTrace is for teams building agents that need private, inspectable traces:

- replay multi-turn sessions, tool calls, and multi-agent handoffs
- search Chinese trace text with BM25, then blend it with vector recall
- filter search by tenant, agent, status, and time
- attribute token cost per trace and per agent
- collect failed spans into eval datasets and track regressions

Status: alpha, runnable today. Storage, WAL recovery, OTLP ingest, SDKs, search,
vector recall, and evals are covered by offline tests. RBAC/TLS/hosted deployment
are roadmap items.
```

中文：

```md
# yiTrace

**给 AI Agent 用的单机 trace 数据库。**

一个 Rust 单二进制，把 Agent 的多轮对话、工具调用、多 Agent 协作 trace 灌进去，
本地完成 trace 回放、中文检索、向量召回、成本归因和 eval，不把数据送出内网。

中文 · [English](README.md)

[badges...]

![yiTrace 控制台](docs/images/console-overview.png)

yiTrace 适合正在做私有化 Agent 的团队：

- 回放多轮会话、工具调用、多 Agent 移交
- 用中文 BM25 搜 trace，再和向量召回混合
- 按租户、agent、状态、时间过滤
- 按 trace / agent 归因 token 成本
- 把失败 span 收进 eval 数据集，做回归评测

状态：alpha，可本地运行。存储、WAL 恢复、OTLP 摄入、SDK、检索、向量召回、eval
都有离线测试覆盖。RBAC/TLS/托管部署仍在路线图中。
```

## 执行顺序

1. 重写首屏：定位、截图说明、状态表达、语言链接。
2. Quick Start 改成“先起服务打开控制台”，测试命令移到 Development。
3. 加竞品/适用场景对比表。
4. SDK 示例改成真实 `HttpExporter` 上报。
5. `Current Status` 改成表格，清掉过时测试数字和分词描述。
6. 加 GIF 或至少补截图说明。

做到这 6 件，README 会从“工程审查能看懂”变成“开源用户愿意 star 并转发”。
