# yiTrace Scale Bench Report

- generatedAtUnix: 1783662929
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
- dataDir: /var/folders/j6/3tm8zfn56fn47lfvwstq5y100000gp/T//yitrace-scale.5ludau
- openAndRecoverSeconds: 47.472
- rssAfterOpenKiB: 9603692
- rssAfterQueriesKiB: 10246396

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
| search_common_text | low | 5 | 3 | 393.708 | 372.630 | 393.708 | 393.708 | 393.708 | 0/0 | n/a | 4650 |
| search_common_text_project | medium | 5 | 1 | 2032.813 | 2011.097 | 2032.813 | 2032.813 | 2032.813 | 0/0 | n/a | 4722 |
| search_rare_text | high | 20 | 6 | 180.867 | 173.417 | 175.494 | 180.867 | 180.867 | 0/0 | n/a | 4305 |
| trace_search_low_cardinality | low | 20 | 1 | 1054.435 | 1090.825 | 1175.775 | 1192.924 | 1192.924 | 0/0 | trajectory_rollup 20/20 | 11534 |
| trace_search_high_cardinality | high | 20 | 1 | 690.154 | 690.499 | 713.424 | 718.670 | 718.670 | 0/0 | trajectory_rollup 20/20 | 11497 |
| trace_search_text_tenant_index | medium | 5 | 3 | 402.368 | 360.431 | 402.368 | 402.368 | 402.368 | 0/0 | filter_index 5/5 | 21751 |
| trace_aggregate_rollup | low | 20 | 1 | 1277.320 | 1274.866 | 1340.598 | 1418.781 | 1418.781 | 0/0 | aggregate_rollup 20/20 | 3810 |
| storage_stats_rollup | low | 20 | 1 | 1666.593 | 1658.616 | 1757.058 | 1802.431 | 1802.431 | 0/0 | trajectory_rollup 20/20 | 726 |
| trace_trajectories_rollup | high | 20 | 2 | 685.172 | 655.704 | 691.245 | 704.715 | 704.715 | 0/0 | trajectory_rollup 20/20 | 41798 |
| trajectory_groups_rollup | high | 20 | 1 | 1568.301 | 1551.253 | 1592.167 | 1600.379 | 1600.379 | 0/0 | trajectory_rollup 20/20 | 38107 |
| loops_page_rollup | high | 20 | 1 | 750.181 | 754.967 | 770.802 | 784.830 | 784.830 | 0/0 | trajectory_rollup 20/20 | 4584 |
| task_traces_rollup | medium | 20 | 1 | 966.731 | 961.590 | 992.036 | 1003.192 | 1003.192 | 0/0 | trajectory_rollup 20/20 | 41801 |
| sessions_page_index | low | 5 | 9 | 529.511 | 13.292 | 529.511 | 529.511 | 529.511 | 0/0 | n/a | 8610 |
| trace_detail | point | 5 | 13 | 75.749 | 75.764 | 76.557 | 76.557 | 76.557 | 0/0 | n/a | 9927 |
| trace_diff | point | 20 | 8304 | 0.207 | 0.112 | 0.134 | 0.207 | 0.207 | 0/0 | n/a | 2603 |
