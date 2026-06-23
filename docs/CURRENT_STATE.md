# yiTrace 当前态（唯一权威现状索引）

> 更新：2026-06-22
> 这篇是**现状的唯一权威入口**。docs/ 下文档很多（41+ 篇，含多轮红队过程产物），新读者从这里看，不要被历史过程文档带偏。
> 一句话：**项目走过一次大转向（openGauss 扩展 → 自研 Rust 引擎），当前承重的是 Rust 引擎；仓库里两套代码并存，本文讲清哪套是当前态。**

---

## 1. 当前承重代码（看这些）

| 目录 | 是什么 | 状态 |
|---|---|---|
| `yitrace-engine/` | **自研 Rust 引擎**（5 crate：core/wal/manifest/engine + 示例）。摄入/折叠/检索/eval/持久化全在这。 | **当前承重**，93 测试绿 |
| `yitrace-segstore-vortex/` | **列式段存储（Vortex）**，实现引擎的 `SegmentStore`。独立 crate、工作区外，**不污染零依赖骨架**。 | 已落地：写读 + 谓词下推 + 投影下推 + 默认压缩，7 测试绿 |
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
- **但当前是验证级，非生产级**：BM25 走 CJK bigram（未上 jieba 词级）、ANN 是单层 NSW（无量化/SIMD）。算法在手、精度/性能尚未对标 jieba/DiskANN。
- **生产级路径**：接团队 graph_index / jieba 倒排的 C ABI FFI（同套算法）。
- 一句话对外口径：**"自研"成立，"已生产级"还不成立。**

## 5. 已验证 vs 占位（诚实边界）

**已是真的（有测试）**：确定性 event_id（跨语言逐字节一致）、四源读时折叠、快照隔离、崩溃重放幂等（含 upgrade 重叠窗口）、时间分层 compaction、重启不丢；中文 BM25 多概念召回完胜子串；带过滤 ANN 召回表驱动实测（1% 选择性 post-filter 0.17 / in-graph 1.00，到 20% 收敛）；列式段谓词+投影下推；端到端 SDK/OTLP→HTTP→折叠→检索/eval/成本。

**仍是占位/待接**：jieba 词级分词、生产级 ANN（量化/SIMD）、LLM-judge eval、DataFusion 查询执行、索引驱动的 Vortex 随机取行。

**已暂缓但有止损点**：等保三级 / TLS / RBAC / 落盘加密 / PII 脱敏 / 持久防篡改审计。**止损条件**：任一真实金融/政企 PoC 立项 → TLS + RBAC + 持久审计日志必须先于该 PoC 落地（PoC 安全评审最低门槛）。

## 6. 已知工程债（骨架够用、上量必换，按优先级）

- **GC 回收的安全条件 (3) 是近似**：`reclaim` 用当前内存版本近似"已提交 manifest 不再引用"，且 `safe_version` 与 `dead_set` 锁之间无联合原子性。骨架在「段 id 永不复用 + compaction 只产新段不复活旧段」下安全；真实实现要上**持久化 GC 日志**（写"将删 seg X" → fsync → 删 → 标记完成），防"删一半崩溃、manifest 没更新"。见技术文档 GC 小节。
- **`safe_version` 对 Tentative 读者返回 0**（保守、完全不回收）；上量换 observed_epoch 精确下限。
- **Snapshot 强引用 `Arc<Current>`**：单例下不泄漏，无锁化（crossbeam-epoch）时要重设计。
- **CRC32 已换查表**（零依赖，已做）；BM25 logs 编码已换可逆转义（含 NUL/二进制/CJK 安全，已做）。

---

*相关：产品说明 §8 路线选择、技术文档、列式段落地计划、引擎选型决策 `2026-06-17_yitrace-engine-decision.md`。*
