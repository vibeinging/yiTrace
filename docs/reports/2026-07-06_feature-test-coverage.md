# 2026-07-06 功能点测试覆盖清单

## 判断标准

每个功能点至少要有这些测试：

- 正常路径：功能能跑通，关键字段返回正确。
- 错误输入：坏 JSON、坏字段、越界值要返回明确错误。
- 租户隔离：带 `X-Tenant-Id` 时不能串数据。
- 持久化：写盘后重启还能查到，删除后重启仍不露出。
- 分布式功能：还要测 fanout、部分失败、严格一致性、读副本和 route table 变化。

## eval 框架口径

目标口径：所有项目级测试用例都要挂到 eval 框架下，统一输出 case、check、pass/fail 和失败原因。

硬性要求：

- 每个 eval case 必须真实执行代码，不能只检查静态文本，不能用 mock 结果冒充。
- API 类 case 必须通过 `EngineJsonApi` 或真实 socket 走真实入口。
- 需要数据时，case 自己先造最小数据，再通过真实写入入口灌入，最后用真实查询入口读回验证。
- 负向 case 不能只看 400，还要查回确认坏数据没有落入引擎。

当前已经补了通用 API eval runner：

- `evalkit::ApiEvalCase`
- `evalkit::ApiEvalStep`
- `evalkit::run_api_eval_suite`
- `evalkit::EvalSuiteReport`

已迁入真实 API eval 的旧 HTTP 合同：

- SDK 线格式 ingest 后查 trace summary / trace detail。
- 外部字符串 trace/span/session id、attrs、搜索过滤。
- `X-Tenant-Id` 租户隔离，body 里的 `tenant_id` 不能越权。
- OTLP `/v1/traces` 生态入口 ingest 后查回。
- 中文搜索、agent 过滤、坏 search body 返回 400。

后续普通测试迁移时，按下面几类接入 eval 框架：

| 类型 | 是否真实执行代码 | 是否走 eval 框架 | 说明 |
|---|---|---|---|
| API 合同 eval | 是 | 是 | 走 `EngineJsonApi`，每个 case 可包含 seed / execute / verify 多个真实步骤 |
| 业务场景 eval | 是 | 是 | 走 `eval_harness.rs`，用业务 trace 串起 traceSearch、aggregate、Golden Path、retention、cluster read model |
| 持久化 eval | 是 | 迁移中 | 应由 eval case 创建 data dir、写入、重启、验证 |
| 真实进程 / socket eval | 是 | 待接入 | `distributed_process_eval.rs` 会起真实 shard/gateway 子进程并走 TCP，后续要包装成 eval suite |
| kill -9 eval | 是 | 待接入 | `crash_recovery_kill9.sh` 会真 kill -9，后续要输出 eval 报告 |

仍有少量旧测试还没迁移，不能算最终状态：

- 纯单元测试仍散在模块内。
- `FailingShardClient` 这类失败模拟还没包装成 eval case。
- 控制台 `mockApi` 测试还只是前端 mock 稳定性测试。
- 部分 `InMemorySegmentStore` 测试还不是磁盘路径 eval。

## 本轮新增用例

| 功能点 | 新增用例 | 目的 |
|---|---|---|
| eval 框架 | `evalkit::ApiEvalStep` + `run_api_eval_suite` | 让一个 eval case 包含多步真实 API 执行链 |
| 摄入线格式 | `eval_harness::ingest_wire_contract_eval_cases` | `attrs` 不是 object、`event_type/status` 越界时必须返回 400，且坏数据不能写入 |
| HTTP 摄入入口 | `eval_harness::ingest_wire_contract_eval_cases` | `/v1/ingest` 成功后必须通过 `/v1/trace-search` 查回自己写的数据 |
| API 入口合同 | `eval_harness::api_entrypoints_use_real_eval_steps` | ingest、外部 ID、租户、OTLP、中文搜索都走 seed / verify 真实步骤 |
| 分布式 gateway 恢复 | `distributed_process_eval::gateway_process_recovers_after_backend_shard_restart` | 真实启动 3 shard + 1 gateway，杀掉一个 shard 后确认降级，再重启同 data dir 同端口并查回全量；检查结果进入 eval report |

同时修正解析行为：

- `attrs` 缺失或 `null` 仍允许。
- `attrs` 是 string / array / bool / number 时返回 400。
- `event_type` / `status` 大于 255 时返回 400。

## 功能点覆盖

