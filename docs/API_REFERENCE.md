# yiTrace HTTP API Reference

> yiTrace 的所有功能都通过 HTTP JSON API 暴露。**自带控制台前端没有特权**——它和任何第三方前端调的是同一套 `/v1/*` 端点。想写自己的前端 / Dashboard / 接入 Grafana，照着本文即可。
>
> 字段契约直接取自 `yitrace-engine/crates/yt-engine/src/http.rs` 的实现，不是从前端反推。

---

## 起服务

```bash
cd yitrace-engine && cargo run -p yt-engine --example server
# → http://127.0.0.1:7878  （自带 eval 种子数据，开箱可调）
```

控制台前端已内嵌进单二进制，`GET /`（非 `/v1/*`）返回前端页面。你要用自己的前端，**忽略 `GET /`，直接调下面的 `/v1/*`**。

---

## 通用约定

### 请求
- 所有端点都在 `/v1/` 下。
- 请求/响应都是 JSON（`Content-Type: application/json`）。
- 路径参数 `:id` / `:spanId` 可传内部数字 ID，也可传 direct ingest 时的外部字符串 ID（例如 UUID）。

### 鉴权
- **不配 token（默认）**：所有请求放行。仅限本机开发。
- **配了 token**（`YT_TOKEN=secret cargo run ... --example server`）：`/v1/*` API 请求须带 `Authorization: Bearer <token>` 头，精确匹配，否则 401。控制台静态页面 `GET /` 仍可匿名加载，页面里的 API 请求再带 token。

控制台前端的 HTTP 客户端支持：

| 配置 | 用途 |
|---|---|
| `VITE_API_TOKEN` | 构建时注入 `Authorization: Bearer <token>` |
| `VITE_TENANT_ID` | 构建时注入 `X-Tenant-Id` |
| `localStorage["yitrace.tenantId"]` | 浏览器运行时设置租户（未配置 `VITE_TENANT_ID` 时生效） |

### 多租户隔离
- 租户从 **`X-Tenant-Id` 请求头**取（数字），**不信任请求体**——客户端不能越权选别人的租户。
- 影响的端点：`GET /v1/traces`、`POST /v1/search`、`GET /v1/sessions`（及 turns / trace / span 详情）都按 `X-Tenant-Id` 过滤，只返回该租户的数据。
- 摄入时（`POST /v1/ingest` / `POST /v1/traces`）：服务端会用 `X-Tenant-Id` 覆盖 body / OTLP attributes 里的租户字段。未带租户头时数据按 `tenant_id=null` 写入，仅适合本机开发或单租户调试。

### 状态码
| 码 | 含义 |
|---|---|
| 200 | 成功 |
| 400 | 请求体非法（JSON 解析失败 / 缺字段 / 非法数值字段） |
| 401 | 鉴权失败（配了 token 但没带 / 不匹配） |
| 404 | trace / span 不存在 |

---

## ⚠️ 两套字段风格（写前端前必读）

yiTrace 有**两类端点**，JSON 字段命名风格不同，别混用：

| 类别 | 端点 | 字段风格 | 用途 |
|---|---|---|---|
| **原始 API** | `GET /v1/traces`、`POST /v1/search` | **snake_case**，引擎原始命名（`trace_id`、`duration_ns`） | 程序化对接、检索 |
| **控制台 API** | `/v1/trace-search`、`/v1/trace-aggregate`、`/v1/trajectory-groups`、`/v1/trace-trajectories`、`/v1/storage-stats`、`/v1/retention-plan`、`/v1/retention/apply`、`/v1/retention-audits`、`/v1/retention-policies`、`/v1/retention-policies/run-due`、`/v1/traces/diff`、`/v1/golden-paths`、`/v1/path-adherence`、`/v1/golden-path-evidence`、`/v1/golden-path-export`、`/v1/golden-path-health`、`/v1/loops`、`/v1/loops/:id`、`/v1/tasks/:fingerprint/traces`、`/v1/sessions`、`/v1/sessions/:id/turns`、`/v1/traces/:id`、`/v1/traces/:id/steps`、`/v1/traces/:id/spans/:sid` | **camelCase**，面向 UI（`traceId`、`durMs`） | 写 Trace 浏览器 / 瀑布 / 时间线 / loop/path mining / storage governance |

下面每节会标注属于哪一类。

---

## 摄入

### POST /v1/ingest  —— 灌入 SDK 线格式 JSON 批（原始 API）

高效的自定义批量摄入格式（Python/TS SDK 默认产出此格式）。

**请求体**：JSON 数组，每个元素是一个事件：

```json
[
  {
    "trace_id": "run-uuid",
    "span_id": "span-uuid",
    "ts": 1,
    "seq": 1,
    "event_type": 1,
    "ext_span_id": "span-uuid",
    "status": 0,
    "duration_ns": null,
    "input_tokens": 900,
    "output_tokens": null,
    "session_id": "session-uuid",
    "tenant_id": null,
    "agent_name": "风控",
    "tool_name": null,
    "model": null,
    "input_text": null,
    "output_text": null,
    "logs": ["开始"],
    "attrs": {
      "external_run_id": "run-uuid",
      "project_id": "agentic-data",
      "skill": "review",
      "mode": "auto",
      "call_site": "worker.ts:10"
    }
  }
]
```

| 字段 | 类型 | 说明 |
|---|---|---|
| `trace_id` | u64 或 string | 数字会作为内部 id；UUID/字符串会稳定 hash 成内部 `u64`，原文保存在 `external_trace_id` |
| `span_id` | u64 或 string | 数字会作为内部 id；UUID/字符串会稳定 hash 成内部 `u64`，原文保存在 `external_span_id` |
| `ts` | i64 | 纳秒时间戳 |
| `seq` | u32 | 同一 span 内的事件序号（去重键的一部分） |
| `event_type` | u8 | **1=SpanStart，2=SpanEnd**，3+=属性补写/日志 |
| `ext_span_id` | string | span 外部身份（去重键的一部分） |
| `parent_span_id` | u64 或 string? | 父 span（建树）；字符串原文保存在 `external_parent_span_id` |
| `status` | u8? | 0=ok，非 0=error（SpanEnd 时给） |
| `duration_ns` | u64? | 耗时纳秒（SpanEnd 时给） |
| `input_tokens`/`output_tokens` | u64? | token 计数 |
| `cached_input_tokens` / `reasoning_tokens` / `total_tokens` | u64? | cache 命中输入、推理 token、上游总 token；缺 `total_tokens` 时响应按各 token 字段派生 |
| `cost_usd` / `cost_usd_nanos` / `cost_currency` | number/string? | 显式成本。落盘使用 `cost_usd_nanos` 整数；传 `cost_usd` 时会转换成 nano USD；未显式传成本时，查询层会优先按 provider/model 内置价格表估算，再回退默认单价 |
| `session_id` | u64 或 string? | 会话归属；字符串原文保存在 `external_session_id` |
| `tenant_id` | u64? | 租户归属；HTTP 服务会被 `X-Tenant-Id` 覆盖 |
| `agent_name`/`tool_name`/`model`/`provider` | string? | 标注 |
| `input_text`/`output_text` | string? | 大文本（晚物化） |
| `logs` | string[] | 日志行 |
| `attrs` | object? | 原始/扩展属性；贯穿折叠、WAL/segment/manifest 和查询输出，同 key 后到覆盖 |

`attrs.project_id` / `attrs.skill` / `attrs.mode` / `attrs.call_site` / `attrs.task_fingerprint` / `attrs.loop_id` / `attrs.harness_version` / `attrs.schema_fingerprint` / `attrs.intent_signature` / `attrs.validation_status` / `attrs.review_status` / `attrs.eval_status` / `attrs.path_memory_id` / `attrs.stop_reason` / `attrs.phase` / `attrs.validator` 会在摄入时 schema-on-write 提升为折叠后的一等字段，并继续保留在 `attrs` 中用于 round-trip。旧数据或手写引擎记录即使只带 `attrs`，查询也会回退读取 attrs；新数据即使只写一等字段、不镜像 attrs，也能被过滤和返回。

**响应**：`200 {"ingested":N}`（N=实际灌入条数）。

> **去重**：`event_id = hash(ext_span_id, seq, event_type)`，内容决定身份——重传/崩溃重放天然幂等，token/成本不重复计数。

**attrs round-trip 契约**：

- `attrs` 必须是 JSON object。
- value 可为 string / number / bool / null / array / object。
- yiTrace 会校验并保存 value 的 JSON 字面量；search、trace、span detail 返回时恢复成相同 JSON 形态。
- 同一 span 多个事件写同一个 attr key 时，后到事件覆盖先到事件。
- `project_id`、`skill`、`mode`、`call_site`、`task_fingerprint`、`loop_id`、`harness_version`、`schema_fingerprint`、`intent_signature`、`validation_status`、`review_status`、`eval_status`、`path_memory_id`、`stop_reason`、`phase`、`validator` 是一等字段；`connection_ids`、`data_source_ids`、`schema_fingerprint`、`intent_signature`、`review_status`、`eval_status` 默认进入过滤 sidecar。`path_memory_id` 虽然是一等字段，但默认不进 postings，以免高基数字段撑大索引；它仍可通过折叠校验精确过滤。其他 attrs 会持久化并返回，但可能退回折叠校验慢路径。

### POST /v1/traces  —— OTLP/HTTP 标准端点（生态入口 / 原始 API）

**已埋点 OTLP/OpenInference 的应用不改一行即可灌入**（OTel GenAI `gen_ai.*`、Arize `llm.*`）。请求体是标准 OTLP/HTTP JSON（`{"resourceSpans":[...]}`）。非法/缺字段返回 400。

常用属性映射：

| OTLP / OpenInference 属性 | yiTrace 字段 |
|---|---|
| `gen_ai.request.model` / `gen_ai.response.model` / `llm.model_name` | `model` |
| `gen_ai.system` / `llm.provider` / `llm.system` / `yitrace.provider` | `provider` |
| `gen_ai.usage.input_tokens` / `gen_ai.usage.prompt_tokens` / `llm.token_count.prompt` | `input_tokens` |
| `gen_ai.usage.output_tokens` / `gen_ai.usage.completion_tokens` / `llm.token_count.completion` | `output_tokens` |
| `gen_ai.usage.cached_input_tokens` / `llm.token_count.prompt_details.cached` | `cached_input_tokens` |
| `gen_ai.usage.reasoning_tokens` / `llm.token_count.completion_details.reasoning` | `reasoning_tokens` |
| `gen_ai.usage.total_tokens` / `llm.token_count.total` | `total_tokens` |
| `yitrace.cost_usd` / `gen_ai.usage.cost_usd` / `llm.cost.usd` | `cost_usd` |
| `yitrace.cost_currency` / `gen_ai.usage.cost_currency` / `llm.cost.currency` | `cost_currency` |
| `gen_ai.agent.name` / `agent.name` | `agent_name` |
| `gen_ai.tool.name` / `tool.name` | `tool_name` |
| `input.value` / `gen_ai.prompt` | `input_text` |
| `output.value` / `gen_ai.completion` | `output_text` |
| `yitrace.session_id` / `session.id` / `gen_ai.conversation.id` / `session_id` | `session_id` |
| `yitrace.tenant_id` / `tenant.id` / `tenant_id` | direct ingest 的 `tenant_id`；HTTP 摄入仍由 `X-Tenant-Id` 覆盖 |
| `yitrace.project_id` / `project.id` / `project_id` | `attrs.project_id`，摄入后提升为一等字段 |
| `yitrace.skill` / `agent.skill` / `skill` | `attrs.skill`，摄入后提升为一等字段 |
| `yitrace.mode` / `agent.mode` / `mode` | `attrs.mode`，摄入后提升为一等字段 |
| `yitrace.call_site` / `code.call_site` / `call_site` | `attrs.call_site`，摄入后提升为一等字段 |
| `yitrace.task_fingerprint` / `agent.task_fingerprint` / `task.fingerprint` / `task_fingerprint` | `attrs.task_fingerprint`，摄入后提升为一等字段 |
| `yitrace.loop_id` / `agent.loop_id` / `loop.id` / `loop_id` | `attrs.loop_id`，摄入后提升为一等字段 |
| `yitrace.harness_version` / `agent.harness_version` / `harness.version` / `harness_version` | `attrs.harness_version`，摄入后提升为一等字段 |
| `yitrace.schema_fingerprint` / `schema.fingerprint` / `schema_fingerprint` | `attrs.schema_fingerprint`，摄入后提升为一等字段 |
| `yitrace.intent_signature` / `task.intent_signature` / `intent.signature` / `intent_signature` | `attrs.intent_signature`，摄入后提升为一等字段 |
| `yitrace.validation_status` / `validation.status` / `validation_status` | `attrs.validation_status`，摄入后提升为一等字段 |
| `yitrace.review_status` / `review.status` / `review_status` | `attrs.review_status`，摄入后提升为一等字段 |
| `yitrace.eval_status` / `eval.status` / `eval_status` | `attrs.eval_status`，摄入后提升为一等字段 |
| `yitrace.path_memory_id` / `agent.path_memory_id` / `path_memory.id` / `path_memory_id` | `attrs.path_memory_id`，摄入后提升为一等字段；默认不建 postings |
| `yitrace.stop_reason` / `agent.stop_reason` / `stop.reason` / `stop_reason` | `attrs.stop_reason`，摄入后提升为一等字段 |
| `yitrace.phase` / `agent.phase` / `loop.phase` / `phase` | `attrs.phase`，摄入后提升为一等字段 |
| `yitrace.validator` / `validation.validator` / `validator` | `attrs.validator`，摄入后提升为一等字段 |

