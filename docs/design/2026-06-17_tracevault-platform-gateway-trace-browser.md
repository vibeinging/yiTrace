# yiTrace 平台层（一）：Platform Gateway + Trace 浏览器

> 日期：2026-06-17｜配套 schema / ingestion / scheduling。
> 平台层第一块：① **Platform Gateway**（UI↔引擎的唯一契约，所有平台模块都经它）；② **Trace 浏览器**（桌面赌注，最高优先级模块）。
> 前端栈复用 AgenticData webui：**Vue3 + Vue Flow + dagre（树/DAG）+ ECharts（图表）+ CodeMirror + virtual scroll**。

## 1. Platform Gateway（契约）
**原则**：UI 永不直连引擎（不写裸 SQL）。Gateway = 一个 Rust 服务（gRPC + REST），承载租户上下文/认证/配额/审计，把 UI 请求翻成引擎 SQL/函数。海量 trace 进 yiTrace；平台元数据（租户/权限/项目/数据集/告警规则/审计）进**内嵌 openGauss metastore**。

```
QueryAPI(读)
  q.searchTraces(filter, sort, page)      → trace 摘要列表          走 ix_cur_recent + 倒排/真列/GIN
  q.getTraceTree(traceId)                 → [pre,post] 物化树        走 ix_cur_subtree (BETWEEN)
  q.getSpan(spanId, fields[])             → span 详情(大字段懒物化)  走 PK + payload_store(按 ref 取)
  q.getThread(threadId)                   → 线程重建会话序列          走 ix_cur_thread
  q.aggregate(metric, groupBy[], range)   → 仪表盘聚合(列式下推)      走 span_current_cold 真列
  q.semanticSearch(text|vec, k, filter)   → 语义找相似 trace(招牌)    走 diskann 带过滤 ANN
  q.scanActiveTraces(filter)              → 运行中 span(活 trace)     走 span_live() 集合函数(参数化)
CommandAPI(写回, 飞轮)
  c.attachScore(targetRef, score, source) → 评估/标注分回写          → fold 机制(事件 type=6 feedback)
  c.tagTrace / c.addToDataset
SubscribeAPI(流)
  s.tailEvents(filter)                    → 增量事件流(活 trace 跟随 / 告警喂数据)
MetaAPI(治理, openGauss metastore, 不进 trace 引擎)
  租户/项目/用户/角色/数据集定义/eval 配置/告警规则/审计日志
```
**横切**：每请求带租户上下文（强制 `tenant_id` 谓词，物理隔离）+ 审计留痕（等保三级）+ per-tenant 配额。

## 2. Trace 浏览器（桌面赌注）

### 2.1 模块与数据来源
```
┌─ trace 列表 ──────────────────────────────────────────────┐
│ 多维过滤(时间/模型/状态/tag/用户/延迟/成本/分数) + 中文搜索  │ q.searchTraces
│ 语义搜索框: 中文自然语言 → 召回相似 trace                    │ q.semanticSearch (招牌)
├─ trace 详情 ─────────────────────────────────────────────┤
│ 树视图 / 瀑布时间线(span 嵌套、耗时条、关键路径高亮)         │ q.getTraceTree([pre,post])
│   ↳ Vue Flow + dagre 渲染; 万级 span 必须虚拟化            │
│ span 详情面板(input/output/metadata/token/cost/工具调用)   │ q.getSpan(fields[]) 懒加载
│   ↳ 大字段晚物化: 点开才拉 payload                         │
│ 中文 input/output diff(两 span/两 trace 对比)              │ 前端 diff + 引擎分词
│ 图片预览(多模态)                                          │ payload external_uri
│ "找相似 trace" 按钮(任一 trace → 语义召回历史同类)          │ q.semanticSearch
│ 活 trace 实时跟随(运行中自动刷新瀑布)                       │ s.tailEvents + q.scanActiveTraces
└──────────────────────────────────────────────────────────┘
```

### 2.2 关键实现点
- **树/瀑布虚拟化（最难）**：一条 trace 可上万 span。引擎给 `[pre,post]/lvl` 物化区间 → **前端只渲染可视窗口的节点**（virtual scroll + 按 pre 范围分页拉子树），不一次拉全树。Vue Flow 渲染 DAG，dagre 布局；瀑布按 start_time/latency 画耗时条。
- **大字段懒物化**：列表/树只回 `*_ref`（不拉 input/output 全文）；用户点开某 span 才 `q.getSpan(['input','output'])` → 引擎按 payload_ref 取 CAS。彻底避免无界大 payload 拉爆前端。
- **中文 diff**：两 span input/output 对比，按**词**（接引擎 `bm25_tokenize`/jieba）而非按字 diff，中文体验更好。
- **语义找相似（招牌差异化）**：选中一条 trace → `q.semanticSearch(该trace的embedding, k, 当前过滤器)` → 带过滤召回语义相似历史 trace（复现 bug/找同类失败/看正例）。这是 SmithDB/Langfuse 给不了的桌面级能力。
- **活 trace 跟随**：运行中 trace（只有 start 无 end）走 `q.scanActiveTraces`（引擎 `span_live()` 从热区事件实时折叠 running 态）+ `s.tailEvents` 推子树增量 → 瀑布自动刷新。

### 2.3 v1 / 后置
| 子功能 | v1 | 后置 |
|---|---|---|
| 列表+多维过滤、树/瀑布虚拟化、span 详情懒加载、中文 diff、图片预览、**语义找相似** | ✅ | |
| 活 trace 只读跟随 | ✅ | 活 trace **在线**语义召回 |
| PDF/音频/视频预览 | | ✅ |

## 3. 后续模块（同 Gateway 模式，下一步细化）
评估框架（LLM-judge+中文模板/中文 RAG 指标/标注队列/数据集/实验对比）· 告警（阈值+国内 IM 企微钉钉飞书+轻量异常检测）· 仪表盘（cost/latency/token/error，`q.aggregate` 列式下推）· 项目/会话/搜索 · RBAC/多租户/审计（默认免费内置）· Prompt/Playground（后置）。

## 4. 待核实 [验]
1. Vue Flow + dagre 在万级节点的渲染性能（需虚拟化 + 按需展开，POC 压测最大 payload/最深 trace）。
2. `s.tailEvents` 流式实现（引擎侧增量推送机制：轮询 `span_live()` vs WAL 订阅，单机轮询足够）。
3. `q.semanticSearch` 端到端延迟（进交互路径，需 SLA + 超时降级到关键词搜）。

> 一句话：Gateway 是 UI↔引擎唯一契约（读/写回/流/治理四类，全部映射到已设计的引擎 SQL/函数）；Trace 浏览器消费它，靠引擎的 `[pre,post]` 区间编码做树虚拟化、`payload_ref` 做大字段懒物化、`diskann 带过滤召回`做语义找相似、`span_live()` 做活 trace 跟随。前端栈复用 AgenticData 现成的 Vue Flow/dagre/ECharts。
