# yiTrace 项目全面 Review

> 日期：2026-06-22
> 范围：产品定位、技术架构、代码实现、竞品叙事,逐层评审。
> 方法：顶层文档(产品说明/技术文档/竞品分析/国产竞品调研/决策摘要)逐篇读;Rust 引擎 5 crate + Vortex 段存储 + Python/TS SDK + PG 扩展逐行读;`cargo test` 跑通 86 个测试(9+69+3+2+3)全绿;release 模式复测招牌召回数字。
> 一句话结论:**工程品味很高的"验证级骨架",技术前提用代码钉得很扎实,但存在一处会让决策层误判的重大文档缺陷——项目路线已从"openGauss 扩展"转向"Rust 自研引擎",两套代码并存且文档没有交代关系。**

---

## 一、总览:这个东西到底是什么

| | |
|---|---|
| **定位** | 单机、单目录、零外部依赖的 AI Agent 可观测性数据库,面向中国金融政企气隙机房 |
| **代码规模** | Rust 引擎 7035 行(5 crate) + Vortex 段存储 505 行 + Python SDK 406 行 + TS SDK 412 行 + PG 扩展 SQL 364 行 + 文档 41 篇约 1.4 万行 |
| **测试** | 86 个测试全绿,大量"会失败的测试"钉死技术前提 |
| **成熟度** | **验证级骨架,不是成品**——文档自己反复强调,这点是诚实的 |

---

## 二、🔴 高优先级问题(影响决策或正确性)

### H1. 路线分叉未交代:openGauss 扩展 vs Rust 自研引擎,两套代码并存

这是最严重的问题,**直接关系到"自有 IP / 自主可控"那条护城河能不能成立**。

- `2026-06-17_决策摘要` 显示**原始路线**是:基于 openGauss/yiTrace 内核做扩展(`tracevault-extension/`,364 行 SQL,自称"数据库本体·产物③"),用内核自带的 DiskANN + BM25 + vex_jieba。
- `2026-06-22 产品说明 + 技术文档` 已经**切换到**:`yitrace-engine/` 全自研 Rust 引擎,中文 BM25 / 图式 ANN 都自己写。
- **但产品说明和技术文档里"openGauss / tracevault / 产物③"一个字都没有**,反过来 tracevault README 也没提 Rust 引擎。两套代码、两套叙事,谁也没说谁。

**为什么是高优**:
1. 决策摘要 §4.1 自己点名了风险:"openGauss 是华为 IP——用华为内核做信创护城河,等于把叙事控制权交给一个能顺手做掉你的竞品"。转向自研正是对这条的回应。**但这个转向的决策逻辑没有沉淀进任何一篇当前文档**,新读者会以为只有一套 Rust 引擎,完全不知道前面还有一条被放弃的路。
2. 产品说明第 50 行说"核心算法是团队自己的,不是调外部库",**这个表述对当前 Rust 引擎成立,对 openGauss 扩展路线不成立**(那是用内核的 DiskANN/jieba)。两条路线的"自有 IP"成色不同,文档必须交代清楚现在押的是哪条。
3. 竞品分析里反复说对手"把存储外包给 ClickHouse",但**自研 Rust 引擎目前的 BM25 是 bigram、ANN 是单层 NSW 无量化,都是文档自承的"验证级"**——离"自有 IP 能压住 DiskANN/jieba"还有距离。这个 gap 在路线分叉不澄清的情况下会被掩盖。

**建议**:在产品说明里加一段"路线选择",明确:(a) openGauss 扩展是第一版方案、为何转向;(b) Rust 引擎是当前承重路线;(c) tracevault SQL 是保留参考还是放弃;(d) "自有 IP"当前的真实成色(算法自研、但精度/性能未对线生产级)。

---

### H2. `reclaim` 的 GC 安全条件 (3) 是近似实现,与文档承诺有偏差

`yt-engine/src/lib.rs:1708` 的 `reclaim`:

```rust
let safe = self.current.safe_version();        // 先拿
let mut dead = self.dead_set.lock().unwrap();  // 后上锁
dead.retain(|r| {
    let ok = r.v_dead <= safe
        && !self.buffer_pins.is_pinned(r.seg)
        && !self.current.contains_segment(r.seg);  // ← 这里
    ...
});
```

文档(`lib.rs:1706` 注释)承诺条件 (3) 是"**不被任何已提交 manifest 引用(metastore)**",但 `contains_segment` 实现是查**当前内存版本**(`yt-manifest/src/lib.rs:201`),代码注释自己也写了"骨架用当前版本近似 metastore"。

