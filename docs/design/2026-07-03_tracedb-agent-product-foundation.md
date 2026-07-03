# TraceDB 支撑 Agent 产品方向的底层改造设计

> 日期：2026-07-03
> 范围：只讨论 yiTrace 作为底层库 / TraceDB 需要补的能力，不写代码实现。
> 背景：基于 `docs/research/2026-07-03_agent-trace-data-applications.md` 和 `docs/plans/2026-07-03_loop-engineering-tracedb-plan.md`。

## 2026-07-03 实现状态

已先落地需求方 P0 的可用内核版本：

- 泛化 attrs 过滤：`search()` / `sessions()` 支持任意 `attrs` key，JSON 标量 exact，字符串数组 includes；常见 AgenticData 字段提供顶层别名。
- Trace list attrs 过滤与高频字段视图：`traces()` 支持同样的 attrs filter，响应保留原始 `attrs` 的同时提供稳定 `fields` 子集。
- P0.5 第一批高频字段入列：`project_id`、`skill`、`mode`、`call_site`、`task_fingerprint`、`loop_id`、`harness_version`、`validation_status`、`stop_reason`、`phase`、`validator` 已进入 `SpanFields` / WAL / segment / fold / HTTP / Node 类型；支持 attrs fallback、OTLP alias、builder alias、过滤和 group-by。
- 高频 attrs 过滤第一阶段加速：新增 segment-local attrs postings sidecar。持久 segment 写 `attr_postings/seg-*.attrs` 派生文件，内存只保留 term → segment ids 轻量目录和有预算的 LRU posting-list cache；MemTable/WAL tail 使用 live overlay。`One` + small sorted vec + HashSet 的分层 posting list 避免大量单元素/中小 HashSet；`traceSearch()`、`traces({ attrs })`、`sessions({ attrs })` 和 `/v1/search` 对已索引高频字段先取候选 span，再用 snapshot 折叠结果做 tenant/delete/attrs 校验；未索引或被预算降级的 key 走慢路径或只用其他完整索引缩小候选；`fields` 只为当前 trace list 可见 trace 做窄投影补全。
- 跨 session 结构化搜索：新增 `POST /v1/trace-search`，Node 包暴露 `db.traceSearch()`，支持 text contains、status/kind/tool/model/attrs、annotation、dataset association、分页和排序。
- Trace snapshot：新增 `GET /v1/traces/:id/snapshot`，Node 包暴露 `db.traceSnapshot()`，返回 `snapshotId` / `snapshotHash` / `createdAt` / 完整 trace 证据。
- Span/event 顺序：trace/detail/snapshot 返回 `spanOrdinal`、`siblingOrdinal`、`sortKey`；`logEvents` 返回 `eventOrdinal` 和事件 `sortKey`。
- 批量 span detail：新增 `GET /v1/traces/:id/spans` 和 `POST /v1/traces/:id/spans/batch`，Node 包暴露 `db.spans()` / `db.spansBatch()`。
- 大字段契约：新增 detail/snapshot 系列返回 `{ preview, full, contentHash, byteLength, truncated, blobRef }`，列表默认不带 full，snapshot 和 `includeFull` 带 full。
- 业务元数据基础层：新增 tenant-scoped durable annotation store 和 external dataset association store；HTTP 暴露 `POST/GET /v1/annotations`、`POST/GET /v1/dataset-associations`，Node 包暴露 `db.annotate()` / `db.annotations()` / `db.linkDatasetItem()` / `db.datasetAssociations()`；持久化到 `metadata.dat`，在线备份一起拷走；`traceSearch()`、`traces()`、`sessions()` 已支持按 annotation/dataset association 反查 source trace/span。
- 标准 usage/cost 契约：direct ingest、OTLP 和 Node builder 支持 `provider`、`cached_input_tokens`、`reasoning_tokens`、`total_tokens`、`cost_usd` / `cost_usd_nanos`、`cost_currency`；WAL/segment/fold 持久化这些字段；trace list、sessions、trace detail、traceSearch、span detail/snapshot 输出 `usage` 和 `costDetail`，旧 `cost` / `inTok` / `outTok` 保持兼容。
- Path mining group-by：新增 `POST /v1/trace-aggregate`，Node 包暴露 `db.traceAggregate()`；复用 `traceSearch()` 的 attrs/annotation/dataset/text/status/tool/model 过滤语义，按 `skill` / `mode` / `task_fingerprint` / `validation_status` / `toolName` / `model` / `provider` / `status` / 任意 attrs 分组，输出 spanCount、traceCount、errorRate、duration、usage、cost 和 examples。
- Trajectory group 候选路径：新增 `POST /v1/trajectory-groups`，Node 包暴露 `db.trajectoryGroups()`；先按 `traceSearch()` 语义筛候选 trace，再按完整 trajectory signature 分桶，输出 success rate、duration、usage/cost、eval/annotation/dataset score stats 和 examples，作为 golden path mining 的候选证据层。
- Agent loop/task 读模型：新增 `GET /v1/loops`、`GET /v1/loops/:loopId`、`GET /v1/tasks/:fingerprint/traces`，Node 包暴露 `db.loops()` / `db.loop()` / `db.taskTraces()`；基于 P0.5 字段输出 loop 摘要、loop 详情和同类 task trace 列表。
- Trace diff 基础版：新增 `POST /v1/traces/diff`（兼容 `POST /v1/trace-diff`），Node 包暴露 `db.traceDiff()`；返回两侧摘要、trajectory signature、route、逐 step 变化和 duration/token/cost/status/eval delta，作为 trajectory comparison / golden path mining 的确定性证据层。

