use std::net::TcpListener;
use std::sync::Arc;

use yt_engine::{RemoteGatewayServer, RemoteShardGateway};

fn main() {
    let gateway = match gateway_from_env() {
        Ok(gateway) => gateway,
        Err(error) => {
            eprintln!("{error}");
            eprintln!(
                "usage: cargo run -p yt-engine --example gateway_server -- /path/to/route-table.json"
            );
            eprintln!("   or: YT_SHARDS=127.0.0.1:7901,127.0.0.1:7902 cargo run -p yt-engine --example gateway_server");
            std::process::exit(2);
        }
    };

    let bind = std::env::var("YT_BIND").unwrap_or_else(|_| "127.0.0.1:7880".to_string());
    let workers = std::env::var("YT_GATEWAY_WORKERS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(8);
    let max_body = std::env::var("YT_MAX_BODY")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0);

    let mut server = RemoteGatewayServer::new(gateway);
    if let Ok(token) = std::env::var("YT_TOKEN") {
        if !token.trim().is_empty() {
            server = server.with_auth_token(token);
        }
    }
    if let Some(bytes) = max_body {
        server = server.with_max_body(bytes);
    }

    let listener = TcpListener::bind(&bind).expect("bind gateway server");
    eprintln!("yiTrace gateway listening on http://{bind} with {workers} workers");
    Arc::new(server).serve_pool(listener, workers);
}

fn gateway_from_env() -> Result<RemoteShardGateway, String> {
    if let Some(path) = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("YT_ROUTE_TABLE").ok())
        .filter(|value| !value.trim().is_empty())
    {
        let body =
            std::fs::read_to_string(&path).map_err(|e| format!("read route table {path}: {e}"))?;
        return RemoteShardGateway::from_route_table_json(&body);
    }

    let shards = std::env::var("YT_SHARDS")
        .map_err(|_| "set YT_ROUTE_TABLE, pass a route table path, or set YT_SHARDS".to_string())?
        .split(',')
        .filter_map(|addr| {
            let addr = addr.trim();
            if addr.is_empty() {
                None
            } else {
                Some(addr.to_string())
            }
        })
        .collect::<Vec<_>>();
    RemoteShardGateway::new(shards)
}
