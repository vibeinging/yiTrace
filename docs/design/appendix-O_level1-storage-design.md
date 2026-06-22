# 附录 O:Level-1 存储引擎设计(三方案 + 首席综合)

## O1 首席综合(推荐方案A traceseg)

All load-bearing facts confirmed:

1. **Table-AM closure confirmed** — `NUM_TABLE_AM = 2` (a `const int`, compiled in), `TableAmType` enum only `{INVALID=-1, HEAP=0, USTORE=1}`. Proposal B's "塞进 TableAmType" cost is real and the enum is genuinely closed.

2. **RM auto-enumeration confirmed** — `RM_NEXT_ID` auto-enumerates, `RM_MAX_ID = RM_NEXT_ID - 1`. New RM = append one `PG_RMGR` line, no fixed slot ceiling. **But** `RmgrId = uint8` and the comment warns widening affects XLOG format — so there's a 256 ceiling, not unlimited, though plenty of headroom. All three proposals share this; the finding holds.

3. **CStore index restriction + fork aliasing** confirmed above.

I now have everything needed. I have firsthand-verified the four facts the recommendation hinges on (CStore-no-GIN, Table-AM-closed, RM-auto-enumerate, fork-aliasing). Here is the chief-architect synthesis.

---

# yiTrace Level 1 推荐设计（首席架构师裁决稿）

> 设计日期 2026-06-17。综合方案 A（Index-AM 自管段）/ B（fork Table-AM）/ C（复用 CStore）三稿，给出**推荐主路线 + 完整可落地设计 + 分阶段路线图**。
> 关键内核事实均**首席亲自读码二次核实**（本轮新核实四处，标注于下文）；凡静态读码推断未跑 PoC 者标【PoC待验】，自研无现成者标【自研】，不编造。

---

## 第一部分 · 裁决：选哪条路线，硬理由

### 0. 三句话裁决

> **主路线 = 方案 A（Index-AM + 自管列式段 `traceseg`，循 DiskANN `vector_smgr` 先例，不 fork Table-AM）。**
> **方案 C 的 CStore 复用降级为"Level 0.5 过渡底座"**（共存期冷数据先压上去，止血"压缩 XOR 可检索"，但不作为终态）。
> **方案 B（fork Table-AM + 新 orientation）列为"Level 2 演进期权"**，只有当 A 的 PoC 在向量化/HA 上被实测卡死、且业务规模到了非列式向量化扫描不可的临界点，才支付那 ~40 人月 + 最重信创回炉。

### 1. 为什么是 A 而不是 C：CStore 的"半成品"恰好缺在最贵的两块

C 的诱惑力在于"CStore 已替你做了一半"——不可变 CU、压缩、zone-map、delete-bitmap、delta-LSM 五件现成。这是真的，且很值钱。但**首席读码二次核实**确认 C 缺的两块恰恰是 trace 引擎的命门，且都补不回 Vortex 的形态：

**硬伤一:倒排永远只能旁路,不能内嵌（本轮亲验 `hw_cstore_index.source:8-16`）。**
列存表上 `btree/hash/gist/spgist/gin/unique/partial/expression` 索引**全部 unsupported，只剩 psort**。这意味着 SmithDB/Vortex 的核心价值——"倒排作为段内一列、与数据列共文件、共 I/O 调度、共 zone-map 剪枝、doc-id 即段内行号"——在 CStore 上**物理上做不到**。C 只能把倒排做成独立 fork 的旁路 index AM（TraceInv），靠 RowId 逻辑挂回。结果是：

- 多一跳 I/O（倒排片与 CU 不同文件，无法合并预取调度）;
- 倒排片与 CU 的生命周期/原子性要自己缝（flush CU 与 build 倒排片的崩溃边界，C 自己也标了【PoC待验】）;
- "压缩 XOR 可检索"虽被打破（冷 trace 既压着又能搜），但**是用"两个物理结构 + 逻辑挂钩"打破的，不是用"一份字节同时压缩+可检索"打破的**。这是治标，不是 Vortex 式根治。

**硬伤二:CU 行序不可重排,区间树子树扫退化（C 自陈 §4.2）。**
Vortex/方案 A 的不可变段红利之一是"封盖时按 `pre` 物理重排行 → 子树 = 连续行区间 = 连续 zone 顺序扫"。CStore 的 CU 行序由 delta flush 序锁死，重排=重写 CU=废掉 RowId=废掉旁路倒排。于是 C 的子树扫从"连续顺序扫"退化为"候选 CU 内 `pre BETWEEN` 列过滤"——比 Level 0 的 btree 回表好,但拿不到 A 的最优形态。

**为什么这两块最贵**：trace 产品的差异化正是"中文全文检索 + 树结构导航"。把这两块做成旁路/退化形态,等于把产品最值钱的两个能力建在 CStore 给不了的地基上。**C 用省下的 ~8-12 人月格式自研,换来的是把核心能力永久锁在次优形态。** 这笔买卖在"团队有内核能力、信创要自主可控、这是要长期建的产品组件"的前提下不划算。

**但 C 不是没用**——它的不可变 CU + delete-bitmap + 现成压缩,是**共存过渡期最快的止血方案**:Level 0 的冷分区今天就能 `ALTER ... SET orientation=column` 压上去,先解决膨胀和存储成本,再慢慢迁到 `traceseg`。所以 C 降级为"Level 0.5",见第四部分。

### 2. 为什么是 A 而不是 B：B 多拿的收益未被证明需要,代价却是最重的一档

B 的论证本身是诚实且正确的:真正"被优化器/向量化原生覆盖"靠的不是挤进 `TableAmType`（本轮亲验 `tupdesc.h:31` `NUM_TABLE_AM=2` 是编译期 `const int`,枚举只有 HEAP/USTORE,确系封闭),而是新增 `REL_TRACE_ORIENTED` orientation + 仿 CStore 发向量化 TraceScan。B 自己也算清了:90% 收益在轨 B（向量化扫描),轨 A（78 回调塞 TAM）对 append-only 不可变引擎大半语义不匹配。

问题在于**B 多拿的三样东西(向量化扫描 / 优化器原生 / HA 一等)目前都是"为未证明的瓶颈预付最重成本"**:

- **向量化扫描的收益,要在"大规模聚合/导出/训练扫描"负载下才兑现**。trace 的主负载是"按 trace_id/span_id 取单条树 + 中文全文检索 + 时间窗列表",这些是**点查/小范围 + 检索**,不是 OLAP 大扫描。向量化 batch 扫对前者收益有限。只有当出现"全租户跨月聚合导出训练集"这类负载且成为 SLA 瓶颈时,向量化才是质变。**这个瓶颈现在不存在,是推测的未来。**
- **HA 一等的收益**:A 的段文件走 `disk_container` + 标准 buffer-redo + `RM_DISKANN_ID` 复用,本身就跟随主备 WAL 复制(DiskANN 已是主备可复制的产品)。A 不是没有 HA,是"索引段的 HA",对 trace 这种"派生结构可重建"的数据,够用。B 的"表级一等 HA"是更强保证,但对可重建的 trace 段是过度保险。
- **代价是最重一档**:~35-42 人月内核工程 + 改动面横跨 `TableAmType`枚举/`g_tableam_routines`/`GetTableAMIndex`(每 tuple baked AM index)/orientation 枚举/`createplan`/`nodeTraceScan`/HA 恢复/`redo_xlogutils`——**且改内核二进制 = 送测客体变更 = 触发内核级重新测评**(信创回炉,年约 2 窗,错过丢半年)。B 自陈这是"最重一档"。

**裁决逻辑:B 是"正确的终态形态,但错误的起手时机"。** 在 A 的 PoC 还没证明向量化/HA 缺失会卡死目标 SLA 之前就上 B,是用最贵的认证代价和工程代价,去买一个推测的未来收益。**正确做法:A 先上、把段格式/merge-on-read/倒排/compaction 全部建成"orientation-ready"的形态(见 §B.10 的演进钩子),一旦 PoC 实测被向量化卡死,再把 A 的段引擎"升格"为 B 的 orientation——届时段格式不用重写,只补 planner/executor 接入。** A→B 是平滑升格,不是推倒重来。这是把 B 从"现在的豪赌"变成"将来的期权"。

### 3. 为什么 A 最稳:循的是这个内核里被验证过三遍的先例

A 的全部地基都不是新发明,是 DiskANN/BM25 在本仓**已经趟通并上线**的模式:

| A 依赖的能力 | 已上线先例 | 本轮/前稿核实 |
|---|---|---|
| 自管段文件 + 独立 buffer 池 + 后台刷盘线程 | DiskANN `vector_smgr` + `vector_buffers` + `vec_writer` | `vector_smgr.h:84-101` |
| 新增 fork 号自管字节流 | DiskANN `VECTOR_FORKNUM=5` | 本轮亲验 `relfilenode.h:49`（且发现 5 已被 PCA 别名,见下) |
| 段内类型化容器(列/倒排/字典/zone-map/deletion 向量) | `disk_container` 模板库(DiskANN+BM25 共用) | `diskvector.hpp`/`blockmgr.hpp`/`freespace.hpp` |
| 分块 postings + skip + 分级 + vacuum/upgrade | BM25 `InvertedList` | `bm25_inverted_list.h:33-133` |
| deletion 向量 + 延迟物理回收 | BM25 `doc_store`/`InvertedList::vacuum` | `bm25_doc_store.h:58-70` |
| 中文分词 | jieba `TSTemplateJiebaId` / `bm25_tokenize` | `dict_jieba.cpp` |
| 扫描吐 ctid 接执行器 | DiskANN `diskann_scan` | `diskann_scan.cpp:116,134,216` |
| WAL 自动注册新 RM | `RM_NEXT_ID` 自增(本轮亲验 `rmgr.h:24-30`) | append 一行即可,但 `RmgrId=uint8` 有 256 上限(够用) |

**A 的内核改动面是"轻 fork":pg_am 加一行 + (可能)RM 加一行 + redo 派发一处,其余全是 include 现成模板库 + 照抄 handler。** 这是三套里对信创回炉最友好的——改动局限在新增 AM 的独立编译单元,不动 HEAP/USTORE/CStore 任何既有路径。

### 4. 本轮新核实修正三稿的两处

1. **fork 号已拥挤(修正 A 的"复用 VECTOR_FORKNUM 零改动"乐观面)**:本轮亲验 `relfilenode.h:49-51` 发现 `VECTOR_FORKNUM=5` **与 `PCA_FORKNUM=5` 已是别名**(同值 5),`PCD_FORKNUM=6`,`MAX_FORKNUM=5`。即 fork 命名空间已经在"复用同一物理 fork 号承载不同语义"。→ **traceseg 复用 `VECTOR_FORKNUM` 在物理文件层按 relation 隔离应当可行(不同 relation 不同 relfilenode,`_vec` 文件不冲突),但语义别名已经很挤;一旦要拆独立 `TRACESEG_FORKNUM=7`,要同步改 `MAX_FORKNUM` 并审视 PCA/PCD 别名是否受影响。** 这是 A 的首个 PoC 必验项。

2. **新 RM 有 256 硬上限(补全 B/C 的"无上限"表述)**:`RmgrId=uint8`,`rmgr.h:19` 明确"widening 影响 XLOG 文件格式"。当前 ~37 个 RM,头room 充足,但不是"无上限"——是 ≤256。对单个新增 RM 无影响,表述精确化。

---

## 第二部分 · 完整设计（推荐主路线 A:`traceseg`）

> 命名:trace 存储引擎 = **TraceSeg**,作为 index AM `USING traceseg` 挂在 trace 主表上;段格式 = **TraceSeg Segment(TSS)**。

### B.1 总体形状

```
应用/网关层  ──SQL不变──>  openGauss 执行器
                              │
              ┌───────────────┴───────────────┐
              │  trace 主表(普通 ASTORE 堆表)   │   ← 只存极小占位行(trace_id + ctid锚)
              │  USING traceseg index(挂主表)   │   ← 宽数据全在 AM 段里
              └───────────────┬───────────────┘
                              │ amgettuple: 段内 merge-on-read 折叠 → 吐 ctid
        ┌─────────────────────┼─────────────────────────────┐
        │  TraceSeg AM 自管存储(MAIN_FORKNUM + VECTOR_FORKNUM)│
        │  ┌─ L0 memtable(内存有序) ──flush──> L1 小段        │
        │  ├─ L1/L2+ TSS 不可变段: 列区+倒排+树列+向量+删除向量│
        │  ├─ metastore: 段清单 + upgrade map + deletion 增量 │
        │  └─ 时间分层 compaction(后台线程+IO限速)            │
        └────────────────────────────────────────────────────┘
                              │
              旁挂全局 DiskANN(向量主路径,复用现成)
              大字段 CAS(本地NVMe/MinIO + SHA256去重,晚物化)
```

一个 TSS 段 = 一批事件的不可变快照,自包含:列区(轻量编码+zone-map)+ 内嵌中文倒排 + 区间树三列 + 可选段内 flat 向量 + deletion 向量。**段不可变是三处根治的总开关。**

### B.2 段格式与磁盘布局（TSS）

**两条 fork 分层(照 DiskANN):**

- **`MAIN_FORKNUM`(页式 + 标准 WAL)**:SegMeta、列区、内嵌倒排、区间树列、zone-map、deletion 向量、FreeSpace 页链。全用 `disk_container`(`DiskVector`/`VarDiskVector`/`DiskHashTable`/`FreeSpace`,fork-aware,include 即用,零内核改动)。
- **`VECTOR_FORKNUM`(自管字节流 + 独立 buffer 池)**:大字段 payload 指针解析后的本体、可选段内 flat 向量。走 `vec_read`/`vec_write` 按偏移随机读,复用 `vector_buffers`。
  > 【PoC待验·首验项】复用 `VECTOR_FORKNUM`(已与 PCA 别名)与同库 DiskANN 索引是否冲突;不行则拆 `TRACESEG_FORKNUM=7`(改 `MAX_FORKNUM` + `forkNames[]`,并审 PCA/PCD 别名)。

**段内逻辑布局(low→high,footer 在尾):**

```
[ColChunk 列区]  每逻辑列 → 一串 zone(8192行/zone)
                 zone头: encoding_id|bitwidth|min|max|count|null_count
[InvIndex 倒排区] per-text-col: 字典(一期DiskHashTable/二期FST) + 分块postings
[TreeBlock 树区]  pre/post/lvl 三列(delta+bitpack),段内按pre物理重排
[VecBlock 向量区] 可选: 段内采样span的flat向量 + zone-map(VECTOR_FORKNUM)
[Deletion 向量]   per-seg bitmap(DiskVector<uint64>)
[RowMap]         seg-local rownum ↔ span_id; doc-id=段内行号
[FreeSpace]      空闲页链(compaction回收用)
┌─ SegFooter(尾部定长,最后写) ─────────────────────────┐
│ magic/version/seg_id/row_count                        │
│ 时间边界[min_ts,max_ts] / trace_id边界(排序键)         │
│ 列目录/zone目录/倒排目录/树区指针/向量区指针            │
│ deletion偏移 / 段级统计 / CRC                          │
└───────────────────────────────────────────────────────┘
```

**核心三同粒度:压缩单元 = 随机读单元 = 剪枝单元 = zone(8192行)。** 取第 N 行第 C 列 = `zone=N/8192; off=N%8192` → O(1) 定位 zone(复用 `DiskVector::navigate_blkno_offset` 的几何页组+位运算,`diskvector.hpp:512-528`)→ 只解一个 zone(几 KB)。**这是"压缩 XOR 随机读"二选一的物理破解点**——不是 Parquet 读 footer 多往返,不是 CStore 整 CU 解压。

**可插拔轻量编码(每 zone 独立选):**

| 编码 | trace 列 | 状态 |
|---|---|---|
| delta+bitpack | ts/start/end/event_id(雪花单调)/pre/post | 【自研】团队页面整理能力覆盖 |
| RLE | status/event_type/span_kind/tenant_id | 【自研】 |
| dict | model/name 枚举(过滤在码上比较) | 【自研】 |
| plain | 兜底/高熵列 | 直接 `DiskVector<T>` |
| FSST | 短字符串 name/dotted_order | 【自研/移植·二期】本仓无现成,一期 plain 兜底 |
| ALP | total_cost/latency_ms 浮点 | 【自研/移植·二期】本仓无现成,一期 plain 兜底 |

编码器做成 `encode(zone)->bytes` / `decode_at(bytes,off)->value` 函数表,按 `encoding_id` 分派;变长结果用 `VarDiskVector`。**一期四种(自研三件套+plain)就跑通主路径,FSST/ALP 二期补,不阻塞。**

**zone-map 剪枝 + 大字段晚物化:** 段级 `[min_ts,max_ts]` 跳整段 → zone min/max/count 跳 zone → 才解码。大 payload(input/output 全文/媒体)只存 `payload_ref`(VECTOR_FORKNUM 偏移或 CAS 的 SHA256 指针),**只在显式 project 时取**;list/filter/聚合/树加载只动小核心列。

### B.3 merge-on-read 读路径（根治膨胀 + 根治 fold 的核心）

**数据模型:** 同一 `span_id` 的多事件(start/update/end/tool/retry/晚到 feedback)落进不同段,append-only,永不原地改。

- **deletion vector**(每段 bitmap):标记本段第 i 行已被更高版本取代/删除,挂 SegFooter,不重写段。
- **upgrade vector**(段间,metastore):`span_id → 最新版本(seg_id,rownum)`;旧段对应行在其 deletion vector 置位。

**折叠算法(amgettuple 内部):**

```
1. 段裁剪:  各段SegFooter [min_ts,max_ts]/[min_trace_id,max_trace_id] + zone-map(谓词) → 选相关段集S
2. 候选收集: 对S每段, 经段内RowMap/span_id索引取命中行, 过滤deletion已置位行
3. 多路归并: 段内有序 + 段间k路归并(k=段数), 归并键(span_id, seq, event_id雪花全局单调)  → O(n log k)
4. 折叠语义: 与Level 0逐字节一致——标量后写覆盖/token,cost累加/attrs深合并/end补全/status推断
5. upgrade校正: span最新版本若在更老已compaction段, 经upgrade map直接定位补取
6. 投影+晚物化: 只解码投影列; 大字段此刻才按payload_ref取CAS
```

**这把 fold 从 Level 0 的"应用层 SQL MERGE / 大 trace 退化 DFS",下沉为段读路径的 O(n log k) 归并算子。** 输入已被段级时间边界 + zone-map + span_id 索引 + deletion 向量四重剪枝压到极小。

**正确性三件套:**
- 乱序:归并键含雪花 `event_id`(全局单调),物理乱序落不同段不影响折叠结果。
- 晚到(冻结后才到的 feedback/eval):**= 普通写一个新段**。其 span_id 命中旧段,下次读自然纳入候选,upgrade 指新段、旧行 deletion 置位。**零特殊路径**——免掉 Level 0 的 `late_event_inbox + 重融化重写冷分区`。
- 删除/TTL:deletion 置位(廉价逻辑标记),物理回收延到 compaction。

**同构证据(团队已写过):** BM25 `doc_store`(`doc_id↔tid`+`erase`,`bm25_doc_store.h:58-66`)、`InvertedList::vacuum(doc_id_track&)`(`bm25_inverted_list.h:127`)、`InvertedListPageData::DELETE_FLAG`(line 54)。TraceSeg 把它从 BM25 内推广为段级通用机制。

### B.4 内嵌倒排（中文 jieba + 字典 + 分块 postings，根治"压缩 XOR 可检索" + 加速搜索）

倒排作为段内"几列",与数据列同段、同生命周期、同 zone-map 剪枝。**这是 A 相对 C 的决定性优势——C 只能旁路,A 真内嵌。**

1. **中文分词(复用现成,差异化点)**:段构建时对 input/output/name 调既有 jieba(`dict_jieba.cpp` 的 `TSTemplateJiebaId`,`bm25_tokenize`)。**SmithDB 无中文分词,我方现成。** 领域词典走 `vexjieba_add_userdict`/`reload`。
2. **term 字典(一期 DiskHashTable / 二期 FST)**:term→postings 块号。一期复用 BM25 `DiskHashTable`(按词长分桶,现成 79KB);二期换 FST 拿压缩比(参照 SmithDB term_key 88.8MiB→3.8KiB)+ 前缀扫描。**不可变段恰好让 FST 可行**(FST 要构建期一次性排序,不支持增量插入,正契合封盖段)。【自研·FST 本仓无现成,一期 DiskHashTable 不阻塞】。
3. **分块 postings(直接复用 BM25 InvertedList)**:多级 skip(`InvertedListSkipPointers`)、按长度分级(`il_threshold_levels={4,32,162}`)、`try_upgrade/downgrade` 级间迁移。**团队已写过。**
4. **doc-id = 段内行号(省翻译表)**:postings 直接存段内物理行号。term 查中 → 行号集 → O(1) 回列区取任意列 → 喂 §B.3 折叠。**全文命中→取列→折叠当前态,段内闭环,零跨结构翻译。** 对比 BM25 需 `doc_id↔tid` 翻译表(因建在可变堆表上),TraceSeg 段不可变+自描述,这层翻译被消掉——这是"内嵌"相对"旁挂"的实质收益,也是 A 相对 C 的实质收益。
5. **字节预算 row group(防 term 倾斜)**:每段倒排片 postings ≤32MB、term-string ≤64MB(照 SmithDB,按字节非 term 数),单个高频词不撑爆段。

### B.5 区间树（[pre,post] 物化进段，加速搜索）

段封盖时对段内 span 做一次性 O(n) DFS,产出每行 `(pre,post,lvl)` 作为三列入段,delta+bitpack(DFS 序近单调,压缩好)。

