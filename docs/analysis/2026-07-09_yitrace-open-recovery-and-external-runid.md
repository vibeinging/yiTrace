# yiTrace open/recovery 与外部 runId 查询优化

日期：2026-07-09

## 结论

客户反馈的问题成立，本次已按 P0 修复三条主路径：

1. `YiTraceDB.open()` / `recover()` 命中 `trace_rollup.dat`、`filter_attrs.dat`、`bm25.dat`、`segment_bloom.dat` 时，不再同步扫历史 segment。
2. `traceSearch` 只按外部 runId / attrs 查询、且没有全文 `text` 时，直接走 rollup 缓存，不再为了折叠结果触碰 segment。
3. 第一次 `search_text` / hybrid 不再因为补建 BM25 和 segment bloom 扫历史 segment；只有缓存缺失或过期时才降级补扫。

外部传入的 id 也已统一按业务主键处理：`traceSearch({ filter: { traceId: "business-run-id" } })`、`traceSearch({ filter: { traceId: 123456 } })` 和 `traceSearch({ filter: { externalTraceId } })` 都按 external trace id 语义查询。

## 客户现场信号

日志：

```text
recover_start version=422
trace_rollup_cache_load rows=32977
filter_attrs_cache_load rows=32977 postings=262346
recover_done segs_scanned=422
```

通俗解释：

- `version=422` / `segs_scanned=422` 表示当前 manifest 里有 422 个 live segment。
- `trace_rollup_cache_load` 和 `filter_attrs_cache_load` 表示 rollup/filter 两份缓存命中了。
- 修复前：即使两份缓存命中，open 流程仍会逐段扫 segment，重建 BM25、session index、segment bloom 等易失索引。
- 这个开销发生在 `YiTraceDB.open()` / recovery 阶段，不是前端接口查询本身。

修复后，四份缓存命中且 manifest 中没有 delete / upgrade 脏段时，recover 只加载缓存和 WAL tail：

```text
trace_rollup_cache_load ...
filter_attrs_cache_load ...
bm25_cache_load ...
segment_bloom_cache_load ...
recover_fast_ready segments_deferred=422
recover_done segs_scanned=0 ...
```

如果 `bm25.dat` 或 `segment_bloom.dat` 缺失，则 open 仍然先快速可用，首次全文检索再扫描历史 segment，并输出：

```text
segment_scan_indexes_rebuild_done segs_scanned=422
```

如果四份缓存都命中，`segment_scan_indexes_stale=false`，首次全文检索直接使用持久 BM25 和 bloom，不会补扫。

## 已处理项

### P0：已有库 open 不再因缓存命中场景全量扫段

代码行为：

- `rebuild_volatile_from_current_locked()` 先尝试加载 `trace_rollup.dat`、`filter_attrs.dat`、`bm25.dat`、`segment_bloom.dat`。
- `trace_rollup.dat` 和 `filter_attrs.dat` 命中，且 segment 没有 delete / upgrade 派生脏状态时，直接进入 fast-ready。
- fast-ready 阶段只重放 WAL tail，保证最近未 flush 的数据仍可见。
- BM25 / bloom 缓存也命中时，`segment_scan_indexes_stale=false`，全文检索直接可用。
- 只有 BM25 / bloom 缓存缺失时，段扫描派生索引通过 `segment_scan_indexes_stale` 延后：
  - `search_text` / hybrid 搜索首次使用时调用 `ensure_segment_scan_indexes_current()` 补建 BM25 / segment bloom。
  - 纯 external id / attrs 的 trace-search 走 rollup，不触发补建。

验收测试：

- `durable_recover_defers_segment_scan_when_read_model_caches_hit`
- `multiprocess_embedded` 中多进程 reopen 场景可见 `recover_done segs_scanned=0`
- `cargo test --offline --manifest-path yitrace-engine/Cargo.toml`

边界：

- 缓存缺失、缓存版本不匹配、或 manifest 内存在 delete / upgrade 脏段时，仍会同步扫描 segment 重建缓存。这是正确性优先的降级路径。
- 只有 `bm25.dat` 或 `segment_bloom.dat` 缺失时，第一次全文检索才会触发 segment 扫描；这不阻塞 open，也不影响只按 external runId 查存在性的工作台路径。

