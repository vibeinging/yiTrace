# yiTrace 功能缺口评审与开发优先级

> 日期：2026-06-18｜方法：8-agent 竞品调研 + 综合 + 两路红队，再按红队读真码逐条纠偏
> 竞品详情见 `docs/research/2026-06-18_agent-observability-competitors.md`
> **本文已纠正初稿两处过度乐观（红队读码证伪），结论比初稿更冷一档，见 §7。**

## 0. 一句话结论（已纠偏）

我方现在是一台**算法正确、但全在内存里、还没盖产品层的 trace 引擎雏形**。

- **存储不是"世界级地基",是"设计对、测试对、但没落盘的算法骨架"**：WAL 不 fsync（`// TODO`，是进程内 `Vec`）、段是 `InMemorySegmentStore`、"崩溃恢复"测试只丢内存表却把 WAL 当活 Vec 保着。**今天真重启 = 数据全没。** 协议设计(确定性 event_id/四源折叠/三水位回收/快照隔离)是对的、有测试，但**磁盘持久化尚未实现**——这是地基的承重墙,还没浇。
- **产品层(eval/prompt/dataset/dashboard/告警/OTel 入口)几乎全空**。竞品把存储外包给 ClickHouse、把楼盖得很高;我方反过来。
- **差异化(中文检索/语义召回)目前是占位 mock**：BM25=子串匹配(无分词)、graph=暴力 L2、带过滤召回的下推路径被写死 `|_,_| true` 整个绕过。差异化还停在"接口边界",不是"有雏形"。

**核心矛盾**:不是"再加固内存算法",是**先把数据真落盘(否则连存储引擎都不算)→ 打通入口(否则没数据可观测)→ 再盖能产生产品价值的楼(eval)**,全程别丢中文/语义/agent dogfood 这三条唯一拿得出手的差异化。

## 1. 功能矩阵（按真码标注，红队已逐行降级）

"竞品普遍"=过半竞品标配。我方现状分:**有**(扎实实现有测试) / **占位**(mock 或骨架,逻辑通但非真件) / **无**。

| 能力 | 竞品普遍 | 我方 | 真实成熟度(按代码) |
|---|---|---|---|
| 自有 SDK 上报(Python/TS) | 有 | **有** | 真,跨进程实测;但集成面窄(仅手动建 span) |
| OTLP/OpenInference 入口 | 有(事实标准) | **无** | 只有私有 JSON,生态绝缘 |
| 自动埋点(零改码 20+框架) | 有 | **无** | 要手写 span |
| trace/span 层级 + 父子树/瀑布 | 有 | **有** | 真,dfs 序 |
| **持久化/重启不丢数据** | 有(落 DB) | **无** | ⚠️ WAL 不 fsync、段在内存,**重启全丢** |
| Session/Thread 多轮 | 有(一等) | **无** | 数据模型无 session_id;仅原型 HTML 演示 |
| Environment(dev/stg/prod) | 有 | **无** | 加字段可起步 |
| 中文全文检索 | 少 | **占位** | BM25=子串 contains,无分词,InMemory |
| 向量/语义相似 | 部分 | **占位** | 暴力 L2,InMemory |
| 带过滤召回(ACORN) | 部分 | **无** | 下推被写死全放行,路径未接 |
| 混合召回(RRF) | 少 | **有** | 真(纯函数测过),亮点 |
| token/cost trace 级汇总 | 有 | **有** | 真,穿折叠 |
| 成本 per-agent/per-tool/model | 有 | **无** | SpanFields 无 agent/tool/model 字段 |
| 跨模型单价表(token→金额) | 有 | **无** | 且是带时间版本的运营负债(算旧 trace 用当时价) |
| **Eval(LLM-judge/code/在线/人工标注)** | 有(标配) | **无** | **最大缺口,整层空** |
| Datasets(+生产trace→集) | 有 | **无** | eval 的燃料 |
| Prompt 管理/Playground/A-B | 有 | **无** | |
| Dashboard 可配 + 告警 | 有 | **无** | 有静态原型 HTML(假数据),非可配 dashboard |
| 质量:幻觉/PII/毒性 | 有 | **无** | PII 是金融刚需 |
| 漂移/异常检测、语义聚类失败 | 部分 | **无** | 但我方有 ANN,做聚类有结构优势 |
| agent 执行图(DAG)/state diff/死循环 | 部分 | **DAG 已做**(`agent_graph`,2026-06-22);state diff/死循环检测仍无 | 有树有图了;数据模型 agent 维度已补 |
| 轨迹回放/time-travel | 少 | **无** | append 模型几乎免费,可做亮点 |
| 查询/读 API 面 | 有 | **占位** | 只有 list_traces;无按 id 取单条/span 查/检索 HTTP/导出 |
| 私有化/数据不出境/单二进制 | 部分 | **有(更强)** | 真优势 |

