//! 风险矩阵 eval：把当前最容易退化的能力放到同一个可跑报告里。
//!
//! 这里不是替代细分单测，而是给“发布前/大改后我最担心什么”一个统一入口。

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use yt_engine::evalkit::{
    run_api_eval_suite, ApiEvalCase, ApiEvalStep, EvalCaseReport, EvalCheckReport, EvalSuiteReport,
};
use yt_engine::{
    EngineJsonApi, HttpIngestServer, InMemorySegmentStore, RemoteGatewayServer, RemoteShardGateway,
    WriteCoordinator,
};

fn fresh_api() -> (Arc<WriteCoordinator>, EngineJsonApi) {
    let coord = WriteCoordinator::new(Arc::new(InMemorySegmentStore::default()));
    (Arc::clone(&coord), EngineJsonApi::new(coord))
}

fn durable_dir(name: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "yt_risk_eval_{name}_{}_{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .expect("yt-engine crate should live under yitrace-engine/crates/yt-engine")
        .to_path_buf()
}

fn check(name: &str, passed: bool, detail: impl Into<String>) -> EvalCheckReport {
    EvalCheckReport {
        name: name.to_string(),
        passed,
        detail: detail.into(),
    }
}

fn case(name: &str, category: &str, checks: Vec<EvalCheckReport>) -> EvalCaseReport {
    EvalCaseReport {
        name: name.to_string(),
        category: category.to_string(),
        checks,
    }
}

fn assert_suite(report: EvalSuiteReport) {
    assert!(report.passed(), "{}", report.failure_report());
}

fn socket_request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    body: &str,
    tenant: Option<u64>,
    token: Option<&str>,
) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).unwrap();
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(tenant) = tenant {
        req.push_str(&format!("X-Tenant-Id: {tenant}\r\n"));
    }
    if let Some(token) = token {
        req.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes()).unwrap();
    stream.write_all(body.as_bytes()).unwrap();
    let mut resp = String::new();
    if let Err(e) = stream.read_to_string(&mut resp) {
        if resp.is_empty() {
            return (0, format!("read response failed: {e}"));
        }
    }
    response_status_and_body(&resp)
}

fn socket_request_declared_length(
    addr: SocketAddr,
    method: &str,
    path: &str,
    declared_len: usize,
    tenant: Option<u64>,
    token: Option<&str>,
) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).unwrap();
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: x\r\nContent-Length: {declared_len}\r\nConnection: close\r\n"
    );
    if let Some(tenant) = tenant {
        req.push_str(&format!("X-Tenant-Id: {tenant}\r\n"));
    }
    if let Some(token) = token {
        req.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes()).unwrap();
    let mut resp = String::new();
    if let Err(e) = stream.read_to_string(&mut resp) {
        if resp.is_empty() {
            return (0, format!("read response failed: {e}"));
        }
    }
    response_status_and_body(&resp)
}

fn response_status_and_body(resp: &str) -> (u16, String) {
    let status = resp
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let body = resp
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
        .unwrap_or_default();
    (status, body)
}

fn distributed_write_safety_case() -> EvalCaseReport {
    let err = RemoteShardGateway::from_route_table_json(
        r#"{
          "version":13,
          "shards":[
            {"shardId":"tenant-a","replicas":[
              {"replicaId":"a","addr":"127.0.0.1:19001","role":"leader","writable":true},
              {"replicaId":"b","addr":"127.0.0.1:19002","role":"leader","writable":true}
            ]}
          ]
        }"#,
    )
    .unwrap_err();

    let valid = RemoteShardGateway::from_route_table_json(
        r#"{
          "version":14,
          "shards":[
            {"shardId":"tenant-a","replicas":[
              {"replicaId":"a-leader","addr":"http://127.0.0.1:19101","role":"leader","readable":true,"writable":true},
              {"replicaId":"a-follower","addr":"127.0.0.1:19102","role":"follower","readable":true,"writable":false}
            ]},
            {"shardId":"tenant-b","replicas":[
              {"replicaId":"b-leader","addr":"127.0.0.1:19103","role":"leader","readable":true,"writable":true}
            ]}
          ]
        }"#,
    );

    let mut checks = vec![check(
        "rejects dual writers in one logical shard",
        err.contains("exactly one writable replica"),
        err,
    )];
    match valid {
        Ok(gateway) => {
            checks.push(check(
                "valid route table exposes version",
                gateway.route_table_version() == Some(14),
                format!("version={:?}", gateway.route_table_version()),
            ));
            checks.push(check(
                "writer view has one writer per logical shard",
                gateway.shard_count() == 2,
                format!("writer_shard_count={}", gateway.shard_count()),
            ));
        }
        Err(e) => checks.push(check("valid route table parses", false, e)),
    }

    case(
        "distributed route table write safety",
        "distributed-write",
        checks,
    )
}

