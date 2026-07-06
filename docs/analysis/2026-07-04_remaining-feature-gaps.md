# yiTrace 剩余功能缺口

> 日期：2026-07-04
> 2026-07-06 口径修正：Golden Path 治理增强不属于 yiTrace 底座待开发能力；底座只保留证据、导出、健康统计和候选资产存储。
> 口径：基于 `docs/CURRENT_STATE.md`、`docs/plans/2026-07-03_yitrace-remaining-requirements.md`、`docs/plans/2026-07-02_next-logic-roadmap.md` 和 `docs/design/2026-07-04_multi-writer-upgrade.md`。
> 后续执行计划：`docs/plans/2026-07-06_read-model-index-and-distributed-production-plan.md`。

## 总判断

当前底座已经不缺基础 TraceDB 能力：嵌入式 Node DB、外部 ID、attrs、一等字段、traceSearch、traceAggregate、trajectoryGroups、traceDiff、Golden Path、retention、storageStats、annotation/dataset association、usage/cost、in-process cluster prototype 都已有基础实现和 eval 覆盖。

下一阶段最缺的不是继续堆新读模型，而是四类能力：

1. 发版和真实消费验证。
2. 生产安全与运维闭环。
3. 高频查询的物化/索引化。
4. 多进程/分布式的真实落地。

## P0：发版与真实消费

- 正式 npm version bump。
- public npm、internal registry、tarball 三条分发路径定稿。
- 多平台 optional native package CI matrix：
  - darwin-arm64
  - darwin-x64
  - linux-x64-gnu
  - linux-arm64-gnu
  - win32-x64-msvc
- clean consumer 验证继续扩展：
  - ESM/CJS/native load
  - builder ingest
  - search/sessions/trace detail
  - annotation update/delete
  - Golden Path export
  - retention policy
- Electron packaging smoke test：
  - asar unpack
  - optional native packages 不被裁剪
  - `NAPI_RS_NATIVE_LIBRARY_PATH` fallback

## P0：生产最小安全闭环

- 配置系统：`--config yitrace.toml`，集中管理 bind/data dir/auth/max body/flush/vector cache/log level。
- `require_tenant_header` 生产模式开关：没有 tenant context 的写入可拒绝。
- token scope：至少区分 ingest、read、metadata-write、retention-apply。
- 审计日志从 stderr 升级成 JSONL 文件，记录 method/path/status/tenant/body_len。
- 简单请求限流，避免一个 client 把 embedded/server 打爆。
- `OpenOptions.readOnly` 等 engine 真只读打开路径完成后再重新暴露。

## P1：高频查询高性能化

- 2026-07-06 已补第一版高频 read-model cache：`traceAggregate()`、`trajectoryGroups()`、`traceTrajectories()`、`loops()`、`taskTraces()` 会返回 `readModelCache:"miss|hit"`，同一 tenant + request body 可复用结果；ingest、annotation、dataset association、Golden Path、retention 等写路径会显式失效。
- `traceAggregate()` 已有 segment 级 rollup sidecar 第一片，安全查询返回 `segment_rollup_tail_overlay`；后续仍需更细的 group counter rollup 和 100k+ 性能 bench。
- `loops()` / `taskTraces()` 已复用 segment rollup sidecar 第一片，安全查询返回 `loop_task_sidecar+tail_overlay`；后续仍可补专门的 loop/task postings 或 counter index，进一步减少从 rollup row 聚合列表的 CPU。
- `trajectoryGroups()` / `traceTrajectories()` 已有物化缓存层；`goldenPathHealth()` 如果成为高频入口，仍需 trajectory-level 物化索引。
- metadata index：annotation/dataset/golden path/retention policy 按 tenant + status + project/task + createdAt 建索引。
- task-level text index，服务相似任务召回。
- 可选 task/span/trajectory vector index，按 tenant/project/schema_fingerprint 过滤。

## 非底座：Golden Path 治理产品层

- DB 只负责 evidence，不自动判优；Best/Challenger 策略属于 Agent Memory / 控制台 / 业务产品层。
- Golden Path promote/deprecate 自动化策略暂不放入 yiTrace 底座开发队列，包括：
  - 样本数阈值
  - eval profile
  - score margin
  - 时间窗口
  - stale reason 到状态流转
- yiTrace 侧已完成或仍应保留的底座边界是：
  - 保存 Golden Path candidate/source trace/snapshot 引用。
  - `pathAdherence()` 做确定性 trajectory 对比。
  - `goldenPathEvidence()` 导出可复核证据包。
  - `goldenPathExport()` 输出稳定 JSONL schema。
  - `goldenPathHealth()` 返回 followed/extended/partial/deviated 统计，不替业务改状态。
- 重复 trace / canonical source 压缩仍是独立需求：
  - 是否保存重复 raw trace
  - 是否只保存 hit/reference count
  - retention 如何保护 canonical snapshot

## P1：真实多进程/分布式

