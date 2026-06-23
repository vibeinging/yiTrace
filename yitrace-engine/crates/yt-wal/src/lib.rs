//! yt-wal —— 写前日志。支持两种后端：
//! - **内存**（`Wal::new`）：测试用，不落盘。
//! - **文件**（`Wal::open(path)`）：**真落盘 + fsync**，进程崩溃/重启后能重放（§M.6）。
//!
//! 崩溃安全的帧格式（只用标准库，零依赖）：
//!   每批一帧 = `[first_lsn u64][payload_len u32][payload][crc32 u32][marker=1 u8]`
//!   - 整帧（含 crc+marker）写完并 **fsync** 之后才回 ack。
//!   - 重放时遇到第一个撕裂/损坏帧（短读 / marker≠1 / crc 不符）即停 —— 那批从未 ack，丢弃合法。
//!     → 不丢已 ack、不重放半截批。
//!
//! payload 是该批记录的自研二进制编码（定长字段 LE + 长度前缀字符串），同样零依赖。
#![allow(dead_code)]

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use yt_core::event::{EventIdentity, EventType};
use yt_core::fold::{FoldInput, SpanFields};
use yt_core::ids::WalLsn;

/// 一条 WAL 记录 = 一个事件。
#[derive(Clone)]
pub struct WalRecord {
    pub trace_id: u64,
    pub span_id: u64,
    pub ts: i64,
    pub identity: EventIdentity,
    pub fields: SpanFields,
}

impl WalRecord {
    pub fn to_fold_input(&self) -> FoldInput {
        FoldInput {
            trace_id: self.trace_id,
            span_id: self.span_id,
            identity: self.identity.clone(),
            fields: self.fields.clone(),
        }
    }
}

/// 内存模式下的一批（保留 crc+marker 以复用 is_committed 语义）。
struct MemBatch {
    first_lsn: u64,
    records: Vec<WalRecord>,
    crc32: u32,
    committed: bool,
}

enum Backing {
    Mem(Vec<MemBatch>),
    File { file: File, path: PathBuf },
}

pub struct Wal {
    next_lsn: u64,
    backing: Backing,
}

impl Default for Wal {
    fn default() -> Self {
        Self::new()
    }
}

impl Wal {
    /// 内存模式（测试用，不落盘）。
    pub fn new() -> Self {
        Self { next_lsn: 1, backing: Backing::Mem(Vec::new()) }
    }

    /// 文件模式：真落盘。打开已有文件并扫描出 next_lsn（恢复用），之后 append+fsync。
    pub fn open(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let existing = std::fs::read(&path).unwrap_or_default();
        let frames = parse_frames(&existing);
        // next_lsn = 最后一帧的 first_lsn + 其记录数（与 append 的 n.max(1) 递增一致）
        let next_lsn = frames
            .last()
            .map(|(first, recs)| first + (recs.len() as u64).max(1))
            .unwrap_or(1);
        let file = OpenOptions::new().create(true).append(true).read(true).open(&path)?;
        Ok(Self { next_lsn, backing: Backing::File { file, path } })
    }

    /// 追加一批并提交（组提交）。文件模式 fsync 后才返回 → 之后调用方才回 ack。
    pub fn append_committed(&mut self, records: Vec<WalRecord>) -> WalLsn {
        let first = self.next_lsn;
        let n = records.len() as u64;
        match &mut self.backing {
            Backing::Mem(batches) => {
                let crc = crc32_bytes(&encode_batch(&records));
                batches.push(MemBatch { first_lsn: first, records, crc32: crc, committed: true });
            }
            Backing::File { file, .. } => {
                let payload = encode_batch(&records);
                let crc = crc32_bytes(&payload);
                let mut frame = Vec::with_capacity(payload.len() + 17);
                frame.extend_from_slice(&first.to_le_bytes());
                frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
                frame.extend_from_slice(&payload);
                frame.extend_from_slice(&crc.to_le_bytes());
                frame.push(1u8); // commit marker
                let _ = file.write_all(&frame);
                let _ = file.sync_data(); // ★ fsync：落盘后才算 ack
            }
        }
        self.next_lsn += n.max(1);
        WalLsn::new(self.next_lsn - 1)
    }

