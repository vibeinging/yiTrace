I have a complete, reviewed corpus: four full architecture proposals (A/B/C/D) plus a three-perspective jury verdict. My task as chief architect is synthesis, not new research. I'll produce the final recommended architecture directly.

The jury split is the crux: the commercial-viability judge picks A (9), while the technical-reliability judge picks B/D (8/8, A=6) and the moat/team-reuse judge picks D (9, A=7). I need to resolve this honestly rather than just averaging. Let me write the final design.

---

# 最终推荐架构 —— **yiTrace**：单机商业版 Agent Trace 数据库

> 首席架构师终裁。对标 LangChain SmithDB（2026-05，对象存储 + 无状态三服务形态）。
> 定位：国内首个「Agent 可观测性专用、纯单机、私有化、原生中文、向量飞轮内建」的商业数据库。
> 终裁方法：不取四方案平均，而是按「**先赢得交付与合规，再赢得性能与护城河**」的商业优先级，做一次**分层路线裁决**——主线走自研 Rust LSM 内核（D），但用「openGauss/信创交付外壳 + Lance 提速 MVP」把 A 的上市速度与合规优势嫁接进来，把 D 的「上市慢、投入重」这唯一短板补平。

---

## 第一部分：主路线裁决与硬核理由

### 1.1 评审团分歧的本质与裁决

三位评审的首选不一致，这恰恰指明了真正的决策点：

| 评审视角 | 首选 | 对 A 评分 | 对 D 评分 | 核心主张 |
|---|---|---|---|---|
| 商业上市速度与成本 | **A** (9) | 9 | 7 | openGauss 复用 → 上市最快、信创合规是一票否决项的护城河 |
| 技术可靠性与单机性能 | **B/D** (8/8) | 6 | 8 | 自研 LSM + 列式不可变 segment 是与 trace 负载匹配最干净的形态，A 的性能天花板 + 第二套 WAL 自我抵消了「复用即可靠」 |
| 差异化护城河与团队复用 | **D** (9) | 7 | 9 | 飞轮五原语 + 语义召回是 SmithDB 结构性空档，D 把它做成一等体系；团队 DiskANN/页面整理框架命中率最高 |

**关键洞察**：A 赢在「交付与合规」这两个**与存储引擎正交**的维度——它们其实是**部署形态与适配工程**，不是**内核形态**。而 A 输掉的「单机性能天花板」与「向量融合度弱」却是**内核形态决定的、事后改不动的**。

> 因此终裁的核心判断是：**A 的优势可以嫁接，A 的劣势不可逆;D 的劣势可以补,D 的优势不可替代。**
> - A 的上市速度优势 = 「复用成熟件 + 不自研列存格式」→ 可用 **Lance 提速 MVP**（D 已预留此路径）嫁接，把 D 的 MVP 拉到与 A 同一量级。
> - A 的合规/信创优势 = 「国产内核背书 + Mulan PSL」→ 可用 **openGauss 作为可选 metastore + 信创适配层 + 优化器代价模型移植**嫁接，把「国产化身份证」装进 D 的交付外壳，**而不必让 openGauss 决定存储引擎形态**。
> - D 的性能上限与向量飞轮护城河 = 「自研 Rust LSM + 向量原生融合 DataFusion」→ **内核形态决定，A 路线永远追不上**。

### 1.2 终裁：主线 = 方案 D（自研 Rust LSM 内核），嫁接 A 的交付与合规、B/C 的工程纪律

**选定主路线：自研 Rust LSM 存储内核（行式热区 + 列式不可变 segment）+ DataFusion 执行 + Lance/Vortex 列式格式 + tantivy 中文全文 + 团队 DiskANN 向量层 + 数据飞轮原语。形态 = 嵌入式 Rust 内核 + 单机服务外壳。**

五维对照终裁结论：

| 维度 | 终裁后判断 | 说明 |
|---|---|---|
| **单机性能** | **强（上限最高）** | 自研 LSM + 列式 + 本地 NVMe/mmap 是与 trace 负载（高频/碎片化/乱序/晚到/活 trace + 列式聚合）匹配最干净的形态，砍掉 SmithDB 全部对象存储读放大；A 的 PG 行存/MVCC/第二套轻量 WAL 是结构性天花板，不可逆 |
| **上市速度** | **中→强（嫁接后）** | D 原生 ~9 人月 MVP 已快于纯自研；**嫁接 Lance 提速路径后**（Lance 内置中文 jieba + ANN 直接抵掉两块自研），MVP 可压到与 A 同量级（~6-7 人月日历），补平 D 唯一短板 |
| **私有化契合** | **极强** | 单静态二进制 + 本地盘零外部依赖 + 物理目录多租户 + per-tenant 加密；正面打击 Langfuse 式 4-6 组件重运维 |
| **差异化护城河** | **极强** | 原生中文 + 语义 trace 召回（SmithDB 架构补不上）+ 飞轮五原语 + 原生 SQL；复制成本对 SmithDB/ClickHouse 派均为 1-2 年 |
| **复用团队能力** | **极强，几乎零知识断层** | DiskANN/HNSW/IVF 成招牌、页面整理框架管 segment+向量段 compaction、PG 内核经验直迁 WAL/MVCC/JSON GIN/中文/metastore、优化器做 filtered ANN |

**为什么不选 A 做主线**（对决策者诚实交代）：A 在「上市速度 + 合规」上确实领先，但这两项可被嫁接补足；而 A 用 PG 内核换来的**性能天花板**与**向量外挂的弱融合**是**内核级、不可逆**的——一旦 PoC 不达标，A 的兜底是「向列存 AM 深度自研滑动」，即**事实上滑回 D 的工程量**，却已先背了 PG 内核耦合的包袱。**与其先选 A 再被迫滑向 D，不如直接选 D 并把 A 的可嫁接优势拿来。**

