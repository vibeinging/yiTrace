# yiTrace Scale Bench Report

- generatedAtUnix: 1783405561
- spans: 100000
- queriesPerEndpoint: 100
- vectorQueries: 0
- queryCacheMode: cold
- vectorDim: 32
- vectorCount: 500
- dataDir: /var/folders/j6/3tm8zfn56fn47lfvwstq5y100000gp/T/yt_scale_bench_20141
- dataBytes: 214228434
- walBytes: 75618658
- segmentLikeFiles: 75

## Notes

- vector_namespace query was skipped because vectorQueries is 0; vector_index still measures vector write/build cost.
- Run with `--vector-queries N` to benchmark vector reads. Medium/large skip it by default because this path is currently a known slow path.

## Write Path

| Step | Count | Seconds | Rate |
|---|---:|---:|---:|
| ingest_wire | 100000 | 12.362 | 8089 spans/s |
| flush_memtable | 100000 | 0.121 | - |
| vector_index | 500 | 2.227 | 224 vectors/s |

## Read Path

| Query | Count | QPS | P50 ms | P95 ms | P99 ms | Max ms | Errors | Avg bytes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| search_text_attrs | 100 | 76 | 11.212 | 12.091 | 12.346 | 200.518 | 0 | 1136 |
| trace_search_attrs | 100 | 4 | 252.660 | 272.112 | 279.299 | 319.029 | 0 | 27650 |
| trace_aggregate_rollup | 100 | 355 | 2.785 | 3.130 | 3.375 | 3.379 | 0 | 7292 |
| storage_stats | 100 | 125 | 7.948 | 8.276 | 8.470 | 8.756 | 0 | 1769 |
| vector_namespace | 0 | 0 | 0.000 | 0.000 | 0.000 | 0.000 | 0 | 0 |
| trace_search_page_100 | 100 | 3 | 286.535 | 303.427 | 319.438 | 359.550 | 0 | 135792 |