    /// 崩溃重放：返回「已 ack」批次里 LSN 在 `from`(不含) 之后的每条记录，带其 LSN。
    /// 文件模式重新读盘解析；撕裂尾被丢弃。返回 owned（文件模式无法借用）。
    pub fn replay_after(&self, from: WalLsn) -> Vec<(u64, WalRecord)> {
        let from = from.get();
        let mut out = Vec::new();
        let mut push = |first: u64, recs: &[WalRecord]| {
            for (i, r) in recs.iter().enumerate() {
                let lsn = first + i as u64;
                if lsn > from {
                    out.push((lsn, r.clone()));
                }
            }
        };
        match &self.backing {
            Backing::Mem(batches) => {
                for b in batches {
                    if b.committed && b.crc32 == crc32_bytes(&encode_batch(&b.records)) {
                        push(b.first_lsn, &b.records);
                    }
                }
            }
            Backing::File { path, .. } => {
                let bytes = std::fs::read(path).unwrap_or_default();
                for (first, recs) in parse_frames(&bytes) {
                    push(first, &recs);
                }
            }
        }
        out
    }

    pub fn committed_tail(&self) -> WalLsn {
        WalLsn::new(self.next_lsn - 1)
    }
}

// ───────────────────────── 帧解析 ─────────────────────────

/// 解析文件里所有「已提交」帧（crc 通过 + marker=1）。遇撕裂/损坏即停（视为未 ack 的尾）。
fn parse_frames(bytes: &[u8]) -> Vec<(u64, Vec<WalRecord>)> {
    let mut out = Vec::new();
    let mut i = 0usize;
    loop {
        if i + 12 > bytes.len() {
            break; // 不足 first_lsn(8)+len(4)
        }
        let first = u64::from_le_bytes(bytes[i..i + 8].try_into().unwrap());
        i += 8;
        let len = u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap()) as usize;
        i += 4;
        if i + len + 5 > bytes.len() {
            break; // payload+crc(4)+marker(1) 不全 → 撕裂尾
        }
        let payload = &bytes[i..i + len];
        i += len;
        let crc = u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap());
        i += 4;
        let marker = bytes[i];
        i += 1;
        if marker != 1 || crc != crc32_bytes(payload) {
            break; // 未提交 / 损坏 → 停
        }
        match decode_batch(payload) {
            Some(recs) => out.push((first, recs)),
            None => break,
        }
    }
    out
}

// ───────────────────────── 二进制编解码（std-only） ─────────────────────────

fn put_u64(b: &mut Vec<u8>, v: u64) {
    b.extend_from_slice(&v.to_le_bytes());
}
fn put_str(b: &mut Vec<u8>, s: &str) {
    put_u64(b, s.len() as u64);
    b.extend_from_slice(s.as_bytes());
}
fn put_opt_u64(b: &mut Vec<u8>, v: Option<u64>) {
    match v {
        Some(x) => {
            b.push(1);
            put_u64(b, x);
        }
        None => b.push(0),
    }
}
fn put_opt_u8(b: &mut Vec<u8>, v: Option<u8>) {
    match v {
        Some(x) => {
            b.push(1);
            b.push(x);
        }
        None => b.push(0),
    }
}
fn put_opt_str(b: &mut Vec<u8>, v: &Option<String>) {
    match v {
        Some(s) => {
            b.push(1);
            put_str(b, s);
        }
        None => b.push(0),
    }
}

/// 把一批记录编码成自研二进制（定长 LE + 长度前缀字符串）。WAL 用它，**段落盘也复用同一套编码**
/// （`FileSegmentStore`），避免两处各写一份记录序列化。
pub fn encode_records(records: &[WalRecord]) -> Vec<u8> {
    encode_batch(records)
}

/// `encode_records` 的逆。损坏/截断返回 None。
pub fn decode_records(payload: &[u8]) -> Option<Vec<WalRecord>> {
    decode_batch(payload)
}

/// 无表 CRC32（IEEE），段文件完整性校验复用 WAL 同一实现。
pub fn crc32(data: &[u8]) -> u32 {
    crc32_bytes(data)
}

/// SpanFields 的二进制编码（唯一一份）—— WAL、段落盘、manifest 持久化都复用它，避免字段列表抄多份。
fn encode_span_fields_into(b: &mut Vec<u8>, f: &SpanFields) {
    put_opt_u8(b, f.status);
    put_opt_u64(b, f.duration_ns);
    put_opt_u64(b, f.parent_span_id);
    put_opt_u64(b, f.input_tokens);
    put_opt_u64(b, f.output_tokens);
    put_opt_u64(b, f.session_id);
    put_opt_str(b, &f.agent_name);
    put_opt_str(b, &f.tool_name);
    put_opt_str(b, &f.model);
    put_opt_str(b, &f.input_text);
    put_opt_str(b, &f.output_text);
    put_opt_u64(b, f.eval_score.map(|v| v as u64));
    put_opt_str(b, &f.eval_label);
    put_u64(b, f.logs.len() as u64);
    for l in &f.logs {
        put_str(b, l);
    }
}

