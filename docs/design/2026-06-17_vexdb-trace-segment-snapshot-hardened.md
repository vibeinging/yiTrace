# yiTrace 承重设计草案（加固定稿）：段生命周期 + 跨四源单一快照

> **本文性质**：这是进入 40–60 人月实现前的**承重设计草案**。它的目的是把并发/崩溃/回收的不变量钉到可证伪的精度，**让 Phase 0 PoC 去证伪它、而不是从零重新发明**。凡标注 `[OPEN]` 的点都是已知未闭合的风险，PoC 必须正面打它们；不准用“应该没问题 / 一般来说”这类乐观措辞糊过去。
>
> **栈定位**：Rust + DataFusion（查询执行）+ Vortex 不可变段（仅 layouts/zone-map/统计；删除/manifest/版本/MVCC 全部 out-of-scope，自研）+ graph_index 向量索引（FFI 复用算法、存储重写）+ BM25 中文倒排（FFI 复用评分、倒排重写）+ 自研 MergeOnReadExec + 自研 WAL + 单写者 WriteCoordinator + 嵌入式 KV manifest（RocksDB/SQLite，Spice.ai 蓝本）+ per-段 RoaringBitmap deletion vector + upgrade 旁路列。
>
> **规模**：单机，<1 亿 span/天（均值 ~1157 span/s），金融政企私有化，“审计可复现”零容忍。
>
> **本次定稿相对初稿的实质改动**（4 个 critical/high 红队漏洞全部正面修掉，详见 §7）：
> 1. pin 协议从 “deref-then-announce” 改为教科书 EBR “**announce-before-deref-then-validate**”，关闭 GC use-after-free 窗口。
> 2. MemTable 升级为**带下界保留水位**的不可变 ring，纳入与段文件同构的 epoch 回收，关闭 flush-evict 漏行窗口。
> 3. **upgrade 旁路列拉到与 deletion_vec 完全对称的保护等级**（per-version `Arc` 不可变块 + 进 dead_set 水位 + 三源原子同提交），关闭串版本 / use-after-free / 漏 upgrade。
> 4. **event_id / seq 锁死为纯确定性身份**（删除全栈雪花定义、seq 必须客户端给且原样持久化、WAL 单批 CRC+commit-marker），关闭 replay 漂移导致的重复折叠与已 ack 丢失。

---

# 第 0 部分：加固后的共享底座

> 两份草案都建在本节。统一词汇，确保 snapshot_id / 版本号 / 锁 / GC 自洽。Lance MVCC 已否决。单写者 WriteCoordinator。

## M.1 三类不可变标识（全单调递增，单写者分配，无并发分配竞争）
- `segment_id : u64` —— 物理段身份，全局唯一、**永不复用**（GC 后也不复用，避免 ABA）。**upgrade 旁路列块与 deletion_vec 块同样分配永不复用的 `chunk_id`**（新增，见 M.2）。
- `manifest_version : u64`（= `V`）—— 每次 commit 产出一个**全新不可变** manifest 快照，严格 +1。
- `snapshot_id : u64` —— 读者 pin 的对外句柄。当前**约定 `snapshot_id == manifest_version`**（一对一，简化心智）。`[OPEN-1]` 该一对一是否限制时间旅行 / 隔离级别 / 长读者租约，见 §6。

## M.2 Manifest = 一个不可变值（copy-on-write，整体原子发布）
```rust
struct Manifest {                 // 不可变；Arc 共享
    version: u64,                          // = snapshot_id
    segments: Map<SegmentId, SegmentEntry>,// 只含本版本“可见”的段
    memtable_watermark: WalLsn,            // 本版本封口的 WAL 提交点（§M.6）
    epoch: u64,                            // 发布本版本时的全局 epoch 值
}
struct SegmentEntry {             // 不可变；版本间靠 Arc 结构共享
    segment_id: u64,
    level: L0|L1|L2,
    state: Live | Compacting,             // manifest 里只可能出现这两态
    time_range: (min_ts, max_ts), zone_stats: ...,        // 剪枝用
    deletion_vec: Arc<RoaringBitmap>,     // per-段删除位图（不可变值，版本间共享）
    deletion_seq: u64,                    // 该段删除位图版本（每次 delete 提交 +1）
    upgrade_ref: Option<Arc<UpgradeColChunk>>, // ★改：与 deletion_vec 完全对称的不可变块
    upgrade_seq: u64,                     // ★新增：该段 upgrade 块版本（每次 upgrade 提交 +1）
}
```
**关键性质**：manifest 是值，不是可变结构。`commit = 在旧 manifest 上 copy-on-write 生成新 Manifest{version+1}，再用一次原子指针交换`把 `current` 从 `Arc<V>` 切到 `Arc<V+1>`。旧 `Arc<V>` 不被释放，直到最后一个 pin 它的读者 unpin（§M.4）。

