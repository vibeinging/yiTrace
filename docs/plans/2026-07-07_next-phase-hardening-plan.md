# yiTrace 下一阶段硬化计划

> 日期：2026-07-07
> 状态：已启动
> 决策：暂停继续补产品功能，把下一阶段用于补盲区、压真实场景、打发版闭环。

## 背景

这几天已经把很多底座能力补齐了：嵌入式 DB、读模型、retention、Golden Path 证据、分布式 gateway、replication 原语、chaos eval、风险矩阵 eval。

现在继续堆 API 和产品功能，边际收益会下降。真正的风险已经从“有没有这个能力”变成：

- 能不能在真实数据量下长期稳定。
- 真实用户能不能安装、打包、升级。
- trace 里的敏感数据能不能管住。
- 分布式能力到底是数据路径，还是已经到了托管式生产集群。
- 对外叙事会不会变得太散，让用户不知道从哪里开始用。

所以下一阶段目标不是增加更多 endpoint，而是把当前能力变得更可信。

## 阶段目标

一句话：**功能冻结，转入生产成熟度。**

具体目标：

1. 跑真实规模和长时间稳定性，确认不会被数据量、段数、索引和 GC 拖垮。
2. 补安全和隐私模型，明确哪些敏感数据默认存、怎么脱敏、谁能看、怎么删。
3. 做多平台发版闭环，确保别人能稳定安装 Node/Python/Rust 包。
4. 把分布式边界说清楚并验证清楚：现阶段是可验证的数据路径，不包装成完整托管集群。
5. 收紧 API / SDK 合同，减少后续改动导致的隐性破坏。

## 明确不做

下一阶段先不做这些：

- 不继续扩 Golden Path 的自动治理逻辑。
- 不继续增加新的产品级 endpoint。
- 不做新的 Agent Memory 产品层逻辑。
- 不做 multi-leader active-active。
- 不把 gateway 包装成已经完整生产可托管的数据库集群。
- 不把 README 继续堆功能清单。

允许做的是：

- 测试、压测、发版、CI、文档、合同收敛。
- 为安全、隐私、稳定性、可恢复性服务的底层修复。
- 为现有功能补必要的验收门槛。

## P0：真实规模和长期稳定性

### 问题

当前测试很强，但多数是短时间、小数据、确定性场景。还没有证明它能承受真实 Agent 生产流量。

### 要做

1. 新建规模数据集生成器：
   - 10k spans：日常 smoke。
   - 100k spans：默认性能回归。
   - 1M spans：本地大样本。
   - 10M+ spans：发布前或夜间任务。
2. 新建 soak test：
   - 持续写入。
   - 持续 search / traceSearch / traceAggregate / vectorSearch。
   - 周期 flush / compaction / retention。
   - 插入 kill/restart。
3. 输出稳定指标：
   - 写入吞吐。
   - 查询 P50/P95/P99。
   - 内存曲线。
   - 段数曲线。
   - WAL 大小。
   - compaction 时间。
   - attrs postings / rollup / vector namespace 占用。
4. 把 `bench_qps` 升级成可复用的 eval/bench 套件，不只是一条 demo 命令。

### 验收

- `./scripts/eval_all.sh` 继续作为功能风险入口。
- 新增 `./scripts/bench_scale.sh` 或同等入口。（已完成第一版：`scale_bench` + markdown 报告输出）
- 100k spans 试跑时暴露出一个真实问题：vector namespace 查询仍是慢路径。当前先把 `--vector-queries` 从普通 `--queries` 中拆出来，并让 medium/large 默认只记录向量索引构建、不跑向量查询，让整体报告能完成；向量查询性能作为后续专项压测，显式打开 `--vector-queries` 验证。
- 100k spans 作为默认性能回归样本。
- 1M spans 本机可跑，并生成报告。
- 明确“当前机器、当前数据量、当前 QPS/P95”的基线，不再只说大概性能。

## P0：安全和隐私模型

### 问题

Agent trace 里会有 prompt、tool input/output、API key、数据库结构、内部错误和用户数据。TraceDB 比普通日志更容易存敏感内容。

### 要做

1. 写清敏感字段分类：
   - prompt / input_text / output_text。
   - logs。
   - tool args / tool result。
   - attrs。
   - external ids。
   - cost / token。