**不可变段独有红利:段内按 `pre` 物理重排行**(段不可变→封盖时可自由重排;这是 A 有、C 没有的)。于是子树扫:

```
1. span_id定位根行 → 读(pre_root, post_root)
2. 子树 = 所有 pre ∈ [pre_root, post_root] 的行
3. 段内按pre排序 → zone-map的pre min/max二分定位起止zone → 连续顺序扫
4. 扫出即DFS先序(=树展示序),无需再排序
5. 每行过deletion向量,喂§B.3折叠
```

比 Level 0 `(tenant,trace,pre) BETWEEN` 二级索引省:段内连续行 + zone-map,**顺序 I/O,不走索引随机回表**。

**跨段/晚到正确性:** 段内区间编码只在段内自洽,是"trace 完整落单段、已封盖"的快路径;跨段/未稳定时以 §B.3 折叠出的逻辑当前态为准,按 `parent_span_id`(写侧邻接永远正确)内存重算 pre/post 展示,`dotted_order` 全序兜底。【PoC待验:跨段树一致性规则 + 段内按 pre 重排成本 vs 子树扫收益,用真实乱序 trace 验证】。

### B.6 向量（复用 DiskANN 旁挂主路径 + 段内 flat 兜底）

**主路径复用全局 DiskANN,段内只做 flat 兜底。** 理由:① DiskANN 是成熟存量产品,自带自管段+独立 buffer 池+PQ+带过滤 ANN;② HNSW/Vamana 图要全局连通性,每不可变小段一张小图会割裂图、毁召回;③ 采样降规模(只对 root/LLM/error span 建 embedding)让单一全局索引够用。

| 层 | 做法 |
|---|---|
| 主向量索引 | 全局一个 DiskANN(`USING diskann(embedding,tenant_id,span_kind)` inplace-filter),随 trace 增量 insert |
| 段内 flat 兜底 | 段可选存采样 span 原始向量+zone-map:高选择度过滤后候选<阈值时段内暴力精排(recall=100%);段自包含可独立迁移 |
| merge 衔接 | DiskANN 命中 span_id → upgrade vector 指最新段行号 → §B.3 折叠取当前态 |

### B.7 LSM 写路径 + 时间分层 compaction（根治膨胀的写侧）

```
L0  memtable(内存,按span_id,seq,ts有序) ──满阈值/定时flush──> L1不可变小段(带zone-map+倒排+树)
L1  近期小段: 高频乱序晚到落这里(append-only),不compaction(还在等end/feedback)
L2+ 时间分层compaction: 时间稳定的老段合并成大段; 合并时应用deletion(真删)、upgrade(只留最新)、TTL
```

**写放大四杠杆:**
1. **时间分层(非大小分层)**:近期段不压实(还会收 end/feedback,过早压成大文件=反复重写=写放大);只压时间稳定的老段。trace"写一次、短期补几次、之后永久只读"的特性让时间分层天然低写放大。
2. **memtable 批量封段**:攒批排序一次性封压缩段,写放大=1 次顺序写,无 in-place(对比 Level 0 ASTORE 折叠 UPDATE 产死元组要 vacuum)。
3. **zero-copy compaction(二期)**:合并时对未被 deletion 命中、编码相同的 zone 直接搬压缩字节不解码;只有含被删行的 zone 才解开重写。【PoC待验:zone 编码兼容判定】。
4. **WAL 复用既有**:走 `LogManager`(`diskann_extend_newpages`/`diskann_xlog_add_elem`),不自造。

**回收闭环 = deletion/upgrade 向量的物理兑现点**:读路径只逻辑标记(廉价),compaction 才真删真合并真回收——与 BM25 `InvertedList::vacuum` 走 `doc_id_track` 完全同构。**单机 P99 隔离**:compaction = 独立后台线程池 + IO 令牌桶限速(照 `vec_writer_main` 后台刷写模式),与前台解耦。

### B.8 openGauss 落地机制（轻 fork，对应 DiskANN/BM25 先例）

| 改动点 | 性质 | 先例 |
|---|---|---|
| `pg_am.h` 加 `traceseg` AM 行 + `#define traceseg_AM_OID`（OID 待查未占用值） | 改 catalog 头(轻 fork) | bm25=4429 / diskann=4471 同形 |
| 16 个 handler 独立 `access/tracevault/` 编译单元 | 新增编译单元,不动既有路径 | 照 `bm25/`、`diskann/` |
| 段文件创建/读写/截断 | 复用 `vector_smgr`(`create_vec_data`/`vec_read`/`vec_write`/`truncate_vector_file`) | DiskANN 现成 |
| 独立缓存 + 后台刷盘 | 复用 `vector_buffers` + `vec_writer` | DiskANN 现成 |
| 主 fork 容器 | 复用 `disk_container`(全 fork-aware) | DiskANN+BM25 现成,零内核改动 |
| WAL RM | **起步复用 `RM_DISKANN_ID` redo 框架**(traceseg 与 diskann 同用 disk_container LogManager,redo 同形) | 避免新增 RM 槽 + 派发表改动 |
| 扫描吐 ctid + costestimate | 照 `diskann_scan.cpp:116,134,216` | DiskANN 现成 |

**执行器接缝:** 不发明新扫描节点,复用 PG 标准 IndexScan/amgettuple 契约。`tracesgettuple` 内部完成多段读+deletion/upgrade 合并+query-time fold,对外吐通过过滤的 ctid(`so->tids[i]→xs_ctup.t_self`)。谓词经 ScanKey 传入(`scan->keyData`),AM 内用 zone-map 剪枝。多条件 bitmap-AND **在单 AM 内合并多谓词**(trace 查询天然多条件,`amgetbitmap` BM25/DiskANN 都没实现,不强求)。ORDER BY/LIMIT 用 progressive time-window(沿时间倒走、有界窗、尽早停)。

> 【PoC待验】"不回堆表、AM 直接产列值"(真列式投影/晚物化)需 CustomScan,openGauss 对列式晚物化支持度未确认。**起步先 amgettuple+回堆表(宽列放 AM 段,堆表只留占位行+ctid),二期评估 CustomScan / 升格 B 的 orientation。**

### B.9 WAL / 崩溃恢复（分而治之，照 DiskANN）

- **策略1·页式数据走标准 buffer-redo**(段内倒排/字典/zone-map/树/deletion 向量,即 MAIN_FORKNUM 部分):全套是 `disk_container` 内置 `LogManager`(`diskann_xlog_add_elem`/`diskann_extend_newpages`/`diskann_update_meta`)。**起步复用 `RM_DISKANN_ID`**,redo 路径同形,crash-safe、可主备复制、可极限RTO并行恢复。
- **策略2·独立 fork 大块走物理字节 redo**(大字段 payload/flat 向量,VECTOR_FORKNUM 部分):字节整段记 WAL,redo 时 `vec_write` 重放到偏移。代价 WAL 量=数据量,但**不可变段只在 compaction/flush 写一次,之后纯读**——天然契合,WAL 压力远小于 ASTORE 原地 UPDATE。compaction 整段成型用批量 FPI/`log_newpage` 整页镜像,不逐行记。
- **memtable 崩溃恢复**:memtable 用轻量 redo 保护(或复用主 WAL append 记录),崩溃后从"最后封盖段 + 重放未 flush 的 memtable WAL"恢复。【自研·DiskANN/BM25 无内存 LSM L0 先例,恢复点一致性需 PoC】。

> **关键风险与对策**:若用**新** RM 而非复用 `RM_DISKANN_ID`,必须同时填 `rmgrlist.h`(PG_RMGR 宏,本轮亲验自增、`RmgrId=uint8` ≤256 上限够用)+ `redo_xlogutils.cpp` 二级派发表,否则主备/极限RTO `default: PANIC unknown rmid`。**起步建议复用 `RM_DISKANN_ID` 把内核固定表改动降到接近零**(待 PoC 坐实 LogManager redo 是否完全 relation-agnostic)。

### B.10 演进钩子（为将来升格 B 预埋，让 A→B 平滑不重写）

A 现在就把这些做成"orientation-ready",一旦 PoC 实测被向量化/HA 卡死,升格 B 只补 planner/executor 接入,**段格式不重写**:

- 折叠算法(§B.3)写成"输入候选行集 → 输出折叠行"的纯函数,起步逐行调(amgettuple),将来直接改成吃 `VectorBatch` 列批输出列批(B 的向量化 TraceScan)。
- 段内解码器(§B.2 `decode_at`)预留"批量解 zone → 列向量"接口,起步逐值调,将来批量供向量化算子。
- 折叠语义抽成**一份规格 + 一致性测试集**(同一组事件流断言结果相等),既是 Level 0↔1 迁移门禁,也是将来 A 逐行折叠 vs B 向量化折叠的等价性门禁。

---

## 第三部分 · 逐条根治三处短板（不回避）

### 短板①·膨胀（折叠/冻结原地 UPDATE 在 ASTORE 产死元组、要 vacuum）

**根治链条(从写模型层面拆掉膨胀来源,而非靠 vacuum 追赶):**
1. **段不可变 + merge-on-read 替代原地 UPDATE**(§B.3):折叠/冻结/更新/删除全部不改段——变更 = append 新事件 + deletion/upgrade 向量标记。**不产死元组、不触发 vacuum。** 长 span 的"早上 start 下午 end"更新不再引发写风暴。这是膨胀来源的**结构性拆除**。
2. **memtable 批量封段**(§B.7):攒批一次顺序写,写放大=1,无 in-place。
3. **时间分层 compaction 批量回收**(§B.7):死版本不靠 vacuum 逐元组清,而是 compaction 批量重写时一次性丢弃(应用 deletion/upgrade)+ zero-copy 搬未删 zone,回收可控、可限速、与前台解耦。
4. **大字段晚物化**(§B.2):MB 级无上界 payload 不进核心列段,隔离到 VECTOR_FORKNUM/CAS。
5. **FreeSpace 页链复用**:compaction 释放页进空闲链复用,不留空洞。

→ **vacuum 从关键路径消失**;膨胀来源(原地 UPDATE 产死元组)被 append + 向量标记 + 批量 compaction 三者根除。

### 短板②·冷数据"压缩 XOR 可检索"二选一（CStore 压了不能建 GIN/BM25/向量）

**根治:一个段同时压缩 + 随机访问 + 内嵌检索,三者同字节共存。这正是 A 相对 C 的决定性优势——C 只能旁路,A 真内嵌。**
1. **列式可插拔编码 + zone O(1) 随机读同粒度**(§B.2):压缩单元=随机读单元=zone,只解一个 zone。压缩与随机读在同一份字节共存——CStore/Parquet 给不了。
2. **内嵌倒排进压缩段**(§B.4):term 字典(FST/DiskHashTable)+ 分块 delta postings 作为段内列,与压缩数据列同段。老 trace 既被编码压着、又能字典/FST 精确/前缀查 + delta-postings 取行号 + 随机访问回原行。**CStore "压了就不能建 GIN/BM25" 的死结被解开,且是 Vortex 式同字节根治,不是 C 的旁路逻辑挂钩。**
3. **doc-id=段内行号**(§B.4):免翻译表,全文命中→取列→折叠当前态段内闭环。
4. **zone-map 剪枝**(§B.2):段级 + zone min/max 先剪后解,老 trace 检索 sub-second。
5. **段内 flat 向量**(§B.6):老 trace 向量精排段内可做,段自包含。

→ 老 trace 同一份压缩字节上**同时**可全文检索(中文 jieba 倒排)、谓词过滤(zone-map)、随机取值(列式 O(1))、向量精排——**不再需要"为可搜而额外留一份不压缩行存"**。

### 短板③·query-time fold 开销（活 trace 读时应用层折叠，活 trace 多/事件量大时慢）

**根治:把折叠从应用层下推到读引擎归并算子 + 多重剪枝缩小 merge 规模。**
1. **折叠下沉为段读路径多路归并算子**(§B.3):fold 从应用层 SQL MERGE/大 trace 退化 DFS,移到 `tracesgettuple` 内部 O(n log k) 多路归并,不走应用层全表聚合。
2. **多重剪枝缩小 merge 规模**:段级 [min_ts,max_ts] 跳整段 + zone-map 跳 zone + span_id 索引直定位 + deletion 向量过滤,让真正 merge 的数据量极小。
3. **progressive time-window**(§B.8):查"最新 N 条"沿时间倒走、有界窗、尽早停,而非全排序再 limit。
4. **活 trace 直读热缓冲**:活段在 memtable/L0 小段,走独立 buffer 池热缓存。
5. **晚到=普通写**(§B.3):免 Level 0 的"重融化重写冷分区"特殊路径,折叠永远是同一条归并算子。

→ 折叠不再是应用层线性开销;活 trace 多/事件量大时因多重剪枝 + O(n log k) 归并,**不再线性变慢**。
> 演进:若 PoC 实测此处仍是瓶颈,升格 B 的向量化折叠算子(§B.10 已预埋钩子),折叠在列批上做,直接喂上层向量化 HashAgg/Sort。

---

## 第四部分 · 与 Level 0 的关系（先上 Level 0，平滑替换底层不改对外 SQL）

**核心承诺:对外 SQL / 扩展接口不变,只替换底层存储。** Level 0/0.5/1 三层共存,灰度迁移,调用方无感。

### 三层定位

| 层 | 形态 | 角色 | 状态 |
|---|---|---|---|
| **Level 0** | 标准 openGauss ASTORE 事件表 + 应用层折叠 | 热活 trace 摄入落点 + 直读 | 先上,已有 |
| **Level 0.5** | Level 0 冷分区 `SET orientation=column`(CStore) | 过渡止血:冷数据压缩,降存储成本 | 可选,今天就能上 |
| **Level 1** | `traceseg` 不可变段引擎 | 冷冻结 trace 终态:压缩+可检索+不膨胀 | 本设计目标 |

> **Level 0.5 是方案 C 的归宿**:把 C 的 CStore 复用价值发挥在"过渡期最快止血",而非终态。冷分区压上 CStore 立即省存储,可搜性暂由 Level 0 的 GIN(在搬走前)或 traceseg(搬走后)提供。这让"压缩 XOR 可检索"在迁移窗口内不至于二选一。

### 平滑替换机制

1. **对外 SQL 不变**:`SELECT ... FROM trace WHERE tenant=? AND start_ts>=? AND status=?` 这类语句在三层上**完全一致**。Level 1 期 trace 主表仍是普通堆表(只存占位行+ctid 锚),`traceseg` 作为其上的索引;优化器选 `traceseg` index scan,`tracesgettuple` 吐 ctid,执行器回堆表取占位行,宽数据由 AM 段提供。**SQL 文本、扩展函数签名、网关 API 零改动。**
2. **双写共存期**:新事件 ① 写 Level 0 事件表(热查/活 trace);② 写 `traceseg` memtable(攒批封段)。读路由按时间:近期走 Level 0,老 trace 走 traceseg 冷段,merge-on-read 统一返回当前态。
3. **后台迁移**:把 Level 0 已冻结分区批量灌进 traceseg(一次性封大段,走 compaction 路径),灌完弃旧分区。**复用 §B.7 compaction 框架做搬迁器。**
4. **折叠语义一致性(迁移命门)**:Level 0 折叠在应用层/SQL,Level 1 折叠在 AM 归并算子。两者必须产出**逐字节相同**的当前态。**抽一份折叠规格 + 双实现一致性测试集**(同一事件流喂两边,断言相等),作迁移门禁(也是 §B.10 升格 B 的门禁)。这是最隐蔽的工程债,需长期回归。
5. **回退阀**:traceseg 段不可变+自描述,迁移失败保留 Level 0 分区,零数据丢失;drop traceseg 即回纯 Level 0。
6. **schema 对接**:traceseg 列 = Level 0 折叠后 span 的列;倒排建 input/output/name;树列来自 parent_span_id/dotted_order;向量复用 Level 0 采样策略;雪花 event_id 直接复用为归并键。

---

## 第五部分 · 分阶段落地（先做哪块、工程量、PoC 验证点）

> 假设 2-3 名有内核经验工程师。人月为经验估算,未做 WBS 排期。

### Phase 0 · PoC 决策门（2-3 人月）——先用最小代价验掉最致命的不确定项

**目标:不写完整引擎,只验"A 这条路能不能走通"的四个生死问题。**

| PoC 项 | 验什么 | 决定 |
|---|---|---|
| **P0-1 fork 复用** | 同库 DiskANN 索引 + traceseg 索引共用 `VECTOR_FORKNUM`(已与 PCA 别名)是否冲突 | 不行 → 拆 `TRACESEG_FORKNUM=7`(改 MAX_FORKNUM/forkNames,审 PCA/PCD 别名) |
| **P0-2 LogManager redo 是否 relation-agnostic** | 读 `diskann_redo` 实现,确认复用 `RM_DISKANN_ID` 能否承载 traceseg 段的 redo | 行 → 内核固定表改动接近零;不行 → 新增 RM(确认 uint8≤256 有空位)+ 填 `redo_xlogutils` 派发 |
| **P0-3 disk_container 跑通最小段** | 用 `DiskVector`/`DiskHashTable` 在新 AM 里建一个含 1 列 + 1 倒排 + deletion 向量的最小段,amgettuple 吐 ctid 接通执行器 | 跑通 → 地基坐实;不通 → 重估 |
| **P0-4 折叠语义等价** | Level 0 折叠 vs 归并算子折叠,同事件流断言逐字节相等(含晚到/乱序/深合并) | 建立一致性测试集,作后续门禁 |

**Phase 0 是 go/no-go 门:四项全绿才进 Phase 1。** 这把 A 的全部【PoC待验】风险前置,避免在错误地基上盖楼。

### Phase 1 · MVP 主路径（约 13-15 人月，含 Phase 0）

| 模块 | 复用度 | 人月 |
|---|---|---|
| AM 注册 + 16 handler 骨架(照 diskann/bm25) | 高(套模板) | 1.0 |
| 段格式 + 列区(DiskVector/VarDiskVector + zone 切分) | 高(复用容器) | 1.5 |
| 可插拔编码框架 + delta/RLE/dict 三件套 + plain | 中(自研,团队有页面整理能力) | 2.0 |
| 内嵌倒排(jieba+DiskHashTable+InvertedList 全复用,doc-id=行号) | 高 | 1.5 |
| merge-on-read 折叠归并算子 + deletion/upgrade 向量 | 中(语义复用 BM25 vacuum) | 2.5 |
| 区间树物化 + 子树区间扫 | 中 | 1.5 |
| LSM 写路径(memtable+flush) + 时间分层 compaction | 中 | 2.5 |
| 段文件自管(复用 vector_smgr) + 独立缓存接线 | 高(复用) | 1.0 |
| WAL/恢复接线(复用 LogManager/RM_DISKANN redo) | 高(复用) | 1.0 |
| 执行器接缝(amgettuple 吐 ctid + costestimate,照 diskann_scan) | 高 | 1.0 |
| 向量段内 flat 兜底(复用 DiskANN 主路径) | 高 | 0.5 |
| Level 0 双写/路由/迁移器 + 折叠一致性门禁 | 中 | 1.5 |

**MVP 交付:** 冷 trace 可压缩存储 + 中文全文检索 + 子树导航 + merge-on-read 折叠 + 时间分层 compaction,对外 SQL 不变。**这一期就正面根治了三处短板的主体。**

### Phase 2 · 性能强化（约 6-8 人月，按实测瓶颈选做）

- FSST/ALP 编码(字符串/浮点压缩比)【自研/移植】
- FST term 字典替 DiskHashTable(压缩比 + 前缀扫描)【自研】
- zero-copy compaction(zone 编码兼容直搬字节)
- progressive time-window 优化 + bitmap 多谓词合并

### Phase 3 · 期权:升格 B（仅当 Phase 1/2 实测被向量化/HA 卡死才启动，约 +15-20 人月 + 信创回炉）

- 新增 `REL_TRACE_ORIENTED` orientation + 仿 CStore 发向量化 TraceScan(`createplan` + `nodeTraceScan`)
- 折叠算子升格为吃 `VectorBatch` 列批(§B.10 钩子已预埋,段格式不重写)
- (可选)接 `TableAmType` 最小回调子集拿表级 HA 一等
- **触发条件明确化**:出现"全租户跨月聚合/导出/训练扫描"且成为 SLA 瓶颈,或 A 的回堆表投影被实测证明拖垮大扫描——**没有这个实测信号就不启动 Phase 3**,不为推测的未来预付最重认证成本。

---

## 裁决一句话总结

**推荐方案 A 为主路线**:循 DiskANN `vector_smgr` + BM25 `InvertedList` + `disk_container` 三个本仓已上线先例,做 Index-AM + 自管不可变列式段 `traceseg`,**轻 fork(pg_am 一行 + 复用 RM_DISKANN redo,内核固定表改动接近零)、对信创回炉最友好**;三处短板被结构性根治——膨胀靠不可变段+merge-on-read+时间分层 compaction 拆掉原地 UPDATE,"压缩 XOR 可检索"靠 zone 同粒度压缩+随机读+**真内嵌**中文倒排共存(A 相对 C 的决定性优势:C 只能旁路、CStore 禁 GIN 已亲验),fold 靠下沉为 O(n log k) 归并算子+多重剪枝。**方案 C 降级为 Level 0.5 过渡止血底座**(冷分区压 CStore 最快省存储),**方案 B 列为 Level 2 期权**(段格式预埋 orientation-ready 钩子,A→B 平滑升格不重写,仅当实测被向量化/HA 卡死才支付那 ~40 人月+最重信创回炉)。落地以 Phase 0 四项 PoC 决策门前置全部生死风险,MVP 约 13-15 人月正面根治三处短板,对外 SQL/扩展接口全程不变。

