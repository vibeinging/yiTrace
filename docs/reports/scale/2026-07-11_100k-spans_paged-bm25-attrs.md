# yiTrace Scale Bench Report

- generatedAtUnix: 1783734584
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
- requestedQueriesPerEndpoint: 5
- dataDir: /var/folders/j6/3tm8zfn56fn47lfvwstq5y100000gp/T//yitrace-scale.4ijvPb
- openAndRecoverSeconds: 0.001
- openAndRecoverMillis: 1.171
- rssAfterOpenKiB: 3868
- rssAfterQueriesKiB: 514196

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
| search_common_text | low | 5 | 7 | 233.689 | 135.219 | 233.689 | 233.689 | 233.689 | 0/0 | n/a | 4609 |
| search_common_text_project | medium | 5 | 4 | 251.606 | 256.706 | 257.518 | 257.518 | 257.518 | 0/0 | n/a | 4609 |
| search_rare_text | high | 5 | 7 | 138.634 | 138.634 | 141.965 | 141.965 | 141.965 | 0/0 | n/a | 4188 |
| trace_search_low_cardinality | low | 5 | 8 | 254.696 | 89.586 | 254.696 | 254.696 | 254.696 | 0/0 | trajectory_rollup 5/5 | 11531 |
| trace_search_high_cardinality | high | 5 | 19 | 53.287 | 51.485 | 53.287 | 53.287 | 53.287 | 0/0 | trajectory_rollup 5/5 | 11494 |
| trace_search_text_tenant_index | medium | 5 | 3 | 317.794 | 313.393 | 317.794 | 317.794 | 317.794 | 0/0 | filter_index 5/5 | 22496 |
| trace_aggregate_rollup | low | 5 | 9 | 106.716 | 108.540 | 109.686 | 109.686 | 109.686 | 0/0 | aggregate_rollup 5/5 | 3719 |
| storage_stats_rollup | low | 5 | 6 | 156.519 | 155.591 | 159.039 | 159.039 | 159.039 | 0/0 | trajectory_rollup 5/5 | 712 |
| trace_trajectories_rollup | high | 5 | 20 | 49.864 | 50.023 | 50.503 | 50.503 | 50.503 | 0/0 | trajectory_rollup 5/5 | 41795 |
| trajectory_groups_rollup | high | 5 | 7 | 143.212 | 143.169 | 144.619 | 144.619 | 144.619 | 0/0 | trajectory_rollup 5/5 | 38429 |
| loops_page_rollup | high | 5 | 18 | 54.400 | 54.722 | 55.510 | 55.510 | 55.510 | 0/0 | trajectory_rollup 5/5 | 4581 |
| task_traces_rollup | medium | 5 | 12 | 77.918 | 85.646 | 87.467 | 87.467 | 87.467 | 0/0 | trajectory_rollup 5/5 | 41797 |
| sessions_page_index | low | 5 | 135 | 34.283 | 0.683 | 34.283 | 34.283 | 34.283 | 0/0 | n/a | 8569 |
| trace_detail | point | 5 | 30 | 32.445 | 32.889 | 34.041 | 34.041 | 34.041 | 0/0 | n/a | 10404 |
| trace_diff | point | 5 | 9261 | 0.175 | 0.095 | 0.175 | 0.175 | 0.175 | 0/0 | n/a | 2370 |
