# feature/yitrace-distributed-upgrade 可回流主线能力分析

日期：2026-07-07

## 结论

`feature/yitrace-distributed-upgrade` 相对 `main` 很大：194 个文件，约 6.4 万行新增。它不适合整分支合并到主线。

但这个分支里不只有分布式能力，也有很多单机版、嵌入式版能直接借鉴的东西。推荐做法是：从 `main` 新建一个主线回流分支，按小块摘取，不把 gateway、远程复制、route table、failover 等分布式代码带进来。

## 值得优先回流主线

### 1. 嵌入式 DB 包

涉及目录：

- `yitrace-node/`
- `yitrace-db-python/`
- `yitrace-db-rs/`

价值：

- 让 Node/Electron、Python、Rust 项目可以不启动 HTTP server，直接嵌入 yiTrace。
- 这不是分布式能力，是采用门槛能力。
- 对 AgenticData 这类项目很关键。

回流建议：

- 优先回流。
- 保留进程内 `EngineJsonApi` 边界。
- 不把 distributed gateway API 暴露给这些包。
- 回流后必须跑 clean consumer、Node/Python/Rust 三套包级测试。

### 2. 单机读模型和查询能力

涉及能力：

- `traceSearch`
- `traceAggregate`
- `trajectoryGroups`
- `traceTrajectories`
- `storageStats`
- `traceDiff`
- loops / task traces
- 一等字段过滤：`project_id`、`skill`、`mode`、`task_fingerprint`、`validation_status` 等

价值：

- 这些能力解决的是 trace 数据如何被使用，不依赖分布式。
- 对 Agent 复盘、评测、最佳路径发现、成本分析都有用。
- 主线现在如果只保留原始 trace 查询，产品价值会偏弱。

回流建议：

- 优先回流，但拆成两块：
  - 查询 API 和基础 read model。
  - 性能索引和 rollup。
- 不要把 Golden Path 治理当成底座硬能力一起塞进来；底座只提供 trajectory、diff、aggregate、metadata 这些原料。

### 3. 元数据、标注、dataset 关联、保留策略

涉及能力：

- annotation
- dataset association
- metadata store
- retention plan / apply / audit / policy
- storage stats

价值：

- 这是 TraceDB 从“能查日志”变成“能沉淀数据资产”的关键。
- 单机版同样需要清理老数据、保护被标注的数据、统计空间。
- 这部分比 Golden Path 更像底座能力。

回流建议：

- 可以回流。
- retention 默认只能 dry-run 或显式执行，不能加后台自动删除。
- 保护规则要保守：被标注、被 dataset 绑定、被 snapshot/eval 引用的数据默认不删。

### 4. 性能和 eval 基础设施

涉及文件：

- `scripts/bench_scale.sh`
- `scripts/eval_all.sh`
- `scripts/test_all.sh`
- `yitrace-engine/crates/yt-engine/examples/scale_bench.rs`
- `yitrace-engine/crates/yt-engine/tests/eval_harness.rs`
- `yitrace-engine/crates/yt-engine/tests/risk_eval_matrix.rs`

价值：

- 这些不是分布式功能，是质量保障。
- 能回答“过滤有没有走索引”“性能有没有退化”“长尾场景有没有覆盖”。

回流建议：

- 优先回流脚本和测试框架。
- scale bench 报告不要全量搬回主线，保留脚本和少量最新报告即可。
- 分布式相关 eval 暂时留在实验分支。

### 5. 文档和 README 的表达方式

涉及文件：

- `README.md`
- `README.zh-CN.md`
- `docs/API_REFERENCE.md`
- `docs/CURRENT_STATE.md`
- `docs/design/2026-07-07_readme-narrative-redesign.md`
- `docs/plans/2026-07-06_github-seo-star-growth-plan.md`

价值：

- 分支里的 README/API 文档比主线更接近当前产品形态。
- 但是分布式描述太重，直接搬会让外部用户误以为项目已经是成熟分布式数据库。

回流建议：

- 借鉴写法，不原样合并。
- 主线 README 应该先讲：
  - 本地优先 Agent TraceDB。
  - Node/Python/Rust 可嵌入。
  - trace 查询、检索、聚合、评测、成本分析。
  - 分布式作为演进方向，而不是默认卖点。

### 6. 代码拆分

涉及文件：

- `yitrace-engine/crates/yt-engine/src/lib.rs`
- `yitrace-engine/crates/yt-engine/src/http.rs`
- `yitrace-engine/crates/yt-engine/src/http/*`
- `yitrace-engine/crates/yt-engine/src/write_coordinator_*`

价值：

- 分支把超大文件拆小了，符合“每个类/模块尽量少于 800 行”的方向。
- 这对长期维护有价值。

风险：

- 这块和功能改动混在一起，直接 cherry-pick 很容易带入分布式依赖。

回流建议：

- 单独做一次“纯拆文件、不改行为”的重构。
- 先拆 `http.rs` 和 `lib.rs`，测试必须证明行为没变。
- 分布式模块不进入主线。

## 暂时不要回流主线

以下能力先留在实验分支：

- `remote_gateway*`
- `route_table.rs`
- `replication_api.rs`
- `shard_client*`
- `gateway_server.rs`
- distributed chaos / process / production / replica eval
- network replication
- heartbeat / failover
- dynamic route table
- remote snapshot lease
- follower read
- 多 shard 写入路径

原因：

- 用户已经明确说短期用不到完整分布式。
- 这些能力会抬高主线理解成本。
- 如果没有真实生产部署约束，合入主线容易变成维护负担。

## 建议回流顺序

1. 质量工具先回流：`eval_all.sh`、`bench_scale.sh`、risk eval、scale bench。
2. 嵌入式 DB 包回流：Node、Python、Rust 三个包，但不带分布式入口。
3. 单机 read model 回流：traceSearch、aggregate、trajectory、diff、loop/task。
4. metadata/annotation/dataset/retention 回流：先保守实现，删除必须显式触发。
5. 性能索引和 rollup 回流：只保留单机可解释、可测试的路径。
6. README/API 文档重写：主线定位改成“本地优先、可嵌入、以后可分片演进”。
7. 最后再做代码拆分：只拆结构，不混功能。

## 推荐新分支

如果要开始回流，建议从 `main` 新建：

```bash
git checkout main
git checkout -b feature/yitrace-mainline-hardening
```

不要直接从 `feature/yitrace-distributed-upgrade` 继续改，否则很难保证不会把分布式代码一起带回来。