2. 加脱敏策略设计：
   - 默认不脱敏，但文档强提示。
   - 支持 ingest 前 SDK 脱敏。
   - 支持 server/embedded 层字段过滤 hook。
   - 支持保留 hash、删除原文。
3. 明确权限模型：
   - tenant。
   - project。
   - read trace。
   - export snapshot。
   - apply retention。
   - view raw text。
4. 明确审计要求：
   - retention apply。
   - export snapshot。
   - metadata mutation。
   - route table reload。
5. 明确合规边界：
   - 当前不是默认企业安全版。
   - 金融/政企 PoC 前必须先补 TLS、RBAC、持久审计、脱敏策略。

### 验收

- 新增安全设计文档。（已完成第一版：`docs/design/2026-07-07_security-privacy-hardening.md`）
- README 不把当前安全能力说过头。
- 风险 eval 中补至少一类“未带租户/错误租户/导出权限”的合同测试。
- `docs/CURRENT_STATE.md` 明确当前安全边界。

## P0：多平台发版闭环

### 问题

现在 Node/Python/Rust 包都有了，但 native 包最容易在真实用户机器上失败。

### 要做

1. Node：
   - root package。
   - macOS arm64/x64 optional package。
   - Linux x64/arm64 optional package。
   - Windows x64 optional package。
   - clean consumer 验证 ESM/CJS/native load。
   - Electron asar unpack 文档和验证。
2. Python：
   - macOS / Linux / Windows wheel 策略。
   - maturin build matrix。
   - clean venv install。
3. Rust：
   - crate 发布策略。
   - examples 编译。
   - docs.rs/readme 检查。
4. CI：
   - `eval_all.sh` 默认入口。
   - `eval_all.sh --packages` 包级入口。
   - `eval_all.sh --pack` 打包入口。
   - release tag 时跑完整 matrix。

### 验收

- 发版矩阵计划已建立：`docs/plans/2026-07-07_release-matrix-hardening-plan.md`。
- 至少 macOS arm64 本地完整可发。
- CI 里能跑 root package + optional package 的安装验证。
- 发布文档不再停留在“理论上怎么发”，而是给出实际命令和失败排查。

## P1：分布式边界和控制面

### 问题

现在分布式数据路径已经打通，但不是完整托管式生产集群。如果文档或 README 说得太满，会给用户错误预期。

### 要做

1. 明确当前状态：
   - gateway 是真实入口。
   - route table reload 是显式控制面。
   - follower read 有 bounded-stale 策略。
   - chaos eval 已覆盖 kill/promote/reload。
2. 明确未完成：
   - 自动 failover。
   - fencing。
   - 后台 route watcher。
   - 后台复制 worker。
   - snapshot bootstrap。
   - sealed segment / sidecar / metadata / GC log 同步。
3. 做更长的分布式 soak：
   - 多 shard。
   - 多 follower。
   - leader kill。
   - follower lag。
   - route table 变更。
   - strict / partial 查询。

### 验收

- README 和 CURRENT_STATE 使用同一句边界话术。
- `distributed_chaos_eval` 继续作为核心 eval。
- 增加长时间故障注入计划或脚本。

## P1：API 和 SDK 合同收敛

### 问题

现在端点、字段、SDK 方法很多。如果没有稳定合同，后续改动容易破坏 Node/Python/Rust/HTTP 其中一层。

### 要做

1. 梳理稳定 API：
   - ingest。
   - search。
   - traceSearch。
   - traceAggregate。
   - traces/sessions/span detail。
   - retention。
   - metadata。
   - vector namespace。
2. 建 fixture-based contract tests：
   - 同一批 fixture 同时跑 HTTP、Node、Python、Rust。
   - 输出字段做快照。
   - 错误输入也做快照。
3. 明确版本策略：
   - 哪些字段稳定。
   - 哪些字段是 beta。
   - 哪些字段只用于 diagnostics。

### 验收

- `risk_eval_matrix` 不只测 Rust 进程内 API，也逐步覆盖 Node/Python/Rust 包合同。
- 文档里明确稳定字段和 beta 字段。

## P1：产品叙事收敛

### 问题

当前能力很多，很容易让用户看不懂 yiTrace 到底是什么。

### 要做

继续坚持这个顺序：

1. 先是 trace-sdk 和运行回放。
2. 然后是本地 TraceDB。
3. 再是 Agent Memory / Golden Path 的证据底座。
4. 最后才是分布式数据路径。