**★对称化改动（堵 tombstone-upgrade-order）**：`deletion_vec` 与 `upgrade_ref` 现在**结构完全同构** —— 都是 `Arc<不可变块>` + 单调 `_seq`。delete/upgrade/flush/compaction 全部表现为“新版本里某些 `SegmentEntry` 指向新的不可变块 / 新增段 / state 翻转”，**绝不原地改旧版本的任何字节，绝不向旧的 deletion/upgrade 物理文件 append/重排**。新补写一律生成**新 chunk_id 的新不可变块**。

## M.3 单一事务性 metastore（Spice.ai 蓝本，裂脑根源的物理消除）
- manifest 全量 + 段元数据 + deletion 元数据 + **upgrade 块元数据** 落**单一嵌入式 KV（RocksDB/SQLite）**，**一次 commit = 一条原子 metastore 事务**。
- 段数据文件、deletion 块、**upgrade 块**先各自写完、fsync、得到稳定 id，**之后**才在一条 metastore 事务里发布新 manifest_version。
- **三源原子同提交（★）**：任一 commit 若同时改动段集合 / deletion_vec / upgrade_ref，这三者必须在**同一条** metastore 事务里一并翻版 → 读者要么看到全新三元组、要么看到全旧三元组，无交叉。
- 原子性判定式：`重启后可见版本 = metastore 里已提交的最大 manifest_version`。文件存在但未被任何已提交 manifest 引用 = **孤儿**，重启走 §M.7 清理。**无中间态**。

## M.4 epoch-based reclamation（1 写者 vs N 读者的回收核心）
全局 `AtomicU64 GLOBAL_EPOCH` + 一张读者活跃登记表（per-reader slot：`{observed_epoch, pinned_version, retained_watermark}`）。

**★pin 次序更正（堵 gc-use-after-free，详见 §7-A 与 D2.1）**：必须 **announce-before-deref-then-validate**。任何 reclaimer 计算水位时，对“已 announce 但 `pinned_version` 仍为 `TENTATIVE`”的 slot，**按其 `observed_epoch` 设下 epoch 下限**，禁止回收 `epoch ≤ slot.observed_epoch` 的任何 dead 资源。

- 读者 unpin：drop 各 `Arc`（refcount-1）+ `deregister(slot)`。**deregister 与 drop 必须 RAII 绑定**（同一守卫的 Drop 内完成，杜绝异常路径下 slot 先于 Arc 释放）。`[OPEN-5]` PoC 须 fault-inject 异常提前 deregister，验证不出现“manifest 还引用段、段文件已删”。
- 写者每次 commit 后 `GLOBAL_EPOCH += 1`。
- **GC 安全条件（精确判定式，三类资源共用：段文件 / deletion 块 / upgrade 块）**：资源 `R` 在版本 `V_dead` 起变 dead（不再被任一 ≥ V_dead 的 manifest 引用），其物理删除当且仅当：
  ```
  (1) V_dead ≤ safe_version
      其中 safe_version = min over active readers of pinned_version
                          （含 TENTATIVE slot 的 epoch 下限保护；无读者时 = current.version）
  (2) ∧ R 无未 release 的 buffer pin               // 字节级最后保险
  (3) ∧ metastore 中 R 不被任何已提交 manifest 引用 // 防崩溃竞态
  ```
  `safe_version` **单调不减**（读者只会 pin ≥ 自己启动时 current 的版本）→ 水位只前进、无回退、无活锁。

## M.5 三方协作的唯一可变态
- 跨版本：只有 `current: Atomic<Arc<Manifest>>` 一个可变点。单写者写、N 读者读，发布用 release-store / acquire-load 保证可见性（CAS 不需要，因单写者）。
- 段内字节：Vortex 段经独立 buffer 池读取，`vec_read_buffer` 返回带 pin 的 buffer，用完 `release()`（复用本仓 `vector_smgr` pin/release）。**deletion 块与 upgrade 块的物理读取走同一 pin/release 语义**。任何物理删除前提是 §M.4 epoch 水位 **且** 该资源无未 release 的 buffer pin。

