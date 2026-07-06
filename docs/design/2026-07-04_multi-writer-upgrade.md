# yiTrace 从单机单写升级到多写的架构思路

> 日期：2026-07-04
> 背景：当前 yiTrace 的承重实现是单机 `WriteCoordinator`，所有 WAL、MemTable、flush、compaction、delete、upgrade 和 manifest commit 都在单写者锁下串行。这保证了实现简单、恢复确定、快照一致。升级到多写时，核心目标不是让多个进程同时改同一个 data dir，而是把单写边界缩小到 shard 内部，在系统层形成多写能力。

## 结论

最稳的路线是 **single-writer per shard, multi-writer at cluster level**。

也就是说：

- 一个 shard 里仍然只有一个 writer leader 修改 WAL/manifest/segments。
- 多个 tenant/project/session/trace 被路由到不同 shard。
- 多个 shard 并行写入，所以整体系统是多写。
- 查询层做 fanout 和 merge，trace/session 详情尽量命中单 shard。

不要第一步做 multi-leader active-active。Trace 事件虽然 append-only、`event_id` 可幂等去重，天然适合分布式日志；但 yiTrace 现在不只有 append：还有 manifest commit、compaction、retention、annotation、dataset association、Golden Path 状态、read model 和索引边车。这些如果多个 writer 同时改同一份 manifest，会把当前最值钱的简单正确性打碎。

## 外部架构对标

这条路线不是照抄某个系统，但和主流分布式数据库的稳定模式一致：

- CockroachDB：数据按 range 分布，每个 range 通过 Raft 复制；官方文档明确一个 Raft group 里由 leaseholder/leader 负责服务读和发起写入。这对应 yiTrace 的“shard 内单 leader 写，系统层多 shard 并行写”。
  - https://www.cockroachlabs.com/docs/stable/architecture/replication-layer
  - https://www.cockroachlabs.com/docs/stable/architecture/life-of-a-distributed-transaction
- TiDB/TiKV：数据按 Region 分布，PD 记录 Region 分布和调度；Region 有多个 replica，组成 Raft Group，默认读写经过 Leader。这对应 yiTrace 后续的 `ShardRouter + shard leader + follower replay`。
  - https://docs.pingcap.com/tidb/stable/tidb-architecture/
  - https://docs.pingcap.com/tidb/stable/tidb-storage/
- MongoDB：用 sharding 解决大数据和高吞吐；每个 shard 通常部署为 replica set，replica set 内只有 primary 接收写入，secondary 复制 oplog。这对应“分片扩展吞吐，主备复制解决 HA”，不是 multi-writer 抢同一份数据。
  - https://www.mongodb.com/docs/manual/sharding/
  - https://www.mongodb.com/docs/manual/replication/
- Citus：应用连接 coordinator，coordinator 根据元数据把请求路由到单个 worker 或并行发给多个 worker 并汇总结果；worker failure 通过 PostgreSQL WAL streaming replication 兜底。这对应 yiTrace 的 `gateway/query coordinator + shard worker + WAL shipping`。
  - https://docs.citusdata.com/en/stable/get_started/concepts.html
  - https://learn.microsoft.com/en-us/postgresql/citus/cluster-management

因此，文档里的方案属于“经过行业架构模式对标后，结合 yiTrace 当前实现约束做的裁剪版”。它没有引入分布式事务数据库的完整复杂度，因为 TraceDB 的主写入是 append event，天然更适合先做 hash routing + shard-local consistency。

## 100 个用户同时使用 agent，当前架构是否支持

支持，但要分清“100 个用户”和“100 个 writer 修改同一份存储”不是一回事。

当前单机架构是：

- 多个 client / SDK / HTTP request 可以同时进来。
- 引擎内部对 WAL、MemTable、manifest commit 串行化。
- 读请求通过 snapshot 多读者并发，不和写路径共享同一个全局读锁。

所以“单写”不是“单用户”。它更像 SQLite / DuckDB 这类嵌入式存储里的单写者模型：并发请求可以很多，只是修改存储状态的临界区串行。

如果 100 个用户同时使用 agent，每个 agent 平均每秒产生几十个 span/event，当前单机模型通常是够的。真正的关键是：

- SDK 要批量 flush，不要每个 log event 都同步写一次。
- server 侧要把 ingest 请求批处理成 WAL append。
- trace detail / session detail 尽量按 trace/session 读，避免高频全库聚合。
- compaction / retention 不要在高峰期 aggressive 运行。

粗略判断：

- 100 个用户，每人 1 个 agent run，普通工具调用和 LLM span：单机可以先跑。
- 100 个用户，每人同时开多 agent、多工具、高频 log，并且 UI 同时做大量聚合：需要 L1 分片。
- 100 个企业租户、持续 7x24 写入、要求 HA：需要 L1 分片 + L2 主备复制。

因此第一版产品可以继续保留单机单写；但服务化产品应尽早把接口设计成“可分片”，避免后面改 API。

## 后期做分布式的升级路径

分布式升级不要一次性重写存储引擎。推荐按“接口抽象 → 进程内多 shard → 多进程 shard → 复制 → 再均衡”的顺序走。

### 阶段 1：先抽象单机引擎边界

目标：让上层不再直接依赖单个 `WriteCoordinator`，而是依赖一个 storage facade。

新增概念：

- `StorageMode::SingleNode`
- `StorageMode::Cluster`
- `ShardId`
- `ShardRouter`
- `TraceStorage` / `TraceQueryExecutor`

单机模式下：

- 只有一个 shard，名字可以是 `shard-0`。
- 旧 data dir 不迁移，直接挂到 `shard-0`。
- `EngineJsonApi` 继续可用，只是内部调用 facade。

这一阶段的关键收益是：API、Node 包、HTTP server 不需要知道底层是单机还是多 shard。

### 阶段 2：进程内多 shard 原型

目标：先验证路由、fanout、merge，不引入网络和复制复杂度。

目录结构：

```text
data/
  cluster.json
  shards/
    shard-0001/
      wal.log
      manifest.dat
      segments/
      attr_postings/
      vecindex/
      metadata.dat
    shard-0002/
      ...
```

一个进程里启动多个 `WriteCoordinator`，每个 shard 一个 data dir。写入路由：

- 有 `session_id`：`hash(tenant_id, session_id) -> shard`
- 没有 `session_id`：`hash(tenant_id, trace_id) -> shard`

这一步先不做动态扩容。shard 数启动时固定，例如 4/8/16。

必须补的测试：

- 同一 trace 的 start/log/end 都路由到同一 shard。
- 同一 session 的多轮 trace 默认在同一 shard。
- `traceSearch` 跨 shard fanout 后排序稳定。
- `traceAggregate` / `trajectoryGroups` / `storageStats` partial reduce 正确。
- 分页带 snapshot token，不重复、不漏。

### 阶段 3：多进程 shard server

目标：把 shard 从同进程拆成独立进程，可以部署到多台机器。

新增组件：

- `yitrace-gateway`：无状态入口，负责鉴权、租户上下文、路由。
- `yitrace-shard`：持有一个或多个 shard 的存储进程。
- `cluster registry`：记录 shard 到节点的映射。

第一版 registry 可以很简单：

```json
{
  "version": 1,
  "shards": [
    {"id": "shard-0001", "leader": "http://10.0.0.11:7878"},
    {"id": "shard-0002", "leader": "http://10.0.0.12:7878"}
  ]
}
```

不必一开始上 etcd/Consul。单机配置文件 + reload 就能覆盖私有化部署。SaaS 版再换成控制面服务。

### 阶段 4：跨 shard 查询协调

目标：让所有已有查询 API 在 cluster mode 下语义不变。

需要为每类查询定义 merge contract：

- `traceSearch`：每个 shard 返回局部 top-N，gateway 用 `(sort_key, trace_id, shard_id)` 做稳定 merge。
- `traceAggregate`：每个 shard 返回 partial group，gateway reduce count/sum/min/max/avg。
- `trajectoryGroups`：按 trajectory signature 合并桶，examples 做截断采样。
- `traceTrajectories`：按 trace sort merge。
- `storageStats`：计数和 bytes 累加，引用计数累加。
- `goldenPathHealth`：各 shard 统计 followed/extended/partial/deviated，gateway 合并分布。

分页必须引入 cluster snapshot token：

```json
{
  "snapshot": {
    "leaseId": "lease-42",
    "shards": [
      {"shardId": "shard-0001", "manifestVersion": 1024},
      {"shardId": "shard-0002", "manifestVersion": 998}
    ]
  }
}
```

