//! yt-manifest —— 单写者 / 多读者下的版本发布、快照 pin、与回收水位。
//!
//! 这是整个引擎的正确性脊梁，直接落地加固设计文档的 §M.4 与草案 2 §D2.1：
//!
//! 1. 读者 pin 协议必须是「先登记、再解引用、最后校验」(announce-before-deref-then-validate)。
//!    初稿写反了（先解引用后登记），被红队用 use-after-free 打穿。次序见 `pin_snapshot`。
//! 2. 回收水位 `safe_version = 所有活跃读者 pinned_version 的最小值`；对「已登记但未落定
//!    (Tentative)」的读者要保守保护，否则就是被打穿的那个残窗。
//! 3. 快照释放走 RAII（`Drop`），保证「注销 slot」与「释放 manifest 引用」严格同生死（OPEN-5）。
//!
//! 骨架取舍（真实实现该换的）：
//! - 这里用 `RwLock<Arc<Manifest>>` 当「current 原子指针」+ `Mutex<Vec<slot>>` 当读者登记表。
//!   真实实现换 `arc-swap`（无锁原子换指针）+ `crossbeam-epoch`（无锁纪元回收）。
//!   但「先登记后解引用」的**次序**在这里是忠实的——次序才是正确性，锁实现只是性能。
//! - Tentative slot 的处理：用 `observed_min_version` 设精确回收下限（登记时的 current 版本）。
//!   它绝不会 pin 到比这更老的版本，所以下限安全，且避免"有未落定读者就完全不回收"的堆积问题。
#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use yt_core::ids::{SegmentId, WalLsn};
use yt_core::manifest::Manifest;

/// 读者登记表里的一个槽位。
struct ReaderSlot {
    /// 登记时观测到的全局 epoch（用于保护 Tentative 残窗）。
    observed_epoch: AtomicU64,
    /// pin 的版本号；`PINNED_TENTATIVE` 表示「已登记、版本尚未落定」。
    pinned_version: AtomicU64,
    /// **登记那一刻的 current 版本**（在 (a) 读 epoch 之后、(c) 解引用 current 之前读到）。
    /// Tentative slot（pinned_version 还没落定）用它当回收下限——它绝不会 pin 到比这更老的版本
    /// （slot 已公开，写者 commit 得先更新 current 才推进 epoch，读者 (c) 读在公开之后）。
    /// 这是精确下限，替代旧的「有 Tentative 就返回 0」保守做法（避免 dead_set 无限堆积）。
    observed_min_version: AtomicU64,
    /// 本读者 MemTable 读取的下界保留点（= pin 版本的 memtable_watermark）。
    /// MemTable 物理 evict 受所有活跃读者此值的最小值 gate（草案 2 §D2.3，堵 flush-evict 漏行）。
    retained_watermark: AtomicU64,
    /// false = 已注销（释放或回滚），回收线程跳过。
    active: AtomicBool,
}

const PINNED_TENTATIVE: u64 = u64::MAX;

/// 跨版本唯一的可变态：current 指针 + 全局 epoch + 读者登记表 + 已提交 WAL 尾。
pub struct Current {
    /// 「current 原子指针」。真实实现换 arc-swap。
    inner: RwLock<Arc<Manifest>>,
    /// 每次 commit 后 +1。
    global_epoch: AtomicU64,
    /// 活跃读者登记表。
    readers: Mutex<Vec<Arc<ReaderSlot>>>,
    /// 已 ack 的最大 WAL LSN（WAL 层推进，读者 pin 时取作 live_lsn 上界）。
    committed_tail: AtomicU64,
}

impl Current {
    pub fn new(initial: Manifest) -> Arc<Self> {
        let epoch = initial.epoch;
        Arc::new(Self {
            inner: RwLock::new(Arc::new(initial)),
            global_epoch: AtomicU64::new(epoch),
            readers: Mutex::new(Vec::new()),
            committed_tail: AtomicU64::new(0),
        })
    }

    /// 当前版本号（无读者时回收水位的上界）。
    pub fn version(&self) -> u64 {
        self.inner.read().unwrap().version.get()
    }

    /// WAL 层在 ack 后调用，推进已提交尾。
    pub fn advance_committed_tail(&self, lsn: WalLsn) {
        self.committed_tail.fetch_max(lsn.get(), Ordering::AcqRel);
    }

