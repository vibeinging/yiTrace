# Trace 领域现状调研

> 日期：2026-07-07
> 类型：领域调研 / 竞品格局
> 目的：在 yiTrace 进入「功能冻结、生产成熟度硬化」阶段前，搞清 trace 赛道当前格局、主流技术栈、yiTrace 的真实卡位和差异化叙事，校准硬化方向。
> 关联：[`CURRENT_STATE.md`](../CURRENT_STATE.md)、[`plans/2026-07-07_next-phase-hardening-plan.md`](../plans/2026-07-07_next-phase-hardening-plan.md)、[`research/2026-07-03_agent-trace-data-applications.md`](2026-07-03_agent-trace-data-applications.md)、[`research/2026-06-18_agent-observability-competitors.md`](2026-06-18_agent-observability-competitors.md)

---

## 1. 市场格局：已收敛到 ~6 家生产级平台

LLM/Agent trace + observability 赛道经 2024–2025 洗牌，从「百花齐放」收敛到 6 家公认生产级领导者：

| 平台 | 开源/自托管 | 定位 |
|---|---|---|
| **Langfuse** | ✅ open-core，可自托管 | 开发者体验最好，dashboard 开箱即用；v3 用 ClickHouse 做 OLAP |
| **LangSmith** | ❌ 云 only | 与 LangChain/LangGraph 深度绑定 |
| **Arize Phoenix** | ✅ 完全开源，默认 SQLite | span 级 tracing + 嵌入可视化，OTel 兼容 |
| **Helicone** | ⚠️ 部分 | 代理层 observability，成本/吞吐监控强 |
| **Braintrust** | ⚠️ 部分 | eval + 自托管 evals |
| **MLflow** | ✅ | 老牌，tracing 是其中一块 |

**关键趋势**：trace 与 eval 正在合并成同一类产品，而非两个独立工具。Morph（2026）指出当前 trace **仍缺高层 agent 行为**（planning 质量、工具选择推理），是下一前沿。