**本轮首席新核实(修正三稿):** ① `hw_cstore_index.source:8-16` 亲验 CStore 禁 btree/gin/gist/hash/spgist 仅留 psort——坐实 C 倒排只能旁路;② `tupdesc.h:31` 亲验 `NUM_TABLE_AM=2` 编译期 const、枚举仅 HEAP/USTORE——坐实 B 的 Table-AM 封闭代价;③ `rmgr.h:24-30` 亲验 RM 自增但 `RmgrId=uint8` 有 256 硬上限(非"无上限",当前~37 个,头room充足);④ `relfilenode.h:49-51` 亲验 `VECTOR_FORKNUM=5` 已与 `PCA_FORKNUM=5` 别名、`PCD=6`、`MAX_FORKNUM=5`——fork 命名空间已拥挤,A 复用 VECTOR_FORKNUM 列为 Phase 0 首验项。所有结论基于静态读码,未编译未跑 PoC,生死项已收敛进 Phase 0 决策门。
---

## O2 方案A Index-AM自管段

# 方案 A: Index-AM + 自管列式段(循 DiskANN vector_smgr 先例,不 fork 内核)

I now have complete, file-level grounding for every claim. I have everything needed to produce the design. Here is the complete Level 1 design.

---

# yiTrace Level 1 设计：方案 A — Index-AM + 自管列式段（`traceseg`）

> 路线：把 trace 存储做成**一个自定义 index AM（`USING traceseg`）+ 自管 smgr 段文件**，完全循 DiskANN 的 `vector_smgr` / `disk_container` / BM25 `InvertedList` 三套现成先例，**绕开封闭 Table-AM**。
> 已用真实代码核实的事实在每节标注（文件:行）。**自研/未验证项明确标注**，不编造。
> 全文末尾有专节《三处短板怎么根治》逐条交代膨胀 / 冷数据压缩可检索 / fold 开销。

---

## 0. 总体形状与一句话定位

`traceseg` = 挂在 trace 主表上的一个 index AM。trace 行宽数据**不进堆表**（堆表只留极小占位行 + ctid，见 §10），全部进 AM 自管的**列式不可变段（TraceSeg）**。一个段 = 一批事件的不可变快照，内部含：列区（轻量编码 + zone-map）、内嵌中文倒排、区间编码树、可选段内 flat 向量、deletion/upgrade 向量。写走 LSM（memtable → flush 成段 → 时间分层 compaction）。读走 merge-on-read：按 `span_id` 折叠多版本事件，对外只吐 ctid（起步阶段，§9）。

三套落地载体（全部已存在于本仓）：

| 需求 | 落地件 | 证据（文件:行） |
|---|---|---|
| 段文件自管 + 独立缓存 | `vector_smgr` 模式 + `VECTOR_FORKNUM` | `vector_smgr.h:84-101`，`relfilenode.h:49,57`，`catalog.cpp:90-96` |
| 段内类型化容器（倒排/字典/zone-map/向量列/deletion 向量/freespace） | `disk_container::{DiskVector,VarDiskVector,DiskHashTable,FreeSpace,BlockMgr}` | `diskvector.hpp:147-198,366-409,512-528`；`blockmgr.hpp:96-124`；`freespace.hpp` 全文 |
| 分块 postings + skip + vacuum + upgrade/downgrade | BM25 `InvertedList` | `bm25_inverted_list.h:33-89,127-133`；`bm25_doc_store.h:58-70` |
| 扫描吐 ctid 接执行器 | DiskANN scan | `diskann_scan.cpp:18,116,134,150` |
| WAL/恢复 | `LogManager` + `RM_DISKANN_ID`/`RM_BM25_ID` | `diskvector.hpp:179,244,373`；`rmgrlist.h:85,88` |

---

## 1. 段格式与磁盘布局（TraceSeg）

### 1.1 存储分层（两条 fork，照 DiskANN 抄）

DiskANN 已示范"原始向量在 `VECTOR_FORKNUM`（无页头、字节流、独立 buffer 池），图/元数据在 `MAIN_FORKNUM`（标准 PageHeader + `disk_container`）"。`traceseg` 同款分层：

- **`MAIN_FORKNUM`（页式 + 标准 WAL）**：SegMeta、列区（用 `DiskVector`/`VarDiskVector`）、内嵌倒排（`InvertedList` + 字典）、区间树三列、zone-map、deletion/upgrade 向量、FreeSpace 页链。**这一层零内核改动**——`disk_container` 已是 fork-aware（`get_disk_vector(rel, is_wal, fork_num)`，`diskvector.hpp:168`），include 即用。
- **`VECTOR_FORKNUM`（自管字节流 + 独立缓存）**：大字段 payload（input/output 全文、大 JSON、媒体）以及（可选）段内 flat 向量本体。走 `vec_read`/`vec_write`（`vector_smgr.h:97-101`）按字节偏移随机读，不受 8KB 页边界约束。复用独立 buffer 池 `vector_buffers`（`vector_smgr.h:84-89`）。

> 诚实标注：`VECTOR_FORKNUM`(=5) 当前是 DiskANN 专用的单一 fork。`traceseg` 复用它需确认同库内 DiskANN 索引与 traceseg 索引不冲突（同一 relfilenode 的 `_vec` 文件按 relation 隔离，不同 relation 物理文件不同，**应当无冲突**，但需 PoC 坐实）。若要彻底干净，新增一个 `TRACESEG_FORKNUM` 是"轻 fork"（改 `relfilenode.h:49,57` 两行 + `catalog.cpp:96` 一行 forkNames，编译期）。**起步建议先复用 `VECTOR_FORKNUM`，零内核改动验证可行性，再决定是否拆 fork。**

### 1.2 段内逻辑布局

一个 TraceSeg 段 = 主 fork 上一段连续 block（用 `BlockMgr::reserve_new_pages` 几何增长分配，`blockmgr.hpp:96`）。布局：

```
SegMeta（一页，自定义 opaque + 页体）
  magic / version / seg_id / 行数 nrows
  时间边界 [min_ts, max_ts]            ← 段级粗剪枝（"最近7天"直接跳整段）
  span_id 边界 / tenant 集合摘要
  列目录:    col_id -> {首 zone 的 DiskVectorMeta blkno, 行类型, 编码族}
  zone 目录: 每列每 zone -> {encoding_id, min, max, count, null_count, 数据块号}
  倒排目录:  text_col -> {term 字典 meta blkno, postings meta blkno}
  树区指针:  pre/post/lvl 三列的 DiskVectorMeta blkno
  向量区指针:（可选）段内 flat 向量 VECTOR_FORKNUM 偏移 + zone-map
  deletion_vec blkno / upgrade 信息（见 §2）
  大字段映射: payload_ref 列（指向 VECTOR_FORKNUM 偏移或外部 CAS）
列区  每逻辑列 = 一个 DiskVector / VarDiskVector，按 8192 行/zone 切分，每 zone 独立编码
倒排区 每文本列 = 字典(term->postings块号) + 分块 postings(InvertedList)
树区  pre / post / lvl 三列(随段物化，delta+bitpack)
向量区（可选）段内采样 span 的 flat 向量 + zone-map（VECTOR_FORKNUM）
deletion 向量  per-seg bitmap（DiskVector<uint64> 位图）
FreeSpace    空闲页链（compaction 回收用，freespace.hpp）
```

**段不可变**：封盖后只读。这是三处根治的总开关——无原地 UPDATE、无死元组、无 vacuum。

### 1.3 列与 zone（压缩单元 = 随机读单元 = 剪枝单元，三者同粒度）

每列按 8192 行切 zone。zone 头存 `encoding_id`；段元里存该 zone 的 `{min,max,count,null_count}`。

随机取第 N 行第 C 列：`zone = N/8192; off = N%8192` → O(1) 定位 zone → 解一个 zone（几 KB）即得。**不是 Parquet 那种读 footer + row-group 多次往返**。这是"压缩与随机读不二选一"的物理根因。

复用：`DiskVector::navigate_blkno_offset`（`diskvector.hpp:512-528`）已实现"行号→(几何页组,块,页内偏移)"的 O(1) 定位（几何页组 + `__builtin_clzl` 位运算）。`traceseg` 在其上叠一层"行号→zone→zone 内偏移"。

### 1.4 可插拔轻量编码（每 zone 独立选）

| 编码 | trace 列 | 状态 |
|---|---|---|
| **delta+bitpack** | `ts`/`start_ts`/`end_ts`/`event_id`(雪花单调)/`pre`/`post` | **自研**（团队页面整理能力覆盖） |
| **RLE** | `status`/`event_type`/`span_kind`/`tenant_id` | **自研** |
| **dict** | `model`/`name` 枚举 | **自研**（过滤可在码上比较，不解字符串） |
| **FSST** | 短字符串 `name`/`dotted_order` | **自研/移植**（本仓无现成实现，二期；一期 plain 兜底） |
| **ALP** | `total_cost`/`latency_ms`/数值 attrs | **自研/移植**（本仓无现成；二期；一期 plain 兜底） |
| **plain** | 兜底/高熵列 | 直接用 `DiskVector<T>` |

> 诚实标注：FSST/ALP **本仓无现成实现**（grep 确认，是 Vortex/SmithDB 算法），列为自研/移植，**不可声称复用**。工程兑现：编码器做成 `encode(zone)->bytes` / `decode_at(bytes,off)->value` 函数表，按 `encoding_id` 分派；变长结果用 `VarDiskVector<T>`（`diskvector.hpp:549`）落盘。一期先上 plain/delta/RLE/dict 四种（自研三件套 + plain），FSST/ALP 二期补，**不阻塞主路径**。

### 1.5 zone-map 剪枝 + 大字段晚物化

- **zone-map 段剪枝**：查询带谓词（`tenant_id=? AND start_ts>=? AND total_cost>?`），先用段级 `[min_ts,max_ts]` 跳整段，再用 zone min/max/count 跳 zone，最后才解码命中 zone。
- **大字段晚物化**：核心列（id/parent/start/end/status/cost/latency/token/tags）入 `MAIN_FORKNUM` 列区，小而密、压缩好；大 payload（input/output 全文、大 JSON、媒体）只存 `payload_ref`（VECTOR_FORKNUM 偏移或外部 CAS 指针 + SHA256），**只在查询显式 project 时才取**。list/filter/聚合/树加载只动小核心列，不被大 payload 毒化。

---

## 2. merge-on-read 读路径算法（按 span_id 折叠多版本事件）

这是【根治膨胀】与【根治 fold 开销】的核心。span 的折叠/冻结/更新/删除**全部不改段**：逻辑当前态 = 多段 + deletion/upgrade 向量在读路径上合并出来。

### 2.1 数据模型

- 写：同一 `span_id` 的多条事件（start / update / end / tool_call / retry / 晚到 feedback）落进**不同段**，append-only。
- **deletion vector**（每段一个 bitmap，`DiskVector<uint64>` 位图）：标记"本段第 i 行已被逻辑删除/被更高版本取代"。挂 SegMeta，不重写段。
- **upgrade vector**（段间）：metastore 记 `span_id → 最新版本 (seg_id, rownum)`；旧段对应行在其 deletion vector 置位。metastore 落 Level 0 的小元数据表或内嵌 catalog 即可。

### 2.2 读路径折叠算法

查一个 trace / 一个 span 的当前态：

```
1. 时间裁剪：用各段 SegMeta 的 [min_ts,max_ts] + zone-map 选出相关段集 S（跳无关段）。
2. 候选行收集：对 S 中每段，经段内 span_id 索引取出该 span_id 的所有行号，
   过滤掉 deletion vector 已置位的行。
3. 多路归并：把候选行按归并键 (seq, ts, event_id) 排序（雪花全局单调保序），
   段内有序 + 段间 k 路归并 = O(n log k)，k=段数。
4. 折叠语义（与 Level 0 完全一致）：后写覆盖标量、token/cost 累加、attrs 深合并、
   end 补全、status 推断。
5. 投影 + 晚物化：只对最终投影列解码；大字段此刻才按 payload_ref 去取（§1.5）。
```

**关键**：折叠从 Level 0 的"应用层 SQL MERGE / 大 trace 退化 DFS"**下沉为段读路径里的归并算子**，是 O(n log k) 的多路归并，不是应用层全表聚合。这正面消除任务点③。

### 2.3 正确性：乱序 / 晚到 / 删除

- **乱序**：归并键 `(seq, ts, event_id)`，`event_id` 用应用端雪花（全局单调）。即使物理乱序落进不同段，归并排序后语义确定。
- **晚到（冻结后才到的 feedback/eval）**：**晚到 = 普通写一个新段**（append-only 永远成立）。其 `span_id` 命中已存在旧段；下次读时该 span 候选行自然含新段行，归并即得正确当前态。upgrade vector 指到新段，旧段对应行 deletion 置位。**零特殊路径**——这是事件模型相对 Level 0 物化表的结构性优势（Level 0 要 `late_event_inbox + 重融化重写冷分区`）。
- **删除/TTL**：合规删 = deletion vector 置位（廉价逻辑标记）；物理回收延到 compaction（§6）。

### 2.4 同构证据（不是空想）

BM25 已在做同形操作：
- `bm25_doc_store` 维护 `doc_id ↔ (ItemPointerData tid, Oid part_id)` 并有 `erase`（`bm25_doc_store.h:58-66`）——段-local id ↔ 行位置翻译 + 逻辑删除。
- `bm25_doc_store::vacuum(callback, ...)` 返回 `doc_id_track`（`bm25_doc_store.h:70`）——即被删 doc 集合（= deletion vector 物化输入）。
- `InvertedList::vacuum(doc_id_track&, ...)`（`bm25_inverted_list.h:127`）按被删 doc 集在 compaction 时物理清理 postings。
- `InvertedListPageData::DELETE_FLAG`（`bm25_inverted_list.h:54`）= 页内条目删除标记。

→ deletion 向量 + 延迟物理回收，**团队已写过、内核已有**。`traceseg` 是把它从 BM25 内推广为段级通用机制。

---

## 3. 内嵌倒排（中文 jieba + term 字典 + 分块 postings）

把倒排作为段内"几列"，与数据列同段、同生命周期、同 zone-map 剪枝。

### 3.1 三件套

1. **中文分词 = 复用既有 jieba**【加速搜索，差异化点】
   段构建时对 `input_text`/`output_text`/`name` 调既有 jieba。本仓 `dict_jieba.cpp` 通过 TS 模板 `TSTemplateJiebaId` 暴露 `Jieba` 对象做 cut/lexize；`bm25_tokenize` 已内置。**这是 SmithDB 的公开空白（无中文分词），我方现成**。领域词典走 `vexjieba_add_userdict`/`vexjieba_reload`。

2. **term 字典：一期 `DiskHashTable`，二期 FST**【加速搜索 + 根治膨胀】
   term → postings 块号。一期复用 BM25 现成 `DiskHashTable`（按词长分桶，`bm25_token_index`，`disk_hashtable.hpp` 79KB 现成）。二期换 **FST**（有限状态转换器）拿压缩比（参照 SmithDB term_key 88.8MiB→3.8KiB）+ 前缀扫描。
   > 诚实标注：FST 本仓**无现成实现**，是新增组件。但 `traceseg` 段不可变 → FST 要求的"term 一次性排序构建"在封盖段上顺水推舟（FST 不支持增量插入，正契合不可变段）。**一期 `DiskHashTable` 顶上，不阻塞。**

3. **分块 postings（复用 BM25 InvertedList）**【加速搜索 + 根治膨胀】
   每 term 的 postings 按块存，块内 doc-id delta + 变长 bitwidth。**直接复用** `InvertedList`：已有多级 skip pointer（`InvertedListSkipPointers`，`bm25_inverted_list.h:33-39`）、按 postings 长度分级（`il_threshold_levels={4,32,162}`，line 41）、`try_upgrade/try_downgrade` 级间迁移（line 128-131）。分块 + skip + 分级，**团队已写过**。

### 3.2 doc-id 直接 = 段内行号（省翻译表）

postings 里的 doc-id **直接是段内物理行号**。于是：term 查中 → postings 给行号集 → 行号 O(1) 回列区取任意列（§1.3）→ 直接喂 §2 折叠。全文命中→取列→折叠当前态，**段内闭环，零跨结构翻译**。倒排区与列区共享同一 zone-map：先 zone min/max/count 剪枝，再走字典/FST。

> 对比 BM25 现状：BM25 `doc_store` 需 `doc_id↔tid` 翻译表（`bm25_doc_store.h:58`），因它建在可变堆表上。`traceseg` 段不可变 + 自描述，行号即 doc-id，**这层翻译被消掉**——这是"内嵌"相对"旁挂索引"的实质收益。

---

## 4. 区间编码树（[pre,post] 物化进段）

子树 = 区间范围扫，命中 zone-map。

### 4.1 物化进段

段封盖时对段内 span 做一次性 O(n) DFS，产出每行 `(pre, post, lvl)`，作为三列入段，delta+bitpack 编码（DFS 序近单调，压缩比高）。

**关键红利（不可变段独有）**：段内**按 `pre` 物理重排行**（段不可变 → 封盖时可自由重排）。于是子树扫 = 段内**连续行区间** `[pre_root, post_root]` = 连续 zone 扫，命中 zone-map 的 pre min/max 直接定位起止 zone。

### 4.2 子树范围扫算法

```
1. 由 span_id 定位根行，读 (pre_root, post_root)。
2. 子树 = 所有 pre ∈ [pre_root, post_root] 的行（区间编码不变式）。
3. 因段内按 pre 排序：zone-map pre min/max 二分定位起止 zone → 连续扫。
4. 扫出即 DFS 先序（=树展示序），无需再排序。
5. 每行过 deletion vector，喂 §2 折叠。
```

比 Level 0 `(tenant,trace,pre) BETWEEN` 二级索引更省：段内连续行 + zone-map，**顺序 I/O，不走索引随机回表**。

### 4.3 跨段 / 晚到正确性

一个 trace 可能跨多段（晚到子节点落新段）。段内区间编码只在段内自洽。跨段时以"逻辑当前态"为准：先 §2 折叠出全部当前 span，再按 `parent_span_id`（写侧邻接，永远正确）内存重算 pre/post 展示。即段内物化是**快路径**（trace 完整落单段、已封盖时直接区间扫），跨段/未稳定退化到邻接重建（`dotted_order` 全序作兜底）。

> 诚实标注：跨段树一致性规则 + "段内按 pre 重排成本 vs 子树扫收益"需 PoC 用真实乱序 trace 验证。

---

## 5. 向量：复用 DiskANN（旁挂主路径）+ 段内 flat 兜底

**结论：主路径复用全局 DiskANN，段内只做 flat 兜底。**

理由：① DiskANN 是成熟存量产品，自带"自管段文件 + 独立 buffer 池"；② HNSW/Vamana 图要全局连通性，每不可变小段一张小图会割裂图、毁召回；③ 采样降规模（只对 root/LLM/error span 建 embedding）让向量集足够小，单一全局 DiskANN 即可。

| 层 | 做法 | 标注 |
|---|---|---|
| 主向量索引 | 全局一个 DiskANN（`USING diskann (embedding, tenant_id, span_kind)` inplace-filter），随 trace 增量 insert | 复用既有，零新内核 |
| 段内 flat 兜底 | 段可选存本段采样 span 原始向量 + zone-map（`VarDiskVector<float>` 或 VECTOR_FORKNUM）：高选择度过滤后候选<阈值时段内暴力精排(recall=100%)；段自包含可独立迁移 | 新增轻量 |
| merge-on-read 衔接 | DiskANN 命中 span_id → 经 upgrade vector 指最新段行号 → §2 折叠取当前态 | 复用 §2 |

→ **不嵌全图（避召回灾难），复用 DiskANN；段内仅 flat 做精排兜底与自包含。**

---

## 6. LSM 写路径与时间分层 compaction（写放大控制）

### 6.1 三级写路径

```
L0  memtable(内存)：新事件先进内存有序结构(按 span_id, seq, ts)。
     → 满阈值/定时 flush，封成 L1 不可变小段(带 zone-map+倒排+树)。
L1  近期小段：高频乱序晚到落这里(append-only)，不 compaction（还在等 end/feedback）。
L2+ 时间分层 compaction：老段(时间稳定、无新事件)合并成大段，
     合并时应用 deletion vector(真删行)、upgrade vector(只留最新版本)、TTL 过期。
```

### 6.2 写放大四杠杆

1. **时间分层（非大小分层）**：近期段不压实（还会再收 end/feedback，过早压成大文件=反复重写=写放大）；只对"时间稳定、不再变"的老段压实。trace "写一次、短期补几次、之后永久只读"的特性让时间分层天然低写放大。
2. **memtable 批量封段**：内存攒一批、排好序、一次性封成压缩段——写放大 = 1 次顺序写，无 in-place 更新（对比 Level 0 ASTORE 折叠 UPDATE 产死元组要 vacuum）。
3. **zero-copy compaction**（二期）：合并时对未被 deletion 命中、编码相同的 zone，直接搬压缩字节不解码再编码。只有被删行所在 zone 才解开重写。
   > 诚实标注：需 zone 编码兼容判定逻辑，二期优化。
4. **WAL 复用既有机制**：段写入走既有 `LogManager`（`diskvector.hpp:179,244` 的 `diskann_extend_newpages`/`diskann_xlog_add_elem`），不自造 WAL。

### 6.3 回收闭环

compaction = §2 的 deletion/upgrade 向量**物理兑现点**：读路径只逻辑标记（廉价），compaction 才真删、真合并、真回收空间——与 BM25 `InvertedList::vacuum` 走 `doc_id_track` 物理清理（`bm25_inverted_list.h:127`）完全同构。

**单机 P99 隔离**：compaction = 独立后台线程池 + IO 限速（令牌桶），照 `vec_writer_main`（`vector_smgr.h:92`）的后台刷写线程模式，与前台解耦。

---

