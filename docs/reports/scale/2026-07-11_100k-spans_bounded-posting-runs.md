# yiTrace Scale Bench Report

- generatedAtUnix: 1783735661
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
- dataDir: /var/folders/j6/3tm8zfn56fn47lfvwstq5y100000gp/T//yitrace-scale.ame0B7
- openAndRecoverSeconds: 0.001
- openAndRecoverMillis: 1.209
- rssAfterOpenKiB: 3888
- rssAfterQueriesKiB: 451396

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
| search_common_text | low | 1 | 4 | 228.926 | 228.926 | 228.926 | 228.926 | 228.926 | 0/0 | n/a | 4609 |
| search_common_text_project | medium | 1 | 4 | 255.796 | 255.796 | 255.796 | 255.796 | 255.796 | 0/0 | n/a | 4609 |
| search_rare_text | high | 1 | 7 | 142.303 | 142.303 | 142.303 | 142.303 | 142.303 | 0/0 | n/a | 4188 |
| trace_search_low_cardinality | low | 1 | 4 | 267.964 | 267.964 | 267.964 | 267.964 | 267.964 | 0/0 | trajectory_rollup 1/1 | 11531 |
| trace_search_high_cardinality | high | 1 | 20 | 49.591 | 49.591 | 49.591 | 49.591 | 49.591 | 0/0 | trajectory_rollup 1/1 | 11494 |
| trace_search_text_tenant_index | medium | 1 | 3 | 318.950 | 318.950 | 318.950 | 318.950 | 318.950 | 0/0 | filter_index 1/1 | 22496 |
| trace_aggregate_rollup | low | 1 | 10 | 103.241 | 103.241 | 103.241 | 103.241 | 103.241 | 0/0 | aggregate_rollup 1/1 | 3719 |
| storage_stats_rollup | low | 1 | 7 | 146.638 | 146.638 | 146.638 | 146.638 | 146.638 | 0/0 | trajectory_rollup 1/1 | 712 |
| trace_trajectories_rollup | high | 1 | 21 | 47.609 | 47.609 | 47.609 | 47.609 | 47.609 | 0/0 | trajectory_rollup 1/1 | 41795 |
| trajectory_groups_rollup | high | 1 | 7 | 140.665 | 140.665 | 140.665 | 140.665 | 140.665 | 0/0 | trajectory_rollup 1/1 | 38429 |
| loops_page_rollup | high | 1 | 18 | 56.684 | 56.684 | 56.684 | 56.684 | 56.684 | 0/0 | trajectory_rollup 1/1 | 4581 |
| task_traces_rollup | medium | 1 | 11 | 88.044 | 88.044 | 88.044 | 88.044 | 88.044 | 0/0 | trajectory_rollup 1/1 | 41797 |
| sessions_page_index | low | 1 | 29 | 34.164 | 34.164 | 34.164 | 34.164 | 34.164 | 0/0 | n/a | 8557 |
| trace_detail | point | 1 | 31 | 32.669 | 32.669 | 32.669 | 32.669 | 32.669 | 0/0 | n/a | 10404 |
| trace_diff | point | 1 | 6502 | 0.153 | 0.153 | 0.153 | 0.153 | 0.153 | 0/0 | n/a | 2370 |