**问题**:`safe_version` 和 `dead_set.lock()` 之间没有联合原子性,`contains_segment` 在循环里对每条 dead 段单独读 `RwLock`。在 reclaim 线程判断 `contains_segment(seg)==false` 之后、`unlink_segment` 之前,如果写者正好 commit 了一个**重新引用该段的新版本**(虽然段 id 永不复用,理论上不会发生,但 compaction 失败回滚路径需确认),存在理论残窗。文档明确标注"防崩溃竞态",但当前实现防不住崩溃竞态——**崩溃在 retain 中间,reclaim 已删一半,manifest 没更新**。

并发测试 `concurrent_readers_writer_reclaimer_stay_consistent` 跑 4 读 + 1 写 + 1 回收能过,但**它没构造"写者重新引用 dead 段"的路径**(当前 compaction 只产生新段、不复活旧段),所以测不出这个残窗。

**建议**:(a) 在文档里把"骨架近似"标得更显眼(现在 buried 在注释里);(b) 真实实现必须上持久化 metastore 的 GC 日志(写"即将删 seg X"→ fsync → 删 → 标记完成),这是决策摘要 §3.B"并发正确性回归"点名 risky 的第 4 条同类问题。

---

### H3. "召回 0.50 → 1.00"是 release 模式、固定种子、单点实测,口径要收紧

产品说明第 35 行把这个数字当招牌。实测:
- `cargo test -p yt-engine --release`:`post-filter=0.50  in-graph=1.00 (命中集=67)` ✓
- debug 模式数字会变(LCG 种子固定,但优化级别影响浮点调度)。

**问题**:
1. 这是**单个查询点**(`matching[mid]`)+**单一选择性**(8.3%)+**800 点**的实测。要当"招牌钩子"对外说,需要**多组选择性、多个查询点、多次平均 + 方差**,否则被竞品一句"你 cherry-pick 了一个点"顶回来。决策摘要 §3.B 自己也写了 recall 要测"高选择度 under-fill",但代码里这个 under-fill 场景没有专门测试。
2. `assert!(r_in >= 0.8)` 和 `assert!(r_post <= 0.6)` 的阈值是按当前实测挖的,**换一组分布可能不成立**。这是"会通过的测试"而非"会暴露问题的测试",钉不死结论。

**建议**:把召回测试扩成表驱动(选择性 1%/5%/10%/20% × 多查询点),给出均值+最差,文档里报区间不报单点。这是把"招牌"从"实测一次"升级到"可复现回归"的必经一步。

---

## 三、🟡 中优先级问题(影响上量或一致性)

### M1. 自研引擎 vs 文档承诺的"自有 IP 算法"有落差

技术文档 §10 承认三块要换团队自有件:BM25→jieba+倒排的 C ABI,graph→团队 graph_index 的 C ABI。**但当前 Bm25TextIndex 和 GraphAnnIndex 是这次新写的 Rust 实现**(`bm25.rs` / `graph.rs`),不是 FFI 包装。

这意味着:**产品说的"自有 IP"目前是"自研占位算法",不是"团队积累的生产级索引"**。bigram 分词对"盗刷风控"这类样例召回好,但对真实金融文本(专业术语、英文混排、数字)的召回质量没有基准;NSW 单层图在百万向量下的延迟/召回曲线没有压测。

**这不是 bug,是诚实边界**——文档标了,但产品说明第 50 行"核心算法是团队自己的"会让商务误读成已经生产可用。建议在产品说明里把这条的成色降一档表述。

### M2. `fold_events` 的去重顺序敏感:同 event_id 保留"首次见到",源顺序未规范

`yt-core/src/lib.rs:526`:
```rust
if !seen.insert(eid) { continue; }  // 保留首次见到的
```

inputs 的构造顺序是 MemTable → segments(`lib.rs:1022` 段源在前,1037 MemTable 在后——实际是段先 push)。当同一事件在段和 MemTable 都有(崩溃窗口的重叠),保留的是段里的版本。

**问题**:虽然 `event_id` 相同的记录字段应当一致(确定性),但**字段值可能因 upgrade 补写而不同**——段里的版本可能已经带了 upgrade,MemTable 里的是原始值,或反之。`last-non-null-wins` 是在折叠后对多事件做的,但去重阶段已经丢了一份,留下的那份如果恰好是"没带某个字段"的版本,结果字段为 None。

测试 `crash_recovery_replay_is_idempotent_no_double_fold` 验证了"重叠窗口字段一致",但**没有验证重叠窗口里 upgrade 已经打了的情况**。建议加一个:段已 flush + upgrade 已 commit + 崩溃 → 重放后字段取值确定。

### M3. HTTP 服务无 TLS、鉴权是明文 Bearer、审计打 stderr

`http.rs:209` 审计注释自己写了"骨架打 stderr,真实实现落持久防篡改审计日志并接 SIEM"。产品说明也说"上量换 axum/hyper"。这些都标了。

