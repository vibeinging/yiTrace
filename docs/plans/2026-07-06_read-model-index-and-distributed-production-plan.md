# yiTrace 读模型索引化与生产分布式开发计划

> 日期：2026-07-06
> 背景：当前 yiTrace 的基础 TraceDB 能力已经闭环，下一阶段重点从“补 API”转向“冷查询高性能化”和“生产分布式完整路径”。

## 目标

这份计划覆盖两条主线：

1. **高频读模型索引化**：把 `traceAggregate`、`loops`、`taskTraces`、metadata 查询、全文/向量召回从 folded snapshot 扫描或轻量 cache，推进到可重建、可观测、可恢复的物化索引/rollup。
2. **生产分布式完整路径**：在现有 process gateway / shard server / WAL shipping eval 基础上，补动态路由表、远程 snapshot lease、网络复制、heartbeat/failover、retry/熔断和一致性策略。

非目标：

- 不做 multi-leader active-active。近期继续坚持 **single-writer per shard, multi-writer at cluster level**。
- 不把 Golden Path Best/Challenger 自动治理放进 DB 底座。DB 只提供证据、索引、导出、health 和 adherence。
- 不在 `yt-engine` 引入外部依赖。索引/rollup 先用 std-only sidecar；重依赖继续隔离到独立 crate。

## 设计原则

- **派生索引可重建**：rollup、loop/task index、metadata index、全文/向量增强都不能成为唯一事实源。事实源仍是 WAL + segment + manifest + metadata.dat。
- **索引命中可观测**：每个读 API 返回稳定 `index` / `aggregationIndex` / `trajectoryIndex` / `metadataIndex` 字段，eval 断言是否走索引。
- **冷查询优先**：已有 `readModelCache` 解决重复请求；本计划重点解决第一次查询的扫描成本。
- **一致性先于速度**：索引可以延迟构建，但不能漏召回。索引不可用时必须显式 degraded/slow path，不允许静默返回不完整结果。
- **单机不退化**：分布式代码不能破坏 embedded / single-node 的 API、测试和性能。
- **每个阶段都写 eval**：功能、边界、重启恢复、性能、部分失败都要有覆盖。

## 当前基线

已完成：

- 高频字段一等化：project/skill/mode/task/loop/harness/schema/intent/validation/review/eval/path memory 等。
- attrs postings sidecar：高频 attrs 过滤可走候选集 + folded verify。
- `readModelCache`：`traceAggregate`、`trajectoryGroups`、`traceTrajectories`、`loops`、`taskTraces` 重复查询可 hit。
- `traceAggregate` 第一版 read-plan + segment rollup 已落地：sealed segment 会生成可重建 `trace_aggregate_rollups/seg-*.agg` sidecar，查询安全时走 `segment_rollup_tail_overlay`，否则回退 folded scan；响应返回 `readPlan.spanReadIndex`、`usedSegmentRollup`、`segmentRollupRows`、`tailFoldedSpanCount`、`usedAttrPostings`、`candidateSpanKeys`、`scannedSegments`、`unsupportedAttrKeys`、`aggregationPlanner`、`rollupEligible`、`rollupBlockedBy` 和 `rollupFallbackReason`，eval 覆盖 segment 命中、tail-only、跨 segment span fallback、自定义 attrs 降级、cluster fanout 和 durable reopen。
- process gateway 第一版：路由写入、search/traceSearch fanout、aggregate/trajectory/storage/metadata/retention fanout 与诊断。
- follower read 原语：WAL tail shipping、replica status、bounded-stale follower read、snapshot token 绑定 read target。
- 远端生产化接缝第一版：`RemoteShardRouteTable` 支持 JSON 路由表解析、版本、role/readable/writable/weight 和 fingerprint；`RemoteShardGateway::from_route_table_json()` 可从路由表构造 writer gateway，并在 cluster diagnostics 返回 `routeTableVersion`；`POST /v1/cluster/route-table/reload` 可显式热更新同一 gateway 的 writer 视图，拒绝旧版本回退并清空路由缓存；`trace-search` / `trace-aggregate` 可返回 `mode:"remote_gateway"` 的 composite snapshot，下一次请求由 gateway 拆成 shard-local snapshot 回放，route table 变更会返回 `route_table_expired`；`POST /v1/snapshots/lease` / `POST /v1/snapshots/renew` / `DELETE /v1/snapshots/:id` 已支持 in-process cluster 和 remote gateway 显式 lease lifecycle；`RemoteShardClient` 支持最小 retry、timeout 和 circuit breaker，eval 用真实 TCP fake shard 覆盖 retry、非幂等写不重试、half-open 恢复、reload 后新写入走新 shard、remote snapshot 按 shard 回放和 remote lease release 后 `snapshot_expired`。

