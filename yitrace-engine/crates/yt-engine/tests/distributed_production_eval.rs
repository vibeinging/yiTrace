use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use yt_engine::{RemoteShardClient, RemoteShardGateway};

struct SequenceServer {
    addr: String,
    hits: Arc<AtomicUsize>,
    handle: Option<JoinHandle<()>>,
}

impl SequenceServer {
    fn spawn(statuses: Vec<u16>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let hits = Arc::new(AtomicUsize::new(0));
        let thread_hits = Arc::clone(&hits);
        let handle = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_millis(800);
            while Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
                        let mut request = [0u8; 2048];
                        let _ = stream.read(&mut request);
                        let idx = thread_hits.fetch_add(1, Ordering::SeqCst);
                        let status = statuses
                            .get(idx)
                            .copied()
                            .or_else(|| statuses.last().copied())
                            .unwrap_or(200);
                        let reason = if status == 200 { "OK" } else { "Unavailable" };
                        let body = if status == 200 {
                            format!(r#"{{"ok":true,"attempt":{idx}}}"#)
                        } else {
                            format!(r#"{{"error":"attempt-{idx}"}}"#)
                        };
                        let response = format!(
                            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = stream.write_all(response.as_bytes());
                        if idx + 1 >= statuses.len() {
                            break;
                        }
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            addr,
            hits,
            handle: Some(handle),
        }
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

impl Drop for SequenceServer {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

struct SnapshotServer {
    addr: String,
    bodies: Arc<Mutex<Vec<String>>>,
    handle: Option<JoinHandle<()>>,
}

impl SnapshotServer {
    fn spawn(
        shard_id: &'static str,
        lease_id: &'static str,
        trace_id: u64,
        max_requests: usize,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let thread_bodies = Arc::clone(&bodies);
        let handle = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut handled = 0usize;
            while Instant::now() < deadline && handled < max_requests {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
                        let mut request = [0u8; 8192];
                        let n = stream.read(&mut request).unwrap_or(0);
                        let raw = String::from_utf8_lossy(&request[..n]).to_string();
                        let body = raw.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
                        thread_bodies.lock().unwrap().push(body);
                        let is_aggregate = raw.contains("POST /v1/trace-aggregate ");
                        let snapshot = format!(
                            r#"{{"mode":"in_process_cluster","leaseId":"{}","shards":[{{"shardId":"{}-local","manifestVersion":7}}]}}"#,
                            lease_id, shard_id
                        );
                        let response_body = if is_aggregate {
                            format!(
                                r#"{{"items":[{{"key":{{"project_id":"remote-snapshot"}},"spanCount":1,"traceCount":1,"errorCount":0,"durationNs":{{"sum":1,"max":1,"count":1}},"usage":{{"inputTokens":0,"outputTokens":0,"cachedInputTokens":0,"reasoningTokens":0,"totalTokens":0}},"examples":[]}}],"total":1,"spanTotal":1,"snapshot":{snapshot}}}"#
                            )
                        } else {
                            format!(
                                r#"{{"items":[{{"traceId":{},"spanId":1,"rank":0,"attrs":{{"project_id":"remote-snapshot"}}}}],"nextCursor":null,"total":1,"index":"materialized","snapshot":{snapshot}}}"#,
                                trace_id
                            )
                        };
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            response_body.len(),
                            response_body
                        );
                        let _ = stream.write_all(response.as_bytes());
                        handled += 1;
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            addr,
            bodies,
            handle: Some(handle),
        }
    }

    fn bodies(&self) -> Vec<String> {
        self.bodies.lock().unwrap().clone()
    }
}

impl Drop for SnapshotServer {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

struct CaptureServer {
    addr: String,
    requests: Arc<Mutex<Vec<String>>>,
    handle: Option<JoinHandle<()>>,
}

fn read_http_request(stream: &mut TcpStream, max_bytes: usize) -> String {
    let mut raw = Vec::new();
    let deadline = Instant::now() + Duration::from_millis(500);
    while raw.len() < max_bytes && Instant::now() < deadline {
        let mut chunk = [0u8; 4096];
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                raw.extend_from_slice(&chunk[..n]);
                if http_request_complete(&raw) {
                    break;
                }
            }
            Err(err)
                if err.kind() == std::io::ErrorKind::WouldBlock
                    || err.kind() == std::io::ErrorKind::TimedOut =>
            {
                if http_request_complete(&raw) {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&raw).to_string()
}

fn http_request_complete(raw: &[u8]) -> bool {
    let Some(header_end) = raw.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    let header_text = String::from_utf8_lossy(&raw[..header_end]);
    let content_len = header_text
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.eq_ignore_ascii_case("content-length") {
                value.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);
    raw.len() >= header_end + 4 + content_len
}

impl CaptureServer {
    fn spawn(max_requests: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let thread_requests = Arc::clone(&requests);
        let handle = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut handled = 0usize;
            while Instant::now() < deadline && handled < max_requests {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
                        let raw = read_http_request(&mut stream, 16 * 1024);
                        thread_requests.lock().unwrap().push(raw.clone());
                        let body = if raw.contains("POST /v1/ingest ") {
                            let request_body = raw.split("\r\n\r\n").nth(1).unwrap_or("");
                            let count = request_body.matches(r#""trace_id""#).count().max(1);
                            format!(r#"{{"ingested":{count}}}"#)
                        } else {
                            r#"{"mode":"single_node","shards":[]}"#.to_string()
                        };
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        let _ = stream.write_all(response.as_bytes());
                        handled += 1;
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            addr,
            requests,
            handle: Some(handle),
        }
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    fn ingest_count(&self) -> usize {
        self.requests()
            .iter()
            .filter(|request| request.contains("POST /v1/ingest "))
            .count()
    }
}

impl Drop for CaptureServer {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

struct ReplicationStatusServer {
    addr: String,
    requests: Arc<Mutex<Vec<String>>>,
    handle: Option<JoinHandle<()>>,
}

impl ReplicationStatusServer {
    fn spawn(committed_tail: u64, max_requests: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let thread_requests = Arc::clone(&requests);
        let handle = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut handled = 0usize;
            while Instant::now() < deadline && handled < max_requests {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
                        let mut request = [0u8; 4096];
                        let n = stream.read(&mut request).unwrap_or(0);
                        let raw = String::from_utf8_lossy(&request[..n]).to_string();
                        thread_requests.lock().unwrap().push(raw.clone());
                        let body = if raw.contains("GET /v1/replication/status ") {
                            format!(
                                r#"{{"committedTail":{committed_tail},"manifestVersion":1,"memtableWatermark":0,"memtableRows":0,"segmentCount":0}}"#
                            )
                        } else {
                            format!(
                                r#"{{"mode":"single_node","shards":[{{"shardId":"local","committedTail":{committed_tail}}}]}}"#
                            )
                        };
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        let _ = stream.write_all(response.as_bytes());
                        handled += 1;
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            addr,
            requests,
            handle: Some(handle),
        }
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for ReplicationStatusServer {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn unused_addr() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().to_string()
}

fn assert_contains(body: &str, needle: &str) {
    assert!(body.contains(needle), "missing {needle:?} in {body}");
}

fn extract_json_object_field(body: &str, field: &str) -> String {
    let needle = format!("\"{field}\":");
    let value_start = body
        .find(&needle)
        .unwrap_or_else(|| panic!("missing JSON field {field} in {body}"))
        + needle.len();
    let object_start = body[value_start..]
        .find('{')
        .map(|pos| value_start + pos)
        .unwrap_or_else(|| panic!("field {field} is not an object in {body}"));
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, ch) in body[object_start..].char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return body[object_start..=object_start + offset].to_string();
                }
            }
            _ => {}
        }
    }
    panic!("unterminated object field {field} in {body}");
}

#[test]
fn remote_shard_client_retries_retryable_socket_calls() {
    let server = SequenceServer::spawn(vec![503, 200]);
    let client = RemoteShardClient::new(format!("http://{}", server.addr))
        .with_retry(2, Duration::ZERO)
        .with_timeout(Duration::from_millis(300));

    let (status, body) = client
        .route_json_with_tenant("GET", "/v1/cluster/shards", "", None)
        .unwrap();

    assert_eq!(status, 200, "{body}");
    assert_eq!(server.hits(), 2, "retry should issue a second socket call");
}

#[test]
fn remote_shard_client_does_not_retry_unsafe_writes_by_default() {
    let server = SequenceServer::spawn(vec![503, 200]);
    let client = RemoteShardClient::new(format!("http://{}", server.addr))
        .with_retry(2, Duration::ZERO)
        .with_timeout(Duration::from_millis(300));

    let (status, body) = client
        .route_json_with_tenant("POST", "/v1/annotations", r#"{"traceId":1}"#, Some(7))
        .unwrap();

    assert_eq!(status, 503, "{body}");
    assert_eq!(
        server.hits(),
        1,
        "metadata creation should not be replayed without an idempotency key"
    );
}

#[test]
fn remote_shard_client_circuit_breaker_blocks_then_half_opens() {
    let server = SequenceServer::spawn(vec![503, 200]);
    let client = RemoteShardClient::new(format!("http://{}", server.addr))
        .with_retry(1, Duration::ZERO)
        .with_circuit_breaker(1, Duration::from_millis(40))
        .with_timeout(Duration::from_millis(300));

    let (first_status, first_body) = client
        .route_json_with_tenant("GET", "/v1/cluster/shards", "", None)
        .unwrap();
    assert_eq!(first_status, 503, "{first_body}");
    assert_eq!(server.hits(), 1);

    let blocked = client
        .route_json_with_tenant("GET", "/v1/cluster/shards", "", None)
        .unwrap_err();
    assert!(
        blocked.contains("circuit breaker open"),
        "unexpected error: {blocked}"
    );
    assert_eq!(server.hits(), 1, "open circuit should not hit the socket");

    std::thread::sleep(Duration::from_millis(60));
    let (recovered_status, recovered_body) = client
        .route_json_with_tenant("GET", "/v1/cluster/shards", "", None)
        .unwrap();
    assert_eq!(recovered_status, 200, "{recovered_body}");
    assert_eq!(server.hits(), 2, "half-open probe should reach the shard");
}

#[test]
fn remote_gateway_builds_from_versioned_route_table() {
    let server = SequenceServer::spawn(vec![200]);
    let table = format!(
        r#"{{
          "routeTableVersion":88,
          "shards":[
            {{"shardId":"route-s0","addr":"http://{}","role":"leader","readable":true,"writable":true,"weight":3}},
            {{"shardId":"route-s0-r1","addr":"127.0.0.1:1","role":"follower","readable":true,"writable":false}}
          ]
        }}"#,
        server.addr
    );
    let gateway = RemoteShardGateway::from_route_table_json(&table).unwrap();
    assert_eq!(gateway.shard_count(), 1);
    assert_eq!(gateway.route_table_version(), Some(88));

    let (status, cluster) = gateway.route_with_tenant("GET", "/v1/cluster/shards", "", None);

    assert_eq!(status, 200, "{cluster}");
    assert!(cluster.contains(r#""routeTableVersion":88"#), "{cluster}");
    assert!(cluster.contains(r#""shardId":"route-s0""#), "{cluster}");
    assert!(cluster.contains(r#""role":"leader""#), "{cluster}");
    assert!(cluster.contains(r#""weight":3"#), "{cluster}");
    assert_eq!(server.hits(), 1);
}

#[test]
fn remote_gateway_route_table_v2_manual_promote_switches_writable_replica() {
    let primary = CaptureServer::spawn(4);
    let follower = CaptureServer::spawn(4);
    let table_v1 = format!(
        r#"{{
          "routeTableVersion":50,
          "shards":[
            {{
              "shardId":"logical-a",
              "replicas":[
                {{"replicaId":"a-primary","addr":"http://{}","role":"leader","readable":true,"writable":true,"priority":10}},
                {{"replicaId":"a-follower","addr":"http://{}","role":"follower","readable":true,"writable":false,"maxLagLsn":3}}
              ]
            }}
          ]
        }}"#,
        primary.addr, follower.addr
    );
    let gateway = RemoteShardGateway::from_route_table_json(&table_v1).unwrap();
    assert_eq!(gateway.shard_count(), 1);

    let (status, cluster) = gateway.route_with_tenant("GET", "/v1/cluster/shards", "", None);
    assert_eq!(status, 200, "{cluster}");
    assert_contains(&cluster, r#""shardId":"logical-a""#);
    assert_contains(&cluster, r#""replicaId":"a-primary""#);
    assert_contains(&cluster, r#""replicas":["#);
    assert_contains(&cluster, r#""replicaId":"a-follower""#);
    assert_contains(&cluster, r#""maxLagLsn":3"#);

    let first = r#"[{"trace_id":51001,"span_id":1,"session_id":510,"ts":10,"seq":1,"event_type":2,"ext_span_id":"51001-1","attrs":{"project_id":"manual-promote"}}]"#;
    let (status, written) = gateway.route_with_tenant("POST", "/v1/ingest", first, Some(510));
    assert_eq!(status, 200, "{written}");
    assert_contains(&written, r#""ingested":1"#);
    assert_eq!(primary.ingest_count(), 1);
    assert_eq!(follower.ingest_count(), 0);

    let table_v2 = format!(
        r#"{{
          "routeTableVersion":51,
          "shards":[
            {{
              "shardId":"logical-a",
              "replicas":[
                {{"replicaId":"a-primary","addr":"http://{}","role":"follower","readable":true,"writable":false}},
                {{"replicaId":"a-follower","addr":"http://{}","role":"leader","readable":true,"writable":true,"priority":20}}
              ]
            }}
          ]
        }}"#,
        primary.addr, follower.addr
    );
    let (status, reload) =
        gateway.route_with_tenant("POST", "/v1/cluster/route-table/reload", &table_v2, None);
    assert_eq!(status, 200, "{reload}");
    assert_contains(&reload, r#""routeTableVersion":51"#);
    assert_eq!(gateway.shard_count(), 1);

    let second = r#"[{"trace_id":51002,"span_id":1,"session_id":510,"ts":20,"seq":1,"event_type":2,"ext_span_id":"51002-1","attrs":{"project_id":"manual-promote"}}]"#;
    let (status, written) = gateway.route_with_tenant("POST", "/v1/ingest", second, Some(510));
    assert_eq!(status, 200, "{written}");
    assert_contains(&written, r#""ingested":1"#);
    assert_eq!(
        primary.ingest_count(),
        1,
        "old leader must not receive new writes"
    );
    assert_eq!(
        follower.ingest_count(),
        1,
        "promoted follower should receive new writes"
    );

    let (status, cluster) = gateway.route_with_tenant("GET", "/v1/cluster/shards", "", None);
    assert_eq!(status, 200, "{cluster}");
    assert_contains(&cluster, r#""routeTableVersion":51"#);
    assert_contains(&cluster, r#""replicaId":"a-follower""#);
    assert_contains(&cluster, r#""priority":20"#);
}

#[test]
fn remote_gateway_route_table_reload_file_switches_writer() {
    let primary = CaptureServer::spawn(4);
    let follower = CaptureServer::spawn(4);
    let route_dir = std::env::temp_dir().join(format!(
        "yt_route_table_file_{}_{}",
        std::process::id(),
        primary.addr.replace([':', '.'], "_")
    ));
    let _ = std::fs::remove_dir_all(&route_dir);
    std::fs::create_dir_all(&route_dir).unwrap();
    let route_path = route_dir.join("routes.json");
    let table_v1 = format!(
        r#"{{
          "routeTableVersion":90,
          "shards":[
            {{
              "shardId":"file-logical-a",
              "replicas":[
                {{"replicaId":"file-primary","addr":"http://{}","role":"leader","readable":true,"writable":true}},
                {{"replicaId":"file-follower","addr":"http://{}","role":"follower","readable":true,"writable":false}}
              ]
            }}
          ]
        }}"#,
        primary.addr, follower.addr
    );
    let gateway = RemoteShardGateway::from_route_table_json(&table_v1).unwrap();
    let first = r#"[{"trace_id":90001,"span_id":1,"session_id":900,"ts":10,"seq":1,"event_type":2,"ext_span_id":"90001-1","attrs":{"project_id":"file-route"}}]"#;
    let (status, written) = gateway.route_with_tenant("POST", "/v1/ingest", first, Some(900));
    assert_eq!(status, 200, "{written}");
    assert_eq!(primary.ingest_count(), 1);
    assert_eq!(follower.ingest_count(), 0);

    let table_v2 = format!(
        r#"{{
          "routeTableVersion":91,
          "shards":[
            {{
              "shardId":"file-logical-a",
              "replicas":[
                {{"replicaId":"file-primary","addr":"http://{}","role":"follower","readable":true,"writable":false}},
                {{"replicaId":"file-follower","addr":"http://{}","role":"leader","readable":true,"writable":true}}
              ]
            }}
          ]
        }}"#,
        primary.addr, follower.addr
    );
    std::fs::write(&route_path, table_v2).unwrap();
    let reload_body = format!(r#"{{"path":"{}"}}"#, route_path.display());
    let (status, reloaded) = gateway.route_with_tenant(
        "POST",
        "/v1/cluster/route-table/reload-file",
        &reload_body,
        None,
    );
    assert_eq!(status, 200, "{reloaded}");
    assert_contains(&reloaded, r#""routeTableVersion":91"#);
    assert_contains(&reloaded, r#""source":"file""#);

    let second = r#"[{"trace_id":90002,"span_id":1,"session_id":900,"ts":20,"seq":1,"event_type":2,"ext_span_id":"90002-1","attrs":{"project_id":"file-route"}}]"#;
    let (status, written) = gateway.route_with_tenant("POST", "/v1/ingest", second, Some(900));
    assert_eq!(status, 200, "{written}");
    assert_eq!(primary.ingest_count(), 1);
    assert_eq!(follower.ingest_count(), 1);
    let _ = std::fs::remove_dir_all(route_dir);
}

#[test]
fn remote_gateway_health_refresh_reports_replica_lag_and_unreachable() {
    let leader = ReplicationStatusServer::spawn(5, 3);
    let follower = ReplicationStatusServer::spawn(3, 2);
    let dead_addr = unused_addr();
    let table = format!(
        r#"{{
          "routeTableVersion":60,
          "shards":[
            {{
              "shardId":"logical-a",
              "replicas":[
                {{"replicaId":"a-leader","addr":"http://{}","role":"leader","readable":true,"writable":true}},
                {{"replicaId":"a-follower","addr":"http://{}","role":"follower","readable":true,"writable":false,"maxLagLsn":1}}
              ]
            }},
            {{
              "shardId":"logical-b",
              "replicas":[
                {{"replicaId":"b-leader","addr":"http://{}","role":"leader","readable":true,"writable":true}}
              ]
            }}
          ]
        }}"#,
        leader.addr, follower.addr, dead_addr
    );
    let gateway = RemoteShardGateway::from_route_table_json(&table).unwrap();

    let (status, empty_health) = gateway.route_with_tenant("GET", "/v1/cluster/health", "", None);
    assert_eq!(status, 200, "{empty_health}");
    assert_contains(&empty_health, r#""replicaCount":0"#);

    let (status, health) =
        gateway.route_with_tenant("POST", "/v1/cluster/health/refresh", "", None);
    assert_eq!(status, 200, "{health}");
    assert_contains(&health, r#""routeTableVersion":60"#);
    assert_contains(&health, r#""replicaCount":3"#);
    assert_contains(&health, r#""replicaId":"a-leader""#);
    assert_contains(&health, r#""health":"healthy""#);
    assert_contains(&health, r#""replicaId":"a-follower""#);
    assert_contains(&health, r#""health":"stale""#);
    assert_contains(&health, r#""replicationLagLsn":2"#);
    assert_contains(&health, r#""reason":"lag_exceeds_budget""#);
    assert_contains(&health, r#""replicaId":"b-leader""#);
    assert_contains(&health, r#""health":"unreachable""#);

    let (status, cluster) = gateway.route_with_tenant("GET", "/v1/cluster/shards", "", None);
    assert_eq!(status, 200, "{cluster}");
    assert_contains(&cluster, r#""shardId":"logical-a""#);
    assert_contains(&cluster, r#""replicas":["#);
    assert_contains(&cluster, r#""health":"stale""#);
    assert_contains(&cluster, r#""replicationLagLsn":2"#);
    assert_contains(&cluster, r#""healthReason":"lag_exceeds_budget""#);
    assert_contains(&cluster, r#""shardId":"logical-b""#);
    assert_contains(&cluster, r#""health":"unreachable""#);
    assert!(
        leader
            .requests()
            .iter()
            .any(|request| request.contains("GET /v1/replication/status ")),
        "leader should be probed through replication status"
    );
    assert!(
        follower
            .requests()
            .iter()
            .any(|request| request.contains("GET /v1/replication/status ")),
        "follower should be probed through replication status"
    );
}

#[test]
fn remote_gateway_strict_consistency_rejects_partial_trace_search() {
    let shard = SnapshotServer::spawn("strict-a", "lease-strict-a", 91_001, 2);
    let dead_addr = unused_addr();
    let table = format!(
        r#"{{
          "routeTableVersion":70,
          "shards":[
            {{"shardId":"strict-a","addr":"http://{}","role":"leader","readable":true,"writable":true}},
            {{"shardId":"strict-b","addr":"http://{}","role":"leader","readable":true,"writable":true}}
          ]
        }}"#,
        shard.addr, dead_addr
    );
    let gateway = RemoteShardGateway::from_route_table_json(&table).unwrap();

    let (status, partial) =
        gateway.route_with_tenant("POST", "/v1/trace-search", r#"{"limit":10}"#, Some(91));
    assert_eq!(status, 200, "{partial}");
    assert_contains(&partial, r#""queryMode":"process_gateway_fanout""#);
    assert_contains(&partial, r#""okShards":1"#);
    assert_contains(&partial, r#""degraded":true"#);
    assert_contains(&partial, r#""consistencyUsed":"partial""#);
    assert_contains(&partial, r#""partial":true"#);
    assert_contains(&partial, r#""traceId":91001"#);

    let (status, strict) = gateway.route_with_tenant(
        "POST",
        "/v1/trace-search",
        r#"{"limit":10,"consistency":"strict"}"#,
        Some(91),
    );
    assert_eq!(status, 502, "{strict}");
    assert_contains(
        &strict,
        r#""error":"strict consistency requires all shards""#,
    );
    assert_contains(&strict, r#""okShards":1"#);
    assert_contains(&strict, r#""degraded":true"#);
    assert_contains(&strict, r#""consistencyUsed":"strict""#);
    assert_contains(&strict, r#""partial":false"#);
    assert_contains(&strict, r#""failedShards":["#);
}

#[test]
fn remote_gateway_reloads_route_table_without_restart() {
    let old_server = SequenceServer::spawn(vec![200]);
    let new_server_a = SequenceServer::spawn(vec![200]);
    let new_server_b = SequenceServer::spawn(vec![200]);
    let table_v1 = format!(
        r#"{{
          "routeTableVersion":10,
          "shards":[
            {{"shardId":"route-old","addr":"http://{}","role":"leader","readable":true,"writable":true}}
          ]
        }}"#,
        old_server.addr
    );
    let gateway = RemoteShardGateway::from_route_table_json(&table_v1).unwrap();
    let (status, before) = gateway.route_with_tenant("GET", "/v1/cluster/shards", "", None);
    assert_eq!(status, 200, "{before}");
    assert!(before.contains(r#""routeTableVersion":10"#), "{before}");
    assert!(before.contains(r#""shardId":"route-old""#), "{before}");
    assert_eq!(gateway.shard_count(), 1);
    assert_eq!(old_server.hits(), 1);

    let table_v2 = format!(
        r#"{{
          "routeTableVersion":11,
          "shards":[
            {{"shardId":"route-new-a","addr":"http://{}","role":"leader","readable":true,"writable":true}},
            {{"shardId":"route-new-b","addr":"http://{}","role":"leader","readable":true,"writable":true}}
          ]
        }}"#,
        new_server_a.addr, new_server_b.addr
    );
    let (status, reload) =
        gateway.route_with_tenant("POST", "/v1/cluster/route-table/reload", &table_v2, None);
    assert_eq!(status, 200, "{reload}");
    assert!(reload.contains(r#""routeTableVersion":11"#), "{reload}");
    assert_eq!(gateway.shard_count(), 2);

    let (status, after) = gateway.route_with_tenant("GET", "/v1/cluster/shards", "", None);
    assert_eq!(status, 200, "{after}");
    assert!(after.contains(r#""routeTableVersion":11"#), "{after}");
    assert!(after.contains(r#""shardId":"route-new-a""#), "{after}");
    assert!(after.contains(r#""shardId":"route-new-b""#), "{after}");
    assert_eq!(
        old_server.hits(),
        1,
        "old shard must not be queried after reload"
    );
    assert_eq!(new_server_a.hits(), 1);
    assert_eq!(new_server_b.hits(), 1);

    let (status, stale) =
        gateway.route_with_tenant("POST", "/v1/cluster/route-table/reload", &table_v1, None);
    assert_eq!(status, 400, "{stale}");
    assert!(stale.contains("older than current 11"), "{stale}");
    assert_eq!(gateway.route_table_version(), Some(11));
}

#[test]
fn remote_gateway_snapshot_lease_round_trips_per_shard() {
    let shard_a = SnapshotServer::spawn("route-a", "lease-a", 88_001, 3);
    let shard_b = SnapshotServer::spawn("route-b", "lease-b", 88_002, 3);
    let table_v1 = format!(
        r#"{{
          "routeTableVersion":33,
          "shards":[
            {{"shardId":"route-a","addr":"http://{}","role":"leader","readable":true,"writable":true}},
            {{"shardId":"route-b","addr":"http://{}","role":"leader","readable":true,"writable":true}}
          ]
        }}"#,
        shard_a.addr, shard_b.addr
    );
    let gateway = RemoteShardGateway::from_route_table_json(&table_v1).unwrap();

    let query = r#"{"filter":{"projectId":"remote-snapshot"},"limit":10}"#;
    let (status, first) = gateway.route_with_tenant("POST", "/v1/trace-search", query, Some(88));
    assert_eq!(status, 200, "{first}");
    assert_contains(&first, r#""snapshot":{"mode":"remote_gateway""#);
    assert_contains(&first, r#""routeTableVersion":33"#);
    assert_contains(&first, r#""shardId":"route-a""#);
    assert_contains(&first, r#""shardId":"route-b""#);
    assert_contains(&first, r#""leaseId":"lease-a""#);
    assert_contains(&first, r#""leaseId":"lease-b""#);
    assert_contains(&first, r#""total":2"#);
    let snapshot = extract_json_object_field(&first, "snapshot");

    let replay = format!(
        r#"{{"filter":{{"projectId":"remote-snapshot"}},"limit":10,"snapshot":{snapshot}}}"#
    );
    let (status, second) = gateway.route_with_tenant("POST", "/v1/trace-search", &replay, Some(88));
    assert_eq!(status, 200, "{second}");
    assert_contains(&second, r#""total":2"#);
    let a_bodies = shard_a.bodies();
    let b_bodies = shard_b.bodies();
    assert!(
        a_bodies
            .iter()
            .any(|body| body.contains(r#""leaseId":"lease-a""#)),
        "route-a should receive its local lease on replay: {a_bodies:?}"
    );
    assert!(
        b_bodies
            .iter()
            .any(|body| body.contains(r#""leaseId":"lease-b""#)),
        "route-b should receive its local lease on replay: {b_bodies:?}"
    );
    assert!(
        !a_bodies
            .iter()
            .any(|body| body.contains(r#""leaseId":"lease-b""#)),
        "route-a must not receive route-b lease: {a_bodies:?}"
    );

    let aggregate = format!(
        r#"{{"filter":{{"projectId":"remote-snapshot"}},"groupBy":["projectId"],"limit":10,"snapshot":{snapshot}}}"#
    );
    let (status, aggregate_body) =
        gateway.route_with_tenant("POST", "/v1/trace-aggregate", &aggregate, Some(88));
    assert_eq!(status, 200, "{aggregate_body}");
    assert_contains(&aggregate_body, r#""spanTotal":2"#);
    assert_contains(&aggregate_body, r#""snapshot":{"mode":"remote_gateway""#);

    let replacement = SequenceServer::spawn(vec![200]);
    let table_v2 = format!(
        r#"{{
          "routeTableVersion":34,
          "shards":[
            {{"shardId":"route-c","addr":"http://{}","role":"leader","readable":true,"writable":true}}
          ]
        }}"#,
        replacement.addr
    );
    let (status, reload) =
        gateway.route_with_tenant("POST", "/v1/cluster/route-table/reload", &table_v2, None);
    assert_eq!(status, 200, "{reload}");
    let (status, stale) = gateway.route_with_tenant("POST", "/v1/trace-search", &replay, Some(88));
    assert_eq!(status, 409, "{stale}");
    assert_contains(&stale, r#""code":"route_table_expired""#);
}