不要一上来讲“数据库”“分布式”“Golden Path 自动治理”。用户第一天要理解的是：接入 SDK，看到一次 Agent 怎么跑，能搜索、能回放、能定位问题。

### 验收

- README 首屏不再堆功能。
- Quickstart 能在 5 分钟内跑通。
- `@yitrace/db` 是高级用法，不盖过 trace-sdk。

## P2：剩余功能只走需求单

后续如果要继续做功能，必须单独开需求单，并说明为什么现在要做。

候选但暂缓：

- Golden Path 引用计数和存储压缩。
- Golden Path Best/Challenger 自动裁决。
- 高性能 vector namespace HNSW。
- 完整自动 failover。
- RBAC 实现。
- UI 组件级测试和真实浏览器回归。

这些不是不要做，而是不能和“生产成熟度阶段”混在一起。

## 推荐执行顺序

| 顺序 | 主题 | 为什么先做 |
|---|---|---|
| 1 | 规模压测和长期稳定性 | 最容易揭穿当前系统是否真能扛住 |
| 2 | 多平台发版闭环 | 没有人能安装，就没有真实用户反馈 |
| 3 | 安全和隐私模型 | trace 数据太敏感，越早定边界越好 |
| 4 | API / SDK 合同 | 功能已经多了，必须防止后续破坏 |
| 5 | 分布式长时间故障注入 | 已有短链路 eval，下一步补时间维度 |
| 6 | 产品叙事收敛 | 防止项目看起来什么都做，反而没人敢接 |

## 下一步动作

建议先做三个不增加产品功能的任务：

1. 新建规模压测计划和脚本入口。（已完成第一版）
2. 新建安全/隐私设计文档。（已完成第一版）
3. 新建发布 matrix 和 clean consumer 验证计划。（已完成第一版）

这三件完成后，再决定是否进入具体实现。

## 2026-07-07 当前规模基线

已落盘报告：

- 初始基线：`docs/reports/scale/20260707T030643Z_scale-bench_100000_spans.md`
- 行式投影优化后：`docs/reports/scale/20260707T031611Z_scale-bench_100000_spans.md`
- traceSearch 两阶段补齐后：`docs/reports/scale/20260707T032110Z_scale-bench_100000_spans.md`
- storageStats read model cache 后：`docs/reports/scale/20260707T033135Z_scale-bench_100000_spans.md`
- traceSearch + storageStats read model cache 后 warm：`docs/reports/scale/20260707T033432Z_scale-bench_100000_spans.md`
- cold cache-bust 基线：`docs/reports/scale/20260707T033654Z_scale-bench_100000_spans.md`
- storageStats segment rollup 后 cold：`docs/reports/scale/20260707T035220Z_scale-bench_100000_spans_cold.md`
- storageStats segment rollup 后 warm：`docs/reports/scale/20260707T035508Z_scale-bench_100000_spans_warm.md`
- storageStats 预聚合块后 cold：`docs/reports/scale/20260707T050640Z_scale-bench_100000_spans_cold.md`
- storageStats 预聚合块后 warm：`docs/reports/scale/20260707T050837Z_scale-bench_100000_spans_warm.md`
- traceAggregate 预聚合块后 cold：`docs/reports/scale/20260707T061031Z_scale-bench_100000_spans_cold.md`
- traceAggregate 预聚合块后 warm：`docs/reports/scale/20260707T061210Z_scale-bench_100000_spans_warm.md`
- traceSearch rollup page reader 初版失败样本：`docs/reports/scale/20260707T062423Z_scale-bench_100000_spans_cold.md`
- traceSearch rollup page reader 修正后 cold：`docs/reports/scale/20260707T062859Z_scale-bench_100000_spans_cold.md`
- traceSearch rollup page reader 修正后 warm：`docs/reports/scale/20260707T062957Z_scale-bench_100000_spans_warm.md`

100k spans、100 次普通读查询、500 条向量索引、跳过向量读查询：

- 写入：100k spans 用时 10.661s，约 9380 spans/s。
- 向量索引：500 vectors 用时 2.047s，约 244 vectors/s。
- 普通搜索：P95 约 22ms。
- `trace_search_attrs`：P95 约 227ms。
- `trace_search_page_100`：P95 约 288ms，返回体较大。
- `storage_stats`：P95 约 983ms，P99 约 1979ms，是当前最明显的统计慢点。
- `vector_namespace`：默认 medium 不跑查询。实测 500 到 2000 条向量下单次查询会长时间卡住，应单独作为 P0 性能专项，不要混入默认规模基线。

