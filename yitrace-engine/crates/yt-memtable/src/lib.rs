//! yt-memtable —— 活内存表，带上下界双水位（草案 2 §D2.3）。
//!
//! 这是修「flush-evict 漏行」那个 bug 的落点。MemTable 是四源里**唯一可变**的源，
//! 必须切出「有上界、也有下界保留」的不可变切片：
//!
//! - **上界** `live_lsn`：读者 pin 瞬间的已提交尾，读 MemTable 只接受 `LSN ≤ live_lsn` 的行。
//! - **下界保留** `retained_watermark`：读者 pin 版本的 memtable_watermark，读半开区间
//!   `(retained_watermark, live_lsn]`，与段源不重叠。
//! - **物理 evict 受 gate**：只回收 `LSN ≤ 所有活跃读者 retained_watermark 最小值` 的行
//!   （那个最小值由 yt-manifest 的 `Current::min_retained_watermark()` 给）。
//!
//! 初稿只钉了上界、没钉下界 → flush 把被吸收前缀物理删掉时，旧读者那截行「段里没有、
//! 内存也没了」，被读零次。这里靠下界 gate 保证：没人读完就不删。
//!
//! 骨架：用 `VecDeque<MemRow>` 按 commit_lsn 递增排列，从队头 evict。真实实现是带墓碑的
//! 不可变 ring + 并发安全发布。
#![allow(dead_code)]

use std::collections::VecDeque;

use yt_core::event::EventIdentity;
use yt_core::fold::{FoldInput, SpanFields};
use yt_core::ids::WalLsn;

/// 内存表里的一行 = 一个事件。`commit_lsn` 单调递增，是排序与 evict 的依据。
pub struct MemRow {
    pub commit_lsn: u64,
    pub trace_id: u64,
    pub span_id: u64,
    pub ts: i64,
    pub identity: EventIdentity,
    pub fields: SpanFields,
}

impl MemRow {
    /// 转成折叠输入（读路径用）。
    pub fn to_fold_input(&self) -> FoldInput {
        FoldInput {
            trace_id: self.trace_id,
            span_id: self.span_id,
            identity: self.identity.clone(),
            fields: self.fields.clone(),
        }
    }
}

/// 活内存表骨架。单写者 append、N 读者按区间读、回收受 gate。
#[derive(Default)]
pub struct MemTable {
    /// 按 commit_lsn 递增；队头是最老的、最先被 evict 的。
    rows: VecDeque<MemRow>,
}

impl MemTable {
    pub fn new() -> Self {
        Self { rows: VecDeque::new() }
    }

    /// 单写者 append。要求 commit_lsn 严格递增（由 WAL 的 LSN 保证）。
    pub fn append(&mut self, row: MemRow) {
        debug_assert!(
            self.rows.back().map_or(true, |b| b.commit_lsn < row.commit_lsn),
            "commit_lsn 必须严格递增"
        );
        self.rows.push_back(row);
    }

    /// 读某快照的半开区间 `(retained_watermark, live_lsn]`，与段源不重叠。
    pub fn read_range(&self, retained_watermark: WalLsn, live_lsn: WalLsn) -> impl Iterator<Item = &MemRow> {
        let lo = retained_watermark.get();
        let hi = live_lsn.get();
        self.rows.iter().filter(move |r| r.commit_lsn > lo && r.commit_lsn <= hi)
    }

    /// 物理回收：丢掉 `commit_lsn ≤ gate` 的队头行。
    /// `gate` 必须来自 `Current::min_retained_watermark()` —— 绝不能直接用 flush 的 watermark，
    /// 否则就是被红队打穿的漏行 bug。返回回收了多少行。
    pub fn evict_up_to(&mut self, gate: WalLsn) -> usize {
        let gate = gate.get();
        let mut n = 0;
        while let Some(front) = self.rows.front() {
            if front.commit_lsn <= gate {
                self.rows.pop_front();
                n += 1;
            } else {
                break;
            }
        }
        n
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// 当前内存里最老的 LSN（可观测 / 测试用）。
    pub fn oldest_lsn(&self) -> Option<u64> {
        self.rows.front().map(|r| r.commit_lsn)
    }

    /// 当前内存里最新的 LSN（自动刷盘时作 watermark）。
    pub fn newest_lsn(&self) -> Option<u64> {
        self.rows.back().map(|r| r.commit_lsn)
    }

    /// 遍历所有行（自动刷盘时把内存表内容封段用）。
    pub fn iter(&self) -> impl Iterator<Item = &MemRow> {
        self.rows.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yt_core::event::EventType;

    fn row(lsn: u64) -> MemRow {
        MemRow {
            commit_lsn: lsn,
            trace_id: 1,
            span_id: lsn,
            ts: lsn as i64,
            identity: EventIdentity {
                ext_span_id: format!("s{lsn}"),
                seq: lsn,
                event_type: EventType::SpanEnd,
            },
            fields: SpanFields::default(),
        }
    }

    fn seqs(mt: &MemTable, lo: u64, hi: u64) -> Vec<u64> {
        mt.read_range(WalLsn::new(lo), WalLsn::new(hi)).map(|r| r.commit_lsn).collect()
    }

    #[test]
    fn read_range_is_half_open() {
        let mut mt = MemTable::new();
        for l in 1..=3 {
            mt.append(row(l));
        }
        assert_eq!(seqs(&mt, 0, 3), vec![1, 2, 3]);
        assert_eq!(seqs(&mt, 1, 3), vec![2, 3]);
        assert_eq!(seqs(&mt, 0, 2), vec![1, 2]);
    }

    #[test]
    fn evict_gate_protects_old_reader_rows() {
        // 复现并修掉「flush-evict 漏行」：
        // 写 1,2,3；旧读者下界=0、上界=3，应看到 1,2,3。
        // flush 把 1 吸收进段、watermark 推到 1，但旧读者下界仍是 0。
        // evict 的 gate 必须取「所有读者下界的最小值」=0 → 一行都不删 → 旧读者仍读到 1。
        let mut mt = MemTable::new();
        for l in 1..=3 {
            mt.append(row(l));
        }
        let evicted = mt.evict_up_to(WalLsn::new(0)); // gate = min over readers = 0
        assert_eq!(evicted, 0, "有下界=0 的旧读者在，任何行都不该被删");
        assert_eq!(seqs(&mt, 0, 3), vec![1, 2, 3], "旧读者必须仍看到行 1（不能漏读）");

        // 旧读者走了，新读者下界=1 → gate 升到 1 → 行 1 可回收
        let evicted = mt.evict_up_to(WalLsn::new(1));
        assert_eq!(evicted, 1);
        assert_eq!(mt.oldest_lsn(), Some(2));
    }
}
