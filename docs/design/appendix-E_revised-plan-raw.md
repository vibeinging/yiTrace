This is a synthesis task. I have four detailed design documents in the context and need to produce a revised landing plan that integrates them. No file exploration is needed since all source material is in the prompt. Let me write the deliverable directly.

修订版落地计划 — yiTrace 可观测平台（第二轮终裁）

---

## 1. 产品定位（一句话）

**yiTrace：面向中国客户的「完整 Agent 可观测平台」——双轨摄入（自家 Agent 深集成 + 框架无关接第三方 OTel/OpenInference/LangSmith-compat）、中小规模单机私有化开箱即用、以原生中文检索 + 语义 trace 召回 + 数据飞轮为差异化护城河，企业级治理（多租户/RBAC/审计/SSO）默认免费内置。**

定位的四个支点彼此咬合：完整平台（60-70% 可售价值在平台层）+ 双轨摄入（既做自家底座又框架无关）+ 中小单机（per-customer 私有化，单盘装下，不抄 SmithDB 分布式）+ 中文/语义召回差异化（对手开源全平台后唯一能赢的地方）。

---

## 2. 引擎主路线终裁

**终裁：推迟自研完整 LSM。v1 采用 `Lance（列式不可变 segment，2.1 stable）+ DataFusion（SQL/优化器，Rust 栈契合）+ 轻量自研行式 MemTable/WAL（热区与活 trace）+ 复用团队已有向量索引（HNSW/IVF/DiskANN）与自研中文倒排` 的混合路线。**

### 硬理由（四条，均可证伪、可量化）

1. **写入压力根本不到 LSM 的设计点。** XL 档（100M span/天）持续写入仅 ~1.2k span/s，峰值 6-12k span/s；自研 LSM 是为 >50k-100k/s 持续写入而生。第一轮范式里的「LSM（行式热区→列式 segment→分层 compaction）」在中小单机**退化为**：WAL + 行式 MemTable + 定时 flush 成 Lance fragment + 按时间分层合并小 fragment。这套 ~600-1500 行 Rust 可写出可靠版本，**不是完整 LSM**（无 leveled compaction、无 bloom 分层、无写放大调优）。把它叫"自研 LSM"会高估预算 3-5 倍。

2. **DuckDB/chDB 被单写进程架构性否决。** 摄入是持续写、UI 是持续读，DuckDB 单写进程 + 读写不可多进程并发；Quack 协议要到 2026 fall v2.0 才成熟。强行用要么把摄入/查询塞一个进程（互相阻塞），要么上 DuckLake+外部 catalog（丢掉"单机简单"优势）。**DuckDB 只配做附带的即席分析/导出旁路，不做主存储。**

3. **从零自研完整 LSM 在此规模是明确的过度工程，且直接和"60-70% 必须做的平台层"抢人抢预算。** 你们最直接对手 Langfuse 自己都不自研存储（ClickHouse+PG+Redis+S3 拼）；单机上更没理由自研 LSM。

4. **Lance 让差异化原样保留，且 K6（格式 breaking 迁移地雷）回应最强。** 中文倒排、语义向量、deletion vector、晚物化大字段全可外挂在 Lance 之上；**Lance File 2.1 已 stable 且官方承诺 2.1 向后兼容、breaking 留给 2.2、table 创建时钉死版本号并提供迁移命令**——这是当前最强的开源稳定性承诺，远优于 Vortex（激进、未承诺长期稳定）。配 N-2 回读契约 + golden corpus CI + `yitrace-migrate` 一键迁移 + Parquet 中性归档兜底。

### 自研 LSM 的重启触发条件（任一成立才评估，否则永不做）

持续写入 >30-50k span/s（即单客户 >4亿 span/天，超出本轮 per-customer 定义）；或产品转 SaaS 多客户聚合大盘（单盘装不下，回到对象存储路线）；或保留期×规模致单卷 >30-40 TB 且页缓存失效后 SLA 劣化；或 Lance 写侧/合并经实测成为无法靠参数解决的瓶颈。**预判 per-customer 线 GA 后 18-30 个月内不触发，很可能永不触发。**

> 需 POC 标定的不确定项：① 落盘 3.5 KB/span 是基于典型 LLM payload（3-8 KB 原文）+ 经验压缩比的估算，须用客户真实 trace 标定；② Lance 写侧在持续小写+频繁 flush 下的实测吞吐与 fragment 碎片化行为须用真实写入 pattern 验证。

