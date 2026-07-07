# yiTrace Scale Bench Report

- generatedAtUnix: 1783405791
- spans: 100000
- queriesPerEndpoint: 100
- vectorQueries: 0
- queryCacheMode: cold
- vectorDim: 32
- vectorCount: 500
- dataDir: /var/folders/j6/3tm8zfn56fn47lfvwstq5y100000gp/T/yt_scale_bench_25721
- dataBytes: 214228434
- walBytes: 75618658
- segmentLikeFiles: 75

## Notes

- vector_namespace query was skipped because vectorQueries is 0; vector_index still measures vector write/build cost.
- Run with `--vector-queries N` to benchmark vector reads. Medium/large skip it by default because this path is currently a known slow path.

## Write Path

| Step | Count | Seconds | Rate |
|---|---:|---:|---:|
| ingest_wire | 100000 | 12.701 | 7873 spans/s |
| flush_memtable | 100000 | 0.123 | - |
| vector_index | 500 | 2.209 | 226 vectors/s |

## Read Path

| Query | Count | QPS | P50 ms | P95 ms | P99 ms | Max ms | Errors | Avg bytes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| search_text_attrs | 100 | 74 | 11.598 | 12.368 | 12.543 | 213.912 | 0 | 1136 |
| trace_search_attrs | 100 | 28 | 35.187 | 36.397 | 36.790 | 83.045 | 0 | 27650 |
| trace_aggregate_rollup | 100 | 346 | 2.822 | 3.273 | 3.364 | 3.584 | 0 | 7292 |
| storage_stats | 100 | 125 | 7.968 | 8.374 | 8.487 | 8.719 | 0 | 1769 |
| vector_namespace | 0 | 0 | 0.000 | 0.000 | 0.000 | 0.000 | 0 | 0 |
| trace_search_page_100 | 100 | 32 | 30.013 | 31.003 | 31.873 | 111.495 | 0 | 135792 |