已做一个小优化：`FileSegmentStore` / `InMemorySegmentStore` 支持 projected scan，`storageStats` 改用统计投影，不再把明显用不到的字段带进折叠。

优化后同档位结果：

- 写入：100k spans 用时 8.155s，约 12263 spans/s。
- 普通搜索：P95 约 13ms。
- `trace_search_attrs`：P95 约 130ms。
- `trace_search_page_100`：P95 约 252ms。
- `storage_stats`：P95 约 820ms，P99 约 871ms。

结论：投影有效，但不是最终解。`storageStats` 仍然接近 1s，下一步需要 trace-level byte summary / storage rollup，不能继续靠每次折叠全候选 span 来算统计。

随后对本地 `traceSearch` 做了两阶段读：先用轻投影完成过滤/排序/分页，再只对当前页 span key 读全字段补齐展示 JSON。

结果收益较小：

- `trace_search_attrs`：P95 约 132ms，和投影优化后基本持平。
- `trace_search_page_100`：P95 约 240ms，比 252ms 略好，但不是数量级提升。
- `storage_stats`：P95 约 829ms，仍然是最慢稳定瓶颈。

结论：当前持久段还是行式 WAL 编码，轻投影只能减少后续保留字段，不能避免记录解码和大量候选折叠。下一步要做的是读模型/汇总层：trace summary、storage byte rollup、分页候选物化，而不是继续微调 traceSearch JSON。

随后把 `storageStats` 接入 read model cache，并修正 scale bench 的 percentile 算法，新增 Max 列，避免 100 次查询时漏掉第一次冷 miss。

缓存后结果：

- `storage_stats`：P50/P95 约 0.001ms，说明重复刷新已经走缓存。
- `storage_stats` Max 约 871ms，说明第一次冷统计仍然慢。
- `trace_search_page_100`：P95 约 515ms，Max 约 1717ms，尾延迟仍明显。

结论：cache 解决的是重复查询，不解决冷查询。P0 仍要做真正的 storage rollup / trace summary；traceSearch 也需要 page candidate 物化或列表缓存，不能只看 warm path。

继续把 `traceSearch` 列表页也接入 read model cache，并给 `scale_bench` 加 `--cold-queries`：

- warm 报告中 `trace_search_attrs` P95 约 0.012ms，Max 约 752ms，说明重复查询已经走 cache，但第一次冷查询仍在。
- warm 报告中 `trace_search_page_100` P95 约 0.036ms，Max 约 301ms。
- cold 报告中 `trace_search_attrs` P95 约 127ms，`trace_search_page_100` P95 约 253ms，仍然代表真实冷路径。
- cold 报告中 `trace_aggregate_rollup` P95 约 120ms，说明 repeated warm cache 之前把 rollup 读取成本藏住了。
- cold 报告中 `storage_stats` P95 约 813ms，Max 约 934ms，仍是当前最明确的 P0 性能瓶颈。

后续所有性能报告必须同时看 warm 和 cold：warm 看产品重复刷新体验，cold 看底座是否真的变快。

随后把 `storageStats` 的冷路径接到 segment rollup：rollup row 现在额外保存 trace/span 时间边界、event count、payload/attrs/external id/字段估算字节。符合条件的 `storageStats` 查询直接扫 segment rollup row，不再读取和折叠完整 span；遇到 traceId、时间窗口、文本 contains、metadata filter、删除向量或不支持的 groupBy 时，会保守回退到 folded scan，并在 `readPlan` 里标出原因。

实测结果：

- cold 报告中 `storage_stats`：P95 从约 813ms 降到约 362ms，Max 从约 934ms 降到约 412ms。
- warm 报告中 `storage_stats`：P95 约 0.001ms，Max 约 357ms，说明重复刷新仍由 read model cache 吃掉。
- 新增 `storage_stats_rollup_matches_folded_scan_for_basic_totals`，同一批数据分别走 rollup 和 folded scan，对账 trace/span/session/event/error/bytes 统计。

结论：这一步把最慢的冷 `storageStats` 从“每次完整折叠扫描”降成“扫轻量 rollup row”，已经是有效的底座优化。但它仍然需要遍历匹配的 rollup row，不是最终的预聚合索引。下一步如果继续压性能，应做按 project/task/status 等高频维度的预聚合块，或者把 storage summary 写成 trace-level read model。