---

## 3. 完整范围清单（v1 必备 / v2 / 后置）

### 3.1 存储引擎（底座）

| 项 | 级别 | 说明 |
|---|---|---|
| Lance 列存 + DataFusion SQL + 轻量行式 MemTable/WAL | **v1** | 主存储底座 |
| trace 树双编码（写侧邻接表 + 读侧 [pre,post] 物化区间） | **v1** | 第一轮范式 |
| merge-on-read（deletion/upgrade vector）+ 大字段晚物化 | **v1** | 评估分数回灌/PATCH 合并依赖 |
| 自研中文分词倒排 + thread_id 倒排 | **v1** | 差异化基座 |
| 本地 CAS（sha256 内容寻址）大字段存储 | **v1** | 不引对象存储 |
| `yitrace-migrate` + golden corpus 回读 CI + Parquet 归档出口 | **v1** | K6 防御 |
| 时间分层 compaction 优化 / fragment 碎片治理 | **v2** | 规模上来后调优 |
| DiskANN 磁盘图（上限客户全量语义） | **v2** | 复用现有能力 |

### 3.2 双轨摄入

| 项 | 级别 | 说明 |
|---|---|---|
| P1/P2 OTLP gRPC+HTTP + 内部规范模型 + raw_attrs 无损兜底 | **v1（M1）** | 吃 Python 主流框架 |
| P3 OpenInference 属性方言解析 | **v1（M1）** | Arize/Phoenix 生态 |
| P4 LangSmith-compat（/runs + /runs/multipart + dotted_order + attachment + /otel 入口） | **v1（M2）** | "指 endpoint 迁过来"卖点 |
| 历史数据导入工具（LangSmith export / Phoenix parquet → 批量回灌） | **v1（M2）** | 独立交付项，非"改 endpoint" |
| P6 轨 A 富语义 `vex.*`（thought/decision/tool.intent/reflection）+ embedding 旁路 | **v1（M3）** | 差异化护城河，喂语义召回/飞轮 |
| 认证 LangChain/LangGraph/LlamaIndex/Dify 四框架 + 钉版回归用例 | **v1** | 中国主流，其余 best-effort |
| P5 通用 SDK（Python 先） | **v1（M4）** | 自研框架客户兜底 |
| P5 通用 SDK（TS）+ Go/Ruby（OpenLLMetry 补位） | **v2** | TS/Go 覆盖弱，售前须识别 |
| OTel semconv-genai 版本追随常态化（专人季度跟踪 + 双发兼容） | **v1→常态** | OTel 仍 Development、v1.37 已 breaking |
| 尾部采样（error/高延迟/高成本保留） | **v2** | v1 默认 100% 全量 |

### 3.3 平台层（七模块）

| 模块 | v1 必备 | v2/后置 |
|---|---|---|
| **① Trace 浏览器** | 列表+多维过滤、树/瀑布虚拟化渲染、span 详情懒加载、文本/中文 diff、图片预览、**语义找相似 trace** | 活 trace 只读跟随、PDF/音频/视频预览、活 trace 在线语义召回 |
| **② 评估框架** | 规则评估器、LLM-judge+中文模板库、**中文 RAG 指标**、人工标注队列、数据集管理、实验运行+对比 | 在线评估、pairwise 队列、自定义评估器 |
| **③ 告警/异常检测** | 阈值告警、**国内 IM 通道（企微/钉钉/飞书）**、质量回归联动、轻量统计异常检测（3σ/MAD/EWMA） | 分组去重静默、季节性/Prophet、语义根因聚类 |
| **④ 仪表盘** | 4 个内建看板（cost/latency-p95p99/token/error）+ 筛选 + 下钻、用户/会话统计 | 自定义拖拽仪表盘、导出/定时报表 |
| **⑤ 项目/会话/搜索** | 项目管理、会话/线程视图、用户视图、**中文分词搜索 + 语义搜索 UI** | 保存搜索/收藏 |
| **⑥ RBAC/多租户/审计** | org/tenant/project 三级隔离、预置角色+项目级 RBAC、用户团队控制台、**OIDC SSO**、审计日志、基础脱敏、数据保留 | SAML/LDAP、SCIM、字段级权限 |
| **⑦ Prompt/Playground** | — | 全部 v2（prompt 版本库、playground、回灌运行中 agent） |
| **跨模块基础设施** | Platform Gateway（gRPC/REST+租户上下文）、内嵌 Postgres/openGauss 元数据库、前端地基（i18n/虚拟化/鉴权）、单机打包（Docker Compose 一键+冷备份恢复）、平台自观测 | — |