尚未落地：retention、metadata 分页索引、golden path candidate store / path adherence，P2 SQL/table 专门 diff 与索引增强；group-by、trajectory group、loop/task 读模型和 trace diff 目前是 snapshot 扫描聚合，后续可加物化统计或列式聚合下推。第二批业务字段是否入列还需要基于真实查询频率和基数判断。

## 结论

如果 yiTrace 要支撑 Trace-to-Eval、Golden Path Mining、Trajectory Comparison、Memory Evidence Export、Training Data Export 这些上层产品方向，底层库不能只停留在“存 span、搜文本、看 waterfall”。

它需要升级为：

> 一个面向 agent trajectory 的证据数据库。

必要改造不是一口气做复杂 AI，而是补齐六层底座：

1. **语义字段层**：loop、task、validation、harness、trajectory、outcome 必须有稳定契约。
2. **查询索引层**：这些字段要能过滤、聚合、排序、分页，不能长期只靠任意 attrs。
3. **事件证据层**：logEvents、feedback、validator results、human annotation 要能作为一等证据回查。
4. **派生资产层**：eval item、trajectory、golden path、memory candidate 需要和 source trace 双向关联。
5. **导出治理层**：支持可脱敏、可过滤、可复现的 JSONL / ADP-like / eval export。
6. **嵌入式库层**：Node/Electron/后端服务能直接调用上述能力，而不是只能启动 server 看 UI。

短期仍不建议一口气做复杂 AI mining，但**高频 agent 字段要更早提升为一等列**。如果 Loop / Task / Validation 这些字段长期只存在 attrs JSON 里，后面的 Loop Health、Golden Path、Comparison、Export 都会被临时 sidecar 和 JSON 解析卡住。更合理的路线是：P0 先定义 contract，P0.5 就把第一批高频字段固定进 schema；attrs 继续承载长尾扩展字段。

## 当前能力对照

| 能力 | 当前状态 | 能支撑什么 | 缺口 |
|---|---|---|---|
| trace/span/event ingest | 已有，direct ingest 支持外部字符串 id | 基础执行记录 | 缺 loop/task/validation 约定 |
| attrs 持久化与过滤 | 已有，第一批 project/skill/mode/call_site/task/loop/validation 字段已入列并保留 attrs fallback | 初步产品维度过滤、loop health、task group、path mining 分组 | 第二批业务字段和 range/排序字段还需基于真实查询演进 |
| logEvents 返回 | 已有 trace/span detail `logEvents` | 证据回查、过程日志 | 缺独立 events query / 分页 / event type 扩展 |
| BM25/vector/hybrid search | 已有 | 相似失败、相似任务检索 | 缺 task-level search / trajectory-level search |
| sessions | 已有 | 对话维度聚合 | loop 与 session 关系未建模 |
| eval alpha | 已有规则 scorer + dataset association + trace/session/search 反查 | 初步 eval 闭环，外部 dataset item 可回查 source trace/span | 缺 eval 字段标准化 |
| Node embedded DB | 已有 MVP + annotation/dataset association API + loop/task 基础读模型 | 本地 app / Electron 接入 | 缺 golden path 高阶 API 和物化 loop/task 索引 |
| 导出 | 基本没有 | 无 | 训练数据、memory candidate、eval export 都未建 |
| 安全治理 | 基础 tenant | 本机开发 / PoC | 脱敏、字段白名单、审计导出不足 |

## AgenticData 需求方文档对齐

需求来源：

`/Users/Four/JobProjects/vexdb/AgenticData/docs/plans/2026-07-03_yitrace-loop-engineering-requirements.md`

该文档明确说明：AgenticData 负责产品 UI、review/eval/path memory 业务流程；yiTrace 只需要成为可靠、可查询、可追溯、可标注的 trace 数据层。因此本设计必须优先满足这些底层能力，而不是先做完整 Loop Intelligence 产品。

### 需求映射