**但**:产品主打"金融政企气隙",明文 Bearer + 无 TLS + 无持久审计,**当前状态连内网 PoC 都过不了金融的安全评审**。文档把它列在"用户已明确暂缓"里,这是合理的;但竞品分析里 LangSmith 的合规能力被列为"我方软肋 + 签单门槛",国产 APM 老兵的合规也被列为"它们强项"——**合规不是加分项,是入场券,越往后越堵单**(国产竞品调研 §8 自己的结论)。建议把"合规暂缓"的止损点写进路线,而不是无限期搁置。

### M4. OTLP 适配器对大文本字段的映射是猜测性的

`otlp.rs:66-67`:
```rust
let input_text = first_str(attrs, &["input.value", "gen_ai.prompt"]);
let output_text = first_str(attrs, &["output.value", "gen_ai.completion"]);
```

OTLP GenAI 标准里 `gen_ai.prompt` / `gen_ai.completion` **实际是嵌套结构**(message 数组),不是简单 stringValue。当前解析只能取到扁平 string 值,真实 OpenTelemetry GenAI 语义导出的大多数 LLM trace 在这两个字段上会取空。OpenInference 的 `input.value` 是扁平的,能取到。

**影响**:对 GenAI 约定的 trace,eval 最关键的 input/output 文本可能大面积取不到。这是"认两套约定"宣称成立的核心。建议用真实 OTel GenAI SDK 导出的样本(而非手写的测试 JSON)回归一遍。

### M5. Vortex 段存储的 logs 用 NUL 连接、大文本列无压缩

`vortex-segstore-vortex/src/lib.rs:39`:`const LOG_SEP: char = '\u{0}'`,logs 数组压成单列用 NUL 拼。注释承认"v1 简化,后续换 list<utf8>"。

**问题**:用户日志里出现 NUL 的概率确实低,但**金融系统日志可能包含二进制错误码、协议帧**,NUL 不是零概率。一旦出现,load 时 split 会错切。Vortex 本身支持 list 类型,这里偷懒不太值得。另外 `input_text`/`output_text` 这种大文本列,Vortex 的 benefit 主要是"不读的查询跳过",**但读它的查询(eval、检索)还是会全解**——没开压缩(string 列默认不压),大文本场景的存储放大没解决。建议至少把 FastLZ/FSST 列压缩开。

### M6. 文档体系膨胀,41 篇 1.4 万行,决策溯源成本高

`docs/design/` 下有 appendix A-Q 共 17 个附录 + 13 篇主设计 + 4 篇 research。对骨架项目,文档量是代码量的 2 倍。设计 rigor 是优点,但:
- appendix 里大量是"红队 round 1/2/3 + 修订"的**过程产物**,新读者要还原"当前到底定了什么"得穿越多轮。
- 产品说明和技术文档是好的"当前态"入口,但它们**不提 openGauss 路线**(见 H1),反而让 appendix C/D/E(还在讨论 openGauss 的)成为认知地雷。

**建议**:出一篇 `docs/CURRENT_STATE.md` 作为唯一权威现状索引,把 appendix 标"历史过程产物,非当前态"。

---

## 四、🟢 低优先级问题(打磨项)

### L1. `current: Arc<Current>` 在 Snapshot 里强引用,语义上不该
`yt-manifest/src/lib.rs:243` Snapshot 持 `current: Arc<Current>`。引擎是单例,Current 生命周期 == 进程,所以实际不会泄漏,但语义上 Snapshot 不该强引用其所属的 registry。无锁化时换 crossbeam-epoch 要重新设计这块。

### L2. `KeywordScorer` 是产品唯一的 eval scorer,且只判"含坏词=0/否=1000"
`lib.rs:504`。这是"探路闭环"够用,但产品说明把 eval 列为差异化能力之一。接 LLM-judge 是必经一步,且需要出站 HTTP(产品说明第 152 行已承认"需要出站 HTTP client",气隙场景下出站要走本地小模型,又是一层复杂度)。

### L3. Python SDK 的 `BatchExporter.flush` 仍是逐条转 sink,没真正批量
`exporter.py:50-53` 注释自己写了 TODO。`HttpExporter` 是真批量。建议要么把 BatchExporter 标 deprecated,要么补完——目前两个 Exporter 职责重叠。

### L4. TS SDK `HttpExporter.flush` 失败静默(无 catch)
`exporter.ts:71` `await fetch(...)` 无 try/catch,网络错误会 reject 出去,但调用方 `void this.flush()`(`exporter.ts:64`)丢了 Promise,错误吞掉。生产用必须有重试 + 错误上报,否则 trace 静默丢。

### L5. `index_record` 把 logs 喂进 BM25,但产品说的"中文检索"应该索引 input/output_text
`lib.rs:796`:`self.bm25.index_text(..., &r.fields.logs.join(" "))`。注释"真实实现用 span name/input/output"。当前 search 端到端测试用的都是 logs 字段(`["疑似盗刷 已拦截"]`),**真实 SDK 灌进来的 input/output_text 不会被索引**。这是个会让"中文检索"在真实数据上突然失效的坑。要么改成索引 input/output_text,要么在文档显著位置标注当前只索引 logs。

