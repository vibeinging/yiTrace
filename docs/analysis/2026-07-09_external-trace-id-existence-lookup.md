# externalTraceId 存在性查询分析

## 结论

问题存在。

`db.trace(runId)` 会把字符串 `runId` hash 成内部 `trace_id` 后读详情。它能读回由同一个字符串写入的 trace，但语义上不是按原始 UUID 字段查。`traceSearch({ filter: { traceId: runId } })` 也会走同一套内部 id 逻辑。

原始 UUID 保存在 `external_trace_id` / `externalTraceId`。之前 `traceSearch({ filter: { externalTraceId: runId } })` 会查准，但它只是最终过滤条件，没有进入 filter sidecar postings。结果是：第一次负查仍可能扫很多 span，worker 里再做负缓存只能避免重复慢查，不能解决第一次慢查。

## 修改

- `SearchFilter` 增加 `external_trace_id`。
- `filter_attrs.dat` 的 sidecar 行增加 `external_trace_id`，并为它建立 postings。
- `/v1/trace-search` 解析 `externalTraceId` / `external_trace_id` 后，把它放进索引过滤条件。
- `/v1/search` 也接受 `externalTraceId` / `external_trace_id`。
- `filter_attrs.dat` cache 版本从 1 升到 2。旧 cache 会失效并自动从当前 snapshot 重建。

## worker 推荐用法

存在性探测用：

```js
const page = await db.traceSearch({
  filter: { externalTraceId: runId },
  limit: 1,
});
const exists = page.total > 0;
```

负缓存可以缓存 `exists === false` 的 `runId`。新实现下，没有命中的 run 会返回 `readPlan.usedFilterIndex: true` 和 `readPlan.candidateSpanKeys: 0`，不需要先走慢详情读。

如果后续要拿完整瀑布，再用 `db.trace(runId)` 读详情即可。

## 验证

- `cargo test --offline --manifest-path yitrace-engine/Cargo.toml`：修改前基线通过。
- `cargo test --offline --manifest-path yitrace-engine/Cargo.toml -p yt-engine --lib external_trace_id_posting_supports_fast_positive_and_negative_lookup`
- `cargo test --offline --manifest-path yitrace-engine/Cargo.toml -p yt-engine --lib route_ingest_accepts_external_ids_and_attrs`
- `cargo test --offline --manifest-path yitrace-engine/Cargo.toml -p yt-engine --lib`
- `cd yitrace-node && npm run build && npm test`
- `cargo test --offline --manifest-path yitrace-engine/Cargo.toml`：修改后全量通过。

中间有一次 `cargo test ... -p yt-engine external_trace_id...` 触发 macOS `clang` 链接器 `Segmentation fault: 11`。缩小到 `--lib` 后通过，最后全量复跑也通过，判断是本机链接器偶发问题，不是代码错误。
