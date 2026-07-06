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
| 跨语言合同 | `tests/fixtures/event_id_cases.tsv` | Rust / Python / TypeScript 共用同一组 `event_id` 边界用例 |
| Python SDK | `cd yitrace-sdk/python && python tests/test_sdk.py` | span 生命周期、父子关系、token、session、异常、HTTP exporter 缓冲 |
| TypeScript SDK | `cd yitrace-sdk/typescript && npm test` | BigInt 精度、span 生命周期、HTTP exporter、异步 close |
| 控制台 | `cd yitrace-console && npm test && npm run build` | HTTP header、租户、URL 编码、搜索状态映射、mock 分页和 trace 树 |
| Node 嵌入式 DB | `cd yitrace-node && npm run build && npm test` | ESM/CJS、native 打包入口、ingest/search/read model、metadata、retention、Golden Path |
| 崩溃恢复加压 | `./scripts/test_all.sh --crash --crash-rounds 20` | 持久化 server 真 `kill -9`，重启后验证数据和中文检索 |

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

## 分布式实例数

当前不是只测单进程 mock，已有这些真实或半真实分布式规模：

| 用例 | 实例数 | 说明 |
|---|---:|---|
| `multi_process_shards_ingest_query_and_survive_restart` | 3 个真实 shard 进程 | 每个 shard 独立 data dir 和 TCP 端口；其中 1 个 shard 会 kill 后重启 |
| `gateway_process_routes_ingest_and_merges_real_shards` | 3 个真实 shard 进程 + 1 个真实 gateway 进程 | 请求只打 gateway，gateway 路由写入并 fanout 查询 |
| `gateway_process_query_reports_partial_shard_failure` | 3 个真实 shard 进程 + 1 个真实 gateway 进程 | 杀掉 1 个 shard 后验证 partial failure 诊断 |
| `gateway_process_recovers_after_backend_shard_restart` | 3 个真实 shard 进程 + 1 个真实 gateway 进程 | 杀掉并重启后端 shard，验证 gateway 恢复全量读 |
| `network_wal_replication_between_processes_is_idempotent_and_gap_checked` | 2 个真实 shard 进程 | leader 到 follower 通过 HTTP WAL batch 复制 |
| `distributed_replica_eval.rs` | 1 leader + 1 follower，进程内实例 | 验证副本读、LSN 水位、follower 可见性 |
| `distributed_read_target_eval.rs` | 2 个轻量 TCP read target | 验证 remote read target、snapshot lease、读一致性策略 |

## 运行建议

日常开发：

```bash
./scripts/test_all.sh --skip-node
```

改引擎承重逻辑：

```bash
cd yitrace-engine
cargo test --offline
```

发布前：

```bash
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
| 分布式 | 有 | 进程内集群、真实多进程 shard、remote gateway、route table reload、follower read、snapshot lease、严格一致性失败场景 | 后续可以加更长时间的故障注入循环 |
| 索引 | 有 | BM25、向量、named vector、attrs 过滤、retention 删除后索引隐藏、重开后索引一致 | 大规模数据下的索引膨胀和回收还可以加专项压测 |
| 性能 | 有工具，未做默认门禁 | `bench_qps`、并发请求、并发读写回收、kill -9 加压 | 还没有默认 CI 阈值，比如最低 QPS、P95 延迟、索引构建时间 |

重点结论：单写、单机、分布式、索引已经有自动测试；多写现在是“防双写”和“主写切换”测试；性能现在是 benchmark 和压力脚本，不是默认 pass/fail 门禁。

## 当前基线

`./scripts/test_all.sh` 已经跑通，覆盖 Rust 引擎、Python SDK、TypeScript SDK、控制台测试和构建、Node 嵌入式 DB 构建和测试。

本轮没有默认跑 kill -9 重型测试；需要发布前用：

```bash
./scripts/test_all.sh --crash --crash-rounds 20
```