仍缺：

- `traceAggregate` 更强 group counter rollup / 100k+ 性能 bench（第一片 segment span rollup 已完成）。
- `loops` / `taskTraces` 真索引。
- annotation / dataset / golden path / retention policy 的 metadata index。
- 全文分域索引已完成第一片；仍缺 attrs.* 白名单域、retention soft-delete 后的域索引剔除和 100k+ 性能 bench。
- task/span/trajectory 向量 namespace 已完成第一片（append-only `named_vectors.dat` + 内存 flat index）；仍缺 namespace HNSW/GraphIndex、高性能 filtered ANN、retention soft-delete 和 recall/perf 回归。
- 生产 gateway route table watcher/外部控制面、更多读模型 snapshot 覆盖、sealed segment/manifest/sidecar/vecindex/metadata/GC log 同步、复制 worker/调度、自动 failover、retry budget 诊断和一致性策略配置。

## Track A：读模型索引化

### A0：基准与索引可观测门禁

状态：已启动。2026-07-06 已先给 `traceAggregate` 补 read-plan、segment rollup 和 eval；`loops` / `taskTraces` 已复用同一 segment rollup sidecar 返回 `loop_task_sidecar+tail_overlay`；annotation / dataset association 已补 metadata sidecar 候选集 + verify；`/v1/search` 已补 `searchIndex` / `textDomains`；`/v1/vector-search` 已补 `vectorIndex`。后续继续补 bench fixture 与高性能向量的同款可观测字段。

产物：

- 新增 `examples/read_model_bench` 或 eval bench fixture，构造 10k / 100k / 1M span 三档数据。
- 统一读模型响应字段：
  - `index`
  - `aggregationIndex`
  - `loopIndex`
  - `taskIndex`
  - `metadataIndex`
  - `readModelCache`
  - `slowPathReason`
- 新增测试 helper：断言某查询走索引、走 slow path、cache hit/miss。

eval：

- 单机 100k spans 下，记录当前 `traceAggregate`、`loops`、`taskTraces`、metadata list/filter 的冷查询耗时基线。
- 断言关掉索引时结果和打开索引结果完全一致。
- `cargo test --offline` 仍必须通过。

验收：

- 所有后续性能 PR 都能用同一组 eval 对比。
- 响应里能明确看出“是否走索引”和“为什么回退”。

### A1：traceAggregate rollup / group-by index

状态：已完成第一片。当前实现是 segment 级轻量 span rollup sidecar，而不是所有 group schema 的预聚合 counter。它已经能让安全的 sealed segment 查询避开 folded segment scan，并保留 tail overlay；更激进的 `(group_schema, group_key) -> counters` 仍作为后续性能增强。

目标：

- 让常见 group-by 不再扫全量 folded spans。
- 先支持稳定高频维度，再逐步扩展。

第一批 group-by 维度：

- `project_id`
- `task_fingerprint`
- `loop_id`
- `skill`
- `mode`
- `validation_status`
- `review_status`
- `eval_status`
- `tool_name`
- `model`
- `provider`
- `status`
- `harness_version`
- `schema_fingerprint`

已落地实现：

- 新增可重建 sidecar：`trace_aggregate_rollups/seg-*.agg`。
- 以 segment 为构建单位，存储已折叠的轻量 span 统计行：tenant、trace/span id、external id、status、duration、usage/cost、agent/tool/model/provider 和高频 attrs。
- MemTable/WAL tail 保持 folded overlay，避免新写入不可见。
- 查询 planner：
  - groupBy 全在支持维度内，filter 不涉及全文/metadata/identity/cost-token range，且没有时间窗口裁剪：尝试 rollup + tail overlay。
  - 同一 span 横跨多个 segment、tail 与 segment 重叠、segment 有 deletion vector 或 upgrade patch 时，回退 folded scan，并返回 `readPlan.rollupFallbackReason`。
  - 出现未知 attrs groupBy、大字段 contains、metadata 反向过滤：回退 folded scan，并返回 `rollupBlockedBy`。

后续增强：

