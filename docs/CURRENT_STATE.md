# yiTrace 当前态（唯一权威现状索引）

> 更新：2026-07-08
> 这篇是**现状的唯一权威入口**。docs/ 下文档很多（41+ 篇，含多轮红队过程产物），新读者从这里看，不要被历史过程文档带偏。
> 一句话：**项目走过一次大转向（openGauss 扩展 → 自研 Rust 引擎），当前承重的是 Rust 引擎；仓库里两套代码并存，本文讲清哪套是当前态。**

> **命名沿革**：项目原名 yiTrace（crate 前缀 `yt-`），2026-06-29 全面更名 **yiTrace**（顶层目录 `yitrace-*`、crate `yt-*`、Rust 标识符 `yt_`、Prometheus 指标 `yt_*`、Python SDK 包 `yitrace`）。文档里的历史叙事仍以 yiTrace 指代原 yiTrace。废弃的 openGauss 扩展（tracevault-extension）已随更名删除。

---

## 1. 当前承重代码（看这些）

| 目录 | 是什么 | 状态 |
|---|---|---|
| `yitrace-engine/` | **自研 Rust 引擎**（5 crate：core/wal/manifest/engine + 示例）。摄入/折叠/检索/eval/持久化全在这。**默认用纯 Rust 中文词级分词 `ChineseTokenizer`**（词典 DAG + 最大概率 DP，jieba 全量词典 34.9 万词内嵌，std-only）。 | **当前承重**，yt-core 9 + yt-engine 130 passed / 1 ignored + eval_harness 6 + manifest 4 + memtable 2 + wal 5 测试绿 |
| `yitrace-segstore-vortex/` | **列式段存储（Vortex）**，实现引擎的 `SegmentStore`。独立 crate、工作区外，**不污染零依赖骨架**。 | 已落地：写读 + 谓词下推 + 投影下推 + 默认压缩，7 测试绿 |
| `yitrace-tokenizer-jieba/` | **可选外部分词适配层**（FFI），实现引擎的 `Tokenizer`。Vortex 同款隔离、工作区外。 | 可选，不是默认依赖；默认已用自研纯 Rust `ChineseTokenizer` |
| `yitrace-vecindex-graph/` | **可选外部向量索引适配层**（FFI），实现引擎的 `GraphIndex`。含**进图过滤回调**（C 遍历回调 Rust 谓词）。Vortex 同款隔离。 | 可选，不是默认依赖；默认已用自研 `DiskGraphIndex` |
| `yitrace-sdk/python`、`yitrace-sdk/typescript`、`yitrace-sdk/rust` | 打点 SDK，确定性 event_id 与引擎逐字节一致。Rust SDK 是纯 std crate，适合只上报到 server 的 Rust agent。Python SDK 还提供服务端 embedded 接入层：`BufferedDbExporter` 后台单写线程、`SpoolDbExporter`/`SpoolConsumer` 落盘队列、`init_yitrace`/`shutdown_yitrace` 启停 helper、`yitrace consume-spool` CLI。 | 可用，各带测试；Python SDK clean consumer 验证覆盖 console script 和没有 `yitrace-db` 时的 fail-open |
| `yitrace-node/`、`yitrace-db-python/`、`yitrace-db-rs/` | Node/Electron、Python、Rust 的嵌入式 DB 包。都通过 `EngineJsonApi` 进程内调用引擎，不直接解析 WAL/manifest/segment 文件。Python 侧已有 `yitrace.connect(url/path)`、`DbExporter`、FastAPI router 和 `yitrace-db serve` 单 worker 服务入口；native 调用在 open/recover/route/flush/close 释放 GIL，避免长 IO 卡住 Python 线程。三种 embedded 包都会写 `.yitrace.lock` owner JSON（pid/host/data_dir/executable 等），第二个 writer 报错会带已有 owner。 | 可用：ingest/search/trace/span/sessions/traceSearch/aggregate/storageStats/trajectory/diff/loop/task/annotation/dataset association/retention helpers 有包级测试；`scripts/package_mode_eval.sh` 固化包形态回归，覆盖 Python SDK clean consumer、真实 Python embedded DB eval（buffered/spool/helper）、Rust/Node/Python lock 诊断；Node `pack:verify` 覆盖 ESM/CJS/native-path clean consumer；embedded 多进程共写仍禁止，服务端接入用单 writer helper 或落盘队列 |

**权威产品/技术入口**：`docs/2026-06-22_yitrace-产品说明.md`（决策层）、`docs/design/2026-06-22_yitrace-技术文档.md`（工程）、`docs/design/2026-06-22_列式段存储-vortex-选型与落地计划.md`（列式段）。

