# Loop Engineering 与 yiTrace 功能计划

> 日期：2026-07-03
> 范围：概念调研 + 产品计划。本文只写计划，不写代码。

## 结论

“Loop engineering” 不是一个全新的底层技术，更像是 agent 工程从“写好 prompt”转向“设计可反复执行、可验证、可停止的工作闭环”。

对 yiTrace 来说，这个概念是一个很好的产品叙事入口：yiTrace 不应该只说自己能存 trace、搜 trace，而应该进一步说清楚：

> yiTrace 是 agent loop 的观察层、证据层和改进层。

也就是说，loop 本身可以由 Codex、Claude Code、LangGraph、AgenticData、内部调度器或用户自己的 harness 来跑；yiTrace 负责把 loop 每一轮发生了什么记录下来，帮助用户判断：

- 这个 loop 为什么停？
- 它是不是停早了？
- 它是不是在原地打转？
- 哪个验证信号真的推动了修复？
- 同一个问题被问过很多遍时，哪一次走出了最优路径？
- 这条最优路径能不能被提炼出来，后续稳定复用？
- 哪些失败 trace 应该进入 eval？
- 哪些执行经验应该进入 Agent Memory？

这比单纯做“agent observability”更贴近 2026 年 agent 工程的讨论热点，也和 yiTrace 已有的 trace、session、attrs、logEvents、eval、成本归因能力匹配。更重要的是，trace 数据不只用于事后排错，还可以用于发现历史上的高质量执行路径，再反哺后续 agent 的计划和验证。

## 调研摘要

公开资料对 loop engineering 的说法不完全统一，但核心接近：

- Addy Osmani 把它描述为：不再由人一轮轮 prompt agent，而是设计一个系统来 prompt agent，让 AI 递归迭代直到目标完成；他同时强调 token 成本仍然是早期风险。
- LangChain 的文章把 loop 拆成多层：基础 agent loop、verification loop、event-driven loop 等；重点是 agent 不是只调用一次模型，而是模型、工具、验证器、事件触发之间的循环。
- OpenAI cookbook 的 agent improvement loop 更接近 yiTrace 可以做的方向：trace 捕获行为，human feedback 增加判断，eval 固化期望行为，后续再把证据交给实现工具改 harness。
- Daniel Demmel 的 feedback loop engineering 强调内外两层：内层解决当前任务，外层把完成后的经验沉淀下来；观测、结构化日志、trace correlation、验证工具都会变成 agent 可用的反馈系统。
- Kilo / MindStudio 这类文章把 loop engineering 区分于 prompt engineering：prompt engineering 优化输入，loop engineering 优化完整流程，包括上下文、行动、观察、验证、停止条件和人工介入。

参考资料：

- Addy Osmani, “Loop Engineering”, 2026-06-07: https://addyosmani.com/blog/loop-engineering/
- LangChain, “The Art of Loop Engineering”, 2026-06-16: https://www.langchain.com/blog/the-art-of-loop-engineering
- OpenAI Cookbook, “Build an Agent Improvement Loop with Traces, Evals, and Codex”: https://developers.openai.com/cookbook/examples/agents_sdk/agent_improvement_loop
- Daniel Demmel, “Feedback loop engineering”: https://www.danieldemmel.me/blog/feedback-loop-engineering
- Kilo, “What Is Loop Engineering?”: https://kilo.ai/articles/what-is-loop-engineering
- MindStudio, “Loop Engineering vs Harness Engineering”: https://www.mindstudio.ai/blog/loop-engineering-vs-harness-engineering

## yiTrace 的切入点

Loop engineering 需要四类基础能力：

1. **记录**：每轮 loop 做了什么，调用了哪些工具，看到什么反馈。
2. **判断**：是否成功、失败、卡住、反复、成本过高、停得太早。
3. **固化**：把失败、人工反馈和验证结果变成 eval / regression。
4. **沉淀**：把可复用经验交给 Agent Memory，而不是把所有 trace 都塞进 memory。
5. **复用**：从多次相似任务里发现最优路径，并把它变成后续 agent 可采用的策略提示、模板或 eval baseline。

yiTrace 已经有其中一半底座：

- `trace/session/span` 可以记录执行路径。
- `attrs` 可以挂 `project_id`、`skill`、`mode`、`call_site` 这类 loop 维度。
- `logEvents` 可以返回原始过程日志。
- BM25 / vector / hybrid search 可以找相似失败。
- eval 逻辑已有 alpha 基础。
- 列式/晚物化方向适合解决 trace 膨胀：筛选、聚合、成本分析只读窄列，大文本按需取。

