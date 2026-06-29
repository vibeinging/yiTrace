//! GC 日志：让段回收在崩溃下也不留"删一半 + manifest 没更新"的不一致。
//!
//! 流程（见 `WriteCoordinator::reclaim`）：
//! 1. 判定某段可删后，先 `mark(seg)` 写一条 `MARK <seg>` 并 fsync（意图落盘）
//! 2. `unlink_segment(seg)`（真删文件）
//! 3. `done(seg)` 写一条 `DONE <seg>` 并 fsync（完成落盘）
//!
//! 崩溃恢复（`recover_gc`）：扫日志，**有 MARK 没 DONE 的段**——文件可能删了一半，补删；
//! 已 DONE 的段——manifest 那边已不引用（reclaim 前提），跳过。
//!
//! 格式（纯文本、好排查）：每行 `<TAG> <seg_id u64>\n`，TAG ∈ {MARK, DONE}。
//! fsync 的力度是每行一次（段回收不是热路径，且 GC 正确性优先）。

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

/// 一条 GC 日志记录。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GcEntry {
    Mark(u64),
    Done(u64),
}

/// GC 日志句柄。非 durable 模式下不存在（None），reclaim 走旧的"直接删"路径。
pub struct GcLog {
    file: File,
}

impl GcLog {
    /// 打开（或创建）GC 日志。追加写。
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self { file })
    }

    /// 写一条 MARK（即将删 seg）。fsync 后返回，确保意图先于真删持久。
    pub fn mark(&mut self, seg: u64) -> std::io::Result<()> {
        writeln!(self.file, "MARK {seg}")?;
        self.file.sync_data()?;
        Ok(())
    }

    /// 写一条 DONE（seg 已删完）。fsync 后返回。
    pub fn done(&mut self, seg: u64) -> std::io::Result<()> {
        writeln!(self.file, "DONE {seg}")?;
        self.file.sync_data()?;
        Ok(())
    }

    /// 扫描日志，返回所有 entry（按写入顺序）。
    pub fn scan(path: &Path) -> std::io::Result<Vec<GcEntry>> {
        let f = match File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut out = Vec::new();
        for line in BufReader::new(f).lines() {
            let line = line?;
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let (tag, rest) = match line.split_once(' ') {
                Some(x) => x,
                None => continue, // 坏行忽略（撕裂尾：最后半行没 \n，被 lines() 当一行读出来但 split 不到）
            };
            let seg: u64 = match rest.trim().parse() {
                Ok(v) => v,
                Err(_) => continue, // 坏行忽略
            };
            match tag {
                "MARK" => out.push(GcEntry::Mark(seg)),
                "DONE" => out.push(GcEntry::Done(seg)),
                _ => continue,
            }
        }
        Ok(out)
    }
}

/// 从扫描结果算出"有 MARK 没 DONE"的段——这些要补删。
/// 实现：用集合差。一次 MARK 多次（同一 seg）以最后一次为准；DONE 抵消 MARK。
pub fn pending_deletions(entries: &[GcEntry]) -> Vec<u64> {
    use std::collections::HashSet;
    let mut marked: HashSet<u64> = HashSet::new();
    for e in entries {
        match e {
            GcEntry::Mark(s) => {
                marked.insert(*s);
            }
            GcEntry::Done(s) => {
                marked.remove(s);
            }
        }
    }
    let mut v: Vec<u64> = marked.into_iter().collect();
    v.sort();
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn pending_returns_mark_without_done() {
        let tmp = std::env::temp_dir().join("yt_gctest1.log");
        let _ = std::fs::remove_file(&tmp);
        let mut log = GcLog::open(&tmp).unwrap();
        log.mark(1).unwrap();
        log.done(1).unwrap(); // seg 1 完整删
        log.mark(2).unwrap(); // seg 2 只 MARK 没 DONE → 补删
        log.mark(3).unwrap();
        log.done(3).unwrap();
        log.mark(3).unwrap(); // seg 3 又被标记（比如新死）→ 仍 pending
        drop(log);
        let entries = GcLog::scan(&tmp).unwrap();
        let pending = pending_deletions(&entries);
        assert_eq!(pending, vec![2, 3]);
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn scan_tolerates_torn_tail() {
        let tmp = std::env::temp_dir().join("yt_gctest2.log");
        let _ = std::fs::remove_file(&tmp);
        // 手写一行正常 + 一行坏（撕裂：数字残缺）
        let mut f = std::fs::File::create(&tmp).unwrap();
        writeln!(f, "MARK 42").unwrap();
        write!(f, "DO").unwrap(); // 撕裂尾
        drop(f);
        let entries = GcLog::scan(&tmp).unwrap();
        // 好行解析出来，坏行忽略
        assert!(entries.iter().any(|e| matches!(e, GcEntry::Mark(42))));
        assert_eq!(entries.len(), 1);
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn scan_missing_file_is_empty() {
        let tmp = std::env::temp_dir().join("yt_gctest_nonexistent.log");
        let _ = std::fs::remove_file(&tmp);
        let entries = GcLog::scan(&tmp).unwrap();
        assert!(entries.is_empty());
    }
}