## 2. 历史 / 非当前态（别当现状读）

| 目录/文档 | 是什么 | 处置 |
|---|---|---|
| `tracevault-extension/` | **路线甲**：openGauss/yiTrace 内核扩展（SQL + 内核 AM），用内核自带 DiskANN/BM25/vex_jieba。曾自称"产物③ 数据库本体"。 | **已放弃为交付物**，作 schema/词典/trace 函数的**设计参考保留**。讲"自有 IP"不以它为准（算法是内核的）。 |
| `docs/design/appendix-A … appendix-Q` | 路线甲时期的设计 + 多轮红队过程产物（多在讨论 openGauss/内核边界/信创约束）。 | **历史溯源，非当前态。** 当前态以本文 + 产品/技术文档为准。 |
| `2026-06-16/17 的 tracevault-* 与 l1-datafusion-lance` 等 | 早期架构稿（含已否决的 Lance 方案）。 | 历史。Lance 已否决，列式定 Vortex。 |

## 3. 路线转向一句话（详见产品说明 §8 + `2026-06-17_yitrace-engine-decision.md`）

openGauss 是华为 IP，用它做信创护城河等于把叙事控制权交给一个能顺手做掉你的竞品；且买 ClickHouse/openGauss 会把自有 BM25/graph_index 挤成旁路 sidecar，"自有 IP 当一等索引"的产品命题塌。→ **自研 Rust 引擎，让两块索引作一等公民**；列式格式是整套存储里唯一值得买现成的一件 → Vortex。

## 4. "自有 IP" 的真实成色（避免商务误读）

- **结构上成立**：中文检索 + 图式向量是自己的引擎逻辑，不是外包给内核再调它的算子。
- **中文检索已生产级**：引擎默认用**自研纯 Rust 中文词级分词 `ChineseTokenizer`**（词典 DAG + 最大概率 DP，jieba 默认模式等价），**jieba 全量词典 34.9 万词内嵌、开箱即用**，支持自有词典叠加（`with_user_dict`）。
- **多租户逻辑隔离（共享索引 + 强制过滤，全流程打通）**：`tenant_id` 贯穿 SpanFields/WAL/wire/属性边车/折叠 FoldedSpan/Vortex 列式段。隔离覆盖**全部读写路径**：BM25 文本检索 + 向量找相似（进图过滤）+ 列表 `list_traces`/读 `read_spans_query`（`TraceQuery.tenant_id`）。**HTTP 服务层强制**：tenant 从 `X-Tenant-Id` 鉴权头取（非请求体，客户端不能越权），`/v1/search` + `GET /v1/traces` 都隔离。**SDK**：Python/TS 打点 `trace(name, tenant_id=)` 透传到全部 span。待补：OTLP 的 tenant 属性映射、硬强制中间件（现为服务层注入约定）。
- **向量索引已落盘 + 多层 HNSW**：自研**磁盘型图向量索引 `DiskGraphIndex`**（参考 yiTrace graph_index 落盘三招：定长槽位节点 + 向量单独定长存按需读 + **字节预算缓冲池 `vector_cache_bytes`**，对齐 `vector_buffers`）。**多层 HNSW**：底层(0)+向量在磁盘、上层图稀疏常驻内存+快照持久，顶层贪心下沉→底层 beam+进图过滤；**重启不 rebuild**。`open_durable` 默认用它，append 友好（只写不刷、提交点批量 fsync）。召回@10 ≥ 0.85。参数 `DiskGraphConfig`（m / vector_cache_bytes / ef_construction / ef_search）。**待升级**：SIMD/量化（PQ/SQ）、邻居选择启发式。
- **可替换接缝（不等于默认依赖）**：分词/向量索引都从引擎解耦成 trait 接缝（`Tokenizer` / `GraphIndex`），引擎开了 `CoordinatorBuilder` 注入口（`with_tokenizer` / `with_graph`）。默认路径已经是自研实现：`ChineseTokenizer` + `DiskGraphIndex`。`yitrace-tokenizer-jieba` / `yitrace-vecindex-graph` 只是可选外部适配层，用来接客户或团队已有库、做对标、留退路；不属于主线待完成能力。
- 一句话对外口径：**yiTrace 默认就是自研中文分词 + 自研磁盘图向量索引；FFI 适配层是可选增强，不是核心缺口。**

## 5. 已验证 vs 占位（诚实边界）