> `user.id` 只应作为业务属性处理，不会被当作 tenant。HTTP 多租户边界只认 `X-Tenant-Id`。

---

## 查询

### GET /v1/traces  —— trace 列表（原始 API，snake_case）

**查询参数**：

| 参数 | 说明 |
|---|---|
| `attrs` | URL 编码 JSON object，如 `{"project_id":"agentic-data"}` |
| `projectId` / `project_id` / `skill` / `mode` / `callSite` / `taskFingerprint` / `task_fingerprint` / `loopId` / `loop_id` / `harnessVersion` / `harness_version` / `schemaFingerprint` / `schema_fingerprint` / `intentSignature` / `intent_signature` / `validationStatus` / `validation_status` / `reviewStatus` / `review_status` / `evalStatus` / `eval_status` / `pathMemoryId` / `path_memory_id` / `stopReason` / `stop_reason` / `phase` / `validator` | attrs 字符串精确过滤便捷参数 |
| `annotationLabel` / `annotationSource` / `annotationTarget` / `annotationScoreMin` / `annotationScoreMax` | 按 annotation 反向过滤 trace |
| `datasetId` / `itemId` / `evalRunId` / `datasetLabel` / `datasetScoreMin` / `datasetScoreMax` | 按 dataset association 反向过滤 trace |

metadata 过滤语义：trace 级 annotation/link 命中整条 trace；span 级 annotation/link 只要命中该 trace 中任一 span，就返回该 trace。租户仍从 `X-Tenant-Id` 头取。

**响应**：JSON 数组，每条：

```json
{
  "trace_id": 7,
  "span_count": 3,
  "total_duration_ns": 4200000,
  "max_duration_ns": 3000000,
  "error_count": 0,
  "total_input_tokens": 900,
  "total_output_tokens": 120,
  "total_cached_input_tokens": 0,
  "total_reasoning_tokens": 0,
  "total_tokens": 1020,
  "total_cost_usd": 0.0012,
  "total_cost_usd_nanos": 1200000,
  "usage": {
    "inputTokens": 900,
    "outputTokens": 120,
    "cachedInputTokens": 0,
    "reasoningTokens": 0,
    "totalTokens": 1020
  },
  "costDetail": {
    "costUsd": 0.0012,
    "costUsdNanos": 1200000,
    "currency": "USD",
    "source": "mixed"
  }
}
```

### GET /v1/metrics  —— Prometheus 指标

返回 Prometheus 文本格式（`# HELP` / `# TYPE` / 值），可直接被 Prometheus 抓、Grafana 出看板。指标：`yt_manifest_version`、`yt_segments_live`、`yt_memtable_rows`、`yt_segments_dead`、`yt_readers_active`、`yt_wal_committed_tail`、`yt_flush_threshold`、`yt_filter_attrs`、`yt_fold_cache_entries`、`yt_seg_bloom_count`、`yt_datasets`、`yt_annotations`、`yt_dataset_associations`。

### GET /v1/healthz / GET /v1/readyz  —— 进程探针

返回 `{"ok":true}`。用于 Docker Compose / Kubernetes / 反向代理健康检查，只表示 HTTP 服务可路由，不做深度数据一致性扫描。

---

## 检索

### POST /v1/search  —— 中文检索 / 向量召回 / 混合（原始 API，snake_case）

**按给了什么自动选检索路**：

| 给了 | 走哪路 |
|---|---|
| 只 `text` | 中文 BM25 检索 |
| `text` + `textDomains` | 分域 BM25，只在 input/output/log/tool/model/agent 等指定域内检索 |
| 只 `vector` | 向量找相似（带过滤进图） |
| 两个都给 | 混合（RRF 融合） |

**请求体**：

```json
{
  "text": "盗刷",
  "textDomains": ["input_text", "logs"],
  "vector": [0.1, 0.2, 0.3],
  "k": 10,
  "includeFanout": false,
  "filter": {
    "trace_id": 7,
    "agent_name": "风控",
    "status": 1,
    "time_from": 1000,
    "time_to": 5000,
    "attrs": {
      "project_id": "agentic-data",
      "skill": "review",
      "mode": "auto",
      "call_site": "worker.ts:10"
    }
  }
}
```

| 字段 | 类型 | 必需 | 说明 |
|---|---|---|---|
| `text` | string | 二选一 | 中文检索词（CJK 分词） |
| `textDomains` / `text_domains` / `domains` / `fields` | string[] | 否 | 分域全文检索，可选 `input_text`、`output_text`、`logs`、`tool_name`、`model`、`agent_name` |
| `inputTextContains` / `outputContains` / `logContains` / `toolNameContains` / `modelContains` / `agentNameContains` | string | 否 | 便捷分域查询别名；等价于设置对应 domain 并使用该文本 |
| `vector` | f32[] | 二选一 | 查询向量（维度需与索引一致） |
| `k` | usize | 否 | 返回数，默认 10 |
| `includeFanout` / `include_fanout` | bool | 否 | 仅 cluster mode 有意义；默认 `false` 保持旧数组响应，传 `true` 时返回 envelope 并带 fanout 诊断 |
| `filter.trace_id` | u64? | 否 | 限定 trace |
| `filter.agent_name` | string? | 否 | 限定 agent |
| `filter.status` | u8? | 否 | 限定状态（0=ok，非 0=error） |
| `filter.time_from`/`time_to` | i64? | 否 | 时间窗（纳秒） |
| `filter.project_id` / `filter.projectId` | JSON value? | 否 | 精确匹配一等字段 `project_id`，兼容 attrs.project_id |
| `filter.skill` | JSON value? | 否 | 精确匹配一等字段 `skill`，兼容 attrs.skill |
| `filter.mode` | JSON value? | 否 | 精确匹配一等字段 `mode`，兼容 attrs.mode |
| `filter.call_site` / `filter.callSite` | JSON value? | 否 | 精确匹配一等字段 `call_site`，兼容 attrs.call_site |
| `filter.task_fingerprint` / `filter.taskFingerprint` | JSON value? | 否 | 精确匹配一等字段 `task_fingerprint`，兼容 attrs.task_fingerprint |
| `filter.loop_id` / `filter.loopId` | JSON value? | 否 | 精确匹配一等字段 `loop_id`，兼容 attrs.loop_id |
| `filter.harness_version` / `filter.harnessVersion` | JSON value? | 否 | 精确匹配一等字段 `harness_version`，兼容 attrs.harness_version |
| `filter.schema_fingerprint` / `filter.schemaFingerprint` | JSON value? | 否 | 精确匹配一等字段 `schema_fingerprint`，兼容 attrs.schema_fingerprint |
| `filter.intent_signature` / `filter.intentSignature` | JSON value? | 否 | 精确匹配一等字段 `intent_signature`，兼容 attrs.intent_signature |
| `filter.validation_status` / `filter.validationStatus` | JSON value? | 否 | 精确匹配一等字段 `validation_status`，兼容 attrs.validation_status |
| `filter.review_status` / `filter.reviewStatus` | JSON value? | 否 | 精确匹配一等字段 `review_status`，兼容 attrs.review_status |
| `filter.eval_status` / `filter.evalStatus` | JSON value? | 否 | 精确匹配一等字段 `eval_status`，兼容 attrs.eval_status |
| `filter.path_memory_id` / `filter.pathMemoryId` | JSON value? | 否 | 精确匹配一等字段 `path_memory_id`，兼容 attrs.path_memory_id；默认不走 postings |
| `filter.stop_reason` / `filter.stopReason` | JSON value? | 否 | 精确匹配一等字段 `stop_reason`，兼容 attrs.stop_reason |
| `filter.phase` / `filter.validator` | JSON value? | 否 | 精确匹配一等字段 `phase` / `validator`，兼容 attrs fallback |
| `filter.attrs.<key>` | JSON value? | 否 | 与上面字段等价，也可传任意扩展 attrs filter |

> `filter.tenant_id` **不能在请求体里指定**——强制取 `X-Tenant-Id` 头。
> attrs 过滤是精确匹配，比较的是规范化 JSON 字面量：字符串匹配字符串，数字匹配数字，布尔匹配布尔，`null` 匹配 null。

**响应**：JSON 数组（按 score 降序），每条命中：

```json
{
  "trace_id": 7,
  "external_trace_id": "run-uuid",
  "span_id": 1,
  "external_span_id": "span-uuid",
  "score": 3.2720,
  "searchIndex": "text_domain_bm25",
  "textDomains": ["input_text", "logs"],
  "status": 0,
  "duration_ns": 4200000,
  "agent_name": "风控研判",
  "logs": ["研判结论 ..."],
  "attrs": {
    "project_id": "agentic-data",
    "skill": "review",
    "mode": "auto",
    "call_site": "worker.ts:10"
  }
}
```

`searchIndex` 解释本次命中的索引路径：`bm25_all_text` 是原始 all-text BM25，`text_domain_bm25` 是分域全文索引，`vector_graph` 是 span 向量图，`hybrid_bm25_vector` / `hybrid_text_domain_vector` 是 RRF 融合。分域全文索引会在摄入时分别索引 `input_text`、`output_text`、`logs`、`tool_name`、`model`、`agent_name`，并继续复用租户和 attrs filter；没有指定 domain 时保持旧的 all-text 行为。

cluster mode 下默认仍返回同样的数组，保证旧客户端兼容。若请求传 `includeFanout:true`，响应会改为 envelope：

```json
{
  "items": [
    {
      "trace_id": 7,
      "span_id": 1,
      "score": 3.272
    }
  ],
  "total": 1,
  "queryMode": "fanout_merge",
  "shardCount": 3,
  "okShards": 3,
  "degraded": false,
  "failedShards": [],
  "consistencyUsed": "partial",
  "partial": true,
  "readTargets": [
    {
      "shard": 0,
      "shardId": "logical-a",
      "replicaId": "a-follower",
      "addr": "http://127.0.0.1:7902",
      "role": "follower",
      "health": "healthy",
      "replicationLagLsn": 0,
      "reason": "bounded_stale_follower"
    }
  ]
}
```

`degraded:true` 表示本次结果只来自成功 shard，`total` 也只统计成功 shard；业务侧不能把它当作全量结果。写入类 fanout 仍应 fail-fast，避免部分写成功被误判为完整成功。

remote/process gateway 读 fanout 第一版支持两种查询策略：

- 默认 `partial`：部分 shard 失败时仍返回成功 shard 的结果，响应带 `degraded:true`、`failedShards`、`consistencyUsed:"partial"`、`partial:true`。
- 显式 `strict`：请求体传 `{"consistency":"strict"}` / `{"consistency":"strong"}`，或传 `{"partial":false}`；GET list 类接口也可用 `?consistency=strict` / `?partial=false`。任一 shard 失败时整体返回 `502`（部分成功）或 `503`（全失败），响应带 `consistencyUsed:"strict"`、`partial:false`。

第一版已接入 `/v1/search`、`/v1/trace-search`、`/v1/trace-aggregate`、`/v1/trajectory-groups`、`/v1/trace-trajectories`、`/v1/storage-stats`、metadata list 和 `golden-path-export` 等读 fanout。默认 `partial` 模式会使用最近一次 health refresh 结果做 bounded-stale read target 选择：同一 logical shard 内优先读取 `healthy` 且 `replicationLagLsn <= maxLagLsn` 的 readable follower；没有合格 follower 时回 leader。显式 `strict` / `strong` 会强制读 leader。响应的 `readTargets` 解释每个 logical shard 实际读到哪个 replica，以及原因。remote gateway snapshot 会记录 `replicaId`，后续分页会回到同一个 replica，避免跨 replica replay 破坏一致性。

### Snapshot lease  —— 显式快照租约（分布式/分页 API）

显式 snapshot lease 用于长分页、跨多次请求的一致读。单机 / in-process cluster 会 pin 当前 manifest snapshot；remote gateway 会向每个 shard 建 shard-local lease，并在 gateway 内保存 composite lease。release 后同一个 lease 再 renew 或 replay 会返回 `409` + `code:"snapshot_expired"`。

| 方法 | 端点 | 用途 |
|---|---|---|
| POST | `/v1/snapshots/lease` | 创建显式 snapshot lease |
| POST | `/v1/snapshots/renew` | 续租并刷新最近使用时间 |
| DELETE | `/v1/snapshots/:leaseId` | 释放 lease |

创建：

```json
{
  "consistency": "bounded_stale"
}
```

响应：

```json
{
  "snapshot": {
    "mode": "remote_gateway",
    "leaseId": "remote-lease-1",
    "routeTableVersion": 80,
    "shards": [
      {
        "shard": 0,
        "shardId": "logical-a",
        "replicaId": "a-follower",
        "snapshot": {
          "mode": "single_node",
          "leaseId": "lease-1",
          "shards": [
            { "shardId": "local", "manifestVersion": 3 }
          ]
        }
      }
    ]
  },
  "leaseId": "remote-lease-1",
  "leaseState": "active"
}
```