**残酷事实:"无/占位"占三分之二,且集中在(a)持久化承重墙 (b)eval/dataset/prompt/dashboard 整条产品价值链 (c)我方差异化的真件落地。**

## 2. 红队补的、初稿漏掉的维度（都该进路线）

- **持久化(WAL fsync + 真段落盘)** —— 初稿当成已有,实为最大漏项(见 §0)。
- **多模态 I/O**(图像/音频 trace + 多模态计费);OTel 对多模态实时评测仍无标准 = **空白机会**。
- **流式 / 首 token 延迟(TTFT)+ tokens/sec** —— LLM 核心延迟指标不是总 duration;我方只有 duration_ns。
- **采样 + 保留/TTL + 存储配额** —— 单机磁盘受限,比 SaaS 更刚需;不采样会被一个跑量 agent 写爆。
- **预算/配额/花钱刹车告警** —— 金融客户最在意"超支熔断",不只是 cost spike。
- **终端用户(user_id)维度 + 用户反馈(赞/踩)** —— 与 eval 人工标注是两回事。
- **引擎自监控 /metrics**(吞吐/磁盘/段数/回收延迟,Prometheus) —— 单机客户要监控这台 DB 本身。
- **乱序到达 / 时钟漂移下的树重建正确性** —— multi-agent 跨进程重灾区。
- **至少一次投递 + 幂等摄入** —— 我方确定性 event_id 天生吃得下,该把它变成卖点(竞品做得烂)。

## 3. 开发优先级（先依赖拓扑，再价值；这是对初稿最大的纠正）

> 初稿优先级公式 =(贴定位×竞品刚性)÷工作量,**漏了"依赖关系"**,把 eval 排在它依赖(落盘/数据模型/数据集)前面 = 给沙堡排装修。下面**先按依赖排,再按价值排**。

### 第 0 层 — 承重墙（不补,上面全是写进会蒸发的内存）

**[P0] 真持久化:WAL fsync + 段真落盘**
- 为什么:重启不丢数据,是"存储引擎"的定义。eval 分数写回 upgrade、OTLP 落盘、所有产品价值都预设数据已可靠落盘。
- 接在哪:WAL `append_committed` 真 fsync(独立可先做);`InMemorySegmentStore`→ 真段格式(Vortex,需先定型,见引擎决策文档)。崩溃测试改成真进程 kill 而非 `simulate_crash_lose_memtable`。
- 提醒:WAL fsync 可立刻做;真段落盘绑 Vortex 决策,稍大。
- **进展(2026-06-22)—— 承重墙已浇,重启不丢已闭合**:三层都落盘了。① WAL fsync(早先已做);② **段落盘** `FileSegmentStore`(每段一文件 `[crc32][payload]`,原子写 tmp+fsync+rename,损坏当空段);③ **manifest 持久化** `persist.rs`(段集合+删除位图+upgrade 补写块+水位+epoch+id 计数器,原子写,crc 守门)。`open_durable(dir)` 一个目录管全套。**关键测试 `flush_then_restart_survives_via_durable_segments_and_manifest`**:flush 推进水位(WAL 不再重放那段)→ 删一行 → 丢整个引擎 → 重开 → 数据从持久段读回、删除还在、段 id 不复用 —— 正是 WAL-only 补不上的洞。崩溃测试仍是 drop 引擎(非真 kill 进程),但段/manifest/WAL 都过 fsync。**仍缺:Vortex 列式格式(现行式,需单独决定加依赖)、manifest 增量写(现全量)。** SpanFields 序列化统一到 `yt_wal::encode_span_fields` 一份(WAL/段/manifest 复用)。

### 第 1 层 — 入口与底线（无下游依赖，可与第 0 层并行）

