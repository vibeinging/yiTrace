//! 主线风险矩阵 eval。
//!
//! 这里放最容易被迁移误伤的单机能力：租户隔离、错误请求不落脏数据、
//! attrs 过滤、trace/span 详情、持久化恢复和基础 HTTP 安全边界。

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use yt_engine::{EngineJsonApi, HttpIngestServer, InMemorySegmentStore, WriteCoordinator};

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

fn assert_contains(body: &str, needle: &str) {
    assert!(body.contains(needle), "missing {needle:?} in {body}");
}

fn assert_not_contains(body: &str, needle: &str) {
    assert!(!body.contains(needle), "unexpected {needle:?} in {body}");
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

#[test]
fn tenant_header_overrides_body_tenant_and_filters_reads() {
    let (_, api) = fresh_api();
    let batch = r#"[{"trace_id":77001,"span_id":1,"session_id":7701,"tenant_id":999,"ts":10,"seq":1,"event_type":2,"ext_span_id":"77001-1","status":0,"duration_ns":10,"input_text":"tenant header wins 盗刷","attrs":{"project_id":"risk-api","skill":"contract"}}]"#;
    let (status, body) = api.route_with_tenant("POST", "/v1/ingest", batch, Some(777));
    assert_eq!(status, 200, "{body}");
    assert_contains(&body, r#""ingested":1"#);

    let (_, visible) = api.route_with_tenant("GET", "/v1/traces", "", Some(777));
    assert_contains(&visible, r#""trace_id":77001"#);

    let (_, hidden) = api.route_with_tenant("GET", "/v1/traces", "", Some(999));
    assert_not_contains(&hidden, r#""trace_id":77001"#);

    let (_, search) =
        api.route_with_tenant("POST", "/v1/search", r#"{"text":"盗刷","k":10}"#, Some(777));
    assert_contains(&search, r#""trace_id":77001"#);
}

#[test]
fn bad_ingest_does_not_persist_partial_data() {
    let (_, api) = fresh_api();
    let bad = r#"[
      {"trace_id":77801,"span_id":1,"session_id":7781,"ts":10,"seq":1,"event_type":2,"ext_span_id":"77801-1","input_text":"partial write must not survive"},
      {"span_id":2,"session_id":7781,"ts":11,"seq":1,"event_type":2,"ext_span_id":"77801-2"}
    ]"#;
    let (status, body) = api.route_with_tenant("POST", "/v1/ingest", bad, Some(778));
    assert_eq!(status, 400, "{body}");

    let (_, traces) = api.route_with_tenant("GET", "/v1/traces", "", Some(778));
    assert_not_contains(&traces, r#""trace_id":77801"#);

    let (status, search) = api.route_with_tenant(
        "POST",
        "/v1/search",
        r#"{"text":"77801","k":10}"#,
        Some(778),
    );
    assert_eq!(status, 200, "{search}");
    assert_not_contains(&search, r#""trace_id":77801"#);
}

#[test]
fn attrs_external_ids_and_log_events_round_trip() {
    let (_, api) = fresh_api();
    let batch = r#"[
      {"trace_id":"run-risk","span_id":"span-risk","session_id":"session-risk","ts":10,"seq":1,"event_type":1,"ext_span_id":"span-risk","agent_name":"风控 Agent","input_text":"疑似盗刷","attrs":{"project_id":"risk-api","skill":"review","mode":"auto","call_site":"worker.ts:10"}},
      {"trace_id":"run-risk","span_id":"span-risk","session_id":"session-risk","ts":11,"seq":2,"event_type":4,"ext_span_id":"span-risk","logs":["读取 package.json"],"attrs":{"phase":"read"}},
      {"trace_id":"run-risk","span_id":"span-risk","session_id":"session-risk","ts":20,"seq":3,"event_type":2,"ext_span_id":"span-risk","status":0,"duration_ns":100,"output_text":"需要人工复核","attrs":{"validation_status":"pass"}}
    ]"#;
    let (status, body) = api.route_with_tenant("POST", "/v1/ingest", batch, Some(1));
    assert_eq!(status, 200, "{body}");

    let (status, search) = api.route_with_tenant(
        "POST",
        "/v1/search",
        r#"{"text":"盗刷","k":10,"filter":{"attrs":{"project_id":"risk-api","skill":"review"}}}"#,
        Some(1),
    );
    assert_eq!(status, 200, "{search}");
    assert_contains(&search, r#""external_trace_id":"run-risk""#);

    let (status, trace) = api.route_with_tenant("GET", "/v1/traces/run-risk", "", Some(1));
    assert_eq!(status, 200, "{trace}");
    assert_contains(&trace, r#""externalTraceId":"run-risk""#);
    assert_contains(&trace, r#""project_id":"risk-api""#);
    assert_contains(&trace, r#""logEvents""#);
    assert_contains(&trace, "读取 package.json");

    let (status, span) =
        api.route_with_tenant("GET", "/v1/traces/run-risk/spans/span-risk", "", Some(1));
    assert_eq!(status, 200, "{span}");
    assert_contains(&span, r#""externalSpanId":"span-risk""#);
    assert_contains(&span, r#""logEvents""#);
    assert_contains(&span, r#""phase":"read""#);
}

#[test]
fn trace_search_aggregate_and_storage_stats_are_tenant_scoped() {
    let (_, api) = fresh_api();
    let tenant1 = r#"[
      {"trace_id":"run-read-model-1","span_id":"span-plan","session_id":"session-read-model","ts":10,"seq":1,"event_type":1,"ext_span_id":"span-plan","agent_name":"planner","input_text":"规划退款审核","attrs":{"project_id":"read-model","skill":"refund","validation_status":"pass"}},
      {"trace_id":"run-read-model-1","span_id":"span-plan","session_id":"session-read-model","ts":20,"seq":2,"event_type":2,"ext_span_id":"span-plan","model":"qwen3","status":0,"duration_ns":100,"output_text":"完成退款审核","input_tokens":10,"output_tokens":5,"attrs":{"project_id":"read-model","skill":"refund","validation_status":"pass"}},
      {"trace_id":"run-read-model-2","span_id":"span-tool","session_id":"session-read-model","ts":30,"seq":1,"event_type":2,"ext_span_id":"span-tool","tool_name":"shell","status":1,"duration_ns":300,"output_text":"退款审核失败","input_tokens":20,"output_tokens":8,"attrs":{"project_id":"read-model","skill":"refund","validation_status":"fail"}}
    ]"#;
    let tenant2 = r#"[
      {"trace_id":"run-read-model-hidden","span_id":"span-hidden","session_id":"session-hidden","ts":10,"seq":1,"event_type":2,"ext_span_id":"span-hidden","status":0,"output_text":"退款审核 hidden","attrs":{"project_id":"read-model","skill":"refund","validation_status":"pass"}}
    ]"#;
    assert_eq!(
        api.route_with_tenant("POST", "/v1/ingest", tenant1, Some(1))
            .0,
        200
    );
    assert_eq!(
        api.route_with_tenant("POST", "/v1/ingest", tenant2, Some(2))
            .0,
        200
    );

    let search_body =
        r#"{"filter":{"projectId":"read-model","skill":"refund"},"text":"退款审核","limit":10}"#;
    let (status, search) = api.route_with_tenant("POST", "/v1/trace-search", search_body, Some(1));
    assert_eq!(status, 200, "{search}");
    assert_contains(&search, r#""total":2"#);
    assert_contains(&search, r#""externalTraceId":"run-read-model-1""#);
    assert_contains(&search, r#""externalTraceId":"run-read-model-2""#);
    assert_contains(&search, r#""readPlan":{"source":"filter_index""#);
    assert_contains(&search, r#""usedFilterIndex":true"#);
    assert_contains(&search, r#""candidateSpanKeys":2"#);
    assert_not_contains(&search, "run-read-model-hidden");

    let tool_body = r#"{"filter":{"toolName":"shell","validationStatus":"fail"},"limit":10}"#;
    let (status, tool_search) =
        api.route_with_tenant("POST", "/v1/trace-search", tool_body, Some(1));
    assert_eq!(status, 200, "{tool_search}");
    assert_contains(&tool_search, r#""total":1"#);
    assert_contains(&tool_search, r#""externalTraceId":"run-read-model-2""#);
    assert_contains(&tool_search, r#""usedFilterIndex":true"#);
    assert_contains(&tool_search, r#""candidateSpanKeys":1"#);

    let model_body = r#"{"filter":{"model":"qwen3","validationStatus":"pass"},"limit":10}"#;
    let (status, model_search) =
        api.route_with_tenant("POST", "/v1/trace-search", model_body, Some(1));
    assert_eq!(status, 200, "{model_search}");
    assert_contains(&model_search, r#""total":1"#);
    assert_contains(&model_search, r#""externalTraceId":"run-read-model-1""#);
    assert_contains(&model_search, r#""usedFilterIndex":true"#);

    let text_only_body = r#"{"text":"退款审核","limit":10}"#;
    let (status, text_only) =
        api.route_with_tenant("POST", "/v1/trace-search", text_only_body, None);
    assert_eq!(status, 200, "{text_only}");
    assert_contains(&text_only, r#""source":"bm25""#);
    assert_contains(&text_only, r#""usedFilterIndex":false"#);
    assert_contains(&text_only, r#""fallbackReason":"no_indexed_filter""#);

    let aggregate_body = r#"{"filter":{"projectId":"read-model","skill":"refund"},"groupBy":["validationStatus"],"limit":10}"#;
    let (status, aggregate) =
        api.route_with_tenant("POST", "/v1/trace-aggregate", aggregate_body, Some(1));
    assert_eq!(status, 200, "{aggregate}");
    assert_contains(&aggregate, r#""validation_status":"pass""#);
    assert_contains(&aggregate, r#""validation_status":"fail""#);
    assert_contains(&aggregate, r#""spanCount":1"#);
    assert_contains(&aggregate, r#""readPlan":{"source":"aggregate_rollup""#);
    assert_contains(&aggregate, r#""candidateSpanKeys":2"#);

    let text_aggregate_body = r#"{"text":"退款审核","filter":{"projectId":"read-model","skill":"refund"},"groupBy":["validationStatus"],"limit":10}"#;
    let (status, text_aggregate) =
        api.route_with_tenant("POST", "/v1/trace-aggregate", text_aggregate_body, Some(1));
    assert_eq!(status, 200, "{text_aggregate}");
    assert_contains(&text_aggregate, r#""readPlan":{"source":"filter_index""#);
    assert_contains(&text_aggregate, r#""total":2"#);

    let storage_body = r#"{"filter":{"projectId":"read-model"},"groupBy":["validationStatus"]}"#;
    let (status, storage) =
        api.route_with_tenant("POST", "/v1/storage-stats", storage_body, Some(1));
    assert_eq!(status, 200, "{storage}");
    assert_contains(&storage, r#""traceCount":2"#);
    assert_contains(&storage, r#""spanCount":2"#);
    assert_contains(&storage, r#""estimatedBytes":"#);
    assert_contains(&storage, r#""readPlan":{"source":"trajectory_rollup""#);

    let (status, hidden) = api.route_with_tenant("POST", "/v1/trace-search", search_body, Some(2));
    assert_eq!(status, 200, "{hidden}");
    assert_contains(&hidden, r#""total":1"#);
    assert_contains(&hidden, "run-read-model-hidden");
    assert_not_contains(&hidden, "run-read-model-1");
}

#[test]
fn trajectory_loop_task_and_diff_read_models_are_tenant_scoped() {
    let (_, api) = fresh_api();
    let tenant1 = r#"[
      {"trace_id":"task-run-a","span_id":"plan","session_id":"session-task","ts":10,"seq":1,"event_type":1,"ext_span_id":"task-run-a-plan","agent_name":"planner","input_text":"退款审核规划","attrs":{"project_id":"read-model","skill":"refund","task_fingerprint":"refund-v1","loop_id":"loop-refund","validation_status":"pass"}},
      {"trace_id":"task-run-a","span_id":"plan","session_id":"session-task","ts":20,"seq":2,"event_type":2,"ext_span_id":"task-run-a-plan","status":0,"duration_ns":100,"output_text":"规划完成","input_tokens":10,"output_tokens":5,"attrs":{"project_id":"read-model","skill":"refund","task_fingerprint":"refund-v1","loop_id":"loop-refund","validation_status":"pass"}},
      {"trace_id":"task-run-a","span_id":"tool","parent_span_id":"plan","session_id":"session-task","ts":30,"seq":1,"event_type":2,"ext_span_id":"task-run-a-tool","tool_name":"sql.check","status":0,"duration_ns":50,"output_text":"检查通过","attrs":{"project_id":"read-model","skill":"refund","task_fingerprint":"refund-v1","loop_id":"loop-refund","validation_status":"pass"}},

      {"trace_id":"task-run-b","span_id":"plan","session_id":"session-task","ts":40,"seq":1,"event_type":2,"ext_span_id":"task-run-b-plan","agent_name":"planner","status":0,"duration_ns":80,"input_text":"退款审核规划","attrs":{"project_id":"read-model","skill":"refund","task_fingerprint":"refund-v1","loop_id":"loop-refund","validation_status":"fail"}},
      {"trace_id":"task-run-b","span_id":"manual","parent_span_id":"plan","session_id":"session-task","ts":50,"seq":1,"event_type":2,"ext_span_id":"task-run-b-manual","tool_name":"manual.review","status":1,"duration_ns":300,"output_text":"需要人工介入","attrs":{"project_id":"read-model","skill":"refund","task_fingerprint":"refund-v1","loop_id":"loop-refund","validation_status":"fail"}}
    ]"#;
    let tenant2 = r#"[
      {"trace_id":"task-run-hidden","span_id":"plan","session_id":"hidden","ts":10,"seq":1,"event_type":2,"ext_span_id":"task-run-hidden-plan","agent_name":"planner","status":0,"duration_ns":10,"attrs":{"project_id":"read-model","skill":"refund","task_fingerprint":"refund-v1","loop_id":"loop-refund","validation_status":"pass"}}
    ]"#;
    assert_eq!(
        api.route_with_tenant("POST", "/v1/ingest", tenant1, Some(1))
            .0,
        200
    );
    assert_eq!(
        api.route_with_tenant("POST", "/v1/ingest", tenant2, Some(2))
            .0,
        200
    );

    let filter =
        r#"{"filter":{"projectId":"read-model","taskFingerprint":"refund-v1"},"limit":10}"#;
    let (status, trajectories) =
        api.route_with_tenant("POST", "/v1/trace-trajectories", filter, Some(1));
    assert_eq!(status, 200, "{trajectories}");
    assert_contains(&trajectories, r#""total":2"#);
    assert_contains(&trajectories, r#""externalTraceId":"task-run-a""#);
    assert_contains(&trajectories, r#""externalTraceId":"task-run-b""#);
    assert_contains(&trajectories, r#""steps""#);
    assert_contains(&trajectories, "sql.check");
    assert_contains(&trajectories, r#""readPlan":{"source":"trajectory_rollup""#);
    assert_contains(&trajectories, r#""scannedSegments":0"#);
    assert_contains(&trajectories, r#""traceFetchSource":"trajectory_rollup""#);
    assert_contains(&trajectories, r#""traceFetchSpanCount":4"#);
    assert_not_contains(&trajectories, "task-run-hidden");

    let (status, groups) = api.route_with_tenant("POST", "/v1/trajectory-groups", filter, Some(1));
    assert_eq!(status, 200, "{groups}");
    assert_contains(&groups, r#""total":2"#);
    assert_contains(&groups, r#""traceCount":1"#);
    assert_contains(&groups, r#""successCount":1"#);
    assert_contains(&groups, "manual.review");
    assert_contains(&groups, r#""readPlan":{"source":"trajectory_rollup""#);
    assert_contains(&groups, r#""scannedSegments":0"#);
    assert_contains(&groups, r#""traceFetchSource":"trajectory_rollup""#);
    assert_contains(&groups, r#""traceFetchSpanCount":4"#);
    assert_not_contains(&groups, "task-run-hidden");

    let diff_body = r#"{"baseTraceId":"task-run-a","candidateTraceId":"task-run-b"}"#;
    let (status, diff) = api.route_with_tenant("POST", "/v1/traces/diff", diff_body, Some(1));
    assert_eq!(status, 200, "{diff}");
    assert_contains(&diff, r#""sameSignature":false"#);
    assert_contains(&diff, r#""commonPrefix":1"#);
    assert_contains(&diff, r#""missingSteps""#);
    assert_contains(&diff, r#""extraSteps""#);

    let (status, loops) =
        api.route_with_tenant("GET", "/v1/loops?projectId=read-model", "", Some(1));
    assert_eq!(status, 200, "{loops}");
    assert_contains(&loops, r#""loopId":"loop-refund""#);
    assert_contains(&loops, r#""traceCount":2"#);
    assert_contains(&loops, r#""readPlan":{"source":"trajectory_rollup""#);
    assert_contains(&loops, r#""scannedSegments":0"#);

    let (status, loop_detail) = api.route_with_tenant("GET", "/v1/loops/loop-refund", "", Some(1));
    assert_eq!(status, 200, "{loop_detail}");
    assert_contains(&loop_detail, r#""summary""#);
    assert_contains(&loop_detail, r#""traces""#);
    assert_contains(&loop_detail, r#""spans""#);
    assert_contains(&loop_detail, r#""readPlan":{"source":"trajectory_rollup""#);
    assert_contains(&loop_detail, r#""scannedSegments":0"#);
    assert_contains(&loop_detail, r#""traceFetchSource":"trajectory_rollup""#);
    assert_contains(&loop_detail, r#""traceFetchSpanCount":4"#);
    assert_not_contains(&loop_detail, "task-run-hidden");

    let (status, task_pass) = api.route_with_tenant(
        "GET",
        "/v1/tasks/refund-v1/traces?validationStatus=pass",
        "",
        Some(1),
    );
    assert_eq!(status, 200, "{task_pass}");
    assert_contains(&task_pass, r#""total":1"#);
    assert_contains(&task_pass, "task-run-a");
    assert_contains(&task_pass, r#""readPlan":{"source":"trajectory_rollup""#);
    assert_contains(&task_pass, r#""scannedSegments":0"#);
    assert_contains(&task_pass, r#""traceFetchSource":"trajectory_rollup""#);
    assert_contains(&task_pass, r#""traceFetchSpanCount":4"#);
    assert_not_contains(&task_pass, "task-run-b");

    let (status, hidden) = api.route_with_tenant("POST", "/v1/trace-trajectories", filter, Some(2));
    assert_eq!(status, 200, "{hidden}");
    assert_contains(&hidden, r#""total":1"#);
    assert_contains(&hidden, "task-run-hidden");
    assert_not_contains(&hidden, "task-run-a");
}

#[test]
fn durable_reopen_preserves_searchable_trace() {
    let dir = durable_dir("reopen");
    {
        let coord = WriteCoordinator::open_durable(&dir).unwrap();
        coord.recover();
        let api = EngineJsonApi::new(Arc::clone(&coord));
        let batch = r#"[{"trace_id":88001,"span_id":1,"session_id":8800,"ts":10,"seq":1,"event_type":2,"ext_span_id":"88001-1","agent_name":"planner","status":0,"duration_ns":10,"output_text":"reopen 盗刷 source","attrs":{"project_id":"reopen-risk","skill":"durable","task_fingerprint":"reopen-task","loop_id":"reopen-loop","validation_status":"pass"}}]"#;
        assert_eq!(
            api.route_with_tenant("POST", "/v1/ingest", batch, Some(880))
                .0,
            200
        );
        coord.flush_memtable();
        assert!(
            dir.join("trace_rollup.dat").exists(),
            "flush 后应写出 segment-only trace rollup cache"
        );
        assert!(
            dir.join("filter_attrs.dat").exists(),
            "flush 后应写出 segment-only attrs filter sidecar cache"
        );
        assert!(
            dir.join("wal.state").exists(),
            "flush 后应写出 WAL checkpoint，恢复只扫描 watermark 后的尾部"
        );
    }
    {
        let reopened = WriteCoordinator::open_durable(&dir).unwrap();
        reopened.recover();
        let api = EngineJsonApi::new(reopened);
        let (status, body) =
            api.route_with_tenant("POST", "/v1/search", r#"{"text":"盗刷","k":10}"#, Some(880));
        assert_eq!(status, 200, "{body}");
        assert_contains(&body, r#""trace_id":88001"#);
        assert_contains(&body, r#""project_id":"reopen-risk""#);

        let (status, point_read) = api.route_with_tenant(
            "POST",
            "/v1/trace-search",
            r#"{"text":"盗刷","filter":{"projectId":"reopen-risk"},"limit":10}"#,
            Some(880),
        );
        assert_eq!(status, 200, "{point_read}");
        assert_contains(&point_read, r#""pointLookupSegments":1"#);
        assert_contains(&point_read, r#""decodedSegmentRows":1"#);
        assert_contains(&point_read, r#""indexBytesRead":"#);
        assert_contains(&point_read, r#""dataBytesRead":"#);
        assert_contains(&point_read, r#""indexesValidated":"#);
        assert_contains(&point_read, r#""indexesRebuilt":"#);
        assert_contains(&point_read, r#""total":1"#);

        let aggregate_body =
            r#"{"filter":{"projectId":"reopen-risk"},"groupBy":["skill"],"limit":10}"#;
        let (status, aggregate) =
            api.route_with_tenant("POST", "/v1/trace-aggregate", aggregate_body, Some(880));
        assert_eq!(status, 200, "{aggregate}");
        assert_contains(&aggregate, r#""readPlan":{"source":"aggregate_rollup""#);
        assert_contains(&aggregate, r#""scannedSegments":0"#);
        assert_contains(&aggregate, r#""total":1"#);

        let trajectory_body =
            r#"{"filter":{"projectId":"reopen-risk","taskFingerprint":"reopen-task"},"limit":10}"#;
        let (status, trajectories) =
            api.route_with_tenant("POST", "/v1/trace-trajectories", trajectory_body, Some(880));
        assert_eq!(status, 200, "{trajectories}");
        assert_contains(&trajectories, r#""readPlan":{"source":"trajectory_rollup""#);
        assert_contains(&trajectories, r#""scannedSegments":0"#);
        assert_contains(&trajectories, r#""traceId":"88001""#);

        let (status, loops) =
            api.route_with_tenant("GET", "/v1/loops?projectId=reopen-risk", "", Some(880));
        assert_eq!(status, 200, "{loops}");
        assert_contains(&loops, r#""readPlan":{"source":"trajectory_rollup""#);
        assert_contains(&loops, r#""scannedSegments":0"#);
        assert_contains(&loops, r#""loopId":"reopen-loop""#);

        let (status, task_traces) = api.route_with_tenant(
            "GET",
            "/v1/tasks/reopen-task/traces?validationStatus=pass",
            "",
            Some(880),
        );
        assert_eq!(status, 200, "{task_traces}");
        assert_contains(&task_traces, r#""readPlan":{"source":"trajectory_rollup""#);
        assert_contains(&task_traces, r#""scannedSegments":0"#);
        assert_contains(&task_traces, r#""traceId":"88001""#);

        let search_body = r#"{"filter":{"projectId":"reopen-risk","validationStatus":"pass","agentName":"planner"},"limit":10}"#;
        let (status, indexed_search) =
            api.route_with_tenant("POST", "/v1/trace-search", search_body, Some(880));
        assert_eq!(status, 200, "{indexed_search}");
        assert_contains(
            &indexed_search,
            r#""readPlan":{"source":"trajectory_rollup""#,
        );
        assert_contains(&indexed_search, r#""candidateSpanKeys":1"#);
        assert_contains(&indexed_search, r#""total":1"#);
    }
    std::fs::write(dir.join("trace_rollup.dat"), b"bad-cache").unwrap();
    std::fs::write(dir.join("filter_attrs.dat"), b"bad-cache").unwrap();
    std::fs::write(dir.join("bm25.dat"), b"bad-cache").unwrap();
    std::fs::write(dir.join("segment_bloom.dat"), b"bad-cache").unwrap();
    {
        let reopened = WriteCoordinator::open_durable(&dir).unwrap();
        reopened.recover();
        let api = EngineJsonApi::new(reopened);
        let aggregate_body =
            r#"{"filter":{"projectId":"reopen-risk"},"groupBy":["skill"],"limit":10}"#;
        let (status, aggregate) =
            api.route_with_tenant("POST", "/v1/trace-aggregate", aggregate_body, Some(880));
        assert_eq!(status, 200, "{aggregate}");
        assert_contains(&aggregate, r#""readPlan":{"source":"aggregate_rollup""#);
        assert_contains(&aggregate, r#""total":1"#);

        let trajectory_body =
            r#"{"filter":{"projectId":"reopen-risk","taskFingerprint":"reopen-task"},"limit":10}"#;
        let (status, trajectories) =
            api.route_with_tenant("POST", "/v1/trace-trajectories", trajectory_body, Some(880));
        assert_eq!(status, 200, "{trajectories}");
        assert_contains(&trajectories, r#""readPlan":{"source":"trajectory_rollup""#);
        assert_contains(&trajectories, r#""total":1"#);

        let search_body = r#"{"filter":{"projectId":"reopen-risk","validationStatus":"pass","agentName":"planner"},"limit":10}"#;
        let (status, indexed_search) =
            api.route_with_tenant("POST", "/v1/trace-search", search_body, Some(880));
        assert_eq!(status, 200, "{indexed_search}");
        assert_contains(
            &indexed_search,
            r#""readPlan":{"source":"trajectory_rollup""#,
        );
        assert_contains(&indexed_search, r#""total":1"#);

        let (status, text_search) =
            api.route_with_tenant("POST", "/v1/search", r#"{"text":"盗刷","k":10}"#, Some(880));
        assert_eq!(status, 200, "{text_search}");
        assert_contains(&text_search, r#""trace_id":88001"#);
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn metadata_annotations_and_dataset_links_are_tenant_scoped_and_durable() {
    let dir = durable_dir("metadata");
    {
        let coord = WriteCoordinator::open_durable(&dir).unwrap();
        coord.recover();
        let api = EngineJsonApi::new(Arc::clone(&coord));
        let batch = r#"[
          {"trace_id":"meta-run","span_id":"meta-span","session_id":"meta-session","ts":10,"seq":1,"event_type":2,"ext_span_id":"meta-span","status":0,"duration_ns":10,"output_text":"metadata source","attrs":{"project_id":"agentic","skill":"review"}}
        ]"#;
        assert_eq!(
            api.route_with_tenant("POST", "/v1/ingest", batch, Some(42))
                .0,
            200
        );

        let annotation_body = r#"{"traceId":"meta-run","spanId":"meta-span","label":"best_path","score":920,"reason":"人工确认","source":"human","attrs":{"project_id":"agentic","skill":"review"}}"#;
        let (status, created) =
            api.route_with_tenant("POST", "/v1/annotations", annotation_body, Some(42));
        assert_eq!(status, 200, "{created}");
        assert_contains(&created, r#""annotationId":"1""#);
        assert_contains(&created, r#""externalTraceId":"meta-run""#);

        let (status, visible) = api.route_with_tenant(
            "GET",
            "/v1/annotations?projectId=agentic&label=best_path",
            "",
            Some(42),
        );
        assert_eq!(status, 200, "{visible}");
        assert_contains(&visible, r#""total":1"#);
        assert_contains(&visible, r#""skill":"review""#);

        let (_, hidden) =
            api.route_with_tenant("GET", "/v1/annotations?projectId=agentic", "", Some(7));
        assert_not_contains(&hidden, "best_path");

        let patch = r#"{"status":"resolved","reviewer":"qa","attrs":{"mode":"eval"}}"#;
        let (status, updated) =
            api.route_with_tenant("PATCH", "/v1/annotations/1", patch, Some(42));
        assert_eq!(status, 200, "{updated}");
        assert_contains(&updated, r#""status":"resolved""#);
        assert_contains(&updated, r#""mode":"eval""#);
        assert_contains(&updated, r#""project_id":"agentic""#);

        let (status, deleted) = api.route_with_tenant(
            "DELETE",
            "/v1/annotations/1",
            r#"{"reviewer":"qa","reason":"stale"}"#,
            Some(42),
        );
        assert_eq!(status, 200, "{deleted}");
        assert_contains(&deleted, r#""status":"deleted""#);

        let (_, without_deleted) =
            api.route_with_tenant("GET", "/v1/annotations?projectId=agentic", "", Some(42));
        assert_not_contains(&without_deleted, "best_path");

        let (_, with_deleted) = api.route_with_tenant(
            "GET",
            "/v1/annotations?projectId=agentic&includeDeleted=true",
            "",
            Some(42),
        );
        assert_contains(&with_deleted, r#""status":"deleted""#);
        assert_contains(&with_deleted, "stale");

        let link_body = r#"{"datasetId":"agentic-regression","itemId":"case-1","traceId":"meta-run","spanId":"meta-span","split":"eval","label":"pass","score":900,"attrs":{"project_id":"agentic","skill":"review"}}"#;
        let (status, linked) =
            api.route_with_tenant("POST", "/v1/dataset-associations", link_body, Some(42));
        assert_eq!(status, 200, "{linked}");
        assert_contains(&linked, r#""associationId":"1""#);
        assert_contains(&linked, r#""externalSpanId":"meta-span""#);

        let (_, links) = api.route_with_tenant(
            "GET",
            "/v1/dataset-associations?datasetId=agentic-regression&projectId=agentic",
            "",
            Some(42),
        );
        assert_contains(&links, r#""total":1"#);
        assert_contains(&links, r#""itemId":"case-1""#);

        let (_, hidden_links) = api.route_with_tenant(
            "GET",
            "/v1/dataset-associations?datasetId=agentic-regression",
            "",
            Some(7),
        );
        assert_not_contains(&hidden_links, "case-1");
    }
    {
        let reopened = WriteCoordinator::open_durable(&dir).unwrap();
        reopened.recover();
        let api = EngineJsonApi::new(reopened);
        let (_, annotations) = api.route_with_tenant(
            "GET",
            "/v1/annotations?projectId=agentic&includeDeleted=true",
            "",
            Some(42),
        );
        assert_contains(&annotations, r#""status":"deleted""#);
        assert_contains(&annotations, "best_path");

        let (_, links) = api.route_with_tenant(
            "GET",
            "/v1/dataset-associations?datasetId=agentic-regression",
            "",
            Some(42),
        );
        assert_contains(&links, r#""itemId":"case-1""#);
        assert_contains(&links, r#""split":"eval""#);
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn retention_plan_apply_audit_and_policy_are_durable() {
    let dir = durable_dir("retention");
    {
        let coord = WriteCoordinator::open_durable(&dir).unwrap();
        coord.recover();
        let api = EngineJsonApi::new(Arc::clone(&coord));
        let cold = r#"[
          {"trace_id":91001,"span_id":1,"session_id":9100,"ts":10,"seq":1,"event_type":2,"ext_span_id":"91001-1","status":0,"duration_ns":10,"output_text":"retention protected","attrs":{"project_id":"retention","skill":"cleanup"}},
          {"trace_id":91002,"span_id":1,"session_id":9100,"ts":20,"seq":1,"event_type":2,"ext_span_id":"91002-1","status":0,"duration_ns":10,"output_text":"retention deletable","attrs":{"project_id":"retention","skill":"cleanup"}}
        ]"#;
        assert_eq!(
            api.route_with_tenant("POST", "/v1/ingest", cold, Some(91))
                .0,
            200
        );
        coord.flush_memtable();

        let hot = r#"[
          {"trace_id":91003,"span_id":1,"session_id":9100,"ts":30,"seq":1,"event_type":2,"ext_span_id":"91003-1","status":0,"duration_ns":10,"output_text":"retention hot tail","attrs":{"project_id":"retention","skill":"cleanup"}}
        ]"#;
        assert_eq!(
            api.route_with_tenant("POST", "/v1/ingest", hot, Some(91)).0,
            200
        );

        let annotation = r#"{"traceId":91001,"label":"keep","source":"eval","attrs":{"project_id":"retention"}}"#;
        assert_eq!(
            api.route_with_tenant("POST", "/v1/annotations", annotation, Some(91))
                .0,
            200
        );

        let plan_body = r#"{"filter":{"projectId":"retention"},"deleteBeforeTs":100,"protect":{"annotations":true,"datasetAssociations":true,"snapshots":true,"evalLinks":true,"pathMemory":true},"requestedBy":"risk-eval","reason":"ttl","limit":10}"#;
        let (status, dry_run) =
            api.route_with_tenant("POST", "/v1/retention-plan", plan_body, Some(91));
        assert_eq!(status, 200, "{dry_run}");
        assert_contains(&dry_run, r#""dryRun":true"#);
        assert_contains(&dry_run, r#""traceCount":3"#);
        assert_contains(&dry_run, r#""protectedReasons":{"91001":["annotation"]}"#);
        assert_contains(&dry_run, r#""deletableTraceIds":["91002","91003"]"#);

        let (status, applied) =
            api.route_with_tenant("POST", "/v1/retention/apply", plan_body, Some(91));
        assert_eq!(status, 200, "{applied}");
        assert_contains(&applied, r#""applied":true"#);
        assert_contains(&applied, r#""deletedTraceIds":["91002"]"#);
        assert_contains(&applied, r#""skippedLiveTraceIds":["91003"]"#);
        assert_contains(&applied, r#""auditId":"1""#);

        let (status, traces) = api.route_with_tenant("GET", "/v1/traces", "", Some(91));
        assert_eq!(status, 200, "{traces}");
        assert_contains(&traces, r#""trace_id":91001"#);
        assert_not_contains(&traces, r#""trace_id":91002"#);
        assert_contains(&traces, r#""trace_id":91003"#);

        let aggregate_body =
            r#"{"filter":{"projectId":"retention"},"groupBy":["skill"],"limit":10}"#;
        let (status, aggregate) =
            api.route_with_tenant("POST", "/v1/trace-aggregate", aggregate_body, Some(91));
        assert_eq!(status, 200, "{aggregate}");
        assert_contains(&aggregate, r#""readPlan":{"source":"aggregate_rollup""#);
        assert_contains(&aggregate, r#""total":2"#);
        assert_not_contains(&aggregate, "91002");

        let (status, audits) =
            api.route_with_tenant("GET", "/v1/retention-audits?source=risk-eval", "", Some(91));
        assert_eq!(status, 200, "{audits}");
        assert_contains(&audits, r#""total":1"#);
        assert_contains(&audits, r#""deletedSegmentRowCount":1"#);
        let (status, audit_by_id) = api.route_with_tenant(
            "GET",
            "/v1/retention-audits?id=1&source=risk-eval",
            "",
            Some(91),
        );
        assert_eq!(status, 200, "{audit_by_id}");
        assert_contains(&audit_by_id, r#""total":1"#);

        let policy_body = r#"{"name":"nightly-retention","intervalNs":1000,"nextRunAtNs":1,"query":{"filter":{"projectId":"retention"},"deleteBeforeTs":100,"protect":{"annotations":true},"requestedBy":"policy"},"source":"policy","reason":"ttl"}"#;
        let (status, policy) =
            api.route_with_tenant("POST", "/v1/retention-policies", policy_body, Some(91));
        assert_eq!(status, 200, "{policy}");
        assert_contains(&policy, r#""policyId":"1""#);
        let (status, enabled_policy) = api.route_with_tenant(
            "GET",
            "/v1/retention-policies?name=nightly-retention&enabled=true",
            "",
            Some(91),
        );
        assert_eq!(status, 200, "{enabled_policy}");
        assert_contains(&enabled_policy, r#""total":1"#);
        let (status, disabled_policy) = api.route_with_tenant(
            "GET",
            "/v1/retention-policies?name=nightly-retention&enabled=false",
            "",
            Some(91),
        );
        assert_eq!(status, 200, "{disabled_policy}");
        assert_contains(&disabled_policy, r#""total":0"#);

        let (status, run_due) = api.route_with_tenant(
            "POST",
            "/v1/retention-policies/run-due",
            r#"{"nowNs":2,"limit":1}"#,
            Some(91),
        );
        assert_eq!(status, 200, "{run_due}");
        assert_contains(&run_due, r#""ran":1"#);
        assert_contains(&run_due, r#""skippedLiveTraceIds":["91003"]"#);
    }
    {
        let reopened = WriteCoordinator::open_durable(&dir).unwrap();
        reopened.recover();
        let api = EngineJsonApi::new(reopened);
        let (_, traces) = api.route_with_tenant("GET", "/v1/traces", "", Some(91));
        assert_contains(&traces, r#""trace_id":91001"#);
        assert_not_contains(&traces, r#""trace_id":91002"#);
        assert_contains(&traces, r#""trace_id":91003"#);

        let aggregate_body =
            r#"{"filter":{"projectId":"retention"},"groupBy":["skill"],"limit":10}"#;
        let (_, aggregate) =
            api.route_with_tenant("POST", "/v1/trace-aggregate", aggregate_body, Some(91));
        assert_contains(&aggregate, r#""readPlan":{"source":"aggregate_rollup""#);
        assert_contains(&aggregate, r#""total":2"#);
        assert_not_contains(&aggregate, "91002");

        let (_, audits) = api.route_with_tenant("GET", "/v1/retention-audits", "", Some(91));
        assert_contains(&audits, r#""total":2"#);
        assert_contains(&audits, r#""source":"policy""#);
        let (_, policy_audits) =
            api.route_with_tenant("GET", "/v1/retention-audits?source=policy", "", Some(91));
        assert_contains(&policy_audits, r#""total":1"#);

        let (_, policies) = api.route_with_tenant(
            "GET",
            "/v1/retention-policies?name=nightly-retention&enabled=true",
            "",
            Some(91),
        );
        assert_contains(&policies, r#""total":1"#);
        assert_contains(&policies, r#""lastRunAtNs":"2""#);
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn http_auth_body_limit_and_tenant_header_work_together() {
    let coord = WriteCoordinator::new(Arc::new(InMemorySegmentStore::default()));
    let server = HttpIngestServer::new(Arc::clone(&coord))
        .with_auth_token("secret")
        .with_max_body(256);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || server.serve_n(&listener, 3));

    let (status, body) = socket_request(addr, "GET", "/v1/traces", "", None, None);
    assert_eq!(status, 401, "{body}");

    let (status, body) =
        socket_request_declared_length(addr, "POST", "/v1/ingest", 10_000, Some(1), Some("secret"));
    assert_eq!(status, 413, "{body}");

    let batch = r#"[{"trace_id":99001,"span_id":1,"session_id":9900,"tenant_id":2,"ts":10,"seq":1,"event_type":2,"ext_span_id":"99001-1","status":0,"duration_ns":10,"input_text":"http tenant"}]"#;
    let (status, body) = socket_request(addr, "POST", "/v1/ingest", batch, Some(1), Some("secret"));
    assert_eq!(status, 200, "{body}");
    assert_contains(&body, r#""ingested":1"#);

    handle.join().unwrap();

    let api = EngineJsonApi::new(coord);
    let (_, t1) = api.route_with_tenant("GET", "/v1/traces", "", Some(1));
    assert_contains(&t1, r#""trace_id":99001"#);
    let (_, t2) = api.route_with_tenant("GET", "/v1/traces", "", Some(2));
    assert_not_contains(&t2, r#""trace_id":99001"#);
}

#[test]
fn automatic_flush_defers_full_sidecar_save_until_explicit_flush() {
    let dir = durable_dir("deferred-sidecar-save");
    {
        let coord = WriteCoordinator::open_durable(&dir).unwrap();
        coord.recover();
        let rollup_path = dir.join("trace_rollup.dat");
        let attrs_path = dir.join("filter_attrs.dat");
        assert!(!rollup_path.exists());
        assert!(!attrs_path.exists());
        let api = EngineJsonApi::new(Arc::clone(&coord));
        coord.set_flush_threshold(100);
        let first = r#"[{"trace_id":99011,"span_id":1,"ts":10,"seq":1,"event_type":2,"ext_span_id":"99011-1","status":0,"output_text":"自动刷盘检索一","attrs":{"project_id":"deferred-save"}}]"#;
        assert_eq!(
            api.route_with_tenant("POST", "/v1/ingest", first, Some(990))
                .0,
            200
        );
        let rollup_before = std::fs::read(&rollup_path).unwrap();
        let attrs_before = std::fs::read(&attrs_path).unwrap();

        coord.set_flush_threshold(1);
        let second = r#"[{"trace_id":99012,"span_id":1,"ts":20,"seq":1,"event_type":2,"ext_span_id":"99012-1","status":0,"output_text":"自动刷盘检索二","attrs":{"project_id":"deferred-save"}}]"#;
        assert_eq!(
            api.route_with_tenant("POST", "/v1/ingest", second, Some(990))
                .0,
            200
        );
        assert_eq!(std::fs::read(&rollup_path).unwrap(), rollup_before);
        assert_eq!(std::fs::read(&attrs_path).unwrap(), attrs_before);

        coord.flush_memtable();
        assert!(rollup_path.exists(), "显式 flush 应写 rollup");
        assert!(attrs_path.exists(), "显式 flush 应写 attrs");
        assert_ne!(std::fs::read(rollup_path).unwrap(), rollup_before);
        assert_ne!(std::fs::read(attrs_path).unwrap(), attrs_before);
        assert!(dir.join("bm25.dat").exists());
    }
    {
        let reopened = WriteCoordinator::open_durable(&dir).unwrap();
        reopened.recover();
        let api = EngineJsonApi::new(reopened);
        let (status, body) = api.route_with_tenant(
            "POST",
            "/v1/search",
            r#"{"text":"自动刷盘","k":10,"filter":{"attrs":{"project_id":"deferred-save"}}}"#,
            Some(990),
        );
        assert_eq!(status, 200, "{body}");
        assert_contains(&body, r#""trace_id":99011"#);
    }
    let _ = std::fs::remove_dir_all(dir);
}