fn decode_span_fields_from(c: &mut Cur) -> Option<SpanFields> {
    let status = c.opt_u8()?;
    let duration_ns = c.opt_u64()?;
    let parent_span_id = c.opt_u64()?;
    let input_tokens = c.opt_u64()?;
    let output_tokens = c.opt_u64()?;
    let session_id = c.opt_u64()?;
    let agent_name = c.opt_str()?;
    let tool_name = c.opt_str()?;
    let model = c.opt_str()?;
    let input_text = c.opt_str()?;
    let output_text = c.opt_str()?;
    let eval_score = c.opt_u64()?.map(|v| v as u32);
    let eval_label = c.opt_str()?;
    let log_n = c.u64()? as usize;
    let mut logs = Vec::with_capacity(log_n);
    for _ in 0..log_n {
        logs.push(c.string()?);
    }
    Some(SpanFields {
        status,
        duration_ns,
        parent_span_id,
        input_tokens,
        output_tokens,
        session_id,
        agent_name,
        tool_name,
        model,
        input_text,
        output_text,
        eval_score,
        eval_label,
        logs,
    })
}

/// 把一组 `SpanFields` 字段编成独立字节块（manifest 持久化 upgrade 补写块时用）。
pub fn encode_span_fields(f: &SpanFields) -> Vec<u8> {
    let mut b = Vec::new();
    encode_span_fields_into(&mut b, f);
    b
}

/// `encode_span_fields` 的逆。
pub fn decode_span_fields(bytes: &[u8]) -> Option<SpanFields> {
    decode_span_fields_from(&mut Cur { b: bytes, i: 0 })
}

fn encode_batch(records: &[WalRecord]) -> Vec<u8> {
    let mut b = Vec::new();
    put_u64(&mut b, records.len() as u64);
    for r in records {
        put_u64(&mut b, r.trace_id);
        put_u64(&mut b, r.span_id);
        put_u64(&mut b, r.ts as u64); // i64 位模式
        put_str(&mut b, &r.identity.ext_span_id);
        put_u64(&mut b, r.identity.seq);
        b.push(r.identity.event_type.tag());
        encode_span_fields_into(&mut b, &r.fields);
    }
    b
}

struct Cur<'a> {
    b: &'a [u8],
    i: usize,
}
impl<'a> Cur<'a> {
    fn u64(&mut self) -> Option<u64> {
        let e = self.i + 8;
        let s = self.b.get(self.i..e)?;
        self.i = e;
        Some(u64::from_le_bytes(s.try_into().ok()?))
    }
    fn u8(&mut self) -> Option<u8> {
        let v = *self.b.get(self.i)?;
        self.i += 1;
        Some(v)
    }
    fn string(&mut self) -> Option<String> {
        let n = self.u64()? as usize;
        let e = self.i + n;
        let s = self.b.get(self.i..e)?;
        self.i = e;
        Some(String::from_utf8_lossy(s).into_owned())
    }
    fn opt_u64(&mut self) -> Option<Option<u64>> {
        if self.u8()? == 1 {
            Some(Some(self.u64()?))
        } else {
            Some(None)
        }
    }
    fn opt_u8(&mut self) -> Option<Option<u8>> {
        if self.u8()? == 1 {
            Some(Some(self.u8()?))
        } else {
            Some(None)
        }
    }
    fn opt_str(&mut self) -> Option<Option<String>> {
        if self.u8()? == 1 {
            Some(Some(self.string()?))
        } else {
            Some(None)
        }
    }
}

fn decode_batch(payload: &[u8]) -> Option<Vec<WalRecord>> {
    let mut c = Cur { b: payload, i: 0 };
    let n = c.u64()? as usize;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let trace_id = c.u64()?;
        let span_id = c.u64()?;
        let ts = c.u64()? as i64;
        let ext = c.string()?;
        let seq = c.u64()?;
        let event_type = EventType::from_tag(c.u8()?);
        let fields = decode_span_fields_from(&mut c)?;
        out.push(WalRecord {
            trace_id,
            span_id,
            ts,
            identity: EventIdentity { ext_span_id: ext, seq, event_type },
            fields,
        });
    }
    Some(out)
}

