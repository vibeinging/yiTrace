# yiTrace Scale Bench Report

- generatedAtUnix: 1783690658
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
- dataDir: /tmp/yitrace-scale-1m-lazy-data
- openAndRecoverSeconds: 0.001
- rssAfterOpenKiB: 4092
- rssAfterQueriesKiB: 5890544

## Data Shape

| Item | Value |
|---|---:|
| total bytes | 5920012808 |
| bytes / folded span | 5920.0 |
| WAL bytes | 1989523860 |
| segment bytes | 1989357225 |
| sidecar / index bytes | 1941102513 |
| manifest bytes | 28972 |
| other bytes | 238 |
| segment files | 556 |

## Read Path

| Query | Selectivity | Count | QPS | First ms | P50 ms | P95 ms | P99 ms | Max ms | Errors | Plan evidence | Avg bytes |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---|---:|
| search_common_text | low | 1 | 0 | 16659.483 | 16659.483 | 16659.483 | 16659.483 | 16659.483 | 0/0 | n/a | 4650 |
| search_common_text_project | medium | 1 | 1 | 1627.052 | 1627.052 | 1627.052 | 1627.052 | 1627.052 | 0/0 | n/a | 4722 |
| search_rare_text | high | 1 | 6 | 168.687 | 168.687 | 168.687 | 168.687 | 168.687 | 0/0 | n/a | 4305 |
| trace_search_low_cardinality | low | 1 | 0 | 2948.392 | 2948.392 | 2948.392 | 2948.392 | 2948.392 | 0/0 | trajectory_rollup 1/1 | 11534 |
| trace_search_high_cardinality | high | 1 | 2 | 601.347 | 601.347 | 601.347 | 601.347 | 601.347 | 0/0 | trajectory_rollup 1/1 | 11497 |
| trace_search_text_tenant_index | medium | 1 | 3 | 362.531 | 362.531 | 362.531 | 362.531 | 362.531 | 0/0 | filter_index 1/1 | 21751 |
| trace_aggregate_rollup | low | 1 | 1 | 1170.693 | 1170.693 | 1170.693 | 1170.693 | 1170.693 | 0/0 | aggregate_rollup 1/1 | 3810 |
| storage_stats_rollup | low | 1 | 1 | 1571.845 | 1571.845 | 1571.845 | 1571.845 | 1571.845 | 0/0 | trajectory_rollup 1/1 | 726 |
| trace_trajectories_rollup | high | 1 | 2 | 592.209 | 592.209 | 592.209 | 592.209 | 592.209 | 0/0 | trajectory_rollup 1/1 | 41798 |
| trajectory_groups_rollup | high | 1 | 1 | 1527.875 | 1527.875 | 1527.875 | 1527.875 | 1527.875 | 0/0 | trajectory_rollup 1/1 | 38107 |
| loops_page_rollup | high | 1 | 2 | 615.569 | 615.569 | 615.569 | 615.569 | 615.569 | 0/0 | trajectory_rollup 1/1 | 4584 |
| task_traces_rollup | medium | 1 | 1 | 842.541 | 842.541 | 842.541 | 842.541 | 842.541 | 0/0 | trajectory_rollup 1/1 | 41801 |
| sessions_page_index | low | 1 | 2 | 442.653 | 442.653 | 442.653 | 442.653 | 442.653 | 0/0 | n/a | 8600 |
| trace_detail | point | 1 | 14 | 69.485 | 69.485 | 69.485 | 69.485 | 69.485 | 0/0 | n/a | 9927 |
| trace_diff | point | 1 | 5639 | 0.177 | 0.177 | 0.177 | 0.177 | 0.177 | 0/0 | n/a | 2603 |