- 再加 `(tenant, group_schema_id, group_key) -> counters` 预聚合层，减少从 rollup row 到 bucket 的 CPU。
- 为 trace_count 增加有预算的精确集合或显式 estimate 模式。
- 给 time window、cost/token range 做更细的 rollup predicate 或分桶。

关键风险：

- 当前为了正确性，deletion vector / upgrade patch / 跨 segment span 会触发 fallback。后续可在 rollup 里记录 per-field seq 或在 compaction 后更积极地物化这些状态。
- `trace_count` 精确去重在当前 row rollup 上仍由查询时 bucket HashSet 计算；预聚合 counter 版需要额外空间策略。

eval：

- 已覆盖：segment-only 命中 rollup、多字段 groupBy、status/attrs filter、MemTable-only tail、跨 segment span fallback、自定义 attrs 降级 folded scan、重启后 rollup 可加载、cluster fanout rollup。
- 待覆盖：segment+tail 非重叠 overlay、retention 删除后 fallback/compaction 后重建、sidecar 文件缺失重建、100k spans 性能 bench。

验收：

- 安全查询的响应为 `aggregationIndex:"segment_rollup_tail_overlay"` 或 cluster 下 `fanout_segment_rollup_tail_overlay`，`readPlan.scannedSegments:0`。
- 结果与 folded scan 对照完全一致；不满足安全门时必须显式 fallback，不静默返回估算。

### A2：loop/task 真索引

状态：已完成第一片。当前复用 `trace_aggregate_rollups/seg-*.agg` 的轻量 span rollup row 直接生成 loop/task 列表，并保留 WAL/MemTable tail overlay；带文本过滤、metadata 反向过滤或 rollup 安全门失败时回退 folded scan。loop detail 仍回源读 folded spans，因为它需要返回完整 span 列表。

目标：

- `GET /v1/loops`
- `GET /v1/loops/:id`
- `GET /v1/tasks/:fingerprint/traces`

不再依赖全量 folded span 扫描。

已落地实现：

- 复用 A1 的 segment rollup sidecar，不新增重复存储。
- `GET /v1/loops` 从 rollup rows 聚合出 loop 摘要；响应 `loopIndex:"loop_task_sidecar+tail_overlay"`，cluster 下为 `fanout_loop_task_sidecar+tail_overlay`。
- `GET /v1/tasks/:fingerprint/traces` 从 rollup rows 聚合 trace 摘要；响应 `taskIndex:"loop_task_sidecar+tail_overlay"`。
- 支持按 project/skill/mode/harness/schema/validation/review/eval/status 等 attrs 精确过滤。
- loop detail 仍可按 trace id 回源读取 span detail，避免复制大字段。
- tail overlay 同 A1。

后续增强：

- 如果 loop/task 页面成为极高频入口，再增加专门的 `tenant + loop_id` / `tenant + task_fingerprint` postings 或 counter index，减少每次从 rollup row 聚合 bucket 的 CPU。
- metadata 反向过滤接入 A3 后，可先用 metadata index 给候选 trace/span，再与 rollup row 求交。

eval：

- 已覆盖：loop/task 单机 tail-only、segment sidecar 命中、文本过滤 fallback、cluster fanout sidecar、snapshot 分页、tenant 隔离。
- 待覆盖：sidecar 缺失重建、retention/compaction 后 loop/task 统计、metadata 反向过滤与 metadata index 联动、100k spans 性能 bench。
- 同一 loop 跨多个 trace、跨 segment、跨 WAL tail。

验收：

- `loopIndex` / `taskIndex` 不再只是 cache 标签，而能标明 `loop_task_sidecar+tail_overlay`。

### A3：metadata index

状态：已完成第一片。annotation / dataset association 已建立内存可重建 index，事实源仍是 `metadata.dat`；写入、更新、删除后重建候选集，查询时按 id 候选集收窄后再调用原 matcher verify。Golden Path / retention policy 仍沿用现有列表过滤，后续按高频场景再加 index。

目标：

- annotation / dataset association / golden path / retention policy 查询不再全量扫 `metadata.dat`。
- metadata 反向过滤进入 traceSearch / aggregate / loops / taskTraces 时能快速给出候选 trace/span。

已落地实现：

- 保留 `metadata.dat` 作为事实源。
- 新增内存可重建 index：
  - annotation：tenant、target、trace_id、span_id、label、source、status、attrs。
  - dataset association：tenant、dataset_id、item_id、trace_id、span_id、eval_run_id、split、label、attrs。
