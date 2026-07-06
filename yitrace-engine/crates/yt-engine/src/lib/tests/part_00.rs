use super::*;

use yt_core::fold::SpanFields;

/// 测试用的空段存储。
struct NoopStore;
impl SegmentStore for NoopStore {
    fn flush_to_segment(&self, _seg: SegmentId, _records: &[WalRecord]) {}
    fn scan_fold_inputs(&self, _seg: SegmentId) -> Vec<(u32, FoldInput)> {
        Vec::new()
    }
    fn scan_records(&self, _seg: SegmentId) -> Vec<WalRecord> {
        Vec::new()
    }
    fn unlink_segment(&self, _seg: SegmentId) {}
}

/// 记录被 unlink 的段 id，供回收测试断言。
#[derive(Default)]
struct RecordingStore {
    unlinked: Mutex<Vec<u64>>,
}
impl RecordingStore {
    fn unlinked(&self) -> Vec<u64> {
        self.unlinked.lock().unwrap().clone()
    }
}
impl SegmentStore for RecordingStore {
    fn flush_to_segment(&self, _seg: SegmentId, _records: &[WalRecord]) {}
    fn scan_fold_inputs(&self, _seg: SegmentId) -> Vec<(u32, FoldInput)> {
        Vec::new()
    }
    fn scan_records(&self, _seg: SegmentId) -> Vec<WalRecord> {
        Vec::new()
    }
    fn unlink_segment(&self, seg: SegmentId) {
        self.unlinked.lock().unwrap().push(seg.get());
    }
}

/// 支持下推的 mock 段存储：时间下推 / 投影下推都真做，并把「最近一次收到的投影」与「时间下推次数」
/// 记下来，供测试断言引擎确实走了下推、且传下来的投影是窄的（聚合不带文本列）。
#[derive(Default)]
struct PushdownStore {
    rows: Mutex<std::collections::HashMap<u64, Vec<WalRecord>>>,
    pushdowns: std::sync::atomic::AtomicUsize,
    /// 最近一次任意下推（时间/投影）收到的投影位，供断言"聚合查询不要文本列"。
    last_proj: std::sync::atomic::AtomicU32,
}
impl PushdownStore {
    fn last_proj(&self) -> Projection {
        Projection::of(self.last_proj.load(std::sync::atomic::Ordering::Relaxed))
    }
}
impl SegmentStore for PushdownStore {
    fn flush_to_segment(&self, seg: SegmentId, records: &[WalRecord]) {
        self.rows
            .lock()
            .unwrap()
            .insert(seg.get(), records.to_vec());
    }
    fn scan_fold_inputs(&self, seg: SegmentId) -> Vec<(u32, FoldInput)> {
        self.rows
            .lock()
            .unwrap()
            .get(&seg.get())
            .map(|rs| {
                rs.iter()
                    .enumerate()
                    .map(|(i, r)| (i as u32, r.to_fold_input()))
                    .collect()
            })
            .unwrap_or_default()
    }
    fn scan_records(&self, seg: SegmentId) -> Vec<WalRecord> {
        self.rows
            .lock()
            .unwrap()
            .get(&seg.get())
            .cloned()
            .unwrap_or_default()
    }
    fn unlink_segment(&self, seg: SegmentId) {
        self.rows.lock().unwrap().remove(&seg.get());
    }
    fn scan_fold_inputs_projected(
        &self,
        seg: SegmentId,
        proj: Projection,
    ) -> Option<Vec<(u32, FoldInput)>> {
        self.last_proj
            .store(proj.bits(), std::sync::atomic::Ordering::Relaxed);
        Some(self.scan_fold_inputs(seg))
    }
    fn scan_fold_inputs_in_time(
        &self,
        seg: SegmentId,
        from: i64,
        to: i64,
        proj: Projection,
    ) -> Option<Vec<FoldInput>> {
        self.pushdowns
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.last_proj
            .store(proj.bits(), std::sync::atomic::Ordering::Relaxed);
        let g = self.rows.lock().unwrap();
        Some(
            g.get(&seg.get())
                .map(|rs| {
                    rs.iter()
                        .filter(|r| r.ts >= from && r.ts <= to)
                        .map(|r| r.to_fold_input())
                        .collect()
                })
                .unwrap_or_default(),
        )
    }
}

/// 端到端测试用的段存储 = 公开的内存段存储（flush 存、scan 返回、unlink 真删）。
use super::InMemorySegmentStore as CapturingStore;

fn rec(span: &str, seq: u64) -> WalRecord {
    WalRecord {
        trace_id: 1,
        span_id: seq,
        ts: seq as i64,
        identity: EventIdentity {
            ext_span_id: span.into(),
            seq,
            event_type: EventType::SpanEnd,
        },
        fields: SpanFields::default(),
    }
}

/// 带可折叠字段的事件构造器（ts 默认 = seq）。
fn ev(
    trace: u64,
    span: u64,
    seq: u64,
    status: Option<u8>,
    dur: Option<u64>,
    logs: &[&str],
) -> WalRecord {
    ev_at(trace, span, seq, seq as i64, status, dur, logs)
}

/// 指定时间戳的事件构造器。
fn ev_at(
    trace: u64,
    span: u64,
    seq: u64,
    ts: i64,
    status: Option<u8>,
    dur: Option<u64>,
    logs: &[&str],
) -> WalRecord {
    WalRecord {
        trace_id: trace,
        span_id: span,
        ts,
        identity: EventIdentity {
            ext_span_id: format!("{trace}-{span}"),
            seq,
            event_type: EventType::Attr,
        },
        fields: SpanFields {
            status,
            duration_ns: dur,
            logs: logs.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        },
    }
}
