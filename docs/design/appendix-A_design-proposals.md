# 附录 A：四套候选架构方案（A/B/C/D 原始产出）

> 由设计阶段 4 个并行 agent 独立产出。评审与综合见主文档。

# 方案 A: 基于 PostgreSQL/openGauss 内核扩展

The working directory is essentially empty (just a docx). This is a pure design task — I have comprehensive research material already in my context. No further research or file reading is needed; the task is to synthesize the design for **方案 A (PostgreSQL/openGauss 内核扩展路线)**.

I'll produce the complete architecture design as my final response.

---

# 单机商业版 Agent Trace DB 架构设计（方案 A：PostgreSQL/openGauss 内核扩展路线）

> 代号建议：**TraceGauss**（对内）/ 对外可叫「**观池**」或「**AgentTrace DB**」。
> 定位：国内首个 Agent 可观测性专用数据库，纯单机、私有化、原生 SQL、原生中文检索、trace 树/活 trace 一等公民、向量飞轮内建。
> 路线特征：**以 openGauss 内核为地基**（自有内核 → 授权最干净、信创加分、上市最快），把 trace 场景的专用能力以「扩展 + 内核小改」方式叠加，务实补齐 PG 在高频小写、列式聚合、大 payload 上的三块短板。

---

## 0. 设计总纲：为什么是 openGauss，以及它和 SmithDB 的关系

SmithDB 验证的数据范式是 **LSM + 列式不可变 segment + 晚物化 + deletion vector + 自研倒排 + 时间分层 compaction**，其复杂度几乎全花在「分布式 + 对象存储 + 无状态弹性」上。我们是**纯单机**，因此：

- **照搬其数据范式思想**（run=事件序列、merge-on-read、late materialization、时间分层 compaction、deletion vector）；
- **砍掉其分布式包袱**（cluster manager / sticky routing / 对象存储读放大优化全部不要）；
- **不照搬其「Rust 全自研引擎」实现**——本方案选择把这套范式**落到 openGauss 内核之上**，用「成熟内核 + 表访问方法（TableAM）扩展 + 后台搬迁」实现，换取**最快上市 + 最干净授权（Mulan PSL v2，可闭源商用、信创友好）+ 团队零知识断层**。

openGauss 提供的现成地基（直接复用，几乎零成本）：
WAL/Redo 崩溃恢复、MVCC、事务、分区表、行级安全(RLS)、TOAST 大字段外置、GIN/GiST 索引框架、全文检索框架(tsvector/tsquery)、查询优化器与执行器、原生 SQL、catalog 元数据、主备（单机版用不到但备份能力可用）。openGauss 还自带 **Astore/Ustore 双存储引擎**与**列存表(CStore)**，是本方案列式能力的内核起点。

> 一句话：**SmithDB 用 Rust 从零造一个 trace 专用库；我们用 openGauss 这个已经造好的成熟数据库，做减法和定向改造，做一个 trace 专用形态。** 单机本地盘让我们延迟有望持平甚至优于其公开基线。

---

## ① 数据模型与 trace 树编码方案

### 1.1 统一数据模型：run/span = 树上一个节点（兼容 OTel + LangSmith 双协议）

核心表 `span`（逻辑视图；物理上分热/冷两套存储，见 ②）。字段分三组：

**A. 核心列（小、密、必索引；走列存友好布局）**

| 列 | 类型 | 说明 |
|---|---|---|
| `tenant_id` | int / oid | 租户，所有查询强制前缀，分区最高维 |
| `span_id` | uuid (UUIDv7) | 节点唯一 ID，时间有序 |
| `trace_id` | uuid | 所属 trace（树根 ID），冗余冗存到每个 span |
| `root_id` | uuid | = trace_id 的根 run（多数情况等于 trace_id，保留以兼容多根） |
| `parent_span_id` | uuid | 直接父节点（可为空 / 暂悬空，容忍乱序） |
| `thread_id` | text | 会话/线程键（session_id/conversation_id 归一），跨 trace 线程重建用 |
| `span_kind` | enum/int2 | 归一类型：llm/chat/chain/tool/retriever/embedding/prompt/parser/agent/workflow/memory/thought |
| `name` | text | span 名 |
| `start_time` / `end_time` | timestamptz | 时间界；end 可为空(=pending/活trace) |
| `status` | enum/int2 | success / error / pending |
| `dotted_order` | text | LangSmith 风格可排序路径键（见 1.3） |
| `pre` / `post` | int8 | 区间编码（DFS 前序/后序号），flush 冷区时物化（见 1.3） |
| `lvl` | int2 | 节点深度 |
| 聚合热点列 | numeric/int8 | `input_tokens` `output_tokens` `total_tokens` `cache_read_tokens` `reasoning_tokens` `total_cost` `prompt_cost` `completion_cost` `latency_ms` `ttft_ms` |
| 维度列 | text/int | `model` `provider` `finish_reason` `tool_name` `agent_id` `error_code` `tags`(text[]) |

**B. 大字段列（大、稀疏；late materialization，TOAST/外置）**

| 列 | 类型 | 说明 |
|---|---|---|
| `inputs` / `outputs` | jsonb | 自然语言输入输出 / 结构化消息数组；OTel 的 input/output messages 归一进来 |
| `metadata` / `extra` | jsonb | 任意嵌套用户元数据 |
| `error` | text | 错误栈 |
| `events` | jsonb | 流式状态变更序列 |
| `media_refs` | jsonb | 多模态大 payload 引用 token 数组（见 ② 大 payload 处理） |

**C. 事件流列（支持 run=事件序列）**：见 1.2。

> 双协议归一：写入层（⑧ 摄入）把 OTel `gen_ai.*` attributes 与 LangSmith `inputs/outputs/run_type/dotted_order` 都映射进上表，**保留原始字段于 `raw_attrs jsonb`**，做无损归一。两套 ID（OTLP 16/8-byte 二进制 vs UUID）在入口处互转（二进制 trace_id/span_id 编码进 UUID 容器）。

### 1.2 run = 事件序列（解决「早上出生、下午死亡」+ 乱序 + 活 trace）

**这是最关键的存储语义决策。** 不做「先 INSERT 占位、后 UPDATE 改行」（对列存/堆表是写放大 + 膨胀灾难），而是 **append-only 事件 + 查询期折叠**：

- 热区 `span_event`（unlogged-ish 行表，见 ②）：每个对同一 `span_id` 的写入（start / partial / end / tool_result / error / feedback）追加一行事件 `(span_id, seq, event_type, patch_jsonb, ts)`。
- 查询/搬迁时按 `span_id` **fold（折叠合并）** 成最终 span 状态（后写覆盖前写、patch 合并）。
- 长 span 在 end 到来前，热区已有 start 事件 → **活 trace 直接可查**（status=pending）。
- 晚到 end 事件：若 span 仍在热区 → 直接追加事件、折叠即生效；若已搬迁到冷区 → 走 **upgrade vector**（见 ②）补丁，物理重写延到 compaction。

### 1.3 trace 树编码：写侧邻接表 + 冗余根列 → 冷区物化区间编码（双编码）

针对 trace「不断生长 + 乱序到达 + 高频碎片化插入」的特性，**枪毙嵌套集**（任何插入改半棵树，不可用），采用双编码：

**写侧（热区）= 邻接表 + 冗余 `trace_id/root_id`**
- 只存 `span_id + parent_span_id + trace_id + root_id`，写入永远 O(1)，**完全不在乎到达顺序**，父晚到也无妨（邻接关系暂悬空）。
- 找根 = O(1) 读 `trace_id` 字段。
- 加载整棵 trace 树 = `WHERE trace_id = ?` 等值过滤（冷区 segment 按 trace_id 排序聚簇 + min/max 跳块，一次顺序扫一小段）→ 对应 SmithDB 92ms。

**读侧（冷区）= flush 时一次性物化区间编码 `[pre, post]`**
- 搬迁到列存时，对每棵基本成形的 trace 做一次 DFS，物化 `pre`(前序号) / `post`(后序号) / `lvl`。
- **子树查询** = `WHERE trace_id=? AND pre BETWEEN $root.pre AND $root.post` → 列存上一段连续区间扫描，极快。
- 因 segment 不可变，区间编码**一旦物化永不重算**，彻底规避嵌套集插入死穴。
- flush 后才晚到的极少数 span：走 upgrade vector 补丁，或留邻接表回退路径用递归 CTE 补齐。

**`dotted_order` 作为一等列同时保留**（LangSmith 生态兼容 + 字典序=先序遍历 + 前缀匹配=子树），与区间编码互为冗余加速：`dotted_order` 服务 LangSmith 协议直读，`[pre,post]` 服务列存高速区间扫。

**线程重建**：`thread_id → [span_id...] 按时间排序` 二级索引（GIN/B-tree on `(tenant_id, thread_id, start_time)`），拉 run 列表 + 各 run 根 span 的 input/output 小列（late materialization，不拉大 payload）→ 对应 SmithDB 131ms。

---

## ② 存储引擎与读 / 写路径

### 2.1 三层存储（LSM 思想落到 openGauss 之上）

```
   写入路径                                          读取路径
 ┌──────────────┐
 │ WAL/Xlog 顺序追加│ ← openGauss 现成,崩溃恢复真相源
 └──────┬─────────┘
        │
 ┌──────▼──────────────────────┐  点查/活trace/单run/单trace树
 │ L0 热区: 行存 span_event 表    │ ←───────────────────────  优先命中
 │  - openGauss Ustore/UNLOGGED  │  (近N分钟span + 全部活trace在此)
 │  - 按(tenant,trace_id)哈希/索引 │
 │  - append-only事件, 折叠合并    │
 └──────┬──────────────────────┘
        │ 后台搬迁线程 (定时/阈值/trace闭合)
 ┌──────▼──────────────────────┐  列式扫描/聚合/全文/JSON/向量
 │ L1..Ln 冷区: 列存不可变 segment │ ←───────────────────────  历史走这里
 │  - 自研列存AM(基于CStore演进)   │
 │  - 时间分层 compaction          │
 │  - 内嵌 zone-map/倒排/JSON索引   │
 │  - deletion/upgrade 向量        │
 │  - 区间编码[pre,post]已物化      │
 └─────────────────────────────┘
```

**为什么行式热区 + 列式冷区**（PG 短板的正面补法）：
- **PG 短板 1：高频碎片化小写入**。堆表 + 完整 WAL + MVCC 对每秒上万小 span 会产生膨胀与 WAL 放大。
  **补法**：热区用 **UNLOGGED 行表 + 组提交**，或 openGauss **Ustore（in-place update，膨胀友好）**；写入只碰热表，与查询零锁竞争。崩溃时热区可由**自维护的轻量 WAL（独立于 PG 主 WAL）+ 事件重放**恢复（见 ⑥），用最终一致性换写吞吐。
- **PG 短板 2：列式聚合**。堆表/行存做 cost/latency/token 全表聚合是数量级劣势。
  **补法**：冷区列存 + 向量化扫描；聚合走列存 AM。
- **PG 短板 3：大 payload**。
  **补法**：TOAST 已能外置大字段，但仍在表内；本方案进一步把大 payload 抽离为**外部对象引用**（见 2.3），核心行只留指针。

### 2.2 写路径（高频碎片化 / 乱序 / 活 trace）

1. 摄入服务接收 span 事件（OTLP/LangSmith），归一 + 抽离大 payload。
2. **大 payload 抽离**：在摄入期检测 `inputs/outputs` 中 base64 data URI / 超阈值文本，抽出上传对象存储（本地盘/MinIO），原位替换为引用 token；防止大 payload 毒化列存核心行与小行扫描。
3. 写 **L0 热区**：append 一条 `span_event`（+ 写热区轻量 WAL，组提交，fsync 即 ack）。活 trace 此刻已可查。
4. 后台搬迁线程按 **(时间阈值 / 行数阈值 / trace 闭合信号)** 触发：把热区按 `(tenant_id, trace_id)` 折叠成最终 span 行，DFS 物化 `[pre,post]`，写出**列存不可变 segment**，同时一次性建 zone-map + 全文倒排 + JSON 路径索引 + 向量段，登记到 manifest（segment 元数据：tenant、trace_id/time 边界、行数、位置、deletion/upgrade vector 初值）。
5. 搬迁完成 → 截断对应热区数据与轻量 WAL。

### 2.3 读路径（merge-on-read + late materialization）

- 查询同时扫 **(活跃热区 + 待搬迁热区 + 命中的列存 segment)**，各源出结果后归并；**应用 deletion/upgrade vector**（被覆盖/删除的行在归并时剔除/替换）。
- **点查（单 trace/活 trace）**优先命中热区哈希/索引；**聚合/全文/JSON/向量**走列存 segment（zone-map 先剪枝整段，再走细索引）。
- **late materialization**：list/filter/聚合只读核心小列；`inputs/outputs/media` 等大字段只在用户真正 project（点开某条 trace）时才去 TOAST/对象存储取 → 对应 SmithDB 的 large-field 分离。

### 2.4 Compaction（时间分层 + deletion/upgrade vector）

- **时间分层（time-tiered）**：新数据留小 segment（写优化、还在等晚到 end 事件，少压实）；老数据合并成大 segment（查询优化、强压缩、索引更紧凑）。**不用 size-tiered/leveled**（trace 时间局部性让时间分层既简单又让按时间裁剪几乎免费）。
- **Mutation = deletion/upgrade vector**：已 flush 的 span 晚到更新/删除，不重写 segment，只在 manifest 给 segment 挂向量，读时合并、compaction 时才物化重写。对应「出生在早上死亡在下午」不引发写风暴。
- **IO 限速**：compaction 独立后台线程池 + 令牌桶/IO 配额，避免抢占前台写入与查询，保 P99 稳定。
- **复用团队「磁盘索引与页面整理框架」**管理 segment 落盘、mmap、引用计数与 GC、页面整理——这是团队现成资产，迁移成本极低。

---

## ③ 索引体系（树 / 全文含中文分词 / JSON / 向量）

| 索引 | 实现 | 服务查询 | build vs 复用 |
|---|---|---|---|
| **树/区间** | 冷区物化 `[pre,post]` 列 + B-tree on `(tenant,trace_id,pre)`；热区 `parent_span_id` 邻接 + 递归 CTE 回退 | 子树过滤、找根、先序遍历 | 复用 PG B-tree + 自研物化逻辑 |
| **线程** | B-tree/GIN on `(tenant,thread_id,start_time)` | 线程重建 | 复用 PG 索引 |
| **全文(中文)** | PG 全文检索框架 tsvector/tsquery + **zhparser(SCWS) / pg_jieba 中文分词** + GIN；冷区**每 segment 内嵌倒排**（term row-group + min/max zone 剪枝、postings/positions 分块） | 中文短语检索（自然语言 input/output） | **复用 PG GIN + zhparser/pg_jieba（成熟生产级）**；segment 内嵌倒排为自研演进 |
| **JSON** | 主：搬迁时**高频路径物化成独立列**(zone-map/字典)；补：jsonb GIN `jsonb_path_ops` 路径倒排（任意/低频嵌套字段） | 任意嵌套 metadata 过滤 | **复用 PG jsonb GIN**，迁移布局到不可变 segment 免 vacuum |
| **向量(语义召回)** | **复用团队 HNSW/IVF/DiskANN**，单机本地 NVMe；段级向量索引随 compaction 合并；支持 filtered ANN（IVF 分区裁剪 + 标量谓词融合） | 相似 trace/few-shot/bad-case 召回 | **复用团队向量索引工程（招牌差异化）**；pgvector 作 PoC 起点，DiskANN 做十亿级单机 |

**中文全文是相对 SmithDB 的核心差异点**（国际产品全军覆没）：zhparser/pg_jieba 是 PG 系成熟方案，**团队 PG 背景可直接生产化**；细粒度搜索模式 + n-gram 兜底 + Agent/LLM 领域自定义词典（工具名/模型名）保召回，短语检索靠 position。

