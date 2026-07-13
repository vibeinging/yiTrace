# yiTrace Trace Agent Research

> 日期：2026-07-09
> 更新：2026-07-10
> 分支：trace-engineering
> 范围：只做调研和产品/架构判断，不写实现代码。

## 结论

yiTrace 可以在 TraceDB 之上加一层 **Trace Agent**。

它不是新的通用 Agent 框架，也不是日志 UI。它是一套接入现有 Agent 框架的 Loop Engineering SDK：

> 业务继续用 Pi、LangGraph、OpenAI Agents SDK、Pydantic AI 或自研框架运行 Agent；Trace Agent 负责打点、存储接入、历史检索、按需下钻、eval 和 best path 候选等通用逻辑。

业务框架负责当前任务怎么跑，Trace Agent 负责让每次运行留下证据，并让后续运行能使用这些证据。

如果做对，yiTrace 对外就不只是“本地 TraceDB”，而是：

> TraceDB + 可接入现有 Agent 框架的 Loop Engineering SDK。

别人接入后，不需要自己从 trace 里重复拼这些逻辑：

- 把框架的 model、tool、step、run 生命周期统一成 yiTrace 事件
- 查找相似历史 run
- 下钻相关 span
- 选择上下文片段
- 生成 eval case
- 提取 best path 候选
- 给 Agent Memory 提供证据

这条闭环可以概括为：

```text
记录 -> 存储 -> 检索 -> 下钻 -> 评测 -> 复用 -> 再验证
```

## 为什么需要这一层

现在很多 agentic engineering 的做法很粗：

- 把完整上下文塞给 Agent
- 把每轮日志都塞给 Agent
- 靠 grep / 全文搜索找历史
- 靠人工总结“这次为什么成功”
- eval case 靠人工挑

问题是：

- token 成本高
- Agent 容易被无关上下文带偏
- 相似历史 run 很难稳定复用
- 成功路径没有证据
- 失败路径很难沉淀成回归样本

TraceDB 的价值在这里更明显：

1. 先搜到相关 run。
2. 再下钻到具体 span。
3. 只取工具参数、模型输入输出、错误、耗时、token、结果等必要片段。
4. 把片段交给 Agent，而不是把整条 trace 都塞进上下文。

## 外部调研

GitHub 竞品、公开 issue 和线上需求的补充调研见：

- [`2026-07-10_trace-agent-competitor-demand-research.md`](2026-07-10_trace-agent-competitor-demand-research.md)

补充调研后的重要修正是：Agent 查询 trace、trace-to-eval 和 trajectory eval 已经有较强竞品。yiTrace 的差异应放在 embedded、token-first 下钻、task/loop 跨 run 比较和候选路径持续验证，不应只做一组通用 MCP 查询工具。

### 1. LangSmith / Langfuse：trace 到 eval 是主流方向

LangSmith 的 agent eval 强调要看完整 trajectory，包括步骤、工具调用和 reasoning，用 evaluator 打分中间决策。

Langfuse 的公开文档也强调：最有价值的测试样本来自生产 trace 里的失败案例；要用 annotation / dataset 把失败 trace 变成 eval 数据。

这说明：

- trace 不是终点
- trace 后面一定会接 eval
- 只评最终答案不够，要评 trajectory 和单步行为

对 yiTrace 的启发：

- 我们已经有 trace、span、annotation、dataset association，这是基础。
- 下一步应该把“从 trace 生成 eval case”产品化。
- 不是只提供 API，让用户自己拼。

### 2. OpenTelemetry GenAI：行业正在标准化 agent span

OpenTelemetry GenAI 已经定义 agent、conversation、tool call、input/output messages、token usage、evaluation score 等属性。

这说明：

- agent trace 的基础 schema 会越来越标准。
- yiTrace 不应该闭门造一套完全不兼容的 agent 事件。
- 但 OTel 只解决“怎么记录”，不解决“Agent 如何使用历史 trace”。

对 yiTrace 的启发：

- 底层 ingest 应继续兼容 OTLP / OpenInference / GenAI semconv。
- Trace Agent 可以在标准 trace 之上做更高层能力。
- `tool_name`、`tool_args`、`tool_result`、`model_input`、`model_output`、`eval_score`、`task_fingerprint` 应该成为一等字段或稳定 attrs。

### 3. Agent Memory 研究：成功/失败 trajectory 是长期记忆的一类