缺的是把这些 trace 明确组织成“loop”，并进一步把多次相似 loop 组织成可比较的 trajectory，找出值得复用的 golden path。

## 建议产品名

暂定叫 **Loop Intelligence**，比 “loop engineering support” 更像功能模块。

对外一句话：

> Turn agent traces into loop diagnostics, evals, golden paths, and memory evidence.

中文：

> 把 agent trace 变成 loop 诊断、eval 回归集、最优路径和记忆证据。

## P0：先定义 Loop Trace Contract

不要一开始就做 orchestrator。第一步只定义约定，让任何 agent/harness 都能把 loop 信息打进 yiTrace。

建议先用 `attrs` 承载，后续再决定哪些字段提升为一等列：

| 字段 | 说明 |
|---|---|
| `loop_id` | 一次 loop 运行的稳定 id |
| `loop_type` | `coding` / `research` / `review` / `eval_repair` / `data_pipeline` 等 |
| `goal_id` | 业务目标或用户任务 id |
| `iteration` | 第几轮，从 1 开始 |
| `attempt` | 某轮里的重试次数 |
| `phase` | `plan` / `act` / `observe` / `verify` / `repair` / `summarize` |
| `harness_version` | agent harness / prompt / workflow 版本 |
| `agent_version` | agent 配置版本 |
| `validator` | 触发反馈的验证器，例如 `npm test`、`typecheck`、`screenshot`、`human_review` |
| `validation_status` | `pass` / `fail` / `skipped` / `unknown` |
| `stop_reason` | `goal_met` / `max_iterations` / `budget_exceeded` / `human_gate` / `blocked` / `error` |
| `budget_tokens` | 预算 token |
| `budget_ms` | 时间预算 |

P0 不要求所有接入方一次性填满。最小可用字段：

```text
loop_id, iteration, phase, validation_status, stop_reason, harness_version
```

验收：

- 用现有 `@yitrace/db` builder 可以写入 loop attrs。
- `search()` 能按 `loop_id` / `harness_version` / `validation_status` 精确过滤。
- `trace()` / `span()` 能看到每个 span 的 loop attrs 和 `logEvents`。

## P0：Loop Health 指标

先不做复杂 AI 分析，先做确定性指标。它们很适合 yiTrace 的列式读取和聚合。

核心指标：

| 指标 | 价值 |
|---|---|
| `iterations_total` | 这个 loop 跑了几轮 |
| `repair_count` | 有多少轮是修复/重试 |
| `validation_fail_count` | 验证失败次数 |
| `validation_pass_after_fail` | 失败后是否真的修好 |
| `tokens_total` / `tokens_per_iteration` | 成本是否膨胀 |
| `duration_total` / `duration_per_iteration` | 是否拖太久 |
| `same_error_repeat_count` | 是否重复踩同一个坑 |
| `tool_error_rate` | 哪个工具最常拖垮 loop |
| `stop_reason` 分布 | 是正常完成，还是预算耗尽/人工中断 |

第一批诊断标签：

- `converged`：失败后最终通过验证。
- `thrashing`：多轮失败且错误文本高度相似。
- `premature_success`：声明成功后仍有失败验证或人工负反馈。
- `budget_runaway`：token 或时间超过阈值。
- `weak_validation`：缺少 verify phase 或验证器一直 skipped。
- `human_gate_needed`：多次自动修复后仍失败。

验收：

- 用户能按项目/skill/mode 找出最浪费 token 的 loop。
- 用户能找出“重复失败但没有升级人工”的 loop。
- 用户能看到某个 harness version 的 loop 成功率和成本变化。

## P1：Loop Explorer 控制台

控制台新增一个“Loop”视图，不替代 trace waterfall，而是在 trace 之上做聚合。

页面结构：

1. 左侧：Loop 列表
   - `loop_id`
   - goal / project / skill
   - status
   - iteration count
   - token / duration
   - diagnostics 标签

2. 中间：Iteration Timeline
   - 每一轮一行：plan / act / observe / verify / repair
   - 标出验证失败、重试、人工反馈、stop reason
   - 点击进入对应 trace/span waterfall