- metadata 写入、PATCH、DELETE 后从当前 metadata 快照重建 index；崩溃恢复时从 `metadata.dat` 重建。
- `coord.annotations()` / `coord.dataset_associations()` 透明使用候选 id 集 + verify，所以 traceSearch / aggregate / loops / taskTraces 的 metadata 反向过滤自动受益。
- HTTP list 响应返回 `metadataIndex:"metadata_sidecar+verify"`，cluster 返回 `fanout_metadata_sidecar+verify`。

后续增强：

- Golden Path / retention policy list/filter 建同款 index。
- 做持久化 `metadata.idx` 或更细的增量更新，避免 metadata 极大时每次写入全量重建。
- `created_at_ns` 倒序游标进入 index，减少 list 后排序成本。

eval：

- 已覆盖：create/update/delete annotation 后索引即时生效；deleted annotation 默认不返回，显式 `status=deleted` 可查；dataset attrs/filter；tenant 隔离；cluster co-location + fanout metadata index；durable reopen 后从 `metadata.dat` 重建。
- 待覆盖：golden path / retention policy index、metadata 极大规模性能 bench、created_at 游标索引。

验收：

- metadata 查询响应返回 `metadataIndex:"metadata_sidecar"`。
- 10k metadata items 下 list/filter 不再线性扫描。

### A4：全文索引增强

状态：已完成第一片。`TextDomainIndexes` 在摄入/recover 时同步维护 input/output/log/tool/model/agent 六个分域 BM25；`POST /v1/search` 支持 `textDomains` / `text_domains` / `domains` / `fields` 和 `inputTextContains` / `outputContains` / `logContains` 等别名，响应返回 `searchIndex:"text_domain_bm25"` 与 `textDomains`；remote gateway 读 fanout 可合并分域搜索结果。eval 覆盖 input/output/log 域隔离、attrs/tenant filter 和 durable reopen。

目标：

- 支持 Trace Inbox 搜错误、SQL、表名、字段名、工具输出片段。
- 把当前 BM25 从主要检索 span text 扩展到 input/output/log 分域索引。

实现思路：

- 已建立字段域：
  - `input_text`
  - `output_text`
  - `logs`
  - `tool_name`
  - `model`
  - `agent_name`
- 后续可选 `attrs.*` 白名单
- 查询 DSL 已增加 domain filter：
  - `text`
  - `inputTextContains`
  - `outputContains`
  - `logContains`
  - `fieldText`
- 分词仍走现有 tokenizer trait。
- 索引仍要 tenant-aware，支持 attrs postings 先收窄再文本召回。

eval：

- 已覆盖：input/output/log 域单独命中；tenant/project filter 下不串租户；segment + WAL tail recover 后一致。
- 待覆盖：中文、英文、SQL、表名、字段名混合；删除/retention 后不可命中；attrs.* 白名单域；100k spans 性能 bench。

验收：

- Trace Inbox 典型搜索不需要全量 scan。
- 响应 `index` 能标明 text domain index + attrs filter。

### A5：向量增强

状态：已完成第一片。新增 span/task/trajectory 命名空间向量底座：`POST /v1/vector-index` 写入 `(tenant, namespace, key)`，`POST /v1/vector-search` 按 namespace + tenant + attrs filter 召回；durable 目录写 append-only `named_vectors.dat`，recover 时重建内存 flat index；Node 暴露 `db.indexVector()` / `db.searchVector()`；remote gateway 的 vector-index 按 key hash 路由，vector-search fanout 合并 top-k。它先保证语义、恢复和 API 合同正确，不代表最终高性能 ANN。

目标：

- 给 task/span/trajectory 相似召回提供稳定底层能力。
- 支持 Agent Memory 后续做相似任务、相似失败、相似路径召回。

实现思路：

- 不在 engine 内调用 embedding 模型；只接收外部 embedding。
- 已支持三类 vector namespace：
  - `span`
  - `task`
  - `trajectory`
- 每条 vector 带：
  - tenant
  - project_id
  - schema_fingerprint
  - model/provider/version
  - source kind/id
- 复用现有 `GraphIndex` 接缝和 disk HNSW；增加 namespace 和 metadata filter。
- trajectory vector 可由上层基于 steps 文本或 canonical path embedding 写入。

eval：

- 已覆盖：task/trajectory namespace 不串；attrs/tenant filter；重启后召回稳定；remote gateway fanout 合并 top-k。
- 待覆盖：span namespace 端到端；metadata filter 进图过滤；删除/retention 后 soft delete 生效；HNSW recall vs brute force 回归；100k vectors 性能 bench。

