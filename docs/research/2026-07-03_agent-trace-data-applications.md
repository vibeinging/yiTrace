# Agent Trace 数据应用深度调研

> 日期：2026-07-03
> 范围：学界论文/数据集 + GitHub/开源项目 + 对 yiTrace 的产品机会判断。

## 结论

Agent trace 数据正在从“可观测日志”变成三类更有价值的数据资产：

1. **行为证据**：用于调试、回放、失败归因、成本归因、安全审计。
2. **评测资产**：用于从失败 trace 生成 eval、回归测试、agent 版本对比。
3. **经验资产**：用于挖掘最优路径、生成 playbook、训练/微调 agent、沉淀 Agent Memory。

开源生态里，Langfuse、Phoenix、AgentOps、OpenInference 已经把“采集 trace + 展示 + eval”做成主流形态；学界则更进一步，把 trajectory 当作训练数据、过程监督数据、检索监督数据、workflow 优化数据。

yiTrace 的机会不在“再做一个 SaaS 观测平台”，而在更底层、更数据化的方向：

> 做一个本地/私有化 TraceDB，把 agent trace 变成可查询、可比较、可评分、可导出、可复用的数据资产。

最值得优先做的应用是：

- **Trace-to-Eval**：失败 trace 变成 eval item。
- **Golden Path Mining**：多次相似任务里找出最优执行路径。
- **Trajectory Comparison**：比较两个 agent/harness 版本为什么一个成功一个失败。
- **Memory Evidence Export**：给 Agent Memory 提供带证据的候选经验。
- **Training Data Export**：把高质量 trajectory 导出为 SFT/RL/process supervision 数据。

## 术语

这里把几个词分清楚：

| 词 | 含义 |
|---|---|
| trace | 一次 agent 运行的完整记录，包含多轮、span、工具调用、模型输入输出、日志、验证结果 |
| span | trace 内的一步，如模型调用、工具调用、测试、检索、人工反馈 |
| trajectory | 把 trace 抽象成可比较的行动路径：步骤序列 + 观察 + 验证 + 结果 |
| loop | agent 在 plan / act / observe / verify / repair 之间迭代的闭环 |
| golden path | 某类任务下历史上更可靠、成本更低、验证更充分的 trajectory 模板 |
| memory candidate | 从 trace 中提炼出的经验候选，需要证据和 scope，不等于直接写入长期记忆 |

## 学界方向

### 1. Trajectory Analysis：把 agent 运行过程当成研究对象

近期已经出现专门研究 LLM agent trajectory analysis 的工作。HuggingFace 上的 “A Survey for LLM Agent Trajectory Analysis” 把相关论文整理成数据集，说明这个方向已经形成独立问题域。

核心关注点：

- agent 在多步任务里如何失败。
- 哪些 action / observation 影响最终结果。
- 能否从 trajectory 判断模型能力边界。
- 能否自动归因失败是 plan 错、tool 错、observation 错，还是验证不足。

对 yiTrace 的启发：

- trace 不能只存原始日志，要支持 trajectory-level query。
- 需要有 outcome、phase、validation、stop_reason 这些字段。
- 需要支持“同类任务下多条 trajectory 对比”。

参考：

- A Survey for LLM Agent Trajectory Analysis: https://huggingface.co/datasets/RobinChen2001/A-Survey-for-LLM-Agent-Trajectory-Analysis

### 2. Agent Benchmark：trace 是 benchmark 的核心数据

WebArena、AgentBoard、AgentGym、AgentTrek 等项目都围绕 agent 在复杂环境中的多步行为展开。它们的重点不是单次问答，而是完整轨迹：任务目标、行动序列、环境反馈、最终成功与否。

代表项目：