3. 右侧：Evidence Panel
   - 关键 `logEvents`
   - 工具错误
   - input/output 摘要
   - 相似历史失败
   - 可加入 eval 的候选片段

验收：

- 一个 loop 不再散落在多条 trace 里，而是能按 iteration 串起来看。
- 用户可以直接回答：“第几轮开始跑偏？哪个验证信号让它修回来了？”

## P1：Trace-to-Eval 工作流

OpenAI cookbook 里最值得借鉴的是“trace + feedback -> eval -> harness change”的闭环。yiTrace 应该先做前半段。

流程：

1. 用户在 Loop Explorer 里选一个失败 loop。
2. 标注失败原因或选择诊断标签。
3. yiTrace 生成 eval candidate：
   - source trace/span
   - input fixture
   - expected behavior
   - prohibited behavior
   - deterministic assertions
   - optional LLM judge rubric
4. 用户确认后加入 dataset。
5. 后续 harness version 变化时跑 regression。

验收：

- “失败 trace -> eval item” 不需要用户手写 JSON。
- eval item 必须保留 source trace/span 链接，避免失去证据。
- dataset 能按 `project_id`、`skill`、`harness_version` 过滤。

## P1：Loop Pattern Mining

yiTrace 可以基于历史 trace 挖出可复用模式，但要保持“证据优先”，不要直接生成无法追溯的建议。

第一批模式：

- 同一个工具调用在多个 loop 中失败，且错误相似。
- 某个验证器经常在同一 phase 后失败。
- 某个 agent/harness version 成本突然上升。
- 某个 call_site 的 loop 更容易进入 `max_iterations`。
- 某个 skill 下，人工反馈总是要求补同类约束。

输出形式：

```text
发现：skill=packaging 下，native binding 错误在 7 个 loop 中重复出现。
证据：trace A/B/C...
建议：把 Node arch 与 .node 文件架构检查加入 verify phase。
置信度：基于 7 次出现、5 次最终修复路径一致。
```

这部分可以后续接 MemMe：yiTrace 给证据，MemMe 存“经验”。

## P1：Golden Path Mining

这是另一条更产品化的主线：trace 数据不只是用来排错，还可以用来发现“最优路径”。

典型场景：

> 同一个问题问了很多遍，中间有一次 agent 用更少步骤、更低成本、更高成功率完成了任务。系统应该能发现这条路径，并让后续相似任务优先使用它。

这里的“最优”不能简单等于最短、最快或最便宜。更合理的定义是一个可配置 score：

```text
path_score =
  success_weight * final_success
  + validation_weight * validation_strength
  + human_weight * human_acceptance
  - cost_weight * tokens
  - latency_weight * duration
  - retry_weight * repair_count
  - risk_weight * unsafe_or_manual_steps
```

不同场景权重不同：

- coding agent：更看重测试通过、diff 小、没有回归。
- research agent：更看重来源质量、覆盖度、引用准确。
- data agent：更看重结果可复现、schema 正确、成本可控。
- enterprise workflow：更看重合规步骤、人审节点、审计完整。

因此 yiTrace 应该发现的不是“全局唯一最优路径”，而是：

> 在某个 project / skill / mode / call_site / task class 下，历史上更可靠、更低成本、更少返工的路径模板。

### 1. Task Equivalence：先判断是不是同一类问题

如果不先聚类，最优路径会变成噪音。用户说“同一个问题问了很多遍”，系统需要把这些 trace 归到同一个 task class。

建议组合三种信号：

| 信号 | 用途 |
|---|---|
| 显式 attrs | `project_id`、`skill`、`mode`、`call_site`、`goal_id` |
| 文本相似 | 用户问题、目标描述、错误日志、关键文件名 |
| 结构相似 | 使用过的工具序列、验证器、失败类型、span DAG 形状 |

第一版不要追求完美自动聚类，可以先提供：

- `task_fingerprint`：接入方可显式传。
- `similar_task_search`：yiTrace 用 BM25 / vector 找相似历史 trace。
- 人工 merge/split：用户可以把一批 trace 标成同一类任务。

验收：

- 用户能查询“过去所有类似 native binding / npm pack / eval regression 的任务”。
- 系统能把多次相似尝试放到同一个 comparison set 里。

### 2. Trajectory Model：把 trace 变成可比较的路径

原始 span 太细，不能直接当“路径”。需要抽象成 trajectory：