    /// 写者提交一个新 manifest 版本：原子换指针 + epoch +1。
    /// 调用方（WriteCoordinator）保证单写者串行；这里不做并发写保护之外的事。
    pub fn commit(&self, new_manifest: Manifest) {
        debug_assert_eq!(
            new_manifest.version.get(),
            self.version() + 1,
            "manifest 版本号必须严格 +1（单写者串行保证）"
        );
        {
            let mut g = self.inner.write().unwrap();
            *g = Arc::new(new_manifest);
        }
        self.global_epoch.fetch_add(1, Ordering::AcqRel);
    }

    /// 读取当前 manifest 的写时复制草稿（写者用来在其上改段集合后 `commit`）。
    pub fn cow_next(&self) -> Manifest {
        let cur = self.inner.read().unwrap();
        let next_epoch = self.global_epoch.load(Ordering::Acquire) + 1;
        cur.cow_next(next_epoch)
    }

    /// ★ 快照 pin：announce-before-deref-then-validate（草案 2 §D2.1）。
    ///
    /// 次序就是正确性本身：必须让「公开 slot」happens-before「解引用 current」。
    /// 这样任何回收线程要么已看见该 slot（水位被压低、资源受保护），
    /// 要么没看见、但读者随后的 `current` 读必然观测到 commit 后的新值 → 校验失败重试。
    pub fn pin_snapshot(self: &Arc<Self>) -> Snapshot {
        loop {
            // (a) 先读 epoch
            let local_epoch = self.global_epoch.load(Ordering::Acquire);
            // (b) 先公开 slot（store-release：经 Mutex 发布，严格先于下面的解引用）
            //     observed_min_version 记登记那一刻的 current 版本（此锁内读，和 slot 发布原子）。
            let cur_version_at_register = self.version();
            let slot = Arc::new(ReaderSlot {
                observed_epoch: AtomicU64::new(local_epoch),
                pinned_version: AtomicU64::new(PINNED_TENTATIVE),
                observed_min_version: AtomicU64::new(cur_version_at_register),
                retained_watermark: AtomicU64::new(0),
                active: AtomicBool::new(true),
            });
            self.readers.lock().unwrap().push(slot.clone());

            // (c) 公开 slot 之后才解引用 current
            let m = self.inner.read().unwrap().clone();
            // (d) 落定 pin 版本 + 下界保留点
            slot.pinned_version.store(m.version.get(), Ordering::Release);
            slot.retained_watermark
                .store(m.memtable_watermark.get(), Ordering::Release);

            // (f) 双重校验：指针未变 且 epoch 未变
            let same_ptr = Arc::ptr_eq(&m, &*self.inner.read().unwrap());
            let same_epoch = self.global_epoch.load(Ordering::Acquire) == local_epoch;
            if same_ptr && same_epoch {
                let live_lsn = WalLsn::new(self.committed_tail.load(Ordering::Acquire));
                return Snapshot {
                    snapshot_id: m.version.get(),
                    retained_watermark: m.memtable_watermark,
                    live_lsn,
                    manifest: m,
                    slot,
                    current: Arc::clone(self),
                };
            }
            // 校验失败：注销该 slot，重试（drop(m) 自动释放 Arc 引用）
            slot.active.store(false, Ordering::Release);
            self.prune_inactive();
        }
    }

    /// 回收水位：所有活跃读者 pinned_version 的最小值；无读者时 = current 版本。
    ///
    /// **Tentative slot（已登记、版本未落定）用 `observed_min_version` 当精确下限**——
    /// 它绝不会 pin 到比"登记那一刻 current 版本"更老的版本（slot 已公开，写者 commit 得先更新
    /// current 才推进 epoch，读者 (c) 读在公开之后）。这替代旧的"有 Tentative 就返回 0"保守做法，
    /// 避免高并发读时 dead_set 无限堆积。正确性仍守住：Tentative 读者的实际 pin 版本要么 == 它
    /// (c) 读到的（≥ observed_min_version），要么校验失败重试看到更新版本——都 ≥ observed_min_version。
    pub fn safe_version(&self) -> u64 {
        let readers = self.readers.lock().unwrap();
        let mut min_v = self.version(); // 无读者时 = current
        for s in readers.iter() {
            if !s.active.load(Ordering::Acquire) {
                continue;
            }
            let pv = s.pinned_version.load(Ordering::Acquire);
            let contribution = if pv == PINNED_TENTATIVE {
                // 未落定：用登记时的下限（绝不会比它 pin 到的更老）。
                s.observed_min_version.load(Ordering::Acquire)
            } else {
                pv
            };
            if contribution < min_v {
                min_v = contribution;
            }
        }
        min_v
    }