**[P0] OTLP / OpenInference 摄入端点** —— 解除生态绝缘。客户现有埋点灌不进来,eval 评的是空气。**比 eval 更早做**(没数据进来谈什么评分)。HTTP 服务已在,加 endpoint + `gen_ai.*`→我方 span 的映射(做成可演进 adapter,OTel GenAI 部分仍 experimental)。
- **进展(2026-06-22)**:**OTLP/HTTP 入口已做**。`parse_otlp_traces`/`ingest_otlp`(`otlp.rs`)把 OTLP/HTTP JSON 映射成 WireRecord,认 GenAI(`gen_ai.*`)+ OpenInference(`llm.*`/`input.value`)两套约定;一条 OTLP span 拆 SpanStart+SpanEnd,128/64 位 hex id 取低位、原 hex 作去重身份;暴露在标准端点 `POST /v1/traces`。8 个测试(适配器 5 + 端到端 + 2 路由)。**还差:OTLP gRPC 版、protobuf 二进制(现仅 JSON)、resource/scope 级属性继承到 span。**

**[P0] PII 脱敏 / 敏感数据卫生** —— 金融政企**签单门槛**,在摄入侧、零下游依赖,可与落盘并行。默认正则(身份证/银行卡/手机号)+ 可配 callback + 对 LLM 消息字段豁免(抄 Logfire 成熟做法)。**初稿把它排第 5 步与"刚需"自相矛盾,上提。**

**[P0-验证] 真 BM25/graph_index 的窄场景验证(不是全量工程)** —— 唯一技术差异化,但也是**最可能翻车点**(过滤下推目前整个被绕过)。先在一个真实窄场景把"带过滤召回能不能拉回来"跑通再投全量(你们自己标的 C 风险:拉不回 → 3-5 人月变 8-10)。**先验证,别梭哈。**
- **进展(2026-06-22)—— graph_index 带过滤召回已验证**:`graph.rs` 写了真图式 ANN(NSW,非暴力 L2 占位),实测两种过滤策略:稀疏谓词(命中集 67/800)下 **post-filter 召回 0.50 vs in-graph(进图过滤,ACORN 思路)召回 1.00**。结论:**进图过滤确实把 post-filter 丢掉的召回救回来了**,红队 C 风险("拉不回")在这个窄场景被证伪。会失败的测试 `in_graph_filter_recovers_recall_that_post_filter_loses` 兜住、确定性可复算。**仍是验证级**(单层 NSW、无量化/SIMD、std-only),真上量换团队 graph_index 的 C ABI。
- **进展(2026-06-22)—— BM25 中文倒排已验证**:`bm25.rs` = 真倒排 + BM25 打分,中文用**无词典 CJK bigram** 分词。实测查"盗刷风控"非连续多概念串,真 BM25 按 tf-idf 召回排序、**子串占位一条都召不回**(`bm25_ranks_by_relevance_where_substring_returns_nothing`)。**两块真索引(BM25+graph)已设为引擎默认**,既有端到端检索测试照常通过。验证级(bigram 非 jieba 词级);真上量换团队 FFI。**至此 #1(核心 IP 验证)两半都做完;剩 #2 段/manifest 落盘。**

### 第 2 层 — 数据模型前置（eval/成本/session 的共同地基）

**[P1] SpanFields 扩字段:input/output 文本、agent_name、tool_name、model、session_id、user_id**
- 为什么:这是 **eval(judge 要评 input/output 文本)、成本下钻(per-agent/tool/model)、session、终端用户** 四件事的共同前置。改 SpanFields + wire 协议 + SDK 三层。**初稿没单列这个前置,导致把 eval/成本估得过轻——必须先做。**
- **进展(2026-06-21)**:引擎侧已落地 `session_id/agent_name/tool_name/model/input_text/output_text/eval_score/eval_label`,贯穿折叠/WAL 编解码/wire 解析/upgrade 补写。**SDK 侧(Python/TS)设值也已接上**:`tracer.trace(name, session_id=...)` 把 session 透传到全部 span(含嵌套)、span 上 `set_agent/set_tool/set_model/set_io`(输入输出文本)。eval_score/label 不走摄入(服务端算)。user_id 暂缓。

### 第 3 层 — 产品价值层（依赖第 0/1/2 层就位）

