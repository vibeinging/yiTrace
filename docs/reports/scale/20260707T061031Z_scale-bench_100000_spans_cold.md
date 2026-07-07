# yiTrace Scale Bench Report

- generatedAtUnix: 1783404712
- spans: 100000
- queriesPerEndpoint: 100
- vectorQueries: 0
- queryCacheMode: cold
- vectorDim: 32
- vectorCount: 500
- dataDir: /var/folders/j6/3tm8zfn56fn47lfvwstq5y100000gp/T/yt_scale_bench_62658
- dataBytes: 214228434
- walBytes: 75618658
- segmentLikeFiles: 75

## Notes

- vector_namespace query was skipped because vectorQueries is 0; vector_index still measures vector write/build cost.
- Run with `--vector-queries N` to benchmark vector reads. Medium/large skip it by default because this path is currently a known slow path.

## Write Path

| Step | Count | Seconds | Rate |
|---|---:|---:|---:|
| ingest_wire | 100000 | 12.337 | 8106 spans/s |
| flush_memtable | 100000 | 0.116 | - |
| vector_index | 500 | 2.183 | 229 vectors/s |

## Read Path

| Query | Count | QPS | P50 ms | P95 ms | P99 ms | Max ms | Errors | Avg bytes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| search_text_attrs | 100 | 74 | 11.564 | 12.573 | 13.052 | 194.535 | 0 | 1136 |
| trace_search_attrs | 100 | 8 | 125.325 | 130.544 | 144.727 | 739.097 | 0 | 27306 |
| trace_aggregate_rollup | 100 | 364 | 2.730 | 2.959 | 3.020 | 3.838 | 0 | 7292 |
| storage_stats | 100 | 127 | 7.885 | 8.097 | 8.348 | 9.264 | 0 | 1769 |
| vector_namespace | 0 | 0 | 0.000 | 0.000 | 0.000 | 0.000 | 0 | 0 |
| trace_search_page_100 | 100 | 4 | 243.403 | 248.306 | 263.520 | 265.422 | 0 | 135447 |