| 项目 | 作用 | GitHub 状态 |
|---|---|---|
| WebArena | 真实网站环境中的 autonomous agent benchmark | `web-arena-x/webarena`，约 1.5k stars |
| AgentBoard | 多轮 LLM agents 分析评测板，NeurIPS 2024 Oral | `hkust-nlp/AgentBoard`，约 423 stars |
| AgentGym | 多环境 agent 训练/演化框架，ACL 2025 | `WooooDyy/AgentGym`，约 807 stars |
| AgentTrek | 通过 web tutorial 引导 replay 合成 agent trajectory，ICLR 2025 Spotlight | `xlang-ai/AgentTrek`，约 60 stars |

对 yiTrace 的启发：

- benchmark 本质上需要一个 trace store。
- 如果 yiTrace 能把真实生产 trace 转成 benchmark/eval 数据，会比只跑公开 benchmark 更有商业价值。
- “trajectory replay + validation + dataset” 可以成为 yiTrace 的平台能力。

参考：

- WebArena: https://github.com/web-arena-x/webarena
- AgentBoard: https://github.com/hkust-nlp/AgentBoard
- AgentGym: https://github.com/WooooDyy/AgentGym
- AgentTrek: https://github.com/xlang-ai/AgentTrek

### 3. Agent Improvement Loop：trace -> feedback -> eval -> harness 改进

OpenAI Cookbook 的 agent improvement loop 很接近 yiTrace 可以落地的产品线：先用 traces 观察 agent 行为，再用 human feedback 标注，再生成 eval 固化期望，最后让实现工具改进 agent harness。

这个方向的本质：

```text
trace
  -> feedback
  -> eval
  -> harness change
  -> new trace
  -> regression comparison
```

对 yiTrace 的启发：

- yiTrace 不需要直接改 agent 代码。
- yiTrace 应该把 trace 到 eval 的前半段做扎实。
- 每个 eval item 都应该保留 source trace/span 链接。
- 回归结果应该能按 harness_version / agent_version 对比。

参考：

- OpenAI Cookbook, Build an Agent Improvement Loop with Traces, Evals, and Codex: https://developers.openai.com/cookbook/examples/agents_sdk/agent_improvement_loop

### 4. Trajectory 数据用于训练和自我改进

学界和开源社区已经在尝试把 agent trajectory 用作训练数据：

- 成功 trajectory 可用于 behavior cloning / SFT。
- 失败 trajectory + 修复过程可用于 process supervision。
- 多条候选 trajectory 的偏好比较可用于 preference learning。
- 高质量 replay trajectory 可用于生成训练样本。

这说明 trace 数据未来不只是观测数据，也可能是“agent 训练数据”。

对 yiTrace 的启发：

- 需要把 trace 导出成训练友好的格式。
- 不能只导出最终答案，要导出 action / observation / thought-like step / validation。
- 要区分可训练数据和敏感数据，支持脱敏、过滤、采样。

可能的导出形态：

```json
{
  "task": "...",
  "trajectory": [
    {"role": "agent", "action": "inspect_file", "input": "..."},
    {"role": "environment", "observation": "..."},
    {"role": "agent", "action": "run_test", "input": "npm test"},
    {"role": "environment", "observation": "passed"}
  ],
  "outcome": {"success": true, "cost_tokens": 18000},
  "source_trace_id": "..."
}
```

### 5. Workflow Optimization：从静态模板到动态运行图

IBM 的 awesome-agentic-workflow-optimization 收集了 agent workflow optimization 相关论文，核心问题是：agent workflow 不应只是手写模板，而应根据运行反馈动态优化。

trace 数据在这里的作用：

- 统计哪个 workflow 分支更常成功。
- 找出无效步骤和冗余调用。
- 判断什么时候需要 human gate。
- 基于历史结果优化 tool order / validator placement / retry policy。

对 yiTrace 的启发：

- Golden Path Mining 是 workflow optimization 的数据层。
- yiTrace 可先做“发现”和“建议”，不直接自动改 workflow。
- 需要有 trajectory comparison 和 path score。

参考：

- IBM awesome-agentic-workflow-optimization: https://github.com/IBM/awesome-agentic-workflow-optimization

## GitHub / 开源生态方向

### 1. AI Observability 平台

