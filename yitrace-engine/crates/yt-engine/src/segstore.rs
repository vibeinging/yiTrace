//! segstore.rs —— **段落盘**（`FileSegmentStore`）：把不可变段写到磁盘文件，重启不丢。
//!
//! 替掉 `InMemorySegmentStore`（进程没了段就没）。补的是"一 flush 推进水位、那批数据就只活在内存段里、
//! 重启就没"这个真痛点 —— flush 后 WAL 重放只补水位**之后**的尾巴，水位之前的数据必须靠**持久化的段**。
//!
//! 编码复用 WAL 同一套（`yt_wal::encode_records`/`decode_records`），不再各写一份记录序列化。
//! 每个段一个文件 `seg-<id>.dat`，格式 `[crc32 u32][payload]`：
//! - **原子落盘**：先写 `seg-<id>.tmp` + `fsync`，再 `rename` 到正式名（rename 在同目录是原子的）→
//!   不会出现"写一半的段文件"。
//! - **读时校验 crc**：损坏/截断的段当空段（不返回脏数据；上层压测会立刻抓到读空）。
//!
//! 仍缺：列式（现在是行式 WAL 编码，Vortex 列式替换是后续要不要加依赖的单独决定）。manifest 持久化另做
//! —— 段文件在盘上，但"有哪些段、各段的删除/补写"靠 manifest，那块单独一单元。
#![allow(dead_code)]

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use yt_core::fold::FoldInput;
use yt_core::ids::SegmentId;
use yt_wal::WalRecord;

use crate::SegmentStore;

/// 段落盘到一个目录，每段一个文件。
pub struct FileSegmentStore {
    dir: PathBuf,
}

impl FileSegmentStore {
    /// 打开/创建段目录。
    pub fn open(dir: impl AsRef<Path>) -> std::io::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    fn seg_path(&self, seg: SegmentId) -> PathBuf {
        self.dir.join(format!("seg-{}.dat", seg.get()))
    }
    fn tmp_path(&self, seg: SegmentId) -> PathBuf {
        self.dir.join(format!("seg-{}.tmp", seg.get()))
    }

    /// 原子写：写 tmp + fsync + rename。失败静默（与 InMemory 行为对齐；真实实现应上报）。
    fn write_atomic(&self, seg: SegmentId, bytes: &[u8]) {
        let tmp = self.tmp_path(seg);
        if let Ok(mut f) = OpenOptions::new().create(true).write(true).truncate(true).open(&tmp) {
            if f.write_all(bytes).is_ok() {
                let _ = f.sync_all(); // ★ fsync：落盘后才 rename
                let _ = fs::rename(&tmp, self.seg_path(seg));
            }
        }
    }
}

impl SegmentStore for FileSegmentStore {
    fn flush_to_segment(&self, seg: SegmentId, records: &[WalRecord]) {
        let payload = yt_wal::encode_records(records);
        let mut buf = Vec::with_capacity(payload.len() + 4);
        buf.extend_from_slice(&yt_wal::crc32(&payload).to_le_bytes());
        buf.extend_from_slice(&payload);
        self.write_atomic(seg, &buf);
    }

    fn scan_fold_inputs(&self, seg: SegmentId) -> Vec<(u32, FoldInput)> {
        self.scan_records(seg)
            .iter()
            .enumerate()
            .map(|(i, r)| (i as u32, r.to_fold_input()))
            .collect()
    }

    fn scan_records(&self, seg: SegmentId) -> Vec<WalRecord> {
        let bytes = fs::read(self.seg_path(seg)).unwrap_or_default();
        if bytes.len() < 4 {
            return Vec::new(); // 缺文件 / 太短
        }
        let crc = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
        let payload = &bytes[4..];
        if crc != yt_wal::crc32(payload) {
            return Vec::new(); // 损坏/截断 → 当空段，绝不返回脏数据
        }
        yt_wal::decode_records(payload).unwrap_or_default()
    }

    fn unlink_segment(&self, seg: SegmentId) {
        let _ = fs::remove_file(self.seg_path(seg));
    }
}

/// 确保目录 fsync（rename 的目录项也要落盘才真持久；调用方在一批写后调一次即可）。
pub fn fsync_dir(dir: impl AsRef<Path>) {
    if let Ok(f) = File::open(dir.as_ref()) {
        let _ = f.sync_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use yt_core::event::{EventIdentity, EventType};
    use yt_core::fold::SpanFields;

    fn temp_dir() -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "yt_segstore_{}_{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&p);
        p
    }

    fn rec(span: &str, seq: u64, log: &str) -> WalRecord {
        WalRecord {
            trace_id: 1,
            span_id: seq,
            ts: seq as i64,
            identity: EventIdentity { ext_span_id: span.into(), seq, event_type: EventType::SpanEnd },
            fields: SpanFields { logs: vec![log.into()], ..Default::default() },
        }
    }

    #[test]
    fn segment_survives_reopen_real_disk() {
        // 真落盘:写段 → drop store(模拟进程没了)→ 新 store 开同一目录 → 段还在。
        let dir = temp_dir();
        let seg = SegmentId::new(7);
        {
            let store = FileSegmentStore::open(&dir).unwrap();
            store.flush_to_segment(seg, &[rec("反洗钱", 1, "日志1"), rec("盗刷", 2, "日志2")]);
        }
        // 重开（相当于重启进程）
        let store2 = FileSegmentStore::open(&dir).unwrap();
        let recs = store2.scan_records(seg);
        assert_eq!(recs.len(), 2, "段从磁盘读回来");
        assert_eq!(recs[0].identity.ext_span_id, "反洗钱");
        assert_eq!(recs[0].fields.logs, vec!["日志1"]);
        assert_eq!(recs[1].identity.ext_span_id, "盗刷");
        // 行号映射
        let folds = store2.scan_fold_inputs(seg);
        assert_eq!(folds.len(), 2);
        assert_eq!(folds[0].0, 0);
        assert_eq!(folds[1].0, 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_segment_reads_as_empty_not_garbage() {
        // crc 守门:文件被改坏 → 当空段,绝不返回脏数据。
        let dir = temp_dir();
        let seg = SegmentId::new(3);
        let store = FileSegmentStore::open(&dir).unwrap();
        store.flush_to_segment(seg, &[rec("ok", 1, "x")]);
        assert_eq!(store.scan_records(seg).len(), 1);

        // 翻末尾一个字节（破坏 payload，crc 不符）
        let path = store.seg_path(seg);
        let mut bytes = fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        fs::write(&path, &bytes).unwrap();

        assert!(store.scan_records(seg).is_empty(), "损坏段读成空,不返回脏数据");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unlink_removes_segment_file() {
        let dir = temp_dir();
        let seg = SegmentId::new(5);
        let store = FileSegmentStore::open(&dir).unwrap();
        store.flush_to_segment(seg, &[rec("a", 1, "x")]);
        assert!(store.seg_path(seg).exists());
        store.unlink_segment(seg);
        assert!(!store.seg_path(seg).exists(), "unlink 真删段文件");
        assert!(store.scan_records(seg).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_segment_scans_empty() {
        let dir = temp_dir();
        let store = FileSegmentStore::open(&dir).unwrap();
        assert!(store.scan_records(SegmentId::new(999)).is_empty(), "不存在的段读成空,不 panic");
        let _ = fs::remove_dir_all(&dir);
    }
}