| 需求方优先级 | 需求 | yiTrace 当前覆盖 | 设计处理 |
|---|---|---|---|
| P0 | 不可变可锁定 artifact | 已有 `pack:local` / `pack:verify`，文件名带 commit/dirty timestamp | 保持为发版/DX P0，后续正式 version bump 或内部 registry |
| P0 | 泛化 attrs 索引与过滤 | 仅承诺少数 attrs 精确过滤 | 升级为 P0：通用 attrs exact/includes 过滤 + 保留字段 registry |
| P0 | 跨 session trace/span 搜索 API | 有 search/traces/sessions，但不是 inbox 级搜索 | 升级为 P0：project/session/run/span search，支持分页、排序、projection |
| P0 | Trace snapshot/export | 基本没有 | 升级为 P0：trace snapshot JSON + hash + source evidence |
| P0 | Span sequence 标准化 | 有 ts/seq/event_id，控制台排序但缺统一 ordinal 契约 | 升级为 P0：span/event ordinal 与 sibling order |
| P0 | 批量 span detail / 完整 trace detail | 有单 span detail，trace detail 不拉大字段 | 升级为 P0：批量 span detail、cursor、projection |
| P0 | 大字段 / blob ref | 有 input/output 晚物化，但缺 preview/hash/blob 策略 | 升级为 P0：preview/full/hash/byte_len/blob_ref contract |
| P1 | Generic annotations / scores | annotation store + query + trace/session/search 反查、trace aggregate group-by 已落地 | 后续分页索引和更新/删除状态机 |
| P1 | Dataset/eval association | 外部 dataset/eval item source link + trace/session/search 反查已落地 | 后续 eval 字段标准化 |
| P1 | 标准 token/cost 字段 | 已落地 usage/cost 标准化、显式成本和默认估算、trace/session/span/traceAggregate 聚合输出 | 后续补模型价格表和成本归因策略 |
| P1 | Path mining group by | 已落地 `traceAggregate()` 基础版 | 后续补物化统计/列式聚合下推 |
| P1 | Retention/storage stats | metrics/backup 有，但无按项目统计/保留规则 | P1：storage stats + retention policy + protected trace |
| P2 | Optional text/vector index | 已有 BM25/vector，但主要 span 搜索，不是 inbox/task 专用 | P2：input/output/log 全文和 task-level vector |
| P2 | Trace diff API | 已落地基础版：`POST /v1/traces/diff` + `db.traceDiff()`，返回 trajectory signature、route/step/delta/eval 证据 | 后续补 SQL/table 字段级 diff |
| P2 | Trajectory group / candidate ranking | 已落地基础版：`POST /v1/trajectory-groups` + `db.trajectoryGroups()`，按完整 trajectory signature 分桶并输出 success/eval/annotation/dataset/cost/duration 证据 | 后续补 trajectory 物化索引、golden path candidate store 和 path adherence |

### 需求方 P0 对本设计的修正

此前本文把 Trace-to-Eval、Golden Path、Memory Export 作为产品机会展开，但需求方当前最急的是**数据底座**：

1. 能跨 session 查到候选 trace/span。
2. 能稳定保存和过滤业务 attrs。
3. 能导出不可变 trace snapshot 作为 eval draft 证据。
4. 能还原稳定 span 顺序用于 path mining。
5. 能完整/分页/投影读取 span detail。
6. 能处理大字段而不拖慢列表。

因此底层路线要调整为：

- **P0：查询、快照、顺序、大字段、批量 detail。**
- **P0.5：第一批高频字段入列已落地，后续评估第二批业务字段。**
- **P1：annotation/eval association/group by/retention。**
- **P2：自动 mining、diff、text/vector 增强。**

## 必要改造一：Trace Contract 从“span 观测”升级到“agent trajectory”

当前 wire event 已能记录 span，但上层产品需要更稳定的语义字段。

### 1.1 先定义保留 attrs key

短期仍走 `attrs`，但必须把这些 key 变成官方 contract：

| 类别 | 字段 | 说明 |
|---|---|---|
| task | `task_fingerprint` | 同类任务聚合 key，可由接入方传入，也可后续由系统生成 |
| task | `task_class` | 人工或系统聚类后的任务类型 |
| task | `goal_id` | 用户目标 / issue / job id |
| loop | `loop_id` | 一次 agent loop 运行 |
| loop | `iteration` | loop 第几轮 |
| loop | `phase` | `plan` / `act` / `observe` / `verify` / `repair` / `summarize` |
| loop | `stop_reason` | `goal_met` / `max_iterations` / `budget_exceeded` / `error` 等 |
| version | `harness_version` | workflow/prompt/harness 版本 |
| version | `agent_version` | agent 配置或模型版本 |
| validation | `validator` | `npm test`、`typecheck`、`human_review` 等 |
| validation | `validation_status` | `pass` / `fail` / `skipped` / `unknown` |
| validation | `validation_strength` | `none` / `weak` / `normal` / `strong` |
| trajectory | `trajectory_step` | 抽象后的步骤名 |
| golden path | `golden_path_id` | 命中的 golden path |
| golden path | `path_adherence` | `followed` / `partial` / `deviated` |

### 1.2 SpanEventBuilder 要支持 agent 语义 helper

底层库需要提供 helper，而不是让接入方手写 attrs。

建议新增 builder 层概念：

```ts
builder.startLoop({ loopId, taskFingerprint, harnessVersion })
builder.phase("verify", { validator: "npm test" })
builder.validation({ status: "fail", message, strength: "normal" })
builder.endLoop({ stopReason: "goal_met" })
```

注意：这只是 API helper，不等于引擎内必须新增 event_type。第一期可仍展开为普通 span/log/attrs。

### 1.3 OTLP/OpenInference 映射扩展

如果要兼容生态，OTLP 侧也要约定属性映射：

```text
yitrace.task_fingerprint
yitrace.loop_id
yitrace.loop.iteration
yitrace.loop.phase
yitrace.harness.version
yitrace.validation.status
yitrace.validation.validator
yitrace.stop_reason
```

HTTP server 和 Node embedded 都要吃同一套 contract。

## 必要改造二：高频 attrs 过滤不能只靠当前四个 key

此前只承诺 `project_id`、`skill`、`mode`、`call_site` 可过滤；现在已扩展为通用 attrs filter，并把 task/loop/validation 第一批字段提升为一等字段。需求方明确要求 `search()` / `sessions()` / trace 列表支持通用 attrs filter，且支持 string、number、bool、array string 的 exact / includes。

### 2.0 P0：需求方明确要求的 attrs key

第一批必须支持：

