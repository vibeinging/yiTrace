# Trajectory Groups 基础版任务记录

> 日期：2026-07-03
> 状态：已落地基础版

## 背景

上层需要从多次相似 agent 执行里发现可复用路径：同一个问题问很多遍时，可能只有一条路径稳定、便宜、成功率高。单条 `traceDiff()` 能比较两次运行，但不能回答“这一批 trace 里哪种路径最好”。因此需要 trace 维度的 trajectory 聚合。

## 本次落地

- HTTP API：`POST /v1/trajectory-groups`。
- 兼容别名：`POST /v1/trajectory-aggregate`、`POST /v1/best-paths`。
- Node API：`db.trajectoryGroups(query, options?)`。
- 查询过滤复用 `/v1/trace-search`：支持 text、status、tool/model、attrs、annotation、dataset 等过滤。
- 聚合逻辑：
  - 先用过滤条件找到候选 trace。
  - 再读取每条 trace 的完整 folded spans。
  - 用 `tool/agent/model + phase + validator` 生成 normalized steps。
  - 对 steps 做稳定 `fnv1a64` trajectory signature。
  - 按 signature 分桶。
- 响应包含：
  - `signature`、`steps`、`stepCount`。
  - `traceCount`、`spanCount`、`successCount`、`successRate`、`errorTraceCount`。
  - duration、usage、cost 聚合。
  - eval / annotation / dataset score stats。
  - examples，用于 UI drill-down。

## 边界

- 这是 golden path mining 的候选证据层，不创建 golden path 资产。
- `qualityScore` 是确定性启发式：success rate 是基础证据，有 eval / annotation / dataset 分数时纳入平均；它不是 LLM 判断。
- 当前是 snapshot 扫描基础版，没有 trajectory 物化索引。
- 如果 trajectory groups 成为高频入口，后续应补：
  - tenant/project/task 维度的 trajectory postings 或 rollup。
  - golden path candidate store。
  - path adherence 检查。
  - candidate 生命周期治理和人工确认状态机。

## 验证

- `cd yitrace-engine && cargo test --offline trajectory_groups -- --nocapture`：通过。
- `cd yitrace-node && npm run build && npm test`：通过。
