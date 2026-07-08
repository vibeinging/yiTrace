# yiTrace API Reference

> yiTrace 的主要功能都通过一套 JSON API 暴露。HTTP server 调的是 `/v1/*` 端点；Node / Python / Rust 嵌入式 DB 包也是进程内调用同一层 `EngineJsonApi`。**自带控制台前端没有特权**——它和任何第三方前端调的是同一套 API。想写自己的前端 / Dashboard / Agent 后端，照着本文即可。
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

## 这份 API 文档怎么用

yiTrace 有两类运行方式：独立服务和嵌入式。嵌入式目前有 Node / Python / Rust 三种包，但字段契约是一套：

| 方式 | 入口 | 是否走 HTTP socket | 适合谁 |
|---|---|---|---|
| 独立服务 | `cargo run -p yt-engine --example server` 后调 `/v1/*` | 是 | 自己写前端、已有服务要集中上报、接 OTLP/OpenInference |
| Node / Electron 嵌入式 | `@yitrace/db` | 否 | Node 后端、Electron main process，要像 SQLite/Chroma 一样本地打开 |
| Python 嵌入式 | `yitrace-db` / `import yitrace_db` | 否 | Python agent 或本地工具要直接写入并搜索 |
| Rust 嵌入式 | `yitrace-db` crate | 否 | Rust agent 或本地服务要直接嵌入 |

嵌入式包不是直接读文件。它们都把 Rust engine 加载到当前进程，再调用同一套 `EngineJsonApi`。
所以本文里的请求体、响应字段、`readPlan`、attrs 过滤、trace/span 详情等契约，也适用于嵌入式包的高级方法或 `route_json` 风格方法。
嵌入式只适合单进程持有一个 writer。FastAPI `uvicorn --workers 1` 可以嵌入；`uvicorn --workers N`、
多个容器、多个服务共享同一个 data dir 时，应改为一个 yiTrace server 进程，其他 worker 走 HTTP。

对外推荐顺序：

1. **只是打点上报**：优先用 `yitrace` Python SDK、`@yitrace/trace-sdk` 或 Rust `yitrace` crate。
2. **已经有 OTel/OpenInference**：直接把 OTLP/HTTP JSON 发到 `POST /v1/traces`。
3. **应用内需要本地搜索和 trace 详情**：用 `@yitrace/db`、`yitrace-db` Python 包或 Rust crate。
4. **自己写 UI / 服务**：直接调 `/v1/*`。

alpha 阶段适合早期用户接入和 dogfood。已稳定的能力包括摄入、重启恢复、search、trace/span detail、attrs 过滤、read-model rollup、annotation/dataset/retention 基座。仍在路线图里的上量项包括 attrs postings 磁盘分页、独立 loop/task/trajectory 索引、百万 span 冷/热性能基线、段内持久 BM25 倒排。

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
| **控制台 API** | `/v1/sessions`、`/v1/sessions/:id/turns`、`/v1/traces/:id`、`/v1/traces/:id/steps`、`/v1/traces/:id/spans/:sid` | **camelCase**，面向 UI（`traceId`、`durMs`） | 写 Trace 浏览器 / 瀑布 / 时间线 |

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
| `session_id` | u64 或 string? | 会话归属；字符串原文保存在 `external_session_id` |
| `tenant_id` | u64? | 租户归属；HTTP 服务会被 `X-Tenant-Id` 覆盖 |
| `agent_name`/`tool_name`/`model` | string? | 标注 |
| `input_text`/`output_text` | string? | 大文本（晚物化） |
| `logs` | string[] | 日志行 |
| `attrs` | object? | 原始/扩展属性；贯穿折叠、WAL/segment/manifest 和查询输出，同 key 后到覆盖 |

**响应**：`200 {"ingested":N}`（N=实际灌入条数）。

> **去重**：`event_id = hash(ext_span_id, seq, event_type)`，内容决定身份——重传/崩溃重放天然幂等，token/成本不重复计数。

**attrs round-trip 契约**：