```text
task_class: npm-native-packaging
trajectory:
  1. inspect_package_metadata
  2. build_native_module
  3. run_node_tests
  4. inspect_runtime_arch
  5. rebuild_target_artifact
  6. pack_tarball
  7. verify_clean_consumer
outcome:
  success: true
  validation: npm_test + pack_verify
  tokens: 18k
  duration: 12m
  repair_count: 1
```

抽象层级要介于“具体命令”和“空泛建议”之间：

- 太细：`sed -n 1,260p package.json` 这种无法复用。
- 太粗：`fix packaging` 没有指导价值。
- 合适：`inspect_runtime_arch`、`verify_clean_consumer` 这种可迁移步骤。

第一版可以用规则 + 少量 LLM 辅助：

- 工具名映射 step kind。
- 文件路径/命令归一化。
- 验证命令识别为 `verify` phase。
- LLM 只用于把多个低层 span 合成 step label，必须保留 source span ids。

验收：

- 同一类任务的多条 trace 能以 step sequence 对齐。
- 用户能看到 A 路径比 B 路径少了哪些验证步骤、在哪一步节省成本。

### 3. Golden Path Miner：找出候选最优路径

候选最优路径不是单次最高分就直接采用。需要防止偶然成功。

建议规则：

- 至少出现 `N >= 3` 次相似任务，或有人工确认。
- 成功率明显高于同类平均。
- 成本/耗时没有明显劣化。
- 验证强度不低于同类平均。
- 最近没有反例，或反例能被解释为不同 scope。

输出：

```text
golden_path_id: gp-npm-native-packaging-v1
scope:
  project_id: agentic-data
  skill: packaging
  call_site: yitrace-node
steps:
  - read package metadata
  - build native module for current Node arch
  - run npm test
  - pack root + platform tarballs
  - install tarballs in clean consumer
  - verify ESM/CJS/native load
evidence:
  positive_traces: [...]
  negative_traces: [...]
score:
  success_rate: 0.86
  median_tokens: 18k
  median_duration: 12m
  validation_strength: high
```

验收：

- 系统能推荐“这个 task class 当前最可靠路径”。
- 推荐结果必须能点击回原始 trace 证据。
- 用户能 reject / accept / narrow scope。

### 4. Path Reuse：后续怎么一直使用

这里不要直接“重放命令”。真实项目会变，文件名、依赖、错误上下文都可能不同。更稳的是把最优路径提炼成三种可用产物：

1. **Playbook**：人和 agent 都能读的步骤模板。
2. **Policy Hint**：给 agent 的简短策略提示，例如“遇到 native binding 问题，先检查 Node arch 与 .node 文件架构，再重建对应 target”。
3. **Eval Baseline**：后续类似任务必须包含的验证动作，例如 clean consumer 安装 tarball。

调用时机：

- 新任务进入时，用 `task_fingerprint` 或相似检索召回 golden path。
- agent planning 前，把 playbook / policy hint 放进上下文。
- agent 完成后，检查它是否执行了 golden path 里的关键验证步骤。
- 如果跳过关键步骤但仍宣称成功，标记 `weak_validation` 或 `premature_success`。

这就是 trace 数据的合理使用闭环：

```text
历史 trace
  -> 相似任务聚类
  -> 路径评分
  -> golden path
  -> planning hint / eval baseline
  -> 新 trace 验证
  -> 更新 golden path
```

验收：

- 相似问题再次出现时，agent 能自动拿到历史最佳路径提示。
- 用户能看到“本次执行是否遵循 golden path，哪里偏离了”。
- 如果新的路径比旧 golden path 更好，系统能提出升级候选。

### 5. 防止错误复用

最优路径复用有风险。yiTrace 必须保留几个刹车：

- **scope 明确**：路径只在 project / skill / call_site / task class 下生效。
- **过期机制**：依赖升级、工具版本变化、错误类型变化后降权。
- **反例记录**：新 trace 证明旧路径失败时，不能继续强推。
- **验证优先**：没有验证通过的路径不能成为 golden path。
- **人工确认**：影响生产或安全的路径要有人确认。

这和 Agent Memory 的区别：

- Agent Memory 记的是“经验”。
- Golden Path 记的是“做这类事时应该优先尝试的执行路径”。
- Eval 负责验证“这条路径是否仍然有效”。
- yiTrace 负责提供证据和持续更新。

## P2：Loop Memory Handoff

和 MemMe 的边界建议：

