# yiTrace 当前态（唯一权威现状索引）

> 更新：2026-06-22
> 这篇是**现状的唯一权威入口**。docs/ 下文档很多（41+ 篇，含多轮红队过程产物），新读者从这里看，不要被历史过程文档带偏。
> 一句话：**项目走过一次大转向（openGauss 扩展 → 自研 Rust 引擎），当前承重的是 Rust 引擎；仓库里两套代码并存，本文讲清哪套是当前态。**

> **命名沿革**：项目原名 yiTrace（crate 前缀 `yt-`），2026-06-29 全面更名 **yiTrace**（顶层目录 `yitrace-*`、crate `yt-*`、Rust 标识符 `yt_`、Prometheus 指标 `yt_*`、Python SDK 包 `yitrace`）。文档里的历史叙事仍以 yiTrace 指代原 yiTrace。废弃的 openGauss 扩展（tracevault-extension）已随更名删除。

---

## 1. 当前承重代码（看这些）

| 目录 | 是什么 | 状态 |
|---|---|---|
| `yitrace-engine/` | **自研 Rust 引擎**（5 crate：core/wal/manifest/engine + 示例）。摄入/折叠/检索/eval/持久化全在这。**默认用纯 Rust 中文词级分词 `ChineseTokenizer`**（词典 DAG + 最大概率 DP，jieba 全量词典 34.9 万词内嵌，std-only）。 | **当前承重**，90+6 测试绿 |
| `yitrace-segstore-vortex/` | **列式段存储（Vortex）**，实现引擎的 `SegmentStore`。独立 crate、工作区外，**不污染零依赖骨架**。 | 已落地：写读 + 谓词下推 + 投影下推 + 默认压缩，7 测试绿 |
| `yitrace-tokenizer-jieba/` | **团队 jieba 词级分词接入**（FFI），实现引擎的 `Tokenizer`。Vortex 同款隔离、工作区外。 | 接缝 + ABI 契约 + 离线 mock 测试绿（3 测）；真库在构建机 `--features link` 接 |
| `yitrace-vecindex-graph/` | **团队 graph_index 向量 ANN 接入**（FFI），实现引擎的 `GraphIndex`。含**进图过滤回调**（C 遍历回调 Rust 谓词）。Vortex 同款隔离。 | 接缝 + ABI 契约（带过滤回调）+ 离线 mock 测试绿（4 测）；真库在构建机 `--features link` 接 |
| `yitrace-sdk/python`、`yitrace-sdk/typescript` | 打点 SDK，确定性 event_id 与引擎逐字节一致。 | 可用，各带测试 |

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
- **生产级路径（BM25 + ANN 两条接缝都铺到位）**：分词/向量索引都从引擎解耦成 trait 接缝（`Tokenizer` / `GraphIndex`），引擎开了 `CoordinatorBuilder` 注入口（`with_tokenizer` / `with_graph`），两条都走独立 FFI crate（`yitrace-tokenizer-jieba` / `yitrace-vecindex-graph`，Vortex 同款隔离）—— 团队库到构建机即 `--features link` 接，**引擎逻辑一行不动**。ANN 的护城河「进图过滤」已设计成跨 FFI 回调（C 图遍历回调 Rust 谓词，见 crate 的 ABI.md + 实测），不是搜完再筛。
- 一句话对外口径：**"自研"成立，"已生产级"还不成立（但 BM25 接 jieba 的路已铺通，差真库链接）。**

## 5. 已验证 vs 占位（诚实边界）

**性能（本机单机 release，2 万 span/128 维实测，仅供量级参考）**：摄入 ~4 万 span/s；向量建图 ~1.5k 点/s（HNSW 建图本就重，ef_construction 可调速度/召回）；BM25 检索 ~1500 QPS（0.65ms）；向量检索 ~1000 QPS（0.9ms）。关键优化：缓存 O(1) 访问、节点/向量缓存、段折叠缓存、**段级 key Bloom（跳无关段）+ BM25 WAND（剪枝，与暴力逐位一致）**。旋钮 `CoordinatorBuilder.with_ef_construction/with_ef_search/with_vector_cache_bytes`。跨段/TB 扩展性分析见 `docs/analysis/2026-06-24_检索跨段扩展性分析.md`。

