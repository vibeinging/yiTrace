# 附录 D：第二轮平台层设计（按新约束：完整平台/中小单机/双轨摄入）

> 4 路并行 agent 产出：平台层 / 双轨摄入 / 中小单机引擎再决策+SKU / 语义召回可行性。

# 完整可观测平台层

I now have a solid, verified competitive baseline. Producing the platform layer design.

---

# 平台层架构设计（第二轮）— 单机私有化 · 原生中文 · 企业多租户

> 范围说明：本文只设计**平台层**（UI + 评估 + 告警 + 仪表盘 + 项目管理 + RBAC + Prompt/Playground）。底座 yiTrace 引擎（LSM + merge-on-read + 倒排 + 区间编码树 + 语义召回）在第一轮已定，此处只定义平台层**调用引擎的接口契约**，不重述引擎。工时单位为「人周」，团队按 1 个全栈 Web 团队（3-4 人 React/TS）+ 1 个平台后端团队（2-3 人 Rust/服务层）+ 1 个评估/算法人（共享算法团队）估算。

---

## 0. 竞品功能基线核查（决定"做不做"的依据）

四家对标产品的**真实**功能边界（2026 联网核查结论，含不确定标注）：

| 能力 | Langfuse（ClickHouse 已于 2026-01 收购，承诺保持 MIT 全平台自托管） | LangSmith | Arize Phoenix（OSS 无 feature gate） | 对我们的含义 |
|---|---|---|---|---|
| Trace 浏览/瀑布/span 详情 | ✅ 全开源 | ✅ | ✅ | **桌面赌注**，必须做且做到中文体验更好 |
| 多模态预览（图/PDF/音频） | ✅（S3 直传） | ✅（playground/queue/dataset 全支持） | ✅ | v1 做图片+文本 diff，PDF/音频后置 |
| 评估：LLM-as-judge / 规则 / 人工 / pairwise | ✅（2025-06 起评估、playground、标注队列全部转 MIT，不再 gated） | ✅（single-run + pairwise 队列） | ✅ | 必须做，是第二护城河 |
| RAGAS 类 RAG 指标 | 通过集成（Langfuse 自身不内建 RAGAS，靠 cookbook 接） | 自定义 evaluator | Phoenix **内建** Relevance/QA/Hallucination evaluator | 我们内建中文 RAG 指标=差异化 |
| 数据集 / 实验 / A/B | ✅ | ✅（UI 内建一站式） | ✅（dataset 版本化） | v1 做数据集+实验对比 |
| 仪表盘（cost/latency/token/error） | ✅ 内建 Latency/Cost dashboard + 自定义仪表盘（多级聚合查询引擎） | ✅ | ✅ | 必须做，引擎侧已有聚合能力可直供 |
| 告警 | ✅「dashboards + automated alerts」（但成熟度有限，偏阈值） | ✅ 实时打分驱动质量漂移 | 偏 batch eval | **异常检测**是我们可超越点 |
| Sessions / Users / 线程 | ✅ sessions + userId 维度 | ✅ | ✅ | 引擎侧 thread_id 倒排已直接支撑 |
| RBAC / 审计 / SSO / SCIM | **企业 gated**：project-RBAC、audit log、SCIM、数据保留、UI 定制、server-side masking 都在企业版（$2,499/mo）；org-RBAC 与 SSO 在 OSS/Teams | 企业 | 自托管开源 | 这恰是我们**默认就送**的卖点（私有化客户最在意） |
| Prompt 管理 / Playground | ✅ 全开源 | ✅（2026-04 起 playground 改动可直接回灌运行中 agent） | 有 prompt playground | 我们**后置**（见 ⑦） |

**关键战略结论**：
1. **对手已"开源全平台"**（ClickHouse + Langfuse MIT 全功能、Phoenix 零 gate）。靠"功能清单对齐"赢不了——我们的差异化必须压在 **(a) 原生中文（分词/中文 RAG 指标/中文 judge prompt 模板）、(b) 语义 trace 召回（向量检索找相似 trace/根因聚类）、(c) 单机私有化开箱即用且企业治理默认不收费、(d) 数据飞轮原语**。
2. 平台层**不要从零造轮子**的地方：仪表盘渲染（评估嵌 vs 自研，见 ④）、告警通道（用现成 webhook/IM 网关）。**必须自研**的地方：trace 浏览器（中文 + 引擎深耦合，嵌不进 Grafana）、评估编排、RBAC（要默认送、深植多租户）。

---

## 平台与引擎的统一接口契约（所有模块共享，先定这层）

平台层通过一个内部 **Platform Query/Command Gateway**（Rust 服务，gRPC + REST）对接 yiTrace，禁止 UI 直接打引擎。契约分四类：

```
QueryAPI（读）
  q.searchTraces(filter, sort, page)            → trace 摘要列表（走倒排 + 列段扫描）
  q.getTraceTree(traceId)                        → 区间编码[pre,post] 物化树 + span 节点
  q.getSpan(spanId, fields[])                    → span 详情（大字段晚物化，按需拉 input/output)
  q.getThread(threadId)                          → thread_id 倒排重建会话序列
  q.aggregate(metric, groupBy[], timeRange, filter) → 仪表盘聚合（引擎侧列式聚合下推）
  q.semanticSearch(vector|text, topK, filter)    → 语义 trace 召回（差异化，引擎向量索引）
  q.scanActiveTraces(filter)                     → MemTable 直查运行中 span（活 trace）
CommandAPI（写回，飞轮）
  c.attachScore(targetRef, score, source)        → 评估/标注分数回写（deletion/upgrade vector 机制）
  c.tagTrace(traceId, tags[])                     → 标签/标注
  c.addToDataset(datasetId, exampleRef)          → 从 trace 抽样建数据集
SubscribeAPI（流，告警/活 trace）
  s.tailEvents(filter)                           → 增量事件流（活 trace 监控 / 实时告警喂数据）
MetaAPI（治理，平台自有元数据库，不进引擎）
  小 Postgres：tenant/project/user/role/dataset 定义/eval 配置/alert 规则/audit log
```

**架构要点**：仿 SmithDB「Rust 引擎 + 小 Postgres」。**trace 海量数据进 yiTrace；平台元数据（租户、权限、配置、数据集定义、告警规则、审计）进一个内嵌 PostgreSQL（信创场景换 openGauss——你们本来就是 openGauss 内核团队，零成本）**。评估分数/标注既写 Postgres（可查可改）又通过 `attachScore` 回灌引擎（让 trace 列表可按分数过滤排序，走 merge-on-read）。

---

## ① Trace 浏览器（桌面赌注 · 最高优先级）

**功能范围**：trace 列表（倒排搜索 + 多维过滤）→ trace 详情页（树视图 / 瀑布时间线 / span 详情面板）→ span 输入输出查看 → 两条 trace/span 的 input-output diff → 多模态预览。

| 子功能 | v1 必备 | 后置 | 说明 |
|---|---|---|---|
| trace 列表 + 过滤（时间/模型/状态/tag/用户/延迟/成本/分数） | ✅ | | 直接消费 `q.searchTraces`，引擎倒排已支撑 |
| 树视图 + 瀑布时间线（span 嵌套、耗时条、关键路径高亮） | ✅ | | 消费 `q.getTraceTree` 的 [pre,post] 物化区间，前端只渲染不算树 |
| span 详情（input/output/metadata/token/cost/latency/error/工具调用） | ✅ | | 大字段走 `q.getSpan(fields)` 按需懒加载，避免拉爆前端 |
| 文本 input/output diff（两 span 或两 trace 对比） | ✅ | | 前端 diff（jsdiff 类），中文按字/词分词 diff（接引擎分词器更佳） |
| 图片预览 | ✅ | | 多模态第一优先级 |
| PDF / 音频 / 视频预览 | | ✅ | 对齐 LangSmith/Phoenix，但量小可后置 |
| **语义"找相似 trace"**（选中一条 → 召回相似/同类根因 trace） | ✅（差异化） | | 消费 `q.semanticSearch`，这是别人没有的桌面级能力 |
| **活 trace 实时跟随**（运行中 trace 自动刷新瀑布） | | ✅（K3） | 消费 `s.tailEvents` + `q.scanActiveTraces`；中小规模增量向量成本可控但先做只读跟随，语义召回对活 trace 后置 |
| 线程/会话视图入口（跳转到 ⑤） | ✅ | | thread_id 关联 |

**build vs 用开源**：**全自研 React 前端**。理由：(a) trace 树/瀑布与引擎 [pre,post] 编码、晚物化、语义召回深耦合，嵌 Grafana/任何通用工具都做不出来；(b) 中文渲染、中文 diff、中文搜索高亮需要细控；(c) 这是桌面赌注，体验必须自己掌握。技术栈建议 React + TS + TanStack Table/Query + 虚拟滚动（trace 可能上万 span，必须虚拟化）。

**接口**：`q.searchTraces` / `q.getTraceTree` / `q.getSpan` / `q.semanticSearch` / `s.tailEvents`。

**工时**：列表+过滤 3w；树/瀑布渲染（虚拟化是难点）4w；span 详情+懒加载 2w；文本 diff（含中文）2w；图片预览 1w；语义找相似 trace（前端+联调）2w；活 trace 只读跟随（后置）2w。**v1 ≈ 14 人周，后置 ≈ 4 人周。**

---

## ② 评估框架（第二护城河 · 飞轮入口）

**功能范围**：评估器类型（LLM-as-judge / 规则启发式 / RAGAS 类 RAG 指标 / 人工标注队列 / pairwise A/B）+ 数据集（dataset-as-tests）+ 实验运行与对比。

| 子功能 | v1 必备 | 后置 | 说明 |
|---|---|---|---|
| 规则/启发式评估器（正则、JSON schema 校验、包含/精确匹配、长度、关键词） | ✅ | | 最便宜、确定性高，先做 |
| LLM-as-judge（可配 judge 模型、打分 prompt 模板、rubric） | ✅ | | **内置中文 judge prompt 模板库**=差异化 |
| **中文 RAG 指标（RAGAS 类：忠实度/上下文相关性/答案相关性/上下文召回，中文 prompt 调优）** | ✅（差异化） | | Phoenix 内建英文版；我们内建**中文**版是关键卖点 |
| 人工标注队列（single-run 逐条评审 + 自定义 rubric） | ✅ | | 对齐 LangSmith，回写 `c.attachScore` |
| pairwise 标注队列（两 run 并排选优） | | ✅ | A/B 人工裁判，量小可二期 |
| 数据集管理（建集、版本化、从 trace 一键抽样 `c.addToDataset`） | ✅ | | dataset-as-tests 基础 |
| 实验运行（拿数据集跑当前 prompt/链，自动评分） | ✅ | | 离线批量，消费数据集 + 评估器 |
| 实验对比视图（两次实验 metric diff、回归高亮、逐样本下钻） | ✅ | | "质量回归"靠它 + ③ 告警闭环 |
| 在线评估（生产流量实时采样打分，喂质量漂移告警） | | ✅ | 消费 `s.tailEvents`，对齐 LangSmith 实时打分 |
| 自定义评估器（Python/JS 用户自带逻辑） | | ✅ | 企业会要，但二期 |

**build vs 用开源**：评估**编排自研**（调度、采样、回写、对比视图）；RAG 指标算法**借鉴 RAGAS 开源思路但用中文重写 prompt**（不直接绑 RAGAS Python 依赖，避免私有化环境装包地狱，用我们自己的 judge 调用链实现）。judge 模型调用走客户自己的 LLM 网关（私有化客户多用内网模型，需可配 base_url/openai-compat）。

