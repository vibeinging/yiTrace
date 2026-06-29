//! 持久化服务（崩溃测试用）：`cargo run -p yt-engine --example server_durable -- /path/to/data`
//!
//! 和 `server` 区别：用 `open_durable`（段 + WAL + manifest + 向量索引全落盘），
//! 进程被 kill -9 后重启数据仍在。用于真·崩溃测试集成脚本。
//!
//! 也可作为最小持久化部署样板：
//!   cargo run -p yt-engine --example server_durable -- /data/yitrace
//!   # 然后 curl 灌/查；kill -9 后重启同样命令，数据还在。
use std::net::TcpListener;
use std::sync::Arc;

use yt_engine::{HttpIngestServer, WriteCoordinator};

fn main() {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "./yitrace-data".to_string());
    println!("open_durable 目录: {dir}");
    let coord = WriteCoordinator::open_durable(&dir).expect("open_durable");
    coord.recover(); // WAL 重放水位之后的尾巴 + 重建派生索引

    let mut server = HttpIngestServer::new(Arc::clone(&coord));
    if let Ok(tok) = std::env::var("YT_TOKEN") {
        server = server.with_auth_token(tok);
    }
    let server = Arc::new(server);
    let addr = "127.0.0.1:7879";
    let listener = TcpListener::bind(addr).expect("bind");
    println!("持久化服务 → http://{addr}/  (POST /v1/ingest, GET /v1/traces)");
    server.serve_pool(listener, 4);
}