**[P1] Eval 闭环(命门,但不是第一个动工)** —— LLM-judge + code/正则 scorer + 在线评分,分数走 **upgrade 机制写回**(王牌:upgrade 块天生为"trace 后补字段"设计,score 就是补写,`read_spans_applies_upgrade` 已验证)。
- 依赖:第 0 层落盘 + 第 2 层文本字段 + 出站 LLM client(代码里还没有任何 HTTP 出站)。**先做一个不需 LLM 的正则/code scorer 探路**,降依赖。
- **进展(2026-06-21)**:**探路闭环已跑通**——`Scorer` trait + `KeywordScorer`(规则版,不依赖 LLM)+ `eval_and_writeback(scorer, q)`:"存→评→分数走 upgrade 写回→读回折叠进 `eval_score/eval_label`"主链有两个会失败的测试兜住。**还差:LLM-judge impl(需出站 HTTP client)、在线评分/采样、人工标注。**
- 本地 judge:数据不出境只能本地评,是差异化;但**有硬件可行性硬伤**(单机已跑 agent+引擎,再塞个够格的 judge 模型可能挤垮业务)—— 降为"可选/需评估",不当确定性卖点。

**[P1] Datasets + 生产 trace→数据集** —— eval 的燃料;用我方语义召回"捞相似失败 trace 入集"是别人做不了的差异化做法。
- **进展(2026-06-22)**:**已做**。`collect_into_dataset`(按谓词采集 span 成命名集,典型 `eval_score==Some(0)` 收失败样本;存冻结 span 快照)+ `eval_dataset`(对集现跑 scorer 出通过率看板,回归基准)+ `dataset`/`list_datasets`。1 个端到端测试(收集→去重→回归→修好 scorer 通过率回升)。**还差:接 `search_similar` 做"语义捞相似失败入集"的差异化采集(需真 embedding);数据集持久化(现在内存态,随进程没);CSV/JSONL 导入导出。**

**[P1] Session 扶正 + 成本下钻** —— 数据模型字段就位后,聚合加 session/agent/tool 维度(会话级/agent 级成本、轮数)。复用 `list_traces_rolls_up_tokens`。

**[P1] agent 专属差异化首发(护城河,自家 dogfood)** —— 执行图(树→DAG)+ 死循环检测 + tool 一等视图。用 AgenticData(SuperAgent/ReAct)当现成 dogfood,把 ReAct 的 thought/action/observation 建成一等 span,做到别人抄不动的细。

### 第 4 层 — P2（以后再说）

Dashboard 全家桶 / 告警(先做预算熔断那条)、Prompt 管理整套(偏离 trace 引擎定位)、CI/CD 回归门禁(eval+dataset 的下游终点)、漂移检测(依赖数据量+分数)、多轮 simulation、语义聚类失败归并(中期护城河)、轨迹回放(append 模型几乎免费 → 可在第 3 层顺手做的 demo 杀手锏,不必无限后置)。

## 4. 明确不做 / 暂不做（诚实清单）

- **通用 RBAC/ABAC/多租户/SSO/SAML/SCIM**:单机私有化部署边界即隔离边界;多租户是 SaaS 摊成本的需求。真签单按客户等保具体要求定制,别提前造通用多租户。
- **TLS/落盘加密**:暂缓,但比 RBAC 更接近"签单前必须有"。**架构预留可插拔加密点**,免返工。
- **20+ 框架自动埋点全家桶**:打不过广度。只做 **OTLP 标准入口**蹭生态 + 自家 SDK 埋透自家 agent。
- **Gateway/Proxy 模式(Helicone 路线)**:另一种产品形态,Helicone 自己都进维护模式,不碰。
- **训练栈集成 / 数百模型 dashboard / 按量计费倾向**:单机私有化无关或做不了,license 制别引入按量复杂度。

一句话:**不做"通用 SaaS 可观测平台的单机克隆"。**

## 5. 差异化往哪挖护城河（SaaS 结构上抄不动的三条 + 一条）