Agent memory 相关资料把 long-term memory 分为不同类型，其中有一类是从经验学习：

- trajectories
- success/failure lessons
- reusable skills
- workflows

Memp / procedural memory 方向强调把历史 trajectory 转成可复用模板：推理模式、工具序列、恢复策略。

Trajectory-informed memory 方向强调从 agent execution trajectories 自动提取 actionable learnings，并在未来通过上下文检索复用。

Agent Workflow Memory 方向强调从过去经验中归纳 reusable workflows，再选择性提供给 agent。

对 yiTrace 的启发：

- Agent Memory 不能只存自然语言总结。
- TraceDB 应该保留原始证据。
- Trace Agent 可以把 trace 变成三种产物：
  - evidence：原始 span 片段
  - lesson：自然语言经验
  - workflow / path：可复用路径

### 4. Reflexion / ExpeL / self-improvement：失败也有价值，但需要治理

Reflexion 类方法会把失败经验写成记忆，帮助后续尝试。

ExpeL 类方法会从成功和失败 trajectories 中蒸馏经验，在后续任务中检索使用。

新的 self-improvement / harness engineering 讨论也提醒：不能简单相信当前 evaluator 认为的 best path。当前看起来最优的路径，可能只是当前评分器偏好下的局部最优。

对 yiTrace 的启发：

- `best path` 不能直接等于“最近一次成功路径”。
- 需要候选、证据、评分、适用范围、过期机制。
- 失败路径也要保留，但不能直接注入给 Agent，应该转成风险提示或 eval。

## 我们应该做的产品层

产品能力可以叫：

```text
Trace Agent
```

对外包名继续带 yiTrace 品牌：

```text
@yitrace/agent
yitrace-agent
```

它在业务 Agent 框架和 TraceDB 之间，提供两组能力：

1. 给框架用：统一打点、运行结束处理、eval 生成和路径候选更新。
2. 给 Agent 用：搜索历史 run、查看 trace 轮廓、下钻 span、比较路径和构造少量上下文。

```text
业务 Agent 框架
  |  model/tool/step/run hooks
  v
Trace Agent
  |-- 框架适配器
  |-- Agent 可调用的 trace 工具
  |-- context/eval/best-path 通用逻辑
  v
yiTrace DB（embedded 或 server）
```

## 核心对象

### 1. Task Fingerprint

用来判断“这是不是同一类问题”。

字段建议：

- project_id
- skill
- mode
- call_site
- user_intent
- tool_set
- input_signature
- environment_signature

用途：

- 找相似历史 run
- 聚合同类任务
- 判断 best path 是否适用

### 2. Trace Slice

Agent 不应该拿整条 trace。

Agent 应该拿 slice：

- 某个 span 的 input/output
- 某次 tool call 的 args/result
- 某个失败节点的 error
- 某条成功路径的关键步骤
- 某个 evaluator 的解释

Trace Slice 是上下文预算的基本单位。

字段建议：

- slice_id
- trace_id
- span_id
- kind: prompt/tool/error/output/eval/path
- text
- token_estimate
- importance
- evidence_refs

### 3. Best Path Candidate

不是最终 best path，而是候选。

字段建议：

- candidate_id
- task_fingerprint
- trace_id
- span_path
- outcome: success/failure/partial
- score
- confidence
- evidence_count
- failure_count
- last_seen_at
- scope
- status: candidate/promoted/deprecated/rejected

注意：

- 初版不要做引用计数压缩，用户已明确暂不做。
- 初版只做候选存储和检索。

### 4. Lesson

从 trace 提炼出来的自然语言经验。

字段建议：

- lesson_id
- task_fingerprint
- source_trace_ids
- source_span_ids
- lesson_text
- lesson_type: do/dont/risk/check
- confidence
- status

注意：

- lesson 必须带 evidence refs。
- 没有证据的 lesson 不应该进入默认上下文。

### 5. Eval Case

从失败或关键 run 生成 eval。

字段建议：

- eval_case_id
- source_trace_id
- source_span_ids
- input
- expected_behavior
- failure_reason
- evaluator
- labels
- dataset_id

## Trace Agent 的核心 API

### P0：框架生命周期接入

目标：让不同 Agent 框架产出的事件进入同一套 yiTrace 数据模型。

框架适配器至少要能上报：

- run start/end
- step start/end
- model input/output
- tool args/result/error
- token、cost、status、feedback

