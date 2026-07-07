# yiTrace Scale Bench Report

- generatedAtUnix: 1783404747
- spans: 100000
- queriesPerEndpoint: 100
- vectorQueries: 0
- queryCacheMode: warm
- vectorDim: 32
- vectorCount: 500
- dataDir: /var/folders/j6/3tm8zfn56fn47lfvwstq5y100000gp/T/yt_scale_bench_65988
- dataBytes: 214228434
- walBytes: 75618658
- segmentLikeFiles: 75

## Notes

- vector_namespace query was skipped because vectorQueries is 0; vector_index still measures vector write/build cost.
- Run with `--vector-queries N` to benchmark vector reads. Medium/large skip it by default because this path is currently a known slow path.

## Write Path

| Step | Count | Seconds | Rate |
|---|---:|---:|---:|
| ingest_wire | 100000 | 12.217 | 8185 spans/s |
| flush_memtable | 100000 | 0.121 | - |
| vector_index | 500 | 2.227 | 224 vectors/s |

## Read Path

| Query | Count | QPS | P50 ms | P95 ms | P99 ms | Max ms | Errors | Avg bytes |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| search_text_attrs | 100 | 74 | 11.387 | 12.273 | 16.892 | 203.958 | 0 | 1136 |
| trace_search_attrs | 100 | 133 | 0.009 | 0.011 | 0.043 | 751.110 | 0 | 27305 |
| trace_aggregate_rollup | 100 | 24590 | 0.006 | 0.007 | 0.021 | 3.426 | 0 | 7291 |
| storage_stats | 100 | 11981 | 0.001 | 0.001 | 0.003 | 8.247 | 0 | 1768 |
| vector_namespace | 0 | 0 | 0.000 | 0.000 | 0.000 | 0.000 | 0 | 0 |
| trace_search_page_100 | 100 | 364 | 0.025 | 0.035 | 0.097 | 271.994 | 0 | 135446 |