- `project_id`
- `session_id`
- `external_run_id`
- `skill`
- `mode`
- `call_site`
- `connection_ids`
- `data_source_ids`
- `schema_fingerprint`
- `intent_signature`
- `review_status`
- `eval_status`
- `path_memory_id`

过滤语义：

| 类型 | 语义 |
|---|---|
| string | exact |
| number | exact |
| bool | exact |
| string[] | includes / contains one |

这比我们最初的 Loop / Golden Path 字段更偏业务查询，但它是 AgenticData Trace Inbox 和 Path Mining 的硬前置。

Loop / Golden Path 还需要：

- `task_fingerprint`
- `task_class`
- `loop_id`
- `harness_version`
- `agent_version`
- `phase`
- `validation_status`
- `stop_reason`
- `validator`
- `golden_path_id`
- `path_adherence`

### 2.1 P0：扩展 attrs sidecar 过滤 key

短期实现思路：

- 保持 attrs JSON round-trip 不变。
- 扩展 `FilterAttrs` / sidecar，把需求方 key 和 loop key 纳入过滤。
- `search()`、`sessions()`、未来 `loops()` 都支持这些 key。
- Node `SearchFilter` / `SessionsOptions` 同步类型声明。
- trace list 也支持 attrs filter，避免 AgenticData 逐 session 扫描。

### 2.2 P1：做字段 registry

不能无限硬编码 key。建议引入 field registry：

```text
field_name
type: string | number | bool | enum
indexed: exact | range | text | none
source: attrs | first_class
```

第一版可以静态表，不需要动态 schema。

### 2.3 P0.5：第一批高频字段提升为一等列

这件事已前置落地。理由是：这些字段会成为后续 API、UI、导出、派生资产的 join/filter/group 基础；越晚提升，迁移成本越高。

第一批已直接进入 `SpanFields` / WAL / segment / HTTP / Node type：

- `task_fingerprint`
- `loop_id`
- `harness_version`
- `validation_status`
- `stop_reason`
- `phase`
- `validator`

原因：

- 列式段可以只读这些窄列做聚合。
- 支持 range / order / group by。
- 减少 attrs JSON 解析成本。
- 让 `LoopSummary`、`TaskGroup`、Trace-to-Eval、Trajectory Comparison 不依赖临时 attrs 解析。

第二批再进入一等列：

- `task_class`
- `agent_version`
- `validation_strength`
- `trajectory_step`
- `golden_path_id`
- `path_adherence`

这些字段更偏派生结果或后续优化，可以等 P1/P2 的 trajectory/golden path 模型稳定后再入列。

## 必要改造三：新增 Loop / Task / Trajectory 读模型

上层产品不能每次自己扫 trace 拼 loop。底层库要提供稳定读模型。

### 3.0 P0：跨 session trace/span 搜索 API

需求方当前最急的是 Trace Inbox 和 Path Mining 的项目级查询。底层库需要先提供通用搜索读模型，不等 Loop View 完整后再做。

建议对象：

```text
TraceSearchQuery {
  scope: tenant/project/session/run?
  time_range?
  status?
  span_kind?
  tool_name?
  model?
  attrs?
  text_contains?: { input, output, logs }
  sort?: created_at | updated_at | duration | cost | token_count | status
  order?: asc | desc
  projection?: summary | spans | fields[]
  cursor?
  limit?
}
```

建议返回：

```text
TraceSearchPage {
  items: TraceSearchHit[]
  next_cursor?
}
```

`TraceSearchHit` 至少包含：

- trace/run/session/project identity。
- external ids。
- status。
- span/tool/model summary。
- duration/token/cost。
- matched span ids。
- attrs projection。

这不是替代 BM25/vector search，而是补一个项目级 operational query API。

### 3.1 Task View

用途：找同类任务和历史尝试。

建议逻辑对象：

```text
TaskGroup {
  task_fingerprint
  task_class
  project_id
  skill
  trace_count
  success_count
  last_seen_at
  median_tokens
  median_duration
}
```

核心查询：

- list task groups
- get task group traces
- similar task search

### 3.2 Loop View

用途：Loop Health / Loop Explorer。

```text
LoopSummary {
  loop_id
  task_fingerprint
  harness_version
  iterations_total
  validation_fail_count
  final_status
  stop_reason
  tokens_total
  duration_total
  diagnostics[]
}
```

需要支持：

- `GET /v1/loops`
- `GET /v1/loops/:id`
- `GET /v1/loops/:id/diagnostics`

### 3.3 Trajectory View

用途：Golden Path Mining / Comparison / Export。

```text
Trajectory {
  trace_id
  task_fingerprint
  steps: [
    {
      step_index
      step_kind
      label
      source_span_ids
      tool_name
      validator
      status
      duration_ns
      tokens
    }
  ]
  outcome
  score
}
```

第一版 trajectory 可以按规则从 span/logEvents 派生，不必持久化。等 UI 和产品价值成立后，再缓存。

## 必要改造四：派生资产要有“证据链接”

Trace-to-Eval、Golden Path、Memory Candidate、Training Export 都是从 trace 派生出的资产。底层库必须保证双向可追溯。

### 4.0 P0：Trace Snapshot / Export

需求方明确要求 eval draft 不能只保存 trace id。因为后续 trace 可能被 retention 清理、截断，或者 schema 发生变化。yiTrace 需要提供不可变 snapshot/export API。

建议结构：