### 3.4 语义召回 / 飞轮

| 项 | 级别 | 说明 |
|---|---|---|
| 三区增量索引：活区在线（内存小 HNSW 单条插入，秒级）/温区近线（per-segment 子索引，分钟级）/冷区批量（compaction 重建） | **v1** | K3 诚实定级 |
| 带过滤 ANN 三分支（pre-filter 暴力 / ACORN in-filter / post-filter 迭代）+ 静态阈值 + 强制 re-rank | **v1** | 靠租户+时间分区裁剪压小搜索域 |
| 气隙 embedding：Qwen3-Embedding-0.6B（Apache 2.0）int8 ONNX/CPU + Matryoshka 256 维，异步队列+降级 | **v1** | 私有化必答，许可证已核实可商用 |
| 复用 EMBEDDING span 已有向量旁路截获 | **v1** | 显著降本（轨 A 尤甚） |
| 优化器自定义代价节点自动选分支 | **v2** | v1 先静态阈值 |
| 飞轮原语：轨迹导出（JSONL/few-shot）、few-shot 池、SOP 提炼、奖励视图 | **v1（基础）/v2（聚类）** | `addToDataset`/`attachScore` 已在引擎 |
| 自家 agent 在线召回回路（语义召回→few-shot 注入→轨迹回流闭环） | **v2** | 深集成杀手锏，依赖自家 SDK |
| GPU 顶配 4B embedding + 昇腾/CANN 路径 | **v2** | 政企国产卡 |

### 3.5 私有化打包 / 信创 / HA

| 项 | 级别 | 说明 |
|---|---|---|
| 单二进制/Docker Compose 一键部署 + 健康检查 | **v1** | 单机开箱即用卖点 |
| 形态 1：单机增量备份（WAL 归档+周期快照，RPO≤5min/RTO 30-90min）+ `yitrace-restore` 单命令 | **v1** | 默认 SKU |
| 元数据库用 openGauss（信创零额外成本，你们自己内核） | **v1** | K7 缓解 |
| 模型权重离线打包（气隙零外联） | **v1** | |
| 形态 2：主备只读热备（RPO 秒级/RTO 分钟级，双机盘翻倍）作付费 HA SKU | **v2** | 金融政企单独报价 |
| 信创认证（CPU/OS/数据库适配 + 昇腾推理栈） | **v2 并行启动** | 卖金融政企须 Phase 2 起跑，**单列预算 12-24 月七位数，非开发工时** |
| 形态 3：异地冷备（DR 兜底，RPO 24h） | **后置** | 高配合规条款 |

---

## 4. 团队构成（对比第一轮缺口）

**第一轮只算了 3-4 人存储内核，严重低估。本轮真实需要 ~13-16 人的产品工程团队。**

| 职能 | 人数 | 第一轮 | 缺口 | 职责 |
|---|---|---|---|---|
| **存储引擎（Rust）** | 3-4 | 3-4 ✓ | 0 | Lance 集成、行式 MemTable/WAL、merge-on-read、倒排、compaction、迁移工具。**注意：因不自研 LSM，原 3-4 人足够，省下的精力投平台** |
| **平台前端（React/TS）** | 3-4 | **0** | **+3-4** | ①④⑤ 重前端 + ②⑥ 管理界面；trace 浏览器虚拟化需 1 资深 |
| **平台后端（Rust 服务层）** | 2-3 | **0** | **+2-3** | Gateway、评估编排、告警引擎、RBAC、与引擎接口联调 |
| **摄入层（Rust + Py/TS）** | 2 | **0** | **+2** | 多协议网关、归一器、LangSmith-compat、instrumentor 认证/钉版、OTel 版本追随、历史导入工具 |
| **向量/飞轮（算法）** | 1-1.5 | **0** | **+1-1.5** | 三区索引、带过滤 ANN、embedding 部署调优、中文 RAG 指标、judge 模板、异常检测统计、飞轮原语 |
| **评估/算法（共享）** | 0.5-1 | 0 | +0.5-1 | 与向量职能部分重叠，可合并到 1.5-2 人算法小组 |
| **私有化/信创/部署** | 1 | **0** | **+1** | 打包、备份恢复、信创适配、气隙交付（与底座协作） |
| **合计** | **13-16** | **3-4** | **+10-12** | |