**向量是 SmithDB 完全没有的结构性空档**：其对象存储 + 无状态架构与 ANN（有状态、随机访问敏感）哲学冲突；我们纯单机本地 NVMe 正是 DiskANN/HNSW 最优环境——**对手架构劣势 = 我们约束下的天然优势**。

---

## ④ 活 trace 与实时聚合实现

### 4.1 活 trace（运行中即查）
- 运行中 trace 的 span 事件 100% 在 **L0 热区行表**，按 `trace_id` 哈希/索引直接命中 → 这就是 SmithDB「ingestion 节点直读本地缓存」的单机版，且**无对象存储/网络，延迟更低**。
- 查询读热区 MVCC 快照即可看到「未完成 trace 的当前状态」（含 pending 节点），无需特殊机制。
- 树编码用邻接表 + 冗余 `trace_id`，活 trace 即便父子乱序也能按 trace_id 拉全集 + 递归 CTE 组装。

### 4.2 实时聚合（cost / latency / token usage）
- **近线层（增量物化视图）**：对 cost/latency/token/成功率按 `(tenant, time_bucket, model, tool, status)` 维度建**增量物化视图**（复用 openGauss MV + 团队优化器/MV 能力），随搬迁增量维护 → dashboard 级 <1s。
- **冷区即席聚合**：列存 + 向量化扫描 + zone-map 剪枝，服务任意维度即席聚合。
- **奖励信号物化视图（飞轮原语）**：把人工反馈、LLM-judge 分、cost/latency 作为奖励信号与 trace 关联，增量 MV 实时维护 → 训练时按奖励采样（RLHF/RFT 数据源）。把「看板指标」升维成「训练信号」。

---

## ⑤ 单机内多租户隔离

三层隔离（私有化合规硬约束）：

1. **数据隔离（强制）**：`tenant_id` 作为**分区最高维 + 所有 segment 的最高排序前缀 / 物理分目录**。每租户数据落独立 segment 文件集合（独立目录）。查询强制带 `tenant_id`，存储层目录/前缀直接裁剪——**一个租户的查询永不扫另一个租户的文件**。「删租户」= 删目录，极简。
   - PG 层加 **行级安全(RLS)** 作第二道逻辑兜底（防应用漏带 tenant_id）。
   - 默认「按租户分区 + 共享实例」最大化单机简洁性。
2. **资源隔离**：per-tenant 写入配额、查询并发、热区内存(MemTable)上限、compaction IO 配额独立计量（per-tenant 调度器 + 令牌桶）。防单租户高频写打满 IO 饿死他人。
3. **加密/合规**：per-tenant 静态加密密钥，落盘加密，满足金融/政企私有化。
   - 大客户/强隔离档位：「一租户一进程 + 共享磁盘格式」部署形态（见 ⑦）。

---

## ⑥ 崩溃恢复与一致性

**双真相源 + 单写者多读者 MVCC**（团队 PG 经验直接迁移）：

- **冷区（列存 segment）**：不可变 + manifest 原子提交。openGauss 主 WAL/Redo 负责 catalog 与 manifest 的崩溃一致性。segment 写完 → fsync → 原子改 manifest（= 整库一致性提交点）；不可变 → 永不半写损坏已有数据。
- **热区（行事件表）**：高频小写若全走 PG 完整 WAL 会拖慢。两档可选：
  - **稳妥档**：热区用普通 logged 行表，复用 openGauss WAL/Redo，崩溃零丢失（牺牲部分吞吐）。
  - **高吞吐档**：热区 UNLOGGED + **自维护轻量 WAL（事件日志，组提交 fsync）**；崩溃后重放轻量 WAL 重建热区。最近一个搬迁 checkpoint 之后的事件可恢复；接受「最终一致 + 单 run 内一致」（trace 场景弱事务，可接受）。
- **manifest / metastore**：记录有效 segment 集合 + 各 segment deletion/upgrade vector + 热区 checkpoint 位点。崩溃恢复 = 读 manifest 定有效 segment + 重放 checkpoint 后的热区事件。可直接用本实例的 catalog 表承载（即 SmithDB「小 Postgres metastore」角色，单机内嵌，零额外组件）。
- **一致性级别**：单写者（搬迁/compaction 串行发布）+ 多读者 MVCC（读端持 segment+热区不可变快照，一致视图）。flush/compaction 用**原子指针/manifest 切换**，读端持快照引用，旧 segment 引用计数归零后 GC。对「追加为主、偶有更新」的 trace 负载完全够用。

---

## ⑦ 私有化打包与部署形态

**形态：单机服务进程为主（一个 openGauss 实例 + trace 扩展），可选「嵌入式 SDK 直连」。**

- **核心 = openGauss 单实例 + 我们的扩展集**（列存 trace AM、区间编码逻辑、segment 倒排、向量索引、摄入网关、搬迁/compaction 后台 worker）。**单进程模型**（内部多线程池：摄入 / 查询 / 搬迁-compaction 三组），不照搬 SmithDB「无状态三服务」（那是分布式包袱，单机纯负担）。
- **私有化交付 = 一个安装包**：openGauss 内核 + 扩展 + 摄入网关 + 内嵌对象存储（默认本地盘，客户可切 MinIO）打成单一发行物。**本地盘零外部依赖 → 离线/气隙机房一键起**，这是相对 Langfuse（4-6 组件 + ClickHouse DBA + $3-4K/月）和 SmithDB（对象存储原生）的**降维打击**。
- **信创适配**：openGauss 本就国产内核 + Mulan PSL v2，适配国产 OS/CPU（鲲鹏/飞腾/海光等），过国内私有化采购门槛——国际玩家拿不到的护城河。
- **易用性**：对外**原生 SQL**（相对 SmithDB 私有 API 的差异化卖点）+ trace 专用函数/视图：`load_trace_tree(trace_id)`、`rebuild_thread(thread_id)`、`subtree(span_id)`、`semantic_recall(embedding, filters, k)`、`export_trajectory(...)`，让场景开箱即用。
- **多租户档位**：默认共享实例多租户；强隔离档「一租户一进程 + 共享磁盘格式」。
- **嵌入式 SDK 选项**：提供轻客户端直写热区/直读的库形态，给边缘/小规模场景（非主形态）。

---

## ⑧ 摄入接口（OTel/SDK，兼容 LangSmith 生态）

- **双协议入口**：
  - **OTLP/OpenInference**（gRPC + HTTP）：吃掉 OpenLLMetry/Traceloop 采集生态，映射 `gen_ai.*` 语义约定（v1.41，用 `OTEL_SEMCONV_STABILITY_OPT_IN` 管实验态兼容）。
  - **LangSmith 兼容 API**：接受 `run_type/inputs/outputs/dotted_order/parent_run_id`，让客户「换存储不换 SDK」，迁移成本最低。
- **摄入网关职责**：协议归一 → ID 互转 → 大 payload 抽离上传 + 引用 token 替换（SHA256 去重、presigned URL、MIME 白名单、20MB/文件上限参考 Langfuse/LangSmith）→ 折叠 run 事件 → 批量写热区。
- **写可见性基线**：对标 SmithDB ingestion P50 630ms（含落对象存储）；单机本地盘写热区即可查，可见性应远优于此。
- **导出（飞轮出口）**：`export_trajectory` 树感知 + 多模态引用解析，导出 messages/prompt-completion/DPO 偏好对/tool-call 轨迹，对接训练管线。

---

## ⑨ 每个组件的 build-vs-开源取舍

| 组件 | 取舍 | 理由 |
|---|---|---|
| 内核底座（WAL/MVCC/事务/分区/RLS/优化器/执行器/SQL） | **复用 openGauss** | 自有内核、Mulan PSL 可闭源商用、信创加分、团队最熟、上市最快 |
| 行存热区 | **复用**（Ustore/UNLOGGED） | 膨胀友好的高频小写 |
| 列存冷区 segment | **自研 AM（基于 openGauss CStore 演进）** | trace 专用布局 + late materialization + 区间编码物化，现成 CStore 不够 |
| 列式文件格式 | **务实选 openGauss 列存为主；Vortex 作可选演进/PoC 对照** | 主路线避免押注 1 年新格式风险；Vortex（随机读 100x）作后续加速选项 |
| 时间分层 compaction + 页面整理 + GC | **自研，复用团队「磁盘索引/页面整理框架」** | 团队现成资产 |
| 树区间编码 | **自研逻辑**（基于 PG B-tree） | trace 专用，无现成 |
| 全文 + 中文分词 | **复用 PG 全文框架 + zhparser/pg_jieba**；segment 内嵌倒排自研演进 | 成熟生产级中文分词，团队可直接产线化 |
| JSON 索引 | **复用 PG jsonb GIN/jsonb_path_ops** + 自研高频路径物化列 | 现成 + 单机免 vacuum 优化 |
| 向量索引 | **复用团队 HNSW/IVF/DiskANN**（pgvector 起步，DiskANN 做十亿级） | 招牌差异化、团队独有资产 |
| 对象存储抽象 | **开源 object_store 思路 / MinIO**（默认本地盘） | 私有化一码通本地盘/MinIO，无争议 |
| metastore | **复用本实例 catalog**（内嵌，零额外组件） | 即 SmithDB「小 Postgres」角色，单机更省 |
| 摄入网关（OTLP/LangSmith 兼容） | **自研薄层** | 协议归一 + 大 payload 抽离 |
| 执行引擎向量化 | 主用 openGauss 执行器；**向量化算子按需自研** | 列存聚合性能 |

> 与方案 E（纯 Rust 自研）/ 方案 A 路线 C（Rust 底座 + 复用 PG 模块）的边界：本方案**坚持 openGauss 整机内核为地基**，把自研集中在「列存 AM + 区间编码 + segment 索引 + 向量层 + 搬迁/compaction」这几块 trace 专用热路径，最大化复用、最快上市；代价是性能天花板受 PG 内核形态约束（见风险与自评）。

---

## ⑩ 粗略工时与上线节奏

团队充足（PG 内核 + 向量 + Rust + 优化器 + 页面整理）。按「内核扩展路线上市最快」估：

| 阶段 | 周期 | 交付 | 关键工作 |
|---|---|---|---|
| **M0 PoC** | 1.5 月 | 跑通端到端 | openGauss + zhparser + pgvector + 普通行表/分区，OTLP/LangSmith 摄入，trace 树（递归 CTE）+ 中文全文 + 基础聚合。验证产品形态。 |
| **M1 热冷分层 MVP** | +2.5 月 | 可交付内测 | 热区行表 + 后台搬迁 + 列存冷区 segment + 区间编码物化 + late materialization + 大 payload 抽离 + 时间分层 compaction（基础版）。达 SmithDB P50 同级。 |
| **M2 索引体系完整** | +2 月 | 商业可售 v1 | segment 内嵌倒排 + JSON 高频路径物化 + deletion/upgrade vector + 增量物化视图聚合 + 活 trace 直读优化 + 多租户（分区+目录隔离+RLS+配额）。 |
| **M3 向量飞轮 + 私有化** | +2.5 月 | 招牌 v1.5 | DiskANN 单机十亿级语义召回 + filtered ANN + 轨迹导出 + 奖励物化视图 + 单包私有化 + 信创适配 + per-tenant 加密 + 备份。 |
| **M4 打磨** | +1.5 月 | GA | compaction IO 限速调优、P99 稳定性、词典/分词调优、压测、文档。 |

**累计约 10 个月到商业可售 v1（M2 末），约 12.5 个月 GA。** 比纯 Rust 自研（12-18 月）显著快，是「上市最快」路线。

---

## ⑪ 主要风险

1. **PG 内核形态的性能天花板（最大风险）**：行存 + MVCC + 完整 WAL 对「每秒上万乱序小 span」和「列式聚合天花板」天生不如纯 LSM+列存（这正是 LangSmith 当初弃 ClickHouse、SmithDB 自研的根因）。**缓解**：热区 UNLOGGED + 轻量 WAL + 组提交、冷区自研列存 AM、搬迁解耦写读。**残留风险**：极端高频/超大聚合下仍可能落后 SmithDB；需 PoC 实测对标其 P50 基线，不达标则把列存 AM 进一步自研化（向路线 C 滑动）。
2. **自研列存 AM 与 openGauss 内核耦合**：在 CStore 上做 late materialization + 区间编码 + segment 倒排，可能与内核假设冲突、受版本节奏约束。**缓解**：用 TableAM 接口封装、最小化内核侵入。
3. **热区高吞吐档的一致性弱化**：UNLOGGED + 轻量 WAL 是「最终一致」，崩溃可能丢失最后未 checkpoint 的少量事件。**缓解**：提供 logged 稳妥档让客户按 SLA 选；trace 场景对最后几毫秒丢失容忍度高。
4. **中文分词词典质量**：通用 jieba/SCWS 对 LLM/Agent 领域词（工具名、模型名、专有名）切分不佳。**缓解**：内置领域词典 + n-gram 兜底 + 自定义词典接口。
5. **filtered ANN 工程难度**：「租户A + 近7天 + cost>X 内找语义相似」是公认难点。**缓解**：IVF 分区裁剪 + DiskANN filter + 优化器混合代价模型（先粗召回再精过滤 vs 反之）；分阶段（v1 近线、v2 在线低延迟）。
6. **deletion/upgrade vector + merge-on-read 复杂度**：读路径归并多源 + 应用向量，正确性与性能要细打磨。**缓解**：复用团队页面整理框架的成熟 GC/引用计数。
7. **市场假设风险**：「国内尚无 Agent 可观测专用 DB」是否定命题不可证伪，且大厂（阿里 AgentLoop/火山 APMPlus）可能快速补单机私有化。**缓解**：以「单机极简 + 原生中文 + 向量飞轮 + 信创」三+一刀抢时间窗，绑 OTel/LangSmith 生态降迁移成本。

---

## 自评（五维各一句强弱判断）

- **单机性能**：**中**。热冷分层 + 自研列存 AM 能逼近 SmithDB P50 基线，但受 openGauss 行存/MVCC/WAL 内核形态约束，在极端高频小写与超大列式聚合上是本方案的天花板，难做到纯 Rust LSM 引擎的极致——这是用「上市速度」换来的最大让步。
- **上市速度**：**强（最强）**。复用 openGauss 整机内核 + zhparser/pgvector 现成生态，约 10 个月到商业可售 v1、12.5 个月 GA，显著快于纯自研，是五条路线里最快落地的。
- **私有化契合**：**强（最强）**。自有国产内核 + Mulan PSL 可闭源商用 + 信创适配 + 单包本地盘零外部依赖，正面命中国内金融/政企/气隙机房私有化采购的全部硬门槛，国际玩家与开源重栈方案都够不着。
- **差异化护城河**：**强**。原生 SQL + 生产级中文全文 + 单机十亿级 DiskANN 语义召回 + 数据飞轮原语（轨迹导出/奖励物化视图/SOP 抽取），其中向量语义召回是 SmithDB 结构性空档、中文是国际产品全军覆没点，五合一复制成本对手需 1-2 年工程纵深。
- **复用团队能力**：**强（最强）**。PG/openGauss 内核、向量索引（HNSW/IVF/DiskANN）、查询优化器、磁盘索引与页面整理框架、Rust，几乎全部资产零知识断层直接复用，是五条路线里团队能力契合度最高的。

> 一句话定位：在 SmithDB 验证的「LSM + 列存 + merge-on-read + 晚物化 + 自研倒排 + 时间分层 compaction」正确范式上，用 **openGauss 内核做地基、做减法**，砍掉分布式/对象存储包袱，叠加 **原生 SQL + 中文分词 + 物理多租户 + 向量飞轮 + 信创**，以「上市最快 + 授权最干净 + 团队零断层」为核心优势——代价是接受 PG 内核形态带来的性能天花板，用 PoC 实测兜底。