- `attrs` 必须是 JSON object。
- value 可为 string / number / bool / null / array / object。
- yiTrace 会校验并保存 value 的 JSON 字面量；search、trace、span detail 返回时恢复成相同 JSON 形态。
- 同一 span 多个事件写同一个 attr key 时，后到事件覆盖先到事件。
- 当前高频 attrs 已进入读模型过滤：`project_id`、`skill`、`mode`、`call_site`、`task_fingerprint`、`loop_id`、`harness_version`、`schema_fingerprint`、`intent_signature`、`validation_status`、`review_status`、`eval_status`、`path_memory_id`、`stop_reason`、`phase`、`validator`。其他 attrs 会持久化并返回，但不保证有专门索引。

### POST /v1/traces  —— OTLP/HTTP 标准端点（生态入口 / 原始 API）

**已埋点 OTLP/OpenInference 的应用不改一行即可灌入**（OTel GenAI `gen_ai.*`、Arize `llm.*`）。请求体是标准 OTLP/HTTP JSON（`{"resourceSpans":[...]}`）。非法/缺字段返回 400。

常用属性映射：

| OTLP / OpenInference 属性 | yiTrace 字段 |
|---|---|
| `gen_ai.request.model` / `gen_ai.response.model` / `llm.model_name` | `model` |
| `gen_ai.usage.input_tokens` / `gen_ai.usage.prompt_tokens` / `llm.token_count.prompt` | `input_tokens` |
| `gen_ai.usage.output_tokens` / `gen_ai.usage.completion_tokens` / `llm.token_count.completion` | `output_tokens` |
| `gen_ai.agent.name` / `agent.name` | `agent_name` |
| `gen_ai.tool.name` / `tool.name` | `tool_name` |
| `input.value` / `gen_ai.prompt` | `input_text` |
| `output.value` / `gen_ai.completion` | `output_text` |
| `yitrace.session_id` / `session.id` / `gen_ai.conversation.id` / `session_id` | `session_id` |
| `yitrace.tenant_id` / `tenant.id` / `tenant_id` | direct ingest 的 `tenant_id`；HTTP 摄入仍由 `X-Tenant-Id` 覆盖 |

> `user.id` 只应作为业务属性处理，不会被当作 tenant。HTTP 多租户边界只认 `X-Tenant-Id`。

---

## 查询

### GET /v1/traces  —— trace 列表（原始 API，snake_case）

**查询参数**：无（租户从头取）。

**响应**：JSON 数组，每条：

```json
{
  "trace_id": 7,
  "span_count": 3,
  "total_duration_ns": 4200000,
  "max_duration_ns": 3000000,
  "error_count": 0,
  "total_input_tokens": 900,
  "total_output_tokens": 120
}
```

### GET /v1/metrics  —— Prometheus 指标

返回 Prometheus 文本格式（`# HELP` / `# TYPE` / 值），可直接被 Prometheus 抓、Grafana 出看板。指标：`yt_manifest_version`、`yt_segments_live`、`yt_memtable_rows`、`yt_segments_dead`、`yt_readers_active`、`yt_wal_committed_tail`、`yt_flush_threshold`、`yt_filter_attrs`、`yt_filter_attr_postings`、`yt_filter_attr_disabled_postings`、`yt_fold_cache_entries`、`yt_seg_bloom_count`、`yt_datasets`。

### GET /v1/healthz / GET /v1/readyz  —— 进程探针

返回 `{"ok":true}`。用于 Docker Compose / Kubernetes / 反向代理健康检查，只表示 HTTP 服务可路由，不做深度数据一致性扫描。

---

## 检索

### POST /v1/search  —— 中文检索 / 向量召回 / 混合（原始 API，snake_case）

**按给了什么自动选检索路**：

| 给了 | 走哪路 |
|---|---|
| 只 `text` | 中文 BM25 检索 |
| 只 `vector` | 向量找相似（带过滤进图） |
| 两个都给 | 混合（RRF 融合） |

**请求体**：

