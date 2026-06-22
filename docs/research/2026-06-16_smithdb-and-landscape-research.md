> **勘误（来自对抗性核查阶段，可信度=high）**：本底稿绝大多数事实经独立联网复核为「已证实」，仅以下细节需更正——
> ① `vortex-datafusion` 当前版本实为 **0.75.0（2026-06-12）**，非 0.69.x；
> ② Arize Phoenix「Postgres 1–5M span/天上限」系**编造数字**，真实公开数据点为「约 200M spans / >2TB 时某生产部署变得不可用」；
> ③ Vortex 入 Linux Foundation 孵化时间为 **2025-08**，非 2026；
> ④ SmithDB 线程过滤「268ms」官方标注为 **P95**（非 P99）；官方同页正文「12x」与 Key Takeaways「15x」自相矛盾；
> ⑤ SmithDB 内部「Slicer/Dicer 路由 / sticky routing」等具体组件名属推测，官方材料未出现（Cluster Manager、对象存储、小 Postgres、无状态服务 已证实）；
> ⑥ 「国内尚无 Agent 可观测性专用 DB」无法被证伪、未检索到反例，作为定位假设可接受，但**销售前需一手坐实**（另见红队对竞品的更新情报）。
>
> 以下为各调研 agent 的原始产出，保留全部细节。

---

# 调研：SmithDB 内幕与真实性

I now have comprehensive, cross-verified information from the official LangChain blog, the ZenML LLMOps technical deep-dive on the inverted index, the Vortex project pages, and search snippets of the co-founder's X threads. I have enough to write the detailed report with proper confidence labeling.

---

# LangChain SmithDB 深度调研报告

> 调研日期：2026-06-16。SmithDB 发布于 2026-05-13，晚于模型训练截止，全部结论基于联网检索。每条结论已标注「已证实 / 未证实 / 推测」。

## 信息源可信度说明

