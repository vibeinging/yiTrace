# yiTrace Scale Bench Report

- generatedAtUnix: 1783736357
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
- requestedQueriesPerEndpoint: 1
- dataDir: /var/folders/j6/3tm8zfn56fn47lfvwstq5y100000gp/T//yitrace-scale.fFFXBI
- openAndRecoverSeconds: 0.002
- openAndRecoverMillis: 2.086
- rssAfterOpenKiB: 4152
- rssAfterQueriesKiB: 2789880

## Data Shape

| Item | Value |
|---|---:|
| total bytes | 6170979941 |
| bytes / folded span | 6171.0 |
| WAL bytes | 1989523860 |
| segment bytes | 1989357225 |
| sidecar / index bytes | 2192069646 |
| manifest bytes | 28972 |
| other bytes | 238 |
| segment files | 556 |

## Read Path

| Query | Selectivity | Count | QPS | First ms | P50 ms | P95 ms | P99 ms | Max ms | Errors | Plan evidence | Avg bytes |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---|---:|
| search_common_text | low | 1 | 1 | 999.098 | 999.098 | 999.098 | 999.098 | 999.098 | 0/0 | n/a | 4650 |
| search_common_text_project | medium | 1 | 0 | 2033.016 | 2033.016 | 2033.016 | 2033.016 | 2033.016 | 0/0 | n/a | 4722 |
| search_rare_text | high | 1 | 6 | 164.363 | 164.363 | 164.363 | 164.363 | 164.363 | 0/0 | n/a | 4305 |
| trace_search_low_cardinality | low | 1 | 0 | 3096.965 | 3096.965 | 3096.965 | 3096.965 | 3096.965 | 0/0 | trajectory_rollup 1/1 | 11534 |
| trace_search_high_cardinality | high | 1 | 2 | 625.371 | 625.371 | 625.371 | 625.371 | 625.371 | 0/0 | trajectory_rollup 1/1 | 11497 |
| trace_search_text_tenant_index | medium | 1 | 3 | 393.371 | 393.371 | 393.371 | 393.371 | 393.371 | 0/0 | filter_index 1/1 | 21751 |
| trace_aggregate_rollup | low | 1 | 1 | 1189.991 | 1189.991 | 1189.991 | 1189.991 | 1189.991 | 0/0 | aggregate_rollup 1/1 | 3810 |
| storage_stats_rollup | low | 1 | 1 | 1598.207 | 1598.207 | 1598.207 | 1598.207 | 1598.207 | 0/0 | trajectory_rollup 1/1 | 726 |
| trace_trajectories_rollup | high | 1 | 2 | 609.282 | 609.282 | 609.282 | 609.282 | 609.282 | 0/0 | trajectory_rollup 1/1 | 41798 |
| trajectory_groups_rollup | high | 1 | 1 | 1505.987 | 1505.987 | 1505.987 | 1505.987 | 1505.987 | 0/0 | trajectory_rollup 1/1 | 38107 |
| loops_page_rollup | high | 1 | 2 | 662.903 | 662.903 | 662.903 | 662.903 | 662.903 | 0/0 | trajectory_rollup 1/1 | 4584 |
| task_traces_rollup | medium | 1 | 1 | 949.917 | 949.917 | 949.917 | 949.917 | 949.917 | 0/0 | trajectory_rollup 1/1 | 41801 |
| sessions_page_index | low | 1 | 2 | 440.711 | 440.711 | 440.711 | 440.711 | 440.711 | 0/0 | n/a | 8597 |
| trace_detail | point | 1 | 14 | 69.785 | 69.785 | 69.785 | 69.785 | 69.785 | 0/0 | n/a | 9927 |
| trace_diff | point | 1 | 5512 | 0.181 | 0.181 | 0.181 | 0.181 | 0.181 | 0/0 | n/a | 2603 |
