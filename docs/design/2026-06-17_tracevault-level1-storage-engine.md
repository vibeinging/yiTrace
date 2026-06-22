# yiTrace Level 1:trace 专用存储引擎 —— 设计与诚实裁决

> 日期：2026-06-17｜目标:认真设计自研 trace 存储引擎(列式不可变段 + merge-on-read + 内嵌倒排),从根上解决膨胀/压缩可检索/fold,对标 SmithDB Vortex,落 openGauss。
> 方法:9 agent 工作流(3 调研 + 3 方案 + 首席综合 + 2 红队),全程钉在你们真实内核代码上。设计全文/红队见 `appendix-O/P/Q`。
> **本文最重要的是第 0 节的诚实裁决 —— 认真设计的结果,恰恰证明了"真存储引擎不可能轻"。**

## 0. 诚实裁决(先读)
**认真设计 + 对抗性读码复审的结论:做一个真正根治膨胀/搜索的 trace 存储引擎(Level 1),必然要动 openGauss 内核,与你们锁定的"v1 不动内核 / 信创继承 / 上市快"战略直接冲突。** 首席推荐的方案 A("Index-AM 自管段、轻 fork、对外 SQL 不变、复用 DiskANN redo")听起来两全,但红队**读码证伪了它的两个承重卖点**:

- **致命①·执行器模型物理不成立**:openGauss 标准 IndexScan 里 `amgettuple` 只能吐 TID,执行器随后**回堆表取列、在堆上判 MVCC 可见性**(亲验 `nodeIndexscan.cpp:100-114`、`indexam.cpp:537-577`)。所以"宽数据全在 AM 段、堆表只留占位行、SQL 不变"做不到:要么**堆里存折叠后的宽行 → ASTORE 死元组膨胀回归(短板①前提崩塌)**,要么堆里是占位行→查询返回占位数据。要让 AM 不回堆直供折叠列,必须上 **CustomScan / 自管可见性的合成 index-only** —— **那正是方案 B 的执行器工程**。→ **"A 比 B 轻"大幅坍塌。**(DiskANN/BM25 这两个先例恰恰反证:它们索引的是真实堆行,宽数据是额外副本,堆里有源真行;A 把这关系倒置了。)
- **致命②·必须动内核,信创回炉在 Phase 1 触发,不是 Phase 3**:`disk_container` 把 WAL 写死成 DiskANN 专属 opcode(亲验 `diskvector.hpp:244/268/291/321`→`XLogInsert(RM_DISKANN_ID,...)`,redo 非 relation-agnostic);**BM25 同用 disk_container 却照样自建了 `RM_BM25_ID` + 完整 xlog/redo + 15 处恢复派发注册**。所以 traceseg 必须自建新 RM = 改内核固定表;新 Index AM 无 `CREATE ACCESS METHOD` 语法,必须编进 bootstrap `pg_am.h` + 内核二进制。→ **traceseg = 改内核二进制 = 送测客体变更 = 触发内核级重测评**,而你们 2026-06-17 决策摘要把"不动内核"列为 v1 战略前提、把"动内核→信创回炉→丢批次窗口"列为承重风险。**"轻 fork、对回炉最友好"是失实定性。**

**所以这不是"Level 1 设计好了",而是暴露了一个你必须拍的战略叉路(见 §3):真存储引擎 = 内核 fork = 信创回炉,鱼与熊掌(不动内核/信创继承)不可兼得。**

> 红队还点名:首席稿"又在 punt"——把执行器命门推给"二期"、把 fold 性能推给"以后升格 B"、把真压缩(FST/FSST/ALP)推给 Phase 2、快照一致性只字未提。这正是你警告我别犯的回避;诚实记在这里。

## 1. 设计本身(方案 A `traceseg` 精华,技术上是对的)
抛开"轻/不动内核"的失实包装,**段引擎的数据范式设计是扎实的、且循你们已上线先例**:
- **不可变列式段 TSS**:列区(可插拔编码 delta/RLE/dict/plain;FST/FSST/ALP 二期)+ **zone(8192行)= 压缩单元=随机读单元=剪枝单元** → 一份压缩字节上 O(1) 随机取值(破 Parquet/CStore 的"压缩 XOR 随机读")。
- **内嵌中文倒排**(段内一列,doc-id=段内行号免翻译表;jieba 复用现成、term 字典一期 DiskHashTable 二期 FST、分块 delta postings 借 BM25 `InvertedList`)→ 老 trace 同字节既压缩又可中文检索(破"压缩 XOR 可检索")。
- **merge-on-read**(段不可变 + deletion/upgrade 向量,读路径 O(n log k) 归并折叠多版本 span 事件)→ 免原地 UPDATE(根治膨胀)+ fold 下沉读引擎(降 fold 开销);晚到=普通写新段、零特殊路径。
- **区间树**([pre,post] 物化进段、段内按 pre 物理重排 → 子树=连续顺序扫)。
- **时间分层 compaction**(近段不压、老段合并真删 + zero-copy 搬未删 zone;独立线程 + IO 限速)。
- **落地载体**:循 DiskANN `vector_smgr`(自管段文件 + 独立 buffer)+ `disk_container` 模板 + BM25 `InvertedList`/jieba —— 这些你们**已上线三遍**,所以"能建"是真的(只是不"轻")。

机制层面,三处短板确实被范式根治(详见 appendix-O 第三部分);**问题不在范式,在落地代价被低估**。