## 7. 在 openGauss 上的落地机制（AM / smgr / fork，与 DiskANN 先例对应）

### 7.1 注册 AM（pg_am 一行 DATA，照 diskann/bm25 抄）

`pg_am.h` 加一行（与 `diskann` OID 4471、`bm25` OID 4429 同形，`pg_am.h:166,182`）：

```
DATA(insert OID = 4490 ( traceseg 0 0 f t f f t t f f f f f 0
   tracesinsert tracesbeginscan tracesgettuple - tracesrescan tracesendscan
   - - - tracesbuild tracesbuildempty tracesbulkdelete tracesvacuumcleanup
   - tracescostestimate tracesoptions));
#define traceseg_AM_OID 4490
```

16 个 handler 独立成 `access/tracevault/` 子目录编译单元（像 `bm25/`、`diskann/`）。

> 诚实标注：OID 4490 是占位，需查未占用 OID。openGauss 用 `.h` 里 `DATA()` 行（非 PG13+ `.dat`），注册 AM 本身就是改这个内核 catalog 头——属"轻 fork"。

### 7.2 自管 smgr 段文件（照 vector_smgr）

- 段文件创建/打开/截断：复用 `create_vec_data`/`vec_read`/`vec_write`/`truncate_vector_file`（`vector_smgr.h:96-101,94`），骑标准 md.c 1GB 段链。
- 独立缓存：复用 `vector_buffers` 独立 buffer 池 + `vec_writer` 后台刷写线程（`vector_smgr.h:84-92`）。`vec_invalidate_buffer_cache`（line 87-88）给段失效语义。
- 主 fork 容器：`disk_container::{DiskVector,VarDiskVector,DiskHashTable,FreeSpace}`，全部 fork-aware，几何扩页 + O(1) 定位 + 可选 WAL，**include 即用，零内核改动**。

### 7.3 与 DiskANN 先例的逐项对应

| `traceseg` 机制 | DiskANN/BM25 先例 | 证据 |
|---|---|---|
| 列区/树列容器 | `DiskVector`/`VarDiskVector` | `diskvector.hpp:547-549` |
| zone-map/deletion 向量 | `DiskVector<POD>` + `FreeSpace` | `diskvector.hpp:147`，`freespace.hpp` |
| 段内倒排 postings | BM25 `InvertedList` | `bm25_inverted_list.h:91-172` |
| term 字典 | BM25 `DiskHashTable` | `disk_hashtable.hpp` |
| 大字段/flat 向量字节流 | `vector_smgr` + VECTOR_FORKNUM | `vector_smgr.h:97-101` |
| 独立缓存 + 后台刷写 | `vector_buffers` + `vec_writer_main` | `vector_smgr.h:84-92` |
| 扫描吐 ctid | `diskann_scan` so->tids → xs_ctup.t_self | `diskann_scan.cpp:116,134` |
| cost estimate | `diskanncostestimate_internal` | `diskann_scan.cpp:216` |

---

## 8. WAL / 崩溃恢复（分而治之，照 DiskANN 抄）

DiskANN 同时示范两种 WAL，`traceseg` 分而治之：

**策略 1 — 页式数据走标准 buffer-redo**（段内倒排/字典/zone-map/树/deletion 向量，即 MAIN_FORKNUM 部分）
全套是 `disk_container` 内置的 `LogManager`：set/push_back 时 `diskann_xlog_add_elem`、扩页时 `diskann_extend_newpages`、改 meta 时 `diskann_update_meta_*`（`diskvector.hpp:244,179,373`）。redo 走 `RM_DISKANN_ID`/`RM_BM25_ID`（`rmgrlist.h:85,88`，已存在）。crash-safe、可主备复制、可极限 RTO 并行恢复。

**策略 2 — 独立 fork 大块走"物理字节 redo"**（大字段 payload / flat 向量，即 VECTOR_FORKNUM 部分）
照 DiskANN 对 VECTOR_FORKNUM 的处理：把字节整段记进 WAL 记录体，redo 时直接 `vec_write` 重放到指定偏移，绕开 page-redo。
- **代价/边界**：WAL 量 = 数据量（无 FPI 折叠），对大列块+高写入会放大 WAL。**对策**：`traceseg` 段不可变，WAL 只在 compaction/flush 写一次，之后纯读——天然契合不可变段，WAL 压力远小于 ASTORE 原地 UPDATE。compaction 整段成型时用批量 FPI / `log_newpage` 式整页镜像，不逐行记。

**关键风险（必须填两处，否则恢复 PANIC）**：若 `traceseg` 用**新** RM（而非复用 RM_DISKANN_ID），必须同时填 `rmgrlist.h`（PG_RMGR 宏）+ `redo_xlogutils.cpp` 的二级派发表，否则主备/极限RTO 恢复 `default: PANIC unknown rmid`。
> 诚实标注：`RM_MAX_ID = RM_NEXT_ID - 1`（`rmgr.h:30`），当前已用 37 个 RM 槽（grep `^PG_RMGR` = 37）。新增 RM 需确认 `RM_NEXT_ID` 还有空位。**起步建议复用现成 `RM_DISKANN_ID` 的 redo 框架**（traceseg 与 diskann 都用 disk_container 的 LogManager，redo 路径同形），避免新增 RM 槽位与派发表改动——这能把"轻 fork"降到只改 pg_am + 复用 VECTOR_FORKNUM，**理论上接近零新内核固定表改动**（待 PoC 坐实 LogManager 的 redo 是否完全 relation-agnostic）。

---

## 9. merge-on-read 扫描怎么挂进执行器（与现有 SQL/优化器的接缝）

**核心：不发明新扫描节点，复用 PG 标准 IndexScan/amgettuple 契约**（照 DiskANN）。

- `tracesgettuple` 内部完成：读多段 + 合并 deletion/upgrade 向量 + query-time fold（§2），对外只吐通过过滤的 ctid（`so->tids[i] → scan->xs_ctup.t_self`，照 `diskann_scan.cpp:116,134`）。merge 逻辑 = `diskann_search` 那段图搜索的位置，换成"多段归并 + 删除位图过滤 + 折叠"。
- `tracescostestimate` 照 `diskanncostestimate_internal`（`diskann_scan.cpp:216`）填，优化器据此选它。
- **接缝细节**：
  - **谓词下推**：trace 的 `tenant_id`/`start_ts`/`status` 谓词通过 ScanKey 传入（`scan->keyData`），AM 内部用 zone-map 剪枝（这是段内剪枝，与 Level 0 分区裁剪正交）。
  - **bitmap-scan**：BM25/DiskANN 的 pg_am 行 `amgetbitmap` 列是 `-`（未实现，`pg_am.h:182`）。多条件 bitmap-AND 要么自实现 `amgetbitmap`，要么在单 AM 内部做多列合并。建议**在单 AM 内合并多谓词**（trace 查询天然多条件）。
  - **ORDER BY / LIMIT**：trace 列表"最新 N 条"用 progressive time-window：沿时间倒走、对最新候选段建有界时间窗，read newest bounded slice → stream → merge → dedupe → 尽早停，而非全排序再 limit。

### 与 Level 0 的 SQL 接缝

起步阶段 trace 主表是普通堆表（只存极小占位列 + ctid），`traceseg` 作为其上的索引；`tracesgettuple` 吐 ctid，执行器回堆表取占位行，宽数据由 AM 段提供。SQL 层 `SELECT ... FROM trace WHERE ...` 不变，优化器选 `traceseg` index scan。

> 诚实标注：若要"不回堆表、AM 直接产列值"（真列式投影/晚物化），amgettuple 不够（IndexScan 必然回堆表），需 CustomScan Provider。**openGauss CustomScan 对列式晚物化的支持度本次未确认**。建议起步先 amgettuple+回堆表（宽列放堆表或 AM 段，AM 吐 ctid），二期再下沉到 CustomScan。

---

## 10. 与 Level 0（事件表 + 折叠）的迁移 / 共存

**双轨共存，灰度切换：**

1. **共存期**：Level 0 事件表（标准 openGauss 表）+ 应用层折叠继续服务热活 trace；`traceseg` 先接管**冷冻结段**（老 trace）。新事件双写：① Level 0 事件表（热查/活 trace）；② `traceseg` memtable（攒批封段）。
2. **读路由**：查询按时间路由——近期 trace 走 Level 0（或 traceseg L0/L1 小段）；老 trace 走 traceseg 冷段。merge-on-read 统一返回当前态，调用方无感。
3. **迁移**：后台任务把 Level 0 已冻结分区批量灌进 traceseg（一次性封成大段，走 compaction 路径），灌完弃旧分区。这正是把 Level 0 "CStore 压缩但不可检索 / 行存可检索但不压缩" 的二选一，替换成 traceseg "压缩+可检索同段共存"。
4. **回退**：traceseg 段不可变 + 自描述，迁移失败可保留 Level 0 分区，零数据丢失。
5. **schema 对接**：traceseg 列 = Level 0 折叠后 span 的列；倒排建在 input/output/name；树列来自 `parent_span_id`/`dotted_order`；向量复用 Level 0 §8 采样策略。

---

## 11. 工程量（人月）与风险

### 工程量（按模块，假设 2-3 名有内核经验工程师）

| 模块 | 复用度 | 人月 |
|---|---|---|
| AM 注册 + 16 handler 骨架（照 diskann/bm25） | 高（套模板） | 1.0 |
| 段格式 + 列区（DiskVector/VarDiskVector + zone 切分） | 高（复用容器） | 1.5 |
| 可插拔编码框架 + delta/RLE/dict 三件套 | 中（自研，团队有页面整理能力） | 2.0 |
| FSST/ALP 编码（二期） | 低（自研/移植） | 2.0 |
| 内嵌倒排（jieba 复用 + DiskHashTable 复用 + InvertedList 复用） | 高 | 1.5 |
| FST term 字典（二期，替 DiskHashTable） | 低（新组件） | 2.0 |
| merge-on-read 折叠归并算子 + deletion/upgrade 向量 | 中（语义复用 BM25 vacuum） | 2.5 |
| 区间树物化 + 子树区间扫 | 中 | 1.5 |
| LSM 写路径（memtable + flush） + 时间分层 compaction | 中 | 2.5 |
| 段文件自管（复用 vector_smgr） + 独立缓存接线 | 高（复用） | 1.0 |
| WAL/恢复接线（复用 LogManager/RM_DISKANN redo） | 高（复用） | 1.0 |
| 执行器接缝（amgettuple 吐 ctid + costestimate） | 高（照 diskann_scan） | 1.0 |
| Level 0 迁移/双写/路由 | 中 | 1.5 |
| 向量段内 flat 兜底（复用 DiskANN 主路径） | 高 | 0.5 |

**MVP（一期，不含 FSST/ALP/FST/zero-copy）**：约 **15-17 人月**。
**完整（含二期编码/FST/zero-copy compaction）**：约 **22-26 人月**。

### 风险（按严重度）

| 风险 | 严重度 | 缓解 |
|---|---|---|
| **VECTOR_FORKNUM 复用是否与 DiskANN 冲突** | 中 | PoC 验证同库 diskann+traceseg 共存；不行则拆 TRACESEG_FORKNUM（轻 fork 3 行） |
| **LogManager redo 是否 relation-agnostic（决定能否零 RM 改动）** | 中高 | PoC 读 `diskann_redo` 实现确认；不行则新增 RM + 填 redo_xlogutils 派发表（确认 RM_NEXT_ID 有空位） |
| **不回堆表的列式投影需 CustomScan，openGauss 支持度未知** | 中 | 起步 amgettuple+回堆表；二期评估 CustomScan |
| **跨段树一致性 + 段内按 pre 重排成本** | 中 | PoC 真实乱序 trace 验证；不划算则退邻接重建 |
| **FST/FSST/ALP 自研（本仓无现成）** | 中 | 一期 DiskHashTable+plain/delta/RLE/dict 兜底，不阻塞 |
| **高频乱序 span + 频繁 compaction 的写入/compaction 稳定性** | 中高 | PoC 压测；时间分层 + IO 限速隔离前台 P99 |
| **amgetbitmap 未实现，多条件 bitmap-AND** | 低 | 单 AM 内部合并多谓词 |
| **WAL 量 = 数据量（VECTOR_FORKNUM 物理字节 redo）** | 低 | 不可变段只在 compaction 写一次；批量 FPI |

---

## 12. 三处短板怎么根治（逐条交代，不回避）

### ① 膨胀（Level 0：折叠/冻结的原地 UPDATE 在 ASTORE 产死元组、要 vacuum，高写入下膨胀）

**根治手段（多管齐下，从存储模型层面拆掉膨胀来源）：**

1. **段不可变 + merge-on-read 替代原地 UPDATE**（§2）：折叠/冻结/更新/删除全部不改段——变更 = append 新事件 + deletion/upgrade 向量标记。**不产死元组、不触发 vacuum**。"早上出生下午死亡"的长 span 更新不再引发写风暴。这是膨胀来源的结构性拆除。
2. **memtable 批量封段**（§6.2）：内存攒批一次性封成压缩不可变段，写放大 = 1 次顺序写，无 in-place 更新。
3. **时间分层 compaction 批量回收**（§6）：死版本不靠 vacuum 逐元组清，而是 compaction 批量重写时一次性丢弃（应用 deletion/upgrade 向量）——回收成本可控、可限速、与前台解耦。zero-copy 搬字节（二期）进一步降 compaction 写放大。
4. **大字段晚物化**（§1.5）：MB 级无上界 payload 不进核心列段，膨胀压力隔离到 VECTOR_FORKNUM 字节层。
5. **FreeSpace 页链复用**（`freespace.hpp`）：compaction 释放的页进空闲链复用，不留空洞。

→ **vacuum 从关键路径消失**；膨胀来源（原地 UPDATE 产死元组）被 append + 向量标记 + 批量 compaction 三者根除。

### ② 冷数据"压缩 XOR 可检索"二选一（Level 0：CStore 压了不能建 GIN/BM25/向量；要可搜只能留不压缩行存）

**根治手段（一个段同时压缩 + 随机访问 + 内嵌检索，三者同字节共存）：**

1. **列式可插拔编码 + zone O(1) 随机读同粒度**（§1.3-1.4）：压缩单元=zone（独立编码、级联压缩→高压缩比），随机读单元=zone（O(1) 定位+只解一个 zone→不牺牲随机读）。压缩与随机读在同一份字节上共存——CStore 给不了。
2. **内嵌倒排进压缩段**（§3）：term 字典（一期 DiskHashTable / 二期 FST）+ 分块 delta postings 作为段内列，与压缩数据列同段。老 trace 既被编码压着、又能字典/FST 精确/前缀查 + delta-postings 取行号 + 随机访问回原行。**CStore "压了就不能建 GIN/BM25" 的死结被解开。**
3. **doc-id = 段内行号**（§3.2）：免翻译表，全文命中→取列→折叠当前态段内闭环。
4. **zone-map 剪枝**（§1.5）：段级 [min_ts,max_ts] + zone min/max/count 先剪后解，老 trace 检索 sub-second。
5. **段内 flat 向量**（§5）：老 trace 的向量精排在段内可做，段自包含。

→ 老 trace 同一份压缩字节上**同时**可被全文检索（中文 jieba 倒排）、谓词过滤（zone-map）、随机取值（列式 O(1)）、向量精排——压缩与可检索不再二选一。

### ③ query-time fold 开销（Level 0：活/未冻结 trace 读时在应用层折叠事件，活 trace 多/事件量大时慢）

**根治手段（把折叠从应用层下推到读引擎 + 多重剪枝缩小 merge 规模）：**

1. **折叠下沉为段读路径多路归并算子**（§2.2）：fold 从应用层 SQL MERGE / 大 trace 退化 DFS，移到 `tracesgettuple` 内部的 O(n log k) 多路归并（段内有序+段间 k 路归并），不再走应用层全表聚合。
2. **多重剪枝缩小 merge 规模**：段级 [min_ts,max_ts] 跳整段 + zone-map 跳 zone + span_id 索引直定位 + deletion 向量过滤，让真正要 merge 的数据量极小。
3. **progressive time-window**（§9）：查"最新 N 条"沿时间倒走、有界时间窗、尽早停，而非全排序再 limit。
4. **活 trace 直读热缓冲**：活段在 memtable/L0 小段，走独立 buffer 池热缓存（`vector_buffers`），不落冷路径。
5. **晚到 = 普通写**（§2.3）：免 Level 0 的"重融化重写冷分区"特殊路径，折叠永远是同一条归并算子。

→ 折叠不再是应用层线性开销；活 trace 多/事件量大时，因 zone-map + 排序键 + progressive window 大幅剪枝 + O(n log k) 归并，**不再线性变慢**。

---

## 关键文件路径（均绝对路径，供实现定位）

- 自管段文件先例：`/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/include/access/annvector/store/{vector_smgr.h,bulkbuf_smgr.h}`
- 页式容器库：`/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/include/templates/vtl/disk_container/{diskvector,blockmgr,diskarray,disk_hashtable,freespace}.hpp`
- fork 枚举：`/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/include/storage/smgr/relfilenode.h:49,57` + `/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/common/backend/catalog/catalog.cpp:90-96`
- AM 注册：`/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/include/catalog/pg_am.h:166(diskann 4471),182(bm25 4429)`
- 倒排/postings/deletion：`/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/include/access/bm25/{bm25_inverted_list.h,bm25_doc_store.h}` + `.../bm25/bm25_inverted_list.cpp`
- 中文分词：`/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/gausskernel/storage/access/bm25/tokenizer/dict_jieba.cpp`
- 扫描吐 ctid：`/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/gausskernel/storage/access/diskann/diskann_scan.cpp:18,116,134,150,216`
- WAL 资源管理器：`/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/include/access/rmgrlist.h:85(RM_DISKANN_ID),88(RM_BM25_ID)`；`/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/include/access/rmgr.h:30(RM_MAX_ID)`

---

## 诚实标注汇总（未亲验 / 推断 / 自研项）

- 以上为**静态源码核实**（已读 `diskvector.hpp`/`blockmgr.hpp`/`vector_smgr.h`/`bulkbuf_smgr.h`/`bm25_inverted_list.h`/`bm25_doc_store.h`/`diskann_scan.cpp`/`freespace.hpp` + grep 确认 pg_am/rmgrlist/forkNames/RM_MAX_ID/jieba），**未编译、未跑 PoC**。
- **自研/移植项（本仓无现成，不可声称复用）**：FSST、ALP 编码器；FST term 字典；merge-on-read 折叠归并算子；可插拔编码框架；zone-map 框架；zero-copy compaction 兼容判定；跨段树一致性规则。
- **关键待 PoC 坐实**：① VECTOR_FORKNUM 复用是否与 DiskANN 冲突（决定是否拆 TRACESEG_FORKNUM）；② `LogManager`/`diskann_redo` 是否 relation-agnostic（决定能否零 RM 改动、接近零内核固定表改动）；③ openGauss CustomScan 对列式晚物化支持度（决定能否不回堆表）；④ 段内按 pre 重排成本 vs 子树扫收益；⑤ 高频乱序 span + 频繁 compaction 稳定性。
- SmithDB/Vortex 性能宣称（100x 随机读、FST 88.8MiB→3.8KiB、zero-copy compaction）来自研究底稿引用的官方/二手来源，属对标参照，非本设计实测。
- pg_am OID 4490、新 RM 槽位均为占位，需查未占用值；起步建议复用 RM_DISKANN_ID redo 框架以最小化内核改动。
---

## O3 方案B fork Table-AM

# 方案 B: 内核 fork + 自定义 Table AM(第三种存储引擎)

I now have all the kernel-level evidence I need. Key findings confirmed:

1. **TableAmType** is a closed enum (`TAM_INVALID=-1, TAM_HEAP=0, TAM_USTORE=1`, `NUM_TABLE_AM=2`), with a 78-callback `TableAmRoutine` struct, fixed 2-element `g_tableam_routines[]`, and the AM index is baked into every tuple via `tupTableType >> 1` (`GetTableAMIndex`).
2. **Critical precedent**: `RelOrientation` enum (COL/ROW/PAX/TIMESERIES) is the *real extensible dispatch axis* — the planner branches on `rte->orientation == REL_COL_ORIENTED` to emit vectorized `CStoreScan`, entirely bypassing the TableAm enum. CStore proves a "third storage engine the optimizer + vectorized engine cover natively" already exists in this kernel without being a TAM.
3. `VECTOR_FORKNUM=5` fork + `vector_smgr` is the DiskANN self-managed-segment precedent.
4. `RM_NEXT_ID` auto-enumerates RM IDs (headroom exists; `RmgrTable[RM_MAX_ID+1]`).

I have enough to write the complete design. This is a design-document task — per my instructions I return the design directly as my final message (not a file). Here it is.

---

# yiTrace Level 1 设计（方案 B：内核 fork + 自定义 Table AM 第三种存储引擎）

> 基线：openGauss 5.x / PG 9.2.4 谱系内核 fork（`/Users/Four/JobProjects/yitrace/openGauss-vector-main`）。
> 本文走**最重路线**：在 `TAM_HEAP/TAM_USTORE` 之外引入**第三种一等表存储引擎 = trace LSM 列式段（TraceStore，AM 名暂定 `tracelsm`）**，让它被优化器/向量化执行/HA 原生覆盖。
> 全文每个机制标注【根治膨胀】/【加速搜索】/【根治 fold】。凡未在源码亲自验证或属推演的，标【推测】或【待 PoC】，不编造。

---

## 0. 一句话定位与一处必须先讲清的内核事实

方案 A（前几轮已定）把 TraceSeg 做成"**自定义存储 backed 的 Index-AM**"（沿 DiskANN `vector_smgr` + BM25 `disk_container`），绕开封闭 Table-AM——代价是它**藏在索引背后**，优化器只把它当一个索引、回堆表取数，拿不到向量化扫描/原生 HA/列裁剪下推。

