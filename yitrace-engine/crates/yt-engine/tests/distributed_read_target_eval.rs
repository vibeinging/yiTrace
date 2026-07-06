use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use yt_engine::RemoteShardGateway;

struct ReadTargetServer {
    addr: String,
    requests: Arc<Mutex<Vec<String>>>,
    handle: Option<JoinHandle<()>>,
}

impl ReadTargetServer {
    fn spawn(committed_tail: u64, trace_id: u64, label: &'static str, max_requests: usize) -> Self {
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
                        let mut request = [0u8; 8192];
                        let n = stream.read(&mut request).unwrap_or(0);
                        let raw = String::from_utf8_lossy(&request[..n]).to_string();
                        thread_requests.lock().unwrap().push(raw.clone());
                        let body = if raw.contains("GET /v1/replication/status ") {
                            format!(
                                r#"{{"committedTail":{committed_tail},"manifestVersion":1,"memtableWatermark":0,"memtableRows":0,"segmentCount":0}}"#
                            )
                        } else if raw.contains("POST /v1/snapshots/lease ")
                            || raw.contains("POST /v1/snapshots/renew ")
                        {
                            format!(
                                r#"{{"snapshot":{{"mode":"single_node","leaseId":"lease-{label}","shards":[{{"shardId":"local","manifestVersion":1}}]}},"leaseId":"lease-{label}","leaseState":"active"}}"#
                            )
                        } else if raw.contains("DELETE /v1/snapshots/") {
                            format!(
                                r#"{{"released":true,"leaseId":"lease-{label}","leaseState":"released"}}"#
                            )
                        } else if raw.contains("POST /v1/trace-search ") {
                            format!(
                                r#"{{"items":[{{"traceId":{trace_id},"spanId":1,"rank":0,"attrs":{{"project_id":"{label}"}}}}],"nextCursor":null,"total":1,"snapshot":{{"mode":"single_node","leaseId":"lease-{label}","shards":[{{"shardId":"local","manifestVersion":1}}]}}}}"#
                            )
                        } else if raw.contains("POST /v1/vector-index ") {
                            r#"{"ok":true,"vectorIndex":"vector_namespace_flat"}"#.to_string()
                        } else if raw.contains("POST /v1/vector-search ") {
                            let score = if label.contains("fresh") { 0.9 } else { 0.7 };
                            let distance = if label.contains("fresh") { 0.1 } else { 0.3 };
                            format!(
                                r#"{{"items":[{{"namespace":"task","key":"task-{label}","tenantId":null,"traceId":{trace_id},"spanId":null,"distance":{distance},"score":{score},"attrs":{{"project_id":"{label}"}}}}],"total":1,"vectorIndex":"vector_namespace_flat"}}"#
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

    fn trace_search_count(&self) -> usize {
        self.requests()
            .iter()
            .filter(|request| request.contains("POST /v1/trace-search "))
            .count()
    }
}

#[test]
fn remote_gateway_fanout_merges_vector_namespace_search() {
    let shard_a = ReadTargetServer::spawn(10, 94_001, "vector-fresh-a", 4);
    let shard_b = ReadTargetServer::spawn(10, 94_002, "vector-cold-b", 4);
    let gateway = RemoteShardGateway::new(vec![
        format!("http://{}", shard_a.addr),
        format!("http://{}", shard_b.addr),
    ])
    .unwrap();

    let (status, indexed) = gateway.route_with_tenant(
        "POST",
        "/v1/vector-index",
        r#"{"namespace":"task","key":"native-pack","vector":[0.0,0.0],"attrs":{"project_id":"remote-vector"}}"#,
        Some(94),
    );
    assert_eq!(status, 200, "{indexed}");
    assert_contains(&indexed, r#""queryMode":"process_gateway_route""#);
    assert_contains(&indexed, r#""vectorIndex":"vector_namespace_flat""#);

    let (status, page) = gateway.route_with_tenant(
        "POST",
        "/v1/vector-search",
        r#"{"namespace":"task","vector":[0.1,0.1],"k":2,"filter":{"attrs":{"project_id":"remote-vector"}}}"#,
        Some(94),
    );
    assert_eq!(status, 200, "{page}");
    assert_contains(&page, r#""queryMode":"process_gateway_fanout""#);
    assert_contains(&page, r#""vectorIndex":"fanout_vector_namespace_flat""#);
    assert_contains(&page, r#""okShards":2"#);
    assert!(
        page.find("task-vector-fresh-a").unwrap() < page.find("task-vector-cold-b").unwrap(),
        "higher-score vector hit should sort first: {page}"
    );
}

#[test]
fn remote_gateway_explicit_snapshot_lease_renew_release_and_replay() {
    let shard_a = ReadTargetServer::spawn(10, 95_001, "lease-a", 8);
    let shard_b = ReadTargetServer::spawn(10, 95_002, "lease-b", 8);
    let gateway = RemoteShardGateway::new(vec![
        format!("http://{}", shard_a.addr),
        format!("http://{}", shard_b.addr),
    ])
    .unwrap();

    let (status, lease) = gateway.route_with_tenant("POST", "/v1/snapshots/lease", "{}", Some(95));
    assert_eq!(status, 200, "{lease}");
    assert_contains(&lease, r#""leaseId":"remote-lease-1""#);
    assert_contains(&lease, r#""leaseState":"active""#);
    assert_contains(&lease, r#""snapshot":{"mode":"remote_gateway""#);
    assert_contains(&lease, r#""leaseId":"lease-lease-a""#);
    assert_contains(&lease, r#""leaseId":"lease-lease-b""#);

    let compact_snapshot = r#"{"snapshot":{"mode":"remote_gateway","leaseId":"remote-lease-1","shards":[]},"limit":10}"#;
    let (status, page) =
        gateway.route_with_tenant("POST", "/v1/trace-search", compact_snapshot, Some(95));
    assert_eq!(status, 200, "{page}");
    assert_contains(&page, r#""traceId":95001"#);
    assert_contains(&page, r#""traceId":95002"#);
    assert_contains(&page, r#""snapshot":{"mode":"remote_gateway""#);

    let (status, renewed) = gateway.route_with_tenant(
        "POST",
        "/v1/snapshots/renew",
        r#"{"leaseId":"remote-lease-1"}"#,
        Some(95),
    );
    assert_eq!(status, 200, "{renewed}");
    assert_contains(&renewed, r#""leaseId":"remote-lease-1""#);
    assert_contains(&renewed, r#""leaseState":"active""#);

    let (status, released) =
        gateway.route_with_tenant("DELETE", "/v1/snapshots/remote-lease-1", "", Some(95));
    assert_eq!(status, 200, "{released}");
    assert_contains(&released, r#""released":true"#);
    assert_contains(&released, r#""releasedShards":2"#);

    let (status, expired) = gateway.route_with_tenant(
        "POST",
        "/v1/snapshots/renew",
        r#"{"leaseId":"remote-lease-1"}"#,
        Some(95),
    );
    assert_eq!(status, 409, "{expired}");
    assert_contains(&expired, r#""code":"snapshot_expired""#);

    let (status, replay_after_release) =
        gateway.route_with_tenant("POST", "/v1/trace-search", compact_snapshot, Some(95));
    assert_eq!(status, 409, "{replay_after_release}");
    assert_contains(&replay_after_release, r#""code":"snapshot_expired""#);
}

#[test]
fn remote_gateway_snapshot_lease_ttl_expires_without_release() {
    let shard_a = ReadTargetServer::spawn(10, 95_101, "ttl-a", 2);
    let shard_b = ReadTargetServer::spawn(10, 95_102, "ttl-b", 2);
    let gateway = RemoteShardGateway::new(vec![
        format!("http://{}", shard_a.addr),
        format!("http://{}", shard_b.addr),
    ])
    .unwrap();

    let (status, lease) =
        gateway.route_with_tenant("POST", "/v1/snapshots/lease", r#"{"ttlNs":1}"#, Some(95));
    assert_eq!(status, 200, "{lease}");
    assert_contains(&lease, r#""leaseId":"remote-lease-1""#);
    assert_contains(&lease, r#""expiresAtNs":"#);

    std::thread::sleep(Duration::from_millis(2));
    let compact_snapshot = r#"{"snapshot":{"mode":"remote_gateway","leaseId":"remote-lease-1","shards":[]},"limit":10}"#;
    let (status, expired) =
        gateway.route_with_tenant("POST", "/v1/trace-search", compact_snapshot, Some(95));
    assert_eq!(status, 409, "{expired}");
    assert_contains(&expired, r#""code":"snapshot_expired""#);
}

impl Drop for ReadTargetServer {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn assert_contains(body: &str, needle: &str) {
    assert!(body.contains(needle), "missing {needle:?} in {body}");
}

#[test]
fn remote_gateway_bounded_stale_reads_fresh_follower_and_falls_back_when_stale() {
    let leader = ReadTargetServer::spawn(10, 92_001, "leader-fresh", 4);
    let follower = ReadTargetServer::spawn(10, 92_002, "follower-fresh", 4);
    let table = format!(
        r#"{{
          "routeTableVersion":80,
          "shards":[
            {{
              "shardId":"logical-a",
              "replicas":[
                {{"replicaId":"a-leader","addr":"http://{}","role":"leader","readable":true,"writable":true}},
                {{"replicaId":"a-follower","addr":"http://{}","role":"follower","readable":true,"writable":false,"maxLagLsn":0,"priority":20}}
              ]
            }}
          ]
        }}"#,
        leader.addr, follower.addr
    );
    let gateway = RemoteShardGateway::from_route_table_json(&table).unwrap();
    let (status, health) =
        gateway.route_with_tenant("POST", "/v1/cluster/health/refresh", "", None);
    assert_eq!(status, 200, "{health}");
    assert_contains(&health, r#""replicaId":"a-follower""#);
    assert_contains(&health, r#""replicationLagLsn":0"#);

    let (status, page) =
        gateway.route_with_tenant("POST", "/v1/trace-search", r#"{"limit":10}"#, Some(92));
    assert_eq!(status, 200, "{page}");
    assert_contains(&page, r#""traceId":92002"#);
    assert_contains(&page, r#""readTargets":["#);
    assert_contains(&page, r#""replicaId":"a-follower""#);
    assert_contains(&page, r#""reason":"bounded_stale_follower""#);
    assert_contains(&page, r#""snapshot":{"mode":"remote_gateway""#);
    assert_contains(&page, r#""replicaId":"a-follower""#);
    assert_eq!(
        leader.trace_search_count(),
        0,
        "fresh follower should serve reads"
    );
    assert_eq!(follower.trace_search_count(), 1);

    let stale_leader = ReadTargetServer::spawn(10, 93_001, "leader-stale", 4);
    let stale_follower = ReadTargetServer::spawn(8, 93_002, "follower-stale", 4);
    let stale_table = format!(
        r#"{{
          "routeTableVersion":81,
          "shards":[
            {{
              "shardId":"logical-b",
              "replicas":[
                {{"replicaId":"b-leader","addr":"http://{}","role":"leader","readable":true,"writable":true}},
                {{"replicaId":"b-follower","addr":"http://{}","role":"follower","readable":true,"writable":false,"maxLagLsn":1,"priority":20}}
              ]
            }}
          ]
        }}"#,
        stale_leader.addr, stale_follower.addr
    );
    let stale_gateway = RemoteShardGateway::from_route_table_json(&stale_table).unwrap();
    let (status, health) =
        stale_gateway.route_with_tenant("POST", "/v1/cluster/health/refresh", "", None);
    assert_eq!(status, 200, "{health}");
    assert_contains(&health, r#""replicaId":"b-follower""#);
    assert_contains(&health, r#""health":"stale""#);

    let (status, page) =
        stale_gateway.route_with_tenant("POST", "/v1/trace-search", r#"{"limit":10}"#, Some(93));
    assert_eq!(status, 200, "{page}");
    assert_contains(&page, r#""traceId":93001"#);
    assert_contains(&page, r#""replicaId":"b-leader""#);
    assert_contains(&page, r#""reason":"leader_fallback""#);
    assert_eq!(stale_leader.trace_search_count(), 1);
    assert_eq!(
        stale_follower.trace_search_count(),
        0,
        "stale follower must not serve reads"
    );
}
