//! eval 测试框架的集成测试：用 evalkit 自造多场景数据、真实摄入、跑 eval 闭环，断言不变量。
//! 验证「框架真把数据灌进去了、eval 真还原了注入的失败、回归机制真能检出退步」。

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use yt_engine::evalkit;
use yt_engine::{
    DatasetAssociationFilter, EngineJsonApi, GoldenPathFilter, InMemorySegmentStore, KeywordScorer,
    ReplicationStatus, ShardId, TraceAnnotationFilter, TraceQuery, WalReplicationBatch, WireRecord,
    WriteCoordinator,
};

fn fresh() -> Arc<WriteCoordinator> {
    WriteCoordinator::new(Arc::new(InMemorySegmentStore::default()))
}

fn durable_dir(name: &str) -> std::path::PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "yt_eval_{name}_{}_{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn json_str(s: &str) -> String {
    format!("{s:?}")
}

fn assert_json_contains(body: &str, needle: &str) {
    assert!(body.contains(needle), "missing {needle:?} in {body}");
}

#[test]
fn high_frequency_read_models_use_materialized_cache_and_invalidate_on_write() {
    let coord = fresh();
    let api = EngineJsonApi::new(coord);
    let body = r#"[
      {"trace_id":91001,"span_id":1,"session_id":9101,"ts":10,"seq":1,"event_type":2,"ext_span_id":"91001-1","status":0,"duration_ns":100,"agent_name":"cache-agent","tool_name":"tool-a","input_tokens":10,"output_tokens":5,"attrs":{"project_id":"read-model-cache","task_fingerprint":"cache-task","loop_id":"loop-cache","skill":"review","mode":"auto"}},
      {"trace_id":91002,"span_id":1,"session_id":9102,"ts":20,"seq":1,"event_type":2,"ext_span_id":"91002-1","status":1,"duration_ns":200,"agent_name":"cache-agent","tool_name":"tool-b","input_tokens":11,"output_tokens":6,"attrs":{"project_id":"read-model-cache","task_fingerprint":"cache-task","loop_id":"loop-cache","skill":"review","mode":"auto"}}
    ]"#;
    let (status, ingest) = api.route_with_tenant("POST", "/v1/ingest", body, Some(91));
    assert_eq!(status, 200, "{ingest}");

    let aggregate_query =
        r#"{"filter":{"projectId":"read-model-cache"},"groupBy":["taskFingerprint"],"limit":10}"#;
    let (status, aggregate_first) =
        api.route_with_tenant("POST", "/v1/trace-aggregate", aggregate_query, Some(91));
    assert_eq!(status, 200, "{aggregate_first}");
    assert_json_contains(&aggregate_first, r#""aggregationIndex":"tail_folded_scan""#);
    assert_json_contains(
        &aggregate_first,
        r#""aggregationPlanner":"tail_only_query_time_reduce""#,
    );
    assert_json_contains(&aggregate_first, r#""rollupEligible":true"#);
    assert_json_contains(&aggregate_first, r#""spanReadIndex":"tail_folded_scan""#);
    assert_json_contains(&aggregate_first, r#""usedSegmentRollup":false"#);
    assert_json_contains(&aggregate_first, r#""usedAttrPostings":false"#);
    assert_json_contains(&aggregate_first, r#""candidateSpanKeys":null"#);
    assert_json_contains(&aggregate_first, r#""readModelCache":"miss""#);
    assert_json_contains(&aggregate_first, r#""spanTotal":2"#);
    let (status, aggregate_second) =
        api.route_with_tenant("POST", "/v1/trace-aggregate", aggregate_query, Some(91));
    assert_eq!(status, 200, "{aggregate_second}");
    assert_json_contains(&aggregate_second, r#""readModelCache":"hit""#);

    let (status, loops_first) = api.route_with_tenant(
        "GET",
        "/v1/loops?projectId=read-model-cache&taskFingerprint=cache-task",
        "",
        Some(91),
    );
    assert_eq!(status, 200, "{loops_first}");
    assert_json_contains(&loops_first, r#""loopIndex":"loop_task_tail_folded_scan""#);
    assert_json_contains(&loops_first, r#""usedSegmentRollup":false"#);
    assert_json_contains(&loops_first, r#""spanReadIndex":"tail_folded_scan""#);
    assert_json_contains(&loops_first, r#""readModelCache":"miss""#);
    let (status, loops_second) = api.route_with_tenant(
        "GET",
        "/v1/loops?projectId=read-model-cache&taskFingerprint=cache-task",
        "",
        Some(91),
    );
    assert_eq!(status, 200, "{loops_second}");
    assert_json_contains(&loops_second, r#""readModelCache":"hit""#);

    let (status, task_first) = api.route_with_tenant(
        "GET",
        "/v1/tasks/cache-task/traces?projectId=read-model-cache",
        "",
        Some(91),
    );
    assert_eq!(status, 200, "{task_first}");
    assert_json_contains(&task_first, r#""taskIndex":"loop_task_tail_folded_scan""#);
    assert_json_contains(&task_first, r#""usedSegmentRollup":false"#);
    assert_json_contains(&task_first, r#""spanReadIndex":"tail_folded_scan""#);
    assert_json_contains(&task_first, r#""readModelCache":"miss""#);
    let (status, task_second) = api.route_with_tenant(
        "GET",
        "/v1/tasks/cache-task/traces?projectId=read-model-cache",
        "",
        Some(91),
    );
    assert_eq!(status, 200, "{task_second}");
    assert_json_contains(&task_second, r#""readModelCache":"hit""#);

    let trajectories_query = r#"{"filter":{"projectId":"read-model-cache"},"limit":10}"#;
    let (status, trajectories_first) = api.route_with_tenant(
        "POST",
        "/v1/trajectory-groups",
        trajectories_query,
        Some(91),
    );
    assert_eq!(status, 200, "{trajectories_first}");
    assert_json_contains(
        &trajectories_first,
        r#""trajectoryIndex":"materialized_trajectory_cache""#,
    );
    assert_json_contains(&trajectories_first, r#""readModelCache":"miss""#);
    let (status, trajectories_second) = api.route_with_tenant(
        "POST",
        "/v1/trajectory-groups",
        trajectories_query,
        Some(91),
    );
    assert_eq!(status, 200, "{trajectories_second}");
    assert_json_contains(&trajectories_second, r#""readModelCache":"hit""#);

    let late = r#"[{"trace_id":91003,"span_id":1,"session_id":9103,"ts":30,"seq":1,"event_type":2,"ext_span_id":"91003-1","status":0,"duration_ns":300,"attrs":{"project_id":"read-model-cache","task_fingerprint":"cache-task","loop_id":"loop-cache","skill":"review","mode":"auto"}}]"#;
    let (status, late_ingest) = api.route_with_tenant("POST", "/v1/ingest", late, Some(91));
    assert_eq!(status, 200, "{late_ingest}");
    let (status, aggregate_after_write) =
        api.route_with_tenant("POST", "/v1/trace-aggregate", aggregate_query, Some(91));
    assert_eq!(status, 200, "{aggregate_after_write}");
    assert_json_contains(&aggregate_after_write, r#""readModelCache":"miss""#);
    assert_json_contains(&aggregate_after_write, r#""spanTotal":3"#);
}

#[test]
fn trace_aggregate_uses_segment_rollup_after_flush() {
    let coord = fresh();
    let api = EngineJsonApi::new(Arc::clone(&coord));
    let body = r#"[
      {"trace_id":93001,"span_id":1,"session_id":9301,"ts":10,"seq":1,"event_type":2,"ext_span_id":"93001-1","status":0,"duration_ns":100,"tool_name":"planner","input_tokens":10,"output_tokens":1,"attrs":{"project_id":"rollup-hit","validation_status":"pass","skill":"review","mode":"auto"}},
      {"trace_id":93002,"span_id":1,"session_id":9302,"ts":20,"seq":1,"event_type":2,"ext_span_id":"93002-1","status":0,"duration_ns":200,"tool_name":"planner","input_tokens":20,"output_tokens":2,"attrs":{"project_id":"rollup-hit","validation_status":"pass","skill":"review","mode":"auto"}},
      {"trace_id":93003,"span_id":1,"session_id":9303,"ts":30,"seq":1,"event_type":2,"ext_span_id":"93003-1","status":1,"duration_ns":300,"tool_name":"executor","input_tokens":30,"output_tokens":3,"attrs":{"project_id":"rollup-hit","validation_status":"fail","skill":"review","mode":"auto"}},
      {"trace_id":93004,"span_id":1,"session_id":9304,"ts":40,"seq":1,"event_type":2,"ext_span_id":"93004-1","status":0,"duration_ns":400,"tool_name":"planner","input_tokens":40,"output_tokens":4,"attrs":{"project_id":"other","validation_status":"pass","skill":"review","mode":"auto"}}
    ]"#;
    let (status, ingest) = api.route_with_tenant("POST", "/v1/ingest", body, Some(93));
    assert_eq!(status, 200, "{ingest}");
    coord.flush_memtable();

    let query = r#"{"filter":{"projectId":"rollup-hit"},"groupBy":["validationStatus","toolName"],"sort":"count","limit":10}"#;
    let (status, aggregate) = api.route_with_tenant("POST", "/v1/trace-aggregate", query, Some(93));
    assert_eq!(status, 200, "{aggregate}");
    assert_json_contains(
        &aggregate,
        r#""aggregationIndex":"segment_rollup_tail_overlay""#,
    );
    assert_json_contains(
        &aggregate,
        r#""aggregationPlanner":"segment_rollup_tail_overlay""#,
    );
    assert_json_contains(&aggregate, r#""usedSegmentRollup":true"#);
    assert_json_contains(&aggregate, r#""spanReadIndex":"segment_rollup""#);
    assert_json_contains(&aggregate, r#""segmentRollupSegments":1"#);
    assert_json_contains(&aggregate, r#""segmentRollupRows":4"#);
    assert_json_contains(&aggregate, r#""tailFoldedSpanCount":0"#);
    assert_json_contains(&aggregate, r#""scannedSegments":0"#);
    assert_json_contains(&aggregate, r#""spanTotal":3"#);
    assert_json_contains(
        &aggregate,
        r#""key":{"validation_status":"pass","toolName":"planner"},"spanCount":2"#,
    );
    assert_json_contains(
        &aggregate,
        r#""key":{"validation_status":"fail","toolName":"executor"},"spanCount":1"#,
    );
}

#[test]
fn trace_aggregate_rollup_falls_back_for_cross_segment_span() {
    let coord = fresh();
    let api = EngineJsonApi::new(Arc::clone(&coord));
    let first = r#"[
      {"trace_id":94001,"span_id":1,"session_id":9401,"ts":10,"seq":1,"event_type":0,"ext_span_id":"94001-1","status":0,"duration_ns":100,"tool_name":"planner","attrs":{"project_id":"rollup-fallback","validation_status":"pass"}}
    ]"#;
    let (status, ingest) = api.route_with_tenant("POST", "/v1/ingest", first, Some(94));
    assert_eq!(status, 200, "{ingest}");
    coord.flush_memtable();
    let second = r#"[
      {"trace_id":94001,"span_id":1,"session_id":9401,"ts":20,"seq":2,"event_type":2,"ext_span_id":"94001-1","status":1,"duration_ns":250,"tool_name":"planner","attrs":{"project_id":"rollup-fallback","validation_status":"fail"}}
    ]"#;
    let (status, ingest) = api.route_with_tenant("POST", "/v1/ingest", second, Some(94));
    assert_eq!(status, 200, "{ingest}");
    coord.flush_memtable();

    let query = r#"{"filter":{"projectId":"rollup-fallback"},"groupBy":["validationStatus","status"],"limit":10}"#;
    let (status, aggregate) = api.route_with_tenant("POST", "/v1/trace-aggregate", query, Some(94));
    assert_eq!(status, 200, "{aggregate}");
    assert_json_contains(
        &aggregate,
        r#""aggregationIndex":"folded_query_time_reduce""#,
    );
    assert_json_contains(
        &aggregate,
        r#""aggregationPlanner":"rollup_safety_fallback_folded_scan""#,
    );
    assert_json_contains(
        &aggregate,
        r#""rollupFallbackReason":"span_crosses_multiple_segments""#,
    );
    assert_json_contains(&aggregate, r#""usedSegmentRollup":false"#);
    assert_json_contains(&aggregate, r#""scannedSegments":2"#);
    assert_json_contains(&aggregate, r#""spanTotal":1"#);
    assert_json_contains(
        &aggregate,
        r#""key":{"validation_status":"fail","status":1},"spanCount":1"#,
    );
}

#[test]
fn durable_trace_aggregate_rollup_survives_reopen() {
    let dir = durable_dir("trace_aggregate_rollup");
    {
        let coord = WriteCoordinator::open_durable(&dir).unwrap();
        let api = EngineJsonApi::new(Arc::clone(&coord));
        let body = r#"[
          {"trace_id":95001,"span_id":1,"session_id":9501,"ts":10,"seq":1,"event_type":2,"ext_span_id":"95001-1","status":0,"duration_ns":100,"tool_name":"planner","attrs":{"project_id":"rollup-durable","validation_status":"pass"}},
          {"trace_id":95002,"span_id":1,"session_id":9502,"ts":20,"seq":1,"event_type":2,"ext_span_id":"95002-1","status":1,"duration_ns":200,"tool_name":"executor","attrs":{"project_id":"rollup-durable","validation_status":"fail"}}
        ]"#;
        let (status, ingest) = api.route_with_tenant("POST", "/v1/ingest", body, Some(95));
        assert_eq!(status, 200, "{ingest}");
        coord.flush_memtable();
    }
    {
        let coord = WriteCoordinator::open_durable(&dir).unwrap();
        coord.recover();
        let api = EngineJsonApi::new(coord);
        let query = r#"{"filter":{"projectId":"rollup-durable"},"groupBy":["validationStatus"],"limit":10}"#;
        let (status, aggregate) =
            api.route_with_tenant("POST", "/v1/trace-aggregate", query, Some(95));
        assert_eq!(status, 200, "{aggregate}");
        assert_json_contains(
            &aggregate,
            r#""aggregationIndex":"segment_rollup_tail_overlay""#,
        );
        assert_json_contains(&aggregate, r#""usedSegmentRollup":true"#);
        assert_json_contains(&aggregate, r#""segmentRollupSegments":1"#);
        assert_json_contains(&aggregate, r#""segmentRollupRows":2"#);
        assert_json_contains(&aggregate, r#""spanTotal":2"#);
        assert_json_contains(
            &aggregate,
            r#""key":{"validation_status":"pass"},"spanCount":1"#,
        );
        assert_json_contains(
            &aggregate,
            r#""key":{"validation_status":"fail"},"spanCount":1"#,
        );
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn loop_task_read_models_use_sidecar_after_flush_and_fallback_on_text() {
    let coord = fresh();
    let api = EngineJsonApi::new(Arc::clone(&coord));
    let body = r#"[
      {"trace_id":96001,"span_id":1,"session_id":9601,"ts":10,"seq":1,"event_type":2,"ext_span_id":"96001-1","status":0,"duration_ns":100,"tool_name":"planner","input_text":"计划步骤","attrs":{"project_id":"loop-task-sidecar","task_fingerprint":"sidecar-task","loop_id":"loop-a","validation_status":"pass","phase":"plan"}},
      {"trace_id":96002,"span_id":1,"session_id":9602,"ts":20,"seq":1,"event_type":2,"ext_span_id":"96002-1","status":0,"duration_ns":200,"tool_name":"executor","input_text":"执行步骤","attrs":{"project_id":"loop-task-sidecar","task_fingerprint":"sidecar-task","loop_id":"loop-a","validation_status":"pass","phase":"act"}},
      {"trace_id":96003,"span_id":1,"session_id":9603,"ts":30,"seq":1,"event_type":2,"ext_span_id":"96003-1","status":1,"duration_ns":300,"tool_name":"validator","input_text":"校验失败","attrs":{"project_id":"loop-task-sidecar","task_fingerprint":"sidecar-task","loop_id":"loop-a","validation_status":"fail","phase":"verify"}}
    ]"#;
    let (status, ingest) = api.route_with_tenant("POST", "/v1/ingest", body, Some(96));
    assert_eq!(status, 200, "{ingest}");
    coord.flush_memtable();

    let (status, loops) = api.route_with_tenant(
        "GET",
        "/v1/loops?projectId=loop-task-sidecar&taskFingerprint=sidecar-task",
        "",
        Some(96),
    );
    assert_eq!(status, 200, "{loops}");
    assert_json_contains(&loops, r#""loopIndex":"loop_task_sidecar+tail_overlay""#);
    assert_json_contains(&loops, r#""spanReadIndex":"loop_task_sidecar""#);
    assert_json_contains(&loops, r#""usedSegmentRollup":true"#);
    assert_json_contains(&loops, r#""segmentRollupSegments":1"#);
    assert_json_contains(&loops, r#""segmentRollupRows":3"#);
    assert_json_contains(&loops, r#""loopId":"loop-a""#);
    assert_json_contains(&loops, r#""spanCount":3"#);
    assert_json_contains(&loops, r#""sessionCount":3"#);
    assert_json_contains(&loops, r#""errorCount":1"#);

    let (status, task) = api.route_with_tenant(
        "GET",
        "/v1/tasks/sidecar-task/traces?projectId=loop-task-sidecar",
        "",
        Some(96),
    );
    assert_eq!(status, 200, "{task}");
    assert_json_contains(&task, r#""taskIndex":"loop_task_sidecar+tail_overlay""#);
    assert_json_contains(&task, r#""spanReadIndex":"loop_task_sidecar""#);
    assert_json_contains(&task, r#""usedSegmentRollup":true"#);
    assert_json_contains(&task, r#""total":3"#);
    assert_json_contains(&task, r#""traceId":"96003""#);

    let (status, filtered) = api.route_with_tenant(
        "GET",
        "/v1/loops?projectId=loop-task-sidecar&taskFingerprint=sidecar-task&filter=校验",
        "",
        Some(96),
    );
    assert_eq!(status, 200, "{filtered}");
    assert_json_contains(&filtered, r#""loopIndex":"loop_folded_scan""#);
    assert_json_contains(&filtered, r#""spanReadIndex":"folded_scan""#);
    assert_json_contains(&filtered, r#""rollupFallbackReason":"text_filter""#);
    assert_json_contains(&filtered, r#""spanCount":1"#);
}

#[test]
fn trace_aggregate_read_plan_marks_unindexed_attrs_as_folded_scan() {
    let coord = fresh();
    let api = EngineJsonApi::new(coord);
    let body = r#"[
      {"trace_id":92001,"span_id":1,"session_id":9201,"ts":10,"seq":1,"event_type":2,"ext_span_id":"92001-1","status":0,"duration_ns":100,"attrs":{"project_id":"read-plan","custom_dimension":"tail-a"}},
      {"trace_id":92002,"span_id":1,"session_id":9202,"ts":20,"seq":1,"event_type":2,"ext_span_id":"92002-1","status":0,"duration_ns":100,"attrs":{"project_id":"read-plan","custom_dimension":"tail-b"}}
    ]"#;
    let (status, ingest) = api.route_with_tenant("POST", "/v1/ingest", body, Some(92));
    assert_eq!(status, 200, "{ingest}");

    let query =
        r#"{"filter":{"attrs":{"custom_dimension":"tail-a"}},"groupBy":["custom_dimension"]}"#;
    let (status, aggregate) = api.route_with_tenant("POST", "/v1/trace-aggregate", query, Some(92));
    assert_eq!(status, 200, "{aggregate}");
    assert_json_contains(&aggregate, r#""index":"attrs_folded_scan""#);
    assert_json_contains(&aggregate, r#""spanReadIndex":"folded_scan""#);
    assert_json_contains(&aggregate, r#""usedAttrPostings":false"#);
    assert_json_contains(&aggregate, r#""candidateSpanKeys":null"#);
    assert_json_contains(&aggregate, r#""unsupportedAttrKeys":["custom_dimension"]"#);
    assert_json_contains(&aggregate, r#""rollupEligible":false"#);
    assert_json_contains(
        &aggregate,
        r#""rollupBlockedBy":["unsupported_group_by:custom_dimension"]"#,
    );
    assert_json_contains(&aggregate, r#""spanTotal":1"#);
    assert_json_contains(&aggregate, r#""custom_dimension":"tail-a""#);
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

fn extract_json_string_field(body: &str, field: &str) -> String {
    let needle = format!("\"{field}\":\"");
    let value_start = body
        .find(&needle)
        .unwrap_or_else(|| panic!("missing JSON string field {field} in {body}"))
        + needle.len();
    let mut out = String::new();
    let mut escaped = false;
    for ch in body[value_start..].chars() {
        if escaped {
            out.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            return out;
        } else {
            out.push(ch);
        }
    }
    panic!("unterminated string field {field} in {body}");
}

fn url_encode_component(value: &str) -> String {
    let mut out = String::new();
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn metric_value(metrics: &str, name: &str) -> u64 {
    metrics
        .lines()
        .filter(|line| !line.starts_with('#'))
        .find_map(|line| {
            let mut parts = line.split_whitespace();
            let metric = parts.next()?;
            let value = parts.next()?;
            (metric == name)
                .then(|| value.parse::<u64>().ok())
                .flatten()
        })
        .unwrap_or_else(|| panic!("missing metric {name} in:\n{metrics}"))
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

fn session_for_shard(tenant: u64, shard: usize, shard_count: usize) -> u64 {
    (10_000u64..20_000)
        .find(|session| {
            (test_route_hash(Some(tenant), Some(*session), 0) as usize) % shard_count == shard
        })
        .unwrap_or_else(|| panic!("could not find session for shard {shard}"))
}

fn eval_trace(trace: u64, project: &str, output: &str, status: u8) -> Vec<WireRecord> {
    let span = 3;
    let ext = format!("{trace}-{span}");
    let mut attrs = BTreeMap::new();
    attrs.insert("project_id".to_string(), json_str(project));
    attrs.insert("skill".to_string(), json_str("eval-sidecar"));
    vec![
        WireRecord {
            trace_id: trace,
            span_id: span,
            ts: trace as i64,
            seq: 1,
            event_type_tag: 1,
            ext_span_id: ext.clone(),
            parent_span_id: None,
            status: None,
            duration_ns: None,
            input_tokens: Some(100),
            output_tokens: None,
            cached_input_tokens: None,
            reasoning_tokens: None,
            total_tokens: None,
            cost_usd_nanos: None,
            cost_currency: None,
            provider: None,
            session_id: Some(trace / 10),
            tenant_id: None,
            external_trace_id: None,
            external_span_id: None,
            external_parent_span_id: None,
            external_session_id: None,
            agent_name: Some("eval-agent".to_string()),
            tool_name: None,
            model: Some("qwen-max".to_string()),
            input_text: Some("评估 attrs sidecar 过滤后的 eval 结果".to_string()),
            output_text: None,
            logs: Vec::new(),
            attrs,
        },
        WireRecord {
            trace_id: trace,
            span_id: span,
            ts: trace as i64 + 1,
            seq: 2,
            event_type_tag: 2,
            ext_span_id: ext,
            parent_span_id: None,
            status: Some(status),
            duration_ns: Some(1_000_000),
            input_tokens: None,
            output_tokens: Some(80),
            cached_input_tokens: None,
            reasoning_tokens: None,
            total_tokens: None,
            cost_usd_nanos: None,
            cost_currency: None,
            provider: None,
            session_id: Some(trace / 10),
            tenant_id: None,
            external_trace_id: None,
            external_span_id: None,
            external_parent_span_id: None,
            external_session_id: None,
            agent_name: Some("eval-agent".to_string()),
            tool_name: None,
            model: Some("qwen-max".to_string()),
            input_text: None,
            output_text: Some(output.to_string()),
            logs: Vec::new(),
            attrs: BTreeMap::new(),
        },
    ]
}

/// eval 应当**精确还原**每个场景注入的失败数：
/// 每条 trace 恰有一个被打分的 answer span，通过数 = trace 数 − 注入失败数。
#[test]
fn eval_recovers_injected_failures_per_scenario() {
    let coord = fresh();
    let report = evalkit::run_harness(&coord, 30, 7);
    assert_eq!(report.scenarios.len(), 4);
    for s in &report.scenarios {
        let overall = &s.summary[0];
        assert_eq!(
            overall.scored_spans, s.traces,
            "场景[{}]：每条 trace 应恰好一个被评 span",
            s.key
        );
        assert_eq!(
            overall.pass_count,
            s.traces - s.injected_failures,
            "场景[{}]：通过数应等于 trace 数减去注入失败数",
            s.key
        );
        // 既有通过也有失败，eval 才有意义（注入比例都在 (0,1) 内）。
        assert!(s.injected_failures > 0, "场景[{}]：应注入了一些失败", s.key);
        assert!(
            s.injected_failures < s.traces,
            "场景[{}]：不应全失败",
            s.key
        );
    }
}

/// 多 agent 场景（风控研判）应能看出 per-agent 通过率差异：
/// 「风控研判」(低失败权重) 通过率应高于「反洗钱核查」(高失败权重)。
#[test]
fn per_agent_pass_rate_differs_in_multi_agent_scenario() {
    let coord = fresh();
    let report = evalkit::run_harness(&coord, 80, 42);
    let risk = report
        .scenarios
        .iter()
        .find(|s| s.key == "风控研判")
        .expect("有风控场景");

    let rate_of = |agent: &str| -> f32 {
        risk.summary
            .iter()
            .find(|r| r.agent_name.as_deref() == Some(agent))
            .map(|r| r.pass_rate())
            .unwrap_or(0.0)
    };
    let good_agent = rate_of("风控研判");
    let bad_agent = rate_of("反洗钱核查");
    assert!(
        good_agent > bad_agent,
        "表现好的 agent 通过率应更高：风控研判={good_agent:.2} 反洗钱核查={bad_agent:.2}"
    );
}

/// 回归机制：同一冻结数据集，评判标准收紧后通过率应下降（检出退步）。
#[test]
fn dataset_regression_drops_under_stricter_scorer() {
    let coord = fresh();
    let report = evalkit::run_harness(&coord, 80, 11);
    assert!(report.dataset_size > 0, "应采集到回归样本");
    let base = report.dataset_baseline[0].pass_rate();
    let strict = report.dataset_stricter[0].pass_rate();
    assert!(
        strict < base,
        "更严评判通过率应低于基准：基准={base:.2} 更严={strict:.2}"
    );
}

/// Trace Diff 是 golden path / trajectory comparison 的证据层：
/// eval 写回以后，diff 应能同时暴露路径差异、失败验证和 eval 分数差异。
#[test]
fn trace_diff_eval_fixture_exposes_golden_path_evidence() {
    let coord = fresh();
    let api = EngineJsonApi::new(coord.clone());
    let batch = r#"[
      {
        "trace_id":901,
        "span_id":1,
        "ts":100,
        "seq":1,
        "event_type":2,
        "ext_span_id":"901-1",
        "status":0,
        "duration_ns":10,
        "tool_name":"repo-inspect",
        "input_tokens":10,
        "output_tokens":5,
        "total_tokens":15,
        "cost_usd_nanos":1000,
        "output_text":"确认是 macOS arm64 native binding 问题",
        "attrs":{"project_id":"agentic-data","skill":"packaging","mode":"auto","task_fingerprint":"native-binding-pack","loop_id":"loop-pass","validation_status":"pass","phase":"inspect","validator":"npm test"}
      },
      {
        "trace_id":901,
        "span_id":2,
        "ts":120,
        "seq":1,
        "event_type":2,
        "ext_span_id":"901-2",
        "status":0,
        "duration_ns":20,
        "tool_name":"npm-test",
        "input_tokens":10,
        "output_tokens":10,
        "total_tokens":20,
        "cost_usd_nanos":2000,
        "output_text":"npm test passed",
        "attrs":{"project_id":"agentic-data","skill":"packaging","mode":"auto","task_fingerprint":"native-binding-pack","loop_id":"loop-pass","validation_status":"pass","phase":"verify","validator":"npm test"}
      },
      {
        "trace_id":902,
        "span_id":1,
        "ts":100,
        "seq":1,
        "event_type":2,
        "ext_span_id":"902-1",
        "status":0,
        "duration_ns":15,
        "tool_name":"repo-inspect",
        "input_tokens":15,
        "output_tokens":10,
        "total_tokens":25,
        "cost_usd_nanos":1500,
        "output_text":"误判为 Rust host target 问题",
        "attrs":{"project_id":"agentic-data","skill":"packaging","mode":"auto","task_fingerprint":"native-binding-pack","loop_id":"loop-fail","validation_status":"fail","phase":"inspect","validator":"npm test"}
      },
      {
        "trace_id":902,
        "span_id":2,
        "ts":130,
        "seq":1,
        "event_type":2,
        "ext_span_id":"902-2",
        "status":0,
        "duration_ns":30,
        "tool_name":"cargo-build",
        "input_tokens":10,
        "output_tokens":20,
        "total_tokens":30,
        "cost_usd_nanos":3000,
        "output_text":"重新构建 native module",
        "attrs":{"project_id":"agentic-data","skill":"packaging","mode":"auto","task_fingerprint":"native-binding-pack","loop_id":"loop-fail","validation_status":"fail","phase":"build","validator":"npm test"}
      },
      {
        "trace_id":902,
        "span_id":3,
        "ts":160,
        "seq":1,
        "event_type":2,
        "ext_span_id":"902-3",
        "status":1,
        "duration_ns":40,
        "tool_name":"npm-test",
        "input_tokens":20,
        "output_tokens":30,
        "total_tokens":50,
        "cost_usd_nanos":5000,
        "output_text":"cannot find darwin-arm64 node binding",
        "attrs":{"project_id":"agentic-data","skill":"packaging","mode":"auto","task_fingerprint":"native-binding-pack","loop_id":"loop-fail","validation_status":"fail","phase":"verify","validator":"npm test"}
      }
    ]"#;
    let (status, body) = api.route_with_tenant("POST", "/v1/ingest", batch, Some(77));
    assert_eq!(status, 200, "{body}");

    let scorer = KeywordScorer::new(&["cannot find", "误判"]);
    let scored = coord.eval_and_writeback(&scorer, &TraceQuery::all());
    assert!(
        scored
            .iter()
            .any(|s| s.trace_id == 901 && s.outcome.score == 1000),
        "通过路径应得到满分 eval"
    );
    assert!(
        scored
            .iter()
            .any(|s| s.trace_id == 902 && s.outcome.score == 0),
        "失败路径应得到零分 eval"
    );

    let (status, diff) = api.route_with_tenant(
        "POST",
        "/v1/traces/diff",
        r#"{"leftTraceId":901,"rightTraceId":902}"#,
        Some(77),
    );
    assert_eq!(status, 200, "{diff}");
    assert!(diff.contains(r#""delta":{"spanCount":1"#), "{diff}");
    assert!(diff.contains(r#""errorCount":1"#), "{diff}");
    assert!(diff.contains(r#""durationNs":55"#), "{diff}");
    assert!(diff.contains(r#""totalTokens":70"#), "{diff}");
    assert!(diff.contains(r#""costUsdNanos":6500"#), "{diff}");
    assert!(
        diff.contains(r#""trajectory":{"left":{"signature":"fnv1a64:"#),
        "{diff}"
    );
    assert!(diff.contains(r#""same":false"#), "{diff}");
    assert!(
        diff.contains(r#""tool:npm-test|phase:verify|validator:npm_test""#),
        "{diff}"
    );
    assert!(diff.contains(r#""status":"right_only""#), "{diff}");
    assert!(diff.contains(r#""toolName":"cargo-build""#), "{diff}");
    assert!(diff.contains(r#""evalScore":1000"#), "{diff}");
    assert!(diff.contains(r#""evalScore":0"#), "{diff}");
    assert!(diff.contains(r#""evalLabel":"未通过""#), "{diff}");
    assert!(diff.contains(r#""evalScore""#), "{diff}");

    let (status, groups) = api.route_with_tenant(
        "POST",
        "/v1/trajectory-groups",
        r#"{"filter":{"taskFingerprint":"native-binding-pack"},"sort":"best"}"#,
        Some(77),
    );
    assert_eq!(status, 200, "{groups}");
    assert!(groups.contains(r#""total":2"#), "{groups}");
    assert!(groups.contains(r#""traceTotal":2"#), "{groups}");
    assert!(groups.contains(r#""spanTotal":5"#), "{groups}");
    assert!(
        groups.contains(r#""steps":["tool:repo-inspect|phase:inspect|validator:npm_test","tool:npm-test|phase:verify|validator:npm_test"]"#),
        "{groups}"
    );
    assert!(groups.contains(r#""qualityScore":1000"#), "{groups}");
    assert!(
        groups.contains(r#""eval":{"count":2,"avg":1000"#),
        "{groups}"
    );
    assert!(groups.contains(r#""errorTraceCount":1"#), "{groups}");

    let (status, golden) = api.route_with_tenant(
        "POST",
        "/v1/golden-paths",
        r#"{"sourceTraceId":901,"taskFingerprint":"native-binding-pack","score":1000,"label":"eval winner","reason":"eval score 1000","source":"eval_harness","projectId":"agentic-data"}"#,
        Some(77),
    );
    assert_eq!(status, 200, "{golden}");
    assert!(golden.contains(r#""status":"candidate""#), "{golden}");
    assert!(
        golden.contains(r#""trajectorySignature":"fnv1a64:"#),
        "{golden}"
    );

    let (status, confirmed) = api.route_with_tenant(
        "POST",
        "/v1/golden-paths/1/status",
        r#"{"status":"confirmed","reason":"eval accepted","source":"eval_harness"}"#,
        Some(77),
    );
    assert_eq!(status, 200, "{confirmed}");
    assert!(confirmed.contains(r#""status":"confirmed""#), "{confirmed}");

    let (status, paths) = api.route_with_tenant(
        "GET",
        "/v1/golden-paths?taskFingerprint=native-binding-pack&status=confirmed&projectId=agentic-data",
        "",
        Some(77),
    );
    assert_eq!(status, 200, "{paths}");
    assert!(paths.contains(r#""count":1"#), "{paths}");

    let (hidden_status, hidden) = api.route_with_tenant(
        "POST",
        "/v1/traces/diff",
        r#"{"leftTraceId":901,"rightTraceId":902}"#,
        Some(78),
    );
    assert_eq!(hidden_status, 404, "{hidden}");
}

/// 数据是**真灌进引擎**的：摄入后能从 trace 列表读出来。
#[test]
fn ingested_data_is_visible_in_trace_list() {
    let coord = fresh();
    let report = evalkit::run_harness(&coord, 20, 3);
    let total_traces: usize = report.scenarios.iter().map(|s| s.traces).sum();

    let snap = coord.pin_snapshot();
    let traces = coord.list_traces(&snap, &TraceQuery::all());
    assert_eq!(
        traces.len(),
        total_traces,
        "trace 列表条数应等于灌入的 trace 总数"
    );
    // 每条 trace 三个 span（root/tool/answer），且有 token 成本。
    assert!(
        traces.iter().all(|t| t.span_count == 3),
        "每条 trace 应有 3 个 span"
    );
    assert!(
        traces.iter().any(|t| t.total_input_tokens > 0),
        "应有输入 token 成本"
    );
}

/// attrs sidecar 不能只是查询快；它还要能支撑 eval 闭环后的数据读取。
/// 这里走 durable data dir：ingest → eval 写回 → recover → attrs filter → 读回 eval_score。
#[test]
fn durable_attrs_sidecar_filters_eval_results_after_recover() {
    let dir = durable_dir("attrs_sidecar");
    {
        let coord = WriteCoordinator::open_durable(&dir).unwrap();
        let mut records = Vec::new();
        records.extend(eval_trace(
            10,
            "agentic-data",
            "研判结论明确，可以放行。",
            0,
        ));
        records.extend(eval_trace(11, "agentic-data", "抱歉，无法完成研判。", 1));
        records.extend(eval_trace(20, "other-project", "其他项目结果正常。", 0));
        coord.ingest_wire(records);

        let scorer = KeywordScorer::new(evalkit::BAD_WORDS);
        let scored = coord.eval_and_writeback(&scorer, &TraceQuery::all());
        assert_eq!(scored.len(), 3, "三条 answer span 都应被打分");
        assert!(
            dir.join("attr_postings").join("seg-1.attrs").exists(),
            "eval flush 后应生成 attrs segment sidecar"
        );
    }

    {
        let coord = WriteCoordinator::open_durable(&dir).unwrap();
        coord.recover();
        let snap = coord.pin_snapshot();
        let attrs = BTreeMap::from([("project_id".to_string(), json_str("agentic-data"))]);
        let mut spans = coord.read_spans_query_for_attrs(&snap, &TraceQuery::all(), &attrs);
        spans.sort_by_key(|s| s.trace_id);

        assert_eq!(spans.len(), 2, "attrs filter 只应返回目标项目的 span");
        assert_eq!(
            spans.iter().map(|s| s.trace_id).collect::<Vec<_>>(),
            vec![10, 11]
        );
        assert_eq!(spans[0].eval_score, Some(1000), "通过样本分数应保留");
        assert_eq!(spans[1].eval_score, Some(0), "失败样本分数应保留");

        let summary = coord.eval_summary(&snap, &TraceQuery::all(), 1000);
        assert_eq!(summary[0].scored_spans, 3);
        assert_eq!(summary[0].pass_count, 2);
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// 索引/性能契约：attrs segment sidecar 必须是按需加载的派生索引。
/// 这个测试不做脆弱的耗时断言，而是用指标约束查询路径：recover 不预热 posting list，
/// 首次索引查询产生 sidecar load/miss，重复查询命中 cache；高基数字段仍能慢路径精确返回。
#[test]
fn attrs_sidecar_index_cache_is_lazy_and_trace_summaries_stay_complete() {
    let dir = durable_dir("attrs_index_perf");
    {
        let coord = WriteCoordinator::open_durable(&dir).unwrap();
        coord.recover();
        let api = EngineJsonApi::new(coord.clone());
        let batch1 = r#"[
          {
            "trace_id":701,
            "span_id":1,
            "session_id":7701,
            "ts":10,
            "seq":1,
            "event_type":2,
            "ext_span_id":"701-1",
            "status":0,
            "duration_ns":10,
            "input_tokens":10,
            "output_tokens":5,
            "cost_usd_nanos":1000,
            "attrs":{"project_id":"index-perf","connection_ids":["conn-a","conn-b"],"task_fingerprint":"index-task","path_memory_id":"pm-701"}
          },
          {
            "trace_id":701,
            "span_id":2,
            "session_id":7701,
            "ts":20,
            "seq":1,
            "event_type":2,
            "ext_span_id":"701-2",
            "status":0,
            "duration_ns":20,
            "input_tokens":20,
            "output_tokens":10,
            "cost_usd_nanos":2000,
            "attrs":{"project_id":"index-perf","task_fingerprint":"index-task"}
          },
          {
            "trace_id":702,
            "span_id":1,
            "session_id":7702,
            "ts":30,
            "seq":1,
            "event_type":2,
            "ext_span_id":"702-1",
            "status":0,
            "duration_ns":10,
            "attrs":{"project_id":"other-project","connection_ids":["conn-z"],"task_fingerprint":"index-task"}
          }
        ]"#;
        let (status, body) = api.route_with_tenant("POST", "/v1/ingest", batch1, Some(16));
        assert_eq!(status, 200, "{body}");
        coord.flush_memtable();

        let batch2 = r#"[
          {
            "trace_id":703,
            "span_id":1,
            "session_id":7703,
            "ts":40,
            "seq":1,
            "event_type":2,
            "ext_span_id":"703-1",
            "status":0,
            "duration_ns":10,
            "input_tokens":30,
            "output_tokens":15,
            "cost_usd_nanos":3000,
            "attrs":{"project_id":"index-perf","connection_ids":["conn-a"],"task_fingerprint":"index-task","path_memory_id":"pm-703"}
          }
        ]"#;
        let (status, body) = api.route_with_tenant("POST", "/v1/ingest", batch2, Some(16));
        assert_eq!(status, 200, "{body}");
        coord.flush_memtable();

        let sidecar_dir = dir.join("attr_postings");
        let sidecar_files = std::fs::read_dir(&sidecar_dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().and_then(|s| s.to_str()) == Some("attrs"))
            .count();
        assert!(
            sidecar_files >= 2,
            "two flushes should create segment-local attrs sidecars"
        );
    }

    {
        let coord = WriteCoordinator::open_durable(&dir).unwrap();
        coord.recover();
        let api = EngineJsonApi::new(coord.clone());
        let before = coord.metrics();
        assert!(
            metric_value(&before, "yt_attr_sidecar_segments") >= 2,
            "{before}"
        );
        assert_eq!(
            metric_value(&before, "yt_attr_sidecar_cache_entries"),
            0,
            "recover should rebuild only the sidecar directory, not prewarm posting lists"
        );
        assert_eq!(
            metric_value(&before, "yt_attr_sidecar_cache_loads"),
            0,
            "no sidecar posting list should be loaded before the first query"
        );

        let indexed_query =
            r#"{"filter":{"projectId":"index-perf","connectionIds":"conn-a"},"limit":10}"#;
        let (status, indexed) =
            api.route_with_tenant("POST", "/v1/trace-search", indexed_query, Some(16));
        assert_eq!(status, 200, "{indexed}");
        assert_json_contains(&indexed, r#""total":2"#);
        assert_json_contains(&indexed, r#""traceId":"701""#);
        assert_json_contains(&indexed, r#""traceId":"703""#);
        assert_json_contains(&indexed, r#""index":"attrs_postings+folded_verify""#);

        let after_first = coord.metrics();
        assert!(
            metric_value(&after_first, "yt_attr_sidecar_cache_loads") > 0,
            "{after_first}"
        );
        assert!(
            metric_value(&after_first, "yt_attr_sidecar_cache_misses") > 0,
            "{after_first}"
        );
        assert!(
            metric_value(&after_first, "yt_attr_sidecar_cache_entries") > 0,
            "{after_first}"
        );

        let first_hits = metric_value(&after_first, "yt_attr_sidecar_cache_hits");
        let (status, indexed_again) =
            api.route_with_tenant("POST", "/v1/trace-search", indexed_query, Some(16));
        assert_eq!(status, 200, "{indexed_again}");
        assert_json_contains(&indexed_again, r#""total":2"#);
        let after_second = coord.metrics();
        assert!(
            metric_value(&after_second, "yt_attr_sidecar_cache_hits") > first_hits,
            "repeat indexed query should hit the sidecar cache\nbefore:\n{after_first}\nafter:\n{after_second}"
        );

        let (status, trace_list) =
            api.route_with_tenant("GET", "/v1/traces?connectionIds=conn-a", "", Some(16));
        assert_eq!(status, 200, "{trace_list}");
        assert_json_contains(&trace_list, r#""trace_id":701"#);
        assert_json_contains(&trace_list, r#""span_count":2"#);
        assert_json_contains(&trace_list, r#""total_cost_usd_nanos":3000"#);

        let (status, sessions) =
            api.route_with_tenant("GET", "/v1/sessions?connectionIds=conn-a", "", Some(16));
        assert_eq!(status, 200, "{sessions}");
        assert_json_contains(&sessions, r#""sessionId":"7701""#);
        assert_json_contains(&sessions, r#""sessionId":"7703""#);
        assert_json_contains(&sessions, r#""total":2"#);

        let loads_before_unindexed = metric_value(&coord.metrics(), "yt_attr_sidecar_cache_loads");
        let (status, slow_path) = api.route_with_tenant(
            "POST",
            "/v1/trace-search",
            r#"{"filter":{"pathMemoryId":"pm-703"}}"#,
            Some(16),
        );
        assert_eq!(status, 200, "{slow_path}");
        assert_json_contains(&slow_path, r#""total":1"#);
        assert_json_contains(&slow_path, r#""traceId":"703""#);
        let loads_after_unindexed = metric_value(&coord.metrics(), "yt_attr_sidecar_cache_loads");
        assert_eq!(
            loads_after_unindexed, loads_before_unindexed,
            "path_memory_id is intentionally not a sidecar-postings key; query should not load new sidecar lists"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// 分布式升级第一步：单机也要显式暴露 shard facade。
/// 这个 eval 同时验证 facade 不会改变 tenant 写入、trace-search 语义和 attrs postings 性能路径。
#[test]
fn single_shard_facade_reports_cluster_shape_and_keeps_indexed_search_path() {
    let dir = durable_dir("single_shard_facade");
    {
        let coord = WriteCoordinator::open_durable(&dir).unwrap();
        coord.recover();
        let api = EngineJsonApi::new_single_shard(coord.clone(), ShardId::new("tenant-77-shard-0"));
        let batch = r#"[
          {
            "trace_id":80101,
            "span_id":1,
            "session_id":8801,
            "ts":10,
            "seq":1,
            "event_type":2,
            "ext_span_id":"80101-1",
            "status":0,
            "duration_ns":10,
            "attrs":{"project_id":"cluster-facade","skill":"routing","task_fingerprint":"cluster-v1"}
          },
          {
            "trace_id":80102,
            "span_id":1,
            "session_id":8802,
            "ts":20,
            "seq":1,
            "event_type":2,
            "ext_span_id":"80102-1",
            "status":1,
            "duration_ns":20,
            "attrs":{"project_id":"cluster-facade","skill":"routing","task_fingerprint":"cluster-v1"}
          }
        ]"#;
        let (status, body) = api.route_with_tenant("POST", "/v1/ingest", batch, Some(77));
        assert_eq!(status, 200, "{body}");
        coord.flush_memtable();

        let (status, cluster) = api.route("GET", "/v1/cluster/shards", "");
        assert_eq!(status, 200, "{cluster}");
        assert_json_contains(&cluster, r#""mode":"single_node""#);
        assert_json_contains(&cluster, r#""writeModel":"single_writer_per_shard""#);
        assert_json_contains(&cluster, r#""routing":"single_shard""#);
        assert_json_contains(&cluster, r#""shardId":"tenant-77-shard-0""#);
        assert_json_contains(&cluster, r#""manifestVersion":1"#);
        assert_json_contains(&cluster, r#""committedTail":2"#);
        assert_json_contains(&cluster, r#""memtableWatermark":2"#);
        assert_json_contains(&cluster, r#""segmentCount":1"#);
        assert_json_contains(&cluster, r#""memtableRows":0"#);
        assert_json_contains(&cluster, r#""readable":true"#);
        assert_json_contains(&cluster, r#""syncState":"leader""#);
        assert_json_contains(&cluster, r#""replicationLagLsn":0"#);
    }

    {
        let coord = WriteCoordinator::open_durable(&dir).unwrap();
        coord.recover();
        let api = EngineJsonApi::new_single_shard(coord.clone(), ShardId::new("tenant-77-shard-0"));
        let before = coord.metrics();
        assert_eq!(
            metric_value(&before, "yt_attr_sidecar_cache_loads"),
            0,
            "recover should not prewarm segment posting lists"
        );

        let query = r#"{"filter":{"projectId":"cluster-facade","skill":"routing"},"limit":10}"#;
        let (status, indexed) = api.route_with_tenant("POST", "/v1/trace-search", query, Some(77));
        assert_eq!(status, 200, "{indexed}");
        assert_json_contains(&indexed, r#""total":2"#);
        assert_json_contains(&indexed, r#""traceId":"80101""#);
        assert_json_contains(&indexed, r#""traceId":"80102""#);
        assert_json_contains(&indexed, r#""index":"attrs_postings+folded_verify""#);

        let after = coord.metrics();
        assert!(
            metric_value(&after, "yt_attr_sidecar_cache_loads") > 0,
            "cluster facade must preserve attrs sidecar indexed path\n{after}"
        );

        let (status, hidden) = api.route_with_tenant("POST", "/v1/trace-search", query, Some(78));
        assert_eq!(status, 200, "{hidden}");
        assert_json_contains(&hidden, r#""total":0"#);
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// 分布式升级第二步：进程内固定多 shard。
/// eval 覆盖写入按 session 路由、跨 shard traceSearch fanout merge、traceAggregate reduce，
/// 以及每个 shard 的 attrs sidecar 性能路径都被实际触发。
#[test]
fn in_process_cluster_routes_ingest_and_merges_indexed_queries() {
    let root = durable_dir("in_process_cluster");
    let tenant = 88u64;
    let shard_count = 3usize;
    let mut coords = Vec::new();
    let mut specs = Vec::new();
    for shard in 0..shard_count {
        let dir = root.join(format!("shard-{shard}"));
        let coord = WriteCoordinator::open_durable(&dir).unwrap();
        coord.recover();
        coords.push(coord.clone());
        specs.push((ShardId::new(format!("shard-{shard}")), coord));
    }
    let api = EngineJsonApi::new_in_process_cluster(specs).unwrap();
    let sessions: Vec<u64> = (0..shard_count)
        .map(|shard| session_for_shard(tenant, shard, shard_count))
        .collect();
    let batch = format!(
        r#"[
          {{"trace_id":91001,"span_id":1,"session_id":{},"ts":10,"seq":1,"event_type":2,"ext_span_id":"91001-1","status":0,"duration_ns":10,"input_tokens":10,"output_tokens":5,"attrs":{{"project_id":"cluster-router","skill":"distributed","validation_status":"pass","task_fingerprint":"cluster-v2"}}}},
          {{"trace_id":91002,"span_id":1,"session_id":{},"ts":20,"seq":1,"event_type":2,"ext_span_id":"91002-1","status":0,"duration_ns":20,"input_tokens":20,"output_tokens":10,"attrs":{{"project_id":"cluster-router","skill":"distributed","validation_status":"pass","task_fingerprint":"cluster-v2"}}}},
          {{"trace_id":91003,"span_id":1,"session_id":{},"ts":30,"seq":1,"event_type":2,"ext_span_id":"91003-1","status":1,"duration_ns":30,"input_tokens":30,"output_tokens":15,"attrs":{{"project_id":"cluster-router","skill":"distributed","validation_status":"fail","task_fingerprint":"cluster-v2"}}}}
        ]"#,
        sessions[0], sessions[1], sessions[2]
    );
    let (status, body) = api.route_with_tenant("POST", "/v1/ingest", &batch, Some(tenant));
    assert_eq!(status, 200, "{body}");
    for coord in &coords {
        coord.flush_memtable();
    }

    let (status, cluster) = api.route("GET", "/v1/cluster/shards", "");
    assert_eq!(status, 200, "{cluster}");
    assert_json_contains(&cluster, r#""mode":"in_process_cluster""#);
    assert_json_contains(&cluster, r#""routing":"hash_tenant_session_trace""#);
    assert_json_contains(&cluster, r#""shardCount":3"#);
    assert_json_contains(&cluster, r#""shardId":"shard-0""#);
    assert_json_contains(&cluster, r#""shardId":"shard-1""#);
    assert_json_contains(&cluster, r#""shardId":"shard-2""#);
    assert_eq!(
        cluster.matches(r#""committedTail":1"#).count(),
        3,
        "each shard should expose WAL committed tail for replica freshness: {cluster}"
    );
    assert_eq!(
        cluster.matches(r#""memtableWatermark":1"#).count(),
        3,
        "each flushed shard should expose watermark for snapshot/follower planning: {cluster}"
    );
    assert_eq!(
        cluster.matches(r#""replicationLagLsn":0"#).count(),
        3,
        "leader shards should report zero replica lag: {cluster}"
    );
    assert_eq!(
        cluster.matches(r#""segmentCount":1"#).count(),
        3,
        "each routed shard should have one flushed segment: {cluster}"
    );

    let query = r#"{"filter":{"projectId":"cluster-router","skill":"distributed"},"limit":10}"#;
    let before_sidecar: Vec<(u64, u64)> = coords
        .iter()
        .map(|coord| {
            let metrics = coord.metrics();
            (
                metric_value(&metrics, "yt_attr_sidecar_cache_loads"),
                metric_value(&metrics, "yt_attr_sidecar_cache_hits"),
            )
        })
        .collect();
    let (status, search) = api.route_with_tenant("POST", "/v1/trace-search", query, Some(tenant));
    assert_eq!(status, 200, "{search}");
    assert_json_contains(&search, r#""total":3"#);
    assert_json_contains(&search, r#""queryMode":"fanout_merge""#);
    assert_json_contains(&search, r#""shardCount":3"#);
    assert_json_contains(&search, r#""okShards":3"#);
    assert_json_contains(&search, r#""degraded":false"#);
    assert_json_contains(&search, r#""failedShards":[]"#);
    assert_json_contains(&search, r#""index":"attrs_postings+folded_verify""#);
    assert_json_contains(&search, r#""traceId":"91001""#);
    assert_json_contains(&search, r#""traceId":"91002""#);
    assert_json_contains(&search, r#""traceId":"91003""#);
    assert_json_contains(&search, r#""snapshot":{"mode":"in_process_cluster""#);
    let snapshot = extract_json_object_field(&search, "snapshot");
    assert_json_contains(&snapshot, r#""shardId":"shard-0""#);
    assert_json_contains(&snapshot, r#""shardId":"shard-1""#);
    assert_json_contains(&snapshot, r#""shardId":"shard-2""#);
    assert_eq!(
        snapshot.matches(r#""manifestVersion":1"#).count(),
        3,
        "each flushed shard should contribute a stable version: {snapshot}"
    );
    let search_with_snapshot = format!(
        r#"{{"filter":{{"projectId":"cluster-router","skill":"distributed"}},"cursor":1,"limit":2,"snapshot":{snapshot}}}"#
    );
    let (status, stable_page) = api.route_with_tenant(
        "POST",
        "/v1/trace-search",
        &search_with_snapshot,
        Some(tenant),
    );
    assert_eq!(status, 200, "{stable_page}");
    assert_json_contains(&stable_page, r#""total":3"#);
    assert_json_contains(&stable_page, r#""okShards":3"#);
    assert_json_contains(&stable_page, r#""degraded":false"#);
    assert_json_contains(&stable_page, r#""snapshot":{"mode":"in_process_cluster""#);
    let stale_snapshot = snapshot.replacen(r#""manifestVersion":1"#, r#""manifestVersion":0"#, 1);
    let stale_search = format!(
        r#"{{"filter":{{"projectId":"cluster-router","skill":"distributed"}},"snapshot":{stale_snapshot}}}"#
    );
    let (status, stale) =
        api.route_with_tenant("POST", "/v1/trace-search", &stale_search, Some(tenant));
    assert_eq!(status, 409, "{stale}");
    assert_json_contains(&stale, r#""code":"snapshot_mismatch""#);
    for (idx, coord) in coords.iter().enumerate() {
        let metrics = coord.metrics();
        let loads = metric_value(&metrics, "yt_attr_sidecar_cache_loads");
        let hits = metric_value(&metrics, "yt_attr_sidecar_cache_hits");
        assert!(
            loads > before_sidecar[idx].0 || hits > before_sidecar[idx].1,
            "fanout indexed query should touch every shard sidecar\nbefore={:?}\nafter loads={loads} hits={hits}\n{metrics}",
            before_sidecar[idx]
        );
    }

    let aggregate = r#"{"filter":{"projectId":"cluster-router"},"groupBy":["validationStatus"],"sort":"count","limit":10}"#;
    let (status, agg) =
        api.route_with_tenant("POST", "/v1/trace-aggregate", aggregate, Some(tenant));
    assert_eq!(status, 200, "{agg}");
    assert_json_contains(&agg, r#""spanTotal":3"#);
    assert_json_contains(
        &agg,
        r#""aggregationIndex":"fanout_segment_rollup_tail_overlay""#,
    );
    assert_json_contains(&agg, r#""usedSegmentRollup":true"#);
    assert_json_contains(&agg, r#""spanReadIndex":"segment_rollup""#);
    assert_json_contains(&agg, r#""queryMode":"fanout_merge""#);
    assert_json_contains(&agg, r#""okShards":3"#);
    assert_json_contains(&agg, r#""degraded":false"#);
    assert_json_contains(&agg, r#""failedShards":[]"#);
    assert_json_contains(&agg, r#""snapshot":{"mode":"in_process_cluster""#);
    assert_json_contains(&agg, r#""key":{"validation_status":"pass"},"spanCount":2"#);
    assert_json_contains(&agg, r#""key":{"validation_status":"fail"},"spanCount":1"#);

    let (status, hidden) =
        api.route_with_tenant("POST", "/v1/trace-search", query, Some(tenant + 1));
    assert_eq!(status, 200, "{hidden}");
    assert_json_contains(&hidden, r#""total":0"#);

    let _ = std::fs::remove_dir_all(&root);
}

/// `@yitrace/db.search()` 底层走 `/v1/search`，cluster mode 不能只查 primary shard。
/// eval 覆盖 text、vector、hybrid 三条搜索路径的 fanout merge 和 tenant 隔离。
#[test]
fn in_process_cluster_fanout_merges_db_search_endpoint() {
    let root = durable_dir("cluster_search_endpoint");
    let tenant = 96u64;
    let shard_count = 3usize;
    let sessions: Vec<u64> = (0..shard_count)
        .map(|shard| session_for_shard(tenant, shard, shard_count))
        .collect();
    let mut coords = Vec::new();
    let mut specs = Vec::new();
    for shard in 0..shard_count {
        let dir = root.join(format!("shard-{shard}"));
        let coord = WriteCoordinator::open_durable(&dir).unwrap();
        coord.recover();
        coords.push(coord.clone());
        specs.push((ShardId::new(format!("shard-{shard}")), coord));
    }
    let api = EngineJsonApi::new_in_process_cluster(specs).unwrap();
    let batch = format!(
        r#"[
          {{"trace_id":99001,"span_id":1,"session_id":{},"ts":10,"seq":1,"event_type":2,"ext_span_id":"99001-1","status":0,"duration_ns":10,"agent_name":"risk-0","input_text":"盗刷 检索 shard 0","attrs":{{"project_id":"cluster-search","skill":"db-search","task_fingerprint":"search-cluster"}}}},
          {{"trace_id":99002,"span_id":1,"session_id":{},"ts":20,"seq":1,"event_type":2,"ext_span_id":"99002-1","status":0,"duration_ns":20,"agent_name":"risk-1","input_text":"盗刷 检索 shard 1","attrs":{{"project_id":"cluster-search","skill":"db-search","task_fingerprint":"search-cluster"}}}},
          {{"trace_id":99003,"span_id":1,"session_id":{},"ts":30,"seq":1,"event_type":2,"ext_span_id":"99003-1","status":0,"duration_ns":30,"agent_name":"risk-2","input_text":"盗刷 检索 shard 2","attrs":{{"project_id":"cluster-search","skill":"db-search","task_fingerprint":"search-cluster"}}}}
        ]"#,
        sessions[0], sessions[1], sessions[2]
    );
    let (status, body) = api.route_with_tenant("POST", "/v1/ingest", &batch, Some(tenant));
    assert_eq!(status, 200, "{body}");
    for (idx, coord) in coords.iter().enumerate() {
        coord.flush_memtable();
        coord.index_embedding(99001 + idx as u64, 1, vec![0.1 + idx as f32 * 0.01, 0.1]);
    }

    let text_body = r#"{"text":"盗刷","k":10,"includeFanout":true,"filter":{"projectId":"cluster-search","skill":"db-search"}}"#;
    let (status, text) = api.route_with_tenant("POST", "/v1/search", text_body, Some(tenant));
    assert_eq!(status, 200, "{text}");
    assert_json_contains(&text, r#""queryMode":"fanout_merge""#);
    assert_json_contains(&text, r#""total":3"#);
    assert_json_contains(&text, r#""shardCount":3"#);
    assert_json_contains(&text, r#""okShards":3"#);
    assert_json_contains(&text, r#""degraded":false"#);
    assert_json_contains(&text, r#""failedShards":[]"#);
    assert_json_contains(&text, r#""trace_id":99001"#);
    assert_json_contains(&text, r#""trace_id":99002"#);
    assert_json_contains(&text, r#""trace_id":99003"#);
    assert_json_contains(&text, r#""project_id":"cluster-search""#);

    let vector_body = r#"{"vector":[0.1,0.1],"k":10,"includeFanout":true,"filter":{"projectId":"cluster-search"}}"#;
    let (status, vector) = api.route_with_tenant("POST", "/v1/search", vector_body, Some(tenant));
    assert_eq!(status, 200, "{vector}");
    assert_json_contains(&vector, r#""queryMode":"fanout_merge""#);
    assert_json_contains(&vector, r#""okShards":3"#);
    assert_json_contains(&vector, r#""trace_id":99001"#);
    assert_json_contains(&vector, r#""trace_id":99002"#);
    assert_json_contains(&vector, r#""trace_id":99003"#);

    let hybrid_body = r#"{"text":"盗刷","vector":[0.1,0.1],"k":10,"includeFanout":true,"filter":{"projectId":"cluster-search"}}"#;
    let (status, hybrid) = api.route_with_tenant("POST", "/v1/search", hybrid_body, Some(tenant));
    assert_eq!(status, 200, "{hybrid}");
    assert_json_contains(&hybrid, r#""queryMode":"fanout_merge""#);
    assert_json_contains(&hybrid, r#""okShards":3"#);
    assert_json_contains(&hybrid, r#""trace_id":99001"#);
    assert_json_contains(&hybrid, r#""trace_id":99002"#);
    assert_json_contains(&hybrid, r#""trace_id":99003"#);

    let (status, hidden) = api.route_with_tenant("POST", "/v1/search", text_body, Some(tenant + 1));
    assert_eq!(status, 200, "{hidden}");
    assert_json_contains(&hidden, r#""items":[]"#);
    assert_json_contains(&hidden, r#""total":0"#);
    assert_json_contains(&hidden, r#""degraded":false"#);

    let legacy_body =
        r#"{"text":"盗刷","k":10,"filter":{"projectId":"cluster-search","skill":"db-search"}}"#;
    let (status, legacy) = api.route_with_tenant("POST", "/v1/search", legacy_body, Some(tenant));
    assert_eq!(status, 200, "{legacy}");
    assert!(
        legacy.starts_with('['),
        "default cluster /v1/search response must keep the legacy array shape: {legacy}"
    );
    assert!(!legacy.contains(r#""queryMode":"#), "{legacy}");

    let _ = std::fs::remove_dir_all(&root);
}

/// OTLP/OpenInference 是生态入口，也必须复用 shard router。
/// eval 覆盖 yitrace.session_id 路由到非 primary、tenant header 覆盖、attrs/search/detail 查回。
#[test]
fn in_process_cluster_routes_otlp_ingest_to_owner_shard() {
    let root = durable_dir("cluster_otlp_ingest");
    let tenant = 97u64;
    let shard_count = 3usize;
    let owner_shard = 2usize;
    let session = session_for_shard(tenant, owner_shard, shard_count);
    let trace_id = 0xa001u64;
    let mut coords = Vec::new();
    let mut specs = Vec::new();
    for shard in 0..shard_count {
        let dir = root.join(format!("shard-{shard}"));
        let coord = WriteCoordinator::open_durable(&dir).unwrap();
        coord.recover();
        coords.push(coord.clone());
        specs.push((ShardId::new(format!("shard-{shard}")), coord));
    }
    let api = EngineJsonApi::new_in_process_cluster(specs).unwrap();
    let otlp = format!(
        r#"{{
          "resourceSpans":[{{"scopeSpans":[{{"spans":[{{
            "traceId":"0000000000000000000000000000a001",
            "spanId":"0000000000000001",
            "name":"cluster otlp chat",
            "startTimeUnixNano":"100",
            "endTimeUnixNano":"150",
            "status":{{"code":1}},
            "attributes":[
              {{"key":"yitrace.tenant_id","value":{{"stringValue":"999"}}}},
              {{"key":"yitrace.session_id","value":{{"stringValue":"{}"}}}},
              {{"key":"yitrace.project_id","value":{{"stringValue":"cluster-otlp"}}}},
              {{"key":"yitrace.skill","value":{{"stringValue":"otlp"}}}},
              {{"key":"input.value","value":{{"stringValue":"OTLP 盗刷 cluster"}}}},
              {{"key":"output.value","value":{{"stringValue":"OTLP owner shard ok"}}}},
              {{"key":"gen_ai.usage.input_tokens","value":{{"intValue":"42"}}}}
            ]
          }}]}}]}}]
        }}"#,
        session
    );
    let (status, body) = api.route_with_tenant("POST", "/v1/traces", &otlp, Some(tenant));
    assert_eq!(status, 200, "{body}");
    assert_json_contains(&body, r#""partialSuccess":{}"#);
    for coord in &coords {
        coord.flush_memtable();
    }

    let primary_snap = coords[0].pin_snapshot();
    assert!(
        coords[0]
            .console_trace_spans_for_tenant(&primary_snap, trace_id, Some(tenant))
            .is_empty(),
        "OTLP cluster ingest must not fall back to primary shard"
    );
    let owner_snap = coords[owner_shard].pin_snapshot();
    let owner_spans =
        coords[owner_shard].console_trace_spans_for_tenant(&owner_snap, trace_id, Some(tenant));
    assert_eq!(owner_spans.len(), 1, "owner shard should contain OTLP span");
    assert_eq!(owner_spans[0].input_tokens, 42);

    let (status, search) = api.route_with_tenant(
        "POST",
        "/v1/search",
        r#"{"text":"盗刷","k":10,"filter":{"projectId":"cluster-otlp","skill":"otlp"}}"#,
        Some(tenant),
    );
    assert_eq!(status, 200, "{search}");
    assert_json_contains(&search, r#""trace_id":40961"#);
    assert_json_contains(
        &search,
        r#""external_trace_id":"0000000000000000000000000000a001""#,
    );

    let (status, detail) = api.route_with_tenant("GET", "/v1/traces/40961", "", Some(tenant));
    assert_eq!(status, 200, "{detail}");
    assert_json_contains(&detail, r#""project_id":"cluster-otlp""#);

    let (status, span) = api.route_with_tenant("GET", "/v1/traces/40961/spans/1", "", Some(tenant));
    assert_eq!(status, 200, "{span}");
    assert_json_contains(&span, r#""input":"OTLP 盗刷 cluster""#);
    assert_json_contains(&span, r#""output":"OTLP owner shard ok""#);

    let (status, spoofed) = api.route_with_tenant("GET", "/v1/traces/40961", "", Some(999));
    assert_eq!(status, 404, "{spoofed}");

    let _ = std::fs::remove_dir_all(&root);
}

/// L2 前置能力：shard follower 先支持 WAL tail shipping，再接真实网络/复制协议。
/// eval 覆盖增量同步、部分重叠重试、重复批次幂等、LSN gap 拒绝和 follower 重启恢复。
#[test]
fn shard_wal_shipping_follower_replays_incrementally_and_recovers() {
    let leader_dir = durable_dir("wal_shipping_leader");
    let follower_dir = durable_dir("wal_shipping_follower");
    {
        let leader = WriteCoordinator::open_durable(&leader_dir).unwrap();
        leader.recover();
        let follower = WriteCoordinator::open_durable(&follower_dir).unwrap();
        follower.recover();

        leader.ingest_wire_for_tenant(
            eval_trace(88_001, "wal-shipping", "第一批 follower 可见", 0),
            Some(880),
        );
        let first = leader.export_wal_after(0);
        assert_eq!(first.from_lsn, 0);
        assert_eq!(first.to_lsn, 2);
        assert_eq!(first.records.len(), 2);
        let status = follower.apply_wal_replication_batch(&first).unwrap();
        assert_eq!(status.committed_tail, 2);
        assert_eq!(status.memtable_rows, 2);

        leader.ingest_wire_for_tenant(
            eval_trace(88_002, "wal-shipping", "第二批 follower 可见", 0),
            Some(880),
        );
        let lagging_decision = follower
            .replication_status()
            .replica_read_decision(&leader.replication_status(), 0);
        assert!(!lagging_decision.readable);
        assert_eq!(lagging_decision.sync_state, "stale");
        assert_eq!(lagging_decision.replication_lag_lsn, 2);
        assert_eq!(lagging_decision.reason, "lag_exceeds_budget");
        let bounded_stale_decision = follower
            .replication_status()
            .replica_read_decision(&leader.replication_status(), 2);
        assert!(bounded_stale_decision.readable);
        assert_eq!(bounded_stale_decision.sync_state, "catching_up");
        assert_eq!(bounded_stale_decision.reason, "within_lag_budget");
        let combined = leader.export_wal_after(0);
        assert_eq!(combined.to_lsn, 4);
        assert_eq!(combined.records.len(), 4);

        let status = follower.apply_wal_replication_batch(&combined).unwrap();
        assert_eq!(status.committed_tail, 4);
        assert_eq!(status.memtable_rows, 4);
        let caught_up_decision = follower
            .replication_status()
            .replica_read_decision(&leader.replication_status(), 0);
        assert!(caught_up_decision.readable);
        assert_eq!(caught_up_decision.sync_state, "ready");
        assert_eq!(caught_up_decision.replication_lag_lsn, 0);
        assert_eq!(caught_up_decision.reason, "caught_up");
        let repeated = follower.apply_wal_replication_batch(&combined).unwrap();
        assert_eq!(repeated.committed_tail, 4);
        assert_eq!(repeated.memtable_rows, 4);
        let ahead = ReplicationStatus {
            committed_tail: leader.replication_status().committed_tail + 1,
            ..follower.replication_status()
        };
        let ahead_decision = ahead.replica_read_decision(&leader.replication_status(), 0);
        assert!(!ahead_decision.readable);
        assert_eq!(ahead_decision.sync_state, "diverged");
        assert_eq!(ahead_decision.reason, "replica_tail_after_leader");

        let api = EngineJsonApi::new(follower.clone());
        let (status, page) = api.route_with_tenant(
            "POST",
            "/v1/trace-search",
            r#"{"filter":{"projectId":"wal-shipping"},"limit":10}"#,
            Some(880),
        );
        assert_eq!(status, 200, "{page}");
        assert_json_contains(&page, r#""total":2"#);
        assert_json_contains(&page, r#""traceId":"88001""#);
        assert_json_contains(&page, r#""traceId":"88002""#);

        let gap = WalReplicationBatch {
            from_lsn: repeated.committed_tail + 1,
            to_lsn: repeated.committed_tail + 3,
            records: first.records.clone(),
        };
        let err = follower.apply_wal_replication_batch(&gap).unwrap_err();
        assert!(
            err.contains("replication gap"),
            "expected gap error, got {err}"
        );
    }

    let reopened = WriteCoordinator::open_durable(&follower_dir).unwrap();
    reopened.recover();
    let status = reopened.replication_status();
    assert_eq!(status.committed_tail, 4);
    assert_eq!(status.memtable_rows, 4);
    let api = EngineJsonApi::new(reopened);
    let (status, page) = api.route_with_tenant(
        "POST",
        "/v1/trace-search",
        r#"{"filter":{"projectId":"wal-shipping"},"limit":10}"#,
        Some(880),
    );
    assert_eq!(status, 200, "{page}");
    assert_json_contains(&page, r#""total":2"#);

    let _ = std::fs::remove_dir_all(&leader_dir);
    let _ = std::fs::remove_dir_all(&follower_dir);
}

/// leader 已经 flush 后，follower 不能只靠 WAL tail 从 0 追数据。
/// 正确流程是先在线 snapshot bootstrap，再从 follower committed tail 追 leader WAL 增量。
#[test]
fn shard_snapshot_bootstrap_then_wal_catchup_covers_flushed_segments() {
    let leader_dir = durable_dir("replica_bootstrap_leader");
    let follower_dir = durable_dir("replica_bootstrap_follower");
    {
        let leader = WriteCoordinator::open_durable(&leader_dir).unwrap();
        leader.recover();
        leader.ingest_wire_for_tenant(
            eval_trace(88_101, "replica-bootstrap", "已封段数据", 0),
            Some(881),
        );
        leader.flush_memtable();
        let leader_status = leader.replication_status();
        assert_eq!(leader_status.committed_tail, 2);
        assert_eq!(leader_status.memtable_watermark, 2);
        assert_eq!(leader_status.segment_count, 1);

        leader.backup_snapshot(&follower_dir).unwrap();
        let follower = WriteCoordinator::open_durable(&follower_dir).unwrap();
        follower.recover();
        let follower_status = follower.replication_status();
        assert_eq!(follower_status.committed_tail, 2);
        assert_eq!(follower_status.memtable_watermark, 2);
        assert_eq!(follower_status.segment_count, 1);
        assert_eq!(follower_status.memtable_rows, 0);

        let api = EngineJsonApi::new(follower.clone());
        let (status, bootstrapped) = api.route_with_tenant(
            "POST",
            "/v1/trace-search",
            r#"{"filter":{"projectId":"replica-bootstrap"},"limit":10}"#,
            Some(881),
        );
        assert_eq!(status, 200, "{bootstrapped}");
        assert_json_contains(&bootstrapped, r#""total":1"#);
        assert_json_contains(&bootstrapped, r#""traceId":"88101""#);

        leader.ingest_wire_for_tenant(
            eval_trace(88_102, "replica-bootstrap", "WAL tail 追平", 0),
            Some(881),
        );
        let delta = leader.export_wal_after(follower_status.committed_tail);
        assert_eq!(delta.from_lsn, 2);
        assert_eq!(delta.to_lsn, 4);
        assert_eq!(delta.records.len(), 2);
        let caught_up = follower.apply_wal_replication_batch(&delta).unwrap();
        assert_eq!(caught_up.committed_tail, 4);
        assert_eq!(caught_up.segment_count, 1);
        assert_eq!(caught_up.memtable_rows, 2);

        let (status, after_catchup) = api.route_with_tenant(
            "POST",
            "/v1/trace-search",
            r#"{"filter":{"projectId":"replica-bootstrap"},"limit":10}"#,
            Some(881),
        );
        assert_eq!(status, 200, "{after_catchup}");
        assert_json_contains(&after_catchup, r#""total":2"#);
        assert_json_contains(&after_catchup, r#""traceId":"88101""#);
        assert_json_contains(&after_catchup, r#""traceId":"88102""#);
    }

    let reopened = WriteCoordinator::open_durable(&follower_dir).unwrap();
    reopened.recover();
    let status = reopened.replication_status();
    assert_eq!(status.committed_tail, 4);
    assert_eq!(status.segment_count, 1);
    assert_eq!(status.memtable_rows, 2);
    let api = EngineJsonApi::new(reopened);
    let (status, page) = api.route_with_tenant(
        "POST",
        "/v1/trace-search",
        r#"{"filter":{"projectId":"replica-bootstrap"},"limit":10}"#,
        Some(881),
    );
    assert_eq!(status, 200, "{page}");
    assert_json_contains(&page, r#""total":2"#);

    let _ = std::fs::remove_dir_all(&leader_dir);
    let _ = std::fs::remove_dir_all(&follower_dir);
}

/// 分布式升级第三步前置：trace/session 列表页不能只读 primary shard。
/// eval 覆盖跨 shard 列表 fanout merge、attrs postings 性能路径、session 分页排序和 tenant 隔离。
#[test]
fn in_process_cluster_lists_traces_and_sessions_across_shards() {
    let root = durable_dir("cluster_list_fanout");
    let tenant = 89u64;
    let shard_count = 3usize;
    let mut coords = Vec::new();
    let mut specs = Vec::new();
    for shard in 0..shard_count {
        let dir = root.join(format!("shard-{shard}"));
        let coord = WriteCoordinator::open_durable(&dir).unwrap();
        coord.recover();
        coords.push(coord.clone());
        specs.push((ShardId::new(format!("shard-{shard}")), coord));
    }
    let api = EngineJsonApi::new_in_process_cluster(specs).unwrap();
    let sessions: Vec<u64> = (0..shard_count)
        .map(|shard| session_for_shard(tenant, shard, shard_count))
        .collect();
    let mut sorted_sessions = sessions.clone();
    sorted_sessions.sort_by(|a, b| b.cmp(a));
    let batch = format!(
        r#"[
          {{"trace_id":93001,"span_id":1,"session_id":{},"ts":10,"seq":1,"event_type":2,"ext_span_id":"93001-1","agent_name":"list-agent-0","status":0,"duration_ns":10,"input_tokens":10,"output_tokens":5,"attrs":{{"project_id":"cluster-list","skill":"list","mode":"shard-0"}}}},
          {{"trace_id":93002,"span_id":1,"session_id":{},"ts":20,"seq":1,"event_type":2,"ext_span_id":"93002-1","agent_name":"list-agent-1","status":0,"duration_ns":20,"input_tokens":20,"output_tokens":10,"attrs":{{"project_id":"cluster-list","skill":"list","mode":"shard-1"}}}},
          {{"trace_id":93003,"span_id":1,"session_id":{},"ts":30,"seq":1,"event_type":2,"ext_span_id":"93003-1","agent_name":"list-agent-2","status":1,"duration_ns":30,"input_tokens":30,"output_tokens":15,"attrs":{{"project_id":"cluster-list","skill":"list","mode":"shard-2"}}}}
        ]"#,
        sessions[0], sessions[1], sessions[2]
    );
    let (status, body) = api.route_with_tenant("POST", "/v1/ingest", &batch, Some(tenant));
    assert_eq!(status, 200, "{body}");
    for coord in &coords {
        coord.flush_memtable();
    }

    let primary_snap = coords[0].pin_snapshot();
    let mut q = TraceQuery::all();
    q.tenant_id = Some(tenant);
    assert_eq!(
        coords[0].list_traces(&primary_snap, &q).len(),
        1,
        "test setup should keep only one trace on the primary shard"
    );

    let before_sidecar: Vec<(u64, u64)> = coords
        .iter()
        .map(|coord| {
            let metrics = coord.metrics();
            (
                metric_value(&metrics, "yt_attr_sidecar_cache_loads"),
                metric_value(&metrics, "yt_attr_sidecar_cache_hits"),
            )
        })
        .collect();
    let list_query = "/v1/traces?projectId=cluster-list&skill=list";
    let (status, traces) = api.route_with_tenant("GET", list_query, "", Some(tenant));
    assert_eq!(status, 200, "{traces}");
    assert_json_contains(&traces, r#""trace_id":93001"#);
    assert_json_contains(&traces, r#""trace_id":93002"#);
    assert_json_contains(&traces, r#""trace_id":93003"#);
    assert_json_contains(&traces, r#""project_id":"cluster-list""#);
    assert_json_contains(&traces, r#""skill":"list""#);
    for (idx, coord) in coords.iter().enumerate() {
        let metrics = coord.metrics();
        let loads = metric_value(&metrics, "yt_attr_sidecar_cache_loads");
        let hits = metric_value(&metrics, "yt_attr_sidecar_cache_hits");
        assert!(
            loads > before_sidecar[idx].0 || hits > before_sidecar[idx].1,
            "cluster trace list attrs query should touch every shard sidecar\nbefore={:?}\nafter loads={loads} hits={hits}\n{metrics}",
            before_sidecar[idx]
        );
    }

    let (status, hidden_traces) = api.route_with_tenant("GET", list_query, "", Some(tenant + 1));
    assert_eq!(status, 200, "{hidden_traces}");
    assert_eq!(hidden_traces, "[]");

    let (status, page1) = api.route_with_tenant(
        "GET",
        "/v1/sessions?projectId=cluster-list&limit=2",
        "",
        Some(tenant),
    );
    assert_eq!(status, 200, "{page1}");
    assert_json_contains(&page1, r#""total":3"#);
    assert_json_contains(&page1, r#""nextCursor":2"#);
    assert_json_contains(&page1, r#""queryMode":"fanout_merge""#);
    assert_json_contains(&page1, r#""shardCount":3"#);
    assert_json_contains(&page1, r#""snapshot":{"mode":"in_process_cluster""#);
    assert_json_contains(&page1, &format!(r#""sessionId":"{}""#, sorted_sessions[0]));
    assert_json_contains(&page1, &format!(r#""sessionId":"{}""#, sorted_sessions[1]));
    assert!(
        !page1.contains(&format!(r#""sessionId":"{}""#, sorted_sessions[2])),
        "first page should not include the third sorted session: {page1}"
    );
    let session_snapshot = extract_json_object_field(&page1, "snapshot");
    let session_snapshot_query = url_encode_component(&session_snapshot);

    let (status, page2) = api.route_with_tenant(
        "GET",
        &format!(
            "/v1/sessions?projectId=cluster-list&cursor=2&limit=2&snapshot={session_snapshot_query}"
        ),
        "",
        Some(tenant),
    );
    assert_eq!(status, 200, "{page2}");
    assert_json_contains(&page2, r#""total":3"#);
    assert_json_contains(&page2, r#""nextCursor":null"#);
    assert_json_contains(&page2, &format!(r#""sessionId":"{}""#, sorted_sessions[2]));

    let (status, explicit_lease) =
        api.route_with_tenant("POST", "/v1/snapshots/lease", "{}", Some(tenant));
    assert_eq!(status, 200, "{explicit_lease}");
    assert_json_contains(&explicit_lease, r#""leaseState":"active""#);
    let explicit_snapshot = extract_json_object_field(&explicit_lease, "snapshot");
    let explicit_lease_id = extract_json_string_field(&explicit_lease, "leaseId");
    let explicit_snapshot_query = url_encode_component(&explicit_snapshot);

    let (status, bad_snapshot) = api.route_with_tenant(
        "GET",
        "/v1/sessions?projectId=cluster-list&snapshot=not-json",
        "",
        Some(tenant),
    );
    assert_eq!(status, 400, "{bad_snapshot}");
    assert_json_contains(&bad_snapshot, r#""code":"bad_snapshot""#);

    let mutation = format!(
        r#"[
          {{"trace_id":93004,"span_id":1,"session_id":{},"ts":40,"seq":1,"event_type":2,"ext_span_id":"93004-1","agent_name":"list-agent-new","status":0,"duration_ns":40,"attrs":{{"project_id":"cluster-list","skill":"list","mode":"after-snapshot"}}}}
        ]"#,
        sessions[0]
    );
    let (status, body) = api.route_with_tenant("POST", "/v1/ingest", &mutation, Some(tenant));
    assert_eq!(status, 200, "{body}");
    coords[0].flush_memtable();
    let (status, leased_page_after_write) = api.route_with_tenant(
        "GET",
        &format!("/v1/sessions?projectId=cluster-list&cursor=2&limit=2&snapshot={session_snapshot_query}"),
        "",
        Some(tenant),
    );
    assert_eq!(status, 200, "{leased_page_after_write}");
    assert_json_contains(&leased_page_after_write, r#""total":3"#);
    assert!(
        !leased_page_after_write.contains("list-agent-new"),
        "leased snapshot should keep reading the old manifest: {leased_page_after_write}"
    );
    let (status, explicit_page_after_write) = api.route_with_tenant(
        "GET",
        &format!("/v1/sessions?projectId=cluster-list&limit=10&snapshot={explicit_snapshot_query}"),
        "",
        Some(tenant),
    );
    assert_eq!(status, 200, "{explicit_page_after_write}");
    assert_json_contains(&explicit_page_after_write, r#""total":3"#);
    assert!(
        !explicit_page_after_write.contains("list-agent-new"),
        "explicit lease should pin the pre-mutation snapshot: {explicit_page_after_write}"
    );
    let (status, renewed) = api.route_with_tenant(
        "POST",
        "/v1/snapshots/renew",
        &format!(r#"{{"leaseId":"{}"}}"#, explicit_lease_id),
        Some(tenant),
    );
    assert_eq!(status, 200, "{renewed}");
    assert_json_contains(&renewed, r#""leaseState":"active""#);
    let (status, released) = api.route_with_tenant(
        "DELETE",
        &format!("/v1/snapshots/{}", explicit_lease_id),
        "",
        Some(tenant),
    );
    assert_eq!(status, 200, "{released}");
    assert_json_contains(&released, r#""leaseState":"released""#);
    let (status, renew_after_release) = api.route_with_tenant(
        "POST",
        "/v1/snapshots/renew",
        &format!(r#"{{"leaseId":"{}"}}"#, explicit_lease_id),
        Some(tenant),
    );
    assert_eq!(status, 409, "{renew_after_release}");
    assert_json_contains(&renew_after_release, r#""code":"snapshot_expired""#);

    let tampered_snapshot =
        session_snapshot.replacen(r#""manifestVersion":1"#, r#""manifestVersion":0"#, 1);
    let tampered_snapshot_query = url_encode_component(&tampered_snapshot);
    let (status, stale_page) = api.route_with_tenant(
        "GET",
        &format!(
            "/v1/sessions?projectId=cluster-list&cursor=2&limit=2&snapshot={tampered_snapshot_query}"
        ),
        "",
        Some(tenant),
    );
    assert_eq!(status, 409, "{stale_page}");
    assert_json_contains(&stale_page, r#""code":"snapshot_mismatch""#);

    let (status, fresh_after_write) = api.route_with_tenant(
        "GET",
        "/v1/sessions?projectId=cluster-list&limit=10",
        "",
        Some(tenant),
    );
    assert_eq!(status, 200, "{fresh_after_write}");
    assert_json_contains(&fresh_after_write, r#""total":3"#);
    assert_json_contains(&fresh_after_write, r#""turnCount":2"#);

    for _ in 0..70 {
        let (status, body) = api.route_with_tenant(
            "GET",
            "/v1/sessions?projectId=cluster-list&limit=1",
            "",
            Some(tenant),
        );
        assert_eq!(status, 200, "{body}");
    }
    let (status, expired_page) = api.route_with_tenant(
        "GET",
        &format!(
            "/v1/sessions?projectId=cluster-list&cursor=2&limit=2&snapshot={session_snapshot_query}"
        ),
        "",
        Some(tenant),
    );
    assert_eq!(status, 409, "{expired_page}");
    assert_json_contains(&expired_page, r#""code":"snapshot_expired""#);

    let (status, filtered) = api.route_with_tenant(
        "GET",
        "/v1/sessions?projectId=cluster-list&filter=list-agent-1",
        "",
        Some(tenant),
    );
    assert_eq!(status, 200, "{filtered}");
    assert_json_contains(&filtered, r#""total":1"#);
    assert_json_contains(&filtered, r#""title":"list-agent-1""#);

    let (status, hidden_sessions) = api.route_with_tenant(
        "GET",
        "/v1/sessions?projectId=cluster-list",
        "",
        Some(tenant + 1),
    );
    assert_eq!(status, 200, "{hidden_sessions}");
    assert_json_contains(&hidden_sessions, r#""total":0"#);

    let _ = std::fs::remove_dir_all(&root);
}

/// 分布式产品读模型也必须跨 shard：trajectory read model、path mining 和 storage stats
/// 都复用 traceSearch 过滤语义，并保持 attrs postings 性能路径。
#[test]
fn in_process_cluster_merges_trajectory_and_storage_read_models() {
    let root = durable_dir("cluster_product_read_models");
    let tenant = 90u64;
    let shard_count = 3usize;
    let mut coords = Vec::new();
    let mut specs = Vec::new();
    for shard in 0..shard_count {
        let dir = root.join(format!("shard-{shard}"));
        let coord = WriteCoordinator::open_durable(&dir).unwrap();
        coord.recover();
        coords.push(coord.clone());
        specs.push((ShardId::new(format!("shard-{shard}")), coord));
    }
    let api = EngineJsonApi::new_in_process_cluster(specs).unwrap();
    let sessions: Vec<u64> = (0..shard_count)
        .map(|shard| session_for_shard(tenant, shard, shard_count))
        .collect();
    let batch = format!(
        r#"[
          {{"trace_id":94001,"span_id":1,"session_id":{},"ts":10,"seq":1,"event_type":2,"ext_span_id":"94001-1","agent_name":"planner","status":0,"duration_ns":10,"input_tokens":10,"output_tokens":5,"attrs":{{"project_id":"cluster-products","skill":"read-models","validation_status":"pass","task_fingerprint":"cluster-v5"}}}},
          {{"trace_id":94001,"span_id":2,"parent_span_id":1,"session_id":{},"ts":11,"seq":1,"event_type":2,"ext_span_id":"94001-2","tool_name":"runner","status":0,"duration_ns":20,"input_tokens":20,"output_tokens":10,"attrs":{{"project_id":"cluster-products","skill":"read-models","validation_status":"pass","task_fingerprint":"cluster-v5"}}}},
          {{"trace_id":94002,"span_id":1,"session_id":{},"ts":20,"seq":1,"event_type":2,"ext_span_id":"94002-1","agent_name":"planner","status":0,"duration_ns":30,"input_tokens":30,"output_tokens":15,"attrs":{{"project_id":"cluster-products","skill":"read-models","validation_status":"pass","task_fingerprint":"cluster-v5"}}}},
          {{"trace_id":94002,"span_id":2,"parent_span_id":1,"session_id":{},"ts":21,"seq":1,"event_type":2,"ext_span_id":"94002-2","tool_name":"runner","status":0,"duration_ns":40,"input_tokens":40,"output_tokens":20,"attrs":{{"project_id":"cluster-products","skill":"read-models","validation_status":"pass","task_fingerprint":"cluster-v5"}}}},
          {{"trace_id":94003,"span_id":1,"session_id":{},"ts":30,"seq":1,"event_type":2,"ext_span_id":"94003-1","agent_name":"planner","status":1,"duration_ns":50,"input_tokens":50,"output_tokens":25,"attrs":{{"project_id":"cluster-products","skill":"read-models","validation_status":"fail","task_fingerprint":"cluster-v5"}}}},
          {{"trace_id":94003,"span_id":2,"parent_span_id":1,"session_id":{},"ts":31,"seq":1,"event_type":2,"ext_span_id":"94003-2","tool_name":"runner","status":1,"duration_ns":60,"input_tokens":60,"output_tokens":30,"attrs":{{"project_id":"cluster-products","skill":"read-models","validation_status":"fail","task_fingerprint":"cluster-v5"}}}}
        ]"#,
        sessions[0], sessions[0], sessions[1], sessions[1], sessions[2], sessions[2]
    );
    let (status, body) = api.route_with_tenant("POST", "/v1/ingest", &batch, Some(tenant));
    assert_eq!(status, 200, "{body}");
    for coord in &coords {
        coord.flush_memtable();
    }

    let before_sidecar: Vec<(u64, u64)> = coords
        .iter()
        .map(|coord| {
            let metrics = coord.metrics();
            (
                metric_value(&metrics, "yt_attr_sidecar_cache_loads"),
                metric_value(&metrics, "yt_attr_sidecar_cache_hits"),
            )
        })
        .collect();

    let filter = r#"{"filter":{"projectId":"cluster-products","skill":"read-models"},"limit":10}"#;
    let (status, trajectories) =
        api.route_with_tenant("POST", "/v1/trace-trajectories", filter, Some(tenant));
    assert_eq!(status, 200, "{trajectories}");
    assert_json_contains(&trajectories, r#""total":3"#);
    assert_json_contains(&trajectories, r#""spanTotal":6"#);
    assert_json_contains(
        &trajectories,
        r#""index":"fanout_materialized_trace_trajectory_cache""#,
    );
    assert_json_contains(&trajectories, r#""queryMode":"fanout_merge""#);
    assert_json_contains(&trajectories, r#""shardCount":3"#);
    assert_json_contains(&trajectories, r#""snapshot":{"mode":"in_process_cluster""#);
    assert_json_contains(&trajectories, r#""traceId":"94003""#);
    assert_json_contains(&trajectories, r#""traceId":"94002""#);
    assert_json_contains(&trajectories, r#""traceId":"94001""#);
    assert_json_contains(&trajectories, r#""steps":["agent:planner","tool:runner"]"#);
    let trajectory_snapshot = extract_json_object_field(&trajectories, "snapshot");
    let trajectory_with_snapshot = format!(
        r#"{{"filter":{{"projectId":"cluster-products","skill":"read-models"}},"cursor":1,"limit":2,"snapshot":{trajectory_snapshot}}}"#
    );
    let (status, stable_trajectories) = api.route_with_tenant(
        "POST",
        "/v1/trace-trajectories",
        &trajectory_with_snapshot,
        Some(tenant),
    );
    assert_eq!(status, 200, "{stable_trajectories}");
    assert_json_contains(&stable_trajectories, r#""total":3"#);
    let stale_trajectory_snapshot =
        trajectory_snapshot.replacen(r#""manifestVersion":1"#, r#""manifestVersion":0"#, 1);
    let stale_trajectory_body = format!(
        r#"{{"filter":{{"projectId":"cluster-products","skill":"read-models"}},"snapshot":{stale_trajectory_snapshot}}}"#
    );
    let (status, stale_trajectories) = api.route_with_tenant(
        "POST",
        "/v1/trace-trajectories",
        &stale_trajectory_body,
        Some(tenant),
    );
    assert_eq!(status, 409, "{stale_trajectories}");
    assert_json_contains(&stale_trajectories, r#""code":"snapshot_mismatch""#);

    let (status, groups) =
        api.route_with_tenant("POST", "/v1/trajectory-groups", filter, Some(tenant));
    assert_eq!(status, 200, "{groups}");
    assert_json_contains(&groups, r#""total":1"#);
    assert_json_contains(&groups, r#""traceTotal":3"#);
    assert_json_contains(&groups, r#""spanTotal":6"#);
    assert_json_contains(
        &groups,
        r#""trajectoryIndex":"fanout_materialized_trajectory_cache""#,
    );
    assert_json_contains(&groups, r#""queryMode":"fanout_merge""#);
    assert_json_contains(&groups, r#""snapshot":{"mode":"in_process_cluster""#);
    assert_json_contains(&groups, r#""traceCount":3"#);
    assert_json_contains(&groups, r#""successCount":2"#);
    assert_json_contains(&groups, r#""errorTraceCount":1"#);
    assert_json_contains(&groups, r#""steps":["agent:planner","tool:runner"]"#);

    let storage_body = r#"{"filter":{"projectId":"cluster-products","skill":"read-models"},"groupBy":["validationStatus"]}"#;
    let (status, storage) =
        api.route_with_tenant("POST", "/v1/storage-stats", storage_body, Some(tenant));
    assert_eq!(status, 200, "{storage}");
    assert_json_contains(&storage, r#""queryMode":"fanout_merge""#);
    assert_json_contains(&storage, r#""shardCount":3"#);
    assert_json_contains(&storage, r#""snapshot":{"mode":"in_process_cluster""#);
    assert_json_contains(&storage, r#""traceCount":3"#);
    assert_json_contains(&storage, r#""spanCount":6"#);
    assert_json_contains(&storage, r#""eventCount":6"#);
    assert_json_contains(
        &storage,
        r#""key":{"validation_status":"pass"},"traceCount":2,"spanCount":4"#,
    );
    assert_json_contains(
        &storage,
        r#""key":{"validation_status":"fail"},"traceCount":1,"spanCount":2"#,
    );

    for (idx, coord) in coords.iter().enumerate() {
        let metrics = coord.metrics();
        let loads = metric_value(&metrics, "yt_attr_sidecar_cache_loads");
        let hits = metric_value(&metrics, "yt_attr_sidecar_cache_hits");
        assert!(
            loads > before_sidecar[idx].0 || hits > before_sidecar[idx].1,
            "cluster trajectory/storage queries should touch every shard sidecar\nbefore={:?}\nafter loads={loads} hits={hits}\n{metrics}",
            before_sidecar[idx]
        );
    }

    let (status, hidden) =
        api.route_with_tenant("POST", "/v1/trace-trajectories", filter, Some(tenant + 1));
    assert_eq!(status, 200, "{hidden}");
    assert_json_contains(&hidden, r#""total":0"#);
    assert_json_contains(&hidden, r#""spanTotal":0"#);

    let _ = std::fs::remove_dir_all(&root);
}

/// 分布式升级第六步：loop/task 产品读模型也必须跨 shard fanout merge。
/// eval 覆盖同一个 loop_id 分布在多个 shard、task trace 全局分页/过滤、tenant 隔离，
/// 并确认 project/task attrs 查询触达每个 shard 的 sidecar 索引。
#[test]
fn in_process_cluster_merges_loop_and_task_read_models() {
    let root = durable_dir("cluster_loop_task_read_models");
    let tenant = 92u64;
    let shard_count = 3usize;
    let mut coords = Vec::new();
    let mut specs = Vec::new();
    for shard in 0..shard_count {
        let dir = root.join(format!("shard-{shard}"));
        let coord = WriteCoordinator::open_durable(&dir).unwrap();
        coord.recover();
        coords.push(coord.clone());
        specs.push((ShardId::new(format!("shard-{shard}")), coord));
    }
    let api = EngineJsonApi::new_in_process_cluster(specs).unwrap();
    let sessions: Vec<u64> = (0..shard_count)
        .map(|shard| session_for_shard(tenant, shard, shard_count))
        .collect();
    let batch = format!(
        r#"[
          {{"trace_id":95001,"span_id":1,"session_id":{},"ts":10,"seq":1,"event_type":2,"ext_span_id":"95001-1","agent_name":"planner","status":0,"duration_ns":10,"input_tokens":10,"output_tokens":5,"attrs":{{"project_id":"cluster-loop-task","skill":"planner","loop_id":"loop-shared","task_fingerprint":"cluster-task","validation_status":"pass","phase":"plan"}}}},
          {{"trace_id":95001,"span_id":2,"parent_span_id":1,"session_id":{},"ts":11,"seq":1,"event_type":2,"ext_span_id":"95001-2","tool_name":"runner","status":0,"duration_ns":20,"input_tokens":20,"output_tokens":10,"attrs":{{"project_id":"cluster-loop-task","skill":"planner","loop_id":"loop-shared","task_fingerprint":"cluster-task","validation_status":"pass","phase":"verify","validator":"npm test"}}}},
          {{"trace_id":95002,"span_id":1,"session_id":{},"ts":20,"seq":1,"event_type":2,"ext_span_id":"95002-1","agent_name":"planner","status":1,"duration_ns":30,"input_tokens":30,"output_tokens":15,"attrs":{{"project_id":"cluster-loop-task","skill":"planner","loop_id":"loop-shared","task_fingerprint":"cluster-task","validation_status":"fail","phase":"verify","validator":"npm test"}}}},
          {{"trace_id":95003,"span_id":1,"session_id":{},"ts":30,"seq":1,"event_type":2,"ext_span_id":"95003-1","agent_name":"reviewer","status":0,"duration_ns":40,"input_tokens":40,"output_tokens":20,"attrs":{{"project_id":"cluster-loop-task","skill":"review","loop_id":"loop-other","task_fingerprint":"cluster-task","validation_status":"pass","phase":"review"}}}}
        ]"#,
        sessions[0], sessions[0], sessions[1], sessions[2]
    );
    let (status, body) = api.route_with_tenant("POST", "/v1/ingest", &batch, Some(tenant));
    assert_eq!(status, 200, "{body}");
    for coord in &coords {
        coord.flush_memtable();
    }

    let (status, loops) = api.route_with_tenant(
        "GET",
        "/v1/loops?projectId=cluster-loop-task&taskFingerprint=cluster-task",
        "",
        Some(tenant),
    );
    assert_eq!(status, 200, "{loops}");
    assert_json_contains(&loops, r#""total":2"#);
    assert_json_contains(
        &loops,
        r#""loopIndex":"fanout_loop_task_sidecar+tail_overlay""#,
    );
    assert_json_contains(&loops, r#""usedSegmentRollup":true"#);
    assert_json_contains(&loops, r#""spanReadIndex":"loop_task_sidecar""#);
    assert_json_contains(&loops, r#""queryMode":"fanout_merge""#);
    assert_json_contains(&loops, r#""shardCount":3"#);
    assert_json_contains(&loops, r#""snapshot":{"mode":"in_process_cluster""#);
    assert_json_contains(&loops, r#""loopId":"loop-shared""#);
    assert_json_contains(&loops, r#""traceCount":2"#);
    assert_json_contains(&loops, r#""errorCount":1"#);
    assert_json_contains(&loops, r#""phases":["plan","verify"]"#);
    assert_json_contains(&loops, r#""loopId":"loop-other""#);

    let (status, loop_detail) = api.route_with_tenant(
        "GET",
        "/v1/loops/loop-shared?projectId=cluster-loop-task",
        "",
        Some(tenant),
    );
    assert_eq!(status, 200, "{loop_detail}");
    assert_json_contains(&loop_detail, r#""queryMode":"fanout_merge""#);
    assert_json_contains(&loop_detail, r#""summary":{"loopId":"loop-shared""#);
    assert_json_contains(&loop_detail, r#""traceId":"95001""#);
    assert_json_contains(&loop_detail, r#""traceId":"95002""#);
    assert!(
        !loop_detail.contains(r#""traceId":"95003""#),
        "{loop_detail}"
    );

    let (status, task_page_1) = api.route_with_tenant(
        "GET",
        "/v1/tasks/cluster-task/traces?projectId=cluster-loop-task&validationStatus=pass&limit=1",
        "",
        Some(tenant),
    );
    assert_eq!(status, 200, "{task_page_1}");
    assert_json_contains(&task_page_1, r#""total":2"#);
    assert_json_contains(
        &task_page_1,
        r#""taskIndex":"fanout_loop_task_sidecar+tail_overlay""#,
    );
    assert_json_contains(&task_page_1, r#""usedSegmentRollup":true"#);
    assert_json_contains(&task_page_1, r#""spanReadIndex":"loop_task_sidecar""#);
    assert_json_contains(&task_page_1, r#""nextCursor":1"#);
    assert_json_contains(&task_page_1, r#""queryMode":"fanout_merge""#);
    assert_json_contains(&task_page_1, r#""snapshot":{"mode":"in_process_cluster""#);
    assert_json_contains(&task_page_1, r#""traceId":"95003""#);
    assert!(
        !task_page_1.contains(r#""traceId":"95001""#),
        "{task_page_1}"
    );
    assert!(
        !task_page_1.contains(r#""traceId":"95002""#),
        "{task_page_1}"
    );
    let task_snapshot = extract_json_object_field(&task_page_1, "snapshot");
    let task_snapshot_query = url_encode_component(&task_snapshot);

    let (status, task_page_2) = api.route_with_tenant(
        "GET",
        &format!(
            "/v1/tasks/cluster-task/traces?projectId=cluster-loop-task&validationStatus=pass&cursor=1&limit=1&snapshot={task_snapshot_query}"
        ),
        "",
        Some(tenant),
    );
    assert_eq!(status, 200, "{task_page_2}");
    assert_json_contains(&task_page_2, r#""total":2"#);
    assert_json_contains(&task_page_2, r#""nextCursor":null"#);
    assert_json_contains(&task_page_2, r#""traceId":"95001""#);
    assert!(
        !task_page_2.contains(r#""traceId":"95002""#),
        "{task_page_2}"
    );

    let (status, hidden) = api.route_with_tenant(
        "GET",
        "/v1/loops?projectId=cluster-loop-task&taskFingerprint=cluster-task",
        "",
        Some(tenant + 1),
    );
    assert_eq!(status, 200, "{hidden}");
    assert_json_contains(&hidden, r#""total":0"#);

    let (status, hidden_detail) = api.route_with_tenant(
        "GET",
        "/v1/loops/loop-shared?projectId=cluster-loop-task",
        "",
        Some(tenant + 1),
    );
    assert_eq!(status, 404, "{hidden_detail}");

    let _ = std::fs::remove_dir_all(&root);
}

/// 分布式升级第七步：annotation / dataset 这类后验证据要跟 source trace owner 同 shard。
/// 否则 cluster traceSearch 的 metadata 反向过滤会在数据 shard 看不到证据。
#[test]
fn in_process_cluster_colocates_metadata_with_trace_owner() {
    let root = durable_dir("cluster_metadata_colocation");
    let tenant = 93u64;
    let shard_count = 3usize;
    let owner_shard = 2usize;
    let session = session_for_shard(tenant, owner_shard, shard_count);
    let mut coords = Vec::new();
    let mut specs = Vec::new();
    for shard in 0..shard_count {
        let dir = root.join(format!("shard-{shard}"));
        let coord = WriteCoordinator::open_durable(&dir).unwrap();
        coord.recover();
        coords.push(coord.clone());
        specs.push((ShardId::new(format!("shard-{shard}")), coord));
    }
    let api = EngineJsonApi::new_in_process_cluster(specs).unwrap();
    let batch = format!(
        r#"[
          {{"trace_id":96001,"span_id":1,"session_id":{},"ts":10,"seq":1,"event_type":2,"ext_span_id":"96001-1","agent_name":"planner","status":0,"duration_ns":10,"input_text":"metadata owner trace","output_text":"metadata owner ok","attrs":{{"project_id":"cluster-metadata","skill":"review","task_fingerprint":"metadata-task"}}}},
          {{"trace_id":96002,"span_id":1,"session_id":{},"ts":20,"seq":1,"event_type":2,"ext_span_id":"96002-1","agent_name":"planner","status":0,"duration_ns":20,"input_text":"unmarked trace","output_text":"not selected","attrs":{{"project_id":"cluster-metadata","skill":"review","task_fingerprint":"metadata-task"}}}}
        ]"#,
        session, session
    );
    let (status, body) = api.route_with_tenant("POST", "/v1/ingest", &batch, Some(tenant));
    assert_eq!(status, 200, "{body}");
    for coord in &coords {
        coord.flush_memtable();
    }

    let annotation = r#"{"traceId":96001,"spanId":1,"target":"span","label":"best_path","score":980,"source":"human","projectId":"cluster-metadata","skill":"review"}"#;
    let (status, annotation_body) =
        api.route_with_tenant("POST", "/v1/annotations", annotation, Some(tenant));
    assert_eq!(status, 200, "{annotation_body}");
    let annotation_id = (((owner_shard as u64) + 1) << 56) + 1;
    assert_json_contains(
        &annotation_body,
        &format!(r#""annotationId":"{}""#, annotation_id),
    );

    let dataset = r#"{"datasetId":"cluster-regression","itemId":"case-96001","traceId":96001,"spanId":1,"label":"pass","score":970,"projectId":"cluster-metadata"}"#;
    let (status, dataset_body) =
        api.route_with_tenant("POST", "/v1/dataset-associations", dataset, Some(tenant));
    assert_eq!(status, 200, "{dataset_body}");
    let association_id = (((owner_shard as u64) + 1) << 56) + 1;
    assert_json_contains(
        &dataset_body,
        &format!(r#""associationId":"{}""#, association_id),
    );

    let annotation_filter = TraceAnnotationFilter {
        tenant_id: Some(tenant),
        trace_id: Some(96001),
        ..TraceAnnotationFilter::default()
    };
    assert_eq!(
        coords[0].annotations(&annotation_filter).len(),
        0,
        "cluster metadata must not fall back to primary shard"
    );
    assert_eq!(
        coords[owner_shard].annotations(&annotation_filter).len(),
        1,
        "annotation should be co-located with trace owner shard"
    );
    let dataset_filter = DatasetAssociationFilter {
        tenant_id: Some(tenant),
        trace_id: Some(96001),
        ..DatasetAssociationFilter::default()
    };
    assert_eq!(coords[0].dataset_associations(&dataset_filter).len(), 0);
    assert_eq!(
        coords[owner_shard]
            .dataset_associations(&dataset_filter)
            .len(),
        1
    );

    let (status, annotations) = api.route_with_tenant(
        "GET",
        "/v1/annotations?traceId=96001&projectId=cluster-metadata",
        "",
        Some(tenant),
    );
    assert_eq!(status, 200, "{annotations}");
    assert_json_contains(&annotations, r#""queryMode":"fanout_merge""#);
    assert_json_contains(&annotations, r#""shardCount":3"#);
    assert_json_contains(
        &annotations,
        r#""metadataIndex":"fanout_metadata_sidecar+verify""#,
    );
    assert_json_contains(&annotations, r#""count":1"#);
    assert_json_contains(&annotations, r#""label":"best_path""#);

    let (status, datasets) = api.route_with_tenant(
        "GET",
        "/v1/dataset-associations?datasetId=cluster-regression&projectId=cluster-metadata",
        "",
        Some(tenant),
    );
    assert_eq!(status, 200, "{datasets}");
    assert_json_contains(
        &datasets,
        r#""metadataIndex":"fanout_metadata_sidecar+verify""#,
    );
    assert_json_contains(&datasets, r#""queryMode":"fanout_merge""#);
    assert_json_contains(&datasets, r#""count":1"#);
    assert_json_contains(&datasets, r#""datasetId":"cluster-regression""#);

    let (status, by_annotation) = api.route_with_tenant(
        "POST",
        "/v1/trace-search",
        r#"{"filter":{"projectId":"cluster-metadata","annotation":{"label":"best_path","source":"human","scoreMin":900}}}"#,
        Some(tenant),
    );
    assert_eq!(status, 200, "{by_annotation}");
    assert_json_contains(&by_annotation, r#""queryMode":"fanout_merge""#);
    assert_json_contains(&by_annotation, r#""total":1"#);
    assert_json_contains(&by_annotation, r#""traceId":"96001""#);
    assert!(
        !by_annotation.contains(r#""traceId":"96002""#),
        "{by_annotation}"
    );

    let (status, by_dataset) = api.route_with_tenant(
        "POST",
        "/v1/trace-search",
        r#"{"filter":{"projectId":"cluster-metadata","dataset":{"datasetId":"cluster-regression","itemId":"case-96001","scoreMin":900}}}"#,
        Some(tenant),
    );
    assert_eq!(status, 200, "{by_dataset}");
    assert_json_contains(&by_dataset, r#""total":1"#);
    assert_json_contains(&by_dataset, r#""traceId":"96001""#);

    let update_path = format!("/v1/annotations/{annotation_id}/status");
    let (status, updated) = api.route_with_tenant(
        "POST",
        &update_path,
        r#"{"status":"resolved","reviewer":"cluster-eval"}"#,
        Some(tenant),
    );
    assert_eq!(status, 200, "{updated}");
    assert_json_contains(&updated, r#""status":"resolved""#);
    assert_json_contains(&updated, r#""reviewer":"cluster-eval""#);

    let delete_path = format!("/v1/annotations/{annotation_id}");
    let (status, deleted) = api.route_with_tenant(
        "DELETE",
        &delete_path,
        r#"{"reviewer":"cluster-eval","reason":"covered"}"#,
        Some(tenant),
    );
    assert_eq!(status, 200, "{deleted}");
    assert_json_contains(&deleted, r#""status":"deleted""#);

    let (status, hidden) = api.route_with_tenant(
        "POST",
        "/v1/trace-search",
        r#"{"filter":{"projectId":"cluster-metadata","annotation":{"label":"best_path"}}}"#,
        Some(tenant),
    );
    assert_eq!(status, 200, "{hidden}");
    assert_json_contains(&hidden, r#""total":0"#);

    let (status, deleted_visible) = api.route_with_tenant(
        "GET",
        "/v1/annotations?traceId=96001&label=best_path&status=deleted",
        "",
        Some(tenant),
    );
    assert_eq!(status, 200, "{deleted_visible}");
    assert_json_contains(&deleted_visible, r#""count":1"#);

    let (status, other_tenant) = api.route_with_tenant(
        "GET",
        "/v1/dataset-associations?datasetId=cluster-regression",
        "",
        Some(tenant + 1),
    );
    assert_eq!(status, 200, "{other_tenant}");
    assert_json_contains(&other_tenant, r#""count":0"#);

    let _ = std::fs::remove_dir_all(&root);
}

/// 分布式升级第八步：Golden Path 候选资产跟 source trace owner 同 shard。
/// eval 覆盖 source/candidate 跨 shard 时的 create、fanout list、status、adherence、evidence、export 和 health。
#[test]
fn in_process_cluster_colocates_golden_path_with_source_trace_owner() {
    let root = durable_dir("cluster_golden_path_colocation");
    let tenant = 94u64;
    let shard_count = 3usize;
    let source_shard = 1usize;
    let candidate_shard = 2usize;
    let source_session = session_for_shard(tenant, source_shard, shard_count);
    let candidate_session = session_for_shard(tenant, candidate_shard, shard_count);
    let mut coords = Vec::new();
    let mut specs = Vec::new();
    for shard in 0..shard_count {
        let dir = root.join(format!("shard-{shard}"));
        let coord = WriteCoordinator::open_durable(&dir).unwrap();
        coord.recover();
        coords.push(coord.clone());
        specs.push((ShardId::new(format!("shard-{shard}")), coord));
    }
    let api = EngineJsonApi::new_in_process_cluster(specs).unwrap();
    let batch = format!(
        r#"[
          {{"trace_id":97001,"span_id":1,"session_id":{},"ts":10,"seq":1,"event_type":2,"ext_span_id":"97001-1","agent_name":"planner","status":0,"duration_ns":10,"attrs":{{"project_id":"cluster-golden","skill":"review","task_fingerprint":"cluster-golden-task","phase":"plan"}}}},
          {{"trace_id":97001,"span_id":2,"parent_span_id":1,"session_id":{},"ts":11,"seq":1,"event_type":2,"ext_span_id":"97001-2","tool_name":"runner","status":0,"duration_ns":20,"attrs":{{"project_id":"cluster-golden","skill":"review","task_fingerprint":"cluster-golden-task","phase":"verify"}}}},
          {{"trace_id":97002,"span_id":1,"session_id":{},"ts":20,"seq":1,"event_type":2,"ext_span_id":"97002-1","agent_name":"planner","status":0,"duration_ns":30,"attrs":{{"project_id":"cluster-golden","skill":"review","task_fingerprint":"cluster-golden-task","phase":"plan"}}}},
          {{"trace_id":97002,"span_id":2,"parent_span_id":1,"session_id":{},"ts":21,"seq":1,"event_type":2,"ext_span_id":"97002-2","tool_name":"runner","status":0,"duration_ns":40,"attrs":{{"project_id":"cluster-golden","skill":"review","task_fingerprint":"cluster-golden-task","phase":"verify"}}}},
          {{"trace_id":97002,"span_id":3,"parent_span_id":2,"session_id":{},"ts":22,"seq":1,"event_type":2,"ext_span_id":"97002-3","tool_name":"lint","status":0,"duration_ns":50,"attrs":{{"project_id":"cluster-golden","skill":"review","task_fingerprint":"cluster-golden-task","phase":"extra"}}}}
        ]"#,
        source_session, source_session, candidate_session, candidate_session, candidate_session
    );
    let (status, body) = api.route_with_tenant("POST", "/v1/ingest", &batch, Some(tenant));
    assert_eq!(status, 200, "{body}");
    for coord in &coords {
        coord.flush_memtable();
    }

    let (status, annotation) = api.route_with_tenant(
        "POST",
        "/v1/annotations",
        r#"{"traceId":97001,"label":"golden_source","score":990,"source":"human","projectId":"cluster-golden"}"#,
        Some(tenant),
    );
    assert_eq!(status, 200, "{annotation}");
    let (status, dataset) = api.route_with_tenant(
        "POST",
        "/v1/dataset-associations",
        r#"{"datasetId":"cluster-golden-regression","itemId":"case-97001","traceId":97001,"label":"pass","score":980,"projectId":"cluster-golden"}"#,
        Some(tenant),
    );
    assert_eq!(status, 200, "{dataset}");

    let create = r#"{"sourceTraceId":97001,"taskFingerprint":"cluster-golden-task","score":1000,"label":"cluster source","source":"eval","projectId":"cluster-golden","status":"candidate"}"#;
    let (status, golden) = api.route_with_tenant("POST", "/v1/golden-paths", create, Some(tenant));
    assert_eq!(status, 200, "{golden}");
    let golden_path_id = (((source_shard as u64) + 1) << 56) + 1;
    assert_json_contains(&golden, &format!(r#""goldenPathId":"{}""#, golden_path_id));
    assert_json_contains(&golden, r#""sourceTraceId":"97001""#);
    assert_json_contains(&golden, r#""source_trajectory_step_count":2"#);

    let gp_filter = GoldenPathFilter {
        tenant_id: Some(tenant),
        golden_path_id: Some(golden_path_id),
        ..GoldenPathFilter::default()
    };
    assert_eq!(
        coords[0].golden_paths(&gp_filter).len(),
        0,
        "golden path must not fall back to primary shard"
    );
    assert_eq!(
        coords[source_shard].golden_paths(&gp_filter).len(),
        1,
        "golden path should be co-located with source trace owner"
    );

    let (status, listed) = api.route_with_tenant(
        "GET",
        "/v1/golden-paths?taskFingerprint=cluster-golden-task&projectId=cluster-golden",
        "",
        Some(tenant),
    );
    assert_eq!(status, 200, "{listed}");
    assert_json_contains(&listed, r#""queryMode":"fanout_merge""#);
    assert_json_contains(&listed, r#""count":1"#);
    assert_json_contains(&listed, &format!(r#""goldenPathId":"{}""#, golden_path_id));

    let update_path = format!("/v1/golden-paths/{golden_path_id}/status");
    let (status, updated) = api.route_with_tenant(
        "POST",
        &update_path,
        r#"{"status":"confirmed","reason":"cluster eval accepted","source":"eval"}"#,
        Some(tenant),
    );
    assert_eq!(status, 200, "{updated}");
    assert_json_contains(&updated, r#""status":"confirmed""#);

    let (status, adherence) = api.route_with_tenant(
        "POST",
        "/v1/path-adherence",
        &format!(r#"{{"goldenPathId":"{}","traceId":97002}}"#, golden_path_id),
        Some(tenant),
    );
    assert_eq!(status, 200, "{adherence}");
    assert_json_contains(&adherence, r#""adherence":"extended""#);
    assert_json_contains(&adherence, r#""traceId":"97002""#);

    let (status, evidence) = api.route_with_tenant(
        "POST",
        "/v1/golden-path-evidence",
        &format!(
            r#"{{"goldenPathId":"{}","candidateTraceId":97002}}"#,
            golden_path_id
        ),
        Some(tenant),
    );
    assert_eq!(status, 200, "{evidence}");
    assert_json_contains(&evidence, r#""source":{"available":true"#);
    assert_json_contains(&evidence, r#""annotationCount":1"#);
    assert_json_contains(&evidence, r#""datasetAssociationCount":1"#);
    assert_json_contains(&evidence, r#""adherence":"extended""#);
    assert_json_contains(&evidence, r#""traceDiff":{"left""#);

    let (status, export_page) = api.route_with_tenant(
        "POST",
        "/v1/golden-path-export",
        r#"{"filter":{"taskFingerprint":"cluster-golden-task","projectId":"cluster-golden"},"limit":10}"#,
        Some(tenant),
    );
    assert_eq!(status, 200, "{export_page}");
    assert_json_contains(&export_page, r#""count":1"#);
    assert_json_contains(
        &export_page,
        r#""schemaVersion":"yitrace.golden_path_export.v1""#,
    );
    assert_json_contains(&export_page, r#""recordType":"golden_path""#);

    let (status, health) = api.route_with_tenant(
        "POST",
        "/v1/golden-path-health",
        &format!(
            r#"{{"goldenPathId":"{}","filter":{{"projectId":"cluster-golden"}},"limit":10,"examples":10}}"#,
            golden_path_id
        ),
        Some(tenant),
    );
    assert_eq!(status, 200, "{health}");
    assert_json_contains(&health, r#""matchingTraceTotal":1"#);
    assert_json_contains(&health, r#""analyzedTraceTotal":1"#);
    assert_json_contains(&health, r#""extended":1"#);
    assert_json_contains(&health, r#""usable":1.000000"#);

    let (status, hidden) = api.route_with_tenant(
        "GET",
        "/v1/golden-paths?taskFingerprint=cluster-golden-task",
        "",
        Some(tenant + 1),
    );
    assert_eq!(status, 200, "{hidden}");
    assert_json_contains(&hidden, r#""count":0"#);

    let _ = std::fs::remove_dir_all(&root);
}

/// 分布式升级第九步：retention 不能只在 primary shard 上 dry-run/apply。
/// eval 覆盖跨 shard 候选汇总、metadata 保护、segment-row 删除和审计 fanout。
#[test]
fn in_process_cluster_applies_retention_per_shard_and_fanout_audits() {
    let root = durable_dir("cluster_retention_fanout");
    let tenant = 95u64;
    let shard_count = 3usize;
    let sessions: Vec<u64> = (0..shard_count)
        .map(|shard| session_for_shard(tenant, shard, shard_count))
        .collect();
    let mut coords = Vec::new();
    let mut specs = Vec::new();
    for shard in 0..shard_count {
        let dir = root.join(format!("shard-{shard}"));
        let coord = WriteCoordinator::open_durable(&dir).unwrap();
        coord.recover();
        coords.push(coord.clone());
        specs.push((ShardId::new(format!("shard-{shard}")), coord));
    }
    let api = EngineJsonApi::new_in_process_cluster(specs).unwrap();
    let batch = format!(
        r#"[
          {{"trace_id":98001,"span_id":1,"session_id":{},"ts":10,"seq":1,"event_type":2,"ext_span_id":"98001-1","status":0,"duration_ns":10,"input_text":"old deletable shard 0","attrs":{{"project_id":"cluster-retention","skill":"ttl","task_fingerprint":"retention-cluster"}}}},
          {{"trace_id":98002,"span_id":1,"session_id":{},"ts":20,"seq":1,"event_type":2,"ext_span_id":"98002-1","status":0,"duration_ns":20,"input_text":"old deletable shard 1","attrs":{{"project_id":"cluster-retention","skill":"ttl","task_fingerprint":"retention-cluster"}}}},
          {{"trace_id":98003,"span_id":1,"session_id":{},"ts":30,"seq":1,"event_type":2,"ext_span_id":"98003-1","status":0,"duration_ns":30,"input_text":"old protected shard 2","attrs":{{"project_id":"cluster-retention","skill":"ttl","task_fingerprint":"retention-cluster"}}}}
        ]"#,
        sessions[0], sessions[1], sessions[2]
    );
    let (status, body) = api.route_with_tenant("POST", "/v1/ingest", &batch, Some(tenant));
    assert_eq!(status, 200, "{body}");
    for coord in &coords {
        coord.flush_memtable();
    }

    let (status, annotation) = api.route_with_tenant(
        "POST",
        "/v1/annotations",
        r#"{"traceId":98003,"label":"retain","source":"human","projectId":"cluster-retention"}"#,
        Some(tenant),
    );
    assert_eq!(status, 200, "{annotation}");

    let plan_body =
        r#"{"filter":{"projectId":"cluster-retention"},"deleteBeforeTs":100,"limit":10}"#;
    let (status, plan) =
        api.route_with_tenant("POST", "/v1/retention-plan", plan_body, Some(tenant));
    assert_eq!(status, 200, "{plan}");
    assert_json_contains(&plan, r#""queryMode":"fanout_merge""#);
    assert_json_contains(&plan, r#""shardCount":3"#);
    assert_json_contains(&plan, r#""candidates":{"traceCount":3"#);
    assert_json_contains(&plan, r#""protected":{"traceCount":1"#);
    assert_json_contains(&plan, r#""deletable":{"traceCount":2"#);
    assert_json_contains(&plan, r#""98003":["annotation"]"#);
    assert_json_contains(&plan, r#""deletableTraceIds":["98001","98002"]"#);
    assert_eq!(
        plan.matches(r#""shardId":"shard-"#).count(),
        3,
        "plan should include each shard result: {plan}"
    );

    let apply_body = r#"{"filter":{"projectId":"cluster-retention"},"deleteBeforeTs":100,"requestedBy":"cluster-retention-eval","reason":"ttl cleanup"}"#;
    let (status, applied) =
        api.route_with_tenant("POST", "/v1/retention/apply", apply_body, Some(tenant));
    assert_eq!(status, 200, "{applied}");
    assert_json_contains(&applied, r#""queryMode":"fanout_merge""#);
    assert_json_contains(&applied, r#""deletedTraceCount":2"#);
    assert_json_contains(&applied, r#""deletedTraceIds":["98001","98002"]"#);
    assert_json_contains(&applied, r#""auditId":"72057594037927937""#);
    assert_json_contains(&applied, r#""auditId":"144115188075855873""#);
    assert_json_contains(&applied, r#""auditId":"216172782113783809""#);

    let (status, after) = api.route_with_tenant(
        "POST",
        "/v1/trace-search",
        r#"{"filter":{"projectId":"cluster-retention"},"limit":10}"#,
        Some(tenant),
    );
    assert_eq!(status, 200, "{after}");
    assert_json_contains(&after, r#""total":1"#);
    assert_json_contains(&after, r#""traceId":"98003""#);
    assert!(!after.contains(r#""traceId":"98001""#), "{after}");
    assert!(!after.contains(r#""traceId":"98002""#), "{after}");

    let (status, audits) = api.route_with_tenant(
        "GET",
        "/v1/retention-audits?source=cluster-retention-eval",
        "",
        Some(tenant),
    );
    assert_eq!(status, 200, "{audits}");
    assert_json_contains(&audits, r#""queryMode":"fanout_merge""#);
    assert_json_contains(&audits, r#""shardCount":3"#);
    assert_json_contains(&audits, r#""total":3"#);
    assert_json_contains(&audits, r#""source":"cluster-retention-eval""#);

    let (status, hidden) = api.route_with_tenant(
        "GET",
        "/v1/retention-audits?source=cluster-retention-eval",
        "",
        Some(tenant + 1),
    );
    assert_eq!(status, 200, "{hidden}");
    assert_json_contains(&hidden, r#""total":0"#);

    let _ = std::fs::remove_dir_all(&root);
}

/// 分布式升级第三步：trace/session detail 必须路由到 owner shard。
/// eval 故意把数据写到非 primary shard，并用冷 owner cache 的 API 查询详情，覆盖 miss fanout 回填、
/// session turns、trace waterfall、snapshot、span page、batch detail、logEvents 和 tenant 隔离。
#[test]
fn in_process_cluster_routes_detail_apis_to_owner_shard() {
    let root = durable_dir("cluster_detail_owner");
    let tenant = 91u64;
    let shard_count = 3usize;
    let owner_shard = 2usize;
    let session = session_for_shard(tenant, owner_shard, shard_count);
    let mut coords = Vec::new();
    let mut specs = Vec::new();
    for shard in 0..shard_count {
        let dir = root.join(format!("shard-{shard}"));
        let coord = WriteCoordinator::open_durable(&dir).unwrap();
        coord.recover();
        coords.push(coord.clone());
        specs.push((ShardId::new(format!("shard-{shard}")), coord));
    }
    let api = EngineJsonApi::new_in_process_cluster(specs).unwrap();
    let batch = format!(
        r#"[
          {{"trace_id":92001,"span_id":1,"session_id":{},"ts":10,"seq":1,"event_type":1,"ext_span_id":"92001-1","agent_name":"owner-agent","input_text":"检查 owner shard 路由","attrs":{{"project_id":"cluster-detail","skill":"owner-route","task_fingerprint":"cluster-v3"}}}},
          {{"trace_id":92001,"span_id":1,"session_id":{},"ts":11,"seq":2,"event_type":4,"ext_span_id":"92001-1","logs":["open repo","inspect owner shard","route detail api"],"attrs":{{"call_site":"cluster_detail.rs:91","mode":"eval"}}}},
          {{"trace_id":92001,"span_id":1,"session_id":{},"ts":12,"seq":3,"event_type":2,"ext_span_id":"92001-1","status":0,"duration_ns":2000000,"output_text":"owner shard detail ok","input_tokens":30,"output_tokens":12}},
          {{"trace_id":92002,"span_id":1,"session_id":{},"ts":20,"seq":1,"event_type":2,"ext_span_id":"92002-1","status":0,"duration_ns":1000000,"input_text":"第二轮","output_text":"同 session 仍在 owner shard","attrs":{{"project_id":"cluster-detail","skill":"owner-route","task_fingerprint":"cluster-v3"}}}}
        ]"#,
        session, session, session, session
    );
    let (status, body) = api.route_with_tenant("POST", "/v1/ingest", &batch, Some(tenant));
    assert_eq!(status, 200, "{body}");
    for coord in &coords {
        coord.flush_memtable();
    }

    let primary_snap = coords[0].pin_snapshot();
    assert!(
        coords[0]
            .console_trace_spans_for_tenant(&primary_snap, 92001, Some(tenant))
            .is_empty(),
        "test setup must keep trace away from primary shard"
    );
    let owner_snap = coords[owner_shard].pin_snapshot();
    assert_eq!(
        coords[owner_shard]
            .console_trace_spans_for_tenant(&owner_snap, 92001, Some(tenant))
            .len(),
        1,
        "owner shard should contain the folded trace"
    );

    let cold_specs: Vec<_> = coords
        .iter()
        .enumerate()
        .map(|(shard, coord)| (ShardId::new(format!("shard-{shard}")), coord.clone()))
        .collect();
    let cold_api = EngineJsonApi::new_in_process_cluster(cold_specs).unwrap();

    let turns_path = format!("/v1/sessions/{session}/turns");
    let (status, turns) = cold_api.route_with_tenant("GET", &turns_path, "", Some(tenant));
    assert_eq!(status, 200, "{turns}");
    assert_json_contains(&turns, r#""traceId":"92001""#);
    assert_json_contains(&turns, r#""traceId":"92002""#);
    assert_json_contains(&turns, r#""sessionId":""#);
    assert_json_contains(&turns, r#""spanCount":1"#);

    let (status, trace) = cold_api.route_with_tenant("GET", "/v1/traces/92001", "", Some(tenant));
    assert_eq!(status, 200, "{trace}");
    assert_json_contains(&trace, r#""traceId":"92001""#);
    assert_json_contains(&trace, r#""name":"owner-agent""#);
    assert_json_contains(&trace, r#""logEvents":[{"#);
    assert_json_contains(
        &trace,
        r#""messages":["open repo","inspect owner shard","route detail api"]"#,
    );

    let (status, span) =
        cold_api.route_with_tenant("GET", "/v1/traces/92001/spans/1", "", Some(tenant));
    assert_eq!(status, 200, "{span}");
    assert_json_contains(&span, r#""input":"检查 owner shard 路由""#);
    assert_json_contains(&span, r#""output":"owner shard detail ok""#);
    assert_json_contains(&span, r#""call_site":"cluster_detail.rs:91""#);

    let (status, page) = cold_api.route_with_tenant(
        "GET",
        "/v1/traces/92001/spans?includeFull=true&limit=10",
        "",
        Some(tenant),
    );
    assert_eq!(status, 200, "{page}");
    assert_json_contains(&page, r#""total":1"#);
    assert_json_contains(&page, r#""inputText":{"preview":"检查 owner shard 路由""#);
    assert_json_contains(&page, r#""full":"检查 owner shard 路由""#);

    let (status, batch) = cold_api.route_with_tenant(
        "POST",
        "/v1/traces/92001/spans/batch",
        r#"{"spanIds":[1],"includeFull":true}"#,
        Some(tenant),
    );
    assert_eq!(status, 200, "{batch}");
    assert_json_contains(&batch, r#""items":[{"#);
    assert_json_contains(&batch, r#""outputText":{"preview":"owner shard detail ok""#);
    assert_json_contains(&batch, r#""full":"owner shard detail ok""#);

    let (status, snapshot) =
        cold_api.route_with_tenant("GET", "/v1/traces/92001/snapshot", "", Some(tenant));
    assert_eq!(status, 200, "{snapshot}");
    assert_json_contains(&snapshot, r#""snapshotHash":"fnv1a64:"#);
    assert_json_contains(&snapshot, r#""traceId":"92001""#);

    let (status, steps) =
        cold_api.route_with_tenant("GET", "/v1/traces/92001/steps", "", Some(tenant));
    assert_eq!(status, 200, "{steps}");
    assert_json_contains(&steps, r#""input":"检查 owner shard 路由""#);
    assert_json_contains(&steps, r#""output":"owner shard detail ok""#);

    let (status, hidden_trace) =
        cold_api.route_with_tenant("GET", "/v1/traces/92001", "", Some(tenant + 1));
    assert_eq!(status, 404, "{hidden_trace}");
    let (status, hidden_turns) =
        cold_api.route_with_tenant("GET", &turns_path, "", Some(tenant + 1));
    assert_eq!(status, 200, "{hidden_turns}");
    assert_eq!(hidden_turns, "[]");

    let _ = std::fs::remove_dir_all(&root);
}

/// 这两天新增的 cost 查询不能只覆盖显式 cost。
/// eval 场景里常见三种混用：上游直接报 cost、按 provider/model 估算、未知模型走默认估算。
#[test]
fn eval_trace_search_filters_explicit_model_and_default_cost_sources() {
    let coord = fresh();
    let api = EngineJsonApi::new(coord.clone());
    let batch = r#"[
      {
        "trace_id":301,
        "span_id":1,
        "ts":10,
        "seq":1,
        "event_type":2,
        "ext_span_id":"301-1",
        "status":0,
        "duration_ns":10,
        "tool_name":"explicit-cost",
        "input_tokens":20,
        "output_tokens":10,
        "total_tokens":30,
        "cost_usd_nanos":5000,
        "output_text":"显式 cost 通过",
        "attrs":{"project_id":"eval-cost","task_fingerprint":"cost-long-tail","validation_status":"pass","path_memory_id":"pm-explicit"}
      },
      {
        "trace_id":302,
        "span_id":1,
        "ts":20,
        "seq":1,
        "event_type":2,
        "ext_span_id":"302-1",
        "status":0,
        "duration_ns":10,
        "tool_name":"model-priced",
        "provider":"openai",
        "model":"gpt-4o-mini",
        "input_tokens":10,
        "cached_input_tokens":10,
        "output_tokens":10,
        "output_text":"模型估算 cost 通过",
        "attrs":{"project_id":"eval-cost","task_fingerprint":"cost-long-tail","validation_status":"pass","path_memory_id":"pm-model"}
      },
      {
        "trace_id":303,
        "span_id":1,
        "ts":30,
        "seq":1,
        "event_type":2,
        "ext_span_id":"303-1",
        "status":1,
        "duration_ns":10,
        "tool_name":"default-priced",
        "provider":"unknown",
        "model":"unknown-model",
        "input_tokens":10,
        "output_tokens":10,
        "output_text":"默认估算失败，无法完成",
        "attrs":{"project_id":"eval-cost","task_fingerprint":"cost-long-tail","validation_status":"fail","path_memory_id":"pm-default"}
      }
    ]"#;
    let (status, body) = api.route_with_tenant("POST", "/v1/ingest", batch, Some(9));
    assert_eq!(status, 200, "{body}");

    let scorer = KeywordScorer::new(&["无法"]);
    let scored = coord.eval_and_writeback(&scorer, &TraceQuery::all());
    assert_eq!(scored.len(), 3, "三条 eval span 都应被打分");
    assert!(scored
        .iter()
        .any(|s| s.trace_id == 303 && s.outcome.score == 0));

    let (status, explicit) = api.route_with_tenant(
        "POST",
        "/v1/trace-search",
        r#"{"filter":{"projectId":"eval-cost","minCostUsdNanos":4000,"maxCostUsdNanos":6000}}"#,
        Some(9),
    );
    assert_eq!(status, 200, "{explicit}");
    assert_json_contains(&explicit, r#""total":1"#);
    assert_json_contains(&explicit, r#""traceId":"301""#);
    assert_json_contains(&explicit, r#""costUsdNanos":5000"#);
    assert_json_contains(&explicit, r#""source":"explicit""#);
    assert_json_contains(&explicit, r#""index":"attrs_postings+folded_verify""#);

    let (status, model_priced) = api.route_with_tenant(
        "POST",
        "/v1/trace-search",
        r#"{"filter":{"projectId":"eval-cost","minCostUsd":0.000008,"maxCostUsd":0.000009,"minTokens":30,"maxTokens":30}}"#,
        Some(9),
    );
    assert_eq!(status, 200, "{model_priced}");
    assert_json_contains(&model_priced, r#""total":1"#);
    assert_json_contains(&model_priced, r#""traceId":"302""#);
    assert_json_contains(&model_priced, r#""costUsdNanos":8250"#);
    assert_json_contains(&model_priced, r#""source":"estimated_model_price""#);

    let (status, default_priced) = api.route_with_tenant(
        "POST",
        "/v1/trace-search",
        r#"{"filter":{"projectId":"eval-cost","minCostUsdNanos":47000,"maxCostUsdNanos":49000}}"#,
        Some(9),
    );
    assert_eq!(status, 200, "{default_priced}");
    assert_json_contains(&default_priced, r#""total":1"#);
    assert_json_contains(&default_priced, r#""traceId":"303""#);
    assert_json_contains(&default_priced, r#""costUsdNanos":48000"#);
    assert_json_contains(&default_priced, r#""source":"estimated_default""#);

    let (status, path_memory) = api.route_with_tenant(
        "POST",
        "/v1/trace-search",
        r#"{"filter":{"pathMemoryId":"pm-default"}}"#,
        Some(9),
    );
    assert_eq!(status, 200, "{path_memory}");
    assert_json_contains(&path_memory, r#""total":1"#);
    assert_json_contains(&path_memory, r#""traceId":"303""#);
}

/// 顶层 evalProfile 是 Golden Path 治理元数据，不应污染 attrs scope。
/// 否则后续 health 会额外带上 attrs.eval_profile 过滤，导致同 scope trace 全部被筛掉。
#[test]
fn golden_path_eval_profile_governance_does_not_pollute_scope_filter() {
    let coord = fresh();
    let api = EngineJsonApi::new(coord);
    let batch = r#"[
      {
        "trace_id":401,
        "span_id":1,
        "ts":10,
        "seq":1,
        "event_type":2,
        "ext_span_id":"401-1",
        "status":0,
        "duration_ns":10,
        "tool_name":"planner",
        "attrs":{"project_id":"eval-governance","task_fingerprint":"governance-scope","skill":"review","mode":"auto","harness_version":"h1","schema_fingerprint":"s1","phase":"plan"}
      },
      {
        "trace_id":402,
        "span_id":1,
        "ts":20,
        "seq":1,
        "event_type":2,
        "ext_span_id":"402-1",
        "status":0,
        "duration_ns":10,
        "tool_name":"planner",
        "attrs":{"project_id":"eval-governance","task_fingerprint":"governance-scope","skill":"review","mode":"auto","harness_version":"h1","schema_fingerprint":"s1","phase":"plan"}
      },
      {
        "trace_id":403,
        "span_id":1,
        "ts":30,
        "seq":1,
        "event_type":2,
        "ext_span_id":"403-1",
        "status":0,
        "duration_ns":10,
        "tool_name":"planner",
        "attrs":{"project_id":"eval-governance","task_fingerprint":"governance-scope","skill":"review","mode":"auto","harness_version":"h1","schema_fingerprint":"s1","phase":"plan"}
      },
      {
        "trace_id":403,
        "span_id":2,
        "ts":31,
        "seq":1,
        "event_type":2,
        "ext_span_id":"403-2",
        "status":0,
        "duration_ns":10,
        "tool_name":"validator",
        "attrs":{"project_id":"eval-governance","task_fingerprint":"governance-scope","skill":"review","mode":"auto","harness_version":"h1","schema_fingerprint":"s1","phase":"verify","validator":"npm test"}
      },
      {
        "trace_id":404,
        "span_id":1,
        "ts":40,
        "seq":1,
        "event_type":2,
        "ext_span_id":"404-1",
        "status":1,
        "duration_ns":10,
        "tool_name":"fallback",
        "attrs":{"project_id":"eval-governance","task_fingerprint":"governance-scope","skill":"review","mode":"auto","harness_version":"h1","schema_fingerprint":"s1","phase":"fallback"}
      }
    ]"#;
    let (status, body) = api.route_with_tenant("POST", "/v1/ingest", batch, Some(10));
    assert_eq!(status, 200, "{body}");

    let create = r#"{
      "sourceTraceId":401,
      "taskFingerprint":"governance-scope",
      "score":980,
      "evalProfile":"release-gate",
      "minSampleCount":4,
      "marginScore":900,
      "projectId":"eval-governance"
    }"#;
    let (status, golden) = api.route_with_tenant("POST", "/v1/golden-paths", create, Some(10));
    assert_eq!(status, 200, "{golden}");
    assert_json_contains(&golden, r#""evalProfile":"release-gate""#);
    assert_json_contains(&golden, r#""project_id":"eval-governance""#);
    assert_json_contains(
        &golden,
        r#""attrs":{"harness_version":"h1","mode":"auto","project_id":"eval-governance","schema_fingerprint":"s1","skill":"review","task_fingerprint":"governance-scope"}"#,
    );

    let (status, listed) = api.route_with_tenant(
        "GET",
        "/v1/golden-paths?taskFingerprint=governance-scope&evalProfile=release-gate",
        "",
        Some(10),
    );
    assert_eq!(status, 200, "{listed}");
    assert_json_contains(&listed, r#""count":1"#);

    let (status, health) = api.route_with_tenant(
        "POST",
        "/v1/golden-path-health",
        r#"{"goldenPathId":1,"filter":{"projectId":"eval-governance"},"limit":10,"examples":10}"#,
        Some(10),
    );
    assert_eq!(status, 200, "{health}");
    assert_json_contains(&health, r#""matchingTraceTotal":3"#);
    assert_json_contains(&health, r#""analyzedTraceTotal":3"#);
    assert_json_contains(&health, r#""followed":1"#);
    assert_json_contains(&health, r#""extended":1"#);
    assert_json_contains(&health, r#""deviated":1"#);
    assert_json_contains(&health, r#""usable":0.666667"#);
    assert_json_contains(&health, r#""stale":true"#);
    assert_json_contains(&health, r#""insufficient_samples""#);
    assert_json_contains(&health, r#""health_below_margin""#);
}

/// Golden Path 只保存 source trace 的引用和轻量 trajectory。
/// retention 可以清掉 source trace payload，但底座仍应能用保存的 trajectory 做 adherence/health。
#[test]
fn golden_path_adherence_survives_source_trace_retention_cleanup() {
    let dir = durable_dir("golden_path_retention");
    {
        let coord = WriteCoordinator::open_durable(&dir).unwrap();
        coord.recover();
        let api = EngineJsonApi::new(coord.clone());
        let batch = r#"[
          {
            "trace_id":501,
            "span_id":1,
            "ts":10,
            "seq":1,
            "event_type":2,
            "ext_span_id":"501-1",
            "status":0,
            "duration_ns":10,
            "tool_name":"planner",
            "attrs":{"project_id":"retained-source","task_fingerprint":"retention-golden","phase":"plan"}
          },
          {
            "trace_id":502,
            "span_id":1,
            "ts":2000,
            "seq":1,
            "event_type":2,
            "ext_span_id":"502-1",
            "status":0,
            "duration_ns":10,
            "tool_name":"planner",
            "attrs":{"project_id":"retained-source","task_fingerprint":"retention-golden","phase":"plan"}
          }
        ]"#;
        let (status, body) = api.route_with_tenant("POST", "/v1/ingest", batch, Some(11));
        assert_eq!(status, 200, "{body}");
        coord.flush_memtable();

        let (status, golden) = api.route_with_tenant(
            "POST",
            "/v1/golden-paths",
            r#"{"sourceTraceId":501,"taskFingerprint":"retention-golden","score":1000,"projectId":"retained-source","status":"confirmed"}"#,
            Some(11),
        );
        assert_eq!(status, 200, "{golden}");
        assert_json_contains(&golden, r#""source_trajectory_step_count":1"#);

        let apply = r#"{
          "filter":{"projectId":"retained-source"},
          "deleteBeforeTs":100,
          "protect":{"goldenPaths":false,"annotations":false,"datasetAssociations":false,"snapshots":false,"evalLinks":false,"pathMemory":false},
          "requestedBy":"eval-retention-test"
        }"#;
        let (status, applied) =
            api.route_with_tenant("POST", "/v1/retention/apply", apply, Some(11));
        assert_eq!(status, 200, "{applied}");
        assert_json_contains(&applied, r#""deletedTraceIds":["501"]"#);

        let (status, search) = api.route_with_tenant(
            "POST",
            "/v1/trace-search",
            r#"{"filter":{"projectId":"retained-source"}}"#,
            Some(11),
        );
        assert_eq!(status, 200, "{search}");
        assert_json_contains(&search, r#""total":1"#);
        assert!(!search.contains(r#""traceId":"501""#), "{search}");
        assert_json_contains(&search, r#""traceId":"502""#);

        let (status, adherence) = api.route_with_tenant(
            "POST",
            "/v1/path-adherence",
            r#"{"goldenPathId":1,"traceId":502}"#,
            Some(11),
        );
        assert_eq!(status, 200, "{adherence}");
        assert_json_contains(&adherence, r#""adherence":"followed""#);
        assert_json_contains(&adherence, r#""sourceAvailable":false"#);
        assert_json_contains(&adherence, r#""sourceRetained":true"#);
        assert_json_contains(&adherence, r#""sameSignature":true"#);
        assert_json_contains(&adherence, r#""storedSignatureMatchesSource":true"#);

        let (status, evidence) = api.route_with_tenant(
            "POST",
            "/v1/golden-path-evidence",
            r#"{"goldenPathId":1,"candidateTraceId":502}"#,
            Some(11),
        );
        assert_eq!(status, 200, "{evidence}");
        assert_json_contains(&evidence, r#""source":{"available":false"#);
        assert_json_contains(&evidence, r#""pathAdherence":{"goldenPath""#);
        assert_json_contains(&evidence, r#""traceDiff":null"#);

        let (status, health) = api.route_with_tenant(
            "POST",
            "/v1/golden-path-health",
            r#"{"goldenPathId":1,"filter":{"projectId":"retained-source"},"limit":10}"#,
            Some(11),
        );
        assert_eq!(status, 200, "{health}");
        assert_json_contains(&health, r#""sourceAvailable":false"#);
        assert_json_contains(&health, r#""sourceRetained":true"#);
        assert_json_contains(&health, r#""matchingTraceTotal":1"#);
        assert_json_contains(&health, r#""followed":1"#);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// span detail 应直接返回 LOG events；eval 写回之后也不能要求业务把日志镜像进 attrs。
#[test]
fn span_detail_log_events_round_trip_after_eval_writeback() {
    let coord = fresh();
    let api = EngineJsonApi::new(coord.clone());
    let batch = r#"[
      {
        "trace_id":601,
        "span_id":1,
        "ts":10,
        "seq":1,
        "event_type":1,
        "ext_span_id":"601-1",
        "input_text":"检查打包失败",
        "attrs":{"project_id":"log-events","task_fingerprint":"log-round-trip"}
      },
      {
        "trace_id":601,
        "span_id":1,
        "ts":11,
        "seq":2,
        "event_type":4,
        "ext_span_id":"601-1",
        "logs":["open repo","read package.json","发现 native binding 缺失"],
        "attrs":{"call_site":"pack.js:42","attempt":2,"nested":{"ok":true}}
      },
      {
        "trace_id":601,
        "span_id":1,
        "ts":12,
        "seq":3,
        "event_type":2,
        "ext_span_id":"601-1",
        "status":1,
        "duration_ns":2000,
        "output_text":"无法加载 darwin-arm64 node binding"
      }
    ]"#;
    let (status, body) = api.route_with_tenant("POST", "/v1/ingest", batch, Some(12));
    assert_eq!(status, 200, "{body}");

    let scorer = KeywordScorer::new(&["无法"]);
    let scored = coord.eval_and_writeback(&scorer, &TraceQuery::all());
    assert_eq!(scored.len(), 1);
    assert_eq!(scored[0].outcome.score, 0);

    let (status, detail) = api.route_with_tenant("GET", "/v1/traces/601/spans/1", "", Some(12));
    assert_eq!(status, 200, "{detail}");
    assert_json_contains(&detail, r#""logEvents":[{"#);
    assert_json_contains(&detail, r#""eventOrdinal":0"#);
    assert_json_contains(&detail, r#""eventType":4"#);
    assert_json_contains(
        &detail,
        r#""messages":["open repo","read package.json","发现 native binding 缺失"]"#,
    );
    assert_json_contains(&detail, r#""call_site":"pack.js:42""#);
    assert_json_contains(&detail, r#""attempt":2"#);
    assert_json_contains(&detail, r#""nested":{"ok":true}"#);
}

/// 后验 annotation / dataset association 是 eval 和 golden path 的证据层。
/// 它们要能反向过滤 trace/session，并在 retention 中保护仍有效的证据引用。
#[test]
fn eval_metadata_lifecycle_reverse_filters_and_retention_guards() {
    let dir = durable_dir("metadata_eval");
    {
        let coord = WriteCoordinator::open_durable(&dir).unwrap();
        coord.recover();
        let api = EngineJsonApi::new(coord.clone());
        let batch = r#"[
          {
            "trace_id":"meta-run-1",
            "span_id":"meta-span-1",
            "session_id":"meta-session",
            "ts":10,
            "seq":1,
            "event_type":2,
            "ext_span_id":"meta-span-1",
            "status":0,
            "duration_ns":10,
            "agent_name":"eval-agent",
            "output_text":"验证通过，可以沉淀为回归样本",
            "attrs":{"project_id":"metadata-eval","skill":"review","task_fingerprint":"metadata-task"}
          },
          {
            "trace_id":"meta-run-2",
            "span_id":"meta-span-2",
            "session_id":"meta-session",
            "ts":20,
            "seq":1,
            "event_type":2,
            "ext_span_id":"meta-span-2",
            "status":1,
            "duration_ns":10,
            "agent_name":"eval-agent",
            "output_text":"无法完成验证",
            "attrs":{"project_id":"metadata-eval","skill":"review","task_fingerprint":"metadata-task"}
          },
          {
            "trace_id":"meta-run-3",
            "span_id":"meta-span-3",
            "session_id":"meta-session",
            "ts":300,
            "seq":1,
            "event_type":2,
            "ext_span_id":"meta-span-3",
            "status":0,
            "duration_ns":10,
            "agent_name":"eval-agent",
            "output_text":"新 trace 不应被旧 TTL 清理",
            "attrs":{"project_id":"metadata-eval","skill":"review","task_fingerprint":"metadata-task"}
          }
        ]"#;
        let (status, body) = api.route_with_tenant("POST", "/v1/ingest", batch, Some(13));
        assert_eq!(status, 200, "{body}");

        let scorer = KeywordScorer::new(&["无法"]);
        let scored = coord.eval_and_writeback(&scorer, &TraceQuery::all());
        assert!(scored
            .iter()
            .any(|s| s.trace_id != 0 && s.outcome.score == 0));
        coord.flush_memtable();

        let (status, annotation) = api.route_with_tenant(
            "POST",
            "/v1/annotations",
            r#"{"traceId":"meta-run-1","spanId":"meta-span-1","target":"span","label":"best_path","score":950,"source":"human","projectId":"metadata-eval","skill":"review","attrs":{"evidence_tags":["golden","stable"],"review":{"owner":"qa"}}}"#,
            Some(13),
        );
        assert_eq!(status, 200, "{annotation}");
        assert_json_contains(&annotation, r#""annotationId":"1""#);
        assert_json_contains(&annotation, r#""externalTraceId":"meta-run-1""#);
        let (status, annotation_by_array_attr) = api.route_with_tenant(
            "GET",
            "/v1/annotations?attrs.evidence_tags=golden",
            "",
            Some(13),
        );
        assert_eq!(status, 200, "{annotation_by_array_attr}");
        assert_json_contains(
            &annotation_by_array_attr,
            r#""metadataIndex":"metadata_sidecar+verify""#,
        );
        assert_json_contains(&annotation_by_array_attr, r#""count":1"#);
        assert_json_contains(&annotation_by_array_attr, r#""label":"best_path""#);

        let (status, dataset) = api.route_with_tenant(
            "POST",
            "/v1/dataset-associations",
            r#"{"datasetId":"eval-regression","itemId":"case-1","traceId":"meta-run-1","spanId":"meta-span-1","snapshotId":"snap-1","snapshotHash":"fnv1a64:meta","evalRunId":"eval-1","label":"pass","score":940,"projectId":"metadata-eval","attrs":{"evidence_tags":["regression","stable"]}}"#,
            Some(13),
        );
        assert_eq!(status, 200, "{dataset}");
        assert_json_contains(&dataset, r#""datasetId":"eval-regression""#);
        let (status, dataset_by_array_attr) = api.route_with_tenant(
            "GET",
            "/v1/dataset-associations?attrs.evidence_tags=regression",
            "",
            Some(13),
        );
        assert_eq!(status, 200, "{dataset_by_array_attr}");
        assert_json_contains(
            &dataset_by_array_attr,
            r#""metadataIndex":"metadata_sidecar+verify""#,
        );
        assert_json_contains(&dataset_by_array_attr, r#""count":1"#);
        assert_json_contains(&dataset_by_array_attr, r#""datasetId":"eval-regression""#);

        let (status, bad_annotation) = api.route_with_tenant(
            "POST",
            "/v1/annotations",
            r#"{"traceId":"meta-run-2","spanId":"meta-span-2","target":"span","label":"bad_answer","score":100,"source":"eval","projectId":"metadata-eval"}"#,
            Some(13),
        );
        assert_eq!(status, 200, "{bad_annotation}");
        assert_json_contains(&bad_annotation, r#""annotationId":"2""#);

        let (status, deleted) = api.route_with_tenant(
            "DELETE",
            "/v1/annotations/2",
            r#"{"reviewer":"four","reason":"superseded by fixed run"}"#,
            Some(13),
        );
        assert_eq!(status, 200, "{deleted}");
        assert_json_contains(&deleted, r#""status":"deleted""#);

        let (status, hidden_annotation) = api.route_with_tenant(
            "GET",
            "/v1/annotations?traceId=meta-run-2&label=bad_answer",
            "",
            Some(13),
        );
        assert_eq!(status, 200, "{hidden_annotation}");
        assert_json_contains(
            &hidden_annotation,
            r#""metadataIndex":"metadata_sidecar+verify""#,
        );
        assert_json_contains(&hidden_annotation, r#""count":0"#);

        let (status, deleted_annotation) = api.route_with_tenant(
            "GET",
            "/v1/annotations?traceId=meta-run-2&label=bad_answer&status=deleted",
            "",
            Some(13),
        );
        assert_eq!(status, 200, "{deleted_annotation}");
        assert_json_contains(
            &deleted_annotation,
            r#""metadataIndex":"metadata_sidecar+verify""#,
        );
        assert_json_contains(&deleted_annotation, r#""count":1"#);
        assert_json_contains(&deleted_annotation, r#""reviewer":"four""#);

        let (status, by_annotation) = api.route_with_tenant(
            "POST",
            "/v1/trace-search",
            r#"{"filter":{"annotation":{"label":"best_path","source":"human","scoreMin":900}}}"#,
            Some(13),
        );
        assert_eq!(status, 200, "{by_annotation}");
        assert_json_contains(&by_annotation, r#""total":1"#);
        assert_json_contains(&by_annotation, r#""externalSpanId":"meta-span-1""#);
        assert_json_contains(&by_annotation, r#""index":"metadata_filter+folded_scan""#);

        let (status, by_deleted_annotation) = api.route_with_tenant(
            "POST",
            "/v1/trace-search",
            r#"{"filter":{"annotation":{"label":"bad_answer"}}}"#,
            Some(13),
        );
        assert_eq!(status, 200, "{by_deleted_annotation}");
        assert_json_contains(&by_deleted_annotation, r#""total":0"#);

        let (status, by_dataset) = api.route_with_tenant(
            "GET",
            "/v1/sessions?datasetId=eval-regression&datasetLabel=pass",
            "",
            Some(13),
        );
        assert_eq!(status, 200, "{by_dataset}");
        assert_json_contains(&by_dataset, r#""externalSessionId":"meta-session""#);

        let (status, traces) = api.route_with_tenant(
            "GET",
            "/v1/traces?annotationLabel=best_path&annotationScoreMin=900",
            "",
            Some(13),
        );
        assert_eq!(status, 200, "{traces}");
        assert_json_contains(&traces, r#""external_trace_id":"meta-run-1""#);

        let retention_query = r#"{"filter":{"projectId":"metadata-eval"},"deleteBeforeTs":100}"#;
        let (status, plan) =
            api.route_with_tenant("POST", "/v1/retention-plan", retention_query, Some(13));
        assert_eq!(status, 200, "{plan}");
        assert_json_contains(&plan, r#""candidates":{"traceCount":2"#);
        assert_json_contains(&plan, r#""protected":{"traceCount":1"#);
        assert_json_contains(&plan, r#""deletable":{"traceCount":1"#);
        assert_json_contains(&plan, r#""annotation""#);
        assert_json_contains(&plan, r#""datasetAssociation""#);

        let apply = r#"{"filter":{"projectId":"metadata-eval"},"deleteBeforeTs":100,"compact":true,"requestedBy":"eval-metadata-retention","reason":"ttl cleanup"}"#;
        let (status, applied) =
            api.route_with_tenant("POST", "/v1/retention/apply", apply, Some(13));
        assert_eq!(status, 200, "{applied}");
        assert_json_contains(&applied, r#""deletedTraceCount":1"#);
        assert_json_contains(&applied, r#""source":"eval-metadata-retention""#);

        let (status, after) = api.route_with_tenant(
            "POST",
            "/v1/trace-search",
            r#"{"filter":{"projectId":"metadata-eval"}}"#,
            Some(13),
        );
        assert_eq!(status, 200, "{after}");
        assert_json_contains(&after, r#""total":2"#);
        assert_json_contains(&after, r#""externalTraceId":"meta-run-1""#);
        assert_json_contains(&after, r#""externalTraceId":"meta-run-3""#);
        assert!(
            !after.contains(r#""externalTraceId":"meta-run-2""#),
            "{after}"
        );
    }
    {
        let coord = WriteCoordinator::open_durable(&dir).unwrap();
        coord.recover();
        let api = EngineJsonApi::new(coord);
        let (status, audits) = api.route_with_tenant(
            "GET",
            "/v1/retention-audits?source=eval-metadata-retention",
            "",
            Some(13),
        );
        assert_eq!(status, 200, "{audits}");
        assert_json_contains(&audits, r#""total":1"#);
        assert_json_contains(&audits, r#""deletedTraceCount":1"#);

        let (status, dataset) = api.route_with_tenant(
            "GET",
            "/v1/dataset-associations?datasetId=eval-regression&itemId=case-1",
            "",
            Some(13),
        );
        assert_eq!(status, 200, "{dataset}");
        assert_json_contains(&dataset, r#""metadataIndex":"metadata_sidecar+verify""#);
        assert_json_contains(&dataset, r#""count":1"#);
        assert_json_contains(&dataset, r#""snapshotHash":"fnv1a64:meta""#);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Trace Inbox 需要区分“用户输入里提到 SQL”和“工具日志里报 SQL 错”。
/// 分域全文索引必须比 all-text BM25 更窄，并且能从 segment+WAL 重建。
#[test]
fn full_text_domain_index_filters_input_output_logs_and_recovers() {
    let dir = durable_dir("text_domain_index");
    {
        let coord = WriteCoordinator::open_durable(&dir).unwrap();
        coord.recover();
        let api = EngineJsonApi::new(coord.clone());
        let batch = r#"[
          {"trace_id":97101,"span_id":1,"session_id":971,"ts":10,"seq":1,"event_type":2,"ext_span_id":"97101-1","status":0,"input_text":"SELECT password FROM users where tenant_id = 1","output_text":"正常完成","attrs":{"project_id":"text-domain","skill":"search","mode":"input"}},
          {"trace_id":97102,"span_id":1,"session_id":971,"ts":20,"seq":1,"event_type":2,"ext_span_id":"97102-1","status":0,"input_text":"普通问题","output_text":"SELECT password FROM users 被策略拦截","attrs":{"project_id":"text-domain","skill":"search","mode":"output"}},
          {"trace_id":97103,"span_id":1,"session_id":971,"ts":30,"seq":1,"event_type":2,"ext_span_id":"97103-1","status":1,"input_text":"普通问题","output_text":"失败","logs":["database deadlock retry exhausted"],"attrs":{"project_id":"text-domain","skill":"search","mode":"logs"}},
          {"trace_id":97104,"span_id":1,"session_id":971,"ts":40,"seq":1,"event_type":2,"ext_span_id":"97104-1","status":0,"input_text":"SELECT password FROM users other project","attrs":{"project_id":"other","skill":"search","mode":"input"}}
        ]"#;
        let (status, body) = api.route_with_tenant("POST", "/v1/ingest", batch, Some(21));
        assert_eq!(status, 200, "{body}");
        coord.flush_memtable();

        let (status, input_hits) = api.route_with_tenant(
            "POST",
            "/v1/search",
            r#"{"text":"password users","textDomains":["input_text"],"k":10,"filter":{"attrs":{"project_id":"text-domain"}}}"#,
            Some(21),
        );
        assert_eq!(status, 200, "{input_hits}");
        assert_json_contains(&input_hits, r#""searchIndex":"text_domain_bm25""#);
        assert_json_contains(&input_hits, r#""textDomains":["input_text"]"#);
        assert_json_contains(&input_hits, r#""trace_id":97101"#);
        assert!(
            !input_hits.contains(r#""trace_id":97102"#)
                && !input_hits.contains(r#""trace_id":97104"#),
            "{input_hits}"
        );

        let (status, output_hits) = api.route_with_tenant(
            "POST",
            "/v1/search",
            r#"{"outputContains":"password users","k":10,"filter":{"attrs":{"project_id":"text-domain"}}}"#,
            Some(21),
        );
        assert_eq!(status, 200, "{output_hits}");
        assert_json_contains(&output_hits, r#""textDomains":["output_text"]"#);
        assert_json_contains(&output_hits, r#""trace_id":97102"#);
        assert!(
            !output_hits.contains(r#""trace_id":97101"#),
            "{output_hits}"
        );

        let (status, log_hits) = api.route_with_tenant(
            "POST",
            "/v1/search",
            r#"{"logContains":"deadlock","k":10,"filter":{"attrs":{"project_id":"text-domain"}}}"#,
            Some(21),
        );
        assert_eq!(status, 200, "{log_hits}");
        assert_json_contains(&log_hits, r#""textDomains":["logs"]"#);
        assert_json_contains(&log_hits, r#""trace_id":97103"#);
    }
    {
        let coord = WriteCoordinator::open_durable(&dir).unwrap();
        coord.recover();
        let api = EngineJsonApi::new(coord);
        let (status, output_hits) = api.route_with_tenant(
            "POST",
            "/v1/search",
            r#"{"text":"password users","textDomains":["output_text"],"k":10,"filter":{"attrs":{"project_id":"text-domain"}}}"#,
            Some(21),
        );
        assert_eq!(status, 200, "{output_hits}");
        assert_json_contains(&output_hits, r#""searchIndex":"text_domain_bm25""#);
        assert_json_contains(&output_hits, r#""trace_id":97102"#);
        assert!(
            !output_hits.contains(r#""trace_id":97101"#),
            "{output_hits}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Agent Memory 后续需要“相似任务/相似路径”召回，但 embedding 由外部系统提供。
/// yiTrace 只做 namespace-aware 的向量存储、过滤和恢复，不在 engine 内调用模型。
#[test]
fn vector_namespace_index_filters_task_trajectory_and_recovers() {
    let dir = durable_dir("vector_namespace_index");
    {
        let coord = WriteCoordinator::open_durable(&dir).unwrap();
        coord.recover();
        let api = EngineJsonApi::new(coord);

        for body in [
            r#"{"namespace":"task","key":"native-pack","vector":[0.0,0.0],"attrs":{"project_id":"vector-eval","skill":"packaging","mode":"auto"}}"#,
            r#"{"namespace":"task","key":"billing-fix","vector":[5.0,5.0],"attrs":{"project_id":"vector-eval","skill":"billing","mode":"auto"}}"#,
            r#"{"namespace":"task","key":"other-project-near","vector":[0.05,0.05],"attrs":{"project_id":"other","skill":"packaging","mode":"auto"}}"#,
            r#"{"namespace":"trajectory","key":"plan>build>test","vector":[0.2,0.1],"traceId":"traj-source","attrs":{"project_id":"vector-eval","task_fingerprint":"native-pack"}}"#,
        ] {
            let (status, indexed) =
                api.route_with_tenant("POST", "/v1/vector-index", body, Some(31));
            assert_eq!(status, 200, "{indexed}");
            assert_json_contains(&indexed, r#""vectorIndex":"vector_namespace_flat""#);
        }

        let (status, task_hits) = api.route_with_tenant(
            "POST",
            "/v1/vector-search",
            r#"{"namespace":"task","vector":[0.1,0.1],"k":5,"filter":{"attrs":{"project_id":"vector-eval"}}}"#,
            Some(31),
        );
        assert_eq!(status, 200, "{task_hits}");
        assert_json_contains(&task_hits, r#""vectorIndex":"vector_namespace_flat""#);
        assert_json_contains(&task_hits, r#""key":"native-pack""#);
        assert!(!task_hits.contains("other-project-near"), "{task_hits}");

        let (status, trajectory_hits) = api.route_with_tenant(
            "POST",
            "/v1/vector-search",
            r#"{"namespace":"trajectory","vector":[0.2,0.1],"k":5,"filter":{"attrs":{"task_fingerprint":"native-pack"}}}"#,
            Some(31),
        );
        assert_eq!(status, 200, "{trajectory_hits}");
        assert_json_contains(&trajectory_hits, r#""namespace":"trajectory""#);
        assert_json_contains(&trajectory_hits, r#""key":"plan>build>test""#);
    }
    {
        let coord = WriteCoordinator::open_durable(&dir).unwrap();
        coord.recover();
        let api = EngineJsonApi::new(coord);
        let (status, task_hits) = api.route_with_tenant(
            "POST",
            "/v1/vector-search",
            r#"{"namespace":"task","vector":[0.1,0.1],"k":5,"filter":{"attrs":{"project_id":"vector-eval","skill":"packaging"}}}"#,
            Some(31),
        );
        assert_eq!(status, 200, "{task_hits}");
        assert_json_contains(&task_hits, r#""key":"native-pack""#);
        assert!(!task_hits.contains("billing-fix"), "{task_hits}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// loop/task/trajectory 三组 read model 都应该复用同一套高频字段、metadata 和 attrs 过滤语义。
#[test]
fn eval_loop_task_and_trajectory_read_models_share_filters_and_scores() {
    let coord = fresh();
    let api = EngineJsonApi::new(coord.clone());
    let batch = r#"[
      {
        "trace_id":801,
        "span_id":1,
        "ts":10,
        "seq":1,
        "event_type":2,
        "ext_span_id":"801-1",
        "session_id":8801,
        "status":0,
        "duration_ns":10,
        "tool_name":"planner",
        "input_tokens":10,
        "output_tokens":5,
        "total_tokens":15,
        "cost_usd_nanos":1000,
        "output_text":"规划完成",
        "attrs":{"project_id":"loop-eval","skill":"packaging","mode":"auto","task_fingerprint":"native-pack","loop_id":"loop-native","harness_version":"h2","validation_status":"pass","phase":"plan"}
      },
      {
        "trace_id":801,
        "span_id":2,
        "ts":20,
        "seq":1,
        "event_type":2,
        "ext_span_id":"801-2",
        "session_id":8801,
        "status":0,
        "duration_ns":20,
        "tool_name":"tester",
        "input_tokens":20,
        "output_tokens":10,
        "total_tokens":30,
        "cost_usd_nanos":2000,
        "output_text":"npm test passed",
        "attrs":{"project_id":"loop-eval","skill":"packaging","mode":"auto","task_fingerprint":"native-pack","loop_id":"loop-native","harness_version":"h2","validation_status":"pass","phase":"verify","validator":"npm test"}
      },
      {
        "trace_id":802,
        "span_id":1,
        "ts":30,
        "seq":1,
        "event_type":2,
        "ext_span_id":"802-1",
        "session_id":8802,
        "status":0,
        "duration_ns":10,
        "tool_name":"planner",
        "input_tokens":15,
        "output_tokens":5,
        "total_tokens":20,
        "cost_usd_nanos":1500,
        "output_text":"规划完成",
        "attrs":{"project_id":"loop-eval","skill":"packaging","mode":"auto","task_fingerprint":"native-pack","loop_id":"loop-native","harness_version":"h2","validation_status":"fail","phase":"plan"}
      },
      {
        "trace_id":802,
        "span_id":2,
        "ts":40,
        "seq":1,
        "event_type":2,
        "ext_span_id":"802-2",
        "session_id":8802,
        "status":1,
        "duration_ns":30,
        "tool_name":"fallback",
        "input_tokens":30,
        "output_tokens":10,
        "total_tokens":40,
        "cost_usd_nanos":3000,
        "output_text":"无法找到 native binding",
        "attrs":{"project_id":"loop-eval","skill":"packaging","mode":"auto","task_fingerprint":"native-pack","loop_id":"loop-native","harness_version":"h2","validation_status":"fail","stop_reason":"error","phase":"fallback"}
      },
      {
        "trace_id":803,
        "span_id":1,
        "ts":50,
        "seq":1,
        "event_type":2,
        "ext_span_id":"803-1",
        "session_id":8803,
        "status":0,
        "duration_ns":10,
        "tool_name":"planner",
        "attrs":{"project_id":"loop-eval","skill":"review","mode":"manual","task_fingerprint":"other-task","loop_id":"loop-other","validation_status":"pass","phase":"plan"}
      }
    ]"#;
    let (status, body) = api.route_with_tenant("POST", "/v1/ingest", batch, Some(14));
    assert_eq!(status, 200, "{body}");

    let scorer = KeywordScorer::new(&["无法"]);
    let scored = coord.eval_and_writeback(&scorer, &TraceQuery::all());
    assert!(scored
        .iter()
        .any(|s| s.trace_id == 802 && s.outcome.score == 0));

    let (status, annotation) = api.route_with_tenant(
        "POST",
        "/v1/annotations",
        r#"{"traceId":801,"label":"best_path","score":960,"source":"human","projectId":"loop-eval"}"#,
        Some(14),
    );
    assert_eq!(status, 200, "{annotation}");
    let (status, dataset) = api.route_with_tenant(
        "POST",
        "/v1/dataset-associations",
        r#"{"datasetId":"loop-regression","itemId":"native-pack-pass","traceId":801,"label":"pass","score":940,"projectId":"loop-eval"}"#,
        Some(14),
    );
    assert_eq!(status, 200, "{dataset}");

    let (status, loops) = api.route_with_tenant(
        "GET",
        "/v1/loops?taskFingerprint=native-pack&projectId=loop-eval",
        "",
        Some(14),
    );
    assert_eq!(status, 200, "{loops}");
    assert_json_contains(&loops, r#""total":1"#);
    assert_json_contains(&loops, r#""loopId":"loop-native""#);
    assert_json_contains(&loops, r#""traceCount":2"#);
    assert_json_contains(&loops, r#""errorCount":1"#);
    assert_json_contains(&loops, r#""phases":["fallback","plan","verify"]"#);

    let (status, loop_detail) = api.route_with_tenant("GET", "/v1/loops/loop-native", "", Some(14));
    assert_eq!(status, 200, "{loop_detail}");
    assert_json_contains(&loop_detail, r#""summary":{"loopId":"loop-native""#);
    assert_json_contains(&loop_detail, r#""traceId":"801""#);
    assert_json_contains(&loop_detail, r#""traceId":"802""#);

    let (status, task_traces) = api.route_with_tenant(
        "GET",
        "/v1/tasks/native-pack/traces?validationStatus=pass&projectId=loop-eval",
        "",
        Some(14),
    );
    assert_eq!(status, 200, "{task_traces}");
    assert_json_contains(&task_traces, r#""total":1"#);
    assert_json_contains(&task_traces, r#""traceId":"801""#);
    assert!(!task_traces.contains(r#""traceId":"802""#), "{task_traces}");

    let (status, trajectories) = api.route_with_tenant(
        "POST",
        "/v1/trace-trajectories",
        r#"{"filter":{"taskFingerprint":"native-pack","projectId":"loop-eval"},"limit":10}"#,
        Some(14),
    );
    assert_eq!(status, 200, "{trajectories}");
    assert_json_contains(&trajectories, r#""total":2"#);
    assert_json_contains(&trajectories, r#""spanTotal":4"#);
    assert_json_contains(&trajectories, r#""index":"materialized""#);
    assert_json_contains(&trajectories, r#""trajectory":{"signature":"fnv1a64:"#);

    let (status, groups) = api.route_with_tenant(
        "POST",
        "/v1/trajectory-groups",
        r#"{"filter":{"taskFingerprint":"native-pack","projectId":"loop-eval"},"sort":"best","limit":10}"#,
        Some(14),
    );
    assert_eq!(status, 200, "{groups}");
    assert_json_contains(&groups, r#""total":2"#);
    assert_json_contains(&groups, r#""traceTotal":2"#);
    assert_json_contains(&groups, r#""spanTotal":4"#);
    assert_json_contains(&groups, r#""index":"attrs_postings+folded_verify""#);
    assert_json_contains(
        &groups,
        r#""trajectoryIndex":"materialized_trajectory_cache""#,
    );
    assert_json_contains(&groups, r#""annotation":{"count":1,"avg":960"#);
    assert_json_contains(&groups, r#""dataset":{"count":1,"avg":940"#);
}

/// Golden Path 需要覆盖完整候选资产生命周期：确认、challenger、evidence、export 和 health。
#[test]
fn eval_golden_path_full_lifecycle_exports_evidence_and_challenger_health() {
    let coord = fresh();
    let api = EngineJsonApi::new(coord);
    let batch = r#"[
      {
        "trace_id":901,
        "span_id":1,
        "ts":10,
        "seq":1,
        "event_type":2,
        "ext_span_id":"901-1",
        "status":0,
        "duration_ns":10,
        "tool_name":"planner",
        "model":"qwen",
        "provider":"openai",
        "attrs":{"project_id":"golden-eval","skill":"review","mode":"auto","task_fingerprint":"golden-task","harness_version":"h3","schema_fingerprint":"s3","phase":"plan"}
      },
      {
        "trace_id":901,
        "span_id":2,
        "ts":20,
        "seq":1,
        "event_type":2,
        "ext_span_id":"901-2",
        "status":0,
        "duration_ns":20,
        "tool_name":"tester",
        "model":"qwen",
        "provider":"openai",
        "attrs":{"project_id":"golden-eval","skill":"review","mode":"auto","task_fingerprint":"golden-task","harness_version":"h3","schema_fingerprint":"s3","phase":"verify","validator":"npm test"}
      },
      {
        "trace_id":902,
        "span_id":1,
        "ts":30,
        "seq":1,
        "event_type":2,
        "ext_span_id":"902-1",
        "status":0,
        "duration_ns":10,
        "tool_name":"planner",
        "model":"qwen",
        "provider":"openai",
        "attrs":{"project_id":"golden-eval","skill":"review","mode":"auto","task_fingerprint":"golden-task","harness_version":"h3","schema_fingerprint":"s3","phase":"plan"}
      },
      {
        "trace_id":902,
        "span_id":2,
        "ts":40,
        "seq":1,
        "event_type":2,
        "ext_span_id":"902-2",
        "status":0,
        "duration_ns":20,
        "tool_name":"tester",
        "model":"qwen",
        "provider":"openai",
        "attrs":{"project_id":"golden-eval","skill":"review","mode":"auto","task_fingerprint":"golden-task","harness_version":"h3","schema_fingerprint":"s3","phase":"verify","validator":"npm test"}
      },
      {
        "trace_id":903,
        "span_id":1,
        "ts":50,
        "seq":1,
        "event_type":2,
        "ext_span_id":"903-1",
        "status":0,
        "duration_ns":10,
        "tool_name":"planner",
        "model":"qwen",
        "provider":"openai",
        "attrs":{"project_id":"golden-eval","skill":"review","mode":"auto","task_fingerprint":"golden-task","harness_version":"h3","schema_fingerprint":"s3","phase":"plan"}
      },
      {
        "trace_id":903,
        "span_id":2,
        "ts":60,
        "seq":1,
        "event_type":2,
        "ext_span_id":"903-2",
        "status":0,
        "duration_ns":20,
        "tool_name":"tester",
        "model":"qwen",
        "provider":"openai",
        "attrs":{"project_id":"golden-eval","skill":"review","mode":"auto","task_fingerprint":"golden-task","harness_version":"h3","schema_fingerprint":"s3","phase":"verify","validator":"npm test"}
      },
      {
        "trace_id":903,
        "span_id":3,
        "ts":70,
        "seq":1,
        "event_type":2,
        "ext_span_id":"903-3",
        "status":0,
        "duration_ns":15,
        "tool_name":"exporter",
        "model":"qwen",
        "provider":"openai",
        "attrs":{"project_id":"golden-eval","skill":"review","mode":"auto","task_fingerprint":"golden-task","harness_version":"h3","schema_fingerprint":"s3","phase":"export"}
      }
    ]"#;
    let (status, body) = api.route_with_tenant("POST", "/v1/ingest", batch, Some(15));
    assert_eq!(status, 200, "{body}");

    let (status, annotation) = api.route_with_tenant(
        "POST",
        "/v1/annotations",
        r#"{"traceId":901,"label":"golden_source","score":980,"source":"human","projectId":"golden-eval"}"#,
        Some(15),
    );
    assert_eq!(status, 200, "{annotation}");
    let (status, dataset) = api.route_with_tenant(
        "POST",
        "/v1/dataset-associations",
        r#"{"datasetId":"golden-regression","itemId":"golden-case","traceId":901,"label":"pass","score":970,"projectId":"golden-eval"}"#,
        Some(15),
    );
    assert_eq!(status, 200, "{dataset}");

    let create = r#"{
      "sourceTraceId":901,
      "taskFingerprint":"golden-task",
      "score":980,
      "label":"stable path",
      "source":"eval_harness",
      "evalProfile":"release-gate",
      "minSampleCount":2,
      "marginScore":900,
      "comparisonWindowNs":1000,
      "projectId":"golden-eval"
    }"#;
    let (status, golden) = api.route_with_tenant("POST", "/v1/golden-paths", create, Some(15));
    assert_eq!(status, 200, "{golden}");
    assert_json_contains(&golden, r#""goldenPathId":"1""#);
    assert_json_contains(&golden, r#""sourceTrajectory":{"signature":"fnv1a64:"#);
    assert_json_contains(&golden, r#""source_trajectory_step_count":2"#);

    let (status, confirmed) = api.route_with_tenant(
        "POST",
        "/v1/golden-paths/1/status",
        r#"{"status":"confirmed","reason":"accepted by eval","source":"eval_harness"}"#,
        Some(15),
    );
    assert_eq!(status, 200, "{confirmed}");
    assert_json_contains(&confirmed, r#""status":"confirmed""#);

    let challenger = r#"{
      "sourceTraceId":903,
      "taskFingerprint":"golden-task",
      "score":960,
      "label":"extended challenger",
      "challengerOf":1,
      "evalProfile":"release-gate",
      "projectId":"golden-eval"
    }"#;
    let (status, challenger_body) =
        api.route_with_tenant("POST", "/v1/golden-paths", challenger, Some(15));
    assert_eq!(status, 200, "{challenger_body}");
    assert_json_contains(&challenger_body, r#""goldenPathId":"2""#);
    assert_json_contains(&challenger_body, r#""challengerOf":"1""#);

    let (status, challengers) = api.route_with_tenant(
        "GET",
        "/v1/golden-paths?challengerOf=1&evalProfile=release-gate",
        "",
        Some(15),
    );
    assert_eq!(status, 200, "{challengers}");
    assert_json_contains(&challengers, r#""count":1"#);
    assert_json_contains(&challengers, r#""goldenPathId":"2""#);

    let (status, adherence) = api.route_with_tenant(
        "POST",
        "/v1/path-adherence",
        r#"{"goldenPathId":1,"traceId":902}"#,
        Some(15),
    );
    assert_eq!(status, 200, "{adherence}");
    assert_json_contains(&adherence, r#""adherence":"followed""#);
    assert_json_contains(&adherence, r#""sameSignature":true"#);

    let (status, evidence) = api.route_with_tenant(
        "POST",
        "/v1/golden-path-evidence",
        r#"{"goldenPathId":1,"candidateTraceId":903}"#,
        Some(15),
    );
    assert_eq!(status, 200, "{evidence}");
    assert_json_contains(&evidence, r#""source":{"available":true"#);
    assert_json_contains(&evidence, r#""annotationCount":1"#);
    assert_json_contains(&evidence, r#""datasetAssociationCount":1"#);
    assert_json_contains(&evidence, r#""adherence":"extended""#);
    assert_json_contains(&evidence, r#""traceDiff":{"left""#);

    let (status, export_page) = api.route_with_tenant(
        "POST",
        "/v1/golden-path-export",
        r#"{"filter":{"taskFingerprint":"golden-task","projectId":"golden-eval"},"limit":10}"#,
        Some(15),
    );
    assert_eq!(status, 200, "{export_page}");
    assert_json_contains(
        &export_page,
        r#""schemaVersion":"yitrace.golden_path_export.v1""#,
    );
    assert_json_contains(&export_page, r#""count":1"#);
    assert_json_contains(&export_page, r#""recordType":"golden_path""#);
    assert_json_contains(
        &export_page,
        r#""jsonl":"{\"schemaVersion\":\"yitrace.golden_path_export.v1\""#,
    );

    let (status, health) = api.route_with_tenant(
        "POST",
        "/v1/golden-path-health",
        r#"{"goldenPathId":1,"filter":{"projectId":"golden-eval"},"limit":10,"examples":10}"#,
        Some(15),
    );
    assert_eq!(status, 200, "{health}");
    assert_json_contains(&health, r#""matchingTraceTotal":2"#);
    assert_json_contains(&health, r#""followed":1"#);
    assert_json_contains(&health, r#""extended":1"#);
    assert_json_contains(&health, r#""usable":1.000000"#);
    assert_json_contains(&health, r#""governance":{"evalProfile":"release-gate""#);
}

// ───────────────────────── 会话级（多轮）评测 ─────────────────────────

/// 会话级评测应把多轮对话准确分成「一次到位 / 绕圈后解决 / 未解决」三类，
/// 且分类与生成时注入的会话弧线一一对账（生成什么弧线，就该评成什么类）。
#[test]
fn session_eval_classifies_multi_turn_conversations() {
    let coord = fresh();
    let r = evalkit::run_session_harness(&coord, 60, 99);
    // 三类是一个划分（互斥且周全）。
    assert_eq!(
        r.efficient + r.looped_resolved + r.unresolved,
        r.evals.len()
    );
    // 与生成弧线对账：一次到位=resolved_fast；绕圈后解决=重试+重复问；未解决=始终失败。
    assert_eq!(r.efficient, r.gen.resolved_fast, "一次到位");
    assert_eq!(
        r.looped_resolved,
        r.gen.resolved_after_retry + r.gen.repeat_question,
        "绕圈后解决"
    );
    assert_eq!(r.unresolved, r.gen.unresolved, "未解决");
    // 各类都得有样本，演示才立得住。
    assert!(
        r.efficient > 0 && r.looped_resolved > 0 && r.unresolved > 0,
        "三类应都有样本"
    );
    // 确实是多轮（平均 > 1 轮）。
    assert!(r.avg_turns > 1.0, "应是多轮会话");
}

/// 绕圈检测要双管齐下：连续失败、重复问，都应被判 looped。
#[test]
fn looping_is_detected_for_both_retry_and_repeat() {
    let coord = fresh();
    let r = evalkit::run_session_harness(&coord, 80, 7);
    // 被判 looped 的会话数应 ≥ 重试类（连续失败必触发 looped）。
    let looped = r.evals.iter().filter(|e| e.looped).count();
    assert!(
        looped >= r.gen.resolved_after_retry,
        "连续失败的重试会话都应被判绕圈"
    );
    // 「绕圈后解决」的会话：既 resolved 又 looped。
    assert!(
        r.evals.iter().any(|e| e.resolved && e.looped),
        "应有绕圈后解决的会话"
    );
    // 未解决的会话最后一轮一定是失败的。
    assert!(r
        .evals
        .iter()
        .filter(|e| !e.resolved)
        .all(|e| e.failed_turns > 0));
}