公共接入优先兼容 OpenTelemetry GenAI / OpenInference，再映射到现有 yiTrace SpanEvent。Pi、LangGraph 等适配器负责事件翻译，但不能把某个框架的数据格式变成新的公共标准。

接口草案：

```ts
const traceAgent = createTraceAgent({
  projectId: "agentic-data",
  transport: { path: "./data" } // 也可以是 { url: "http://..." }
})

framework.use(traceAgent.adapter("pi"))
```

自研框架可以直接调用通用 hooks：

```ts
traceAgent.onRunStart(...)
traceAgent.onModelEnd(...)
traceAgent.onToolEnd(...)
traceAgent.onRunEnd(...)
```

### P0：给 Agent 使用的 trace 工具

目标：让业务 Agent 自己查看历史执行，不要求业务方重新封装工具。

建议直接提供：

- `search_runs`
- `get_trace_outline`
- `get_span`
- `compare_runs`
- `get_best_path_candidates`
- `create_eval_draft`

这些工具返回结构化结果、证据引用和 token 估算。外部框架只需要把工具注册给自己的 Agent。

### P0：Agent 上下文检索

目标：让 Agent 不再加载全部日志。

接口草案：

```ts
agentContext.search({
  task: "...",
  project_id: "...",
  skill: "...",
  budget_tokens: 2000
})
```

返回：

- 相关历史 run
- 推荐 trace slices
- 每个 slice 的来源和 token 估算
- 为什么推荐

### P0：Span 下钻

目标：Agent 先搜 run，再按需打开 span。

接口草案：

```ts
agentContext.drillDown({
  trace_id,
  span_id,
  include: ["tool_args", "tool_result", "model_input", "model_output", "error"]
})
```

返回：

- 结构化 span detail
- log events
- attrs
- token estimate

### P0：Trace Slice 生成

目标：把长 trace 切成 Agent 可吃的小块。

接口草案：

```ts
agentContext.slices({
  trace_id,
  budget_tokens: 1500,
  focus: "why_failed"
})
```

返回：

- slices
- omitted reason
- evidence refs

### P1：Best Path 候选

目标：找同类任务中表现好的路径。

接口草案：

```ts
agentContext.bestPathCandidates({
  task_fingerprint,
  k: 5
})
```

返回：

- 候选路径
- 证据 trace
- 适用范围
- 分数和置信度

### P1：Eval Case 生成

目标：从失败 trace 变成 eval 样本。

接口草案：

```ts
agentContext.createEvalCase({
  trace_id,
  span_id,
  reason: "tool_failed"
})
```

返回：

- eval case draft
- source evidence
- labels

### P1：Lesson 生成与治理

目标：从 trace 生成可复用经验，但不无脑注入。

接口草案：

```ts
agentContext.extractLessons({
  trace_id,
  mode: "success_or_failure"
})
```

返回：

- lessons
- evidence_refs
- confidence
- status

## 必须坚持的边界

### 1. TraceDB 还是底座

Trace Agent 不应该破坏底层。

底层继续做：

- ingest
- WAL
- segment
- search
- span detail
- trace aggregate
- retention
- annotation
- dataset association

Trace Agent 只通过公开的 DB/client 接口消费这些能力，最多要求底层补字段和索引。它不能直接解析 WAL、manifest 或 segment。

### 2. 不要一开始做“自动优化 Agent”

自动改 prompt、自动改工具链、自动发布 best path 都太危险。

初版只做：

- 推荐
- 证据
- 候选
- draft
- 人或上层 Agent 再决定是否采用

### 3. Best path 不能绝对化

best path 应该叫 candidate。

原因：

- evaluator 可能错
- 场景可能变
- 工具版本可能变
- 成功可能是偶然
- 新路径可能更好

所以需要：

- scope
- evidence_count
- failure_count
- last_seen_at
- status
- confidence

### 4. 上下文预算是一等约束

Trace Agent 的核心价值就是少塞上下文。

所以所有 retrieval / drilldown API 都要返回：

- token_estimate
- why_selected
- omitted_count
- can_drill_down_more

## yiTrace 当前底座还缺什么

结合 `docs/CURRENT_STATE.md`，当前已有：

- trace-search
- trace-aggregate
- trajectory-groups
- loops
- task traces
- annotations
- dataset associations
- attrs filter
- trace/span/log events
- readPlan

这些能力可以直接组成 Trace Agent 的底座：