续租：

```json
{ "leaseId": "remote-lease-1" }
```

带 lease 查询时可以把完整 `snapshot` 原样放回请求体；remote gateway 也接受只带 lease id 的 compact token：

```json
{
  "snapshot": {
    "mode": "remote_gateway",
    "leaseId": "remote-lease-1",
    "shards": []
  },
  "limit": 50
}
```

route table reload 会清空 gateway 内的 remote composite lease；旧 token 会返回 `route_table_expired` 或 `snapshot_expired`，调用方应重新创建 lease。

### POST /v1/vector-index / POST /v1/vector-search  —— 命名空间向量索引（原始 API）

这组 API 给 Agent Memory / 相似任务 / 相似路径召回提供底座。yiTrace 不负责生成 embedding，只接收上层写入的向量。当前第一片实现是 append-only `named_vectors.dat` + 内存 flat index，保证 namespace、tenant、attrs filter 和重启恢复语义正确；后续再把 task/trajectory namespace 接到 HNSW/GraphIndex 性能路径。

写入向量：

```json
{
  "namespace": "task",
  "key": "npm-native-packaging",
  "vector": [0.12, 0.34, 0.56],
  "traceId": "run-uuid",
  "spanId": "span-uuid",
  "attrs": {
    "project_id": "agentic-data",
    "schema_fingerprint": "schema-v1",
    "embedding_model": "text-embedding-3-large"
  }
}
```

查询相似向量：

```json
{
  "namespace": "task",
  "vector": [0.10, 0.30, 0.58],
  "k": 10,
  "filter": {
    "attrs": {
      "project_id": "agentic-data",
      "schema_fingerprint": "schema-v1"
    }
  }
}
```

响应：

```json
{
  "items": [
    {
      "namespace": "task",
      "key": "npm-native-packaging",
      "score": 0.997,
      "distance": 0.003,
      "traceId": "run-uuid",
      "spanId": "span-uuid",
      "attrs": {
        "project_id": "agentic-data",
        "schema_fingerprint": "schema-v1"
      }
    }
  ],
  "total": 1,
  "vectorIndex": "vector_namespace_flat"
}
```

`namespace` 支持 `span`、`task`、`trajectory`。`key` 是命名空间内的稳定业务键；重复写同一个 `(tenant, namespace, key)` 会更新内存索引，并追加一条可恢复记录。remote gateway 下 `POST /v1/vector-index` 按 key hash 路由到一个 writable shard，`POST /v1/vector-search` fanout 到 readable shards 后按 score 合并 top-k，响应 `vectorIndex:"fanout_vector_namespace_flat"`。

### POST /v1/cluster/route-table/reload  —— Remote gateway route table reload（分布式运维 API）

仅 process gateway / remote gateway 模式使用。请求体是 route table JSON，包含 `version` / `routeTableVersion` 和 `shards`。reload 会在同一 gateway 进程内替换 writer 视图、清空 tenant/session/trace 路由缓存，并拒绝低于当前版本的 route table。兼容别名：`POST /v1/route-table/reload`。

v1 扁平格式：每个 `shards[]` item 就是一条 route，至少需要 `id`/`shardId` 和 `addr`，可选 `role`、`readable`、`writable`、`weight`。

```json
{
  "version": 11,
  "shards": [
    { "id": "shard-a", "addr": "127.0.0.1:7901", "role": "leader", "readable": true, "writable": true, "weight": 1 },
    { "id": "shard-b", "addr": "127.0.0.1:7902", "role": "leader", "readable": true, "writable": true, "weight": 1 }
  ]
}
```

v2 logical shard + replicas 格式：`shards[].shardId` 是稳定分片身份，`replicas[]` 表达同一分片下的 leader/follower/candidate。当前 gateway 写路径要求每个 logical shard **恰好一个** `writable:true` replica，手动 promote 时通过 reload 切换 writable replica；旧 leader fencing 仍由部署/控制面保证。

```json
{
  "routeTableVersion": 51,
  "shards": [
    {
      "shardId": "logical-a",
      "replicas": [
        { "replicaId": "a-primary", "addr": "127.0.0.1:7901", "role": "follower", "readable": true, "writable": false },
        { "replicaId": "a-follower", "addr": "127.0.0.1:7902", "role": "leader", "readable": true, "writable": true, "priority": 20, "maxLagLsn": 0 }
      ]
    }
  ]
}
```

成功响应：

```json
{ "ok": true, "routeTableVersion": 11 }
```

`GET /v1/cluster/shards` 会返回当前 writable replica，并在 route table v2 下附带 `replicas` 诊断列表。这个接口是显式 reload 接缝，不等同于完整控制面：周期 watcher、心跳驱动 failover、old leader fencing 仍在分布式路线后续阶段；读 follower 已支持显式 health refresh 驱动的 bounded-stale 选择。

### Cluster health / heartbeat  —— 显式副本健康采样

route table v2 下 gateway 可以显式刷新每个 replica 的健康状态。yiTrace 第一版不在 embedded 进程里自动启动后台 heartbeat；运维进程、控制面或测试可以主动调用刷新接口，然后用 `GET /v1/cluster/health` 或 `GET /v1/cluster/shards` 查看最近一次采样结果。

| 方法 | 端点 | 用途 |
|---|---|---|
| GET | `/v1/cluster/health` | 返回最近一次 replica health snapshot |
| POST | `/v1/cluster/health/refresh` | 立即探测 route table 中的所有 replica |
| GET/POST | `/v1/cluster/heartbeat` | health snapshot / refresh 的兼容别名 |

刷新会优先请求每个 replica 的 `GET /v1/replication/status`，老节点返回 404 时回退到 `GET /v1/cluster/shards`。gateway 会按同一 logical shard 的 writable leader tail 计算 follower `replicationLagLsn`；如果 follower lag 超过 route table 中的 `maxLagLsn`，健康状态会从 `healthy` 降为 `stale`，并标记 `readable:false`。连接失败会标记为 `unreachable`。这只是诊断和后续读 follower/failover 的底座，不会自动切 leader。

响应示例：

```json
{
  "routeTableVersion": 60,
  "replicaCount": 3,
  "replicas": [
    {
      "shardId": "logical-a",
      "replicaId": "a-leader",
      "addr": "http://127.0.0.1:7901",
      "health": "healthy",
      "httpStatus": 200,
      "latencyMs": 2,
      "checkedAtNs": "1783350000000000000",
      "committedTail": 5,
      "leaderTail": 5,
      "replicationLagLsn": 0,
      "readable": true,
      "reason": "ok"
    },
    {
      "shardId": "logical-a",
      "replicaId": "a-follower",
      "addr": "http://127.0.0.1:7902",
      "health": "stale",
      "httpStatus": 200,
      "latencyMs": 3,
      "checkedAtNs": "1783350000000000000",
      "committedTail": 3,
      "leaderTail": 5,
      "replicationLagLsn": 2,
      "readable": false,
      "reason": "lag_exceeds_budget"
    }
  ]
}
```

### Shard replication  —— leader/follower WAL 复制底座（分布式运维 API）

这些端点给同一 shard 内 leader/follower 复制使用，不是普通业务查询 API。第一版是显式拉取/应用：外部复制 worker 或测试进程从 leader 拉 WAL batch，再 POST 到 follower；yiTrace 目前不会在 embedded 进程里自动启动后台复制线程。

| 方法 | 端点 | 用途 |
|---|---|---|
| GET | `/v1/replication/status` | 返回本实例复制水位 |
| GET | `/v1/replication/wal?afterLsn=...` | 从 leader 导出 `afterLsn` 之后的已提交 WAL records |
| POST | `/v1/replication/wal` | follower 幂等应用一批 WAL records |

`after_lsn` / `afterLsn` / `from_lsn` / `fromLsn` 都可用。WAL batch 中的 `records` 使用 `/v1/ingest` 同款 wire event JSON，因此会保留原始 `tenant_id`、external ids、usage/cost、logs 和 attrs round-trip。

状态响应：

```json
{
  "committedTail": 2,
  "manifestVersion": 1,
  "memtableWatermark": 0,
  "memtableRows": 2,
  "segmentCount": 0
}
```

拉取响应：

```json
{
  "fromLsn": 0,
  "toLsn": 1,
  "recordCount": 1,
  "records": [
    {
      "trace_id": 77001,
      "span_id": 1,
      "ts": 77001,
      "seq": 1,
      "event_type": 2,
      "ext_span_id": "77001-1",
      "tenant_id": 770,
      "attrs": { "project_id": "process-distributed-eval" }
    }
  ],
  "status": {
    "committedTail": 1,
    "manifestVersion": 0,
    "memtableWatermark": 0,
    "memtableRows": 1,
    "segmentCount": 0
  }
}
```

应用响应：

```json
{
  "applied": true,
  "fromLsn": 0,
  "toLsn": 1,
  "recordCount": 1,
  "status": {
    "committedTail": 1,
    "manifestVersion": 0,
    "memtableWatermark": 0,
    "memtableRows": 1,
    "segmentCount": 0
  }
}
```

语义：

- 完整重复 batch 是幂等 no-op。
- 部分重叠 batch 会跳过已应用前缀，只追加缺失后缀。
- follower tail 小于 `fromLsn` 时返回 `409 {"code":"replication_apply_failed"}`，说明缺了一段 WAL，需要 snapshot/bootstrap。
- 这只覆盖 WAL tail 复制。sealed segment、manifest、attrs sidecar、vecindex、metadata、GC log 的远程同步仍在后续分布式路线里。

### POST /v1/trace-search  —— 跨 session 结构化 span 搜索（控制台/产品 API，camelCase 响应）

用于 trace inbox、review 列表、golden path 候选列表。它不是 BM25/向量召回，而是对折叠后的 span 做结构化过滤、分页和排序。

**请求体**：

```json
{
  "text": "builder",
  "limit": 20,
  "sort": "duration",
  "order": "desc",
  "filter": {
    "toolName": "planner",
    "attrs": {
      "project_id": "agentic-data"
    },
    "annotation": {
      "label": "best_path",
      "source": "human",
      "scoreMin": 900
    },
    "dataset": {
      "datasetId": "best-path-regression",
      "itemId": "case-1",
      "evalRunId": "eval-1"
    }
  }
}
```

常用过滤字段：

| 字段 | 说明 |
|---|---|
| `filter.sessionId` / `traceId` / `spanId` | 内部数字 id 或外部字符串 id |
| `filter.status` / `kind` / `agentName` / `toolName` / `model` | span 结构化字段 |
| `filter.inputContains` / `outputContains` / `logContains` / 顶层 `text` | 文本 contains 过滤 |
| `filter.minCostUsdNanos` / `maxCostUsdNanos` / `minCostUsd` / `maxCostUsd` | 成本范围过滤；`costUsd` 会转换成 nano USD 比较 |
| `filter.minTotalTokens` / `maxTotalTokens` / `minTokens` / `maxTokens` | total token 范围过滤；未传 `total_tokens` 时按 input+output+cached+reasoning 派生 |
| `filter.attrs` 或 `projectId` / `skill` / `mode` / `callSite` / `taskFingerprint` / `loopId` / `harnessVersion` / `schemaFingerprint` / `intentSignature` / `validationStatus` / `reviewStatus` / `evalStatus` / `pathMemoryId` / `stopReason` / `phase` / `validator` | attrs 精确过滤；这些 key 已提升为一等字段并保留 attrs fallback |
| `filter.annotation` | 嵌套对象，支持 `target`、`label`、`source`、`status`、`includeDeleted`、`scoreMin`、`scoreMax`、`attrs` |
| `filter.annotationLabel` / `annotationSource` / `annotationStatus` / `annotationScoreMin` | annotation 常用顶层别名 |
| `filter.dataset` | 嵌套对象，支持 `datasetId`、`itemId`、`evalRunId`、`split`、`label`、`scoreMin`、`scoreMax`、`attrs` |
| `filter.datasetId` / `itemId` / `evalRunId` / `datasetLabel` | dataset association 常用顶层别名 |

annotation / dataset association 的反向过滤规则：trace 级记录命中整条 trace；span 级记录只命中对应 span。`status="deleted"` 的 annotation 默认不参与反向过滤；显式传 `status:"deleted"` 或 `includeDeleted:true` 才会命中。tenant 仍由 `X-Tenant-Id` 强制注入。

**响应**：

```json
{
  "items": [
    {
      "rank": 0,
      "traceId": "12629570674344444284",
      "spanId": "12068206367433246855",
      "externalTraceId": "run-uuid",
      "externalSpanId": "span-uuid",
      "kind": "agent",
      "name": "risk-agent",
      "durationNs": 12000000,
      "usage": {
        "inputTokens": 900,
        "outputTokens": 120,
        "cachedInputTokens": 0,
        "reasoningTokens": 0,
        "totalTokens": 1020
      },
      "costUsd": 0.0012,
      "costDetail": {
        "costUsd": 0.0012,
        "costUsdNanos": 1200000,
        "currency": "USD",
        "source": "explicit"
      },
      "provider": "openai",
      "inputText": { "preview": "疑似盗刷订单", "full": null, "contentHash": "fnv1a64:...", "byteLength": 18, "truncated": false, "blobRef": null },
      "outputText": { "preview": "建议人工复核", "full": null, "contentHash": "fnv1a64:...", "byteLength": 18, "truncated": false, "blobRef": null },
      "fields": {
        "project_id": "agentic-data",
        "task_fingerprint": "npm-native-packaging",
        "validation_status": "pass"
      },
      "attrs": {
        "project_id": "agentic-data",
        "task_fingerprint": "npm-native-packaging",
        "validation_status": "pass"
      }
    }
  ],
  "nextCursor": null,
  "total": 1,
  "index": "attrs_postings+folded_verify"
}
```