验收：

- 新增 similarity API 已返回 vector namespace 和 `vectorIndex:"vector_namespace_flat"` / `fanout_vector_namespace_flat`。
- 高性能验收后续以 namespace HNSW、filtered ANN 和 recall/perf 回归为准。

## Track B：生产分布式完整路径

### B0：动态路由表

状态：已完成第二片。2026-07-06 已完成静态 JSON route table 模型、版本/fingerprint、writer gateway 构造和 cluster diagnostics 暴露；`POST /v1/cluster/route-table/reload` 可显式热更新 writer 视图、清空路由缓存并拒绝旧版本；remote snapshot token 绑定 `routeTableVersion`，版本变化会返回 `route_table_expired`；route table v2 已支持 logical shard + replicas schema，gateway 按 logical shard 选择唯一 writable replica，cluster diagnostics 返回 writable `replicaId` 和 `replicas` 列表。后续仍缺周期 watcher / 外部控制面订阅。

目标：

- 替换静态 shard URL 列表，支持 gateway reload 和版本化路由。

实现思路：

- route table v1 扁平格式已落地：

```json
{
  "routeTableVersion": 1,
  "shards": [
    {
      "shardId": "route-a",
      "addr": "127.0.0.1:7901",
      "role": "leader",
      "readable": true,
      "writable": true,
      "weight": 1
    }
  ]
}
```

- route table v2 logical shard + replicas 已落地：

```json
{
  "routeTableVersion": 51,
  "shards": [
    {
      "shardId": "logical-a",
      "replicas": [
        { "replicaId": "a-primary", "addr": "127.0.0.1:7901", "role": "follower", "readable": true, "writable": false },
        { "replicaId": "a-follower", "addr": "127.0.0.1:7902", "role": "leader", "readable": true, "writable": true, "priority": 20, "maxLagLsn": 0 }
      ]
    }
  ]
}
```

- gateway 显式 reload API 已落地；后续补周期 watcher 或外部控制面订阅。
- 路由表版本进入 fanout response。
- 第一版不做自动 rebalance，只支持配置化新增/下线 shard。

eval：

- reload 后新写入走新路由（已覆盖真实 TCP fake shard eval）。
- v2 手动 promote 后，新写入走 promoted replica，旧 leader 不再接新写（已覆盖真实 TCP fake shard eval）。
- old route table 下的 snapshot token 仍能完成分页，或返回明确 `route_table_expired`。
- tenant/session/trace hash 路由稳定。
- route table 损坏时 gateway 拒绝启动或保留旧版本；旧版本 route table reload 会被拒绝（已覆盖 eval）。

验收：

- 不重启 gateway 可通过显式 reload API 切换 route table。
- 响应带 `routeTableVersion`。

### B1：远程 snapshot lease

状态：已完成第二片。`trace-search` / `trace-aggregate` 已经通过查询响应里的 shard-local snapshot 实现 remote gateway composite snapshot；gateway replay 时会把 composite token 拆给对应 shard，route table version/shard id 不匹配时返回 `route_table_expired`。`POST /v1/snapshots/lease` / `POST /v1/snapshots/renew` / `DELETE /v1/snapshots/:leaseId` 已支持 in-process cluster 和 remote gateway：remote gateway 会向各 shard 建 shard-local lease，自己保存 composite lease，renew 时续租所有 shard-local lease，release 后 replay/renew 返回 `snapshot_expired`。

目标：

- process gateway 查询远程 shard 时，分页能 pin 远程 shard 的 manifest version/read target，而不是只在 gateway 进程内保留 snapshot。

实现思路：

- 已落地第一版：shard 的 `trace-search` / `trace-aggregate` 返回本地 snapshot token，remote gateway 包装为：
  - `mode:"remote_gateway"`
  - `routeTableVersion`
  - `shards[].shardId`
  - `shards[].snapshot`
- 查询请求带 remote gateway snapshot 时，gateway 按 shard index/id 拆回 shard-local snapshot，再转发给对应 shard。
- 已增加显式 lifecycle：
  - `POST /v1/snapshots/lease`
  - `POST /v1/snapshots/renew`
  - `DELETE /v1/snapshots/:leaseId`
- lease max entries 已有 LRU eviction；remote route table reload 会清空 composite lease。TTL 仍待做。
- cache key 必须绑定 read target + manifest version + read_model_revision。

eval：

