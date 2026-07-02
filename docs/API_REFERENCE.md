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
- 路径参数 `:id` / `:spanId` 是数字（内部 trace_id / span_id 是 `u64`）。

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
| 400 | 请求体非法（JSON 解析失败 / 缺字段 / id 不是数字） |
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
    "trace_id": 7,
    "span_id": 1,
    "ts": 1,
    "seq": 1,
    "event_type": 1,
    "ext_span_id": "7-1",
    "status": 0,
    "duration_ns": null,
    "input_tokens": 900,
    "output_tokens": null,
    "session_id": null,
    "tenant_id": null,
    "agent_name": "风控",
    "tool_name": null,
    "model": null,
    "input_text": null,
    "output_text": null,
    "logs": ["开始"]
  }
]
```

| 字段 | 类型 | 说明 |
|---|---|---|
| `trace_id` | u64 | trace 内部 id |
| `span_id` | u64 | span 内部 id |
| `ts` | i64 | 纳秒时间戳 |
| `seq` | u32 | 同一 span 内的事件序号（去重键的一部分） |
| `event_type` | u8 | **1=SpanStart，2=SpanEnd**，3+=属性补写/日志 |
| `ext_span_id` | string | span 外部身份（去重键的一部分） |
| `parent_span_id` | u64? | 父 span（建树） |
| `status` | u8? | 0=ok，非 0=error（SpanEnd 时给） |
| `duration_ns` | u64? | 耗时纳秒（SpanEnd 时给） |
| `input_tokens`/`output_tokens` | u64? | token 计数 |
| `session_id`/`tenant_id` | u64? | 会话/租户归属 |
| `agent_name`/`tool_name`/`model` | string? | 标注 |
| `input_text`/`output_text` | string? | 大文本（晚物化） |
| `logs` | string[] | 日志行 |

**响应**：`200 {"ingested":N}`（N=实际灌入条数）。

> **去重**：`event_id = hash(ext_span_id, seq, event_type)`，内容决定身份——重传/崩溃重放天然幂等，token/成本不重复计数。

### POST /v1/traces  —— OTLP/HTTP 标准端点（生态入口 / 原始 API）

**已埋点 OTLP/OpenInference 的应用不改一行即可灌入**（OTel GenAI `gen_ai.*`、Arize `llm.*`）。请求体是标准 OTLP/HTTP JSON（`{"resourceSpans":[...]}`）。非法/缺字段返回 400。

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

返回 Prometheus 文本格式（`# HELP` / `# TYPE` / 值），可直接被 Prometheus 抓、Grafana 出看板。指标：`yt_manifest_version`、`yt_segments_live`、`yt_memtable_rows`、`yt_segments_dead`、`yt_readers_active`、`yt_wal_committed_tail`、`yt_flush_threshold`、`yt_filter_attrs`、`yt_fold_cache_entries`、`yt_seg_bloom_count`、`yt_datasets`。

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
    "time_to": 5000
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

> `filter.tenant_id` **不能在请求体里指定**——强制取 `X-Tenant-Id` 头。

**响应**：JSON 数组（按 score 降序），每条命中：

```json
{
  "trace_id": 7,
  "span_id": 1,
  "score": 3.2720,
  "status": 0,
  "duration_ns": 4200000,
  "agent_name": "风控研判",
  "logs": ["研判结论 ..."]
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
      "depth": 0
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

> **晚物化**：本端点**不含** input/output 大文本（瀑布图不需要）。要大文本见下面的 span 详情。`startMs` 是逻辑瀑布（按 span 顺序累加，不保留真实起始时刻）。

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
  "output": "..."
}
```

| 字段 | 类型 | 说明 |
|---|---|---|
| `id` | string | span id |
| `input` | string? | 输入文本（null=无） |
| `output` | string? | 输出文本（null=无） |

找不到返回 `404 {"error":"span not found"}`。

---

## 典型前端流程

写一个 Trace 浏览器（仿自带控制台）的最小流程：

1. **左栏会话列表**：`GET /v1/sessions?cursor=0&limit=50` → 滚到底用 `nextCursor` 翻页。`filter` 做标题搜索。
2. **选中会话**：`GET /v1/sessions/:id/turns` → 渲染多轮时间线（每轮一个节点）。
3. **选中某轮**：`GET /v1/traces/:traceId` → 拿 `spans[]` 渲染瀑布（`startMs`/`durMs` 定位，`depth` 缩进，`kind` 着色）。
4. **点某个 span**：`GET /v1/traces/:traceId/spans/:spanId` → 拉大文本渲染输入/输出。
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
