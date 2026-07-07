# yiTrace Scale Bench Report

- generatedAtUnix: 1783400901
- spans: 100000
- queriesPerEndpoint: 100
- vectorQueries: 0
- queryCacheMode: cold
- vectorDim: 32
- vectorCount: 500
- dataDir: /var/folders/j6/3tm8zfn56fn47lfvwstq5y100000gp/T/yt_scale_bench_4997
- dataBytes: 214228434
- walBytes: 75618658
- segmentLikeFiles: 75

## Notes

- vector_namespace query was skipped because vectorQueries is 0; vector_index still measures vector write/build cost.
- Run with `--vector-queries N` to benchmark vector reads. Medium/large skip it by default because this path is currently a known slow path.

## Write Path

| Step | Count | Seconds | Rate |
|---|---:|---:|---:|
| ingest_wire | 100000 | 9.264 | 10794 spans/s |
| flush_memtable | 100000 | 0.080 | - |
| vector_index | 500 | 2.308 | 217 vectors/s |

## Read Path

| Query | Count | QPS | P50 ms | P95 ms | P99 ms | Max ms | Errors | Avg bytes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| search_text_attrs | 100 | 85 | 9.932 | 10.612 | 11.925 | 191.008 | 0 | 1136 |
| trace_search_attrs | 100 | 8 | 116.279 | 123.399 | 132.196 | 713.268 | 0 | 27306 |
| trace_aggregate_rollup | 100 | 3 | 307.481 | 318.392 | 323.791 | 324.288 | 0 | 7184 |
| storage_stats | 100 | 133 | 7.461 | 7.805 | 8.789 | 9.418 | 0 | 1769 |
| vector_namespace | 0 | 0 | 0.000 | 0.000 | 0.000 | 0.000 | 0 | 0 |
| trace_search_page_100 | 100 | 4 | 225.906 | 233.006 | 235.434 | 235.651 | 0 | 135447 |