**接口**：读 `q.searchTraces`/`q.getSpan` 取被评对象；写 `c.attachScore`（分数回灌引擎，使 trace 列表可按分数过滤）+ `c.addToDataset`；元数据（数据集定义、评估器配置、实验记录）入 Postgres。

**工时**：规则评估器 2w；LLM-judge + 中文模板库 3w；中文 RAG 指标（算法+调优） 4w；人工标注队列 3w；数据集管理 3w；实验运行+对比视图 4w；在线评估（后置）3w；pairwise（后置）2w。**v1 ≈ 19 人周，后置 ≈ 5 人周。** （评估是平台层第二重的模块，工时实事求是地高。）

---

## ③ 告警与异常检测（我们可超越对手的点）

**功能范围**：监控错误率 / 延迟 / 成本突变 / token 用量 / 质量回归（来自 ②），触发通知。

| 子功能 | v1 必备 | 后置 | 说明 |
|---|---|---|---|
| 阈值告警（error rate / p95 latency / cost / token 超阈，按 model/tenant/project 维度） | ✅ | | 对齐 Langfuse「automated alerts」基线，消费 `q.aggregate` 定时轮询 |
| 通知通道（Webhook + 企业微信/钉钉/飞书 + 邮件） | ✅ | | **中国 IM 渠道是刚需**，Langfuse 没原生钉钉/企微 |
| 质量回归告警（实验/在线评估分数跌破基线） | ✅ | | 与 ② 联动，私有化客户最关心 |
| **异常检测（成本/延迟/错误率突变，无需手设阈值）** | ✅ 轻量统计版（同比/环比 + 3σ/MAD/EWMA） | ✅ 算法增强版（季节性、Prophet 类） | 比对手成熟，但 v1 先上轻量统计，避免过度工程 |
| 告警分组/去重/静默/升级 | | ✅ | 二期治理 |
| 异常根因辅助（语义聚类相似失败 trace） | | ✅（差异化） | 复用 ① 语义召回，把同类报错 trace 聚一起 |

**build vs 用开源**：告警规则引擎**自研**（轻量，定时跑 `q.aggregate` 比对规则），不引入 Prometheus/Alertmanager（单机私有化不值得拖一套 TSDB）。通知通道**用开源 SDK / HTTP**（企微钉钉飞书 webhook）。异常检测统计算法自研（几百行，3σ/EWMA/MAD），季节性/Prophet 后置。

**接口**：`q.aggregate`（轮询指标）+ `s.tailEvents`（实时流式触发，后置）；规则存 Postgres；与 ② 的实验分数表联动。

**工时**：阈值告警引擎 3w；通知通道（含国内 IM） 2w；质量回归联动 1w；轻量异常检测 3w；分组去重静默（后置）2w；语义根因聚类（后置）2w。**v1 ≈ 9 人周，后置 ≈ 4 人周。**

---

## ④ 仪表盘（cost / latency / token / error，多维）

**功能范围**：内建仪表盘（成本/延迟/token/错误率，按 model/tenant/project/time 维度切片）+ 自定义仪表盘。

| 子功能 | v1 必备 | 后置 | 说明 |
|---|---|---|---|
| 内建仪表盘：Cost（按 model/tenant/time）、Latency（p50/p95/p99）、Token、Error rate | ✅ | | 引擎列式聚合下推 `q.aggregate` 直供，性能好 |
| 时间范围/维度筛选器 + 下钻到 trace 列表 | ✅ | | 仪表盘点一下 → 跳 ① 过滤后的 trace 列表 |
| 用户/会话维度统计（Top 用户成本、活跃会话） | ✅ | | thread_id/userId 倒排支撑 |
| 自定义仪表盘（用户拖拽 metric + groupBy 组合） | | ✅ | 对齐 Langfuse 自定义仪表盘，但 v1 先给固定看板 |
| 看板导出/分享/定时报表 | | ✅ | 二期 |

**build vs 用开源——这里最值得讨论嵌 Grafana**：
- **结论：v1 自研轻量图表（ECharts/Recharts），不嵌 Grafana。** 理由：(a) 数据源在 yiTrace，Grafana 需要写一个数据源插件去对接 `q.aggregate`，工作量不比自研画几个固定看板少；(b) 自定义仪表盘 v1 不做，固定看板自研更快更可控；(c) 嵌 Grafana 会引入额外进程/许可证（Grafana AGPL 顾虑）/中文化/RBAC 打通成本，与"单机轻量私有化"理念冲突；(d) 下钻联动到 ① trace 浏览器，自研才能无缝。
- **若后期客户强要自定义仪表盘**，再评估嵌 Grafana 或做拖拽 builder。

**接口**：`q.aggregate`（核心，引擎侧聚合下推是性能关键，避免平台层拉明细自己算）。

**工时**：4 个内建看板 + 筛选器 3w；下钻联动 1w；用户/会话统计 1w；自定义仪表盘（后置）4w；导出报表（后置）2w。**v1 ≈ 5 人周，后置 ≈ 6 人周。**

---

## ⑤ 项目 / 会话 / 线程管理 + 搜索

**功能范围**：项目（顶层隔离单元）→ 会话/线程（多轮对话重建）→ 全局搜索。

| 子功能 | v1 必备 | 后置 | 说明 |
|---|---|---|---|
| 项目管理（创建/归档、项目级配置、与租户绑定） | ✅ | | 元数据进 Postgres，引擎查询带 project 过滤 |
| 会话/线程视图（按 thread_id 重建多轮序列、串起跨 trace 的对话） | ✅ | | 直接消费 `q.getThread`（引擎 thread_id 倒排已做） |
| 用户视图（按 userId 聚合该用户所有会话/成本） | ✅ | | userId 维度 |
| 全局搜索（trace/span 内容、metadata、tag；**中文分词搜索**） | ✅（差异化） | | 引擎**原生中文分词倒排**=核心差异化，远超对手英文 tokenizer |
| 语义搜索（自然语言找 trace） | ✅（差异化） | | `q.semanticSearch`，"找所有用户抱怨退款的对话"这类 |
| 保存的搜索/视图、收藏 | | ✅ | 二期便利功能 |

**build vs 用开源**：全自研。搜索能力**完全依赖引擎**（中文分词倒排 + 向量），平台层只是 UI + 查询编排。这是把第一轮引擎差异化（中文分词、语义召回）**变现到产品面**的关键模块。

**接口**：`q.getThread` / `q.searchTraces`（中文分词）/ `q.semanticSearch`；项目元数据 Postgres。

**工时**：项目管理 2w；会话/线程视图 3w；用户视图 1w；中文+语义搜索 UI 与联调 3w；保存搜索（后置）1w。**v1 ≈ 9 人周，后置 ≈ 1 人周。**

---

## ⑥ RBAC / 多租户管理控制台 + 审计（私有化客户的硬需求 · 我们默认送）

**功能范围**：组织 → 租户 → 项目三级隔离 + 角色权限 + 用户/团队管理 + SSO + 审计日志。

| 子功能 | v1 必备 | 后置 | 说明 |
|---|---|---|---|
| 多租户隔离（org/tenant/project 三级，数据查询强制带租户过滤） | ✅ | | **引擎所有查询必须带 tenant_id 作为分区/过滤维度**——需引擎侧确认隔离边界（见红队提醒） |
| 角色与权限（预置 Owner/Admin/Member/Viewer + 项目级 RBAC） | ✅ | | **对手把 project-RBAC 放企业版 $2,499/mo，我们默认送**=私有化卖点 |
| 用户/团队管理控制台（邀请、停用、改角色） | ✅ | | Postgres |
| SSO（OIDC / SAML / LDAP，对接客户企业 IdP） | ✅（OIDC 必备） | SAML/LDAP 视客户 | 私有化金融政企几乎必问，OIDC 先行 |
| 审计日志（谁在何时对什么做了什么：查看 trace、改配置、导数据） | ✅ | | **对手企业版才有，我们默认送**；合规刚需 |
| SCIM 自动 provisioning | | ✅ | 大型政企才要，二期 |
| 数据脱敏 / 字段级权限（敏感 input/output 打码） | ✅ 基础脱敏 | ✅ 字段级 | 私有化 + 信创场景对 PII 敏感，v1 先做规则脱敏 |
| 数据保留策略（按租户配 TTL） | ✅ | | 引擎侧分层 compaction 配合，平台层配规则 |

**build vs 用开源**：**自研为主**。RBAC/多租户必须深植引擎查询路径（每个 `q.*` 都带租户上下文），无法外挂。SSO **用开源库**（如基于 OIDC/SAML 标准库，Rust 侧或 BFF 侧集成），不自己写协议。审计日志自研（写 Postgres，所有 Command/敏感 Query 落审计）。

**接口**：所有 `q.*`/`c.*` 调用强制注入 `tenant_ctx`；引擎需保证租户级数据隔离与过滤下推（**这是与引擎团队的硬接口约定，需第一轮引擎确认 tenant 是否为一等分区维度**）。治理元数据全在 Postgres/openGauss。

**工时**：多租户隔离 + 租户上下文贯穿 4w；RBAC（角色/项目级权限）4w；用户团队控制台 2w；OIDC SSO 3w；审计日志 2w；基础脱敏 2w；数据保留策略 1w；SAML/LDAP（后置）3w；SCIM（后置）2w；字段级权限（后置）2w。**v1 ≈ 18 人周，后置 ≈ 7 人周。**

> 注：⑥ 是被最容易低估、但私有化企业客户**采购评分表上的硬项**。把对手的企业版治理功能**默认免费**是清晰的差异化定位，但工时不能省。

---

## ⑦ Prompt 管理 / Playground（可后置）

**功能范围**：prompt 版本管理 + playground 试跑 + 接回运行中 agent。

| 子功能 | v1 必备 | 后置 | 说明 |
|---|---|---|---|
| Prompt 版本库（存储、版本、标签、回滚） | | ✅ | 对齐三家，但非可观测核心，后置 |
| Playground（试跑 prompt、调模型参数、对比版本输出） | | ✅ | LangSmith 强项，但我们核心是 trace+eval |
| Playground 改动回灌运行中 agent | | ✅（且依赖自家 agent 产品 SDK 深集成） | LangSmith 2026-04 才有，仅对自家 agent 可行 |

**理由**：整个模块**后置到 v2**。可观测 + 评估 + 治理才是"客户主动要求部署"的本体；Prompt/Playground 是锦上添花，且与自家 agent 产品 SDK 深集成的那部分价值更高、可作为"自家底座"专属差异化在 v2 做。

**工时**：prompt 版本库 3w；playground 4w；回灌 agent 3w。**全部后置 ≈ 10 人周。**

---

## 跨模块基础设施（平台层地基，不可省）

| 项 | v1 | 工时 | 说明 |
|---|---|---|---|
| Platform Gateway 服务（gRPC/REST，租户上下文注入，对接引擎 + Postgres） | ✅ | 5w | 所有模块的后端入口，最先做 |
| 内嵌元数据库（Postgres / 信创换 openGauss）schema + 迁移框架 | ✅ | 2w | 你们 openGauss 团队零学习成本 |
| 前端框架地基（路由/状态/i18n 中文/组件库/鉴权拦截/虚拟化基建） | ✅ | 4w | 一次性投入 |
| 单机打包与运维（单二进制/Docker Compose 一键部署、备份恢复、健康检查） | ✅ | 4w | **HA 主备红队提醒：v1 做冷备份+恢复脚本；主备热 HA 后置**（中小客户先冷备可接受，金融客户单列） |
| 可观测自身（平台自身日志/指标） | ✅ | 1w | |