随后补了第一版 `storageStats` 预聚合块：每个 segment rollup 在内存里派生固定高频维度 bucket，当前覆盖空分组、单字段分组，以及常见二字段组合，例如 `project_id + validation_status`、`project_id + task_fingerprint`、`task_fingerprint + validation_status`。查询如果只用这些高频字段做精确过滤/groupBy，会走 `readPlan.spanReadIndex="storage_preaggregate"`；如果遇到未覆盖组合、traceId/time/text/metadata/delete-vector 等情况，仍回退到上一层 `storage_segment_rollup` 或 `folded_scan`。

实测结果：

- cold 报告中 `storage_stats`：P95 从 segment rollup 的约 362ms 降到约 7.8ms，Max 约 9.4ms。
- warm 报告中 `storage_stats`：P95 约 0.001ms，Max 约 7.6ms。
- 写入成本有小幅上升：100k ingest 约 9.3s，flush 约 0.08s，仍在可接受范围。
- 新增测试把预聚合、row rollup、folded scan 三条路径都卡住：预聚合命中、unsupported groupBy 回 row rollup、traceId filter 回 folded scan，并继续对账预聚合和 folded scan 的 trace/span/session/event/error/bytes 统计。

结论：`storageStats` 这个 P0 性能点已经从 1s 级降到 10ms 以内。下一步更值得盯 `traceSearch` 冷查询和 `traceAggregate` 冷查询，它们现在仍在百毫秒级；向量 namespace 查询仍是单独专项。

随后把同样的思路用到 `traceAggregate`：每个 segment rollup 除了轻量 span row，还派生一组常见聚合 profile bucket。bucket 保存 trace 去重、duration 分布、token/cost 汇总和少量 examples，所以返回 JSON 合同不变。当前重点覆盖 `project_id + validation_status + tool_name`、`project_id + task_fingerprint + validation_status`、`project_id + skill + mode` 等常见聚合；未覆盖组合仍回到 row rollup 或 folded scan。

实测结果：

- cold 报告中 `trace_aggregate_rollup`：P95 从约 318ms 降到约 3.0ms，Max 约 3.8ms。
- warm 报告中 `trace_aggregate_rollup`：P95 约 0.007ms，Max 约 3.4ms。
- 写入成本继续上升：100k ingest 约 12.3s，flush 约 0.12s。读收益明显，但 profile 数量已经开始形成写放大。
- eval 已覆盖 local、durable reopen、fanout cluster 三种 `aggregate_preaggregate` readPlan，同时保留跨段 span 回退 folded scan 的安全测试。

结论：`storageStats` 和 `traceAggregate` 两个统计型冷查询已经从百毫秒/秒级降到毫秒级。下一步不应继续无脑增加 profile，应该给 profile 做上限、配置或按需物化；剩下最明显的冷路径是 `traceSearch` 列表页，仍在 120ms 到 250ms 级。

随后给 `traceSearch` 列表页接入 segment rollup page reader。第一次尝试直接复用 `trace_aggregate_rollup_read`，结果不理想：虽然可以命中 rollup，但它会克隆所有命中 row，再排序和页内补全，100k cold 下 `trace_search_attrs` P95 反而到约 272ms，`trace_search_page_100` P95 到约 303ms。

修正后改成专用 page reader：只扫描 rollup row，收集轻量 `(trace_id, span_id, sort key)` 候选，排序后只返回当前页 key，再用原折叠读补齐当前页 input/output/log 预览。安全门仍沿用 rollup 规则：traceId/time/text/metadata/identity/token/cost 范围、删除向量、upgrade patch、跨段 span 都会回退 folded scan，并在 `readPlan` 中说明。

实测结果：

- cold 报告中 `trace_search_attrs`：P95 从约 130ms 降到约 36ms，Max 约 83ms。
- cold 报告中 `trace_search_page_100`：P95 从约 253ms 降到约 31ms，Max 约 111ms。
- warm 报告中 `trace_search_attrs`：P95 约 0.030ms，Max 约 75ms。
- warm 报告中 `trace_search_page_100`：P95 约 0.029ms，Max 约 124ms。
- 新增 eval 覆盖简单列表页命中 `readPlan.spanReadIndex="trace_search_rollup"`，以及文本搜索回退 `folded_scan`，同时保留 attrs sidecar 懒加载测试。