- traceSearch / traceAggregate remote snapshot round-trip 按 shard-local lease 回放（已覆盖真实 TCP fake shard eval）。
- 显式 remote snapshot lease / renew / release / release 后 replay 409 `snapshot_expired`（已覆盖真实 TCP fake shard eval）。
- follower 追平后，旧 snapshot token 仍看旧结果。
- lease 过期、篡改、route table 变更都返回明确错误；route table 变更返回 `route_table_expired` 已覆盖。
- shard 宕机时 partial/strict 策略符合预期。

验收：

- `trace-search` / `trace-aggregate` 远程 snapshot replay 与 in-process snapshot lease 语义已对齐；后续扩展到 trajectory/storage/loop/task/session 等读模型，并补 TTL。

### B2：网络复制

状态：已完成第一片。2026-07-06 已新增 `GET /v1/replication/status`、`GET /v1/replication/wal?afterLsn=...`、`POST /v1/replication/wal`，把已有 in-process WAL shipping 暴露为真实 HTTP 复制协议；多进程 eval 启动 leader/follower 两个 shard server，覆盖空 batch、catch-up、重复 batch 幂等和缺口 batch 409。它仍是显式 pull/apply 底座，不包含后台复制 worker、leaderTail/lag 聚合、snapshot bootstrap 或 sealed segment/metadata/sidecar 同步。

目标：

- 把当前 in-process WAL tail shipping 升级成 shard leader/follower 的远程复制协议。

实现思路：

- 已落地 leader/follower endpoint：
  - `GET /v1/replication/wal?after_lsn=...`
  - `GET /v1/replication/status`
  - `POST /v1/replication/wal`
- 后续补 follower loop：
  - 拉取 WAL batch
  - 幂等 apply
  - 定期 checkpoint
  - lag metrics
- 后续可选 `POST /v1/replication/snapshot` / snapshot bootstrap，用于 WAL 缺口、leader compaction/retention 后的恢复。
- 初版 pull-based，避免 leader 维护复杂连接。
- 复制只在同一 shard 内；跨 shard 不做事务。

eval：

- follower 从空目录 bootstrap（空 batch 已覆盖，完整 snapshot bootstrap 待补）。
- follower 从落后 LSN catch up（已覆盖真实双进程）。
- 重复 WAL batch 幂等（已覆盖真实双进程）。
- gap batch 返回 409，需要 snapshot/bootstrap（已覆盖真实双进程）。
- torn response / network error 后 retry 不重复写。
- leader compaction/retention 后 follower 仍可恢复，必要时要求 snapshot bootstrap。

验收：

- 当前 follower status 可报告 `committedTail`、`manifestVersion`、`memtableWatermark`、`memtableRows`、`segmentCount`。
- 后续复制 worker/cluster status 再聚合 `leaderTail`、`lagLsn`、`readable`、`reason`。

### B3：heartbeat / health / failover

状态：已完成第三片。2026-07-06 复核后已把 `RemoteShardRouteTable` 升级为兼容 v1 扁平 route 和 v2 logical shard + replicas；v2 强制每个 logical shard 恰好一个 writable replica，手动 promote 可通过 reload route table 切换写 leader，并有真实 TCP fake shard eval 覆盖。显式 heartbeat/health refresh 已落地：`POST /v1/cluster/health/refresh` 会探测 route table 中所有 replica 的 `/v1/replication/status`，聚合 `committedTail`、`leaderTail`、`replicationLagLsn`、`healthy/stale/unreachable/diverged` 和原因；`GET /v1/cluster/health` 返回最近一次采样，`GET /v1/cluster/shards` 会带上 health diagnostics。读 fanout 已接入 bounded-stale read target：默认 partial 查询优先读 healthy 且 lag 不超过 `maxLagLsn` 的 follower，stale/unreachable 时回 leader；`readTargets` 返回实际 replica、lag 和原因；remote gateway snapshot 写入 `replicaId`，后续分页会回到同一个 replica。真实 TCP eval 覆盖 follower lag 超阈值变 stale、不可达 replica 变 unreachable、fresh follower 被读、stale follower 回 leader。周期 watcher、自动 failover 和 fencing 仍未实现。

目标：

- gateway 能感知 shard/follower 健康，并在安全条件下读 follower、避开不可用节点。

实现思路：

