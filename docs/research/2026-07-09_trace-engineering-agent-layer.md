# Trace Engineering Agent Layer Research

> 日期：2026-07-09
> 分支：trace-engineering
> 范围：只做调研和产品/架构判断，不写实现代码。

## 结论

yiTrace 可以在 TraceDB 之上加一层 **Agent Layer**。

这层不是替代底层数据库，也不是再做一个日志 UI。它要解决的是：

> Agent 自己在执行任务时，能搜索历史 run，下钻到相关 span，只取必要上下文，复用成功路径，把失败路径变成 eval。

如果做对，yiTrace 对外就不只是“本地 TraceDB”，而是：

> TraceDB + agentic engineering layer。

别人集成时，不需要自己从 trace 里拼逻辑。SDK/DB 可以直接提供：

- 查找相似历史 run
- 下钻相关 span
- 选择上下文片段
- 生成 eval case
- 提取 best path 候选
- 给 Agent Memory 提供证据

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
- Agent Layer 可以在标准 trace 之上做更高层能力。
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
- Agent Layer 可以把 trace 变成三种产物：
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

建议名字先叫：

```text
Trace Engineering Layer
```

或者对外更简单：

```text
Agent Layer
```

它在 TraceDB 之上，提供给 Agent 用的能力。

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

## Agent Layer 的核心 API

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

Agent Layer 不应该破坏底层。

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

Agent Layer 只消费这些能力，最多要求底层补字段和索引。

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

Agent Layer 的核心价值就是少塞上下文。

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

要支撑 Agent Layer，还需要补这些底座能力：

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

### MVP-A：Agent Context Retrieval

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

### MVP-B：Eval From Trace

做这几个：

1. 从失败 trace 生成 eval draft
2. 人审后进入 dataset
3. 支持按 task_fingerprint / tool_name / error 筛选

价值：

- 和现有 eval 叙事强相关
- 产品闭环明确

### MVP-C：Best Path Candidate

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

1. 先定义 Agent Layer schema 和 API。
2. 补底座字段：tool args/result、model input/output、task fingerprint。
3. 做 runtime slice builder，不急着持久化。
4. 做 agent context retrieval：搜索 run -> 下钻 span -> 构造上下文。
5. 做 eval draft from trace。
6. 做 best path candidate store。
7. 再考虑 lesson extraction 和 memory promotion。

## `yitrace-agent` 包定义

`yitrace-agent` 不应该定义成通用 agent framework。

它的定位应该是：

> 给 Agent 使用 yiTrace 历史执行数据的库。

更具体一点：

> `yitrace-agent` 负责把 TraceDB 里的 run/span/log/tool/model/eval 数据，变成 Agent 可以安全放进上下文的小片段、候选路径和 eval 草稿。

它不负责通用 agent loop。

### 它负责什么

1. **找历史**

根据当前任务，找到相关历史 run：

- 同项目
- 同 skill
- 同 mode
- 同 call_site
- 相似输入
- 相似工具链
- 相似失败类型

2. **下钻**

从相关 run 下钻到具体 span：

- tool args
- tool result
- model input
- model output
- error
- duration
- token
- status

3. **切上下文**

把长 trace 切成 Agent 能吃的小块：

- trace slice
- token estimate
- importance
- evidence refs
- why selected

核心目标是：不要把整条 trace 塞进上下文。

4. **生成 eval draft**

从失败 run 或关键 span 生成 eval 草稿：

- input
- expected behavior
- failure reason
- source trace/span
- labels

5. **维护 best path candidate**

记录同类任务里表现好的路径候选：

- 不是自动采用
- 不是永久最优
- 必须带证据、scope、confidence、状态

6. **提取 lesson**

从 trace 提取经验，但 lesson 必须带 evidence refs。

没有证据的 lesson 不进默认上下文。

### 它不负责什么

- 不调 LLM
- 不管理 tool loop
- 不做多 agent 编排
- 不决定最终答案
- 不自动改 prompt
- 不自动改工具链
- 不替代 LangGraph / Pi / OpenAI Agents SDK / Pydantic AI

这些属于外部 agent framework。

`yitrace-agent` 只给它们提供 trace-derived context。

### 包边界

建议多语言包保持同一个心智：

```text
yitrace-db      = 存、搜、下钻、回放 trace
yitrace-agent   = 让 Agent 使用历史 trace
```

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
agent.searchContext(query)
agent.drillDown(ref)
agent.buildContextPack(options)
agent.createEvalDraft(ref)
agent.bestPathCandidates(query)
```

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

### 一句话对外口径

```text
yitrace-agent lets agents search, drill down, and reuse their own execution history without loading full traces into context.
```

中文：

```text
yitrace-agent 让 Agent 自己搜索、下钻和复用历史执行过程，而不是把整条 trace 塞进上下文。
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