### P0：持久化 BM25 与 segment bloom，解决第一次全文检索慢

代码行为：

- 默认 `Bm25TextIndex` 支持 `load_cache()` / `save_cache()`，落盘文件为 `bm25.dat`。
- `segment_bloom.dat` 保存每个 live segment 的 key bloom，用于全文检索候选 key join 时跳过无关段。
- 两份缓存都绑定 manifest version 和 memtable watermark；不匹配、损坏或 segment 集合不一致时直接丢弃并重建。
- WAL tail 仍从 WAL 重放后叠加进 BM25，所以 flush 后未进入 segment 的新数据不会漏。
- delete、retention apply、upgrade 会按当前快照重建 BM25 / bloom，避免已删除或补写前的 span 留在倒排里。

验收测试：

- `bm25_cache_roundtrip_preserves_results`
- `durable_recover_defers_segment_scan_when_read_model_caches_hit`
- `cargo test --offline --manifest-path yitrace-engine/Cargo.toml`

### P0：按 external runId 查不存在必须快

`traceSearch` 已有 `externalTraceId` 字段，但之前只作为最终过滤条件，未进入 `SearchFilter` 和 `filter_attrs.dat` postings。结果是：

- `traceSearch({ filter: { traceId: runId } })` 可能查的是内部 hash 后的数字 id。
- `traceSearch({ filter: { externalTraceId: runId } })` 语义正确，但不存在时可能仍触发慢路径。

本次修复：

- 外部 id 都按业务主键保留，不管传入的是 JSON string 还是 JSON number。
- `SearchFilter` 增加 `external_trace_id`。
- `FilterAttrsIndex` 存储并索引 `external_trace_id` postings。
- `trace-search` / `search` 解析 `traceId`、`externalTraceId` 和 `external_trace_id`。
- 不存在的 external trace id 直接得到空候选，`candidateSpanKeys=0`。
- 无全文 `text` 的 `trace-search` 优先走 `trace_aggregate_rollup_spans()`，读计划来源为 `trajectory_rollup`，`scannedSegments=0`。

## 后续需求

### P1：提供按 external runId 直接读完整 trace 的快接口

当前上层仍容易自己拼：

- `traceTrajectories`
- `traceSearch`
- 再按 trace id 读详情

建议提供单一接口：

```ts
await db.traceByExternalId("run-uuid", { tenantId });
```

或 HTTP：

```http
GET /v1/traces/by-external-id/{externalTraceId}
```

要求：

- 先走 external id postings 找内部 `trace_id`。
- 命中后直接走 rollup / trace detail 快路径。
- miss 直接返回 404 或 `{ found: false }`。

### P1：提供 recovery 进度/状态 API

上层需要知道“还在恢复”，而不是只能等 query 超时。

建议 Node API：

```ts
await db.recoveryStatus()
```

建议返回：

```json
{
  "state": "recovering",
  "phase": "build_segment_bloom",
  "manifestVersion": 422,
  "segmentsTotal": 422,
  "segmentsScanned": 120,
  "traceRollupCache": "hit",
  "filterAttrsCache": "hit",
  "startedAtMs": 1752060000000,
  "elapsedMs": 4200
}
```

## 验收标准

- `YiTraceDB.open()` 在缓存命中、无 delete / upgrade 脏段时，`recover_done.segs_scanned=0`。
- `traceSearch({ filter: { externalTraceId }, limit: 1 })` 命中或未命中都走 rollup / indexed sidecar，`scannedSegments=0`。
- `traceSearch({ filter: { traceId: "missing-business-run-id" }, limit: 1 })` 或 `traceSearch({ filter: { externalTraceId: "missing-business-run-id" }, limit: 1 })` 返回：
  - `total=0`
  - `readPlan.usedFilterIndex=true` 或 `readPlan.source=trajectory_rollup`
  - `readPlan.candidateSpanKeys=0`
  - 单次耗时几十毫秒级
- `traceByExternalId` 命中时不要求上层拼多个接口。
- recovery 状态 API 能区分 `idle` / `recovering` / `ready` / `degraded`，并给出当前 phase 和进度。