- 已升级 route table v2，把“分片”和“副本”分开：
  - `shards[].shardId`
  - `shards[].replicas[].replicaId`
  - `replicas[].addr`
  - `replicas[].role = leader|follower|candidate`
  - `replicas[].readable/writable`
  - `replicas[].priority`
  - `replicas[].maxLagLsn`
- gateway writer 视图按 logical shard 选择唯一 writable replica，而不是把所有 writable addr 展平成多个 shard。
- reader 视图可在同一 logical shard 的 readable replicas 中按 health + lag 选择。
- gateway health monitor：
  - 已落地显式刷新：`POST /v1/cluster/health/refresh`
  - 已落地查询：`GET /v1/cluster/health`
  - 采样优先请求 `/v1/replication/status`，老节点 404 时回退 `/v1/cluster/shards`
  - 后续再做周期 watcher 或外部控制面驱动
  - 记录 latency、error rate、replication lag、last success
- node state：
  - `healthy`
  - `suspect`
  - `unreachable`
  - `draining`
  - `read_only`
- 初版自动 failover 只做读路径：
  - search/read 可以用 readable follower（已落地：remote fanout 默认 partial 下使用 bounded-stale follower）
  - 写路径仍必须 leader
- 写 leader failover 第一版先手动 promote：
  - operator 更新 route table
  - gateway reload
  - old leader fencing 由部署层保证

eval：

- follower lag 超阈值标记为 stale/readable:false（已覆盖真实 TCP eval）。
- fresh follower 被 remote fanout 读到，stale follower 回 leader（已覆盖真实 TCP eval）。
- node timeout 后进入 suspect/unreachable（unreachable 已覆盖真实 TCP eval）。
- draining shard 不接新写，但旧 snapshot 可读。
- 手动 promote 后新写入走新 leader（已覆盖）。

验收：

- cluster status 和 fanout response 能解释每个 shard 为什么可读/不可读；`readTargets` 能解释本次实际读了哪个 replica。

### B4：retry / backoff / circuit breaker

状态：已启动。2026-07-06 已完成 std-only `RemoteShardClient` 的最小 timeout、固定 backoff retry 和 circuit breaker；eval 覆盖 GET retry、非幂等 metadata write 不 retry、open 后快速失败、reset 后 half-open 探测。尚未做 retry budget、指数 backoff+jitter、breaker 诊断进入 fanout response。

目标：

- 远程 fanout 不因为单个慢 shard 拖垮整体。

实现思路：

- `RemoteShardClient` 增加：
  - connect timeout
  - read timeout
  - max body
  - retry budget
  - exponential backoff + jitter
  - circuit breaker
- breaker key：route table version + shard id + endpoint。
- breaker states：
  - closed
  - open
  - half_open
- 对非幂等写入默认不自动 retry；ingest 可依赖 deterministic event_id 做 retry-safe。

eval：

- 慢 shard 超时后查询 degraded。
- all shards failed 返回 503。
- breaker open 后短时间快速失败，不阻塞请求。
- half-open 成功后恢复。
- ingest retry 不重复计数。

验收：

- fanout response 有 `failedShards`、`retryable`、`breakerState` 或诊断详情。

### B5：一致性策略

状态：已完成第二片。2026-07-06 已在 remote/process gateway 读 fanout 上落地 `partial` / `strict` 策略：默认 partial 保持旧行为，部分 shard 失败时返回成功 shard 结果并标记 `degraded:true`；请求传 `consistency:"strict"` / `consistency:"strong"` 或 `partial:false` 时，任一 shard 失败都会整体返回 502/503。默认 partial 已接入 bounded-stale follower 选择，响应返回 `consistencyUsed`、`partial` 和 `readTargets`；显式 strict/strong 会强制读 leader。真实 TCP eval 覆盖一个 shard 成功、一个 shard 不可达时 partial 成功/strict 拒绝，以及 fresh follower 被读、stale follower 回 leader。`staleBoundLsn`、gateway 默认策略配置和更细的 read target metrics 仍未实现。

目标：

- 明确每个 API 在分布式下的读一致性，不让调用方猜。

策略枚举：

- `strong`：只读 leader / pinned snapshot；失败则失败。
- `bounded_stale`：可读 lag 在阈值内的 follower。
- `eventual`：允许任意 readable follower，适合低价值列表。
- `partial`：允许部分 shard 成功，返回 degraded。
- `strict`：任一 shard 失败则整体失败。

默认建议：