## M.6 两层事务接缝（WAL ack vs manifest commit）
- 第 1 层 **WAL commit**：事件写入 → group commit → **per-batch CRC + commit-marker fsync** → 回 ack（★强化，见 §7-D 洞C）。ack 只保证“事件持久、可重放”，**不保证已进段、不保证可见于某 manifest 版本**。
- 第 2 层 **manifest commit**：flush 把一批已 ack 事件封段并发布新 version；compaction/delete/upgrade 同样各自是一次 manifest commit。
- 接缝定义：`Manifest{V}.memtable_watermark = 该版本已“吸收进段”的最大 WAL LSN`。**可见行 = (该版本段集合折叠出的行) ∪ (MemTable 中 `watermark < LSN ≤ 读者 live_lsn` 的已 ack 行)**。
- 崩溃 replay：`replay_from = 现存最大已提交 manifest 的 memtable_watermark`，重放其后的 WAL 进 MemTable。

## M.7 幂等与崩溃不变量（★锁死“确定性”，堵 commit-crash-atomicity）
- **去重键 = 确定性 `event_id = hash(ext_span_id, seq, event_type)`，全栈唯一定义。删除 appendix-O / ingestion doc 里所有“雪花 event_id / snowflake_or_hash / _version_ts 排序”写法。** 雪花 / LSN **只能做归并 tiebreak，绝不进去重键、绝不进排序主键**。
- **`seq`（上报序）规则收口**：
  - seq 必须由客户端给出，并作为**不可变字段原样持久化进 WAL record**；replay 只能从 WAL 原值恢复，**严禁引擎重补**。
  - 客户端确实未给 seq 的事件：用**稳定派生量** `seq = hash(ts_normalized, payload_canonical)`，**绝不用易变的 ingest 到达序**。保证同一物理事件无论何时 replay 都得同一 seq → 同一 event_id。
- **event_id 的 hash 输入字段（`ext_span_id, seq, event_type`）必须是“不可被 upgrade 补写覆盖”的 immutable 身份字段**。upgrade 只允许补写**非身份属性**（status/duration/attrs）。身份字段一旦定版进 event_id 即冻结 → 同一逻辑事件跨段 / 跨源（段 vs MemTable）恒算出同一 event_id，union 去重真幂等。
- kill -9 不变量：重启后 `current = 最大已提交 version`；孤儿 = “文件存在 ∧ 不在任何已提交 manifest”，判定后物理删；悬空 tombstone 不可能（deletion_vec / upgrade_ref 是 manifest 内嵌值，随版本原子发布）。WAL 尾批若无完整 commit-marker → 该批未 ack，丢弃合法。

> **词汇锚定（两草案统一使用）**：`segment_id / chunk_id / manifest_version=V / snapshot_id(=V) / GLOBAL_EPOCH / observed_epoch / pinned_version / TENTATIVE / safe_version=min pinned / retained_watermark / deletion_seq / upgrade_seq / memtable_watermark / live_lsn / current 原子指针`。

---

# 草案 1：段生命周期锁状态机协议（≤2 页）

本文用共享底座全部词汇。

## D1.1 “锁”的本质（利用单写者）
段五态 `building → sealed → live → compacting → dead`。状态转移**只**由 WriteCoordinator 串行驱动 → **没有状态字段的并发写锁**，状态转移 = 发布一个新 manifest 版本。读者从不改状态，只 pin。真正需要的不是互斥锁，而是三层叠加保证：
- **可见性发布**：`current` 原子指针 release-store（写者）/ acquire-load（读者）。
- **存活保证**：Arc refcount（manifest 对象 + 各 `Arc<块>` 级）+ epoch 水位（物理文件级）+ buffer pin（字节级）。

## D1.2 状态转移表
| From | To | 触发（单写者动作） | manifest 体现 | 锁/refcount/可见性规则 |
|---|---|---|---|---|
| ∅ | building | flush 启动 / compaction 选中输出段 | **不进 manifest** | 段文件正写；读者不可见。无锁。 |
| building | sealed | 字节写完 + fsync + footer 校验 + 稳定 segment_id | **仍不进 manifest** | 持久但未发布；不可见。崩溃则孤儿（§M.7）。 |
| sealed | live | metastore **一条原子事务**发布 `Manifest{V+1}`（含该 entry，state=Live，**且段 / deletion_vec / upgrade_ref 三源同事务就绪**）+ `current` 切 V+1 + `GLOBAL_EPOCH+=1` | 进 V+1 | 切指针**之后**对 pin≥V+1 可见；pin≤V 永不可见此段。→ **原子多源可见**。 |
| live | compacting | compaction 选中该段为输入 | V+1 里该 entry `state: Live→Compacting`（仅标记，内容/文件不动） | compacting 段对读**仍完全可见、仍按旧内容读**。compaction 重写产出**新 segment_id 的新文件**，绝不原地改输入段字节。 |
| compacting | dead | compaction 提交：发布 `Manifest{V+2}`，新段 Live、旧输入段从 segments 移除 | 旧段在 V+2 不再出现；在 ≤V+1 仍出现 | 旧段成 dead：对 pin≥V+2 不可见，对 pin≤V+1 **仍可见且必须可读**。**禁止物理删**，直到 §M.4 水位放行。 |
| dead | （物理删除） | reclaimer 后台扫描 | 不在任何版本 | 见 D1.4。删段文件 + **同步删其 dead 的 deletion/upgrade 块** + 回收 epoch slot + metastore 删 entry。 |

