# yiTrace Scale Bench Report

- generatedAtUnix: 1783400932
- spans: 100000
- queriesPerEndpoint: 100
- vectorQueries: 0
- queryCacheMode: warm
- vectorDim: 32
- vectorCount: 500
- dataDir: /var/folders/j6/3tm8zfn56fn47lfvwstq5y100000gp/T/yt_scale_bench_5853
- dataBytes: 214228434
- walBytes: 75618658
- segmentLikeFiles: 75

## Notes

- vector_namespace query was skipped because vectorQueries is 0; vector_index still measures vector write/build cost.
- Run with `--vector-queries N` to benchmark vector reads. Medium/large skip it by default because this path is currently a known slow path.

## Write Path

| Step | Count | Seconds | Rate |
|---|---:|---:|---:|
| ingest_wire | 100000 | 9.301 | 10752 spans/s |
| flush_memtable | 100000 | 0.078 | - |
| vector_index | 500 | 2.383 | 210 vectors/s |

## Read Path

| Query | Count | QPS | P50 ms | P95 ms | P99 ms | Max ms | Errors | Avg bytes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| search_text_attrs | 100 | 83 | 9.992 | 10.897 | 11.570 | 198.598 | 0 | 1136 |
| trace_search_attrs | 100 | 142 | 0.008 | 0.011 | 0.046 | 701.736 | 0 | 27305 |
| trace_aggregate_rollup | 100 | 326 | 0.005 | 0.006 | 0.019 | 306.382 | 0 | 7183 |
| storage_stats | 100 | 12986 | 0.001 | 0.001 | 0.003 | 7.603 | 0 | 1768 |
| vector_namespace | 0 | 0 | 0.000 | 0.000 | 0.000 | 0.000 | 0 | 0 |
| trace_search_page_100 | 100 | 410 | 0.024 | 0.027 | 0.058 | 241.459 | 0 | 135446 |