- yiTrace 不直接保存长期“经验记忆”。
- yiTrace 生成 memory candidate，附带证据 trace。
- MemMe 决定是否写入、如何召回、何时过期。

handoff 结构建议：

| 字段 | 说明 |
|---|---|
| `memory_candidate_id` | 候选 id |
| `scope` | project / skill / call_site / tool |
| `lesson` | 可复用经验 |
| `evidence_trace_ids` | 来源 trace |
| `positive_count` | 支持次数 |
| `negative_count` | 反例次数 |
| `last_seen_at` | 最近出现时间 |
| `risk` | 低/中/高，避免把偶然经验写进长期 memory |

验收：

- yiTrace 能导出“带证据的 memory candidate”。
- MemMe 不需要直接读 yiTrace 文件，只通过 API 拉候选和证据。
- memory 页面可以跳回 yiTrace trace detail。

## P2：Loop Optimization Handoff

更远一步可以做“给 Codex / Claude Code / 内部 agent 的改进交接单”，但这不该是第一期。

输入：

- 当前 harness version
- loop diagnostics
- 失败 trace
- human feedback
- eval results
- cost regression

输出：

- ranked harness changes
- suggested evals
- risk notes
- human review checklist

这里要有人类 gate。自动改 harness 风险很高，尤其在企业和金融场景。

## 数据库层影响

短期不需要大改内核，先用 `attrs` 和现有 trace/span/logEvents 即可验证产品价值。

中期如果 Loop Intelligence 成为核心模块，再考虑提升为一等索引字段：

- `loop_id`
- `iteration`
- `phase`
- `harness_version`
- `validation_status`
- `stop_reason`

原因：

- 这些字段会高频过滤和聚合。
- Loop Health 不应该扫描大文本。
- 列式读取 + late materialization 正好适合：先读窄字段算指标，需要证据时再拉 input/output/logEvents。

## 不做什么

第一阶段不要做：

- 不做 agent orchestrator。
- 不替代 Codex / Claude Code / LangGraph。
- 不自动修改用户代码。
- 不把所有 trace 自动总结进 memory。
- 不用 LLM 给每条 trace 做昂贵总结。
- 不把 “loop engineering” 当成纯营销词贴到 README。

yiTrace 的核心应该是：

> 让 loop 可见、可查、可评估、可复盘，并从历史 trace 中提炼可复用的最优路径。

## 推荐路线

### 第一周：契约和样例

- 写 `docs/LOOP_TRACE_CONTRACT.md`。
- 在 README 增加一小段：yiTrace 如何支持 loop engineering。
- 做一个样例 trace：同一个 `loop_id` 下 3 个 iteration，包含失败、修复、最终 pass。
- 确认现有 `attrs` filter 足够支撑最小查询。

### 第二周：Loop Health API 设计

- 设计 `/v1/loops`、`/v1/loops/:id`、`/v1/loops/:id/diagnostics`。
- 先不实现复杂 LLM，总结确定性指标。
- 明确和 `/v1/sessions` 的关系：session 是用户对话维度，loop 是执行闭环维度，两者可以一对多或多对一。
- 增加 task equivalence 设计：`task_fingerprint`、相似 trace 检索、comparison set。

### 第三周：控制台 Loop Explorer 原型

- 先用 mock 或 seed data。
- 列表 + iteration timeline + evidence panel。
- 点击能跳回 trace/span detail。
- 增加 Golden Path 对比原型：同一 task class 下多条 trajectory 的成功率、成本、验证强度对比。

### 第四周：Trace-to-Eval MVP

- 从失败 loop 生成 eval candidate。
- 人工确认后加入 dataset。
- eval item 保留 source trace/span 链接。
- 从成功 loop 生成 golden path candidate。
- golden path candidate 必须保留 positive/negative trace 证据。

## 成功标准

这个方向是否值得继续，看五个问题：

1. 用户是否能用 yiTrace 更快定位 loop 卡住原因？
2. 用户是否愿意把失败 trace 转成 eval？
3. 用户是否认为 Loop Health 比普通 trace list 更有价值？
4. MemMe 是否能从 yiTrace 提供的 evidence 中提炼出更可靠的 Agent Memory？
5. 用户是否愿意让 yiTrace 推荐某类任务的 golden path，并在后续任务中复用？

如果答案是肯定的，yiTrace 的定位可以从：

> agent trace database

升级为：

> trace database for agent loops, evals, golden paths, and memory evidence