> delete / upgrade **不改段状态**：它们是 live 段在新版本里换一个 `Arc<RoaringBitmap>`（deletion_seq+1）/ 换一个 `Arc<UpgradeColChunk>`（upgrade_seq+1）。同一 segment_id 在不同版本可绑不同 deletion/upgrade 块 —— 旧版本读者看旧块、新版本读者看新块、互不串。

## D1.3 compaction 全程（无读写互斥）+ ★delete/upgrade 合并规则
1. 选输入段集 `{Sᵢ}` → 发布 V+1 标 Compacting（轻量，读者无感）。**记录选段瞬间各 Sᵢ 的 `(deletion_seq, upgrade_seq)`**。
2. 后台读 `{Sᵢ}` 旧字节（buffer pin/release）+ 应用其 deletion_vec + upgrade 块 → 写出新段 `Sₙ`（building→sealed，新 segment_id、新文件）。**期间前台读照常读 `{Sᵢ}`**。
3. **★提交前重读合并（堵 `[OPEN-3]` 的丢删除/漏 upgrade）**：在 metastore 提交事务**内**，重读 `{Sᵢ}` 当前最新 `(deletion_seq, upgrade_seq)`。若大于步 1 记录值（说明 compaction 期间有并发 delete/upgrade 命中输入段）→ **必须把这些后到的 delete/upgrade 合并进 Sₙ 后才允许提交**（否则丢删除 / 串旧 upgrade）。单写者串行化保证 delete/upgrade 提交与 compaction 提交全序，本步是这条全序的显式 reconcile 点。`[OPEN-3]` PoC：compaction 进行中高频 delete/upgrade 打同一输入段，验证提交后无丢删、无旧 upgrade 残留。
4. 一条原子 metastore 事务：加 `Sₙ`(Live) + 移除 `{Sᵢ}` + watermark 不变（compaction 不动 WAL）→ `current=V+2` → `epoch+=1`。
5. `{Sᵢ}` 及其 dead 掉的 deletion/upgrade 块登记进 `dead_set{V_dead=V+2}`。**不在此刻删任何文件。**

## D1.4 GC 安全条件（精确判定式）
对 `dead_set` 里**每一个资源 R**（段文件 / 被弃 deletion 块 / 被弃 upgrade 块）于 `V_dead` 变 dead，**可物理删 ⟺** 同时满足 M.4 (1)(2)(3)。

```
reclaimer 周期:
  safe_version = compute_min_pinned()     // 含 TENTATIVE slot 的 epoch 下限保护
  for R in dead_set:
      if V_dead(R) ≤ safe_version ∧ no_buffer_pin(R) ∧ not_referenced_in_metastore(R):
          unlink_file(R); metastore.del(R)
```
`safe_version` 单调不减 → 水位只前进。

## D1.5 验收不变量映射（草案 1）
- “并发读仍看旧段一致版本”：读者 pin V+1（或更早），其 manifest 里 `{Sᵢ}` 仍 Live、`(deletion_seq, upgrade_seq)` 固定 → 全程读同一份字节 + 同一位图 + 同一 upgrade 块。compaction 产出 V+2 不影响已 pin V+1 的读者（manifest 不可变）。
- “旧段引用全释放前不得 GC”：D1.4 (1) 把“引用释放”精确化为 `safe_version ≥ V_dead`；(2)(3) 补字节级与崩溃级；**dead_set 现在含 deletion/upgrade 块**，三类资源同水位。
- “无读到半写段”：building/sealed 不进 manifest → 物理不可见；只有 fsync+footer 后才发布。compaction 输出走新 segment_id，旧段字节永不被原地改。
- **24h 高并发 + compaction 共跑验收点**：assert `safe_version` 单调；assert 任一 unlink 前 (1)(2)(3) 三条同真；fault-inject 在 D1.3 步 4 各子步 kill -9，重启后 `current ∈ {V+1, V+2}` 二者之一、`{Sᵢ}` 与 `Sₙ` 不同时悬空。

---

# 草案 2：跨四源单一快照读规则（≤2 页）

四源：① 活 MemTable ② 不可变段(Vortex) ③ per-段 deletion bitmap ④ upgrade 旁路列。目标：一次查询在**单一逻辑视图**上跨四源读，并发 flush/compaction/delete/upgrade 不改变本次读结果（**不多、不少、不串版本**）。