`index` 用于观测本次查询的候选收窄路径：`attrs_postings+folded_verify` 表示先用 attrs postings / segment sidecar 收窄，再用 folded span 校验；`metadata_filter+folded_scan` 表示使用 annotation/dataset 元数据过滤；`folded_scan` 表示当前仍是折叠快照扫描。

cluster mode 下，`/v1/trace-search` 还会返回 `queryMode:"fanout_merge"`、`shardCount`、`okShards`、`degraded`、`failedShards` 和 `snapshot`。`degraded:false` 表示本次 fanout 覆盖了全部 shard；远程 gateway 遇到部分 shard 失败时会用同一字段表达不完整结果。remote gateway 的 snapshot 形如 `{"mode":"remote_gateway","routeTableVersion":33,"shards":[{"shard":0,"shardId":"route-a","snapshot":{...}}]}`；下一页或后续同条件查询可把这个 snapshot 原样放回请求体，gateway 会拆成 shard-local snapshot 转发。route table version 或 shard id 不匹配时返回 `409` + `code:"route_table_expired"`。

### POST /v1/trace-aggregate  —— 跨 session group-by 聚合（控制台/产品 API，camelCase 响应）

用于 trace inbox 统计和 path mining：复用 `/v1/trace-search` 的过滤语义，再按字段分组，返回 span 数、trace 去重数、错误率、duration、usage、cost 和少量示例。常见高频维度会优先走 segment 级 traceAggregate rollup + WAL tail overlay；安全条件不满足时自动回退 folded scan。

**请求体**：

```json
{
  "groupBy": ["skill", "mode", "toolName"],
  "limit": 50,
  "sort": "errorRate",
  "order": "desc",
  "filter": {
    "attrs": {
      "project_id": "agentic-data"
    },
    "annotation": {
      "label": "best_path"
    },
    "dataset": {
      "datasetId": "best-path-regression"
    }
  }
}
```

`groupBy` 支持 string 或 string array。常用字段：

| 字段 | 说明 |
|---|---|
| `projectId` / `project_id` | 按 `project_id` 一等 attrs 字段分组 |
| `skill` / `mode` / `callSite` / `call_site` | 按 AgenticData 常用 attrs 字段分组 |
| `taskFingerprint` / `task_fingerprint` | 按同类任务 fingerprint 分组 |
| `loopId` / `loop_id` | 按 agent loop 运行分组 |
| `harnessVersion` / `harness_version` | 按 workflow/prompt/harness 版本分组 |
| `validationStatus` / `validation_status` | 按验证结果分组 |
| `stopReason` / `stop_reason` | 按 loop 停止原因分组 |
| `phase` / `validator` | 按执行阶段或验证器分组 |
| `agentName` / `toolName` / `model` / `provider` / `kind` / `status` | 按 span 结构化字段分组 |
| `attrs.<key>` 或 `<key>` | 按任意 attrs key 分组，保持 JSON value 形态 |

排序支持：`count`、`traceCount`、`errorCount`、`errorRate`、`duration`、`avgDuration`、`maxDuration`、`cost`、`tokens`。默认按 `count desc`。

**响应**：

```json
{
  "items": [
    {
      "key": {
        "skill": "review",
        "mode": "auto",
        "toolName": "planner"
      },
      "spanCount": 12,
      "traceCount": 7,
      "errorCount": 2,
      "errorRate": 0.166667,
      "durationNs": {
        "sum": 930000000,
        "avg": 77500000,
        "max": 210000000,
        "p50": 63000000,
        "p95": 210000000,
        "count": 12
      },
      "usage": {
        "inputTokens": 12000,
        "outputTokens": 2100,
        "cachedInputTokens": 800,
        "reasoningTokens": 900,
        "totalTokens": 15800
      },
      "costUsd": 0.018,
      "costDetail": {
        "costUsd": 0.018,
        "costUsdNanos": 18000000,
        "currency": "USD",
        "source": "mixed"
      },
      "examples": [
        {
          "traceId": "12629570674344444284",
          "spanId": "12068206367433246855",
          "externalTraceId": "run-uuid",
          "externalSpanId": "span-uuid",
          "name": "risk-agent"
        }
      ]
    }
  ],
  "total": 1,
  "spanTotal": 12,
  "index": "attrs_postings+folded_verify",
  "aggregationIndex": "segment_rollup_tail_overlay",
  "aggregationPlanner": "segment_rollup_tail_overlay",
  "rollupEligible": true,
  "rollupBlockedBy": [],
  "readPlan": {
    "spanReadIndex": "segment_rollup",
    "usedSegmentRollup": true,
    "segmentRollupSegments": 3,
    "segmentRollupRows": 12000,
    "tailFoldedSpanCount": 0,
    "usedAttrPostings": false,
    "candidateSpanKeys": null,
    "scannedSegments": 0,
    "foldedSpanCount": 12,
    "unsupportedAttrKeys": [],
    "verification": "rollup_scope_safety",
    "rollupFallbackReason": null
  },
  "readModelCache": "miss"
}
```

高频重复调用会复用进程内 read-model cache，响应返回 `readModelCache:"miss"` 或 `readModelCache:"hit"`。`readPlan` 用来判断本次冷查询是否真正走了索引：

- `spanReadIndex:"segment_rollup"` + `usedSegmentRollup:true` 表示读取 segment 级 traceAggregate rollup，没有扫描 folded segment。
- `spanReadIndex:"tail_folded_scan"` 表示当前只有 WAL/MemTable tail，没有 sealed segment rollup 可用。
- `spanReadIndex:"attrs_postings"` 表示回退 folded scan 时，先用高频 attrs sidecar 缩小候选 span，再 folded verify。
- `spanReadIndex:"folded_scan"` 表示没有可用 postings 或 rollup，只能扫描折叠。

`segmentRollupSegments` / `segmentRollupRows` 表示本次读取了多少个 rollup sidecar 和 rollup 行；`tailFoldedSpanCount` 表示 overlay 的 WAL/MemTable tail span 数；`candidateSpanKeys` 是 attrs postings 给出的候选 span key 数；`scannedSegments` 是 folded scan 触及的 segment 数。`aggregationPlanner:"rollup_safety_fallback_folded_scan"` 且 `readPlan.rollupFallbackReason` 非空时，说明 group-by 本来可用 rollup，但因为安全条件回退，例如同一 span 横跨多个 segment、segment 有 deletion vector / upgrade patch、使用了时间窗口等。`rollupBlockedBy` 会列出 metadata 反向过滤、全文 contains、identity filter、cost/token range 或 unsupported groupBy 等规划级阻塞原因。

cluster / process gateway 模式下，`/v1/trace-aggregate` 还会返回 `queryMode`、`shardCount`、`okShards`、`degraded`、`failedShards` 和可选 `snapshot`；in-process cluster 在所有 shard 都满足安全条件时返回 `aggregationIndex:"fanout_segment_rollup_tail_overlay"`，否则整次查询退回 fanout folded reduce。process gateway 的 remote reduce 仍通过 shard API fanout 返回。聚合结果只覆盖成功 shard；当 `degraded:true` 时，`spanTotal` 和每个 bucket 的计数都不是全局完整值。remote gateway 的 snapshot 契约与 `/v1/trace-search` 相同，可用于让聚合页和列表页读取同一组 shard-local manifest。

### POST /v1/trajectory-groups  —— Trajectory group / best-path candidates（控制台/产品 API，camelCase 响应）

按过滤条件先找候选 trace，再读取每条 trace 的完整 folded spans，按规范化 trajectory signature 分桶。这个端点用于发现“同类任务中哪些执行路径最稳定、成功率最高、成本最低”，返回确定性统计证据；它不创建 golden path，也不替业务自动做最终判优。兼容别名：`POST /v1/trajectory-aggregate`、`POST /v1/best-paths`。

**请求体**：

```json
{
  "filter": {
    "taskFingerprint": "npm-native-packaging",
    "attrs": { "project_id": "agentic-data" },
    "annotation": { "label": "best_path", "scoreMin": 900 }
  },
  "sort": "best",
  "limit": 20,
  "exampleLimit": 3
}
```

过滤语义复用 `/v1/trace-search`：支持 text、status、toolName、model、attrs、annotation、dataset 等条件。默认 `sort="best"`，综合 success rate、evalScore、annotation score、dataset score，并用 traceCount、duration、cost 做 tie-breaker。可选排序：`traceCount`、`successRate`、`evalScore`、`annotationScore`、`datasetScore`、`duration`、`cost`、`tokens`。

**响应**：

```json
{
  "items": [
    {
      "signature": "fnv1a64:94db9e1afaf33121",
      "stepCount": 2,
      "steps": ["tool:planner|phase:plan", "tool:tester|phase:verify|validator:npm_test"],
      "traceCount": 2,
      "spanCount": 4,
      "successCount": 2,
      "errorTraceCount": 0,
      "errorSpanCount": 0,
      "successRate": 1.0,
      "errorRate": 0.0,
      "qualityScore": 960,
      "durationNs": { "sum": 54, "avg": 27, "max": 30, "p50": 24, "p95": 30, "count": 2 },
      "usage": { "inputTokens": 54, "outputTokens": 27, "cachedInputTokens": 0, "reasoningTokens": 0, "totalTokens": 81 },
      "costUsd": 0.000005,
      "costDetail": { "costUsd": 0.000005, "costUsdNanos": 5400, "currency": "USD", "source": "mixed" },
      "scores": {
        "eval": { "count": 0, "avg": null, "min": null, "max": null },
        "annotation": { "count": 2, "avg": 940, "min": 920, "max": 960 },
        "dataset": { "count": 2, "avg": 940, "min": 930, "max": 950 }
      },
      "examples": [
        {
          "traceId": "401",
          "externalTraceId": "run-401",
          "status": "ok",
          "qualityScore": 970,
          "fields": { "task_fingerprint": "npm-native-packaging", "validation_status": "pass" }
        }
      ]
    }
  ],
  "total": 1,
  "traceTotal": 2,
  "spanTotal": 4,
  "index": "attrs_postings+folded_verify",
  "trajectoryIndex": "materialized_cache"
}
```

高频重复调用会复用进程内 read-model cache，响应会额外返回 `readModelCache:"miss"` 或 `readModelCache:"hit"`。在 process gateway 模式下，该接口会跨 shard fanout + reduce，`trajectoryIndex` 会变为 `remote_fanout_materialized_reduce` 并带 `okShards` / `degraded` / `failedShards` 诊断。

### POST /v1/trace-trajectories  —— Materialized trace trajectory read model（控制台/产品 API，camelCase 响应）

按 `/v1/trace-search` 的过滤语义筛出 trace，再返回每条 trace 的轻量 trajectory 摘要。它面向 Trace Inbox、Golden Path review 和 Agent Memory 导出前的列表页：不读取 input/output/log 大字段，只返回路径、耗时、usage/cost 和 scope 字段。兼容别名：`POST /v1/trajectories`。

**请求体**：

```json
{
  "filter": {
    "taskFingerprint": "npm-native-packaging",
    "attrs": {
      "project_id": "agentic-data",
      "skill": "builder"
    }
  },
  "limit": 50,
  "cursor": 0
}
```

**响应**：

```json
{
  "items": [
    {
      "trace": {
        "traceId": "401",
        "externalTraceId": "run-401",
        "spanCount": 2,
        "errorCount": 0,
        "status": "ok",
        "durationNs": { "sum": 54, "max": 30 },
        "usage": { "inputTokens": 54, "outputTokens": 27, "cachedInputTokens": 0, "reasoningTokens": 0, "totalTokens": 81 },
        "costUsd": 0.000005,
        "fields": {
          "project_id": "agentic-data",
          "task_fingerprint": "npm-native-packaging",
          "model": "qwen"
        }
      },
      "trajectory": {
        "signature": "fnv1a64:94db9e1afaf33121",
        "stepCount": 2,
        "steps": ["tool:planner|phase:plan", "tool:tester|phase:verify|validator:npm_test"]
      },
      "index": "materialized"
    }
  ],
  "nextCursor": null,
  "total": 1,
  "spanTotal": 2,
  "index": "materialized"
}
```

高频重复调用会复用进程内 read-model cache，响应会额外返回 `readModelCache:"miss"` 或 `readModelCache:"hit"`。在 process gateway 模式下，该接口会跨 shard merge/page，`index` 会变为 `remote_fanout_materialized` 并带 shard 诊断字段。

### POST /v1/storage-stats  —— Storage governance stats（控制台/产品 API，camelCase 响应）

