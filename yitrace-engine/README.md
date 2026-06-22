# yitrace-engine

yiTrace 自研单机引擎的代码起点。**这是能编译、能跑测试的骨架,不是成品** —— 把三份设计文档里
最要命的正确性骨架做成了真代码,重活留 `todo!()`。刻意只用标准库、零外部依赖,`cargo check`/`cargo test`
离线就能过。

```
cargo test --offline                              # 84 个测试(含真线程并发压测 + socket HTTP 往返 + 带过滤 ANN 召回实测 + flush 后重启不丢)
cargo run -p yt-engine --example demo --offline  # 灌几条银行风控假 trace,跑写入→折叠→中文搜→找相似→混合召回
cargo run -p yt-engine --example server          # 起 HTTP 摄入服务(8 线程池),curl 即可灌/查
YT_TOKEN=secret cargo run ... --example server   # 开 Bearer token 鉴权
cargo test -p yt-engine --features gzip          # 含 gzip 请求体解压(可选 feature,默认离线 std-only)
#   curl -XPOST localhost:7878/v1/ingest -d '[{"trace_id":7,"span_id":1,"ts":1,"seq":1,"event_type":1,"ext_span_id":"7-1","status":0,"input_tokens":900,"logs":["开始"]}]'
#   curl localhost:7878/v1/traces
#   curl -XPOST localhost:7878/v1/search -d '{"text":"盗刷","k":10,"filter":{"agent_name":"风控","status":1}}'   # 中文搜 + 按 agent/状态过滤
#   curl -XPOST localhost:7878/v1/search -d '{"vector":[0.1,0.2],"k":10}'                # 找相似(纯向量)
#   curl -XPOST localhost:7878/v1/search -d '{"text":"盗刷","vector":[0.1,0.2],"k":10}'   # 混合(关键词+语义 RRF 融合)
```

`demo` 输出示例:读出 1002「疑似盗刷」trace 的完整 span(start+end 折叠成一条)、搜「盗刷」命中、
向量找最近邻、混合召回里关键词指盗刷/向量指反洗钱时 RRF 怎么融。

## 四个 crate

| crate | 干什么 | 对应设计 |
|---|---|---|
| `yt-core` | 核心类型:三类不可变标识、**确定性 event_id**、不可变 Manifest(写时复制)、deletion/upgrade 两个对称的不可变块;**四源折叠算法**(`fold` 模块,纯函数) | hardened §0 共享底座、§M.2、§M.7、草案 2 §D2.2 |
| `yt-manifest` | **整个引擎的正确性脊梁**:读者 pin 协议(先登记再读)、回收水位、RAII 自动注销 | hardened §M.4、草案 2 §D2.1 |
| `yt-wal` | 写前日志:**文件落盘 + fsync**(`Wal::open`)/内存(`Wal::new`);崩溃安全帧(长度+CRC+marker,撕裂尾自动截断);自研二进制编码,零依赖 | hardened §M.6、§7-D |
| `yt-memtable` | 活内存表:**上下界双水位** + 受 gate 的 evict(修「flush 后漏读一截」) | 草案 2 §D2.3 |
| `yt-engine` | 单写者协调器、段五态生命周期、三块外部件的接口边界、四源折叠算子骨架 | 草案 1 §D1、草案 2 §D2.2、决策文档 §2 |

## 测试在验什么(都是真会失败的不变量,不是摆设)

- `event_id` 确定性:同一身份恒得同一 id;seq 或类型不同则 id 不同(折叠去重的根)。
- 回收水位取**最老读者**:旧读者没释放,新版本的段就不能删 —— 这是初稿被红队用 use-after-free
  打穿、修好之后的那条。`pinned_reader_holds_back_safe_version` 就是把那个场景钉成测试。
- 旧读者在**并发删除**下仍看到一致版本(`flush_then_delete_keeps_old_snapshot_consistent`)。
- **flush 回收内存时不漏读旧读者那截**:gate 取所有读者下界的最小值,有旧读者在就不删它的行
  (`evict_gate_protects_old_reader_rows` / `flush_evict_does_not_drop_old_reader_rows`,红队棱镜 B)。
