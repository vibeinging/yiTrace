# yiTrace 剩余需求清单

> 日期：2026-07-03
> 口径：基于当前 `CURRENT_STATE`、TraceDB Agent 产品底层改造设计、Node 集成文档和已落地代码。

## 当前结论

P0 接入底座已基本完成：嵌入式 Node DB、外部字符串 id、attrs round-trip/filter、traceSearch、traceAggregate、trajectoryGroups、snapshot、span detail、logEvents、annotation、dataset association、usage/cost 都已有基础版。

后续重点不再是“能不能接入”，而是：

- 第一批关键字段已从 attrs 提升为稳定 schema，下一步是第二批业务字段和更强聚合索引。
- 大规模查询/聚合从扫描走向列式下推或物化统计。
- review/eval/path mining 的派生资产具备生命周期治理。
- Agent loop、trace diff、trajectory group 已有基础读模型；真正的 golden path store、path adherence 和高性能物化聚合仍待补。

## P0.5：第一批高频字段入列（基础版已落地）

已完成第一批。目标是避免 loop health、task group、path mining、diff 都依赖临时 attrs JSON。

第一批已入列：

- `task_fingerprint`
- `loop_id`
- `harness_version`
- `validation_status`
- `stop_reason`
- `phase`
- `validator`

已覆盖 wire / OTLP / builder / WAL / segment / fold / HTTP / Node type / tests。字段可用于 search、traces、sessions、traceSearch、traceAggregate 的过滤或分组，并在响应 `fields` 中稳定返回。

下一批继续评估 AgenticData 高频字段：

- `schema_fingerprint`
- `intent_signature`
- `review_status`
- `eval_status`
- `path_memory_id`

下一批不急于全部入列，先看真实查询频率、基数和是否需要 group-by/排序。

## P1：存储治理和派生资产治理（基础版部分已落地）

- Retention / storage stats：基础版已落地。`POST /v1/storage-stats` / `db.storageStats()` 复用 `traceSearch` filter，按 project/session/time 等 group-by 统计 trace/span/event 数量、payload/attrs/external id 估算字节和 annotation/dataset association/Golden Path 引用。
- Retention policy：基础版已落地。`POST /v1/retention-plan` / `db.retentionPlan()` 默认保护 annotation、dataset association、active Golden Path（candidate/confirmed）、snapshot、eval link、path memory 引用的 trace；`POST /v1/retention/apply` / `db.applyRetention()` 只软删除已经 flush 的 segment rows，跳过 MemTable/WAL tail 热 trace；`compact: true` 会把 deletion vector 物化进新段，并走已有 GC log 安全 reclaim；`GET/POST /v1/retention-audits` / `db.retentionAudits()` 可查真实 apply 的策略、计数和 trace id 样本审计；`POST/GET /v1/retention-policies` / `db.createRetentionPolicy()` / `db.retentionPolicies()` / `db.runRetentionPolicies()` 提供持久化 TTL 策略和显式 run-due 调度底座。后续仍需后台守护或外部自动化示例。
- Annotation/dataset association 分页索引：当前是基础查询，后续要支持 cursor/limit 和更高效过滤。
- Annotation 更新/删除状态机：从 append-only 进入 review workflow 需要的状态变更模型。

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
