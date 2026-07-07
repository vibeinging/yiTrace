use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use yt_engine::{HttpIngestServer, RemoteGatewayServer, RemoteShardGateway, WriteCoordinator};

const CHILD_ENV: &str = "YT_CHAOS_EVAL_CHILD";
const GATEWAY_ENV: &str = "YT_CHAOS_EVAL_GATEWAY";
const CHILD_DIR_ENV: &str = "YT_CHAOS_EVAL_DIR";
const CHILD_BIND_ENV: &str = "YT_CHAOS_EVAL_BIND";
const ROUTE_TABLE_ENV: &str = "YT_CHAOS_EVAL_ROUTE_TABLE";

#[derive(Debug)]
struct RunningNode {
    addr: String,
    child: Option<Child>,
}

impl RunningNode {
    fn kill(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for RunningNode {
    fn drop(&mut self) {
        self.kill();
    }
}

fn durable_dir(name: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "yt_chaos_eval_{name}_{}_{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn free_addr() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().to_string()
}

fn spawn_shard(dir: &PathBuf, addr: &str) -> RunningNode {
    let exe = std::env::current_exe().unwrap();
    let child = Command::new(exe)
        .arg("chaos_shard_child_process")
        .arg("--ignored")
        .arg("--exact")
        .env(CHILD_ENV, "1")
        .env(CHILD_DIR_ENV, dir)
        .env(CHILD_BIND_ENV, addr)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut node = RunningNode {
        addr: addr.to_string(),
        child: Some(child),
    };
    wait_ready(&mut node);
    node
}

fn spawn_gateway(addr: &str, route_table_path: &PathBuf) -> RunningNode {
    let exe = std::env::current_exe().unwrap();
    let child = Command::new(exe)
        .arg("chaos_gateway_child_process")
        .arg("--ignored")
        .arg("--exact")
        .env(GATEWAY_ENV, "1")
        .env(CHILD_BIND_ENV, addr)
        .env(ROUTE_TABLE_ENV, route_table_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut node = RunningNode {
        addr: addr.to_string(),
        child: Some(child),
    };
    wait_ready(&mut node);
    node
}

fn wait_ready(node: &mut RunningNode) {
    let start = Instant::now();
    loop {
        if let Some(child) = node.child.as_mut() {
            if let Some(status) = child.try_wait().unwrap() {
                panic!("node {} exited before ready: {status}", node.addr);
            }
        }
        if let Ok((200, _)) = http_request(&node.addr, "GET", "/v1/cluster/shards", "", None) {
            return;
        }
        if start.elapsed() > Duration::from_secs(5) {
            panic!("node {} did not become ready", node.addr);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn http_request(
    addr: &str,
    method: &str,
    path: &str,
    body: &str,
    tenant: Option<u64>,
) -> std::io::Result<(u16, String)> {
    let mut stream = TcpStream::connect(addr)?;
    let tenant_header = tenant
        .map(|id| format!("X-Tenant-Id: {id}\r\n"))
        .unwrap_or_default();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{tenant_header}Connection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(req.as_bytes())?;
    stream.flush()?;

    let mut raw = String::new();
    stream.read_to_string(&mut raw)?;
    let status = raw
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(0);
    let body = raw
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
        .unwrap_or_default();
    Ok((status, body))
}

fn assert_http_status(
    addr: &str,
    method: &str,
    path: &str,
    body: &str,
    tenant: Option<u64>,
    expect: u16,
) -> String {
    let (status, response) = http_request(addr, method, path, body, tenant).unwrap();
    assert_eq!(
        status, expect,
        "{method} {addr}{path} expected {expect}, got {status}: {response}"
    );
    response
}

fn assert_http_contains(
    addr: &str,
    method: &str,
    path: &str,
    body: &str,
    tenant: Option<u64>,
    needle: &str,
) -> String {
    let response = assert_http_status(addr, method, path, body, tenant, 200);
    assert!(
        response.contains(needle),
        "missing {needle:?} from {method} {addr}{path}: {response}"
    );
    response
}

fn test_route_hash(tenant: Option<u64>, session_id: Option<u64>, trace_id: u64) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    fn mix(mut hash: u64, value: u64) -> u64 {
        for b in value.to_le_bytes() {
            hash ^= b as u64;
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        hash
    }
    let mut hash = FNV_OFFSET;
    hash = mix(hash, tenant.unwrap_or(0));
    match session_id {
        Some(session) => {
            hash = mix(hash, 1);
            mix(hash, session)
        }
        None => {
            hash = mix(hash, 0);
            mix(hash, trace_id)
        }
    }
}

fn session_for_gateway_shard(tenant: u64, shard: usize, shard_count: usize, start: u64) -> u64 {
    (start..start + 50_000)
        .find(|session| {
            (test_route_hash(Some(tenant), Some(*session), 0) as usize) % shard_count == shard
        })
        .unwrap_or_else(|| panic!("could not find session for shard {shard}"))
}

fn chaos_event(trace_id: u64, session_id: u64, shard: &str, phase: &str) -> String {
    format!(
        r#"{{"trace_id":{trace_id},"span_id":1,"session_id":{session_id},"ts":{trace_id},"seq":1,"event_type":2,"ext_span_id":"{trace_id}-1","status":0,"duration_ns":1000,"agent_name":"chaos-agent","output_text":"chaos {phase} {shard} trace {trace_id}","attrs":{{"project_id":"distributed-chaos-eval","skill":"chaos","mode":"{phase}","task_fingerprint":"distributed-chaos","phase":"{shard}"}}}}"#
    )
}

fn chaos_batch(events: &[(u64, u64, &str, &str)]) -> String {
    let items = events
        .iter()
        .map(|(trace_id, session_id, shard, phase)| {
            chaos_event(*trace_id, *session_id, shard, phase)
        })
        .collect::<Vec<_>>();
    format!("[{}]", items.join(","))
}

fn route_table_v1(a_leader: &str, a_follower: &str, b_leader: &str, b_follower: &str) -> String {
    format!(
        r#"{{
          "routeTableVersion":1,
          "shards":[
            {{
              "shardId":"logical-a",
              "replicas":[
                {{"replicaId":"a-leader","addr":"http://{a_leader}","role":"leader","readable":true,"writable":true}},
                {{"replicaId":"a-follower","addr":"http://{a_follower}","role":"follower","readable":true,"writable":false,"maxLagLsn":0,"priority":20}}
              ]
            }},
            {{
              "shardId":"logical-b",
              "replicas":[
                {{"replicaId":"b-leader","addr":"http://{b_leader}","role":"leader","readable":true,"writable":true}},
                {{"replicaId":"b-follower","addr":"http://{b_follower}","role":"follower","readable":true,"writable":false,"maxLagLsn":0,"priority":20}}
              ]
            }}
          ]
        }}"#
    )
}

fn route_table_v2_promote_a(
    a_old_leader: &str,
    a_new_leader: &str,
    b_leader: &str,
    b_follower: &str,
) -> String {
    format!(
        r#"{{
          "routeTableVersion":2,
          "shards":[
            {{
              "shardId":"logical-a",
              "replicas":[
                {{"replicaId":"a-follower","addr":"http://{a_new_leader}","role":"leader","readable":true,"writable":true}},
                {{"replicaId":"a-leader","addr":"http://{a_old_leader}","role":"follower","readable":true,"writable":false,"maxLagLsn":0,"priority":10}}
              ]
            }},
            {{
              "shardId":"logical-b",
              "replicas":[
                {{"replicaId":"b-leader","addr":"http://{b_leader}","role":"leader","readable":true,"writable":true}},
                {{"replicaId":"b-follower","addr":"http://{b_follower}","role":"follower","readable":true,"writable":false,"maxLagLsn":0,"priority":20}}
              ]
            }}
          ]
        }}"#
    )
}

fn pull_once(follower: &RunningNode, leader: &RunningNode) -> String {
    let body = format!(r#"{{"leaderAddr":"http://{}"}}"#, leader.addr);
    assert_http_contains(
        &follower.addr,
        "POST",
        "/v1/replication/pull",
        &body,
        None,
        r#""pulled":true"#,
    )
}

fn extract_object_field(body: &str, field: &str) -> String {
    let needle = format!(r#""{field}":"#);
    let mut pos = body
        .find(&needle)
        .unwrap_or_else(|| panic!("missing object field {field} from {body}"))
        + needle.len();
    while body
        .as_bytes()
        .get(pos)
        .is_some_and(u8::is_ascii_whitespace)
    {
        pos += 1;
    }
    assert_eq!(
        body.as_bytes().get(pos).copied(),
        Some(b'{'),
        "field {field} is not an object in {body}"
    );
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escape = false;
    for (offset, ch) in body[pos..].char_indices() {
        if in_string {
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
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
                    return body[pos..pos + offset + 1].to_string();
                }
            }
            _ => {}
        }
    }
    panic!("unterminated object field {field} in {body}");
}

/// 真实分布式 chaos eval：
///
/// - 起 2 个 logical shard，每个 shard 一个 leader + 一个 follower，共 5 个进程（含 gateway）。
/// - 先写入并把 follower 拉平，验证默认读会走 follower。
/// - kill logical-a leader，strict 查询必须失败，不能悄悄从旧 follower 补结果。
/// - promote logical-a follower，gateway reload route table 后继续写。
/// - 旧 leader 重启后仍不能收到新写，证明 route table promote 后没有双写回退。
/// - old snapshot 在 route table 变更后必须返回 `route_table_expired`。
#[test]
fn distributed_chaos_promote_keeps_single_writer_and_snapshot_contract() {
    let tenant = 880;
    let a_leader_dir = durable_dir("a_leader");
    let a_follower_dir = durable_dir("a_follower");
    let b_leader_dir = durable_dir("b_leader");
    let b_follower_dir = durable_dir("b_follower");
    let route_table_path = durable_dir("route_table").with_extension("json");

    let a_leader_addr = free_addr();
    let a_follower_addr = free_addr();
    let b_leader_addr = free_addr();
    let b_follower_addr = free_addr();
    let gateway_addr = free_addr();

    let mut a_leader = spawn_shard(&a_leader_dir, &a_leader_addr);
    let a_follower = spawn_shard(&a_follower_dir, &a_follower_addr);
    let b_leader = spawn_shard(&b_leader_dir, &b_leader_addr);
    let b_follower = spawn_shard(&b_follower_dir, &b_follower_addr);

    let table_v1 = route_table_v1(
        &a_leader.addr,
        &a_follower.addr,
        &b_leader.addr,
        &b_follower.addr,
    );
    std::fs::write(&route_table_path, &table_v1).unwrap();
    let gateway = spawn_gateway(&gateway_addr, &route_table_path);

    let session_a = session_for_gateway_shard(tenant, 0, 2, 88_000);
    let session_b = session_for_gateway_shard(tenant, 1, 2, 88_000);
    let initial = chaos_batch(&[
        (88_101, session_a, "logical-a", "before-promote"),
        (88_102, session_a, "logical-a", "before-promote"),
        (88_201, session_b, "logical-b", "before-promote"),
        (88_202, session_b, "logical-b", "before-promote"),
    ]);
    let ingest = assert_http_contains(
        &gateway.addr,
        "POST",
        "/v1/ingest",
        &initial,
        Some(tenant),
        r#""ingested":4"#,
    );
    assert!(
        ingest.contains(r#""queryMode":"process_gateway_route""#),
        "{ingest}"
    );

    pull_once(&a_follower, &a_leader);
    pull_once(&b_follower, &b_leader);
    let health = assert_http_contains(
        &gateway.addr,
        "POST",
        "/v1/cluster/health/refresh",
        "",
        None,
        r#""replicaCount":4"#,
    );
    assert!(health.contains(r#""replicaId":"a-follower""#), "{health}");
    assert!(health.contains(r#""replicationLagLsn":0"#), "{health}");

    let query = r#"{"filter":{"projectId":"distributed-chaos-eval"},"limit":20}"#;
    let default_page = assert_http_contains(
        &gateway.addr,
        "POST",
        "/v1/trace-search",
        query,
        Some(tenant),
        r#""total":4"#,
    );
    assert!(
        default_page.contains(r#""reason":"bounded_stale_follower""#),
        "{default_page}"
    );
    assert!(default_page.contains(r#""replicaId":"a-follower""#));
    assert!(default_page.contains(r#""replicaId":"b-follower""#));
    let old_snapshot = extract_object_field(&default_page, "snapshot");

    a_leader.kill();
    let strict_body =
        r#"{"filter":{"projectId":"distributed-chaos-eval"},"limit":20,"consistency":"strict"}"#;
    let strict_failed = assert_http_status(
        &gateway.addr,
        "POST",
        "/v1/trace-search",
        strict_body,
        Some(tenant),
        502,
    );
    assert!(
        strict_failed.contains("strict consistency requires all shards"),
        "{strict_failed}"
    );
    assert!(
        strict_failed.contains(r#""failedShards":["#),
        "{strict_failed}"
    );
    assert!(strict_failed.contains(r#""shard":0"#), "{strict_failed}");

    let table_v2 = route_table_v2_promote_a(
        &a_leader_addr,
        &a_follower.addr,
        &b_leader.addr,
        &b_follower.addr,
    );
    let reloaded = assert_http_contains(
        &gateway.addr,
        "POST",
        "/v1/cluster/route-table/reload",
        &table_v2,
        None,
        r#""routeTableVersion":2"#,
    );
    assert!(reloaded.contains(r#""ok":true"#), "{reloaded}");

    let stale_snapshot_body = format!(
        r#"{{"filter":{{"projectId":"distributed-chaos-eval"}},"limit":20,"snapshot":{old_snapshot}}}"#
    );
    let stale_snapshot = assert_http_status(
        &gateway.addr,
        "POST",
        "/v1/trace-search",
        &stale_snapshot_body,
        Some(tenant),
        409,
    );
    assert!(
        stale_snapshot.contains(r#""code":"route_table_expired""#),
        "{stale_snapshot}"
    );

    let after_down = chaos_batch(&[(88_103, session_a, "logical-a", "after-promote")]);
    assert_http_contains(
        &gateway.addr,
        "POST",
        "/v1/ingest",
        &after_down,
        Some(tenant),
        r#""ingested":1"#,
    );

    a_leader = spawn_shard(&a_leader_dir, &a_leader_addr);
    let health_after_restart = assert_http_contains(
        &gateway.addr,
        "POST",
        "/v1/cluster/health/refresh",
        "",
        None,
        r#""routeTableVersion":2"#,
    );
    assert!(
        health_after_restart.contains(r#""replicaId":"a-leader""#),
        "{health_after_restart}"
    );
    assert!(
        health_after_restart.contains("lag_exceeds_budget")
            || health_after_restart.contains(r#""replicaId":"a-follower""#),
        "{health_after_restart}"
    );

    let after_restart = chaos_batch(&[(88_104, session_a, "logical-a", "after-promote")]);
    assert_http_contains(
        &gateway.addr,
        "POST",
        "/v1/ingest",
        &after_restart,
        Some(tenant),
        r#""ingested":1"#,
    );

    let direct_after =
        r#"{"filter":{"projectId":"distributed-chaos-eval","mode":"after-promote"},"limit":10}"#;
    let old_leader_page = assert_http_contains(
        &a_leader.addr,
        "POST",
        "/v1/trace-search",
        direct_after,
        Some(tenant),
        r#""total":0"#,
    );
    assert!(!old_leader_page.contains(r#""traceId":"88103""#));
    assert!(!old_leader_page.contains(r#""traceId":"88104""#));

    let promoted_page = assert_http_contains(
        &a_follower.addr,
        "POST",
        "/v1/trace-search",
        direct_after,
        Some(tenant),
        r#""total":2"#,
    );
    assert!(
        promoted_page.contains(r#""traceId":"88103""#),
        "{promoted_page}"
    );
    assert!(
        promoted_page.contains(r#""traceId":"88104""#),
        "{promoted_page}"
    );

    let strict_after = assert_http_contains(
        &gateway.addr,
        "POST",
        "/v1/trace-search",
        strict_body,
        Some(tenant),
        r#""total":6"#,
    );
    assert!(
        strict_after.contains(r#""replicaId":"a-follower""#),
        "{strict_after}"
    );
    assert!(
        strict_after.contains(r#""replicaId":"b-leader""#),
        "{strict_after}"
    );

    drop(gateway);
    drop(a_leader);
    drop(a_follower);
    drop(b_leader);
    drop(b_follower);
    let _ = std::fs::remove_file(route_table_path);
    let _ = std::fs::remove_dir_all(a_leader_dir);
    let _ = std::fs::remove_dir_all(a_follower_dir);
    let _ = std::fs::remove_dir_all(b_leader_dir);
    let _ = std::fs::remove_dir_all(b_follower_dir);
}

#[test]
#[ignore]
fn chaos_shard_child_process() {
    if std::env::var(CHILD_ENV).ok().as_deref() != Some("1") {
        return;
    }
    let dir = std::env::var(CHILD_DIR_ENV).expect("child dir env");
    let bind = std::env::var(CHILD_BIND_ENV).expect("child bind env");
    let coord = WriteCoordinator::open_durable(&dir).expect("open_durable");
    coord.recover();
    let server = Arc::new(HttpIngestServer::new(coord));
    let listener = TcpListener::bind(&bind).expect("bind chaos shard server");
    server.serve_pool(listener, 4);
}

#[test]
#[ignore]
fn chaos_gateway_child_process() {
    if std::env::var(GATEWAY_ENV).ok().as_deref() != Some("1") {
        return;
    }
    let bind = std::env::var(CHILD_BIND_ENV).expect("gateway bind env");
    let path = std::env::var(ROUTE_TABLE_ENV).expect("route table env");
    let body = std::fs::read_to_string(path).expect("read route table");
    let gateway = RemoteShardGateway::from_route_table_json(&body).expect("remote shard gateway");
    let listener = TcpListener::bind(&bind).expect("bind chaos gateway server");
    Arc::new(RemoteGatewayServer::new(gateway)).serve_pool(listener, 4);
}