这类项目已经比较成熟，证明“trace 采集 + UI + eval”是刚需。

| 项目 | 定位 | GitHub 状态 |
|---|---|---|
| Langfuse | Open source AI engineering platform: evals、observability、metrics、prompt、datasets | `langfuse/langfuse`，约 30.3k stars |
| Phoenix | AI observability & evaluation | `Arize-ai/phoenix`，约 10.4k stars |
| AgentOps | AI agent monitoring、cost tracking、benchmarking | `AgentOps-AI/agentops`，约 5.7k stars |
| Braintrust | evals、logs、prompt experiments、datasets | `braintrustdata/braintrust` 生态 |
| Helicone / Laminar / LangSmith | 观测、eval、prompt 管理、线上 tracing | 不同程度开源或托管 |

它们说明：

- trace 已经是 LLM app/agent engineering 的基础设施。
- observability alone 已经竞争激烈。
- eval + dataset + prompt/workflow 版本管理正在成为平台标配。

yiTrace 不应正面复制它们完整平台，而应突出：

- 本地/私有化可嵌入。
- TraceDB 内核能力。
- 中文检索 + 向量召回 + 列式/晚物化。
- trace 作为数据资产的深度使用。

参考：

- Langfuse: https://github.com/langfuse/langfuse
- Phoenix: https://github.com/Arize-ai/phoenix
- AgentOps: https://github.com/AgentOps-AI/agentops

### 2. Trace 标准化：OpenTelemetry / OpenInference / Agent Data Protocol

OpenInference 和 OpenTelemetry GenAI semantic conventions 正在把 LLM/agent trace 规范化。Agent Data Protocol 则尝试为 agent 轨迹数据提供更标准的表示。

这说明生态会越来越重视 interoperability：

- span 如何表示 tool call。
- prompt、completion、token、cost 如何记录。
- eval / feedback / dataset 如何关联 trace。
- trajectory 如何跨框架导出。

对 yiTrace 的启发：

- 摄入必须兼容 OTLP/OpenInference。
- 导出可以考虑 ADP / JSONL / OpenAI fine-tuning style。
- yiTrace 自己的 schema 不应封闭成孤岛。

参考：

- OpenInference: https://github.com/Arize-ai/openinference
- OpenTelemetry GenAI semantic conventions: https://opentelemetry.io/docs/specs/semconv/gen-ai/
- Agent Data Protocol: https://github.com/neulab/agent-data-protocol

### 3. Agent Evals：trajectory evaluator 正在工具化

LangChain 的 `agentevals` 明确定位为 “readymade evaluators for agent trajectories”。这说明 eval 的对象正在从 final answer 扩展到完整 trajectory。

trajectory evaluator 可以评估：

- agent 是否调用了正确工具。
- 是否重复无效动作。
- 是否跳过验证。
- 是否在错误 observation 后继续推进。
- 是否有过度调用或成本异常。

对 yiTrace 的启发：

- yiTrace 可以内置确定性 evaluator。
- LLM judge evaluator 可以作为可选外部模块。
- evaluator 结果应写回 trace，成为可过滤字段。

参考：

- AgentEvals: https://github.com/langchain-ai/agentevals

### 4. Coding Agent / SWE Agent 的真实轨迹

OpenHands、SWE-agent、AutoCodeRover、RepairAgent 这类 coding agent 的共同点是：最终能否解决 issue，不只取决于模型答案，而取决于完整执行轨迹。

有研究开始分析 automated program repair agent 的 action trajectories，比较不同 agent 在同一 bug 上的行为差异。

对 yiTrace 的启发：

- coding agent 是 Golden Path Mining 最适合的首个垂直场景。
- 因为它有天然验证器：tests、typecheck、lint、build、diff。
- 最优路径很容易定义：通过测试、改动小、成本低、步骤可解释。

可做功能：

- 同一 bug/issue 多次尝试比较。
- 找出“先读哪些文件、先跑哪些测试、何时搜索”的高成功路径。
- 提炼项目级 playbook。
- 自动发现 weak validation：只改代码但没跑相关测试。