方案 B 正面承认并支付那笔成本：**fork 内核，把 trace 段存储升级成第三种 Table AM**，让一张 `CREATE TABLE ... USING tracelsm` 的表对优化器、向量化执行器、主备 HA、WAL/恢复都是**一等公民**。

但落地前必须先纠正一个常见误解（已读源码坐实）：**在这版内核里，"被优化器+向量化原生覆盖"并不等于"挤进 `TableAmType` 枚举"。** 真正承担"第三种存储引擎被引擎层识别"的轴，是 **`RelOrientation` 枚举**：

```c
// src/include/nodes/parsenodes.h:43
typedef enum RelOrientation {
    REL_COL_ORIENTED,   /* CU/CStore 列存 */
    REL_ROW_ORIENTED,
    REL_PAX_ORIENTED,
    REL_TIMESERIES_ORIENTED
} RelOrientation;
```

CStore（列存）**根本不在 `TableAmType` 里**（那里只有 HEAP/USTORE 两个值），它靠 `rte->orientation == REL_COL_ORIENTED` 让 planner 在 `create_cstorescan_plan`（`createplan.cpp:2367`）发**向量化 `CStoreScan` 节点**，而不是 `SeqScan`。这就是本内核里"**第三种存储引擎，被优化器/向量化原生覆盖，且不属于那两个 TAM**"的现成范本。

所以方案 B 的正确形态是**双轨改造**，而不是天真地"枚举 +1"：

- **轨 A（重，但语义最完整）**：把 trace 段接进 `TableAmType`（`TAM_TRACELSM=2`）+ `g_tableam_routines[]` 第三项 + 78 个回调，拿到 DML/DDL/索引构建/HA 全套一等待遇。
- **轨 B（更聪明，复用 CStore 已铺好的向量化通路）**：同时新增 `REL_TRACE_ORIENTED`（第 5 个 orientation），让 planner/executor 像认 CStore 一样认 trace 表，发**专用向量化 TraceScan 节点**，把 merge-on-read 折叠、zone-map 剪枝、列裁剪直接做成向量化算子。

**判断：方案 B 真正值得做的收益，90% 来自轨 B（向量化扫描通路），而非轨 A（塞进 TAM 枚举）。** 轨 A 的 78 回调里大量是 ASTORE/USTORE 行存语义（`tops_form_cmprs_tuple`/`tcap_*` 时间胶囊/`tuple_lock_updated`），对一个**不可变段 + append-only + merge-on-read** 的引擎要么是空实现要么语义不匹配。下文 §8 给出"轨 A 最小可用子集 + 轨 B 主收益"的取舍。

---

## 1. 段格式与磁盘布局（TraceStore Segment Format）

### 1.1 物理载体：新增 `TRACE_FORKNUM`，照搬 DiskANN fork 先例

DiskANN 已经新增过一个 fork（已读源码）：

```c
// src/include/storage/smgr/relfilenode.h
#define VECTOR_FORKNUM 5
#define MAX_FORKNUM VECTOR_FORKNUM    // 上界顶到 5
// src/common/backend/catalog/catalog.cpp:90  forkNames[] 尾部加 "vec"
```

TraceStore 同法新增 `#define TRACE_FORKNUM 6`、`MAX_FORKNUM` 顶到 6、`forkNames[]` 加 `"trc"`。于是段文件物理名 `<relfilenode>_trc`，**复用 md.c 的 1GB 段链、relpath、unlink、smgr 创建、storage WAL**——这条路 DiskANN 已经趟通，不是新发明。

> 与 DiskANN 的差异：DiskANN 的 `_vec` fork 存"纯字节流向量、无页头"；TraceStore 的 `_trc` fork 存"自描述列式段（有段头 + zone 索引）"，介于"裸字节"和"标准 8KB 页"之间——段内用自己的块格式，但 mmap/缓存/刷盘走 `vector_smgr` 同款独立 buffer 池（见 §7）。

### 1.2 段 = 一个不可变自描述文件区

一个 **TraceSeg** = `_trc` fork 上一段连续区域，封盖后**只读不可变**。布局（low→high）：

```
┌─ SegFooter (尾部，最后写,定长) ─────────────────────────────┐
│  magic / version / seg_id / row_count                       │
│  时间边界 [min_event_ts, max_event_ts]                       │
│  trace_id 边界 [min_trace_id, max_trace_id] (排序键)         │
│  列目录偏移 col_dir_off  / 索引目录偏移 idx_dir_off          │
│  倒排目录偏移 inv_dir_off / 树区偏移 tree_off                │
│  deletion-vector 内嵌副本偏移 (见 §2)                        │
│  段级统计 (null_count、distinct 估计) / CRC                  │
└─────────────────────────────────────────────────────────────┘
[ColChunk 列区]   每"逻辑列" → 一串 zone(默认 8192 行/zone)
   每 zone:  [zone_header: encoding_id | bitwidth | min | max | count | null_count]
             [encoded_payload]    ← 按列特征独立选编码(见 1.3)
[InvIndex 倒排区] per-text-column: FST(term→postings_blkno) + 分块 postings(§3)
[TreeBlock 树区]  pre/post/lvl 三列(delta+bitpack) + span_id↔rownum 映射(§4)
[VecBlock 向量区] 可选: 段内 flat 向量 + zone-map(§5)
[RowMap]         seg-local rownum ↔ span_id ; doc-id 直接 = 段内行号(§3 省翻译表)
```

**关键设计点（决定三处根治）：**

- **段不可变**：封盖即只读。折叠/冻结/更新/删除一律不改段——这是【根治膨胀】的总开关：没有原地 UPDATE、没有死元组、没有 vacuum。
- **footer 在尾**：段一次顺序写到底再写 footer（类似 Parquet footer，但 footer 偏移固定可一次读到），崩溃恢复用 footer CRC 判段是否完整封盖（见 §6）。
- **zone 是压缩单元也是随机读单元**：取第 N 行 = `zone=N/8192; off=N%8192`，O(1) 定位单 zone，只解一个 zone——**不是 Parquet 整页解压**。这正面回答"压缩 XOR 随机读"。

### 1.3 可插拔轻量编码（每 zone 独立选）【根治膨胀 + 加速搜索】

| 编码 | 适用 trace 列 | 机制 | 本仓状态 |
|---|---|---|---|
| **delta+bitpack** | `event_ts`/`start/end_time`/`event_id`(雪花单调)/`pre`/`post` | 单调列 delta 后值域极小，按 zone 最小 bitwidth 打包 | 自研，团队有页面整理能力 |
| **RLE** | `status`/`span_kind`/`tenant_id`/`thread_id` | 低基数长游程 | 自研 |
| **dict** | `model`/`name` 文本枚举 | dict + 小码宽；过滤在码上比较不解字符串 | 自研 |
| **FSST** | 短字符串 `name`/`dotted_order` | 压缩域可前缀比较 | 【待移植】内核现无 |
| **ALP** | `total_cost`/`latency_ms`/数值 attrs | 浮点无损轻量压缩 | 【待移植】内核现无 |
| **plain** | 兜底/高熵列 | 不压缩 | 自研 |

一期上 delta-bitpack/RLE/dict/plain（自研可控），二期补 FSST/ALP（诚实标注：**FSST/ALP 在本内核树无现成实现**，grep 确认是 Vortex/SmithDB 算法，非本仓代码，需自研或移植）。

### 1.4 zone-map 段剪枝【加速搜索】

每 zone 存 `min/max/count/null_count`；SegFooter 存段级 `[min_ts,max_ts]`、`[min_trace_id,max_trace_id]`。查询带谓词（`tenant_id=? AND start_time>=? AND total_cost>?`）：先用段级边界跳过整段 → 再用 zone-map 跳 zone → 才解码。与 Level 0 的 RANGE/INTERVAL 分区裁剪正交（分区裁天级，zone-map 裁段内）。

---

## 2. merge-on-read 读路径算法 + deletion/upgrade vector【根治膨胀 + 根治 fold】

### 2.1 数据模型：run = 事件序列，不是一行可变记录

- 写：同一 `span_id` 的多条事件（start / update / tool_call / retry / end / 晚到 feedback）落进**不同段**，append-only，永不原地改。
- **deletion vector**（每段一个 bitmap）：标记本段第 i 行已被逻辑删除/被更高版本取代。存在 SegFooter，**不重写段**。
- **upgrade vector**（段间）：metastore（见 §2.4）记录 `span_id → 最新版本所在 (seg_id, rownum)`；被取代的旧段对应行在其 deletion vector 置位。

逻辑当前态 = 多个不可变段 + deletion/upgrade vector **在读路径上**归并出来。

### 2.2 读路径折叠算法（按 trace_id / span_id）

```
输入: 查询谓词 P, 投影列集 C, 目标 trace_id 集 T
1. 段裁剪:   用各段 SegFooter 的 [min_ts,max_ts]/[min_trace_id,max_trace_id] + zone-map(P)
             选出相关段集 S, 跳过无关段。                          ← 加速搜索
2. 候选收集: 对 S 中每段, 用段内 RowMap / trace_id 索引取出命中 trace 的行,
             过滤掉 deletion vector 已置位的行。
3. 多路归并: 各段内行已按 (trace_id, span_id, seq) 有序;
             跨段做 k 路归并(k=段数), 归并键 (span_id, seq, event_id);
             event_id=应用端雪花(全局单调)保证乱序事件可定序。     ← 根治 fold
4. 折叠:     对同一 span_id 的事件序列, 执行与 Level 0 完全一致的折叠语义:
             后写覆盖标量 / token,cost 累加 / attrs 深合并 / end 补全 / status 推断。
5. 晚物化:   只对投影列 C 解码; 大字段(input/output 全文/媒体)此刻才按 payload_ref
             去 CAS 取(见 §5.3)。                                  ← 根治膨胀(大payload不入段)
```

复杂度 **O(n log k)**（n=候选行，k=段数），不是 Level 0 的"SQL MERGE INTO + query_dop=1 自定义聚合 / 大 trace 退化应用层 DFS"。**fold 从应用层/SQL 层下沉为存储引擎的归并算子**——这是【根治 fold】的核心。

### 2.3 方案 B 相对方案 A 在此处的增量：向量化折叠算子

方案 A 里这套归并藏在 `amgettuple` 背后，一次吐一个 ctid，**逐行**。方案 B 走轨 B（§0），把折叠做成**向量化 TraceScan 算子**：一次产出一个 `VectorBatch`（列批），折叠在列批上做（dict 码上比较、bitpack 域上累加），直接喂上层向量化 HashAgg/Sort。这是方案 B 对 fold 的**额外**加速——A 拿不到。

### 2.4 metastore（段清单 + 向量）落地

单机用**内嵌系统目录表**（普通 ASTORE 行存，跟着主库 WAL/HA 走，不自造）：

- `pg_tracelsm_segment(relid, seg_id, fork_blk_start, blk_len, min_ts, max_ts, min_trace_id, max_trace_id, row_count, level, state)` —— 段清单。
- deletion vector 双写：段内 footer 存一份（自包含），metastore 存可变的"增量删除位图"（因为段不可变，新删除只能记在段外）。读时 `footer_dv OR metastore_dv`。
- upgrade vector：`pg_tracelsm_upgrade(relid, span_id, latest_seg, latest_row)`，或更省地用"段级单调 seq + 读时取最大版本"隐式表达【推测：两种都可行，需 PoC 比成本】。

### 2.5 正确性：乱序 / 晚到 / 删除

- **乱序**：归并键含 `event_id`（雪花全局单调），物理乱序到达不影响折叠结果。
- **晚到（冻结后才到的 eval/feedback）**：**就是普通写一个新段**。新段含该 `span_id` → 下次读自然纳入候选 → 归并即得正确当前态。**晚到 = 零特殊路径**，彻底去掉 Level 0 的 `late_event_inbox + 重融化重写冷分区`。这是事件模型相对物化表的结构性优势。
- **删除/TTL**：合规删 = deletion vector 置位（廉价逻辑标记），物理回收推迟到 compaction（§6）。

### 2.6 本仓同构证据（不是空想）

BM25 已在做同形操作（已读源码）：`bm25_doc_store` 维护 `doc_id ↔ (ItemPointerData tid, part_id)` 且有 `erase`（`bm25_doc_store.h:58-66`）；`InvertedList::vacuum(doc_id_track&...)`（`bm25_inverted_list.cpp:1024`）按"被删 doc 集"在 compaction 物理清理；`InvertedListPageData::DELETE_FLAG`（`bm25_inverted_list.h:54`）= 条目删除标记。**deletion vector + 延迟物理回收，团队已写过**。

---

## 3. 内嵌倒排（中文 jieba + FST term + 分块 postings）【加速搜索】

倒排作为段内的"几列"，与数据列同段、同生命周期、同 zone-map 剪枝（对标 SmithDB 把倒排嵌进 Vortex 段）。

### 3.1 中文分词 = 复用既有 vex_jieba

段构建时对文本列（`input_text`/`output_text`/`name`）调既有 jieba：`dict_jieba.cpp` 经 TS 模板 `TSTemplateJiebaId=3884` 暴露 `cut/lexize`，`bm25_tokenize`（OID 4528）内置。**这是 SmithDB 的空白（无中文分词）、我方现成差异化点。**

### 3.2 term 列用 FST【加速搜索 + 根治膨胀】

不可变段封盖时，对去重 term 一次性构建 **FST**（term→postings 块号），压缩域上做精确查找/前缀扫描/自动机遍历。SmithDB 实测 term_key 88.8MiB→3.8KiB。

- 复用/演进：BM25 现用 `DiskHashTable<Token, TokenIndexEntry>`（`bm25_token_index.h:48-57`，按词长分桶）。**不可变段恰好让 FST 可行**（FST 要求构建期一次性排序，不支持增量插入；段封盖即一次性建）。
- 诚实标注：**FST 是新增组件**（本内核现为 hashtable，非 FST），需自研（引入 `fst`/`tantivy-fst` 思路或在 token_index 上做前缀压缩）。一期可先用 `DiskHashTable` 顶上，二期换 FST 拿压缩比。

### 3.3 分块 postings（128 元素块 + 变长 bitwidth delta）【加速搜索 + 根治膨胀】

每 term 的 postings 按 128 个一块、块内 doc-id 做 delta + 每块最小 bitwidth 打包（高频词低至 3-4 bit/doc），块头存 skip。复用 BM25：`InvertedListSkipPointers`（`bm25_inverted_list.h:33-39`）多级 skip、`il_threshold_levels={4,32,162}` 分级、`try_upgrade/try_downgrade`（`bm25_inverted_list.cpp:1376/1498`）级间迁移——**分块+skip+分级 postings 团队已写过**。

### 3.4 doc-id 直接 = 段内行号【加速搜索】

postings 里的 doc-id **就是段内物理行号**，省掉"segment-local id ↔ 行位置"翻译表。于是：FST 查 term → 行号集 → O(1) 回列区取列（§1.2 随机读）→ 喂 §2 折叠。**全文命中→取列→折叠当前态，段内闭环，零跨结构翻译。**

> 对比 BM25 现状：BM25 doc_store 需 `doc_id↔tid` 翻译表（因建在可变堆表上）；TraceStore 段不可变+自描述，行号即 doc-id，这层翻译被消掉——这是"内嵌"相对"旁挂索引"的实质收益。

### 3.5 字节预算 row group（防 term 频率倾斜）

照 SmithDB：每 row group postings ≤32MB、term-string ≤64MB（按字节而非 term 数，因 term count 是 I/O 大小的差代理）。单个高频词（"agent"）不会把段撑爆。

---

## 4. 区间树编码（pre/post 物化进段）【加速搜索】

### 4.1 物化布局

段封盖时对段内 span 做一次性 O(n) DFS，产出每行 `(pre, post, lvl)`，作为列存进段，用 **delta+bitpack**（DFS 序近单调，压缩比高）。

**关键红利：段内按 `pre` 物理重排行**（段不可变 → 封盖时可自由重排，这是不可变段相对可变表的红利）。于是"子树扫" = 段内**连续行区间** `[pre_root, post_root]` = 连续 zone 扫。

### 4.2 子树范围扫算法

```
1. 由 span_id 定位根行, 读 (pre_root, post_root)。
2. 子树 = 所有 pre ∈ [pre_root, post_root] 的行(区间编码不变式)。
3. 段内按 pre 排序 → 用 zone-map 的 pre min/max 二分定位起止 zone → 连续扫。
4. 扫出行直接是 DFS 先序(=树展示序), 无需再排序。
5. 每行过 deletion vector, 喂 §2 折叠取当前态。
```

比 Level 0 `(tenant,trace,pre) BETWEEN` 二级索引更省：段内连续行 + zone-map，**顺序 I/O，不走索引随机回表**。线程视图同理：`thread_id` 列 + RLE + zone-map。

### 4.3 跨段 / 晚到的树正确性

一个 trace 可能跨多段（晚到子节点落新段）。规则：**段内区间编码只在段内自洽**，是"完整落单段、已封盖"trace 的**快路径**；跨段/未稳定时，以 §2 折叠出的逻辑当前态为准，按 `parent_span_id`（写侧邻接，永远正确）在内存重算 pre/post 展示。`dotted_order`（Level 0 §5 的抗晚到全序）作兜底。诚实标注：**跨段树物化一致性规则需 PoC 用真实乱序 trace 验证**。

---

## 5. 向量【加速搜索】

### 5.1 结论：主路径复用 DiskANN（旁挂），段内只做 flat 兜底

理由（基于已查实）：DiskANN 是成熟存量产品，自带"自管 fork + 独立 buffer 池 + PQ 压缩 + `idx_diskann_inplace_filter` 带过滤 ANN"；**HNSW/Vamana 图要全局连通性，每不可变小段一张小图会割裂图、毁召回**——绝不能为不可变小段重做 ANN 图。采样降规模（只对 root/LLM/error span 建 embedding）让向量集足够小，单一全局 DiskANN 索引即可。

### 5.2 分工

| 层 | 做法 |
|---|---|
| 主向量索引 | 全局一个 DiskANN（复用 `USING diskann`），随 trace 增量 insert |
| 段内 flat 兜底 | 段可选存本段采样 span 原始向量 + zone-map，用于：① 高选择度过滤后候选<阈值时段内暴力精排(recall=100%)；② 段自包含、可独立迁移/校验 |
| merge-on-read 衔接 | DiskANN 命中 span_id → 经 upgrade vector 指到最新段行号 → §2 折叠取当前态 |

### 5.3 大字段晚物化【根治膨胀】

核心列段只存"指向大字段文件的指针 token"（Langfuse `@@@langfuseMedia:...@@@` 范式）；input/output 全文、媒体 payload 落本地盘/MinIO（`object_store` crate 一码通本地/MinIO，满足信创私有化），SHA256 去重 + refcount。list/filter/聚合/树加载只读小核心列，大 payload 仅在用户显式 project 时才取——**MB 级无上界 payload 不入核心段**，核心段小而密、扫描快。

---

## 6. LSM 写路径 + 时间分层 compaction（写放大控制）【根治膨胀】

### 6.1 写路径三级

```
L0  memtable(内存): 新事件先进内存有序结构(按 trace_id, span_id, seq)。
     满阈值/定时 flush → 封成 L1 不可变小段(TraceSeg), 带 zone-map+倒排+树。
L1  近期小段: 高频乱序晚到都落这里(append-only), 不 compaction(还在等 end/feedback)。
L2+ 时间分层 compaction: 时间已稳定、不再变的老段, 合并成大段;
     合并时应用 deletion vector(真删行)、upgrade vector(只留最新版本)、TTL 过期。
```

### 6.2 写放大四杠杆

1. **时间分层（time-tiered），不是 size-tiered**：近期段不压实（很可能还收 end/feedback，过早压成大文件=反复重写=写放大）；只对时间稳定的老段压实成大段。trace"写一次后短期补几次、之后永久只读"的特性让时间分层天然低写放大。
2. **memtable 批量封段**：内存攒批、排好序、一次性封成压缩好的不可变段——写放大=1 次顺序写，无 in-place 更新（对比 Level 0 ASTORE 折叠 UPDATE 产死元组要 vacuum，这是膨胀根治的写侧）。
3. **zero-copy compaction**：合并时对未被 deletion 命中、编码相同的 zone，**直接搬压缩字节不解码再编码**（对标 Microsoft 在 Iceberg 的 zero-copy LSM compaction）。只有含被删行的 zone 才解开重写。诚实标注：需 zone 编码兼容判定，二期优化。
4. **compaction = deletion/upgrade 的物理兑现点**：读路径只逻辑标记（廉价），compaction 才真删真合并真回收——与 BM25 `InvertedList::vacuum` 走 `doc_id_track` 物理清理同构。

### 6.3 单机隔离

compaction = 独立后台线程池 + **IO 限速（令牌桶）** 隔离前台（代替 SmithDB 无状态 compaction service），保 P99 稳定。复用团队"页面整理框架" + `BulkBufferManager` 段生命周期。

---

## 7. 在 openGauss 上的落地机制（AM/smgr/fork，与 DiskANN 先例对应）

### 7.1 双轨改造总表

