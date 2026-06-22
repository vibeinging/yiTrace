//! 可运行摄入服务：`cargo run -p yt-engine --example server`
//!   POST /v1/ingest  —— SDK 线格式 JSON 批
//!   GET  /v1/traces  —— trace 列表
//!
//! 试：
//!   curl -s localhost:7878/v1/traces
//!   curl -s -XPOST localhost:7878/v1/ingest -d '[{"trace_id":7,"span_id":1,"ts":1,"seq":1,"event_type":1,"ext_span_id":"7-1","status":0,"input_tokens":900,"logs":["开始"]}]'
use std::net::TcpListener;
use std::sync::Arc;

use yt_engine::{HttpIngestServer, InMemorySegmentStore, WriteCoordinator};

fn main() {
    let coord = WriteCoordinator::new(Arc::new(InMemorySegmentStore::default()));
    let mut server = HttpIngestServer::new(coord);
    // 设了 YT_TOKEN 就要求 Bearer 鉴权（金融政企最低门槛）。
    if let Ok(tok) = std::env::var("YT_TOKEN") {
        server = server.with_auth_token(tok);
        println!("（已开启 Bearer token 鉴权）");
    }
    let server = Arc::new(server);
    let addr = "127.0.0.1:7878";
    let listener = TcpListener::bind(addr).expect("bind");
    println!("yiTrace 摄入服务 → http://{addr}  (POST /v1/ingest, GET /v1/traces, 8 线程池)");
    server.serve_pool(listener, 8);
}