- **被合并掉的旧段只在三条水位都满足时才删文件**:有读者还 pin 着旧版本、或段上还有 buffer pin,
  就不删(`reclaimer_frees_dead_segments_only_when_safe` / `buffer_pin_blocks_reclaim`)。
- WAL 重放只返回已确认、且 watermark 之后的记录;CRC 能查出篡改。
- **四源折叠**:同一事件按 event_id 去重(跨源/重传只算一次)、属性"最后非空值优先"(乱序输入也按 seq
  定先后、null 不抹掉已有值)、日志并集保序去重(`yt_core::fold` 5 个测试)。
- **端到端读一条 trace**:一条 span 的开始事件在段里、结束事件在内存里,`read_spans` 折叠成一条完整 span;
  且尊重 deletion + 快照隔离(老读者仍看到后来被删的行)(`read_spans_folds_segment_and_memtable_end_to_end` /
  `read_spans_respects_deletion_vector`)。
- **崩溃重放幂等**:重启从 WAL 重放重建内存表,即便段与重放重叠,确定性 event_id 去重保证折叠结果
  崩溃前后逐字段一致、事件不算两遍(token/cost 不翻倍)(`crash_recovery_replay_is_idempotent_no_double_fold`)。
- **晚到属性补写(upgrade)**:第四个源,补写非身份属性盖到老 span 上,且尊重快照隔离(升级前的读者看不到)
  (`read_spans_applies_upgrade_and_respects_snapshot`)。读路径现在是真·四源:内存 + 段 + 删除 + 补写。
- **时间窗 + trace 剪枝**:读一条 trace 时,按段 zone-map(min_ts/max_ts) 把时间窗外的段整段跳过、不扫,
  再按 trace_id 行级过滤;`read_spans_query` 返回实际扫了几个段,测试断言扇出收敛
  (`time_window_prunes_segments_and_trace_filter_narrows`)。
- **按内容搜 / 找相似(产品噱头)**:`search_text` 走 BM25 中文检索找命中 span、`search_similar` 走向量近邻找
  相似 span,都折叠成完整 span 返回、保持相关性排序(`search_text_and_vector_find_and_fold_spans`)。
  现在是内存朴素实现,真实 FFI(团队 BM25 + graph_index)按 trait 换进去。
- **混合召回(关键词 + 语义)**:`search_hybrid` 用 RRF 把 BM25 和向量两路排序融成一路,双命中的 span 排更前;
  测试证明它给出单走向量给不出的排序(`hybrid_fusion_beats_single_signal`);RRF 是纯函数单独测(`yt_core::rank`)。

- **compaction 并发重读合并(OPEN-3)**:合并段分两步(`compaction_begin`/`compaction_finish`),选段后、提交前
  并发打到输入段的删除/补写,提交时按当前状态重读合并 —— 删除不丢、补写不丢
  (`compaction_reconciles_concurrent_delete_and_upgrade_open3`)。
- **真·多线程压测**:4 读 + 1 写 + 1 回收 同时跑,且回收会**真删段文件** —— 若回收水位有 bug 过早回收了
  读者还需要的段,该读者读到空、种子 span 不见、断言当场抓住。跑下来不崩、不死锁、种子 span 始终可见
  (`concurrent_readers_writer_reclaimer_stay_consistent`,连跑 5 遍稳定)。这是这套并发设计第一次被真线程验证。
- **内存表自动刷盘**:行数超阈值自动封段,内存被兜住、数据一条不丢(`memtable_auto_flushes_to_bound_memory`,OPEN-2)。
- **SDK→引擎数据契约 + JSON 解析**:`parse_wire_batch`(自带极小 std-only JSON 解析器)把 SDK `to_wire()`
  的 JSON 批量解析成 `WireRecord` → `ingest_wire` → 折叠 → 读回。处理了大整数超 f64 精度(数字按原始字符串
  存)、Python 发数字/TS 发字符串两种、转义引号、中文、null。引擎**自算 event_id**且与跨语言基准一致
  (`ingest_wire_maps_sdk_format_end_to_end` / `parse_wire_batch_then_ingest_reads_back` / wire 模块 3 测试)。
