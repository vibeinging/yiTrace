use super::*;
use crate::InMemorySegmentStore;

fn server() -> HttpIngestServer {
    HttpIngestServer::new(WriteCoordinator::new(Arc::new(
        InMemorySegmentStore::default(),
    )))
}

const BATCH: &str = r#"[
      {"trace_id":7,"span_id":1,"ts":100,"seq":1,"event_type":1,"ext_span_id":"7-1","status":0,"input_tokens":900,"cached_input_tokens":100,"reasoning_tokens":20,"total_tokens":1170,"cost_usd":0.0025,"cost_currency":"USD","provider":"openai","logs":["开始"]},
      {"trace_id":7,"span_id":1,"ts":150,"seq":2,"event_type":2,"ext_span_id":"7-1","duration_ns":50,"output_tokens":150,"logs":["结束"]}
    ]"#;

fn durable_temp_dir(name: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "yt_http_{name}_{}_{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

include!("tests/part_00.rs");
include!("tests/part_01.rs");
include!("tests/part_02.rs");
include!("tests/part_03.rs");
