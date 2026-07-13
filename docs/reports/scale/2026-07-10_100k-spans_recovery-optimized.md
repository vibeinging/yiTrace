# yiTrace Scale Bench Report

- generatedAtUnix: 1783666925
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
- dataDir: /var/folders/j6/3tm8zfn56fn47lfvwstq5y100000gp/T//yitrace-scale.wZ8o9R
- openAndRecoverSeconds: 1.082
- rssAfterOpenKiB: 639492
- rssAfterQueriesKiB: 1009924

## Data Shape

| Item | Value |
|---|---:|
| total bytes | 589473898 |
| bytes / folded span | 5894.7 |
| WAL bytes | 198010448 |
| segment bytes | 197993731 |
| sidecar / index bytes | 193466520 |
| manifest bytes | 2972 |
| other bytes | 227 |
| segment files | 56 |

## Read Path

| Query | Selectivity | Count | QPS | First ms | P50 ms | P95 ms | P99 ms | Max ms | Errors | Plan evidence | Avg bytes |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---|---:|
| search_common_text | low | 20 | 8 | 135.615 | 127.406 | 135.298 | 135.615 | 135.615 | 0/0 | n/a | 4609 |
| search_common_text_project | medium | 20 | 5 | 211.903 | 207.045 | 269.746 | 298.493 | 298.493 | 0/0 | n/a | 4609 |
| search_rare_text | high | 100 | 7 | 153.885 | 151.026 | 160.313 | 173.201 | 189.219 | 0/0 | n/a | 4188 |
| trace_search_low_cardinality | low | 100 | 12 | 88.244 | 85.767 | 91.621 | 96.480 | 101.380 | 0/0 | trajectory_rollup 100/100 | 11531 |
| trace_search_high_cardinality | high | 100 | 21 | 49.942 | 48.631 | 51.392 | 52.775 | 53.275 | 0/0 | trajectory_rollup 100/100 | 11494 |
| trace_search_text_tenant_index | medium | 20 | 3 | 314.141 | 314.097 | 332.133 | 334.788 | 334.788 | 0/0 | filter_index 20/20 | 22496 |
| trace_aggregate_rollup | low | 100 | 9 | 105.777 | 102.866 | 117.411 | 167.689 | 168.276 | 0/0 | aggregate_rollup 100/100 | 3719 |
| storage_stats_rollup | low | 100 | 7 | 142.238 | 142.247 | 148.623 | 160.850 | 166.528 | 0/0 | trajectory_rollup 100/100 | 712 |
| trace_trajectories_rollup | high | 100 | 21 | 63.077 | 48.109 | 51.737 | 63.306 | 69.334 | 0/0 | trajectory_rollup 100/100 | 41795 |
| trajectory_groups_rollup | high | 100 | 7 | 140.490 | 140.092 | 145.345 | 152.776 | 221.402 | 0/0 | trajectory_rollup 100/100 | 38429 |
| loops_page_rollup | high | 100 | 18 | 58.544 | 56.931 | 59.217 | 59.660 | 59.904 | 0/0 | trajectory_rollup 100/100 | 4581 |
| task_traces_rollup | medium | 100 | 13 | 78.428 | 78.030 | 89.892 | 112.145 | 131.197 | 0/0 | trajectory_rollup 100/100 | 41797 |
| sessions_page_index | low | 20 | 100 | 45.087 | 8.024 | 9.931 | 45.087 | 45.087 | 0/0 | n/a | 8557 |
| trace_detail | point | 20 | 25 | 42.598 | 39.959 | 41.664 | 42.598 | 42.598 | 0/0 | n/a | 10404 |
| trace_diff | point | 100 | 10633 | 0.234 | 0.088 | 0.103 | 0.166 | 0.234 | 0/0 | n/a | 2370 |