**性能（本机单机 release，2 万 span/128 维实测，仅供量级参考）**：摄入 ~4 万 span/s；向量建图 ~1.5k 点/s（HNSW 建图本就重，ef_construction 可调速度/召回）；BM25 检索 ~1500 QPS（0.65ms）；向量检索 ~1000 QPS（0.9ms）。关键优化：缓存 O(1) 访问、节点/向量缓存、段折叠缓存、**段级 key Bloom（跳无关段）+ 内存 BM25 WAND（剪枝，与暴力逐位一致）**。旋钮 `CoordinatorBuilder.with_ef_construction/with_ef_search/with_vector_cache_bytes`。跨段/TB 扩展性分析见 `docs/analysis/2026-06-24_检索跨段扩展性分析.md`。

**已是真的（有测试）**：确定性 event_id（跨语言逐字节一致）、四源读时折叠、快照隔离、崩溃重放幂等（含 upgrade 重叠窗口）、时间分层 compaction、重启不丢；中文 BM25 多概念召回完胜子串；**纯 Rust 中文词级分词**（词典 DAG + 最大概率 DP，jieba 全量词典内嵌默认装、引擎默认用，歧义"研究生命→研究/生命"判对、自有词典叠加、接 BM25 端到端，8 测）；自研磁盘型多层 HNSW + 带过滤 ANN 召回表驱动实测（1% 选择性 post-filter 0.17 / in-graph 1.00，到 20% 收敛）；列式段谓词+投影下推；端到端 SDK/OTLP→HTTP→折叠→检索/eval/成本。

**当前单机读模型（有测试）**：`/v1/trace-search`、`/v1/trace-aggregate`、`/v1/storage-stats`、`/v1/trace-trajectories`、`/v1/trajectory-groups`、`/v1/traces/diff`、`/v1/loops`、`/v1/loops/:loopId`、`/v1/tasks/:fingerprint/traces` 已有正确优先的单机版，并已接入 Node/Python/Rust 嵌入式包。`trace-search`、`trace-aggregate`、`storage-stats`、`trace-trajectories`、`trajectory-groups`、`loops`、`loop detail`、`task traces` 会对 `project_id`、`skill`、`mode`、`call_site`、`task_fingerprint`、`loop_id`、`validation_status`、`review_status`、`eval_status`、`tool_name`、`model` 等常用过滤走 attrs sidecar，并在响应里返回 `readPlan`，说明是否命中索引、候选 span 数和扫描段数。attrs sidecar 内存态是 postings，不再每次过滤扫全量 span；postings 有内存预算，单个值太宽或总条目太多时禁用对应 postings，退回扫描 sidecar 行，结果不丢。持久模式会写 `filter_attrs.dat` 作为 segment-only sidecar cache，重启时加载 cache 后再叠加 WAL tail。无文本的 `trace-aggregate` 会走 `aggregate_rollup`；无文本的 trajectory/loop/task 读模型会走 `trajectory_rollup`，只读路径所需小字段，不读 input/output/logs；拿到候选 trace 后，会按 trace_id 从 rollup 取完整 span，不再为少量候选 trace 重新组装全租户 span，`readPlan.traceFetchSource` 会暴露这一步是否命中 rollup。持久模式还会写 `trace_rollup.dat` 作为 segment-only rollup cache。两份 cache 都不是数据源，损坏或版本不匹配会自动扫描 segment 重建。删除、retention apply、upgrade 和 recover 后会按当前快照同步重建 rollup/sidecar。带 text 的聚合和路径查询仍会回到正确扫描。postings 按需分页的磁盘 buffer manager、loop-task 独立磁盘索引仍是后续优化，不是当前实现。

**当前元数据和 retention 账本（有测试）**：`/v1/annotations` 和 `/v1/dataset-associations` 已有单机持久版，支持 tenant 隔离、attrs 过滤、annotation 更新/软删除、dataset item 关联，并接入 Node/Python/Rust 嵌入式包；annotation/dataset 查询会先走内存 metadata postings，再做最终校验。`/v1/retention-plan`、`/v1/retention/apply`、`/v1/retention-audits`、`/v1/retention-policies`、`/v1/retention-policies/run-due` 已迁入主线：先 dry-run，再显式 apply；默认保护 annotation、dataset、snapshot、eval link、path memory 引用；只软删除已 flush 的 segment row，跳过 MemTable/WAL tail 热 trace；audit/policy 随 `metadata.dat` 持久化，查询按 id/tenant/source/name/enabled 走内存 postings。它不复制 trace 大字段，也不改 WAL/segment 格式。后台自动执行仍是后续优化。