参考：

- OpenHands: https://github.com/All-Hands-AI/OpenHands
- SWE-agent: https://github.com/SWE-agent/SWE-agent

## 应用分类与成熟度

| 应用 | 成熟度 | 现有代表 | yiTrace 机会 |
|---|---:|---|---|
| trace 采集/回放/瀑布图 | 高 | Langfuse、Phoenix、AgentOps、LangSmith | 必须有，但不是差异化核心 |
| 成本/token/latency 归因 | 高 | observability 平台普遍支持 | 结合本地 TraceDB 和列式聚合 |
| Trace-to-Eval | 中高 | OpenAI cookbook、Braintrust、Langfuse datasets | 做成 yiTrace 核心 workflow |
| trajectory evaluator | 中 | AgentEvals、研究 benchmark | 内置确定性 evaluator + 可选 LLM judge |
| failure attribution | 中 | 学界 trajectory analysis | 用 trace schema + diagnostics 做产品化 |
| Golden Path Mining | 低到中 | workflow optimization 研究，少量平台功能 | 重点机会，适合 yiTrace |
| Agent Memory evidence | 低到中 | Reflexion/Voyager 类思想，产品少 | 与 MemMe 联动 |
| training data export | 中 | AgentGym、AgentTrek、ADP、各种 SFT/RL 数据 | 提供高质量 trajectory export |
| workflow optimization | 低到中 | IBM survey、LangGraph 生态 | yiTrace 做建议层，不做 orchestrator |
| safety/audit/compliance trace | 中 | OTel/observability、安全日志 | 私有化和审计场景有价值 |

## 对 yiTrace 的功能机会

### 机会 1：Trace-to-Eval

这是最近、最容易落地，也最容易让用户理解的方向。

功能：

- 从失败 search result / loop / span 生成 eval candidate。
- 保留 source trace/span。
- 支持 expected behavior、prohibited behavior、deterministic assertion、optional judge rubric。
- 跑 eval 后写回 trace 和 harness_version。

为什么适合 yiTrace：

- 已有 trace/search/eval 基础。
- 不需要先解决复杂聚类。
- 很适合 README 和 demo。

### 机会 2：Golden Path Mining

这是你刚提出的核心方向：一个问题问了很多遍，中间有一次最优路径，系统要发现并复用。

功能：

- `task_fingerprint` / similar task search。
- trajectory 抽象：把 span 合成 step sequence。
- path score：成功率、验证强度、成本、耗时、重试、人工反馈。
- golden path candidate：带 positive/negative trace 证据。
- path adherence：新任务是否遵循 golden path。

为什么适合 yiTrace：

- 需要查询大量历史 trace，正是 DB 强项。
- 需要比较路径、成本、验证结果，观测平台通常做得浅。
- 可以直接服务 Agent Memory 和 eval。

### 机会 3：Trajectory Comparison

用户最常问的问题不是“这条 trace 是什么”，而是：

> 为什么这次失败，上次成功？

功能：

- 选两条相似 trace。
- 对齐 step sequence。
- 标出分叉点。
- 比较工具调用、验证器、成本、错误、logEvents。
- 生成“关键差异”。

MVP 可以先规则化：

- 工具序列 diff。
- 验证结果 diff。
- 错误日志相似度。
- input/output token 差异。

### 机会 4：Memory Evidence Export

Agent Memory 最怕 hallucinated memory。yiTrace 可以提供证据约束。

功能：

- 从多条 trace 生成 memory candidate。
- 每条 candidate 带 evidence trace ids。
- 有 scope、positive_count、negative_count、last_seen_at、risk。
- MemMe 或其他 memory 系统通过 API 拉取。

边界：

- yiTrace 不直接做 memory recall。
- yiTrace 只做证据和候选。

### 机会 5：Training Data Export

这是中长期方向。用户未必一开始就训练模型，但会需要把优质 trajectory 导出给 fine-tuning / reward modeling / process supervision。

功能：