| Trace Agent 需求 | 现有 yiTrace 能力 | 处理方式 |
|---|---|---|
| 框架打点和存储 | SDK ingest、SpanEventBuilder、embedded/server | 复用，新增框架事件翻译 |
| 找相关 run | search、trace-search、task traces | 复用，新增统一查询方法 |
| 看路径 | trace-trajectories、trajectory-groups | 复用，包装成 Agent 工具 |
| 比较路径 | traces diff | 复用，补适合 Agent 的结果格式 |
| 下钻 span | trace/span detail、logEvents | 复用，补 token budget 和 evidence refs |
| 标记好坏 | annotations | 复用，增加固定 label 约定 |
| 生成 eval 数据集 | dataset associations | 复用，增加 eval draft 和审核流程 |
| embedded/server 切换 | 多语言 DB 包、HTTP API | 复用，统一 Trace Agent 连接入口 |

因此 Trace Agent 真正要新增的是：

1. OTel/OpenInference 映射、包内 `NormalizedAgentEvent` 和 hooks。
2. Pi、LangGraph 等事件适配器。
3. 可直接注册给业务 Agent 的 trace 工具。
4. Trace Slice / Context Pack 选择逻辑。
5. Eval draft、Evaluator 接口和执行结果记录。
6. Best Path Candidate 的评分、状态变化和持续验证。

不需要新增另一套 WAL、segment、全文检索、向量索引或 trace 存储。

要支撑 Trace Agent，还需要补这些底座能力：

### P0 底座字段

- tool_args
- tool_result
- model_input
- model_output
- prompt_template
- tool_error
- output_summary
- input_summary
- task_fingerprint
- environment_signature

部分字段可以先放 attrs，但高频字段最好提升为一等字段。

### P0 下钻 API

已有 span detail 和 logEvents，但需要更适合 Agent：

- 支持 include/exclude 字段
- 支持 token budget
- 支持自动摘要或截断
- 返回 evidence refs

### P0 Trace Slice Store

不一定一开始持久化。

MVP 可以运行时生成 slice。

但需要稳定 schema，方便后面缓存和索引。

### P1 候选路径表

新增 best path candidate store。

只存候选和证据引用，不做数据压缩。

### P1 eval draft 表

现有 dataset association 能关联，但还缺“eval draft”这个中间态。

需要支持：

- 从 trace 生成 draft
- 人审后进 dataset
- 保留来源 trace/span

## MVP 建议

### MVP-A：Framework Adapter + Agent Trace Tools

先完成最薄但完整的闭环：

1. 定义统一的 run/step/model/tool 事件。
2. 提供不依赖具体框架的 hooks。
3. 做一个 Pi 适配器验证接入方式。
4. 提供 `search_runs`、`get_trace_outline`、`get_span` 三个 Agent 工具。
5. 同时支持 embedded DB 和 yiTrace server。

价值：

- 证明“任意 Agent 框架可以接 yiTrace”不是一句口号。
- Agent 能自己搜索和下钻历史执行。
- Pi 只是第一套适配器，不进入 Rust core。

### MVP-B：Agent Context Retrieval

最有差异化，也最符合小红书/X 当前叙事。

做这几个：

1. `searchRelevantRuns`
2. `drillDownSpan`
3. `buildContextSlices`
4. 返回 token_estimate 和 evidence_refs

价值：

- 立刻解决“不要把完整日志塞上下文”
- 对 AgenticData / 其他 agent 项目可直接用
- 不需要先解决 best path 治理复杂问题

### MVP-C：Eval From Trace

做这几个：

1. 从失败 trace 生成 eval draft
2. 人审后进入 dataset
3. 支持按 task_fingerprint / tool_name / error 筛选

价值：

- 和现有 eval 叙事强相关
- 产品闭环明确

### MVP-D：Best Path Candidate

做候选，不做自动采用。

做这几个：

1. 记录成功路径候选
2. 聚合同类 task
3. 返回 top-k candidates
4. 支持 evidence_count / confidence / scope

价值：

- 支撑 “best path” 方向
- 但不把项目推到不可靠的自动优化上

## 不建议现在做

- 自动修改 prompt
- 自动修改工具编排
- 自动把 best path 注入所有任务
- 自动删除重复 trace / 引用计数压缩
- 基于 LLM 的无证据 memory 写入
- 大而全的可视化工作台

这些可以后面做，但不是底座第一步。

## 推荐路线

