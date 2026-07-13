# yiTrace Scale Bench Report

- generatedAtUnix: 1783667524
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
- dataDir: /var/folders/j6/3tm8zfn56fn47lfvwstq5y100000gp/T//yitrace-scale.JMTT3I
- openAndRecoverSeconds: 8.739
- rssAfterOpenKiB: 4929004
- rssAfterQueriesKiB: 6289752

## Data Shape

| Item | Value |
|---|---:|
| total bytes | 5920012808 |
| bytes / folded span | 5920.0 |
| WAL bytes | 1989523860 |
| segment bytes | 1989357225 |
| sidecar / index bytes | 1941102513 |
| manifest bytes | 28972 |
| other bytes | 238 |
| segment files | 556 |

## Read Path

| Query | Selectivity | Count | QPS | First ms | P50 ms | P95 ms | P99 ms | Max ms | Errors | Plan evidence | Avg bytes |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---|---:|
| search_common_text | low | 5 | 3 | 365.658 | 353.675 | 365.658 | 365.658 | 365.658 | 0/0 | n/a | 4650 |
| search_common_text_project | medium | 5 | 1 | 1844.236 | 1822.493 | 1844.236 | 1844.236 | 1844.236 | 0/0 | n/a | 4722 |
| search_rare_text | high | 20 | 6 | 179.210 | 174.387 | 187.718 | 192.200 | 192.200 | 0/0 | n/a | 4305 |
| trace_search_low_cardinality | low | 20 | 1 | 1119.218 | 965.546 | 1039.731 | 1119.218 | 1119.218 | 0/0 | trajectory_rollup 20/20 | 11534 |
| trace_search_high_cardinality | high | 20 | 2 | 636.670 | 631.278 | 642.196 | 646.882 | 646.882 | 0/0 | trajectory_rollup 20/20 | 11497 |
| trace_search_text_tenant_index | medium | 5 | 3 | 366.133 | 359.132 | 366.870 | 366.870 | 366.870 | 0/0 | filter_index 5/5 | 21751 |
| trace_aggregate_rollup | low | 20 | 1 | 1139.681 | 1141.540 | 1168.122 | 1237.742 | 1237.742 | 0/0 | aggregate_rollup 20/20 | 3810 |
| storage_stats_rollup | low | 20 | 1 | 1520.980 | 1564.429 | 1671.289 | 1709.047 | 1709.047 | 0/0 | trajectory_rollup 20/20 | 726 |
| trace_trajectories_rollup | high | 20 | 2 | 649.257 | 603.087 | 635.957 | 649.257 | 649.257 | 0/0 | trajectory_rollup 20/20 | 41798 |
| trajectory_groups_rollup | high | 20 | 1 | 1640.395 | 1485.180 | 1585.200 | 1640.395 | 1640.395 | 0/0 | trajectory_rollup 20/20 | 38107 |
| loops_page_rollup | high | 20 | 1 | 688.414 | 685.463 | 689.791 | 697.209 | 697.209 | 0/0 | trajectory_rollup 20/20 | 4584 |
| task_traces_rollup | medium | 20 | 1 | 898.629 | 885.665 | 917.599 | 943.133 | 943.133 | 0/0 | trajectory_rollup 20/20 | 41801 |
| sessions_page_index | low | 5 | 8 | 582.349 | 12.812 | 582.349 | 582.349 | 582.349 | 0/0 | n/a | 8599 |
| trace_detail | point | 5 | 13 | 75.036 | 75.036 | 77.655 | 77.655 | 77.655 | 0/0 | n/a | 9927 |
| trace_diff | point | 20 | 9078 | 0.191 | 0.104 | 0.119 | 0.191 | 0.191 | 0/0 | n/a | 2603 |
