# yiTrace Scale Bench Report

- generatedAtUnix: 1783657399
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
- requestedQueriesPerEndpoint: 3
- dataDir: /var/folders/j6/3tm8zfn56fn47lfvwstq5y100000gp/T//yitrace-scale.f030yj
- openAndRecoverSeconds: 3.676
- rssAfterOpenKiB: 1249672
- rssAfterQueriesKiB: 1517940

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
| search_common_text | low | 3 | 14 | 161.283 | 28.764 | 161.283 | 161.283 | 161.283 | 0/0 | n/a | 4609 |
| search_common_text_project | medium | 3 | 6 | 263.073 | 126.612 | 263.073 | 263.073 | 263.073 | 0/0 | n/a | 4609 |
| search_rare_text | high | 3 | 25 | 103.926 | 8.506 | 103.926 | 103.926 | 103.926 | 0/0 | n/a | 4188 |
| trace_search_low_cardinality | low | 3 | 12 | 85.832 | 84.305 | 85.832 | 85.832 | 85.832 | 0/0 | trajectory_rollup 3/3 | 11531 |
| trace_search_high_cardinality | high | 3 | 20 | 52.578 | 49.504 | 52.578 | 52.578 | 52.578 | 0/0 | trajectory_rollup 3/3 | 11494 |
| trace_search_text_tenant_index | medium | 3 | 1 | 1641.796 | 903.168 | 1641.796 | 1641.796 | 1641.796 | 0/0 | filter_index 3/3 | 26938 |
| trace_aggregate_rollup | low | 3 | 10 | 105.775 | 105.130 | 105.775 | 105.775 | 105.775 | 0/0 | aggregate_rollup 3/3 | 3719 |
| storage_stats_rollup | low | 3 | 7 | 144.199 | 142.499 | 144.199 | 144.199 | 144.199 | 0/0 | trajectory_rollup 3/3 | 712 |
| trace_trajectories_rollup | high | 3 | 19 | 51.023 | 51.023 | 53.829 | 53.829 | 53.829 | 0/0 | trajectory_rollup 3/3 | 41795 |
| trajectory_groups_rollup | high | 3 | 7 | 144.812 | 141.328 | 144.812 | 144.812 | 144.812 | 0/0 | trajectory_rollup 3/3 | 38429 |
| loops_page_rollup | high | 3 | 18 | 54.765 | 54.450 | 54.765 | 54.765 | 54.765 | 0/0 | trajectory_rollup 3/3 | 4581 |
| task_traces_rollup | medium | 3 | 13 | 76.861 | 76.662 | 76.861 | 76.861 | 76.861 | 0/0 | trajectory_rollup 3/3 | 41797 |
| sessions_page_index | low | 3 | 1 | 1662.603 | 1662.603 | 1671.344 | 1671.344 | 1671.344 | 0/0 | n/a | 8591 |
| trace_detail | point | 3 | 0 | 2114.791 | 2114.791 | 2119.828 | 2119.828 | 2119.828 | 0/0 | n/a | 10404 |
| trace_diff | point | 3 | 8003 | 0.176 | 0.103 | 0.176 | 0.176 | 0.176 | 0/0 | n/a | 2370 |
