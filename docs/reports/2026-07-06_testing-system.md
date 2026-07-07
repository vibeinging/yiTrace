# 2026-07-06 测试体系报告

## 目标

给 yiTrace 建一套日常能跑、边界能卡住、发布前能加压的测试体系。重点不是堆工具，而是守住这些核心风险：

- 引擎崩溃恢复、折叠、检索、租户隔离不能退化。
- `event_id` 必须跨 Rust、Python、TypeScript 逐字节一致。
- SDK 上报失败不能静默丢数据。
- Node 嵌入式 DB 的 ESM/CJS、锁、租户、metadata、retention、Golden Path API 要能回归。
- 控制台 HTTP 契约和 mock 数据要稳定，避免字段名和状态映射漂移。

## 测试分层

| 层级 | 入口 | 覆盖内容 |
|---|---|---|
| Rust 引擎 | `cd yitrace-engine && cargo test --offline` | WAL、manifest、fold、BM25、向量索引、HTTP、eval、retention、Golden Path、集群读模型 |
| 风险矩阵 eval | `./scripts/eval_all.sh` 或 `cargo test --offline -p yt-engine --test risk_eval_matrix` | 分布式写安全、API 租户合同、读计划可观测、retention、包发布合同、gateway 安全 |
| 跨语言合同 | `tests/fixtures/event_id_cases.tsv` | Rust / Python / TypeScript 共用同一组 `event_id` 边界用例 |
| Python SDK | `cd yitrace-sdk/python && python tests/test_sdk.py` | span 生命周期、父子关系、token、session、异常、HTTP exporter 缓冲 |
| TypeScript SDK | `cd yitrace-sdk/typescript && npm test` | BigInt 精度、span 生命周期、HTTP exporter、异步 close |
| 控制台 | `cd yitrace-console && npm test && npm run build` | HTTP header、租户、URL 编码、搜索状态映射、mock 分页和 trace 树 |
| Node 嵌入式 DB | `cd yitrace-node && npm run build && npm test` | ESM/CJS、native 打包入口、ingest/search/read model、metadata、retention、Golden Path |
| 崩溃恢复加压 | `./scripts/test_all.sh --crash --crash-rounds 20` | 持久化 server 真 `kill -9`，重启后验证数据和中文检索 |
| 规模压测 | `./scripts/bench_scale.sh --smoke` | durable ingest、flush、search、traceSearch、traceAggregate、storageStats、vector namespace，并输出 Markdown 报告；区分 warm/cold 查询，向量查询单独采样，避免慢路径掩盖其他读 API |

## 新增内容

1. 新增共享 fixture：`tests/fixtures/event_id_cases.tsv`
   - 覆盖中文、空 `ext_span_id`、混合 unicode、`u64::MAX` seq。
   - Rust、Python、TypeScript 现在都读这份文件。

2. 新增控制台测试框架：
   - `yitrace-console/src/api/http-client.ts`
   - `yitrace-console/tsconfig.test.json`
   - `yitrace-console/test/http-client.test.mjs`
   - `yitrace-console/test/mock.test.mjs`

3. 新增总入口：
   - `scripts/test_all.sh`
   - 默认跑日常测试；`--crash` 才跑重型崩溃恢复。

4. 新增功能点覆盖清单：
   - `docs/reports/2026-07-06_feature-test-coverage.md`
   - 按摄入、查询、检索、索引、metadata、retention、Golden Path、分布式、SDK、Node、控制台和性能逐项列测试现状与缺口。

5. 完善 eval 框架并迁入摄入合同用例：
   - `evalkit::ApiEvalCase`
   - `evalkit::ApiEvalStep`
   - `evalkit::run_api_eval_suite`
   - `eval_harness::ingest_wire_contract_eval_cases`
   - `eval_harness::api_entrypoints_use_real_eval_steps`
   - API eval case 现在可以包含 seed / execute / verify 多个真实步骤；需要数据时用例自己造数据、真实写入、再真实查回。
   - 已把 ingest、外部 ID、租户隔离、OTLP、中文搜索入口从旧 HTTP 单测迁到真实 eval case。

6. 补强真实分布式进程用例：
   - `distributed_process_eval::gateway_process_recovers_after_backend_shard_restart`
   - 真实启动 3 个 shard 子进程 + 1 个 gateway 子进程，共 4 个 live 实例。
   - 测试先经 gateway 写入 3 个分片，再杀掉 1 个 shard，确认查询降级为 2 个可用分片；随后用同一个 data dir 和端口重启该 shard，确认 gateway 下一次查询恢复到 3 个分片全量结果。
   - 该用例已经接入 `EvalSuiteReport / EvalCaseReport / EvalCheckReport`，每个 HTTP 步骤和实例数检查都会进入 eval 报告。

