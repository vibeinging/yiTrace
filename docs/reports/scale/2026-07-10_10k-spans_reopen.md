# yiTrace Scale Bench Report

- generatedAtUnix: 1783656625
- phase: query
- queryProcessMode: separate-process-reopen
- seed: 11400714819323198485
- foldedSpans: 10000
- wireEvents: 22774
- traces: 1021
- sessions: 256
- loops: 128
- logEvents: 2694
- duplicateEvents: 90
- incompleteSpans: 10
- requestedQueriesPerEndpoint: 200
- dataDir: /var/folders/j6/3tm8zfn56fn47lfvwstq5y100000gp/T//yitrace-scale.pksOc2
- openAndRecoverSeconds: 0.403
- rssAfterOpenKiB: 178488
- rssAfterQueriesKiB: 479284

## Data Shape

| Item | Value |
|---|---:|
| total bytes | 58370443 |
| bytes / folded span | 5837.0 |
| WAL bytes | 19555448 |
| segment bytes | 19553771 |
| sidecar / index bytes | 19260635 |
| manifest bytes | 372 |
| other bytes | 217 |
| segment files | 6 |

## Read Path

| Query | Selectivity | QPS | First ms | P50 ms | P95 ms | P99 ms | Max ms | Errors | Plan evidence | Avg bytes |
|---|---|---:|---:|---:|---:|---:|---:|---:|---|---:|
| search_common_text | low | 96 | 67.475 | 10.017 | 11.866 | 14.933 | 67.475 | 0/0 | n/a | 4406 |
| search_common_text_project | medium | 58 | 67.768 | 16.916 | 17.846 | 21.621 | 67.768 | 0/0 | n/a | 4533 |
| search_rare_text | high | 124 | 8.647 | 8.007 | 8.192 | 10.137 | 15.791 | 0/0 | n/a | 4251 |
| trace_search_low_cardinality | low | 188 | 6.062 | 5.301 | 5.859 | 6.144 | 6.166 | 0/0 | trajectory_rollup 200/200 | 11528 |
| trace_search_high_cardinality | high | 355 | 3.559 | 2.756 | 3.293 | 3.468 | 3.648 | 0/0 | trajectory_rollup 200/200 | 11488 |
| trace_search_text_tenant_index | medium | 11 | 97.054 | 87.576 | 92.829 | 98.585 | 111.981 | 0/0 | filter_index 200/200 | 26931 |
| trace_aggregate_rollup | low | 144 | 6.843 | 6.896 | 7.653 | 8.515 | 9.385 | 0/0 | aggregate_rollup 200/200 | 3621 |
| storage_stats_rollup | low | 97 | 9.322 | 10.125 | 11.184 | 15.602 | 18.826 | 0/0 | trajectory_rollup 200/200 | 698 |
| trace_trajectories_rollup | high | 258 | 4.449 | 3.879 | 4.214 | 4.393 | 4.483 | 0/0 | trajectory_rollup 200/200 | 41790 |
| trajectory_groups_rollup | high | 93 | 9.918 | 10.412 | 12.939 | 15.075 | 16.088 | 0/0 | trajectory_rollup 200/200 | 35066 |
| loops_page_rollup | high | 286 | 3.867 | 3.470 | 4.114 | 4.330 | 5.999 | 0/0 | trajectory_rollup 200/200 | 4576 |
| task_traces_rollup | medium | 172 | 6.372 | 5.808 | 6.427 | 7.168 | 8.213 | 0/0 | trajectory_rollup 200/200 | 41790 |
| sessions_page_index | low | 6 | 173.394 | 168.426 | 175.551 | 183.145 | 193.706 | 0/0 | n/a | 8541 |
| trace_detail | point | 5 | 210.202 | 212.690 | 219.324 | 227.901 | 270.751 | 0/0 | n/a | 21034 |
| trace_diff | point | 6307 | 0.240 | 0.154 | 0.180 | 0.208 | 0.240 | 0/0 | n/a | 3537 |
