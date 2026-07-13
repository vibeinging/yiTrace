# yiTrace Scale Bench Report

- generatedAtUnix: 1783658870
- phase: query
- queryProcessMode: separate-process-reopen
- seed: 11400714819323198485
- foldedSpans: 1000000
- wireEvents: 2277482
- traces: 98205
- sessions: 24552
- loops: 12276
- logEvents: 269482
- duplicateEvents: 9000
- incompleteSpans: 1000
- requestedQueriesPerEndpoint: 200
- dataDir: /var/folders/j6/3tm8zfn56fn47lfvwstq5y100000gp/T//yitrace-scale.GYmscu
- openAndRecoverSeconds: 44.768
- rssAfterOpenKiB: 9592624
- rssAfterQueriesKiB: 16427548

## Data Shape

| Item | Value |
|---|---:|
| total bytes | 5920012768 |
| bytes / folded span | 5920.0 |
| WAL bytes | 1989523820 |
| segment bytes | 1989357225 |
| sidecar / index bytes | 1941102513 |
| manifest bytes | 28972 |
| other bytes | 238 |
| segment files | 556 |

## Read Path

| Query | Selectivity | Count | QPS | First ms | P50 ms | P95 ms | P99 ms | Max ms | Errors | Plan evidence | Avg bytes |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---|---:|
| search_common_text | low | 5 | 3 | 513.406 | 317.112 | 513.406 | 513.406 | 513.406 | 0/0 | n/a | 4650 |
| search_common_text_project | medium | 5 | 0 | 2833.375 | 2589.975 | 2833.375 | 2833.375 | 2833.375 | 0/0 | n/a | 4722 |
| search_rare_text | high | 20 | 57 | 191.199 | 8.750 | 9.109 | 191.199 | 191.199 | 0/0 | n/a | 4305 |
| trace_search_low_cardinality | low | 20 | 1 | 978.173 | 970.564 | 988.258 | 997.495 | 997.495 | 0/0 | trajectory_rollup 20/20 | 11534 |
| trace_search_high_cardinality | high | 20 | 2 | 618.737 | 623.142 | 630.068 | 631.124 | 631.124 | 0/0 | trajectory_rollup 20/20 | 11497 |
| trace_search_text_tenant_index | medium | 5 | 0 | 29687.151 | 30882.652 | 31262.130 | 31262.130 | 31262.130 | 0/0 | filter_index 5/5 | 26943 |
| trace_aggregate_rollup | low | 20 | 1 | 1201.521 | 1201.521 | 1244.780 | 1292.927 | 1292.927 | 0/0 | aggregate_rollup 20/20 | 3810 |
| storage_stats_rollup | low | 20 | 1 | 1651.268 | 1609.759 | 1651.268 | 1678.037 | 1678.037 | 0/0 | trajectory_rollup 20/20 | 726 |
| trace_trajectories_rollup | high | 20 | 2 | 633.359 | 618.754 | 632.617 | 633.359 | 633.359 | 0/0 | trajectory_rollup 20/20 | 41798 |
| trajectory_groups_rollup | high | 20 | 1 | 1506.877 | 1511.806 | 1534.479 | 1537.111 | 1537.111 | 0/0 | trajectory_rollup 20/20 | 38107 |
| loops_page_rollup | high | 20 | 1 | 734.277 | 712.532 | 730.230 | 734.277 | 734.277 | 0/0 | trajectory_rollup 20/20 | 4584 |
| task_traces_rollup | medium | 20 | 1 | 914.242 | 908.747 | 914.242 | 914.747 | 914.747 | 0/0 | trajectory_rollup 20/20 | 41801 |
| sessions_page_index | low | 5 | 0 | 17828.906 | 17713.609 | 17828.906 | 17828.906 | 17828.906 | 0/0 | n/a | 8639 |
| trace_detail | point | 5 | 0 | 21777.952 | 21784.105 | 21863.390 | 21863.390 | 21863.390 | 0/0 | n/a | 9927 |
| trace_diff | point | 20 | 7745 | 0.506 | 0.100 | 0.208 | 0.506 | 0.506 | 0/0 | n/a | 2603 |
