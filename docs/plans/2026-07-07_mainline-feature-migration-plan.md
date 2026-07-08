# 从 feature/yitrace-distributed-upgrade 回流主线的迁移计划

日期：2026-07-07

当前分支：`feature/yitrace-mainline-hardening`

## 目标

把 `feature/yitrace-distributed-upgrade` 里对单机版、嵌入式版有价值的能力迁回主线，同时不把完整分布式路径带进来。

这次迁移不是简单 cherry-pick。实验分支里混了很多能力，必须按小块回流，确保每一步都能单独测试、单独回滚。

## 不迁移的边界

以下内容暂时不进入主线：

- remote gateway
- shard client
- route table
- replication API
- heartbeat / failover
- remote snapshot lease
- distributed chaos / process / replica eval
- gateway server example
- 多 shard 写入路径

原因：这些属于分布式实验。短期主线仍然保持单机优先、可嵌入优先。

## 迁移顺序

## 当前进度

已完成：

- 第 1 步质量工具：新增 `eval_all.sh`、`test_all.sh`、`bench_scale.sh`、`scale_bench`、主线版 `risk_eval_matrix`。
- 第 2 步嵌入式 DB 包：迁入 `yitrace-db-python`、`yitrace-db-rs`，并补 Node/Python/Rust 对 `traceSearch`、`traceAggregate`、`storageStats` 的稳定包装。
- 第 3/4 步的一部分：主线新增 `/v1/trace-search`、`/v1/trace-aggregate`、`/v1/storage-stats` 的单机基础版，常用过滤先走内存派生索引，再折叠候选 span；响应返回 `readPlan`，可判断是否命中索引。
- 第 4 步继续完成：新增 `/v1/trace-trajectories`、`/v1/trajectory-groups`、`/v1/traces/diff`、`/v1/loops`、`/v1/loops/:loopId`、`/v1/tasks/:fingerprint/traces` 的单机基础版，并补 Node/Python/Rust 嵌入式包装和 eval。
- 第 5 步已完成主线基础版：新增单机持久化 metadata/retention 账本，提供 `/v1/annotations`、`/v1/dataset-associations`、`/v1/retention-plan`、`/v1/retention/apply`、`/v1/retention-audits`、`/v1/retention-policies`、`/v1/retention-policies/run-due`，支持 tenant 隔离、attrs 过滤、annotation 更新/软删除、dataset item 关联、retention dry-run/apply/audit/policy，并补 Node/Python/Rust 嵌入式包装和 eval。
- 第 6 步已完成第一段：扩大内存派生索引到 `task_fingerprint`、`loop_id`、`validation_status`、`review_status`、`eval_status`、`tool_name`、`model` 等高频字段；`trace-search`、`trace-aggregate`、`storage-stats`、`trace-trajectories`、`trajectory-groups` 返回 `readPlan`；risk eval 覆盖索引命中和回退扫描。
- 第 6 步已完成第二段：annotation/dataset association 查询接入内存 metadata postings，新增、更新、软删除、reopen 都有 risk eval 覆盖。
- 第 6 步已完成第三段：`trace-aggregate` 的无文本聚合接入内存 rollup，`readPlan.source` 返回 `aggregate_rollup`；retention 删除、segment upgrade、recover 后会按当前快照同步重建 rollup；带 text 仍回退到正确扫描，risk eval 已覆盖。
- 第 6 步已完成第四段：`/v1/loops`、`/v1/loops/:loopId`、`/v1/tasks/:fingerprint/traces` 复用内存派生索引并返回 `readPlan`；task traces 先用 `task_fingerprint` 缩小候选 trace，再展开完整 trace 做最终过滤，risk eval 已覆盖。
- 第 6 步已完成第五段：retention audit/policy 查询接入 metadata postings，按 audit id、tenant、source、policy id、name、enabled 拿候选，再做最终校验；新增、reopen、run-due 后查询都有 risk eval 覆盖。
- 第 6 步已完成第六段：持久模式写 `trace_rollup.dat` 作为 segment-only span 小字段缓存；recover 先加载缓存再叠加 WAL tail，缓存损坏或版本不匹配会扫描 segment 重建；`trace-aggregate` 和无文本的 trajectory/loop/task 都复用这份缓存，risk eval 覆盖正常 reopen、坏缓存 fallback 和路径读模型。
- 第 6 步已完成第七段：attrs filter sidecar 改成 postings 索引，并写 `filter_attrs.dat` 作为 segment-only cache；recover 先加载 cache，再叠加 WAL tail，cache 损坏或版本不匹配会扫描 segment 重建；risk eval 覆盖 reopen 后 `filter_index` 命中和坏 cache fallback。
- 第 6 步已完成第八段：attrs postings 增加内存预算保护；单个 postings 过宽或总 entries 超预算时禁用对应 postings，查询改走其他 postings 或扫描 `filter_attrs` 行后最终校验，保证结果不丢；模块单测覆盖宽 postings 和总预算两种长尾。
- 第 6 步已完成第九段：`trace_rollup` 增加 trace_id 二级索引；`trace-trajectories`、`trajectory-groups`、`loop detail`、`task traces`、`trace diff` 在拿到候选 trace 后只从 rollup 取候选 trace 的完整 span，不再为少量候选 trace 重新组装全租户 span；`readPlan.traceFetchSource` / `traceFetchSpanCount` 会暴露第二阶段是否命中 rollup；新增单测覆盖 trace_id 精确取数和 tenant 隔离，risk eval 覆盖接口级命中。
- 第 8 步已完成第一段：`http.rs` 按主题拆成 `src/http/*.rs` 小文件，主文件从 3821 行降到 520 行，所有 `src/http/*.rs` 都低于 800 行；只做结构拆分，不改路由行为，`http::tests` 已通过。
- 第 8 步已完成第二段：`lib.rs` 拆出 `src/tests.rs`、`src/engine/*.rs` 和 `src/engine/write_*.rs`，主文件降到 105 行，`WriteCoordinator` 方法按打开、写入、读取、控制台、评测、元数据、retention、检索、恢复提交分组；只做结构拆分，`route_metrics_reports_prometheus_format` 已通过。
- 第 8 步已完成第三段：继续拆分 `metadata.rs`、`evalkit.rs`、`vecindex_disk.rs` 和聚合测试文件，`yt-engine/src` 下所有 `.rs` 文件都低于 800 行；顺手清理拆分前遗留的三个 warning，`cargo test --offline --manifest-path yitrace-engine/Cargo.toml -p yt-engine` 和 `./scripts/eval_all.sh` 已通过。