| 等级 | 来源 | 用途 |
|---|---|---|
| 一手权威 | LangChain 官方博客 [Introducing SmithDB](https://www.langchain.com/blog/introducing-smithdb) | 定位、架构、性能数字 |
| 一手权威 | 联合创始人 Ankush Gola 的 X 线程（[1](https://x.com/ankush_gola11/status/2054661816249360553)、[2](https://x.com/ankush_gola11/status/2054681251513254260)，正文需登录，仅取搜索摘要） | 设计动机、DataFusion 用法 |
| 一手技术 | ZenML LLMOps Database：[全文检索倒排索引实现](https://www.zenml.io/llmops-database/building-full-text-search-for-agent-traces-with-custom-inverted-index-on-object-storage) | 倒排索引内部细节 |
| 一手技术 | [Vortex 官方/GitHub](https://github.com/vortex-data/vortex)、[Spiral 博客](https://spiraldb.com/post/vortex-a-linux-foundation-project) | 存储格式细节 |
| 二手/转载 | [softwarechains](https://www.softwarechains.com/insights/smithdb-agent-observability-langsmith)、Phemex/KuCoin 快讯 | 交叉验证，权重低 |

⚠️ 关键提醒：不同来源对性能数字和「12x vs 15x」表述不一致，对「distributed vs 单机/无状态」表述也有冲突，下文逐项标注。

---

## (1) SmithDB 是否真实存在 / 发布时间 / 官方定位

**【已证实】SmithDB 真实存在。**
- 在 LangChain 年度大会 **Interrupt** 上发布，官方博客发布日 **2026-05-13**，作者为联合创始人 Ankush Gola。
- 官方一句话定位（原文引用）：**"SmithDB is LangSmith's data layer purpose-built for agent observability and evaluation workloads."**（SmithDB 是 LangSmith 为「Agent 可观测性与评估」工作负载量身打造的数据层。）
- 技术栈（原文）：**"built in Rust with Apache DataFusion and Vortex, with heavy customizations for LangSmith's unique workloads."**

**官方原文要点（核心论点）：**
> "Agent traces have outgrown the databases built to hold them."（Agent trace 已经超出了用来承载它们的数据库的能力边界。）

官方列出的"trace 是新型数据"的三大特征（与你给的场景需求高度吻合，已证实）：
1. 单条 trace 可包含**数千甚至数万个中间 span**，以及**大体积、无上界的 payload**；
2. **"a run is a sequence of events, not a single immutable row"**（一个 run 是事件序列，不是一行不可变记录）——长运行 span 会在完成前多次更新；
3. **乱序到达**：原文 **"the start event for an agent span can arrive minutes, maybe even hours before an end event"**（start 事件可能比 end 事件早几分钟甚至几小时到达）——即你说的"早上出生、下午死亡"。

**官方声称当前生产状态【已证实为官方声明，真实生产占比无法独立核实】：**
- 承载 **100% 的美国 Cloud ingestion** 与 **100% 的 tracing UI 查询流量**；
- 元数据过滤、feedback、文本检索、树过滤等主要 filter 已全量切换；
- run rules、bulk export、experiments 等集成"接近完成"；
- **自托管（self-hosted）部署列在 "Soon"（即将推出），发布时尚未 GA**。

---

## (2) 架构组件与各自职责

**【已证实】整体形态：对象存储 + 一个小 Postgres 元数据库 + 一组无状态服务。**

```
        ┌─────────────────────────────────────────────┐
        │          无状态服务（Stateless Services）        │
        │  Ingestion │ Query │ Compaction │ ClusterMgr  │
        └─────────────────────────────────────────────┘
                  │  写/读 segment 文件      │ 读写 segment 元信息
                  ▼                          ▼
        ┌──────────────────┐      ┌──────────────────────┐
        │   对象存储 Object   │      │   小 Postgres 元数据库   │
        │   Storage          │     │  (Metastore)           │
        │  segment / 大字段    │     │ segment 位置/时间界/行数  │
        │  文件 (Vortex 格式)  │     │ /delete & update 向量  │
        └──────────────────┘      └──────────────────────┘
```

| 组件 | 职责（来源：官方博客 + softwarechains 转述，已证实） |
|---|---|
| **Ingestion Service（摄取）** | 接收 trace 写入，按 **partition / 时间桶（time bucket）** 批量聚合，写出**不可变文件**到对象存储；其**本地缓存**保留最新数据供查询直读（见下文"读新数据"）。 |
| **Query Service（查询）** | 暴露查询接口，运行**为本工作负载定制的执行计划（custom execution plans）**，理解 LangSmith run 语义与对象存储布局；利用 **SSD + 内存缓存**。 |
| **Compaction Service（压实）** | 重写 segment，应用删除、TTL 过期、索引合并；采用**时间分层压实（time-tiered compaction）**——近期数据少压实（还会等 end 事件），老数据合并成大文件。 |
| **Cluster Manager（集群管理）** | 把服务节点按 **key range** 分配，采用**粘性路由（sticky routing）**，让相关查询命中同一节点缓存以提升命中率。 |
| **对象存储** | trace 数据与大字段文件的持久层（私有化可用本地盘 / 自带 MinIO，官方强调"无需管理本地磁盘"）。 |
| **小 Postgres（Metastore）** | 记录 **segment 元信息**：位置、时间边界（time bounds）、行数、以及 **update/delete 向量（deletion vectors）**。是整套系统的"目录/真相源"。 |

**对你的项目的直接启示**：这是一个典型的"存算分离 + 控制面用 Postgres"模式。你们团队有 PostgreSQL/openGauss 内核能力，**用 Postgres 当 metastore 几乎零成本**，这是可以直接复用的点。

---

## (3) 存储格式 Vortex + 查询引擎 DataFusion + 对象存储 backed LSM 设计

### 3.1 Vortex 存储格式【已证实】

- **本质**：新一代**列式文件格式**，由 **Spiral（SpiralDB）** 开发，**2026 年已捐给 Linux Foundation（LFAI&Data，孵化阶段）**，背后支持者包括 Microsoft、Palantir、Snowflake、NVIDIA、InfluxData。
- **核心设计目标**：**object-store native（对象存储原生）**、compute-agnostic（可对接 DataFusion/DuckDB/Spark/Polars 等）、pushdown（在压缩数据上直接计算）。
- **逻辑/物理分离**：logical schema 与 physical layout 解耦，**可插拔编码**，级联压缩，内置 **BtrBlocks / FastLanes / FSST / ALP** 等研究级算法，与 **Apache Arrow 零拷贝**互通。
- **关键性能宣称（vs Parquet，来自 Vortex 官方，已证实为其官方说法）**：
  - **随机读快 ~100x**；
  - 扫描快 **10–20x**；
  - 写快 **~5x**；
  - 压缩比与 Parquet 相当。
  - 对象存储往返次数：**Vortex 典型 1–2 次 vs Parquet 3 次**（智能预取 + segment coalescing）。
  - Microsoft 在 Iceberg 中用 Vortex 替换 Parquet：**运行时降 30%、存储降 20%**；并探索 **"zero-copy LSM compaction"**（压实时可直接搬运压缩字节而不解压）。

> **为什么 SmithDB 选 Vortex 而不是 Parquet**【推测，但论据充分】：trace 场景的核心是"随机访问单个 run / 完整 trace 树"，Parquet 的随机读在对象存储上很差（需读 footer + row group 元数据多次往返），而 Vortex 的 100x 随机读和 1–2 次往返恰好命中。Vortex 的 LSM-friendly compaction 也直接支撑了 SmithDB 的 LSM 设计。

### 3.2 DataFusion 查询引擎【已证实】

- Ankush Gola 原文（X 线程摘要）：**"We built custom execution plans specifically tuned for our workloads and storage backend, and DataFusion made it straightforward to plumb..."**
- 即：DataFusion 提供**可扩展的 Rust 查询引擎骨架**（SQL 解析、逻辑/物理计划、向量化执行），SmithDB 在其上**自定义物理执行计划**，让算子理解 run 语义和对象存储布局。
- Vortex 已有官方 `vortex-datafusion` crate（当前版本 0.69.x），二者集成是现成路径。

### 3.3 "对象存储 backed LSM-Tree" 具体设计【已证实核心、部分推测细节】

官方原文（核心句，已证实）：
> **"SmithDB is built as an object-storage backed log-structured merge tree (LSM), which buffers writes in memory, flushes them to durable storage as immutable sorted batches, and periodically compacts those segments together."**

拆解为写入/查询路径：

**写路径（Write Path）：**
```
小 span 写入 → Ingestion 内存写缓冲(memtable)
   → 按 partition/time bucket 攒批
   → flush 成「不可变 sorted batch（immutable segment，Vortex 文件）」到对象存储
   → 在 Postgres metastore 登记 segment（时间界/行数/位置）
   → Compaction 后台按 time-tiered 策略合并 + 应用 delete/TTL 向量
```

**查询路径（Read Path）——这是工程难点，官方点名 3 个创新：**

1. **渐进式时间窗查询（Progressive querying / time-window scan）**【已证实】
   原文意译：不对全量数据排序，而是 **"walk backward through time and build a bounded time window over the newest candidate segments"**（沿时间倒序，对最新候选 segment 构建有界时间窗）。这解决"按租户取最新 N 条 run"这类高频查询——不必扫全历史。

2. **读取尚未落对象存储的新数据**【已证实】
   查询规划器**直接扫 ingestion 节点的本地缓存文件**，而不是等数据写到对象存储——这就是支撑 **"活 trace（live trace，运行中即可查）"** 的机制。

3. **查询时合并 segment（merge-on-read）+ deletion vector**【已证实】
   删除/TTL 不是同步删行，而是 metastore 给 segment **挂 deletion vector**，查询时合并、压实时才真正重写文件。**update 同理**——长运行 span 的"更新"在新 segment 追加事件，查询时按 run_id 合并多个事件成最终状态（呼应"run 是事件序列"）。

4. **大字段延迟物化（late materialization of large fields）**【已证实】
   核心行只存**指向大字段文件的指针**，只有当查询真正 project 到该字段时才去对象存储取大 payload——这就是**多模态大 payload 引用**的实现。

---

### 3.4 全文检索倒排索引（最有工程含量的部分）【已证实，来源 ZenML 技术拆解】

这是单独一篇技术文记录的、SmithDB 自研的**嵌入 Vortex 列式格式内的倒排索引**：

- **三元结构**：terms（JSON path 或文本 token）+ postings（排序的 doc-id 集合）+ positions（词位置，用于短语匹配）。
- **三种查询模式**：`json_key`（字段是否存在，支持 LIKE）、`json_key_search`（键值对匹配）、`search`（跨所有 JSON 值全文检索）。
- **字节预算式 row group**：每组 **32MB postings + 64MB term-string**，解决高频词（如 "agent"）撑爆 column-group 的 term 频率倾斜问题。
- **四列**：term 列用 **FST（有限状态转换器）**——可在压缩字节上直接做精确查找/前缀扫描/自动机遍历，实测 **88.8 MiB 原始数据压到 3.8 KiB**；postings/positions 列用 **128 元素块 + 每词变长 bitwidth 的 delta 编码**（高频词低至 3–4 bit/doc）。
- **对象存储 I/O 优化**：借 Vortex 的 I/O 调度器，**把 1MB 内的读合并进 16MB 窗口**；用 zone 级 min/max/count 统计**先剪枝整个 row group 再走 FST**；doc-id 直接是行位置，省掉 segment-local id 翻译表。
- 这套设计达成 **全文检索 + JSON 过滤中位数 400ms**。

> ⚠️ **中文分词【未证实】**：所有一手资料**完全没有提到中文/多语言分词**。SmithDB 的 token 化策略、是否支持 CJK 均无公开信息。**对你们是机会点而非威胁**——国内私有化场景必须中文分词（你们可用 IK / jieba / 自研词典），这是 SmithDB 当前明确的空白。

---

## (4) 公布的性能数据

**【已证实为官方公布值，但无独立第三方复现，且不同来源数字略有出入】**

官方/转载给出的 P50/P99（softwarechains 转述的完整表）：

| 工作负载 | P50 | P99 |
|---|---|---|
| Trace 树加载（trace tree load） | **92ms** | 595ms |
| 单 run 加载（single run load） | 71ms | 358ms |
| Run 过滤（runs filtering） | 82ms | 434ms |
| **全文检索（full-text search）** | **400ms** | 870ms |
| Trace 摄取（trace ingestion） | 630ms | 1.47s |
| Thread 过滤（threads filtering） | 131ms | 268ms |

**整体提速宣称**：⚠️ **数字不一致**——官方博客口径多为 **"up to 12x faster"**，部分转载/搜索摘要写 **"up to 15x"**。两者都见于来源，**以官方博客 12x 为准，15x 视为营销夸大或子场景值**。

**规模宣称**：每天处理"数亿（hundreds of millions）Agent 可观测性事件"。

---

## (5) 工程实质创新 vs 营销话术

### ✅ 判断为真实工程创新（有技术细节支撑）

1. **"run = 事件序列"的存储模型**：把长运行、乱序、可更新的 span 建模成追加事件 + merge-on-read，而非可变行。这是对 trace 数据本质的正确抽象，是实打实的设计决策。
2. **对象存储原生倒排索引**：FST + 字节预算 row group + 块 bitpacking delta + I/O 合并窗口，是非平凡的工程，有完整技术文佐证。**含金量最高**。
3. **渐进式时间窗 + 读 ingestion 本地缓存**：以较低成本同时拿到"最新数据低延迟"和"活 trace 可查"，针对性强。
4. **大字段延迟物化 + 时间分层压实**：直接对应多模态大 payload 与"晚到 end 事件"，是对症设计。
5. **选型组合（Rust + DataFusion + Vortex + 对象存储 + Postgres 元数据）**：把"对象存储原生列存 + 可扩展查询引擎"用在 trace 场景，整合本身有工程价值。

### ⚠️ 判断为营销话术 / 需打折扣

1. **"distributed database（分布式数据库）"措辞**：⚠️ **来源自相矛盾**。媒体快讯（KuCoin/Phemex）和官方推文用 "distributed"；但官方博客描述的是**无状态服务 + 对象存储 + 单个小 Postgres 元数据**——这更像**存算分离的弹性单逻辑库**，而非分片的分布式 OLTP。**"distributed" 带营销色彩**。**对你们而言这点关键：SmithDB 的"分布式"其实是无状态计算层弹性伸缩，核心存储/元数据是中心化的——这意味着你们的"纯单机"约束并不会在架构本质上落后它太多。**
2. **"up to 15x / 12x faster"**：典型 "up to" 话术，取最优子场景，无对照基线说明（vs 旧 ClickHouse？vs 什么硬件？均未公开）。
3. **未点名 ClickHouse**：⚠️ **【部分证实】** LangSmith **历史上确实用 ClickHouse**（有 ClickHouse 官方 2024 年 "Why we choose ClickHouse to power LangSmith" 博文、以及 LangSmith 自托管的 ClickHouse 迁移/备份文档为证）。SmithDB 博客**刻意不提 ClickHouse**，只说"超出了承载它们的数据库"——回避竞品对比是营销策略。**"SmithDB 替代了 ClickHouse" 这一说法：方向上证实（自建替代通用 OLAP），但官方未明确承认，属【部分证实】。**
4. **"数据飞轮（反哺评估/训练）"**：⚠️ **【未证实】** SmithDB 博客原文**没有**"data flywheel"或反哺训练的表述。这是 LangSmith 平台层（evaluation 产品）的叙事，**不是 SmithDB 这个数据库本身的能力**。你给的需求里"数据飞轮"应理解为上层产品功能，DB 层只需提供高效读出 trace 的能力。

### ❓ 明确的信息空白（未证实，给你们留的机会）

- **中文/多语言全文分词**：完全无公开信息（最大空白）。
- **事务/隔离级别**：无说明（trace 场景弱事务，推测为最终一致 + 单 run 内一致）。
- **自托管成熟度**：发布时仍是 "Soon"，私有化体验未知——**这正是你们对标国内私有化销售的切入窗口**。
- **多租户隔离的具体机制**：仅提到查询按 tenant 过滤，无租户级资源/存储隔离细节。

---

## 对贵司新数据库设计的提炼（基于本次调研，供决策参考）

| SmithDB 做法 | 是否适合贵司「纯单机私有化」 | 建议 |
|---|---|---|
| 对象存储 backed LSM | 部分适合 | 单机可退化为**本地盘 LSM**（团队已有"磁盘索引/页面整理框架"，直接复用），无需对象存储复杂度 |
| Vortex 列式格式 | 适合 | 可直接用开源 Vortex（LF 项目、向后兼容），省去自研列存格式 |
| DataFusion 查询引擎 | 高度适合 | 团队有 Rust + 优化器能力，DataFusion 是现成可扩展引擎 |
| run=事件序列 + merge-on-read | 必须借鉴 | 这是 trace 场景的正确抽象，强烈建议采用 |
| 对象存储原生倒排索引 | 单机可简化 | 单机本地盘做倒排更简单；**务必补中文分词（SmithDB 空白）** |
| Postgres 当 metastore | 高度适合 | 团队 PG/openGauss 内核能力可直接复用，甚至单机内嵌 |
| "distributed" 弹性计算层 | 不需要 | 单机约束反而省掉 cluster manager / 粘性路由复杂度，更简洁 |

**一句话结论**：SmithDB 真实存在、有真材实料（尤其是 run 事件模型与对象存储倒排索引），但"分布式"和"15x"有营销成分；其核心组件（Rust+DataFusion+Vortex+Postgres 元数据+LSM merge-on-read）贵司团队几乎全部具备复用能力，且 SmithDB 在**中文分词**和**私有化成熟度**上的空白，正是国内商业产品的切入点。

---

**Sources:**
- [LangChain 官方博客：We built SmithDB, the data layer for agent observability](https://www.langchain.com/blog/introducing-smithdb)
- [ZenML LLMOps：Building Full-Text Search for Agent Traces with Custom Inverted Index on Object Storage](https://www.zenml.io/llmops-database/building-full-text-search-for-agent-traces-with-custom-inverted-index-on-object-storage)
- [softwarechains：SmithDB Explained: AI Agent Observability](https://www.softwarechains.com/insights/smithdb-agent-observability-langsmith)
- [Vortex GitHub (vortex-data/vortex)](https://github.com/vortex-data/vortex)
- [Spiral 博客：Vortex, a Linux Foundation Project](https://spiraldb.com/post/vortex-a-linux-foundation-project)
- [Ankush Gola on X（SmithDB 架构线程，正文需登录）](https://x.com/ankush_gola11/status/2054661816249360553)
- [Ankush Gola on X（DataFusion 自定义执行计划线程）](https://x.com/ankush_gola11/status/2054681251513254260)
- [LangChain on X：SmithDB announced at Interrupt](https://x.com/LangChain/status/2054658661776244936)
- [Phemex News：LangChain Launches SmithDB](https://phemex.com/news/article/langchain-unveils-smithdb-for-enhanced-agent-observability-81135) / [KuCoin 快讯](https://www.kucoin.com/news/flash/langchain-launches-smithdb-a-distributed-database-for-agent-observability)
- [ClickHouse 官方（历史）：Why we Choose ClickHouse to Power LangSmith](https://clickhouse.com/blog/langchain-why-we-choose-clickhouse-to-power-langchain) + [LangSmith ClickHouse 迁移文档](https://support.langchain.com/articles/6463985661-how-do-i-resolve-clickhouse-migration-errors-after-upgrading-langsmith)（证明 LangSmith 历史用 ClickHouse）

---

# 调研：Trace 数据模型与查询需求

I have comprehensive, well-sourced data across all five required areas. Let me synthesize the final research deliverable.

---

# Agent Trace 数据模型与查询需求精确化（设计输入）

> 调研对象：OpenTelemetry GenAI Semantic Conventions（v1.41，2026-05 仍为 Development 状态）、OTel Trace/Span 模型、LangSmith Run/Trace 数据模型、SmithDB（2026-05 发布）、OpenLLMetry/Traceloop、Langfuse 多模态实现。SmithDB 晚于训练截止，以下关于它的所有数字与术语均来自 2026-05 官方博客联网检索，已逐项标注。

---

## 一、Trace / Run / Span 的字段集合与父子树结构

### 1.1 两套主流模型并存，是数据模型设计的第一约束

国内场景接入数据多来自两类 SDK，新库的写入协议必须同时吃下二者：

| 维度 | OpenTelemetry GenAI（OTel-native，含 OpenLLMetry） | LangSmith Run 模型 |
|---|---|---|
| 基本单元 | Span（OTLP 标准） | Run（= span 的别称） |
| 节点类型字段 | `gen_ai.operation.name`（枚举） | `run_type`（枚举） |
| 树关联 | `trace_id` + `span_id` + `parent_span_id` | `trace_id` + `parent_run_id` + `dotted_order` |
| 标识符 | 16-byte trace_id / 8-byte span_id（OTLP 二进制） | UUID（且建议 UUIDv7，时间有序） |
| 内容载体 | span attributes / span events | `inputs` / `outputs`（JSON 对象） |

**设计含义**：内部统一数据模型应以「Run/Span = 一棵树上的一个节点」为核心抽象，同时保留 OTLP 的 trace_id/span_id 二进制语义和 LangSmith 的 dotted_order 排序语义，两套 ID 体系需可互转。

### 1.2 节点类型（span 分类）

**LangSmith `run_type` 全集**（驱动 UI 渲染和类型专属能力）：`llm`、`chain`、`tool`、`retriever`、`prompt`、`embedding`、`parser`。

**OTel GenAI `gen_ai.operation.name` 全集**（v1.41）：`chat`、`text_completion`、`generate_content`（多模态）、`embeddings`、`retrieval`、`execute_tool`、`create_agent`、`invoke_agent`、`invoke_workflow`、`plan`、`create_memory`、`search_memory` 等。

**SmithDB 官方对节点分类的抽象**：thought / tool_call / llm_call 等 span 深度嵌套，「一个现代 agent trace 可以有数百个深度嵌套的 span」（hundreds of deeply nested spans）。

> 设计建议：内部用一张归一化的 `span_kind` 枚举（llm / chat / chain / tool / retriever / embedding / prompt / parser / agent / workflow / memory / thought），并保留原始 `run_type` 与 `operation.name` 原文字段，避免有损映射。

### 1.3 字段集合（合并 OTel + LangSmith，建议作为内部 schema 草案）

**标识与树结构（核心列，必须列存 + 索引）**
- `id` / `span_id`：节点唯一 ID（UUID 或 8-byte）
- `trace_id`：所属 trace（树根 ID）
- `parent_run_id` / `parent_span_id`：直接父节点
- `dotted_order`：`<ts>Z<uuid>.<ts>Z<uuid>...` 形式的可排序路径键 —— 末段 UUID = 本节点 id，首段 UUID = trace id，倒数第二段 = 父节点 id。**这是树遍历高性能的关键：前缀匹配即子树查询，字典序即先序遍历**
- 衍生（LangSmith 还提供）：`parent_run_ids`（全祖先）、`child_run_ids` / `direct_child_run_ids`、`session_id`（项目/会话）

**类型与时间**
- `run_type` / `gen_ai.operation.name`、`name`
- `start_time` / `end_time`、`first_token_time`（流式首 token，TTFT）
- `status`：`success` / `error` / `pending`（`pending` 即"活 trace"未完成态）

**内容（大字段，需 late materialization）**
- `inputs` / `outputs`（LangSmith，JSON）≈ `gen_ai.input.messages` / `gen_ai.output.messages` / `gen_ai.system_instructions`（OTel，结构化消息数组）
- 消息结构：`{ "role": "user|assistant|system", "parts": [{"type":"text|image|audio|...","content":"..."}], "finish_reason": "..." }`
- `error`（错误信息）、`serialized`（执行时对象状态）、`events`（流式状态变更序列）

**LLM/调用元数据（聚合查询的核心维度）**
- 模型：`gen_ai.request.model` / `gen_ai.response.model` / `gen_ai.response.id`、`gen_ai.provider.name`
- 参数：`temperature` / `top_p` / `top_k` / `max_tokens` / `frequency_penalty` / `presence_penalty` / `stop_sequences` / `seed` / `stream`
- 结束原因：`gen_ai.response.finish_reasons`（如 `["stop"]` / `["tool_calls"]`）
- **Token（聚合热点）**：`usage.input_tokens` / `output_tokens` / `total_tokens`，及 `cache_read.input_tokens` / `cache_creation.input_tokens` / `reasoning.output_tokens`（o1/o3 类）
- **成本（聚合热点）**：`total_cost` / `prompt_cost` / `completion_cost`

**工具 / Agent / MCP**
- `gen_ai.tool.name` / `tool.call.id` / `tool.call.arguments` / `tool.call.result` / `tool.type` / `tool.definitions`
- `gen_ai.agent.id` / `agent.name` / `agent.description`
- MCP：`mcp.method.name`（如 `tools/call`）、`mcp.session.id`、`mcp.protocol.version`、`jsonrpc.request.id`、`network.transport`

**会话/线程与过滤维度**
- `gen_ai.conversation.id` ≈ LangSmith thread key（`session_id` / `thread_id` / `conversation_id` 三选一，**必须传播到所有子 run**）
- `tags`（string[]）、`extra` / `metadata`（任意嵌套 JSON）、`feedback_stats`（评分聚合）、`reference_example_id`（评估数据集关联）、`in_dataset`

**Span Kind（OTel）**：`CLIENT`（远程模型调用、agent 创建、MCP client）、`INTERNAL`（本地 agent 推理、tool 执行）、`SERVER`（MCP server）。

---

## 二、大 Payload 与多模态（图像/音频）的引用与存储

业界已收敛到一致模式：**核心行只存指针，大字段外置对象存储，查询时按需取（late materialization）**。

**OTel GenAI 官方建议三档内容捕获模式**：
1. 不记录（默认，隐私优先）
2. 作为 span 属性的序列化 JSON（小内容）
3. **外部存储 + span 内仅存 reference URL**（生产推荐，针对大/敏感内容）—— 「Full content in external storage (S3, GreptimeDB, etc.), span holds only a reference URL」。

**Langfuse 实现（可直接借鉴的工程范式）**：
- SDK 客户端侧检测 `inputs`/`outputs` 中的 base64 data URI，**抽出 → 上传对象存储 → 用引用 token 替换原内容**，使 trace payload 保持精简
- 引用 token 格式：`@@@langfuseMedia:type={MIME_TYPE}|id={MEDIA_ID}|source={base64_data_uri|bytes|file}@@@`
- 上传走 **presigned URL + 内容校验（length / type / SHA256）**，并按 (project, content_type, hash) **去重**
- 支持：图像 PNG/JPG/WebP；音频 MPEG/MP3/WAV；附件 PDF/纯文本
- 私有化要求 S3 兼容对象存储（MinIO 即可）

**LangSmith 实现**：
- 附件结构 `{ "presigned_url": str, "mime_type": str, "reader": BinaryIO }`，UI 单文件上限 **20MB**
- 支持任意二进制（图像/音频/视频/PDF），base64 多模态内容可直接进 evaluator

**SmithDB 实现**：
- 「late materialization of large fields」——「核心行携带指向大字段文件的指针，查询引擎仅在 query 真正投影这些大 payload 时才去取」
- trace 原生包含「多模态内容」含图像和音频

> **设计含义（结合硬约束「不强制对象存储」）**：
> 1. 引用 token 方案与底层存储解耦 —— 后端可挂本地盘 / 自带 MinIO，引用 token 只是逻辑指针。
> 2. 在 ingestion 路径就做 base64 抽离（防止大 payload 污染列存核心行 → 保护写入吞吐和小行扫描性能）。
> 3. 核心行 / 大字段分两套存储布局：核心列（id、树结构、时间、token、cost、tags、metadata）走列存+索引；大字段（inputs/outputs 全文、媒体）走外置文件 + 指针，late materialization。
> 4. SHA256 去重 + presigned URL + MIME 白名单 + 大小上限（参考 20MB/文件）应作为内置能力。

---

## 三、七类核心查询的精确语义（输入 / 输出 / 延迟期望）

下表的 P50/P99 直接来自 **SmithDB 2026-05 官方基准**（对象存储后端、Rust + DataFusion + Vortex），可作为新库单机版的**性能对标基线**。注：SmithDB 是分布式对象存储架构，单机版若用本地盘，I/O 路径更短，P50 有望持平或更优，但需自行验证。

| # | 查询类型 | 精确语义（输入 → 输出） | SmithDB P50 / P99 | 延迟期望（交互级） |
|---|---|---|---|---|
| 1 | **随机访问** | 输入：单个 run_id 或 trace_id。输出：单个 run 完整内容 / 整棵 trace 树（含数百 span）。 | 单 run **71ms / 358ms**；trace 树 **92ms / 595ms** | < 100ms（点查/树加载，UI 打开即见） |
| 2 | **树感知查询（树遍历/过滤）** | 输入：按「根 run / 子 run / 任意节点」+ 条件过滤。输出：匹配的节点或子树。依赖 `dotted_order` 前缀匹配（子树）与字典序（先序）。 | 计入 runs filtering **82ms / 434ms** | < 150ms |
| 3 | **全文检索** | 输入：自然语言短语/模式（agent 的 inputs/outputs 自由文本）。**中文需分词**。输出：命中的 run 列表 + 高亮。SmithDB 用「针对对象存储优化的自定义倒排索引」，按 row group 分组使 exact/prefix term 查询能先剪枝再取 postings。 | **400ms / 870ms** | < 500ms（可接受 sub-second） |
| 4 | **JSON / 元数据过滤** | 输入：对任意用户自定义 metadata / 结构化 tool 输出的嵌套字段条件（如 `metadata.user.tier = 'vip'`）。输出：匹配 run 列表。与 #2/#5 常组合。 | 计入 runs filtering **82ms / 434ms** | < 150ms |
| 5 | **线程重建（跨 trace 拼会话）** | 输入：thread_id / session_id / conversation_id。输出：跨多个 trace、按时序拼接的完整长对话。「rebuild long-running conversations across many agent traces instantly」，100% threads UI 流量已走 SmithDB。 | threads filtering **131ms / 268ms** | < 200ms |
| 6 | **实时聚合** | 输入：过滤条件（metadata/feedback/latency/error/tag/时间窗）+ 聚合维度。输出：cost / latency / token usage / evaluator 分数的聚合值（sum/avg/p50/p99/分组）。 | （博客未单列，归入 filtering + DataFusion 分析能力）| < 1s（dashboard 级） |
| 7 | **活 trace（运行中查询）** | 输入：仍在执行、未收到 end event 的 trace_id。输出：当前已落地的部分子树（可能含 `pending` 节点）。基础是「run 是事件序列、非单条不可变行」（a run is a sequence of events, not a single immutable row），无需等 trace 闭合即可查。 | 复用 #1/#2 路径 | < 100ms（与点查同级） |

**交互过滤（SmithDB 第 2 类官方表述）补充**：按 metadata、feedback、latency、errors、tags、time 切大规模 trace 数据集 —— 这是 #2/#4/#6 在 UI 上的统一入口，是高频交互面，延迟必须稳定在百毫秒级。

---

## 四、写入特征：QPS、Payload 分布、碎片化、乱序、跨时段更新

### 4.1 QPS 量级
- **SmithDB 单 trace 含数百个深度嵌套 span**；写入是「高频碎片化」的小 span 流。
- **Langfuse 参照**：单 ClickHouse shard 可处理「每天 100 万 traces / 200 万 observations」量级；社区有「100K events/sec」的 agent 可观测性实践。
- **设计目标（单机）**：按「每秒数千～数万 span」设计稳态写入吞吐；峰值需抗住单个长 agent 任务在短时间爆发数百 span 的脉冲。
- SmithDB **trace ingestion 端到端 P50 630ms / P99 1.47s**（含落对象存储），可作为写入可见性（write-to-queryable）延迟基线。

### 4.2 Payload 大小分布（强双峰）
- 绝大多数 span 是小 JSON（几百 B ～ 几 KB：thought、tool 参数、token 计数）。
- 少数 span 携带大 payload（LLM 长上下文全文、多模态 base64 可达 MB 级，附件上限参考 20MB）。
- **设计含义**：核心行小而密集 → 适合列存压缩 + 紧凑扫描；大字段必须在 ingestion 抽离外置（见第二节），否则毒化写入与扫描。

### 4.3 碎片化 + 乱序到达
- 每秒大量小 span，**乱序到达**（子 span 可能先于父 span 落库；同一 trace 的 span 分散在不同时间窗）。
- **设计含义**：**不能在 ingest 时做 trace 组装**。应「原始 span 入表，查询时才按 trace_id 拉全树组装」（materialized view 持续维护每个 trace 的 start/end time）。父子关联靠 `dotted_order` / `parent_id` 在查询期解析，容忍父节点暂缺。

### 4.4 Span 跨时段更新（先建后补完，"早上出生下午死亡"）
- OTel/SmithDB 明确：**agent span 的 start event 可能比 end event 早到数分钟乃至数小时**（「a start event ... can arrive minutes, maybe even hours before an end event」）。
- SmithDB 的根本抽象：**「run = 事件序列，而非单条不可变行」**，原生支持「每个 run 多事件」。
- **设计含义**：
  - 存储模型应是 **append-only 事件流 + 查询期折叠（fold）**，而非「先 INSERT 占位、后 UPDATE 改行」的原地更新（原地更新对列存/对象存储是写放大灾难）。
  - 用 **deletion / upgrade vectors**（SmithDB 做法：metastore 给不可变 segment 挂删除/升级向量，查询与 compaction 路径据此解释 immutable 文件）实现逻辑更新/删除，物理在 compaction 时合并。
  - **时间分层 compaction**：新数据更可能再收 end event，过早压成大文件会写放大；老数据稳定且常被反复扫，值得合并成大文件。

> **设计含义（关键架构决策）**：写入层应是 **LSM/事件日志风格的 append-only + 异步 compaction**，而非传统 OLTP 原地更新。这与团队的「磁盘索引与页面整理框架」能力（compaction、页面整理）高度契合。

---

## 五、多租户、数据保留/TTL、冷热分层

### 5.1 多租户
- SmithDB 博客未公开租户隔离细节；硬约束要求「多租户 + 私有化 + 强易用」。
- LangSmith 以 **project / session（`session_id`）** 作为逻辑隔离单元；threads 靠 metadata key 传播分组。
- **设计含义（单机多租户）**：在 `tenant_id`（或 workspace/project）维度做**物理/逻辑分区**（分区裁剪 + 存储隔离 + 配额/限流 + 行级权限）。团队已有「分区」能力可直接复用。单机下首选「按租户分区 + 共享实例」而非每租户独立进程，以最大化单机简洁性。

### 5.2 数据保留 / TTL（非均匀保留是核心需求）
- SmithDB 原话：**「保留很少是均匀的」**——多数 trace 仅近期用于 debug/监控/评估，只有**一小部分需基于内容长期保留**。
- 删除/升级通过 **deletion/upgrade vectors** 异步落地，compaction 时物理回收。
- **设计含义**：
  - TTL 不能只按 trace 的「年龄」一刀切，要支持**按内容/标签/规则的差异化保留**（如 error trace、被人工标注/进数据集的 trace 长期留存；普通 trace 30 天回收）。
  - 删除是高频操作（合规/成本），必须做成**廉价的逻辑标记 + 后台 compaction 物理清除**，不可同步重写。

### 5.3 冷热分层
- SmithDB 用 **time-tiered compaction**：近期数据保持小文件（待补 end event、低写放大）；老数据合并大文件（稳定、利于重复扫描）—— 这天然就是一种时间维度的冷热布局。
- **数据飞轮需求**：trace 还要反哺评估/推理优化/训练 → 冷数据要可被批量导出/扫描（DataFusion 这类向量化引擎对全表扫描友好）。
- **设计含义（单机 + 不强制对象存储）**：
  - 热层：近期数据小文件 + 内存/SSD 缓存 + 倒排索引就绪，服务交互查询（百毫秒级）。
  - 冷层：老数据大文件、强压缩，落本地大容量盘或自带 MinIO，服务聚合扫描与训练数据导出。
  - 缓存命中靠**粘性路由（sticky routing）**提升局部性（SmithDB 借鉴 Google Slicer / Databricks Dicer）。单机可用本地缓存 + 文件亲和调度近似实现。

---

## 六、对新库设计的关键提炼（设计输入结论）

1. **统一数据模型**须同时承接 OTel GenAI（span/attributes）与 LangSmith（run/inputs-outputs/dotted_order）两套协议，保留原始字段、做无损归一。
2. **`dotted_order` 是树查询的灵魂**：可排序路径键让「子树过滤=前缀匹配、先序遍历=字典序」，应作为一等索引列。
3. **写入即事件流，非可变行**：append-only + deletion/upgrade vector + 时间分层 compaction，原生解决乱序、late-arriving、span 跨时段补完、活 trace 查询——这与团队「页面整理/磁盘索引框架」能力对口。
4. **大 payload 强制外置**：ingestion 期抽离 base64/大文本 → 引用 token → 本地盘/MinIO，core 行 late materialization。引用 token 与存储后端解耦以满足私有化。
5. **三套索引并重**：列存（聚合/过滤）、JSON 路径索引（嵌套 metadata 过滤）、面向块存储的自定义倒排索引（全文，**中文分词必备**，是国内场景相对 SmithDB 的差异化点）。
6. **性能对标基线（SmithDB P50）**：单 run 71ms、trace 树 92ms、run 过滤 82ms、threads 131ms、全文 400ms、ingestion 630ms —— 单机版应以「持平或更优」为目标。
7. **多租户/非均匀保留/冷热分层**复用团队「分区」能力，差异化保留规则（按内容/标签）+ 廉价逻辑删除 + 时间分层冷热是商业私有化的硬功能。

---

## 待补充 / 信息缺口（未编造，如实说明）
- **SmithDB 多租户隔离机制**：官方博客未公开，上述为基于 LangSmith project/session 模型的推断。
- **SmithDB 实时聚合的独立延迟数字**：博客未单列，仅可归入 filtering + DataFusion 能力。
- **明确的生产 QPS 上限**：SmithDB 未公布绝对 events/sec；以上量级来自 Langfuse/ClickHouse 公开参照（百万级/天、社区 100K events/sec 实践），需在选型阶段以目标客户实际负载二次校准。
- OTel GenAI 在 2026-06 仍为 **Development（实验）状态**（v1.41），属性名仍可能变动，需用 `OTEL_SEMCONV_STABILITY_OPT_IN=gen_ai_latest_experimental` 管理兼容。

---

**Sources:**
- [Introducing SmithDB — LangChain Blog](https://www.langchain.com/blog/introducing-smithdb)
- [LangSmith Run (span) data format — LangChain Docs](https://docs.langchain.com/langsmith/run-data-format)
- [Configure threads — LangChain Docs](https://docs.langchain.com/langsmith/threads)
- [Run an evaluation with multimodal content — LangSmith Docs](https://docs.smith.langchain.com/evaluation/how_to_guides/evaluate_with_attachments)
- [OpenTelemetry GenAI semantic conventions — gen-ai-spans (moved repo)](https://github.com/open-telemetry/semantic-conventions-genai)
- [Semantic Conventions for GenAI agent and framework spans — OpenTelemetry](https://opentelemetry.io/docs/specs/semconv/gen-ai/gen-ai-agent-spans/)
- [How OpenTelemetry Traces LLM Calls, Agent Reasoning, and MCP Tools — Greptime](https://greptime.com/blogs/2026-05-09-opentelemetry-genai-semantic-conventions)
- [Inside the LLM Call: GenAI Observability with OpenTelemetry — OpenTelemetry Blog](https://opentelemetry.io/blog/2026/genai-observability/)
- [OpenLLMetry / Traceloop — Prompts, Completions and Embeddings](https://www.traceloop.com/docs/openllmetry/privacy/traces)
- [Langfuse Multi-Modality — tracing features](https://langfuse.com/docs/tracing-features/multi-modality)
- [Langfuse and ClickHouse: A new data stack for modern LLM applications — ClickHouse](https://clickhouse.com/blog/langfuse-and-clickhouse-a-new-data-stack-for-modern-llm-applications)
- [How We Built Agent Observability at 100K Events/Sec — DEV](https://dev.to/aishiteru/how-we-built-agent-observability-at-100k-eventssec-pa1)

---

# 调研：开源积木选型

I now have comprehensive coverage of all components plus the SmithDB reference architecture. I have enough to deliver the full comparison and conclusions.

---

# 单机 trace DB 开源积木评估报告

> 任务：为「AI Agent 可观测性 / trace 存储」单机商业数据库选型，对标 LangChain SmithDB。
> 调研日期：2026-06-16。SmithDB（2026-05 发布）等新组件均已联网核实。

## 0. 关键参照系：SmithDB 到底用了什么（已核实）

SmithDB 于 2026-05 在 LangChain Interrupt 大会发布，替代了原来的 ClickHouse。其架构对我们的选型有直接指导意义：

- **技术栈 = Rust + Apache DataFusion（查询引擎）+ Vortex（列式文件格式）+ LSM 架构 + 对象存储**。元数据存在一个**小型 Postgres**（segment metadata），服务层无状态。
- **不是直接用现成 DB**，而是用 DataFusion + Vortex 当积木，自己写了：自定义执行计划、**面向对象存储优化的自研倒排索引**（term-sorted row groups + 分块 postings/positions）、**时间分层 compaction**（写优化段→查询优化段）、**late materialization**（核心字段与大 payload 分离，用指针按需取）、**"run = 事件序列而非单行"** 模型（支持长跑 span 在完成前持续 emit 更新）。
- 性能：trace 树加载 P50 92ms、单 run 71ms、run 过滤 82ms、全文检索 400ms、线程过滤 131ms；摄入 P50 630ms（批量）。核心体验提速最高 12-15x。

**对我们的核心启示**：SmithDB 是分布式 + 对象存储路线（因为要服务全美 Cloud）。**而我们的硬约束是纯单机**——这反而让我们可以做得**更简洁、更快**：单机本地 NVMe 上 mmap + 页面缓存，省掉 SmithDB 为对象存储付出的大量复杂度（cluster manager、sticky routing、Slicer/Dicer 路由）。SmithDB 验证了"DataFusion + Vortex + 自研索引层"这条积木路线是工业级可行的，但我们不必照抄它的分布式包袱。

---

## 1. 逐组件能力对比表

维度评分：●=强/原生　◐=可用但需工程　○=弱/需自建　—=不适用

| 组件 | 核心能力 | 单机/嵌入式 | 树查询 | 全文+中文分词 | JSON 过滤 | 随机访问 | 列式聚合 | 向量检索 | 高频写入 | 成熟度 | 许可证(商用) |
|---|---|---|---|---|---|---|---|---|---|---|---|
| **Apache DataFusion** | Rust 可嵌入 SQL 查询引擎（Arrow 内存格式），可扩展 TableProvider/UDF/优化器规则 | ●库 | ○(递归CTE弱,需自建) | ○(无内置FTS) | ◐(可建JSON UDF) | ◐(靠底层存储) | ●(向量化执行,强) | ○(需自建/扩展) | —(只查询非存储) | ●高,2026默认引擎 | Apache 2.0 ✅ |
| **Vortex** | SOTA 列式文件格式+压缩框架；级联编码,压缩域上算子下推,late materialization | ●库 | — | ○ | ◐(下推过滤) | ●(宣称比Parquet快100x随机访问) | ●(扫描快10-20x) | ○ | ◐(写优化,5x快;追加为主) | ◐中,0.36+格式稳定向后兼容,LF孵化 | Apache 2.0 ✅ |
| **Apache Arrow** | 列式内存格式+IPC,跨语言零拷贝 | ●库 | — | — | ○ | ◐ | ●(计算内核) | — | — | ●很高 | Apache 2.0 ✅ |
| **Parquet** | 通用列式磁盘格式 | ●库 | — | ○ | ○ | ○(行组级,随机访问差) | ●(扫描/聚合好) | — | ○(只追加,不可改) | ●很高 | Apache 2.0 ✅ |
| **Lance / LanceDB** | AI-native 多模态 lakehouse 格式;文件+表+catalog 三合一 | ●嵌入式 | ○ | ●(内置倒排,BM25,**内置jieba/lindera**) | ◐(标量索引) | ●(分块+双结构编码,随机访问极强) | ◐(可,但偏点查) | ●(billion级毫秒,SOTA) | ◐(支持改/加列只写新文件;碎片化小写需compaction) | ◐中,较新但活跃 | Apache 2.0 ✅ |
| **DuckDB** | 进程内 SQL OLAP,扩展生态(FTS/JSON/spatial/vss) | ●嵌入式 | ◐(递归CTE) | ◐(FTS+BM25,但**无CJK分词,需外部预分词**) | ●(JSON扩展强) | ○(分析型,点查弱) | ●(分析聚合极强) | ◐(vss扩展HNSW) | ○(批量好,单行高频差) | ●很高 | MIT ✅ |
| **chDB(嵌入式ClickHouse)** | 进程内 ClickHouse OLAP 引擎,70+格式 | ●嵌入式 | ○ | ◐(tokenbf/ngram,中文需ngram无真分词) | ●(JSON类型+函数强) | ○(MergeTree点查弱) | ●(OLAP聚合天花板) | ◐(近期加向量) | ◐(批量极强,**小批高频insert是已知痛点**) | ●高(ClickHouse内核) | Apache 2.0 ✅ |
| **tantivy** | Rust 全文检索库(类Lucene),倒排/BM25 | ●库 | — | ●(**cang-jie/tantivy-jieba/lindera 成熟中文分词**) | ○ | ◐(doc id 取文档) | ○ | ◐(近期加ANN) | ●(近实时索引,segment合并) | ●高,Quickwit背书 | MIT ✅ |
| **RocksDB / LSM** | 嵌入式持久化 KV,LSM,写优化 | ●库 | ○(需自建key编码) | — | — | ●(KV点查极强) | ○ | — | ●(写吞吐天花板,WAL+memtable) | ●很高(Meta) | Apache 2.0 / GPLv2 ✅ |
| **object_store crate** | S3/GCS/Azure/本地文件 统一抽象 | ●库 | — | — | — | ◐(stateless get/range) | — | — | — | ●高(Arrow项目) | Apache 2.0 / MIT ✅ |
| **SQLite** | 嵌入式行存 SQL,事务 | ●嵌入式 | ◐(递归CTE) | ◐(FTS5可挂自定义tokenizer,可接中文) | ◐(JSON1) | ●(行存点查强) | ○(行存,大聚合弱) | ○(扩展) | ◐(单写者,WAL) | ●极高 | Public Domain ✅ |

---

## 2. 逐组件作为底座的优劣（精炼）

**Apache DataFusion** — *最合适的"主板"*
优：纯 Rust 可嵌入,向量化执行,TableProvider/UDF/优化器全可扩展;团队 Rust + 查询优化器能力直接复用;SmithDB 已用它生产验证。
劣：本身不带存储、不带 FTS、不带 JSON 路径过滤,递归/树查询要自己写优化器规则。它是"引擎"不是"数据库",这正是我们要补的工程量。

**Vortex** — *最合适的"存储格式",但仍年轻*
优：随机访问号称比 Parquet 快 100x(对单 run/单 span 随机取至关重要)、扫描快 10-20x、压缩域算子下推 + late materialization(正好匹配 trace 大 payload 与指针分离)。格式自 0.36 起向后兼容,DuckDB 已出官方扩展。
劣：进 Linux Foundation 孵化不久(2025-08),生态/文档比 Parquet 薄;写仍以不可变追加为主,乱序/长跑更新需上层 compaction 配合。**最大风险=押注一个 1 年内的新格式**。

**Lance / LanceDB** — *单机最接近"开箱即用"的多模态底座*
优：唯一一个**同时**内置高性能随机访问 + 向量检索 + 全文倒排(且**原生 jieba/lindera 中文分词**)+ 多模态大 payload + 列式扫描的嵌入式格式;支持加列/回填只写新文件(契合数据飞轮反哺 embedding/特征)。Apache 2.0。
劣：树查询、JSON 任意嵌套过滤、实时聚合不是它强项;碎片化高频小写需要 compaction;FTS 纯文本场景比专用引擎略慢;中文需自行下载语言模型(LANCE_LANGUAGE_MODEL_HOME)。

**DuckDB** — *最快出 MVP,但中文 FTS 是硬伤*
优：MIT、嵌入式、JSON/分析聚合极强、扩展生态丰富、上手最快。
劣：**FTS 扩展无 tokenizer hook,不支持 CJK 分词**(必须外部预分词再建索引,工程绕路);点查/单行高频写弱;不是为"活 trace 随机访问 + 乱序更新"设计。适合做分析侧,不适合做主存。

**chDB(嵌入式 ClickHouse)** — *聚合天花板,但写模型与场景冲突*
优：Apache 2.0、OLAP 聚合(cost/latency/token)性能天花板、JSON 类型强、嵌入式。
劣：**MergeTree 小批高频 insert 是公认痛点**(trace 正是每秒大量乱序小 span);点查弱;中文只有 ngram/tokenbf 近似无真分词;树查询/活 trace 更新不自然。可作分析加速层,不宜作主存。

**tantivy** — *中文全文检索的最佳专用件*
优：MIT、Lucene 级倒排、近实时、**cang-jie / tantivy-jieba 中文分词成熟**、Quickwit 生产背书。正好补 DataFusion/Vortex/Lance 在"中文短语检索"上的弱项。
劣：只是检索库,不是 DB;需自己把 doc_id 映射回 trace 存储。

**RocksDB / LSM** — *高频写入与随机访问的地基*
优：Apache 2.0、写吞吐天花板、KV 点查极强,天然适配"每秒大量小 span 乱序到达 + 长跑后更新"。可作热层/主键索引/活 trace 缓冲。
劣：无 SQL、无列式聚合、树查询要自己设计 key 编码(如 `trace_id|span_path`)。需大量上层工程。

**object_store crate** — *私有化部署存储抽象,直接用*
Apache/MIT,一套代码同时支持本地盘 / MinIO / S3。私有化场景默认本地盘,客户自带 MinIO 时零改动。**强烈建议采用,无争议。**

**SQLite** — *元数据/索引/单机事务的可靠选择*
Public Domain、极成熟、FTS5 可挂自定义中文 tokenizer。可替代 SmithDB 里那个"小 Postgres metastore"角色(单机更轻)。不适合做大 payload 列存主体。

---

## 3. 候选组合方案对比（工程量 / 风险）

| 方案 | 组成 | 树查询 | 中文FTS | 高频乱序写 | 随机访问 | 聚合 | 向量 | 工程量 | 风险 | 适配度 |
|---|---|---|---|---|---|---|---|---|---|---|
| **A. SmithDB 同构(推荐)** DataFusion+Vortex+自研索引层 | DataFusion引擎 + Vortex列存 + RocksDB热层 + tantivy(中文FTS) + SQLite元数据 + object_store | 自研优化器规则 ● | tantivy+cang-jie ● | RocksDB热层+Vortex冷段 ● | Vortex+RocksDB ● | DataFusion向量化 ● | DataFusion UDF/Lance索引 ◐ | **大(6-9人月起)** | 中(Vortex较新;但路线已被SmithDB证明) | ★★★★★ |
| **B. Lance 为主** LanceDB格式 + DataFusion查询 | Lance(随机访问/向量/中文FTS内置) + DataFusion(SQL/聚合) + SQLite元数据 | 中(Lance不擅树查,DataFusion补) ◐ | Lance内置jieba ● | Lance compaction ◐ | Lance ● | DataFusion ● | Lance ● | **中(3-5人月)** | 中(Lance较新;树/聚合需补) | ★★★★ |
| **C. DuckDB + 扩展** | DuckDB + JSON/vss扩展 + 外部中文分词预处理 + 自建树编码 | 递归CTE ◐ | **绕路:外部预分词** ○ | 弱(批量化缓冲) ○ | 点查弱 ○ | ● | vss ◐ | 小(1-2人月MVP) | 高(中文FTS与高频写两大硬伤,难产品化) | ★★ |
| **D. chDB / ClickHouse 系** | chDB嵌入 + ngram索引 + 物化视图聚合 | 弱 ○ | ngram近似 ◐ | **小批insert痛点** ◐ | 弱 ○ | ●● | ◐ | 小-中 | 高(写模型与trace碎片化写冲突,等于重蹈LangSmith弃ClickHouse的覆辙) | ★★ |
| **E. 纯自研 Rust** | 全自研存储+索引+查询 | 完全可控 ● | 自研/集成tantivy ● | 完全可控 ● | ● | 需自研 ◐ | 需自研 ◐ | **极大(12+人月)** | 高(造轮子,周期长) | ★★★(长期) |
| **F. PG/openGauss 内核扩展** 复用yiTrace能力 | PG内核 + 自研trace AM/索引 + pgvector + zhparser中文分词 | ltree/递归 ◐ | zhparser ◐ | 行存高频写中等 ◐ | 行存点查 ● | 行存聚合弱 ○ | pgvector ◐ | 中(团队最熟) | 中-低(团队能力最匹配,但行存做不出列式聚合天花板,且偏离单机极致简洁) | ★★★ |

---

## 4. 重点结论与建议

**首选：方案 A（SmithDB 同构，单机精简版）= DataFusion + Vortex + RocksDB 热层 + tantivy(cang-jie 中文分词) + SQLite 元数据 + object_store。**

理由：
1. **被验证的路线**：SmithDB 已用 Rust+DataFusion+Vortex 在生产服务全美流量,证明这套积木能扛 trace 场景的全部硬需求(树查询、活 trace、长跑 span、late materialization、自研倒排)。我们抄架构思想、但**砍掉它的分布式/对象存储包袱**——单机本地 NVMe 让随机访问和延迟更优、复杂度更低。
2. **能力完美复用**:DataFusion(查询优化器)、Vortex(磁盘列存/页面整理)、Rust、向量索引——全是团队现有储备,**几乎零知识断层**。
3. **逐维度覆盖**:高频乱序小 span → RocksDB 热层(LSM 写天花板)缓冲 + 时间分层 compaction 落 Vortex 冷段;随机访问单 run/trace 树 → Vortex 100x + RocksDB 点查;中文全文检索 → tantivy+cang-jie(这是 DataFusion/DuckDB/chDB 都欠缺、而 tantivy 最成熟的一环);JSON 嵌套过滤 → DataFusion JSON UDF + 下推;实时聚合 → DataFusion 向量化;活 trace/长跑更新 → "run=事件序列"模型 + LSM 覆盖写;多模态大 payload → late materialization 指针 + object_store(本地盘/MinIO)。

**次选：方案 B（Lance 为主）做 MVP 提速**。Lance 是唯一一个把随机访问+向量+**内置中文 jieba 全文**+多模态打包好的嵌入式格式,工程量比 A 小一半,适合先快速验证产品形态;弱点(树查询、实时聚合)用 DataFusion 兜。**可作为 A 的过渡或并行验证路径**——甚至可在 A 里直接用 Lance 当向量/多模态子系统。

**明确不推荐做主存的**:
- **C(DuckDB)** 与 **D(chDB/ClickHouse)**:两者聚合都强,但 **DuckDB 无 CJK 分词、chDB 小批高频 insert 是结构性痛点**——后者恰恰是 LangSmith 当初弃 ClickHouse 的核心原因之一,直接对标场景却选会被淘汰的引擎,逻辑不成立。二者可作**离线分析/导出加速旁路**,不作主存。
- **E(纯自研)**:12+ 人月、风险最高,除非 A 中某积木出现不可接受的阻塞,否则不必从零造。
- **F(PG 内核扩展)**:团队最熟、风险最低,但行存做不出 trace 聚合(cost/latency/token)的列式天花板,且与"单机极致简洁、对标列式 SmithDB"的定位冲突;建议仅作为**兜底方案**或借用其中文分词(zhparser)/向量(pgvector)经验。

**无争议立即采用的辅件**:object_store(私有化本地盘/MinIO 一码通)、SQLite 或小型嵌入式 PG(承担 SmithDB 里"metastore"角色)、tantivy+cang-jie(中文全文检索)。

**主要风险点**:Vortex 仍是 ~1 年新格式(2025-08 进 LF),需在 PoC 阶段验证其写入/compaction 在高频乱序 span 下的稳定性;若不达预期,**Lance 是现成的格式层降级方案**(B 路线),二者均 Apache 2.0、均与 DataFusion 集成,可平滑切换。

**许可证总览(全部商用友好)**:DataFusion / Vortex / Arrow / Parquet / Lance / chDB / RocksDB / object_store = Apache 2.0;DuckDB / tantivy = MIT;SQLite = Public Domain。**无 GPL 传染、无 SSPL/BSL 限制,均可闭源商业私有化交付。**

Sources:
- [LangChain blog: We built SmithDB](https://www.langchain.com/blog/introducing-smithdb)
- [KuCoin: LangChain Launches SmithDB](https://www.kucoin.com/news/flash/langchain-launches-smithdb-a-distributed-database-for-agent-observability)
- [Vortex GitHub (vortex-data)](https://github.com/vortex-data/vortex) / [Announcing Vortex Support in DuckDB](https://duckdb.org/2026/01/23/duckdb-vortex-extension)
- [Lance random access blog](https://www.lancedb.com/blog/benchmarking-random-access-in-lance) / [Test-Driving Lance in DuckDB](https://duckdb.org/2026/05/21/test-driving-lance) / [Lance Tokenizer (jieba/lindera)](https://lance.org/guide/tokenizer/)
- [chDB GitHub](https://github.com/chdb-io/chdb) / [chDB docs](https://clickhouse.com/docs/chdb) / [ClickHouse is Apache 2.0 (Altinity)](https://altinity.com/blog/clickhouse-is-apache-2-0)
- [Apache DataFusion (docs.rs)](https://docs.rs/datafusion/latest/datafusion/) / [DataFusion GitHub](https://github.com/apache/datafusion)
- [tantivy GitHub](https://github.com/quickwit-oss/tantivy) / [cang-jie 中文分词](https://github.com/DCjanus/cang-jie)
- [DuckDB FTS extension](https://duckdb.org/docs/current/core_extensions/full_text_search) / [DuckDB FTS 无 CJK tokenizer 限制](https://github.com/duckdb/duckdb-fts)
- [RocksDB GitHub](https://github.com/facebook/rocksdb)
- [object_store crate](https://crates.io/crates/object_store) / [InfluxData 捐赠给 Arrow](https://www.influxdata.com/blog/rust-object-store-donation/)

---

# 调研：竞品与市场格局

I now have comprehensive material across all competitors and the market-gap dimensions. Let me synthesize the final report.

# AI Agent 可观测性 / Trace 存储数据库 — 竞品与市场调研报告

> 调研时间：2026-06；面向「纯单机 + 私有化 + 商业化 + 中文场景」的新数据库定位。所有结论基于联网检索，关键事实附来源；查不到的明确标注。

---

## 一、核心趋势判断（先给结论）

1. **行业已从"通用 OLAP 复用"走向"为 Agent trace 自研专用存储引擎"。** 2026 年三大头部玩家不约而同自研了专用数据库：LangChain 的 **SmithDB**（2026-05 发布，替代 ClickHouse）、Braintrust 的 **Brainstore**、二者架构高度趋同——**对象存储原生 + LSM/段式写入 + 自研倒排索引 + Rust + 小 Postgres 存元数据**。这说明"通用数据库（ClickHouse/PG/ES）做 Agent trace 有结构性不匹配"已是行业共识。

2. **结构性不匹配点（这正是机会所在）：**
   - trace 是**不断生长的决策树**，不是一行不可变日志；run 需要被建模成"事件序列"而非单行（长运行 span "早上出生、下午死亡"）。
   - 需要**树感知查询**（按根/子/任意节点过滤）、**线程重建**（跨多 trace 瞬间拼长对话）、**JSON 任意嵌套字段过滤**、**全文检索**、**多模态大 payload 延迟物化**——这些通用引擎都不原生擅长。
   - 高频碎片化写入 + 乱序到达 + 活 trace（运行中即查）。

3. **国内市场处于早期。** 国内目前是**大厂的"可观测性平台顺手加 AI 模块"**（阿里云 AgentLoop、火山引擎 APMPlus、快猫 Flashcat FlashAI），**没有一个"Agent 可观测性专用数据库"产品**，更没有"纯单机私有化 + 中文优化"的商业化 DB。**这就是空白。**

---

## 二、竞品逐一拆解

### A. 国际头部（自研专用存储派）

#### 1. LangSmith / SmithDB（对标基准）
- **底层存储**：2026-05 发布 SmithDB，**替代了 ClickHouse**。架构 = 对象存储支撑的 **LSM 树**（内存缓冲→刷成不可变有序段→后台 compaction）+ **Apache DataFusion**（深度定制查询引擎）+ **Vortex**（列式文件工具）+ **小 Postgres 存段元数据** + 无状态的 ingestion/query/compaction 服务。用 Rust 写。
- **关键设计**：run = 事件序列（支持长运行 span 持续更新）；树感知查询；线程重建；JSON key-path 过滤（payload >1MB）；自研对象存储优化的倒排索引（row-group min/max 剪枝）；**延迟物化**（核心行存指针，只在投影时才取大 payload）；**新鲜数据从 ingestion 节点本地 SSD/内存缓存直读**（活 trace）；删改用 deletion/upgrade vector 不重写文件。
- **性能（P50）**：trace 树加载 92ms，单 run 71ms，run 过滤 82ms，全文检索 400ms，线程过滤 131ms，写入 630ms。整体比旧版快 12-15x。
- **部署**：SaaS 为主；企业版支持自托管（K8s + ClickHouse 历史栈复杂）。
- **定价**：Plus $39/席/月；trace 超量 $2.50/1K（14天）或 $5/1K（400天）；**Enterprise 自托管 ~$100K+/年起、不公开报价**。
- **目标客户**：LangChain/LangGraph 生态的全球开发者与企业。
- **强**：生态绑定、架构最先进、性能标杆。**弱**：服务端闭源、企业自托管贵且重、**无中文优化、国内无合规落地**。

#### 2. Braintrust / Brainstore
- **底层存储**：**Brainstore**——对象存储原生（S3/GCS/Azure），WAL append 到对象存储；三类节点（Writer/Reader/Fast-Reader）；段按时间排序（同一 trace 落一起）；异步 compaction 生成**倒排索引+行存+列存+bloom filter**；用 **Tantivy**（开源倒排+列存库）。**单个 Rust 二进制**，只需指向 S3（数据）+ Postgres（元数据）+ Redis（分布式锁）。
- **性能**：trace 查询秒级跨百万 span，号称比传统库快 ~80x；热搜 <50ms、冷搜 <500ms。
- **定价**：免费版 1M span；付费 $249/月起；典型 Pro ~$339/月；Enterprise 定制。
- **目标客户**：把 LLM 产品推向生产的工程团队，**eval + 可观测一体**（强在评估闭环）。
- **强**：架构干净（单二进制易部署）、eval 能力强。**弱**：仍依赖对象存储+Redis、闭源、无中文、定位偏 eval 而非纯可观测。

> **战略含义**：SmithDB 和 Brainstore 验证了"专用引擎"路线正确，但两者都**对象存储原生 / 多服务 / 为云水平扩展设计**。这恰恰给"**纯单机、本地盘、单进程极简私有化**"留出了清晰差异化——它们的架构在"一台机器、离线、本地 NVMe"场景下是**过度设计**。

### B. 国际开源/SaaS（通用存储复用派）

#### 3. Langfuse（最重要的开源对手）
- **底层存储**：v3 起核心迁到 **ClickHouse**（trace/observation/score）+ **Postgres**（元数据）+ **S3/Blob**（大 payload）+ **Redis/Valkey**（队列缓存）。**2026-01-16 被 ClickHouse 收购**（Series D $400M），仍 MIT 开源。
- **部署**：SaaS + 自托管（MIT，无 license key，仅付基础设施费）。**但自托管是多组件重型栈**（PG + ClickHouse 集群 + S3 + Redis + 应用服务，4-6 个 deployment）。
- **定价**：云 Pro $199-300/月；**自托管中等规模实际 $3,000-4,000/月**（基础设施 + DevOps + 可选企业 license）。
- **目标客户**：要数据自主、开源可控的团队。**国内很多团队默认拿它当 LangSmith 替代。**
- **强**：开源免费、生态成熟、OTel 原生。**弱（=我们的机会）**：**自托管运维极重**（ClickHouse 多十亿行 schema 变更要专职 DBA）、**中文分词弱**、非单机、被 ClickHouse 收购后路线不确定。

#### 4. Arize Phoenix
- **底层存储**：**单容器 + Postgres**（生产推荐）；SQLite 仅本地试用（重启丢数据）；基于 OpenTelemetry/OpenInference。
- **部署**：Docker/K8s，可纯本地/单机。**是当前最轻量的自托管选项**（2-3 个服务）。
- **定价**：Phoenix 开源永久免费；商业云 Arize AX：Free / Pro $50/月 / Enterprise 定制。
- **目标客户**：要免费、自托管、PII 不出域的 AI 工程师。
- **强**：轻量、免费、OTel 生态、eval 强。**弱**：**Postgres 单库扛不住高频写**（1-5M span/天上限），无专用 trace 引擎、无中文优化、大规模性能受限。

#### 5. Helicone
- **底层存储**：Cloudflare Workers + **ClickHouse** + Kafka，网关代理优先架构（gateway-first，drop-in proxy）。处理过 20 亿+ 交互，加 50-80ms 延迟。
- **部署**：SaaS + 自托管（Docker/K8s）。**2026-03 被 Mintlify 收购，已进入维护模式**（重要：方向不明）。
- **强**：代理式接入零改码、成本/缓存能力。**弱**：维护模式、ClickHouse 栈、非树感知（偏请求级而非 Agent 树）、无中文。

#### 6. OpenLLMetry / Traceloop
- **本质**：**不是数据库**，是基于 OpenTelemetry 的 SDK/instrumentation（Apache-2.0），把 LLM trace 吐成标准 OTel，接 Datadog/Grafana 等后端。
- **部署**：Traceloop 平台支持本地/私有/**气隙环境**部署。定价：免费 50K span/月，付费 $59/月起。
- **角色**：**它是采集层标准，不是存储层竞品**——反而是我们应该**兼容的接入协议**（支持 OTLP/OpenInference 是刚需）。

### C. 通用分布式追踪（被借鉴对象，非直接对手）

#### 7. Grafana Tempo / Jaeger
- **Tempo**：**仅用对象存储**（S3/GCS/MinIO）做持久层，不建全量索引，按 trace ID 查；ingester 把 span 排进 **Apache Parquet** 列式 schema 写对象存储。**便宜、能存全量**。
- **Jaeger**：传统追踪，索引每条 trace，**贵且难扩展**。
- **与 Agent 场景差距**：为**微服务 APM**设计——**无 LLM 语义**（无 token/cost/prompt 概念）、**无树感知业务查询、无全文检索/中文分词、无 JSON 任意字段过滤、无 eval/数据飞轮**。Tempo 的"对象存储 + Parquet 列存"思路值得借鉴，但**不能直接用于 Agent 可观测性产品**。

### D. 国内玩家（全部是"平台加 AI 模块"，无专用 DB）

#### 8. 阿里云 AgentLoop
- **定位**：企业级智能体一站式"自进化平台"——Agent 全栈观测审计 + 评估实验 + 资产管理 + 持续优化。
- **存储**：内置 Pipeline 引擎把海量 trace 转成"数据资产"（Golden/BadCase Dataset）；"数据集"是为 AI 设计的新型存储，支持 CRUD + 灵活 Schema + **向量检索 + 多维分析**。底层具体引擎未公开披露（推测复用阿里云日志/存储栈）。
- **部署**：**云服务为主**（部分功能 2026-06 才上线）。**私有化/单机能力未见公开。**
- **强**：阿里云生态、数据飞轮（trace→训练）理念到位。**弱**：**绑云、私有化不明、非独立可售 DB、重平台**。

#### 9. 火山引擎 APMPlus（字节）
- **定位**：APM 全链路版顺手加"AI 场景监控"——大模型会话分析、监控看板、MaaS 链路监控。字节内部把可观测闭环整进 AI 工作平台/飞书 AaaS。
- **存储**：复用其 APM 后端，**非 Agent trace 专用引擎**。
- **强**：字节内部实战、大盘能力。**弱**：**绑火山云、非专用 trace DB、私有化/单机不明、非独立产品**。

#### 10. MyScale Telemetry
- **本质**：开源工具，把 **LangChain trace 存进 MyScaleDB 或 ClickHouse**。是 Langfuse 式方案的"ClickHouse + 向量"变体。
- **强**：向量检索 + ClickHouse。**弱**：**仅采集/适配层、依赖 ClickHouse 系、非独立专用产品、MyScale 自身重心在向量 DB**。

#### 11. 快猫星云 Flashcat / FlashAI
- **定位**：开源夜莺（Nightingale）内核的一站式智能观测平台（指标/日志/链路统一）；**FlashAI** 是其 AI 运维 Agent（RAG + 知识库做根因分析）。给出了 Agent 监控五大指标（成功率/p95 延迟/Token/错误率/业务结果）。
- **存储**：传统可观测栈（时序/日志/链路），**非 Agent trace 专用引擎**。
- **强**：**国内私有化交付经验丰富、信创友好、有客户基础**。**弱**：**FlashAI 是"用 AI 做运维"，不是"为 Agent 做可观测 DB"——方向不同**；底层无 trace 树/活 trace/线程重建等专用能力。**它更像潜在的渠道/集成伙伴或正面竞品的渠道威胁，而非同类产品。**

---

## 三、市场空白与差异化机会（核心回答）

### 空白地图

| 维度 | 国际专用 DB (SmithDB/Brainstore) | 国际开源 (Langfuse/Phoenix) | 国内大厂 (AgentLoop/APMPlus) | **我们的空位** |
|---|---|---|---|---|
| Agent 专用引擎 | ✅ 最强 | ❌ 复用 ClickHouse/PG | ❌ 复用 APM 栈 | ✅ 专用 |
| 纯单机 | ❌ 对象存储/多服务 | ❌ 多组件重型 | ❌ 绑云 | ✅✅ **唯一** |
| 私有化/离线 | △ 贵且重 | △ 重运维 | ❌/不明 | ✅✅ 极简 |
| 中文场景 | ❌ | ❌ | △ | ✅✅ **唯一原生** |
| 商业化可售 | ✅(贵/合规难) | 开源(运维贵) | 平台模块 | ✅ 独立产品 |

### 五个差异化机会（按优先级）

1. **"单进程、本地盘、装上就能用"的极简私有化** —— 这是最锋利的楔子。
   竞品的痛：SmithDB/Brainstore 对象存储原生、Langfuse 自托管要 4-6 个组件 + ClickHouse DBA + $3-4K/月。**国内私有化/信创/离线机房场景，"一个二进制 + 本地 NVMe + 零外部依赖"是降维打击。** 团队的 PG/openGauss 内核 + 磁盘索引 + 页面整理框架能力，正好做"单机把对象存储该有的列存/倒排/段式 compaction 在本地盘上做到极致"。

2. **原生中文全文检索（竞品全军覆没的点）** —— 唯一原生中文优化。
   所有国际产品在"中文自然语言 input/output 里搜短语"上**没有原生分词**（ClickHouse/Tantivy 默认英文 tokenizer，中文要外挂 IK/HanLP/jieba 且效果差）。**内置中文分词（IK/HanLP 级）+ 倒排索引**是国内场景刚需，且是国际玩家短期补不上的本地化壁垒。

3. **真正的 trace 树/活 trace/线程重建一等公民** —— 对齐 SmithDB 能力但做进单机。
   run = 事件序列、长运行 span 持续更新、树感知查询、跨 trace 线程重建、活 trace 直读——这些是"Agent 专用"的硬门槛。通用 ClickHouse/PG 做不好，这是和 Langfuse/Phoenix/国内大厂拉开差距的关键。

4. **复用 yiTrace 向量能力做"数据飞轮"** —— 国际开源对手没有的整合。
   trace 反哺 eval/训练/检索：原生把"标量 + 向量 + 全文 + JSON"统一在一个单机引擎里（团队的 IVF/HNSW/DiskANN 储备直接复用），做"trace → Golden/BadCase Dataset → 向量检索/相似 case 召回"。**AgentLoop 有这个理念但绑云；我们能做成单机可售。**

5. **OTel/OpenInference 兼容接入 + 信创合规** —— 降低迁移成本、过采购门槛。
   兼容 OTLP/OpenInference（吃掉 OpenLLMetry/Traceloop 的采集生态），让客户"换存储不换 SDK"；同时做信创操作系统/CPU 适配——这是国内私有化采购的硬门槛，国际玩家拿不到。

### 国内企业私有化采购 Agent 可观测性的真实痛点

1. **数据不能出域 / SaaS 不可用** —— prompt、completion、PII、业务数据高度敏感，金融/政企/电信合规要求数据自主可控，LangSmith/Braintrust 这类 SaaS 直接出局。
2. **现有自托管方案运维太重、太贵** —— Langfuse 式栈要 ClickHouse 集群 + PG + Redis + S3 + DBA，**$3-4K/月 + 一个工程师 10-20% 时间**；ClickHouse 多十亿行 schema 变更需专职 DBA。私有化客户机房没有这种运维能力。
3. **离线/气隙环境部署难** —— 多组件、依赖对象存储、依赖云，离线机房落地成本高。**"一个包、本地盘、离线可装"是刚需。**
4. **中文检索体验差** —— 国际工具中文分词缺失，"在中文对话里搜短语"基本不可用。
5. **信创/国产化适配** —— 需要适配国产 OS/CPU/数据库栈，国际产品无法满足；这同时是**门槛也是护城河**。
6. **缺独立产品、只能"平台绑定"** —— 国内现状是买阿里云/火山就送 AI 监控模块，**没有"可独立采购、私有化、专注 Agent 可观测的数据库产品"**——这正是要填的空白。

### 一句话定位建议

> **"国内首个 Agent 可观测性专用数据库：单进程私有化、本地盘零外部依赖、原生中文检索、trace 树/活 trace 一等公民、向量飞轮内建"** —— 在 SmithDB/Brainstore 验证的"专用引擎"正确路线上，用**单机极简 + 中文 + 信创**三把刀，吃下国际玩家因架构（对象存储原生/重运维）和本地化（无中文/无信创）够不着、国内大厂因"绑云/非专用 DB"够不着的中间地带。

---

## 主要来源

- [LangChain: Introducing SmithDB](https://www.langchain.com/blog/introducing-smithdb)
- [SmithDB Explained (Softwarechains)](https://www.softwarechains.com/insights/smithdb-agent-observability-langsmith)
- [Braintrust: Brainstore architecture](https://www.braintrust.dev/blog/brainstore-architecture) / [Brainstore blog](https://www.braintrust.dev/blog/brainstore) / [Braintrust pricing 2026 (Cekura)](https://www.cekura.ai/blogs/braintrust-pricing)
- [Langfuse + ClickHouse data stack](https://clickhouse.com/blog/langfuse-and-clickhouse-a-new-data-stack-for-modern-llm-applications) / [ClickHouse acquires Langfuse](https://clickhouse.com/blog/clickhouse-acquires-langfuse-open-source-llm-observability) / [Langfuse pricing 2026 (Coverge)](https://coverge.ai/blog/langfuse-pricing)
- [Self-Host LLMOps 2026: Postgres/ClickHouse tradeoffs (FutureAGI)](https://futureagi.com/blog/llm-observability-self-hosting-guide-2026) / [Best Self-Hosted LLM Observability 2026](https://futureagi.com/blog/best-self-hosted-llm-observability-2026/)
- [Arize Phoenix GitHub](https://github.com/arize-ai/phoenix) / [Phoenix pricing](https://phoenix.arize.com/pricing/) / [Phoenix pricing (Costbench)](https://costbench.com/software/ai-observability/arize-phoenix/)
- [Helicone observability guide (Spheron)](https://www.spheron.network/blog/llm-observability-gpu-cloud-langfuse-arize-phoenix-helicone/) / [Helicone review (ChatForest)](https://chatforest.com/reviews/helicone-llm-observability-gateway/)
- [LangSmith pricing 2026 (CheckThat)](https://checkthat.ai/brands/langsmith/pricing) / [LangSmith pricing (Coverge)](https://coverge.ai/blog/langsmith-pricing)
- [OpenLLMetry GitHub](https://github.com/traceloop/openllmetry) / [Traceloop self-host docs](https://www.traceloop.com/docs/self-host/introduction) / [Traceloop pricing](https://www.traceloop.com/pricing)
- [Grafana Tempo architecture](https://grafana.com/docs/tempo/latest/introduction/architecture/) / [Tempo object storage](https://grafana.com/docs/tempo/latest/reference-tempo-architecture/object-storage/) / [Tempo vs Jaeger (Last9)](https://last9.io/blog/grafana-tempo-vs-jaeger/)
- [阿里云 什么是 AgentLoop](https://help.aliyun.com/zh/document_detail/3033860.html) / [阿里云可观测 2026-03 动态](https://www.cnblogs.com/alisystemsoftware/p/19836552)
- [火山引擎 Agent 可观测性：从 Trace 到回放](https://developer.volcengine.com/articles/7647092173612433444) / [火山引擎 APMPlus](https://www.volcengine.com/docs/6431/69088?lang=zh)
- [MyScale Telemetry GitHub](https://github.com/myscale/myscale-telemetry)
- [快猫 Flashcat: AI Agent 监控 2025](https://flashcat.cloud/blog/ai-agent-monitoring-2025/) / [Flashcat AI-Ready](https://flashcat.cloud/blog/flashcat-ai-ready/)
- [滴滴基于 ClickHouse 构建日志存储（中文分词/倒排瓶颈）](https://zhuanlan.zhihu.com/p/685452939) / [信创 CDC 实战 (Tapdata)](https://tapdata.net/tidb-cdc-to-clickhouse.html)

**待补充/未查到的事实**（如实标注）：AgentLoop、火山 APMPlus 的**底层存储引擎与私有化/单机能力**官方均未公开披露，上述为基于公开资料的推断；如需精确，建议直接向其商务索取私有化部署白皮书。

---

# 调研：单机引擎技术方案

I now have all the material I need. Let me write the report.

# 单机引擎技术深挖：在单节点上统一 Agent Trace 的七大工作负载

> 调研对象 SmithDB 于 2026-05 发布，晚于训练截止，本报告所有关于它的结论均来自联网检索（来源见文末）。其余技术结论结合公开资料与团队 PG/openGauss 内核背景给出。

## 0. 先看清楚对标对象 SmithDB 做了什么，再决定我们怎么"反着抄"

SmithDB 的核心架构（已联网核实）：

- **Rust + Apache DataFusion（深度定制查询引擎）+ Vortex（列式文件格式）+ 对象存储（trace 数据）+ Postgres（segment 元数据）+ 无状态 ingestion/query/compaction 三类服务**。
- 存储模型是**对象存储之上的 LSM**：内存缓冲写入 → flush 成不可变 sorted segment → 后台 compaction。
- 关键设计点（来自其博客/解读文，逐条核实）：
  - **run 是事件序列而非单行**（completion / tool_call / retry / handoff 多事件合并），查询引擎全程处理 fanout filter + event merge。
  - **late materialization（晚物化）**：核心 run 字段与大字段（大 JSON payload）分离存储，只有显式 project 时才去拉大字段 → list/filter 查询快。
  - **progressive time-windowed query**：查"最新的 run"时倒着走时间轴、在最新 segment 上建一个有界时间窗，而不是先全排序。
  - **ingestion 节点直接服务新鲜数据**：每个 segment 记录 writer 的 server ID，活跃 ingestion 节点用 SSD/内存 cache 直接服务最近数据，避开对象存储读 → 这就是它解决"活 trace"的方式。
  - **全文检索**：自研倒排布局，term 组织成 row group + min/max term zone 做 pruning，postings 与 positions 分块存储防止大分配。
  - **JSON 过滤**：对大 payload 的任意 key-path 查询走索引结构。
  - **mutation（trace 后续更新/删除）**：挂 deletion/upgrade vector 到 segment 元数据，不同步重写文件，重写延到后台 compaction → 这是它解决"出生在早上、死亡在下午"的长运行 trace 更新的方式。
  - **时间分层 compaction**：新数据留在写优化的小 segment，老数据压成查询优化的大 segment。
  - **slice-based 集群管理**（仿 Google Slicer / Databricks Dicer），sticky routing 让重复查询命中带缓存的节点。

它公布的单机 P50 延迟基线（这是我们的"性能标尺"）：trace 树加载 92ms / 单 run 加载 71ms / run 过滤 82ms / thread 过滤 131ms / 全文检索 400ms。

**关键判断 ——SmithDB 的复杂度几乎全部花在"分布式 + 对象存储 + 无状态弹性扩展"上**（slice 路由、stateless 三服务、object-storage LSM）。而**我们的硬约束是纯单机**。这意味着：SmithDB 一半以上的工程量（slicer、stateless 编排、object-storage 读放大优化、sticky routing）我们**直接砍掉**。我们要做的是把它验证过的**数据模型与存储布局思想**（LSM + 列式不可变 segment + 晚物化 + deletion vector + 时间分层 compaction + zone-map pruning + 自研倒排）落到**单机本地盘**上，用本地盘的低延迟换掉它为对象存储付出的所有复杂度。这恰好是团队（PG 内核 + 磁盘索引/页面整理框架 + Rust + 查询优化器）最擅长的事。

下面逐条给推荐取舍。

---

## 1. 存储布局：行式热缓冲 + 列式不可变 segment + LSM/compaction

**推荐：三层结构（与 SmithDB 同构，但落到本地盘 + mmap）。**

```
       写入路径                          读取路径
   ┌──────────────┐
   │  WAL (顺序追加)│  ← 崩溃恢复唯一真相源
   └──────┬───────┘
          │
   ┌──────▼─────────────────┐   随机访问/活trace/点查
   │ L0: 行式热缓冲 (MemTable)│ ←─────────────────────  优先命中这里
   │  行存, 按 run_id 索引    │   (最近几分钟的span/活trace全在此)
   │  支持原地更新(span晚到)  │
   └──────┬─────────────────┘
          │ flush (定时/阈值)
   ┌──────▼─────────────────┐   列式扫描/聚合/全文/JSON过滤
   │ L1..Ln: 列式不可变segment│ ←─────────────────────  历史数据走这里
   │  Vortex格式, 时间分层    │
   │  + 内嵌zone map/倒排/JSON│
   │  + deletion/upgrade向量  │
   └─────────────────────────┘
```

**为什么是"行式热缓冲 + 列式 segment"而不是纯列式或纯行式：**

trace 的写入特征是**高频碎片化、乱序、可更新**（span 出生在早上死亡在下午）。列式格式对"原地更新一个字段"极不友好（要重写整列 chunk）。所以热区必须**行式**，理由有三：

1. **写放大最小**：碎片化小 span 追加进行存结构 + WAL，O(1)。
2. **晚到 span 原地 merge**：长运行 run 的后续 update 在行式 MemTable 里就是改一行/追加一个 event，不触发文件重写。
3. **活 trace 点查极快**：运行中的 trace 100% 在内存行缓冲里，按 `run_id` / `trace_id` 哈希直接命中——这就是 SmithDB "ingestion 节点直接服务新鲜数据"的单机版，且我们因为没有对象存储/网络,延迟更低。

冷区用**列式不可变 segment**，理由：

1. **实时聚合**（cost/latency/token usage）是 OLAP 扫描,列存 + 向量化是数量级优势。
2. **不可变**让全文倒排、JSON 路径索引、zone map 都能在 flush 时一次性建好、永不维护更新。
3. **late materialization（强烈建议照搬）**：segment 内把"核心 run 字段（id/parent/start/end/status/cost/latency/token）"与"大字段（input/output 自然语言、大 JSON、多模态 payload 引用）"分列、甚至分文件存。list/filter/聚合只读小列,大 payload 只在用户真点开某条 trace 时按需拉取。这是把 trace "无界大 payload" 问题解决掉的关键一招。

**列式格式选型：直接用 Vortex（已联网核实其特性）。**
- Vortex 是 LFAI&Data（Linux 基金会）孵化项目,Rust,自适应级联编码,对外宣称相对 Parquet **随机访问快 ~100x、扫描快 10-20x、写快 5x、压缩比相当**。
- 它的 layout 树天然契合我们的需求:`StructLayout`(列裁剪)→`ZonedLayout`(每 8k 行一个 zone map 做 pruning)→`ChunkedLayout`(2MB chunk)→`FlatLayout`(IPC 序列化)。**100x 随机访问**正是"从一个列里只取单 trace 的几个值而不解压整列"——这直接服务"随机访问单 trace"。
- 它本就是 SmithDB 用的格式,生态对齐、风险低。**但要复用团队的磁盘索引/页面整理框架去管 Vortex segment 的本地落盘、mmap、引用计数与 GC**,而不是照搬 SmithDB 的对象存储读路径。

**Compaction:照搬时间分层(time-tiered)。**
trace 是时序数据,几乎只追加、按时间查询、老数据不再变。时间分层完美匹配:新数据小 segment(写优化)→老数据合并成大 segment(查询优化、压缩比更高、索引更紧凑)。**不要用 size-tiered/leveled 这种为通用 KV 设计的策略**——trace 的时间局部性让时间分层既简单又高效,也让"按时间范围裁剪"几乎免费。

**Mutation:照搬 deletion/upgrade vector。**
span 晚到更新若已 flush 到 segment,不重写文件,只在 segment 元数据上挂"这行被某个更新版本覆盖/删除"的向量,读时合并、写时延迟到下次 compaction 物化。这让"出生在早上死亡在下午"的长运行 trace 不会引发写风暴。

---

## 2. Trace 树编码:瞬间加载子树 / 找根 / 线程重建

这是 trace 数据区别于普通时序日志的**最核心命题**。逐方案评估,然后给**组合推荐**。

| 方案 | 找根/祖先 | 加载子树 | 插入(碎片化写) | 节点晚到/乱序 | 评价 |
|---|---|---|---|---|---|
| **邻接表** (parent_id) | 递归CTE,慢 | 递归CTE,慢 | O(1),极友好 | 无所谓顺序 | 写最爽,读最差 |
| **物化路径** (root.a.b.c) | 字符串前缀,快 | 前缀匹配,快 | 需知道父路径 | 父晚到则路径未知 | 读快但依赖父先到 |
| **嵌套集** (lft/rgt) | 区间包含,快 | 区间包含,极快 | **灾难**:插一个要改半棵树的lft/rgt | 完全不适用 | trace 高频插入下不可用 |
| **区间/嵌套区间编码** | 区间包含,快 | 区间包含,极快 | 用分数/重标号可免重排 | 较复杂 | 读极快,写需技巧 |
| **闭包表** (ancestor,descendant,depth) | 直接查,极快 | 直接查,极快 | 插一个节点要插 O(depth) 行 | 父晚到可补 | 读极快,写中等、占空间 |
| **ltree** (PG扩展) | GiST前缀,快 | 前缀,快 | 较友好 | 父晚到则标签未知 | PG生态好,但深树受GiST限制 |

**关键约束回顾:trace 是"不断生长 + 乱序到达 + 高频碎片化插入"的树。** 这一条直接**枪毙嵌套集**(任何插入改半棵树),也让纯物化路径/ltree 难受(父 span 可能比子 span 晚到,子来时根本不知道自己的路径)。

**推荐:双编码 —— 行式热区用邻接表,flush 到列式 segment 时一次性物化成区间编码 + 显式根列。**

具体:

1. **热区(MemTable)只存 `span_id` + `parent_span_id` + `trace_id`(=根run_id) + `root_id`**。
   - 写入永远 O(1),完全不在乎到达顺序。父晚到也没关系,只是邻接关系暂时悬空。
   - **`trace_id`/`root_id` 作为每个 span 的冗余冗存字段**,在写入时直接由客户端/ingestion 带上(LangSmith 式 SDK 本就在 span 上携带 trace 上下文)。这样"找根"是 O(1) 直接读字段,"加载整棵 trace 树"= 按 `trace_id` 等值过滤(配合 segment 的 `trace_id` 排序/zone map,一次顺序扫一小段)——这正对应 SmithDB 92ms 的 trace 树加载。
2. **flush 时,对每棵已基本成形的 trace 做一次 DFS,物化两个字段:`pre`(前序序号)和 `post`(后序序号),即区间编码 `[low, high]`**。
   - 子树查询 = `WHERE trace_id=? AND pre BETWEEN root.pre AND root.post`,在列式 segment 上是一段连续区间扫描,**极快**。
   - 因为 segment 不可变,区间编码**一旦物化永不需要重算**——彻底规避嵌套集"插入改半棵树"的死穴。乱序/晚到带来的复杂度全部被吸收在"热区→冷区的一次性物化"这一步。
   - 对于 flush 后才晚到的极少数 span,走 upgrade vector 补丁,或留在邻接表回退路径用递归补齐,不影响主路径。
3. **"线程重建(跨多个 trace 瞬间拼长对话)"**单独建一个 `thread_id` 维度:
   - 每个 run 携带 `thread_id`(会话 ID)。建一个 **`thread_id → [run_id...] 按时间排序** 的二级索引(本质是倒排/跳表)。
   - 重建长对话 = 按 `thread_id` 拉 run 列表 + 各 run 的根 span 的 input/output 小列(late materialization,不拉大 payload)→ 一次范围扫 + 小列投影。对应 SmithDB 的 131ms thread 过滤。

**一句话取舍:写入侧用邻接表(对乱序/碎片化最友好),读取侧用区间编码 + 冗余根列(对子树/找根最友好),用"不可变 segment 的一次性物化"把两者的矛盾消解掉。** 闭包表是强力备选(读更灵活、可查任意 depth),但每 span O(depth) 行的写放大在"每秒大量小 span"下成本偏高,作为可选的二级加速结构而非主编码。不建议 ltree/纯物化路径做主键编码,因为父晚到时路径不可知。

---

## 3. 全文检索(含中文分词)与 JSON 倒排/路径索引

### 3.1 全文检索

**推荐:把倒排索引内嵌进每个不可变列式 segment(per-segment inverted index),用 tantivy 做引擎、cang-jie/tantivy-jieba 做中文分词。**

- **为什么 per-segment 而非全局索引**:segment 不可变 → 倒排在 flush 时一次性建好、永不更新;查询时各 segment 的倒排并行查、结果归并。这与 SmithDB"term 组织成 row group + min/max term zone 做 pruning、postings/positions 分块"的思路一致,且单机下省掉了对象存储读放大,延迟应远好于其 400ms。zone map 让"这个 segment 不可能含该 term"的 segment 被整段跳过。
- **引擎:tantivy(已核实)**。Rust、Lucene 式倒排、启动 <10ms,天然嵌入式,与我们的 Rust 主栈一致。它支持把索引文件按需 mmap,契合不可变 segment + 本地盘。
- **中文分词:cang-jie 或 tantivy-jieba(均已核实存在,基于 jieba-rs)**。两者都是 tantivy 的中文 tokenizer。
  - 建议:**jieba 搜索模式(细粒度) + 可选 n-gram 兜底**,保证"在自然语言 input/output 里搜短语"召回。短语检索靠 tantivy 的 position(positions 分块存,与 SmithDB 一致)。
  - 注意自定义词典(Agent/LLM 领域词、工具名、模型名)以提升分词准确度。
- **建在哪些列**:trace span 的 `input` / `output` / `name` / `error` 等自然语言文本列。late materialization 下这些是"大字段",倒排建在 segment 的大字段子文件上,核心小列不受影响。

### 3.2 JSON / 元数据过滤(任意嵌套字段)

trace 的 metadata、tool 参数、LLM 参数都是**任意 schema 的嵌套 JSON**,且要"对任意嵌套字段过滤"。两条路线,推荐组合:

**路线 A(主):写时 schema 提取 + 列式化高频路径。**
- flush 时扫描 JSON,把出现频率高的路径(如 `metadata.model`、`metadata.user_id`、`ls_provider`)**自动提升为独立的物化列**,建 zone map / 字典编码。高频过滤直接走列式 pruning,极快。
- 这是 ClickHouse 的 `JSON` 类型 / 物化列思路,也是单机下成本最低、收益最高的做法。

**路线 B(补):全 JSON 的路径倒排索引,服务"任意/低频嵌套字段"。**
- 对 JSON 做**路径展平**,生成 `(json_path, value) → row_id` 的倒排(类似 PG 的 `jsonb_path_ops` GIN,但内嵌进 segment)。任意深字段 `a.b.c = x` 直接查倒排。
- value 按类型分别建(字符串走字典/倒排,数值走 zone map 支持范围过滤)。
- 同样 per-segment、不可变、zone-map 可跳段。

**团队复用点**:PG 的 GIN/`jsonb_path_ops` 是这套路径倒排的现成参考实现,团队 PG 内核背景可直接迁移这套布局到不可变 segment 上(免去 GIN 的增量更新/vacuum 复杂度,因为 segment 不可变)。

---

## 4. 写读分离与后台 compaction

**推荐:严格写读路径分离,单进程内多线程 + 明确的资源隔离。**

- **写路径**:`WAL 顺序追加 → 行式 MemTable(原地可更新) → 定时/阈值触发 flush 成不可变 Vortex segment(同时建倒排/JSON/区间编码)`。写路径只碰 MemTable 与 WAL,与查询零锁竞争(MemTable 用无锁/分片结构)。
- **读路径**:`查询同时扫 (活跃MemTable + immutable MemTable + 所有列式segment)`,各源出结果后归并;deletion/upgrade vector 在归并时应用。点查(单 trace/活 trace)优先命中 MemTable;聚合/全文/JSON 扫 segment。
- **compaction**:独立后台线程池,时间分层合并小 segment → 大 segment,物化 deletion vector、重建更紧凑的索引、合并 trace 区间编码。**compaction 限速**(令牌桶/IO 配额),避免抢占前台写入与查询的 IO/CPU——这是单机下保证 P99 稳定的关键,SmithDB 靠把 compaction 拆成无状态服务隔离,我们单机内靠 cgroup/线程优先级 + IO 限速达到同样效果。
- **可见性**:flush/compaction 用**原子指针切换**(把新 segment 集合的不可变快照原子发布给读端),读端持有快照引用,旧 segment 引用计数归零后由 GC 回收。无读写锁、无长事务。

---

## 5. 单机内多租户隔离

私有化 + 多租户是硬约束。单机下推荐**三层隔离**:

1. **数据隔离(强制):`tenant_id` 作为所有 segment 的最高排序前缀 / 物理分目录。**
   - 每个租户的数据落在独立的 segment 文件集合(独立目录),物理隔离。查询强制带 `tenant_id`,在存储层用目录/前缀直接裁剪,做到"一个租户的查询永远不扫另一个租户的文件"。这比单纯逻辑过滤更安全(满足私有化合规),也让"删租户"= 删目录,极简。
2. **资源隔离:每租户的写入配额、查询并发、内存(MemTable 上限)、compaction IO 配额独立计量。**
   - 防止单租户高频写入打满 IO 饿死其他租户。单进程内用 per-tenant 调度器 + 令牌桶;若需更强隔离,可一租户一进程(见第 6 节)。
3. **加密/合规(私有化加分项):per-tenant 静态加密密钥**,租户数据用各自密钥加密落盘,满足金融/政企私有化要求。

> 说明:SmithDB 的多租户依赖其 slice 路由 + 对象存储 bucket 隔离,这套在单机不适用;单机的多租户本质是"目录隔离 + 资源配额",反而更简单可控。

---

## 6. 嵌入式库 vs 单机服务进程

这是产品形态的关键决策。结合"商业产品 + 私有化 + 多租户 + 强易用性"硬约束:

**推荐:核心做成嵌入式存储引擎库(Rust crate,DuckDB 式 in-process),外面包一层单机服务进程(带 SQL/HTTP/gRPC 端点、连接管理、多租户、认证、备份)。即"嵌入式内核 + 服务化外壳"。**

理由:

- **嵌入式内核(借鉴 DuckDB 而非 DataFusion 的定位,已核实区别)**:DataFusion 是"查询引擎框架"(给系统构建者拼数据库用),DuckDB 是"开箱即用的完整库"(自带存储/事务/WAL)。**我们要造的恰恰是一个完整数据库,所以内核要像 DuckDB 一样自带存储+事务+WAL,而查询执行层复用 DataFusion(SmithDB 同款,深度定制)**。即:**存储/事务/WAL 自研(团队强项),查询执行用 DataFusion,文件格式用 Vortex**。这三者正是 SmithDB 的组合,被生产验证过。
- **为什么不止于嵌入式库**:多租户、私有化、认证、网络访问、在线备份、监控——这些是商业产品必需的,纯嵌入式库给不了。所以外面必须有**单机服务进程**。
- **为什么不直接照搬 SmithDB 的"无状态三服务"**:那是为分布式弹性扩展设计的,单机下是纯负担。单机就是**一个进程(可内部多线程池:ingestion / query / compaction 三组线程)**,简单、低延迟、易部署(私有化一键起)。
- **多租户隔离的进程选项**:默认单进程多租户(资源配额隔离);对强隔离/大客户提供"一租户一进程 + 共享磁盘格式"的部署档位,兼顾易用与隔离强度。

> 易用性落点:对外暴露**原生 SQL**(团队强项,也是相对 ClickHouse/SmithDB 的差异化卖点——SmithDB 对外不是通用 SQL 库),内置 trace 树/线程/聚合的专用函数与视图(`load_trace_tree(trace_id)`、`rebuild_thread(thread_id)`、`subtree(span_id)`),让 Agent 可观测性场景"开箱即用"。

---

## 7. 崩溃恢复(WAL)与一致性

**推荐:经典 WAL + 不可变 segment 的"双真相源"模型,团队 PG 经验可直接迁移。**

- **WAL 是唯一可变状态的真相源**:所有写(新 span、晚到 update、deletion)先顺序写 WAL(组提交 group commit,摊薄 fsync 成本,匹配高频碎片化写)。WAL 落盘即返回 ack → 写入低延迟且持久。
- **MemTable 是 WAL 的内存物化**:崩溃后,重放 WAL 重建 MemTable 即可恢复到崩溃前一刻。
- **不可变 segment 自带持久性**:flush 一旦把 segment 原子落盘(写完 + fsync + 原子改 manifest),其覆盖的 WAL 区段即可截断(checkpoint)。segment 不可变 → 永不会半写损坏一条已有数据。
- **manifest / metastore**:用一个本地元数据存(可以是内嵌的事务性 KV,或 SmithDB 同款的内嵌 Postgres/SQLite)记录"当前有效 segment 集合 + 各 segment 的 deletion/upgrade vector + WAL checkpoint 位点"。**manifest 的原子更新 = 整库一致性提交点**。崩溃恢复 = 读 manifest 确定有效 segment + 重放 checkpoint 之后的 WAL。
- **一致性级别**:单机下天然可做到**单写者 + 多读者 MVCC**(DuckDB 同款,已核实 DuckDB 即单写者 + WAL)。读端持有 segment+MemTable 的不可变快照,看到一致视图;写端串行追加。对 trace 这种"以追加为主、偶有更新"的负载,单写者完全够用,且省掉复杂并发控制。
- **活 trace 的一致性**:运行中 trace 的 span 在 MemTable,查询读 MemTable 快照即可看到"未完成 trace 的当前状态",无需特殊机制——这就是"活 trace 可查"的实现。

---

## 总结:推荐技术栈一页纸

| 维度 | 推荐取舍 | 关键理由 |
|---|---|---|
| **整体形态** | 嵌入式内核(自带存储/WAL/事务)+ 单机服务外壳,**砍掉 SmithDB 的分布式/对象存储/无状态三服务** | 硬约束是纯单机;本地盘换掉它所有为对象存储付的复杂度 |
| **存储布局** | 行式热缓冲(MemTable,可原地更新)+ 列式不可变 segment(Vortex)+ 时间分层 LSM compaction;**late materialization 分离小列/大payload**;**deletion/upgrade vector** 处理晚到更新 | 写友好(碎片化/乱序/晚到)+ 读友好(列式聚合/全文/随机访问)的统一 |
| **列式格式** | **Vortex**(LFAI 项目,SmithDB 同款),随机访问 ~100x、扫描 10-20x 于 Parquet;落到本地盘 + mmap,用团队页面整理框架管 GC | 单 trace 随机访问 + OLAP 扫描双赢 |
| **树编码** | 写侧**邻接表 + 冗余 `trace_id/root_id`**(对乱序/碎片化最友好);读侧 flush 时一次性物化**区间编码 `[pre,post]`**(子树极快);**枪毙嵌套集**(插入改半棵树);`thread_id` 倒排做线程重建 | 不可变 segment 把"写友好编码"与"读友好编码"的矛盾消解掉 |
| **全文检索** | 每 segment 内嵌**tantivy 倒排** + **cang-jie/tantivy-jieba 中文分词**(细粒度+自定义词典),postings/positions 分块、term zone-map 跳段 | 与 SmithDB 全文布局同构,单机无对象存储读放大 → 应优于其 400ms |
| **JSON 过滤** | 主:flush 时高频路径**物化成列**(zone map/字典);补:全 JSON **路径倒排**(PG GIN/jsonb_path_ops 迁移) | 高频快、低频全覆盖,segment 不可变免维护 |
| **写读分离** | WAL→MemTable→flush;读扫(MemTable+segment)归并;compaction 独立线程池 + **IO 限速** | 单机 P99 稳定靠 compaction 限速 |
| **多租户** | `tenant_id` 物理分目录(数据隔离)+ per-tenant 资源配额 + per-tenant 加密;大客户可一租户一进程 | 私有化合规 + 删租户=删目录 |
| **崩溃恢复** | 组提交 WAL(唯一可变真相源)+ 不可变 segment + manifest 原子提交;**单写者多读者 MVCC** | 团队 PG 经验直迁;活 trace = 读 MemTable 快照 |

**一句话战略**:SmithDB 证明了"LSM + 列式不可变 segment + 晚物化 + deletion vector + 自研倒排 + 时间分层 compaction"是 Agent 可观测性的正确数据范式;它的复杂度几乎全在分布式与对象存储。我们在**纯单机本地盘**上复刻它的数据范式、砍掉它的分布式包袱、加上**原生 SQL + 中文分词 + 物理多租户**三个国内私有化差异化卖点,用团队最强的 PG 内核 + Rust + 磁盘索引能力即可落地,且单机延迟有望全面优于其公布基线。

---

**Sources:**
- [We built SmithDB, the data layer for agent observability — LangChain](https://www.langchain.com/blog/introducing-smithdb)
- [SmithDB Explained: AI Agent Observability for Production Teams — softwarechains](https://www.softwarechains.com/insights/smithdb-agent-observability-langsmith)
- [Ankush Gola on X (SmithDB announcement)](https://x.com/ankush_gola11/status/2054661816249360553)
- [What is Vortex? Columnar File Format for Analytics — Spice AI](https://spice.ai/learn/vortex)
- [Vortex — vortex.dev](https://vortex.dev/) / [vortex-data/vortex — GitHub](https://github.com/vortex-data/vortex)
- [Vortex File Format and Storage — DeepWiki](https://deepwiki.com/vortex-data/vortex/3-file-format-and-storage) / [Vortex Layouts docs](https://docs.vortex.dev/concepts/layouts)
- [Apache Parquet vs. Newer File Formats (BtrBlocks, FastLanes, Lance, Vortex) — Medium](https://dipankar-tnt.medium.com/apache-parquet-vs-newer-file-formats-btrblocks-fastlanes-lance-vortex-cdf02130182c)
- [quickwit-oss/tantivy — GitHub](https://github.com/quickwit-oss/tantivy) / [DCjanus/cang-jie — GitHub](https://github.com/DCjanus/cang-jie)
- [Hierarchical models in PostgreSQL — Ackee](https://www.ackee.agency/blog/hierarchical-models-in-postgresql) / [Store Trees As Materialized Paths — sqlfordevs](https://sqlfordevs.com/tree-as-materialized-path)
- [Nested Sets vs. Nested Intervals — Google Groups](https://groups.google.com/g/comp.databases.theory/c/omiiY_bFabI/m/OfctMmAgd3sJ)
- [What is DuckDB? — Spice AI](https://spice.ai/learn/duckdb) / [Apache DataFusion FAQ](https://datafusion.apache.org/user-guide/faq.html) / [Duck Hunt: moving Bauplan from DuckDB to DataFusion — Bauplan](https://www.bauplanlabs.com/post/duck-hunt-moving-bauplan-from-duckdb-to-datafusion)

---

# 调研：差异化与团队能力复用

I have enough verified primary-source material to answer. I'll write the report directly as my final response (no file, per instructions).

---

# Trace DB 调研：差异化护城河与团队能力复用

> 本报告基于联网核实的 SmithDB 公开资料（LangChain 2026-05 Interrupt 发布）。SmithDB 关键事实：用 Rust + Apache DataFusion + Vortex 文件格式构建，对象存储承载 trace 数据，Postgres 存 segment 元数据，无状态 ingestion/query/compaction 服务，LSM 架构。它支持 trace 树加载、全文检索、JSON key-path 过滤、线程重建、聚合。**经多方核对，SmithDB 公开材料中完全没有提及向量检索 / 语义召回 / embedding / few-shot / 数据飞轮** —— 这正是我们最大的差异化空档。

---

## 0. 核心结论（先给决策者）

1. **团队资产可直接转成护城河，但要换骨不换皮**：复用的应是「能力与代码模块」（向量索引引擎、磁盘页面整理框架、查询优化器、PG 生态/SQL 兼容），而**不是** yiTrace 那套「关系内核 + 多模索引大杂烩」的整机架构。trace 是 LSM + 列存 + 对象引用的负载，PG 的堆表/MVCC/WAL 不是它的最优形态。
2. **最强差异化 = 原生「语义 trace 召回」**：把可观测性存储与向量检索在同一引擎内融合，让"找相似 trace/最佳实践/失败案例"成为一等查询原语。SmithDB 没做，ClickHouse 路线也做不顺。这是我们团队（HNSW/IVF/DiskANN + Rust）独有的、对手难抄的点。**价值高、可行性高，建议作为产品的招牌能力。**
3. **引擎路线建议：混合策略**。不是非此即彼。**底座自研 Rust LSM 列存 + 复用 DiskANN/HNSW 向量层**，但**用 openGauss 的成熟模块（优化器、执行器片段、SQL 解析、Chinese 分词生态）做"加速器"而非"地基"**。纯 openGauss 扩展上市快但性能/形态受限；纯 Rust 全新引擎可控性/性能最佳但慢。详见第 3 节。
4. **数据飞轮必须 DB 原生**：轨迹导出、奖励信号物化视图、SOP/few-shot 抽取、语义召回——这四个原语让我们从"存 trace 的库"升级为"让 trace 增值的飞轮引擎"，是对 SmithDB 的降维差异化。

---

## 1. 如何把现有资产转成 trace DB 的护城河

团队的四块能力，对应 trace DB 的四个最难工程点。逐一映射：

### 1.1 向量索引工程（HNSW/IVF/DiskANN）→ 语义召回护城河（最值钱）
- 这是**别人没有、我们现成**的资产。trace DB 赛道里，ClickHouse 派、SmithDB 派都在"标量 + 全文"上卷，向量是它们的薄弱区（ClickHouse 的向量能力是后加的、HNSW 实现不成熟；SmithDB 干脆不提）。
- **DiskANN 尤其关键**：单机私有化场景，trace embedding 量级可达数十亿，内存放不下全部向量。DiskANN 是为"SSD 上的十亿级 ANN、低内存占用"设计的，正好匹配"单机最大化性能 + 不强制对象存储"的硬约束。团队已有 DiskANN 工程 = 直接拿来做单机十亿级 trace 语义召回。
- **磁盘索引与页面整理框架**复用价值极高：trace 是 LSM 负载（高频小写、乱序、长 span 后更新），需要 compaction/段合并/页面整理——这套框架团队已有，迁移成本远低于从零写。

### 1.2 PostgreSQL/openGauss 内核能力 → SQL 易用性 + 生态护城河
- SmithDB 的查询是**私有 API**，不是标准 SQL。我们若提供**原生 SQL（含 trace 树/线程/向量召回的 SQL 扩展语法）**，对国内企业 = 巨大易用性优势：现有 BI 工具、SQL 人才、报表系统直接接。
- openGauss 元数据存储正是 SmithDB 也在用的模式（"Postgres for segment metadata"）——我们团队对 PG catalog/元数据管理极熟，等于在对手的同一选择上拥有更深的掌控力。
- **中文分词生态直接复用**：PG 系的 `zhparser`(SCWS) / `pg_jieba` 已是成熟方案。场景硬需求"在自然语言输入输出里搜中文短语"，我们能立刻提供生产级中文全文检索——这是面向国内企业的刚需，海外产品（SmithDB 的全文检索）对中文分词支持薄弱，是天然差异点。

### 1.3 查询优化器能力 → 复杂 trace 查询的性能护城河
- trace 查询是"树感知 + JSON 任意字段过滤 + 时间范围 + 聚合 + 向量召回"的混合，谓词下推、late materialization（SmithDB 也用：把大 payload 列分离、按需取）、代价估计都需要优化器。团队的优化器能力让我们能做**混合查询的统一代价模型**（比如：先向量粗召回再标量精过滤 vs 反之，由优化器决定）——这是把"标量+向量混合检索"做快的核心，纯堆砌引擎做不到。

### 1.4 Rust 能力 → 与 SmithDB 同代的工程底座
- SmithDB 用 Rust + DataFusion + Vortex。我们团队有 Rust，意味着可以走**同一代技术栈**而非落后一代（C 内核扩展）。Rust 的内存安全 + 零成本抽象，对"高频碎片化写入 + 无 GC 停顿"的 trace 负载是正确选择。
- 可直接复用 Apache DataFusion（向量化执行 + 查询计划框架，Apache-2.0）作为执行引擎骨架，把团队精力集中在 trace 专用算子（树遍历、向量召回算子、LSM merge）上。

> **护城河总结**：单点能力对手或许都能补，但"**Rust LSM 列存 + 原生 SQL + 生产级中文全文 + 单机十亿级 DiskANN 语义召回 + 混合查询优化器**"五合一，且团队已有现成代码与人才储备——这个组合的复制成本，对 SmithDB（要补向量与中文）和 ClickHouse 派（要补 trace 语义与树模型）都是 1~2 年的工程纵深。

---

## 2. 关键差异化：原生「语义 trace 召回」—— 价值与可行性评估

**定义**：对每个 run/span 的自然语言 input/output（及结构化决策上下文）生成 embedding，在引擎内建 ANN 索引；查询时支持"给定一个 trace/span/query，召回语义最相似的历史 trace"，并与标量/JSON/时间/树结构过滤**在同一次查询内融合**。

### 2.1 为什么它是核心原语（价值）
源对话指出语义召回是评估、few-shot 注入、纠错的核心原语，这与 Agent 工程的真实工作流一致：

| 工作流 | 语义召回的作用 |
|---|---|
| **评估（eval）** | 新 trace 进来，召回历史相似 trace 的人工/自动评分，作为基线对比；找"同类问题上 agent 表现"的分布 |
| **few-shot 注入** | 运行时给 agent 检索"过去成功处理过的相似任务轨迹"，作为 in-context 示例 → 直接提升 agent 在线效果（RAG over traces） |
| **纠错 / 回归排查** | 出现 bad case，召回历史相似失败模式，定位是否为已知问题、复现条件 |
| **最佳实践沉淀** | 在海量 trace 里召回"同类任务 cost/latency 最优的轨迹" → 抽成 SOP |
| **数据飞轮** | 语义聚类找高价值/罕见 trace 喂训练（见第 4 节） |

**战略价值**：这把产品定位从"事后看日志的可观测性存储"，升级为"**在线参与 agent 决策的检索底座**"。后者的客户黏性、单价、不可替代性远高于前者——它直接进入客户 agent 的推理回路，而非只是旁路监控。这正是 SmithDB **没有强调**的方向，是结构性空档而非边缘特性。

### 2.2 SmithDB 为什么没做 / 难做（差异化成立性）
- SmithDB 架构是"对象存储 + 无状态计算 + Vortex 列存 + 倒排索引"，为**标量/全文/树查询**优化。ANN（尤其图索引 HNSW/DiskANN）是**有状态、对随机访问延迟敏感**的负载，与"对象存储 + 无本地盘 + 无状态"的设计哲学**相冲突**——在对象存储上跑 HNSW 图遍历延迟会爆炸。这是它架构层面的取舍，不是疏忽。
- **而我们的硬约束恰好相反**：纯单机、可用本地盘、不强制对象存储。单机本地 NVMe 上跑 DiskANN/HNSW 正是最优环境。**对手的架构劣势 = 我们约束下的天然优势。** 这是可行性的关键支点。

### 2.3 可行性评估
- **技术可行性：高**。团队已有 HNSW/IVF/DiskANN 生产代码，缺的只是"接入 trace 写入流 + 增量索引 + 与标量过滤融合"的胶水层，不是从零造索引。
- **核心工程挑战（需正视）**：
  1. **embedding 来源/成本**：trace 量大，全量 embedding 成本高。方案：可配置采样/按租户策略/异步管线/复用客户已有 embedding（很多 agent 本就调 embedding API，可直接旁路捕获写入）。
  2. **增量与乱序**：trace 是流式、乱序、可后更新。ANN 索引需支持增量插入 + 段合并时重建——这正好复用团队的 LSM/页面整理框架（向量段也走 compaction）。
  3. **过滤性 ANN（filtered vector search）**：真实查询是"在 租户A、最近7天、cost>X 的 trace 里找语义相似"。带标量谓词的 ANN 是公认难点，但团队的优化器能力 + IVF（按分区裁剪）+ DiskANN 的 filter 支持，使其可控。这本身也是一道技术壁垒。
- **建议**：作为 **P0 招牌能力**立项，但分阶段——v1 先做"离线/近线语义召回 + eval 辅助"（批量 embedding、段级索引），v2 再做"在线低延迟 few-shot 召回"（进入 agent 推理回路，延迟要求更苛刻）。

> 一句话定位：**"全球第一个把 Agent 可观测性与语义检索原生融合的单机数据库"** —— 可作为产品的核心 slogan 与融资/销售叙事。

---

## 3. 引擎路线：openGauss 内核扩展 vs 全新 Rust 引擎

不建议二选一极端，但需明确各自影响，并给出推荐的混合方案。

| 维度 | 路线A：openGauss 内核扩展 | 路线B：全新 Rust 引擎 | 路线C（推荐）：Rust 自研底座 + 复用 openGauss 模块 |
|---|---|---|---|
| **上市速度** | **最快**（复用整机：SQL/事务/HA/分区/向量扩展现成，6~9 月可出 MVP） | **最慢**（LSM/列存/全文/向量从头搭，12~18 月） | **中**（自研 LSM 列存核心，但优化器/SQL 解析/中文分词/向量索引模块移植，~9~12 月） |
| **性能** | **受限**：PG 堆表 + MVCC + WAL + 行存，对"高频小 span 写入 + 列式聚合 + 树扫描"非最优；膨胀与 vacuum 是长期痛点 | **最佳**：LSM + 列存 + 向量化执行专为 trace 负载设计，与 SmithDB 同代 | **接近B**：核心数据路径自研最优，外围复用不拖累热路径 |
| **可控性** | **低**：受 openGauss 版本节奏、内核约定约束；trace 专用改造常与内核假设冲突 | **最高**：每一行代码自主，架构随场景演进 | **高**：底座自主，复用模块以"库"形式集成而非"寄生于内核" |
| **商业授权** | **可控**：openGauss = Mulan PSL v2（宽松、permissive，允许闭源商用与衍生品，无 copyleft 传染）。比 GPL 友好 | **最干净**：自有代码 + Apache-2.0 依赖（DataFusion/Vortex/Arrow），无任何 copyleft 风险，私有化销售零顾虑 | **干净**：Mulan PSL v2(宽松) + Apache-2.0，均可闭源商用 |
| **团队复用度** | 高（PG 人才直接上手），但**强迫接受 PG 整机形态** | 中（Rust 复用，但 PG 资产闲置） | **最高**（Rust + PG 双资产都用上，各取所长） |

**授权要点（已核实）**：
- openGauss 采用 **Mulan PSL v2**，是 OSI 认证的**宽松型**许可证，明确授予永久、全球、免费、不可撤销的版权与专利许可，**允许修改、闭源商用、制作衍生品**，无 GPL 式传染条款 → 私有化销售无授权障碍。
- Rust 生态核心（Apache DataFusion / Arrow / Vortex）均 **Apache-2.0**，含专利授权，商用友好。
- **风险提示**：openGauss 是华为主导的国产内核，对"信创/国产化"销售是**加分项**；但若复用其代码，需做许可证合规审计（保留版权声明、NOTICE）。这点对国内企业客户反而是卖点。

**推荐落地（路线C 具体拆解）**：
- **自研（热路径，决定性能）**：LSM 写入引擎、列式 segment 格式（或直接用 Vortex/Arrow）、compaction、trace 树编码、向量召回算子、混合查询执行。
- **复用 openGauss/PG 资产（外围，决定速度与易用）**：① SQL parser/语法框架做"原生 SQL + trace/向量扩展语法"；② 优化器代价模型思想与部分实现；③ 元数据 catalog 管理经验（甚至直接用一个 PG 实例存 segment 元数据，与 SmithDB 同构）；④ 中文全文检索直接移植 zhparser/pg_jieba 的分词内核。
- **复用团队向量资产（差异化）**：DiskANN/HNSW/IVF 作为独立向量层模块嵌入。
- **复用 Rust 生态**：DataFusion 做执行引擎骨架，避免重造向量化执行。

---

## 4. 数据飞轮（trace → 评估 → 推理/训练优化）需要的 DB 原生原语

飞轮的本质：让 trace 不只是"被查看"，而是"自动产出可用于改进 agent 的资产"。DB 原生提供以下原语，把飞轮内建进存储层（这是 SmithDB 完全没覆盖、对手要靠外部管线拼凑的领域）：

### 4.1 语义召回原语（Semantic Recall）— 飞轮的引擎
- 即第 2 节能力。它同时服务 eval（找相似基线）、few-shot（在线注入）、训练（找高价值样本）。**飞轮的四个环节都依赖它**，是飞轮的"轴承"。

### 4.2 轨迹导出原语（Trajectory Export）— 飞轮的出口
- 原生支持把"一棵完整 trace 树 / 一段线程对话 / 一组语义召回结果"一键导出为**训练/微调标准格式**（如 messages 数组、prompt-completion 对、DPO 偏好对、tool-call 轨迹）。
- 关键：导出要**树感知 + 多模态引用解析**（把 payload 引用还原），且支持增量/流式导出大数据集。这是"trace DB"对"日志库"的本质升级——直接对接训练管线。

### 4.3 奖励信号物化视图（Reward Signal Materialized Views）— 飞轮的度量
- 把人工反馈、自动评估分（LLM-judge）、cost/latency/token、成功/失败标签，作为**奖励信号**与 trace 关联，用**增量物化视图**实时维护（团队优化器/物化视图能力可复用）。
- 让"哪些轨迹是高奖励/低成本/高效"成为可实时聚合、可索引、可被语义召回过滤的一等数据 → 训练时直接按奖励采样（RLHF/RFT 数据源）。
- 这正好契合场景的"实时聚合 cost/latency/token usage"硬需求，但把它从"看板指标"升维成"训练信号"。

### 4.4 SOP / few-shot 抽取原语（Pattern Extraction）— 飞轮的产物
- 在语义召回 + 奖励视图之上，提供原生算子：对"同类任务的高奖励轨迹"做聚类，抽取**共性步骤模板（SOP）**和**最佳 few-shot 示例集**。
- 输出可被 agent 运行时直接拉取（"给我这类任务的 top-k 成功示范"）→ 闭环回到在线推理，飞轮转起来。

### 4.5 配套基础原语（保证飞轮数据质量）
- **活 trace 查询**：运行中 trace 即可被召回/评估（SmithDB 已支持读 ingestion 节点缓存，我们需对标——查询计划直接读未 flush 的内存/SSD 段）。
- **多模态大 payload 引用 + late materialization**：飞轮导出时按需解析引用，列表/过滤时不读大 blob（对标 SmithDB 的 large-field 分离）。
- **乱序/长 span 的 run-as-event-sequence 建模**：run 建模为事件序列而非不可变行，支持"早上出生下午死亡"的更新——这是 trace 正确性的地基，飞轮喂的数据才干净。

> **飞轮护城河**：上述原语让客户的每一次 agent 运行都自动沉淀为"可评估、可召回、可训练"的资产，且越用越值钱、迁移成本越高 —— 形成数据层面的客户锁定。SmithDB 停在"可观测性存储"，我们把终点设在"agent 自我改进的数据底座"。

---

## 附：与 SmithDB 的差异化定位一图

| 能力 | SmithDB（已核实） | 我们的差异化 |
|---|---|---|
| trace 树 / 线程 / JSON 过滤 / 全文 | ✅ 强（92ms/131ms/400ms P50） | 对标，需追平 |
| 部署形态 | 对象存储 + 无状态 + 分布式 | **纯单机极致性能**（本地 NVMe），更简洁、私有化更友好 |
| 查询接口 | 私有 API | **原生 SQL**（易用性、生态） |
| 中文全文检索 | 弱（英文倒排为主） | **生产级中文分词**（zhparser/pg_jieba） |
| **语义 trace 召回（向量）** | ❌ **未提供** | ✅ **DiskANN/HNSW 单机十亿级，招牌能力** |
| **数据飞轮原语** | ❌ 仅观测，无飞轮 | ✅ 轨迹导出 / 奖励物化视图 / SOP 抽取 / 语义召回闭环 |
| 信创/国产化 | ❌ | ✅ openGauss 系 + Mulan PSL，信创加分 |

---

**Sources（已核实，2026-06）：**
- [LangChain: We built SmithDB, the data layer for agent observability](https://www.langchain.com/blog/introducing-smithdb)（架构/延迟/LSM/Vortex/late materialization/无向量检索 一手来源）
- [SmithDB Explained — softwarechains](https://www.softwarechains.com/insights/smithdb-agent-observability-langsmith)（确认无 vector/semantic/few-shot/flywheel）
- [LangChain Interrupt 2026 分析 — solafide.ca](https://solafide.ca/blog/2026-05-langchain-interrupt-agent-infrastructure)
- [Mulan Permissive Software License v2 — OSI](https://opensource.org/license/MulanPSL-2.0) / [openGauss 采用 Mulan PSL v2 — openGauss 官方](https://opengauss.org/en/)
- [zhparser — PGXN](https://pgxn.org/dist/zhparser/) / [pg_jieba — GitHub](https://github.com/jaiminpan/pg_jieba)（PG 中文分词生态）

> 诚实声明：SmithDB 的 Vortex 文件格式具体定制细节、其 self-hosted 版与 eval 集成的最终形态，官方标注为"nearing completion"，公开资料未披露完整内幕，本报告未对这些未公开细节做推断。