fn api_contract_cases() -> Vec<EvalCaseReport> {
    let (_, api) = fresh_api();
    let cases = vec![
        ApiEvalCase {
            name: "tenant header overrides body tenant",
            category: "api-contract",
            steps: vec![
                ApiEvalStep {
                    name: "seed spoofed tenant",
                    method: "POST",
                    path: "/v1/ingest",
                    tenant: Some(777),
                    body: r#"[{"trace_id":77001,"span_id":1,"session_id":7701,"tenant_id":999,"ts":10,"seq":1,"event_type":2,"ext_span_id":"77001-1","status":0,"duration_ns":10,"input_text":"tenant header wins","attrs":{"project_id":"risk-api","skill":"contract"}}]"#,
                    expect_status: 200,
                    expect_contains: &[r#""ingested":1"#],
                    reject_contains: &[],
                },
                ApiEvalStep {
                    name: "header tenant can read",
                    method: "POST",
                    path: "/v1/trace-search",
                    tenant: Some(777),
                    body: r#"{"filter":{"projectId":"risk-api"},"limit":10}"#,
                    expect_status: 200,
                    expect_contains: &[r#""total":1"#, r#""traceId":"77001""#],
                    reject_contains: &[],
                },
                ApiEvalStep {
                    name: "body tenant cannot read",
                    method: "POST",
                    path: "/v1/trace-search",
                    tenant: Some(999),
                    body: r#"{"filter":{"projectId":"risk-api"},"limit":10}"#,
                    expect_status: 200,
                    expect_contains: &[r#""total":0"#],
                    reject_contains: &[r#""traceId":"77001""#],
                },
            ],
        },
        ApiEvalCase {
            name: "bad ingest does not persist partial data",
            category: "api-contract",
            steps: vec![
                ApiEvalStep {
                    name: "bad attrs rejected",
                    method: "POST",
                    path: "/v1/ingest",
                    tenant: Some(778),
                    body: r#"[{"trace_id":77801,"span_id":1,"session_id":7781,"ts":10,"seq":1,"event_type":2,"ext_span_id":"77801-1","attrs":"not-an-object"}]"#,
                    expect_status: 400,
                    expect_contains: &[],
                    reject_contains: &[r#""ingested""#],
                },
                ApiEvalStep {
                    name: "bad trace not visible",
                    method: "POST",
                    path: "/v1/trace-search",
                    tenant: Some(778),
                    body: r#"{"filter":{"traceId":77801},"limit":10}"#,
                    expect_status: 200,
                    expect_contains: &[r#""total":0"#],
                    reject_contains: &[r#""traceId":"77801""#],
                },
                ApiEvalStep {
                    name: "bad search body rejected",
                    method: "POST",
                    path: "/v1/search",
                    tenant: Some(778),
                    body: r#"not-json"#,
                    expect_status: 400,
                    expect_contains: &[r#""error""#],
                    reject_contains: &[],
                },
            ],
        },
    ];
    run_api_eval_suite("risk-api-contract", &api, &cases).cases
}

fn read_plan_observability_case() -> EvalCaseReport {
    let (coord, api) = fresh_api();
    let batch = r#"[
      {"trace_id":78101,"span_id":1,"session_id":7811,"ts":10,"seq":1,"event_type":2,"ext_span_id":"78101-1","status":0,"duration_ns":100,"tool_name":"planner","input_tokens":10,"output_tokens":1,"attrs":{"project_id":"risk-read-plan","validation_status":"pass","skill":"review","mode":"auto","task_fingerprint":"risk-task"}},
      {"trace_id":78102,"span_id":1,"session_id":7812,"ts":20,"seq":1,"event_type":2,"ext_span_id":"78102-1","status":1,"duration_ns":200,"tool_name":"executor","input_tokens":20,"output_tokens":2,"attrs":{"project_id":"risk-read-plan","validation_status":"fail","skill":"review","mode":"auto","task_fingerprint":"risk-task"}}
    ]"#;
    let (ingest_status, ingest_body) =
        api.route_with_tenant("POST", "/v1/ingest", batch, Some(781));
    coord.flush_memtable();
    let query = r#"{"filter":{"projectId":"risk-read-plan"},"groupBy":["validationStatus","toolName"],"limit":10}"#;
    let (status, body) = api.route_with_tenant("POST", "/v1/trace-aggregate", query, Some(781));
    let (_, cached) = api.route_with_tenant("POST", "/v1/trace-aggregate", query, Some(781));

    case(
        "trace aggregate exposes real read plan",
        "performance-read-plan",
        vec![
            check(
                "seed ingest succeeds",
                ingest_status == 200,
                format!("{ingest_status} {ingest_body}"),
            ),
            check(
                "aggregate succeeds",
                status == 200,
                format!("{status} {body}"),
            ),
            check(
                "read plan names span index",
                body.contains(r#""readPlan""#)
                    && body.contains(r#""spanReadIndex":"aggregate_preaggregate""#),
                body.clone(),
            ),
            check(
                "rollup path is observable",
                body.contains(r#""usedSegmentRollup":true"#)
                    && body
                        .contains(r#""aggregationPlanner":"aggregate_preaggregate_tail_overlay""#),
                body.clone(),
            ),
            check(
                "cache hit is observable on repeated query",
                cached.contains(r#""readModelCache":"hit""#),
                cached,
            ),
        ],
    )
}

fn retention_storage_case() -> EvalCaseReport {
    let dir = durable_dir("retention");
    let coord = WriteCoordinator::open_durable(&dir).unwrap();
    coord.recover();
    let api = EngineJsonApi::new(Arc::clone(&coord));
    let batch = r#"[
      {"trace_id":78201,"span_id":1,"session_id":7821,"ts":10,"seq":1,"event_type":2,"ext_span_id":"78201-1","status":0,"duration_ns":10,"input_text":"old deletable","attrs":{"project_id":"risk-retention"}},
      {"trace_id":78202,"span_id":1,"session_id":7822,"ts":20,"seq":1,"event_type":2,"ext_span_id":"78202-1","status":0,"duration_ns":10,"input_text":"old protected","attrs":{"project_id":"risk-retention"}},
      {"trace_id":78203,"span_id":1,"session_id":7823,"ts":200,"seq":1,"event_type":2,"ext_span_id":"78203-1","status":0,"duration_ns":10,"input_text":"new keep","attrs":{"project_id":"risk-retention"}}
    ]"#;
    let (ingest_status, ingest_body) =
        api.route_with_tenant("POST", "/v1/ingest", batch, Some(782));
    coord.flush_memtable();
    let (annotation_status, annotation_body) = api.route_with_tenant(
        "POST",
        "/v1/annotations",
        r#"{"traceId":78202,"target":"trace","label":"retain","source":"risk-eval"}"#,
        Some(782),
    );
    let plan_body = r#"{"filter":{"projectId":"risk-retention"},"deleteBeforeTs":100}"#;
    let (plan_status, plan) =
        api.route_with_tenant("POST", "/v1/retention-plan", plan_body, Some(782));
    let apply_body = r#"{"filter":{"projectId":"risk-retention"},"deleteBeforeTs":100,"requestedBy":"risk-eval","reason":"ttl cleanup"}"#;
    let (apply_status, applied) =
        api.route_with_tenant("POST", "/v1/retention/apply", apply_body, Some(782));
    let (after_status, after) = api.route_with_tenant(
        "POST",
        "/v1/trace-search",
        r#"{"filter":{"projectId":"risk-retention"},"limit":10}"#,
        Some(782),
    );
    let _ = std::fs::remove_dir_all(&dir);

    case(
        "retention protects metadata and deletes only eligible cold traces",
        "retention-storage",
        vec![
            check(
                "seed ingest succeeds",
                ingest_status == 200,
                format!("{ingest_status} {ingest_body}"),
            ),
            check(
                "annotation seed succeeds",
                annotation_status == 200,
                format!("{annotation_status} {annotation_body}"),
            ),
            check(
                "plan sees candidate, protected and deletable sets",
                plan_status == 200
                    && plan.contains(r#""candidates":{"traceCount":2"#)
                    && plan.contains(r#""protected":{"traceCount":1"#)
                    && plan.contains(r#""deletable":{"traceCount":1"#)
                    && plan.contains(r#""78202":["annotation"]"#),
                plan.clone(),
            ),
            check(
                "apply deletes only unprotected old trace",
                apply_status == 200
                    && applied.contains(r#""deletedTraceCount":1"#)
                    && applied.contains(r#""deletedTraceIds":["78201"]"#),
                applied.clone(),
            ),
            check(
                "query hides deleted trace and keeps protected/new traces",
                after_status == 200
                    && after.contains(r#""traceId":"78202""#)
                    && after.contains(r#""traceId":"78203""#)
                    && !after.contains(r#""traceId":"78201""#),
                after,
            ),
        ],
    )
}

fn package_contract_case() -> EvalCaseReport {
    let root = repo_root();
    let node = std::fs::read_to_string(root.join("yitrace-node/package.json"));
    let python = std::fs::read_to_string(root.join("yitrace-db-python/pyproject.toml"));
    let rust = std::fs::read_to_string(root.join("yitrace-db-rs/Cargo.toml"));

    let node_body = node.unwrap_or_else(|e| format!("read failed: {e}"));
    let python_body = python.unwrap_or_else(|e| format!("read failed: {e}"));
    let rust_body = rust.unwrap_or_else(|e| format!("read failed: {e}"));

    case(
        "embedded package manifests keep lockable install contract",
        "release-packaging",
        vec![
            check(
                "node root package exposes ESM/CJS/types",
                node_body.contains(r#""exports""#)
                    && node_body.contains(r#""import": "./index.js""#)
                    && node_body.contains(r#""require": "./index.cjs""#)
                    && node_body.contains(r#""types": "./index.d.ts""#),
                node_body.clone(),
            ),
            check(
                "node root package has platform optional packages and pack verify",
                node_body.contains(r#""optionalDependencies""#)
                    && node_body.contains(r#""@yitrace/db-darwin-arm64""#)
                    && node_body.contains(r#""@yitrace/db-linux-x64-gnu""#)
                    && node_body.contains(r#""pack:verify""#),
                node_body,
            ),
            check(
                "python embedded db declares maturin native module",
                python_body.contains("maturin")
                    && python_body.contains(r#"name = "yitrace-db""#)
                    && python_body.contains(r#"module-name = "yitrace_db._native""#),
                python_body,
            ),
            check(
                "rust embedded db keeps public crate boundary explicit",
                rust_body.contains(r#"name = "yitrace-db""#)
                    && rust_body
                        .contains(r#"yt-engine = { path = "../yitrace-engine/crates/yt-engine" }"#)
                    && rust_body.contains("publish = false"),
                rust_body,
            ),
        ],
    )
}

fn gateway_security_case() -> EvalCaseReport {
    let gateway = RemoteShardGateway::new(vec!["127.0.0.1:1".to_string()]).unwrap();
    let server = RemoteGatewayServer::new(gateway)
        .with_auth_token("secret")
        .with_max_body(4);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || server.serve_n(&listener, 2));

    let (unauth_status, unauth_body) =
        socket_request(addr, "GET", "/v1/cluster/shards", "", None, None);
    let (large_status, large_body) = socket_request_declared_length(
        addr,
        "POST",
        "/v1/ingest",
        999_999,
        Some(700),
        Some("secret"),
    );
    handle.join().unwrap();

    case(
        "gateway server enforces auth and body limits",
        "security",
        vec![
            check(
                "missing bearer token is rejected",
                unauth_status == 401 && unauth_body.contains("unauthorized"),
                format!("{unauth_status} {unauth_body}"),
            ),
            check(
                "oversized body is rejected before routing",
                large_status == 413 && large_body.contains("body too large"),
                format!("{large_status} {large_body}"),
            ),
        ],
    )
}

fn http_server_tenant_socket_case() -> EvalCaseReport {
    let coord = WriteCoordinator::new(Arc::new(InMemorySegmentStore::default()));
    let server = HttpIngestServer::new(coord);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || server.serve_n(&listener, 3));

    let body = r#"[{"trace_id":78301,"span_id":1,"session_id":7831,"tenant_id":999,"ts":10,"seq":1,"event_type":2,"ext_span_id":"78301-1","status":0,"duration_ns":10,"input_text":"socket tenant header wins","attrs":{"project_id":"risk-socket"}}]"#;
    let (ingest_status, ingest_body) =
        socket_request(addr, "POST", "/v1/ingest", body, Some(783), None);
    let query = r#"{"filter":{"projectId":"risk-socket"},"limit":10}"#;
    let (right_status, right_body) =
        socket_request(addr, "POST", "/v1/trace-search", query, Some(783), None);
    let (wrong_status, wrong_body) =
        socket_request(addr, "POST", "/v1/trace-search", query, Some(999), None);
    handle.join().unwrap();

    case(
        "http server tenant comes from header over socket",
        "security",
        vec![
            check(
                "socket ingest succeeds",
                ingest_status == 200,
                format!("{ingest_status} {ingest_body}"),
            ),
            check(
                "header tenant can read socket trace",
                right_status == 200 && right_body.contains(r#""traceId":"78301""#),
                format!("{right_status} {right_body}"),
            ),
            check(
                "body tenant cannot read socket trace",
                wrong_status == 200
                    && wrong_body.contains(r#""total":0"#)
                    && !wrong_body.contains(r#""traceId":"78301""#),
                format!("{wrong_status} {wrong_body}"),
            ),
        ],
    )
}

#[test]
fn risk_eval_matrix_framework_covers_project_risks() {
    let mut cases = vec![distributed_write_safety_case()];
    cases.extend(api_contract_cases());
    cases.push(read_plan_observability_case());
    cases.push(retention_storage_case());
    cases.push(package_contract_case());
    cases.push(gateway_security_case());
    cases.push(http_server_tenant_socket_case());

    assert_suite(EvalSuiteReport {
        name: "risk_eval_matrix".to_string(),
        cases,
    });
}