## D2.1 snapshot pin / release 协议（★announce-before-deref-then-validate，堵 gc-use-after-free）
```
pin_snapshot():                       // 查询开始，DataFusion 物理计划构建时一次性执行
  loop:
    local_epoch = GLOBAL_EPOCH.load(Acquire)          // (a) 先读 epoch
    slot = register_reader(observed_epoch = local_epoch,
                           pinned_version = TENTATIVE) // (b) store-release 先公开 slot
    m   = current.load(Acquire)                        // (c) 公开 slot 之后才解引用 current
    slot.pinned_version = m.version                     // (d) 落定 pin 版本
    arc = m.clone()                                     // (e) refcount+1，钉住 Manifest 值
    if current.load(Acquire) is m
       && GLOBAL_EPOCH.load(Acquire) == local_epoch:    // (f) 双重 validate
        break
    deregister(slot); drop(arc); retry                  // 否则回滚重试
  // ★MemTable 下界保留：把本读者的保留水位登记进 slot（堵 snapshot-torn-view）
  slot.retained_watermark = m.memtable_watermark
  live_lsn = memtable.committed_tail_at(now)            // 上界（见 D2.3）
  return Snapshot{ manifest: arc, snapshot_id: m.version,
                   retained_watermark: m.memtable_watermark, live_lsn }

release_snapshot(s):                  // RAII：查询树 root 的 Drop 统一调用
  deregister(s.slot); drop(s.manifest)
```
**为什么这个次序对**：让“公开 slot”严格 **happens-before** “解引用 current”。任何 reclaimer 扫描时，要么**已看见该 slot**（safe_version 被压低 / TENTATIVE 的 epoch 下限生效 → 文件受保护），要么没看见、但读者随后的 `current.load` 必然观测到 commit 后的新 `current` → validate (f) 失败 → 重试或改 pin 新版本。grace 窗口被关闭。初稿的 “deref-then-announce” 正好反了，故被打穿。

- **全程同一 `Snapshot`**：`MergeOnReadExec` 及所有 partition / source 算子共享同一 `Arc<Manifest>`、同一 `live_lsn`、同一 `retained_watermark`。
- `[OPEN-6]` **DataFusion 传递粒度**：多 partition / spill / 跨线程 / 取消 / 背压 / 错误提前终止下，必须保证**所有 partition 拿同一 `Snapshot` 句柄**，且**仅在计划真正结束时**才 `release_snapshot`。实现：在 `MergeOnReadExec` 之上挂一个持有 `Snapshot` 的 root 算子，依赖其 Drop 统一 release。PoC 必须验证 cancel / error 路径不提前 release（提前 release → 水位前移而其他 partition 仍在读 → use-after-free）。

## D2.2 MergeOnReadExec 在固定版本上的四源折叠
所有判定**只用 `snapshot.manifest / snapshot.live_lsn / snapshot.retained_watermark`，零处读 `current` / memtable 实时尾**。
```
1. 段裁剪：仅遍历 snapshot.manifest.segments（Live+Compacting 同等对待），
           按 time_range/zone_stats + 谓词剪枝 → 候选段集 {Sⱼ}
2. 每段取行：按 span_id/谓词命中行号集，应用 Sⱼ.deletion_vec
           （snapshot 版本绑定的 Arc<RoaringBitmap>，deletion_seq 固定）→ 跳过已删/被覆盖行
3. upgrade 校正：若 snapshot.manifest 中该段 upgrade_ref=Some(Arc<UpgradeColChunk>)，
           按 (trace_id,span_id) 取 upgrade 块覆盖【非身份属性】（status/duration/attrs）
           ——读的是 snapshot 钉住的那份 Arc 块字节，绝不读新版本块
4. MemTable 源：扫活 memtable，仅取 retained_watermark < LSN ≤ live_lsn 的已 ack 行
5. 四源 k 路归并折叠：归并键=(trace_id, span_id, seq)；去重键=event_id=hash(ext_span_id,seq,event_type)
           折叠语义：status/duration 等 last-non-null-wins；events/links union 去重；end 补全
6. 投影 + 大字段晚物化：仅显式 project 时按 payload_ref 取 CAS
```
**可见性核心规则（哪个版本的 deletion/upgrade/段集合适用）**：
- **deletion bitmap 适用版本 = snapshot.manifest 里该 `SegmentEntry.deletion_vec`**。并发 delete 进 `Manifest{V+1}` 换新块（deletion_seq+1），对 pin 在 V 的读者不可见 → 读进行中并发 delete 不改本次结果。
- **upgrade 块适用版本 = snapshot.manifest 里该 `Arc<UpgradeColChunk>`**（★现在与 deletion 完全对称、per-version 不可变）。并发 upgrade 进新版本换**新 chunk_id 的新块**，本读者读钉住的旧 `Arc` 块字节 → 不串版本、不读半写。
- **段集合适用版本 = snapshot.manifest.segments**。并发 flush 新增段 / compaction 替换段都进新版本 → 本读者既看不到新 flush 段（不多），也不会因 compaction 让旧段消失而漏读（不少，旧段+其 deletion/upgrade 块受草案 1 §D1.4 三源同水位保护仍可读）。