否则跨 shard 分页会因为某个 shard 新写入而出现跳页、重复或漏数据。

当前实现状态：

- in-process cluster 已实现 server-side snapshot lease：首次查询会 pin 每个 shard 的 `yt_manifest::Snapshot`，响应返回 `snapshot.leaseId`。
- 后续请求带回同一个 `snapshot` 时，fanout merge 会复用 lease 中保活的 shard snapshot；即使中间发生写入，也能读到同一个旧 manifest 视图。
- lease book 当前是同进程内存结构，最多保留 64 个 lease；被挤出后返回 `409 snapshot_expired`，调用方应重拉第一页。
- 篡改 shard/version 会返回 `409 snapshot_mismatch`；坏 JSON token 返回 `400 bad_snapshot`。
- 当前 lease 固定的是 trace/span/segment manifest 视图；annotation/dataset/Golden Path 等 metadata 仍是独立元数据文件，尚未做版本化快照。

这已经比单纯 stale detection 更进一步，但还不是多进程分布式 snapshot lease。下一阶段 shard 拆进程后，需要让 gateway token 能映射到远端 shard lease，或让 shard 支持按 manifest version pin。

### 阶段 5：shard 复制和 HA

目标：解决单 shard 节点宕机。

先做 WAL shipping，不急着 Raft：

- leader 写 WAL 后异步发给 follower。
- follower replay WAL。
- sealed segment 和 `attr_postings/` 文件按 manifest version 同步。
- follower 暴露只读查询，返回已 apply 的 manifest version。

这能先获得：

- 热备。
- 读分流。
- 备份节点。

然后再升级为 Raft per shard：

- 只有 leader 接写。
- WAL entry 复制到 quorum 后 ack。
- follower 严格按 log order apply。
- leader 切换后继续使用同一个 shard id。

注意：即使用 Raft，也不要让同一 shard 多 leader 同时写 manifest。

### 阶段 6：再均衡和 shard split

目标：某个 shard 太大或太热时能迁移。

第一版避免在线 split，先支持 whole-shard move：

- 把 `shard-0003` 从节点 A 复制到节点 B。
- gateway 暂停该 shard 写入或短暂 drain。
- B catch up WAL。
- registry 切 leader 到 B。
- A 删除旧副本。

下一步再做 shard split：

- 只按 tenant 或 project 粒度 split，避免把一个 session/trace 拆开。
- split 后新 trace 写新 shard，老 trace 保持原 shard。
- 查询层通过 routing table 同时查老 shard 和新 shard。

不要第一版做 row-level re-sharding。trace 数据天然按 trace/session 有局部性，移动整租户/整 project 更简单。

### 阶段 7：控制面元数据独立

分布式后，下面这些可以逐步从 shard-local `metadata.dat` 拆到 control plane：

- tenant / project / API key / RBAC
- retention policy
- Golden Path registry
- dataset definition
- eval run config
- alert rule

trace/span 主数据仍留在 shard。控制面只存轻元数据和索引，不复制大 payload。

### 兼容当前单机/嵌入式模式

分布式能力不能破坏 `@yitrace/db` 的嵌入式定位。

建议拆成两个运行形态：

- Embedded mode：`YiTraceDB.open("./data")`，单 data dir，单写者，适合 Node/Electron/本地 agent memory。
- Server cluster mode：`yitrace-gateway + yitrace-shard`，多 shard，多进程/多机，适合 SaaS 和团队服务。

两者共享：

- wire event 格式。
- fold 语义。
- WAL/segment 编码。
- query JSON contract。
- eval/golden path/retention 的语义测试。

两者不同：

- embedded 不需要 cluster registry、snapshot token fanout、replication。
- cluster 必须有 shard id、routing table、query coordinator、复制和运维指标。

## 最小可行分布式版本

如果只做一个能跑的 V1，范围应该控制在：

1. 进程内多 shard。
2. 固定 shard 数。
3. hash `(tenant_id, session_id/trace_id)` 路由。
4. `traceSearch` / `traceAggregate` / `traceTrajectories` / `storageStats` fanout merge。
5. trace detail/session detail 单 shard 命中。
6. 不做复制，不做在线 split，不做 multi-leader。

这个版本已经能证明架构从“单机库”升级为“可横向扩展的 TraceDB 服务”，同时不会破坏底层存储正确性。

## 迭代记录

### 2026-07-04 迭代 1：单 shard facade

已完成：

- `EngineJsonApi` 增加显式 `StorageMode::SingleNode` 和 `ShardId`。
- 单机默认 shard id 为 `shard-0`。
- 新增 `EngineJsonApi::new_single_shard(coord, shard_id)`，方便后续把同一套 API 挂到 shard router。
- 新增 `GET /v1/cluster`、`GET /v1/cluster/shards`、`GET /v1/shards`，返回当前 storage mode、write model、shard key、shard id、manifest version、segment count、memtable rows。
- 不改底层 `WriteCoordinator`，不改 WAL/manifest/segment 正确性路径。

新增 eval：

- `single_shard_facade_reports_cluster_shape_and_keeps_indexed_search_path`
- 覆盖 cluster shape 返回、custom shard id、tenant 写入隔离、traceSearch 语义、attrs postings `attrs_postings+folded_verify` 路径、recover 后 sidecar cache 不预热、首次 indexed query 触发 sidecar load。

这一步只是分布式前置骨架，不是多 shard。下一步才进入进程内多 shard router。

### 2026-07-04 迭代 2：进程内固定多 shard router

已完成：

- `StorageMode::InProcessCluster`。
- `EngineJsonApi::new_in_process_cluster(Vec<(ShardId, Arc<WriteCoordinator>)>)`。
- `POST /v1/ingest` 在 cluster mode 下按稳定 hash 路由：
  - 优先 `(tenant_id, session_id)`，保证同一 session 的多轮 trace 默认同 shard。
  - 没有 session 时用 `(tenant_id, trace_id)`。
- `POST /v1/trace-search` 在 cluster mode 下 fanout 到所有 shard，合并 folded spans 后复用现有排序/分页 JSON contract。
- `POST /v1/trace-aggregate` 在 cluster mode 下 fanout 到所有 shard，合并 spans 后复用现有 bucket/reduce 逻辑。
- `GET /v1/cluster/shards` 在 cluster mode 下返回全部 shard 的 manifest version、segment count、memtable rows，routing 标记为 `hash_tenant_session_trace`。

新增 eval：

- `in_process_cluster_routes_ingest_and_merges_indexed_queries`
- 覆盖 3 个 durable shard、按 session 定向路由写入、每 shard flush 出独立 segment、cluster shape、跨 shard `traceSearch` fanout merge、跨 shard `traceAggregate` reduce、tenant 隔离、attrs postings `attrs_postings+folded_verify` 路径，以及 fanout 查询确实触达每个 shard 的 sidecar cache（load 或 hit 增长）。

当前取舍：

- 这是语义版 fanout：各 shard 返回完整匹配 spans，gateway 再排序/聚合。后续需要改成 shard-local top-N / partial aggregate，减少大结果集内存。
- trace detail/session detail 已在迭代 3 补 route-to-owner。
- 暂不支持动态扩 shard、在线 split、复制。

### 2026-07-04 迭代 3：trace/session detail route-to-owner

已完成：

- `EngineJsonApi` 在 cluster mode 下维护 `trace_owner` / `session_owner` 进程内 owner cache。
- `POST /v1/ingest` 计算 shard 后记录 trace/session owner；后续同一 trace 的记录优先按已知 trace owner 路由，避免同一 trace 因部分事件缺少 `session_id` 而被拆到不同 shard。
- `GET /v1/sessions/:id/turns` 在 cluster mode 下先查 session owner；cache miss 时逐 shard 探测，命中后回填 owner cache，再把请求委托给对应 single-shard API。
- `GET /v1/traces/:id`、`/snapshot`、`/steps`、`/spans`、`/spans/batch`、`/spans/:spanId` 在 cluster mode 下先查 trace owner；cache miss 时逐 shard 探测并回填，再命中单 shard。
- 单机模式不变；底层 WAL、manifest、fold、logEvents 读取路径不变。

新增 eval：

- `in_process_cluster_routes_detail_apis_to_owner_shard`
- 覆盖 3 个 durable shard、数据写入非 primary shard、冷 owner cache fallback、session turns、trace waterfall、snapshot、steps、span page、batch detail、span detail、logEvents round-trip、tenant 隔离。
- eval 先直接断言 primary shard 没有目标 trace，再通过 cluster API 成功查询详情，证明详情 API 不再依赖 primary shard。

