# yiTrace Scale Bench Report

- generatedAtUnix: 1783396522
- spans: 100000
- queriesPerEndpoint: 100
- vectorQueries: 0
- queryCacheMode: warm
- vectorDim: 32
- vectorCount: 500
- dataDir: /var/folders/j6/3tm8zfn56fn47lfvwstq5y100000gp/T/yt_scale_bench_19426
- dataBytes: 214228434
- walBytes: 75618658
- segmentLikeFiles: 75

## Notes

- vector_namespace query was skipped because vectorQueries is 0; vector_index still measures vector write/build cost.
- Run with `--vector-queries N` to benchmark vector reads. Medium/large skip it by default because this path is currently a known slow path.

## Write Path

| Step | Count | Seconds | Rate |
|---|---:|---:|---:|
| ingest_wire | 100000 | 8.402 | 11901 spans/s |
| flush_memtable | 100000 | 0.043 | - |
| vector_index | 500 | 2.053 | 244 vectors/s |

## Read Path

| Query | Count | QPS | P50 ms | P95 ms | P99 ms | Max ms | Errors | Avg bytes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| search_text_attrs | 100 | 72 | 11.546 | 13.890 | 15.381 | 230.963 | 0 | 1136 |
| trace_search_attrs | 100 | 134 | 0.009 | 0.018 | 0.088 | 743.956 | 0 | 27305 |
| trace_aggregate_rollup | 100 | 294 | 0.006 | 0.008 | 0.024 | 338.907 | 0 | 7183 |
| storage_stats | 100 | 280 | 0.001 | 0.001 | 0.014 | 357.315 | 0 | 1674 |
| vector_namespace | 0 | 0 | 0.000 | 0.000 | 0.000 | 0.000 | 0 | 0 |
| trace_search_page_100 | 100 | 374 | 0.025 | 0.032 | 0.430 | 264.525 | 0 | 135446 |