- **OTLP / OpenInference 摄入入口(生态入口)**:`parse_otlp_traces`/`ingest_otlp` 把业界标准的 OpenTelemetry
  trace 导出(OTLP/HTTP JSON)映射成 `WireRecord` —— 任何已用 OTel/OpenInference 埋点的 agent 应用**不改打点**
  就能灌进来。两套语义约定都认:OTel GenAI(`gen_ai.request.model`/`gen_ai.usage.*`)+ OpenInference
  (`llm.model_name`/`llm.token_count.*`/`input.value`)。一条 OTLP span 拆成 SpanStart+SpanEnd 两事件(确定性
  event_id 自然成立),128 位 trace/64 位 span 十六进制 id 取低 64 位、原 hex 作去重身份。HTTP 暴露在 OTLP 标准
  端点 `POST /v1/traces`(`otlp.rs` 5 测试 + `ingest_otlp_end_to_end_folds_genai_span` + `route_otlp_ingest_then_query`)。
- **trace 列表/摘要**:`list_traces` 按 trace 聚合折叠出的 span(span 数、总/最大耗时、报错数、
  **token 汇总**),控制台主视图用(`list_traces_aggregates_per_trace` / `list_traces_rolls_up_tokens`)。
- **token 计数(agent 成本核心)**:span 带输入/输出 token,穿过折叠、按 trace 汇总 —— LLM 成本可观测。
- **会话视图 + per-agent 成本**:span 带 session_id/agent_name/tool_name/model;`list_sessions` 按会话聚合
  (多轮对话视图)、`cost_by_agent` 按 agent 归因成本(`session_and_per_agent_cost_aggregation`)。这四个字段也是
  eval(judge 评 input/output 文本)的前置 —— 已贯穿折叠/WAL/线格式。
- **父子 span 树**:span 带 `parent_span_id`,trace 是棵树(root→child→grandchild);父子链穿过折叠
  保留下来(`parent_span_id_survives_fold_for_tree`)。`load_trace_tree` 把一条 trace 连成树
  (roots + 各节点 children),`dfs_order` 给瀑布视图的深度优先顺序(`load_trace_tree_assembles_parent_child`)。
- **agent 执行图(DAG)**:`agent_graph` 把 span 父子树按 agent/工具维度收拢成"谁调用了谁"——
  角色判定(有 agent_name→Agent;否则 tool_name→Tool;都没有→`span:<id>`),边=父角色→子角色(同角色自环剔除、
  按次数聚合),节点带 span 数/token 聚合。这是 dogfood 自家 SuperAgent 最想看的视图:哪个 agent 调了哪个工具、
  把任务交给了哪个 agent(`agent_graph_collapses_tree_into_caller_callee`)。
- **评测(eval 闭环)**:把产品从"看 trace"推到"评 trace"。span 带 `input_text/output_text`(judge 的评测对象)
  + `eval_score`(千分制)/`eval_label`。`eval_and_writeback(scorer, q)` 跑 scorer 打分,**分数走 upgrade
  (晚到补写)通道写回** —— 评测分本质就是"trace 事后才有的字段",与晚到属性同构,直接复用 upgrade 王牌、
  不另起存储。现在是不依赖 LLM 的规则 scorer(`KeywordScorer`),换 LLM-judge/本地裁判时闭环骨架不变。
  ("存→评→写回→读分"主链、无文本的 span 跳过:`eval_scores_written_back_via_upgrade_and_read_again` /
  `scorer_skips_spans_without_output_text`)。`eval_summary` 把打分后的 span 聚合成**通过率/均分**(整体 +
  per-agent),就是 eval 的产品出口:回归视图"哪个 agent 退步了"(`eval_summary_aggregates_pass_rate_overall_and_per_agent`)。
- **评测数据集(Datasets,eval 的燃料)**:`collect_into_dataset` 按谓词把生产 span 采集成命名数据集
  (典型:`|s| s.eval_score==Some(0)` 收集失败样本;或配 `search_similar` 先捞"相似失败 trace"再入集——
  中文/语义召回的差异化用法),存的是**冻结的 span 快照**(底层 trace 被合并/回收也不影响基准)。`eval_dataset`
  对数据集现跑 scorer 出通过率看板——**同集同 scorer 反复跑,通过率掉了就是退步**,这是回归基准。
  ("评→收集失败样本→回归重跑"闭环、去重、修好 scorer 后通过率回升:`dataset_collect_failures_then_eval_regression`)。

