# Trace Diff 基础版任务记录

> 日期：2026-07-03
> 状态：已落地，并通过全量验证

## 背景

上层产品方向需要从 trace 数据里比较多次相似任务的执行路径，找出更优路径并沉淀为后续默认策略。底层库第一步不应该直接做“哪条更好”的判断，而是提供稳定、可复核的 diff 证据。

## 本次落地

- HTTP API：`POST /v1/traces/diff`，兼容别名 `POST /v1/trace-diff`。
- Node API：`db.traceDiff(leftTraceId, rightTraceId, options?)` 和 `db.traceDiff({ leftTraceId, rightTraceId }, options?)`。
- trace id 支持数字 id 和外部字符串 id。
- 响应包含：
  - `left` / `right` trace 摘要。
  - 整体 `delta`：span/error/duration/token/cost。
  - `trajectory.left/right`：规范化 steps、稳定 `fnv1a64` signature 和 `same` 布尔值。
  - `routes.left` / `routes.right`：两侧执行路径。
  - `steps`：按 span 顺序输出 `same` / `changed` / `left_only` / `right_only`，并给出字段变化和 per-step delta。
  - span detail 中返回 `evalScore` / `evalLabel`，并把 eval 变化纳入 `changes`，方便 golden path / regression 页面直接使用。
- 租户隔离沿用 `EngineJsonApi.route_with_tenant`，tenant 不匹配时返回 404。

## 边界

- 当前是确定性结构 diff，不做 LLM 判优，不自动挑 golden path。
- 当前按 folded span 顺序比较，不做复杂路径对齐。
- trajectory signature 当前是规则派生：`kind:name|phase:x|validator:y` 的规范化序列，再做 FNV-1a 64；它适合做同路径/不同路径 evidence，不等同业务最优路径 id。
- SQL/table/field 等专门字段级 diff 还需要后续在高频字段或 attrs registry 明确契约后继续补。
- 性能当前是 snapshot folded span 读取；如果 trace diff 成为高频入口，再补 trace-level cache 或列式投影下推。

## 验证计划

- `cd yitrace-engine && cargo test --offline`：通过。
- `cd yitrace-node && npm run build && npm test`：通过。
- `git diff --check`：通过。

## 2026-07-03 追加验证

- 新增 eval harness 用例：两条同类任务 trace 先经规则 scorer 写回 eval，再用 trace diff 验证 route、失败状态、成本 delta 和 eval 分数差异。
