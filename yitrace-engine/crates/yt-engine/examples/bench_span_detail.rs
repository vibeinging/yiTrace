//! 单 Span 详情点读基准。
//!
//! 跑：`cargo run --release -p yt-engine --example bench_span_detail`
//! 分别构造包含 10 / 100 / 1000 个 span 的单条 Trace，数据 flush 到真实文件段后，
//! 通过 `GET /v1/traces/:traceId/spans/:spanId` 的同一进程 JSON API 测 P50/P95。

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use yt_engine::{EngineJsonApi, WireRecord, WriteCoordinator};

const TRACE_ID: u64 = 88;
const QUERIES: usize = 500;

fn record(span_id: u64) -> WireRecord {
    WireRecord {
        trace_id: TRACE_ID,
        span_id,
        ts: span_id as i64,
        seq: 1,
        event_type_tag: 2,
        ext_span_id: format!("{TRACE_ID}-{span_id}"),
        parent_span_id: None,
        status: Some(0),
        duration_ns: Some(1_000),
        input_tokens: Some(10),
        output_tokens: Some(5),
        cache_read_tokens: None,
        cache_write_tokens: None,
        session_id: Some(1),
        tenant_id: None,
        external_trace_id: None,
        external_span_id: None,
        external_parent_span_id: None,
        external_session_id: None,
        span_name: Some(format!("worker.{span_id}")),
        display_name: None,
        agent_name: Some("worker".to_string()),
        tool_name: None,
        model: None,
        input_text: Some("input".to_string()),
        output_text: Some("output".to_string()),
        logs: vec!["detail-log".to_string()],
        attrs: BTreeMap::new(),
    }
}

fn percentile(samples: &[Duration], pct: usize) -> Duration {
    samples[((samples.len() - 1) * pct / 100).min(samples.len() - 1)]
}

fn measure(api: &EngineJsonApi, path: &str) -> (u128, u128, u128) {
    let (status, _) = api.route("GET", path, "");
    assert_eq!(status, 200);
    let mut samples = Vec::with_capacity(QUERIES);
    for _ in 0..QUERIES {
        let started = Instant::now();
        let (status, body) = api.route("GET", path, "");
        samples.push(started.elapsed());
        assert_eq!(status, 200);
        assert!(body.contains(r#""logEvents":[{"#));
    }
    samples.sort_unstable();
    (
        percentile(&samples, 50).as_micros(),
        percentile(&samples, 95).as_micros(),
        samples.last().unwrap().as_micros(),
    )
}

fn main() {
    println!("source\tspans\tqueries\tp50_us\tp95_us\tmax_us");
    for span_count in [10usize, 100, 1_000] {
        let dir = std::env::temp_dir().join(format!(
            "yt_bench_span_detail_{}_{}",
            std::process::id(),
            span_count
        ));
        let _ = std::fs::remove_dir_all(&dir);

        let coord = Arc::new(WriteCoordinator::open_durable(&dir).expect("open durable engine"));
        coord.ingest_wire((1..=span_count as u64).map(record).collect());
        let api = EngineJsonApi::new(Arc::clone(&coord));
        let path = format!("/v1/traces/{TRACE_ID}/spans/{span_count}");

        let (p50, p95, max) = measure(&api, &path);
        println!(
            "memtable\t{}\t{}\t{}\t{}\t{}",
            span_count, QUERIES, p50, p95, max
        );

        coord.flush_memtable();
        let (p50, p95, max) = measure(&api, &path);
        println!(
            "segment\t{}\t{}\t{}\t{}\t{}",
            span_count, QUERIES, p50, p95, max
        );

        drop(api);
        drop(coord);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