    /// MemTable 物理 evict 的 gate（草案 2 §D2.3）：所有活跃读者下界保留点的最小值；
    /// 无读者时 = current 的 memtable_watermark（可回收一切已吸收行）。
    /// 只有 LSN ≤ 本值的 MemTable 行才允许物理回收 —— 保证任一活跃读者的
    /// `(其 retained_watermark, live_lsn]` 区间所需行恒在内存（堵 flush-evict 漏行）。
    pub fn min_retained_watermark(&self) -> u64 {
        let readers = self.readers.lock().unwrap();
        let mut min_w = self.inner.read().unwrap().memtable_watermark.get();
        for s in readers.iter() {
            if !s.active.load(Ordering::Acquire) {
                continue;
            }
            if s.pinned_version.load(Ordering::Acquire) == PINNED_TENTATIVE {
                // 保守：有未落定读者，不回收任何 MemTable 行。
                return 0;
            }
            let w = s.retained_watermark.load(Ordering::Acquire);
            if w < min_w {
                min_w = w;
            }
        }
        min_w
    }

    /// 当前 manifest 的 memtable_watermark（已吸收进段的最大 LSN）。
    pub fn memtable_watermark(&self) -> u64 {
        self.inner.read().unwrap().memtable_watermark.get()
    }

    /// 取当前 manifest（克隆 Arc）。写者在 write_lock 下用它读段的当前 deletion_seq/upgrade_seq
    /// 做 compaction 提交期的重读合并（OPEN-3）。
    pub fn manifest(&self) -> Arc<Manifest> {
        self.inner.read().unwrap().clone()
    }

    /// 某段是否还被当前 manifest 引用。GC 安全条件 (3) 的骨架代理：
    /// 真实实现应查持久化 metastore「该段不被任何已提交 manifest 引用」，
    /// 这里用当前内存 manifest 近似（dead_set 里的段已从当前版本移除，故恒为 false）。
    pub fn contains_segment(&self, seg: SegmentId) -> bool {
        self.inner.read().unwrap().segments.contains_key(&seg.get())
    }

    /// GC 安全条件（草案 1 §D1.4 / §M.4）：三条同真才可物理删除一个 dead 资源。
    ///   (1) v_dead ≤ safe_version
    ///   (2) ∧ 无未释放的 buffer pin（字节级最后保险）
    ///   (3) ∧ metastore 中不被任何已提交 manifest 引用（防崩溃竞态）
    pub fn can_reclaim(&self, v_dead: u64, no_buffer_pin: bool, not_referenced_in_metastore: bool) -> bool {
        v_dead <= self.safe_version() && no_buffer_pin && not_referenced_in_metastore
    }

    /// 清掉已注销的 slot（释放/回滚后）。真实实现由纪元回收顺带完成。
    fn prune_inactive(&self) {
        let mut readers = self.readers.lock().unwrap();
        readers.retain(|s| s.active.load(Ordering::Acquire));
    }

    /// 当前活跃读者数（测试/可观测用）。
    pub fn active_reader_count(&self) -> usize {
        self.readers
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.active.load(Ordering::Acquire))
            .count()
    }
}

/// 一次查询期间钉住的单一一致快照。
///
/// 持有 `Arc<Manifest>`（钉住段/deletion/upgrade 三源的不可变值）+ 上界 live_lsn + 下界
/// retained_watermark（MemTable 双水位，草案 2 §D2.3）。`Drop` 时注销 slot —— 注销与
/// 释放 manifest 引用严格同生死（OPEN-5：杜绝「元数据还指着段、段文件已删」）。
pub struct Snapshot {
    pub snapshot_id: u64,
    pub manifest: Arc<Manifest>,
    /// MemTable 读取的下界保留点（堵 flush-evict 漏行）。
    pub retained_watermark: WalLsn,
    /// MemTable 读取的上界（pin 瞬间的已提交尾）。
    pub live_lsn: WalLsn,
    slot: Arc<ReaderSlot>,
    current: Arc<Current>,
}