## 2. 红队揪出的其余硬伤(除两处致命外)
- **热 trace merge 读放大可能打不过 Level 0**:活 trace 事件散在 L0 memtable + 多个未压实 L1 小段,读一条活 trace = k 路归并,而 zone-map 对"同时段同 trace_id"的热数据几乎剪不掉 → 短板③在最热场景可能没根治,只是搬走。**Phase 0 必须 benchmark 热 trace 读扇出 vs Level 0,并定"活 trace 读 ≤N 段"硬不变量。**
- **memtable 持久化洞**:内核里无 LSM L0 先例;要么 per-event WAL(=你想避免的写量回来),要么崩溃丢未刷事件、与 Level 0(堆持久)发散、破"折叠逐字节一致"门禁。**对策:共存期让 Level 0 堆做持久真相源,traceseg memtable 当可重建缓存。**
- **真压缩推到二期**:Phase 1 字符串/浮点用 plain、term 用 DiskHashTable,"对标 Vortex 同字节根治"在一期只交付"可检索 + 弱压缩",压缩那半是话术。
- **快照/MVCC 一致性缺失**:合成折叠行在堆里无对应可见元组,段+memtable+upgrade-map 在事务快照下如何可重复读、与 DiskANN 增量 insert 如何对账 —— 只字未提。金融政企"审计可复现"零容忍,这是准入项。
- **upgrade-map/deletion 向量无界增长**(长期被 eval/feedback 反复 touch 的 trace);需定大小上界与退休规则。
- **fork 号冲突 blast radius 更大**(`MAX_FORKNUM=VECTOR=PCA=5`,`FirstColForkNum=MAX_FORKNUM`,加 `TRACESEG=7` 牵动列存 fork 编号)。

## 3. 这暴露的真实战略叉路(必须你拍)
认真设计的净产出是把一个二选一摆清楚:

| | 选项 1:Level 0/0.5 为现实终态 | 选项 2:认真做 Level 1 真引擎 |
|---|---|---|
| 形态 | 扩展(表/函数)+ 冷区 CStore 压缩 + 行存检索镜像 | traceseg 内核 AM(段引擎)+ CustomScan + 新 RM |
| 膨胀 | **管理**(append 事件 + 整分区 DROP + CAS,但 span_current 折叠 UPDATE 仍有 vacuum) | **根除**(不可变段,零原地更新) |
| 压缩×可检索 | 旁路/二选一缓解(冷数据压 CStore 但检索靠行存镜像) | **同字节根治**(内嵌倒排进压缩段) |
| 动内核 | **否**(信创继承,符合 v1 战略) | **是**(新 RM+AM+执行器,信创回炉) |
| 工期/成本 | 快(扩展骨架已写) | **MVP 实际 >20 人月 + 重测评 + 招聘**,且 A≈B(执行器都要动) |
| 类比 | TimescaleDB 级(扩展) | SmithDB 级(自研存储引擎) |

**核心矛盾:你不能同时要"不动内核/信创继承/上市快"和"SmithDB 级真存储引擎"。** 之前你问"是不是太简单"——诚实答案是:**真不简单的那个,代价就是内核 fork + 信创回炉。**

## 4. 放行前生死 PoC(Phase 0 #0,先于写任何引擎)
红队把命门收敛成必须最先验的:
1. **#0 执行器命门(决定 A 是否成立)**:在这版 openGauss 上,自定义扫描(CustomScan / 合成 index-only)能否**不回堆、自管可见性、直供多列折叠当前态**?不行→A 退化为"段+回堆取宽行=膨胀回归",要么回 Level 0/0.5、要么直接认 B。
2. **新 RM 必建确认**(BM25 先例已强烈指向必建)+ 据此重估内核改动面与**信创回炉时间线**(对齐决策摘要 §3:测评段串产品成熟之后 + 排批次窗口)。
3. **热 trace 读放大 benchmark**(短板③真伪)+ 折叠正确性一致性集(乱序/晚到/崩溃不丢)。
4. 向 CESI 书面确认"内核新增 Index AM"是否触发底座栈重测(而非应用层增量)。

## 5. 我的诚实建议
1. **v1 现实终态 = Level 0/0.5**(扩展 + 冷区 CStore + 行存检索镜像)。它对得起"不动内核/信创继承/上市快",诚实承认它是"管理而非根除"膨胀/压缩/fold —— 这是 TimescaleDB 级,不是 SmithDB 级,但能交付、能过信创、能先拿客户。
2. **Level 1 真引擎 = v2+ 战略投资**,不是"顺手做的轻 fork"。要做就正视:内核 fork + 信创回炉 + >20 人月 + A≈B。**先用 Phase 0 #0 验掉执行器命门**(几人月),证明能不回堆直供列再投;否则就是在未验证地基上盖楼。
3. **段格式预埋 orientation-ready 钩子**(appendix-O §B.10):Level 0 的折叠语义抽成一份规格 + 一致性测试集,将来无论升 A 还是 B,段格式不重写。
4. **别再用"数据不够/二期/轻 fork"粉饰** —— 这轮已经证明那是回避。要么诚实选 Level 0/0.5 现实终态,要么诚实付 Level 1 的内核 fork 代价。

> 一句话:**我认真设计了 Level 1,而认真的结果是:真正根治膨胀与搜索的 trace 存储引擎必然要 fork openGauss 内核(新 RM + AM + CustomScan 执行器),触发信创回炉,与你们锁定的"v1 不动内核"战略冲突;方案 A 的"轻 fork/SQL不变"被读码证伪、A 与 B 的成本差被高估。这是个真实的二选一:Level 0/0.5(TimescaleDB 级,不动内核,管理三短板)vs Level 1(SmithDB 级,内核 fork+信创回炉,根除三短板)。先做 Phase 0 #0 执行器 PoC 验生死,再拍。** 设计与读码证据全在 appendix-O/P/Q。