**为什么不选 B/C**：B 与 D 技术形态几乎同构，D 胜在风险对冲更彻底（Vortex 0.36+ 格式向后兼容已核实拆雷、Lance 现成热备、内嵌 SQLite metastore 更轻、向量飞轮体系化更深）；C 用外壳绕 DuckDB 三条硬伤（FTS 无中文 hook、vss 持久化实验性、单写者全局串行）导致底座只剩列存扫描、三套 WAL/索引一致性是四方案最复杂，且 openGauss 这块最深资产闲置。

---

## 第二部分：从其它方案嫁接的最佳点子（终裁采纳清单）

按商业价值与去风险价值排序，明确「从哪来、嫁接到哪、为什么」：

| # | 嫁接点 | 来源 | 落地位置 | 价值 |
|---|---|---|---|---|
| **G1** | **Lance 作 MVP 提速主路径 + Vortex 作性能演进路径** | B/D | §3 存储格式层 | Lance 内置 jieba/lindera 中文 + 内置 ANN，M0-M1 直接抵掉「中文全文 + 向量」两块自研 → **把 D 的上市速度拉到 A 量级**，这是补平 D 唯一短板的关键 |
| **G2** | **信创/openGauss 合规嫁接** | A | §7 部署形态 + §6 metastore | 大客户档「可选内嵌 openGauss 作 metastore」+ 国产 OS/CPU(鲲鹏/飞腾/海光)适配 + **移植 openGauss 优化器代价模型** → 补 D 相对 A 最弱的商务护城河，拿信创采购「身份证」，**但不让 openGauss 决定存储引擎** |
| **G3** | **数据飞轮四原语 + 奖励物化视图** | D（强化）/B | §10 | export_trajectory / 奖励信号增量物化视图 / SOP 抽取 / 活 trace 推流 → 产品从「可观测性存储」升级为「参与 agent 推理回路的飞轮引擎」，是定价锚点与最值钱的签单故事 |
| **G4** | **SegmentFormat trait 格式抽象层** | C/B | §3 | 把「押注 Vortex」降级为「可切换格式策略」，Lance↔Vortex↔自研段透明切换，缓解 D 最大风险（新格式成熟度），保留向后滑动能力 |
| **G5** | **manifest 原子切换统一发布多索引可见性（一等正确性纪律）** | C/B/D | §6 崩溃恢复 | flush 严格顺序「写 segment→建倒排→建向量→最后原子改 manifest→截断 WAL」，任一步崩溃则重放 WAL 整批重做 → 根治「列存有数据但倒排没建好」的多源 merge-on-read 正确性风险，是售后成本最大的隐患 |
| **G6** | **可选「logged 零丢失」一致性档（按 SLA 的部署旋钮）** | A | §6 | 对金融/政企强 SLA 客户提供「热区 logged、走完整 WAL 零丢失」档，与默认「组提交轻量 WAL」并存 → 一致性级别成为可选旋钮 |
| **G7** | **外壳攒批削峰 = 把高频小写转成底座舒适区批量写** | C | §2 写路径 | 组提交 WAL + 无锁分片 MemTable + 批量 flush + per-tenant 写队列令牌桶削峰 → 提升单机写吞吐稳定性与 P99 |
| **G8** | **PoC 门禁 = 把每个复用组件的能力边界用可证伪实验钉死** | C/B | §11 路线图 M0 | M0 验收清单：Vortex 乱序 span 写放大/合并抖动、filtered ANN 标量谓词下真实延迟，不达标即触发 SegmentFormat 降级到 Lance |
| **G9** | **多租户混合分档：海量小租户共享文件+RLS，大客户独占目录** | C | §5 | 比单一目录隔离更能覆盖租户数极多的私有化场景 |
| **G10** | **embedding 旁路捕获 + 采样** | B/C/D | §8/§10 | 旁路捕获客户已有 embedding API 调用 + 异步管线 + 按租户采样 → 控成本，保住「私有化低成本」卖点 |
| **G11** | **dotted_order 作一等列 + 区间编码互为冗余加速** | A | §1 | LangSmith 生态兼容更顺滑（前缀匹配语义直读），与区间编码互补 |

---

## 第三部分：完整单机架构

### 3.0 整体形态

```
                         yiTrace 单进程（多线程池，嵌入式内核 + 服务外壳）
   ┌──────────────────────────────────────────────────────────────────────────┐
   │ 接入层  OTLP/gRPC · OpenInference · LangSmith-compat REST · 原生 SQL(PG 线协议) │
   ├──────────────────────────────────────────────────────────────────────────┤
   │ 租户路由 + 认证 + 配额  (per-tenant 调度器 / 令牌桶 / 行级权限 RLS)            │
   ├──────────────┬──────────────┬──────────────┬───────────────┬──────────────┤
   │ 写线程池      │ 查询线程池     │ compaction池  │ embedding/飞轮池 │ 备份/TTL 池   │
   │ WAL→MemTable │ DataFusion执行 │ 时间分层合并   │ 异步向量化/导出   │ 增量快照/保留  │
   ├──────────────┴──────────────┴──────────────┴───────────────┴──────────────┤
   │                  统一存储引擎（自研 Rust LSM 内核）                            │
   │  ┌────────────┐   flush(批量)   ┌────────────────────────────────────────┐  │
   │  │ 行式热区     │ ─────────────► │ 列式不可变 Segment(Lance主/Vortex备/可切换) │  │
   │  │ MemTable    │                │  核心列 · 大字段子文件 · 内嵌索引          │  │
   │  │ 可原地更新/折叠│                │  [zone-map][tantivy倒排][JSON路径][向量段] │  │
   │  └────────────┘                └────────────────────────────────────────┘  │
   │  Manifest/Metastore：内嵌 SQLite(默认) / 可选内嵌 openGauss(大客户·信创档)     │
   │  SegmentFormat trait：Lance ↔ Vortex ↔ 自研段，透明切换                       │
   └──────────────────────────────────────────────────────────────────────────┘
        落盘：本地 NVMe（默认）│ 可选挂 MinIO/S3（object_store crate，零改码）
```