当前取舍：

- owner cache 是进程内派生状态，不是持久路由表；API 重启后会通过 miss fanout 找回 owner。
- 详情接口当前仍以解析后的内部 `u64` trace/session id 为入口；外部 UUID id 会按稳定 hash 解析，后续如果要直接按 external id 做详情查询，需要补 external-id lookup。
- `GET /v1/sessions` 和 `GET /v1/traces` 列表页已在迭代 4 补 cluster fanout/page merge。

### 2026-07-04 迭代 4：trace/session 列表 fanout merge

已完成：

- `GET /v1/traces` 在 cluster mode 下 fanout 到所有 shard，复用每个 shard 原有的 `list_traces` / `list_traces_for_tenant_and_attrs` / metadata filter 语义，再按 `trace_id` 稳定合并。
- trace 列表会从每个 shard 读取当前可见 trace 的一等字段/attrs `fields`，全局 JSON contract 与单机保持一致。
- `GET /v1/sessions` 在 cluster mode 下 fanout 到所有 shard，复用每个 shard 原有的 `console_sessions_for_tenant` / attrs filter / annotation/dataset filter 语义，再按 `session_id` 降序合并和分页。
- 对异常场景保守处理：如果同一个 trace/session 出现在多个 shard，列表层会合并计数、token、cost、status 和 fields，避免 UI 出现明显重复行。
- 单机模式仍走原有路径；底层 session index、attrs sidecar、WAL/manifest 都不变。

新增 eval：

- `in_process_cluster_lists_traces_and_sessions_across_shards`
- 覆盖 3 个 durable shard、primary shard 只含一条 trace、cluster trace 列表返回全部 shard 数据、trace fields 输出、session 列表分页/排序、filter、tenant 隔离。
- eval 还断言 `GET /v1/traces?projectId=...&skill=...` 触达每个 shard 的 attrs sidecar cache（load 或 hit 增长），避免列表页退化成全量折叠慢路。

当前取舍：

- 列表页 fanout 是语义版：每个 shard 先返回完整局部列表，gateway 再合并/分页。后续大数据量下需要 shard-local limit + cluster cursor/snapshot token。
- `GET /v1/traces` 仍是非分页数组的旧 contract；后续应补分页和 snapshot token，避免超大租户一次拉全量。
- `GET /v1/sessions` 现在支持 cluster 全局 offset 分页，但还没有 cluster snapshot token；高并发写入下跨页可能看到不同 shard 版本。

### 2026-07-04 迭代 5：trajectory / storage 产品读模型 fanout merge

已完成：

- `POST /v1/trace-trajectories` 在 cluster mode 下 fanout 到所有 shard，复用每个 shard 的 traceSearch 过滤语义和 `materialized_trace_trajectory`，再按 `trace_id desc` 全局排序/分页。
- `POST /v1/trajectory-groups` 在 cluster mode 下按 shard 构建局部 trajectory buckets，再按完整 trajectory signature 合并 trace/span/error/duration/token/cost/scores/examples。
- `POST /v1/storage-stats` 在 cluster mode 下每个 shard 用自己的 snapshot 计算局部 `StorageStatsReport`，再合并 total 和 group buckets；trace/session id 用 set 合并，bytes 和 metadata 引用计数累加。
- cluster 响应增加 `queryMode:"fanout_merge"` 和 `shardCount`，方便 eval 和上层诊断确认走了分片查询路径。
- 单机模式保持原 JSON contract；底层 materialized trajectory cache、storage stats 估算、metadata 读取都不变。

新增 eval：

- `in_process_cluster_merges_trajectory_and_storage_read_models`
- 覆盖 3 个 durable shard、同一路径 signature 跨 shard 合并、`trace-trajectories` 全局排序、`trajectory-groups` trace/error/success 汇总、`storage-stats` total/group 汇总、tenant 隔离。
- eval 断言 `projectId + skill` 过滤触达每个 shard 的 attrs sidecar cache（load 或 hit 增长），避免产品读模型退化成全量折叠慢路。

当前取舍：

- 仍是语义版 fanout：trajectory 和 storage stats 先取完整局部结果再合并。大租户需要 shard-local topN、partial report limit 和背压。
- `trace-trajectories` 的分页是全局 offset 分页，尚无 cluster snapshot token；高并发写入下跨页可能看到不同版本。
- `storage-stats` 的 metadata 计数是 shard-local 累加；正常路由下一条 trace 只在一个 shard，不会重复。若未来支持跨 shard trace，需要引入 trace owner / metadata owner 去重。

### 2026-07-04 迭代 6：loop / task 产品读模型 fanout merge

已完成：

- `GET /v1/loops` 在 cluster mode 下 fanout 到所有 shard，复用每个 shard 的 attrs / annotation / dataset 过滤语义，再按 `loop_id` 聚合 trace、session、span、error、duration、usage、cost、phase、validator 和 examples。
- `GET /v1/loops/:id` 在 cluster mode 下按 `loop_id` + query filter 跨 shard 汇总，返回全局 summary、trace 摘要和 span 列表。
- `GET /v1/tasks/:fingerprint/traces` 在 cluster mode 下跨 shard 汇总同类 task trace 摘要，并保持 `trace_id desc` 排序和 offset 分页。
- cluster 响应增加 `queryMode:"fanout_merge"` 和 `shardCount`，方便上层诊断和 eval 校验。
- 单机模式保持原 JSON contract；底层 fold、attrs sidecar、metadata filter 都不变。

新增 eval：

- `in_process_cluster_merges_loop_and_task_read_models`
- 覆盖 3 个 durable shard、同一个 `loop_id` 跨 shard 聚合、task trace 全局分页、`validationStatus` 过滤、loop detail 汇总、tenant 隔离。
- eval 断言 `projectId + taskFingerprint` 查询触达每个 shard 的 attrs sidecar cache（load 或 hit 增长），避免 loop/task 读模型退化成 primary shard 或未过滤慢路。

当前取舍：

- loop/task 仍是语义版 fanout：先收集局部匹配 spans，再在 gateway 聚合。大租户需要 shard-local partial loop/task buckets 和全局 top-N merge。
- cluster 分页仍是 offset 分页，尚无 snapshot token；高并发写入下跨页可能看到不同 shard 版本。
- annotation / dataset / Golden Path / retention policy 这类 metadata 写入目前仍在 primary coordinator 路径，后续要决定 co-locate 到 trace owner，还是拆到 control plane。

### 2026-07-04 迭代 7：annotation / dataset metadata co-location

已完成：

- `POST /v1/annotations` 在 cluster mode 下按 source trace owner shard 写入 annotation；owner cache 丢失时会通过 trace detail fanout 找回 owner，找不到才按 `(tenant_id, trace_id)` hash 兜底。
- `POST /v1/dataset-associations` 使用同样的 owner shard co-location 策略，保证 dataset association 与 source trace 在同一个 shard。
- cluster metadata public id 增加 shard 前缀，单机仍保留从 1 开始的本地自增 id，避免不同 shard 的 annotation/dataset id 撞号后 update/delete 找错记录。
- `GET /v1/annotations` 和 `GET /v1/dataset-associations` 在 cluster mode 下 fanout 到所有 shard，全局按 created time / id 倒序排序分页，并返回 `queryMode:"fanout_merge"` 和 `shardCount`。
- `PATCH/POST /v1/annotations/:id/status` 与 `DELETE /v1/annotations/:id` 在 cluster mode 下按 public id 扫 shard 定位记录。
- 因为 metadata 与 trace co-locate，`traceSearch` / `traceAggregate` / loop/task/trajectory/storage 等复用 metadata filter 的 cluster 查询可以在每个数据 shard 本地完成 annotation/dataset 反向过滤。

新增 eval：

- `in_process_cluster_colocates_metadata_with_trace_owner`
- 覆盖 3 个 durable shard、source trace 非 primary、annotation/dataset 写入 owner shard、primary 不落 metadata、cluster GET fanout、annotation/dataset 反向过滤 traceSearch、update/delete 跨 shard 定位、deleted annotation 默认不再命中、tenant 隔离。

当前取舍：

- annotation/dataset 采用 co-locate，而不是 control plane store。优点是 traceSearch metadata 过滤 shard-local；缺点是全局 metadata 列表需要 fanout。
- metadata id 的 shard 前缀是 in-process cluster 原型策略；未来多进程/多机需要把 shard id 到 id prefix 的分配写进 cluster registry，不能依赖 Vec 下标。
- Golden Path 已在迭代 8 迁到 source trace owner；retention policy 仍暂时是 primary/control-plane 路径。