## D2.3 MemTable 源的快照化（★上界 + 下界双保留，堵 snapshot-torn-view）
MemTable 是唯一可变源，必须切出**有上界且有下界保留**的不可变切片。初稿只钉了上界（live_lsn），漏钉下界 → flush evict 时被吸收前缀被读零次。修复：

- **MemTable 改为不可变 ring**：每行携带 `commit_lsn` + 软删墓碑；WriteCoordinator 串行 append，原子发布 `committed_tail`。
- **上界**：`snapshot.live_lsn = pin 时刻 committed_tail`；读 MemTable 只接受 `LSN ≤ live_lsn` 的行。
- **★下界保留（新）**：`snapshot.retained_watermark = pin 时刻钉住版本的 memtable_watermark`；读 MemTable 取**半开区间 `(retained_watermark, live_lsn]`**，**与段源不重叠**。
- **★MemTable 物理 evict 受水位 gate（堵漏行，与 §M.4 段回收同构）**：flush 提交 `V+1`（watermark 推到 `cut`）后，被吸收前缀**只逻辑标记、不立即物理 evict**。
  ```
  retained_watermark_global = min over active readers of slot.retained_watermark
                              （无读者时 = current.memtable_watermark）
  evict 只回收 LSN ≤ retained_watermark_global 的 MemTable 行
  ```
  保证任一活跃读者的 `(其 retained_watermark, live_lsn]` 窗口所需行**恒在内存**。`[OPEN-2]` MemTable 物理回收水位与内存上界的张力（长读者 = 内存无法回收的 head-of-line blocking）须 PoC + 长读者租约策略联动，见 §6。
- watermark 接缝：吸收进段的行 `LSN ≤ watermark` 不再从 MemTable 读 → 同一逻辑事件不会既从段又从 MemTable 进折叠两次；即便重复，确定性 event_id 去重兜底。

## D2.4 与 WAL/commit 两层事务的接缝 + ★replay 幂等收口
- WAL ack（第 1 层）只让事件“可被 MemTable 源在 `LSN ≤ live_lsn` 时看到”，不等于进段。
- manifest commit（第 2 层）让 flush 后的段 + watermark **同一原子事务**进新版本：watermark 推进与段加入同时对新版本生效 → **不存在“既不在 MemTable 又不在段”的空窗**；对仍 pin 旧版本的读者，事件继续从其 `(retained_watermark, live_lsn]` 区间可见（下界保留保证它没被 evict）。
- **★崩溃 replay 一致性（锁死确定性）**：重启 `current = 最大已提交版本`；`replay_from = current.memtable_watermark`，重放其后 WAL 重建 MemTable `(watermark, tail]`。
  - **seq 从 WAL 原值恢复，严禁重补**（§M.7）→ event_id 跨 replay 不漂移。
  - 已 ack 未进段事件不丢（重放回 MemTable）；已进段事件不重复折叠（`LSN ≤ watermark` 不从 MemTable 取 + 确定性 event_id 去重，**双保险均依赖 event_id 跨 replay 不变，现已由 §M.7 保证**）。
  - WAL 尾批无完整 `commit-marker + CRC` → 该批未 ack，丢弃合法 → 不丢已 ack、不重放半截批。

## D2.5 验收不变量映射（草案 2）
- “单一一致快照跨四源”：四源判定全读 `snapshot{manifest, live_lsn, retained_watermark}` 固定值，零处读 `current` / memtable 实时尾。
- “读进行中并发 flush/compaction/delete/upgrade 结果不变”：四类变更只产新版本；旧 `Arc<Manifest>` + 旧 `Arc<deletion>` + 旧 `Arc<upgrade>` 不可变 + MemTable LSN 上下界固定 → 结果 = pin 瞬间逻辑视图。
- “不多 / 不少 / 不串版本”：
  - **不多** = 新版本段 / 更高 LSN 行 / 新 deletion / 新 upgrade 被排除。
  - **不少** = 旧段+其块受 §D1.4 三源同水位保护、deletion 旧位图不提前生效、**MemTable 下界 retained_watermark 保护被吸收前缀不被提前 evict**。
  - **不串** = 全程同一 version + 同一 deletion_seq + 同一 upgrade_seq + 同一 retained_watermark；upgrade 只覆盖非身份属性、event_id 身份字段冻结。
