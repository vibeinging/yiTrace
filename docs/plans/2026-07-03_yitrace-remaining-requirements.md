# yiTrace 剩余需求清单

> 日期：2026-07-03
> 口径：基于当前 `CURRENT_STATE`、TraceDB Agent 产品底层改造设计、Node 集成文档和已落地代码。

## 当前结论

P0 接入底座已基本完成：嵌入式 Node DB、外部字符串 id、attrs round-trip/filter、traceSearch、traceAggregate、trajectoryGroups、snapshot、span detail、logEvents、annotation、dataset association、usage/cost 都已有基础版。

后续重点不再是“能不能接入”，而是：

- 第一批和第二批关键字段已从 attrs 提升为稳定 schema，下一步是更强聚合索引和 rollup。
- 大规模查询/聚合从扫描走向列式下推或物化统计。
- review/eval/path mining 的派生资产具备生命周期治理。
- Agent loop、trace diff、trajectory group、Golden Path store、path adherence、evidence/export/health 都已有基础读写模型；下一步重点是高性能物化、Best/Challenger 治理、重复 trace 压缩和发版/DX。

## 2026-07-04 复核后的开发优先级

### P0：发版和真实消费验证

- 正式 npm version bump，明确 public npm / internal registry / tarball 三种分发路径。
- 多平台 optional native package CI matrix：darwin-arm64、darwin-x64、linux-x64-gnu、linux-arm64-gnu、win32-x64-msvc。
- `pack:verify` 扩展 clean consumer 场景：ESM/CJS/native load、builder ingest、annotation update/delete、golden path export、retention policy。
- Electron packaging smoke test：asar unpack、optional native packages 保留、`NAPI_RS_NATIVE_LIBRARY_PATH` fallback。

### P1：高频查询高性能化（部分落地）

- `traceAggregate()` 从 folded snapshot 扫描升级为列式聚合下推、物化 rollup 或 group-by 索引。
- `loops()` / `taskTraces()` 补物化 loop/task index，避免高频页面反复扫 folded spans。
- `trajectoryGroups()` / `goldenPathHealth()` 补 trajectory-level 物化索引，支撑 Golden Path mining 高频使用。
- metadata index：annotation/dataset/golden path/retention policy 按 tenant + status + project/task + createdAt 建索引，替代全量 metadata scan。
- 2026-07-04 已补查询可观测契约：`traceSearch` / `traceAggregate` / `trajectoryGroups` 顶层返回 `index` / `aggregationIndex` / `trajectoryIndex`，明确 attrs postings、metadata filter、folded scan 和 trajectory materialized cache 的使用情况。真正的列式聚合下推/rollup 仍是后续性能专项。

### P1：Golden Path 治理增强（基础版已落地）

- Best/Challenger 底座已落地：Golden Path metadata 记录 `challengerOf`、`evalProfile`、`minSampleCount`、`marginScore`、`comparisonWindowNs`、`promotedFrom`、`deprecationReason`、`staleReasons`；DB 不自动判优，但给产品层稳定证据。
- Golden Path stale/deprecation signals 基础版已落地：`goldenPathHealth()` 输出 `governance.stale` / `staleReasons`，覆盖样本不足、health 低于阈值、source signature 变化、deprecated 等信号。
- 重复 trace / canonical source 压缩方案单独设计：先做 `storageStats()` 证据和 retention policy，再决定是否做引用计数。

### P1：成本能力补齐（基础版已落地）

- 模型价格表基础版已落地：显式 `cost_usd` / `cost_usd_nanos` 优先；缺失时按少量 provider/model 内置价格表估算，再回退默认 token 单价。
- 成本来源标记已落地：span/detail 返回 `explicit` / `estimated_model_price` / `estimated_default`，聚合返回 `mixed`。
- cost/token 范围过滤已落地：`traceSearch` 支持 `minCostUsdNanos` / `maxCostUsdNanos` / `minCostUsd` / `maxCostUsd` / `minTotalTokens` / `maxTotalTokens`，`traceAggregate` / `trajectoryGroups` / `goldenPathHealth` 复用同一过滤语义。

