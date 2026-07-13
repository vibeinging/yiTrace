# Trace Agent GitHub 竞品与需求调研

> 日期：2026-07-10
> 分支：trace-engineering
> 范围：GitHub 开源项目、官方文档和公开 issue。只做调研，不写代码。

## 结论

网上的需求是真实的，但竞争也比上一版调研判断得更激烈。

市场已经在快速收敛到下面这条链路：

```text
trace -> dataset -> eval -> optimization -> re-test
```

同时，2026 年开始出现另一个明显方向：

```text
让 Agent 通过 MCP / CLI / skills 自己查询 trace 和 eval 数据
```

所以，`yitrace-agent` 只做“让 Agent 搜索和下钻 trace”已经不够。Phoenix、LangWatch、Langfuse 和社区项目都已经在做。

yiTrace 仍然有机会，但要把位置收窄到：

> 给现有 Agent 框架提供本地可嵌入、按 token 预算取证、跨 run 路径比较、trace-to-eval 和候选路径持续验证的 Loop Engineering SDK。

这里真正可能形成差异的不是“有 trace 工具”，而是：

1. embedded 模式，不要求先部署一套平台。
2. Agent 先拿摘要，再按需下钻，不把完整 trace 塞进上下文。
3. 以 task / loop 为单位比较多次运行，而不是只看单条 trace。
4. 从生产历史产生 eval 和 Best Path Candidate，并保留证据、适用范围和后续验证结果。
5. 同一套 API 可以走本地 DB，也可以走 yiTrace server。

## 调研方法

本次查看了三类材料：

- GitHub 仓库 README、目录和发布状态。
- 官方文档里的 tracing、eval、MCP、CLI、optimizer 能力。
- GitHub issue 里的真实限制、需求和后续讨论。

Stars 只用于判断项目关注度，不直接等于产品需求。更有价值的证据是：多个项目同时建设相似能力，以及 issue 中用户反复提出相同问题。

下面的 stars 是 2026-07-10 的 GitHub API 快照，会随时间变化。

## 主要竞品