按 `/v1/trace-search` 的过滤语义统计 trace/span/event 数量和可解释的存储占用估算。它不是物理磁盘精确账单：input/output/log/attrs/external id 字节按 UTF-8 长度计算，segment/WAL 结构开销用 `estimatedBytes` 近似表达。

兼容别名：`POST /v1/storage/stats`。

**请求体**：

```json
{
  "filter": {
    "projectId": "agentic-data",
    "taskFingerprint": "npm-native-packaging"
  },
  "groupBy": ["projectId", "validationStatus"],
  "timeBucketNs": 86400000000000
}
```

`groupBy` 支持 camelCase / snake_case，常用值包括 `projectId`、`skill`、`mode`、`taskFingerprint`、`schemaFingerprint`、`intentSignature`、`validationStatus`、`reviewStatus`、`evalStatus`、`pathMemoryId`、`sessionId`、`time`。`time` 使用 trace 最早事件时间按 `timeBucketNs` 分桶。

**响应**：

```json
{
  "groupBy": ["project_id", "validation_status"],
  "total": {
    "traceCount": 2,
    "spanCount": 2,
    "sessionCount": 2,
    "eventCount": 5,
    "errorSpanCount": 0,
    "firstTs": 100,
    "lastTs": 220,
    "bytes": {
      "inputText": 120,
      "outputText": 80,
      "logs": 30,
      "payload": 230,
      "attrs": 180,
      "externalIds": 70,
      "fields": 160,
      "estimated": 1200,
      "estimatedBytes": 1200
    },
    "metadata": {
      "annotations": 1,
      "datasetAssociations": 1,
      "goldenPaths": 1,
      "snapshotRefs": 1,
      "evalLinks": 1,
      "pathMemoryRefs": 1
    }
  },
  "groups": [
    {
      "key": {
        "project_id": "agentic-data",
        "validation_status": "pass"
      },
      "traceCount": 2,
      "spanCount": 2,
      "sessionCount": 2,
      "eventCount": 5,
      "errorSpanCount": 0,
      "firstTs": 100,
      "lastTs": 220,
      "bytes": { "estimatedBytes": 1200 },
      "metadata": {
        "annotations": 1,
        "datasetAssociations": 1,
        "goldenPaths": 1,
        "snapshotRefs": 1,
        "evalLinks": 1,
        "pathMemoryRefs": 1
      }
    }
  ]
}
```

### POST /v1/retention-plan / POST /v1/retention/apply  —— Retention planning and segment-row delete（控制台/产品 API，camelCase 响应）

`retention-plan` 是 dry-run：按 `/v1/trace-search` 过滤和 `deleteBeforeTs` 找候选 trace，默认保护已被非 deleted annotation、dataset association、active Golden Path（candidate/confirmed）、snapshot 引用、eval link、path memory 引用的 trace。`retention/apply` 执行删除，但只软删除已经 flush 到 segment 的行；仍在 MemTable/WAL tail 的热 trace 会整条跳过，避免半删。

**请求体**：

```json
{
  "filter": {
    "projectId": "agentic-data",
    "taskFingerprint": "npm-native-packaging"
  },
  "deleteBeforeTs": 1751540000000000000,
  "protect": {
    "annotations": true,
    "datasetAssociations": true,
    "goldenPaths": true,
    "snapshots": true,
    "evalLinks": true,
    "pathMemory": true
  },
  "compact": true,
  "compactMinDeletedRows": 1,
  "compactMinDeletedPercent": 1,
  "compactMaxSegments": 64,
  "reclaim": true,
  "requestedBy": "nightly-retention-policy",
  "reason": "ttl cleanup",
  "exampleLimit": 20
}
```

`POST /v1/retention/apply` 必须传 `deleteBeforeTs`。`POST /v1/retention-plan` 不传 `deleteBeforeTs` 时只做“当前 filter 命中的数据如果清理会怎样”的估算。`protect` 默认全开；可分别关闭 `annotations`、`datasetAssociations`、`goldenPaths`、`snapshots`、`evalLinks`、`pathMemory`。其中 `snapshots` 来自 dataset/golden path 上的 `snapshotId` / `snapshotHash`，`evalLinks` 来自 `evalRunId` 或 eval attrs，`pathMemory` 来自 metadata attrs 的 `path_memory_id`。`compact` 默认 `false`；设为 `true` 时，apply 后会选择 deletion ratio 达标的 segment 重写成干净 segment，并按 `reclaim` 尝试走 GC log 安全回收。仍有旧读者 pin 住快照时，`reclaim` 可能暂时回收 0 个段，这是正常的水位保护。

**响应**：

```json
{
  "dryRun": true,
  "applied": false,
  "deleteBeforeTs": 1751540000000000000,
  "protect": {
    "goldenPaths": true,
    "annotations": true,
    "datasetAssociations": true,
    "snapshots": true,
    "evalLinks": true,
    "pathMemory": true
  },
  "compact": {
    "requested": true,
    "minDeletedRows": 1,
    "minDeletedPercent": 1,
    "maxSegments": 64,
    "reclaim": true
  },
  "candidates": { "traceCount": 2, "bytes": { "estimatedBytes": 1200 } },
  "protected": { "traceCount": 1, "bytes": { "estimatedBytes": 700 } },
  "deletable": { "traceCount": 1, "bytes": { "estimatedBytes": 500 } },
  "protectedReasons": {
    "823456789": ["annotation", "datasetAssociation", "goldenPath", "snapshot", "evalLink", "pathMemory"]
  },
  "deletableTraceIds": ["923456789"],
  "applyResult": null,
  "compactResult": null,
  "audit": null
}
```

`retention/apply` 的 `applyResult` 会返回：

```json
{
  "requestedTraceCount": 1,
  "deletedTraceCount": 1,
  "deletedSegmentRowCount": 2,
  "skippedLiveTraceCount": 0,
  "deletedTraceIds": ["923456789"],
  "skippedLiveTraceIds": []
}
```

如果 `compact=true`，`compactResult` 会返回：

```json
{
  "beforeLiveSegmentCount": 1,
  "afterLiveSegmentCount": 1,
  "beforeDeadSegmentCount": 0,
  "afterDeadSegmentCount": 0,
  "selectedSegmentCount": 1,
  "compactedSegmentCount": 1,
  "reclaimedSegmentCount": 1,
  "droppedDeletedRowCount": 2,
  "rewrittenLiveRowCount": 4,
  "selectedSegmentIds": ["12"]
}
```

`retention/apply` 成功执行后会同步写入一条 tenant-scoped audit record，响应中的 `audit` 会返回同一条记录。审计只保存策略、保护开关、计数和 trace id 样本，不复制 trace payload；单批 trace id 样本默认最多 100 条，超过时 `sampleTruncated=true`。

```json
{
  "auditId": "1",
  "tenantId": "1",
  "createdAtNs": "1751540000000000000",
  "source": "nightly-retention-policy",
  "reason": "ttl cleanup",
  "deleteBeforeTs": 1751540000000000000,
  "query": {
    "filter": { "projectId": "agentic-data" },
    "deleteBeforeTs": 1751540000000000000,
    "compact": true
  },
  "protect": {
    "goldenPaths": true,
    "annotations": true,
    "datasetAssociations": true,
    "snapshots": true,
    "evalLinks": true,
    "pathMemory": true
  },
  "compact": {
    "requested": true,
    "reclaim": true,
    "compactedSegmentCount": 1,
    "reclaimedSegmentCount": 1,
    "droppedDeletedRowCount": 2,
    "rewrittenLiveRowCount": 4
  },
  "counts": {
    "candidateTraceCount": 2,
    "protectedTraceCount": 1,
    "deletableTraceCount": 1,
    "requestedTraceCount": 1,
    "deletedTraceCount": 1,
    "deletedSegmentRowCount": 2,
    "skippedLiveTraceCount": 0
  },
  "traceIds": {
    "deletable": ["923456789"],
    "deleted": ["923456789"],
    "skippedLive": [],
    "sampleTruncated": false
  }
}
```

### POST/GET /v1/retention-policies / POST /v1/retention-policies/run-due  —— Retention policy scheduler（控制台/产品 API，camelCase 响应）

Retention policy 是可持久化、可重复执行的清理策略。yiTrace 只提供调度底座：保存策略、计算到期、手动触发到期策略，并复用 `retention/apply` 写审计；它不会在嵌入式 Node/Electron 进程里自动启动后台删除线程。业务侧可以用 cron、队列、Electron main process timer 或运维任务定期调用 `run-due`。

兼容别名：`POST /v1/retention/policies`、`GET /v1/retention/policies`、`POST /v1/retention/policies/run-due`、`POST /v1/retention/run-due`。

创建 policy：

```bash
curl -XPOST localhost:7878/v1/retention-policies \
  -H 'Content-Type: application/json' \
  -d '{
    "name": "nightly-retention-policy",
    "intervalNs": 86400000000000,
    "nextRunAtNs": 1751540000000000000,
    "source": "nightly-retention-policy",
    "reason": "ttl cleanup",
    "query": {
      "filter": { "attrs": { "project_id": "agentic-data" } },
      "olderThanNs": 2592000000000000,
      "compact": true
    }
  }'
```

`query` 是 `retention/apply` 的查询模板，必须包含 `deleteBeforeTs` 或 `olderThanNs` / `ttlNs` / `retentionNs`。推荐用 `olderThanNs` 这类 TTL 字段；每次执行时 yiTrace 会按本次 `nowNs` 动态换算成 `deleteBeforeTs`，再注入 `apply:true`、`requestedBy` 和 `reason`。

创建响应：

```json
{
  "policyId": "1",
  "tenantId": "1",
  "name": "nightly-retention-policy",
  "enabled": true,
  "createdAtNs": "1751540000000000000",
  "updatedAtNs": "1751540000000000000",
  "lastRunAtNs": null,
  "nextRunAtNs": "1751540000000000000",
  "intervalNs": "86400000000000",
  "source": "nightly-retention-policy",
  "reason": "ttl cleanup",
  "query": {
    "filter": { "attrs": { "project_id": "agentic-data" } },
    "olderThanNs": 2592000000000000,
    "compact": true
  }
}
```

查询 policy：

```bash
curl "localhost:7878/v1/retention-policies?name=nightly-retention-policy&enabled=true&limit=20"
```

响应：

```json
{
  "items": [
    { "policyId": "1", "name": "nightly-retention-policy", "enabled": true }
  ],
  "nextCursor": null,
  "total": 1
}
```

执行到期 policy：

```bash
curl -XPOST localhost:7878/v1/retention-policies/run-due \
  -H 'Content-Type: application/json' \
  -d '{"nowNs":1751540000000000000,"limit":10}'
```

响应：

```json
{
  "nowNs": "1751540000000000000",
  "ran": 1,
  "failed": 0,
  "skipped": 0,
  "items": [
    {
      "policy": {
        "policyId": "1",
        "lastRunAtNs": "1751540000000000000",
        "nextRunAtNs": "1751626400000000000"
      },
      "ok": true,
      "statusCode": 200,
      "result": {
        "applied": true,
        "applyResult": { "deletedTraceCount": 1 },
        "audit": { "source": "nightly-retention-policy" }
      }
    }
  ]
}
```

只有 `statusCode=200` 的 policy 会推进 `lastRunAtNs/nextRunAtNs`。执行失败会保留原有 `nextRunAtNs`，便于下次重试。

### GET/POST /v1/retention-audits  —— Retention audit log（控制台/产品 API，camelCase 响应）

查询历史 `retention/apply` 执行审计。兼容别名：`/v1/retention/audits`。GET 用 query string，POST 用 JSON body，二者都按 `X-Tenant-Id` 隔离。

```bash
curl "localhost:7878/v1/retention-audits?source=nightly-retention-policy&limit=20"
```

```json
{
  "filter": {
    "source": "nightly-retention-policy"
  },
  "limit": 20,
  "cursor": 0
}
```

响应：

```json
{
  "items": [
    { "auditId": "1", "source": "nightly-retention-policy", "counts": { "deletedTraceCount": 1 } }
  ],
  "nextCursor": null,
  "total": 1
}
```

### Golden Path Candidate Store  —— 候选路径资产（控制台/产品 API，camelCase 响应）

`trajectory-groups` 负责发现候选路径，`trace-trajectories` 负责逐 trace 读取轻量路径摘要，Golden Path Store 负责把其中一条 trace/trajectory 登记成可治理资产。它保存源 trace/snapshot 引用、trajectory signature、scope attrs、轻量 source trajectory、证据摘要、状态和评审信息；不复制 input/output/log 等 trace payload。

默认 scope 会从 source trace 补齐 `project_id`、`task_fingerprint`、`skill`、`mode`、`harness_version`、`schema_fingerprint`、`eval_profile`、`model`、`provider`、`tool_version`。这些字段用于后续 `goldenPaths()`、`pathAdherence()` 和 `goldenPathHealth()` 收窄比较范围，避免跨模型、跨工具版本或跨 schema 混在一起判优。

当前版本不提供重复命中记录或引用计数。重复场景的 raw trace 仍按正常摄入和保留策略处理；是否要做 hit/reference count、压缩重复 trace、或只保留 canonical source snapshot，后续作为单独需求设计。

**创建候选：`POST /v1/golden-paths`**