```json
{
  "text": "盗刷",
  "vector": [0.1, 0.2, 0.3],
  "k": 10,
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
| `vector` | f32[] | 二选一 | 查询向量（维度需与索引一致） |
| `k` | usize | 否 | 返回数，默认 10 |
| `filter.trace_id` | u64? | 否 | 限定 trace |
| `filter.agent_name` | string? | 否 | 限定 agent |
| `filter.status` | u8? | 否 | 限定状态（0=ok，非 0=error） |
| `filter.time_from`/`time_to` | i64? | 否 | 时间窗（纳秒） |
| `filter.project_id` / `filter.projectId` | JSON value? | 否 | 精确匹配 attrs.project_id |
| `filter.skill` | JSON value? | 否 | 精确匹配 attrs.skill |
| `filter.mode` | JSON value? | 否 | 精确匹配 attrs.mode |
| `filter.call_site` / `filter.callSite` | JSON value? | 否 | 精确匹配 attrs.call_site |
| `filter.attrs.project_id` / `skill` / `mode` / `call_site` | JSON value? | 否 | 与上面字段等价，适合统一传 attrs filter |

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

---

## 单机读模型 API（原始 API，camelCase 响应）

这组端点用于把 trace 数据进一步筛选、聚合和估算空间。当前实现是**单机基础版**：结果语义已经稳定；`project_id`、`skill`、`mode`、`call_site`、`task_fingerprint`、`loop_id`、`validation_status`、`review_status`、`eval_status`、`tool_name`、`model` 等常用过滤会先走 attrs sidecar 缩小候选 span，再折叠候选数据。attrs sidecar 在内存里有 postings，会选最小 postings 起步并做最终校验，避免每次过滤扫全量 span；postings 有内存预算，单个值太宽或总条目太多时会禁用对应 postings，并回退扫描 sidecar 行，保证结果不丢。没有可用索引时会回退扫描，响应里的 `readPlan` 会说明这次读到底走了哪条路径。`trace-aggregate` 的无文本聚合有 rollup 快路径；trajectory/loop/task 的无文本路径也复用同一份 span 小字段 rollup。路径类接口拿到候选 trace 后，会按 trace_id 从 rollup 只取这些 trace 的完整 span；`readPlan.traceFetchSource` 会说明这一步是否也命中 rollup。持久模式会把已 flush 的 segment 小字段写成 `trace_rollup.dat`，把过滤 sidecar 写成 `filter_attrs.dat`；重启时先加载缓存，再叠加 WAL tail。缓存不是数据源，删掉、损坏或版本不匹配都会自动扫描 segment 重建。带 `text` 的聚合和路径查询仍会回到正确扫描。把 postings 做成按需分页的磁盘 buffer manager、以及 loop/task 独立磁盘索引仍是后续优化。

### POST /v1/trace-search  —— 结构化 span 搜索

**请求体**：

```json
{
  "text": "退款审核",
  "filter": {
    "projectId": "agentic-data",
    "skill": "review",
    "validationStatus": "pass",
    "agentName": "planner",
    "status": 0
  },
  "sortBy": "duration",
  "cursor": 0,
  "limit": 50
}
```

支持的过滤：

| 字段 | 说明 |
|---|---|
| `filter.traceId` / `trace_id` | 内部数字 id 或外部字符串 id |
| `filter.spanId` / `span_id` | 内部数字 id 或外部字符串 id |
| `filter.sessionId` / `session_id` | 内部数字 id 或外部字符串 id |
| `filter.externalTraceId` / `external_trace_id` | 外部 trace id 精确匹配 |
| `filter.externalSpanId` / `external_span_id` | 外部 span id 精确匹配 |
| `filter.externalSessionId` / `external_session_id` | 外部 session id 精确匹配 |
| `filter.status` | 0=ok，非 0=error |
| `filter.agentName` / `agent_name` | agent 精确匹配 |
| `filter.toolName` / `tool_name` | tool 精确匹配 |
| `filter.model` | model 精确匹配 |
| `filter.projectId` / `project_id`、`skill`、`mode`、`callSite` / `call_site`、`taskFingerprint` / `task_fingerprint`、`validationStatus` / `validation_status` 等 | attrs 精确匹配 |
| `filter.attrs` | 任意 attrs 精确匹配 |
| `text` / `q` | 在 input/output/logs 中做 contains 过滤 |
| `sortBy` | `trace` / `duration` / `tokens` / `status` |

**响应**：

```json
{
  "items": [
    {
      "traceId": "123",
      "spanId": "1",
      "externalTraceId": "run-uuid",
      "externalSpanId": "span-uuid",
      "status": 0,
      "durationNs": 12000000,
      "inputTokens": 900,
      "outputTokens": 120,
      "agentName": "planner",
      "toolName": null,
      "model": "qwen",
      "attrs": {
        "project_id": "agentic-data",
        "skill": "review"
      }
    }
  ],
  "total": 1,
  "cursor": 0,
  "limit": 50,
  "scannedSpans": 4,
  "readPlan": {
    "source": "filter_index",
    "usedFilterIndex": true,
    "candidateSpanKeys": 12,
    "scannedSegments": 4,
    "matchedSpans": 1,
    "fallbackReason": null,
    "unsupportedAttrKeys": [],
    "traceFetchSource": null,
    "traceFetchSpanCount": null,
    "traceFetchFallbackReason": null
  }
}
```

`scannedSpans` 是兼容旧响应的字段，当前值等同于扫描段数；新代码应读取 `readPlan.scannedSegments` 和 `readPlan.matchedSpans`。如果 `source` 是 `scan`，说明本次没有可用索引，例如只有 `text` contains 过滤或只用了未知 attrs key；这时 `fallbackReason` 会给出原因。`source: "filter_index"` 表示本次先用了 attrs sidecar postings 拿候选 span key；如果某个 postings 被预算禁用，查询会用其他可用 postings 或扫描 sidecar 行后再做最终校验。持久库重启后可从 `filter_attrs.dat` 恢复 segment sidecar。聚合接口还可能返回 `source: "aggregate_rollup"`，表示本次没有折叠 trace 大字段，直接用了 rollup 聚合行；持久库重启后可从 `trace_rollup.dat` 恢复这部分 segment rollup。路径类接口还可能返回 `source: "trajectory_rollup"`，表示本次用同一份 rollup 小字段生成 trajectory/loop/task 摘要，没有读取 input/output/logs。路径类接口还会返回 `traceFetchSource` 和 `traceFetchSpanCount`：它说明拿到候选 trace 后，完整 trace 的 span 是继续从 rollup 按 trace_id 精确取，还是回退扫描。

### POST /v1/trace-aggregate  —— 对搜索结果做 groupBy

**请求体**：

```json
{
  "filter": {
    "projectId": "agentic-data"
  },
  "groupBy": ["skill", "validationStatus"],
  "limit": 20
}
```

`filter` 语义与 `/v1/trace-search` 相同。`groupBy` 支持常见字段：`projectId`、`skill`、`mode`、`callSite`、`taskFingerprint`、`validationStatus`、`status`、`agentName`、`toolName`、`model` 等；未知字段按 attrs key 处理。

无 `text` 的聚合会优先走 `aggregate_rollup`，只读取小字段、token、duration、status 和 attrs，不读取 input/output/logs。持久模式会把已进入 segment 的 rollup 写到 `trace_rollup.dat`，文件里带 manifest version 和 memtable watermark；恢复时如果匹配，就直接加载这份 segment-only 缓存，再从 WAL 叠加还没 flush 的尾部事件。请求里带 `text` 时必须检查大字段内容，所以会回到 `filter_index` 或 `scan` 路径。执行 retention 删除、segment upgrade 或重启恢复后，rollup 会按当前快照同步重建，删除行不会被算回来，补写字段也会反映到聚合结果里。`trace_rollup.dat` 可以安全删除；损坏或过期只会让下一次启动多扫一次 segment。

**响应**：

```json
{
  "items": [
    {
      "key": {
        "skill": "review",
        "validation_status": "pass"
      },
      "spanCount": 12,
      "traceCount": 8,
      "errorCount": 0,
      "durationSumNs": "42000000",
      "durationMaxNs": 12000000,
      "inputTokens": 9000,
      "outputTokens": 1200
    }
  ],
  "total": 12,
  "groupBy": ["skill", "validation_status"],
  "scannedSpans": 4,
  "readPlan": {
    "source": "aggregate_rollup",
    "usedFilterIndex": true,
    "candidateSpanKeys": 12,
    "scannedSegments": 0,
    "matchedSpans": 12,
    "fallbackReason": null,
    "unsupportedAttrKeys": [],
    "traceFetchSource": null,
    "traceFetchSpanCount": null,
    "traceFetchFallbackReason": null
  }
}
```

### POST /v1/storage-stats  —— 估算空间与数据量

**请求体**：

```json
{
  "filter": {
    "projectId": "agentic-data"
  },
  "groupBy": ["skill"]
}
```

**响应**：

```json
{
  "total": {
    "traceCount": 8,
    "spanCount": 12,
    "eventCount": 36,
    "estimatedBytes": 18240
  },
  "groups": [
    {
      "key": {
        "skill": "review"
      },
      "traceCount": 8,
      "spanCount": 12,
      "eventCount": 36,
      "estimatedBytes": 18240
    }
  ],
  "groupBy": ["skill"],
  "scannedSpans": 4,
  "readPlan": {
    "source": "filter_index",
    "usedFilterIndex": true,
    "candidateSpanKeys": 12,
    "scannedSegments": 4,
    "matchedSpans": 12,
    "fallbackReason": null,
    "unsupportedAttrKeys": [],
    "traceFetchSource": null,
    "traceFetchSpanCount": null,
    "traceFetchFallbackReason": null
  }
}
```

### POST /v1/trace-trajectories  —— trace 路径摘要

按 `/v1/trace-search` 的过滤语义先找 trace，再返回每条 trace 的完整路径摘要。无 `text` 时会优先走 `trajectory_rollup`，只用小字段生成路径摘要；持久库重启后可以复用 `trace_rollup.dat` 里的 segment rollup，再叠加 WAL tail。带 `text` 时需要检查 input/output/logs，会回到普通折叠读。后续若继续优化，是做独立的 trajectory/loop/task 磁盘索引。

```json
{
  "filter": {
    "projectId": "agentic-data",
    "taskFingerprint": "refund-v1"
  },
  "cursor": 0,
  "limit": 50
}
```

响应里每个 item 含 `summary` 和 `steps`：

```json
{
  "items": [
    {
      "summary": {
        "traceId": "123",
        "externalTraceId": "run-uuid",
        "taskFingerprint": "refund-v1",
        "loopId": "loop-1",
        "validationStatus": "pass",
        "signature": "agent|planner|||0>tool|sql.check|sql.check||0"
      },
      "steps": [
        {"index": 0, "kind": "agent", "name": "planner", "status": 0},
        {"index": 1, "kind": "tool", "name": "sql.check", "status": 0}
      ]
    }
  ],
  "total": 1,
  "cursor": 0,
  "limit": 50,
  "scannedSpans": 0,
  "readPlan": {
    "source": "trajectory_rollup",
    "usedFilterIndex": true,
    "candidateSpanKeys": 12,
    "scannedSegments": 0,
    "matchedSpans": 12,
    "fallbackReason": null,
    "unsupportedAttrKeys": [],
    "traceFetchSource": "trajectory_rollup",
    "traceFetchSpanCount": 12,
    "traceFetchFallbackReason": null
  }
}
```

### POST /v1/trajectory-groups  —— 相同路径分桶

用于找“同类问题里哪些路径反复出现”。它不判断 Best Path，只提供底座证据。

```json
{
  "filter": {
    "projectId": "agentic-data",
    "taskFingerprint": "refund-v1"
  },
  "sort": "best",
  "limit": 20
}
```

响应字段：

```json
{
  "items": [
    {
      "signature": "agent|planner|||0>tool|sql.check|sql.check||0",
      "traceCount": 12,
      "spanCount": 24,
      "successCount": 11,
      "errorCount": 1,
      "steps": [],
      "examples": []
    }
  ],
  "total": 3,
  "scannedSpans": 0,
  "readPlan": {
    "source": "trajectory_rollup",
    "usedFilterIndex": true,
    "candidateSpanKeys": 12,
    "scannedSegments": 0,
    "matchedSpans": 12,
    "fallbackReason": null,
    "unsupportedAttrKeys": [],
    "traceFetchSource": "trajectory_rollup",
    "traceFetchSpanCount": 12,
    "traceFetchFallbackReason": null
  }
}
```

### POST /v1/traces/diff  —— 两条 trace 对比

比较两条 trace 的路径、共同前缀、缺失步骤、额外步骤和粗粒度指标差异。

```json
{
  "baseTraceId": "run-a",
  "candidateTraceId": "run-b",
  "includeSteps": true
}
```

响应：

```json
{
  "sameSignature": false,
  "commonPrefix": 1,
  "left": {},
  "right": {},
  "delta": {
    "durationNs": "230000000",
    "inputTokens": -10,
    "outputTokens": 5,
    "spanCount": 1
  },
  "missingSteps": ["tool|sql.check|sql.check||0"],
  "extraSteps": ["tool|manual.review|manual.review||1"]
}
```

### GET /v1/loops  —— loop 汇总

按 `attrs.loop_id` 汇总。支持 `cursor` / `limit`，以及 `projectId`、`taskFingerprint`、`skill`、`validationStatus` 等 attrs 查询参数或 `attrs={...}`。

```bash
GET /v1/loops?projectId=agentic-data&taskFingerprint=refund-v1
```

响应含 `items`、`total`、`cursor`、`limit`、`scannedSpans` 和 `readPlan`。无 `text` 的 loop 查询会优先走 `trajectory_rollup`，用小字段汇总 loop；如果 rollup 不可用，会回退到普通折叠读。

### GET /v1/loops/:loopId  —— loop 详情

返回单个 loop 的 summary、trace trajectory 列表和 span 明细。404 表示当前租户下没有这个 loop。

响应含 `summary`、`traces`、`spans`、`scannedSpans` 和 `readPlan`。`loopId` 本身会作为索引过滤条件；无 `text` 时优先走 `trajectory_rollup`。如果详情里的完整 trace 也从 rollup 按 trace_id 精确取到，`readPlan.traceFetchSource` 会是 `trajectory_rollup`。

### GET /v1/tasks/:fingerprint/traces  —— 同类 task 的 trace 列表

按 `attrs.task_fingerprint` 查 trace。过滤是 trace 级语义：同一条 trace 里只要能共同证明这些 attrs，就返回完整 trace。

```bash
GET /v1/tasks/refund-v1/traces?validationStatus=pass&limit=20
```

响应含 `items`、`total`、`cursor`、`limit`、`scannedSpans` 和 `readPlan`。实现会先用 `task_fingerprint` 缩小候选 trace，再展开完整 trace 做最终过滤；无 `text` 时优先走 `trajectory_rollup`。展开完整 trace 时会优先按 trace_id 从 rollup 取，命中情况看 `readPlan.traceFetchSource`。

---

## 元数据 API（标注 / 数据集关联）

这组端点不改 trace/WAL/segment 格式，也不复制大字段。它是独立的轻量账本，用来记录“这条 trace/span 被人工怎么判定”“它属于哪个回归集样本”。持久模式下写入数据目录里的 `metadata.dat`。查询会先走内存 metadata postings，再对候选记录做最终校验；列表响应里的 `metadataIndex: "sidecar"` 表示这条路径已启用。

### POST /v1/annotations  —— 给 trace/span 加标注

请求体：

```json
{
  "traceId": "run-uuid",
  "spanId": "span-uuid",
  "target": "span",
  "label": "best_path",
  "score": 950,
  "reason": "human confirmed",
  "source": "qa",
  "attrs": {
    "project_id": "agentic-data",
    "skill": "review"
  }
}
```

必填：`traceId/trace_id`、`label`。`spanId/span_id` 可选；不传则默认标注整条 trace。`status` 可为 `active`、`resolved`、`rejected`、`deleted`，默认 `active`。

响应：

```json
{
  "annotationId": "1",
  "target": "span",
  "traceId": "123",
  "spanId": "456",
  "externalTraceId": "run-uuid",
  "externalSpanId": "span-uuid",
  "label": "best_path",
  "status": "active",
  "attrs": {
    "project_id": "agentic-data"
  }
}
```

### GET /v1/annotations  —— 查询标注

支持 `cursor`、`limit`、`traceId`、`spanId`、`target`、`label`、`source`、`status`、`includeDeleted=true`，以及 `projectId/skill/mode/callSite` 等 attrs 查询参数或 `attrs={...}`。

```bash
GET /v1/annotations?projectId=agentic-data&label=best_path&includeDeleted=true
```

默认不返回 `deleted` 标注；需要审计/回收站视图时显式传 `includeDeleted=true`。

### PATCH /v1/annotations/:id  —— 更新标注

请求体可包含 `label`、`score`、`reason`、`source`、`status`、`reviewer`、`attrs`。默认合并 attrs；传 `replaceAttrs=true` 时替换整份 attrs。

```json
{
  "status": "resolved",
  "reviewer": "qa",
  "attrs": {
    "mode": "eval"
  }
}
```

### DELETE /v1/annotations/:id  —— 软删除标注

不会物理删除记录，只把 `status` 改成 `deleted`，可带 `reviewer` / `reason`。

### POST /v1/dataset-associations  —— 关联外部数据集样本

请求体：

```json
{
  "datasetId": "agentic-regression",
  "itemId": "case-1",
  "traceId": "run-uuid",
  "spanId": "span-uuid",
  "split": "eval",
  "label": "pass",
  "score": 900,
  "attrs": {
    "project_id": "agentic-data",
    "skill": "review"
  }
}
```

必填：`datasetId/dataset_id`、`itemId/item_id`、`traceId/trace_id`。这只是记录关联关系，不复制 trace 内容。

### GET /v1/dataset-associations  —— 查询数据集关联

支持 `cursor`、`limit`、`datasetId`、`itemId`、`traceId`、`spanId`、`evalRunId`、`split`、`label`，以及 attrs 查询参数。

```bash
GET /v1/dataset-associations?datasetId=agentic-regression&projectId=agentic-data
```

---

## Retention API（清理计划 / 执行 / 审计 / 策略）

Retention 是显式调用的存储治理能力：先 dry-run 看会删什么，再 apply 软删除已 flush 到 segment 的行。它不会在嵌入式进程里自动启动后台删除线程。仍在 MemTable/WAL tail 的热 trace 会整条跳过，响应里返回 `skippedLiveTraceIds`，避免半条 trace 被删。

默认保护被 annotation、dataset association、snapshot、eval link、path memory 引用的 trace。audit/policy 查询会先走内存 metadata postings，再做最终校验；执行本身仍然必须显式触发。当前主线不把 Golden Path 治理作为底座能力。

### POST /v1/retention-plan  —— dry-run 清理计划

```json
{
  "filter": { "projectId": "agentic-data", "skill": "review" },
  "deleteBeforeTs": 100000,
  "protect": {
    "annotations": true,
    "datasetAssociations": true,
    "snapshots": true,
    "evalLinks": true,
    "pathMemory": true
  },
  "limit": 20
}
```

返回 `candidates`、`protected`、`deletable`、`protectedReasons` 和 `deletableTraceIds` 样本。

### POST /v1/retention/apply  —— 执行清理

请求体同 `retention-plan`，但必须提供 `deleteBeforeTs`。执行后写入一条 tenant-scoped audit。

可选参数：

| 参数 | 说明 |
|---|---|
| `compact` | `true` 时尝试把 deletion vector 物化进新段 |
| `reclaim` | compaction 后是否尝试安全回收旧段 |
| `requestedBy` / `source` | 审计来源 |
| `reason` | 审计原因 |

### GET/POST /v1/retention-audits  —— 查询清理审计

GET 支持 `cursor`、`limit`、`auditId`、`source`、`createdAfterNs`、`createdBeforeNs`。`auditId`、`tenant`、`source` 会先走 metadata postings；时间范围做最终校验。

```bash
GET /v1/retention-audits?source=nightly-retention&limit=20
```

### POST/GET /v1/retention-policies  —— 保存和查询清理策略

策略只保存和显式触发，不自动后台运行。GET 查询支持 `policyId`、`name`、`enabled`，并会先走 metadata postings。

```json
{
  "name": "nightly-retention",
  "intervalNs": 86400000000000,
  "nextRunAtNs": 1,
  "query": {
    "filter": { "projectId": "agentic-data" },
    "olderThanNs": 2592000000000000,
    "protect": { "annotations": true }
  },
  "source": "cron",
  "reason": "ttl"
}
```

`query` 必须包含 `deleteBeforeTs`，或 `olderThanNs` / `ttlNs` / `retentionNs` 这类 TTL 字段。run-due 时会按当前 `nowNs` 转成 `deleteBeforeTs`。

### POST /v1/retention-policies/run-due  —— 显式运行到期策略

```json
{
  "nowNs": 2,
  "limit": 10
}
```

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
| `project_id` / `skill` / `mode` / `call_site` | string | 空 | attrs 字符串精确过滤的便捷查询参数 |

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
| `cost` | f64 | 该轮成本 |
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
      "inTok": null,
      "outTok": null,
      "model": null,
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
| `cost` | f64 | 成本 |
| `inTok`/`outTok` | u64? | token（仅 llm） |
| `model` | string? | 模型名（仅 llm） |
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

# 向量找相似
curl -XPOST localhost:7878/v1/search -d '{"vector":[0.1,0.2],"k":10}'

# 带鉴权 + 租户
curl -H "Authorization: Bearer secret" -H "X-Tenant-Id: 1" localhost:7878/v1/traces

# Prometheus 指标
curl localhost:7878/v1/metrics
```
