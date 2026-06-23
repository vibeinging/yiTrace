//! 可运行控制台服务：`cargo run -p yt-engine --example server`
//!   GET  /                 —— 内嵌的控制台前端（若已 vite build + 拷到 console_dist/）
//!   GET  /v1/sessions      —— 会话列表（游标分页）
//!   GET  /v1/sessions/:id/turns、/v1/traces/:id、/v1/traces/:id/spans/:sid —— 控制台数据
//!   POST /v1/ingest、/v1/traces(OTLP)、/v1/search —— 摄入 / 检索
//!
//! 启动时用 evalkit 灌一批多轮会话假数据，开箱即有内容可看。
use std::net::TcpListener;
use std::sync::Arc;

use yt_engine::{evalkit, HttpIngestServer, InMemorySegmentStore, WriteCoordinator};

fn main() {
    let coord = WriteCoordinator::new(Arc::new(InMemorySegmentStore::default()));

    // 种子数据：多轮会话（客服问答四种弧线）+ 四类 agent 场景，控制台开箱有料。
    let s = evalkit::run_session_harness(&coord, 60, 20_260_623);
    println!("已灌 {} 个多轮会话（共 {} 轮）", s.gen.sessions, s.gen.turns);
    let r = evalkit::run_harness(&coord, 40, 7);
    let traces: usize = r.scenarios.iter().map(|x| x.traces).sum();
    println!("已灌 {} 条场景 trace", traces);

    let mut server = HttpIngestServer::new(Arc::clone(&coord));
    if let Ok(tok) = std::env::var("YT_TOKEN") {
        server = server.with_auth_token(tok);
        println!("（已开启 Bearer token 鉴权）");
    }
    let server = Arc::new(server);
    let addr = "127.0.0.1:7878";
    let listener = TcpListener::bind(addr).expect("bind");
    println!("yiTrace 控制台 → http://{addr}/  （前端需先 build 并拷到 console_dist/）");
    server.serve_pool(listener, 8);
}