### 2026-07-04 迭代 8：Golden Path source-owner co-location

已完成：

- `POST /v1/golden-paths` 在 cluster mode 下按 source trace owner shard 写入 Golden Path，source trace 不在 primary shard 时也能创建候选资产。
- cluster Golden Path public id 增加 shard 前缀，单机仍保持本地自增 id，避免不同 shard 的 `golden_path_id` 撞号后状态更新找错记录。
- `GET /v1/golden-paths` 在 cluster mode 下 fanout 到所有 shard，全局排序并返回 `queryMode:"fanout_merge"` / `shardCount`。
- `POST /v1/golden-paths/:id/status` 在 cluster mode 下按 public id 跨 shard 定位并更新状态。
- `POST /v1/path-adherence` / `/v1/golden-path-evidence` / `/v1/golden-path-export` / `/v1/golden-path-health` 支持 Golden Path、source trace、candidate trace 分布在不同 shard。
- evidence 内的 source annotation / dataset association 走 fanout，因此与 trace owner co-locate 的后验证据能被导出和健康检查看到。

新增 eval：

- `in_process_cluster_colocates_golden_path_with_source_trace_owner`
- 覆盖 source trace 非 primary、candidate trace 另一个 shard、Golden Path 写入 source owner shard、primary 不落 Golden Path、fanout list、status update、path adherence、evidence annotation/dataset、confirmed export、health 统计和 tenant 隔离。

当前取舍：

- Golden Path 选择 co-locate 到 source trace owner，而不是 control plane store。这样 source evidence 和 retention 保护更局部，但全局 Golden Path 列表/export/health 需要 fanout。
- Golden Path id 前缀沿用 in-process cluster 的 shard 下标策略；多进程版本需要 registry 管理稳定 id prefix。
- retention dry-run/apply/audit 已在迭代 9 补 cluster fanout；retention policy 仍暂时是 primary/control-plane 路径，run-due 执行时会走 cluster retention apply。

### 2026-07-04 迭代 9：retention plan/apply shard-local fanout

已完成：

- `POST /v1/retention-plan` 在 cluster mode 下 fanout 到所有 shard，每个 shard 复用自己的 traceSearch 过滤、attrs postings、metadata 保护和 trace time bounds，再把 candidates/protected/deletable 统计合并。
- `POST /v1/retention/apply` 在 cluster mode 下按 shard-local deletion vector 删除 segment rows；仍保留单 shard 的热 trace skip 保护，避免删除 MemTable/WAL tail 里的半条 trace。
- retention apply 的 audit 改为每个 shard 写一条本地审计，public `auditId` 增加 shard 前缀，避免 fanout 查询时 id 撞号。
- `GET/POST /v1/retention-audits` 在 cluster mode 下 fanout merge，返回 `queryMode:"fanout_merge"` 和 `shardCount`。
- cluster retention 响应保留单机顶层字段，同时增加 `audits` 和 `shards`，方便排查每个 shard 的 protected/deletable/apply/compact 结果。
- 单机模式保持原有 JSON contract；底层 WAL、manifest、deletion vector、compaction 和 GC 逻辑不变。

新增 eval：

- `in_process_cluster_applies_retention_per_shard_and_fanout_audits`
- 覆盖 3 个 durable shard、跨 shard old traces、annotation 保护、dry-run 全局 candidates/protected/deletable 汇总、apply 删除两片 shard 的 segment rows、protected trace 留存、audit id 前缀、audit fanout 查询和 tenant 隔离。

当前取舍：

- retention apply 是 shard-local fanout，不提供跨 shard 原子事务。某个 shard apply 失败时，当前原型会返回错误；后续多进程版本需要 policy run id、幂等重试和 per-shard 状态表。
- retention policy 仍存 primary/control-plane；`run-due` 执行具体 policy 时会调用 cluster retention apply。后续如果多进程部署，需要把 policy/audit control plane 独立出来，或给 policy id 也引入稳定 registry 前缀。
- cluster retention 仍没有 snapshot token；dry-run 与 apply 如果隔很久，中间写入可能改变候选集。正式版应支持 dry-run plan id 或 policy run snapshot。

### 2026-07-04 迭代 10：`/v1/search` fanout merge

已完成：

- `POST /v1/search` 在 cluster mode 下 fanout 到所有 shard，分别执行 shard-local text / vector / hybrid 检索，再按 score 全局排序、去重和截断。
- 单机模式保持原数组 JSON contract；cluster mode 也继续返回数组，避免破坏 `@yitrace/db.search()` / 旧 HTTP client 的兼容性。
- text 搜索继续走 shard-local BM25 + attrs filter；vector 搜索继续走 shard-local graph filter；hybrid 搜索继续使用每个 shard 的 RRF 结果。

新增 eval：

- `in_process_cluster_fanout_merges_db_search_endpoint`
- 覆盖 3 个 durable shard、`text` / `vector` / `text+vector hybrid` 三条搜索路径、`projectId/skill` attrs filter 和 tenant 隔离。

当前取舍：

- cluster `/v1/search` 的排序是 shard-local top-k 后全局 merge。BM25 与 hybrid RRF 的分数还不是全局归一化，初版先保证“不漏 shard”；后续大规模版本应做 shard-local overfetch、score normalization 或 coordinator-level RRF。
- 为兼容旧 contract，cluster `/v1/search` 默认仍返回旧数组；迭代 24 已补 `includeFanout:true` 诊断 envelope，可显式返回 `queryMode`、`shardCount`、`okShards`、`degraded` 和 `failedShards`。

### 2026-07-04 迭代 11：OTLP/OpenInference ingest 复用 shard router

已完成：

- `POST /v1/traces` 在 cluster mode 下不再直接写 primary coordinator，而是先用 `parse_otlp_traces` 转成 `WireRecord`，再复用 `/v1/ingest` 的 shard router。
- OTLP start/end 双事件保持同 trace owner：start event 可按 `yitrace.session_id` 路由，后续同 trace event 走 owner cache，避免一个 OTLP span 被拆到不同 shard。
- HTTP tenant header 仍覆盖 OTLP attributes 里的 `yitrace.tenant_id`，多租户安全边界保持不变。
- 单机模式继续返回 OTLP 约定的 `{"partialSuccess":{}}`，行为兼容。

新增 eval：

- `in_process_cluster_routes_otlp_ingest_to_owner_shard`
- 覆盖 3 个 durable shard、`yitrace.session_id` 路由到非 primary、body tenant spoof 被 header 覆盖、owner shard 折叠 span、cluster `/v1/search` 查回、trace detail/span detail 查回、tenant 隔离。

当前取舍：

- OTLP cluster ingest 仍是进程内 fanout，单个请求跨多个 shard 时没有跨 shard 原子事务。正式多进程版本需要 per-record 幂等 event_id 和 per-shard retry 结果。
- OTLP response 仍保持标准 partialSuccess 空对象；后续如果要暴露 shard 写入诊断，应放到 debug header 或 v2 endpoint，避免破坏 OTLP exporter 兼容性。

### 2026-07-04 迭代 12：拆分大实现，控制单元规模

已完成：

- `EngineJsonApi` 不再把所有 HTTP/embedded JSON API 逻辑堆在 `http.rs`：
  - route/core、列表、搜索、console、metadata、retention、trajectory、Golden Path 等按职责拆到 `src/http/*.rs`。
  - trace/search/storage/retention/trajectory 等共享 helper 按职责拆成小分片，保持原 module 作用域，避免可见性 churn。
  - HTTP 单测拆成 `src/http/tests/part_*.rs`，最大分片低于 800 行。
- `WriteCoordinator` 的大 impl 拆成多个职责 impl：
  - open/ingest、read/query、trace views、metadata/eval、tree/graph、search、recovery/commit、metrics/migrate。
  - 每个 impl 分片低于 800 行，避免继续形成新的维护热点。
- `lib.rs` 内部测试模块拆成 `src/lib/tests/part_*.rs`，最大分片低于 800 行。

验证：

- `cargo fmt --all`
- `cargo test --offline -p yt-engine --lib`
- 结构扫描：`yt-engine` src/tests 中没有单个 top-level impl/function/test module 超过 800 行。

当前取舍：

- 这次只做源码组织，不改 WAL、manifest、shard route、fanout merge 或 metadata co-location 行为。
- helper 分片仍保持在原 module 作用域，避免一次性引入大量 `pub(super)` 字段和 re-export；后续如果某一组 helper 稳定下来，再升级成真正的子模块 API。