暂未迁移：

- attrs sidecar 已有持久 cache + 内存 postings + 预算保护；postings 按需分页的磁盘 buffer manager 仍是后续优化。
- retention audit/policy 已接内存 postings；磁盘 postings 仍是后续优化。
- trajectory、traceDiff、loops、task traces 暂未迁入独立高性能磁盘物化索引版；当前先复用 `trace_rollup.dat`。
- 分布式相关模块仍保持不迁入。

### 第 1 步：迁移质量工具

迁移范围：

- `scripts/eval_all.sh`
- `scripts/test_all.sh`
- `scripts/bench_scale.sh`
- `scale_bench` example
- `risk_eval_matrix`
- 非分布式的 eval harness 增强

为什么先做：

- 后续每迁一个功能，都要有统一验证入口。
- 先有测试工具，后面不容易把问题拖到最后才发现。

验收：

```bash
cd yitrace-engine
cargo test --offline -p yt-engine --test risk_eval_matrix
cargo run -p yt-engine --example scale_bench --release -- --spans 10000
../scripts/eval_all.sh
```

### 第 2 步：迁移嵌入式 DB 包

迁移范围：

- `yitrace-node/`
- `yitrace-db-python/`
- `yitrace-db-rs/`

保留：

- 进程内 `EngineJsonApi`
- 单写者锁
- builder/helper
- search / trace / span / sessions
- attrs round-trip
- clean consumer pack 验证