- 当前是 in-process multi-shard prototype，不是多进程集群。
- `TraceStorage` / `ShardRouter` 第一层边界已抽出：`EngineJsonApi` 现在通过 storage facade 访问 single-node / in-process cluster；但 query fanout merge 仍主要在 HTTP JSON 层。
- 跨 shard snapshot lease 第一版已完成：`sessions`、`traceSearch`、`traceAggregate`、`trajectoryGroups`、`traceTrajectories`、`storageStats`、`loops`、`taskTraces` 返回/接受 token；同进程 lease 会保活每个 shard 的 `yt_manifest::Snapshot`，后续分页可 fixed-version read。
- snapshot lease 当前仍是 in-process：lease book 最多保留 64 个 token；被挤出返回 `409 snapshot_expired`，篡改 token 返回 `409 snapshot_mismatch`。
- 多进程版本仍缺远程 lease / 按 manifest version pin：gateway 需要把 token 映射到 shard server 上的 lease，或请求 shard 按指定 manifest version 读取。
- metadata 版本化仍缺：annotation/dataset/Golden Path/retention policy 仍是独立元数据文件，没有纳入 snapshot lease。
- WAL tail shipping follower 原语已完成：leader 可导出 WAL 增量，follower 可按 LSN 幂等应用并在重启后 recover。
- in-process follower topology registry 已完成：`/v1/cluster/shards` 可展示每个 shard 的 follower、lag、readable、syncState、reason 和 maxLagLsn，供后续读路由/控制面消费。
- `/v1/search` 已接入 bounded-stale follower read route：可读 follower 承接高频召回，严格读或 follower 不可读时回落 leader。
- `traceSearch` / `traceAggregate` 已接入 follower-target snapshot lease：snapshot token 会记录 readTarget，翻页复用同一个 follower `coord + snapshot`。
- 真实多进程 eval 已补：测试会启动 3 个独立 shard server 进程，经 TCP ingest/query，并 kill/restart 单 shard 验证 durable 恢复。
- 真实 gateway 进程 eval 已补：测试额外启动 gateway 子进程，请求只打 gateway，再由 gateway 对多个 shard 做路由写入和 fanout 查询。
- 真实 gateway 部分失败 eval 已补：查询类 fanout 在 1 个 shard 宕机时返回 `degraded:true`、`okShards` 和 `failedShards`，并只统计成功 shard 的结果；写入类 fanout 返回 `partialSuccess`、成功写入数、失败 shard 和 `retrySafe:"event_id_dedup"`。
- fanout 诊断合同已开始标准化：`traceSearch` / `traceAggregate` 默认返回 `okShards`、`degraded`、`failedShards`；`/v1/search` 为兼容旧数组响应，显式传 `includeFanout:true` 才返回诊断 envelope。
- `ShardClient` 统一边界已补：支持 `route_with_tenant`、`ingest_wire_for_tenant`、`search_hits` 和 `replication_status`；cluster `/v1/search` 通过 client 调 shard-local search，并把 client error 汇入 `FanoutReport`；fake failing client eval 覆盖 all-shards-failed 返回 503。
- `RemoteShardClient` 完整 HTTP 边界已补：std-only TCP client 可做远端 `/v1/ingest`、任意 shard-local JSON route、`/v1/search` typed fast path 和 `/v1/cluster/shards` status；真实 socket eval 覆盖 ingest/status/search/tenant 隔离。
- 真实 gateway 进程 eval 已切到 `RemoteShardClient`：gateway 通过正式 client 完成写入路由、trace-search fanout、部分失败降级和 cluster status 查询，不再使用测试内手写 shard HTTP serializer。
- `RemoteShardGateway` facade 已补：产品代码可完成远端 shard 路由写入、`trace-search` fanout、`search` fanout 和 cluster status 汇总；路由已对齐 `hash(tenant_id, session_id/trace_id)`，查询 fanout 已并发化，`search` 做全局 top-k，`trace-search` 做全局排序/分页；真实 gateway 子进程 eval 已复用这个 facade。
- 2026-07-06 已补 aggregate / trajectory / storage / metadata / retention 的 process gateway remote fanout 第一版：`traceAggregate` 和 `trajectoryGroups` 做跨 shard reduce，`traceTrajectories` 做跨 shard merge/page，`storageStats` 做总量与分组归并，annotation/dataset/golden path/retention policy 走 gateway-scoped id 或全 shard fanout，retention plan/apply/run-due 返回 shard 级诊断。
- 仍缺 sealed segment/manifest/attr sidecar/vecindex/metadata/GC log 同步、生产 gateway 动态路由表、网络复制、远程 snapshot lease、心跳/租约、角色管理、自动 failover、正式 timeout/retry/熔断策略、strict/partial query policy，以及 metadata 控制面的一致性/迁移策略。
- control plane metadata 策略：annotation/dataset/Golden Path/retention policy 是 co-locate 还是独立控制面。
- Raft per shard 可以后置；近期不建议做 multi-leader active-active。

## P2：开源/DX

- GitHub Actions：
  - `cargo test --offline`
  - Python pytest
  - TypeScript build/test
  - console build
  - Node package pack/verify
- release workflow：
  - macOS/Linux/Windows artifacts
  - Docker image
  - checksums
- 示例项目：
  - `examples/python-agent/`
  - `examples/typescript-agent/`
  - `examples/otlp-openinference/`
  - Electron main-process IPC example
- 文档：
  - `docs/DEPLOYMENT.md`
  - `docs/SECURITY.md`
  - `docs/COMPARISON.md`

## P2：安全与数据治理增强

- redaction/export profile：
  - Golden Path export
  - trace snapshot
  - Memory handoff
- PII/API key 脱敏策略。
- 落盘加密、TLS/RBAC、持久防篡改审计按 PoC 安全评审触发。

## 推荐下一步

优先顺序建议：

1. 发版与 clean consumer/Electron 验证。
2. 配置系统 + require tenant + audit JSONL + 简单限流。
3. metadata index + loop/task/trajectory 物化索引。
4. 远程 snapshot lease / manifest-version pin + segment/manifest sync follower。
5. CI/release/examples/docs 补齐。