| 功能点 | 当前状态 | 已有测试 | 还缺的测试 |
|---|---|---|---|
| SDK 线格式摄入 `/v1/ingest` | 偏少，已开始补 | SDK 到 WireRecord、外部字符串 id、attrs、token/cost、坏 JSON、本轮新增坏 attrs 和越界值 | 每个必填字段缺失都要有 400；重复提交同一批 HTTP 事件后 trace/token 不翻倍 |
| OTLP 摄入 `/v1/traces` | 中等 | OTLP 正常摄入、坏 body、租户头覆盖 body tenant | 更多 OpenInference 字段别名、resource/scope 多层属性覆盖顺序、异常 span 状态映射 |
| 单写 WAL / 重启 / 崩溃 | 较强 | WAL 重启、flush 后重启、crash replay 幂等、kill -9 脚本、随机 op fuzz | HTTP 维度的重复 ingest 幂等小用例；更多半写 WAL frame 损坏用例 |
| 多写控制 | 偏少 | route table 拒绝双 writable、手动 promote 切换写主、并发读写回收 | 同一 data dir 双进程打开锁；双写主配置 reload 拒绝后旧 route table 不变 |
| 单机查询列表 | 中等 | trace 列表、summary 聚合、token/cost 汇总、时间过滤、tenant 隔离 | 分页边界、空结果、坏 query 参数、外部 trace id 与数字 id 混用 |
| Trace 详情 / Span 详情 | 中等 | 瀑布、树结构、父子关系、大字段晚物化、span batch、snapshot、tenant 隔离 | span batch 中部分 span 不存在；cursor/limit 越界；snapshot hash 稳定性专项 |
| Session / Turn | 中等 | session timeline、turn 顺序、cache 命中和失效、多轮分类 | 空 session、跨租户同 session id、分页边界、异常轮状态 |
| 中文 BM25 搜索 | 中等 | 中文检索、字段过滤、tenant 隔离、text domains、重启重建索引 | 空 text、超大 text、特殊符号、只搜 agent/tool/model 域的独立小用例 |
| 向量搜索 / 混合搜索 | 中等 | vector、hybrid、过滤进图、disk vector 重启、namespace vector | 向量维度不一致、空向量、k 越界、filtered ANN recall/perf 固定样本 |
| attrs 过滤 / 一等字段 | 较强 | attrs sidecar、flush/recover、未索引 attrs 回退、数组 attrs、schema 字段过滤 | 复杂 JSON object attr 过滤语义；同 key 多事件覆盖后索引一致 |
| 读模型 cache / rollup | 中等 | cache hit/miss、写入失效、traceAggregate rollup、fallback 原因、loop/task sidecar | 多租户 cache key 隔离小用例；retention 后 cache 必须失效的小用例 |
| Annotation / Dataset | 较强 | 创建、查询、分页、更新、软删、反向过滤、持久化、租户隔离 | 坏状态值、坏 target、span 级和 trace 级同时存在时的精确过滤 |
| Golden Path | 较强 | 创建、确认、adherence、evidence、export、health、retention 后 source retained、租户隔离 | 拒绝 / deprecated 状态查询；candidateTraceId 不存在；重复创建同 source 的行为 |
| Retention / Storage | 中等 | dry-run、保护 annotation/dataset/golden/snapshot/eval/path memory、apply、compact、audit、policy run-due | deleteBeforeTs 缺失或非法；MemTable/WAL tail 热 trace 跳过的 HTTP 小用例；重复 apply 幂等 |
| 分布式读写 | 较强但还不是生产集群 | 进程内 cluster、真实多进程 shard、3 shard + 1 gateway fanout、gateway 后端 shard 重启恢复、partial/strict、follower read、snapshot lease、WAL replication | 后台 route watcher、failover fencing、复制断点恢复长循环、sealed segment/metadata/vecindex 同步 |
| Node 嵌入式 DB | 中等偏强 | ESM/CJS、open/close、lock、ingest/search/traces/sessions/metadata/retention/golden path/reopen | 每个 JS API 的坏参数测试；Electron asar/native path 模拟 |
| Python SDK | 中等 | event_id fixture、span 生命周期、父子 span、tokens、异常、HTTP exporter headers/retry/buffer cap | exporter HTTP 非 2xx 细分、timeout、close 多次调用、并发 span |
| TypeScript SDK | 偏少 | event_id fixture、span 生命周期、父子 span、tokens、异常 | HTTP exporter retry/buffer cap、BigInt wire 全字段、async close 失败场景 |
| 控制台前端 | 偏少 | HTTP client header/query/path encode、搜索字段映射、mock 分页和 trace 树、build | 组件渲染测试、错误态、空态、真实 API 合同截图/浏览器回归 |
| 性能 | 有工具，缺门禁 | `bench_qps`、并发请求、并发读写回收、kill -9 加压 | 固定阈值脚本：最低 QPS、P95、索引构建时间、内存上限 |

## 下一批优先级

P0：

1. `/v1/ingest` 必填字段逐个缺失时都返回 400。
2. HTTP 重复摄入同一批事件，trace/span/token/cost 不重复累计。
3. 同一 data dir 双打开必须失败，保护单写者。
4. `/v1/search` 向量维度不一致、空向量、非法 `k` 返回 400。
5. retention apply 缺 `deleteBeforeTs` 返回 400，重复 apply 保持幂等。

P1：

1. trace/span 分页边界。
2. 多租户 cache key 隔离。
3. Golden Path 不存在 candidate/source 的错误返回。
4. TypeScript HTTP exporter retry/buffer cap。
5. 控制台组件空态和错误态。

P2：

1. 性能门禁脚本。
2. 分布式长时间故障注入。
3. Electron 打包路径模拟。