### 2026-07-04 迭代 13：抽出 TraceStorage / ShardRouter 第一层边界

已完成：

- 新增 `TraceStorage` trait，作为 `EngineJsonApi` 到底层 shard/coordinator 的第一层 storage facade。
- 新增 `LocalTraceStorage`，承接当前 single-node 和 in-process cluster 两种模式。
- 新增 `ShardRouter`，把 trace/session owner cache、hash route 和 owner 回填从 `EngineJsonApi` 挪到 storage 层。
- `EngineJsonApi` 现在持有 `Arc<dyn TraceStorage>`，通过 `coord()` / `shards()` 过渡访问器兼容现有 API 模块。
- 写入路由、OTLP ingest、trace/session detail owner 查找和 metadata co-location 语义保持不变。

验证：

- `cargo fmt --all`
- `cargo test --offline -p yt-engine --test eval_harness single_shard_facade_reports_cluster_shape_and_keeps_indexed_search_path -- --nocapture`
- `cargo test --offline -p yt-engine --test eval_harness in_process_cluster_routes_detail_apis_to_owner_shard -- --nocapture`
- `cargo test --offline -p yt-engine --test eval_harness in_process_cluster_routes_otlp_ingest_to_owner_shard -- --nocapture`

当前取舍：

- 这一步仍是单进程 storage facade，不是多进程分布式。
- 现有 API 模块仍有很多 fanout merge 逻辑直接遍历 `shards()`；后续要继续把 query coordinator 能力从 HTTP JSON 层向 storage/query 层下沉。
- 下一步应做跨 shard snapshot token / query merge contract，而不是直接引入 Raft。

### 2026-07-04 迭代 14：cluster snapshot lease / fixed-version read

已完成：

- in-process cluster 的 `sessions`、`traceSearch`、`traceAggregate`、`trajectoryGroups`、`traceTrajectories`、`storageStats`、`loops`、`taskTraces` 返回 `snapshot` token。
- 首次查询会 pin 每个 shard 的 `yt_manifest::Snapshot` 并生成 `leaseId`；后续请求带回同一个 token 时，fanout merge 复用旧 shard snapshot。
- 写入发生在两次分页之间时，旧 lease 仍读旧 manifest 视图，避免跨 shard 分页跳页、重复或漏数据。
- token 支持 JSON body 的 `snapshot` / `snapshotToken` / `snapshot_token`，GET 查询串也支持同名参数。
- token 篡改返回 `409 snapshot_mismatch`；lease 被 LRU 挤出返回 `409 snapshot_expired`；坏 JSON 返回 `400 bad_snapshot`。

新增 eval：

- `in_process_cluster_lists_traces_and_sessions_across_shards` 扩展 snapshot lease 覆盖。
- 覆盖 page1 token、page2 stable read、中途写入后旧 lease 仍读旧视图、新查询读到新视图、token 篡改、超过 64 个 lease 后过期。
- 既有 traceSearch / trajectory / storage / loop / task eval 继续覆盖 token 接受和 mismatch。

当前取舍：

- 这是同进程 lease book，不是远程 shard lease。
- lease 固定的是 trace/span/segment manifest 视图；annotation/dataset/Golden Path/retention policy 仍是独立 metadata 文件，没有版本化。
- 多进程版本需要 gateway token 映射到远端 shard lease，或 shard read path 支持按 manifest version pin。

### 2026-07-04 迭代 15：WAL tail shipping follower primitive

已完成：

- `WriteCoordinator::replication_status()` 暴露 shard 的 `committed_tail`、manifest version、memtable watermark、memtable rows 和 segment count。
- `WriteCoordinator::export_wal_after(from_lsn)` 从 leader WAL 导出已提交的增量记录。
- `WriteCoordinator::apply_wal_replication_batch(batch)` 在 follower 上按 LSN 顺序应用增量：
  - 完整重复批次幂等 no-op。
  - 部分重叠批次跳过已应用前缀，只追加缺失后缀。
  - follower tail 小于 `from_lsn` 时拒绝，返回 replication gap。
  - 应用后同步更新 follower WAL、MemTable、BM25/attrs/vector 以外的派生索引、trace trajectory cache invalidation 和 committed tail。
- follower 使用自己的 durable WAL；重启后 `recover()` 能从 follower WAL 读回已复制但尚未 flush 的 tail。
- 已验证 `backup_snapshot()` 可作为 follower bootstrap：leader 已 flush 的 segment/manifest 先通过在线快照复制到 follower，之后 follower 从自身 `committed_tail` 继续追 leader WAL tail。

新增 eval：

- `shard_wal_shipping_follower_replays_incrementally_and_recovers`
- 覆盖 leader 两批写入、follower 增量应用、combined batch 的部分重叠重试、重复批次幂等、LSN gap 拒绝、follower 重启恢复和 traceSearch 可见性。
- `shard_snapshot_bootstrap_then_wal_catchup_covers_flushed_segments`
- 覆盖 leader flush 后的 segment/manifest bootstrap、follower 从 committed tail 追 WAL tail、segment + WAL tail 混合读、follower 重启恢复。

当前取舍：

- 这是 WAL tail shipping 的本地原语，不包含网络传输、shard registry、leader/follower 角色管理或 failover。
- 还没有持续复制 sealed segment 文件、manifest、attr sidecar、vecindex、metadata 和 GC log；当前只验证了用在线 snapshot 做初始 bootstrap，完整 HA 仍需要增量 segment/manifest sync。
- follower read staleness policy 的底层判定已在迭代 17 补齐；后续还需要把它接入 gateway 的 follower read route。

### 2026-07-04 迭代 16：replica freshness status 暴露

已完成：

- `/v1/cluster/shards` 每个 shard 现在暴露复制/读取水位：
  - `committedTail`
  - `memtableWatermark`
  - `readable`
  - `syncState`
  - `replicationLagLsn`
- 当前 in-process cluster 里的 shard 都是 leader，所以 `syncState="leader"`、`replicationLagLsn=0`、`writable=true`。
- 这些字段来自 `WriteCoordinator::replication_status()`，后续多进程 shard server 可以沿用同一 JSON contract 报告 follower 水位。

新增 eval：

- `single_shard_facade_reports_cluster_shape_and_keeps_indexed_search_path` 扩展断言单 shard 的 committed tail、watermark、readable、sync state 和 lag。
- `in_process_cluster_routes_ingest_and_merges_indexed_queries` 扩展断言每个 shard 的 committed tail、watermark 和 zero lag。

当前取舍：

- 这只是可观测和 gateway 决策底座，还没有实现 follower 读路由。
- 真正 follower 需要把 `leaderCommittedTail` / `replicaCommittedTail` / lag policy 从 shard registry 或 gateway 注入；当前 leader-only 状态无法表达远端复制拓扑。

### 2026-07-04 迭代 17：follower read staleness policy

已完成：

- 新增 `ReplicationStatus::replica_read_decision(leader, max_lag_lsn)`，把 follower 是否可读的水位判断固化到底层：
  - `max_lag_lsn=0`：严格读，要求 follower 追到 leader committed tail。
  - `max_lag_lsn>0`：允许 bounded stale read。
  - follower tail 超过 leader、manifest version 超过 leader、watermark 超过 tail 等异常状态判为 `diverged`。
- 新增 `ReplicaReadDecision`，返回 `readable`、`sync_state`、`replication_lag_lsn` 和 `reason`，供后续 gateway/shard server 直接暴露或路由。

新增 eval：

- `shard_wal_shipping_follower_replays_incrementally_and_recovers` 扩展读新鲜度策略：
  - follower 落后 2 LSN 时，`max_lag_lsn=0` 返回 stale/不可读。
  - `max_lag_lsn=2` 返回 catching_up/可读。
  - follower catch-up 后返回 ready/可读。
  - 构造 tail 超过 leader 的状态，返回 diverged/不可读。

当前取舍：

- follower 已在迭代 18 注册到 `TraceStorage` 和 `/v1/cluster/shards` 的 replica 列表。
- gateway 接入时要按 API 类型选择策略：trace detail / read-your-write 默认 `max_lag_lsn=0`，搜索/聚合可以显式允许 bounded stale。

### 2026-07-04 迭代 18：in-process follower topology registry

已完成：