7. 新增风险矩阵 eval 和统一 eval 入口：
   - `yitrace-engine/crates/yt-engine/tests/risk_eval_matrix.rs`
   - `scripts/eval_all.sh`
   - `risk_eval_matrix` 把当前最容易出问题的能力统一成一份 `EvalSuiteReport`：route table 拒绝同一 logical shard 双写主、租户头覆盖 body tenant、坏 ingest 不落库、traceAggregate 返回真实 readPlan、retention 保护 annotation、Node/Python/Rust 嵌入包发布合同、gateway auth/body limit、HTTP socket 租户隔离。
   - `eval_all.sh` 默认跑风险矩阵、真实分布式 chaos eval、主 eval harness、gateway example 编译和 Rust 嵌入式 DB 测试；`--packages` 跑 SDK/UI/Node/Python DB，`--pack` 跑 `@yitrace/db` clean consumer 打包验证，`--crash` 跑 kill -9。

8. 新增规模压测入口：
   - `yitrace-engine/crates/yt-engine/examples/scale_bench.rs`
   - `scripts/bench_scale.sh`
   - 该入口不增加产品功能，只用于生成可归档性能报告。默认 smoke 档为 10k spans；`--medium` 为 100k spans；`--large` 为 1M spans。报告会写入 `docs/reports/scale/`，包含写入吞吐、flush、vector index、search、traceSearch、traceAggregate、storageStats、vector namespace 的 QPS/P50/P95/P99。
   - 普通读 API 使用 `--queries` 控制次数；vector namespace 使用 `--vector-queries` 单独控制。当前向量查询路径还不是高性能实现，medium/large 默认不跑向量查询，只记录向量索引构建耗时；需要专项验证时显式传 `--vector-queries 1` 或更高。
   - 默认是 warm cache 基线，会体现 read model cache 的重复刷新效果；要压冷查询路径，用 `./scripts/bench_scale.sh --medium --cold-queries`，每次查询都会带不同 cache key。默认报告文件名会带 `_warm` 或 `_cold`。

9. 补强 storageStats 性能 eval：
   - `storage_stats_read_model_cache_hits_and_invalidates_on_ingest` 验证 storageStats 重复查询命中 read model cache，ingest 后会失效。
   - `storage_stats_preaggregate_matches_folded_scan_for_basic_totals` 验证同一批数据分别走 storage preaggregate 和 folded scan 时，trace/span/session/event/error/bytes 统计一致。
   - 测试同时覆盖三条路径：高频字段命中 `storage_preaggregate`，未覆盖 groupBy 回 `storage_segment_rollup`，traceId/time/text/metadata 等安全门回 `folded_scan`。
   - `storageStats` 响应现在带 `readPlan.spanReadIndex`，能看到是 `storage_preaggregate`、`storage_segment_rollup` 还是 `folded_scan`，并带 fallback 原因。
   - 100k spans cold benchmark 中，`storage_stats` P95 从约 813ms 降到 segment rollup 的约 362ms，再降到预聚合块的约 7.8ms；warm cache P95 约 0.001ms。后续性能门禁要同时看 cold 和 warm，不能只看缓存命中。

10. 补强 traceAggregate 性能 eval：
   - `trace_aggregate_uses_segment_rollup_after_flush` 现在验证高频聚合命中 `aggregate_preaggregate`，并检查 profile 字段、segment 数、row 数和返回 bucket。
   - 同一测试也检查 `/v1/metrics` 中的 rollup 写侧体量指标：cached segment、cached row、storage/aggregate profile family 和 bucket 数。这样后续控制写放大时有回归基线，且当前单机查询结果不变。
   - `trace_rollup_profile_budget_falls_back_without_changing_results` 验证显式关闭 profile 物化后，metrics 中 profile family/bucket 为 0，但 rollup row 仍保留；traceAggregate 和 storageStats 会回到 segment rollup，结果不变。
   - `durable_trace_aggregate_rollup_survives_reopen` 验证重启后预聚合仍可由持久 rollup row 派生。
   - `trace_aggregate_rollup_falls_back_for_cross_segment_span` 继续验证跨段 span 会回退 folded scan，不牺牲正确性。
   - 真实 fanout eval 验证 cluster traceAggregate 可走 `fanout_aggregate_preaggregate_tail_overlay`。
   - 100k spans cold benchmark 中，`trace_aggregate_rollup` P95 从约 318ms 降到约 3ms；代价是写入有额外 profile 物化成本，需要后续用 profile 上限或按需物化控制写放大。

11. 补强 traceSearch 列表页性能 eval：
   - `trace_search_uses_segment_rollup_for_simple_list_page_and_falls_back_for_text` 验证简单列表页命中 `readPlan.spanReadIndex="trace_search_rollup"`，并检查页内大字段补齐仍返回 inputText 预览。
   - 同一测试验证文本搜索会回退 `folded_scan`，且 attrs postings 仍能作为过滤入口使用。
   - 原 attrs sidecar 懒加载测试通过加入 `spanId` identity filter 显式走 postings 路径，避免被 rollup 快路径截走。
   - 100k spans cold benchmark 中，`trace_search_attrs` P95 降到约 36ms，`trace_search_page_100` P95 降到约 31ms；warm cache P95 约 0.03ms。