```text
TraceSnapshot {
  snapshot_id
  snapshot_hash
  created_at
  source_trace_id
  source_external_trace_id?
  tenant_id
  summary
  span_tree
  spans: SpanSnapshot[]
  usage
  attrs
  content_hashes
  blob_refs?
}
```

`SpanSnapshot` 包含：

- span identity / external ids。
- parent / children / ordinal。
- input/output，可按 profile 返回 full 或 blob ref。
- attrs。
- logEvents。
- usage/cost。
- content hash / byte length。

要求：

- snapshot hash 对 canonical JSON 计算。
- snapshot 可导出 JSON，供 AgenticData 存入 eval draft。
- snapshot 不依赖后续原始 trace 是否仍保留。
- snapshot 创建时记录 export profile，避免无法解释字段裁剪。

这和在线备份不同：备份是 DB 级一致性，snapshot 是 trace 级证据冻结。

### 4.1 SourceLink 统一结构

建议定义：

```text
SourceLink {
  trace_id
  span_id?
  event_id?
  external_trace_id?
  external_span_id?
  tenant_id
  role: positive | negative | neutral | counterexample
}
```

每个派生资产都必须带 source links。

### 4.2 EvalItem

```text
EvalItem {
  dataset_id
  item_id
  task_fingerprint
  input_fixture
  expected_behavior
  prohibited_behavior
  assertions[]
  judge_rubric?
  source_links[]
  created_at
}
```

必须支持：

- from trace/span 生成。
- 后续 eval run 结果写回。
- 按 source trace 反查 eval item。

需求方还要求支持外部 dataset/eval item association，而不是 yiTrace 必须管理完整 eval 系统。可先提供轻量关联表：

2026-07-03 已落地基础版：`DatasetAssociation` 支持 `dataset_id`、`item_id`、trace/span、snapshot id/hash、eval run、split、label、score 和 attrs，按 tenant 持久化并可查询。它是外部 dataset item 的 source link，不存 dataset item 本体。`traceSearch()` / `traces()` / `sessions()` 已能按 dataset/eval link 反向过滤。

```text
EvalAssociation {
  target_type: trace | span | session
  target_id
  dataset_id
  dataset_item_id
  eval_run_id
  eval_status
  assertion_type
  result_score
  source_links[]
}
```

查询必须支持：

- 从 eval draft 回到来源 trace。
- 从 eval result 回查新旧 trace 对比。
- 按 `dataset_id` / `eval_status` / `result_score` 过滤 traces。

### 4.3 GoldenPath

```text
GoldenPath {
  golden_path_id
  task_fingerprint
  scope
  steps[]
  score
  positive_source_links[]
  negative_source_links[]
  status: candidate | accepted | rejected | deprecated
}
```

### 4.4 MemoryCandidate

```text
MemoryCandidate {
  candidate_id
  scope
  lesson
  evidence_source_links[]
  positive_count
  negative_count
  confidence
  risk
  status
}
```

### 4.4.1 P1：Generic annotations / scores

需求方明确提到类似 Langfuse score 的通用 annotation/score。这个能力比 MemoryCandidate 更基础，应作为派生资产层 P1。

2026-07-03 已落地基础版：`Annotation` 按 tenant 持久化到 `metadata.dat`，支持 trace/span target、label、score、reason、source、attrs，HTTP/Node 都能创建和查询；`traceSearch()` / `traces()` / `sessions()` 已支持按 annotation label/source/score/attrs 反向过滤，`traceAggregate()` 也复用同一套 metadata 过滤做 group-by。当前边界是还没做分页索引和更新/删除状态机。

建议结构：

```text
Annotation {
  annotation_id
  target_type: trace | span | session
  target_id
  name
  label
  score?
  severity?
  comment?
  source: human | rule | eval | model
  metadata
  created_by
  created_at
}
```

要求：

- 支持 list/filter annotations。
- Trace search 可按 annotation label/score/severity/source 过滤。
- annotation 也必须带 tenant/project scope。
- review_status / eval_status 可作为 annotation 的索引化投影字段。

### 4.5 存储策略

短期不要把这些都塞进核心 span 表。

建议：

- 核心 trace 数据保持 append/fold。
- 派生资产走独立 dataset/asset store。
- 资产只保存小 JSON + source links。
- 大文本仍回 trace/span late materialization。

## 必要改造四点五：Span Sequence、批量 detail 和大字段策略

这三项是 Path Mining 和 Eval Draft 的硬前置，应该进入 P0。

### 4.5.1 Span / Event 顺序标准化

当前事件已有 `ts`、`seq`、`event_id`，但需求方要的是可直接消费的稳定路径顺序，不希望 App 再根据聊天消息倒推工具顺序。

建议输出契约：

| 字段 | 说明 |
|---|---|
| `eventOrdinal` | trace 内事件稳定顺序 |
| `spanOrdinal` | trace 内 span 首次出现顺序 |
| `siblingOrdinal` | 同一 parent 下的稳定顺序 |
| `eventSeq` | span 内 event sequence |
| `sortKey` | 可比较排序键，便于客户端稳定排序 |

排序建议：

```text
event: (ts, seq, event_id)
span: first_event_ordinal
sibling: parent_id + first_event_ordinal
```

验收目标：

- trace detail 天然返回可还原 path 的顺序。
- start/end/log event 都可比较。
- Path Mining 可以直接从 trace 得到 route -> tool -> inner agent -> LLM -> validation -> answer。

### 4.5.2 批量 Span Detail / 完整 Trace Detail