- 新增 `InProcessShardSpec` / `InProcessReplicaSpec`，让 in-process cluster 可以显式表达一个 shard 的 leader 和 follower。
- `LocalTraceStorage` 的 `ShardBackend` 现在可携带 `replicas`，但旧的 `new_in_process_cluster(Vec<(ShardId, WriteCoordinator)>)` 仍保持兼容。
- `/v1/cluster/shards` 每个 leader shard 现在返回：
  - `replicaCount`
  - `replicas[]`
- 每个 follower replica 返回：
  - `replicaId`
  - `role="follower"`
  - `writable=false`
  - `readable`
  - `manifestVersion`
  - `committedTail`
  - `memtableWatermark`
  - `segmentCount`
  - `memtableRows`
  - `syncState`
  - `replicationLagLsn`
  - `reason`
  - `maxLagLsn`
- follower 的 `readable/syncState/replicationLagLsn/reason` 直接复用 `ReplicationStatus::replica_read_decision(leader, max_lag_lsn)`。

新增 eval：

- `distributed_replica_eval::cluster_shard_status_reports_follower_freshness_budget`
  - follower 落后 leader 2 LSN 且 `maxLagLsn=0` 时，cluster status 返回 stale/不可读。
  - `maxLagLsn=2` 时，同一个 follower 返回 catching_up/可读。
  - follower 追平后返回 ready/可读、lag 为 0。

当前取舍：

- `/v1/search` 已在迭代 19 接入 eventually-consistent follower read route。
- 其他查询 API 仍默认读 leader；后续要按 endpoint 语义、tenant/project 一致性要求和 `readable` 状态选择 follower。
- 仍没有远程 shard registry、心跳、角色切换或自动 failover。

### 2026-07-04 迭代 19：bounded-stale search follower read route

已完成：

- 新增 `eventually_consistent_read_coord_for_shard()`，用于给可接受陈旧读的 fanout 查询选择读目标：
  - 优先选择 `readable=true` 且 lag 最小的 follower。
  - 没有可读 follower 时回落 leader。
  - 当前只接入 `/v1/search`。
- `/v1/search` 的 cluster fanout 现在按 shard 选择读目标；这让高频召回查询可以先被 follower 承接。

新增 eval：

- `distributed_replica_eval::cluster_search_uses_readable_follower_and_falls_back_when_stale`
  - leader 写入第二条 trace 后，follower 落后 2 LSN。
  - `maxLagLsn=2` 时，`/v1/search` 走 lagging follower，搜不到 leader-only 新 trace。
  - `maxLagLsn=0` 时，follower 不可读，`/v1/search` 回落 leader，可以搜到新 trace。
  - follower 追平后，宽松策略也能搜到新 trace。

当前取舍：

- `/v1/search` 走 eventually-consistent read；它是召回型接口，天然能接受调用方显式配置的 bounded stale。
- `traceSearch` 和 `traceAggregate` 已在迭代 20 接入 follower-target snapshot lease。
- `sessions`、`trajectoryGroups`、`traceTrajectories`、`storageStats` 等带 snapshot lease 的接口仍读 leader。
- trace/session detail、metadata 写后读、retention/golden path 仍必须保持 leader 或 fixed-version 语义。

### 2026-07-04 迭代 20：follower-target snapshot lease

已完成：

- `ClusterSnapshotReadSet` 不再只保存 snapshot；现在每个 shard entry 同时保存：
  - 实际读取的 `WriteCoordinator`
  - 对应的 `yt_manifest::Snapshot`
  - snapshot token 中的 `readTarget`
- leader snapshot token 保持旧 JSON 外形；读 follower 时才额外输出：

```json
{"shardId":"tenant-712-shard-0","readTarget":"tenant-712-shard-0-follower-0","manifestVersion":0}
```

- `traceSearch` / `traceAggregate` 的 cluster fanout 改为：
  - 首次请求：选择可读 follower 并 pin follower snapshot。
  - 后续请求带 `leaseId`：复用同一个 follower `coord + snapshot`。
  - token 被篡改时仍返回 `409 snapshot_mismatch`。
- 旧 token 仍兼容：没有 `readTarget` 的 token 表示 leader read target。

新增 eval：

- `distributed_replica_eval::trace_search_snapshot_lease_pins_follower_read_target`
  - 首次 `trace-search` 在 follower 落后 2 LSN 且 `maxLagLsn=2` 时读 follower，返回 `total=0`，snapshot token 带 `readTarget`。
  - follower 追平后，用旧 token 继续查仍返回 `total=0`，证明 lease pin 住旧 follower snapshot。
  - 不带旧 token 的新请求能看到新 trace。
  - `trace-aggregate` 同样覆盖 stale/fresh 两种 follower snapshot 读。

当前取舍：

- 这是 in-process lease；远程 shard server 还需要把 `leaseId/readTarget` 映射到远端 follower 的 snapshot lease。
- metadata 文件还没有进入 snapshot token，annotation/dataset/golden path 反查仍不适合读 follower。
- 其他带 snapshot 的 read model 暂时仍读 leader，避免在没有逐接口语义评估前扩大陈旧读面。

### 2026-07-04 迭代 21：真实多进程 shard eval

已完成：

- 新增 `distributed_process_eval` 集成测试，不再只用 in-process cluster。
- 父测试进程会启动 3 个真实 OS 子进程；每个子进程：
  - 独立 `open_durable(data_dir)`
  - 独立 `TcpListener`
  - 独立 `HttpIngestServer`
  - 独立 WAL / manifest / segment / metadata 文件
- 子进程入口复用同一个 test binary 的 ignored test：`shard_server_child_process`。父进程通过 env 传入 data dir 和 bind address。
- 测试覆盖：
  - 3 个 shard server 经 TCP `/v1/ingest` 分别写入。
  - 父进程作为 test-side query coordinator，对 3 个 HTTP shard 做 fanout。
  - `/v1/trace-search` 在每个真实实例上返回 shard-local 数据，fanout 合并后覆盖 3 个实例。
  - `X-Tenant-Id` 隔离在多实例下仍有效。
  - kill/restart 其中一个 shard server 后，durable WAL/manifest 恢复，数据仍可查。
- `server_durable` 示例支持 `YT_BIND`，可以手工起多个 durable server：

```bash
YT_BIND=127.0.0.1:7879 cargo run -p yt-engine --example server_durable -- /tmp/yitrace-shard-0
YT_BIND=127.0.0.1:7880 cargo run -p yt-engine --example server_durable -- /tmp/yitrace-shard-1
```

当前取舍：

- 这是真多进程/真 socket/真 durable 文件的 eval，但还不是完整生产 gateway。
- 父测试里的 fanout coordinator 已在迭代 22 补成独立 gateway 子进程。
- 这轮不包含跨进程 WAL shipping；leader/follower 的网络复制协议仍是后续需求。

### 2026-07-04 迭代 22：真实 gateway 进程 eval

已完成：

- `distributed_process_eval` 新增 gateway 子进程入口：`gateway_server_child_process`。
- 测试会启动：
  - 3 个真实 shard server 进程
  - 1 个真实 gateway 进程
- 测试请求只打 gateway TCP 端口；gateway 再通过 HTTP 访问 shard server。
- gateway eval 覆盖：
  - `POST /v1/ingest`：gateway 解析 wire batch，按 `hash(tenant_id, session_id/trace_id)` 路由到不同 shard。
  - `POST /v1/trace-search`：gateway fanout 到所有 shard，合并 `items` 和 `total`。
  - 直接查各 shard，确认 3 条 trace 被分散到 3 个真实实例，而不是落到同一个进程。
  - `X-Tenant-Id` 从 gateway 透传到 shard，跨租户仍查不到。
  - gateway `/v1/cluster/shards` 暴露 process-gateway 拓扑，用于 readiness 和调试。

当前取舍：

- 这是 test gateway，不是生产 gateway：路由策略、错误处理和 merge 只覆盖当前 eval 的最小合同。
- 部分失败的 eval 已在迭代 23 补齐；生产 gateway 仍需要正式抽象：routing table、health check、retry、timeout、对外错误合同、snapshot lease 跨进程映射。
- 这轮仍不包含跨进程 WAL shipping / follower 复制。

### 2026-07-04 迭代 23：真实 gateway 部分失败 eval

已完成：