```json
{
  "sourceTraceId": "run-uuid",
  "taskFingerprint": "npm-native-packaging",
  "score": 960,
  "label": "fast packaging path",
  "reason": "best observed route",
  "source": "human",
  "evalProfile": "release-gate",
  "challengerOf": null,
  "minSampleCount": 5,
  "marginScore": 800,
  "comparisonWindowNs": 86400000000000,
  "staleReasons": [],
  "projectId": "agentic-data"
}
```

字段说明：

- `sourceTraceId` 支持数字 trace id 或外部字符串 id。
- `taskFingerprint` 必填；未传时会尝试从 source trace 的 `task_fingerprint` 字段推断。
- `trajectorySignature` 可选；未传时由 source trace 的 folded spans 自动计算 `fnv1a64:*`。
- `status` 可选，默认 `candidate`；可用值：`candidate` / `confirmed` / `rejected` / `deprecated`。
- `projectId`、`skill`、`mode`、`callSite`、`model`、`provider`、`toolVersion` 等一等 attrs 会写入 `attrs`，用于后续过滤；顶层 `evalProfile` 是 Golden Path 治理字段，不会自动污染 trace scope filter，除非同时放进 `attrs`。
- `challengerOf` / `evalProfile` / `minSampleCount` / `marginScore` / `comparisonWindowNs` 是 Best/Challenger 治理底座。yiTrace 只记录比较窗口和阈值证据，不自动 promote/deprecate。
- `deprecationReason` / `staleReasons` 可由产品层写入；`goldenPathHealth()` 也会基于样本数、health 和 source signature 输出派生 stale reasons。
- `evidence` / `evidenceSummary` 可传入产品层证据摘要，例如 `sampleCount`、`successRate`、`avgCostUsdNanos`、`p95DurationNs`；系统也会补 `source_span_count`、`source_status`、`source_total_tokens`、`source_cost_usd_nanos`、`source_trajectory_signature` 等 source 摘要。

**查询：`GET /v1/golden-paths?taskFingerprint=...&status=confirmed&projectId=...`**

支持按 `goldenPathId`、`taskFingerprint`、`trajectorySignature`、`sourceTraceId`、`challengerOf`、`evalProfile`、`status` 和 attrs 精确过滤；tenant 从 `X-Tenant-Id` 注入。

**确认/拒绝/废弃：`POST /v1/golden-paths/:id/status`**

```json
{
  "status": "confirmed",
  "reason": "manual accept",
  "source": "reviewer"
}
```

响应示例：

```json
{
  "goldenPathId": "1",
  "tenantId": "1",
  "taskFingerprint": "npm-native-packaging",
  "trajectorySignature": "fnv1a64:94db9e1afaf33121",
  "sourceTraceId": "15331101233991596328",
  "externalSourceTraceId": "run-uuid",
  "status": "confirmed",
  "score": 960,
  "label": "fast packaging path",
  "reason": "manual accept",
  "source": "reviewer",
  "evalProfile": "release-gate",
  "challengerOf": null,
  "minSampleCount": "5",
  "marginScore": 800,
  "comparisonWindowNs": "86400000000000",
  "staleReasons": [],
  "governance": {
    "challengerOf": null,
    "evalProfile": "release-gate",
    "minSampleCount": "5",
    "marginScore": 800,
    "comparisonWindowNs": "86400000000000",
    "staleReasons": []
  },
  "attrs": {
    "project_id": "agentic-data",
    "task_fingerprint": "npm-native-packaging",
    "model": "qwen"
  },
  "sourceTrajectory": {
    "signature": "fnv1a64:94db9e1afaf33121",
    "stepCount": 2,
    "steps": ["tool:planner|phase:plan", "tool:tester|phase:verify|validator:npm_test"]
  },
  "evidenceSummary": {
    "source_span_count": 2,
    "source_status": "ok",
    "source_trajectory_step_count": 2
  }
}
```

### POST /v1/path-adherence  —— Golden Path adherence evidence（控制台/产品 API，camelCase 响应）

比较一条新 trace 是否沿着某个 Golden Path 的 trajectory 执行。这个端点只返回底层证据：signature 是否一致、共同步骤、缺失步骤、额外步骤和 coverage；它不判断“这条路径是不是当前最佳”。

兼容路径：`POST /v1/golden-paths/:id/adherence`，此时 body 只需要 `traceId`。

**请求体**：

```json
{
  "goldenPathId": "1",
  "traceId": "candidate-run-uuid"
}
```

**响应要点**：

- `adherence`: `followed` 表示 signature 完全一致；`extended` 表示 Golden Path 步骤都按顺序出现，但新 trace 多走了额外步骤；`partial` 表示只命中部分步骤；`deviated` 表示没有共同轨迹；`unknown` 表示 source trace 已不可读，只能看存下来的 signature。
- `sameSignature`: 新 trace 的 trajectory signature 是否等于 Golden Path 存储的 signature。
- `sourceRetained`: 即使 raw source trace 已被 retention 清理，Golden Path 元数据里仍保留轻量 source trajectory，可继续做 path adherence；如果为 `false`，只能看存储的 signature。
- `commonSteps` / `missingSteps` / `extraSteps`: 用于 UI diff、回归解释和 Agent Memory 候选证据。

示例：

```json
{
  "adherence": "extended",
  "sameSignature": false,
  "sourceAvailable": true,
  "sourceRetained": true,
  "goldenTrajectory": {
    "signature": "fnv1a64:fd0b5f81980a77a2",
    "stepCount": 1,
    "steps": ["tool:planner|phase:plan"]
  },
  "traceTrajectory": {
    "signature": "fnv1a64:94db9e1afaf33121",
    "stepCount": 2,
    "steps": ["tool:planner|phase:plan", "tool:tester|phase:verify"]
  },
  "scores": {
    "commonStepCount": 1,
    "goldenStepCount": 1,
    "traceStepCount": 2,
    "goldenCoverage": 1.0,
    "traceCoverage": 0.5
  },
  "missingSteps": [],
  "extraSteps": ["tool:tester|phase:verify"]
}
```

### POST /v1/golden-path-evidence  —— Golden Path evidence bundle（控制台/产品 API，camelCase 响应）

导出一条 Golden Path 的底层证据包：source trace 摘要、trajectory、annotation、dataset association。传入 `candidateTraceId` 时，会额外附带 `pathAdherence` 和 `traceDiff`。这个端点用于人工 review、回归集构建或 Agent Memory 导出前的证据收集，不更新 Golden Path 状态。

兼容路径：`POST /v1/golden-paths/evidence`、`POST /v1/golden-paths/:id/evidence`。

**请求体**：

```json
{
  "goldenPathId": "1",
  "candidateTraceId": "candidate-run-uuid"
}
```

**响应要点**：

- `source`: Golden Path 引用的 source trace 证据；如果 raw trace 已被保留策略清掉，`available=false`，但 Golden Path 元数据仍保留 `sourceTrajectory` 作为轻量路径证据。
- `candidate`: 未传 `candidateTraceId` 时为 `null`；传入后包含候选 trace 的 evidence、`pathAdherence` 和 `traceDiff`。
- `annotations` / `datasetAssociations`: 保留原始业务元数据，便于上层复核“为什么这条路径被选中”。

示例：

```json
{
  "goldenPath": { "goldenPathId": "1", "status": "confirmed" },
  "source": {
    "available": true,
    "trajectory": { "signature": "fnv1a64:fd0b5f81980a77a2", "steps": ["tool:planner|phase:plan"] },
    "annotationCount": 1,
    "datasetAssociationCount": 1
  },
  "candidate": {
    "pathAdherence": { "adherence": "extended", "sameSignature": false },
    "traceDiff": { "delta": { "spanCount": 1 } }
  }
}
```

### POST /v1/golden-path-export  —— Golden Path JSONL export（控制台/产品 API，camelCase 响应）

按稳定 schema 导出 Golden Path 记录，供 Agent Memory 或 regression dataset 管线消费。默认只导出 `confirmed` Golden Path；如需导出 `candidate` / `rejected` / `deprecated`，显式传 `status`。响应同时返回 `items` 和 `jsonl`：前者方便 SDK 直接用，后者可直接写入 JSONL 文件。消费侧应该把它当作“可追溯候选证据”，而不是数据库替业务下的 BestPath 结论。

兼容路径：`POST /v1/golden-paths/export`。

**请求体**：

```json
{
  "filter": {
    "taskFingerprint": "npm-native-packaging",
    "projectId": "agentic-data"
  },
  "limit": 100
}
```

过滤字段复用 Golden Path 查询语义：`goldenPathId`、`taskFingerprint`、`trajectorySignature`、`sourceTraceId`、`status`、`attrs` / 一等 attrs。`limit` 最大 500。

**响应**：

```json
{
  "schemaVersion": "yitrace.golden_path_export.v1",
  "format": "jsonl",
  "count": 1,
  "items": [
    {
      "schemaVersion": "yitrace.golden_path_export.v1",
      "recordType": "golden_path",
      "goldenPath": { "goldenPathId": "1", "status": "confirmed" },
      "source": {
        "available": true,
        "trajectory": { "signature": "fnv1a64:fd0b5f81980a77a2", "steps": ["tool:planner|phase:plan"] },
        "annotationCount": 1,
        "datasetAssociationCount": 1
      },
      "exportedAtNs": "1783090000000000000"
    }
  ],
  "jsonl": "{\"schemaVersion\":\"yitrace.golden_path_export.v1\",...}"
}
```

### POST /v1/golden-path-health  —— Golden Path adherence health（控制台/产品 API，camelCase 响应）

批量统计一批同 scope trace 是否还遵循某条 Golden Path。默认用 Golden Path 的 `taskFingerprint + attrs` 收窄窗口，并排除 source trace，避免把确认样本本身算进后续健康度。这个端点只返回持续校验证据，不会自动 promote/deprecate，也不会判断“当前最佳”。

兼容路径：`POST /v1/golden-paths/health`、`POST /v1/golden-paths/:id/health`。

**请求体**：

```json
{
  "goldenPathId": "1",
  "filter": {
    "projectId": "agentic-data",
    "timeFrom": 1783090000000000000
  },
  "limit": 100,
  "examples": 5,
  "includeSource": false
}
```

过滤语义复用 `/v1/trace-search`：支持 attrs、一等字段、time range、annotation、dataset 等条件。`limit` 最大 500；`examples` 最大 50。

**响应要点**：

- `counts`: `followed` / `extended` / `partial` / `deviated` / `unknown` 分布。
- `rates.usable`: `followed + extended` 的比例，用来观察旧路径是否仍被后续 trace 覆盖。
- `coverage`: 聚合 common/golden/trace step coverage。
- `sourceRetained`: 使用 Golden Path 元数据里保留的 source trajectory 时也会为 `true`，避免 raw source trace 被清理后 health 直接退化为 unknown。
- `governance`: 返回 `evalProfile`、`challengerOf`、`minSampleCount`、`marginScore`、`comparisonWindowNs` 和 `staleReasons`。常见 stale reason 包括 `insufficient_samples`、`health_below_margin`、`source_signature_changed`、`deprecated`。
- `examples`: 轻量 trace 摘要和 trajectory，不包含大字段正文。

```json
{
  "goldenPath": { "goldenPathId": "1", "status": "confirmed" },
  "sourceAvailable": true,
  "sourceRetained": true,
  "window": {
    "limit": 100,
    "includeSource": false,
    "spanTotal": 42,
    "matchingTraceTotal": 12,
    "analyzedTraceTotal": 12
  },
  "counts": {
    "total": 12,
    "followed": 8,
    "extended": 2,
    "partial": 1,
    "deviated": 1,
    "unknown": 0
  },
  "rates": {
    "followed": 0.666667,
    "usable": 0.833333,
    "deviated": 0.083333,
    "unknown": 0.0
  },
  "coverage": {
    "commonStepCount": 30,
    "goldenStepCount": 36,
    "traceStepCount": 40,
    "goldenCoverage": 0.833333,
    "traceCoverage": 0.75
  },
  "governance": {
    "evalProfile": "release-gate",
    "challengerOf": null,
    "minSampleCount": "5",
    "marginScore": 800,
    "comparisonWindowNs": "86400000000000",
    "stale": false,
    "staleReasons": []
  },
  "examples": [
    {
      "adherence": "deviated",
      "trace": { "traceId": "candidate-run-uuid", "status": "ok" },
      "traceTrajectory": { "signature": "fnv1a64:..." }
    }
  ]
}
```

### POST /v1/traces/diff  —— Trace trajectory diff（控制台/产品 API，camelCase 响应）

比较两条 trace 的 route、step、trajectory signature、duration、token、cost、status 和主要 agent 字段差异。这个端点只返回确定性证据，不自动判断哪条更好；上层可以结合 eval、annotation 或业务规则做 golden path 判优。`POST /v1/trace-diff` 是兼容别名。

**请求体**：

```json
{
  "leftTraceId": "run-old",
  "rightTraceId": "run-new"
}
```

trace id 支持数字 id 或外部字符串 id。字段别名：`leftTraceId` / `left_trace_id` / `left` / `baseTraceId` / `a`，以及 `rightTraceId` / `right_trace_id` / `right` / `candidateTraceId` / `b`。

**响应**：