**基础设施 v1 ≈ 16 人周。**

---

## 汇总：范围 / 团队 / 路线图

### v1 工时汇总（人周）

| 模块 | v1 人周 | 后置人周 |
|---|---|---|
| ① Trace 浏览器 | 14 | 4 |
| ② 评估框架 | 19 | 5 |
| ③ 告警与异常检测 | 9 | 4 |
| ④ 仪表盘 | 5 | 6 |
| ⑤ 项目/会话/搜索 | 9 | 1 |
| ⑥ RBAC/多租户/审计 | 18 | 7 |
| ⑦ Prompt/Playground | 0 | 10 |
| 跨模块基础设施 | 16 | — |
| **合计** | **90 人周** | **37 人周** |

> 90 人周 ÷（约 6-7 人有效并行）≈ **3.5-4 个月**做出 v1 平台层骨架；但 trace 浏览器虚拟化、评估编排、多租户贯穿这三块有串行依赖与联调，**现实化到 5-6 个月（含联调、引擎接口磨合、中文打磨、私有化部署测试）**。后置 37 人周 ≈ v2 再 2-3 个月。

### 团队配置建议

- **Web 前端 3-4 人**（React/TS）：①④⑤ 重前端，②⑥ 也有大量管理界面。trace 浏览器虚拟化需 1 个资深。
- **平台后端 2-3 人**（Rust）：Gateway、评估编排、告警引擎、RBAC、与引擎接口联调。
- **算法 0.5-1 人**（共享）：中文 RAG 指标、judge 模板、异常检测统计、语义召回联调。
- **1 人** 兼顾打包/私有化部署/信创适配（与底座团队协作）。

### 路线图（含平台层，分三阶段）

**Phase 1 — 可观测 MVP（约 2 个月）**：基础设施 + ① trace 浏览器（树/瀑布/详情/中文搜索）+ ⑤ 会话线程 + ④ 内建仪表盘 + ⑥ 基础多租户/RBAC/审计。→ **能 demo、能让客户把自家/第三方 trace 看起来**，对齐摄入双轨（OTel GenAI/OpenInference/LangSmith-compat）。

**Phase 2 — 评估 + 告警 + 治理完整（约 3 个月）**：② 评估框架全套（规则/judge/中文 RAG/标注队列/数据集/实验对比）+ ③ 告警+轻量异常检测+国内 IM 通道 + ⑥ SSO(OIDC)/脱敏/数据保留 + ① 语义找相似 trace。→ **可售完整平台**，差异化（中文 + 语义召回 + 默认企业治理 + 飞轮原语）全部落地。

**Phase 3 — 增强与自家底座深集成（v2，约 2-3 个月）**：⑦ Prompt/Playground + 在线评估 + 自定义仪表盘 + 活 trace 在线语义召回（K3，量大客户）+ pairwise/语义根因聚类 + SAML/SCIM/字段级权限 + **主备 HA**（金融政企）。

### 红队警告的本轮回应

- **K3（活 trace 在线语义召回成本）**：v1 只做活 trace **只读跟随**，语义召回限离线/历史；在线语义召回后置到 Phase 3，且仅对大客户开。中小规模下增量向量更新成本可控，符合本轮判断。
- **K6（格式 breaking 迁移地雷）**：平台层元数据用 Postgres/openGauss（稳定、有迁移框架）；trace 列存格式的冻结由引擎层负责，平台层**所有数据访问走 Gateway 抽象**，格式变更不穿透到 UI。
- **K7（信创 12-24 月七位数）**：平台层元数据库**直接用 openGauss（你们自己内核）= 零额外信创成本**；前端纯 Web 天然适配国产浏览器；若卖金融政企，信创认证须在 Phase 2 并行启动（单列预算，非平台开发工时内）。
- **HA/备份**：v1 冷备份 + 恢复脚本（基础设施已含）；热主备 HA 后置 Phase 3，对金融客户单独报价。
- **ClickHouse+Langfuse 全开源对手**：功能对齐无法取胜，差异化全压在中文 + 语义召回 + 默认免费企业治理 + 单机开箱 + 飞轮——本设计已把这四点分别落到 ①⑤（中文/语义）、⑥（默认治理）、基础设施（单机开箱）、②（飞轮 `addToDataset`/`attachScore`）。
- **框架无关摄入完整度**：属摄入层（非本平台层任务），但平台层 ① 的 trace 模型必须兼容 OTel GenAI/OpenInference/LangSmith schema 三套语义，UI span 详情面板需做字段映射适配层（已在 ① span 详情工时内预留，但需与摄入层团队对齐 schema）。

### 仍不确定 / 需与第一轮引擎团队确认的接口项（诚实标注）

1. **tenant_id 是否为引擎一等分区/过滤维度**（决定 ⑥ 多租户能否过滤下推，影响隔离强度与性能）——⑥ 全部工时建立在"是"的假设上。
2. **`q.aggregate` 聚合下推的维度/算子覆盖**（cost/latency 分位数 p95/p99 是否引擎侧算）——决定 ④ 仪表盘是引擎算还是平台拉明细算，影响性能与 ④ 工时。
3. **`c.attachScore` 回灌分数后，`q.searchTraces` 能否按分数过滤/排序**（merge-on-read 是否支持二次写入字段做过滤）——决定 ② 评估分数能否在 ① trace 列表直接用。
4. **`q.semanticSearch` 的向量来源**（trace 何时/由谁生成 embedding，是摄入时还是查询时）——决定 ①⑤ 语义召回的延迟与活 trace 可用性。

以上四项建议在 Phase 1 启动前与引擎团队签订接口契约，避免平台层返工。

---