- **审计可复现**：给定 `snapshot_id`(=V) 可重放逐字节相同结果（manifest 不可变 + WAL 可定位到 live_lsn + 折叠确定性 event_id + 下界保留保证 MemTable 区间不被掏空）。金融政企准入项的可验证形式。

---

# 第 7 部分：红队攻击与防线（逐条）

| # | 棱镜 | 攻击场景（一句话） | 靠哪条规则挡住 |
|---|---|---|---|
| A | gc-use-after-free | pin 先 clone manifest 后 register epoch，二者之间被抢占的窗口里 reclaimer 看不到该读者，safe_version 越过 V_dead，unlink 了读者将读的旧段 → 醒来 mmap 已回收 inode。 | **D2.1 改为 announce-before-deref-then-validate**：(a)(b) 先公开 slot 再 (c) 解引用 current，公开严格 happens-before 解引用；(f) 双重 validate（指针 + epoch）失败即重试；**M.4 对 TENTATIVE slot 按 observed_epoch 设 epoch 下限**，堵 (b)→(d) 残窗。reclaimer 要么看见 slot、要么读者必观测到新 current 改 pin。 |
| B | snapshot-torn-view | 长读者 pin 在 V，flush 提交 V+1 物理 evict 被吸收的 MemTable 前缀；这些行在 V 段集合里没有（只在对 V 不可见的新段）、又已从 MemTable 抹除 → 读零次（丢行）。 | **D2.3 给 MemTable 加下界保留**：`snapshot.retained_watermark` 钉住下界，读取半开区间 `(retained_watermark, live_lsn]`；**evict 受 `retained_watermark_global = min over readers` gate**（与 §M.4 段回收同构）→ 任一活跃读者所需 MemTable 行恒在。验收：任一 evict 前断言被回收行 `LSN ≤ 所有活跃读者 retained_watermark`，固定 snapshot_id 在 flush 前后重放逐字节一致。 |
| C | tombstone-upgrade-order | upgrade 旁路列只声明为 `Option<UpgradeColRef>`（偏移），未像 deletion_vec 那样是 per-version 不可变 `Arc` —— 旧读者读到被新版本 append/重排或被 compaction 物理兑现后回收的 upgrade 列 → 串版本 / 读半写 / use-after-free；外加 fold 排序键三处不一致 + ext_span_id 可被 upgrade 覆盖致 event_id 跨段不一致 → 重复折叠。 | **M.2 把 upgrade_ref 改为 `Arc<UpgradeColChunk>` + upgrade_seq，与 deletion_vec 完全对称**（新补写生成新 chunk_id 新块、绝不原地改）；**M.3 三源原子同提交**；**D1.3 步 5 / D1.4 把 upgrade 块纳入 dead_set 同 V_dead 三重水位保护**（compaction 物理兑现后不立即删、等 safe_version≥V_dead）；**M.7 统一冻结 fold 键 = (trace_id,span_id,seq) / event_id=hash(ext_span_id,seq,event_type)，删 _version_ts 与雪花并存，身份字段禁被 upgrade 覆盖**。 |
| D | commit-crash-atomicity | replay 幂等全押在确定性 event_id，但源料三处把 event_id 定义成引擎雪花、seq 定义为“缺省按 ingest 到达序补” → replay 后 seq/event_id 漂移，去重失效，已 ack 事件重复折叠（token/cost 双倍）；外加 group-commit 无 per-batch marker 致已 ack 尾批丢失。 | **M.7 锁死确定性**：event_id 全栈唯一定义为 `hash(ext_span_id, seq, event_type)`，删除所有雪花定义；**seq 必须客户端给、原样持久化进 WAL、replay 严禁重补**，缺省用稳定派生量 `hash(ts_normalized, payload_canonical)` 而非到达序；**M.6 / D2.4 WAL 每批 CRC + commit-marker，ack 仅在 marker fsync 后回出，尾批无 marker 即丢弃（合法，因未 ack）**。验收：D1.3 步 4 各子步与 WAL fsync 前后各注 kill -9，replay 后对同一 (ext_span_id, seq) 折叠结果逐字节一致、token/cost 不翻倍。 |

> 已确认**打不穿**的点（保留为既有防线，不动）：compaction 提交单原子事务（D1.3 步 4 + M.3 + M.7 孤儿判定）保证 kill -9 后 `current ∈ {V+1,V+2}`、`{Sᵢ}/Sₙ` 不同时悬空、无悬空 tombstone。