- `distributed_process_eval` 新增 `gateway_process_query_reports_partial_shard_failure`。
- 测试会先启动 3 个真实 shard server 进程和 1 个真实 gateway 进程，通过 gateway 写入 3 条分别落到不同 shard 的 trace。
- 随后 kill 掉其中 1 个 shard 进程，再只通过 gateway 查询 `/v1/trace-search`。
- 查询类 fanout 现在验证降级语义：
  - 只要还有 shard 成功，gateway 返回 `200`。
  - 返回体带 `degraded:true`、`okShards` 和 `failedShards`。
  - `total` 只统计成功 shard 的结果，不会把失败 shard 的数据静默算进去。
  - 失败 shard 的 index、状态码和连接错误会进入 `failedShards`，便于上层判断结果不完整。
  - `/v1/cluster/shards` 会把宕掉的 shard 报成 `httpStatus:0`。
  - 降级查询下 `X-Tenant-Id` 隔离仍然成立。

当前取舍：

- 写入类请求仍保持 fail-fast：任一目标 shard 不可达时返回错误，避免业务侧误以为批量写入完整成功。
- 这仍是 test gateway 的 eval 合同；生产 gateway 还要补 timeout、retry、熔断、错误预算、partial/strict query 策略和跨 shard snapshot token 映射。
- 这轮没有引入自动 failover，也没有让 gateway 改写路由避开失败 shard。

### 2026-07-06 迭代 24：fanout 诊断合同标准化

已完成：

- 新增 `FanoutReport` / `FanoutShardFailure` helper，统一生成 fanout 诊断 JSON 字段：
  - `shardCount`
  - `okShards`
  - `degraded`
  - `failedShards`
- in-process cluster 的 `/v1/trace-search` 和 `/v1/trace-aggregate` 现在默认返回这些字段；当前同进程 fanout 都是 `degraded:false`，但字段合同已经和真实 gateway 部分失败 eval 对齐。
- `/v1/search` 是原始 API，默认保持旧数组响应，避免破坏 `@yitrace/db.search()` 和已有调用方；显式传 `includeFanout:true` 时返回 envelope：
  - `items`
  - `total`
  - `queryMode:"fanout_merge"`
  - fanout 诊断字段
- eval 已覆盖：
  - `traceSearch` / `traceAggregate` cluster response 带 `okShards=3`、`degraded=false`、`failedShards=[]`。
  - `/v1/search` 在 `includeFanout:true` 时返回诊断对象。
  - `/v1/search` 默认仍是 legacy array shape。
  - follower read route 的 `/v1/search` 也可通过 `includeFanout:true` 观察 bounded-stale 查询完整性。

当前取舍：

- helper 先覆盖成功 fanout 的标准字段；真实失败路径仍由多进程 gateway eval 覆盖。
- 生产 gateway 仍需要把远程 shard timeout、retry、熔断和 strict/partial policy 接到同一个 `FanoutReport` 合同。
- 其他 cluster read models 还没有全部补 `okShards/degraded/failedShards`，后续应逐步统一，避免每个 API 自己拼诊断字段。

### 2026-07-06 迭代 25：ShardClient 边界

已完成：

- 新增 `ShardClient` trait，定义 shard-local 的统一远程化边界：
  - `route_with_tenant`
  - `ingest_wire_for_tenant`
  - `search_hits`
  - `replication_status`
- 新增 `LocalShardClient`，用现有 `WriteCoordinator` 实现这个 trait。
- `ShardBackend` / `ShardReplicaBackend` 同时保留：
  - `coord`：继续服务单机、本地 snapshot lease、详情页等热路径。
  - `client`：供后续 gateway/remote shard/fanout error policy 使用。
- cluster `/v1/search` 已从直接调用 `WriteCoordinator` 改为走 `ShardClient`，并把 client 错误汇入 `FanoutReport`。
- 新增 HTTP 内部 eval：fake failing shard client 覆盖所有 shard search 失败时返回 `503`，并带 `queryMode:"fanout_merge"`、`okShards:0`、`degraded:true`、`failedShards`。

当前取舍：

- 远程 shard client 已在迭代 26 补齐 HTTP route/ingest/status 边界；retry、熔断和健康检查仍未做。
- `traceSearch` / `traceAggregate` 仍依赖同进程 snapshot lease 和本地 `coord`；远程化要先设计跨进程 snapshot token。
- 单机默认路径仍走 `EngineJsonApi::new(coord)` 和本地 `WriteCoordinator`，不会引入 gateway 依赖。

### 2026-07-06 迭代 26：RemoteShardClient 完整 HTTP 边界

已完成：

- `RemoteShardClient` 从内部读路径 client 提升为可复用的远端 shard HTTP client，并从 crate root 导出。
- 当前支持分布式 gateway 必需的完整 shard 边界：
  - `ingest_records_for_tenant()`：序列化 `WireRecord`，POST `/v1/ingest`，通过 `X-Tenant-Id` 保持租户上下文。
  - `route_json_with_tenant()`：透传任意 shard-local HTTP JSON API，供 gateway 实现 trace-search、aggregate、trajectory、metadata 等远端 fanout。
  - `search_hits()`：typed fast path，POST `/v1/search`，复用原始 search JSON body。
  - `replication_status_snapshot()` / trait `replication_status()`：GET `/v1/cluster/shards`，解析 manifest/WAL/memtable/segment 状态。
- search 响应解析同时兼容 legacy array 和 `{items:[...]}` envelope；只还原 fanout merge 必需的 `trace_id`、`span_id`、`score`、`status`、`duration_ns`、`agent_name`、`logs`、`fields`、`attrs` 等字段。
- 新增真实 socket eval：启动 `HttpIngestServer`，`RemoteShardClient` 通过 TCP 完成 ingest/status/search，并验证 tenant 7 可见、tenant 8 不可见。
- 真实 gateway 进程 eval 已切到 `RemoteShardClient`：gateway 不再手写 shard HTTP/wire serializer，而是通过正式 client 完成写入路由、trace-search fanout、部分失败降级和 cluster status 查询。
- `SearchJsonRequest` 保留 `raw_body`，为远端 shard 转发原始查询合同，避免 gateway 重新拼 JSON。

当前取舍：

- `RemoteShardClient` 还没有接入默认 cluster 构造器；单机和 in-process cluster 默认行为不变。
- 当前补齐的是 shard HTTP 边界和真实 gateway 进程 eval，不等于完成生产控制面。
- 仍未完成：远程 snapshot lease、生产 gateway 路由表、health check、retry/backoff、circuit breaker、角色/租约管理、自动 failover、网络复制以及 strict/partial query policy。
- 当前 HTTP client 是 std-only 基础实现；生产实现需要连接池、超时分级、错误分类和可观测指标。

### 2026-07-06 迭代 27：RemoteShardGateway facade

已完成：

- 新增 `RemoteShardGateway`，从 crate root 导出。
- 这个 facade 不负责 socket accept、鉴权、限流或 TLS，只负责分布式核心数据路径：
  - `POST /v1/ingest`：解析 wire batch，按和 in-process cluster 一致的 `hash(tenant_id, session_id/trace_id)` 分组，再通过 `RemoteShardClient::ingest_records_for_tenant()` 并发写入远端 shard；部分失败返回 `partialSuccess`、`okShards`、`failedShards` 和 `retrySafe:"event_id_dedup"`。
  - `POST /v1/trace-search`：并发 fanout 到所有 shard，gateway 做全局排序、全局 offset/limit 分页、rank 重写和 `items/total` 合并，返回 `queryMode:"process_gateway_fanout"`、`okShards`、`degraded` 和 `failedShards`。
  - `POST /v1/search`：并发 fanout 到所有 shard，gateway 按 score 全局排序、按 `(trace_id, span_id)` 去重并截断 top-k；默认兼容 legacy array，显式 `includeFanout:true` 时返回 fanout envelope。
  - `GET /v1/cluster/shards`：汇总每个远端 shard 的可达状态。
- 真实 gateway 子进程 eval 已从测试内手写 route/serializer 切换为 `RemoteShardGateway`，测试 socket 层只负责读请求和写响应。
- eval 覆盖：
  - gateway 路由写入 3 个真实 shard 进程。
  - gateway fanout `trace-search` 合并 3 个 shard 结果，并覆盖全局分页和排序。
  - gateway fanout `/v1/search` 合并 3 个 shard 结果，并覆盖全局 top-k。
  - 同一 session 下多条 trace 被路由到同一远端 shard。
  - 1 个 shard 宕机时，trace-search 返回 degraded 诊断且只统计存活 shard。

当前取舍：