> 加固设计时红队挑出的**四个 bug,现在都有代码 + 会失败的测试兜住**:① GC 过早 use-after-free(回收三水位)、
> ② flush 后漏读一截(内存双水位 gate)、③ 删除/版本串味(快照隔离)、④ 崩溃重放算两遍(确定性 event_id)。
> 加上加固设计的 OPEN-3(compaction 重读合并)也已实现 + 测试。

## 真实实现里要换的外部件(注释里都标了)

- `RwLock<Arc<Manifest>>` + `Mutex<Vec<slot>>` → `arc-swap` + `crossbeam-epoch`(无锁原子换指针 + 无锁纪元回收)。**但"先登记后解引用"的次序在骨架里是忠实的——次序才是正确性。**
- `DeletionVec` 位集 → `roaring`;CRC 手写 → `crc32fast`。
- `SegmentStore` → **Vortex** 列式不可变段。
- `Bm25Index` → 团队自有 BM25(cppjieba 分词 FFI + Rust 重写倒排 + block-max-WAND)。
- `GraphIndex` → 团队自有 graph_index(算法/距离/PQ 经 C ABI FFI 复用);**带过滤 ANN 是半成品,PoC C 要先验进图过滤能否把召回拉回来**。
- `MergeOnReadExec` → DataFusion 的 `ExecutionPlan`。

## 还没做(对应 Phase 0 清单与 8 个 OPEN)

- 活 MemTable 改成真正的不可变 ring + 并发安全发布(现在是 `VecDeque` 骨架);OPEN-8 的
  「live_lsn 与 retained_watermark 两步取值非原子」还没验。
- **持久化已闭合(重启不丢)**:三层都落盘了——① WAL fsync;② **段落盘** `FileSegmentStore`(每段一文件
  `[crc32][payload]`,原子写 tmp+fsync+rename,损坏当空段);③ **manifest 持久化** `persist.rs`(段集合 +
  各段删除位图/upgrade 补写块 + 水位 + epoch + id 计数器,原子写,crc 守门)。`open_durable(dir)` 一个目录管全套,
  重启 = 从 manifest 重建段集合(指向盘上段文件)+ WAL 重放水位之后的尾巴。
  **关键测试 `flush_then_restart_survives_via_durable_segments_and_manifest`**:flush 推进水位(WAL 不再重放那段)
  → 删一行 → 丢掉整个引擎 → 重开 → 数据从持久段读回、删除也还在、段 id 不复用。这正是 WAL-only 补不上的洞。
  SpanFields 序列化全靠 `yt_wal::encode_span_fields` 一份(WAL/段/manifest 复用,不抄多份)。
  仍缺:列式格式(现行式;Vortex 替换需单独决定加依赖)、manifest 增量写(现每次 commit 全量写)。
- 回收器接成真正的后台线程 + IO 限速(现在是手动调 `reclaim()`);GC 条件(3) 接真持久化 metastore
  (现在用当前内存 manifest 近似)。
- 内存表已有自动刷盘上界,但**长读者**仍会把旧版本/删除位图钉在内存(OPEN-1/7):还差快照租约
  (长读者超时强制失效)+ deletion/upgrade 历史块版本回收。
- 读路径四源 + 时间窗/trace 剪枝已通(`read_spans_query`),但还差:段**行级**时间过滤(现在时间窗是段粒度,
  段内不再按行筛 ts)、按 trace_id 的段级 zone-map(现在 trace 过滤是行级)、DataFusion `ExecutionPlan` 接入。