当前单 span detail 不够。AgenticData 不能为了 eval draft 只拉前 40 个 span。

建议 API：

```text
GET /v1/traces/:id/spans?cursor=&limit=&fields=input,output,logs,attrs,usage
POST /v1/traces/:id/spans/batch { span_ids, fields }
GET /v1/traces/:id?includeSpanDetails=true&detailLimit=...&cursor=...
```

要求：

- 支持 cursor 分页。
- 支持按 span ids 批量读取。
- 支持 projection：`input`、`output`、`logs`、`attrs`、`usage`、`logEvents`。
- 大 trace 不一次性撑爆内存。

### 4.5.3 大字段 / Blob Ref 策略

AgenticData 需要完整证据，但 UI 列表只需要 preview。yiTrace 需要标准化大字段返回：

```text
LargeTextField {
  preview
  full?
  content_hash
  byte_length
  blob_ref?
  truncated: bool
}
```

策略：

- 列表默认只返回 preview、hash、length。
- detail 可请求 full。
- snapshot 可选择 full 或 blob_ref。
- 大 SQL、大表格、大 prompt 不进入列表热路径。
- hash 用于 eval draft 和 snapshot 校验证据是否一致。

## 必要改造五：相似任务和路径比较需要更强检索

Golden Path Mining 的关键是“找相似任务”和“比较路径”。

### 5.1 Task-level searchable text

需要为每条 trace/loop 生成 task text：

```text
goal + first user request + error summary + key logs + touched files/tools
```

这和 span 文本不同。span 文本用于搜细节，task text 用于搜“同类问题”。

建议：

- P0：用 root span input_text + logs 拼一个 task document。
- P1：显式 `task_summary` 字段。
- P2：embedding / vector for task-level similarity。

### 5.2 Tool sequence / trajectory signature

结构相似不能只靠文本。

建议生成：

```text
tool_sequence = ["read_file", "run_test", "inspect_arch", "pack", "verify_consumer"]
trajectory_signature = hash(normalized_step_sequence)
```

用途：

- 找“同样路径”的历史 trace。
- 找“同类任务但不同路径”的 trace。
- 计算 path adherence。

### 5.3 Comparison API

底层库提供规则版 diff：

- step sequence diff
- validator diff
- token / duration diff
- error/log similarity
- missing verification step

LLM 总结可以是上层调用，不进底层第一版。

### 5.4 P1：Path Mining 基础聚合查询

需求方不要求 yiTrace 判断“最优路径”，但要求 yiTrace 提供聚合数据，减少 AgenticData 全量扫描。

建议 API 能按这些字段 group by：

- `project_id`
- `schema_fingerprint`
- `intent_signature`
- `skill`
- `mode`
- `toolName`

聚合指标：

- count。
- success/error count。
- avg/p50/p95 duration。
- avg token/cost。
- latest trace ids。

这个能力应该建立在 P0.5 入列字段和 attrs registry 之上。它是 Path Mining 页面的候选排序基础。

### 5.5 P2：Trace Diff API

需求方 P2 明确需要比较两个 traces：

- route / skill。
- tool sequence。
- selected tables/fields attrs。
- SQL text。
- duration/token/cost。
- output summary。

这可以复用 trajectory comparison，但第一版应该先提供确定性 diff，不依赖 LLM。

建议：

```text
POST /v1/traces/diff { left_trace_id, right_trace_id, fields }
```

返回：

- common steps。
- diverged steps。
- missing validators。
- tool sequence diff。
- cost/duration/token delta。

2026-07-03 已落地基础版：`POST /v1/traces/diff`（兼容 `POST /v1/trace-diff`）接受 `leftTraceId` / `rightTraceId` 或别名，支持外部字符串 id；响应包含 `left` / `right` 摘要、整体 `delta`、两侧 `trajectory`、两侧 `routes` 和逐项 `steps`。`trajectory.left/right` 输出规范化 steps、稳定 FNV-1a signature 和 `same`；`steps.left/right` 会带 `evalScore` / `evalLabel`，并把 eval 变化纳入 `changes`。它只做确定性结构 diff，不负责判定哪条 trace 更优；判优由 eval、annotation 或业务规则完成。
- matched attrs diff。
- source span links。

## 必要改造六：导出层必须先做治理

Training Data Export 和 Memory Evidence Export 都涉及敏感数据。底层库必须有导出策略。

### 6.1 Export Profile

```text
ExportProfile {
  include_input_text: bool
  include_output_text: bool
  include_logs: bool
  include_attrs: allowlist[]
  redact_patterns[]
  max_text_bytes
  tenant_scope
}
```

### 6.2 支持格式

优先级：

1. `jsonl`：最通用。
2. `eval-jsonl`：source trace + input fixture + expected behavior。
3. `trajectory-jsonl`：action / observation / validation。
4. `memory-candidate-jsonl`：lesson + evidence links。
5. ADP-like：等生态稳定后再兼容。

### 6.3 可复现导出

导出结果要记录：

- query/filter
- snapshot version
- generated_at
- profile hash
- row count

否则训练数据很难审计和复现。

## 必要改造七：隐私、安全和租户隔离从“读写过滤”扩展到“资产隔离”

当前 tenant 已贯穿读写路径，但派生资产会引入新风险：

- eval item 可能引用另一个租户 trace。
- memory candidate 可能跨项目泄露经验。
- export 可能包含敏感 prompt/output/log。
- golden path 可能把内部流程泄露给不该看的项目。