### P2：检索增强

- input/output/log 全文索引，服务 Trace Inbox 搜错误、SQL、表名、字段名。
- task-level text index，服务相似任务召回。
- 可选 task/span/trajectory vector index，按 tenant/project/schema_fingerprint 过滤。
- trajectory derived cache，key 包含 trace id、source version、extractor version。

### P2：安全和生产开关

- 生产模式强制 tenant：没有 `X-Tenant-Id` 或 tenant context 的写入可配置拒绝。
- RBAC / token scope：至少区分 ingest、read、metadata-write、retention-apply。
- redaction/export profile：Golden Path export、trace snapshot、Memory handoff 支持字段脱敏。
- `OpenOptions.readOnly`：engine 真只读打开路径完成后再暴露 Node 类型。

## P0.5：高频字段入列（第一批、第二批均已落地）

目标是避免 loop health、task group、path mining、diff 都依赖临时 attrs JSON。

第一批已入列：

- `task_fingerprint`
- `loop_id`
- `harness_version`
- `validation_status`
- `stop_reason`
- `phase`
- `validator`

已覆盖 wire / OTLP / builder / WAL / segment / fold / HTTP / Node type / tests。字段可用于 search、traces、sessions、traceSearch、traceAggregate 的过滤或分组，并在响应 `fields` 中稳定返回。

第二批已入列：

- `schema_fingerprint`
- `intent_signature`
- `review_status`
- `eval_status`
- `path_memory_id`

第二批已覆盖 wire / OTLP / builder / WAL / segment / fold / HTTP / Node type / tests。`schema_fingerprint`、`intent_signature`、`review_status`、`eval_status` 继续进入 attrs postings sidecar；`path_memory_id` 作为一等字段保留精确过滤和输出，但默认不进 postings，避免高基数字段撑大索引。

## P1：存储治理和派生资产治理（基础版部分已落地）

- Retention / storage stats：基础版已落地。`POST /v1/storage-stats` / `db.storageStats()` 复用 `traceSearch` filter，按 project/session/time 等 group-by 统计 trace/span/event 数量、payload/attrs/external id 估算字节和 annotation/dataset association/Golden Path 引用。
- Retention policy：基础版已落地。`POST /v1/retention-plan` / `db.retentionPlan()` 默认保护 annotation、dataset association、active Golden Path（candidate/confirmed）、snapshot、eval link、path memory 引用的 trace；`POST /v1/retention/apply` / `db.applyRetention()` 只软删除已经 flush 的 segment rows，跳过 MemTable/WAL tail 热 trace；`compact: true` 会把 deletion vector 物化进新段，并走已有 GC log 安全 reclaim；`GET/POST /v1/retention-audits` / `db.retentionAudits()` 可查真实 apply 的策略、计数和 trace id 样本审计；`POST/GET /v1/retention-policies` / `db.createRetentionPolicy()` / `db.retentionPolicies()` / `db.runRetentionPolicies()` 提供持久化 TTL 策略和显式 run-due 调度底座。后续仍需后台守护或外部自动化示例。
- Annotation/dataset association 分页索引：`cursor`/`limit` 基础分页已落地，按 `createdAtNs`/id 倒序稳定返回；后续量大时再补 metadata index 和更高效过滤。
- Annotation 更新/删除状态机：基础版已落地。annotation 支持 `active/resolved/rejected/deleted`、PATCH 更新、DELETE 软删除和 tenant-scoped 持久化；默认查询和 annotation 反向过滤忽略 deleted。后续如进入复杂 review workflow，再补审批历史、批量操作和 metadata index。

## P1：成本和聚合增强