impl Snapshot {
    /// MemTable 源读取的半开区间 `(retained_watermark, live_lsn]`（与段源不重叠）。
    pub fn memtable_lsn_range(&self) -> (u64, u64) {
        (self.retained_watermark.get(), self.live_lsn.get())
    }
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        // RAII 注销：必须与下面 manifest Arc 的释放同生死。
        self.slot.active.store(false, Ordering::Release);
        // self.manifest (Arc) 在此之后自动 drop → refcount-1。
        self.current.prune_inactive();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yt_core::manifest::Manifest;

    #[test]
    fn no_reader_safe_version_equals_current() {
        let c = Current::new(Manifest::empty());
        assert_eq!(c.safe_version(), 0);
    }

    #[test]
    fn pinned_reader_holds_back_safe_version() {
        let c = Current::new(Manifest::empty());
        // 推进若干版本
        for _ in 0..3 {
            let m = c.cow_next();
            c.commit(m);
        }
        assert_eq!(c.version(), 3);

        let snap = c.pin_snapshot(); // pin 在 v3
        assert_eq!(snap.snapshot_id, 3);

        // 再推进到 v5
        for _ in 0..2 {
            let m = c.cow_next();
            c.commit(m);
        }
        assert_eq!(c.version(), 5);
        // 读者把水位钉在 3，v4/v5 的 dead 资源不可回收
        assert_eq!(c.safe_version(), 3);
        assert!(!c.can_reclaim(4, true, true));
        assert!(c.can_reclaim(3, true, true));

        // 释放后水位前移到 current
        drop(snap);
        assert_eq!(c.active_reader_count(), 0);
        assert_eq!(c.safe_version(), 5);
        assert!(c.can_reclaim(4, true, true));
    }

    #[test]
    fn safe_version_is_min_over_readers() {
        let c = Current::new(Manifest::empty());
        let s_old = c.pin_snapshot(); // v0
        let m = c.cow_next();
        c.commit(m); // v1
        let s_new = c.pin_snapshot(); // v1
        assert_eq!(c.safe_version(), 0); // 取最老读者（s_old pin 在 v0）
        drop(s_old);
        assert_eq!(c.safe_version(), 1);
        drop(s_new);
        assert_eq!(c.safe_version(), 1); // 无读者 = current
    }

    /// §1.2 生产就绪路线：Tentative 读者用 observed_min_version 当精确下限，
    /// 不再"有未落定读者就完全不回收（返回 0）"。这避免高并发读时 dead_set 无限堆积。
    ///
    /// 场景：current 在 v3，手动构造一个 Tentative slot（observed_min_version=v3），
    /// 验证 safe_version = 3（不是 0），即 v≤3 的 dead 资源**可回收**——
    /// 旧代码这里返回 0、什么都不让回收。
    #[test]
    fn tentative_reader_uses_observed_min_version_not_zero() {
        let c = Current::new(Manifest::empty());
        // 推到 v3
        for _ in 0..3 {
            let m = c.cow_next();
            c.commit(m);
        }
        assert_eq!(c.version(), 3);

        // 手动塞一个 Tentative slot（模拟"已登记、版本未落定"的中间态），
        // observed_min_version = 登记时 current = 3。
        let tentative = Arc::new(ReaderSlot {
            observed_epoch: AtomicU64::new(c.global_epoch.load(Ordering::Acquire)),
            pinned_version: AtomicU64::new(PINNED_TENTATIVE),
            observed_min_version: AtomicU64::new(3),
            retained_watermark: AtomicU64::new(0),
            active: AtomicBool::new(true),
        });
        c.readers.lock().unwrap().push(tentative.clone());

        // ★ 精确下限：safe_version = 3（Tentative 贡献 observed_min_version），不是 0。
        assert_eq!(c.safe_version(), 3, "Tentative 读者用 observed_min_version 当下限，不卡到 0");
        // v≤3 的 dead 资源可回收（旧代码这里 can_reclaim 全 false）
        assert!(c.can_reclaim(3, true, true), "v3 可回收（≤ Tentative 的 observed_min_version）");
        // v>3 的不可回收
        assert!(!c.can_reclaim(4, true, true));

        // 落定后（pin 到 v3）行为不变
        tentative.pinned_version.store(3, Ordering::Release);
        assert_eq!(c.safe_version(), 3);
    }
}