### L6. `safe_version` 对 Tentative 读者直接返回 0(完全不回收)
`yt-manifest/src/lib.rs:154`。注释承认"保守实现,真实按 observed_epoch 设下限"。在高并发读场景,任何瞬间有 pending pin 就**完全不回收任何 dead 段**,dead_set 会堆积。骨架够用,上量必须换精确下限。

### L7. CRC32 是无表逐位实现(`yt-wal/src/lib.rs:391`)
`for _ in 0..8` 内层循环,每个字节 8 次迭代。WAL fsync 前对每批都算一次,大批量写时是热点。换 crc32fast 是无脑 10x。文档标了,列在这里只是提醒优先级。

---

## 五、做得好的地方(值得保持)

1. **确定性 event_id 跨语言逐字节一致**——`yt-core` 的 FNV-1a、Python 的同款常量、TS 的 BigInt 小端,加上 `cargo run --example print_event_id` 给的基准值 3941713543033365492,测试 `ingest_wire_maps_sdk_format_end_to_end` 钉死。这是去重幂等的根,做得很扎实。
2. **"会失败的测试"文化**——BM25 对子串占位失败、in-graph 对 post-filter 失败、torn tail 被 drop、flush-evict 漏行被红队抓。测试不是凑覆盖率,是钉前提。
3. **零依赖声明是真的**——核了 `yt-engine/Cargo.toml`,只有可选 flate2(gzip feature 默认关),`cargo test --no-run` 离线可过。气隙部署的承诺不是吹的。
4. **四源折叠 + 双水位 memtable + announce-before-deref pin 协议**——这套并发设计是整个项目技术含量最高的部分,pin 次序的红队修复(lib.rs:100 注释)是真理解了 use-after-free 的根。次序才是正确性,锁只是性能——这个洞察写在代码注释里,质量很高。
5. **Vortex 段存储刻意建在 workspace 之外**——大依赖隔离在一个 crate,引擎骨架保持零依赖可审计,投影下推端到端测试(`engine_uses_vortex_pushdown_end_to_end`)真跑通。工程边界划得清楚。
6. **诚实边界写在明面**——产品说明第 139 行"当前状态"、技术文档 §12 表格,都明确区分"已验证"和"占位待换"。这在 PPT 普遍过度承诺的语境下是稀缺品质。

---

## 六、给决策层的三句话

1. **技术骨架的成色超出"验证级"的自我定位**——并发/持久化/检索的接口边界和测试都立住了,继续往里填团队自有索引 + Vortex 是顺的,不是推倒重来。
2. **最大的隐患不在代码,在文档**:openGauss 扩展 vs Rust 自研引擎的路线分叉没交代,"自有 IP"的成色会被商务误读。建议一周内出一篇 `CURRENT_STATE.md` 把现状和路线讲清,否则竞品分析里"打博睿"的话术建立在沙地上。
3. **eval 是命门,合规是门槛,这两条不能一直"探路 / 暂缓"**——竞品分析自己得出的结论。建议把"LLM-judge 接入"和"等保/RBAC 止损点"排进有日期的路线,而不是开放式的"待补"。

---

## 附:评审方法与证据

- **文档**:产品说明、技术文档、竞品分析、国产竞品调研、决策摘要 + 41 篇 design/research 逐篇读过。
- **代码**:Rust 引擎 5 crate(core/manifest/wal/memtable/engine)+ Vortex 段存储 + bm25/graph/wire/otlp/http/persist/segstore/vecstore + Python SDK + TS SDK + PG 扩展 SQL 逐行读过。
- **编译**:`cargo test --no-run` 通过,零外部依赖(仅可选 flate2)。
- **测试**:`cargo test` 86 个全绿(9+69+3+2+3)。
- **招牌数字复测**:`cargo test -p yt-engine --release in_graph_filter -- --nocapture` → `post-filter=0.50  in-graph=1.00 (命中集=67)`,与产品说明第 35 行一致。
- **规模统计**:Rust 7035 + Vortex 505 + Python 406 + TS 412 + SQL 364 ≈ 8722 行代码;文档 41 篇约 1.4 万行。

*配套:产品说明 [`docs/2026-06-22_yitrace-产品说明.md`]｜技术文档 [`docs/design/2026-06-22_yitrace-技术文档.md`]｜竞品分析 [`docs/analysis/2026-06-22_竞品分析.md`]｜国产竞品调研 [`docs/research/2026-06-22_国产竞品调研.md`]｜决策摘要 [`docs/2026-06-17_决策摘要与Phase0行动计划.md`]。*