> 核心缺口结论：第一轮只看见了引擎内核，平台层（前端+平台后端）、摄入层、向量/飞轮、私有化交付**这四块约 10-12 人完全没算进去**。这正是"60-70% 可售价值在平台层"在团队预算上的对应——**钱和人必须从内核挪到平台**。

---

## 5. 现实路线图（dogfood → 交付在要的客户 → 商业化）

> **吸收第一轮红队"乐观 2-3 倍"批评**：平台层 v1 名义 90 人周，但 trace 浏览器虚拟化、评估编排、多租户贯穿三块有强串行依赖与联调，叠加引擎接口磨合、中文打磨、私有化部署测试，**现实化系数 ×1.5-1.8**。下列工时为**已含红队修正的现实值**。

### Phase 0 — 接口契约冻结（2-3 周，启动前必做）

与引擎团队签订四项硬接口，避免平台层返工：① tenant_id 是否引擎一等分区/过滤维度；② `q.aggregate` 聚合下推维度/算子覆盖（p95/p99 是否引擎算）；③ `c.attachScore` 回灌后能否按分数过滤排序；④ `q.semanticSearch` 向量来源（摄入时/查询时生成）。

### Phase 1 — Dogfood 自家 Agent 产品（约 4-5 个月，现实工时）

**目标：在自家 AgenticData 上把双轨摄入的轨 A + 引擎底座 + 可观测 MVP 跑通，自己先用起来。**

里程碑：
- M1（摄入底座）：P1/P2 OTLP + P3 OpenInference + 内部规范模型 + CAS + 引擎底座（Lance+DataFusion+MemTable+倒排+树编码）。
- 基础设施：Platform Gateway + openGauss 元数据库 + 前端地基。
- ① trace 浏览器（树/瀑布虚拟化/详情/中文搜索）+ ⑤ 会话线程 + ④ 内建仪表盘 + ⑥ 基础多租户/RBAC/审计。
- M3 起步：轨 A `vex.*` 富语义 + embedding 旁路 + segment flush 批量建向量索引。

**现实工时**：基础设施 16 人周 + ① 14 + ⑤ 9 + ④ 5 + ⑥ 18（部分）+ 摄入 M1/M3 ≈ 名义 ~70 人周 → **×1.6 现实 ≈ 110-115 人周 ÷ 有效 8-9 人并行 ≈ 4-5 个月**。退出标准：自家 agent 的 trace 在平台里看得见、搜得到（中文+语义）、能 dogfood 出第一批 bug。

### Phase 2 — 交付给"主动在要"的首个客户（约 5-6 个月，含 POC）

**目标：把可售完整平台交付给主动要求部署的客户，跑通双轨摄入轨 B + 评估 + 治理 + 私有化打包。**

里程碑：
- M2（迁移卖点）：P4 LangSmith-compat 全路径 + 历史导入工具 + 认证 LangChain/LangGraph/LlamaIndex/Dify。
- ② 评估框架全套（规则/judge/中文 RAG/标注队列/数据集/实验对比）。
- ③ 告警 + 轻量异常检测 + 国内 IM 通道 + 质量回归联动。
- ⑥ 补全 OIDC SSO + 脱敏 + 数据保留。
- ① 语义找相似 trace + 三区增量索引 + 带过滤 ANN + 气隙 embedding（Qwen3-0.6B）落地。
- 私有化形态 1（单机增量备份）+ 单二进制打包 + **首客 POC 标定**（真实 trace 标定落盘字节/embed 比例/选择度交叉点）。
- **信创认证并行启动**（若客户金融政企，单列预算）。

**现实工时**：② 19 + ③ 9 + ⑥ 余 + 摄入 M2/M4 + 语义召回 v1 + 私有化 ≈ 名义 ~80-90 人周 → **×1.7 现实 ≈ 140-150 人周 ÷ 9-10 人 ≈ 5-6 个月**（含 POC 联调与现场部署测试）。退出标准：首客签收，完整平台可售。

### Phase 3 — 商业化 / 增强 / 深集成（v2，约 3-4 个月）

