use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use yt_engine::{
    evalkit::{EvalCaseReport, EvalCheckReport, EvalSuiteReport},
    HttpIngestServer, RemoteGatewayServer, RemoteShardGateway, WriteCoordinator,
};

const CHILD_ENV: &str = "YT_DISTRIBUTED_EVAL_CHILD";
const CHILD_DIR_ENV: &str = "YT_DISTRIBUTED_EVAL_DIR";
const CHILD_BIND_ENV: &str = "YT_DISTRIBUTED_EVAL_BIND";
const GATEWAY_ENV: &str = "YT_DISTRIBUTED_EVAL_GATEWAY";
const GATEWAY_SHARDS_ENV: &str = "YT_DISTRIBUTED_EVAL_SHARDS";

fn durable_dir(name: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "yt_process_eval_{name}_{}_{}",
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

struct RunningShard {
    addr: String,
    child: Child,
}

impl Drop for RunningShard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_shard(dir: &PathBuf, addr: &str) -> RunningShard {
    let exe = std::env::current_exe().unwrap();
    let child = Command::new(exe)
        .arg("shard_server_child_process")
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
    let mut shard = RunningShard {
        addr: addr.to_string(),
        child,
    };
    wait_ready(&mut shard);
    shard
}

fn spawn_gateway(addr: &str, shard_addrs: &[String]) -> RunningShard {
    let exe = std::env::current_exe().unwrap();
    let child = Command::new(exe)
        .arg("gateway_server_child_process")
        .arg("--ignored")
        .arg("--exact")
        .env(GATEWAY_ENV, "1")
        .env(CHILD_BIND_ENV, addr)
        .env(GATEWAY_SHARDS_ENV, shard_addrs.join(","))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut gateway = RunningShard {
        addr: addr.to_string(),
        child,
    };
    wait_ready(&mut gateway);
    gateway
}

fn wait_ready(shard: &mut RunningShard) {
    let start = Instant::now();
    loop {
        if let Some(status) = shard.child.try_wait().unwrap() {
            panic!("shard {} exited before ready: {status}", shard.addr);
        }
        if let Ok((200, _)) = http_request(&shard.addr, "GET", "/v1/cluster/shards", "", None) {
            return;
        }
        if start.elapsed() > Duration::from_secs(5) {
            panic!("shard {} did not become ready", shard.addr);
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
        body.as_bytes().len()
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

fn assert_http_contains(
    addr: &str,
    method: &str,
    path: &str,
    body: &str,
    tenant: Option<u64>,
    needle: &str,
) -> String {
    let (status, response) = http_request(addr, method, path, body, tenant).unwrap();
    assert_eq!(status, 200, "{response}");
    assert!(
        response.contains(needle),
        "missing {needle:?} from {method} {addr}{path}: {response}"
    );
    response
}

fn eval_check(
    checks: &mut Vec<EvalCheckReport>,
    name: impl Into<String>,
    passed: bool,
    detail: impl Into<String>,
) {
    checks.push(EvalCheckReport {
        name: name.into(),
        passed,
        detail: detail.into(),
    });
}

fn eval_http_request(
    checks: &mut Vec<EvalCheckReport>,
    name: &str,
    addr: &str,
    method: &str,
    path: &str,
    body: &str,
    tenant: Option<u64>,
    expect_status: u16,
) -> String {
    match http_request(addr, method, path, body, tenant) {
        Ok((status, response)) => {
            eval_check(
                checks,
                format!("{name} status"),
                status == expect_status,
                format!("got {status}, expected {expect_status}, body={response}"),
            );
            response
        }
        Err(err) => {
            eval_check(
                checks,
                format!("{name} request"),
                false,
                format!("request failed: {err}"),
            );
            String::new()
        }
    }
}

fn eval_contains(checks: &mut Vec<EvalCheckReport>, name: &str, body: &str, needle: &str) {
    eval_check(
        checks,
        format!("{name} contains {needle:?}"),
        body.contains(needle),
        body.to_string(),
    );
}

fn eval_rejects(checks: &mut Vec<EvalCheckReport>, name: &str, body: &str, needle: &str) {
    eval_check(
        checks,
        format!("{name} rejects {needle:?}"),
        !body.contains(needle),
        body.to_string(),
    );
}

fn json_u64_field(body: &str, field: &str) -> u64 {
    let needle = format!(r#""{field}":"#);
    let start = body
        .find(&needle)
        .unwrap_or_else(|| panic!("missing field {field} from {body}"))
        + needle.len();
    let digits: String = body[start..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits
        .parse()
        .unwrap_or_else(|_| panic!("bad u64 field {field} in {body}"))
}

fn ingest_event_json(trace_id: u64, shard_idx: usize) -> String {
    ingest_event_json_with_session(trace_id, trace_id / 10, shard_idx)
}

fn ingest_event_json_with_session(trace_id: u64, session_id: u64, shard_idx: usize) -> String {
    format!(
        r#"{{"trace_id":{trace_id},"span_id":1,"session_id":{},"ts":{},"seq":1,"event_type":2,"ext_span_id":"{trace_id}-1","status":0,"duration_ns":1000,"agent_name":"process-shard-{shard_idx}","model":"qwen-max","output_text":"distributed multi process shard {shard_idx} unique trace {trace_id}","attrs":{{"project_id":"process-distributed-eval","skill":"multi-process","mode":"shard-{shard_idx}","task_fingerprint":"real-process-cluster"}}}}"#,
        session_id, trace_id as i64
    )
}

fn ingest_body(trace_id: u64, shard_idx: usize) -> String {
    format!("[{}]", ingest_event_json(trace_id, shard_idx))
}

fn gateway_batch_with_sessions(events: &[(u64, u64, usize)]) -> String {
    let items: Vec<String> = events
        .iter()
        .map(|(trace_id, session_id, shard_idx)| {
            ingest_event_json_with_session(*trace_id, *session_id, *shard_idx)
        })
        .collect();
    format!("[{}]", items.join(","))
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

fn session_for_gateway_shard(tenant: u64, shard: usize, shard_count: usize) -> u64 {
    session_for_gateway_shard_after(tenant, shard, shard_count, 70_000)
}

fn session_for_gateway_shard_after(
    tenant: u64,
    shard: usize,
    shard_count: usize,
    start: u64,
) -> u64 {
    (start..start + 20_000)
        .find(|session| {
            (test_route_hash(Some(tenant), Some(*session), 0) as usize) % shard_count == shard
        })
        .unwrap_or_else(|| panic!("could not find session for shard {shard}"))
}

fn json_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// 这个 ignored test 是真实 shard server 子进程入口。
///
/// 父测试通过 `current_exe()` 启动多个这样的进程；每个进程独立打开 data dir、
/// 绑定 TCP 端口并永久 serve。没有设置 `YT_DISTRIBUTED_EVAL_CHILD=1` 时直接返回，
/// 避免手动跑 ignored tests 时挂住。
#[test]
#[ignore]
fn shard_server_child_process() {
    if std::env::var(CHILD_ENV).ok().as_deref() != Some("1") {
        return;
    }
    let dir = std::env::var(CHILD_DIR_ENV).expect("child dir env");
    let bind = std::env::var(CHILD_BIND_ENV).expect("child bind env");
    let coord = WriteCoordinator::open_durable(&dir).expect("open_durable");
    coord.recover();
    let server = Arc::new(HttpIngestServer::new(coord));
    let listener = TcpListener::bind(&bind).expect("bind child shard server");
    server.serve_pool(listener, 4);
}

/// 真实 gateway 子进程入口：监听自己的 TCP 端口，再用 HTTP 访问多个 shard server。
#[test]
#[ignore]
fn gateway_server_child_process() {
    if std::env::var(GATEWAY_ENV).ok().as_deref() != Some("1") {
        return;
    }
    let bind = std::env::var(CHILD_BIND_ENV).expect("gateway bind env");
    let shards: Vec<String> = std::env::var(GATEWAY_SHARDS_ENV)
        .expect("gateway shards env")
        .split(',')
        .filter(|addr| !addr.trim().is_empty())
        .map(|addr| addr.trim().to_string())
        .collect();
    let gateway = RemoteShardGateway::new(shards).expect("remote shard gateway");
    let listener = TcpListener::bind(&bind).expect("bind gateway server");
    Arc::new(RemoteGatewayServer::new(gateway)).serve_pool(listener, 4);
}

/// 真多实例 eval：启动多个独立 shard server 进程，经 TCP 写入和查询，再 kill/restart 单 shard。
///
/// 这里还不是完整控制面/复制协议；它验证的是多 shard server 的真实进程边界：
/// - 每个实例独立 durable data dir。
/// - HTTP ingest/search/trace-search 走真实 socket。
/// - query coordinator 在测试侧 fanout，多实例结果可合并。
/// - 单个 shard 进程重启后数据仍可见。
/// - tenant header 隔离在多实例下仍成立。
#[test]
fn multi_process_shards_ingest_query_and_survive_restart() {
    let dirs = [
        durable_dir("shard_0"),
        durable_dir("shard_1"),
        durable_dir("shard_2"),
    ];
    let addrs = [free_addr(), free_addr(), free_addr()];
    let mut shards: Vec<RunningShard> = dirs
        .iter()
        .zip(addrs.iter())
        .map(|(dir, addr)| spawn_shard(dir, addr))
        .collect();

    for (idx, shard) in shards.iter().enumerate() {
        let trace_id = 73_001 + idx as u64;
        let body = ingest_body(trace_id, idx);
        let response = assert_http_contains(
            &shard.addr,
            "POST",
            "/v1/ingest",
            &body,
            Some(730),
            r#""ingested":1"#,
        );
        assert_eq!(response, r#"{"ingested":1}"#);
    }

    let query = r#"{"filter":{"projectId":"process-distributed-eval"},"limit":10}"#;
    let mut total = 0usize;
    for shard in &shards {
        let page = assert_http_contains(
            &shard.addr,
            "POST",
            "/v1/trace-search",
            query,
            Some(730),
            r#""total":1"#,
        );
        assert!(page.contains(r#""task_fingerprint":"real-process-cluster""#));
        total += 1;
    }
    assert_eq!(
        total, 3,
        "test-side fanout should see all three shard instances"
    );

    let wrong_tenant = http_request(
        &shards[0].addr,
        "POST",
        "/v1/trace-search",
        query,
        Some(731),
    )
    .unwrap()
    .1;
    assert!(
        wrong_tenant.contains(r#""total":0"#),
        "tenant 731 must not see tenant 730 shard data: {wrong_tenant}"
    );

    let restarted_addr = shards[1].addr.clone();
    drop(shards.remove(1));
    let restarted = spawn_shard(&dirs[1], &restarted_addr);
    assert_http_contains(
        &restarted.addr,
        "POST",
        "/v1/trace-search",
        r#"{"filter":{"projectId":"process-distributed-eval","mode":"shard-1"},"limit":10}"#,
        Some(730),
        r#""traceId":"73002""#,
    );
    shards.push(restarted);

    for shard in shards {
        drop(shard);
    }
    for dir in dirs {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// 真 gateway eval：请求只打 gateway 进程，gateway 再按 trace id 路由写入并 fanout 查询多个 shard。
#[test]
fn gateway_process_routes_ingest_and_merges_real_shards() {
    let dirs = [
        durable_dir("gateway_shard_0"),
        durable_dir("gateway_shard_1"),
        durable_dir("gateway_shard_2"),
    ];
    let shard_addrs = [free_addr(), free_addr(), free_addr()];
    let shards: Vec<RunningShard> = dirs
        .iter()
        .zip(shard_addrs.iter())
        .map(|(dir, addr)| spawn_shard(dir, addr))
        .collect();
    let gateway_addr = free_addr();
    let shard_addr_vec: Vec<String> = shards.iter().map(|shard| shard.addr.clone()).collect();
    let gateway = spawn_gateway(&gateway_addr, &shard_addr_vec);

    let s0 = session_for_gateway_shard(740, 0, 3);
    let s1 = session_for_gateway_shard(740, 1, 3);
    let s2 = session_for_gateway_shard(740, 2, 3);
    let body = gateway_batch_with_sessions(&[(74_001, s0, 0), (74_002, s1, 1), (74_003, s2, 2)]);
    let response = assert_http_contains(
        &gateway.addr,
        "POST",
        "/v1/ingest",
        &body,
        Some(740),
        r#""queryMode":"process_gateway_route""#,
    );
    assert!(response.contains(r#""ingested":3"#), "{response}");
    assert!(response.contains(r#""shardCount":3"#), "{response}");

    let page = assert_http_contains(
        &gateway.addr,
        "POST",
        "/v1/trace-search",
        r#"{"filter":{"projectId":"process-distributed-eval"},"limit":10}"#,
        Some(740),
        r#""queryMode":"process_gateway_fanout""#,
    );
    assert!(page.contains(r#""total":3"#), "{page}");
    assert!(page.contains(r#""traceId":"74001""#), "{page}");
    assert!(page.contains(r#""traceId":"74002""#), "{page}");
    assert!(page.contains(r#""traceId":"74003""#), "{page}");

    let first_page = assert_http_contains(
        &gateway.addr,
        "POST",
        "/v1/trace-search",
        r#"{"filter":{"projectId":"process-distributed-eval"},"limit":2}"#,
        Some(740),
        r#""nextCursor":2"#,
    );
    let pos_74003 = first_page.find(r#""traceId":"74003""#).unwrap();
    let pos_74002 = first_page.find(r#""traceId":"74002""#).unwrap();
    assert!(pos_74003 < pos_74002, "{first_page}");
    assert!(!first_page.contains(r#""traceId":"74001""#), "{first_page}");

    let second_page = assert_http_contains(
        &gateway.addr,
        "POST",
        "/v1/trace-search",
        r#"{"filter":{"projectId":"process-distributed-eval"},"cursor":2,"limit":2}"#,
        Some(740),
        r#""nextCursor":null"#,
    );
    assert!(
        second_page.contains(r#""traceId":"74001""#),
        "{second_page}"
    );
    assert!(
        !second_page.contains(r#""traceId":"74002""#),
        "{second_page}"
    );

    let search = assert_http_contains(
        &gateway.addr,
        "POST",
        "/v1/search",
        r#"{"text":"distributed","k":10,"includeFanout":true}"#,
        Some(740),
        r#""queryMode":"process_gateway_fanout""#,
    );
    assert!(search.contains(r#""trace_id":74001"#), "{search}");
    assert!(search.contains(r#""trace_id":74002"#), "{search}");
    assert!(search.contains(r#""trace_id":74003"#), "{search}");

    let top_one = assert_http_contains(
        &gateway.addr,
        "POST",
        "/v1/search",
        r#"{"text":"distributed","k":1,"includeFanout":true}"#,
        Some(740),
        r#""total":1"#,
    );
    assert!(top_one.contains(r#""trace_id":74001"#), "{top_one}");
    assert!(!top_one.contains(r#""trace_id":74002"#), "{top_one}");
    assert!(!top_one.contains(r#""trace_id":74003"#), "{top_one}");

    let aggregate = assert_http_contains(
        &gateway.addr,
        "POST",
        "/v1/trace-aggregate",
        r#"{"filter":{"projectId":"process-distributed-eval"},"groupBy":["mode"],"limit":10}"#,
        Some(740),
        r#""aggregationIndex":"remote_fanout_reduce""#,
    );
    assert!(aggregate.contains(r#""spanTotal":3"#), "{aggregate}");
    assert!(aggregate.contains(r#""mode":"shard-0""#), "{aggregate}");
    assert!(aggregate.contains(r#""mode":"shard-1""#), "{aggregate}");
    assert!(aggregate.contains(r#""mode":"shard-2""#), "{aggregate}");

    let trajectories = assert_http_contains(
        &gateway.addr,
        "POST",
        "/v1/trace-trajectories",
        r#"{"filter":{"projectId":"process-distributed-eval"},"limit":10}"#,
        Some(740),
        r#""index":"remote_fanout_materialized""#,
    );
    assert!(trajectories.contains(r#""spanTotal":3"#), "{trajectories}");

    assert_http_contains(
        &gateway.addr,
        "POST",
        "/v1/trajectory-groups",
        r#"{"filter":{"projectId":"process-distributed-eval"},"limit":10}"#,
        Some(740),
        r#""trajectoryIndex":"remote_fanout_materialized_reduce""#,
    );

    let storage = assert_http_contains(
        &gateway.addr,
        "POST",
        "/v1/storage-stats",
        r#"{"filter":{"projectId":"process-distributed-eval"},"groupBy":["projectId"]}"#,
        Some(740),
        r#""storageIndex":"remote_fanout_reduce""#,
    );
    assert!(storage.contains(r#""traceCount":3"#), "{storage}");

    assert_http_contains(
        &gateway.addr,
        "POST",
        "/v1/annotations",
        r#"{"traceId":74001,"label":"best_path","score":960,"projectId":"process-distributed-eval"}"#,
        Some(740),
        r#""annotationId":"#,
    );
    let annotations = assert_http_contains(
        &gateway.addr,
        "GET",
        "/v1/annotations?label=best_path",
        "",
        Some(740),
        r#""metadataIndex":"remote_fanout_metadata_merge""#,
    );
    assert!(annotations.contains(r#""total":1"#), "{annotations}");

    let retention = assert_http_contains(
        &gateway.addr,
        "POST",
        "/v1/retention-plan",
        r#"{"filter":{"projectId":"process-distributed-eval"},"deleteBeforeTs":999999999}"#,
        Some(740),
        r#""queryMode":"process_gateway_fanout""#,
    );
    assert!(retention.contains(r#""shardCount":3"#), "{retention}");
    assert!(retention.contains(r#""okShards":3"#), "{retention}");

    for (idx, shard) in shards.iter().enumerate() {
        let direct = assert_http_contains(
            &shard.addr,
            "POST",
            "/v1/trace-search",
            &format!(
                r#"{{"filter":{{"projectId":"process-distributed-eval","mode":"shard-{idx}"}},"limit":10}}"#
            ),
            Some(740),
            r#""total":1"#,
        );
        assert!(
            direct.contains(&format!(r#""traceId":"7400{}""#, idx + 1)),
            "{direct}"
        );
    }

    let same_session = session_for_gateway_shard_after(740, 1, 3, s1 + 1);
    let colocated =
        gateway_batch_with_sessions(&[(74_101, same_session, 11), (74_102, same_session, 12)]);
    assert_http_contains(
        &gateway.addr,
        "POST",
        "/v1/ingest",
        &colocated,
        Some(740),
        r#""ingested":2"#,
    );
    let mut owner_hits = 0usize;
    for shard in &shards {
        let page = assert_http_contains(
            &shard.addr,
            "POST",
            "/v1/trace-search",
            &format!(r#"{{"filter":{{"sessionId":{same_session}}},"limit":10}}"#),
            Some(740),
            r#""total":"#,
        );
        if page.contains(r#""total":2"#) {
            owner_hits += 1;
            assert!(page.contains(r#""traceId":"74101""#), "{page}");
            assert!(page.contains(r#""traceId":"74102""#), "{page}");
        } else {
            assert!(page.contains(r#""total":0"#), "{page}");
        }
    }
    assert_eq!(
        owner_hits, 1,
        "same session traces must be co-located on one shard"
    );

    let deep_session = session_for_gateway_shard_after(740, 2, 3, same_session + 1);
    let deep_events: Vec<(u64, u64, usize)> = (0..520u64)
        .map(|offset| (76_000 + offset, deep_session, 20))
        .collect();
    let deep_body = gateway_batch_with_sessions(&deep_events);
    assert_http_contains(
        &gateway.addr,
        "POST",
        "/v1/ingest",
        &deep_body,
        Some(740),
        r#""ingested":520"#,
    );
    let deep_page = assert_http_contains(
        &gateway.addr,
        "POST",
        "/v1/trace-search",
        &format!(r#"{{"filter":{{"sessionId":{deep_session}}},"cursor":510,"limit":5}}"#),
        Some(740),
        r#""nextCursor":515"#,
    );
    assert!(deep_page.contains(r#""traceId":"76009""#), "{deep_page}");
    assert!(deep_page.contains(r#""traceId":"76005""#), "{deep_page}");
    assert!(!deep_page.contains(r#""traceId":"76519""#), "{deep_page}");

    let wrong_tenant = http_request(
        &gateway.addr,
        "POST",
        "/v1/trace-search",
        r#"{"filter":{"projectId":"process-distributed-eval"},"limit":10}"#,
        Some(741),
    )
    .unwrap()
    .1;
    assert!(
        wrong_tenant.contains(r#""total":0"#),
        "gateway must preserve tenant isolation across fanout: {wrong_tenant}"
    );

    drop(gateway);
    for shard in shards {
        drop(shard);
    }
    for dir in dirs {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// 真 gateway 降级 eval：一个 shard 进程宕掉时，查询返回可用分片的结果和失败诊断。
#[test]
fn gateway_process_query_reports_partial_shard_failure() {
    let dirs = [
        durable_dir("gateway_degraded_shard_0"),
        durable_dir("gateway_degraded_shard_1"),
        durable_dir("gateway_degraded_shard_2"),
    ];
    let shard_addrs = [free_addr(), free_addr(), free_addr()];
    let mut shards: Vec<RunningShard> = dirs
        .iter()
        .zip(shard_addrs.iter())
        .map(|(dir, addr)| spawn_shard(dir, addr))
        .collect();
    let gateway_addr = free_addr();
    let shard_addr_vec: Vec<String> = shards.iter().map(|shard| shard.addr.clone()).collect();
    let gateway = spawn_gateway(&gateway_addr, &shard_addr_vec);

    let s0 = session_for_gateway_shard(750, 0, 3);
    let s1 = session_for_gateway_shard(750, 1, 3);
    let s2 = session_for_gateway_shard(750, 2, 3);
    let body = gateway_batch_with_sessions(&[(75_000, s0, 0), (75_001, s1, 1), (75_002, s2, 2)]);
    assert_http_contains(
        &gateway.addr,
        "POST",
        "/v1/ingest",
        &body,
        Some(750),
        r#""ingested":3"#,
    );

    let failed_addr = shards[1].addr.clone();
    drop(shards.remove(1));

    let (status, page) = http_request(
        &gateway.addr,
        "POST",
        "/v1/trace-search",
        r#"{"filter":{"projectId":"process-distributed-eval"},"limit":10}"#,
        Some(750),
    )
    .unwrap();
    assert_eq!(status, 200, "{page}");
    assert!(
        page.contains(r#""queryMode":"process_gateway_fanout""#),
        "{page}"
    );
    assert!(page.contains(r#""degraded":true"#), "{page}");
    assert!(page.contains(r#""okShards":2"#), "{page}");
    assert!(page.contains(r#""failedShards":["#), "{page}");
    assert!(page.contains(r#""shard":1"#), "{page}");
    assert!(page.contains(r#""status":0"#), "{page}");
    assert!(page.contains(r#""total":2"#), "{page}");
    assert!(page.contains(r#""traceId":"75000""#), "{page}");
    assert!(page.contains(r#""traceId":"75002""#), "{page}");
    assert!(
        !page.contains(r#""traceId":"75001""#),
        "down shard result must not be silently counted: {page}"
    );

    let cluster = assert_http_contains(
        &gateway.addr,
        "GET",
        "/v1/cluster/shards",
        "",
        None,
        r#""mode":"process_gateway""#,
    );
    assert!(cluster.contains(&json_escape(&failed_addr)), "{cluster}");
    assert!(cluster.contains(r#""httpStatus":0"#), "{cluster}");

    let wrong_tenant = http_request(
        &gateway.addr,
        "POST",
        "/v1/trace-search",
        r#"{"filter":{"projectId":"process-distributed-eval"},"limit":10}"#,
        Some(751),
    )
    .unwrap()
    .1;
    assert!(
        wrong_tenant.contains(r#""total":0"#),
        "degraded gateway query must still preserve tenant isolation: {wrong_tenant}"
    );

    drop(gateway);
    for shard in shards {
        drop(shard);
    }
    for dir in dirs {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// 真 gateway 恢复 eval：后端 shard 宕机后，原 data dir 原端口重启，gateway 下一次查询能恢复全量。
#[test]
fn gateway_process_recovers_after_backend_shard_restart() {
    let mut checks = Vec::new();
    let dirs = [
        durable_dir("gateway_recover_shard_0"),
        durable_dir("gateway_recover_shard_1"),
        durable_dir("gateway_recover_shard_2"),
    ];
    let shard_addrs = [free_addr(), free_addr(), free_addr()];
    let mut shards: Vec<RunningShard> = dirs
        .iter()
        .zip(shard_addrs.iter())
        .map(|(dir, addr)| spawn_shard(dir, addr))
        .collect();
    let gateway_addr = free_addr();
    let shard_addr_vec: Vec<String> = shards.iter().map(|shard| shard.addr.clone()).collect();
    let gateway = spawn_gateway(&gateway_addr, &shard_addr_vec);
    eval_check(
        &mut checks,
        "creates 3 shard processes plus 1 gateway process",
        shards.len() == 3,
        format!("live shard processes={}, gateway processes=1", shards.len()),
    );

    let s0 = session_for_gateway_shard(752, 0, 3);
    let s1 = session_for_gateway_shard(752, 1, 3);
    let s2 = session_for_gateway_shard(752, 2, 3);
    let body = gateway_batch_with_sessions(&[(75_200, s0, 0), (75_201, s1, 1), (75_202, s2, 2)]);
    let ingest = eval_http_request(
        &mut checks,
        "gateway ingest before restart",
        &gateway.addr,
        "POST",
        "/v1/ingest",
        &body,
        Some(752),
        200,
    );
    eval_contains(
        &mut checks,
        "gateway ingest before restart",
        &ingest,
        r#""ingested":3"#,
    );

    let restarted_addr = shards[1].addr.clone();
    drop(shards.remove(1));
    eval_check(
        &mut checks,
        "one backend shard is stopped before degraded query",
        shards.len() == 2,
        format!("live shard processes={}, gateway processes=1", shards.len()),
    );

    let degraded = eval_http_request(
        &mut checks,
        "gateway degraded query while shard down",
        &gateway.addr,
        "POST",
        "/v1/trace-search",
        r#"{"filter":{"projectId":"process-distributed-eval"},"limit":10}"#,
        Some(752),
        200,
    );
    eval_contains(
        &mut checks,
        "gateway degraded query while shard down",
        &degraded,
        r#""degraded":true"#,
    );
    eval_contains(
        &mut checks,
        "gateway degraded query while shard down",
        &degraded,
        r#""okShards":2"#,
    );
    eval_contains(
        &mut checks,
        "gateway degraded query while shard down",
        &degraded,
        r#""total":2"#,
    );
    eval_contains(
        &mut checks,
        "gateway degraded query while shard down",
        &degraded,
        r#""traceId":"75200""#,
    );
    eval_contains(
        &mut checks,
        "gateway degraded query while shard down",
        &degraded,
        r#""traceId":"75202""#,
    );
    eval_rejects(
        &mut checks,
        "gateway degraded query while shard down",
        &degraded,
        r#""traceId":"75201""#,
    );

    let restarted = spawn_shard(&dirs[1], &restarted_addr);
    eval_check(
        &mut checks,
        "stopped backend shard restarts from same durable dir and port",
        shards.len() == 2,
        format!("live shard processes before reinserting restarted shard={}, restarted_addr={restarted_addr}", shards.len()),
    );

    let recovered = eval_http_request(
        &mut checks,
        "gateway recovered query after shard restart",
        &gateway.addr,
        "POST",
        "/v1/trace-search",
        r#"{"filter":{"projectId":"process-distributed-eval"},"limit":10}"#,
        Some(752),
        200,
    );
    eval_contains(
        &mut checks,
        "gateway recovered query after shard restart",
        &recovered,
        r#""degraded":false"#,
    );
    eval_contains(
        &mut checks,
        "gateway recovered query after shard restart",
        &recovered,
        r#""okShards":3"#,
    );
    eval_contains(
        &mut checks,
        "gateway recovered query after shard restart",
        &recovered,
        r#""total":3"#,
    );
    eval_contains(
        &mut checks,
        "gateway recovered query after shard restart",
        &recovered,
        r#""traceId":"75200""#,
    );
    eval_contains(
        &mut checks,
        "gateway recovered query after shard restart",
        &recovered,
        r#""traceId":"75201""#,
    );
    eval_contains(
        &mut checks,
        "gateway recovered query after shard restart",
        &recovered,
        r#""traceId":"75202""#,
    );

    let direct = eval_http_request(
        &mut checks,
        "restarted shard direct durable read",
        &restarted.addr,
        "POST",
        "/v1/trace-search",
        r#"{"filter":{"projectId":"process-distributed-eval","mode":"shard-1"},"limit":10}"#,
        Some(752),
        200,
    );
    eval_contains(
        &mut checks,
        "restarted shard direct durable read",
        &direct,
        r#""total":1"#,
    );
    eval_contains(
        &mut checks,
        "restarted shard direct durable read",
        &direct,
        r#""traceId":"75201""#,
    );

    shards.insert(1, restarted);
    let report = EvalSuiteReport {
        name: "distributed_process_gateway_recovery".to_string(),
        cases: vec![EvalCaseReport {
            name: "gateway recovers after backend shard restart".to_string(),
            category: "distributed_process".to_string(),
            checks,
        }],
    };
    let passed = report.passed();
    let failure_report = report.failure_report();

    drop(gateway);
    for shard in shards {
        drop(shard);
    }
    for dir in dirs {
        let _ = std::fs::remove_dir_all(dir);
    }
    assert!(passed, "{failure_report}");
}

/// 真网络复制 eval：两个独立 shard 进程通过 HTTP WAL batch 做 leader→follower catch-up。
///
/// 覆盖分布式底座的关键边界：
/// - 空 batch bootstrap 不应报错。
/// - leader 导出 WAL、follower 经 socket 应用后可查询。
/// - 重放同一 batch 幂等，不制造重复 trace。
/// - follower tail 之后的增量可以继续 catch-up。
/// - 缺口 batch 必须 409，不能悄悄接受造成分叉。
#[test]
fn network_wal_replication_between_processes_is_idempotent_and_gap_checked() {
    let leader_dir = durable_dir("replication_leader");
    let follower_dir = durable_dir("replication_follower");
    let leader_addr = free_addr();
    let follower_addr = free_addr();
    let leader = spawn_shard(&leader_dir, &leader_addr);
    let follower = spawn_shard(&follower_dir, &follower_addr);

    let empty = assert_http_contains(
        &leader.addr,
        "GET",
        "/v1/replication/wal?afterLsn=0",
        "",
        None,
        r#""recordCount":0"#,
    );
    assert_http_contains(
        &follower.addr,
        "POST",
        "/v1/replication/wal",
        &empty,
        None,
        r#""applied":true"#,
    );

    assert_http_contains(
        &leader.addr,
        "POST",
        "/v1/ingest",
        &ingest_body(77_001, 0),
        Some(770),
        r#""ingested":1"#,
    );
    let first_batch = assert_http_contains(
        &leader.addr,
        "GET",
        "/v1/replication/wal?afterLsn=0",
        "",
        None,
        r#""recordCount":1"#,
    );
    assert!(first_batch.contains(r#""tenant_id":770"#), "{first_batch}");
    assert!(
        first_batch.contains(r#""project_id":"process-distributed-eval""#),
        "{first_batch}"
    );

    let first_apply = assert_http_contains(
        &follower.addr,
        "POST",
        "/v1/replication/wal",
        &first_batch,
        None,
        r#""applied":true"#,
    );
    let follower_tail = json_u64_field(&first_apply, "committedTail");
    assert_eq!(follower_tail, json_u64_field(&first_batch, "toLsn"));

    let query = r#"{"filter":{"projectId":"process-distributed-eval"},"limit":10}"#;
    let follower_page = assert_http_contains(
        &follower.addr,
        "POST",
        "/v1/trace-search",
        query,
        Some(770),
        r#""total":1"#,
    );
    assert!(
        follower_page.contains(r#""traceId":"77001""#),
        "{follower_page}"
    );

    assert_http_contains(
        &follower.addr,
        "POST",
        "/v1/replication/wal",
        &first_batch,
        None,
        r#""applied":true"#,
    );
    let repeated_page = assert_http_contains(
        &follower.addr,
        "POST",
        "/v1/trace-search",
        query,
        Some(770),
        r#""total":1"#,
    );
    assert!(
        repeated_page.contains(r#""traceId":"77001""#),
        "{repeated_page}"
    );

    assert_http_contains(
        &leader.addr,
        "POST",
        "/v1/ingest",
        &ingest_body(77_002, 1),
        Some(770),
        r#""ingested":1"#,
    );
    let pull_body = format!(r#"{{"leaderAddr":"http://{}"}}"#, leader.addr);
    let pulled = assert_http_contains(
        &follower.addr,
        "POST",
        "/v1/replication/pull",
        &pull_body,
        None,
        r#""pulled":true"#,
    );
    assert!(pulled.contains(r#""recordCount":1"#), "{pulled}");
    assert!(
        pulled.contains(&format!(r#""fromLsn":{follower_tail}"#)),
        "{pulled}"
    );
    let caught_up = assert_http_contains(
        &follower.addr,
        "POST",
        "/v1/trace-search",
        query,
        Some(770),
        r#""total":2"#,
    );
    assert!(caught_up.contains(r#""traceId":"77001""#), "{caught_up}");
    assert!(caught_up.contains(r#""traceId":"77002""#), "{caught_up}");

    let follower_status = assert_http_contains(
        &follower.addr,
        "GET",
        "/v1/replication/status",
        "",
        None,
        r#""committedTail":2"#,
    );
    assert!(
        follower_status.contains(r#""memtableRows":2"#),
        "{follower_status}"
    );

    let gap = first_batch
        .replace(r#""fromLsn":0"#, r#""fromLsn":10"#)
        .replace(r#""toLsn":1"#, r#""toLsn":11"#);
    let (status, body) =
        http_request(&follower.addr, "POST", "/v1/replication/wal", &gap, None).unwrap();
    assert_eq!(status, 409, "{body}");
    assert!(
        body.contains(r#""code":"replication_apply_failed""#),
        "{body}"
    );
    assert!(body.contains("replication gap"), "{body}");

    drop(leader);
    drop(follower);
    let _ = std::fs::remove_dir_all(leader_dir);
    let _ = std::fs::remove_dir_all(follower_dir);
}