参考：
- [Top 5 LLM and Agent Observability Tools in 2026 – MLflow](https://mlflow.org/top-5-agent-observability-tools/)
- [Agent Observability: LangSmith, Langfuse, Arize 2026 – Digital Applied](https://www.digitalapplied.com/blog/agent-observability-platforms-langsmith-langfuse-arize-2026)
- [Agent Observability (2026): What the Trace Can't See – Morph](https://www.morphllm.com/agent-observability)

## 2. 技术栈分两层：协议标准 + 存储后端

### 2.1 协议层（trace 的「语法」）正在标准化

两条主线并存，且在融合：

1. **OpenTelemetry GenAI semantic conventions**（`gen_ai.*`）— 已从主 OTel 仓库迁出到独立 repo `open-telemetry/semantic-conventions-genai`，CNCF 背书的标准。最新动态：正在为 **agentic systems** 单独定义 [span 约定](https://github.com/open-telemetry/semantic-conventions-genai/issues/35)。
2. **OpenInference**（Arize 出）— 定位是「每条 OpenInference trace 都是合法 OTLP trace」，在属性名上加 AI 语义。Phoenix 实际是把 OTel GenAI / OpenLLMetry **翻译成 OpenInference** 来渲染。

> ✅ yiTrace 卡位准确：`POST /v1/traces` 直接是 OTLP/HTTP 标准端点，已埋点 OTel GenAI / OpenInference 的应用零改动可灌入。
> ⚠️ 但 **OTLP 的 tenant 属性映射** 和硬强制中间件仍是 `CURRENT_STATE` 点名的待补项——已埋点 OTel 的应用不会自动带 `X-Tenant-Id` 头，需要从 OTel resource/span attribute 提取 tenant 的映射层。这是生态兼容性硬伤，不只是「待补优化」。

### 2.2 存储层（trace 的「引擎」）分化明显

主流 SaaS 都走 **「组件拼装」** 路线：

| 组件 | 谁用它 | 干什么 |
|---|---|---|
| **ClickHouse** | Langfuse v3、Helicone | 高吞吐 OLAP，trace 事件 + 成本聚合 |
| **Postgres** | Langfuse、Phoenix | 元数据、项目配置、关系数据 |
| **SQLite（默认）** | Phoenix | 本地开发，文件型 |
| **Redis** | Langfuse | 缓存 + 后台任务协调 |
| **S3** | Langfuse | blob/文件 |
| **独立向量库** | 部分平台 | embedding 相似度（Langfuse 自托管文档**未明确**专用向量子系统）|

参考：
- [Langfuse self-hosting architecture](https://langfuse.com/self-hosting)
- [How Langfuse is scaling LLM observability for the agentic era – ClickHouse](https://clickhouse.com/blog/langfuse-llm-analytics)
- [Phoenix self-hosting configuration](https://arize.com/docs/phoenix/self-hosting/configuration)
- [Cost Optimization in LLM Observability: How LangFuse Handles Petabytes](https://medium.com/@sharanharsoor/cost-optimization-in-llm-observability-how-langfuse-handles-petabytes-without-breaking-the-bank-0b0451242d1e)

## 3. yiTrace 的差异化定位

yiTrace = **「自研引擎」路线**，对标的是上面那套「ClickHouse + Postgres + 向量库 + OTel collector」的拼装栈，**而不是任何一家 SaaS**。

| 维度 | 主流 SaaS（Langfuse/Phoenix 等） | yiTrace |
|---|---|---|
| **检索** | BM25 和向量通常分开（ClickHouse 全文 + 独立向量库） | **中文 BM25 + 向量 ANN + RRF 混合检索一体化**，含「进图过滤」而非 post-filter |
| **存储后端** | 多组件拼装（≥3 个进程） | **单进程自研 Rust 引擎**，零外部依赖，WAL+manifest+memtable+段存储 |
| **本地优先** | Phoenix 默认 SQLite，但生产要换 | **durable data dir + 单写者锁**，Node/Python/Rust 三种嵌入式 DB，不起 HTTP server |
| **多租户** | 通常靠应用层 | **tenant_id 全流程贯穿**（WAL/wire/折叠/检索） |
| **崩溃安全** | 靠 Postgres/ClickHouse | **自研 GC log + fsync + 重启不丢**（20 次 kill-9 零失败）|
| **确定性 event_id** | 不强调 | **跨 Python/TS/引擎逐字节一致**，崩溃重放去重 |

### 诚实的边界（避免商务误读）

- yiTrace 的「自研」在**结构上成立**（中文检索 + 图式向量是自己的引擎逻辑），但 `CURRENT_STATE` 自己承认「已生产级」**还不成立**——jieba/graph_index 真库链接仍是占位，差构建机上的库 + 真召回对标。
- 主流 SaaS 的 ClickHouse 路线在**已验证的横向扩展性**上更成熟（Langfuse 有 petabyte 级公开案例）；yiTrace 的分布式还是「可验证的数据路径」，不是托管集群。
- 「完全本地优先 + 嵌入式 + 中文检索/向量是一等公民」是 yiTrace 唯一能赢的叙事，**不要跟 Langfuse 比 dashboard 功能**。

## 4. 对硬化阶段的启示

当前「功能冻结、转入生产成熟度」**方向是对的**，理由从市场现状反推很清楚：

1. **赛道已收敛**，再补一个 endpoint / Golden Path 治理功能，边际收益低于把现有能力的「可信度」补齐。
2. **存储架构是真正的护城河**——主流玩家都在用 ClickHouse，yiTrace 用自研段存储 + 自有索引。所以硬化计划里「规模压测」「崩溃恢复」「真召回对标」比新增 API 重要得多。
3. **安全/隐私是切入金融/政企的入场券**。「regulated industry + on-prem LLM」是真实需求市场，但这些客户的第一道门槛是 **TLS + RBAC + 持久审计 + 脱敏**——正好是 `CURRENT_STATE §5` 点名的「暂缓但有止损点」。P0 安全隐私硬化方向对，但**金融/政企 PoC 落地前必须先于 PoC 完成这四件**，不能拖。
4. **OTLP/GenAI 标准是生态入场券**。`POST /v1/traces` 已对接，但 tenant 映射缺失是生态兼容性硬伤。

### 建议的优先级调整

硬化计划六个 P0/P1 顺序基本正确，建议两处补强：

- **P0 规模压测**：专门对标 Langfuse 公开的 ClickHouse 吞吐数字（他们 blog 有具体 span/s 数据），把 yiTrace 的「自研引擎 vs ClickHouse」做成一张能拿出来讲的对比表。比单纯跑自己的 100k/1M spans 更有说服力。
- **P0 安全隐私**：OTLP tenant 映射应单列——它是生态兼容性硬伤，不是普通「待补优化」。

## 5. 待深入方向（后续可选）

- 拉一份 Langfuse/Phoenix 的真实 ClickHouse / SQLite schema，对照 yiTrace 的段存储列定义，找字段覆盖差距。
- 真实安装跑一遍 Langfuse v3 自托管，记录它的 trace→score→session 数据模型，对照 yiTrace 的折叠/annotation/Golden Path。
- 跟踪 `open-telemetry/semantic-conventions-genai` 的 agentic systems 提案，等它定稿时同步 yiTrace 的 OTLP 端点。
- 把「进图过滤 vs post-filter」的召回数字（yiTrace 已有 1% 选择性 0.17 vs 1.00 的实测）做成对外可讲的素材。

## 6. 真实定位：Agent framework 的嵌入式 TraceDB

> 本节是整篇调研里对 yiTrace 战略影响最大的一节。它推翻了「对标 Langfuse/Phoenix SaaS」的默认假设，把 yiTrace 重新定位到「对标 SQLite/DuckDB 在 agent 领域的位置」。

### 6.1 不用 ClickHouse 的四条路

ClickHouse 只是「LLM trace SaaS 那一派」的选择，不是唯一选择。市场真实存在的「不用 ClickHouse」路线：

| 路线 | 代表 | 特点 | 适合谁 |
|---|---|---|---|
| **嵌入式 OLAP（DuckDB + Parquet）** | PostHog（DuckDB + ClickHouse 都用）、社区 SRE 实践 | 进程内、无 server、直接查 Parquet | 单机/本地优先 observability |
| **嵌入式 ClickHouse（chDB）** | chDB | ClickHouse 引擎做成 in-process 库，对标 DuckDB | 想要 ClickHouse 语义但不要 server |
| **对象存储 + Parquet（无索引）** | Grafana Tempo、Parseable | 故意不做倒排，靠 trace ID + 便宜对象存储 | 大容量、低成本 trace 后端 |
| **自研专用引擎** | Honeycomb（闭源）、**yiTrace** | 当现成 DB 满足不了核心查询模式时 | 有明确差异化查询需求 |

关键信号：PostHog（头部 observability 厂商）博客标题就叫《[DuckDB vs ClickHouse: Why we use both](https://posthog.com/blog/duckdb-vs-clickhouse)》——**头部玩家都不 all-in ClickHouse**。「不用 ClickHouse」≠ 落后，而是部署形态不同。

### 6.2 真实用户画像：coding agent / agent framework 的 trace 痛点

LLM trace SaaS 在卷「云端、给企业团队用」，但 **OpenCode / Hermes / Cursor / Aider 这一类单机/本地 agent 的 trace 需求几乎没人服务好**：

- **OpenCode**：官方缺 trace，社区自己造了第三方插件「OpenCode Observability」（[Reddit](https://www.reddit.com/r/opencodeCLI/comments/1qprqqx/)）。2026-02 OneUptime 专门写了 [《Use Local-First OpenTelemetry Capture for AI Coding Agent Debugging》](https://oneuptime.com/blog/post-2026-02-06-local-otel-ai-coding-agent-debugging/view)——标题里的 **"Local-First"** 直接对上 yiTrace 定位。
- **Hermes（Nous Research）**：官方 GitHub 有 issue [NousResearch/hermes-agent#6741](https://github.com/NousResearch/hermes-agent/issues/6741) 标题就是 *"feat(observability): structured session tracing with start/end timestamps"*，原话："current session schema is **transcript-first rather than trace-first**, making observability harder."——**Hermes 自己承认缺 trace-first 的结构化追踪**。4 个月 175K stars，300+ tool calls 才完成一个目标（Reddit r/LocalLLM 实测），没有结构化 trace 调试是噩梦。
- **Cursor Debug Mode**：本质是「agent 注入 runtime log 看自己」（[官方](https://cursor.com/blog/debug-mode)），是单次 bug 复现的临时方案，**不是持久化 trace 存储 + 跨 session 检索**。跨多轮/多 session 的回溯它解决不了。
- AugmentCode、Dynatrace「AI coding agent monitoring」、VS Code OTel debug 教程——全在讲同一件事：**coding agent 是黑盒，开发者迫切要看里面的 trace**。

### 6.3 yiTrace 的真实卡位

> **yiTrace = Agent framework 的嵌入式 TraceDB。像 SQLite 之于应用数据库，yiTrace 之于 agent trace——进程内、本地优先、跨 session 可检索、崩溃不丢。**

这个定位下：
- **DuckDB + Parquet** 是同类竞品（嵌入式 OLAP），但 DuckDB 不懂 trace 折叠、不懂中文 BM25、不懂向量 ANN、不懂确定性 event_id——这些正是 yiTrace 的护城河。
- **chDB** 是潜在竞品，但它本质还是 ClickHouse 内核，不是 trace-aware 的。
- **没有任何一家**在做「agent-native 的嵌入式 trace DB」。这是蓝海。

### 6.4 叙事重心调整建议

| 当前叙事（建议降级） | 新叙事（建议升级） |
|---|---|
| 「自研分布式 TraceDB 集群」 | **「Agent framework 的嵌入式 TraceDB」** |
| 对标 Langfuse/Phoenix SaaS | 对标 SQLite/DuckDB 在 agent 领域的位置 |
| gateway / route table / replication 当主打 | gateway 当升级路径，嵌入式 DB 当主打 |
| 「中文检索 + 向量是一等公民」（技术特性） | **「coding agent 本地能搜上周那条 prompt」（用户场景）** |

可做的产品动作（属硬化阶段允许的「为现有功能补验收」，非新增功能）：
1. 写一个 OpenCode 的 trace 导出插件示例——社区已在自造 observability 插件，yiTrace 给现成方案是最直接的用户获取。
2. 写一个 Hermes transcript → yiTrace 的 importer——issue #6741 就是现成需求单。
3. README 首屏换故事：不是「分布式 TraceDB」，而是「你跑了一个 coding agent 一晚上，明天早上想知道它到底干了什么、为什么那么决策——yiTrace 让你搜、回放、定位」。

## 7. 分布式需求重估（基于 §6 定位调整）

### 7.1 结论

**对于「Agent framework 嵌入式 TraceDB」这个主定位，分布式不是核心需求，是升级路径。** 当前代码库和文档对分布式投入的权重，明显高于它在新定位下的实际价值，需要降级。

### 7.2 不同用户画像的需求分层

| 用户画像 | trace 量级 | 是否需要分布式 | 现在用什么 |
|---|---|---|---|
| 单开发者本机 coding agent（OpenCode/Cursor/Aider） | 单进程，每 run 几百~几千 span | **不需要** | plain text log、transcript |
| 单机长跑自改进 agent（Hermes） | 单进程，跨 session 累积 | **不需要**（需要跨 session 检索 + 不丢） | transcript-first 文件 |
| 小团队想聚合多开发者 trace | 多进程，单机或小集群 | 弱需要（更像"上传聚合"，非 sharded cluster） | 自己搭 Langfuse |
| 企业级多租户 SaaS | 大集群，高吞吐 | 强需要 | ClickHouse 拼装栈 |

**前三类是 yiTrace 新定位的真实用户，它们都不需要 sharded cluster。** 第四类是 Langfuse/Phoenix 的主场，yiTrace 不该去那里打。

### 7.3 关键判断

- **嵌入式定位 = 单进程为主**。SQLite 模式：一个 data dir、一个写者、崩溃不丢。yiTrace 已经有这个（`.yitrace.lock`、WAL、GC log）。
- **团队/企业聚合 ≠ sharded cluster**。真实形态更像「每个 agent 本地 yiTrace + 可选 sync 到中心」——是 SQLite/CRDT 模型，不是 ClickHouse 分片模型。当前的 gateway/route table/replication/snapshot lease 在这个形态里是过重的。
- **"可分片演进"作为升级路径保留即可，不作为主打**。CURRENT_STATE 里"可分片演进的 AI Agent TraceDB"这一定义，建议改为"本地优先、可聚合的 AI Agent TraceDB"。

### 7.4 对在途工作的影响（重要）

当前分支 `codex/yitrace-distributed-upgrade` 有大量分布式改动未提交：

| 文件 | 状态 | 处置建议 |
|---|---|---|
| `remote_gateway_server.rs` | 未跟踪 | 保留代码，但降级为「升级路径」示例，不进 README 首屏 |
| `distributed_chaos_eval.rs` | 未跟踪 | 保留作为回归 eval，不扩投入 |
| `gateway_server.rs` example | 未跟踪 | 保留 |
| `risk_eval_matrix.rs` | 未跟踪 | 保留（含分布式写安全合同测仍有价值） |
| `distributed_process_eval.rs` | 已修改 | 保留 |

**不是删代码，是停止继续往分布式投入，并把叙事权重移走。** 已落地的分布式代码作为"升级路径"留着没坏处，但：
- README / CURRENT_STATE 不再把 gateway/replication 当主打能力。
- 硬化计划的 P1「分布式长时间故障注入」优先级可下调，让位给嵌入式定位的验收（OpenCode/Hermes 接入示例、跨 session 检索的真实场景验收）。
- 不再以「完整托管集群」为目标补自动 failover / fencing / 后台 route watcher。

### 7.5 仍要保留分布式能力的理由

- 不删代码、不砍 eval，因为它证明「自研引擎也能走分布式数据路径」，是对「自研 vs ClickHouse」质疑的有力反驳。
- 企业/团队聚合是真实（弱）需求，留作升级路径。
- chaos eval 是引擎正确性的副产品承重测试，本身有价值。
