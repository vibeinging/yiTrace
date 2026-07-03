# First-Class Loop and Task Fields Task

> 日期：2026-07-03
> 状态：基础版已落地并通过全量验证

## 背景

AgenticData 的 loop health、task group、path mining、trace diff 和 eval 回放都需要稳定维度。仅把这些维度放在 `attrs` JSON 里，早期能跑通，但后续过滤、分组、导出和列式下推都会反复解析 JSON，也很难形成清晰的对外契约。

这次把第一批高频字段从 attrs fallback 提升为折叠后的稳定 schema：

- `task_fingerprint`
- `loop_id`
- `harness_version`
- `validation_status`
- `stop_reason`
- `phase`
- `validator`

它们和已有 `project_id`、`skill`、`mode`、`call_site` 一起成为第一批 AgenticData / agent workflow 一等字段。

## 已实现

- `SpanFields` / `FoldedSpan` 增加第一批字段。
- WAL/segment 共享 `SpanFields` 编码升级到 v5，兼容旧 v2-v4 数据。
- direct wire ingest 支持 snake_case 和 camelCase 顶层别名，并保留原始 `attrs` round-trip。
- OTLP/OpenInference 映射支持 `yitrace.*`、`agent.*`、`task.*`、`loop.*`、`validation.*` 相关别名。
- search、traces、sessions、traceSearch、traceAggregate 过滤时优先读一等字段，回退 attrs。
- traceAggregate 支持按 `taskFingerprint`、`loopId`、`harnessVersion`、`validationStatus`、`stopReason`、`phase`、`validator` 分组。
- Node builder、ESM/CJS wrapper、类型声明和测试覆盖新增字段。
- API_REFERENCE、Node README、CURRENT_STATE、AGENTS.md 和底层设计文档已同步字段契约。

## 兼容性

- 旧数据如果只在 `attrs` 里有这些字段，查询仍能命中。
- 新数据如果只写一等字段、不镜像 attrs，也能被过滤、分组和 `fields` 输出命中。
- `attrs` 返回形态仍保持 JSON round-trip，不因字段提升改变调用方看到的原始扩展对象。

## 当前边界

- 这些字段目前都是 exact string/JSON equality 维度，不做 range 查询。
- `traceAggregate()` 仍是 folded snapshot 扫描聚合，后续可做列式下推或物化统计。
- 第二批业务字段 `schema_fingerprint`、`intent_signature`、`review_status`、`eval_status`、`path_memory_id` 暂不急于全部入列，先看真实查询频率、基数和是否需要排序/group-by。

## 验证

已通过 targeted 验证：

- `cd yitrace-engine && cargo check --offline`
- `cd yitrace-engine && cargo fmt && cargo test --offline first_class_agentic_fields_filter_without_attrs_after_recover`
- `cd yitrace-engine && cargo test --offline span_fields_v2_roundtrips_external_ids_and_attrs`
- `cd yitrace-engine && cargo test --offline hashes_external_ids_and_preserves_attrs`

全量验证已通过：

- `cd yitrace-engine && cargo test --offline`
- `cd yitrace-node && npm run build && npm test`
- `git diff --check`