底层必须保证：

- source link tenant_id 必填。
- 派生资产有 tenant/project scope。
- export 必须带 tenant context。
- 跨租户 source links 禁止或需要显式 admin。
- attrs allowlist 默认严格。
- 脱敏策略先于 export。

### 7.1 P1：Retention / Storage Stats

需求方需要 AgenticData 设置页展示本地 Trace 占用，并支持清理策略。

底层统计：

- total traces / spans。
- total bytes。
- bytes by project / session / time。
- large payload count。
- snapshot count / annotation count / eval-linked trace count。

Retention policy：

- 按时间清理。
- 按项目清理。
- 保留已 snapshot traces。
- 保留已 annotated traces。
- 保留已 eval-linked traces。
- 保留 active golden path / path memory 关联 traces。

注意：retention 不能只删原始 trace。必须检查 source links 和 derived assets，否则会破坏 eval draft/path memory 的证据链。

## 必要改造七点五：Usage / Cost 标准化

需求方已经在 attrs 中冗余 token 字段，但这不应长期存在。Path scoring 和 Trace Inbox 排序都需要标准 usage/cost。

第一批标准字段：

```text
input_tokens
output_tokens
total_tokens
cached_tokens
cost_usd
model
provider
```

要求：

- span start/end 都可写。
- 折叠后 last-non-null 或 additive 语义要明确。
- trace/session aggregate 自动聚合 token/cost。
- search / trace list / sessions 可按 token/cost 排序。
- Node/HTTP 输出字段统一，不再要求 AgenticData 从 attrs 多处兜底。

建议折叠语义：

- `input_tokens` / `output_tokens` / `cached_tokens`：同一 span 内取最终值，不跨事件累加，避免 start/end 重复。
- trace/session aggregate：按 folded span 汇总。
- `cost_usd`：同一 span 内取最终值；trace/session aggregate 求和。
- `model` / `provider`：last-non-null。

## 必要改造八：嵌入式 DB API 不只是 search/trace

如果 yiTrace 要作为底层库被 Node/Electron/AgenticData 使用，`@yitrace/db` 需要逐步暴露新能力。

P0：

```ts
db.search(...)
db.trace(...)
db.span(...)
db.sessions(...)
```

已基本具备。

P1：

```ts
db.loops(...)
db.loop(loopId)
db.taskTraces(taskFingerprint)
db.compareTraces(traceA, traceB)
db.createEvalItemFromTrace(...)
```

P2：

```ts
db.goldenPaths(...)
db.acceptGoldenPath(...)
db.memoryCandidates(...)
db.exportTrajectories(...)
```

原则：

- Node API 仍走 engine typed/json API，不直接读文件。
- ESM/CJS 类型必须同步。
- Electron renderer 不直接持有 DB，main process 暴露 IPC。

## 必要改造九：性能与存储影响

### 9.1 为什么不能只存在 attrs JSON

初期可以，但长期会有问题：

- 每次 Loop Health 都解析 JSON。
- 无法高效 group by。
- 无法做稳定 cursor。
- 无法对 task/loop/golden_path 做段级剪枝。
- 无法在导出时快速筛高质量 trajectory。

### 9.2 建议索引演进

阶段 1：定义 contract + 扩展 attrs sidecar，用于快速兼容和迁移。

阶段 2：第一批高频字段入列：

```text
task_fingerprint
loop_id
harness_version
validation_status
stop_reason
phase
validator
```

阶段 3：建立 loop/task/golden_path 的轻量二级索引：

```text
tenant_id + task_fingerprint -> trace_ids
tenant_id + loop_id -> trace_ids/span_ids
tenant_id + harness_version -> trace_ids
tenant_id + golden_path_id -> trace_ids
```

阶段 4：第二批派生字段入列，段级 zone-map / dictionary / postings。

阶段 5：task-level vector index，独立于 span-level vector。

### 9.4 P2：Optional text/vector index 对齐

需求方 P2 要求：

- 对 input/output/log text 建全文索引。
- 可选 embedding/vector index。
- 向量 index 必须按 tenant/project/schema_fingerprint 过滤。

当前 yiTrace 有 BM25/vector 基础，但要支持 Trace Inbox 和 Path Mining，需要区分三类索引：

| 索引 | 用途 |
|---|---|
| span text index | 搜 input/output/log 里的表名、字段名、SQL 片段、错误文本 |
| task text index | 搜相似用户问题 / intent / goal |
| trajectory vector index | 搜相似路径和相似失败模式 |

第一版优先 span text index + task text index；trajectory vector 等路径模型稳定后再做。

### 9.3 派生缓存

Trajectory extraction 可以先动态计算，但要设计缓存边界：

- key: trace_id + source_version + extractor_version
- invalidation: 新 event / upgrade / delete
- storage: derived cache，不影响原始 trace

## 分期路线

### P0：AgenticData 第一批必须交付

目标：让 AgenticData 能做 Trace Inbox、Eval Draft 和 Path Mining 的数据读取，不需要逐 session 扫描或自己补顺序/证据。

必须做：