- 模型价格表：按 provider/model/version 估算 cost，兼容 explicit cost。
- 成本归因策略：按 span/agent/tool/session/task 聚合，明确 explicit/estimated/mixed source。
- `traceAggregate()` 高性能版：当前是 snapshot 扫描聚合，后续补列式聚合下推、物化 daily/hourly rollup 或 group-by 索引。

## P1/P2：Agent Loop 读模型（基础版已落地）

为 AgenticData 和未来控制台提供稳定 API：

- `db.loops(...)` / `GET /v1/loops`
- `db.loop(loopId)` / `GET /v1/loops/:loopId`
- `db.taskTraces(taskFingerprint)` / `GET /v1/tasks/:fingerprint/traces`

已基于 P0.5 字段完成基础版：支持 attrs 精确过滤、metadata 反向过滤、分页、usage/cost/error/duration 聚合和 trace/span 明细。当前仍是 folded snapshot 扫描聚合，后续如果 loop 页变成高频入口，再补物化 loop/task 索引。

## P2：Trace Diff / Trajectory Comparison

用于比较多次相似任务中哪条路径更好：

- route / skill 序列差异。
- tool sequence 差异。
- selected tables / fields / SQL 差异。
- duration / token / cost 差异。
- output summary / validation result 差异。

这是 golden path mining 的关键支撑。

2026-07-03 已落地基础版：新增 `POST /v1/traces/diff`（兼容 `POST /v1/trace-diff`）和 Node `db.traceDiff()`，支持数字 trace id 和外部字符串 trace id，返回两侧摘要、整体 span/error/duration/token/cost delta、两侧 route、逐 step `same/changed/left_only/right_only` 和字段变化列表。step detail 已带 `evalScore` / `evalLabel`，eval 变化会进入 `changes`，并补了 eval harness 用例；`trajectory.left/right` 已输出 normalized steps、稳定 FNV-1a signature 和 `same`。当前只做确定性结构 diff，不自动判优；后续更高阶的 selected tables/fields/SQL 专门字段 diff 仍待补。

2026-07-03 已落地 trajectory 聚合基础版：新增 `POST /v1/trajectory-groups`（兼容 `POST /v1/trajectory-aggregate` / `POST /v1/best-paths`）和 Node `db.trajectoryGroups()`。它复用 `traceSearch` 过滤语义，先筛候选 trace，再按完整 trajectory signature 分桶，输出 success rate、duration、usage/cost、eval/annotation/dataset score stats 和 examples。当前是 golden path mining 的候选证据层；可治理的 Golden Path Candidate Store 也已落地（`POST/GET /v1/golden-paths`、`db.createGoldenPath()` / `db.goldenPaths()`），只保存 source trace/snapshot 引用、状态和评审信息，不复制 trace payload。重复命中、引用计数和压缩策略后续单独设计；如果成为高频入口，需要 trajectory-level 物化索引和 path adherence。

## P2：检索增强

- input/output/log 全文索引，服务 Trace Inbox 搜错误、SQL、表名、字段名。
- task-level text index，服务相似任务召回。
- 可选 task/span/trajectory vector index，按 tenant/project/schema_fingerprint 过滤。
- trajectory derived cache，key 包含 trace id、source version、extractor version。

## P3：yiTrace 自身产品化

在底层能力稳定后再做：

- Loop Explorer。
- Loop Health diagnostics。
- GoldenPath candidate store。（2026-07-03 已落地基础版：candidate/status/source trace 引用；hit/reference count 后续单独设计）
- path adherence 检查。
- MemoryCandidate export。
- trajectory JSONL export。
- export profile / redaction / reproducible export。

## 发版 / DX 仍需收尾

- 正式 npm version bump 和 public/internal registry 发布策略。
- 多平台 optional native package CI matrix。
- `pack:verify` 继续覆盖 clean consumer、ESM/CJS/native、builder ingest、search、sessions、traceAggregate。
- `OpenOptions.readOnly` 等 engine 侧只读打开路径后再重新暴露。