| 改动点 | 文件:行 | 性质 | DiskANN/CStore 先例 |
|---|---|---|---|
| **新增 fork** `TRACE_FORKNUM=6` | `relfilenode.h:49,57` + `catalog.cpp:90-96` forkNames | 改枚举+数组 | **DiskANN 已加 `VECTOR_FORKNUM=5`** |
| **轨A: TableAmType 第三值** `TAM_TRACELSM=2`, `NUM_TABLE_AM=3` | `tupdesc.h:31,38-43` | 改闭合枚举 | 无（首次扩这个枚举） |
| **轨A: 路由数组第三项** | `tableam.cpp:1260` `g_tableam_routines[]` + `tableam.h:518` `GetTableAmRoutine` 三元改 switch | 改 const 数组+分派 | 无 |
| **轨A: tupTableType 第三类** (AM index = tupType>>1) | `tableam.h:80` `GetTableAMIndex` | 每 tuple baked AM 索引要容纳新值 | 无 |
| **轨B: RelOrientation 第五值** `REL_TRACE_ORIENTED` | `parsenodes.h:43-49` | 改枚举 | **CStore=`REL_COL_ORIENTED` 同模式** |
| **轨B: planner 发 TraceScan** | `createplan.cpp`(仿 `create_cstorescan_plan:2367`) + 新 `T_TraceScan` 计划节点 | 加 plan 节点+路径 | **CStore `make_cstorescan` 同模式** |
| **轨B: 向量化 executor 节点** | `runtime/executor/` 新 `nodeTraceScan.cpp`(仿 nodeCStoreScan) | 加 exec 节点 | **CStore 向量化节点同模式** |
| **WAL 资源管理器** `RM_TRACELSM_ID` | `rmgrlist.h`(尾部 PG_RMGR 一行, `RM_NEXT_ID` 自动占位) | 改编译期 RM 列表 | **`RM_DISKANN_ID`/`RM_BM25_ID` 同模式** |
| **极限RTO 并行恢复二级派发** | `redo_xlogutils.cpp`(switch + 派发数组) | 改恢复路径 | **DiskANN 已填** |
| **独立 buffer 池 + 刷盘线程** | `g_instance.trace_cxt`(新全局字段) + `postmaster.cpp`(新 `trace_writer_main`) | 改 postmaster+全局实例 | **DiskANN `vec_writer` + `g_instance.diskann_cxt` 同模式** |
| **reloption 识别** `WITH(storage=tracelsm)` | reloptions 解析 + heap 建表路径 | 加 reloption | CStore `orientation=column` 同模式 |
| **bootstrap pg_am(可选)** | `pg_am.h` (若 trace 表也需挂段内索引 AM) | 改 catalog | bm25 4429/diskann 4471 同模式 |

### 7.2 复用件清单（已读源码确认存在）

- `vector_smgr`（独立 FORK + `vector_buffers` 独立池 + `vec_writer` 刷盘线程）→ TraceStore 段文件/缓存/刷盘直接套。
- `disk_container` 模板库（`DiskVector`/`VarDiskVector`/`DiskArray`/`DiskHashTable`/`BlockMgr`/`FreeSpace`，DiskANN+BM25 共用）→ 段内倒排/字典/min-max/deletion vector 用它建，自动获 shared buffer + 标准 WAL。
- `InvertedList`（分块 postings/skip/vacuum/upgrade/downgrade）、`bm25_doc_store`、`vex_jieba`/`bm25_tokenize` → §3 倒排。
- DiskANN → §5 向量主索引。

### 7.3 执行器/优化器怎么接住（两种挂法）

1. **轨 A 起步（amgettuple 同构）**：即便走 TAM，最省力的折叠出口仍是"内部归并 + 对外吐通过过滤的行"，对接标准执行器。DiskANN `diskann_scan.cpp` 已示范 `scan->xs_ctup.t_self = tids[i]` 把行位置塞回执行器。
2. **轨 B 主收益（向量化 TraceScan）**：planner 识别 `REL_TRACE_ORIENTED` → 发 `T_TraceScan` → executor `nodeTraceScan` 直接产 `VectorBatch`，折叠/zone 剪枝/列裁剪在向量化算子里做，喂上层向量化 HashAgg/Sort/Limit。**这是 CStore 已铺好的通路，照抄 `create_cstorescan_plan` + `min_max_optimization`（`createplan.cpp:2285`，CStore 的 zone-map 下推）即得。**

诚实标注：`amgetbitmap` 在 BM25/DiskANN 都没实现，多条件 bitmap-AND 要自己写或在单算子内合并。

---

## 8. WAL / 崩溃恢复

### 8.1 RM 名额：有头room（已查实）

RM ID 由 `rmgrlist.h` 的 `PG_RMGR` 宏自动枚举（`rmgr.h:24` `enum RmgrIds{...RM_NEXT_ID}`，`RM_MAX_ID=RM_NEXT_ID-1`，`RmgrTable[RM_MAX_ID+1]`）——**新增一行 `PG_RMGR(RM_TRACELSM_ID, ...)` 即自动占下一个 ID、数组自动扩容**。不是定长写死的名额，无溢出风险（与早前研究底稿"RM_MAX_ID 名额有限"的担忧相比，实际是宏自动枚举，更宽松）。

### 8.2 两种 WAL 策略，分而治之（照 DiskANN/BM25 抄）

**策略 1 — 页式数据走标准 buffer-redo**（段内倒排/字典/元数据/deletion vector 增量）：
```
写: XLogBeginInsert → XLogRegisterBuffer(0, buf, REGBUF_STANDARD) → XLogRegisterBufData
    → XLogInsert(RM_TRACELSM_ID, op) → PageSetLSN   (全程 START/END_CRIT_SECTION)
读: XLogReadBufferForRedo → memcpy 进页 → PageSetLSN + MarkBufferDirty
```
范本 `bm25xlog.cpp:214-295`(写)/`27-212`(`bm25_redo`)。crash-safe、可主备复制、可极限RTO并行恢复（前提填 §8.3 派发表）。

**策略 2 — 段本体大块走"批量 FPI / log_newpage"**（不可变段一次成型）：
段封盖是**批量、一次性**事件，不逐行记 WAL（那会 WAL 量=数据量、放大严重）。用 `REGBUF_FORCE_IMAGE` 整页镜像 / `log_newpage` 批量记（范本 `Bm25XLogAppendPage` `bm25xlog.cpp:225` 用 `REGBUF_FORCE_IMAGE`、`DiskannRedoExtendFullPages` `diskannxlog.cpp:46` 整页恢复）。**段一旦封存只读，WAL 只在 flush/compaction 写一次，之后纯读**——天然契合不可变段，WAL 压力远小于 ASTORE 原地 UPDATE。

### 8.3 极限RTO / 主备

填 `redo_xlogutils.cpp` 的二级派发（switch + `{TracelsmRedoParseToBlock, RM_TRACELSM_ID}` 派发数组），否则极限RTO并行恢复遇未知 rmid 会 PANIC（`redo_xlogutils.cpp` `default: PANIC unknown rmid`）。DiskANN 已填过这处，照抄。

### 8.4 memtable 崩溃恢复

memtable 用一条轻量 redo 保护（或直接复用主 WAL 流的 append 记录）；崩溃后从"最后封盖段 + 重放未 flush 的 memtable WAL"恢复。诚实标注：**memtable 的 WAL 设计是新工作量**（DiskANN/BM25 无对应物，它们没有内存 LSM L0），需 PoC 验证恢复点一致性。

---

## 9. 与 Level 0（事件表 + 折叠）的迁移 / 共存

### 9.1 共存（同库双形态）

- Level 0 的 `span_events`（ASTORE + RANGE/INTERVAL 分区）继续作为**摄入落点 + 热写缓冲**（最近 N 小时活 trace）。
- TraceStore 表（`USING tracelsm`）作为**冻结/历史层**：后台服务把"时间已稳定"的分区批量灌入 TraceStore 段（= memtable flush 的另一入口）。
- 查询路由：活 trace 查 Level 0（或 TraceStore L0 段），历史 trace 查 TraceStore L1+；跨界查用 UNION ALL + 折叠对齐（两边折叠语义必须**逐字节一致**，见 §9.3）。

### 9.2 迁移（Level 0 → Level 1 灰度）

1. **影子双写**：新 trace 同时写 Level 0 和 TraceStore，比对查询结果（折叠态、树、检索命中）一致性。
2. **冷分区回灌**：存量 Level 0 冷分区按时间批量 compaction 进 TraceStore 大段，灌完后 detach 旧分区。
3. **回滚阀**：TraceStore 表 drop 即回到纯 Level 0；段文件独立 fork，不污染主堆。

### 9.3 折叠语义一致性（迁移正确性的命门）

Level 0 折叠在 SQL（MERGE + 自定义有序聚合 `tv_jsonb_deep_merge_*`）里；Level 1 折叠在向量化算子里。两者必须产出**完全相同**的当前态（标量覆盖序、token/cost 累加、attrs 深合并、status 推断规则）。建议：把折叠语义抽成**一份规格 + 双实现一致性测试集**（同一组事件流喂两边，断言结果相等），作为迁移门禁。诚实标注：这是方案 B 最隐蔽的工程债——**两套折叠实现的等价性需长期回归保障**。

---

## 10. 工程量（人月）与风险

> 对标他们当年加 UStore 的量级。诚实标注：人月是**经验估算**，未做 WBS 排期；区间给"乐观–现实"。

### 10.1 工程量分解（人月，按内核 C 团队计）

| 模块 | 乐观 | 现实 | 说明 |
|---|---|---|---|
| 段格式 + 编码框架(delta/RLE/dict/plain) | 2 | 3.5 | FSST/ALP 二期再 +2 |
| merge-on-read 归并 + deletion/upgrade vector | 2.5 | 4 | 折叠语义 + 向量正确性 |
| 内嵌倒排(复用 BM25 件) + FST(新) | 2 | 3.5 | FST 自研是大头 |
| 区间树物化 + 子树扫 | 1 | 1.5 | 复用 Level 0 DFS |
| 向量(复用 DiskANN, 段内 flat) | 0.5 | 1 | 主要是衔接 |
| LSM 写路径 + memtable + WAL | 2.5 | 4 | memtable WAL 是新活 |
| 时间分层 compaction + zero-copy | 2 | 3.5 | IO 限速隔离 |
| **轨A: TAM 78 回调(最小子集)** | 2 | 4 | 见 §10.3 取舍 |
| **轨B: 向量化 TraceScan plan+exec 节点** | 3 | 5 | 仿 CStore, 优化器集成最易踩坑 |
| 独立 buffer 池+刷盘线程(复用 vector_smgr) | 1 | 2 | |
| HA/主备/极限RTO 恢复打通 | 2 | 4 | 主备一致性测试昂贵 |
| Level0 共存/迁移/折叠一致性测试 | 1.5 | 3 | |
| **小计(不含信创回炉)** | **~24** | **~42** | |

**现实约 35–42 人月**（≈ 3–4 名内核工程师约 1 年），与"加 UStore 是一个大版本级工程"量级一致。轨 A+轨 B 全做是上沿；只做轨 B + TAM 最小子集可压到下沿。

### 10.2 信创 / 重测评代价（决定日历，不只产能）

- **改内核二进制 = 送测客体变了 → 触发内核级重新测评**（不是应用层增量适配）。安可测评结果按批次公告、**一年约 2 次**，错过一窗丢半年。
- 对比方案 A（Index-AM，不改 TAM 但仍改内核 6 处固定表）已经要回炉；方案 B 改动面更大（TAM 枚举 + orientation + 优化器/执行器节点 + HA 恢复），**回炉测评范围更广、周期更长**。
- 诚实标注：精确认证范围/周期需向 CESI/测评机构书面确认 + 拿首单采购需求书，此处不编造数字。结论方向明确：**方案 B 把认证从"应用层增量"推到"内核大改回炉"，是最重的一档。**

### 10.3 关键取舍：轨 A 的 78 回调不要全做

`TableAmRoutine` 的 78 个回调里，**对不可变 append-only + merge-on-read 引擎，很多语义不匹配或可空实现**：`tcap_*`（时间胶囊/闪回）、`tuple_lock_updated`/`tuple_abort_speculative`（行锁/投机插入，append-only 无需）、`tops_form_cmprs_tuple`/`tops_deform_cmprs_tuple`（ASTORE 压缩元组格式）、`tuple_update`（不可变段无原地更新，update=写新段+vector）。

**建议最小可用子集**：实现 scan 全家（`scan_begin/getnexttuple/end/rescan` + `scan_index_fetch_*`）、`tuple_insert/multi_insert`（路由进 memtable）、`tuple_delete`（置 deletion vector）、`index_build_scan`（让段内/旁挂索引可建）、slot/tops 的 deform/getattr（向量化取值）。其余给"不支持"明确报错或空实现。**真正的查询性能收益走轨 B 向量化节点，不靠把 78 回调填满。**

### 10.4 风险登记

| 风险 | 等级 | 说明 / 缓解 |
|---|---|---|
| **优化器/向量化集成深度** | 高 | 轨 B 仿 CStore，但 CStore 在内核里盘根错节(setrefs/streamplan/createplan 多处特判 orientation)；新 orientation 要把这些特判全覆盖到，**遗漏一处=某类查询走错计划或报错**。最易拖期。 |
| **HA / 主备 / 极限RTO 一致性** | 高 | 自管 fork 的 redo + 独立 buffer 池在主备/RTO 下的一致性是历史上最易出隐性 bug 处；DiskANN 趟过但 TraceStore 多了 LSM memtable 维度。 |
| **两套折叠语义等价** | 高 | §9.3，Level0 SQL 折叠 vs Level1 算子折叠长期回归。 |
| **信创回炉周期** | 高 | §10.2，日历瓶颈，可能卡首单。 |
| **memtable WAL/恢复** | 中 | §8.4 新工作量，无先例。 |
| **FST/FSST/ALP 自研** | 中 | 本仓无现成，需自研/移植；一期可降级(hashtable/plain)。 |
| **跨段树一致性** | 中 | §4.3 需 PoC。 |
| **zero-copy compaction zone 兼容判定** | 低 | 二期优化，可先全解码重写兜底。 |

### 10.5 方案 B 相对方案 A 多拿到什么（收益结算）

| 维度 | 方案 A(Index-AM) | 方案 B(第三 TAM + orientation) |
|---|---|---|
| **向量化执行** | 拿不到，amgettuple 逐行回堆表 | ✅ 轨B 发向量化 TraceScan，折叠/剪枝/列裁剪在列批上做 |
| **优化器原生** | 当成普通索引，cost 估算受限 | ✅ orientation 感知，zone-map 下推、列裁剪、谓词下推进算子(仿 CStore min_max_optimization) |
| **HA / 主备 / 极限RTO** | 索引段需自行打通(部分) | ✅ 一等表，跟随主表 HA 语义 |
| **列裁剪 / 晚物化下推** | 须回堆表，难纯列投影 | ✅ 算子直接列投影，大字段 project 才取 |
| **DDL/catalog 一等** | 挂在主表上的特殊索引 | ✅ `CREATE TABLE ... USING tracelsm` 一等表 |
| **工程量 / 认证** | 约 6 处固定表 fork + 应用层 | 35–42 人月 + 内核大改回炉(最重) |

**结论：方案 B 的增量收益集中在"向量化扫描 + 优化器原生 + HA 一等"，对大规模聚合/导出/训练扫描类负载是质变；代价是 ~40 人月 + 最重的信创回炉。是否值得，取决于方案 A 的 PoC 是否在目标 SLA 下被向量化/HA 缺失卡住——若卡住，B 是正解；若没卡，B 是为未证明的瓶颈预付最重成本。**

---

## 11. 三处短板怎么根治（逐条交代，不回避）

### 短板①：膨胀（span_current 折叠/冻结的原地 UPDATE 在 ASTORE 上产死元组、要 vacuum）

**根治链条（多管齐下，从写模型层面拆掉膨胀来源，而非靠 vacuum 追赶）：**

1. **不可变段 + merge-on-read 取代原地 UPDATE**（§2）：折叠/冻结/更新/删除**全部不改段**——append 新事件 + deletion/upgrade vector 标记。**没有原地 UPDATE → 不产死元组 → 不需要 vacuum。** 这是从存储模型层面切断 ASTORE 膨胀的根。
2. **memtable 批量封不可变段**（§6.1-6.2）：写=内存攒批→一次顺序写封段，写放大=1，无 in-place 写。对比 Level 0 每次折叠一次 UPDATE 产一个死元组。
3. **时间分层 compaction 批量回收**（§6.2）：死版本不靠 vacuum 逐元组清，而是 compaction 批量重写时一次性丢弃（+ zero-copy 搬未删 zone），回收成本可控、可 IO 限速、与前台解耦。回收点 = deletion/upgrade vector 的物理兑现点，与 BM25 `InvertedList::vacuum` 同构。
4. **大字段晚物化**（§5.3）：MB 级无上界 payload 不入核心段，隔离在 CAS 层（SHA256 去重 + refcount），核心段小而密。
5. **方案 B 额外**：作为一等表，膨胀监控/段统计走优化器 ANALYZE 原生，不需应用层估算。

**净效果：膨胀来源（原地 UPDATE 产死元组）被结构性消除，回收（死版本）从"持续 vacuum"变为"compaction 批量、限速、可控"。**

### 短板②：冷数据"压缩 XOR 可检索"二选一（CStore 压了不能建 GIN/BM25/向量；要可搜只能留不压缩行存）

**根治：一个段同时承载 [压缩列 + zone-map + 内嵌倒排 + 段内向量]，压缩与随机访问/检索共存于同一份字节。**

1. **zone 既是压缩单元也是随机读单元**（§1.2-1.3）：取任意行 = O(1) 定位单 zone、只解一个 zone（不是 Parquet/CStore 整页/整 CU 解压）。压缩比由可插拔编码（delta/RLE/dict/FSST/ALP）拿到，随机读不被牺牲。**这正面破解"CStore 压了就不能随机检索"。**
2. **内嵌倒排进压缩段**（§3）：FST term 列（压缩域可前缀查）+ 128 块 delta postings（高频词 3-4 bit/doc），doc-id 直接=段内行号（省翻译表）→ FST 查中→行号→O(1) 回压缩列取数。**老 trace 既被压着、又能全文/JSON 检索**——CStore "压了不能建 BM25/GIN" 的死结被解开。
3. **段内向量 flat + 全局 DiskANN**（§5）：冷段语义检索由全局 DiskANN 覆盖，段内 flat 做高选择度精排兜底（recall=100%）——压缩段照样可向量检索。
4. **zone-map 让压缩段先剪后解**（§1.4）：检索/过滤先用 zone min/max/count 跳段跳 zone，再解码——压缩不但不妨碍检索，反而因 zone-map 让检索更快。

**净效果：不再需要"为了老 trace 可搜而额外留一份不压缩行存"。压缩与可检索在同一份字节上共存，这是标准 CStore 给不了、Vortex 给得了、TraceStore 照此实现的核心价值。**

### 短板③：query-time fold 开销（活/未冻结 trace 读时在应用层折叠事件，活 trace 多/事件量大时慢）

**根治：把折叠从应用层/SQL 层下沉为存储引擎的（向量化）归并算子，并用 zone-map/排序键/时间窗大幅缩小要归并的数据量。**

1. **fold 下沉为段读归并算子**（§2.2）：fold 不再是 Level 0 的"SQL MERGE INTO + query_dop=1 自定义有序聚合 / 大 trace 退化应用层 DFS"，而是段内有序 + 段间 k 路归并，复杂度 O(n log k)，在读路径内完成。
2. **方案 B 独有：向量化折叠算子**（§2.3）：走轨 B 的 `nodeTraceScan`，折叠在 `VectorBatch` 列批上做（dict 码上比较、bitpack 域上累加），直接喂上层向量化 HashAgg/Sort——**这是方案 A 的逐行 amgettuple 拿不到的额外加速**，活 trace 多/事件量大时不再线性变慢。
3. **剪枝把归并量压到最小**（§1.4, §2.2）：段级时间/trace_id 边界 + zone-map 先剪段再归并；progressive time-window（查"最新 N 条"沿时间倒走、有界时间窗、read newest→stream→merge→dedupe→尽早停，不全排序再 limit）。
4. **活 trace 直读热缓冲**：L0 memtable / 最近小段在内存或独立 buffer 池热区，活 trace 折叠直接在热数据上做，不打盘。
5. **晚到=普通写、零特殊折叠路径**（§2.5）：晚到事件写新段、下次读自然纳入归并，去掉 Level 0 的 late_event_inbox/重融化——fold 路径不被晚到复杂化。

**净效果：fold 从"应用层全表聚合/串行保序"变为"存储引擎向量化归并 + 强剪枝"，活 trace 规模与事件量上升时不再线性劣化。**

---

## 12. 诚实标注（未验证 / 推断项汇总）

- 全部为**静态源码核实**（已读 `tableam.h`(78回调/枚举/分派)、`tupdesc.h`(NUM_TABLE_AM=2)、`tableam.cpp`(g_tableam_routines 2项)、`parsenodes.h`(RelOrientation 4值)、`createplan.cpp`(CStoreScan 路径/min_max_optimization)、`rmgr.h`/`rmgrlist.h`(RM 自动枚举)、`relfilenode.h`/`catalog.cpp`(VECTOR_FORKNUM 先例)、`bm25_doc_store.h`/`bm25_inverted_list.h`），**未编译、未跑 PoC**。
- **人月估算是经验值**，未做 WBS/资源排期；轨 A/轨 B 的优化器集成深度（CStore 在 setrefs/streamplan/createplan/subselect 多处 orientation 特判）实际工作量可能超估，是最大不确定项。
- **FSST/ALP/FST 在本内核树无现成实现**（grep 确认是 Vortex/SmithDB 算法），列为自研/移植，不可声称"复用既有"。
- **memtable WAL/恢复**无 DiskANN/BM25 先例（它们无内存 LSM L0），是新设计，恢复点一致性需 PoC。
- **跨段树物化一致性**、**段内按 pre 物理重排的成本 vs 子树扫收益**、**upgrade vector 显式表 vs 段级 seq 隐式表达**均为推断，需 PoC。
- **信创回炉范围/周期**未向测评机构确认，方向结论（方案 B 是最重一档）可靠，具体数字不编造。
- SmithDB/Vortex 性能宣称（随机读 100x、FST 88.8MiB→3.8KiB、3-4 bit/doc、zero-copy compaction）来自研究底稿引用的官方/二手来源，属对标参照，非本设计实测。