- 不可变可锁定 artifact：root + platform optional tarball 文件名不可变，`pack:verify` 覆盖 clean consumer、ESM/CJS/native、builder ingest、search、sessions。
- 通用 attrs filter：支持需求方 13 个 attrs key，支持 string/number/bool exact 和 string[] includes。
- 跨 session trace/span search：时间、status、span kind、toolName、model、attrs、input/output/log contains、分页、排序、projection。
- trace snapshot/export：summary、full span tree、input/output、attrs、logEvents、usage/cost、external ids、content hashes/blob refs、snapshot hash。
- span/event sequence 标准化：eventOrdinal、spanOrdinal、siblingOrdinal、sortKey。
- 批量 span detail / 完整 trace detail：cursor、span ids batch、projection。
- 大字段/blob ref：preview/full/hash/byte_len/blob_ref，列表默认 preview。

不做：

- 不做自动 mining。
- 不做复杂 LLM 总结。
- 不做 AgenticData 的 UI / review/eval/path memory 业务流程。

### P0.5：第一批高频字段入列（已落地基础版）

目标：避免后续查询、排序、聚合、snapshot 和 path mining 建立在临时 attrs JSON 上。

已完成：

- `SpanFields` 增加第一批字段：`task_fingerprint`、`loop_id`、`harness_version`、`validation_status`、`stop_reason`、`phase`、`validator`。
- wire / OTLP / builder 把同名 attrs 映射到一等字段，同时保留 attrs round-trip。
- WAL / segment 共享编码已升级到 v5，旧 v2-v4 数据仍兼容读取。
- HTTP / Node 输出和过滤支持这些字段。
- `traceAggregate()` 支持这些字段精确 group-by，不先做复杂全文/向量。

继续评估：

- 需求方业务高频字段是否也入列：`schema_fingerprint`、`intent_signature`、`review_status`、`eval_status`、`path_memory_id`。

不做：

- 不把所有 attrs 都入列。
- 不急着把 `trajectory_step` / `golden_path_id` 入列，等派生模型稳定。

### P1：AgenticData 第二批交付

目标：让 AgenticData 可以把 review/eval/path mining 结果回写并查询，同时控制本地存储。

必须做：

- Generic annotations / scores：trace/span/session annotation，支持 label/score/severity/comment/source/metadata/filter。
- Dataset/eval association：外部 dataset_id、dataset_item_id、eval_run_id、eval_status、assertion_type、result_score。
- 标准 token/cost 字段：total/cached tokens、cost_usd、model、provider，trace/session aggregate。
- Path mining group by：按 project/schema/intent/skill/mode/toolName 聚合 count、成功失败、p50/p95、token/cost、latest trace ids。
- Retention/storage stats：按项目/session/time 的 bytes，大 payload count，保留 snapshot/annotation/eval-linked traces。

### P2：AgenticData 第三批交付

目标：增强 Trace Inbox 和 Path Mining 的智能检索与解释能力。

必须做：

- input/output/log 全文索引。
- 可选 task/span vector index，并按 tenant/project/schema_fingerprint 过滤。
- Trace diff API：route/skill、tool sequence、selected tables/fields、SQL、duration/token/cost、output summary。

### P3：yiTrace 自身产品增强

目标：在满足需求方数据底座后，再做 yiTrace 自己的 Loop Intelligence / Golden Path 产品化。

必须做：

- `/v1/loops`
- `/v1/tasks/:fingerprint/traces`
- trajectory extraction 规则版。
- comparison set。
- Loop Health diagnostics。
- GoldenPath candidate store。（已落地基础版：source trace/snapshot 引用、status、评审信息；reference count / last-seen trace 后续单独设计）
- path adherence。
- MemoryCandidate export。
- trajectory JSONL export。
- export profile / redaction / reproducible export。

## 最小可验证 Demo

要证明底层改造有价值，可以做一个小 demo，不必先做完整 UI：

1. 同一个 `task_fingerprint=npm-native-packaging` 灌入三条 trace：
   - A：失败，没检查 node arch。
   - B：成功但绕远，跑了很多无关命令。
   - C：成功且验证充分，运行 `npm test` + clean consumer pack verify。
2. 查询 task group，列出三次尝试。
3. compare A/B/C，显示 C 的验证强度最高、成本合理、重试少。
4. 生成 golden path candidate。
5. 新 trace 进入后检查 path adherence。

这个 demo 能直接说明：

> yiTrace 不是只看 trace，而是能从 trace 中发现可复用路径。

## 不建议底层库承担的事情

- 不直接 orchestrate agent。
- 不自动修改 workflow。
- 不把 LLM judge 变成必需依赖。
- 不直接做长期 memory recall。
- 不把导出默认打开给所有字段。
- 不把所有 attrs 都承诺可过滤。

## 对外 API/库定位变化

当前：

> `YiTraceDB.open("./data"); db.search(); db.trace();`

目标：

> `YiTraceDB.open("./data"); db.compareTraces(); db.createEvalItemFromTrace(); db.goldenPaths(); db.exportTrajectories();`

但这个变化必须建立在底层 contract 和 source links 之上。否则上层功能会变成没有证据的总结器。

## 最重要的三条改造

如果只能先做三件事：

1. **跨 session 查询 + 通用 attrs 过滤**
   这是 Trace Inbox 和 Path Mining 的入口。没有它，AgenticData 只能逐 session 扫描，产品做不起来。

2. **Trace Snapshot + 大字段/批量 detail**
   Eval Draft 需要完整可追溯证据，而且不能被 retention 或 UI 前 40 个 span 限制。

3. **Span/Event Sequence 标准化**
   Path Mining 的路径顺序是核心数据，不能让 App 再根据聊天消息倒推工具顺序。
