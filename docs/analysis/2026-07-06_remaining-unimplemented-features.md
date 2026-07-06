# yiTrace 当前未实现/未生产化功能清单

> 日期：2026-07-06
> 依据：`docs/CURRENT_STATE.md`、`docs/plans/2026-07-06_read-model-index-and-distributed-production-plan.md`、当前代码标记。

## 结论

核心单机/嵌入式 TraceDB 能力已经基本闭环：ingest、fold、search、attrs、metadata、retention、Golden Path 证据、Node/Electron 嵌入式包、读模型第一版索引和分布式 gateway 原语都已落地，并有 eval 覆盖。

剩余工作主要不是“API 还没有”，而是四类：

1. 第一版索引升级到大规模高性能版本。
2. 分布式从可验证原语升级到生产控制面。
3. 发布与平台包矩阵。
4. 安全合规与真实外部库接入。

## P0：发布/交付仍未完成

- 正式 npm 发布还未执行：`@yitrace/db` root 包和 per-platform optional native packages 还没有发布到 npm。
- 平台包矩阵还需要 CI 化：macOS arm64 已能本地验证，但 linux-x64、linux-arm64、darwin-x64、win32-x64 需要正式 CI build、pack、install smoke test。
- Electron 打包的真实模板/示例还可以补：asar unpack、optional native packages、`NAPI_RS_NATIVE_LIBRARY_PATH` 已有文档，但还缺一个可运行 sample app。

## P1：读模型高性能版仍未全部完成

- `traceAggregate` 已有 segment span rollup，但还没有更激进的 `(group_schema, group_key) -> counters` 预聚合层。
- `loops` / `taskTraces` 已复用 rollup sidecar，不再纯扫 folded spans；但还没有专门的 `loop_id` / `task_fingerprint` postings 或 counter index。
- metadata index 已覆盖 annotation / dataset association；Golden Path / retention policy 的 list/filter 仍未做同款 index。
- 全文分域 BM25 已覆盖 input/output/log/tool/model/agent；还缺 `attrs.*` 白名单域、retention soft-delete 后索引剔除、混合中英/SQL/表名字段名的更完整 eval。
- 向量 namespace 已有 `named_vectors.dat` + 内存 flat index；span/task 带 traceId 的向量已做 retention live-filter；还缺 namespace HNSW/GraphIndex、高性能 filtered ANN、recall/perf 回归。
- 10 万到 100 万 span/vector 级别的性能 bench 还没系统化沉淀。

## P1：分布式还是“生产化路径”，不是完整生产集群

- 动态 route table 已支持显式 reload 和文件 reload hook，但还缺后台 watcher / 外部控制面订阅。
- snapshot lease 已支持显式 create/renew/release 和 TTL 过期，但还缺更多读模型的 remote snapshot 覆盖。
- 网络复制已有 HTTP WAL pull/apply 底座、one-shot follower pull worker 和真实多进程 eval，但还缺后台复制 worker、调度、snapshot bootstrap、sealed segment/manifest/attr sidecar/vecindex/metadata/GC log 同步。
- health/heartbeat 已支持显式刷新和 bounded-stale follower read，但还缺周期 watcher、自动 failover、租约/fencing。
- retry/circuit breaker 已有最小版本，但还缺 retry budget、指数 backoff + jitter、breaker 诊断进入所有 fanout response。
- 一致性策略已支持 partial/strict/strong 第一版；还缺 gateway 默认策略配置、`staleBoundLsn` 和更细 read target metrics。

## P1：安全与企业化能力仍暂缓

- TLS、RBAC、落盘加密、PII 脱敏、持久防篡改审计、等保三级材料仍未实现。
- 这些不影响开源技术预览，但如果进入金融/政企 PoC，需要先补 TLS + RBAC + 持久审计日志。

## P2：外部真库和高级查询接缝仍未接满

- 团队 jieba / graph_index 真库还没在构建机做 `--features link` 的真实链接和召回对标；当前 mock/ABI/eval 接缝已就位。
- BM25 段内倒排 + block-max-WAND 仍是后续上量优化。
- LLM-judge eval 仍未实现。
- DataFusion 查询执行仍未接入；当前仍是手写查询路径。
- Vortex 随机取行 / 索引驱动 Vortex scan 还未做。

## 不建议现在做的项

- Golden Path 自动 Best/Challenger 裁决：暂不属于 DB 底座。DB 已提供 evidence、export、health、adherence；最佳路径决策应留给上层产品/eval 策略。
- active-active multi-leader：当前路线仍建议 single-writer per shard、cluster-level multi-writer。真 multi-leader 会引入冲突解决和跨 shard 事务复杂度，不应在现阶段做。
- trace 重复命中引用计数/压缩：已经被明确拆成独立需求，暂不做。

## 建议下一步

如果继续开发，优先级建议如下：

1. 发版前工程化：npm 平台包 CI matrix + clean consumer install smoke test。
2. 分布式生产化：后台复制调度 + snapshot bootstrap + route table watcher/控制面订阅 + fencing/failover。
3. 性能闭环：100k/1M bench harness + namespace HNSW/GraphIndex + filtered ANN recall/perf 回归。
4. 企业 PoC 门槛：TLS + RBAC + 持久审计日志。