- **检索两块已从占位升级成"验证级真实现"并设为引擎默认**(不再是子串/暴力 L2):
  - `Bm25TextIndex`(`bm25.rs`)= 真倒排 + BM25 打分,中文用**无词典 CJK bigram** 分词。实测查"盗刷风控"这种
    非连续多概念串,真 BM25 按 tf-idf 召回并排序、子串占位一条都召不回(`bm25_ranks_by_relevance_where_substring_returns_nothing`)。
  - `GraphAnnIndex`(`graph.rs`)= 真图式 ANN(NSW),**带过滤召回已实测**:稀疏谓词下 post-filter 0.50 vs
    in-graph(进图过滤)1.00 —— 红队最大翻车点"过滤拉不回召回"被证伪(`in_graph_filter_recovers_recall_that_post_filter_loses`)。
  - 仍是验证级(bigram 非 jieba 词级、单层 NSW 无量化/SIMD、std-only),真上量换团队 FFI(同套算法);`InMemorySegmentStore`→落盘/Vortex 段。
- **端到端跨进程闭环已通**:Python(`HttpExporter`/urllib)与 TS(`HttpExporter`/fetch)SDK 打点 → POST →
  Rust 服务 → 折叠 → `GET /v1/traces` 查回,token/嵌套/中文全对(已实测两种语言)。
- **HTTP server 性能/安全已加固**:① 8 线程池 accept(`serve_pool`,不被并发连接打爆);② gzip 请求体
  解压(`--features gzip`,带防 gzip 炸弹上限);③ **Bearer token 鉴权**、**请求体上限堵 OOM**(巨大
  Content-Length → 413 不预分配)、**审计留痕**(每请求一条)。curl 实测 401/200/413 + 审计全对。
- **安全还差(金融政企/等保三级硬门)**:TLS、RBAC/多租户物理隔离、落盘加密、持久防篡改审计日志(现在
  只打 stderr)、限流/慢连接超时、PII 脱敏。吞吐协议层不是瓶颈(实测解析+灌入 ~48万事件/秒,余量 ~139×);
  要更快先上 gzip(已)+ 异步多线程(已),不用换协议;**OTLP/HTTP 入口已做**(`POST /v1/traces`),gRPC 版留作未来。
- SDK↔引擎契约已对齐:event_id 三方一致、嵌套父子(`parent_span_id`)、token(`set_tokens`/`setTokens`)
  三者都贯通 SDK→线格式→`WireRecord`→折叠→`list_traces` 汇总。只差网络管道(HTTP 网关)。
- **进图过滤已接到引擎层 + 按产品维度过滤**:
  - 底层 `search_similar_filtered`/`search_hybrid_filtered` 把 `(trace,span)` 谓词**下推进 `graph.search`**
    (向量侧走验证过的进图过滤),实测排除"更近但不满足"的点(`filtered_similar_search_pushes_predicate_into_graph`)。
  - 上层 `search_similar_attr`/`search_hybrid_attr` 接受 `SearchFilter`(agent / status / 时间窗 / trace),翻成
    谓词下推 —— 这才是"带过滤 ANN"在真实查询里的样子:"找 agent X 报错的相似 span"。靠摄入时建的**属性边车**
    (每 span 的 status/agent/ts,last-non-null)做 payload(`attr_filtered_search_filters_by_agent_status_and_time`)。
- **检索索引重启重建已闭合**:`recover` 现在一并重建三个索引——BM25 + 属性边车是**派生数据**,扫持久段(水位前)
  + 重放 WAL 尾(水位后)各喂一次重建;**向量段里推不出来**,走**独立向量段文件** `vecstore.rs`(`open_durable` 下
  `index_embedding` 追加写 `vectors.dat`,recover 重载喂回图)。测试 `search_indexes_rebuilt_after_restart`:重启后
  按内容搜/找相似/按 agent 过滤全部照常。喂索引的逻辑统一到 `index_record` 一份(ingest/WAL 重放/段重建共用)。
- **检索只折叠命中行已做**:`join_folded` 把命中 key 集喂给 `fold_query(keys=Some(...))`,只折叠命中的 (trace,span)、
  不再先折叠全库再挑(`search_folds_only_hit_rows_across_sources`:命中 span 仍跨段+内存正确折叠,噪声 span 不进结果)。
  段扫描仍是全段(行级行指针待真实索引/Vortex 的 zone-map + 行选择)。
- 检索还差:BM25 升级 jieba 词级;属性边车字段可扩(model/tool/session);段内行级行指针(免全段扫描)。