1. 先定义 Trace Agent 的标准事件、hooks、transport 和工具协议。
2. 实现不依赖框架的 core，并用 Pi 适配器验证一次完整接入。
3. 补底座字段：tool args/result、model input/output、task fingerprint。
4. 做 runtime slice builder，不急着持久化。
5. 做 agent context retrieval：搜索 run -> 下钻 span -> 构造上下文。
6. 做 eval draft from trace。
7. 做 best path candidate store。
8. 再增加其他框架适配器、lesson extraction 和 memory promotion。

## `yitrace-agent` 包定义

`yitrace-agent` 的准确定位是：

> 建在 yiTrace 之上、接入现有 Agent 框架的 Loop Engineering SDK。

它不是一个会替业务执行任务的 Agent，也不要求用户更换 Agent 框架。

业务方保留自己的 model、tool、prompt、loop 和状态管理。`yitrace-agent` 提供这些框架普遍需要、但不值得每个项目重复实现的能力：

- 接入运行生命周期并统一打点
- 把数据写入 embedded DB 或 yiTrace server
- 给 Agent 注册历史检索和 span 下钻工具
- 从 trace 构造受 token 预算约束的上下文
- 从失败路径生成 eval 草稿
- 从成功路径维护 best path 候选
- 用后续运行继续验证候选是否仍然有效

换句话说，业务框架负责 **run loop**，`yitrace-agent` 负责 **improvement loop**。

### 它负责什么

1. **框架接入与统一打点**

提供通用 hooks 和框架适配器，收集：

- run / step 生命周期
- model input/output
- tool args/result/error
- token、cost、status、feedback

框架适配器只做事件翻译。标准事件、存储和后续逻辑不能依赖 Pi、LangGraph 等框架。

2. **连接 yiTrace**

使用同一套上层 API 支持两种模式：

- embedded：直接连接 `yitrace-db`
- server：通过 HTTP 连接 yiTrace server

它只走公开 DB/client API，不读取底层数据文件。

3. **给 Agent 注册 trace 工具**

外部框架可以把封装好的工具直接交给自己的 Agent：

- 搜索历史 run
- 查看 trace 轮廓
- 下钻具体 span
- 比较成功和失败路径
- 获取 best path 候选
- 创建 eval 草稿

这样“Agent 自己看 trace”不再需要业务方重新写一套工具。

4. **找历史**

根据当前任务，找到相关历史 run：

- 同项目
- 同 skill
- 同 mode
- 同 call_site
- 相似输入
- 相似工具链
- 相似失败类型

5. **下钻**

从相关 run 下钻到具体 span：

- tool args
- tool result
- model input
- model output
- error
- duration
- token
- status

6. **切上下文**

把长 trace 切成 Agent 能吃的小块：

- trace slice
- token estimate
- importance
- evidence refs
- why selected

核心目标是：不要把整条 trace 塞进上下文。

7. **生成和运行 eval**

从失败 run 或关键 span 生成 eval 草稿：

- input
- expected behavior
- failure reason
- source trace/span
- labels

核心层负责样本、证据、数据集和结果记录。需要模型评分时，通过可插拔的 `Evaluator` / `ModelProvider` 调用业务方提供的模型。

8. **维护 best path candidate**

记录同类任务里表现好的路径候选：

- 不是自动采用
- 不是永久最优
- 必须带证据、scope、confidence、状态
- 必须用新的运行结果持续验证或降级

9. **提取 lesson**

从 trace 提取经验，但 lesson 必须带 evidence refs。

没有证据的 lesson 不进默认上下文。

### 它不负责什么

- 不接管业务 Agent 的主循环
- 不选择业务模型和工具
- 不做多 agent 编排
- 不决定最终答案
- 不自动改 prompt
- 不自动改工具链
- 不替代 LangGraph / Pi / OpenAI Agents SDK / Pydantic AI

这些属于外部 Agent 框架。

`yitrace-agent` 可以通过业务方传入的模型做 evaluator、摘要或 lesson 提取，但不能偷偷使用模型，也不能把自己的模型选择强加给业务方。

### 包边界

三个包要保持清楚的分工：

```text
yitrace / @yitrace/trace-sdk = 只负责轻量打点
yitrace-db / @yitrace/db     = 存、搜、下钻、回放 trace
yitrace-agent / @yitrace/agent = 把现有 Agent 框架接入完整的 Loop Engineering 闭环
```

