# trace aggregate / group-by 实现任务

> 日期：2026-07-03
> 状态：已实现基础版

## 背景

Agent 产品侧需要把 trace 数据用于 path mining 和 trace inbox 统计：例如同一个 project 下，哪些 `skill` / `mode` / `toolName` 组合最常出现、最贵、最容易失败。已有 `traceSearch()` 能找明细，但调用方还需要自行拉全量再聚合，成本和语义都不好控。

## 本次范围

- 新增 HTTP API：`POST /v1/trace-aggregate`，别名 `/v1/trace-aggregates`。
- 新增 Node API：`db.traceAggregate(query, options)`，ESM/CJS/类型声明同步。
- 复用 `/v1/trace-search` 的过滤语义：
  - tenant 强制来自调用上下文。
  - 支持 `traceId` / `sessionId` / `spanId`、status/kind/agent/tool/model。
  - 支持 text/input/output/log contains。
  - 支持 attrs exact/includes 过滤。
  - 支持 annotation/dataset association 反向过滤。
- 支持 group-by 字段：
  - `projectId` / `project_id`
  - `skill`
  - `mode`
  - `callSite` / `call_site`
  - `agentName` / `agent_name`
  - `toolName` / `tool_name`
  - `model`
  - `provider`
  - `kind`
  - `status`
  - `attrs.<key>` 或任意 attrs key
- 返回：
  - `spanCount`
  - `traceCount`
  - `errorCount`
  - `errorRate`
  - `durationNs` sum/avg/max/p50/p95/count
  - `usage`
  - `costUsd` / `costDetail`
  - `examples`

## 当前边界

- 当前实现是 snapshot 扫描后的内存聚合，不做物化统计。
- `groupBy` 的 attrs value 保持 JSON round-trip 形态，missing value 归到 `null`。
- examples 只保留每个 bucket 前 3 条，用于 UI drill-down，不是完整明细。
- 后续大规模看板可以继续补列式聚合下推、物化 daily/hourly rollup 或专门的 group-by 索引。

## 验证

- Rust HTTP 单测覆盖：
  - attrs filter 后按 `skill/mode` group。
  - `spanTotal`、`spanCount`、`traceCount`、`errorCount`。
  - duration、usage、cost 汇总。
  - `toolName` group-by。
- Node 集成测试覆盖：
  - `db.traceAggregate()` ESM 调用。
  - `skill/mode/toolName` key。
  - usage/cost 汇总。
  - tenant 隔离。
