# yiTrace 摄入层（写路径 / 双轨归一）

> 日期：2026-06-17｜配套 `2026-06-16_tracevault-schema.md`（表）+ `2026-06-17_tracevault-background-scheduling.md`（调度）。
> 范围：事件如何从多协议进入 `span_events`，归一、事务写入、接上 `fold_dirty`/`frozen_registry`/payload CAS、幂等、背压。纯应用层。

## 1. 架构
```
   接收器(Receivers)                  归一(Normalizer)          写路径(Writer, 事务)
 ┌──────────────────────┐      ┌────────────────────┐     ┌──────────────────────────┐
 │ 轨A 自家 SDK(原生)     │─────►│ 协议方言 → 内部统一  │────►│ 大payload→CAS(sha256)     │
 │  AgenticData          │      │ 事件模型 UnifiedEvent│     │ ↓ 同一事务:                │
 │  SuperAgent/ReAct/tool│      │  · ID 归一            │     │ INSERT span_events         │
 ├──────────────────────┤      │  · event_type 推导   │     │ INSERT fold_dirty(脏队列)  │
 │ 轨B 框架无关           │      │  · span_kind 映射    │     │ payload upsert refcount+1  │
 │  OTLP gRPC/HTTP(OTel) │      │  · gen_ai.*↔run_type │     │ 命中 frozen_registry→inbox │
 │  OpenInference        │      │  · raw_attrs 无损兜底│     │ is_sampled 采样标记        │
 │  LangSmith-compat REST│      └────────────────────┘     │ 组提交 → ack(低延迟持久)   │
 └──────────────────────┘                                  └──────────────────────────┘
   每租户令牌桶限速(超→429)        版本化可插拔(OTel semconv 仍 Development)
```

## 2. 双轨

### 轨 A — 自家 AgenticData 深集成（富语义，你们 100% 可控）
原生埋点 SuperAgent/ReAct/tool_call 内核，捕获框架无关轨拿不到的 `vex.*` 富字段：
| vex.* 字段 | 含义 |
|---|---|
| `vex.thought` | ReAct 思考步原文 |
| `vex.decision` | 决策点（选了哪个工具/分支 + 理由） |
| `vex.tool.intent` | 调用工具的意图（非仅入参） |
| `vex.reflection` | 反思/自纠 |
| `vex.embedding_ref` | 旁路截获的已有 embedding（省一次算） |
→ 喂语义召回/飞轮。轨 A 的 SDK 直接产 UnifiedEvent，跳过方言解析。

### 轨 B — 框架无关（"指 endpoint 就迁过来"）
| 协议 | 接什么 | 现成 instrumentor |
|---|---|---|
| **OTLP gRPC/HTTP + OTel GenAI semconv** | `gen_ai.*` span | OpenLLMetry/Traceloop |
| **OpenInference** | `openinference.*` 属性方言 | Arize instrumentors |
| **LangSmith-compat REST** | `/runs`,`/runs/multipart`,`dotted_order`,attachment | LangChain/LangGraph 原生 |
认证 4 框架（LangChain/LangGraph/LlamaIndex/Dify）+ 钉版多版本矩阵 CI；其余 best-effort + raw_attrs 无损。

## 3. 统一事件模型 UnifiedEvent → span_events 行

```
UnifiedEvent {
  tenant_id, ext_trace_id(原始), ext_span_id, ext_parent_id,   // 外部原始 ID(string/bytes)
  seq,                          // 生产端单调(同 span 内), 无则按 ingest 顺序补
  lifecycle,                    // START|UPDATE|END|TOOL_RESULT|ERROR|FEEDBACK
  span_kind_raw, name, model,
  start_time, end_time, ts,
  tokens{in,out}, cost, latency_ms,
  inputs, outputs,             // 可能很大 → 抽 CAS
  attrs,                       // 半结构化, 进 attrs_patch
  raw,                         // 整条原始, 无损进 raw_attrs
}
```

### 3.1 event_type 推导（各协议生命周期 → 内部枚举）
| 来源信号 | event_type |
|---|---|
| OTel span start / LangSmith run create / 轨A start | 1 START |
| 中间 update / 流式 partial | 2 UPDATE |
| span end / run patch end / status=ok | 3 END |
| tool span 完成 / tool_result | 4 TOOL_RESULT |
| status=error / exception event | 5 ERROR |
| feedback/score 回写 | 6 FEEDBACK |

### 3.2 span_kind 映射（归一枚举）
`llm/chat→1, chain→2, tool→3, retriever→4, embedding→5, agent/workflow→6, prompt/parser→7`；OTel `gen_ai.operation.name`、OpenInference `openinference.span.kind`、LangSmith `run_type` 各自映射，原值留 `raw_attrs`。

### 3.3 ID 归一（外部 string/bytes → 内部 bigint）
schema 用 bigint trace_id/span_id。策略：
- **内部 id = `xxhash64(ext_id) & 0x7FFF...`（63位正 bigint）**，确定性 → 同一外部 trace 多次/多事件映射稳定。
- 原始 ext_id 无损存 `raw_attrs`（round-trip 回 LangSmith/OTel）。
- **碰撞保险**：可选 `id_map(tenant_id, ext_id_hash, ext_id_full, internal_id)` 唯一约束，hash 碰撞时换槽；中小规模碰撞概率可忽略，金融政企严谨档启用。
- `dotted_order`（LangSmith）作一等列保留（schema 已有）。
- **event_id = 提交单调雪花**（节点位+单调时钟+序列，非 SEQUENCE CACHE —— 调度层正确性硬前置）。