```json
{
  "left": {
    "traceId": "301",
    "externalTraceId": "run-old",
    "spanCount": 1,
    "errorCount": 0,
    "status": "ok",
    "durationNs": { "sum": 10, "max": 10 },
    "usage": { "inputTokens": 5, "outputTokens": 10, "cachedInputTokens": 0, "reasoningTokens": 0, "totalTokens": 15 },
    "costUsd": 0.000001,
    "costDetail": { "costUsdNanos": 1000, "currency": "USD", "source": "mixed" },
    "fields": { "task_fingerprint": "diff-task", "loop_id": "loop-a" }
  },
  "right": {
    "traceId": "302",
    "spanCount": 2,
    "errorCount": 1,
    "status": "error"
  },
  "delta": {
    "spanCount": 1,
    "errorCount": 1,
    "durationNs": 18,
    "inputTokens": 1,
    "outputTokens": 1,
    "totalTokens": 2,
    "costUsdNanos": 1500,
    "costUsd": 0.000002
  },
  "trajectory": {
    "left": {
      "signature": "fnv1a64:fd0b5f81980a77a2",
      "stepCount": 1,
      "steps": ["tool:planner|phase:plan"]
    },
    "right": {
      "signature": "fnv1a64:94db9e1afaf33121",
      "stepCount": 2,
      "steps": ["tool:planner|phase:plan", "tool:tester|phase:verify"]
    },
    "same": false
  },
  "routes": {
    "left": [{ "spanId": "1", "kind": "tool", "name": "planner", "spanOrdinal": 0, "sortKey": "00000000000000000000:00000000000000000001", "toolName": "planner", "statusText": "ok" }],
    "right": [
      { "spanId": "1", "kind": "tool", "name": "planner", "spanOrdinal": 0, "sortKey": "00000000000000000000:00000000000000000001", "toolName": "planner", "statusText": "ok" },
      { "spanId": "2", "kind": "tool", "name": "tester", "spanOrdinal": 1, "sortKey": "00000000000000000001:00000000000000000002", "toolName": "tester", "statusText": "error" }
    ]
  },
  "steps": [
    {
      "index": 0,
      "status": "changed",
      "changes": ["durationNs", "totalTokens", "costUsd"],
      "left": { "spanId": "1", "toolName": "planner", "evalScore": 1000, "evalLabel": "通过", "outputPreview": "只跑相关测试" },
      "right": { "spanId": "1", "toolName": "planner", "evalScore": 1000, "evalLabel": "通过", "outputPreview": "只跑相关测试" },
      "delta": { "durationNs": -2, "totalTokens": -3, "costUsdNanos": -500, "costUsd": -0.000001 }
    },
    {
      "index": 1,
      "status": "right_only",
      "changes": [],
      "left": null,
      "right": { "spanId": "2", "toolName": "tester", "statusText": "error", "evalScore": 0, "evalLabel": "未通过", "outputPreview": "npm test failed" }
    }
  ]
}
```

### GET /v1/loops  —— Agent loop 摘要列表（控制台/产品 API，camelCase 响应）

按 `loop_id` 聚合 folded spans，给 Loop Health / Path Mining 页直接用。它不做自动诊断，只返回稳定读模型。

查询参数：`cursor` / `limit` 分页，`filter` / `text` / `q` 做 contains；`attrs`、`projectId`、`skill`、`mode`、`taskFingerprint`、`schemaFingerprint`、`intentSignature`、`validationStatus`、`reviewStatus`、`evalStatus`、`pathMemoryId` 等一等字段可精确过滤；annotation / dataset 反向过滤参数与 `traceSearch` 相同。

响应：

```json
{
  "items": [
    {
      "loopId": "loop-a",
      "loopValue": "loop-a",
      "taskFingerprint": "npm-native-packaging",
      "status": "error",
      "spanCount": 2,
      "traceCount": 2,
      "sessionCount": 2,
      "errorCount": 1,
      "errorRate": 0.5,
      "firstTraceId": "201",
      "lastTraceId": "202",
      "durationNs": { "sum": 30, "avg": 15, "max": 20, "p50": 10, "p95": 20, "count": 2 },
      "usage": { "inputTokens": 12, "outputTokens": 18, "cachedInputTokens": 0, "reasoningTokens": 0, "totalTokens": 30 },
      "costUsd": 0.000003,
      "costDetail": { "costUsd": 0.000003, "costUsdNanos": 3000, "currency": "USD", "source": "mixed" },
      "phases": ["verify"],
      "validators": ["npm test"],
      "fields": { "task_fingerprint": "npm-native-packaging", "loop_id": "loop-a", "validation_status": "pass" },
      "examples": [{ "traceId": "201", "spanId": "1", "externalTraceId": null, "externalSpanId": null, "name": "builder" }]
    }
  ],
  "nextCursor": null,
  "total": 1
}
```

### GET /v1/loops/:loopId  —— Agent loop 详情（控制台/产品 API，camelCase 响应）

返回一个 loop 的 `summary`、该 loop 下的 trace 摘要和 span 列表。`loopId` 按字符串匹配 `loop_id` 一等字段。

```json
{
  "summary": { "loopId": "loop-a", "traceCount": 2, "spanCount": 2 },
  "traces": [{ "traceId": "201", "spanCount": 1, "fields": { "loop_id": "loop-a" } }],
  "spans": [{ "traceId": "201", "spanId": "1", "fields": { "loop_id": "loop-a" } }]
}
```

### GET /v1/tasks/:fingerprint/traces  —— 同类任务 trace 列表（控制台/产品 API，camelCase 响应）

按 `task_fingerprint` 精确过滤并返回 trace 摘要页。支持与 `/v1/loops` 相同的 attrs、metadata、`filter`、`cursor`、`limit` 查询参数。

```json
{
  "items": [
    {
      "traceId": "201",
      "externalTraceId": "run-uuid",
      "spanCount": 1,
      "errorCount": 0,
      "status": "ok",
      "durationNs": { "sum": 10, "max": 10 },
      "usage": { "inputTokens": 5, "outputTokens": 10, "cachedInputTokens": 0, "reasoningTokens": 0, "totalTokens": 15 },
      "costUsd": 0.000001,
      "costDetail": { "costUsd": 0.000001, "costUsdNanos": 1000, "currency": "USD", "source": "mixed" },
      "fields": { "task_fingerprint": "npm-native-packaging", "loop_id": "loop-a" }
    }
  ],
  "nextCursor": null,
  "total": 1
}
```

---

## 业务元数据 API（annotation / dataset association）

这组端点不改写 trace 主数据。它保存的是后验业务判断：某条 trace/span 被人工或自动流程标成什么，以及它对应哪个外部 dataset item。持久化文件是 durable data dir 下的 `metadata.dat`，在线备份会一起拷走。

### POST /v1/annotations

给 trace 或 span 追加一条 annotation。

**请求体**：

```json
{
  "traceId": "run-uuid",
  "spanId": "span-uuid",
  "target": "span",
  "label": "best_path",
  "score": 920,
  "reason": "人工确认这次路径最短",
  "source": "human",
  "status": "active",
  "reviewer": "four",
  "projectId": "agentic-data",
  "skill": "review"
}
```

| 字段 | 类型 | 必需 | 说明 |
|---|---|---|---|
| `trace_id` / `traceId` | u64 或 string | 是 | 支持内部数字 id 或外部字符串 id；字符串会稳定 hash，并保留到 `externalTraceId` |
| `span_id` / `spanId` | u64 或 string | 否 | 传了则默认 `target=span`；字符串原文保留到 `externalSpanId` |
| `target` | `trace` / `span` | 否 | 不传时根据是否有 span id 推断 |
| `label` | string | 是 | 业务标签，如 `best_path`、`bad_answer`、`needs_review` |
| `score` | u32? | 否 | 建议沿用千分制 0–1000 |
| `reason` / `comment` / `note` | string? | 否 | 判断原因 |
| `source` / `createdBy` | string? | 否 | `human` / `rule` / `eval` / `model` 等 |
| `status` | `active` / `resolved` / `rejected` / `deleted` | 否 | 默认 `active`；`deleted` 是软删除状态 |
| `reviewer` / `reviewedBy` | string? | 否 | 人工或自动 review 操作者 |
| `attrs` 或顶层 attrs 别名 | object / JSON value | 否 | `projectId`、`skill`、`mode`、`callSite` 等会保存为 attrs，用于过滤 |

**响应**：

```json
{
  "annotationId": "1",
  "tenantId": "1",
  "target": "span",
  "traceId": "12629570674344444284",
  "spanId": "12068206367433246855",
  "externalTraceId": "run-uuid",
  "externalSpanId": "span-uuid",
  "label": "best_path",
  "score": 920,
  "reason": "人工确认这次路径最短",
  "source": "human",
  "status": "active",
  "reviewer": "four",
  "createdAtNs": "1783000000000000000",
  "updatedAtNs": "1783000000000000000",
  "attrs": {
    "project_id": "agentic-data",
    "skill": "review"
  }
}
```

### PATCH /v1/annotations/:annotationId

更新 annotation 的 review 状态或业务字段。`annotationId` 是 yiTrace 返回的内部 id，不是 trace id。支持字段：`label`、`score`、`reason` / `comment` / `note`、`source` / `updatedBy`、`status`、`reviewer` / `reviewedBy`、`attrs`。`score`、`reason`、`source`、`reviewer` 传 `null` 会清空；`attrs` 默认 merge，传 `replaceAttrs:true` 会整体替换 attrs。

```bash
curl -XPATCH localhost:7878/v1/annotations/1 \
  -d '{"status":"resolved","reviewer":"four","reason":"人工确认","attrs":{"review_round":1}}'
```

响应字段同 `POST /v1/annotations`。

### DELETE /v1/annotations/:annotationId

软删除 annotation：记录会保留在 `metadata.dat` 中，状态变为 `deleted`，默认查询和 annotation 反向过滤不再命中。body 可为空，也可带 `reason` / `comment` / `note`、`reviewer` / `source`。

```bash
curl -XDELETE localhost:7878/v1/annotations/1 \
  -d '{"reviewer":"four","reason":"superseded"}'
```

### GET /v1/annotations

按 tenant、trace/span、label/source、attrs 查询 annotation。

**查询参数**：

| 参数 | 说明 |
|---|---|
| `trace_id` / `traceId` | 内部数字 id 或外部字符串 id |
| `span_id` / `spanId` | 内部数字 id 或外部字符串 id |
| `target` | `trace` / `span` |
| `label` | 精确匹配 |
| `source` | 精确匹配 |
| `status` | `active` / `resolved` / `rejected` / `deleted` 精确匹配 |
| `includeDeleted` / `include_deleted` | 不传时默认过滤掉 `deleted`；显式传 true 且不传 `status` 时返回所有状态 |
| `attrs` | URL 编码 JSON object，如 `{"project_id":"agentic-data"}` |
| `projectId` / `project_id` / `skill` / `mode` / `callSite` / `taskFingerprint` / `task_fingerprint` / `loopId` / `loop_id` / `harnessVersion` / `harness_version` / `schemaFingerprint` / `schema_fingerprint` / `intentSignature` / `intent_signature` / `validationStatus` / `validation_status` / `reviewStatus` / `review_status` / `evalStatus` / `eval_status` / `pathMemoryId` / `path_memory_id` / `stopReason` / `stop_reason` / `phase` / `validator` | attrs 字符串精确过滤便捷参数 |
| `cursor` / `offset` | offset 游标；上一页 `nextCursor` 可直接透传 |
| `limit` | 页大小，默认 50，clamp 1-500 |

响应：`{"items":[...],"count":total,"total":total,"pageCount":items.length,"nextCursor":number|null}`。结果按 `createdAtNs` 倒序排列，同一时间按 `annotationId` 倒序。默认不返回 `status="deleted"` 的记录；传 `status=deleted` 或 `includeDeleted=true` 可查看软删除记录。

### POST /v1/dataset-associations

把一条 trace/span 绑定到外部 dataset item。yiTrace 不管理 dataset item 本体，只保存可回查的 source link。

**请求体**：

```json
{
  "datasetId": "best-path-regression",
  "itemId": "case-1",
  "traceId": "run-uuid",
  "spanId": "span-uuid",
  "snapshotId": "snap-1",
  "snapshotHash": "fnv1a64:abc",
  "evalRunId": "eval-1",
  "split": "train",
  "label": "pass",
  "score": 920,
  "projectId": "agentic-data",
  "skill": "review"
}
```

| 字段 | 类型 | 必需 | 说明 |
|---|---|---|---|
| `dataset_id` / `datasetId` / `dataset` | string | 是 | 外部 dataset id |
| `item_id` / `itemId` / `datasetItemId` | string | 是 | 外部 dataset item id |
| `trace_id` / `traceId` | u64 或 string | 是 | source trace |
| `span_id` / `spanId` | u64 或 string | 否 | source span |
| `snapshotId` / `snapshotHash` | string? | 否 | 建议绑定 `GET /v1/traces/:id/snapshot` 的快照身份 |
| `evalRunId` / `split` / `label` / `score` | 可选 | 否 | 评测或训练集管理字段 |
| `attrs` 或顶层 attrs 别名 | object / JSON value | 否 | project/skill/mode/call_site/task/loop/validation 等过滤维度 |