---

## 关键文件路径（均绝对路径）

**内核 Table-AM 边界（方案 B 的 fork 起点）：**
- `/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/include/access/tableam.h`（78回调 `TableAmRoutine`:88-511、`g_tableam_routines`:513、`GetTableAmRoutine`:518、`GetTableAMIndex` tupType>>1:80）
- `/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/include/access/tupdesc.h`（`NUM_TABLE_AM=2`:31、`TableAmType` 枚举:38-43、`td_tam_ops`:152）
- `/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/gausskernel/storage/access/table/tableam.cpp`（`g_heapam_methods`:443、`g_ustoream_methods`:1145、`g_tableam_routines[]`:1260）

**向量化第三存储引擎先例（轨 B 的真正范本，CStore orientation）：**
- `/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/include/nodes/parsenodes.h`（`RelOrientation` 4值:43-49）
- `/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/gausskernel/optimizer/plan/createplan.cpp`（`create_cstorescan_plan`:2367、`make_cstorescan`:133、`min_max_optimization`:2285、orientation 分派:2323/2629/2815）

**自管 fork / smgr / WAL 先例（DiskANN/BM25）：**
- `/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/include/storage/smgr/relfilenode.h`（`VECTOR_FORKNUM=5`:49、`MAX_FORKNUM`:57）
- `/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/common/backend/catalog/catalog.cpp`（`forkNames[]` 含 "vec":90-96）
- `/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/include/access/annvector/store/{vector_smgr.h,bulkbuf_smgr.h,smgr.md}`
- `/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/include/access/rmgr.h`（`RM_NEXT_ID` 自动枚举:24-30）、`src/include/access/rmgrlist.h`（`RM_DISKANN_ID`:85、`RM_BM25_ID`:88）
- `/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/gausskernel/storage/access/{bm25/bm25xlog.cpp,diskann/diskannxlog.cpp}`

**复用容器 / 倒排 / 分词：**
- `/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/include/templates/vtl/disk_container/{diskvector,blockmgr,diskarray,disk_hashtable,freespace}.hpp`
- `/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/include/access/bm25/{bm25_inverted_list.h,bm25_token_index.h,bm25_doc_store.h}`、`src/gausskernel/storage/access/bm25/bm25_inverted_list.cpp`
- `/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/gausskernel/storage/access/bm25/tokenizer/dict_jieba.cpp`

**设计上下文：**
- 内核边界判定：`/Users/Four/JobProjects/yitrace/vex-x/docs/design/appendix-K_kernel-boundary.md`
- Level 0 schema（被根治对象）：`/Users/Four/JobProjects/yitrace/vex-x/docs/design/2026-06-16_tracevault-schema.md`
- SmithDB/Vortex 对标底稿：`/Users/Four/JobProjects/yitrace/vex-x/docs/research/2026-06-16_smithdb-and-landscape-research.md`

---

**一句话总裁决**：方案 B 在本内核可行，且真正的"被优化器/向量化原生覆盖"靠的是**新增 `REL_TRACE_ORIENTED` orientation + 仿 CStore 发向量化 TraceScan**（轨 B），而非天真地把第三种引擎塞进封闭的 `TableAmType` 枚举（轨 A，且 78 回调里大半与不可变 append-only 语义不匹配，只做最小子集）。三处短板被结构性根治：膨胀靠不可变段+merge-on-read+批量 compaction 拆掉原地 UPDATE，压缩 XOR 可检索靠 zone 同粒度的压缩+随机读+内嵌 FST 倒排共存，fold 靠下沉为向量化归并算子+强剪枝。代价是 ~35–42 人月内核工程 + 最重一档的信创回炉测评——是否支付，取决于方案 A 的 PoC 是否被向量化/HA 缺失卡在目标 SLA 上。
---

## O4 方案C 复用CStore

# 方案 C: 混合:复用 openGauss CStore 列存底 + 自研 merge-on-read 与倒排层

I have all grounding needed. The task says NOT to write report .md files — return findings directly. Here is the complete Level 1 方案 C design.

---

# yiTrace Level 1 · 方案 C：复用 openGauss CStore 列存底 + 自研 merge-on-read 与倒排层

> 设计日期 2026-06-17。所有 CStore 事实均已**读本仓真实代码核实**（`src/include/cstore.h`、`src/include/access/cstore_am.h`、`cstore_delta.h`、`cstore_delete.h`、`src/include/storage/cstore/cstore_compress.h`、`src/gausskernel/storage/cstore/cstore_am.cpp`、回归用例 `hw_cstore_index.source`/`hw_cstore_unsupport.out`/`cstore_array.source`）。凡标【未验证】为静态读码推断、未编译未跑 PoC。

---

## 0. 一句话结论：CStore 已经替你做了一半，缺的是"可检索"和"版本折叠"

把本仓 CStore 拆开看，**SmithDB/Vortex 五机制里它已经现成给了三个**，这是方案 C 相对方案 A/B（自研段格式）的最大价值：

| Vortex/SmithDB 机制 | CStore 现状（已读码核实） | 证据 |
|---|---|---|
| 列式不可变段 | ✅ **CU(Compression Unit) 即不可变列块**，append-only，`CUPointer` 指向文件偏移 | `cstore.h:35,81` `DefaultFullCUSize = BatchMaxSize*60` |
| 压缩（可插拔编码） | ✅ **Delta / Delta2 / RLE / Bitpack / Dictionary / Zlib / LZ4 / DeltaPlusRLEv2** | `cstore_compress.h:32-42,68-69,173` |
| zone-map 剪枝 | ✅ **CUDesc 每 CU 存 min/max**，扫描前 `RoughCheck` 剪 CU | `cstore.h:67-68` CUDescMin/MaxAttr；`cstore_am.cpp` `RoughCheckIfNeed` |
| deletion vector | ✅ **每 CU 一个 delete bitmap**，删=置位，**读时跳过死行，从不原地重写** | `cstore_am.h:376` `m_cuDelMask`；`cstore_am.cpp:3306` `GetCUDeleteMaskIfNeed`、`IsDeadRow`、`IsTheWholeCuDeleted` |
| LSM 写缓冲 | ✅ **delta 表**缓冲小批量插入，攒够 `m_delta_rows_threshold` 行 `MoveDeltaDataToCU` 落 CU | `cstore.h:32-35` FirstCUID=1000 给 delta；`cstore_insert.h:255` `m_delta_rows_threshold`；`cstore_delta.h:35` `MoveDeltaDataToCU` |

**CStore 没给、必须自研的（方案 C 的全部工作量集中在这）：**
1. **可检索**：CStore 表上**只能建 psort 索引**，btree/hash/gist/**gin**/spgist 全部 `Un-support feature`（`hw_cstore_index.source:8-16`）。→ **GIN/BM25 中文倒排建不到 CStore 表上**，这是"压缩 XOR 可检索"在 CStore 上的真实卡点。
2. **版本折叠（merge-on-read 的"折叠"那一半）**：CStore 的 delete bitmap 只能"删行"，**没有 upgrade-vector / 多版本归并**——span 的"早上 start、下午 end、晚到 feedback"这种**同一 span_id 多事件折叠成当前态**，CStore 不会做，需自建读路径折叠层。
3. **时间分层 compaction**：CStore 的 delta→CU 是"小批攒大块"，**不是按时间冷热分层**，死行回收靠 `VACUUM FULL` 重写（写放大大）。
4. **FSST/ALP/FST**：本仓无（`cstore_compress.h` 无此项），字符串只有 dict+lz4/zlib，term 字典无 FST。

→ 方案 C = **CStore 当列式底座（白嫖压缩+zone-map+delete-bitmap+delta-LSM），旁路自建"倒排 Index-AM + 版本折叠读层 + 时间分层 compaction 调度"**，把 fork 量压到最小。下面逐节给设计。

---

## 1. 段格式与磁盘布局

### 1.1 物理形态：一张 CStore 主表 + 三类旁路结构

方案 C **不自定义段文件格式**——段 = CStore 的 CU。一个 trace 事件表建成列存表，物理上每列独立成 `<relfilenode>_C<attid>` 文件，按 CU 切块（`CFileNode`，`cstore.h:96`）。在此之上挂三类自研旁路：

```
┌─────────────────────────────────────────────────────────────┐
│ trace_event  (orientation=column)  —— CStore 主表(不可变 CU 底) │
│   核心列: trace_id span_id parent_span_id seq event_id ts      │
│           start_time end_time status span_kind name            │
│           total_cost latency_ms token_in token_out tenant_id   │
│           pre post lvl thread_id  (区间树编码列, §4)            │
│   引用列: input_ref output_ref attrs_ref  (大字段指针, §5晚物化)│
│   ┌─ CU0..CUn  每CU: 压缩列块 + CUDesc{min,max,rowcount,delmask}│
│   └─ delta表(FirstCUID<1000): 未满CU的行缓冲                    │
├─────────────────────────────────────────────────────────────┤
│ 旁路1: trace_inv  —— 自研倒排 Index-AM (TraceInv)               │
│   FST(term→postings) + 分块postings(doc-id=CU内全局行号RowId)   │
│   建在 input_text/output_text/name 上, jieba中文分词           │
│   独立 fork + disk_container 容器 (照 BM25 模式)                │
├─────────────────────────────────────────────────────────────┤
│ 旁路2: trace_ver  —— 版本折叠元数据 (merge-on-read manifest)    │
│   span_id → 当前态所在(cuid,rowoffset) 的 upgrade map           │
│   普通行存小表 / 或 disk_hashtable; 配合 CU delete-bitmap       │
├─────────────────────────────────────────────────────────────┤
│ 旁路3: 大字段 CAS —— object_store(本地NVMe/MinIO) + SHA256去重  │
│   input_ref/output_ref/attrs_ref 指向这里, project才取(§5)     │
└─────────────────────────────────────────────────────────────┘
```

### 1.2 "段"=CU 的关键属性（决定全盘设计）

- **不可变**：CU 一旦由 delta 表 flush 成型即只读（`MoveDeltaDataToCU`）。折叠/冻结/晚到都不改 CU。这是根治膨胀的总开关——**没有原地 UPDATE 就没有死元组**。
- **doc-id = RowId**：CStore 内部行寻址 = `(cuid, row_offset_in_cu)`，可线性化为全局 RowId。倒排 postings 直接存 RowId，**省 segment-local-id ↔ 行号翻译表**（对标 Vortex doc-id=行号）。CStore 的 `ItemPointer` 风格 ctid 在列存里就是 `(cuid<<13 | offset)` 形态，倒排回表 = 直接按 RowId 取列。
- **CU = zone**：CStore 的 CU 默认 `BatchMaxSize*60`（约 6 万行/CU，`cstore.h:39`）即天然的 zone 粒度；CUDesc 的 min/max = zone-map。SmithDB 的"每 row group 32MB postings 预算"在这里对应"每 CU 的倒排片"。

### 1.3 列编码选择（直接用 CStore 编码，FSST/ALP 列为补强项）

| trace 列 | CStore 现成编码 | 说明 |
|---|---|---|
| `ts`/`start_time`/`end_time`/`event_id`/`seq`/`pre`/`post` | **Delta + Bitpack**（单调列） | `CU_DeltaCompressed`/`CU_BitpackCompressed` 现成 |
| `status`/`span_kind`/`event_type`/`tenant_id` | **RLE / Dictionary**（低基数） | `CU_RLECompressed`、`m_adopt_dict` 现成 |
| `name`/短 `model` | **Dictionary + LZ4** | `StringCoder` 现成 |
| `total_cost`/`latency_ms`/`token_*` | Delta2 / Bitpack / **(补)ALP** | 浮点无损现状走 lz4；ALP 是补强项【未验证：本仓无 ALP】 |
| 长文本（若不外置） | LZ4 / Zlib / **(补)FSST** | 现状 lz4/zlib；FSST 压缩域前缀比较是补强项【未验证：本仓无 FSST】 |

→ **结论：trace 的核心列（时序、低基数枚举、计量数值）全部命中 CStore 现成编码，压缩比直接到位，零自研。** FSST/ALP 只对"长字符串列内联"和"浮点极致压缩"有增量，且 §5 已把长文本外置，FSST 优先级低。

---

## 2. merge-on-read 读路径算法

这是方案 C **自研的核心**。CStore 给了"删行"（delete bitmap），没给"折叠多版本成当前态"。

### 2.1 数据模型：事件 append，当前态读时算

- 写：同一 `span_id` 的 start/update/end/tool_call/retry/晚到 feedback 都是**独立事件行**，append 进 delta 表→flush 成 CU。**永不 UPDATE 原行**。
- **CU delete-bitmap**（CStore 现成）：标记"本 CU 第 i 行已被更新版本取代/已删"，读时 `IsDeadRow` 跳过。
- **upgrade map**（自研 `trace_ver`）：`span_id → 最新版本 RowId`。旧版本行在其 CU 的 delete-bitmap 置位。

### 2.2 读路径折叠算法（按 trace_id / span_id 取当前态）

```
输入: 查询谓词 P (tenant, 时间窗, status, ...) + 投影列 Cols + 可选全文 Q
输出: 折叠后的当前态行

1. 谓词下推 + zone-map 剪枝  [复用 CStore RoughCheck]
   用 CUDesc 的 min/max 对 (tenant_id, start_time, status, total_cost...) 剪掉无关 CU。
   时间窗谓词 → 直接跳过 max_ts < 窗口下界 的整批 CU。

2. (可选)全文召回  [自研 TraceInv 倒排, §3]
   若带 Q: jieba 分词 → FST 查 term → postings 得 RowId 集 → 与步骤1的存活CU取交。

3. 候选行收集  [复用 CStore 列扫 + delete-bitmap]
   对存活 CU 按需读列(只读 Cols 投影 + 折叠键列), GetCUDeleteMaskIfNeed 拿 delmask,
   IsDeadRow 跳死行。得到候选事件行集。

4. 按 span_id 分组 + 多路归并折叠  [纯自研]
   候选行按 (span_id, seq, event_id) 排序;
   event_id=应用端雪花(全局单调, 见Level0)做归并键 → 乱序/晚到天然定序。
   对每个 span_id 的事件序列做与 Level0 完全一致的折叠语义:
     - 标量(status/end_time/name): 后写覆盖
     - 计量(token_in/out/cost/latency): 累加
     - attrs: 深合并
     - end 补全 / status 推断
   归并 = O(n log k), k=参与CU数, 不是应用层全表聚合。

5. upgrade-vector 校正  [自研 trace_ver]
   若某 span_id 的最新版本 RowId 不在候选(跨更老的已compaction段), 经 upgrade map 直接定位补取。

6. 投影 + 晚物化  [复用 CStore 列裁剪 + §5]
   只解码 Cols; input/output/attrs 大字段此刻才按 *_ref 去 CAS 取。
```

### 2.3 落地：折叠逻辑挂在哪

两条挂法，**起步用 (A)**：

- **(A) 应用/网关层折叠 + 下推谓词到 CStore**（起步，零内核）：步骤 1~3 全是 CStore 原生向量化扫描（`CStoreGetNextBatch`，已带 RoughCheck + delete-bitmap），步骤 4~5 在 yiTrace 上层（trace 网关）做归并。相比 Level 0 的痛点③——Level 0 折叠是"应用层对**行存全量事件**做 DFS/聚合"，方案 C 折叠是"对**已被 zone-map + delete-bitmap + 倒排三重剪枝后的极少候选行**做归并"，输入量级差一到两个数量级。**这一步本身就大幅缓解 fold 开销，且零内核改动**。
- **(B) 折叠下推为自定义向量化算子**（演进，要碰内核）：把步骤 4~5 做成一个 openGauss 向量化执行节点（VecAggregate 变体或 CustomScan），在内核内归并。彻底消除上层 fold。**【未验证：openGauss CustomScan 对列式投影的完整度需 PoC】**，故不作为起步。

### 2.4 晚到事件 = 普通写（结构性优势）

Level 0 需要 `late_event_inbox + 重融化重写冷分区`；方案 C 里**晚到事件就是再 append 一行**（永远成立），其 span_id 命中旧数据，下次读时归并自然纳入，upgrade map 指到新行、旧行 delete-bitmap 置位。**晚到 = 零特殊路径**。

---

## 3. 内嵌/旁路倒排（中文 jieba + FST + 分块 postings）

### 3.1 关键约束：倒排不能"内嵌"进 CStore，只能"旁路"

读码确认：**CStore 表上 `CREATE INDEX USING gin/btree/...` 全部 `Un-support feature`，只允许 psort**（`hw_cstore_index.source`）。所以 Vortex 那种"倒排作为段内一列、与数据列共存于一个文件"在 CStore 上**做不到**——这是方案 C 相对方案 B（自研段格式可内嵌）的硬差距，必须诚实承认。

**方案 C 的解法：倒排做成独立的 Index-AM（TraceInv），物理旁路，逻辑挂回 RowId。** 这正是本仓 BM25 已经走通的路（BM25 也是旁路 index AM，不是内嵌进堆表）。

### 3.2 TraceInv 结构（照 BM25 现成件改）

| 组件 | 复用本仓件 | 改造 |
|---|---|---|
| 中文分词 | ✅ `dict_jieba.cpp` 的 `Jieba`(TSTemplateJiebaId)、`bm25_tokenize`(OID 4528) | 直接调，SmithDB 无中文分词=差异化点 |
| term 字典 | ✅ BM25 `DiskHashTable<Token,...>`（按词长分桶） | 一期直接用；二期换 **FST**（不可变 CU→term 一次性排序→FST 可建，拿前缀扫描+压缩比）【未验证：FST 本仓无，需自研/移植 tantivy-fst】 |
| 分块 postings | ✅ BM25 `InvertedListPageData` + skip pointer + 分级(`il_threshold_levels`) | doc-id 改存 **RowId(cuid<<13\|offset)** 而非 BM25 的 doc_id↔tid 翻译 |
| 落盘容器 | ✅ `disk_container`(`DiskVector`/`DiskHashTable`/`BlockMgr`/`FreeSpace`) + WAL | 照 BM25 走 MAIN_FORKNUM + 标准 buffer-redo |
| AM 注册 | ✅ 照 `pg_am.h` BM25(4429) 加一行 `traceinv` OID | 16 handler 独立 `access/traceinv/` |

### 3.3 doc-id = RowId 免翻译表（相对 BM25 的改进）

BM25 的 `bm25_doc_store` 需要 `doc_id ↔ ItemPointerData tid` 翻译表（因为它建在可变堆表上）。TraceInv 建在**不可变 CU** 上，CStore 行号 `(cuid, offset)` 永不变（删=bitmap，不挪行），所以 **postings 直接存 RowId，省掉 doc_store 翻译层**。全文命中 → RowId → 直接 `CStoreGetNextBatch` 按 RowId 回表取列 → 喂 §2 折叠。

### 3.4 与 CU delete-bitmap 协同回收

TraceInv 复用 BM25 的 `InvertedList::vacuum(doc_id_track&, ...)`（`bm25_inverted_list.cpp:1024`）：compaction 重写 CU 时，把被删 RowId 集喂给 vacuum，物理清理对应 postings。**逻辑删（bitmap）廉价、物理删延到 compaction** 的闭环，团队已写过。

### 3.5 倒排 zone 预算对齐 CU

SmithDB 每 row group 倒排 ≤32MB；这里**每 CU 一个倒排片**，CU 即预算单元。查询先用 CUDesc min/max 剪掉整批 CU（连同其倒排片），再对存活 CU 走 FST——与 SmithDB "zone 先剪再 FST" 同构。

---

## 4. 区间树编码（[pre,post] 物化进 CU 列）

### 4.1 编码进 CStore 列，子树=区间扫

trace 树用双编码（同 Level 0 §5），但**把 pre/post/lvl 作为 CStore 列物化**：

- 段（CU 批）封盖时对其中 span 做一次性 O(n) DFS，产出每行 `(pre, post, lvl)`。
- `pre/post` 列用 **Delta+Bitpack**（DFS 序近单调，压缩极好，CStore 现成）。
- 子树 = `pre ∈ [pre_root, post_root]`，用 **CUDesc 的 pre min/max 做 RoughCheck**，直接定位含该区间的 CU 批，跳过其余——顺序列扫，不走索引回表。

### 4.2 CStore 不能按 pre 物理重排行的约束 → 退化方案

Vortex 不可变段可自由重排行使子树成连续区间；**CStore CU 的行序由插入序（delta flush 序）决定，不能任意重排**（重排=重写 CU=失去不可变红利且 RowId 变动会废掉倒排）。所以：

- **不做"段内按 pre 物理排序"**（这是方案 B 的红利，方案 C 拿不到）。
- 退化为：`pre/post` 列 + CUDesc min/max 剪 CU + CU 内向量化过滤 `pre BETWEEN`。子树扫从"连续行区间"退化为"少数候选 CU 内的列过滤"。**仍比 Level 0 的 `(tenant,trace,pre) btree BETWEEN` 回表好**（向量化列扫 + zone 剪枝），但不如方案 B 的纯顺序扫。诚实标注：这是方案 C 为"少 fork、复用 CStore"付的一个性能税。

### 4.3 跨段/晚到树正确性

一个 trace 跨多 CU 批；树展示以 §2 折叠出的当前态全集为准，按 `parent_span_id` 邻接在内存重算 pre/post 展示。`dotted_order` 全序（Level 0 §5）作抗晚到兜底。**【未验证：跨段树一致性规则需真实乱序 trace PoC】**

---

## 5. 向量

**结论与 Level 1 方案 B 一致：主路径复用 DiskANN 全局索引，CU 内不嵌图。**