**仍是占位/待接**：BM25 段内持久倒排 + 分块 postings + 段内 block-max-WAND（当前已有内存倒排 WAND，够 2 万级 demo 和早期 dogfood；百万级、冷启动和低内存场景需要升级）、LLM-judge eval、DataFusion 查询执行、索引驱动的 Vortex 随机取行、`yitrace-db` wheel clean consumer 全链路验证。外部 jieba / graph_index FFI 只算可选适配和对标，不是核心缺口。

**P1 当前处理口径**：默认自研分词和默认自研磁盘图索引已经是主线能力，不再把“接外部 jieba / graph_index”列为 P1。P1 继续关注两类事：第一是生产入口（服务端 embedded helper/spool 已落地，继续补配置、强制 tenant、审计、限流、健康指标）；第二是索引规模化（attrs postings 磁盘分页、loop/task/trajectory 独立物化索引、正式规模压测、BM25 段内持久倒排、删除/更新 postings、ANN 量化/SIMD）。可选 FFI 只有在客户或团队项目明确要复用外部库时再做。

**已暂缓但有止损点**：等保三级 / TLS / RBAC / 落盘加密 / PII 脱敏 / 持久防篡改审计。**止损条件**：任一真实金融/政企 PoC 立项 → TLS + RBAC + 持久审计日志必须先于该 PoC 落地（PoC 安全评审最低门槛）。

## 6. 已知工程债（骨架够用、上量必换，按优先级）

- ~~**GC 回收的安全条件 (3) 是近似**~~ → **已修复（2026-06-26）**：`reclaim` 现走持久化 GC 日志（`gc_log` 模块）—— MARK→fsync→unlink→DONE→fsync；`open_durable` 重启时扫 gc.log 补删"MARK 没 DONE"的段。崩溃安全测试 `gc_log_crash_after_mark_completes_delete_on_restart` 钉死。见 `docs/plans/2026-06-26_生产就绪路线.md` §1.1。
- ~~**`safe_version` 对 Tentative 读者返回 0**~~ → **已修复（2026-06-26）**：Tentative slot 现用 `observed_min_version`（登记时的 current 版本）当精确下限，不再"有未落定读者就完全不回收"。测试 `tentative_reader_uses_observed_min_version_not_zero` 钉死。避免高并发读时 dead_set 无限堆积。
- **Snapshot 强引用 `Arc<Current>`**：单例下不泄漏，无锁化（crossbeam-epoch）时要重设计。
- **CRC32 已换查表**（零依赖，已做）；BM25 logs 编码已换可逆转义（含 NUL/二进制/CJK 安全，已做）。
- **真 kill -9 崩溃测试**（§1.3）：`tests/crash_recovery_kill9.sh` + `server_durable` example，连续 20 次"灌→kill-9→重启→验证数据+检索"，零失败。顺手修了 agent_name 未被 BM25 索引的真 bug（用户按 agent 名搜会搜空）。
- **模糊测试**（§1.4）：`fuzz_fold_semantics_across_random_op_sequences` —— 8 个种子 × 80-119 步随机「ingest/flush/compaction/崩溃重放」，oracle 逐字段断言折叠结果一致、span 数无多无少。钉死随机组合下 last-non-null 折叠、compaction 不丢、崩溃幂等不塌。
- **`/v1/metrics` 端点**（§3.1）：Prometheus 文本格式，暴露 manifest 版本/活跃段数/dead 段数/内存表行数/活跃读者/WAL 尾/刷盘阈值/过滤属性/过滤 postings/被预算禁用的过滤 postings/折叠缓存/Bloom/数据集等指标。curl + 单测实测。Prometheus 可直接抓、Grafana 出看板。
- **在线快照备份**（§3.3）：`backup_snapshot(dest)` 走 pin 协议拿一致快照（GC 不会删被引用的段），拷 segments/ + wal.log + manifest.dat + vecindex/ + gc.log 到目标目录,得到可独立 `open_durable` 恢复的一致快照。备份期间读写不阻塞（snapshot 隔离）。测试 `backup_snapshot_restores_consistent_data` 钉死。
- **升级迁移**（§3.4）：`manifest.dat` 已带 `MAGIC + FORMAT_VER`（=1），decode 区分坏 magic/未来版本/老版本（各走明确日志而非静默 None）。`check_format(dir)` 返回 (磁盘版本, 引擎版本)；`migrate(dir)` 骨架（版本相等=Ok，老版本/未来版本=明确 Err）。`/metrics` 暴露 `yt_format_version`。当前无历史老版本数据，真实逐版本迁移在引入格式变更时扩展。

---

*相关：产品说明 §8 路线选择、技术文档、列式段落地计划、引擎选型决策 `2026-06-17_yitrace-engine-decision.md`。*