来源（竞品基线核查）：
- [ClickHouse 收购 Langfuse（承诺保持 MIT 全平台自托管）](https://clickhouse.com/blog/clickhouse-acquires-langfuse-open-source-llm-observability)
- [Langfuse 企业 gated 功能（project-RBAC/SCIM/审计/数据保留/脱敏在企业版）](https://github.com/orgs/langfuse/discussions/8147)
- [Langfuse 自定义仪表盘与聚合查询引擎](https://langfuse.com/docs/metrics/features/custom-dashboards)
- [LangSmith 标注队列（single-run + pairwise）/数据集/实验/playground 回灌 agent](https://www.langchain.com/langsmith/evaluation)
- [Arize Phoenix 开源零 feature-gate，内建 Relevance/QA/Hallucination evaluator](https://arize.com/docs/phoenix/cookbook/evaluation/evaluate-rag)
- [RAGAS 六项 RAG 指标（忠实度/上下文精度召回/答案相关性）](https://langfuse.com/guides/cookbook/evaluation_of_rag_with_ragas)
---

# 双轨摄入架构

I now have verified facts on all four load-bearing items. Writing the design.

---

# 双轨摄入层设计(yiTrace 摄入层 / 本轮主轴)

> 范围:per-customer 私有化单机部署的可观测平台,摄入层为 60-70% 可售平台提供"入口"。轨 A = 自家 agent 产品深集成;轨 B = 框架无关接第三方。下面给协议矩阵、schema 归一、迁移边界、大 payload/采样/embedding 旁路四块,并在文末给联网核查结论与不确定标注。

---

## 0. 联网核查结论(决定本设计的事实基线,均为 2026-06 现状)

| 事项 | 核查结论 | 对设计的影响 |
|---|---|---|
| OTel GenAI 语义约定状态 | **仍是 Development(实验态)**,且已从主 `semantic-conventions` 仓**拆分到独立的 `semantic-conventions-genai` 仓**;v1.37 用 `gen_ai.input.messages` / `gen_ai.output.messages` **取代了旧的 per-message events**,聊天历史记录方式被重构 | 必须做版本适配层 + 双发(dual-emit)兼容;消息内容捕获方式近一年内已 breaking 变过一次,持续追随成本真实存在(K6 同类风险) |
| OTel 版本切换机制 | 官方提供 `OTEL_SEMCONV_STABILITY_OPT_IN`,`gen_ai_latest_experimental` 可只发最新实验版 | 我们在摄入侧也要按此做"按 schema 版本路由解析",不能写死一套属性名 |
| OpenInference(Arize) | 活跃,**OTel 对齐的补充约定**;Python instrumentor 覆盖广:LangChain、LlamaIndex、DSPy、Haystack、CrewAI、AutoGen AgentChat、PydanticAI、LiteLLM、OpenAI Agents、Claude Agent SDK、Agno、smolagents、Strands、MCP + OpenAI/Anthropic/Mistral/Groq/Google GenAI/Bedrock/VertexAI;JS/TS 覆盖较窄(OpenAI/Anthropic/Bedrock/BeeAI/LangChain.js/Vercel AI SDK/MCP) | 轨 B 的 Python 侧可大幅复用现成 instrumentor;**TS 侧覆盖明显薄**,自研/补齐成本要单列 |
| OpenLLMetry(Traceloop) | 活跃,OTel 之上;覆盖 OpenAI/Anthropic/Cohere/LangChain/Haystack/Pinecone/Qdrant/Weaviate/Chroma 等;语言 Py/TS/Go/Ruby | 与 OpenInference 形成"两套语义约定都得吃"的现实;Go/Ruby 客户走 OpenLLMetry 是补位项 |
| LangSmith 摄入 API | `POST /runs` + `PATCH /runs`(基础,服务端自动算 `trace_id`/`dotted_order`);`POST /runs/multipart`(高吞吐批量,**需客户端自算 `dotted_order`/`trace_id`**,且承载大 payload/二进制 attachment);`dotted_order` 格式 = `YYYYMMDDTHHMMSSffffffZ` + run UUID,父子用 `.` 连接;并已支持 OTLP 入口 `/otel/v1/traces`(自托管全路径 `/api/v1/otel/v1/traces`) | LangSmith-compat 必须**同时**实现两条路径 + multipart attachment + dotted_order 解析;"指 endpoint 迁过来"在协议层可行,但私有字段是地雷(见 §3) |

> 不确定项(已标注,未编造):LangSmith `run_type` 的**完整**枚举官方文档未在公开页完整列出(常见为 `llm/chain/tool/retriever/embedding/prompt/parser`,需以 SDK 源码/`smith-api-ref` 为准核对);OTel GenAI 何时转 Stable **无公开时间表**;multipart attachment 的大小上限/分片细节需抓真实请求样本确认。

---

## 1. 协议矩阵(摄入入口 / 端点设计)

摄入层 = **一个统一接收网关(Rust)** 暴露多协议端点,内部全部归一到 yiTrace 内部事件模型(见 §2)。设计原则:**对外多协议兼容,对内单一规范模型**。

### 1.1 端点总表

| # | 入口 | 传输 | 来源生态 | 复用现成方案 | 优先级 | 团队投入(粗估) |
|---|---|---|---|---|---|---|
| P1 | **OTLP/gRPC** `:4317` | gRPC + protobuf | OTel GenAI、OpenLLMetry、OpenInference(均可 OTLP 导出) | 直接吃 OTLP `TracesData`;复用 `opentelemetry-proto` Rust 绑定 | P0 | 中 |
| P2 | **OTLP/HTTP** `/v1/traces` | HTTP + protobuf/json | 同上(防火墙/网关友好) | 同 P1,多一层 HTTP 编解码 | P0 | 低(共用 P1 解码) |
| P3 | **OpenInference 解析器**(在 P1/P2 之上的 attribute 解析层) | — | Arize 生态、Phoenix 用户 | 不是独立端点,是 OTLP span 上的**属性方言识别**;复用 OpenInference semconv 字典 | P0 | 中 |
| P4 | **LangSmith-compat REST** `/runs`、`/runs/{id}`(PATCH)、`/runs/multipart`、`/otel/v1/traces` | HTTP JSON + multipart | LangChain/LangGraph 既有 LangSmith 用户(迁移主战场) | 自研兼容层;multipart attachment + dotted_order 解析 | P0(迁移卖点) | 高 |
| P5 | **通用 SDK(Py/TS)** → 走 OTLP | OTLP | 自研框架客户、不想接 instrumentor 的客户 | 薄封装 OTel SDK + 我们的 semconv 默认值 | P1 | 中(TS 侧偏重) |
| P6 | **轨 A 富语义私有通道**(自家 agent 产品) | OTLP + 私有扩展属性 `vex.*`,或 protobuf 私有消息 | 自家 AgenticData | 我们控制 SDK,直接埋点 | P0(差异化底座) | 中 |

> 入口层全部落到同一条 **接收队列 → 归一器 → MemTable 写路径**,不为每个协议分叉存储。

### 1.2 各框架 auto-instrumentation 复用程度(轨 B 的真实工作量)

这是"框架无关摄入完整度易被低估"的正面回应。**结论:Python 客户≈80% 可复用现成 instrumentor,TS/Go/Ruby 是缺口区。**

| 框架 | 现成方案 | 复用度 | 缺口 / 我方工作 |
|---|---|---|---|
| LangChain / LangGraph | OpenInference `langchain`、OpenLLMetry、LangSmith 原生 callback | 高(Py);中(TS) | LangGraph 的图节点/state 语义在 OTel 里不规整,需在归一器补 `graph.node`、`graph.edge` 语义 |
| LlamaIndex | OpenInference `llama-index` | 高(Py) | TS 几乎无 → SDK 兜底 |
| AutoGen | OpenInference `autogen-agentchat` | 中 | 多 agent round / handoff 语义在不同版本不稳,需版本探针 |
| Dify | **无一线现成 instrumentor**,Dify 自带 OTLP 导出 / 节点日志 | 低-中 | **重点缺口**:多数靠 Dify 的 OTLP 导出 + 我方 attribute 映射;workflow 节点 → span 映射需自研。建议提供 Dify 插件/webhook 适配 |
| CrewAI / DSPy / Haystack / PydanticAI / OpenAI-Agents | OpenInference 各自 instrumentor | 高(Py) | 验证 + 版本钉版 |
| 自研框架 | 无 | 0 | 走 P5 通用 SDK 或 P1 直发 OTLP |

> 建议:**优先认证 LangChain/LangGraph/LlamaIndex/Dify** 四个(覆盖中国客户主流),其余"best-effort 兼容"。每个 instrumentor 要**钉版本 + 建回归用例**(因上游随框架升级而动,见 §3 持续成本)。

---

## 2. Schema 归一(多方言 → yiTrace 内部模型)

内部模型不绑任何一家方言。**OTLP/OpenInference/LangSmith 三套都映射到下面这张规范表**,落入第一轮已定的行式 MemTable → 列式 segment。

### 2.1 内部规范事件模型(节选关键列)

```
span/run 规范记录:
  span_id        : UUIDv7      // 内部主键,时间有序,直接利好 LSM 时间分层 compaction
  trace_id       : UUIDv7
  parent_span_id : UUIDv7 | null
  pre, post      : int         // 第一轮:读侧 flush 物化的区间编码(trace 树)
  thread_id      : string      // 第一轮:线程倒排 key
  session_id / project_id / tenant_id
  kind           : enum        // LLM/CHAIN/TOOL/RETRIEVER/EMBEDDING/AGENT/RERANKER/GUARDRAIL/UNKNOWN
  name           : string
  start_ns, end_ns
  status         : ok/error;  error_msg
  model_request, model_response : string
  usage_input_tokens, usage_output_tokens, usage_total_tokens : int
  inputs_ref, outputs_ref      : blob 句柄(大字段晚物化,见 §4)
  attributes_norm: 归一后的 KV(已提升的常用维度)
  raw_attrs      : 原始属性兜底(JSON/列式 map,无损保留)
  source_dialect : enum(otel_genai@ver / openinference / openllmetry / langsmith / vex_native)
  schema_ver     : string      // 关键:记录来源 schema 版本,支撑 OTel 实验态追随
```

`raw_attrs` 是**抗 breaking 变更的护城河**:任何方言新增/改名的属性,先无损落 `raw_attrs`,归一器升级后可**回填**到 `attributes_norm`,老数据不需重摄入。

### 2.2 三套 ID 体系映射

| 概念 | OTel | LangSmith | yiTrace 内部 |
|---|---|---|---|
| 单元 ID | trace 16B + span 8B(hex) | run UUID | UUIDv7(由来源 ID 确定性派生或新生成) |
| 顺序 | span start_time + 父子引用 | `dotted_order`(时间戳+UUID,`.` 连接) | `pre/post` 区间编码 + UUIDv7 时间序 |
| 树关系 | `parent_span_id` | `parent_run_id` + dotted_order 前缀 | `parent_span_id` + 写侧邻接表 |
| 会话/线程 | `gen_ai.conversation.id` / `session.id` | `session_id`/`session_name` + metadata thread | `session_id` + `thread_id` 倒排 |

**ID 派生策略**:外部 ID 非 UUIDv7,我们**保留外部 ID 原文于 `raw_attrs.ext_id`**,内部主键用 UUIDv7(保时间有序利于 LSM)。映射表 `ext_id → span_id` 进倒排,保证 PATCH/multipart 后续更新能命中(LangSmith 是两段式 create+update,**必须按 ext_id 去重/合并**,走第一轮的 merge-on-read upgrade vector)。

`dotted_order` → `pre/post`:multipart 直接给了 dotted_order 前缀树,**可在摄入时直接构造邻接表**,无需等读侧全量 flush 才能建树(对"指 endpoint 迁过来"的批量回灌很关键)。

### 2.3 语义字段映射(gen_ai.* ↔ run_type ↔ 内部 kind)

| 内部 kind | OTel GenAI | OpenInference | LangSmith run_type |
|---|---|---|---|
| LLM | `gen_ai.operation.name=chat/text_completion` | `openinference.span.kind=LLM` | `llm` |
| CHAIN | (无原生,框架 span) | `CHAIN` | `chain` |
| TOOL | `execute_tool` / `gen_ai.tool.*` | `TOOL` | `tool` |
| RETRIEVER | (DB semconv) | `RETRIEVER` | `retriever` |
| EMBEDDING | `embeddings` | `EMBEDDING` | `embedding` |
| AGENT | `invoke_agent` / agent spans | `AGENT` | (chain + metadata) |
| RERANKER | — | `RERANKER` | — |

属性级映射(示例):

| 内部 | OTel GenAI(注意 v1.37 后) | OpenInference | LangSmith |
|---|---|---|---|
| model_request | `gen_ai.request.model` | `llm.model_name` | `extra.invocation_params.model` |
| usage_input_tokens | `gen_ai.usage.input_tokens` | `llm.token_count.prompt` | `outputs`/usage 内 |
| inputs(消息) | `gen_ai.input.messages`(v1.37 新,**替代旧 events**) | `llm.input_messages.*` | `inputs` |
| outputs | `gen_ai.output.messages` | `llm.output_messages.*` | `outputs` |
| temperature 等 | `gen_ai.request.temperature` | `llm.invocation_parameters` | `extra.invocation_params` |

> **归一器按 `source_dialect + schema_ver` 选映射表**。OTel GenAI 由于 v1.37 改了消息记录方式,映射表必须**同时认识** `gen_ai.input.messages`(新)与旧 per-message event 两种形态 → 这正是 OTel 实验态追随成本的落点。

### 2.4 轨 A 富语义扩展(差异化)

轨 A 在内部模型上额外开 `vex.*` 命名空间,捕获第三方框架拿不到的语义,**且这些维度直接喂 §0 提到的语义 trace 召回 / 数据飞轮**:

- `vex.thought`(推理链文本)、`vex.decision`(分支决策 + 候选 + 选中理由)、`vex.tool.intent`(为何调此工具)、`vex.context.injected`(注入的上下文片段引用)、`vex.reflection`、`vex.cost.usd`。
- 这些字段**原生进倒排 + 向量旁路**(§4),使"语义 trace 召回"在轨 A 上是一等公民;轨 B 只能从 inputs/outputs 文本反推,质量较低 → 形成**轨 A 独占的可售差异化**。

---

## 3. "指一下 endpoint 就迁过来":能做什么、边界在哪

### 3.1 协议层确实可做(真卖点)

- **OTLP 迁移**:客户把 OTel exporter 的 endpoint 从原后端改到我们的 `:4317`/`/v1/traces`,即时生效,零代码改动(只要他们已用 OpenInference/OpenLLMetry 之一)。这是最干净的一类。
- **LangSmith 迁移**:实现 `/runs`+`/runs/multipart`+`/otel/v1/traces` 兼容后,客户把 `LANGSMITH_ENDPOINT`(或 `LANGCHAIN_ENDPOINT`)指向我们即可。dotted_order/multipart attachment 都按 §0 格式解析。

### 3.2 边界与地雷(必须对客户和内部如实标注)

1. **LangSmith 私有格式会漂移**。`dotted_order`、multipart 边界、`extra`/`metadata` 内的私有结构、feedback/dataset/annotation queue 等**非 trace 的周边 API** 不在 OTLP 标准内,LangChain 可随时改。**承诺范围只到"trace 摄入兼容",不承诺 100% LangSmith 平台 API 兼容**。周边能力(评估、标注)我们用自有平台层做,不做 LangSmith API 镜像。
2. **OTel GenAI 未稳定(Development)+ 已拆仓 + v1.37 已发生过 breaking**。持续追随是**长期运维成本而非一次性**:每次上游版本变,归一器映射表要更新、回归用例要补。建议设**专人/季度跟踪** semconv-genai 仓 release,并用 `schema_ver` + `raw_attrs` 兜底使老数据不受影响。
3. **历史数据回灌 ≠ 改 endpoint**。"指 endpoint"只迁**新流量**。存量历史 trace 要从原系统(LangSmith/Phoenix)**导出再批量回灌**,需单独做导入工具(读 LangSmith export / Phoenix parquet → multipart 批量入)。这是项目交付项,要单列工时。
4. **instrumentor 版本耦合**。轨 B 复用的 OpenInference/OpenLLMetry instrumentor 随上游框架升级而动,客户升级 LangChain 可能令属性变化 → 我们的钉版 + 回归用例是兜底,但**不能保证未认证版本零丢字段**。
5. **TS/Go/Ruby 覆盖弱**(§0):若客户是 TS/Go 重度栈,迁移完整度低于 Python,需提前在售前识别。

---

## 4. 大 payload / 多模态抽离、采样、embedding 旁路

### 4.1 大字段晚物化与多模态抽离(对接第一轮"大字段晚物化")

- **入口即抽离**:归一时,`inputs/outputs`、图片/音频/文件 attachment **不进主 segment**,落 `inputs_ref/outputs_ref` 句柄。主记录只留元信息(长度、mime、hash、前 N 字符预览)。
- **存储**:单机部署 → blob 落本地 NVMe 的**内容寻址存储(CAS,按 sha256)**,天然去重(相同 prompt/系统提示大量重复时省盘明显)。**不引对象存储**(契合"单盘能装""不要 SmithDB 分布式复杂度")。
- **LangSmith multipart attachment / OTLP 大属性**:直接流式写 CAS,不在内存全量驻留(中小规模但单 payload 可能很大,如长上下文/多模态)。
- **冻结格式**(回应 K6 迁移地雷):列式 segment 用 **Lance 或自研冻结格式**,**不用 Vortex**(其格式 breaking 变更对私有化老数据是地雷);blob CAS 用裸文件 + 内部 manifest,版本化、自描述、保证旧版可读。

### 4.2 采样

- **默认不在摄入侧丢 trace**(中小规模、单盘装得下、可观测平台用户要"全量可回溯",且评估/异常检测依赖全量)。采样默认 = 100%。
- **提供可选策略**(客户高峰防爆):头部采样(SDK 侧 `trace_idratio`)、**尾部采样**(在归一器后、写 segment 前,按 error/高延迟/高成本/含特定 kind 保留,正常成功 trace 概率丢弃)。轨 A 因我们控 SDK,可做更聪明的"决策点优先保留"。
- 采样决策记 `raw_attrs.sample`,保证仪表盘聚合可做无偏估计校正。

### 4.3 embedding 旁路捕获(差异化语义召回的供给侧)

这是 §0 差异化"语义 trace 召回"的数据来源,也是 K3(活 trace 在线增量向量更新成本)的落点。

- **旁路而非主路**:span 写 MemTable 是主路(低延迟);取 trace 文本(轨 A 优先用 `vex.thought/decision`,轨 B 用 inputs/outputs 摘要)做 embedding 是**异步旁路任务**,不阻塞摄入。
- **复用已捕获向量**:很多 trace 本身就含 embedding 调用结果(EMBEDDING span 的输出向量)。**直接旁路截获这些已有向量**,避免重复算 embedding → 显著降本(尤其轨 A)。截获的向量进 §0 的向量索引(HNSW/IVF),供语义召回。
- **活 trace(运行中)**:运行中 span 在 MemTable,**默认不实时建向量索引**(K3 增量更新成本);活 trace 召回走"MemTable 直查 + 中文分词倒排"即可满足。向量索引在 **segment flush 时批量构建**(批量比逐条增量便宜得多)→ 中小规模下 K3 大幅缓解,符合本轮约束。
- **中文分词**:摄入归一阶段对文本字段跑原生中文分词,进自研倒排(§0 差异化),与向量召回互补(关键词召回 + 语义召回)。

---

## 5. 给路线图的摄入层切片(含优先级/工时信号)

| 阶段 | 交付 | 含义 |
|---|---|---|
| M1(底座可用) | P1/P2 OTLP + P3 OpenInference 解析 + 内部规范模型 + raw_attrs 兜底 + CAS 大字段抽离 | 能吃下 Python 主流框架(经 OpenInference/OpenLLMetry),平台层可起 |
| M2(迁移卖点) | P4 LangSmith-compat(/runs + multipart + dotted_order + attachment + otel 入口)+ 历史导入工具 | "指 endpoint 迁过来" + 存量回灌成立 |
| M3(差异化) | P6 轨 A 富语义 `vex.*` + embedding 旁路 + segment flush 批量建向量索引 + 中文分词倒排 | 语义 trace 召回 / 数据飞轮供给侧 |
| M4(完整度补齐) | P5 通用 SDK(Py 先、TS 后)+ Dify/LangGraph 节点语义补全 + 尾部采样 + OTel semconv 版本追随机制常态化 | 框架无关完整度 + 实验态追随常态运维 |

---

## 6. 一句话总结与需上层拍板的点

- **架构**:多协议入口(OTLP/gRPC+HTTP、OpenInference 解析、LangSmith-compat REST、通用 SDK、轨 A 私有通道)→ 单一归一器(按 `source_dialect+schema_ver` 选映射,`raw_attrs` 无损兜底)→ 第一轮已定的 MemTable/segment/倒排/trace 树双编码;大字段走本地 CAS,向量在 flush 时批量建。
- **轨 A 是差异化护城河**(`vex.thought/decision` + 复用 EMBEDDING span 向量),轨 B 靠**大量复用 OpenInference/OpenLLMetry(Python 强、TS 弱)**降本。
- **需拍板/排期的风险**:(1) OTel GenAI 仍 Development 且已拆仓、v1.37 已 breaking → 设专人季度追随;(2) LangSmith-compat 只保证 trace 摄入,不保证全平台 API;(3) TS/Go/Ruby 摄入完整度售前要识别;(4) 历史回灌是独立交付项,非"改 endpoint"。

**Sources:**
- https://opentelemetry.io/docs/specs/semconv/gen-ai/ (Moved: GenAI semantic conventions)
- https://github.com/open-telemetry/semantic-conventions-genai
- https://opentelemetry.io/blog/2026/genai-observability/
- https://greptime.com/blogs/2026-05-09-opentelemetry-genai-semantic-conventions
- https://arize-ai.github.io/openinference/
- https://github.com/Arize-ai/openinference
- https://www.traceloop.com/docs/openllmetry/tracing/supported
- https://github.com/traceloop/openllmetry
- https://docs.langchain.com/langsmith/trace-with-api
- https://blog.langchain.com/opentelemetry-langsmith/

> 不确定项已就地标注:LangSmith `run_type` 完整枚举、multipart attachment 大小上限、OTel GenAI 转 Stable 时间表——均需以官方 `smith-api-ref` / semconv-genai 仓 release / 抓真实请求样本进一步确认,本设计未对其编造具体数值。
---

# 中小单机引擎再决策 + 容量SKU

I now have enough verified facts. Key takeaways: Langfuse v3 (the direct competitor) needs ClickHouse + Postgres + Redis + S3 — a heavy multi-service stack, which validates a simpler single-machine play. DuckDB has a hard single-writer-process limit (Quack still beta until v2.0 fall 2026), which matters for the ingest+query concurrency model. Lance 2.1 is stable with explicit back-compat commitment. I have enough to build the quantitative model and decisions.

Let me do one final calculation pass internally and write up the answer.

---

# 第二轮存储引擎决策:中小单机规模下的容量模型、选型与 HA

> 结论先行:**在中小单机规模(单客户 < 1亿 span/天、保留数月、几十 TB NVMe)下,从零自研高吞吐 LSM 是过度工程。推荐"列式不可变 segment 用 Lance + 查询/SQL 用 DataFusion + 热区/活 trace 用一个轻量自研行式 MemTable(不是完整 LSM)"的混合路线。把自研 LSM 推迟到出现"多客户聚合 / 单盘装不下 / 写入 > 50k span/s 持续"任一信号时再启动(预计是 18-30 个月后的事,且很可能永远不需要在 per-customer SKU 里需要)。** 下面是支撑这个结论的定量模型、四方案对比、K6 格式回应、HA 方案。

---

## ① 定量容量模型 / SKU 边界

### 1.1 单 span 的存储分解(建模假设,逐项可调)

一条 GenAI span 不是均匀的。必须把它拆成**结构化核心行 + 大字段 payload + 索引 + 向量**四块分别建模,因为它们落在不同存储、不同压缩比、不同内存需求上。

| 组成部分 | 原始大小(典型) | 说明 |
|---|---|---|
| 核心结构化行 | trace_id/span_id/parent/时间戳/name/status/token 计数/latency/model/[pre,post] 区间编码等 ≈ **300–500 B/span** | 定长+短字符串,列式后压缩比高 |
| 属性 KV(中等字段) | tags、metadata、用户/会话 id 等 ≈ **0.5–1 KB/span** | 半结构化 |
| 大字段 payload(input/output/prompt/completion) | **平均 3–8 KB/span**,LLM 场景常见;大的(长上下文/RAG 文档)单条可达 50 KB–1 MB | 这是磁盘大头,晚物化 + 强压缩 |
| 向量(语义召回用) | 仅对**被采样**的 span 建,1024 维 fp32 = 4 KB/向量;int8 量化 = 1 KB | 见 1.4 采样率 |

**关键压缩假设(列式 + zstd,经验值,标注为估算):**
- 核心行:列式后 ≈ **5–8x** 压缩 → 落盘 **60–80 B/span**
- 属性 KV:≈ **4–6x** → 落盘 **100–200 B/span**
- 大字段 payload:文本 zstd ≈ **3–4x** → 落盘 **平均 1–2.5 KB/span**(取 1.5 KB 作中值)
- 倒排索引(中文分词 + 全文):≈ 大字段原文的 **15–30%** → **0.5–1.5 KB/span**(取 0.8 KB)
- 树/线程索引([pre,post] 物化 + thread_id 倒排):核心行已含区间编码,额外倒排 ≈ **30–60 B/span**

> 综合:**有效落盘 ≈ 2.5–4.5 KB / span(含一份大字段 + 索引,未含向量,未含副本)。取工程中值 3.5 KB/span 做 SKU。**

### 1.2 容量公式

```
盘占用(TB) ≈ span/天 × 保留天数 × 单span落盘字节(~3.5KB) × (1 + 向量增量) × 副本因子
内存(GB)   ≈ MemTable热区 + 倒排/向量索引常驻 + DataFusion查询工作集 + 页缓存目标
```

向量增量(按采样率 s、量化后 1 KB/向量、HNSW 图开销 ≈ 1.5x):`向量盘占用/span ≈ s × 1.5 KB`。

### 1.3 SKU 表(保留 90 天、采样建向量 10%、副本因子 1.0 即单机无副本;另给主备列)

落盘单 span 取 3.5 KB(核心+属性+大字段+全文/树/线程索引),向量按 10% 采样 × 1.5 KB ≈ 0.15 KB/span 摊销。

| SKU | span/天 | 保留 | 总 span | 核心+索引盘 | 大字段盘 | 向量盘(10%) | **裸数据合计** | **建议物理盘(×1.6 余量/WAL/compaction)** | 建议内存 | 主备双机盘 |
|---|---|---|---|---|---|---|---|---|---|---|
| **S(入门)** | 1M | 90d | 90M | ~7 GB | ~135 GB | ~14 GB | **~156 GB** | **~250 GB NVMe** | **32 GB** | 2×250 GB |
| **M(主力)** | 10M | 90d | 900M | ~70 GB | ~1.35 TB | ~135 GB | **~1.55 TB** | **~2.5 TB NVMe** | **64 GB** | 2×2.5 TB |
| **L** | 50M | 90d | 4.5B | ~350 GB | ~6.75 TB | ~675 GB | **~7.8 TB** | **~12.5 TB NVMe** | **128 GB** | 2×12.5 TB |
| **XL(单机天花板)** | 100M | 90d | 9B | ~700 GB | ~13.5 TB | ~1.35 TB | **~15.6 TB** | **~25 TB NVMe** | **256 GB** | 2×25 TB |

> **保留 180 天**:上表盘容量 ×2。XL@180d ≈ 31 TB 裸 → ~50 TB 物理盘,单盘/单卷开始吃力,建议进 RAID 或多 NVMe 卷。这是"保留期"维度的单机天花板信号。

### 1.4 采样率敏感性(向量是最不确定的一块,回应 K3)

向量盘和向量索引内存随采样率线性涨,且**活 trace 在线增量更新**是真实成本点。给三档:

| 采样率(建向量比例) | XL(100M/天,90d)向量盘 | 向量索引常驻内存(HNSW,int8) |
|---|---|---|
| 5% | ~675 GB | ~6–10 GB |
| 10%(默认) | ~1.35 TB | ~12–20 GB |
| 30% | ~4 TB | ~40–60 GB |
| 100%(全量语义) | ~13.5 TB | ~130–200 GB → **单机 RAM 撑不住,必须落盘 DiskANN** |

**结论 / K3 回应**:中小规模下,**默认 10% 采样 + int8 量化 + HNSW 常驻**,XL 档向量索引内存仅 ~20 GB,K3 的"增量更新成本"被规模本身大幅化解。**活 trace 的在线语义召回不要走"实时插全局 HNSW"**——运行中 span 在 MemTable 里,直接走暴力/小图近邻(活 trace 数量级是千~万,brute-force 1024 维毫秒级);span 结束 flush 到 segment 时才批量入主 HNSW。这样避免了 K3 担心的"每条活 span 实时维护大图"的成本。全量 100% 语义召回应作为付费高配开关,且必须切 DiskANN(团队已有 DiskANN 能力,直接复用)。

### 1.5 内存预算分解(M 档 64 GB 为例)

| 用途 | 预算 | 说明 |
|---|---|---|
| 行式 MemTable 热区(活 trace + 未 flush) | 8–12 GB | 按 flush 周期/写入速率定,不是全量驻留 |
| 倒排索引热段 + 字典 | 8–12 GB | 中文分词字典 + 高频段 |
| 向量 HNSW 常驻(10%,int8) | 4–8 GB | 见 1.4 |
| DataFusion 查询工作集(聚合/排序/join 溢写) | 12–16 GB | 配 spill-to-disk,防 OOM |
| OS 页缓存(Lance segment 命中) | 余下 ~16 GB | **这是中小单机的关键杠杆**:几十 GB 页缓存就能让最近数天的查询基本不落盘 |

> **写入吞吐校验**:100M span/天 ≈ **持续 1.2k span/s,峰值按 5–10x ≈ 6–12k span/s**。这远低于"需要自研高吞吐 LSM"的门槛(自研 LSM 通常为 >50k–100k/s 持续写入而生)。**一个带 WAL 的行式 MemTable + 定时 flush 成 Lance segment 即可,根本用不到 LSM 的多层 compaction 复杂度。**

---

## ② 引擎选型对比:还要不要从零自研 LSM?

四个候选,在"中小单机 + 必须快速交付完整平台"两个约束下打分。

| 维度 | (a) Lance + DataFusion | (b) DuckDB / chDB 嵌入式 | (c) 推迟自研 LSM(现成件拼) | (d) 仍从零自研 LSM |
|---|---|---|---|---|
| 列式不可变 segment | Lance 原生,2.1 已 stable,零分支可读 | DuckDB 内建格式/Parquet | = (a) | 自研冻结格式 |
| SQL / 优化器 | DataFusion(Rust,团队 Rust 栈契合) | DuckDB 自带,最成熟 | DataFusion | 自研/嫁接 |
| 向量索引 | LanceDB 原生 IVF/HNSW + 你们 DiskANN 可嵌 | 需外挂(VSS 扩展,弱) | 复用你们自研向量索引 | 自研 |
| 写入/活 trace 模型 | Lance 不擅长高频小写;需自配行式 MemTable | **单写进程**硬限制(Quack 仍 beta 到 v2.0 fall'26),摄入+查询并发难 | 自研轻量 MemTable(几百行级,非完整 LSM) | 完整 LSM |
| 中文分词 + 自研倒排 | 你们自己挂(差异化保留) | 难塞进 DuckDB | 你们自己挂 | 你们自己挂 |
| merge-on-read / deletion vector | Lance 有 row-level delete / fragment 版本 | 较弱 | 自研 deletion vector(轻量) | 自研 |
| 私有化跨版本回读(K6) | **Lance 2.1 back-compat 承诺最强** | DuckDB 格式偶有 break,有迁移工具 | = (a) | 风险全在自己身上 |
| 交付速度(对"60-70% 是平台层"至关重要) | 快(底座现成,精力投平台) | 最快起步,但写并发坑要绕 | 快 | **最慢,挤占平台预算** |
| 工程风险 | 中(Lance 写侧要自配) | 中(单写进程是架构级约束) | 中低 | **高** |

### 推荐:**(a)+(c) 混合 —— Lance(列存) + DataFusion(SQL) + 轻量自研行式 MemTable/WAL(热区与活 trace),复用你们已有的向量索引与中文倒排。不从零自研 LSM。**

理由:

1. **写入压力根本不到 LSM 的设计点**。XL 档持续仅 ~1.2k span/s。第一轮范式里说的"LSM(行式热区→列式 segment→分层 compaction)"在中小单机退化为:**WAL + 行式 MemTable + 定时 flush 成 Lance fragment + 简单的按时间分层合并小 fragment**。这套用 600–1500 行 Rust 就能写出可靠版本,**不是一个完整 LSM 引擎**(无需 leveled compaction、无需 bloom filter 分层、无需复杂的 write amplification 调优)。把它叫"自研 LSM"会误导预算。

2. **DuckDB/chDB(b)被单写进程否决**。摄入是持续写、UI 是持续读,DuckDB 单写进程 + 读写无法多进程并发,Quack 协议要到 2026 fall v2.0 才成熟。强行用要么把摄入和查询塞进一个进程(耦合、互相阻塞),要么上 DuckLake+外部 Postgres catalog(又把"单机简单"的优势丢了)。chDB 同理偏批分析、非持续写。**DuckDB 适合做"附带的即席分析/导出"工具,不适合做主存储。**

3. **从零自研完整 LSM(d)在此规模是明确的过度工程**,且直接和"60-70% 必须做的平台层"抢人抢预算。Langfuse(你们最直接对手)自己都不自研存储,是 ClickHouse+PG+Redis+S3 拼的;你们在单机上更没理由自研 LSM。

4. **Lance 让差异化原样保留**:中文分词倒排、语义召回向量、deletion vector、晚物化大字段,全都能挂在 Lance 之上;Lance 负责"列式不可变 + 版本/事务 + 跨版本可读",你们负责"差异化的索引和召回"。

### "自研 LSM 推迟到何时才需要"——明确触发条件

任一成立才重启自研 LSM 评估,否则永远不做:
- **持续写入 > 30k–50k span/s**(对应单客户 > ~4亿 span/天,已超出本轮 per-customer 定义);或
- 产品形态从 per-customer 私有化转向 **多客户聚合/SaaS 多租户大盘**(单盘装不下,需要分片/分布式,这时才回到 SmithDB 的对象存储路线);或
- **保留期 × 规模导致单卷 > ~30–40 TB** 且查询 SLA 在页缓存失效后劣化,需要更激进的分层 compaction / 冷热分离;或
- Lance 写侧/合并成为实测瓶颈且无法通过参数与 fragment 策略解决。

**预判:per-customer 私有化这条线,大概率到 GA 后 18–30 个月都不触发,很可能永不触发。**

---

## ③ 回应 K6:列式格式选 Lance 还是自研冻结格式

**推荐:用 Lance(2.1 stable),不自研冻结格式;但加一层"防御性封装 + 冻结测试 + 长青回读保证"。**

依据(均为核实事实):
- **Lance File 2.1 已 stable,官方明确承诺 2.1 向后兼容,breaking 变更保留给 2.2;且现有 table 在创建时固定到具体版本号,提供迁移命令**。这正面回应了 K6 最担心的"格式 breaking 砸了私有化老数据"。对比 Vortex(更新更激进、尚未承诺长期稳定),Lance 的稳定性承诺是当前最强的开源选项。

自研冻结格式的代价:你要自己扛"格式演进 × 私有化客户散落 N 个版本 × 每个都要能跨版本回读"的全部维护负担,且没有社区分摊。**只有当你需要 Lance 不提供的特性(如把中文倒排/向量图直接内联进文件)且无法外挂时才值得**——目前不需要,索引外挂即可。

**跨版本回读 / 迁移的工程保证(无论选 Lance 还是任何格式都要做)——这是产品化的硬交付物:**

1. **版本钉死 + 元数据自描述**:每个 Lance dataset/segment 头部写入 `{lance_file_version, yitrace_schema_version, writer_build}`,创建时钉死,绝不隐式升级。
2. **N-2 回读契约**:承诺"任意发布版的引擎能读最近 2 个大版本写出的数据"。CI 里维护一个**格式语料库(golden corpus)**:每个发布版各造一份真实数据样本(含大字段、deletion vector、向量、各类索引),**每次 CI 跨版本回读校验**,任何回读失败即阻断发布。
3. **离线迁移工具 `yitrace-migrate`**:封装 Lance 官方 migrate 命令 + 你们自研索引(倒排/向量)的重建,做成一条命令、可断点续跑、迁移前自动快照、可回滚。私有化升级 SOP 强制"先 dry-run 校验再原地迁移"。
4. **不把格式细节暴露给客户 SQL**:对外是 SQL,内部格式是实现细节,留出未来换格式的空间(把 Lance 包在存储抽象层后,理论上可换,但近期没必要)。
5. **冷归档双写选项**:对要长期合规留存的客户,提供"导出为 Parquet/JSONL"的归档出口,作为格式风险的最终兜底(即便 Lance 版本断代,数据仍可从中性格式恢复)。

---

## ④ HA / 备份:中小单机,企业仍要主备 / RPO / RTO

中小单机不等于可以没有 HA——金融政企会在合同里写 RPO/RTO。分三档产品形态:

### 形态 1:单机 + 增量备份(默认 SKU,覆盖多数中小客户)
- **WAL 归档 + 周期快照**:行式 MemTable 的 WAL 持续归档到第二块盘/客户 NAS/对象存储;Lance fragment 不可变,**增量备份天然友好**(只备新 fragment + deletion vector + 索引增量,不重传历史)。
- **RPO ≈ WAL 归档间隔(可做到 ≤1–5 分钟);RTO = 拉快照 + 回放 WAL + 重建/加载索引**,M 档 ~1.5 TB 数据,RTO 量级 **30–90 分钟**(取决于备份介质带宽)。
- 备份 SLA 建议:**每日全量逻辑快照 + 持续 WAL 归档 + 保留 N 份**;给客户一条 `yitrace-restore` 单命令恢复。

### 形态 2:主备只读热备(企业付费 HA SKU)
- **流复制式备机**:把 WAL 流 + 新 Lance fragment 实时同步到备机(fragment 不可变 → 同步就是文件级追加,极简单,比 PG 流复制还好做);备机持续 apply,保持只读热备,也可承担**读分流**(UI 查询打备机,减主机压力)。
- **RPO ≈ 秒级(异步流复制);RTO ≈ 分钟级**(VIP/DNS 切换 + 备机提主)。
- 故障切换:主机挂 → 备机提主(手动确认或带 fencing 的自动切换,避免脑裂);需要一个轻量 health-check + 切换控制器(可复用 keepalived/VIP 这类成熟件,不自研)。
- 这是**双机盘成本翻倍**(见 SKU 表"主备双机盘"列)+ 一份 HA 控制面工程,作为**独立付费档**卖给有 RPO/RTO 合同的金融政企。

### 形态 3(可选高配):同城双活只读 + 异地冷备
- 异地放一份每日快照(对象存储/磁带),满足"异地容灾/合规"条款;RPO=24h、RTO=数小时,作为 DR 兜底,不参与日常流量。

**HA 设计的工程红利**:因为采用了 **Lance 不可变 fragment + WAL** 这套(而不是一个可变就地更新的自研 LSM),**备份和复制都退化成"文件级增量同步 + WAL 流"**,实现复杂度远低于复制一个 LSM 的多层 SST + compaction 状态。这反过来又是"不自研 LSM"的一个加分项。

---

## 一页纸结论(给路线图/预算用)

| 决策点 | 结论 |
|---|---|
| 自研 LSM? | **不**。中小单机写入仅 ~1.2k span/s,远不到 LSM 设计点。用"WAL+行式 MemTable+flush 成 Lance fragment+简单时间分层合并"替代,~600–1500 行 Rust,不是完整 LSM。 |
| 存储引擎 | **Lance(列存,2.1 stable)+ DataFusion(SQL)+ 轻量自研热区 + 复用现有向量索引/中文倒排**。 |
| 排除项 | DuckDB/chDB(单写进程,摄入+查询并发受限,Quack 未成熟);从零自研完整 LSM(过度工程,挤占平台预算)。 |
| 列式格式(K6) | **Lance**,不自研冻结格式。加 N-2 回读契约 + golden corpus CI + `yitrace-migrate` 一键迁移 + Parquet 中性归档兜底。 |
| 向量/K3 | 默认 10% 采样 + int8 + HNSW 常驻(XL 仅 ~20 GB 内存);活 trace 走 MemTable 内 brute-force/小图,flush 时批量入主图;全量语义召回切 DiskANN 作高配。 |
| SKU 天花板 | 单机 **XL = 100M span/天 × 90d ≈ 16 TB 裸 / 25 TB 物理盘 / 256 GB 内存**站得住。保留 180d 或 >1亿/天 → 进 RAID/多卷或触发架构升级评估。 |
| HA | 三档:单机增量备份(默认,RPO≤5min/RTO 30–90min)、主备热备(付费 HA SKU,RPO 秒级/RTO 分钟级,双机盘翻倍)、异地冷备(DR 兜底)。不可变 fragment 让复制/备份退化成文件增量同步,实现简单。 |
| 自研 LSM 触发条件 | 持续 >30–50k span/s、或转 SaaS 多租户大盘、或单卷 >30–40 TB 且 SLA 劣化。预判 per-customer 线 18–30 个月内不触发。 |

**关键不确定项(诚实标注)**:① 落盘 3.5 KB/span 是基于典型 LLM payload(平均 3–8 KB 原文)+ 经验压缩比的估算,**真实值需用客户实际 trace 样本标定**,大字段大小波动会直接放大/缩小盘容量;② Lance 写侧在持续小写 + 频繁 flush 下的实测吞吐与 fragment 碎片化行为需用你们的写入 pattern 做 POC 验证;③ Quack/DuckLake 在 2026 fall 后若成熟,可重新评估 DuckDB 作为"即席分析旁路"的角色(不改主存储结论)。

**来源**:
- [Lance File 2.1 is Now Stable](https://www.lancedb.com/blog/lance-file-2-1-stable)、[Lance v2 Beta](https://blog.lancedb.com/lance-v2-is-now-in-beta/)、[LanceDB data/versioning docs](https://lancedb.com/documentation/concepts/data.html)
- [ClickHouse acquires Langfuse(官方)](https://clickhouse.com/blog/clickhouse-acquires-langfuse-open-source-llm-observability)、[ClickHouse $400M Series D + Postgres](https://clickhouse.com/blog/clickhouse-raises-400-million-series-d-acquires-langfuse-launches-postgres)、[InfoWorld 报道](https://www.infoworld.com/article/4118621/clickhouse-buys-langfuse-as-data-platforms-race-to-own-the-ai-feedback-loop.html)
- [Langfuse v3 self-host 架构(ClickHouse+PG+Redis+S3)](https://langfuse.com/self-hosting)、[Langfuse+ClickHouse 数据栈](https://clickhouse.com/blog/langfuse-and-clickhouse-a-new-data-stack-for-modern-llm-applications)
- [DuckDB Concurrency(单写进程)](https://duckdb.org/docs/current/connect/concurrency)、[Quack 协议 beta→v2.0](https://siddique-ahmad.medium.com/duckdb-just-changed-the-game-meet-quack-the-protocol-that-unlocks-multiple-writers-d339e92f0bda)
---

# 语义召回差异化（中小规模可行性）

I have enough verified ground truth. Writing the design now.

---

# 单机本地 NVMe 上「带过滤的 trace 语义召回」——中小规模做实 + 回应红队 K3

> 本节是 yiTrace 之上的差异化引擎。立场:**中小单机规模下,十亿级向量的恐惧不成立;真正的工程难点不在 ANN 规模,而在(a)增量更新的"在线 vs 近线"诚实定级,(b)带过滤 ANN 的低选择度退化,(c)气隙 embedding 的中文质量与吞吐,(d)说服客户为此付溢价。** 下文逐条做实,不确定项明确标注。

---

## ① 向量规模数学:先把"十亿恐惧"算掉

红队 K3 担心的是"十亿级在线增量向量"。先用采样把真实条数算出来。

### 关键决策:不是每个 span 都 embed

trace 树里绝大多数 span 是工具调用细节(HTTP、DB query、内部 function span),语义召回价值低且高度重复。**只对"语义检索有意义的单元"做 embedding**:

- **root span / trace 级**:1 条/trace,代表整条轨迹的"意图+结果"(用户输入 + 最终输出拼接)。这是召回的主力。
- **关键 LLM span**:prompt + completion 的 span(一条 trace 里通常 1~5 个)。
- **采样**:对上述再叠加业务采样率(全量 root,或 1%~5% 全量 span)。

### 三档算账(单客户、单盘)

设单客户 **1 亿 span/天**(题给上限),保留 **90 天**。

| 方案 | embed 比例 | 向量/天 | 90 天总量 | 维度/精度 | 原始向量字节 | 索引内存(HNSW≈含图) |
|---|---|---|---|---|---|---|
| A 仅 root/trace | 假设 10 span/trace → 1000 万 trace/天,全量 root | 1000 万 | **9 亿** | 1024×fp16=2KB | ~1.8 TB | 偏大,见下 |
| **B root + 关键 LLM span(推荐)** | ~每 trace 取 1 条代表 → 1000 万/天 | 1000 万 | **9 亿** | 1024×fp16 | ~1.8 TB | 偏大 |
| **C B + 降维到 256 + int8/PQ** | 1000 万/天 | **9 亿** | 256×int8=256B | ~230 GB | **可控** |
| D 1% 全量 span 采样 | 100 万/天 | **9000 万** | 256×int8 | ~23 GB | 轻松 |

**结论(关键)**:即便取上限 1 亿 span/天 + 全量 root,**向量规模是 ~9 亿,不是十亿级的失控**;而且这 9 亿是"90 天累计的冷数据",不是"同时在线增量"。真正需要**在线增量**的只有 MemTable 里**运行中/最近几分钟**的活 trace —— 那是 **几万~几十万条**量级,不是亿级。

而且 9 亿这个数只有在"上限客户 + 全量 root + 保留 90 天 + 不降维"四个最坏假设同时成立才出现。**现实中位客户**(1000 万 span/天、保留 30 天、降维 256+int8)是:

> **中位:300 万 trace/天 × 30 天 ≈ 9000 万向量,256 维 int8 ≈ 23 GB 原始 + ~30–40 GB 索引。一台 128GB~256GB 内存的单机轻松全内存放下。** 上限客户(9 亿)则必须走 **DiskANN/磁盘图 + PQ**,正好打在团队已有的 DiskANN/磁盘索引能力上。

**这直接回应 K3**:十亿级在线增量的成本担忧,在"采样 + 中小 + 区分活/冷"三个杠杆下,降为(i)活 trace 几十万条的真在线,(ii)冷区 9000 万~9 亿条的近线/磁盘图。前者 tractable,后者用团队现成的 DiskANN 框架。

> ⚠️ 不确定项:每 trace 的 span 数(这里取 10)和"代表向量条数/trace"(取 1)是估算,**实际由客户 agent 形态决定**(多轮 ReAct 可能 50+ span/trace)。容量规划必须做成**配置参数**(embed 选择策略 + 采样率 + 维度),而不是硬编码。落地前用客户真实 trace 抽样实测一次。

---

## ② 增量更新:诚实给 v1 的"在线 vs 近线"定级

这是必须诚实的地方。HNSW 支持增量插入,但**单条插入要做 Candidate Acquisition + Neighbor Selection 的图遍历**,并发写入要锁/拷贝图,大批量在线插入会抖动召回质量和尾延迟。([SHINE/HNSWlib 资料](https://arxiv.org/html/2507.17647v1))

我把数据按 yiTrace 的 LSM 分层,分三个区,各用不同策略:

### 三区增量模型

1. **活区(MemTable 里运行中/刚结束的 span,几万~几十万条)**
   - **v1 = 真在线**。维护一个**小型内存 HNSW(单段,几十万条上限)**专供活 trace。新 span 完成 embedding 后**单条 in-memory 插入**,毫秒级可被语义召回命中。规模小,增量成本完全可接受。
   - 这是回应 K3 的"活 trace 在线语义召回"的核心:**把在线增量限制在小图里**,不在全量 9000 万的大图上做在线写。

2. **温区(近几小时 flush 成的列式 segment)**
   - **v1 = 近线,分钟级**。MemTable flush 成不可变 segment 时,**对该 segment 批量构建一个独立的小 HNSW/IVF 子索引**(几十万~百万条,秒级~分钟级构建)。查询时**多段并查**(每段一个图,结果归并),完全契合 yiTrace 的 merge-on-read 范式。

3. **冷区(时间分层 compaction 后的大段,9000 万~9 亿)**
   - **批量重建,小时/天级**。compaction 合并 segment 时,顺带把多个子索引**离线重建成一个大段索引**(全内存场景 HNSW;上限客户场景 **DiskANN + PQ on NVMe**)。这是离线作业,不影响在线写。

### v1 能力定级(写给路线图,别吹)

| 数据新鲜度 | v1 能做到 | 机制 |
|---|---|---|
| 运行中/刚结束的活 trace | **在线,秒级可召回** | 内存小 HNSW 单条插入 |
| 最近几小时 | **近线,分钟级** | flush 时 per-segment 批量建子索引 |
| 历史全量 | **批量,小时/天级一致** | compaction 离线重建 / DiskANN |

**v1 不承诺**:"全量历史数据的实时在线增量"。这点对客户也合理——历史 trace 几小时延迟进入语义索引完全可接受,客户真正要的"我刚跑的这条 agent 轨迹能否立刻语义搜到"由活区满足。

> 工程取舍:多段并查会随段数增多而召回变慢/质量飘移,**必须靠 compaction 控制活跃段数**(如 ≤ 16 段在线图)。这是 LSM 系统的标准 read-amplification 治理,团队有经验。多段 ANN 结果归并要做**全局 re-rank**(用精确距离对各段 topK 候选重排),否则跨段 recall 不可控。

---

## ③ 带过滤 ANN:租户/时间/cost/JSON 过滤后再语义召回

这是查询层真正的硬骨头。典型查询:
> "在租户 A、最近 7 天、cost > $0.1、`metadata.model = 'gpt-4o'` 的 trace 里,语义最接近『用户抱怨退款流程』的 20 条。"

### 选择度决定策略(三分支)

过滤 ANN 的核心难题是**低选择度退化**:过滤后剩很少候选时,post-filter(先 ANN 再过滤)会反复扩大 ef 才凑够 k,延迟随选择度倒数上升;pre-filter(先过滤拿 ID 集再暴力/受限图搜)在候选少时反而快。([Weaviate ACORN](https://weaviate.org/blog/speed-up-filtered-vector-search)、[pgvector iterative scan](https://www.clarvo.ai/blog/optimizing-filtered-vector-queries-from-tens-of-seconds-to-single-digit-milliseconds-in-postgresql))

**三分支执行计划,由代价模型选:**

- **高选择度(过滤后 ≫ k,如剩 >5% 数据)→ ACORN 式 in-filter 图遍历**:在 HNSW 遍历时对邻居做谓词检查 + 2-hop 扩展,只对通过谓词的节点算距离。无需预建谓词专用索引,谓词无关。
- **中选择度 → iterative/post-filter + 动态 ef**:按选择度估算放大倍数(选择度 1/s 则取 k·s·安全系数个候选),pgvector 式迭代加深直到凑够 k。
- **低选择度(过滤后 ≤ 数千)→ pre-filter + 暴力精排**:先用 yiTrace 的**倒排 + 列式 segment 的 zone-map/min-max** 把候选 ID 集求出来(租户/时间是天然分区裁剪,cost/JSON 走列存谓词下推),候选少时**直接对候选向量做精确 L2/cosine 暴力扫**,延迟可控且 recall=100%。

> 关键复用:**租户和时间在 yiTrace 里本就是分区/分层维度**,过滤的第一刀几乎免费(段级裁剪),把 ANN 的真实搜索域从 9000 万压到单租户单时间窗的百万级。这是单机能站住的根本原因——**绝大多数查询的有效搜索域远小于全量。**

### 优化器代价选择(落到团队已有的优化器能力上)

复用 openGauss/PG 优化器框架,为"带过滤语义召回"加一个**自定义代价节点**:

1. 先用列存统计 + 倒排基数估计算出**过滤选择度 s**(团队优化器已有选择度估计基础设施)。
2. 估各分支代价:
   - pre-filter 暴力 ≈ `O(候选数 × 维度)`,候选数 = `N × s`。
   - ACORN/in-filter ≈ `O(ef × 平均度 × 谓词检查 / s)`(选择度越低越贵)。
   - post-filter 迭代 ≈ `O(k/s × log N)`。
3. 取最小代价分支。**交叉点(经验值 s≈0.5%~2%)需用客户数据离线标定**,做成代价模型参数。

> ⚠️ 不确定项:选择度交叉点强依赖维度、过滤字段分布、段数。**v1 先用保守静态阈值 + 强制 re-rank 保证 recall,把"代价模型自动选分支"放 v1.5**。先正确,再快。

---

## ④ 气隙 embedding:客户内网无 API 时的本地模型

这是私有化必答题,且是中文场景的真护城河之一。

### 选型(全部许可证可商用,已核实)

| 模型 | 许可证 | 中文质量 | 维度 | 部署形态 | 适用 |
|---|---|---|---|---|---|
| **Qwen3-Embedding-0.6B** | **Apache 2.0**(可商用) | C-MTEB 强,中英俱佳,支持 32K 长上下文,**输出维度可配 32~1024(Matryoshka)** | 1024→可降 256 | ONNX/TEI,**int8 量化 CPU 可跑** | **气隙默认主推** |
| **BGE-M3** | **MIT**(可商用) | 中文基准级,多语 100+,稠密+稀疏+多向量 | 1024 | CPU/GPU | 备选/需稀疏混检时 |
| Qwen3-Embedding-4B/8B | Apache 2.0 | 更强 | 1024 | **需 GPU** | 客户有 GPU 且要顶配召回 |

来源:[Qwen3-Embedding(Apache 2.0,0.6/4/8B,32K,可变维)](https://github.com/QwenLM/Qwen3-Embedding)、[BGE-M3(MIT,多语)](https://www.bentoml.com/blog/a-guide-to-open-source-embedding-models)、[Qwen3-0.6B int8 ONNX/TEI](https://huggingface.co/janni-t/qwen3-embedding-0.6b-int8-tei-onnx)。

### 部署策略

- **默认 = Qwen3-Embedding-0.6B,int8 量化,ONNX Runtime / TEI 在 CPU 上跑**。理由:0.6B + int8 是"无 GPU 也能用"的甜区,int8 较 fp32 有 2~4× CPU 加速([HF 模型卡](https://huggingface.co/janni-t/qwen3-embedding-0.6b-int8-tei-onnx))。**用 Matryoshka 直接输出 256 维**,省一半多存储和索引内存(对应①方案 C/D)。
- **有 GPU(政企常配国产卡)→ 上 4B,顶配召回**;并支持**国产推理栈**(昇腾/CANN 路径,见 K7)。
- **吞吐保护**:embedding 是写放大来源。设计成**异步队列 + 批处理**:span 落库走 yiTrace 主线(不阻塞),embedding 由独立 worker 池消费,跟不上时**自动降级**(只 embed root span,或临时调低采样率,或排队)。活区在线召回因此可能有秒级 embedding 滞后——可接受。
- **模型随产品离线打包**(权重进安装包),气隙环境零外联。**版本固定 + 升级即全量重 embed**(模型换了向量空间就变,不能混用)——这要写进运维手册,是一次性重建作业。

> ⚠️ 不确定项:0.6B-int8 在客户具体 CPU(可能是国产 x86/ARM)上的真实 sentences/sec 我没有可信公开基准,**必须在目标硬件上实测**后再定容量。题给"1 亿 span/天"若全 embed 对纯 CPU 是重负载——这是②里"异步+降采样+降级"必须存在的原因,也是销售时把"embedding 比例"做成可调档的原因。

---

## ⑤ 产品 UX:语义召回 + 飞轮原语 + 对接自家 agent 在线回路

### 平台里长什么样

1. **语义搜索框(trace 浏览器顶部)**:自然语言查 + 结构化过滤器 side-by-side。输入"用户对退款不满意的对话",叠加租户/时间/cost/model 过滤 → 命中 trace 列表,每条显示相似度 + 高亮匹配 span。**这是②③的直接出口**。
2. **"相似轨迹"按钮(单 trace 详情页)**:任意一条 trace,一键"找语义相似的历史轨迹"。用于:复现 bug、找同类失败、看正例长什么样。
3. **飞轮原语视图**:
   - **轨迹导出**:把一组(语义召回 + 过滤选出的)trace 导成训练/评测集(JSONL / few-shot 格式)。
   - **few-shot 池**:把"高奖励轨迹"语义聚类,沉淀成可复用的 few-shot 示例库。
   - **SOP 提炼**:对某类任务的成功轨迹聚类,辅助人工总结标准操作流程。
   - **奖励视图**:把评估打分/人工标注/线上反馈作为向量元数据,支持"语义召回 + 按奖励排序",一眼看高分/低分轨迹的语义分布。

### 对接自家 agent 产品的在线推理回路(深集成的杀手锏)

因为 SDK 你们自己控制(题给硬约束 1),可以闭环:

```
线上 agent 收到新请求
   → 调 yiTrace 语义召回 API(带过滤:同租户/近 N 天/高奖励)
   → 取 top-k 相似历史成功轨迹
   → 注入为 few-shot / 检索增强上下文
   → agent 据此决策(在线推理回路)
   → 本次轨迹回流入库 → 进入下一轮召回池(飞轮闭环)
```

这把"trace 库"从**事后可观测**升级为**在线 agent 记忆/经验检索层**——这是第三方框架接 OTel 拿不到的深度,是自家 agent 产品的独占增值。低延迟召回(活区+小图+pre-filter)正是为这条在线回路服务。

> ⚠️ 在线回路对召回延迟敏感(进 agent 关键路径)。要给召回 API 设 **SLA + 超时降级**(超时就不注入 few-shot,不阻塞 agent)。

---

## ⑥ 最小验证:客户是否真为语义召回付溢价

这是最该冷静的一条——**别先把 ④的大模型和 ②的三区索引全建完才去问市场。** 验证顺序由轻到重:

1. **影子功能 + 埋点(最轻,先做)**:平台先只上"语义搜索框"+"相似轨迹"按钮,**只对 root span 用 0.6B 近线索引**(不做活区在线、不做三区)。埋点测:语义搜索使用率、人均次数、是否进入留存路径。**若没人用,后面全停。**
2. **A/B 关键词 vs 语义**:同一搜索框,一半用户给传统倒排关键词搜,一半给语义搜。看任务完成率/满意度差异。**证明语义比关键词强多少,才有溢价基础。**
3. **付费意愿探针**:把语义召回 + 飞轮做成**独立加价模块/套餐档**(基础版只有关键词搜 + 瀑布/diff;高级版才有语义召回 + 飞轮 + 在线回路)。看转化。**愿意为高级档付钱 = 真溢价信号。**
4. **自家 agent 回路 ROI(最强证据)**:在自家 agent 产品里 A/B"有无语义召回 few-shot 注入"对任务成功率的提升。**如果在线回路能把自家 agent 成功率提 X%,这就是不靠客户嘴说、用数据证明的溢价依据**,也是对外销售的标杆案例。

> 验证心法:**语义搜索使用率(行为)> A/B 召回质量(效果)> 加价档转化(钱)> agent 回路 ROI(硬证据)**。前两步两周内能出信号,别等引擎全做完。

---

## 一页结论

- **规模**:采样 + 区分活/冷后,最坏 9 亿、中位 9000 万向量;**十亿在线增量的恐惧不成立**。中位全内存单机搞定,上限客户走团队现成 DiskANN+PQ。**K3 缓解成立。**
- **增量**:v1 诚实定级 = **活区在线(秒级,小图)/ 温区近线(分钟,per-segment 子索引)/ 冷区批量(compaction 重建)**。不承诺全量历史实时。
- **带过滤 ANN**:三分支(pre-filter 暴力 / ACORN in-filter / post-filter 迭代),靠 yiTrace 的租户+时间分区把搜索域压小;优化器加自定义代价节点选分支,v1 先静态阈值 + 强制 re-rank。
- **气隙 embedding**:默认 **Qwen3-Embedding-0.6B(Apache 2.0)int8 ONNX/CPU + Matryoshka 256 维**;有 GPU 上 4B;异步队列 + 降级保护写吞吐。许可证均可商用且已核实。
- **UX/回路**:语义搜索 + 相似轨迹 + 飞轮四原语 + **自家 agent 在线召回回路**(深集成独占增值)。
- **验证**:从"埋点测使用率"起步,四级递进到"agent 回路 ROI",**别等引擎做完再问市场**。

**主要不确定项(需实测/标定,不可凭空承诺)**:每 trace 实际 embed 条数;目标硬件上 0.6B-int8 的 CPU 吞吐;带过滤分支的选择度交叉点;多段并查的 recall 退化曲线。这四项都应在与首个客户的 POC 中用真实 trace 标定。

**Sources:**
- [HNSW 增量与内存(SHINE/HNSWlib)](https://arxiv.org/html/2507.17647v1)、[HNSWlib 规模约束](https://zilliz.com/learn/learn-hnswlib-graph-based-library-for-fast-anns)
- [ACORN 谓词无关过滤搜索](https://arxiv.org/pdf/2403.04871)、[Weaviate ACORN 实践](https://weaviate.io/blog/speed-up-filtered-vector-search)、[pgvector 迭代过滤](https://www.clarvo.ai/blog/optimizing-filtered-vector-queries-from-tens-of-seconds-to-single-digit-milliseconds-in-postgresql)
- [Qwen3-Embedding(Apache 2.0,0.6/4/8B,可变维)](https://github.com/QwenLM/Qwen3-Embedding)、[Qwen3-0.6B int8 ONNX/TEI](https://huggingface.co/janni-t/qwen3-embedding-0.6b-int8-tei-onnx)、[BGE-M3(MIT)与开源 embedding 综述](https://www.bentoml.com/blog/a-guide-to-open-source-embedding-models)