**已是真的（有测试）**：确定性 event_id（跨语言逐字节一致）、四源读时折叠、快照隔离、崩溃重放幂等（含 upgrade 重叠窗口）、时间分层 compaction、重启不丢；中文 BM25 多概念召回完胜子串；**纯 Rust 中文词级分词**（词典 DAG + 最大概率 DP，jieba 全量词典内嵌默认装、引擎默认用，歧义"研究生命→研究/生命"判对、自有词典叠加、接真 BM25 端到端，8 测）；带过滤 ANN 召回表驱动实测（1% 选择性 post-filter 0.17 / in-graph 1.00，到 20% 收敛）；列式段谓词+投影下推；端到端 SDK/OTLP→HTTP→折叠→检索/eval/成本。

**仍是占位/待接**：团队 jieba / graph_index 真库链接（两条接缝/FFI crate/契约都已就位，差构建机上的库 + 真召回对标）、BM25 段内倒排 + block-max-WAND（内存倒排够用，上量再换）、LLM-judge eval、DataFusion 查询执行、索引驱动的 Vortex 随机取行。

**已暂缓但有止损点**：等保三级 / TLS / RBAC / 落盘加密 / PII 脱敏 / 持久防篡改审计。**止损条件**：任一真实金融/政企 PoC 立项 → TLS + RBAC + 持久审计日志必须先于该 PoC 落地（PoC 安全评审最低门槛）。

## 6. 已知工程债（骨架够用、上量必换，按优先级）

- ~~**GC 回收的安全条件 (3) 是近似**~~ → **已修复（2026-06-26）**：`reclaim` 现走持久化 GC 日志（`gc_log` 模块）—— MARK→fsync→unlink→DONE→fsync；`open_durable` 重启时扫 gc.log 补删"MARK 没 DONE"的段。崩溃安全测试 `gc_log_crash_after_mark_completes_delete_on_restart` 钉死。见 `docs/plans/2026-06-26_生产就绪路线.md` §1.1。
- ~~**`safe_version` 对 Tentative 读者返回 0**~~ → **已修复（2026-06-26）**：Tentative slot 现用 `observed_min_version`（登记时的 current 版本）当精确下限，不再"有未落定读者就完全不回收"。测试 `tentative_reader_uses_observed_min_version_not_zero` 钉死。避免高并发读时 dead_set 无限堆积。
- **Snapshot 强引用 `Arc<Current>`**：单例下不泄漏，无锁化（crossbeam-epoch）时要重设计。
- **CRC32 已换查表**（零依赖，已做）；BM25 logs 编码已换可逆转义（含 NUL/二进制/CJK 安全，已做）。
- **真 kill -9 崩溃测试**（§1.3）：`tests/crash_recovery_kill9.sh` + `server_durable` example，连续 20 次"灌→kill-9→重启→验证数据+检索"，零失败。顺手修了 agent_name 未被 BM25 索引的真 bug（用户按 agent 名搜会搜空）。
- **模糊测试**（§1.4）：`fuzz_fold_semantics_across_random_op_sequences` —— 8 个种子 × 80-119 步随机「ingest/flush/compaction/崩溃重放」，oracle 逐字段断言折叠结果一致、span 数无多无少。钉死随机组合下 last-non-null 折叠、compaction 不丢、崩溃幂等不塌。
- **`/v1/metrics` 端点**（§3.1）：Prometheus 文本格式，暴露 manifest 版本/活跃段数/dead 段数/内存表行数/活跃读者/WAL 尾/刷盘阈值/过滤属性/折叠缓存/Bloom/数据集 11 个指标。curl + 单测实测。Prometheus 可直接抓、Grafana 出看板。
- **在线快照备份**（§3.3）：`backup_snapshot(dest)` 走 pin 协议拿一致快照（GC 不会删被引用的段），拷 segments/ + wal.log + manifest.dat + vecindex/ + gc.log 到目标目录,得到可独立 `open_durable` 恢复的一致快照。备份期间读写不阻塞（snapshot 隔离）。测试 `backup_snapshot_restores_consistent_data` 钉死。
- **升级迁移**（§3.4）：`manifest.dat` 已带 `MAGIC + FORMAT_VER`（=1），decode 区分坏 magic/未来版本/老版本（各走明确日志而非静默 None）。`check_format(dir)` 返回 (磁盘版本, 引擎版本)；`migrate(dir)` 骨架（版本相等=Ok，老版本/未来版本=明确 Err）。`/metrics` 暴露 `yt_format_version`。当前无历史老版本数据，真实逐版本迁移在引入格式变更时扩展。

---

*相关：产品说明 §8 路线选择、技术文档、列式段落地计划、引擎选型决策 `2026-06-17_yitrace-engine-decision.md`。*
