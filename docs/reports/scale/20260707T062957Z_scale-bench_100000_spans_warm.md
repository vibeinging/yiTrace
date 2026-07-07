# yiTrace Scale Bench Report

- generatedAtUnix: 1783405814
- spans: 100000
- queriesPerEndpoint: 100
- vectorQueries: 0
- queryCacheMode: warm
- vectorDim: 32
- vectorCount: 500
- dataDir: /var/folders/j6/3tm8zfn56fn47lfvwstq5y100000gp/T/yt_scale_bench_26359
- dataBytes: 214228434
- walBytes: 75618658
- segmentLikeFiles: 75

## Notes

- vector_namespace query was skipped because vectorQueries is 0; vector_index still measures vector write/build cost.
- Run with `--vector-queries N` to benchmark vector reads. Medium/large skip it by default because this path is currently a known slow path.

## Write Path

| Step | Count | Seconds | Rate |
|---|---:|---:|---:|
| ingest_wire | 100000 | 12.371 | 8083 spans/s |
| flush_memtable | 100000 | 0.120 | - |
| vector_index | 500 | 2.243 | 223 vectors/s |

## Read Path

| Query | Count | QPS | P50 ms | P95 ms | P99 ms | Max ms | Errors | Avg bytes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| search_text_attrs | 100 | 73 | 11.543 | 13.483 | 18.911 | 207.983 | 0 | 1136 |
| trace_search_attrs | 100 | 1306 | 0.009 | 0.030 | 0.042 | 75.504 | 0 | 27649 |
| trace_aggregate_rollup | 100 | 24323 | 0.006 | 0.011 | 0.025 | 3.412 | 0 | 7291 |
| storage_stats | 100 | 11288 | 0.001 | 0.001 | 0.005 | 8.756 | 0 | 1768 |
| vector_namespace | 0 | 0 | 0.000 | 0.000 | 0.000 | 0.000 | 0 | 0 |
| trace_search_page_100 | 100 | 789 | 0.028 | 0.029 | 0.065 | 123.949 | 0 | 135791 |