形态 = **嵌入式 Rust 内核（自带存储/WAL/事务，DuckDB 式 in-process）+ 单机服务外壳**。内核自研热路径，DataFusion 做向量化执行骨架，Lance/Vortex 做列式格式——这正是 SmithDB 被生产验证的三件套组合，落到单机本地盘。

---

### 3.1 数据模型与 trace 树编码

#### 3.1.1 统一数据模型（同时吃 OTel GenAI 与 LangSmith 两套协议，无损归一）

内部核心抽象：**`Span = 一棵 trace 树上的一个节点；run 是事件序列而非不可变行`**。

**核心列**（小、密、列存 + 索引）：

| 类别 | 字段 |
|---|---|
| 标识/树 | `span_id`(UUIDv7,时间有序) · `trace_id`(=根) · `root_id` · `parent_span_id` · `pre`/`post`(区间编码,flush 物化) · `lvl`(深度) · `tenant_id` · `dotted_order`(**G11**:LangSmith 排序键,一等列) |
| 类型/时间 | `span_kind`(归一枚举:llm/chat/chain/tool/retriever/embedding/prompt/parser/agent/workflow/memory/thought) · `name` · `start_time` · `end_time`(可空=活trace) · `first_token_time`(TTFT) · `status`(success/error/**pending**) |
| 会话 | `thread_id`(=conversation/session,跨 trace 线程重建) |
| 聚合热点 | `input_tokens`/`output_tokens`/`total_tokens` · `cache_read_tokens`/`reasoning_tokens` · `total_cost`/`prompt_cost`/`completion_cost` · `latency_ms` · `model`/`provider` |
| 过滤维度 | `tags[]` · `feedback_stats` · `finish_reason` · `tool_name` · `error_code` · `reference_example_id` · `in_dataset` |
| 原文保留 | `run_type`(LangSmith) · `gen_ai.operation.name`(OTel) · `raw_attrs`(jsonb 无损兜底) |

**大字段**（外置/晚物化，独立子文件 + 核心行只存指针）：`inputs`/`outputs` 自然语言全文、`serialized`、`events` 流式事件序列、`error` 栈、多模态 payload 引用 token。

**协议归一**：摄入层把 OTel 的 16B `trace_id`/8B `span_id` 与 LangSmith 的 UUID/`dotted_order` 双向互转，内部统一 UUIDv7；`gen_ai.*` 属性与 LangSmith `inputs/outputs/run_type` 都映射进上表，原文留 `raw_attrs` 兜底，无损。

#### 3.1.2 run = 事件序列（解决「早上出生、下午死亡」+ 乱序 + 活 trace）

**核心存储语义（照搬 SmithDB，绝不用「先 INSERT 占位、后 UPDATE 改行」）**：append-only 事件 + 查询期折叠。
- 同一 `span_id` 的每次上报（start / partial / end / tool_result / error / feedback）追加独立事件 `(span_id, seq, event_type, patch, ts)`。
- 热区按 `span_id` **fold（折叠合并）** 成最终态：后写覆盖前写、patch 合并。
- 长 span 在 end 到来前，start 事件已在热区 → **活 trace 直接可查**（status=pending）。
- flush 时绝大多数已闭合 span 折叠成单行；flush 后晚到的 end 事件走 **upgrade vector** 补丁（见 §2），物理重写延到 compaction，不引发写风暴。

#### 3.1.3 trace 树编码：写侧邻接表 + 读侧区间编码（双编码，矛盾消解在 flush）

这是区别于普通时序日志的核心命题。**枪毙嵌套集**（任何插入改半棵树，在「每秒大量小 span」下不可用）。

| 阶段 | 编码 | 复杂度/语义 |
|---|---|---|
| **写侧（MemTable）** | 邻接表 `parent_span_id` + 冗余 `trace_id`/`root_id` | 插入 O(1)，**完全不在乎到达顺序**，父晚到只是邻接暂悬空；「找根」O(1) 读字段；「加载整棵 trace 树」= `trace_id` 等值过滤 |
| **读侧（flush 物化）** | 对已成形 trace 做一次 DFS，物化 `pre`/`post`/`lvl` | **子树查询 = `WHERE trace_id=? AND pre BETWEEN root.pre AND root.post`** → 列式 segment 上连续区间扫，极快；segment 不可变 → 区间编码一旦物化永不重算 |

- **矛盾消解点**：写友好编码与读友好编码的冲突，全部吸收在「热→冷一次性物化」这一步。
- **flush 后晚到的极少数 span**：走 upgrade vector 补丁，或回退邻接表递归 CTE 补齐，不污染主路径。
- **`dotted_order` 一等列保留（G11）**：字典序=先序遍历、前缀匹配=子树，服务 LangSmith 协议直读，与 `[pre,post]` 互为冗余加速。
- **线程重建**：独立 `thread_id → [span_id 按时序]` 二级索引（B-tree/倒排）。重建长对话 = 按 thread 拉 run 列表 + 各根 span 的 input/output 小列（晚物化，不拉大 payload）→ 对标 SmithDB 131ms。

---

### 3.2 存储引擎与读/写路径

#### 3.2.1 三层结构（行式热区 + 列式不可变 segment + 大字段外置）

```
WAL(顺序追加,唯一可变真相源) → L0 行式 MemTable(可原地更新/折叠)
   → flush(批量) → L1..Ln 列式不可变 Segment(Lance/Vortex,时间分层) + deletion/upgrade vector
   → 大 payload → object_store(本地盘/MinIO)
```

**为什么行式热区 + 列式冷区**（不是纯列或纯行）：trace 写入 = 高频碎片化 + 乱序 + 可更新；列式对「原地改一个字段」极不友好（重写整列 chunk）。
- 热区**行式**：碎片化小 span 追加 O(1)；长 span 晚到 update = 改一行/追加事件；活 trace 100% 在内存按 `trace_id` 哈希直命中（= SmithDB「ingestion 节点直服务新鲜数据」的单机版，无对象存储/网络，延迟更低）。
- 冷区**列式不可变**：cost/latency/token 聚合列存 + 向量化是数量级优势；不可变让倒排/JSON 路径/zone-map/向量段在 flush 时一次建好、永不维护。
- **晚物化（照搬）**：segment 内核心小列与大 payload 分文件。list/filter/聚合只读小列；大 payload 仅在用户点开某条 trace 时按需拉 → 彻底解决无界大 payload 污染核心行扫描。

#### 3.2.2 写路径（G7 削峰）

```
span 事件 → 摄入(协议归一/中文分词预处理/JSON展平/base64大payload抽离→object_store/embedding旁路捕获)
   → WAL 组提交(group commit,摊薄 fsync) → ack(低延迟持久)
   → 行式 MemTable(无锁分片,按 span_id 哈希;折叠;邻接表+冗余trace_id/root_id)
   → [时间/行数/trace闭合阈值触发 flush] → DFS物化pre/post → 批量写 Segment(Lance舒适区批量append)
       → 同步建 tantivy倒排 + 向量段 + 物化高频JSON列 → 最后原子改 manifest → 截断 WAL
   → per-tenant 写队列令牌桶削峰，把高频小写整形为底座舒适区批量写
```

写路径只碰 MemTable + WAL，与查询零锁竞争。

#### 3.2.3 读路径（merge-on-read + 晚物化）

```
查询 → 解析(原生SQL + trace扩展函数) → DataFusion 计划(谓词下推/晚物化/代价估计)
   → 并发扫 (活跃MemTable + 冻结MemTable + zone-map命中的Segment)
       · 点查/活trace：优先命中 MemTable(哈希直达)
       · 聚合/JSON/树区间扫：走列存 Segment(zone-map先剪枝整段)
       · 全文/向量：先走 tantivy/向量层出候选 id → 回 Segment 取列
   → 归并：按 span_id 折叠事件 + 应用 deletion/upgrade vector(被覆盖/删除行剔除)
   → 晚物化：仅当 project 到大字段才去 object_store 取
   → 渐进式时间窗：查「最新 N 条 run」沿时间倒序在最新 segment 建有界时间窗，不全排序
```

#### 3.2.4 列式格式：SegmentFormat trait（G4）—— Lance 主 / Vortex 备 / 自研段 兜底

**这是终裁对 D「押注 Vortex」最大风险的对冲设计**：

- **`SegmentFormat` trait 抽象层**：Lance / Vortex / 自研段实现同一接口，compaction 时透明切换。
- **MVP 主选 Lance**（G1）：唯一打包好「随机访问 + 向量 ANN + 内置中文 jieba/lindera FTS + 多模态/加列」的嵌入式格式，恰中本产品招牌能力 → M0-M1 直接抵掉「中文全文 + 向量」两块自研，**这是上市提速的关键杠杆**。
- **性能演进切 Vortex**（已核实 0.36+ 格式向后兼容、随机读 100x、扫描 10-20x）：稳定后对历史冷段切 Vortex 抬高随机访问与压缩上限。
- **自研段兜底**：若 PoC（G8）实测 Lance/Vortex 在高频乱序 span 下 compaction 抖动不达标，落自研最小列存段。
- 二者均 Apache-2.0、均接 DataFusion，平滑切换无授权/集成障碍。

#### 3.2.5 Compaction：时间分层（time-tiered）+ deletion/upgrade vector

- **时间分层**：trace 是时序数据（追加为主、按时间查、老数据不变）。新数据小 segment（写优化、还在等 end 事件）→ 老数据合并大 segment（查询优化、压缩更紧、索引更紧凑）。**不用 leveled/size-tiered**（为通用 KV 设计，浪费 trace 时间局部性，按时间裁剪几乎免费）。
- **Mutation = deletion/upgrade vector**：已 flush 的 span 晚到更新不重写文件，只在 manifest 给 segment 挂向量，读时合并、compaction 时物化 → 「出生在早上死亡在下午」不引发写风暴。
- **IO 限速**：compaction 独立线程池 + 令牌桶/IO 配额，避免抢占前台写/查，保 P99（SmithDB 靠拆无状态服务，单机靠线程优先级 + IO 配额达同效）。
- **复用团队磁盘索引/页面整理框架**管 segment 落盘、mmap、引用计数、GC、页面整理——团队现成资产，零知识断层。

---

### 3.3 索引体系（树 / 全文含中文 / JSON / 向量）

四类索引全部 **per-segment 内嵌、不可变、flush 时一次建好、zone-map 可整段跳过**。

#### 3.3.1 树索引
区间编码 `[pre,post]` + 冗余 `trace_id`/`root_id` 列（§3.1.3）。segment 按 `(tenant_id, trace_id, pre)` 排序 → 子树为连续区间扫。`thread_id → [span...]` 倒排做线程重建。热区 `parent_span_id` 邻接 + 递归 CTE 作冷数据回退。

#### 3.3.2 全文检索（含中文分词 —— 国际玩家全军覆没的差异点）
- **双轨**：MVP 用 **Lance 内置 jieba/lindera FTS + 用户词典**（G1，省自建）；性能演进引 **per-segment 内嵌 tantivy 倒排**（Lucene 式、mmap、启动 <10ms、term zone-map 跳段、postings/positions 分块，与 SmithDB 同构但单机无对象存储读放大 → 应优于其 400ms）。两者经 SegmentFormat 抽象统一。
- **中文分词：tantivy-jieba 为主**（已核实纠正：cang-jie 维护较慢，优先 tantivy-jieba，底层 jieba-rs 活跃）。jieba 搜索模式细粒度 + n-gram 兜底召回；短语检索靠 position。**自定义词典**收 Agent/LLM 领域词（工具名、模型名、专有名）。可挂团队 PG 系 `zhparser`(SCWS) 经验做词典增强。
- 建在 `inputs`/`outputs`/`name`/`error` 大字段子文件上，不污染核心小列。
- **离线/气隙部署**：安装包内置词典/语言模型（Lance jieba 需 `LANCE_LANGUAGE_MODEL_HOME`，tantivy-jieba 模型可静态编入），保证气隙可装。

#### 3.3.3 JSON / 元数据过滤（任意嵌套字段）
- **主路线（高频快）**：flush 时扫 JSON，把高频路径（`metadata.model`/`metadata.user_id`/`provider`）自动提升为独立物化列 + zone-map/字典编码 → 走列式 pruning。
- **补路线（低频全覆盖）**：全 JSON 路径展平成 `(json_path, value) → row_id` 倒排（迁移团队 PG `jsonb_path_ops` GIN 布局到不可变 segment，免增量更新/vacuum 复杂度）；字符串走字典/倒排，数值走 zone-map 支持范围。

#### 3.3.4 向量索引（SmithDB 完全没有 —— 招牌差异化，团队独有资产）
- **来源**：对 span 的 input/output（+决策上下文）embedding。**G10 控成本**：旁路捕获客户已有 embedding API 调用 + 异步管线 + 按租户采样。
- **引擎**：单机本地 NVMe 跑 **DiskANN（十亿级、低内存，正对单机私有化）/ HNSW（热数据低延迟）/ IVF（按分区裁剪）**，复用团队现成生产代码；MVP 可用 Lance 内置 ANN 起步。
- **为什么这是 SmithDB 结构性空档**：其对象存储 + 无状态架构与 ANN（有状态、随机访问延迟敏感）哲学冲突，跑图遍历延迟会爆炸；**我们纯单机本地 NVMe 正是 DiskANN/HNSW 最优环境——对手架构劣势 = 我们约束下的天然优势，短期补不上。**
- **向量段走 LSM compaction**（复用页面整理框架，增量插入 + 段合并重建）。
- **过滤性 ANN**（「在 租户A、近7天、cost>X 的 trace 里找语义相似」）：查询优化器在「先向量粗召回再标量精过滤」vs「IVF 分区裁剪后 ANN」之间按代价模型选择——团队优化器能力是把混合检索做快的核心。分阶段：v1 近线、v2 在线低延迟。

---

### 3.4 活 trace 与实时聚合

#### 3.4.1 活 trace（运行中即查未完成 trace）
天然实现，**无需特殊机制**：运行中 trace 的 span 100% 在行式 MemTable，按 `trace_id` 哈希直达；读 MemTable 不可变快照即可看到「未完成 trace 当前状态」（含 pending 节点）。这是 SmithDB「读 ingestion 本地缓存」的单机版，无网络/对象存储，延迟与点查同级（<100ms 目标）。查询规划器自动把未 flush 的 MemTable 段纳入扫描集合。**活 trace 推流**（G3）：运行中 trace 可被订阅（SSE 推送子树增量）。

#### 3.4.2 实时聚合（cost/latency/token usage）
- **DataFusion 向量化执行**扫列存做 sum/avg/p50/p99/group-by，列存数量级优势。
- **增量物化视图**（复用团队物化视图能力）：按 `(tenant, model, time_bucket, status)` 维度预聚合 token/cost/latency/error_rate，flush/compaction 增量维护 → dashboard 级 <1s。
- **活数据聚合**：MemTable 维护轻量增量累加器，查询时与历史聚合合并。
- 这套同时是**数据飞轮「奖励信号物化视图」**底座（§3.10）。

---

### 3.5 单机内多租户隔离（G9 混合分档）

单机多租户本质 = 目录隔离 + 资源配额，比 SmithDB 的 slice 路由 + bucket 隔离更简单可控。四层：

1. **数据隔离（强制）·混合分档（G9）**：
   - 默认/大客户：`tenant_id` 作所有 segment 最高排序前缀 / **物理分目录**，查询强制带 `tenant_id`，存储层目录/前缀直接裁剪 → 一租户永不扫另一租户文件；**删租户 = 删目录**。
   - 海量小租户（租户数极多场景）：**共享文件 + `tenant_id` 行级强制过滤 + RLS**，规避文件句柄/attach 开销。
2. **资源隔离**：per-tenant 写入配额、查询并发、MemTable 内存上限、compaction IO 配额独立计量（令牌桶 + per-tenant 调度器）→ 防单租户高频写打满 IO 饿死他人。
3. **加密/合规**：per-tenant 静态加密密钥落盘加密，满足金融/政企私有化。
4. **强隔离档**：大客户可「一租户一进程 + 共享磁盘格式」部署。

---

### 3.6 崩溃恢复与一致性（G5 原子可见性纪律 + G6 SLA 旋钮）

**WAL + 不可变 segment「双真相源」模型（团队 PG 经验直迁）**：

- **WAL = 唯一可变状态真相源**：所有写（新 span、晚到 update、deletion）先组提交顺序写 WAL，落盘即 ack → 低延迟 + 持久。
- **MemTable = WAL 的内存物化**：崩溃后重放 WAL 重建。
- **不可变 segment 自带持久性**：flush 原子落盘（写完 + fsync + 原子改 manifest）后截断对应 WAL。不可变 → 永不半写损坏已有数据。
- **Manifest/Metastore = 内嵌 SQLite（默认）/ 可选内嵌 openGauss（大客户·信创档，G2）**：记录「有效 segment 集合 + 各 segment deletion/upgrade vector + WAL checkpoint 位点 + 倒排/向量段位置」。**manifest 原子更新 = 整库一致性提交点**。崩溃恢复 = 读 manifest 定有效 segment + 重放 checkpoint 后 WAL。
- **G5 一等正确性纪律（多索引原子可见性）**：列存 segment、tantivy 倒排段、向量段的可见性必须由 manifest 原子切换**统一发布**——三者要么全可见要么全不可见。flush 严格顺序：**写 segment → 建倒排 → 建向量 → 最后原子改 manifest → 截断 WAL**。任一步崩溃，manifest 未提交，重放 WAL 整批重做。这是多源 merge-on-read 正确性与售后成本的关键。
- **一致性级别：单写者 + 多读者 MVCC**（DuckDB 同款）。读端持 segment+MemTable 不可变快照（原子指针切换发布、引用计数 GC）看一致视图；写端串行追加。trace「追加为主、偶有更新」单写者完全够用。
- **G6 SLA 旋钮（按客户可选）**：
  - 默认「高吞吐档」：组提交轻量 WAL，崩溃可能丢最后未 checkpoint 的极少量事件（trace 场景容忍度高）。
  - 「零丢失档」：热区 logged、走完整 WAL，崩溃零丢失（牺牲部分吞吐），供金融/政企强 SLA 客户。

---

### 3.7 私有化打包与部署形态

**形态 = 嵌入式 Rust 内核（自带存储/WAL/事务）+ 单机服务外壳。**

- **内核像 DuckDB（完整库，自带存储+事务+WAL），执行层用 DataFusion，格式用 Lance/Vortex**——SmithDB 验证的三件套组合，落单机本地盘。自研热路径（LSM 写入、segment 管理、compaction、树编码、向量召回算子、混合查询执行），复用 DataFusion 做向量化执行骨架（不重造执行引擎）。
- **为何不止于嵌入式库**：多租户、认证、网络访问、在线备份、监控是商业产品必需 → 外包**单机服务进程**（SQL/HTTP/gRPC 端点）。
- **为何不照搬 SmithDB 无状态三服务**：那是分布式弹性设计，单机纯负担。单机 = **一个进程内多组线程池**（ingestion/query/compaction/embedding-飞轮/备份-TTL），简单、低延迟、一键起。
- **私有化一键部署**：单静态链接二进制 + 本地 NVMe + **零外部依赖**（SQLite 内嵌、object_store crate 默认本地盘、客户自带 MinIO 时零改码）。对比痛点——Langfuse 自托管要 PG+ClickHouse+Redis+S3 4-6 组件 + DBA + $3-4K/月；我们「一个包、本地盘、离线可装」是降维打击。
- **G2 信创/openGauss 合规嫁接**：国产 OS/CPU（鲲鹏/飞腾/海光）适配 + 大客户档「可选内嵌 openGauss 作 metastore」+ **移植 openGauss 优化器代价模型**到 DataFusion 混合查询规划。信创合规是国内金融/政企/气隙采购的一票否决项——这把 A 独有的「国产化身份证」装进 D 的交付外壳，**而不让 openGauss 决定存储引擎形态**。
- **易用性落点**：对外**原生 SQL**（相对 SmithDB 私有 API 的差异化卖点，可走 PG 线协议复用生态工具）+ 内置 trace 专用函数/视图：`load_trace_tree(trace_id)` · `rebuild_thread(thread_id)` · `subtree(span_id)` · `semantic_recall(span_id, k, filters)` · `export_trajectory(...)`。
- **数据保留/TTL（私有化硬功能）**：非均匀保留——error/被标注/进数据集 trace 长期留存；普通 trace 按规则回收。删除 = 廉价逻辑标记（deletion vector）+ 后台 compaction 物理清除。
- **冷热分层**：热层近期小 segment + 内存/SSD 缓存 + 倒排就绪（百毫秒交互）；冷层老数据大 segment 强压缩落本地大盘/自带 MinIO。本地缓存 + 文件亲和调度近似 SmithDB 的 sticky routing。

---

### 3.8 摄入接口（OTel/SDK，兼容 LangSmith 生态）

「**换存储不换 SDK**」是降迁移成本、过采购门槛的关键：

- **OTLP/gRPC + OTLP/HTTP + OTel GenAI 语义约定**（v1.41 仍 Development 态，用 `OTEL_SEMCONV_STABILITY_OPT_IN` 管兼容）：吃掉 OpenLLMetry/Traceloop 采集生态（采集标准、非竞品，应兼容）。
- **OpenInference** 兼容（Arize/Phoenix 生态）。
- **LangSmith-compat REST**：兼容 LangSmith Run 写入格式（`inputs`/`outputs`/`dotted_order`/UUIDv7），让 LangChain/LangGraph 用户「指一下 endpoint 就迁过来」，零改码。
- **摄入层职责**：协议归一（双 ID 互转）+ 中文分词预处理 + JSON 展平/高频路径标记 + **大 payload 抽离**（检测 base64 data URI → 抽出上传本地盘/MinIO → 引用 token `@@@media:type=...|id=...@@@` 替换，与存储后端解耦；SHA256 去重 + MIME 白名单 + 大小上限 20MB 参考）+ **embedding 旁路捕获**（G10）+ 写 WAL + MemTable。
- **不在 ingest 时组装 trace 树**：原始 span 入表，查询时按 trace_id 拉全树组装（容忍父节点暂缺，乱序友好）。
- **写入可见性**：ack（WAL 落盘）→ 立即可查（活 trace），对标 SmithDB ingestion P50 630ms 但单机本地盘应更优。
- **飞轮出口**：`export_trajectory` 树感知 + 多模态引用还原，导出 messages/prompt-completion/DPO 偏好对/tool-call 轨迹，对接训练管线。

---

### 3.9 组件级 build-vs-开源 最终清单（含许可证）

| 组件 | 终裁决策 | 许可证 | 理由 |
|---|---|---|---|
| 列式 segment 格式 | **开源 Lance 主 / Vortex 备 / 自研段兜底**（SegmentFormat trait） | Apache-2.0 | Lance 内置中文+向量+多模态恰中招牌、提速 MVP；Vortex 0.36+ 兼容抬性能上限；可切换对冲风险 |
| 查询执行引擎 | **开源 DataFusion** + 自研 trace 专用算子 | Apache-2.0 | 纯 Rust 可嵌入、向量化、SmithDB 生产验证；自研算子=树遍历/向量召回/LSM merge |
| LSM 写入/segment/compaction | **自研**（复用团队磁盘索引/页面整理框架） | 自有 | 热路径决定性能；trace 专用（晚物化/deletion vector/时间分层）开源件覆盖不了 |
| 树/区间编码 | **自研** | 自有 | trace 专用，无现成件 |
| 全文倒排引擎 | **Lance 内置 FTS 起步 / 开源 tantivy 演进** | Apache-2.0 / MIT | Lucene 级、Rust、嵌入式、mmap 友好 |
| 中文分词 | **tantivy-jieba 主 + Lance 内置 jieba 双保险** + 自研词典/zhparser 经验 | MIT | 国际玩家空白点；jieba-rs 活跃 |
| JSON 路径索引 | **自研**（迁移 PG GIN/jsonb_path_ops 到不可变 segment） | 自有 | 团队 PG 经验直迁、免 vacuum |
| 向量索引 | **复用团队 DiskANN/HNSW/IVF** | 自有 | 现成生产代码、单机十亿级、招牌差异化 |
| Metastore/manifest | **内嵌 SQLite 主 / 可选内嵌 openGauss（大客户·信创档）** | Public Domain / Mulan PSL v2 | 单机更轻；信创档拿国产化背书 |
| 存储后端抽象 | **开源 object_store crate** | Apache/MIT | 本地盘/MinIO/S3 一码通，私有化无争议 |
| WAL/事务/MVCC | **自研**（团队 PG 经验直迁） | 自有 | 单写者多读者，简单可靠 |
| SQL parser/优化器 | **复用 DataFusion + 移植 openGauss 优化器代价模型**（G2） | Apache-2.0 + 自有 | 原生 SQL 差异化 + filtered ANN 混合代价模型 |
| 数据飞轮原语 | **自研** | 自有 | SmithDB 完全没有，最大差异化 |
| OLAP 重聚合/导出旁路（可选） | **可选 chDB** | Apache-2.0 | 训练数据导出加速旁路，非主存 |

**许可证总览（全部商用友好、可闭源私有化，无 GPL 传染/SSPL/BSL）**：Lance/Vortex/DataFusion/Arrow/object_store = Apache-2.0；tantivy/tantivy-jieba = MIT；SQLite = Public Domain；openGauss 复用部分 = Mulan PSL v2（宽松、信创加分）。

---

### 3.10 差异化护城河：语义召回 + 数据飞轮（G3，SmithDB 完全没有）

把产品从「事后看日志的可观测性存储」升级为「在线参与 agent 决策的检索底座 + 数据飞轮引擎」。五大原生原语：

1. **语义 trace 召回（招牌、飞轮轴承）**：`semantic_recall(span, k, filter)` —— 召回语义最相似历史 trace，与标量/JSON/时间/树过滤**同查询内融合**。服务 eval 基线对比、运行时 few-shot 注入（RAG over traces，直接进 agent 推理回路）、bad-case 纠错、最佳实践沉淀。
2. **轨迹导出（飞轮出口）**：`export_trajectory(...)` —— 一键导出 messages 数组/prompt-completion/DPO 偏好对/tool-call 轨迹，树感知 + 多模态引用还原 + 增量/流式。
3. **奖励信号增量物化视图（飞轮度量）**：人工反馈 + LLM-judge 分 + cost/latency/token + 成功标签作奖励信号，增量物化视图实时维护 → 训练时按奖励采样（RLHF/RFT 数据源）。
4. **SOP/few-shot 抽取（飞轮产物）**：对同类任务高奖励轨迹聚类，抽共性步骤模板 + 最佳 few-shot 集，供 agent 运行时直接拉取 → 闭环回在线推理。
5. **活 trace 推流**：运行中 trace 即可被召回/评估/订阅（SSE 推送子树增量）。

---

## 第四部分：分阶段路线图（v1 单机 MVP → 商业化产品）

团队基线：充足 DB 人才 + PG/openGauss 内核 + Rust + 向量索引（HNSW/IVF/DiskANN）+ 磁盘索引/页面整理框架 + 查询优化器。几乎零知识断层。按 3-4 人核心团队并行估算。

| 阶段 | 内容 | 关键里程碑 | 工时 | 关键风险点（门禁） |
|---|---|---|---|---|
| **M0 PoC + 格式定档** | SegmentFormat trait；Lance vs Vortex 在**高频乱序 span** 下写放大/合并抖动实测；filtered ANN 标量谓词真实延迟实测；OTLP 摄入 + WAL+MemTable+flush 最小闭环 | **格式定档 + 能力边界钉死** | ~1.5 月 | **G8 门禁**：Vortex 抖动/filtered ANN 不达标 → 降级 Lance/自研段；这是首选方案上线前硬性门禁 |
| **M1 MVP（可演示，提速路径）** | LSM 全路径；邻接表+区间编码双编码；**Lance 内置 jieba 中文全文 + 内置 ANN 直接抵两块自研（G1）**；DataFusion 接 Lance；活 trace；原生 SQL + trace 函数；OTLP/LangSmith-compat 摄入 | **单 trace/树/线程/中文全文/语义召回跑通**，对国内私有化客户可 PoC 演示 | ~2.5 月 | Lance 中文模型气隙内置；DataFusion 树查询弱→自写区间编码下推规则 |
| **M2 商业可售 Beta** | 多租户（G9 混合分档 + 配额 + RLS + per-tenant 加密）；TTL/差异化保留；deletion/upgrade vector；时间分层 compaction；实时聚合增量物化视图；**G5 manifest 原子可见性纪律**；崩溃恢复加固 + **G6 SLA 旋钮**；在线备份；单包私有化打包 | **可私有化交付 Beta**，拿种子客户 | ~3 月 | G5 多索引一致性是售后成本最大隐患，需专项测试；compaction P99 抖动→IO 限速调优 |
| **M3 招牌差异化 GA** | DiskANN/HNSW 接入 trace 流（替/补 Lance 内置 ANN 做十亿级）；filtered ANN（IVF 分区裁剪 + 优化器混合代价模型）；**飞轮四原语（G3）**：export_trajectory + 奖励物化视图 + SOP 抽取 + 活 trace 推流；**embedding 旁路捕获 + 采样（G10）** | **完整差异化版 GA**，招牌签单 | ~3 月 | filtered ANN 工程难度→分阶段 v1 近线/v2 在线；embedding 成本→旁路+采样 |
| **M4 信创/加固/演进** | **G2 信创适配**（国产 OS/CPU + 可选内嵌 openGauss metastore + 优化器代价模型移植）；tantivy per-segment 倒排演进（抬全文上限）；冷段切 Vortex（抬随机访问上限）；压测对标 SmithDB P50；词典/分词调优；文档 | **信创合规 + 性能演进版** | ~2.5 月 | openGauss 版本耦合用 SegmentFormat/metastore 抽象隔离 |

**累计：MVP 可演示 ~4 个月（M0+M1）；商业可售 Beta ~7 个月（+M2）；完整差异化 GA ~10 个月（+M3）；信创/性能演进 ~12.5 个月（+M4）。**

> **节奏要点（终裁）**：M1 即用 Lance 提速路径拿出「中文全文 + 单机一键起 + 语义召回」的可演示版——这已是相对 SmithDB/Langfuse 的降维差异，可立刻对国内私有化客户做 PoC。**这条 Lance 提速路径是把 D 的「上市慢」短板拉到 A 量级的关键**，让我们既拿 D 的性能上限与护城河，又不丢 A 的时间窗。

**人力建议**：核心 3-4 人（1 存储内核/LSM + 1 向量/飞轮 + 1 查询执行/优化器 + 1 摄入/外壳/私有化），M2 起加 1 人做多租户/信创/打包/测试。

---

## 第五部分：与 SmithDB 的差异化与护城河小结

| 维度 | SmithDB（2026-05） | yiTrace（本方案） | 护城河性质 |
|---|---|---|---|
| 部署形态 | 对象存储 + 无状态三服务（即便自托管仍重栈） | **单静态二进制 + 本地盘 + 零外部依赖 + 离线/气隙可装** | 私有化降维打击（对 Langfuse 4-6 组件亦然） |
| 中文 | 无 | **原生中文分词（tantivy-jieba/Lance jieba + 领域词典）** | 国际玩家全军覆没点 |
| 向量/语义召回 | **完全没有**（对象存储+无状态架构与 ANN 哲学冲突，短期补不上） | **单机十亿级 DiskANN 语义召回 + filtered ANN + 向量原生融合 DataFusion** | **结构性空档**，对手补齐需 1-2 年 |
| 数据飞轮 | 无 | **五原语：语义召回/轨迹导出/奖励物化视图/SOP 抽取/活 trace 推流** | 从「可观测性存储」升级为「参与推理回路的飞轮引擎」，定价锚点 |
| 查询接口 | 私有 API | **原生 SQL（可走 PG 线协议）+ trace 专用函数** | 易用性 + 生态工具复用 |
| 合规/信创 | 无国产化背书 | **国产 OS/CPU 适配 + 可选 openGauss metastore + Mulan PSL** | 国内采购一票否决项的「身份证」 |
| 数据范式 | LSM + 列式不可变 segment + 晚物化 + deletion vector + 时间分层 compaction + 自研倒排 | **同范式**（已被生产验证），砍掉分布式/对象存储包袱落单机 | 不冒范式风险，只在正确范式上做单机最优 + 五把差异化刀 |

**一句话总裁**：在 SmithDB 验证过的正确数据范式（LSM + 列式不可变 segment + merge-on-read + 晚物化 + deletion vector + 时间分层 compaction + 自研倒排）之上，**用自研 Rust 单机内核做到性能上限与向量原生融合（D 的不可替代优势），用 Lance 提速 MVP + openGauss 信创外壳补平上市速度与合规（嫁接 A 的可嫁接优势），用 manifest 原子可见性纪律 + SLA 旋钮 + 多租户混合分档收口工程可靠性（嫁接 B/C 的纪律）**——以「原生中文 + 语义召回 + 数据飞轮 + 单机极简 + 信创合规」五合一，筑起 SmithDB 与 ClickHouse 派短期都够不着的 1-2 年护城河。

> 最大且唯一需 PoC 坐实的硬假设：Lance/Vortex 在「高频乱序小 span」trace 负载下的 compaction 稳定性——已通过 M0 门禁（G8）+ SegmentFormat 可降级（G4）+ 自研段兜底三重对冲，不达标不进 M1。

> 注：本设计为纯架构综合，未读取/修改工作目录代码（除背景 docx 外为空）。如需落盘，建议存至 `/Users/Four/JobProjects/yitrace/vex-x/docs/design/2026-06-16_final-trace-db-architecture.md`。