- ingest / metadata write / retention apply：strong。
- trace detail / span detail / snapshot：strong。
- traceSearch / traceAggregate / trajectoryGroups：bounded_stale + partial 默认可配置。
- sessions / loops / taskTraces：bounded_stale。
- golden path status update：strong。
- golden path health：bounded_stale + partial。

实现思路：

- 已落地：请求支持 `consistency:"strict" | "strong"` 和 `partial:false`；默认 `partial`。
- 已落地：remote fanout response 返回 `consistencyUsed` 和 `partial`。
- 已落地：默认 partial 使用 bounded-stale follower read target；strict/strong 强制 leader。
- 已落地：remote fanout response 返回 `readTargets`。
- 后续：gateway config 提供默认策略。
- response 返回实际策略：
  - `consistencyUsed`（已落地）
  - `partial`（已落地）
  - `degraded`（已落地）
  - `staleBoundLsn`（待做）
  - `readTargets`（已落地）

eval：

- strict 模式一个 shard 失败时返回错误（已覆盖真实 TCP eval）。
- partial 模式一个 shard 失败时返回成功 + degraded（已覆盖真实 TCP eval）。
- bounded_stale follower lag 低时被选中，lag 高时回 leader（已覆盖真实 TCP eval）。
- snapshot token 覆盖 consistency 参数，保证分页稳定。

验收：

- remote/process gateway 读 fanout 的 partial/strict/bounded-stale 行为可预测、可测试、可解释；`staleBoundLsn` 和默认策略配置待补。

## 推荐迭代顺序

### 里程碑 1：性能冷查询可测

范围：

- A0 基准和 index 可观测
- A1 traceAggregate rollup 第一版

为什么先做：

- `traceAggregate` 是看板、path mining、Agent Memory 统计的共用底座。
- 它的性能收益最容易量化。

### 里程碑 2：产品页高频索引

范围：

- A2 loop/task index
- A3 metadata index

为什么第二：

- loop/task/metadata 是 AgenticData 和控制台最容易变成高频页面的路径。
- metadata index 也会反哺 retention、traceSearch 反向过滤和 Golden Path evidence。

### 里程碑 3：检索增强

范围：

- A4 full-text domains
- A5 task/span/trajectory vector namespace

为什么第三：

- 这会扩大产品能力，但依赖真实 query 语料和 embedding 策略。
- 不应阻塞 rollup/index 和发版。

### 里程碑 4：生产 gateway 基座

范围：

- B0 dynamic route table
- B4 retry/backoff/circuit breaker
- B5 consistency policy 第一版

为什么先做这些：

- 没有 route table、timeout 和 policy，远程 shard 只是 eval demo，不能生产化。

### 里程碑 5：远程一致读与复制

范围：

- B1 remote snapshot lease
- B2 network replication
- B3 heartbeat/failover

为什么后做：

- 这部分会碰恢复、租约、节点状态和运维语义，复杂度高。
- 需要 B0/B4/B5 的控制面和错误合同先稳定。

## 全局 eval 矩阵

每个阶段至少覆盖以下维度：

- 功能正确性：索引结果与 folded scan / 单 shard 结果对照一致。
- 租户隔离：tenant A 的索引、route、snapshot、replication 不能读到 tenant B。
- 重启恢复：重启后 sidecar / route table / lease / replication status 正确。
- 缺失重建：删除 sidecar 后可自动 rebuild 或显式 degraded。
- retention/compaction：软删除和压实后索引不返回已删数据。
- tail overlay：新写入在 flush 前对查询可见。
- 部分失败：单 shard timeout/down 时 partial 和 strict 语义正确。
- 性能：每个 hot query 有冷查询 p50/p95 和索引命中断言。
- 内存预算：索引目录和 cache 有上限，超限回退不漏召回。

## 成功标准

短期成功：

- `traceAggregate`、`loops`、`taskTraces`、metadata 查询都有真实索引路径。
- eval 能证明冷查询不再依赖全量 folded scan。
- process gateway 具备 route table、timeout/retry、consistency 策略第一版。

中期成功：

- 远程 shard snapshot lease 语义与 in-process lease 对齐。
- WAL 网络复制可让 follower 持续追 leader，并可参与 bounded-stale read。
- gateway 在单 shard 故障时能稳定返回 degraded 或 strict error，不拖垮整体。

长期成功：

- 单机 embedded、单机 server、process gateway、sharded deployment 共用同一套 API 和 eval 合同。
- 高性能索引和分布式能力都是渐进增强，不破坏 yiTrace 当前“可嵌入、低依赖、确定性恢复”的核心价值。