- 主向量索引：全局一个 DiskANN（`USING diskann (embedding, tenant_id, span_kind)` inplace-filter），随 trace 增量 insert。CStore 表上 DiskANN 是否能直接建需确认——**DiskANN 是独立 index AM 走自己 fork，不依赖被索引表是行存还是列存**【未验证：DiskANN over CStore 表的 build 路径需 PoC，可能需先把 embedding 列以行存影子表喂索引】。
- CU 内 flat 兜底：高选择度过滤后候选 < 阈值时，按 RowId 从 CStore 取 embedding 列做暴力精排（recall=100%），用 CStore 现成列扫即可，**零新结构**。
- 不嵌全图：不可变小 CU 切碎 HNSW/Vamana 图会毁召回；采样降规模（只对 root/LLM/error span 建 embedding）让单一全局索引够用。

---

## 6. LSM 写路径与时间分层 compaction（写放大）

### 6.1 写路径：直接用 CStore delta 表当 memtable（白嫖）

```
L0  delta表(FirstCUID<1000, cstore.h:32-35): 新事件先 INSERT 进 delta 表(行存形态缓冲)
     → 攒够 m_delta_rows_threshold 行(cstore_insert.h:255)
L1  MoveDeltaDataToCU(cstore_delta.h:35): 排序+压缩, 一次性封成不可变 CU
     此刻同步: 建该批 CU 的倒排片(TraceInv) + 物化 pre/post 列 + min/max CUDesc
```

**这一整条 CStore 现成**（delta 缓冲 + 阈值 flush + 压缩落 CU），方案 C 只在 flush 钩子上挂"建倒排片 + 物化树列"。memtable=delta 表是行存，崩溃恢复走 CStore 现成 WAL。

### 6.2 时间分层 compaction（自研调度，CStore 缺这块）

CStore 原生只有"delta→CU"和"VACUUM FULL 重写全表"两档，**没有时间冷热分层**。方案 C 自建 compaction 调度（不改 CStore 存储格式，只编排）：

```
近期CU(热): 还在收 end/feedback, 不合并(过早合=写放大)。delete-bitmap 置位即可。
老CU(冷, max_ts 稳定): 后台合并多个小CU→大CU
     - 应用 delete-bitmap: 跳过死行, 只搬存活行
     - 应用 upgrade-vector: 同span多版本只留最新
     - TTL 过期行直接丢
     - 重建合并后大CU的倒排片 + pre/post + min/max
     - 旧CU整体回收(IsTheWholeCuDeleted 命中的CU可整块释放)
```

### 6.3 写放大四杠杆

1. **时间分层**：近期 CU 不压实（trace"写后短期补几次、之后永久只读"，过早合=反复重写）。
2. **delta 批量封 CU**：CStore 现成——攒批、排序、压缩、一次顺序写，无 in-place（**对比 Level 0 ASTORE 折叠 UPDATE 产死元组要 vacuum，这是痛点①根治点**）。
3. **整 CU 回收**：`IsTheWholeCuDeleted`（`cstore_am.cpp:1747`）命中的 CU 整块释放，不逐行 vacuum。
4. **zero-copy 受限**：Vortex 能搬压缩字节不解码；CStore compaction 需解 CU 再重压（因为要剔死行）。**仅"整 CU 无死行"时可直接复制 CU 文件块不解压**【未验证：需在 compaction 逻辑里判 delmask 全 0 走快路径】。这是方案 C 比 Vortex zero-copy 弱的一处。

### 6.4 compaction 与前台隔离

后台线程池 + IO 令牌桶限速（照 DiskANN `vec_writer` 后台线程模式），避免 compaction 抖动前台 P99。**【单机 P99 稳定关键】**

---

## 7. openGauss 落地机制（AM / smgr / fork，对应 DiskANN 先例）

| 层 | 落地方式 | 是否改内核 | DiskANN/BM25 先例 |
|---|---|---|---|
| **列式段底** | 直接用 CStore 表（`WITH orientation=column`） | ❌ 零改动 | — CStore 是内核现成 |
| **delete-bitmap / zone-map / 压缩 / delta-LSM** | CStore 原生 API（`CStoreBeginScan`/`GetNextBatch`/`GetCUDeleteMaskIfNeed`/`MoveDeltaDataToCU`） | ❌ 零改动 | — |
| **倒排 TraceInv** | 新 index AM，独立 `access/traceinv/` 编译单元 | ⚠️ 轻 fork：pg_am 加 1 行 + rmgrlist 加 1 行 + redo 派发 1 处 | ✅ 照 BM25(4429)：`pg_am.h`、`rmgrlist.h:88 RM_BM25_ID`、`bm25xlog.cpp` |
| 倒排落盘 | `disk_container`(MAIN_FORKNUM) + 标准 buffer-redo | ❌ 复用模板库 | ✅ `diskvector.hpp`/`blockmgr.hpp` |
| 中文分词 | `dict_jieba` / `bm25_tokenize` | ❌ 复用 | ✅ `TSTemplateJiebaId` |
| **版本折叠读层** | 起步上层（网关）；演进 CustomScan | ❌(起步) / ⚠️(演进) | ✅ 扫描吐 RowId 照 `diskann_scan.cpp` |
| **upgrade-map** | 行存小表 / disk_hashtable | ❌ | — |
| **compaction 调度** | 后台线程 + IO 限速 | ⚠️ 注册后台线程(postmaster) | ✅ `vec_writer_main`(postmaster.cpp) |
| 向量 | 复用 DiskANN | ❌ | ✅ 现成产品 |

**方案 C 的内核改动清单（"极轻 fork"，远小于方案 B）：**
1. `pg_am.h` 加 `traceinv` AM 行 + `#define traceinv_AM_OID`（照 BM25）。
2. `rmgrlist.h` 加 `PG_RMGR(RM_TRACEINV_ID, ...)` 一行（**已核实 `RM_NEXT_ID` 自增、`RM_MAX_ID=RM_NEXT_ID-1`，追加即可，无固定槽位上限**，`rmgr.h:24-30`）。
3. `redo_xlogutils.cpp` 倒排 redo 二级派发(若走极限RTO并行恢复)。
4. (可选)compaction 后台线程注册。

**方案 C 不碰的（相对方案 B 省下的）**：不新增 fork 号（CStore 用 MAIN_FORKNUM；倒排用 disk_container 也走 MAIN_FORKNUM）、不自管段文件（CStore 自己管 CU 文件）、不自研段格式/编码/zone-map（CStore 全包）。**这是方案 C "少 fork、少自研格式"承诺的兑现点。**

---

## 8. WAL / 崩溃恢复

- **CStore 数据（CU/delta/CUDesc/delete-bitmap）**：CStore 自带 WAL，零额外工作。delete-bitmap 更新、delta→CU flush 都已是 crash-safe 的内核现成路径。
- **TraceInv 倒排**：照 BM25 `bm25xlog.cpp` 标准 buffer-redo——`XLogBeginInsert → XLogRegisterBuffer(REGBUF_STANDARD) → XLogInsert(RM_TRACEINV_ID, op) → PageSetLSN`；redo 侧 `XLogReadBufferForRedo`。必须同时填 `rmgrlist.h` + `redo_xlogutils.cpp`，否则主备/极限RTO `default: PANIC unknown rmid`。
- **upgrade-map**：行存小表自带 WAL；或 disk_hashtable 走 buffer-redo。
- **崩溃一致性次序**：先落 CStore CU(已 WAL) → 再落倒排片(WAL) → 再更 upgrade-map。恢复后若倒排片缺失，可由对应 CU 重建（倒排是可重建的派生结构，最坏全量 reindex）。**【未验证：CStore flush 与倒排建片的原子性边界需 PoC，建议倒排 build 失败时回滚该批 delta→CU 或标记 CU "倒排待重建"】**

---

## 9. 与 Level 0（事件表 + 折叠）的迁移 / 共存

方案 C 与 Level 0 **同构**——都是"事件 append + 读时折叠"，差别只是底座（Level 0 行存 ASTORE，Level 1 CStore 列存）。迁移平滑：

| 维度 | Level 0 | Level 1 方案 C | 迁移动作 |
|---|---|---|---|
| 事件模型 | 行存事件表 | CStore 列存事件表 | **schema 几乎不变**，建表加 `WITH orientation=column` |
| 折叠语义 | 应用层 MERGE/DFS | §2 归并（同语义） | 折叠代码**复用**，输入从全表→剪枝后候选 |
| 雪花 event_id | 已有 | 直接复用（归并键） | 无 |
| 全文 | GIN on 行存 | TraceInv on CStore | 倒排重建（派生结构） |
| 晚到 | late_inbox + 重融化 | 普通 append | **删掉 late_inbox 特殊路径** |

**共存策略（双层热冷）**：
- **热数据（近 N 天 / 活 trace）**：留 Level 0 行存事件表——高频点写、活 trace 直读、低延迟。
- **冷数据（封盖 trace / 老于 N 天）**：后台搬迁到 Level 1 CStore——压缩 + 可检索 + 不膨胀。
- 搬迁 = "读 Level 0 该 trace 全事件 → 折叠/或保留事件 → 批量 INSERT 进 CStore delta → flush CU → 建倒排片 → 删 Level 0 行"。这天然就是 §6 的 L0→L1 LSM flush，**搬迁器复用 compaction 调度框架**。
- 查询路由：网关按时间窗 / trace 状态决定查 Level 0 还是 Level 1 还是 UNION——对上层 API 透明。

---

## 10. 工程量（人月）与风险

### 10.1 工程量（基于"CStore 现成、BM25/DiskANN 可抄"的前提）

| 模块 | 人月 | 说明 |
|---|---|---|
| CStore 表 schema + 编码选型 + 接入 | 0.5 | 零自研，建表 + 列编码 reloptions 调优 |
| TraceInv 倒排 AM（一期：DiskHashTable 字典 + RowId postings + jieba） | 2.5 | 抄 BM25，改 doc-id=RowId、去 doc_store 翻译 |
| merge-on-read 折叠读层（一期：网关层归并 + 谓词下推） | 2.0 | 折叠语义复用 Level 0；新写归并算子 + upgrade-map |
| upgrade-map + delete-bitmap 协同 | 1.0 | trace_ver 表 + 置位逻辑 |
| 区间树 pre/post 列物化 + 子树扫 | 1.0 | DFS 物化 + RoughCheck 子树定位 |
| 时间分层 compaction 调度（后台线程 + IO 限速 + CU 合并/回收/倒排重建） | 2.5 | CStore 无此，自研编排 |
| 向量接入（复用 DiskANN + flat 兜底） | 0.5 | 主体复用 |
| Level 0↔1 搬迁器 + 查询路由 | 1.5 | 复用 compaction 框架 |
| WAL/恢复（倒排 RM 注册 + redo 派发 + 一致性边界） | 1.5 | 抄 bm25xlog |
| 联调 / PoC / 压测（膨胀、P99、召回、写放大） | 2.5 | 见风险项需实测 |
| **一期合计** | **≈16 人月** | 2~3 人 6~8 个月 |
| 二期补强（FST 字典 / FSST·ALP 编码 / 折叠下推 CustomScan / zero-copy compaction 快路径） | +6~8 | 选做，拿压缩比/性能增量 |

**对比方案 B（自研段格式）**：方案 B 要自研 zone-map/编码框架/段文件 smgr/新 fork，CStore 的那一半（压缩+zone-map+delete-bitmap+delta-LSM）全要从零造，一期估 ≈24~28 人月。**方案 C 用 CStore 换掉约 8~12 人月的格式自研**，这是它的核心性价比。

### 10.2 风险（诚实标注）

| 风险 | 等级 | 说明 / 缓解 |
|---|---|---|
| **倒排只能旁路、不能内嵌进 CStore 段** | 中 | CStore 表禁 GIN（已核实）。失去 Vortex"倒排与列同段共 I/O 调度"的红利；旁路倒排多一次 I/O。缓解：CU 与倒排片按 cuid 对齐、批量预取。**这是方案 C 对比方案 B 的结构性让步，需在设计评审中接受。** |
| **CStore CU 行不能按 pre 重排** | 中 | 子树扫退化为候选 CU 内列过滤(§4.2)，不如方案 B 顺序扫。可接受。 |
| **CStore 高频小批 INSERT 的 delta 表压力** | 中 | trace 写入若直灌 CStore，delta 表频繁 flush 可能抖动。缓解：热数据留 Level 0，只冷搬迁批量进 CStore（§9）——**这也是为什么必须双层共存而非纯 CStore**。【未验证：delta flush 在高写入下的 P99 需压测】 |
| **DiskANN/倒排能否建在 CStore 表上** | 中 | DiskANN/TraceInv 是独立 fork 的 index AM，理论不依赖被索引表 orientation；但 build 时取列数据的路径需确认。**【未验证，需 PoC】** 缓解：embedding/文本以列扫喂索引 build。 |
| **CStore VACUUM/空间回收语义** | 中 | 纯靠 delete-bitmap 不回收空间，回收靠 compaction 重写或 VACUUM FULL（写放大）。自研时间分层 compaction 是必须项，不是可选。 |
| **zero-copy compaction 拿不到（除整CU无死行）** | 低 | compaction CPU/写放大比 Vortex 高。可接受。 |
| **CStore 不支持 CHECK/外键约束**（已核实 `hw_cstore_unsupport.out:44`） | 低 | trace 表本就不依赖这些；schema 用应用层校验。 |
| **CStore 数组支持有限**（实际**支持** int[]/text[]，与任务"无数组"前提相反） | 低（澄清） | 已核实 `cstore_array.source` 支持数组列。attrs 仍建议外置 JSON ref（§5）而非 array 列，避免大字段毒化核心 CU。 |
| FST/FSST/ALP 需自研 | 低 | 二期补强，一期 dict+lz4 + DiskHashTable 顶上，不阻塞。 |

---

## 11. 三处短板怎么根治（逐条交代，不回避）

### 短板①：膨胀（折叠/冻结的原地 UPDATE 在 ASTORE 上产死元组、要 vacuum）

**方案 C 根治手段（含 CStore 哪里够 / 哪里要补）：**

- **够（CStore 白嫖）**：CStore 的 CU **不可变**，删除是**置 delete-bitmap 位**（`m_cuDelMask`，`GetCUDeleteMaskIfNeed`/`IsDeadRow` 已核实），**从不原地 UPDATE，不产死元组**。folder/冻结 = 写新事件 + 置位旧行，不 UPDATE。delta→CU 是批量顺序写（`MoveDeltaDataToCU`），写放大 ≈1。**ASTORE 原地 UPDATE 产死元组的根源被 CStore 不可变模型直接拆掉。**
- **要补（CStore 不够）**：CStore 只有 delete-bitmap，**死行物理回收靠 VACUUM FULL 全表重写（写放大大）**。方案 C 补**时间分层 compaction**（§6.2）：近期 CU 不动（避免过早合并的写放大），老 CU 后台批量合并时一次性丢死行 + 整 CU 回收（`IsTheWholeCuDeleted`）。**回收从"逐元组 vacuum"变成"批量、限速、与前台解耦的 compaction"。**
- **净结果**：写入端无死元组（不可变 CU）+ 回收端批量低写放大（时间分层 compaction）= 膨胀从源头和回收两侧同时根治。

### 短板②：冷数据"压缩 XOR 可检索"二选一

**方案 C 根治手段（这是方案 C 最微妙、必须最诚实的一条）：**

- **压缩这半：完全够（CStore 白嫖）**。CStore CU 现成 Delta/RLE/Dict/Bitpack/LZ4/Zlib（已核实 `cstore_compress.h`），trace 核心列全部命中高压缩比编码。**无需为可检索保留一份不压缩行存镜像**——这是相对 Level 0(CStore 压了不能建 GIN→要可搜只能留行存)的进步。
- **可检索这半：CStore 不够，必须补旁路倒排**。关键诚实点：**CStore 表禁建 GIN/BM25（已核实只允许 psort），所以"倒排内嵌进压缩列段"在 CStore 上做不到**。方案 C 用**独立 Index-AM(TraceInv) 旁路倒排**补上：FST/字典 + 分块 postings，**doc-id 直接 = CStore RowId**（不可变 CU 行号永不变），全文命中→RowId→CStore 列扫回表→折叠。**压缩(CStore CU) 与 可检索(TraceInv 旁路) 在同一份 RowId 空间上共存，不需要不压缩镜像。**
- **与 Vortex 的差距（不回避）**：Vortex 是"倒排与列**同段同文件**"，方案 C 是"倒排**旁路**、靠 RowId 逻辑挂回"。物理上多一跳 I/O、失去同段 I/O 调度合并红利。**这是方案 C 用"复用 CStore、少 fork"换来的让步，但"压缩 XOR 可检索"的二选一本身被打破了**——冷 trace 既被 CStore 压着、又能经 TraceInv 全文检索，无需行存镜像。这是对短板②的实质根治，只是不如方案 B 的内嵌优雅。

### 短板③：query-time fold 开销（活/未冻结 trace 读时应用层折叠，活 trace 多/事件量大时慢）

**方案 C 根治手段：**

- **够（CStore 白嫖剪枝）**：CStore 向量化批量扫 + **RoughCheck 用 CUDesc min/max 剪 CU**（zone-map，已核实）+ delete-bitmap 跳死行。折叠的**输入**从 Level 0 的"行存全量事件全表聚合"缩到"zone-map + delete-bitmap + 倒排三重剪枝后的极少候选行"——**输入量级降一到两个数量级，fold 开销随之骤降**。
- **够（自研折叠下推）**：把折叠从 Level 0 的"应用层对全量事件 DFS/MERGE"改为 §2 的**多路归并算子**（O(n log k)，按雪花 event_id 定序），活 trace 直读 delta 表(热缓冲)。
- **要补**：归并折叠逻辑 CStore 不提供，需自建（§2.2）。起步在网关层做（零内核，已比 Level 0 快，因输入被剪枝）；演进下推为向量化 CustomScan 算子彻底消除上层 fold【未验证：openGauss CustomScan 列式投影完整度需 PoC】。
- **净结果**：活 trace 多/事件量大时，fold 不再线性恶化——剪枝把输入压小 + 归并替代全表聚合 + 活 trace 走热缓冲。短板③根治。

---

## 关键文件路径（均绝对路径，供实现定位）

- CStore 底座（复用）：`/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/include/cstore.h`、`src/include/access/cstore_am.h`（`m_cuDelMask:376`、`GetCUDeleteMaskIfNeed`、`RoughCheckIfNeed`、`IsTheWholeCuDeleted`）、`src/include/access/cstore_delta.h`（`MoveDeltaDataToCU:35`）、`src/include/access/cstore_insert.h`（`m_delta_rows_threshold:255`）、`src/include/storage/cstore/cstore_compress.h`（编码 32-42）、`src/gausskernel/storage/cstore/cstore_am.cpp`（`GetCUDeleteMaskIfNeed:3306`）
- CStore 索引约束（已核实只允许 psort）：`src/test/regress/input/hw_cstore_index.source:8-16`、`src/test/regress/expected/hw_cstore_unsupport.out:44`
- 倒排 TraceInv（抄 BM25）：`src/gausskernel/storage/access/bm25/{bm25_inverted_list.cpp,bm25_token_index.cpp,bm25xlog.cpp,tokenizer/dict_jieba.cpp}`、`src/include/access/bm25/{bm25_inverted_list.h,bm25_token_index.h,bm25_doc_store.h}`
- disk_container 容器库（复用）：`src/include/templates/vtl/disk_container/{diskvector,blockmgr,diskarray,disk_hashtable,freespace}.hpp`
- AM/RM 注册（轻 fork 点）：`src/include/catalog/pg_am.h`（BM25=4429）、`src/include/access/rmgrlist.h:88`、`src/include/access/rmgr.h:24-30`（`RM_NEXT_ID` 自增、追加即可）
- 后台线程/扫描吐 RowId（照 DiskANN）：`src/gausskernel/process/postmaster/postmaster.cpp`（`vec_writer_main`）、`src/gausskernel/storage/access/diskann/diskann_scan.cpp`
- 设计上下文：`/Users/Four/JobProjects/yitrace/vex-x/docs/design/appendix-K_kernel-boundary.md`（Table-AM 封闭/Index-AM 开放）、`2026-06-16_tracevault-schema.md`（Level 0 被根治对象）、`/Users/Four/JobProjects/yitrace/vex-x/docs/research/2026-06-16_smithdb-and-landscape-research.md`（Vortex/SmithDB 对标）

## 诚实标注（未验证 / 推断项汇总）
1. 全部为**静态读码核实**（已读上列 CStore/BM25 头与实现、回归用例），**未编译、未跑 PoC**。
2. CStore 表上能否直接建 DiskANN/TraceInv 等独立 fork index AM（build 取列路径）——**未验证，需 PoC**。
3. CStore delta 表在 trace 高频小批写入下的 flush P99 抖动——**未验证，需压测**（已用"热数据留 Level 0、冷批量搬迁"缓解）。
4. 折叠下推为 openGauss 向量化 CustomScan 算子的完整度——**未验证**，起步用网关层折叠。
5. 跨段树物化一致性规则、倒排片与 CU flush 的原子性边界——**需 PoC**。
6. FST / FSST / ALP 本仓**无现成实现**（已核实 `cstore_compress.h` 仅 delta/RLE/dict/bitpack/lz4/zlib），列为二期自研/移植，不可声称复用。
7. CStore zero-copy compaction（搬压缩字节不解码）仅在"整 CU 无死行"时可走快路径，常规 compaction 需解压重压——比 Vortex 弱。
8. 任务前提"CStore 无数组"与实测不符：CStore **支持** int[]/text[] 数组列（`cstore_array.source` 已核实）；但仍建议大字段外置 ref 而非数组内联。