- 按 score 过滤高质量 trace。
- 脱敏和字段裁剪。
- 导出 JSONL / ADP-like trajectory。
- 保留 outcome 和 validation。

注意：

- 这会碰隐私、安全和数据治理。
- 企业用户会需要项目/租户隔离和脱敏策略。

## 建议的数据模型扩展

短期仍然用 `attrs`，但要把字段约定清楚：

| 字段 | 用途 |
|---|---|
| `task_fingerprint` | 同类任务聚合 |
| `loop_id` | 一次 loop run |
| `iteration` | loop 第几轮 |
| `phase` | plan/act/observe/verify/repair |
| `harness_version` | workflow/prompt/agent harness 版本 |
| `agent_version` | 模型或 agent 配置版本 |
| `validator` | 测试/检查/人工反馈来源 |
| `validation_status` | pass/fail/skipped |
| `stop_reason` | goal_met/max_iterations/budget_exceeded/error |
| `trajectory_step` | 抽象后的步骤名 |
| `task_class` | 人工或系统聚类后的任务类 |
| `golden_path_id` | 命中的 golden path |
| `path_adherence` | followed/partial/deviated |

中期提升为一等列：

- `task_fingerprint`
- `loop_id`
- `harness_version`
- `validation_status`
- `stop_reason`
- `trajectory_step`
- `golden_path_id`

原因：这些字段会高频过滤、聚合、排序、对比，不适合长期只做 attrs 字符串过滤。

## 建议 API 草案

先只做设计，不急着实现：

```text
GET  /v1/tasks/similar?trace_id=...
GET  /v1/tasks/:fingerprint/traces
POST /v1/trajectories/extract
POST /v1/trajectories/compare
GET  /v1/golden-paths?task_fingerprint=...
POST /v1/golden-paths/:id/status
GET  /v1/traces/:id/path-adherence
POST /v1/evals/items/from-trace
GET  /v1/memory-candidates
POST /v1/exports/trajectories
```

## 产品优先级

### P0：把 trace 用起来

- Trace-to-Eval。
- Loop Health。
- task_fingerprint / loop attrs contract。
- trace comparison 的最小规则版。

### P1：从 trace 里发现路径

- trajectory extraction。
- similar task grouping。
- golden path candidate。
- path adherence。

### P2：把路径和经验外送

- memory candidate export。
- trajectory training data export。
- ADP/JSONL export。
- workflow optimization suggestion。

## 风险

### 1. Trace 质量不够

如果 agent 没打 validation、phase、outcome，trace 再多也很难挖出最优路径。

缓解：

- 提供 builder/helper。
- 给接入方最小字段 contract。
- 控制台提示 weak instrumentation。

### 2. 相似任务聚类错误

把不同任务归为同类，会推荐错误路径。

缓解：

- 第一版允许人工 merge/split。
- golden path 必须有 scope。
- 反例会降权。

### 3. 偶然成功被当成最优路径

一次成功可能只是运气。

缓解：

- N 次支持或人工确认。
- 记录 negative traces。
- 需要 validation_strength。

### 4. 训练数据泄露敏感信息

trace 里可能有 API key、代码、客户数据。

缓解：

- export 前脱敏。
- tenant/project 权限。
- 字段白名单。

## 对外叙事

可以把 yiTrace 从：

> A single-binary trace database for AI agents.

逐步升级为：

> A trace database that turns agent runs into evals, golden paths, and memory evidence.

中文：

> yiTrace 不只是存 agent trace，它把执行记录变成 eval、最优路径和记忆证据。

## 下一步建议

1. 补 `docs/LOOP_TRACE_CONTRACT.md`，把 loop/task/validation/golden path 所需 attrs 约定清楚。
2. 做一个 seed demo：同一个任务跑 3 次，1 次失败、1 次绕远、1 次最优。
3. 控制台先做 comparison view，不急着自动 mining。
4. 从 coding agent 场景切入，因为它最容易定义成功和验证。
5. 等 comparison view 有价值后，再做 golden path candidate。
