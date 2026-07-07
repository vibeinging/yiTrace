# yiTrace Scale Bench Report

- generatedAtUnix: 1783396482
- spans: 100000
- queriesPerEndpoint: 100
- vectorQueries: 0
- queryCacheMode: cold
- vectorDim: 32
- vectorCount: 500
- dataDir: /var/folders/j6/3tm8zfn56fn47lfvwstq5y100000gp/T/yt_scale_bench_7125
- dataBytes: 214228434
- walBytes: 75618658
- segmentLikeFiles: 75

## Notes

- vector_namespace query was skipped because vectorQueries is 0; vector_index still measures vector write/build cost.
- Run with `--vector-queries N` to benchmark vector reads. Medium/large skip it by default because this path is currently a known slow path.

## Write Path

| Step | Count | Seconds | Rate |
|---|---:|---:|---:|
| ingest_wire | 100000 | 8.159 | 12256 spans/s |
| flush_memtable | 100000 | 0.051 | - |
| vector_index | 500 | 2.182 | 229 vectors/s |

## Read Path

| Query | Count | QPS | P50 ms | P95 ms | P99 ms | Max ms | Errors | Avg bytes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| search_text_attrs | 100 | 57 | 14.785 | 23.548 | 29.426 | 199.148 | 0 | 1136 |
| trace_search_attrs | 100 | 8 | 121.123 | 140.366 | 181.018 | 779.139 | 0 | 27306 |
| trace_aggregate_rollup | 100 | 3 | 316.366 | 353.478 | 377.888 | 409.759 | 0 | 7184 |
| storage_stats | 100 | 3 | 344.154 | 361.595 | 396.186 | 411.529 | 0 | 1675 |
| vector_namespace | 0 | 0 | 0.000 | 0.000 | 0.000 | 0.000 | 0 | 0 |
| trace_search_page_100 | 100 | 4 | 235.169 | 249.837 | 277.327 | 290.298 | 0 | 135447 |
