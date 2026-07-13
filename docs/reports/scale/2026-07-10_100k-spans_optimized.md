# yiTrace Scale Bench Report

- generatedAtUnix: 1783661958
- phase: query
- queryProcessMode: separate-process-reopen
- seed: 11400714819323198485
- foldedSpans: 100000
- wireEvents: 227735
- traces: 9862
- sessions: 2466
- loops: 1233
- logEvents: 26935
- duplicateEvents: 900
- incompleteSpans: 100
- requestedQueriesPerEndpoint: 100
- dataDir: /var/folders/j6/3tm8zfn56fn47lfvwstq5y100000gp/T//yitrace-scale.s6Cq9d
- openAndRecoverSeconds: 3.741
- rssAfterOpenKiB: 1270340
- rssAfterQueriesKiB: 1425000

## Data Shape

| Item | Value |
|---|---:|
| total bytes | 589473858 |
| bytes / folded span | 5894.7 |
| WAL bytes | 198010408 |
| segment bytes | 197993731 |
| sidecar / index bytes | 193466520 |
| manifest bytes | 2972 |
| other bytes | 227 |
| segment files | 56 |

## Read Path

| Query | Selectivity | Count | QPS | First ms | P50 ms | P95 ms | P99 ms | Max ms | Errors | Plan evidence | Avg bytes |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---|---:|
| search_common_text | low | 20 | 8 | 132.684 | 129.240 | 132.006 | 132.684 | 132.684 | 0/0 | n/a | 4609 |
| search_common_text_project | medium | 20 | 4 | 213.802 | 226.124 | 269.723 | 278.562 | 278.562 | 0/0 | n/a | 4609 |
| search_rare_text | high | 100 | 7 | 153.332 | 152.399 | 155.708 | 172.882 | 172.956 | 0/0 | n/a | 4188 |
| trace_search_low_cardinality | low | 100 | 11 | 86.156 | 86.991 | 94.069 | 96.549 | 113.267 | 0/0 | trajectory_rollup 100/100 | 11531 |
| trace_search_high_cardinality | high | 100 | 20 | 50.356 | 50.472 | 55.086 | 57.224 | 73.565 | 0/0 | trajectory_rollup 100/100 | 11494 |
| trace_search_text_tenant_index | medium | 20 | 3 | 314.562 | 313.151 | 315.429 | 315.493 | 315.493 | 0/0 | filter_index 20/20 | 22496 |
| trace_aggregate_rollup | low | 100 | 10 | 120.553 | 103.814 | 112.243 | 122.635 | 128.854 | 0/0 | aggregate_rollup 100/100 | 3719 |
| storage_stats_rollup | low | 100 | 7 | 148.788 | 143.831 | 150.498 | 184.966 | 197.467 | 0/0 | trajectory_rollup 100/100 | 712 |
| trace_trajectories_rollup | high | 100 | 21 | 52.475 | 48.064 | 50.574 | 52.062 | 52.475 | 0/0 | trajectory_rollup 100/100 | 41795 |
| trajectory_groups_rollup | high | 100 | 7 | 144.767 | 141.445 | 163.469 | 191.527 | 194.471 | 0/0 | trajectory_rollup 100/100 | 38429 |
| loops_page_rollup | high | 100 | 17 | 58.477 | 57.002 | 62.775 | 68.868 | 77.260 | 0/0 | trajectory_rollup 100/100 | 4581 |
| task_traces_rollup | medium | 100 | 13 | 79.699 | 78.869 | 84.075 | 90.333 | 102.459 | 0/0 | trajectory_rollup 100/100 | 41797 |
| sessions_page_index | low | 20 | 96 | 45.208 | 8.922 | 9.082 | 45.208 | 45.208 | 0/0 | n/a | 8557 |
| trace_detail | point | 20 | 25 | 40.797 | 40.259 | 41.491 | 41.608 | 41.608 | 0/0 | n/a | 10404 |
| trace_diff | point | 100 | 10074 | 0.171 | 0.092 | 0.131 | 0.171 | 0.178 | 0/0 | n/a | 2370 |
