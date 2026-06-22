//! 摄入吞吐粗测：`cargo run -p yt-engine --example bench --release`
//! 测「解析 JSON + 灌进引擎折叠」这条服务端热路（不含 HTTP 框架、不含真 fsync）的事件/秒。
use std::sync::Arc;
use std::time::Instant;

use yt_engine::{parse_wire_batch, InMemorySegmentStore, WriteCoordinator};

fn main() {
    // 造一个 50 事件的批（JSON）。
    let mut evs = Vec::new();
    for i in 0..50u64 {
        evs.push(format!(
            r#"{{"trace_id":{},"span_id":{},"ts":{},"seq":1,"event_type":1,"ext_span_id":"{}-{}","status":0,"input_tokens":100,"logs":["事件{}"]}}"#,
            i / 5, i, i * 10, i / 5, i, i
        ));
    }
    let batch = format!("[{}]", evs.join(","));

    let coord = WriteCoordinator::new(Arc::new(InMemorySegmentStore::default()));
    coord.set_flush_threshold(200_000);

    let iters = 40_000usize;
    let t = Instant::now();
    let mut total = 0usize;
    for _ in 0..iters {
        let recs = parse_wire_batch(&batch).unwrap();
        total += recs.len();
        coord.ingest_wire(recs);
    }
    let dt = t.elapsed();
    let eps = total as f64 / dt.as_secs_f64();

    let need_span_s = 1e8 / 86400.0; // <1亿 span/天
    let need_evt_s = 3.0 * need_span_s; // 按 ~3 事件/span
    println!("解析+灌入 {total} 事件 / {dt:.2?} → {eps:.0} 事件/秒（in-process，无 fsync）");
    println!("需求：<1亿 span/天 ≈ {need_span_s:.0} span/s ≈ {need_evt_s:.0} 事件/s");
    println!("余量 ≈ {:.0}×", eps / need_evt_s);
}
