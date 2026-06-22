# Agent/LLM 可观测性竞品功能调研

> 日期：2026-06-18｜方法：8-agent 工作流并行调研 + 两路红队复核｜用途：给 yiTrace 功能缺口评审做底（见 `docs/analysis/2026-06-18_feature-gap-review.md`）
> 说明：竞品能力按"过半竞品标配"归纳，非逐家逐版本核对；红队提示部分维度（多模态/流式/采样几家有）需上线前再精确核。

## 直接对手

**Langfuse**（开源自托管 MIT，**2026-01 被 ClickHouse 收购**，后端 ClickHouse+Postgres+Redis+S3）—— 最贴近我方"开源自托管"对标。全生命周期：层级 trace/span + Sessions + Environments；eval 三件套（LLM-as-judge / code scorer / 人工标注队列）+ 在线/离线双模式；Datasets（含生产 trace→数据集、CI/CD 回归门禁）；Prompt 管理全套（版本/label/回滚/Playground/A-B）；自定义 Dashboard + 告警；**OTel 原生** + 20+ 框架自动埋点。**硬伤(对中国)**：多组件非单二进制、无原生中文检索、数据出境。

**LangSmith**（LangChain 官方，闭源 SaaS+自托管，agent-native）—— 企业能力全(SSO/SAML/SCIM/RBAC/ABAC/审计/加密),正是我方暂缓的等保方向的直接对手。Agent Studio 可视化调试断点；**LangSmith Engine**(2026-05 公测)自主改进闭环:生产失败聚类成 issue→自动起草修复 PR→拉失败 trace 进回归集；评估器深(LLM-judge/pairwise/multi-turn/judge 校准)；逐节点 state diff。**硬伤(对中国)**:无开源核心、无中文、数据出境。

## 开源 / OTel 系

**Arize Phoenix / Helicone / Traceloop(OpenLLMetry) / Pydantic Logfire**：
- **OTel/OpenInference 语义约定** 是共同入口标准（LLM/RETRIEVER/EMBEDDING/RERANKER/TOOL/AGENT/CHAIN/GUARDRAIL span kind + `gen_ai.*` 指标）。
- 自动埋点(patch SDK / proxy 零改码)覆盖 20+ 框架；Helicone 是 Gateway/Proxy 范式(代理即埋点+缓存+限流+Key Vault)。
- 质量：guardrails(toxicity/PII/幻觉)、**默认 PII 脱敏**(正则+callback,对 LLM 消息字段豁免)、RAG retrieval relevancy。
- **多模态** trace(图像/音频,OTel 对多模态实时评测仍无标准=空白机会)；**streaming** chunk 事件(仍 unstable)。
- Logfire：全栈统一 trace(LLM+HTTP+DB 同链)、**SQL/MCP 查询可观测数据**(我方有自有引擎,这块可差异化)。

## eval / 企业系

**Braintrust / W&B Weave / Datadog LLM Obs / New Relic**：
- **在线打分**(生产 trace 落库即异步 judge,可配 scorer+采样率+过滤)；**回归测试门禁**(eval 当 CI 单元测试,PR 跑+回贴评论,Braintrust 标杆)；Experiments 不可变可对比。
- Scorer 三件套(内置 factuality/relevance/safety/regex/embedding + code + LLM-judge)；人工评审与自动打分共享、标注回流 golden dataset。
- **漂移/异常检测**(自适应非静态阈值)、**语义聚类 Patterns**(生产流量按语义分簇)、AI 告警降噪。
- 成本归因到 trace/span/model/provider/prompt(覆盖数百模型),指标如"cost per correct""p95 latency per prompt"；采样(尾采样 Refinery)；SLO 监控。

## Agent 专属(区别于 LLM 可观测,最贴 AgenticData)

- 逐节点 **state diff** / 节点状态快照对比(LangSmith 标杆)；**agent 执行图/决策路径**有向图(输入→tool→子 agent→输出)。
- **trajectory 轨迹回放 + time-travel**(回退重放到偏离点,AgentOps)；**tool/function call 一等 span**(选错工具检测)。
- **死循环检测**(Datadog GA,agent 独有失败模式)；多轮 session 串联；多轮 simulation 预发布测试(前沿)。
- **RAG 检索步骤可观测 + retrieval relevancy / embedding 漂移**(Phoenix —— 与我方 BM25/graph_index 强相关)。
- agent 失败归因 / 相似失败聚类；成本按 agent/tool 拆分；human-in-the-loop；agent 评测(任务成功率/步数/工具正确性/轨迹质量)。

## 给我方的三条总判断

1. **入口标准已收敛到 OTel `gen_ai.*` / OpenInference** —— 不兼容 = 生态绝缘。
2. **竞品的收钱层是 eval / prompt / dataset / dashboard / 告警** —— 这正是我方几乎全空的一层；其中 **eval 是命门**。
3. **没有一家是中文原生 / 自有语义召回 / 自家 agent dogfood / 单机数据不出境** —— 这是我方仅有的、SaaS 结构上抄不动的差异化。
