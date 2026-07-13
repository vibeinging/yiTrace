# yiTrace Scale Bench Report

- generatedAtUnix: 1783736725
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
- requestedQueriesPerEndpoint: 1
- dataDir: /var/folders/j6/3tm8zfn56fn47lfvwstq5y100000gp/T//yitrace-scale.BCkHHq
- openAndRecoverSeconds: 0.001
- openAndRecoverMillis: 1.189
- rssAfterOpenKiB: 3884
- rssAfterQueriesKiB: 439836

## Data Shape

| Item | Value |
|---|---:|
| total bytes | 615475910 |
| bytes / folded span | 6154.8 |
| WAL bytes | 198010448 |
| segment bytes | 197993731 |
| sidecar / index bytes | 219468532 |
| manifest bytes | 2972 |
| other bytes | 227 |
| segment files | 56 |

## Read Path

| Query | Selectivity | Count | QPS | First ms | P50 ms | P95 ms | P99 ms | Max ms | Errors | Plan evidence | Avg bytes |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---|---:|
| search_common_text | low | 1 | 5 | 213.297 | 213.297 | 213.297 | 213.297 | 213.297 | 0/0 | n/a | 4609 |
| search_common_text_project | medium | 1 | 5 | 185.089 | 185.089 | 185.089 | 185.089 | 185.089 | 0/0 | n/a | 4609 |
| search_rare_text | high | 1 | 7 | 142.777 | 142.777 | 142.777 | 142.777 | 142.777 | 0/0 | n/a | 4188 |
| trace_search_low_cardinality | low | 1 | 4 | 283.350 | 283.350 | 283.350 | 283.350 | 283.350 | 0/0 | trajectory_rollup 1/1 | 11531 |
| trace_search_high_cardinality | high | 1 | 19 | 51.775 | 51.775 | 51.775 | 51.775 | 51.775 | 0/0 | trajectory_rollup 1/1 | 11494 |
| trace_search_text_tenant_index | medium | 1 | 3 | 307.342 | 307.342 | 307.342 | 307.342 | 307.342 | 0/0 | filter_index 1/1 | 22496 |
| trace_aggregate_rollup | low | 1 | 10 | 104.672 | 104.672 | 104.672 | 104.672 | 104.672 | 0/0 | aggregate_rollup 1/1 | 3719 |
| storage_stats_rollup | low | 1 | 6 | 156.012 | 156.012 | 156.012 | 156.012 | 156.012 | 0/0 | trajectory_rollup 1/1 | 712 |
| trace_trajectories_rollup | high | 1 | 20 | 50.631 | 50.631 | 50.631 | 50.631 | 50.631 | 0/0 | trajectory_rollup 1/1 | 41795 |
| trajectory_groups_rollup | high | 1 | 7 | 143.258 | 143.258 | 143.258 | 143.258 | 143.258 | 0/0 | trajectory_rollup 1/1 | 38429 |
| loops_page_rollup | high | 1 | 18 | 54.473 | 54.473 | 54.473 | 54.473 | 54.473 | 0/0 | trajectory_rollup 1/1 | 4581 |
| task_traces_rollup | medium | 1 | 12 | 85.242 | 85.242 | 85.242 | 85.242 | 85.242 | 0/0 | trajectory_rollup 1/1 | 41797 |
| sessions_page_index | low | 1 | 30 | 33.820 | 33.820 | 33.820 | 33.820 | 33.820 | 0/0 | n/a | 8566 |
| trace_detail | point | 1 | 30 | 33.006 | 33.006 | 33.006 | 33.006 | 33.006 | 0/0 | n/a | 10404 |
| trace_diff | point | 1 | 5842 | 0.171 | 0.171 | 0.171 | 0.171 | 0.171 | 0/0 | n/a | 2370 |