---

相关文件路径：本设计基于 `/Users/Four/JobProjects/yitrace/vex-x/SmithDB 观测数据库.docx` 所在项目的调研背景，未读取/修改任何代码文件（工作目录除该 docx 外为空）。如需落盘，建议保存至 `/Users/Four/JobProjects/yitrace/vex-x/docs/design/2026-06-16_opengauss-trace-db-architecture.md`。
---

# 方案 B: 自研 Rust 引擎（DataFusion + Lance/Arrow）

Both facts confirmed: Lance has built-in jieba/lindera Chinese tokenizer (with downloadable language models + user dictionary support), and vortex-datafusion is a real, actively-maintained DataFusion FileFormat with 100-200x random access. I have enough verified material. Let me write the complete architecture design.

The final deliverable is the architecture document itself (returned as my text output, not saved to a file per the orchestration constraints).

---

# 单机商业版 Agent Trace DB 架构设计（方案 B：自研 Rust 引擎）

> 路线定性：走 SmithDB 的数据范式（LSM + 列式不可变 segment + late materialization + deletion vector + 时间分层 compaction + 自研倒排），但**砍掉它全部的分布式/对象存储/无状态三服务包袱**，落到单机本地盘（NVMe + mmap）。技术栈对齐 SmithDB 同代：**Rust + DataFusion（查询执行）+ Lance/Vortex（列式 segment）+ tantivy（中文全文）+ 自研 LSM/WAL/树索引/向量层**。
> 产品代号下文用 **TraceDB**。所有 SmithDB 事实均来自 2026-05 联网核实材料。

---

## 0. 设计总览（一页纸）

```
                          ┌────────────────────────────────────────────┐
   OTLP / LangSmith SDK   │            TraceDB 单进程（多线程池）          │
   ───────────────────▶   │                                              │
   gRPC/HTTP 摄入          │  ┌─────────┐  归一化  ┌──────────────────┐   │
                          │  │ Ingest  │────────▶ │ WAL (顺序追加,组提交)│   │
                          │  │ 线程池   │          └────────┬─────────┘   │
                          │  └─────────┘                   │             │
                          │       │ 行式写入                │ 重放恢复     │
                          │  ┌────▼──────────────────────────────────┐   │
                          │  │  L0 行式热缓冲 MemTable (per-tenant)     │   │
                          │  │  按 run_id/trace_id 哈希；可原地更新     │   │
                          │  │  ← 活 trace / 点查 100% 命中这里         │   │
                          │  └────┬──────────────────────────────────┘   │
                          │       │ flush(阈值/定时)：建索引 + 物化区间编码  │
                          │  ┌────▼──────────────────────────────────┐   │
                          │  │ L1..Ln 列式不可变 segment（Lance 主/Vortex 备）│
   SQL/HTTP 查询          │  │  core 列 + big-payload 子文件(late mat.) │   │
   ◀──────────────────    │  │  + zone-map + tantivy倒排 + JSON索引     │   │
   DataFusion 执行         │  │  + 向量索引(HNSW/DiskANN) + deletion向量 │   │
                          │  └────┬──────────────────────────────────┘   │
                          │       │ 时间分层 compaction（独立线程池+IO限速）  │
                          │  ┌────▼────────┐   ┌──────────────────────┐  │
                          │  │ Manifest    │   │ Blob Store (本地盘/MinIO)│  │
                          │  │(嵌入式KV/SQLite)│ │ 大payload/多模态 + SHA256去重│  │
                          │  └─────────────┘   └──────────────────────┘  │
                          └────────────────────────────────────────────┘
```

读路径并发扫描 `(活跃 MemTable + 冻结 MemTable + 所有 segment)`，归并时应用 deletion/upgrade vector；点查/活 trace 优先 MemTable，聚合/全文/JSON/向量走 segment。

---

## ① 数据模型与 trace 树编码方案

### 1.1 统一节点模型（同时吃下 OTel GenAI 与 LangSmith 两套协议）

内部抽象：**Span = 一棵 trace 树上的一个节点**，但物理上建模为 **「run = 事件序列（append-only events），而非单条可变行」**（SmithDB 的核心抽象，必须照搬，因为它是乱序/晚到/长运行 span 正确性的地基）。

核心列（小而密集，列存压缩 + 索引）：