1. **中文检索 + 自有语义召回 → 升级成 agent 调试的语义层**:语义召回相似失败 trace 自动凑回归集、生产流量语义聚类失败主题(我方有现成 ANN,做这个结构成本最低)、中文 RAG 检索可观测。**前提:BM25/graph 从 mock 变真件。**
2. **存储协议硬功夫 → 转产品保证**:确定性 event_id + 崩溃幂等 + 快照隔离 → "金融审计级不重不漏账本""崩溃后成本不算两遍"。**前提:先真落盘,否则是 PPT。** upgrade 机制是隐藏王牌(eval 分数/人工标注/PII 标记全复用它,免新存储设计)。
3. **自家 agent dogfood(AgenticData)**:把 agent 专属功能做到别人抄不动的细;"我们自己的 agent 就靠它在生产跑"对中国客户比任何 benchmark 都有说服力——SaaS 竞品没有自己的旗舰 agent,给不了这个可信度。
4. **(潜在,有硬件前提)本地 LLM-judge**:数据不出境的 eval 只能本地做,SaaS judge 天然违规 → 把 eval 和私有化焊死。但需评估客户机器能否同时跑 agent+引擎+judge。

## 6. 建议下一步顺序（依赖拓扑修正版）

1. **WAL fsync(立即,独立)** —— 先让重启不丢已确认数据,把"存储引擎"坐实。
2. **OTLP 入口 + PII 脱敏(并行,摄入侧,无下游依赖)** —— 解生态绝缘 + 过签单门槛。
3. **BM25/graph 窄场景验证召回(并行,降风险)** —— 验证唯一差异化别翻车,过了再排全量工程。
4. **SpanFields 扩字段(eval/成本/session/user 的共同前置)** —— 一步解锁后面三件。
5. **Eval 最小闭环(先 code/正则 scorer 探路 → 再 LLM-judge,分数走 upgrade 写回)** —— 把存储引擎变成"能改进 agent 的产品",命门补上。
6. 随后:Datasets(语义捞失败入集)→ session/成本下钻 → agent 执行图(dogfood)→ 轨迹回放(顺手亮点)。

刻意往后:Prompt 全套、CI 门禁、dashboard 全家桶、simulation、自动改进闭环 —— 等"落盘→入口→检索→eval→数据集"主链跑通 + 有真实客户反馈再排。

## 7. 修订对账（本稿相对工作流初稿改了什么、为什么）

| 项 | 初稿 | 本稿 | 触发 |
|---|---|---|---|
| 存储定性 | "世界级地基" | "算法对、但全在内存、**重启全丢**;持久化是承重墙未浇" | 红队2:WAL 不 fsync、段在内存、崩溃测试只丢内存表 |
| 持久化优先级 | 未列(当已有) | **P0 第 0 层**,先于一切 | 红队2:在不能持久化的原型上排 eval = 给沙堡排装修 |
| 检索差异化 | "有(半成品)" | **占位 mock**:BM25=子串、graph=暴力、过滤下推被绕过 | 红队1:`search_*` filter 写死 `\|_,_\| true` |
| per-agent 成本/session/state diff | "半成品(底层有)" | **无**:SpanFields 缺 agent/tool/model/session/文本字段 | 红队1 读 SpanFields 只有 6 字段 |
| 数据模型扩字段 | 未单列 | **P1 第 2 层,eval/成本/session 的共同前置** | 红队1/2:eval 被低估,缺前置 |
| OTLP vs eval 先后 | 并列 P0 | **OTLP 先于 eval**(没数据评空气) | 红队2 依赖拓扑 |
| PII 脱敏 | 第 5 步 | **上提 P0**(签单门槛、摄入侧无依赖) | 两路红队:与"刚需"自相矛盾 |
| Eval"最该第一个做" | 第 1 步 | **第 5 步**(依赖最多:落盘+文本字段+出站LLM+数据集) | 红队2:命门≠第一个动工 |
| 本地 judge 护城河 | 确定性卖点 | **可选/需评估**(单机硬件能否再塞 judge 模型) | 红队2 硬件可行性 |
| 缺失维度 | —— | 补:多模态/流式TTFT/采样保留TTL/预算告警/终端用户+反馈/自监控metrics/乱序时钟/幂等投递 | 两路红队 |
| upgrade 机制=王牌 | 有 | **保留**(红队核码属实:UpgradeColChunk 真存在、read 路径已验证) | 红队2 认同 |
| 不做通用克隆 | 有 | **保留**(红队认同方向对) | 两路红队认同 |

> 一句话给老板:**别再被"地基世界级"误导——地基也还在内存里,重启就没。下一步真正的 P0 是先让数据落盘 + 打通 OTLP 入口 + 过 PII 门槛,再盖 eval 这层楼;而唯一的差异化(中文/语义召回)现在还是 mock,要先窄验证别翻车。eval 是命门但依赖最多,排第五不是第一。**