---

# 第 6 部分：仍 OPEN 的点（留给 Phase 0 PoC 证伪，不准乐观糊）

> 这些是已知未闭合风险。PoC 的任务是**正面构造交错把它们打穿或证明闭合**，而非假定已解决。

- **`[OPEN-1]` snapshot_id == manifest_version 一对一是否够用**：未来若需时间旅行（查已被 GC 的历史 version）/ 多隔离级别 / 长读者租约，一对一会限制；对“被 pin 的历史版本”豁免 GC 会阻塞水位前进 → 段无法回收的 head-of-line blocking。**需定长读者超时 / 快照租约策略**（如 pin 超过 T 强制失效并报错重 pin）。PoC：长读者持续 pin V，验证水位停滞与内存增长曲线，确定租约阈值。
- **`[OPEN-2]` MemTable 物理回收水位 vs 内存上界的张力**：D2.3 的 `retained_watermark_global` gate 防漏行，但长读者会把被吸收前缀长期钉在内存 → 内存无界。需与 `[OPEN-1]` 租约联动给出**回收点 ≥ safe_version 对应 watermark 的物理回收判定式 + 内存压力下的强制快照失效策略**。PoC：长读者 + 高 flush 频率，测内存峰值与 OOM 边界。
- **`[OPEN-3]` compaction 提交事务内对“自选段以来新增 deletion_seq/upgrade_seq”的重读合并正确性**：D1.3 步 3 给了规则（提交事务内重读、若 seq 增长则合并后才提交），但**单写者全序下该 reconcile 是否覆盖所有交错**未证。PoC：compaction 进行中高频 delete/upgrade 打同一输入段，构造提交临界点交错，验证无丢删、无旧 upgrade 残留。
- **`[OPEN-5]` Arc refcount 与 epoch 水位双轨的一致性窗口**：M.4 要求 deregister 与 drop(Arc) RAII 严格同生命周期。需**证明并 fault-inject 异常路径（panic / 提前 deregister）下不出现“manifest 还引用段、段文件已删”**。PoC：在 release 路径注入 panic / 提前 deregister，断言无 use-after-free。
- **`[OPEN-6]` DataFusion 物理计划下 Snapshot 传递与 release 时机**：多 partition / spill / 跨线程 / 取消 / 背压 / 错误提前终止下，必须所有 partition 共享同一 Snapshot 且仅在计划真正结束才 release。PoC：构造 cancel / error / spill 路径，断言 release 不早于最后一个 partition 完成。
- **`[OPEN-7]` deletion_vec / upgrade 块的版本膨胀（Arc 共享 vs COW 成本）**：热段被频繁 touch（eval/feedback 反复 delete/upgrade）会产生大量历史块版本，全被旧读者 pin 住不能释放 → 内存膨胀。需定**块版本数上界 + 与 safe_version 联动的版本回收 + RoaringBitmap/upgrade 块增量 diff（而非整块复制）的可行性**。PoC：热段高频 delete/upgrade + 长读者，测块版本数与内存增长，验证 diff 方案可行性。
- **`[OPEN-8]`（由本次定稿新引入，必须 PoC）live_lsn 与 retained_watermark 取值原子性**：D2.1 中 `m = current.load()`（取 retained_watermark）与 `committed_tail_at(now)`（取 live_lsn）不在同一原子点。若两步之间发生 flush 提交（watermark 与 evict 推进），是否仍能保证 `(retained_watermark, live_lsn]` 区间所需行恒在？本定稿用“evict gate = min over readers retained_watermark”从回收侧堵，但**pin 侧两步非原子是否引入新窗口需 PoC 构造 flush 与 pin 交错验证**；可能要求 `(retained_watermark, live_lsn)` 作为一条原子读出，或规定 MemTable 物理行回收点严格 ≥ safe_version 对应 watermark。

---

# 附：本定稿的“一句话承重摘要”
- **GC 安全 = announce-before-deref EBR + 三类资源（段 / deletion / upgrade）同 V_dead 三重水位（safe_version + buffer pin + metastore 引用）。**
- **快照一致 = 单一 `Arc<Manifest>` 钉死段/deletion/upgrade 三源 + MemTable 上界 live_lsn + 下界 retained_watermark 双钉 + evict 受 min-over-readers gate。**
- **崩溃幂等 = 纯确定性 event_id(ext_span_id, 持久化 seq, event_type) + WAL 单批 CRC/marker + 身份字段冻结不被 upgrade 覆盖。**
- 以上每一条都有对应 `[OPEN]` PoC 去证伪。进 40–60 人月前，先让 PoC 打穿这些点。