## 4. 写路径（事务，接上调度脏队列）

```sql
-- 0) 限速(应用层令牌桶, 超→抛 429 retry-after, 见调度§8)
-- 1) 大 payload 抽离到 CAS(同一逻辑作业, 可在主事务前): 命中去重 refcount+1, 否则插入
--    base64 data URI / >阈值文本 → sha256 → payload_ref; 多模态走 external_uri
-- 2) 主事务(组提交): 三写原子 —— 这是调度层"事务不变量"的根
BEGIN;
  -- 幂等: 同一逻辑事件(ext_span_id, seq, event_type)重试不重复(确定性 event_id + 去重)
  INSERT INTO span_events
    (event_id, tenant_id, trace_id, span_id, seq, event_type, ts, parent_span_id,
     span_kind, name, model, input_tokens, output_tokens, total_cost, latency_ms,
     status, start_time, end_time, attrs_patch, payload_ref, ingest_ts)
  VALUES (:eid, :t, :tr, :sp, :seq, :etype, :ts, :par, :kind, :name, :model,
     :itok, :otok, :cost, :lat, :st, :start, :end, :patch, :pref, now())
  ON DUPLICATE KEY UPDATE event_id = span_events.event_id;   -- [改] 重试幂等(PK=(ts,event_id)确定性), 不重复折叠
  -- 关键: 同一事务写脏队列 → "看到所有已提交事件"成事务不变量(调度层折叠靠它, 不靠时间水位)
  INSERT INTO fold_dirty (tenant_id, trace_id, span_id)
  VALUES (:t, :tr, :sp) ON DUPLICATE KEY UPDATE enqueued_at = now();
COMMIT;
-- 3) 提交后: 若该 trace 已 frozen(查 frozen_registry) → 该事件改路由进 late_event_inbox + 标 late_pending
--    (摄入对 frozen trace 取 advisory 锁, 与时间分区 GC 删分区串行, 堵 TOCTOU, 见调度§5)
-- 4) is_sampled: 折叠环写 span_current 时按 should_sample 打标(root/llm/error), 此处不阻塞热路径
```

### 4.1 幂等 / 去重（外部系统会重试）
- **确定性 event_id**：`event_id = snowflake_or_hash(ext_span_id, seq, event_type)` → 重试产同 id → PK 冲突 `ON DUPLICATE KEY UPDATE`（no-op）天然去重，重试不会让 fold 把 token/cost 重复累加。
- 流式 UPDATE（同 span 多次 partial）用递增 seq 区分，非重复。

### 4.2 frozen trace 路由（接调度晚到回流）
```sql
-- 摄入归一后, 提交前查(或提交后补偿):
IF EXISTS(SELECT 1 FROM frozen_registry WHERE tenant_id=:t AND trace_id=:tr) THEN
  -- 取 FREEZE advisory 锁(与 GC 删分区串行) → 事件进 inbox, 不进热分区
  INSERT INTO late_event_inbox (...同 span_events 列...) VALUES (...);
  INSERT INTO remelt_jobs(tenant_id,trace_id) VALUES(:t,:tr) ON DUPLICATE KEY UPDATE state=0;
END IF;
```

## 5. 背压（接调度§8）
per-tenant 令牌桶 → 超限 `429 + Retry-After`；折叠 lag 高时反压调低令牌桶速率（折叠 query_dop=1 单线程不能加并行追赶，只能降摄入）。embedding 旁路队列积压三档降级（正常/调低采样/只 root+error）。

## 6. OTel semconv 版本追随（持续维护项，非一次性）
- OTel GenAI semconv 仍 **Development**（字段会 breaking）→ 归一层**版本化可插拔**：`gen_ai.*` 各版本一个 mapper，`OTEL_SEMCONV_STABILITY_OPT_IN` 双发兼容。
- 合同边界：**只承诺已认证框架的已认证版本区间 100% 保真；其余 raw_attrs 无损保留但字段映射 best-effort**（避免"客户升级 LangChain 后字段丢失"变现场 P1）。
- 多版本矩阵 CI：上游新版自动跑回归 + 告警。

## 7. 待核实 [验]
1. `INSERT ... ON DUPLICATE KEY UPDATE` 用作幂等去重的精确语义（PK=(ts,event_id)，event_id 确定性）。
2. 内部 bigint id 用 xxhash64 截断 vs 维护 id_map 的取舍（碰撞概率 × 严谨档需求）。
3. OpenLLMetry/OpenInference instrumentor 对 Dify（无一线 instrumentor，节点语义需自研）的覆盖 → 可能降级 v2。
4. 大 payload 抽离阈值、SHA256 去重命中率（用真实 trace 标定）。

> 一句话：摄入 = 双轨（自家富语义 vex.* + 框架无关 OTel/OpenInference/LangSmith-compat）→ 归一成 UnifiedEvent → **同一事务写 span_events + fold_dirty**（这是调度层"折叠靠事务不变量不靠时间水位"的根）+ 大 payload 走 CAS + frozen trace 路由 inbox + 确定性 event_id 幂等去重 + 令牌桶背压。