⑦ Prompt/Playground + 在线评估 + 自定义仪表盘 + **自家 agent 在线召回回路**（飞轮闭环，深集成杀手锏）+ 活 trace 在线语义召回（大客户）+ pairwise/语义根因聚类 + SAML/SCIM/字段级权限 + **主备 HA SKU**（金融政企）+ GPU 顶配 embedding/昇腾路径 + 信创认证收尾。

**现实工时**：后置 37 人周 + 飞轮回路/在线语义 ≈ 名义 ~55 人周 → ×1.6 ≈ 90 人周 ÷ 10 人 ≈ **3-4 个月**。

### 总时间线（现实）

**Phase 0（0.5月）→ Phase 1 dogfood（4-5月）→ Phase 2 首客交付（5-6月）→ Phase 3 商业化（3-4月）= 从启动到完整商业化产品约 13-16 个月。** 其中"能 demo / 自家先用起来"在 ~5 个月，"可售完整平台交付首客"在 ~10-11 个月。

> 与第一轮乐观估计对比：若按第一轮口径（只算引擎+名义工时无修正）会得出 ~6-7 个月可售，**本轮明确判定那是低估 2-2.5 倍**——平台层、双轨摄入、私有化交付、中文/语义打磨、引擎接口磨合的真实成本必须计入。

---

## 6. 与 ClickHouse-Langfuse / SmithDB 的差异化小结

**核心战略判断：对手（ClickHouse+Langfuse MIT 全平台、Phoenix 零 gate）已经"开源全功能"，靠功能清单对齐赢不了。差异化必须压在它们结构上做不到或不愿做的地方。**

| 维度 | ClickHouse-Langfuse | SmithDB (LangSmith) | **yiTrace（我们）** |
|---|---|---|---|
| **部署形态** | 自托管需 ClickHouse+PG+Redis+S3 多服务重栈 | 分布式（Rust+DataFusion+Vortex+对象存储） | **单机单二进制/Compose 一键，单盘装下，零分布式复杂度** |
| **中文** | 英文 tokenizer | 英文 | **原生中文分词倒排 + 中文 RAG 指标 + 中文 judge 模板**（对手结构性弱项） |
| **语义召回** | 无 trace 级语义召回 | 无 | **带过滤的 trace 语义召回 + 相似轨迹 + 三区增量**（差异化引擎） |
| **企业治理** | project-RBAC/审计/SCIM/脱敏/数据保留**在企业版 $2,499/mo** | 企业版 | **默认免费内置**（私有化采购评分硬项，直接拆对手付费墙） |
| **格式稳定性** | ClickHouse 自有 | Vortex（激进、未承诺长期稳定，私有化迁移地雷 K6） | **Lance 2.1 stable + 向后兼容承诺 + N-2 回读契约 + 中性归档兜底** |
| **数据飞轮** | 无原生 | 弱 | **轨迹导出/few-shot 池/SOP 提炼/奖励视图 + 自家 agent 在线召回回路** |
| **信创** | 无 | 无 | **元数据库用 openGauss（自有内核零成本）+ 昇腾推理路径** |
| **摄入** | 自家 SDK + OTel | LangSmith 原生 + OTLP | **双轨：自家深集成（vex.* 富语义）+ 框架无关（OTLP/OpenInference/LangSmith-compat 迁移）** |

**一句话差异化**：对手是"英文 SaaS / 重栈自托管，企业治理收费、无中文无语义召回无飞轮"；我们是"**中文母语 + 语义召回 + 飞轮 + 单机开箱 + 企业治理免费 + 信创零成本**的 per-customer 私有化完整平台"。这五点对手要么结构上做不到（单机/中文/Lance 稳定性），要么商业模式上不愿做（治理免费），要么没有动力做（中文/信创/国内 IM）。

> 风险提示（诚实）：摄入完整度（尤其 TS/Go 覆盖、Dify 节点语义、OTel 实验态追随）易被低估，须按 Phase 2 计划专人钉版+回归；语义召回是否值客户溢价须按"埋点使用率→A/B 召回质量→加价档转化→agent 回路 ROI"四级验证，**别等引擎全做完才问市场**。

---

**关键待标定项汇总**（POC 必做，不可凭空承诺）：落盘 3.5 KB/span 字节数；每 trace 实际 embed 条数；目标硬件（可能国产 CPU）上 Qwen3-0.6B-int8 的 CPU 吞吐；带过滤 ANN 选择度交叉点；多段并查 recall 退化曲线；Lance 写侧持续小写吞吐与碎片化行为。