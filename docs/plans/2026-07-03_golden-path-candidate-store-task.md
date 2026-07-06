# Golden Path Candidate Store 落地记录

日期：2026-07-03

## 背景

`trajectoryGroups()` 已能从 trace 数据里发现“同类任务下反复成功的执行路径”，但它只是候选证据层：每次查询都重新按 trajectory signature 分桶，不保存产品决策结果。

这次补 Golden Path Candidate Store，把“某条 trace/trajectory 被确认成 golden path”作为 tenant-scoped 元数据保存下来，供后续 path adherence、回归集、Agent Memory 候选导出复用。

## 存储语义

Golden Path 不复制 trace payload，只保存：

- `source_trace_id` / `external_source_trace_id`
- `trajectory_signature`
- 轻量 `source_trajectory_steps`
- `task_fingerprint`
- 可选 `snapshot_id` / `snapshot_hash`
- `status`: `candidate` / `confirmed` / `rejected` / `deprecated`
- `score` / `label` / `reason` / `source`
- 过滤用 `attrs`
- `evidence` 摘要，例如 sample count、success rate、source status、tokens/cost、source trajectory signature

当前版本不做重复命中记录、引用计数或 raw trace 压缩。重复场景仍按正常 ingest 进入 TraceDB，用于排障、eval、成本分析；是否长期保留由 raw trace retention policy 决定。hit/reference count、重复 trace 压缩、只保留 canonical source snapshot 等能力后续作为单独需求设计。

## Golden Path 是 trace 要做的吗

Golden Path 不是 raw trace 本身，也不应该由 trace 层自动宣称“这就是最优”。更准确的分工是：

- TraceDB 负责保存执行证据：span、事件、工具调用、输入输出、耗时、成本、eval、annotation、dataset link。
- TraceDB 可以派生候选路径：从 trace 折叠出 trajectory steps，计算 signature，按 task/scope 聚合，给出成功率、成本、耗时和分数证据。
- Golden Path 是上层可治理资产：它引用一条 source trace 或 snapshot，表示“这个 scope 下当前认可的一条可复用执行路径”。
- Best 判定需要业务规则或产品层 adjudicator：同一 scope 下比较候选路径，用 eval profile、成功率、样本数、成本、耗时、人工确认等证据决定当前推荐路径。TraceDB 只提供可复核证据。

所以 yiTrace 侧应该做的是底座能力，而不是替所有业务写死“最优”的定义：

1. 把 trace 还原成稳定 trajectory。
2. 按 `tenant + project_id + task_fingerprint + skill + mode + schema/model/tool version` 等 scope 分桶。
3. 提供 trajectory group、trace diff、eval/annotation/dataset 证据。
4. 保存 Golden Path candidate 的 source trace/snapshot 引用和评审状态。
5. 提供 path adherence 这样的确定性对比 API，让业务配置评分规则后再做推荐或替换决策。
6. 提供 evidence bundle，把 source trace 轨迹、annotation、dataset link 和可选 candidate diff 打包给上层 review/export。
7. 提供稳定 JSONL export schema，把 confirmed Golden Path 交给 Agent Memory / regression dataset 管线。
8. 提供 health/read model，按同 scope 后续 trace 批量统计 followed/extended/partial/deviated，用数据告诉产品层旧路径是否还稳定。

一句话：Golden Path 是“trace 数据之上的路径资产”，不是 trace 原始数据本身。

## 如何避免把旧 Golden Path 误当成永远最优

严格说，系统不能保证“之前确认过的 Golden Path 永远就是 Best Golden Path”。更合理的语义是：

> confirmed Golden Path 是某个 scope 下、基于当时 eval/profile/数据分布被认可的一条可复用路径。

它需要持续被新 trace 证据校验，而不是一次确认后永久冻结。

需要补的机制：

1. **scope 必须收窄**：Best 只能在明确范围内成立，例如 `tenant + project_id + task_fingerprint + skill + mode + harness_version/schema_fingerprint`。跨模型、跨工具版本、跨数据源 schema 的比较不应混在一起。
2. **候选池而不是单点结论**：同一 scope 下可以有多条 candidate/confirmed path。`confirmed` 表示可用，不等于数据库内置“唯一最优”。
3. **证据要可复算**：每条候选路径都要绑定 eval profile、样本窗口、成功率、平均/尾部耗时、token/cost、人工 annotation、dataset score 和样本数。
4. **需要产品层 challenger 机制**：新 trace 如果 trajectory 不同，且在相同 eval profile 下稳定更好，可以进入 challenger；超过一定 margin 和最小样本数后，产品层再决定是否替换推荐路径。
5. **需要过期和降级**：模型版本、工具版本、schema、prompt、数据分布变化后，旧路径只能是 stale candidate，需要重新评估。连续失败或 path adherence 下降时，产品层可以降为 deprecated 或 candidate。
6. **不要只看均分**：平均分高但 P95 很慢、成本很高、偶发失败多，都不应该直接当 best。Best score 应该是多目标评分：质量优先，成本/耗时作为 tie-breaker 或约束。