| 类别 | 字段 | 说明 |
|---|---|---|
| 标识/树 | `tenant_id`, `trace_id`(根), `span_id`(=run id), `parent_span_id`, `root_id`(冗余), `dotted_order` | `dotted_order` 是 LangSmith 的可排序路径键，子树=前缀匹配、先序=字典序；与 OTel 的 16B trace_id/8B span_id 双向可转 |
| 类型/时间 | `span_kind`(归一枚举), `run_type`/`operation_name`(原文保留), `name`, `start_time`, `end_time`, `first_token_time`, `status`(success/error/**pending**) | pending = 活 trace 未完成态 |
| 会话/过滤 | `thread_id`(=conversation/session), `tags[]`, `metadata`(JSON) | thread_id 必须传播到所有子 run |
| 度量(聚合热点) | `input_tokens`,`output_tokens`,`total_tokens`,`cache_read_tokens`,`reasoning_tokens`,`total_cost`,`prompt_cost`,`completion_cost`,`latency_ms` | 列存向量化聚合 |
| 模型/工具 | `request_model`,`response_model`,`provider`,`finish_reasons[]`,`tool_name`,`tool_call_id` | 过滤维度 |

大字段子文件（late materialization，core 行只存指针）：
- `inputs`/`outputs`（自然语言全文，可 MB 级）、`error`、`serialized`、`events[]`（流式事件序列）、多模态 payload 引用 token。

**归一原则：无损映射。** 保留 `run_type` 与 `operation_name` 原文，内部 `span_kind` 枚举（llm/chat/chain/tool/retriever/embedding/prompt/parser/agent/workflow/memory/thought）只做加速过滤，避免有损映射导致客户原始字段丢失。

### 1.2 树编码：写侧邻接表，读侧物化区间编码（双编码，矛盾消解在 flush）

这是 trace 区别于普通时序日志的最核心命题。决策依据：trace 是**不断生长 + 乱序到达 + 高频碎片化插入**的树，这直接**枪毙嵌套集**（lft/rgt 插一个节点要改半棵树）和**纯物化路径/ltree**（父 span 可能比子 span 晚到，子来时不知道自己路径）。

| 阶段 | 编码 | 树操作复杂度 |
|---|---|---|
| **热区 MemTable（写）** | 邻接表 `parent_span_id` + 冗余 `trace_id`/`root_id` | 插入 O(1)，完全不在乎到达顺序；父晚到只是邻接关系暂悬空 |
| **flush→冷区 segment（读）** | 对每棵已成形 trace 做一次 DFS，物化区间编码 `pre`/`post`（前序/后序序号）+ 显式 `root_id` 列 | 子树查询 = `trace_id=? AND pre BETWEEN root.pre AND root.post`，列式 segment 上连续区间扫，极快 |

- **找根/加载整棵 trace 树**：`trace_id` 等值过滤（segment 按 `(tenant_id, trace_id)` 排序 + zone-map），一次顺序扫一小段 → 对标 SmithDB 92ms。
- **子树查询**：区间编码 BETWEEN，segment 不可变 → 区间编码一旦物化永不重算，彻底规避嵌套集死穴。乱序/晚到的全部复杂度被吸收进「热→冷一次性物化」这一步。
- **flush 后才晚到的极少数 span**：走 upgrade vector 补丁，或回退到邻接表递归补齐，不污染主路径。
- **闭包表**作为可选二级加速结构（查任意 depth 灵活），但每 span O(depth) 行写放大在「每秒大量小 span」下成本偏高，默认不开。

### 1.3 线程重建（跨多 trace 瞬间拼长对话）

独立 `thread_id` 维度二级索引：`thread_id → [span_id...] 按时间排序`（倒排/跳表）。重建 = 按 `thread_id` 拉 run 列表 + 各 run 根 span 的 input/output 小列（late materialization，不拉大 payload）→ 一次范围扫 + 小列投影 → 对标 SmithDB 131ms。

---

## ② 存储引擎与读/写路径

### 2.1 三层 LSM 结构（行式热缓冲 + 列式不可变 segment）

**为什么不是纯列式/纯行式**：trace 写入是高频碎片化、乱序、可更新（span 出生在早上、死亡在下午）。列式对「原地更新一个字段」极不友好（要重写整列 chunk）。所以热区必须行式（写放大最小、晚到 span 原地 merge、活 trace 点查极快），冷区必须列式（OLAP 聚合数量级优势、不可变让倒排/JSON/zone-map 一次建好永不维护、late materialization 解决无界大 payload）。

### 2.2 写路径

```
小 span 写入 → Ingest 归一化(双协议→内部模型) → base64/大payload 抽离到 Blob Store(SHA256去重)
   → 组提交 WAL(顺序追加,fsync 摊薄) → ack 返回(低延迟持久)
   → 写入 per-tenant 行式 MemTable(无锁分片,按 run_id 哈希;晚到 span 原地 merge 成事件序列)
   → 达阈值/定时 → 冻结当前 MemTable → flush:
        · 列式化 core 列(Lance segment) + 大字段子文件
        · DFS 物化区间编码 [pre,post]
        · 建 zone-map / tantivy 倒排(中文分词) / JSON 路径索引 / 向量索引
        · 原子写 manifest 发布新 segment;截断对应 WAL
```

写路径只碰 MemTable + WAL，与查询零锁竞争。**写入即事件流，非可变行**——这是对乱序/晚到/长运行的根本性解法，绝不用「先 INSERT 占位、后 UPDATE 改行」的原地更新（对列存是写放大灾难）。

### 2.3 读路径

```
查询 → DataFusion 计划(谓词下推/late materialization/代价估计)
   → 并发扫 (活跃MemTable + 冻结MemTable + 候选segment)
       · 点查/活trace：优先命中 MemTable(哈希直达)
       · 聚合/全文/JSON/向量：zone-map 剪枝候选 segment → 走对应索引
   → 归并：应用 deletion/upgrade vector(被覆盖/删除行剔除;长运行 span 多事件按 run_id fold 成最终态)
   → late materialization：仅当 project 到大字段才去 Blob Store 取
   → 渐进式时间窗(progressive time-window)：查「最新 N 条 run」时沿时间倒序在最新 segment 建有界时间窗，不全排序
```

### 2.4 列式格式选型：Lance 为主，Vortex 为备（方案 B 的关键取舍）

| 维度 | Lance（推荐主选） | Vortex（降级备选） |
|---|---|---|
| 随机访问 | 极强（双结构编码，单 trace 取几个值不解压整列） | 号称比 Parquet 快 100-200x（已核实 vortex-datafusion 0.68） |
| 中文全文 | **内置 jieba/lindera tokenizer + 用户词典**（已核实，省去自建） | 无，需全靠 tantivy 外挂 |
| 向量索引 | **内置 ANN（IVF/HNSW），billion 级毫秒**（直接服务语义召回） | 无，需自研向量层 |
| 多模态/加列 | AI-native，加列/回填只写新文件（契合数据飞轮反哺 embedding） | 偏纯列存追加 |
| 树查询/JSON/聚合 | 非强项 → DataFusion 补 | DataFusion 补 |
| 成熟度风险 | 较新但活跃，Apache 2.0 | LF 孵化~1 年的新格式，最大风险 |

**取舍结论**：方案 B 选 **Lance 作为列式 segment 主格式**——它是唯一把「随机访问 + 向量检索 + 内置中文全文 + 多模态」打包好的嵌入式格式，恰好覆盖本产品的招牌差异化（语义召回 + 中文）。Vortex 作为格式层降级方案（二者均 Apache 2.0、均与 DataFusion 集成，可平滑切换）以对冲 Lance 在树查询/聚合上的弱项与新格式风险。**风险对冲设计**：segment 格式经一层 `SegmentFormat` trait 抽象，Lance/Vortex 可在 compaction 时透明切换，PoC 阶段以高频乱序 span 实测二者写入/compaction 稳定性后定档。

### 2.5 Compaction：时间分层（time-tiered）

trace 是时序数据（几乎只追加、按时间查、老数据不再变）。时间分层完美匹配：新数据小 segment（写优化、还在等 end 事件、低写放大）→ 老数据合并大 segment（查询优化、压缩比更高、索引更紧凑）。**不用 size-tiered/leveled**（为通用 KV 设计，trace 的时间局部性让时间分层既简单又让「按时间范围裁剪」几乎免费）。compaction 时物化 deletion vector、重建紧凑索引、合并区间编码。复用团队**磁盘索引/页面整理框架**管 segment 落盘、mmap、引用计数与 GC。

---

## ③ 索引体系（树 / 全文含中文 / JSON / 向量）

四套索引全部 **per-segment 内嵌、不可变、flush 时一次性建好、zone-map 可整段跳过**。

### 3.1 树索引
区间编码 `[pre,post]` + `root_id` + `trace_id` 排序（§1.2）。`thread_id → [span...]` 倒排做线程重建。

### 3.2 全文检索（含中文分词，国内差异化核心）
- **引擎 tantivy**（Rust、Lucene 式倒排、近实时、<10ms 启动、按需 mmap），per-segment 倒排；postings/positions 分块（防大分配）、term zone-map 跳段——与 SmithDB 全文布局同构，但单机无对象存储读放大 → 应优于其 400ms。
- **中文分词**：优先 **tantivy-jieba**（基于活跃维护的 jieba-rs；cang-jie 维护较慢，仅备选）；细粒度搜索模式 + n-gram 兜底保召回；短语检索靠 position。**自定义词典**收 Agent/LLM 领域词（工具名、模型名）。
- **双保险**：Lance 自带 jieba/lindera + 用户词典（已核实），可与 tantivy 互为校验/降级。
- 建在 `inputs`/`outputs`/`name`/`error` 等自然语言列（大字段子文件上，不污染 core 小列）。

### 3.3 JSON / 元数据过滤（任意嵌套字段）
- **主路线**：flush 时扫 JSON，把高频路径（`metadata.model`、`metadata.user_id`、`provider`…）**自动物化成独立列** + zone-map/字典编码 → 高频过滤走列式剪枝，极快。
- **补路线**：全 JSON **路径展平倒排** `(json_path, value) → row_id`（字符串走字典/倒排、数值走 zone-map 支持范围），服务任意/低频深字段 `a.b.c=x`。复用团队 PG **GIN/jsonb_path_ops** 经验迁移到不可变 segment（免 GIN 增量更新/vacuum 复杂度）。

### 3.4 向量索引（语义 trace 召回，最大护城河 —— SmithDB 完全没有）
- **来源**：对 run/span 的自然语言 input/output 生成 embedding（可配置采样/异步管线/旁路捕获客户已有 embedding 调用以控成本）。
- **引擎**：单机本地 NVMe 上跑 **DiskANN（十亿级、低内存，正对单机私有化）/ HNSW（热数据低延迟）**，复用团队现成生产代码；或直接用 Lance 内置 ANN 起步。**这正是 SmithDB 架构（对象存储 + 无状态）做不了的——ANN 是有状态、随机访问延迟敏感负载，与对象存储哲学冲突；而我们的单机本地盘是它的最优环境，对手的架构劣势=我们的天然优势。**
- **增量/乱序**：向量段随 LSM compaction 重建合并（复用页面整理框架）。
- **过滤性 ANN（filtered vector search）**：真实查询是「在 租户A、近7天、cost>X 的 trace 里找语义相似」——靠优化器统一代价模型决定「先向量粗召回再标量精过滤 vs 反之」+ IVF 分区裁剪 + DiskANN filter 支持。这本身是技术壁垒。

---

## ④ 活 trace 与实时聚合

### 4.1 活 trace（运行中即查未完成 trace）
天然落在 §2 架构里：**运行中 trace 的所有 span 100% 在内存行式 MemTable**，按 `trace_id`/`run_id` 哈希直达。这就是 SmithDB「ingestion 节点直接服务新鲜数据」的单机版，且因无对象存储/网络往返，延迟更低。查询读 MemTable 不可变快照即可看到「未完成 trace（含 pending 节点）的当前状态」，无需特殊机制。状态 `pending` 标记 + 「run=事件序列」让 end 事件晚到数小时也只是追加一个事件、查询期 fold。

### 4.2 实时聚合（cost/latency/token usage）
- 在线扫描：列式 core 列 + DataFusion 向量化执行，对 cost/latency/token 做 sum/avg/p50/p99/分组，配合 metadata/feedback/tag/时间窗过滤。dashboard 级 < 1s。
- **奖励信号增量物化视图**（飞轮原语）：把人工反馈、LLM-judge 分、cost/latency/token、成功/失败标签作为奖励信号与 trace 关联，**增量物化视图**实时维护（复用团队优化器/物化视图能力）。让「哪些轨迹高奖励/低成本」成为可实时聚合、可索引、可被语义召回过滤的一等数据 → 训练时按奖励采样。把「看板指标」升维成「训练信号」。

---

## ⑤ 单机内多租户隔离

三层隔离（单机多租户本质是「目录隔离 + 资源配额」，比 SmithDB 的 slice 路由 + bucket 隔离更简单可控）：

1. **数据隔离（强制）**：`tenant_id` 作为所有 segment 的最高排序前缀 + **物理分目录**。查询强制带 `tenant_id`，存储层用目录/前缀直接裁剪，「一个租户的查询永不扫另一个租户的文件」。删租户 = 删目录。满足金融/政企私有化合规。
2. **资源隔离**：per-tenant 写入配额、查询并发、MemTable 内存上限、compaction IO 配额独立计量（per-tenant 调度器 + 令牌桶），防单租户高频写打满 IO 饿死他人。
3. **加密/合规**：per-tenant 静态加密密钥落盘加密。
4. **强隔离档位**：大客户提供「一租户一进程 + 共享磁盘格式」部署档，兼顾易用与隔离强度。

---

## ⑥ 崩溃恢复与一致性

经典 WAL + 不可变 segment 的「双真相源」模型，团队 PG 经验直迁：

- **WAL = 唯一可变状态真相源**：所有写（新 span、晚到 update、deletion）先**组提交（group commit）**顺序写 WAL，fsync 摊薄，落盘即 ack → 低延迟 + 持久。匹配高频碎片化写。
- **MemTable = WAL 的内存物化**：崩溃后重放 WAL 重建 MemTable，恢复到崩溃前一刻。
- **不可变 segment 自带持久性**：flush 原子落盘（写完 + fsync + 原子改 manifest）后，其覆盖的 WAL 区段截断（checkpoint）。segment 不可变 → 永不会半写损坏已有数据。
- **Manifest（嵌入式事务性 KV 或 SQLite，承担 SmithDB「小 Postgres metastore」角色，单机更轻）**：记录有效 segment 集合 + 各 segment 的 deletion/upgrade vector + WAL checkpoint 位点。**manifest 原子更新 = 整库一致性提交点**。崩溃恢复 = 读 manifest 定有效 segment + 重放 checkpoint 后 WAL。
- **一致性级别**：**单写者 + 多读者 MVCC**（DuckDB 同款）。读端持 segment+MemTable 不可变快照看一致视图，写端串行追加。trace 以追加为主、偶有更新，单写者完全够用，省掉复杂并发控制。flush/compaction 用**原子指针切换**发布快照，旧 segment 引用计数归零后 GC 回收，无读写锁、无长事务。
- **活 trace 一致性** = 读 MemTable 快照，零额外机制。

---

## ⑦ 私有化打包与部署形态

**结论：嵌入式存储引擎内核（Rust crate）+ 单机服务化外壳，即「嵌入式内核 + 服务化外壳」。**

- **嵌入式内核（定位像 DuckDB 而非 DataFusion）**：DataFusion 是「查询引擎框架」，DuckDB 是「自带存储/事务/WAL 的完整库」。我们造的是完整数据库 → **存储/事务/WAL 自研（团队强项）+ 查询执行复用 DataFusion + 列式格式用 Lance/Vortex**。这三者组合正是 SmithDB 同款、被生产验证。
- **单机服务外壳**：多租户、认证、网络访问（SQL/HTTP/gRPC 端点）、在线备份、监控、TTL 规则引擎——商业产品必需，纯嵌入式库给不了。
- **不照搬 SmithDB 无状态三服务**：那是分布式弹性扩展设计，单机纯负担。单机就是**一个进程 + 内部三组线程池（ingest / query / compaction）**，简单、低延迟、私有化一键起。
- **交付物**：单二进制 + 本地盘零外部依赖（对标 Langfuse 自托管要 PG+ClickHouse+Redis+S3 4-6 组件 + $3-4K/月，这是降维打击）；离线/气隙可装；**信创适配**（国产 OS/CPU）。
- **易用性落点**：对外**原生 SQL**（相对 SmithDB 私有 API 的差异化卖点）+ trace 专用函数/视图：`load_trace_tree(trace_id)`、`rebuild_thread(thread_id)`、`subtree(span_id)`、`semantic_recall(span_id, k, filters)`。
- **数据保留/TTL（私有化硬功能）**：非均匀保留——按内容/标签/规则差异化（error/被标注/进数据集 trace 长期留存；普通 trace 30 天回收）。删除 = 廉价逻辑标记（deletion vector）+ 后台 compaction 物理清除，绝不同步重写。
- **冷热分层**：热层近期小 segment + 内存/SSD 缓存 + 倒排就绪（百毫秒交互）；冷层老数据大 segment 强压缩落本地大盘/自带 MinIO（聚合扫描 + 训练数据导出）。本地缓存 + 文件亲和调度近似 SmithDB 的 sticky routing。

---

## ⑧ 摄入接口（OTel/SDK，兼容 LangSmith 生态）

- **双协议原生**：① **OTLP/OpenInference**（gRPC + HTTP）——吃掉 OpenLLMetry/Traceloop 采集生态；② **LangSmith ingest API 兼容**（run/inputs-outputs/dotted_order）——让客户「换存储不换 SDK」，零迁移成本。
- **归一层**：两套协议在 Ingest 线程统一映射到 §1.1 内部模型，保留原始字段无损。OTel GenAI 仍是 Development 状态（v1.41，属性名可能变），用兼容开关 `OTEL_SEMCONV_STABILITY_OPT_IN` 管理。
- **写入即可见性**：ack（WAL 落盘）→ 数据立即可查（活 trace），对标 SmithDB ingestion P50 630ms 但单机应更优。
- **大 payload/多模态**：Ingest 期检测 base64 data URI → 抽离上传 Blob Store → 引用 token 替换（`@@@media:type=...|id=...@@@` 式，与存储后端解耦满足私有化）。presigned URL + SHA256 去重 + MIME 白名单 + 大小上限（参考 20MB/文件）。
- **飞轮出口（轨迹导出原语）**：原生把 trace 树/线程/语义召回结果一键导出为训练标准格式（messages 数组、prompt-completion、DPO 偏好对、tool-call 轨迹），树感知 + 多模态引用还原 + 增量/流式导出。

---

## ⑨ 每个组件的 build-vs-开源 取舍

| 组件 | 决策 | 理由 |
|---|---|---|
| 查询执行引擎 | **复用 DataFusion**（Apache 2.0） | 向量化执行/计划框架现成，SmithDB 生产验证，团队精力集中在 trace 专用算子（树遍历/向量召回/LSM merge）。不重造向量化执行。 |
| 列式 segment 格式 | **复用 Lance 主 / Vortex 备**（均 Apache 2.0） | Lance 内置中文全文+向量+多模态恰中招牌能力；Vortex 对冲风险。不自研列存格式。 |
| LSM / WAL / MemTable / compaction | **自研（Rust）** | 热路径，决定性能；team 磁盘索引/页面整理框架直接复用；需 trace 专属（行式热区原地更新、时间分层、deletion vector）。 |
| 树编码与索引 | **自研** | trace 专属，无现成开源（区间编码物化 + 线程重建倒排）。 |
| 全文倒排 + 中文分词 | **复用 tantivy + tantivy-jieba**（MIT）+ Lance 内置 jieba 双保险 | Lucene 级倒排 + 活跃 jieba-rs；自建词典即可。不自研倒排引擎。 |
| JSON 路径索引 | **自研（迁移 PG GIN/jsonb_path_ops 思想）** | 内嵌不可变 segment，免 GIN 增量/vacuum 复杂度。 |
| 向量索引 | **复用团队 HNSW/IVF/DiskANN 生产代码** | 最大护城河，现成资产，仅缺接入 trace 流的胶水层。 |
| Manifest/元数据 | **复用 SQLite 或嵌入式事务 KV**（Public Domain） | 承担 metastore 角色，单机更轻，不上独立 PG。 |
| 存储后端抽象 | **复用 object_store crate**（Apache/MIT） | 一码通本地盘/MinIO/S3，私有化默认本地盘、客户自带 MinIO 零改动。无争议采用。 |
| SQL 解析/优化器 | **复用 DataFusion + 自研 trace/向量扩展语法 + 迁移 openGauss 优化器代价模型思想** | 原生 SQL 易用性 + 混合查询统一代价模型。 |
| 摄入协议 | **复用 OTLP/OpenInference proto + 自研 LangSmith 兼容层** | 兼容生态降迁移成本。 |

---

## ⑩ 粗略工时与上线节奏

团队储备（PG/openGauss 内核、HNSW/IVF/DiskANN、Rust、查询优化器、磁盘索引/页面整理框架）与本方案高度匹配——LSM/compaction、向量层、优化器、页面整理几乎全是现成能力迁移，自研主要在「trace 专属胶水」而非「从零造轮子」。

| 阶段 | 周期 | 里程碑 | 主要工作 |
|---|---|---|---|
| **M0 PoC** | 1–1.5 月 | 格式定档 | Lance vs Vortex 在高频乱序 span 下写入/compaction 实测；OTLP 摄入打通；WAL+MemTable+flush 最小闭环 |
| **M1 MVP（可演示）** | +2.5–3 月 | 单 trace/树/线程查询 + 中文全文 | LSM 全路径、区间编码、tantivy+jieba、DataFusion 接 Lance、活 trace、SQL 外壳 |
| **M2 商业可售 Beta** | +3 月 | 多租户 + TTL + 聚合 + 备份 | 物理分目录隔离、资源配额、deletion vector、时间分层 compaction、实时聚合、崩溃恢复加固、LangSmith 兼容、私有化打包/信创适配 |
| **M3 招牌差异化 GA** | +2.5–3 月 | 语义召回 + 飞轮原语 | DiskANN/HNSW 接入 trace 流、filtered ANN、奖励物化视图、轨迹导出、SOP/few-shot 抽取 |

**总计约 9–12 个人月达 GA（团队并行可压缩日历周期）**。相对方案 A（SmithDB 同构需自研全文倒排层）更省，因 Lance 内置中文全文+向量直接抵掉两块自研；相对纯自研（方案 E，12+ 月）省一半。节奏建议：M1 即可对国内私有化客户做 PoC 演示（中文全文 + 单机一键起已是降维差异），M3 语义召回作为招牌签单。

---

## ⑪ 主要风险

| 风险 | 级别 | 缓解 |
|---|---|---|
| **Lance/Vortex 均为~1–2 年新格式**，高频乱序 span 下写入/compaction 稳定性未经大规模 trace 验证 | 高 | M0 PoC 专项压测；`SegmentFormat` trait 抽象让 Lance↔Vortex 透明切换；二者均 Apache 2.0 可平滑降级；极端情况自研最小列存兜底 |
| **DataFusion 树查询/递归弱**，需自写优化器规则把区间编码下推 | 中 | 团队优化器能力直接补；区间编码本质是范围扫，DataFusion 谓词下推可覆盖 |
| **filtered ANN（带标量谓词的向量检索）是公认难点** | 中 | 优化器统一代价模型 + IVF 分区裁剪 + DiskANN filter；分阶段（v1 近线召回、v2 在线低延迟） |
| **embedding 成本**（trace 量大全量 embedding 贵） | 中 | 可配置采样/按租户策略/异步管线/旁路捕获客户已有 embedding 调用 |
| **OTel GenAI 仍 Development，属性名可能变** | 低 | 兼容开关 + 原文保留 + 归一层隔离变更 |
| **Lance 中文 jieba 需下载语言模型**（LANCE_LANGUAGE_MODEL_HOME），离线/气隙部署需内置 | 低 | 安装包内置词典/模型，tantivy-jieba 作主路径（模型可静态编入） |
| **单写者 MVCC 写吞吐上限**（单租户脉冲数百 span/秒） | 低 | MemTable 无锁分片 + 组提交 WAL；目标稳态每秒数千–数万 span，单写者足够 |
| **新格式生态文档薄**，团队学习曲线 | 低 | 团队 Rust + 列存经验可吸收；Lance/Vortex 社区活跃 |

---

## 自评（五维强弱判断）

- **单机性能：强（上限最高）。** LSM + 列式 + 向量化执行 + 本地 NVMe mmap 专为 trace 负载设计，砍掉 SmithDB 全部对象存储/网络往返开销，单机延迟有望全面优于其公布基线（树 92ms/单 run 71ms/全文 400ms）；是相对 PG 行存路线（堆表/MVCC/vacuum 膨胀、做不出列式聚合天花板）的代际优势。
- **上市速度：中（比 PG 扩展慢、比纯自研快）。** 约 9–12 人月达 GA；M0 格式风险与 M3 语义召回是关键路径；但 Lance 内置中文全文+向量直接抵掉两大块自研，且团队能力几乎零知识断层，比从零造引擎省一半。
- **私有化契合：强（核心卖点）。** 单二进制 + 本地盘零外部依赖 + 物理分目录多租户 + per-tenant 加密 + 信创适配，对 Langfuse 式 4-6 组件重运维栈是降维打击，精准命中国内离线/气隙/合规采购痛点。
- **差异化护城河：很强（结构性领先）。** 「Rust LSM 列存 + 原生 SQL + 生产级中文全文 + 单机十亿级 DiskANN 语义召回 + 数据飞轮原语」五合一；其中语义召回是 SmithDB 架构（对象存储 + 无状态）天然做不了、ClickHouse 派也做不顺的点，对手补齐需 1–2 年工程纵深。
- **复用团队能力：强（匹配度最高）。** LSM/compaction↔页面整理框架、向量层↔HNSW/IVF/DiskANN、优化器↔查询优化器、WAL/恢复↔PG 内核、中文分词↔zhparser/pg_jieba 经验、Rust 主栈对齐 SmithDB 同代——五块储备全部用上，自研集中在「trace 专属胶水」而非造轮子。

> 一处诚实声明：本方案选 Lance 为列式主格式依赖其内置中文 jieba/lindera 与向量 ANN（已联网核实存在），但 Lance/Vortex 在「高频乱序小 span」trace 负载下的 compaction 稳定性无公开第三方大规模验证，必须以 M0 PoC 实测定档，这是方案 B 最大的待验证假设。
---

# 方案 C: 基于嵌入式 OLAP（DuckDB 或 chDB）

All critical facts confirmed. These validate the design constraints precisely:

- **DuckDB FTS**: no tokenizer hook → must pre-tokenize Chinese externally (duckdb-vaporetto pattern proves it works), then build FTS index on tokenized column.
- **DuckDB vss/HNSW**: experimental persistence, non-incremental (full rewrite on checkpoint), WAL recovery broken for custom indexes → cannot be the primary live-write vector path.
- **DuckDB concurrency**: single-writer-process, MVCC+WAL, appends never conflict → the external shell must serialize writes.
- **chDB MergeTree**: "too many parts" at small high-frequency inserts (the exact trace workload), single-process RAM-bound, needs async insert/Buffer engine.

I now have everything needed. Writing the complete architecture design.

I have all the verified facts I need. Here is the complete architecture design.

---

# 单机商业版 Trace DB 架构设计 —— 方案 C：嵌入式 OLAP 底座（DuckDB / chDB）+ 自研薄外壳

> 设计日期：2026-06-16。对标 LangChain SmithDB（2026-05）。所有关于 DuckDB/chDB/SmithDB 的能力边界均已联网核实（来源见文末）。
> 本方案的核心命题：**用成熟嵌入式列存做底座，把工程量压在"薄外壳"上，以最快速度上市**。但调研已暴露两条结构性硬伤——**DuckDB FTS 无中文分词 hook、chDB MergeTree 小批高频写「too many parts」**——本设计的全部精巧之处，就是用外壳把这两条硬伤"绕过去"，而不是假装它们不存在。

---

## 0. 选型裁决：底座选 DuckDB，不选 chDB

先给结论，因为它决定了后面所有设计。

| 维度 | DuckDB | chDB（嵌入式 ClickHouse） | 裁决 |
|---|---|---|---|
| 高频碎片化小 span 写入 | 单写者进程 + MVCC + WAL，**appends 永不冲突**；但列存批量化才高效 | **MergeTree 每次 INSERT 生成 part，小批高频触发「too many parts(300)」**——这正是 LangSmith 当年弃 ClickHouse 的核心原因 | **DuckDB**：硬伤可由外壳的写缓冲彻底消解；chDB 的硬伤是引擎级、外壳只能缓解 |
| 嵌入形态 | 进程内库，单文件 `.duckdb` | 进程内库，但底层是 ClickHouse 存储目录 | 平 |
| JSON 任意嵌套过滤 | 原生 `JSON` 类型 + `->`/`json_extract` + 可物化为虚拟列 | `JSON` 类型 + 函数，更强 | chDB 略强，但 DuckDB 够用 |
| 列式聚合（cost/latency/token） | 向量化执行，强 | OLAP 天花板，更强 | chDB 略强，但 DuckDB 满足交互级 |
| 全文检索 + 中文 | FTS 扩展 **无 tokenizer hook**，须外部预分词 | tokenbf/ngram，**无真分词** | 两者都不行 → **都靠外壳自建倒排**，打平 |
| 向量检索 | vss/HNSW **持久化实验性、非增量、WAL 恢复未实现** | 近期加向量，不成熟 | 两者都不能直接用 → **外壳挂独立向量层**，打平 |
| 许可证 | MIT（最干净） | Apache 2.0 | 平，均可闭源私有化 |
| 单机简洁性 | 单文件、零依赖、进程内 | 带 ClickHouse 存储栈包袱 | **DuckDB 更简洁** |

**裁决：DuckDB 为底座。** 决定性理由有二：
1. **写入模型可救**。trace 是"每秒数千~数万乱序小 span"，chDB 的 MergeTree 在这种负载下会触发 too-many-parts，需要 async insert / Buffer engine 才能勉强缓解，但本质上写模型与场景冲突（重蹈 ClickHouse 覆辙）。DuckDB 的写问题不是"不能写"而是"小批写不经济"——这可以被外壳的**行式写缓冲（攒批后批量 append）彻底解决**，DuckDB 的 append 永不冲突，批量 append 正是它的舒适区。
2. **更单机、更简洁、许可证更松**（MIT）。私有化交付一个单文件库，零外部依赖，符合"纯单机极致简洁"硬约束。

> chDB 不是没用——它的 OLAP 聚合是天花板。**保留为可选的"离线分析/导出加速旁路"**（第 9 节），但**不做主存**。

---

## 0.1 一张总架构图

```
┌──────────────────────────────────────────────────────────────────────┐
│                  自研薄外壳（Rust 单进程，"TraceShell"）                   │
│                                                                        │
│  ┌────────────┐  OTLP/gRPC   ┌──────────────────────────────────────┐ │
│  │ 摄入接口层   │ ──────────▶ │ 写路径：WAL → 行式写缓冲(MemTable)      │ │
│  │ OTLP/LangSmith│ HTTP/JSON │   ↑ 攒批/中文分词/JSON展平/embedding旁路 │ │
│  │ /SDK ingest │            │   │ flush(批量append)                    │ │
│  └────────────┘            └───┼──────────────────────────────────────┘ │
│                                │                                         │
│  ┌────────────┐  SQL/HTTP      ▼          原子可见性切换                  │
│  │ 查询接口层   │ ──────────▶ ┌───────────────────────────────────────┐  │
│  │ 原生SQL +    │            │  读路径：归并(MemTable + DuckDB 列存)     │  │
│  │ trace扩展函数 │ ◀───────── │  + 倒排(tantivy) + 向量(DiskANN/HNSW)   │  │
│  └────────────┘            └───────────────────────────────────────┘  │
│  ┌────────────┐  ┌────────────┐  ┌────────────┐  ┌──────────────────┐  │
│  │多租户/认证   │  │后台compaction│ │ TTL/保留规则 │  │ 备份/监控/限流    │  │
│  └────────────┘  └────────────┘  └────────────┘  └──────────────────┘  │
└──────────────────┬─────────────────┬──────────────────┬────────────────┘
                   │                 │                  │
         ┌─────────▼──────┐  ┌───────▼────────┐  ┌──────▼─────────┐
         │ DuckDB 列存     │  │ tantivy 倒排    │  │ 向量索引层      │
         │ (per-tenant     │  │ (per-segment    │  │ DiskANN/HNSW   │
         │  .duckdb 文件)   │  │  中文分词)      │  │ (本地NVMe)     │
         │ 核心列+物化JSON列│  │                │  │                │
         └────────┬───────┘  └────────────────┘  └────────────────┘
                  │ 大payload引用
         ┌────────▼───────┐
         │ object_store    │ 本地盘 / 自带 MinIO（大 payload + 多模态）
         └────────────────┘
```

**边界划分一句话**：DuckDB 只负责它最擅长的"**列式存储 + 列式聚合 + JSON 过滤 + SQL 执行**"；外壳负责它做不好的"**高频写缓冲、中文倒排、向量、树编码、活 trace、多租户、WAL、TTL**"。

---

## ① 数据模型与 trace 树编码方案

### 1.1 统一数据模型（同时吃下 OTel GenAI 与 LangSmith 两套协议）

内部统一抽象：**`span` = 一棵 trace 树上的一个节点**。核心表 `spans` 列设计（落 DuckDB 列存）：

**标识与树结构（核心小列，热扫描）**
- `tenant_id`（最高排序前缀，多租户物理隔离）
- `span_id`（UUIDv7，时间有序）、`trace_id`（树根 ID，冗余冗存到每个 span）、`root_span_id`
- `parent_span_id`
- `pre` / `post`（**区间编码**，flush 时物化，见 1.3）
- `thread_id`（会话 ID，线程重建用）

**类型与时间**
- `span_kind`（归一化枚举：llm/chat/chain/tool/retriever/embedding/prompt/parser/agent/workflow/memory/thought）
- `raw_run_type` / `raw_operation_name`（保留 LangSmith `run_type` 与 OTel `gen_ai.operation.name` 原文，无损）
- `name`、`start_time`、`end_time`、`first_token_time`（TTFT）、`status`（success/error/pending）

**聚合热点列（列存压缩，向量化聚合）**
- `input_tokens` / `output_tokens` / `total_tokens` / `cache_read_tokens` / `reasoning_tokens`
- `total_cost` / `prompt_cost` / `completion_cost`
- `model`、`provider`、`finish_reasons`

**大字段（不进核心行，late materialization）**
- `inputs_ref` / `outputs_ref` / `error_ref`（指向 object_store 的引用 token，或小内容内联）
- `media_refs`（多模态 payload 引用数组）

**任意 JSON（嵌套过滤）**
- `metadata`（DuckDB `JSON` 类型）、`tags`（`VARCHAR[]`）、`extra`、`feedback_stats`

> **协议归一**：摄入层把 OTel span 的 `trace_id`(16B)/`span_id`(8B) 与 LangSmith 的 UUID/`dotted_order` 双向互转，内部统一用 UUIDv7。`dotted_order` 若 SDK 已带则直接解析出父子与排序，未带则由外壳计算。

### 1.2 「run = 事件序列，非不可变行」建模（核心，照搬 SmithDB）

trace 的本质难点：span "早上出生下午死亡"、乱序到达、长运行后更新。**不能用 UPDATE 改行**（列存 UPDATE 是写放大灾难）。

设计：**append-only 事件流 + 查询期折叠**。
- 同一 `span_id` 的多次上报（start 事件、中间 token、end 事件、retry、补充字段）都作为**独立行**追加进写缓冲，带 `event_seq` / `ingest_ts`。
- 查询时按 `span_id` **fold（折叠）**成最终态：`end_time`/`status`/`tokens`/`cost` 取最新非空事件，`inputs` 取 start 事件，等等。折叠规则做成外壳的查询重写（见 ④）。
- flush 到 DuckDB 时，**对已收到 end 事件的 span 直接折叠成单行**（绝大多数 span 是短命的，在写缓冲里就已闭合）；对 flush 时仍未闭合的长运行 span，保留多事件行，后续 end 事件到达时走 **deletion vector + 新行**（见 ②）。

### 1.3 trace 树编码：双编码（写侧邻接表 / 读侧区间编码）

这是 trace DB 区别于普通日志的最核心命题。逐方案权衡后的**最终方案**：

| 编码 | 写（乱序/碎片化） | 找根/祖先 | 加载子树 | 在本方案的角色 |
|---|---|---|---|---|
| 邻接表 `parent_id` | O(1) 极友好 | 递归CTE慢 | 递归CTE慢 | **热区主编码** |
| 冗余 `trace_id`/`root_id` | O(1)（SDK带上） | **O(1) 读字段** | 等值过滤一段 | **找根/全树加载主力** |
| 区间编码 `[pre,post]` | 插入会改半棵树 ❌ | 区间包含快 | **前缀/区间扫极快** | **flush 时一次性物化到列存** |
| 嵌套集 lft/rgt | **枪毙**（插一个改半棵） | — | — | 不用 |
| 闭包表 | 每节点 O(depth) 行写放大 | 极快 | 极快 | 可选二级加速，非主编码 |

**落地三步**：
1. **热区（写缓冲）只存邻接表 + 冗余 `trace_id`/`root_id`**。写入永远 O(1)、完全不在乎到达顺序；父晚到只是邻接暂时悬空，不影响"按 trace_id 取全树"。
2. **flush 时对已成形的 trace 做一次 DFS，物化 `pre`/`post` 区间编码**。子树查询 = `WHERE tenant_id=? AND trace_id=? AND pre BETWEEN ? AND ?`，在 DuckDB 列存上是连续区间扫，配合 `trace_id` 排序 + zone-map（DuckDB 的 min/max + row group），极快。因 segment 不可变，区间编码**一次物化永不重算**，彻底规避嵌套集死穴。
3. **flush 后才晚到的极少数 span** 走 upgrade vector 补丁；查询时回退邻接表递归补齐。
4. **线程重建**：独立 `thread_id → [span_id 按时序]` 倒排（外壳维护），重建长对话 = 按 `thread_id` 拉 run 列表 + 各根 span 的 input/output 小列（late materialization，不拉大 payload）。

> DuckDB 的递归 CTE 可用但慢，**只作为冷数据回退路径**，主路径不依赖它。

---

## ② 存储引擎与读 / 写路径

### 2.1 三层存储（行式热缓冲 + DuckDB 列存 + object_store 大字段）

```
WAL(顺序追加,外壳自管) → 行式 MemTable(可原地更新/折叠) → flush → DuckDB 列存(不可变segment语义)
                                                                  ↘ 大payload → object_store
```

**为什么不让 DuckDB 直接吃高频写**：DuckDB 单批量 append 高效，但每秒数千次单行 INSERT 不经济。**外壳在前面架一个行式写缓冲**：
- 写入先落**外壳自管 WAL**（顺序追加 + 组提交 group commit，摊薄 fsync），ack 即返回 → 写入低延迟持久。
- 同时进**行式 MemTable**（按 `span_id` 哈希索引，支持折叠/原地更新晚到 span）。
- 定时（如 1~2s）或阈值（行数/字节）触发 **flush：把 MemTable 批量 `COPY`/`INSERT` 进 DuckDB**（一次大批量 append，DuckDB 舒适区），同时建该批的 tantivy 倒排、向量索引、物化 JSON 列。

这样**DuckDB 始终只承受批量写**，彻底绕过它"小批不经济"的弱点，且天然规避了 chDB 的 too-many-parts 问题。

### 2.2 写路径（详细）

```
span 事件 → 摄入层(协议归一/中文分词/JSON展平/base64抽离→object_store/embedding旁路)
   → WAL 顺序追加(组提交) → ack
   → 行式 MemTable(按span_id折叠;邻接表;冗余trace_id/root_id)
   → [触发flush] → DFS物化pre/post → 批量写 DuckDB列存
                 → 建tantivy倒排(中文分词后) + 向量段 + 物化高频JSON列
                 → 截断已checkpoint的WAL段
```

### 2.3 读路径（详细）

```
查询 → 解析(原生SQL + trace扩展函数)
   → 归并三源:
       (a) 活跃MemTable快照(活trace/最新数据,点查极快)
       (b) DuckDB列存(历史:聚合/JSON/区间扫,delete/upgrade向量在归并时应用)
       (c) tantivy倒排(全文) / 向量层(语义召回) → 出row_id/span_id → 回DuckDB取列
   → 按span_id折叠事件序列 → 应用deletion/upgrade向量 → 返回
```

- **点查/活 trace**：优先命中 MemTable（运行中 trace 100% 在内存），按 `trace_id`/`span_id` 哈希直达，延迟最低。
- **聚合/JSON/树扫**：走 DuckDB 列存 + zone-map 裁剪。
- **全文/向量**：先走外壳索引出候选 id，再回 DuckDB 投影列（late materialization）。

### 2.4 列式格式：直接用 DuckDB 原生存储，不引 Vortex

方案 C 的精神是"最大化上市速度"，因此**不引入 Vortex**（那是方案 A 的事）。直接用 DuckDB 的原生列存（row group + 压缩 + zone-map）。代价是随机访问不如 Vortex 的 100x，但**单 run/活 trace 的随机访问已被 MemTable 接管**，DuckDB 主要承担"历史聚合/过滤/树区间扫"这类列式扫描负载——这正是 DuckDB 的强项，够用。

### 2.5 Compaction（外壳驱动，时间分层）

DuckDB 自身有 checkpoint，但 trace 的"不可变 segment + 时间分层 + deletion vector"语义要外壳实现：
- 外壳把数据按**时间桶 + tenant** 组织成 DuckDB 内的逻辑段（分区表或多文件 attach）。
- **时间分层 compaction**：新数据小段（写优化、待补 end 事件），老数据合并大段（查询优化、压缩更紧、重建更紧凑倒排）。新数据少压实（还会等 end 事件），老数据合并。
- compaction **独立后台线程池 + IO 限速（令牌桶）**，避免抢占前台写/查，保 P99（SmithDB 靠拆无状态服务，单机靠线程优先级 + IO 配额达到同效）。

---

## ③ 索引体系（树 / 全文含中文分词 / JSON / 向量）

DuckDB 只能提供其中一部分，其余由外壳自建。明确边界：

| 索引 | 由谁提供 | 实现 |
|---|---|---|
| **树（子树/找根）** | **外壳 + DuckDB** | 区间编码 `pre/post` 物化为 DuckDB 列 + 排序 + zone-map；找根靠冗余 `trace_id` 列 |
| **全文 + 中文分词** | **外壳自建（tantivy）** | DuckDB FTS 无 hook，**不用它**；外壳挂 tantivy + tantivy-jieba |
| **JSON 嵌套过滤** | **DuckDB 为主 + 外壳补** | 高频路径物化为虚拟列；任意低频路径用 DuckDB `json_extract` + 外壳路径倒排兜底 |
| **向量语义召回** | **外壳自建（DiskANN/HNSW）** | DuckDB vss 持久化不可靠，**不用它**；外壳挂独立向量层 |

### 3.1 全文检索 + 中文分词（外壳自建，绕过 DuckDB FTS）

**核实结论：DuckDB FTS 扩展无 tokenizer hook，无法挂中文分词**（社区方案 duckdb-vaporetto 证明唯一出路是"外部预分词 → 在分好词的列上建 FTS index → 查询前用同款分词器切 query"）。

本设计**不走 DuckDB FTS 的预分词绕路**（它仍是 BM25 玩具级，且与外壳的 segment 生命周期脱节），而是**外壳直接挂 tantivy**：
- **per-segment 倒排**：每个 DuckDB 逻辑段 flush 时，外壳用 tantivy 对 `inputs/outputs/name/error` 文本列建一份不可变倒排（与段同生命周期），查询时各段倒排并行查 + 归并，term zone-map 跳段。
- **中文分词：优先 tantivy-jieba**（核实：tantivy-jieba 较新维护、底层 jieba-rs 活跃；cang-jie 维护较慢，作备选）。细粒度切词 + n-gram 兜底保召回；短语检索靠 tantivy positions。
- **自定义词典**（Agent/LLM 领域词、工具名、模型名）提升准确度——这是**国内私有化差异化卖点**，SmithDB 中文是空白。
- doc_id 直接映射 DuckDB 段内行号，查出 id 回 DuckDB 取列。

> 这是相对 DuckDB/chDB/SmithDB 都欠缺、而 tantivy 最成熟的一环，是国内场景的护城河。

### 3.2 JSON / 元数据过滤

- **主：高频路径物化列**。flush 时扫 `metadata`，把高频路径（`metadata.model`、`metadata.user.tier` 等）提升为 DuckDB 独立列（DuckDB 物化/生成列），高频过滤走列式 zone-map 裁剪。
- **补：任意嵌套字段**用 DuckDB 原生 `json_extract`/`->`/`@>` 直接过滤（DuckDB JSON 能力够用），低频深字段不必预建。
- **可选**：对超高基数任意路径场景，外壳建 `(json_path,value)→row_id` 路径倒排（迁移 PG GIN `jsonb_path_ops` 思路），但 v1 先靠 DuckDB 原生 JSON，按需再加。

### 3.3 向量语义召回（外壳自建，差异化招牌）

**核实结论：DuckDB vss/HNSW 持久化是实验性的——非增量（checkpoint 全量重写索引）、WAL 恢复对自定义索引未实现、官方建议勿用于生产。** 因此**绝不能让 DuckDB vss 承担 live trace 的向量写**。

设计：**外壳挂独立向量层（复用团队 DiskANN/HNSW/IVF）**：
- 每个 DuckDB 逻辑段对应一份向量段（本地 NVMe），**DiskANN 服务单机十亿级低内存**（正好匹配"纯单机 + 本地盘"约束，且是 SmithDB 对象存储架构做不顺的点）。
- embedding 来源：摄入层**旁路捕获**客户已有 embedding（很多 agent 本就调 embedding API），或异步采样生成，避免全量成本。
- **过滤性 ANN**：查询"租户A/近7天/cost>X 的相似 trace" = 先按 tenant/时间段裁剪段集 → 段内 ANN → 回 DuckDB 标量精过滤；IVF 按分区裁剪 + 优化器决定先粗召回还是先过滤。
- 向量段同样走外壳 compaction（段合并时重建），增量插入走 LSM 思路。

> 这是 SmithDB **完全没有**的能力（核实：其公开材料无 vector/semantic/embedding），是本产品最强差异化——把可观测性存储升级为"参与 agent 推理回路的语义检索底座 + 数据飞轮引擎"。

---

## ④ 活 trace 与实时聚合实现

### 4.1 活 trace（运行中即可查）

- 运行中 trace 的 span 全在**行式 MemTable**，按 `trace_id` 哈希直达；查询读 MemTable 一致快照即可看到"未闭合 trace 的当前状态"（含 `pending` 节点）。
- 这是 SmithDB "读 ingestion 节点本地缓存" 的单机版，且**无对象存储/网络往返，延迟更低**。
- 查询计划（外壳的查询重写器）对每个查询自动 `UNION` "MemTable 快照 + DuckDB 列存"，对用户透明——一条 SQL 同时看到活数据和历史数据。

### 4.2 实时聚合（cost / latency / token usage）

- **历史聚合**：直接走 DuckDB 向量化执行（`SUM`/`AVG`/`quantile`/`GROUP BY`），DuckDB 强项，交互级延迟。
- **活数据聚合**：MemTable 维护轻量增量累加器（per-tenant 的 cost/token/latency 滚动桶），查询时与 DuckDB 历史聚合合并。
- **奖励信号物化视图（数据飞轮原语）**：把 feedback/eval 分/cost/latency/成功标签作为奖励信号，用 DuckDB 物化视图 + 外壳增量维护，让"高奖励轨迹"可实时聚合、可被向量召回过滤 → 直接做训练采样源。

---

## ⑤ 单机内多租户隔离

三层隔离（私有化硬约束）：

1. **数据隔离（强制，物理）**：**每租户独立 `.duckdb` 文件 + 独立 object_store 子目录 + 独立 tantivy/向量段目录**。`tenant_id` 同时作为段内最高排序前缀。查询强制带 `tenant_id`，存储层目录级裁剪，一个租户永不扫另一租户文件。**删租户 = 删目录**，极简，满足合规。
   - 权衡：每租户一文件在租户数极多（数千）时文件句柄/attach 开销大 → 对小租户提供"共享文件 + `tenant_id` 行级强制过滤 + RLS"的混合档位；大客户走独占文件。
2. **资源隔离**：per-tenant 写入配额、查询并发、MemTable 内存上限、compaction IO 配额，外壳调度器 + 令牌桶。防单租户高频写打满 IO 饿死他人。DuckDB 单写者进程的全局写串行由外壳的 per-tenant 写队列削峰（攒批后批量 append）化解。
3. **加密/合规**：per-tenant 静态加密密钥落盘加密（DuckDB 支持数据库加密），满足金融/政企。

> SmithDB 的多租户依赖 slice 路由 + 对象存储 bucket，单机不适用；单机多租户本质是"目录隔离 + 配额"，反而更简单可控。

---

## ⑥ 崩溃恢复与一致性

**双真相源模型**（团队 PG/WAL 经验直迁）：

- **外壳自管 WAL = 唯一可变状态真相源**：所有写（新 span、晚到 update、deletion）先组提交写 WAL，落盘即 ack。**不依赖 DuckDB 的 WAL 做 span 级持久化**（DuckDB WAL 服务它自己的批量 append）。
- **MemTable = WAL 的内存物化**：崩溃后重放 WAL 重建 MemTable。
- **DuckDB 列存 = 已 flush 数据的持久层**：一次 flush = 一个批量事务（DuckDB MVCC+WAL 保证原子）。flush 提交成功后，对应 WAL 段才 checkpoint 截断。
- **manifest（外壳元数据，可用内嵌 SQLite 或 DuckDB 系统表）**：记录"当前有效段集合 + 各段 deletion/upgrade 向量 + WAL checkpoint 位点 + 倒排/向量段位置"。**manifest 原子更新 = 整库一致性提交点**。崩溃恢复 = 读 manifest 定有效段 + 重放 checkpoint 后 WAL。
- **关键纪律：tantivy 倒排段、向量段、DuckDB 列存段的可见性必须由 manifest 原子切换统一发布**——三者要么都对查询可见，要么都不可见，避免"列存有数据但倒排没建好"的不一致。flush 流程：写 DuckDB → 建倒排 → 建向量 → **最后原子改 manifest** → 截断 WAL。任一步崩溃，manifest 未提交，重放 WAL 重做整批。
- **一致性级别**：单写者 + 多读者 MVCC（DuckDB 同款，核实其即此模型）。读端持快照看一致视图。trace 以追加为主、偶有更新，单写者完全够用。
- **避开 DuckDB vss/HNSW 的 WAL 恢复缺陷**：因为向量索引由外壳管、不入 DuckDB，**DuckDB vss 的崩溃丢索引风险被完全规避**——向量段崩溃后由外壳从 WAL/列存重建。

---

## ⑦ 私有化打包与部署形态

**形态：嵌入式内核 + 服务化外壳（"嵌入式内核 + 单机服务进程"）。**

- **内核 = DuckDB（嵌入式库）+ 外壳的存储/索引模块**，全部进程内、零网络。
- **外壳 = 单机服务进程**（Rust 单二进制），对外暴露 gRPC（OTLP 摄入）+ HTTP/REST + **原生 SQL 端点**，内含多租户/认证/限流/在线备份/监控。
- **为什么不止于嵌入式库**：多租户、私有化、认证、网络访问、在线备份是商业产品必需，纯库给不了 → 必须有服务进程外壳。
- **为什么不照搬 SmithDB 无状态三服务**：那是分布式弹性扩展设计，单机纯负担。单机就是**一个进程，内部三组线程池（ingestion / query / compaction）**，简单、低延迟、一键起。
- **交付物**：单二进制 + 配置文件 + systemd/容器，本地盘即可跑；客户自带 MinIO 则 object_store 零改动指向。**离线/气隙可装**，无外部依赖。
- **信创**：Rust + DuckDB(MIT) 可在国产 OS/CPU 编译；中文分词 + 信创适配是国内私有化采购门槛兼护城河。
- **易用性**：原生 SQL（相对 SmithDB 私有 API 的差异化）+ 内置 trace 专用函数/视图：`load_trace_tree(trace_id)`、`rebuild_thread(thread_id)`、`subtree(span_id)`、`semantic_recall(span_id, k, filters)`，开箱即用。

---

## ⑧ 摄入接口（OTel / SDK，兼容 LangSmith 生态）

- **OTLP/gRPC + OTLP/HTTP**（吃掉 OpenLLMetry/Traceloop 采集生态，"换存储不换 SDK"）。
- **LangSmith 摄入协议兼容端点**：接受 LangSmith Run 格式（含 `dotted_order`/`inputs`/`outputs`），让现用 LangSmith SDK 的客户零改码迁移。
- **OpenInference** 兼容。
- 摄入层职责：协议归一（双 ID 互转）、**中文分词预处理**、JSON 展平/高频路径标记、**base64/大 payload 抽离 → object_store**（SHA256 去重 + presigned URL + MIME 白名单 + 大小上限 20MB 参考）、**embedding 旁路捕获**、写 WAL + MemTable。

---

## ⑨ 每个组件的 build-vs-开源 取舍

| 组件 | 取舍 | 理由 |
|---|---|---|
| 列式存储 + SQL 执行 + JSON + 聚合 | **开源 DuckDB（MIT）** | 方案 C 核心，最快上市；JSON/聚合/SQL 现成 |
| 高频写缓冲 + WAL + MemTable + 折叠 | **自研（外壳）** | DuckDB 小批写不经济；必须外壳攒批 |
| 全文倒排 + 中文分词 | **开源 tantivy + tantivy-jieba，外壳集成** | DuckDB FTS 无 hook；tantivy 最成熟 |
| 向量索引 | **复用团队 DiskANN/HNSW/IVF** | DuckDB vss 持久化不可靠；团队现成资产=招牌差异化 |
| 树编码（区间/邻接/冗余根） | **自研（外壳）** | trace 专用，无现成 |
| 大 payload 存储抽象 | **开源 object_store crate** | 本地盘/MinIO 一码通，无争议 |
| metastore/manifest | **开源 SQLite/内嵌，外壳逻辑自研** | 轻量；SmithDB 用 Postgres，单机用 SQLite 更轻 |
| compaction/TTL/多租户/认证/限流/备份 | **自研（外壳）** | 商业产品必需，无现成 |
| 查询引擎骨架（如需扩算子） | **DuckDB UDF/扩展为主，必要时 DataFusion 旁路** | 优先 DuckDB；复杂自定义算子可引 DataFusion |
| OLAP 重聚合/离线导出旁路 | **可选 chDB（Apache 2.0）** | 聚合天花板，做导出/训练数据加速旁路，非主存 |

---

## ⑩ 粗略工时与上线节奏

团队基线：PG/openGauss 内核 + 向量索引 + Rust + 优化器 + 磁盘/页面整理框架，人才充足。

| 阶段 | 内容 | 工时 | 里程碑 |
|---|---|---|---|
| **M1 MVP（0–2.5 月）** | DuckDB 底座接入；外壳 WAL+MemTable+批量 flush；邻接表+冗余 trace_id；OTLP/LangSmith 摄入；原生 SQL + 基础 trace 函数；单租户 | ~2.5 人月×3 | **能写能查能装**，活 trace + 树加载跑通 |
| **M2 索引（2.5–5 月）** | tantivy + 中文分词集成；高频 JSON 物化列；区间编码物化；线程重建；实时聚合；deletion/upgrade 向量 | ~3 人月×3 | 七类查询全覆盖，对标 SmithDB P50 |
| **M3 向量+飞轮（5–7.5 月）** | DiskANN/HNSW 向量层接入；语义召回 SQL 原语；奖励物化视图；轨迹导出 | ~3 人月×3 | **招牌差异化上线** |
| **M4 商业化（7.5–9.5 月）** | 多租户物理隔离 + 配额 + 加密；TTL/差异化保留；在线备份/监控/限流;信创适配;打包 | ~2.5 人月×3 | **可私有化交付 GA** |

**总计约 9.5 个月、~33 人月**（3 人核心团队节奏）。比方案 A（自研 Vortex LSM，12+ 人月起、Vortex 风险）快，比纯自研（12+ 人月）快得多。**上市速度是方案 C 的核心卖点。**

---

## ⑪ 主要风险

| 风险 | 等级 | 缓解 |
|---|---|---|
| **DuckDB 单写者进程成写入瓶颈** | 中 | 外壳攒批 + 批量 append（DuckDB 舒适区）；per-tenant 文件分散全局写锁；实测峰值脉冲（单任务爆发数百 span）下的 flush 延迟 |
| **DuckDB 列存随机访问不如 Vortex（无 100x）** | 中低 | 活 trace/单 run 随机访问已被 MemTable 接管；DuckDB 只承担列式扫描（其强项）；若历史点查不达标，留 Vortex 作为段格式升级位（方案 A 降级路径） |
| **三套索引（DuckDB列存/tantivy/向量）一致性复杂** | 中高 | manifest 原子切换统一发布可见性；flush 严格顺序 + WAL 重放整批重做；这是外壳最需投入测试的点 |
| **DuckDB 大数据量 checkpoint/compaction 抖动影响 P99** | 中 | compaction 独立线程 + IO 限速；时间分层减少重压；按时间桶分段限制单次 compaction 范围 |
| **tantivy 中文分词成熟度（cang-jie 维护慢）** | 中低 | 优先 tantivy-jieba；自建词典；n-gram 兜底；分词器可插拔 |
| **DuckDB 作为"嵌入库"被当主存的长期可控性**（版本节奏、格式演进、Quack 等新特性方向不定） | 中 | MIT 许可证可 fork 自控；外壳把 DuckDB 隔离为"列存引擎"接口，预留替换为 Vortex/自研段格式的抽象层，降低锁定 |
| **海量小租户下每租户一文件的句柄/attach 开销** | 低 | 小租户共享文件 + RLS 行级隔离档位；大客户独占文件 |
| **embedding 成本与覆盖** | 中 | 旁路捕获客户已有 embedding；采样/按租户策略；v1 近线、v2 在线 |

---

## 自评（五维强弱判断）

- **单机性能**：**中**。DuckDB 列式聚合/JSON 强、活 trace 走 MemTable 点查快，能对标 SmithDB 交互级 P50；但缺 Vortex 的 100x 随机访问，且 DuckDB 单写者 + checkpoint 抖动是性能天花板，极致写吞吐与 P99 稳定性弱于方案 A 的自研 LSM。

- **上市速度**：**强（本方案最大优势）**。复用成熟 DuckDB 省掉列存/SQL/聚合/JSON 自研，~9.5 月可 GA，显著快于方案 A/纯自研——这正是选方案 C 的根本理由。

- **私有化契合**：**强**。单文件 DuckDB(MIT) + Rust 单二进制 + 本地盘/object_store + 离线可装 + 物理多租户 + 信创可编译，"装上就能用"的极简私有化正是国内空白点，契合度极高。

- **差异化护城河**：**中强**。中文分词（tantivy-jieba）+ 原生 SQL + DiskANN 单机十亿级语义召回 + 数据飞轮原语，是 SmithDB 全军覆没的点；但底座用通用 DuckDB 而非自研专用引擎，"专用引擎"叙事弱于方案 A，护城河更多在外壳的中文/向量/飞轮而非存储底座本身。

- **复用团队能力**：**中强**。Rust、向量索引（DiskANN/HNSW/IVF）、磁盘/页面整理框架（用于段管理与 compaction）、查询优化器（混合查询代价模型）、PG 元数据/中文分词经验均可复用；但"PG/openGauss 内核"这块最深的资产**被 DuckDB 替代而闲置**——这是方案 C 相对方案 A/F 在团队能力复用上的主要折损。

---

**关键事实来源（已联网核实，2026-06）**：
- [DuckDB Full-Text Search Extension](https://duckdb.org/docs/current/core_extensions/full_text_search) / [duckdb-vaporetto（CJK 须外部预分词的证明）](https://github.com/hotchpotch/duckdb-vaporetto) / [fts trigram tokenizer discussion](https://github.com/duckdb/duckdb/discussions/16071)
- [DuckDB Concurrency（单写者+MVCC+WAL，appends 永不冲突）](https://duckdb.org/docs/current/connect/concurrency) / [Analytics-Optimized Concurrent Transactions](https://duckdb.org/2024/10/30/analytics-optimized-concurrent-transactions)
- [DuckDB VSS Extension（HNSW 持久化实验性、非增量、WAL 恢复未实现）](https://duckdb.org/docs/current/core_extensions/vss) / [duckdb-vss GitHub](https://github.com/duckdb/duckdb-vss)
- [The 'Too Many Parts' Problem in ClickHouse（chDB 小批高频写硬伤）](https://bigdataboutique.com/blog/clickhouse-too-many-parts) / [ClickHouse vs chDB embedded（chDB 单进程 RAM 限制）](https://www.tinybird.co/blog/clickhouse-vs-chdb-embedded)
- [LangChain: Introducing SmithDB](https://www.langchain.com/blog/introducing-smithdb)（对标基线，调研材料已核实）
- [tantivy-jieba / cang-jie（中文分词）](https://github.com/quickwit-oss/tantivy)
---

# 方案 D: 自由组合最优解

Both facts confirmed. Vortex file format is backward-compatible from 0.36.0 (de-risks the format bet), and SmithDB self-hosting is now positioned as a real requirement. The research corpus is comprehensive and verified. I have everything needed to write the complete architecture. No further searches required.

Here is the complete design.

---

# 单机商业版 Trace DB 架构设计 —— 代号 **yiTrace**

> 方案 D：自由组合最优解。目标 = 国内首个「Agent 可观测性专用、纯单机、私有化、原生中文、向量飞轮内建」的商业数据库。
> 设计原则：**复刻 SmithDB 验证过的数据范式（LSM + 列式不可变 segment + 晚物化 + deletion vector + 时间分层 compaction + 自研倒排），砍掉它的分布式/对象存储包袱，加上「原生 SQL + 中文分词 + 物理多租户 + 语义召回 + 数据飞轮」五把差异化刀。**
> 关键事实已联网核实（2026-06）：Vortex 文件格式自 0.36.0 起向后兼容（格式风险可控）；SmithDB 自托管已成正式诉求，但仍为对象存储+无状态三服务形态——单机本地盘是我们的结构性优势。

---

## 0. 整体形态一览

```
                         yiTrace 单进程（多线程池）
   ┌──────────────────────────────────────────────────────────────────────┐
   │  接入层  OTLP/gRPC · OpenInference · LangSmith-compat REST · 原生 SQL    │
   ├──────────────────────────────────────────────────────────────────────┤
   │  租户路由 + 认证 + 配额  (per-tenant 调度器 / 令牌桶 / 行级权限)           │
   ├───────────────┬───────────────┬───────────────┬──────────────────────┤
   │  写线程池      │   查询线程池    │  compaction池  │   embedding/飞轮池     │
   │  WAL→MemTable  │  DataFusion执行 │  时间分层合并   │   异步向量化/导出       │
   ├───────────────┴───────────────┴───────────────┴──────────────────────┤
   │              统一存储引擎（自研 Rust LSM 内核）                          │
   │   ┌─────────────┐   flush   ┌───────────────────────────────────────┐  │
   │   │ 行式热区      │ ────────► │  列式不可变 Segment (Vortex)            │  │
   │   │ MemTable     │           │  核心列 · 大字段子文件 · 内嵌索引       │  │
   │   │ (可原地更新)  │           │  [zone-map][倒排][JSON路径][向量段]    │  │
   │   └─────────────┘           └───────────────────────────────────────┘  │
   │   Manifest / Metastore（内嵌 SQLite，承担 SmithDB 那个"小 Postgres"角色） │
   └──────────────────────────────────────────────────────────────────────┘
        落盘：本地 NVMe（默认）│ 可选挂 MinIO/S3（object_store crate，零改码）
```

形态选择：**嵌入式 Rust 内核（自带存储/WAL/事务，DuckDB 式 in-process）+ 单机服务外壳**。详见 §7。

---

## 1. 数据模型与 Trace 树编码

### 1.1 统一数据模型（同时吃 OTel GenAI 与 LangSmith 两套协议）

内部抽象核心：**`Span = 一棵 trace 树上的一个节点；run 是事件序列而非不可变行`**。无损归一，保留双协议原文字段。

核心列（小、密、列存 + 索引）：

| 类别 | 字段 |
|---|---|
| 标识/树 | `span_id`(UUIDv7) · `trace_id`(=根) · `root_id` · `parent_span_id` · `pre`/`post`(区间编码,flush 时物化) · `tenant_id` |
| 类型/时间 | `span_kind`(归一枚举:llm/chat/chain/tool/retriever/embedding/prompt/parser/agent/workflow/memory/thought) · `name` · `start_time` · `end_time` · `first_token_time` · `status`(success/error/pending) |
| 会话 | `thread_id`(=conversation/session) |
| 聚合热点 | `input_tokens`/`output_tokens`/`total_tokens` · `cache_read`/`reasoning_tokens` · `total_cost`/`prompt_cost`/`completion_cost` · `model`/`provider` |
| 过滤维度 | `tags[]` · `feedback_stats` · `reference_example_id` · `in_dataset` |
| 原文保留 | `run_type`(LangSmith) · `gen_ai.operation.name`(OTel) · `dotted_order`(LangSmith 排序键) |

大字段（外置/晚物化，独立子文件 + 指针）：`inputs`/`outputs` 自然语言全文、`serialized`、`events` 事件流、多模态 payload 引用。

### 1.2 Trace 树编码：写侧邻接表 + 读侧区间编码（双编码）

这是区别于普通时序日志的核心命题。结论（已在调研中逐方案权衡，**枪毙嵌套集**——任何插入改半棵树，在「每秒大量小 span」下不可用）：

- **写侧（MemTable，对乱序/碎片化最友好）**：只存 `span_id + parent_span_id + trace_id + root_id`。写入永远 O(1)，**父 span 晚到也无所谓**，邻接关系暂时悬空。`trace_id`/`root_id` 由 ingestion 直接带上 → 「找根」O(1) 读字段，「加载整棵 trace 树」= 按 `trace_id` 等值过滤。
- **读侧（flush 时一次性物化，对子树/先序遍历最友好）**：对已基本成形的 trace 做一次 DFS，物化 `pre`(前序号)/`post`(后序号) 区间编码。**子树查询 = `WHERE trace_id=? AND pre BETWEEN root.pre AND root.post`**，列式 segment 上一段连续区间扫，极快。segment 不可变 → 区间编码一旦物化永不重算，彻底规避嵌套集死穴。
- **矛盾消解点**：「写友好编码」与「读友好编码」的冲突，全部被吸收在「热区→冷区一次性物化」这一步。flush 后极少数晚到 span 走 upgrade vector 补丁或邻接表回退路径。
- **线程重建**：独立 `thread_id → [run_id...按时间排序]` 二级索引（跳表/倒排）。重建长对话 = 按 thread 拉 run 列表 + 各根 span 的 input/output 小列（晚物化，不拉大 payload）。

> 与团队能力对齐：`dotted_order` 作为一等索引列保留（LangSmith 生态前缀匹配语义），区间编码作主路径。

---

## 2. 存储引擎与读/写路径

三层结构（与 SmithDB 同构，落到本地盘 + mmap）：

```
WAL(顺序追加,唯一可变真相源) → L0 行式 MemTable(可原地更新)
   → flush → L1..Ln 列式不可变 Segment(Vortex,时间分层) + deletion/upgrade vector
```

**为什么行式热区 + 列式冷区**（不是纯列或纯行）：trace 写入 = 高频碎片化 + 乱序 + 可更新。列式对「原地改一个字段」极不友好（重写整列 chunk）。所以：
- 热区**行式**：碎片化小 span 追加 O(1)；长 span 晚到 update = 改一行；活 trace 100% 在内存按 `run_id`/`trace_id` 哈希直命中（= SmithDB「ingestion 节点直服务新鲜数据」的单机版，因无对象存储/网络，延迟更低）。
- 冷区**列式不可变**：OLAP 聚合（cost/latency/token）列存 + 向量化是数量级优势；不可变让倒排/JSON 路径/zone-map/向量段在 flush 时一次建好、永不维护更新。
- **晚物化（照搬）**：segment 内核心小列与大 payload 分文件存。list/filter/聚合只读小列；大 payload 仅在用户真点开某条 trace 时按需拉取 → 把「无界大 payload」问题彻底解决。

**写路径**：`WAL 组提交(group commit,摊薄 fsync) → MemTable(无锁分片) → 阈值/定时 flush 成 Vortex segment(同时建索引) → SQLite manifest 原子登记`。写路径只碰 MemTable + WAL，与查询零锁竞争。

**读路径**：同时扫 `活跃 MemTable + immutable MemTable + 列式 segment`，各源出结果归并，deletion/upgrade vector 在归并时应用。点查/活 trace 优先命中 MemTable；聚合/全文/JSON/向量扫 segment（zone-map 先剪枝整段）。

**列式格式选型：直接用 Vortex**（已核实：0.36.0 起格式向后兼容，随机读 ~100x、扫描 10-20x、写 5x，SmithDB 同款生态对齐，风险可控）。用团队**磁盘索引/页面整理框架**管 Vortex segment 的本地落盘、mmap、引用计数与 GC（替换 SmithDB 的对象存储读路径）。

**Compaction：时间分层（time-tiered）**。trace 是时序数据（追加为主、按时间查、老数据不变），新数据留小 segment（写优化、还在等 end 事件）、老数据合并大 segment（查询优化、压缩更紧、索引更紧凑）。**不用 leveled/size-tiered**（为通用 KV 设计，浪费 trace 的时间局部性）。compaction 时物化 deletion vector、合并区间编码、重建紧凑索引。

**Mutation：deletion/upgrade vector**。已 flush 的 span 晚到更新不重写文件，只在 segment 元数据挂向量，读时合并、compaction 时物化 → 「出生在早上、死亡在下午」的长运行 trace 不引发写风暴。

---

## 3. 索引体系

四类索引，全部 **per-segment 内嵌、不可变、zone-map 可跳段**：

### 3.1 树索引
区间编码 `[pre,post]` + 冗余 `trace_id`/`root_id` 列（§1.2）。segment 按 `(tenant_id, trace_id, pre)` 排序 → 子树扫为连续区间。

### 3.2 全文检索（含中文分词 —— 国际玩家全军覆没的差异点）
- **每 segment 内嵌 tantivy 倒排**（Rust、Lucene 式、mmap 友好、启动 <10ms）。term 组织成 row group + min/max term zone-map 剪枝，postings/positions 分块（与 SmithDB 同构，单机无对象存储读放大 → 应优于其 400ms）。
- **中文分词：tantivy-jieba 为主**（调研核查纠正：cang-jie 维护较慢，优先 tantivy-jieba；底层 jieba-rs 活跃）。jieba 搜索模式细粒度 + n-gram 兜底召回；短语检索靠 position。**自定义词典**（Agent/LLM 领域词、工具名、模型名）。可选挂团队 PG 系 `zhparser`(SCWS) 经验做词典增强。
- 建在 `inputs`/`outputs`/`name`/`error` 大字段子文件上，不污染核心小列。

### 3.3 JSON / 元数据过滤（任意嵌套字段）
- **主路线（高频快）**：flush 时扫 JSON，把高频路径（`metadata.model`/`metadata.user_id`/`ls_provider`）自动提升为独立物化列 + zone-map/字典编码 → 走列式 pruning。
- **补路线（低频全覆盖）**：全 JSON 路径展平成 `(json_path, value) → row_id` 倒排（迁移团队 PG `jsonb_path_ops` GIN 布局到不可变 segment，免增量更新/vacuum 复杂度）。字符串走字典/倒排，数值走 zone-map 支持范围过滤。

### 3.4 向量索引（SmithDB 没有 —— 招牌差异化，见 §10）
- 对 span 的 input/output（+决策上下文）embedding，**DiskANN 为主**（单机本地 NVMe、低内存、十亿级 ANN，正是「纯单机不强制对象存储」的最优环境；SmithDB 的对象存储+无状态架构跑图遍历延迟会爆炸，**对手的架构劣势 = 我们约束下的天然优势**）。HNSW 做热区小规模、IVF 做按分区裁剪。
- **向量段也走 LSM compaction**（复用团队页面整理框架，增量插入 + 段合并重建）。
- **过滤性 ANN**（「在 租户A、近7天、cost>X 的 trace 里找语义相似」）：由查询优化器在「先向量粗召回再标量精过滤」vs「IVF 分区裁剪后 ANN」之间按代价模型选择 —— 团队优化器能力是把混合检索做快的核心。

---

## 4. 活 Trace 与实时聚合

### 4.1 活 trace（运行中即查）
天然实现，**无需特殊机制**：运行中 trace 的 span 100% 在行式 MemTable，查询读 MemTable 的不可变快照即可看到「未完成 trace 当前状态」（含 `pending` 节点）。这就是 SmithDB「读 ingestion 本地缓存」的单机版，且因无网络/对象存储，延迟与点查同级（<100ms 目标）。查询规划器直接把未 flush 的 MemTable 段纳入扫描集合。

### 4.2 实时聚合（cost/latency/token usage）
- **DataFusion 向量化执行** 扫列式 segment 做 sum/avg/p50/p99/group-by，列存是数量级优势。
- **增量物化视图**（复用团队物化视图能力）：按 `(tenant, model, time_bucket)` 维度预聚合 token/cost/latency/error_rate，flush/compaction 时增量维护 → dashboard 级 <1s。
- 这套同时是**数据飞轮的「奖励信号物化视图」**底座（§10）：把人工反馈、LLM-judge 分、cost/latency 作为奖励信号实时维护，可被语义召回过滤、按奖励采样喂训练。

---

## 5. 单机内多租户隔离

三层隔离（单机的多租户本质 = 目录隔离 + 资源配额，比 SmithDB 的 slice 路由 + bucket 隔离更简单可控）：

1. **数据隔离（强制）**：`tenant_id` 作为所有 segment 的最高排序前缀 / **物理分目录**。查询强制带 `tenant_id`，存储层用目录/前缀直接裁剪 → 一个租户的查询永不扫另一租户文件（满足私有化合规）。**删租户 = 删目录**，极简。
2. **资源隔离**：per-tenant 写入配额、查询并发、MemTable 内存上限、compaction IO 配额独立计量（令牌桶 + per-tenant 调度器）→ 防单租户高频写打满 IO 饿死其他租户。
3. **加密/合规**：per-tenant 静态加密密钥落盘加密，满足金融/政企私有化。
4. **强隔离档位**：大客户可「一租户一进程 + 共享磁盘格式」部署，兼顾易用与隔离强度。

---

## 6. 崩溃恢复与一致性

**WAL + 不可变 segment「双真相源」模型（团队 PG 经验直迁）**：

- **WAL = 唯一可变状态真相源**：所有写（新 span、晚到 update、deletion）先顺序写 WAL（组提交摊薄 fsync），落盘即 ack → 低延迟 + 持久。
- **MemTable = WAL 的内存物化**：崩溃后重放 WAL 重建 MemTable 恢复到崩溃前一刻。
- **不可变 segment 自带持久性**：flush 原子落盘（写完 + fsync + 原子改 manifest）后截断对应 WAL（checkpoint）。不可变 → 永不半写损坏已有数据。
- **Manifest / Metastore = 内嵌 SQLite**（承担 SmithDB「小 Postgres」角色，单机更轻）：记录「有效 segment 集合 + 各 segment 的 deletion/upgrade vector + WAL checkpoint 位点」。**manifest 原子更新 = 整库一致性提交点**。崩溃恢复 = 读 manifest 定有效 segment + 重放 checkpoint 后 WAL。
- **一致性级别：单写者 + 多读者 MVCC**（DuckDB 同款）。读端持 segment+MemTable 不可变快照（原子指针切换发布、引用计数 GC），看一致视图；写端串行追加。trace「以追加为主、偶有更新」单写者完全够用，省掉复杂并发控制。
- **可见性**：flush/compaction 用原子指针切换，无读写锁、无长事务、P99 稳定。

---

## 7. 私有化打包与部署形态

**核心 = 嵌入式 Rust 内核（自带存储/WAL/事务）+ 单机服务外壳**：

- **内核像 DuckDB（完整库，自带存储+事务+WAL），查询执行层用 DataFusion，文件格式用 Vortex** —— 这正是 SmithDB 被生产验证的三件套组合。**自研热路径**（LSM 写入、segment 管理、compaction、树编码、向量召回算子、混合查询执行），**复用 DataFusion** 做向量化执行骨架（不重造执行引擎）。
- **为何不止于嵌入式库**：多租户、认证、网络访问、在线备份、监控是商业产品必需 → 外面包**单机服务进程**（SQL/HTTP/gRPC 端点）。
- **为何不照搬 SmithDB 无状态三服务**：那是分布式弹性设计，单机纯负担。单机 = **一个进程内三组线程池**（ingestion/query/compaction + 飞轮/embedding 池），简单、低延迟、一键起。
- **私有化一键部署**：单个静态链接二进制 + 本地 NVMe + **零外部依赖**（SQLite 内嵌、object_store crate 默认本地盘、客户自带 MinIO 时零改码）。对比痛点 —— Langfuse 自托管要 PG+ClickHouse 集群+S3+Redis 4-6 组件 + DBA + $3-4K/月；我们「一个包、本地盘、离线可装」是降维打击。**信创/国产 OS/CPU 适配**作为采购硬门槛兼护城河。
- **易用性落点**：对外**原生 SQL**（相对 SmithDB 私有 API 的差异化），内置 trace 专用函数/视图：`load_trace_tree(trace_id)` · `rebuild_thread(thread_id)` · `subtree(span_id)` · `semantic_recall(span_id, k, filter)` · `export_trajectory(...)`。

---

## 8. 摄入接口（OTel/SDK，兼容 LangSmith 生态）

「**换存储不换 SDK**」是降低迁移成本、过采购门槛的关键：

- **OTLP/gRPC + OTel GenAI 语义约定**（v1.41 Development 态，用 `OTEL_SEMCONV_STABILITY_OPT_IN` 管兼容）：吃掉 OpenLLMetry/Traceloop 采集生态（OpenLLMetry 是采集标准、非竞品，应兼容）。
- **OpenInference** 兼容（Arize/Phoenix 生态）。
- **LangSmith-compat REST**：兼容 LangSmith Run 写入格式（`inputs`/`outputs`/`dotted_order`/UUIDv7），让 LangChain/LangGraph 用户「指一下 endpoint 就迁过来」。
- **大 payload 抽离在 ingestion 路径完成**：检测 base64 data URI → 抽出上传本地盘/MinIO → 引用 token 替换（`@@@media:type=...|id=...@@@` 式，与存储后端解耦满足私有化）→ SHA256 去重 + MIME 白名单 + 大小上限（参考 20MB/文件）。防大 payload 污染列存核心行。
- **不在 ingest 时组装 trace 树**：原始 span 入表，查询时按 trace_id 拉全树组装（容忍父节点暂缺，乱序友好）。

---

## 9. 每个组件的 build-vs-开源 取舍

| 组件 | 取舍 | 理由 |
|---|---|---|
| 列式文件格式 | **开源 Vortex** | 0.36+ 格式向后兼容（风险可控）、随机读 100x、SmithDB 同款；Lance 为热备降级方案（同 Apache-2.0、同接 DataFusion，可平滑切换） |
| 查询执行引擎 | **开源 DataFusion** + 自研 trace 专用算子 | 纯 Rust 可嵌入、向量化、SmithDB 生产验证；自研算子=树遍历/向量召回/LSM merge |
| LSM 写入/segment/compaction | **自研**（复用团队磁盘索引/页面整理框架） | 热路径决定性能；trace 专用（晚物化/deletion vector/时间分层）开源件覆盖不了 |
| 树/区间编码 | **自研** | trace 专用，无现成件 |
| 全文倒排引擎 | **开源 tantivy** | Lucene 级、Rust、嵌入式、mmap 友好 |
| 中文分词 | **开源 tantivy-jieba** + 自研词典/zhparser 经验 | 国际玩家空白点；jieba-rs 活跃 |
| JSON 路径索引 | **自研**（迁移 PG GIN/jsonb_path_ops 布局到不可变 segment） | 团队 PG 经验直迁、免 vacuum |
| 向量索引 | **复用团队 DiskANN/HNSW/IVF** | 现成生产代码、单机十亿级、招牌差异化 |
| Metastore / manifest | **内嵌 SQLite**（小客户）/ 可选内嵌 PG（大客户） | 承担 SmithDB「小 Postgres」角色，单机更轻；团队 PG catalog 经验 |
| 存储后端抽象 | **开源 object_store crate** | 本地盘/MinIO/S3 一码通，私有化无争议采用 |
| WAL/事务/MVCC | **自研**（团队 PG 经验直迁） | 单写者多读者，简单可靠 |
| SQL parser/优化器 | **复用 DataFusion + 移植 openGauss 优化器思想** | 原生 SQL 是差异化卖点 |
| 数据飞轮原语 | **自研** | SmithDB 完全没有，最大差异化 |

许可证总览（全部商用友好、可闭源私有化）：Vortex/DataFusion/Arrow/object_store/RocksDB = Apache-2.0；tantivy/tantivy-jieba = MIT；SQLite = Public Domain；openGauss 复用部分 = Mulan PSL v2（宽松、无 copyleft，信创加分）。**无 GPL 传染、无 SSPL/BSL。**

---

## 10. 差异化护城河：语义召回 + 数据飞轮（SmithDB 完全没有）

> 经多方核对：SmithDB 公开材料**完全没有**向量检索/语义召回/embedding/few-shot/数据飞轮 —— 这是最大空档，且对手架构（对象存储+无状态）跑 ANN 图遍历会爆炸，**短期补不上**。

把产品从「事后看日志的可观测性存储」升级为「在线参与 agent 决策的检索底座」。五大原生原语：

1. **语义 trace 召回（招牌、飞轮轴承）**：`semantic_recall(span, k, filter)` —— 给定 trace/span 召回语义最相似历史 trace，与标量/JSON/时间/树过滤**同查询内融合**。服务 eval 基线对比、运行时 few-shot 注入（RAG over traces，直接进 agent 推理回路）、bad-case 纠错、最佳实践沉淀。分阶段：v1 离线/近线（批量 embedding + 段级索引），v2 在线低延迟。
2. **轨迹导出（飞轮出口）**：`export_trajectory(...)` —— 一键把 trace 树/线程/召回结果导出为训练标准格式（messages 数组、prompt-completion、DPO 偏好对、tool-call 轨迹）。树感知 + 多模态引用还原 + 增量/流式导出。
3. **奖励信号物化视图（飞轮度量）**：人工反馈 + LLM-judge 分 + cost/latency/token + 成功标签作为奖励信号，增量物化视图实时维护 → 训练时按奖励采样（RLHF/RFT 数据源）。
4. **SOP/few-shot 抽取（飞轮产物）**：对同类任务高奖励轨迹聚类，抽共性步骤模板 + 最佳 few-shot 集，供 agent 运行时直接拉取 → 闭环回在线推理。
5. **活 trace 推流**：运行中 trace 即可被召回/评估/订阅（SSE 推送子树增量），对标并超越 SmithDB 的「读 ingestion 缓存」。

embedding 来源/成本：可配置采样 + 旁路捕获客户已有 embedding API 调用 + 异步管线，控成本。

---

## 11. 粗略工时与上线节奏

团队基线：充足 DB 人才 + PG/openGauss 内核 + Rust + 向量索引（HNSW/IVF/DiskANN）+ 磁盘索引/页面整理框架 + 查询优化器。几乎零知识断层。

| 阶段 | 内容 | 工时 | 累计 |
|---|---|---|---|
| **M0 内核底座** | WAL + MemTable + Vortex flush + manifest(SQLite) + 时间分层 compaction + 单写者 MVCC | ~2.5 人月 | — |
| **M1 树 + 查询** | 邻接表/区间编码双编码 + DataFusion 接入 + trace 树/子树/线程查询 + 原生 SQL + trace 专用函数 | ~2 人月 | — |
| **M2 索引体系** | tantivy 倒排 + tantivy-jieba 中文 + JSON 物化列/路径倒排 + zone-map | ~2 人月 | — |
| **M3 摄入 + 活 trace** | OTLP/OpenInference/LangSmith-compat 接入 + 大 payload 抽离 + 活 trace 读路径 + 实时聚合物化视图 | ~1.5 人月 | — |
| **MVP 可售** | 上述 + 多租户(目录隔离+配额) + 一键部署二进制 + 备份 | — | **~9 人月（首个商用 MVP）** |
| **M4 向量召回(招牌)** | DiskANN/HNSW 接入 trace 流 + 过滤性 ANN + `semantic_recall` | ~2 人月（复用现成代码） | ~11 |
| **M5 数据飞轮** | 轨迹导出 + 奖励物化视图 + SOP 抽取 + 活 trace 推流 | ~2 人月 | ~13 |
| **M6 信创/加固** | 国产 OS/CPU 适配 + per-tenant 加密 + 一租户一进程档位 + 压测调优(对标 SmithDB P50) | ~2 人月 | **~15 人月（完整差异化版 GA）** |

并行建议：M2/M3 可与 M1 并行；MVP（~9 人月，2-3 人团队约 3-4 个月日历时间）先验证产品形态拿种子客户，向量+飞轮作为差异化大版本紧随。可用 Lance 做并行 MVP 提速路径（唯一打包好随机访问+向量+内置中文 FTS 的嵌入式格式），降低 Vortex 早期风险。

---

## 12. 主要风险

1. **Vortex 格式成熟度**（~1 年新格式，2025-08 进 LFAI 孵化）：高频乱序 span 下的写入/compaction 稳定性需 PoC 验证。**缓解**：0.36+ 格式向后兼容已确认；Lance 为现成降级方案（同 Apache-2.0、同接 DataFusion，平滑切换）。
2. **过滤性 ANN 工程难度**（带标量谓词的向量检索是公认难点）：**缓解**：团队优化器 + IVF 分区裁剪 + DiskANN filter 支持；分阶段（v1 近线、v2 在线）。
3. **embedding 成本/吞吐**：全量 embedding 成本高。**缓解**：采样 + 旁路捕获客户已有 embedding + 异步管线 + 按租户策略。
4. **单机写入吞吐天花板**（目标每秒数千~数万 span + 峰值脉冲）：纯单机无水平扩展兜底。**缓解**：WAL 组提交 + 无锁分片 MemTable + compaction IO 限速保 P99；明确单机定位（私有化场景负载远小于 SmithDB 全美流量）。
5. **OTel GenAI 仍 Development 态**（属性名可能变）：**缓解**：保留原文字段 + 归一层解耦 + 兼容开关。
6. **市场假设风险**（「国内尚无专用 DB」是否成立）：调研为否定命题无法完全证伪。**缓解**：销售前一手复核；差异化（中文+单机+信创+向量飞轮）即使有竞品仍成立。
7. **SmithDB 自托管追赶**（已成正式诉求）：但仍对象存储+无状态形态。**缓解**：单机极简 + 中文 + 信创是其架构与本地化短期够不着的中间地带。

---

## 自评（五维）

- **单机性能**：**强**。本地 NVMe + mmap + 单写者无锁 + Vortex 100x 随机读，砍掉 SmithDB 为对象存储付的全部读放大/网络延迟，公布基线（trace 树 92ms / 单 run 71ms / 全文 400ms）有望全面持平或更优；唯一不确定是高频乱序下 Vortex compaction 稳定性，需 PoC 坐实。
- **上市速度**：**中**。MVP ~9 人月（2-3 人约 3-4 个月日历），慢于「纯 openGauss 扩展」但远快于「纯自研」；自研热路径是必要投入，DataFusion/Vortex/tantivy/SQLite 四块开源件 + 团队现成向量/页面整理代码大幅压缩工程量。
- **私有化契合**：**极强**。单静态二进制 + 本地盘 + 零外部依赖 + 物理目录多租户 + per-tenant 加密 + 信创适配，正面打击 Langfuse 式 4-6 组件重运维痛点，是最锋利的楔子。
- **差异化护城河**：**极强**。原生中文分词（国际玩家全空白）+ 语义 trace 召回（SmithDB 没有且架构上短期补不上）+ 数据飞轮四原语 + 原生 SQL，五合一的复制成本对 SmithDB（补向量+中文）和 ClickHouse 派（补 trace 语义+树模型）都是 1-2 年工程纵深。
- **复用团队能力**：**极强，几乎零知识断层**。DiskANN/HNSW/IVF 直接成招牌、磁盘索引/页面整理框架直接管 segment 与向量段 compaction、PG 内核经验直迁 WAL/MVCC/JSON GIN/中文分词/metastore、查询优化器做混合检索代价模型、Rust 与 SmithDB 同代技术栈 —— 团队四块资产逐一精准命中 trace DB 四个最难工程点。