不保留：

- 分布式 gateway 调用
- remote shard 配置
- 分布式状态字段

验收：

```bash
cd yitrace-node
npm install
npm run build
npm test

cd ../yitrace-db-python
python -m pip install -e .
python -m pytest

cd ../yitrace-db-rs
cargo test --offline
```

### 第 3 步：迁移高频字段和基础索引

迁移范围：

- `project_id`
- `skill`
- `mode`
- `call_site`
- `task_fingerprint`
- `loop_id`
- `validation_status`
- `review_status`
- `eval_status`
- `provider`
- `model`
- `tool_name`
- attrs 精确过滤

目标：

- 让这些字段成为查询、聚合、过滤的一等字段。
- 不要求一次把所有高性能 sidecar 都迁完，但 API 语义要先稳定。

验收：

```bash
cd yitrace-engine
cargo test --offline -p yt-engine attrs
cargo test --offline -p yt-engine trace_search
```

### 第 4 步：迁移单机读模型

迁移范围：

- `traceSearch`
- `traceAggregate`
- `traceTrajectories`
- `trajectoryGroups`
- `traceDiff`
- loops
- task traces

不迁移：

- Golden Path 自动治理
- Best Path 裁决
- challenger 策略

说明：

主线只提供底座数据：trajectory、diff、aggregate、loop/task 聚合。Golden Path 可以作为上层产品逻辑，后面单独评估。

验收：

```bash
cd yitrace-engine
cargo test --offline -p yt-engine --test eval_harness
cargo test --offline -p yt-engine trace_aggregate
cargo test --offline -p yt-engine trajectory
cargo test --offline -p yt-engine trace_diff
```

### 第 5 步：迁移 metadata / annotation / dataset / retention

迁移范围：

- annotation（已完成单机持久版）
- dataset association（已完成单机持久版）
- metadata store（已完成 `metadata.dat`，不改 trace/WAL/segment）
- retention plan（已完成单机显式 dry-run）
- retention apply（已完成 segment row 软删除，热 trace 跳过）
- retention audit（已完成 `metadata.dat` 持久化）
- retention policy（已完成显式 run-due，不启动后台线程）

原则：

- annotation / dataset association 是底座关系账本，不复制 trace 大字段。
- retention 必须显式调用，不做后台自动删除。
- 默认保护 annotation、dataset、snapshot、eval 引用的数据。
- 先 dry-run 再 apply。

验收：

```bash
cd yitrace-engine
cargo test --offline -p yt-engine --test risk_eval_matrix metadata_annotations_and_dataset_links_are_tenant_scoped_and_durable
cargo test --offline -p yt-engine --test risk_eval_matrix retention_plan_apply_audit_and_policy_are_durable
cargo test --offline -p yt-engine retention
cargo test --offline -p yt-engine storage_stats
```

### 第 6 步：迁移性能优化

迁移范围：

- trace aggregate rollup
- loop/task 索引
- metadata index
- read plan / fallback diagnostics
- cold/warm benchmark 脚本

当前已完成：