## 分布式实例数

当前不是只测单进程 mock，已有这些真实或半真实分布式规模：

| 用例 | 实例数 | 说明 |
|---|---:|---|
| `multi_process_shards_ingest_query_and_survive_restart` | 3 个真实 shard 进程 | 每个 shard 独立 data dir 和 TCP 端口；其中 1 个 shard 会 kill 后重启 |
| `gateway_process_routes_ingest_and_merges_real_shards` | 3 个真实 shard 进程 + 1 个真实 gateway 进程 | 请求只打 gateway，gateway 路由写入并 fanout 查询 |
| `gateway_process_query_reports_partial_shard_failure` | 3 个真实 shard 进程 + 1 个真实 gateway 进程 | 杀掉 1 个 shard 后验证 partial failure 诊断 |
| `gateway_process_recovers_after_backend_shard_restart` | 3 个真实 shard 进程 + 1 个真实 gateway 进程 | 杀掉并重启后端 shard，验证 gateway 恢复全量读 |
| `distributed_chaos_eval.rs` | 4 个真实 shard 进程 + 1 个真实 gateway 进程 | 2 个 logical shard，每个 leader+follower；覆盖 follower catch-up、kill leader、promote/reload、旧 snapshot 失效、旧 leader 重启不再收新写 |
| `network_wal_replication_between_processes_is_idempotent_and_gap_checked` | 2 个真实 shard 进程 | leader 到 follower 通过 HTTP WAL batch 复制 |
| `distributed_replica_eval.rs` | 1 leader + 1 follower，进程内实例 | 验证副本读、LSN 水位、follower 可见性 |
| `distributed_read_target_eval.rs` | 2 个轻量 TCP read target | 验证 remote read target、snapshot lease、读一致性策略 |

## 运行建议

日常开发：

```bash
./scripts/test_all.sh --skip-node
```

风险回归 / 大改后：

```bash
./scripts/eval_all.sh
```

规模压测 smoke：

```bash
./scripts/bench_scale.sh --smoke
```

改引擎承重逻辑：

```bash
cd yitrace-engine
cargo test --offline
```

发布前：

```bash
./scripts/eval_all.sh --heavy --crash-rounds 20
./scripts/test_all.sh --crash --crash-rounds 20
cd yitrace-node
npm run pack:verify
```

## 六类覆盖状态

| 类别 | 结论 | 已有覆盖 | 还缺什么 |
|---|---|---|---|
| 单写 | 有 | WAL、重启恢复、kill -9、重复 replay 不重复折叠、Node 单进程打开和重开 | 继续保持所有写路径先跑 `cargo test --offline` |
| 多写 | 部分有 | 路由表拒绝同一逻辑分片双写主、手动切换写主、并发读写回收、HTTP 并发请求 | 还没有“多个独立写者同时写同一分片”的 active-active 测试；当前设计仍是单分片单写主 |
| 单机 | 有 | Rust 引擎、SDK、Node 嵌入式 DB、控制台 HTTP 合同都在本机自动跑 | 单机发布包验证需要额外跑 `npm run pack:verify` |
| 分布式 | 有 | 进程内集群、真实多进程 shard、remote gateway、route table reload、follower read、snapshot lease、严格一致性失败场景、真实 chaos eval | 后续可以加更长时间的故障注入循环和自动 failover/fencing |
| 索引 | 有 | BM25、向量、named vector、attrs 过滤、retention 删除后索引隐藏、重开后索引一致 | 大规模数据下的索引膨胀和回收还可以加专项压测 |
| 性能 | 有工具，未做默认门禁 | `bench_qps`、并发请求、并发读写回收、kill -9 加压 | 还没有默认 CI 阈值，比如最低 QPS、P95 延迟、索引构建时间 |

重点结论：单写、单机、分布式、索引已经有自动测试；多写现在是“防双写”和“主写切换”测试；性能已有 readPlan/rollup/cache 是否命中的 eval 门禁，但 QPS/P95 仍是 benchmark，不是默认 pass/fail 门禁。

## 当前基线

`./scripts/test_all.sh` 已经跑通，覆盖 Rust 引擎、Python SDK、TypeScript SDK、控制台测试和构建、Node 嵌入式 DB 构建和测试。

新增的 `./scripts/eval_all.sh` 是风险 eval 总入口。默认覆盖 `risk_eval_matrix`、`distributed_chaos_eval`、`eval_harness`、`gateway_server` example 和 Rust DB crate；需要包级和打包验证时加 `--packages --pack`，需要崩溃恢复时加 `--crash`。

本轮没有默认跑 kill -9 重型测试；需要发布前用：

```bash
./scripts/test_all.sh --crash --crash-rounds 20
```