- 当前 shard key 已和 in-process cluster 对齐：有 `session_id` 时按 `(tenant_id, session_id)`，没有 `session_id` 时按 `(tenant_id, trace_id)`；后续仍要替换为可持久化/可迁移的生产路由表。
- `RemoteShardGateway` 目前只实现 ingest、search、trace-search、cluster status。aggregate、trajectory、metadata、retention 的 remote fanout 还要逐项接入同一 facade。
- 它仍不是生产控制面：没有动态路由表、health check、熔断、重试/backoff、限流、鉴权 scope 或远程 snapshot lease；HTTP client 明确只支持明文 `http://`/`host:port`，不支持 TLS。

## 分层升级路线

### L0：多客户端写入，单进程承接

这是当前模型的延伸：

- 多个 SDK / HTTP client / Node 进程都可以向同一个 yiTrace server 写。
- server 内部仍由一个 `WriteCoordinator` 串行写 WAL 和 manifest。
- 适合本地开发、Electron、单机私有化、小团队部署。

这不是真正的多 writer，但用户体感上已经是多客户端并发写。

### L1：分片写入，shard 内单写

这是推荐的第一版“多写”。

组件：

- `IngestGateway`：无状态接入层，鉴权、限流、解析 tenant，然后按路由规则转发。
- `ShardRouter`：根据 shard key 找目标 shard。
- `ShardNode`：每个 shard 一个 `WriteCoordinator`，拥有自己的 WAL、manifest、segment、attr sidecar、vector index。
- `QueryCoordinator`：对跨 shard 查询做 fanout、分页和结果合并。

关键路由规则：

- 有 `session_id` 时优先按 `(tenant_id, session_id)` 分片，让一个会话的多轮 trace 尽量在同一 shard。
- 没有 `session_id` 时按 `(tenant_id, trace_id)` 分片，保证一条 trace 的所有 span 在同一 shard。
- `tenant_id` 必须在 shard key 中，避免跨租户数据混放导致迁移和隔离困难。

这能保住最重要的局部性：

- trace detail：单 shard。
- session turns：通常单 shard。
- span fold：单 shard。
- retention/compaction：shard-local。

跨 shard 的能力：

- traceSearch：各 shard 返回局部结果，协调层按时间/score merge。
- traceAggregate：各 shard 返回 partial aggregate，协调层 reduce。
- trajectoryGroups：各 shard 分桶，协调层按 signature 合并计数和样本。
- vector search：各 shard 返回 top-k，协调层按距离 merge。
- BM25：第一版可以局部 BM25 分数 merge；高精度版再维护全局 term stats。

### L2：shard 内主备复制和故障切换

L1 解决吞吐和容量，L2 解决可用性。

推荐复制模型：

- shard leader 接收写入。
- 写入先落 leader WAL。
- WAL record 流式复制到 follower。
- follower replay WAL，并同步不可变 segment 文件和 manifest。
- 读请求可以打 follower，但需要暴露 snapshot/version 水位。

不可变 segment 是优势：复制不是同步复杂可变页，而是同步新 segment + WAL tail + manifest 版本。

一致性选项：

- 开发版：异步复制，RPO 取决于 WAL shipping 延迟。
- 生产版：Raft per shard，只有 leader commit 后 ack，follower 用日志顺序 apply。

注意：即使用 Raft，shard 内仍是单 leader 写；这是分布式数据库里最常见、最稳的写法。

### L3：真正 multi-leader active-active

只有在跨地域低延迟写入是硬需求时才做。

需要新增：

- writer identity：`writer_id`。
- writer-local WAL：每个 writer 有自己的 log。
- 全局或分区内 sequencer：给 commit order 排序。
- manifest delta log：不允许多个 writer 直接写整份 manifest，只能提交可合并 delta。
- segment id 命名空间：例如 `(writer_id, local_segment_id)`，避免冲突。
- conflict resolver：annotation、Golden Path status、retention policy 这类非 append 状态要有冲突规则。
- tombstone/retention barrier：不能一个 writer 删除另一个 writer 还没同步完的 trace。

这会显著复杂化恢复、GC 和查询一致性。除非客户明确要求多地域 active-active，否则不建议作为近期方向。

## 当前代码需要抽出的边界

### 1. StorageEngine trait

把当前 `WriteCoordinator` 作为单 shard engine，外面包一层 cluster 接口：

```rust
trait TraceStorage {
    fn ingest(&self, tenant: u64, batch: Vec<WireRecord>) -> IngestResult;
    fn trace_search(&self, tenant: u64, req: TraceSearchRequest) -> TraceSearchResult;
    fn trace_detail(&self, tenant: u64, trace_id: ExternalOrInternalId) -> TraceDetail;
}
```

单机版实现直接调用本地 coordinator；分布式版实现做 router + fanout。

### 2. Shard key 和外部 ID 契约

必须先稳定：

- `tenant_id`
- `session_id`
- `trace_id`
- `external_trace_id`
- `event_id`

写入要保证同一 trace 不会被路由到多个 shard。否则 fold、logEvents、span detail 都会变复杂。

### 3. Snapshot token / lease

跨 shard 查询需要返回和接受 snapshot token：

```json
{
  "snapshot": {
    "leaseId": "lease-42",
    "shards": [
      {"shardId":"s1","manifestVersion":120},
      {"shardId":"s2","manifestVersion":87}
    ]
  }
}
```

分页时带回 snapshot token，避免下一页读到不同 shard 的新版本导致跳页或重复。

当前实现状态：

- in-process cluster 的 `sessions`、`traceSearch`、`traceAggregate`、`trajectoryGroups`、`traceTrajectories`、`storageStats`、`loops`、`taskTraces` 已返回 `snapshot`。
- JSON body 接口接受 `snapshot` / `snapshotToken`；GET 查询串接受 `snapshot` / `snapshotToken` / `snapshot_token`。
- 首次请求会创建同进程 lease，响应 token 带 `leaseId`；后续请求带回 token 会复用旧 shard snapshot，提供 fixed-version read。
- 如果客户端带回的 shard manifest version 与 lease 不一致，返回 `409 snapshot_mismatch`。
- 如果 token 不是合法 JSON 或缺少 `snapshot.shards`，返回 `400 bad_snapshot`。
- 如果 `leaseId` 已被挤出，返回 `409 snapshot_expired`。

这个版本实现的是同进程 fixed-version read。真正分布式时需要让 shard read path 按 token pin 指定 manifest version，或在远程 shard 上持有短生命周期 snapshot lease。

### 4. Query merge contract

每个跨 shard API 要定义 merge 语义：

- `traceSearch`：按 sort key merge，score 相同按 trace id 稳定排序。
- `traceAggregate`：partial aggregate reduce。
- `trajectoryGroups`：按 signature reduce。
- `storageStats`：计数和字节估算求和。
- `goldenPathHealth`：各 shard 统计 followed/extended/partial/deviated 后合并。

### 5. Control plane metadata

annotation、dataset association、Golden Path、retention policy 可以先有两种路线：

- co-locate：跟 source trace 走同 shard，简单，适合第一版。
- control plane store：独立小型元数据库，负责 project/tenant/golden path/policy，全局查询更方便。

第一版建议 co-locate，等管理面复杂后再拆 control plane。

## 建议的近期任务

1. 先不要改底层单写逻辑，先加 `ShardRouter` 和 `TraceStorage` 抽象。
2. 做一个 in-process multi-shard prototype：一个进程里开 N 个 `WriteCoordinator`，每个 shard 一个 data dir。
3. 让 `EngineJsonApi` 支持 cluster mode：写入按 session/trace 路由，查询 fanout merge。
4. 给 traceSearch / traceAggregate / trajectoryGroups / storageStats 补跨 shard eval。
5. 再做 WAL shipping follower，验证 shard 主备复制。
6. 最后再考虑 Raft；不要一开始就引入共识层。

## 判断标准

这条路线好的标志：

- 单 shard 的 crash recovery、manifest、GC、retention 测试几乎不用重写。
- 多写吞吐随 shard 数接近线性增加。
- trace detail 和 session detail 大多数仍是单 shard 查询。
- 跨 shard 查询只是性能和 merge 问题，不影响单条 trace 的正确性。
- 失败恢复仍能用 “WAL + manifest + immutable segments” 解释清楚。

真正危险的信号：

- 多个进程开始直接写同一个 `manifest.dat`。
- segment id 仍是全局递增 u64，却被多个 writer 分配。
- compaction 可以跨 writer 修改同一个 segment 集合。
- retention 可以删除其他 writer 还没观察到的 trace。
- 查询分页没有 snapshot token。

## 一句话产品口径

yiTrace 不应该从“单写”直接跳成“多个 writer 抢同一份存储”。更合理的升级是：**保留 shard 内单写的正确性，把多写能力放到分片、路由、复制和查询协调层。**