- 高频 attrs、`tool_name`、`model` 进入内存派生索引，不改 WAL/segment 格式。
- `/v1/trace-search`、`/v1/trace-aggregate`、`/v1/storage-stats`、`/v1/trace-trajectories`、`/v1/trajectory-groups`、`/v1/loops`、`/v1/loops/:loopId`、`/v1/tasks/:fingerprint/traces` 会先按可索引过滤拿候选 span key，再折叠候选 span。
- 响应返回 `readPlan.source`、`usedFilterIndex`、`candidateSpanKeys`、`scannedSegments`、`matchedSpans`、`fallbackReason`、`unsupportedAttrKeys`、`traceFetchSource`、`traceFetchSpanCount`、`traceFetchFallbackReason`。
- eval 已覆盖 attrs/tool/model 命中索引，以及只有 text 过滤时回退扫描。
- annotation/dataset association 查询已接 metadata postings，更新和软删除后会重建索引。
- `trace-aggregate` 无文本聚合已接内存 rollup；删除、upgrade、recover 后会同步重建，保持快路径可用。
- `trace-trajectories`、`trajectory-groups`、`loops`、`loop detail`、`task traces` 无文本查询已接 `trajectory_rollup`，使用 rollup 小字段生成路径摘要，`readPlan.scannedSegments=0`。
- 路径类读模型拿到候选 trace 后，会按 trace_id 从 rollup 取完整 span；不会为了少量候选 trace 重新组装全租户 span；接口响应会在 `readPlan.traceFetchSource` 暴露这一步。
- retention audit/policy 查询已接 metadata postings。
- 持久模式会写 `trace_rollup.dat` 作为 segment-only span 小字段缓存；recover 加载缓存后只需叠加 WAL tail，缓存坏掉会自动扫描 segment 重建；`trace-aggregate` 和无文本 trajectory/loop/task 共享这份缓存。
- attrs filter sidecar 已改成 postings 索引；持久模式会写 `filter_attrs.dat` 作为 segment-only cache，recover 加载后叠加 WAL tail，cache 坏掉会自动扫描 segment 重建；内存 postings 有预算保护，超预算时退回扫描 sidecar 行。

仍未完成：

- loop/task / trajectory 更细的独立持久化磁盘物化索引。
- attrs postings 按需分页的磁盘 buffer manager。

原则：

- 每个索引都要能解释“有没有命中索引”。
- 如果索引不可用，必须能 fallback 到正确结果。
- 性能测试要覆盖冷启动和热缓存。

验收：

```bash
cd yitrace-engine
cargo run -p yt-engine --example scale_bench --release -- --spans 100000 --cold
cargo run -p yt-engine --example scale_bench --release -- --spans 100000 --warm
cargo test --offline -p yt-engine index
```

### 第 7 步：文档回流

迁移范围：

- README
- README.zh-CN
- API_REFERENCE
- CURRENT_STATE
- package README

写法：

- 主线定位：本地优先、可嵌入、Agent TraceDB。
- 分布式只作为未来演进方向。
- 不写成已经成熟的分布式数据库。

验收：

- README 能在 5 分钟内让用户跑起来。
- Node/Python/Rust 嵌入式用法都有最小例子。
- API 文档和实际端点一致。

### 第 8 步：拆大文件

迁移范围：

- `http.rs`
- `lib.rs`
- `write_coordinator_*`

原则：

- 只拆结构，不混功能。
- 每次拆完都跑测试。
- 不引入分布式模块。

验收：

```bash
cd yitrace-engine
cargo test --offline
```

当前进度：

- `http.rs` 已拆出 `read_model_api.rs`、`metadata_api.rs`、`path_api.rs`、`console_api.rs`、`json_helpers.rs`、`trace_filter_helpers.rs`、`metadata_helpers.rs`、`read_model_helpers.rs`、`trajectory_helpers.rs`、`json_escape.rs`、`tests.rs` 等小文件。
- 拆分后 `http.rs` 只保留 HTTP socket、顶层 route、基础 `/v1/search` 和 trace 列表入口。
- 已验证：`cargo test --offline --manifest-path yitrace-engine/Cargo.toml -p yt-engine http::tests -- --nocapture`。

## 建议提交粒度

每一步单独提交：

1. `Add shared eval and benchmark tooling`
2. `Add embedded DB package surfaces`
3. `Promote common trace fields for filtering`
4. `Add single-node trace read models`
5. `Add metadata and retention foundations`
6. `Add read model indexes and benchmark coverage`
7. `Refresh README and API docs for embedded TraceDB`
8. `Split engine modules without behavior changes`

提交信息不要带 AI 工具名。

## 风险

最大风险不是代码写不出来，而是把分布式实验混进主线。

控制办法：

- 每次迁移前先列出文件白名单。
- 不从实验分支整提交 cherry-pick。
- 如果必须 cherry-pick，只 cherry-pick 后再手动删掉分布式依赖。
- 每步都跑对应 eval。
- 最后跑 `cargo test --offline` 和包级测试。
