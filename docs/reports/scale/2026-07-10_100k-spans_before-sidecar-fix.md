# yiTrace Scale Bench Report

- generatedAtUnix: 1783657150
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
- dataDir: /var/folders/j6/3tm8zfn56fn47lfvwstq5y100000gp/T//yitrace-scale.jShTeG
- openAndRecoverSeconds: 3.721
- rssAfterOpenKiB: 1252800
- rssAfterQueriesKiB: 1654256

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
| search_common_text | low | 20 | 29 | 166.940 | 27.849 | 30.145 | 166.940 | 166.940 | 0/0 | n/a | 4609 |
| search_common_text_project | medium | 20 | 7 | 263.681 | 126.715 | 136.935 | 263.681 | 263.681 | 0/0 | n/a | 4609 |
| search_rare_text | high | 100 | 110 | 106.729 | 8.007 | 8.541 | 10.304 | 106.729 | 0/0 | n/a | 4188 |
| trace_search_low_cardinality | low | 100 | 12 | 90.343 | 80.845 | 88.684 | 108.447 | 130.873 | 0/0 | trajectory_rollup 100/100 | 11531 |
| trace_search_high_cardinality | high | 100 | 21 | 47.858 | 46.742 | 49.439 | 53.370 | 58.218 | 0/0 | trajectory_rollup 100/100 | 11494 |
| trace_search_text_tenant_index | medium | 20 | 1 | 1670.416 | 925.894 | 968.280 | 1670.416 | 1670.416 | 0/0 | filter_index 20/20 | 26938 |
| trace_aggregate_rollup | low | 100 | 10 | 102.162 | 100.739 | 103.946 | 114.799 | 121.659 | 0/0 | aggregate_rollup 100/100 | 3719 |
| storage_stats_rollup | low | 100 | 7 | 142.143 | 137.799 | 142.466 | 192.870 | 205.412 | 0/0 | trajectory_rollup 100/100 | 712 |
| trace_trajectories_rollup | high | 100 | 20 | 47.535 | 48.522 | 53.200 | 75.694 | 91.557 | 0/0 | trajectory_rollup 100/100 | 41795 |
| trajectory_groups_rollup | high | 100 | 7 | 146.263 | 138.037 | 142.030 | 157.847 | 164.296 | 0/0 | trajectory_rollup 100/100 | 38429 |
| loops_page_rollup | high | 100 | 19 | 55.500 | 52.601 | 54.780 | 59.181 | 59.895 | 0/0 | trajectory_rollup 100/100 | 4581 |
| task_traces_rollup | medium | 100 | 13 | 76.143 | 74.632 | 77.225 | 80.035 | 83.574 | 0/0 | trajectory_rollup 100/100 | 41797 |
| sessions_page_index | low | 20 | 1 | 1713.525 | 1661.588 | 1697.720 | 1713.525 | 1713.525 | 0/0 | n/a | 8591 |
| trace_detail | point | 20 | 0 | 2117.034 | 2117.034 | 2140.590 | 2161.926 | 2161.926 | 0/0 | n/a | 10404 |
| trace_diff | point | 100 | 10616 | 0.178 | 0.090 | 0.106 | 0.142 | 0.178 | 0/0 | n/a | 2370 |