一个保守的替换规则可以是：

```text
same_scope(candidate, baseline)
AND candidate.eval_profile == baseline.eval_profile
AND candidate.sample_count >= min_samples
AND candidate.success_rate >= baseline.success_rate + margin
AND candidate.quality_score >= baseline.quality_score + margin
AND candidate.p95_duration <= baseline.p95_duration * max_slowdown
AND candidate.cost <= baseline.cost * max_cost_ratio
THEN product layer may promote candidate
```

所以当前 Store 只是第一步：它让 Golden Path 从“查询结果”变成“可治理资产”。TraceDB 后续重点仍是底座证据：trajectory group、trace diff、path adherence、annotation/dataset/eval 关联；真正“Best”的判定留给产品层策略。

## API

- `POST /v1/trace-trajectories`
- `POST /v1/golden-paths`
- `GET /v1/golden-paths`
- `POST /v1/golden-paths/:id/status`
- `POST /v1/path-adherence`
- `POST /v1/golden-paths/:id/adherence`
- `POST /v1/golden-path-evidence`
- `POST /v1/golden-paths/:id/evidence`
- `POST /v1/golden-path-export`
- `POST /v1/golden-paths/export`
- `POST /v1/golden-path-health`
- `POST /v1/golden-paths/:id/health`

Node/Electron：

- `db.traceTrajectories(query)`
- `db.createGoldenPath(candidate)`
- `db.goldenPaths(filter)`
- `db.updateGoldenPathStatus(goldenPathId, update)`
- `db.pathAdherence(goldenPathId, traceId)` / `db.pathAdherence({ goldenPathId, traceId })`
- `db.goldenPathEvidence(goldenPathId)` / `db.goldenPathEvidence({ goldenPathId, candidateTraceId })`
- `db.goldenPathExport({ filter, limit })`
- `db.goldenPathHealth(goldenPathId, query)` / `db.goldenPathHealth({ goldenPathId, filter, limit })`

## 已验证

- Rust metadata v3 round-trip：Golden Path 保存 source trajectory steps 与 evidence summary。
- Rust HTTP durable 测试：创建、确认、trace trajectories、path adherence、evidence bundle、JSONL export、health 统计、tenant 隔离、重启恢复。
- Node 集成测试：ESM/CJS 包装层、external string trace id、traceTrajectories、path adherence、evidence bundle、JSONL export、health 统计、重启恢复。

## 2026-07-03 P0/P1 补齐

- P0 scope contract：创建 Golden Path 时自动从 source trace 补齐 `project_id`、`task_fingerprint`、`skill`、`mode`、`harness_version`、`schema_fingerprint`、`eval_profile`、`model`、`provider`、`tool_version`，用于同 scope 对比和查询过滤。
- P0 materialized trajectory read model：新增 `POST /v1/trace-trajectories` / `POST /v1/trajectories` 和 Node `db.traceTrajectories()`，复用 `traceSearch` 过滤语义，返回逐 trace 的轻量路径摘要。
- P1 source retention：Golden Path metadata v3 保存 `source_trajectory_steps`；raw source trace 不可读时，`pathAdherence` / `goldenPathHealth` 仍可用 retained source trajectory 做确定性对比，并返回 `sourceRetained`。
- P1 evidence summary：Golden Path 保存 `evidenceSummary`，既可接收产品层传入的 `sampleCount` / `successRate` / `avgCostUsdNanos` / `p95DurationNs`，也会自动补 source span count、status、tokens/cost 和 trajectory signature。
- P1 export consumer：新增 Node 示例 `yitrace-node/examples/golden-path-export-consumer.mjs`，展示如何把 `yitrace.golden_path_export.v1` 转成 Agent Memory / regression dataset 的 domain object。

## 后续

- 产品层 BestPath election / adjudicator：在同一 scope 下维护推荐路径和 challengers。2026-07-06 口径确认：这不属于 yiTrace 底座待开发能力，yiTrace 只提供证据 API 和可持久化候选资产。
- Hit/reference count 与重复 trace 压缩策略。
- Retention policy：把 raw trace 保留策略和 Golden Path 引用/active 状态打通。