/// CRC32（IEEE，反射多项式 0xEDB8_8320）查表实现：256 项表在首用时一次性算好（`OnceLock`，零外部依赖、
/// 不破 std-only），之后每字节一次查表，去掉了原来每字节 8 次内层位运算。WAL fsync 前对每批都算一次，
/// 大批量写是热点，查表是稳妥的常数级加速（保持零依赖，不引 crc32fast）。
fn crc32_table() -> &'static [u32; 256] {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        let mut i = 0usize;
        while i < 256 {
            let mut crc = i as u32;
            let mut j = 0;
            while j < 8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
                j += 1;
            }
            t[i] = crc;
            i += 1;
        }
        t
    })
}

fn crc32_bytes(data: &[u8]) -> u32 {
    let table = crc32_table();
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc = (crc >> 8) ^ table[((crc ^ b as u32) & 0xFF) as usize];
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use yt_core::event::EventType;

    fn rec(span: &str, seq: u64) -> WalRecord {
        WalRecord {
            trace_id: 1,
            span_id: seq,
            ts: seq as i64,
            identity: EventIdentity { ext_span_id: span.into(), seq, event_type: EventType::SpanEnd },
            fields: SpanFields { logs: vec![format!("日志{seq}")], ..Default::default() },
        }
    }

    fn temp_path() -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!("yt_wal_{}_{}.wal", std::process::id(), N.fetch_add(1, Ordering::Relaxed)))
    }

    #[test]
    fn crc32_matches_ieee_known_vectors() {
        // 查表实现必须与 IEEE CRC32 标准逐字节一致（换实现不能改校验和,否则老 WAL/段 全部读不回）。
        assert_eq!(crc32_bytes(b""), 0x0000_0000);
        assert_eq!(crc32_bytes(b"123456789"), 0xCBF4_3926, "标准测试向量");
        assert_eq!(crc32_bytes(b"The quick brown fox jumps over the lazy dog"), 0x414F_A339);
    }

    #[test]
    fn mem_replay_after_watermark() {
        let mut wal = Wal::new();
        wal.append_committed(vec![rec("a", 1)]);
        let l2 = wal.append_committed(vec![rec("b", 2), rec("c", 3)]);
        assert_eq!(wal.committed_tail(), l2);
        let all: Vec<_> = wal.replay_after(WalLsn::new(0)).into_iter().map(|(l, _)| l).collect();
        assert_eq!(all, vec![1, 2, 3]);
        let after: Vec<_> = wal.replay_after(WalLsn::new(1)).into_iter().map(|(_, r)| r.identity.seq).collect();
        assert_eq!(after, vec![2, 3]);
    }

    #[test]
    fn file_wal_survives_reopen_real_disk() {
        // 真落盘：写 → drop(模拟崩溃) → 重开同一文件 → 重放,记录还在。
        let path = temp_path();
        {
            let mut wal = Wal::open(&path).unwrap();
            wal.append_committed(vec![rec("反洗钱", 1), rec("盗刷", 2)]);
            wal.append_committed(vec![rec("转账", 3)]);
            assert_eq!(wal.committed_tail(), WalLsn::new(3));
            // drop → File 关闭。之前每批都 fsync 过。
        }
        // 重开（相当于重启进程）
        let wal2 = Wal::open(&path).unwrap();
        assert_eq!(wal2.committed_tail(), WalLsn::new(3), "重开后 next_lsn 从盘上恢复");
        let recs = wal2.replay_after(WalLsn::new(0));
        let seqs: Vec<u64> = recs.iter().map(|(l, _)| *l).collect();
        assert_eq!(seqs, vec![1, 2, 3], "三条记录从磁盘重放回来");
        // 内容也对（含中文 + 逐字段）
        assert_eq!(recs[0].1.identity.ext_span_id, "反洗钱");
        assert_eq!(recs[0].1.fields.logs, vec!["日志1"]);
        assert_eq!(recs[1].1.identity.ext_span_id, "盗刷");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn torn_tail_is_dropped() {
        // 模拟「最后一帧只写了一半就崩了」：截断文件尾部 → 该批视为未 ack,重放丢弃,前面的不受影响。
        let path = temp_path();
        {
            let mut wal = Wal::open(&path).unwrap();
            wal.append_committed(vec![rec("ok", 1)]);
            wal.append_committed(vec![rec("half", 2)]);
        }
        // 砍掉文件最后 3 字节（破坏第二帧的 crc/marker）
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.truncate(bytes.len() - 3);
        std::fs::write(&path, &bytes).unwrap();

        let wal = Wal::open(&path).unwrap();
        let seqs: Vec<u64> = wal.replay_after(WalLsn::new(0)).iter().map(|(l, _)| *l).collect();
        assert_eq!(seqs, vec![1], "撕裂的第二帧被丢弃,第一帧完好");
        let _ = std::fs::remove_file(&path);
    }
}
