# Agent Loop Read Model Task

> 日期：2026-07-03
> 状态：基础版已落地并通过全量验证

## 背景

P0.5 已把 `task_fingerprint`、`loop_id`、`harness_version`、`validation_status`、`stop_reason`、`phase`、`validator` 提升为一等字段。下一步不能再要求 AgenticData 逐 session / trace 扫描后自己拼 loop 和 task 页面，否则每个调用方都会重复实现同一套聚合逻辑。

## 已实现

- HTTP / 进程内 API：
  - `GET /v1/loops`
  - `GET /v1/loops/:loopId`
  - `GET /v1/tasks/:fingerprint/traces`
- Node / Electron API：
  - `db.loops(options)`
  - `db.loop(loopId, options)`
  - `db.taskTraces(taskFingerprint, options)`
- 过滤能力：
  - attrs 精确过滤，包括 P0.5 一等字段别名。
  - annotation / dataset association 反向过滤。
  - `filter` / `text` / `q` contains 过滤。
- 返回能力：
  - loop 摘要：span/trace/session/error/duration/usage/cost/phases/validators/examples。
  - loop 详情：summary + traces + spans。
  - task traces：按 `task_fingerprint` 返回 trace 摘要页。
- 测试：
  - Rust 路由测试覆盖 loop/task API 和租户隔离。
  - Node 集成测试覆盖 `db.loops()` / `db.loop()` / `db.taskTraces()`。

## 当前边界

- 当前实现复用 folded snapshot 扫描聚合，不是物化 loop/task 索引。
- `loopId` 和 `taskFingerprint` 当前按字符串路径参数匹配，对数字/复杂 JSON 值仍建议通过 attrs filter 传。
- 不做自动 loop diagnosis、golden path mining 或 path adherence 判断，只提供稳定读模型。

## 后续

- loop/task 变成高频页面后，补物化索引：
  - tenant + loop_id -> trace/span keys
  - tenant + task_fingerprint -> trace ids
  - loop/task 级 usage/cost/error rollup
- 增加按 validation_status / stop_reason / duration / cost 的 range 或排序能力。
- 与 Trace Diff / Golden Path candidate store 打通。

## 验证

已通过 targeted 验证：

- `cd yitrace-engine && cargo fmt && cargo test --offline route_loop_and_task_read_models`
- `cd yitrace-node && npm run build && npm test`

全量验证已通过：

- `cd yitrace-engine && cargo test --offline`
- `git diff --check`