结论：`traceSearch` 列表页冷路径已经从百毫秒级降到几十毫秒级。下一步如果继续压性能，不应再在 HTTP JSON 层微调，而应做真正的 trace/span summary 物化、页候选增量 top-k、或者把 rollup row 持久格式压得更窄。

随后先补写侧可观测，而不是直接改 profile 生成策略。`/v1/metrics` 新增 trace rollup 缓存指标：cached segment 数、cached row 数、storageStats profile family/bucket 数、traceAggregate profile family/bucket 数。这个改动只读内存中已经载入的 rollup，不主动加载磁盘 sidecar，也不改变单机写入、WAL、manifest、折叠和查询结果。

原因是现在 `storageStats` / `traceAggregate` 读路径已经很快，但 profile 物化会带来写放大。下一步要做 profile 上限、按需物化或窄格式前，必须先能在真实数据上看到 profile 体量；否则容易为了分布式场景过早牺牲单机版默认体验。eval 已把这些 metrics 加进单机 traceAggregate 回归：flush 后必须能看到 rollup segment/row/profile/bucket，同时原来的 `aggregate_preaggregate` 查询结果保持不变。

继续补了 `TraceRollupProfileConfig`：通过 `CoordinatorBuilder` 可以限制 storageStats / traceAggregate 各自最多物化多少个 profile family，也可以限制单个 profile 的最大 bucket 数。默认仍是 full，`WriteCoordinator::new/open/open_durable` 不需要传配置，单机版行为不变。显式设置限额后，如果某个 profile 没有物化，查询会先回 segment rollup；只有 segment rollup 也不安全时才 folded scan。bucket 超限时不会截断结果，而是整个 profile 不物化，避免聚合数据变错。

新增 eval `trace_rollup_profile_budget_falls_back_without_changing_results`：把 storage/aggregate profile limit 都设成 0，flush 后 metrics 中 profile family/bucket 为 0，但 rollup row 仍保留；同一批数据的 `traceAggregate` 和 `storageStats` 会回到 segment rollup，并继续返回相同 spanTotal、bucket 和 trace/span 统计。这个用例专门防止后续为了控制写放大而影响单机正确性。

## 2026-07-07 分布式降级判断

当前分布式分支已经验证了不少底座接缝：route table、gateway fanout、snapshot lease、heartbeat、WAL pull、retry/熔断、真实多进程 eval。这些工作不是没有价值，但从产品节奏看，近期很长一段时间都不会真正需要托管式分布式部署。

因此主线应从“继续补分布式能力”切回“单机/嵌入式 TraceDB 成熟度”。分布式代码和测试可以保留在实验分支里作为后续参考，但不要继续把 route table watcher、自动 failover、后台复制调度、租约 fencing 这些复杂能力推进主线。它们会带来配置、文档、测试和心智负担，而短期用户并不会受益。

接下来建议按三类处理：

1. **保留并合入单机有价值的部分**
   - `traceSearch` / `traceAggregate` / `storageStats` 的 rollup、预聚合、readPlan、metrics。
   - `bench_scale.sh`、cold/warm 性能报告、`eval_all.sh` 风险入口。
   - profile 写放大指标和 `TraceRollupProfileConfig`，默认 full，不影响单机。
   - 安全、retention、metadata、vector namespace 这类单机也需要的底座能力。

2. **降级为实验资产**
   - remote gateway server、route table v2、heartbeat/failover、remote snapshot lease、网络 WAL pull。
   - 真实多进程分布式 eval 可以留作“未来分布式恢复点”，但不作为短期发布门禁。
   - 文档中对外不要强调“我们已经是分布式数据库”，只说“保留分片演进路径”。

3. **暂停开发**
   - 后台 route table watcher。
   - 自动 failover / fencing / leader lease。
   - 后台复制 worker。
   - sealed segment / sidecar / metadata / vecindex 全量复制。
   - 分布式生产部署体验。

如果要把这条分支收口，推荐不要整分支直接合主线。更稳的做法是从干净主线切一个 `codex/yitrace-single-node-hardening` 分支，只 cherry-pick 单机有价值的提交或文件块；分布式相关文件留在 `codex/yitrace-distributed-upgrade` 作为实验备份。这样能避免“为了以后可能用到的分布式”污染当前最重要的嵌入式体验。