响应字段同请求语义，额外包含 `associationId`、`tenantId`、`createdAtNs`、`externalTraceId`、`externalSpanId`。

### GET /v1/dataset-associations

按 dataset/item、trace/span、evalRunId、split、label、attrs 查询关联。别名 `/v1/dataset-links` 也可用。

**查询参数**：

| 参数 | 说明 |
|---|---|
| `dataset_id` / `datasetId` / `dataset` | 外部 dataset id |
| `item_id` / `itemId` / `datasetItemId` | 外部 dataset item id |
| `trace_id` / `traceId` | 内部数字 id 或外部字符串 id |
| `span_id` / `spanId` | 内部数字 id 或外部字符串 id |
| `evalRunId` / `eval_run_id` | 精确匹配 |
| `split` | 精确匹配 |
| `label` | 精确匹配 |
| `attrs` | URL 编码 JSON object，如 `{"project_id":"agentic-data"}` |
| `projectId` / `project_id` / `skill` / `mode` / `callSite` / `taskFingerprint` / `task_fingerprint` / `loopId` / `loop_id` / `harnessVersion` / `harness_version` / `schemaFingerprint` / `schema_fingerprint` / `intentSignature` / `intent_signature` / `validationStatus` / `validation_status` / `reviewStatus` / `review_status` / `evalStatus` / `eval_status` / `pathMemoryId` / `path_memory_id` / `stopReason` / `stop_reason` / `phase` / `validator` | attrs 字符串精确过滤便捷参数 |
| `cursor` / `offset` | offset 游标；上一页 `nextCursor` 可直接透传 |
| `limit` | 页大小，默认 50，clamp 1-500 |

响应：`{"items":[...],"count":total,"total":total,"pageCount":items.length,"nextCursor":number|null}`。结果按 `createdAtNs` 倒序排列，同一时间按 `associationId` 倒序。

---

## 控制台 API（写 Trace 浏览器用，camelCase）

这一组端点是自带控制台用的，字段面向 UI。**写自己的前端主要用这组。**

### GET /v1/sessions  —— 会话列表（游标分页）

**查询参数**：

| 参数 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `cursor` | usize | 0 | offset 游标（上一页 `nextCursor` 透传） |
| `limit` | usize | 50 | 页大小（clamp 1–500） |
| `filter` | string | 空 | 按标题 / sessionId 子串过滤（URL 编码，支持中文） |
| `attrs` | JSON object string | 空 | URL 编码后的 attrs 精确过滤，如 `{"project_id":"agentic-data","skill":"review"}` |
| `project_id` / `skill` / `mode` / `call_site` / `task_fingerprint` / `loop_id` / `harness_version` / `schema_fingerprint` / `intent_signature` / `validation_status` / `review_status` / `eval_status` / `path_memory_id` / `stop_reason` / `phase` / `validator` | string | 空 | attrs 字符串精确过滤的便捷查询参数 |
| `annotationLabel` / `annotationSource` / `annotationStatus` / `annotationScoreMin` / `annotationIncludeDeleted` | string / number / bool | 空 | 会话内任一 trace/span 有匹配 annotation 时返回该会话；deleted 默认不命中 |
| `datasetId` / `itemId` / `evalRunId` / `datasetLabel` | string | 空 | 会话内任一 trace/span 有匹配 dataset association 时返回该会话 |

attrs 语义：会话内至少一个 span 命中所有 supplied attrs 时返回该会话，返回值仍是完整 session 聚合行。

**响应**：

```json
{
  "items": [
    {
      "sessionId": "400007",
      "title": "数据分析师",
      "turnCount": 5,
      "totalCost": 0.01,
      "status": "error",
      "startedAt": 400007,
      "firstTraceId": "400035"
    }
  ],
  "nextCursor": 3,
  "total": 92
}
```

| 字段 | 类型 | 说明 |
|---|---|---|
| `sessionId` | string | 会话 id |
| `title` | string | 会话标题（取首轮 trace 名） |
| `turnCount` | u32 | 轮数（多轮会话 > 1） |
| `totalCost` | f64 | 会话合计成本（美元） |
| `status` | string | `"ok"` / `"error"` |
| `startedAt` | i64 | 起始（排序/游标用） |
| `firstTraceId` | string | 首轮 trace id（单轮直接选它） |
| `nextCursor` | string? | 下一页游标，`null`=到底 |
| `total` | usize | 总会话数 |

### GET /v1/sessions/:id/turns  —— 一个会话的各轮

**响应**：JSON 数组（按时序），每轮：

```json
{
  "traceId": "400035",
  "sessionId": "400007",
  "turnIndex": 0,
  "name": "如何修改预留手机号",
  "durMs": 7,
  "cost": 0.001,
  "costUsd": 0.0012,
  "costDetail": {
    "costUsd": 0.0012,
    "costUsdNanos": 1200000,
    "currency": "USD",
    "source": "mixed"
  },
  "usage": {
    "inputTokens": 1258,
    "outputTokens": 566,
    "cachedInputTokens": 0,
    "reasoningTokens": 0,
    "totalTokens": 1824
  },
  "inTok": 1258,
  "outTok": 566,
  "spanCount": 3,
  "status": "ok"
}
```

| 字段 | 类型 | 说明 |
|---|---|---|
| `traceId` | string | 该轮的 trace id |
| `turnIndex` | u32 | 第几轮（0 起） |
| `name` | string | 轮标题（取 user_input 截断） |
| `durMs` | u64 | 该轮总耗时毫秒 |
| `cost` | f64 | 旧兼容成本字段 |
| `costUsd` / `costDetail` | object | 标准化成本；`costDetail.source` 为 `explicit` / `estimated_model_price` / `estimated_default` / `mixed` |
| `usage` | object | 标准 token 用量，含 input/output/cached/reasoning/total |
| `inTok`/`outTok` | u64 | 输入/输出 token |
| `spanCount` | u32 | span 数 |
| `status` | string | `"ok"` / `"error"` |

### GET /v1/traces/:id  —— 一条 trace 的折叠 span（瀑布）

**响应**：

```json
{
  "summary": {
    "traceId": "400035",
    "name": "数据分析师",
    "durMs": 6,
    "cost": 0.001,
    "costUsd": 0.0012,
    "usage": {
      "inputTokens": 900,
      "outputTokens": 120,
      "cachedInputTokens": 0,
      "reasoningTokens": 0,
      "totalTokens": 1020
    },
    "spanCount": 3,
    "status": "ok"
  },
  "spans": [
    {
      "id": "400035-s0",
      "parentId": null,
      "kind": "agent",
      "name": "agent.workflow",
      "startMs": 0,
      "durMs": 6,
      "status": "ok",
      "cost": 0.001,
      "costUsd": 0.0012,
      "usage": {
        "inputTokens": 900,
        "outputTokens": 120,
        "cachedInputTokens": 0,
        "reasoningTokens": 0,
        "totalTokens": 1020
      },
      "inTok": null,
      "outTok": null,
      "model": null,
      "provider": null,
      "depth": 0,
      "logEvents": [
        {
          "eventId": "5031140639032392837",
          "ts": 120,
          "seq": 2,
          "eventType": 4,
          "messages": ["读取 package.json"],
          "attrs": {
            "call_site": "package-json"
          }
        }
      ]
    }
  ]
}
```

**`spans[]` 字段**：

| 字段 | 类型 | 说明 |
|---|---|---|
| `id` | string | span id |
| `parentId` | string? | 父 span id（null=root） |
| `kind` | string | `llm`/`tool`/`chain`/`retriever`/`agent` |
| `name` | string | span 名 |
| `startMs` | i64 | 起点（瀑布定位用） |
| `durMs` | i64 | 耗时 |
| `status` | string | `"ok"`/`error`/`run` |
| `cost` | f64 | 旧兼容成本字段 |
| `costUsd` / `costDetail` | object | 标准化成本；显式 `cost_usd` 优先，否则按 provider/model 内置价格表估算，再回退默认单价 |
| `usage` | object | 标准 token 用量 |
| `inTok`/`outTok` | u64? | token（仅 llm） |
| `model` | string? | 模型名（仅 llm） |
| `provider` | string? | 模型供应商 / 系统 |
| `depth` | u32 | 调用深度（缩进/树层级） |
| `logEvents` | object[] | span 内携带 `logs` 的原始事件，按 `ts, seq, eventId` 排序 |

> **晚物化**：本端点**不含** input/output 大文本（瀑布图不需要）。要大文本见下面的 span 详情。`startMs` 是逻辑瀑布（按 span 顺序累加，不保留真实起始时刻）。

**`logEvents[]` 字段**：

| 字段 | 类型 | 说明 |
|---|---|---|
| `eventId` | string | 确定性事件 id，来自 `hash(ext_span_id, seq, event_type)` |
| `ts` | i64 | 事件时间戳 |
| `seq` | u64 | span 内事件序号 |
| `eventType` | u8 | 事件类型，`4` 表示 Log |
| `messages` | string[] | 该事件携带的日志行 |
| `attrs` | object | 该事件携带的 attrs，保持 JSON 形态 |

`logEvents` 是给 UI 和业务接入看的原始日志事件明细。不要把日志镜像进 `attrs.event_logs`；`attrs` 只放 project / skill / mode / call_site 这类元数据和过滤标签。

### GET /v1/traces/:id/steps  —— 步骤流（每步含输入/输出）

与瀑布相反：步骤流要看每一步的输入→输出，故**在此端点一次物化大文本**。返回 `Step[]`：

```json
[
  {
    "id": "400035-s0",
    "kind": "agent",
    "name": "agent.workflow",
    "status": "ok",
    "durMs": 6,
    "inTok": 0,
    "outTok": 0,
    "model": null,
    "input": "第 1 步输入：...",
    "output": "已完成，返回观察结果并更新状态。"
  }
]
```

### GET /v1/traces/:id/spans/:spanId  —— 单个 span 的大字段（晚物化）

瀑布图里选中某个 span，单独拉它的大文本。**响应**：

```json
{
  "id": "400035-s0",
  "input": "...",
  "output": "...",
  "logEvents": [
    {
      "eventId": "5031140639032392837",
      "ts": 120,
      "seq": 2,
      "eventType": 4,
      "messages": ["读取 package.json"],
      "attrs": {
        "call_site": "package-json"
      }
    }
  ]
}
```

| 字段 | 类型 | 说明 |
|---|---|---|
| `id` | string | span id |
| `input` | string? | 输入文本（null=无） |
| `output` | string? | 输出文本（null=无） |
| `logEvents` | object[] | span 内携带 `logs` 的原始事件，字段同 `GET /v1/traces/:id` |

找不到返回 `404 {"error":"span not found"}`。

---

## 典型前端流程

写一个 Trace 浏览器（仿自带控制台）的最小流程：

1. **左栏会话列表**：`GET /v1/sessions?cursor=0&limit=50` → 滚到底用 `nextCursor` 翻页。`filter` 做标题搜索。
2. **选中会话**：`GET /v1/sessions/:id/turns` → 渲染多轮时间线（每轮一个节点）。
3. **选中某轮**：`GET /v1/traces/:traceId` → 拿 `spans[]` 渲染瀑布（`startMs`/`durMs` 定位，`depth` 缩进，`kind` 着色）。
4. **点某个 span**：`GET /v1/traces/:traceId/spans/:spanId` → 拉大文本渲染输入/输出和 `logEvents`。
5. **全局检索**：`POST /v1/search` → 命中跳到对应 trace。

---

## curl 速查

```bash
# 摄入（SDK 线格式）
curl -XPOST localhost:7878/v1/ingest -d '[{"trace_id":7,"span_id":1,"ts":1,"seq":1,"event_type":1,"ext_span_id":"7-1","status":0,"input_tokens":900,"logs":["start"]}]'

# OTLP 摄入（已埋点应用直接接）
curl -XPOST localhost:7878/v1/traces -d '{"resourceSpans":[...]}'

# trace 列表
curl localhost:7878/v1/traces

# 会话列表（游标分页）
curl "localhost:7878/v1/sessions?cursor=0&limit=50"

# 一个会话的各轮
curl localhost:7878/v1/sessions/400007/turns

# 一条 trace 的瀑布 span
curl localhost:7878/v1/traces/400035

# 中文检索 + 过滤
curl -XPOST localhost:7878/v1/search -d '{"text":"盗刷","k":10,"filter":{"agent_name":"风控","status":1}}'

# 给 trace/span 打 annotation
curl -XPOST localhost:7878/v1/annotations \
  -d '{"traceId":"run-uuid","spanId":"span-uuid","label":"best_path","score":920,"projectId":"agentic-data"}'

# 把 trace/span 关联到外部 dataset item
curl -XPOST localhost:7878/v1/dataset-associations \
  -d '{"datasetId":"best-path-regression","itemId":"case-1","traceId":"run-uuid","spanId":"span-uuid","snapshotHash":"fnv1a64:abc"}'

# 向量找相似
curl -XPOST localhost:7878/v1/search -d '{"vector":[0.1,0.2],"k":10}'

# 带鉴权 + 租户
curl -H "Authorization: Bearer secret" -H "X-Tenant-Id: 1" localhost:7878/v1/traces

# Prometheus 指标
curl localhost:7878/v1/metrics
```