只要打点的用户继续用 trace SDK。需要搜索、eval、best path 和 Agent 自助下钻的用户使用 `yitrace-agent`。

为了减少用户心智，`yitrace-agent` 应提供统一的连接入口：

```ts
createTraceAgent({ path: "./data" })
createTraceAgent({ url: "http://localhost:7878" })
```

路径模式内部使用 embedded DB，URL 模式内部使用 HTTP client。业务 API 不因部署方式变化。

### Rust core 与框架适配器

可以跨语言复用、结果必须一致的逻辑放进 Rust core：

- OTel/OpenInference 到 yiTrace 的映射
- 包内归一化事件和对象 schema
- task fingerprint
- trace slice 选择和 token budget
- 候选路径评分与状态变化
- eval 数据集和结果模型

和具体框架有关的接入留在各语言包：

- Pi / Node adapter
- LangGraph / Python adapter
- OpenAI Agents SDK adapter
- Pydantic AI adapter

因此 Pi 可以作为第一套验证适配器，但不能成为 Rust core 或 `yitrace-agent` 的必选依赖。

Rust:

```text
yitrace-db
yitrace-agent
```

Python:

```text
yitrace-db
yitrace-agent
```

Node:

```text
@yitrace/db
@yitrace/agent
```

### 最小 API

```ts
const agent = createTraceAgent({ path: "./data", projectId: "demo" })

agent.hooks()                       // 自研框架接入
agent.adapter("pi")                // 已支持框架接入
agent.tools()                       // 注册给业务 Agent 的 trace 工具
agent.searchRuns(query)
agent.getTraceOutline(traceRef)
agent.getSpan(spanRef)
agent.buildContextPack(options)
agent.createEvalDraft(traceRef)
agent.runEval(datasetRef, evaluator)
agent.bestPathCandidates(query)
agent.recordPathOutcome(candidateRef, outcome)
```

这里的 `agent` 是 Trace Agent handle，不是另一个负责回答用户问题的业务 Agent。

### 核心返回对象

```text
ContextPack
  - slices
  - token_estimate
  - evidence_refs
  - omitted_count
  - can_drill_down_more

TraceSlice
  - kind
  - text
  - trace_id
  - span_id
  - importance
  - token_estimate

BestPathCandidate
  - task_fingerprint
  - span_path
  - score
  - confidence
  - scope
  - evidence_refs
  - status

EvalDraft
  - source_trace_id
  - source_span_ids
  - input
  - expected_behavior
  - failure_reason
  - labels
```

包内仍然需要一个稳定的归一化事件：

```text
NormalizedAgentEvent
  - project_id
  - run_id
  - parent_run_id
  - event_type
  - timestamp
  - name
  - input
  - output
  - error
  - token_usage
  - cost
  - attrs
```

`NormalizedAgentEvent` 只是 `yitrace-agent` 内部对象，不是新的 wire protocol。对外继续接受 OTLP/OpenInference 和 yiTrace SpanEvent；所有原始框架字段应尽量无损映射，不能映射的字段保留在 attrs。

### 一句话对外口径

```text
yitrace-agent adds trace-driven loop engineering to your existing agent framework: capture runs, inspect history, build evals, and reuse better paths.
```

中文：

```text
yitrace-agent 给现有 Agent 框架补上基于 trace 的打点、检索、下钻、eval 和路径复用能力。
```

## 参考资料

- LangSmith Evaluation: https://www.langchain.com/langsmith/evaluation
- Langfuse Agent Evaluation: https://langfuse.com/guides/cookbook/example_pydantic_ai_mcp_agent_evaluation
- Langfuse Agent Observability: https://langfuse.com/blog/2024-07-ai-agent-observability-with-langfuse
- OpenTelemetry GenAI Attributes: https://opentelemetry.io/docs/specs/semconv/registry/attributes/gen-ai/
- Awesome Memory for Agents: https://github.com/TsinghuaC3I/Awesome-Memory-for-Agents
- Memp: Exploring Agent Procedural Memory: https://arxiv.org/html/2508.06433v2
- Trajectory-Informed Memory Generation: https://arxiv.org/html/2603.10600v1
- Agent Workflow Memory discussion: https://medium.com/@techsachin/agent-workflow-memory-using-workflows-to-guide-llm-agent-generations-aad75fe2f78a
- Harness Engineering for Self-Improvement: https://lilianweng.github.io/posts/2026-07-04-harness/