| 项目 | Stars 快照 | 已有能力 | 对 `yitrace-agent` 的威胁 |
|---|---:|---|---|
| [Langfuse](https://github.com/langfuse/langfuse) | 30.8k | tracing、eval、dataset、prompt、实验、托管/自建、MCP | 高：Agent 已能读取 observation、metrics、scores、datasets，并创建回归数据 |
| [Opik](https://github.com/comet-ml/opik) | 20.5k | tracing、eval、在线规则、dataset、Agent Optimizer | 高：已经把 prompt、tool、workflow 优化和 trace 证据接起来 |
| [Phoenix](https://github.com/Arize-ai/phoenix) | 10.5k | tracing、eval、dataset、experiment、replay、PXI、MCP、CLI | 很高：最接近“Agent 自己看 trace 并做工程优化” |
| [OpenLLMetry](https://github.com/traceloop/openllmetry) | 7.3k | OTel 自动打点、框架和模型接入 | 生态层：不应正面竞争，应兼容其事件 |
| [AgentOps](https://github.com/AgentOps-AI/agentops) | 5.7k | Agent 监控、成本、回放、框架集成 | 中：强在低成本接入和回放，闭环相对弱 |
| [LangWatch](https://github.com/langwatch/langwatch) | 3.4k | trace、dataset、eval、prompt 优化、simulation、MCP | 很高：产品口径已经是完整 improvement loop |
| [Weave](https://github.com/wandb/weave) | 1.1k | 函数 tracing、eval、实验数据组织 | 中：偏通用 AI 开发平台 |
| [Scenario](https://github.com/langwatch/scenario) | 914 | 多轮 Agent 仿真、judge、断言、红队 | 中：直接竞争 eval runner，不负责底层 trace DB |
| [langfuse-mcp](https://github.com/avivsinai/langfuse-mcp) | 100 | 本地 MCP、紧凑 trace 下钻、skill、dataset 工具 | 小而直接：证明“Agent 自己查 trace”已有独立需求 |

## 竞品做到哪一步

### Phoenix：最接近 Trace Agent

Phoenix 已经不只是 observability UI：

- 通过 OpenInference / OpenTelemetry 接多种 Agent 框架。
- 有 Python 和 TypeScript eval 包。
- 有 Phoenix MCP，让 Agent 操作 traces、sessions、datasets、experiments。
- 有 Phoenix CLI，明确面向 Claude Code、Cursor 等 coding agent。
- 有 skills，告诉 Agent 如何调试 trace 和运行 eval。
- 有 PXI 内置 Agent，用于调试 trace、修改 prompt 和操作产品。
- [Trajectory Evals](https://github.com/Arize-ai/phoenix/issues/11654) 仍在开发，说明多步路径评测是当前重点。

这意味着“提供几个 trace 查询工具”不能成为 yiTrace 的核心卖点。

Phoenix 的相对空位是：它主要还是一个本地或远程服务和工程平台，不是一个直接嵌入业务进程的 TraceDB；其 Agent 能力也更偏操作 Phoenix，而不是在业务 Agent 每次运行时自动做受预算约束的历史复用。

### LangWatch：产品闭环最接近我们的描述

LangWatch 的 README 直接写出：

```text
Trace -> dataset -> evaluate -> optimize prompts/models -> re-test
```

它还提供：

- Scenario 多轮 Agent 测试框架。
- MCP 的 `search_traces` 和 `get_trace`。
- 默认返回适合 AI 阅读的 trace digest。
- eval、prompt 和 analytics 工具。

[这个 issue](https://github.com/langwatch/langwatch/issues/3885) 要求 MCP 返回逐条 eval 结果，而不只给聚合分数。需求非常具体：Agent 要找到最低分的 evaluator，并解释具体失败样本。它甚至明确要求限制返回行数，保护上下文窗口。

这和我们提出的“Agent 自己查 trace、下钻失败样本、控制 token”高度重合。

LangWatch 的本地启动仍会拉起 Postgres、Redis、ClickHouse 等组件。yiTrace 的 embedded、单进程、Node/Python/Rust 复用仍有明显差异。

### Langfuse：最大的平台型对手

Langfuse 已覆盖 tracing、eval、dataset、prompt、annotation 和实验。2026 年的 MCP 更新让 Agent 可以：

- 查询 observations 和 metrics。
- 读取 scores、datasets、comments、annotation queues。
- 调查生产问题。
- 把失败样本加入回归数据集。

真实 issue 也暴露了 Agent eval 的难点：

- [Span/observation 级 eval 需求](https://github.com/langfuse/langfuse/issues/8151) 中，用户不能把多次 tool call 作为 evaluator 上下文，也不能排除最后一步，导致答案泄漏。多个用户持续追问进度。
- [长会话压缩后的行为漂移](https://github.com/langfuse/langfuse/issues/12873) 提出：只看最终输出不够，需要比较压缩前后的工具选择、语义变化，以及记忆层到底保留和遗漏了什么。
- [generation eval 变量映射错误](https://github.com/langfuse/langfuse/issues/9899) 说明 trace、span、generation 层级不清会直接让评分失真。

这些问题支持 yiTrace 的两个判断：

1. eval 上下文必须按 span 精确选择，并明确排除项和证据来源。
2. context selection 本身也需要被记录，不能只记录最后一次 LLM 调用。

### Opik：优化能力最强的直接对手

Opik 已经有独立 Agent Optimizer：

- 使用已有 dataset、metrics 和 traces。
- 优化 prompt、tool schema 和多步 Agent workflow。
- 支持 MetaPrompt、HRPO、Evolutionary、GEPA 等算法。
- 记录 trials、candidates 和 trace-level evidence。
- 可以本地或在自建环境运行。

因此，yiTrace 不应把“自动优化 Agent”当作近期核心卖点。这个方向已经有成熟团队重投入。

更合理的边界是：yiTrace 先提供可信的数据和候选治理，让业务方以后选择 Opik、GEPA、DSPy 或自己的 optimizer。`yitrace-agent` 可以提供 adapter，但不急着自己做优化算法平台。

### OpenLLMetry / OpenInference：标准层，不是主要对手

OpenLLMetry 和 OpenInference 的重点是自动打点和 OTel 兼容。

[OpenLLMetry Agent Observability RFC](https://github.com/traceloop/openllmetry/issues/3460) 里已经讨论：

- agent / workflow / tool / MCP / eval / memory 事件。
- context selection 记录。
- session、conversation 和 task 关联。
- 不在热路径内联保存大状态，只保存 hash/ref，完整数据异步或按策略记录。

这说明 `yitrace-agent` 不应公开发明另一套互不兼容的 Agent 事件协议。

正确方式是：

- 对外接受 OpenTelemetry GenAI / OpenInference。
- 框架适配器优先发标准 span。
- yiTrace 特有字段放在稳定 attrs 或扩展字段。
- 包内可以有归一化事件，但它只是实现细节，不是新的行业标准。

### Eval 专用项目：路径评测正在独立成类

- [OpenEvals](https://github.com/langchain-ai/openevals) 已支持 strict、unordered、subset、superset 四种 trajectory match。
- [agentevals](https://github.com/agentevals-dev/agentevals) 直接消费 OTel trace，比较工具路径和最终回答。
- [Strands Evals](https://github.com/strands-agents/evals) 提供 trajectory、tool usage 和 trace-based eval。
- [AgentRx](https://github.com/microsoft/AgentRx) 把 trajectory 归一化后，逐步定位关键失败步骤并保留审计证据。

所以“能评估一条 trajectory”已经逐步成为基础能力。

yiTrace 可以做的差异不是再实现一种简单路径匹配，而是：从大量真实 run 中找到可比较的路径候选，再把候选交给 evaluator 验证。

## 他们如何管理 Trace 数据

### 共同做法

这些项目虽然底层不同，但有几个共同点：

1. SDK 先在业务进程内批量缓冲，再通过 HTTP/OTLP 发给服务端。
2. 服务端把 trace 拆成扁平 span 行，用 `trace_id`、`span_id`、`parent_span_id` 重建树，不把整棵树存成一个大 JSON。
3. trace、span、score/eval、dataset 是不同对象，通过 ID 关联。
4. 高频 trace 数据和低频项目配置通常分开存。
5. 图片、音频、附件和原始事件通常进入 S3/MinIO，不直接塞进主要查询表。
6. Agent、MCP 和前端只调用 API，不直接读底层数据库。

典型链路是：

```text
Agent framework
  -> SDK/OTel batch
  -> HTTP/OTLP collector
  -> queue/worker
  -> trace data store
  -> read API / MCP / CLI
```

### Langfuse：S3 原始事件 + Redis 队列 + ClickHouse 读模型

Langfuse 的写入链路是：

```text
SDK/OTel
  -> Web API
  -> 原始事件写 S3
  -> Redis/BullMQ 只传 S3 引用
  -> Worker 异步解析、补充成本等字段
  -> ClickHouse
```

它把存储拆成四部分：

- PostgreSQL：用户、组织、项目、API key、prompt、dataset 和 evaluator 配置。
- ClickHouse：trace、observation、score 和分析结果。
- Redis：摄入队列和缓存。
- S3：原始摄入事件、多模态附件和大批量导出。

数据模型是：

```text
Session
  -> Trace
       -> Observation(span/generation/event/tool)
       -> Score
```

ClickHouse 中的 trace 和 observation 都按项目、时间组织。input/output 是压缩字符串，metadata 是 Map。表使用 `ReplacingMergeTree`：更新不是原地修改，而是插入同 ID 的新版本，后台合并；查询需要 `FINAL` 或其他去重方式取得最新版本。

这个设计适合高吞吐和按项目/时间做统计，但会带来两个成本：

- 更新后的旧版本不会立即消失。
- 读取完整 trace 可能需要去重和组装大量 observation。

Langfuse 新的 Observations API 因此更强调：

- 必须带时间范围。
- 游标分页。
- 按 field group 投影字段。
- 默认返回字符串，不主动解析大 JSON。
- 需要完整 trace 时，由调用方按 `traceId` 组合 observation。

这本质上也是晚物化：先取轻字段，需要时再取 input/output。

删除采用异步清理。项目级 retention 每晚删除过期 trace、observation、score 和 media。一个值得注意的取舍是：retention 不保证引用完整性，dataset 仍可能引用已经被删除的 trace。

### Opik：ClickHouse 数据面 + MySQL 控制面

Opik 的架构更重，但分工很清楚：

- ClickHouse：trace、span、feedback score、experiment item 和结果。
- MySQL：workspace、project、prompt、dataset 定义和自动化配置。
- Redis：缓存、限流、分布式锁、在线 eval stream 和任务队列。
- MinIO/S3：trace 附件、截图、dataset 文件和实验产物。
- Java backend：主要 API 和摄入。
- Python backend：执行 evaluator 和 optimizer。

Python/TypeScript SDK 都会在客户端批量发送。Python 有受内存上限保护的 batch，TypeScript 默认按时间和条数合并；服务端对 ClickHouse 使用异步插入、批处理和去重。

Opik 的 ClickHouse 表也大量使用 `ReplacingMergeTree`：

```text
更新 trace/span
  = 插入同 ID 的新版本
  -> 后台 merge
  -> 查询时 FINAL 或 LIMIT 1 BY 去重
```

它的查询代码还专门做两阶段读取：

1. 先只用 ID、排序字段和过滤字段找当前页。
2. 再为这一页读取 input/output/metadata 等宽列。

这和 yiTrace 的“先 rollup/sidecar 找候选，再读取完整 span”思路很接近。

### LangWatch：不可变事件 + 异步 projection

LangWatch v3 走的是更完整的 event sourcing：

```text
Command
  -> immutable event_log
  -> Redis/BullMQ
  -> fold/map projection
  -> trace/span/eval/analytics 表
```

例如一个 span 到达后，会形成不可变事件，再由 worker 生成或更新：

- trace summary
- span detail
- cost
- metrics
- PII redaction 结果
- eval result
- experiment result

它把处理分成：

- Fold：按 trace/run 聚合状态，同一个 aggregate 串行处理。
- Map：每个事件独立转成一条记录。
- Reactor：projection 成功后再触发 eval、通知、自动加入 dataset 等副作用。

Redis 的 group queue 保证同一个 aggregate 的事件顺序。ClickHouse 同时保存不可变事件和派生 projection。projection 损坏时可以从 event log 重放。

数据分层是：

- PostgreSQL：用户、项目、prompt、evaluator 定义。
- ClickHouse：事件、trace、span、eval、experiment 和分析 projection。
- Redis：队列、重试、背压和 session。
- S3：冷数据、备份、dataset 和按内容 hash 存放的大文件。

ClickHouse 热数据在 SSD，默认 49 天后可以转到 S3 冷层。

优点是审计、重放和异步扩展很强。代价是组件多、最终一致，并且必须运维 Redis、worker、ClickHouse 和对象存储。

### Phoenix：SQLite/PostgreSQL 直接保存 trace 和 span

Phoenix 是四个项目中最简单的：

```text
OpenInference/OTLP
  -> Phoenix collector
  -> SQLite 或 PostgreSQL
  -> REST/GraphQL/UI
```

关系模型大致是：

```text
Project
  -> Trace(trace_id, start/end, session)
       -> Span(span_id, parent_id, name, kind, time)
            -> attributes JSON/JSONB
            -> events JSON/JSONB
            -> cost/token
       -> trace/span annotations
```

本地默认用 SQLite 文件；生产推荐 PostgreSQL。多个 Phoenix 实例可以共享同一个 PostgreSQL，由负载均衡器分流。

它使用普通唯一约束、外键和 B-tree 索引，更新和删除遵循关系数据库事务。查询完整 trace 时批量加载 spans，避免逐条 N+1。

这个方案一致性清楚、容易备份、启动简单，但大规模分析能力受 SQLite/PostgreSQL 限制。Phoenix 官方也明确把更大规模 OLAP 指向商业版 Arize AX。

需要注意：Phoenix 的“本地”仍然是本地 server + SQLite，不是把数据库库直接嵌入业务 Agent 进程。

### OpenLLMetry：不管理数据

OpenLLMetry/OpenInference 主要负责自动打点和事件标准化。它们把 OTLP 数据发送给 Langfuse、Phoenix、Datadog、Grafana 等后端，自己不承担 trace 持久化、retention、eval dataset 或查询索引。

对 yiTrace 来说，它们更像上游数据来源和兼容标准，不是存储层竞品。

### 管理方式对比

| 项目 | Trace 主存 | 更新方式 | 大字段 | 查询方式 | 本地形态 |
|---|---|---|---|---|---|
| Langfuse | ClickHouse | 追加版本 + merge/去重 | ClickHouse 压缩字符串；多模态/原始事件进 S3 | API 按时间、游标、字段投影 | 多服务部署 |
| Opik | ClickHouse | 追加版本 + merge/去重 | 宽列晚读；附件进 MinIO/S3 | 两阶段查询、API | Docker/Kubernetes 多服务 |
| LangWatch | ClickHouse event log + projections | 不可变事件重放生成视图 | 对象按 hash 进 S3，热冷分层 | 读 projection，不扫原始事件 | Kubernetes 多服务 |
| Phoenix | SQLite/PostgreSQL | 关系事务更新 | JSON/JSONB 放 span 行 | SQL 索引 + API | 本地 server 或集中服务 |
| yiTrace | WAL + memtable + columnar segment | 不可变事件折叠为 span | segment/late materialization | sidecar/rollup/BM25/ANN + API | embedded 或 server |

## 对 yiTrace 的启发

yiTrace 当前并不是落后版本的 Phoenix，也不是缩小版 ClickHouse 平台。它实际上混合了两条成熟思路：

- 写入上更像 LangWatch：保留不可变事件，再折叠成读模型。
- 读取上更像 ClickHouse/Opik：列式存储、轻字段先筛、宽字段晚读取。

同时它多了竞品少见的 embedded 模式。

接下来应坚持这些边界：

1. **原始执行证据尽量不可变。** 后来的 eval、annotation、lesson 和 candidate 用独立对象关联，不回写修改历史 span。
2. **Trace Agent 默认读 projection。** 先读 task/loop/trajectory/trace outline，再按需下钻原始 span。
3. **大字段不能进入默认返回。** input/output/tool result/log 继续晚物化，未来多模态可加可选 blob store，而不是塞进 attrs。
4. **保持引用完整。** retention 删除 trace 前继续保护 eval、dataset、annotation 和 candidate 引用；这一点比 Langfuse 的独立过期策略更适合 Agent Memory。
5. **不要把队列和对象存储变成 embedded 必选项。** 它们只应是 server 高吞吐模式的可选组件。
6. **不要让 Agent 直接查询物理表。** Rust/Python/Node API、HTTP、MCP 都应走同一逻辑边界，避免底层格式变化破坏上层。
7. **记录 Context Pack 的选择过程。** 候选数、选中数、忽略数、token 预算和 evidence refs 应作为新事件或关联对象保存，方便以后解释“Agent 为什么看到这些内容”。

## 网上反复出现的需求

### 1. 接入必须足够简单

主流项目都在不断增加框架适配器。OpenLLMetry 的 issue 也持续要求 BeeAI、LangGraph、MCP 等支持。

这说明接入成本是硬门槛。`yitrace-agent` 初版至少需要：

- 标准 OTel/OpenInference 接入。
- 一套通用 hooks，给自研框架使用。
- 一套真实框架适配器做样板。

但不需要第一天追求几十个 adapter。

### 2. Agent 要直接使用 trace 数据

Phoenix CLI/MCP/skills、LangWatch MCP、Langfuse MCP 和社区 langfuse-mcp 同时出现，说明这不是孤立需求。

用户希望 Agent 能直接回答：

- 哪一步失败了？
- 为什么这次比上次慢？
- 哪个 prompt 版本运行了？
- 哪些 eval 样本得分最低？
- 这次工具路径和成功路径有什么不同？

### 3. Eval 不能只看最终回答

公开 issue 反复要求：

- observation/span 级 eval。
- 多个 tool span 共同作为 evaluator 上下文。
- 排除会造成答案泄漏的 span。
- 评估完整 trajectory。
- 查看逐条样本和评分原因，而不只看平均数。

这正是 Trace Slice、include/exclude、evidence refs 应该解决的问题。

### 4. 上下文选择本身需要被观察和评测

仅记录最终 prompt 和 token 数，无法回答：

- 候选上下文有多少？
- 为什么选中这几段？
- 哪些内容被丢掉？
- 记忆检索错了，还是压缩过程丢了信息？

这意味着 `ContextPack` 不只是返回值，还应该能选择性写回 trace，记录 policy、selected/omitted、token before/after 和 evidence refs。

### 5. 从生产 trace 生成回归样本

Langfuse、Phoenix、LangWatch、Opik 都在做 trace 到 dataset/eval 的链路。

需求已经被验证，但基础功能也在商品化。yiTrace 要竞争，必须把这条路径做得更短：

```text
失败 run -> 精确 span -> eval draft -> 审核 -> dataset item
```

而不是只提供 annotation 和 dataset API，让用户自己拼。

### 6. Best Path 有需求，但不能夸大空白

目前已有三类相邻能力：

- reference trajectory match：拿当前路径和人工参考路径比较。
- optimizer：自动尝试 prompt/tool/workflow 候选并用 eval 选优。
- failure diagnosis：从失败 trajectory 定位关键步骤。

相对少见的是：

> 从生产历史自动发现同类任务的优秀路径，保留 scope 和证据，并在后续运行中持续验证、降级或淘汰。

这可以作为 yiTrace 的方向，但必须叫 `Best Path Candidate`，不能承诺永久最优。

## 已经商品化的能力

下面这些不能再当成 yiTrace 的独家卖点：

- Trace 搜索和 span 下钻。
- Agent 框架自动打点。
- Trace 生成 dataset。
- LLM-as-a-judge。
- Agent 通过 MCP 查询 observability 平台。
- trajectory match。
- prompt 和 tool 优化。
- self-hosted observability。

它们仍然需要做，但属于进入市场的门票。

## yiTrace 可以守住的差异

### 1. 真正的 embedded Loop Engineering

大多数竞品的 local 是“在本机启动一套服务”。yiTrace 可以在 Node、Electron、Python、Rust 进程里直接打开 DB。

这对桌面 Agent、开发工具、私有代码、离线环境和小团队很有价值。

### 2. Token-first 的 trace API

不是先返回完整 JSON，再让 Agent 自己截断，而是所有 Agent-facing API 原生返回：

- digest / outline
- token_estimate
- why_selected
- evidence_refs
- omitted_count
- can_drill_down_more

### 3. task / loop 跨 run 查询

主流 observability 平台通常以 trace、session、project 为主要入口。yiTrace 已经有 task fingerprint、loop、trajectory group 和 trace diff，可以把“同类问题多次尝试”做成一等入口。

### 4. 候选路径治理

Best Path Candidate 必须有：

- scope
- evidence count
- failure count
- evaluator version
- environment / tool / model fingerprint
- promoted / deprecated / rejected 状态
- 后续运行验证记录

### 5. 底层数据与上层逻辑一体但不绑部署

相同 Trace Agent API：

```text
path -> embedded yitrace-db
url  -> yiTrace server
```

用户可以从单机开始，未来再切 server，不需要换业务 API。

## 对原方案的修正

### 修正 1：不要公开创造新的 `AgentEvent` 标准

原方案里的统一事件应改成内部归一化对象。

对外兼容顺序：

1. OpenTelemetry GenAI semantic conventions。
2. OpenInference。
3. yiTrace SpanEvent 扩展字段。

框架适配器负责无损映射，原始框架字段可以保留在 attrs。

### 修正 2：Agent trace tools 是门票，不是终点

`search_runs`、`get_trace`、`get_span` 必须有，但差异化要看：

- 是否按 token budget 输出。
- 是否支持跨 run 比较。
- 是否能直接产出 eval draft。
- 是否能给出路径证据和后续验证结果。

### 修正 3：先不做自动 prompt/tool 优化器

Opik、DSPy、GEPA 等已经在这个方向投入。yiTrace 先做好：

- 数据
- 证据
- evaluator 接口
- candidate store
- optimizer adapter

让外部 optimizer 可以消费 yiTrace，而不是马上重写优化算法。

### 修正 4：MCP 是适配层，不是核心 API

核心能力应先是 Rust/Python/Node 可调用的结构化 API。MCP、CLI、framework tools 都建立在同一 API 上。

否则产品会被 MCP 的协议和某一种 Agent 使用方式绑死。

## 建议的 MVP

### P0：标准接入和 Agent 下钻

1. OTel/OpenInference 字段映射契约。
2. 通用 hooks。
3. 一个 Pi adapter。
4. `search_runs`、`get_trace_outline`、`get_span`、`compare_runs`。
5. embedded/server 统一连接。

### P1：Context Pack

1. token budget。
2. include/exclude span。
3. why selected 和 evidence refs。
4. selected/omitted 记录可选写回 trace。

### P1：Trace to Eval

1. 失败 trace 生成 eval draft。
2. 人工或上层 Agent 审核。
3. 进入 dataset。
4. 运行 evaluator 并保留 evaluator version。

### P2：Best Path Candidate

1. 按 task fingerprint 聚合同类 run。
2. 找到 top-k 候选路径。
3. 用 eval 验证，不只看 status=success。
4. 新运行继续更新证据和失败计数。

## 验证需求，不靠自我感觉

在大规模开发前，应先做四个可验证实验：

1. **接入实验**：一个现有 Pi Agent 在 15 分钟内完成打点、搜索和下钻。
2. **上下文实验**：Context Pack 比完整 trace 少至少 70% token，同时保留失败关键 span。
3. **Eval 实验**：从真实失败 trace 生成的 eval draft，有一半以上可以只做小改后进入 dataset。
4. **路径实验**：同类任务使用候选路径后，在固定 eval 集上的通过率提升，而且换环境后能正确降级旧候选。

如果这四项做不到，`yitrace-agent` 就只是 observability API 的另一层包装，不值得单独做成产品。

## yiTrace 当前组合是否合理

### 客观结论

这是一个**对本地/嵌入式 Agent 场景合理，但尚未证明能承受大规模 server 场景**的组合。

它不是因为“和别人不一样”就天然更好。它成立的原因是几部分刚好符合 trace 工作负载：

- trace 是追加多、修改少的数据，适合 WAL 和不可变事件。
- 字段多、分析和过滤多，适合列式 segment。
- input/output/log 很宽，适合先筛候选、再晚物化。
- Agent 需要关键词、语义和结构化条件一起找历史，BM25、ANN 和 attrs filter 有实际用途。
- embedded 用户不想运维 ClickHouse、Redis 和对象存储，自包含引擎有清楚价值。

因此这几部分不是随意拼接，而是可以互相配合：

```text
WAL/immutable events       = 可靠写入和原始证据
columnar segments          = 压缩和批量读取
sidecar/rollup/index       = 快速筛候选
late materialization       = 不读无关大字段
BM25/ANN                   = Agent 历史检索
embedded/server API        = 两种部署共用同一语义
```

### 它比竞品组合更好的地方

1. **部署成本低。** Phoenix 本地仍要启动 server；Langfuse、Opik、LangWatch 都需要多种外部服务。yiTrace 可以真正进入 Node/Python/Rust 进程。
2. **数据路径统一。** embedded 和 server 共用同一个 Rust engine，减少“本地一套、线上另一套”的行为差异。
3. **适合 Agent 自助检索。** 结构过滤、全文、向量和路径读模型在同一引擎里，比从通用 SQL 平台拼多个查询更直接。
4. **缓存可丢弃。** rollup、attrs sidecar、BM25 和 bloom 不是事实来源，损坏可以从 segment 重建，这个边界是对的。
5. **引用保护更稳。** retention 会保护 annotation、dataset、eval 和 path memory 引用，比“到期直接删 trace”更适合把 trace 当长期证据。

### 它比竞品更危险的地方

1. **自研面太宽。** WAL、manifest、compaction、BM25、HNSW、缓存、retention、metadata 和多语言绑定都由小团队维护。每一项单独看合理，合起来维护成本很高。
2. **派生状态越来越多。** `trace_rollup.dat`、`filter_attrs.dat`、`bm25.dat`、`segment_bloom.dat` 都要和删除、compaction、recover、upgrade 保持一致。它们虽然可重建，但重建时间和错误组合会随数据量增长。
3. **embedded 和 server 的最优设计不同。** embedded 适合单写者、少依赖；高吞吐 server 需要队列、背压、对象存储、独立 worker 和横向扩容。如果强行让一个进程模型同时解决两边，会越来越复杂。
4. **列式存储不擅长频繁更新。** 原始 span 没问题，但 annotation、candidate、review 状态等可变对象不应继续堆进同一套 immutable segment。
5. **零依赖有代价。** 它保护了嵌入和离线构建，但也意味着数据库、检索和并发能力都要自己补。不能把“零依赖”变成拒绝成熟组件的信仰。
6. **当前性能证据还小。** 2 万 span 的 release benchmark 能证明方向可跑，不能证明 10 万、100 万 span 下的冷启动、恢复、低选择性过滤和长期 compaction。

### 适用边界

这个组合适合：

- 本地 Agent、桌面应用、Electron、开发工具。
- 单机服务或同机多进程。
- 需要私有、离线、低运维的 trace 搜索。
- 以追加和历史分析为主的工作负载。

它当前不适合直接承诺：

- 跨机器共享同一个 data dir。
- 大规模多租户云平台。
- 任意数量写节点同时写一个 shard。
- 用它替代 ClickHouse 做通用 OLAP。
- 频繁修改原始 trace/span。

### 必须守住的约束

如果继续这条路线，需要把下面几条当成架构红线：

1. WAL/segment/manifest 是事实来源，所有 rollup 和 index 都必须可验证、可重建。
2. 原始 event/span 保持不可变；eval、annotation、lesson、candidate 使用独立 metadata 对象。
3. Trace Agent 留在独立 crate/package，不把 evaluator、框架 adapter 和 Best Path 治理塞进 `yt-engine`。
4. embedded 模式不依赖 Redis、S3 和后台集群；server 模式未来可以增加这些组件，但不能改变 API 语义。
5. 每新增一个物化索引，都要有正确扫描 fallback、索引命中断言、损坏重建和内存预算测试。
6. 在宣称大规模可用前，必须完成 10 万/100 万 span 的冷启动、热查询、恢复、并发写、删除和 compaction 长跑测试。

### 最终评价

```text
产品方向：好
本地架构匹配度：高
当前工程完成度：中上
长期维护风险：高
大规模 server 证明：不足
```

所以不建议推翻这套组合，也不建议继续往引擎里堆能力。下一阶段最重要的是证明已有架构在真实数据量和 Trace Agent 接入下成立，而不是再增加新的存储概念。

## 最终判断

方向可以继续，但必须调整叙事：

```text
不是：又一个 Agent observability / MCP 工具

而是：建在嵌入式 TraceDB 上、让现有 Agent 框架完成
记录 -> 取证 -> eval -> 路径复用 -> 再验证
的 Loop Engineering SDK
```

这个位置比“通用 Agent 框架”更清楚，也比“Agent 可以查日志”更有价值，但要用真实接入和 eval 结果证明。

## 主要来源

- [Langfuse](https://github.com/langfuse/langfuse)
- [Langfuse MCP update](https://langfuse.com/changelog/2026-05-29-mcp-update)
- [Langfuse span-level eval issue](https://github.com/langfuse/langfuse/issues/8151)
- [Langfuse session-boundary drift RFC](https://github.com/langfuse/langfuse/issues/12873)
- [Phoenix](https://github.com/Arize-ai/phoenix)
- [Phoenix coding agents guide](https://arize.com/docs/phoenix/integrations/developer-tools/coding-agents)
- [Phoenix trajectory eval roadmap](https://github.com/Arize-ai/phoenix/issues/11654)
- [LangWatch](https://github.com/langwatch/langwatch)
- [LangWatch MCP](https://langwatch.ai/docs/integration/mcp)
- [LangWatch per-row eval result issue](https://github.com/langwatch/langwatch/issues/3885)
- [Opik](https://github.com/comet-ml/opik)
- [Opik Agent Optimizer](https://www.comet.com/docs/opik/development/optimization-runs/overview)
- [OpenLLMetry Agent Observability RFC](https://github.com/traceloop/openllmetry/issues/3460)
- [OpenEvals](https://github.com/langchain-ai/openevals)
- [AgentRx](https://github.com/microsoft/AgentRx)
- [langfuse-mcp](https://github.com/avivsinai/langfuse-mcp)
