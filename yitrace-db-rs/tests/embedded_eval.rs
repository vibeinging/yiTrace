use std::time::{SystemTime, UNIX_EPOCH};

use yitrace_db::{
    JsonValue, OpenOptions, SearchQuery, SpanEndOptions, SpanEventBuilder, YiTraceDb,
};

fn temp_dir(name: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("yitrace_db_rs_eval_{name}_{nanos}"))
}

#[test]
fn eval_rust_embedded_db_preserves_external_ids_attrs_and_reopens() {
    let dir = temp_dir("durable_attrs");
    let mut db = YiTraceDb::open_with_options(OpenOptions::new(&dir).tenant_id(7)).unwrap();

    let mut events = SpanEventBuilder::new("agent-run-rs-uuid");
    events
        .session_id("agent-session-rs-uuid")
        .attr("project_id", "agentic-data")
        .attr("skill", "refund-review")
        .attr("mode", "eval")
        .attr("validation_status", "pass")
        .attr("score", 920u64)
        .attr(
            "tags",
            JsonValue::Array(vec!["golden".into(), "rust".into(), "embedded".into()]),
        )
        .attr(
            "evidence",
            JsonValue::Object(vec![
                ("attempts".to_string(), 3u64.into()),
                ("verified".to_string(), true.into()),
            ]),
        )
        .start_span_with(
            "agent-span-rs-uuid",
            yitrace_db::SpanStartOptions::new("退款审核")
                .agent_name("refund-agent")
                .input_text("用户反馈疑似重复扣款"),
        )
        .log("agent-span-rs-uuid", "查询订单、核对支付流水、给出退款建议")
        .end_span_with(
            "agent-span-rs-uuid",
            SpanEndOptions::ok()
                .duration_ns(34_000_000)
                .input_tokens(120)
                .output_tokens(45)
                .output_text("建议退款并标记为已验证路径"),
        );

    db.ingest_builder(&events).unwrap();

    let search = db
        .search(
            &SearchQuery::text("退款")
                .k(5)
                .agent_name("refund-agent")
                .attr("project_id", "agentic-data")
                .attr("skill", "refund-review")
                .attr("validation_status", "pass"),
        )
        .unwrap();
    assert!(search.contains("agent-run-rs-uuid"), "{search}");
    assert!(search.contains("agent-span-rs-uuid"), "{search}");
    assert!(search.contains("refund-agent"), "{search}");

    let span = db.span("agent-run-rs-uuid", "agent-span-rs-uuid").unwrap();
    assert!(
        span.contains("\"externalTraceId\":\"agent-run-rs-uuid\""),
        "{span}"
    );
    assert!(
        span.contains("\"externalSpanId\":\"agent-span-rs-uuid\""),
        "{span}"
    );
    assert!(span.contains("\"tags\""), "{span}");
    assert!(span.contains("golden"), "{span}");
    assert!(span.contains("\"evidence\""), "{span}");
    assert!(span.contains("\"logEvents\""), "{span}");

    let trace_search = db
        .trace_search_json(
            r#"{"text":"退款","filter":{"projectId":"agentic-data","skill":"refund-review"},"limit":10}"#,
        )
        .unwrap();
    assert!(trace_search.contains("\"total\":1"), "{trace_search}");
    assert!(
        trace_search.contains("\"externalTraceId\":\"agent-run-rs-uuid\""),
        "{trace_search}"
    );
    assert!(
        trace_search.contains("\"source\":\"filter_index\""),
        "{trace_search}"
    );
    assert!(
        trace_search.contains("\"usedFilterIndex\":true"),
        "{trace_search}"
    );
    assert!(
        trace_search.contains("\"candidateSpanKeys\":1"),
        "{trace_search}"
    );

    let aggregate = db
        .trace_aggregate_json(
            r#"{"filter":{"projectId":"agentic-data"},"groupBy":["skill"],"limit":10}"#,
        )
        .unwrap();
    assert!(
        aggregate.contains("\"skill\":\"refund-review\""),
        "{aggregate}"
    );
    assert!(aggregate.contains("\"spanCount\":1"), "{aggregate}");
    assert!(
        aggregate.contains("\"usedFilterIndex\":true"),
        "{aggregate}"
    );

    let storage = db
        .storage_stats_json(r#"{"filter":{"projectId":"agentic-data"},"groupBy":["skill"]}"#)
        .unwrap();
    assert!(storage.contains("\"traceCount\":1"), "{storage}");
    assert!(storage.contains("\"estimatedBytes\":"), "{storage}");
    assert!(storage.contains("\"usedFilterIndex\":true"), "{storage}");

    db.ingest_json(
        r#"[
          {"trace_id":"rs-task-a","span_id":"plan","ts":200,"seq":1,"event_type":2,"ext_span_id":"rs-task-a-plan","agent_name":"planner","status":0,"duration_ns":100,"attrs":{"project_id":"agentic-data","skill":"refund-review","task_fingerprint":"rs-task","loop_id":"rs-loop","validation_status":"pass"}},
          {"trace_id":"rs-task-a","span_id":"tool","parent_span_id":"plan","ts":210,"seq":1,"event_type":2,"ext_span_id":"rs-task-a-tool","tool_name":"sql.check","status":0,"duration_ns":50,"attrs":{"project_id":"agentic-data","skill":"refund-review","task_fingerprint":"rs-task","loop_id":"rs-loop","validation_status":"pass"}},
          {"trace_id":"rs-task-b","span_id":"plan","ts":220,"seq":1,"event_type":2,"ext_span_id":"rs-task-b-plan","agent_name":"planner","status":1,"duration_ns":300,"attrs":{"project_id":"agentic-data","skill":"refund-review","task_fingerprint":"rs-task","loop_id":"rs-loop","validation_status":"fail"}}
        ]"#,
    )
    .unwrap();

    let trajectories = db
        .trace_trajectories_json(
            r#"{"filter":{"projectId":"agentic-data","taskFingerprint":"rs-task"},"limit":10}"#,
        )
        .unwrap();
    assert!(trajectories.contains("\"total\":2"), "{trajectories}");
    assert!(trajectories.contains("rs-task-a"), "{trajectories}");

    let groups = db
        .trajectory_groups_json(
            r#"{"filter":{"projectId":"agentic-data","taskFingerprint":"rs-task"},"limit":10}"#,
        )
        .unwrap();
    assert!(groups.contains("\"total\":2"), "{groups}");
    assert!(groups.contains("\"successCount\":1"), "{groups}");

    let diff = db
        .trace_diff_json(r#"{"baseTraceId":"rs-task-a","candidateTraceId":"rs-task-b"}"#)
        .unwrap();
    assert!(diff.contains("\"sameSignature\":false"), "{diff}");

    let loops = db.loops().unwrap();
    assert!(loops.contains("\"loopId\":\"rs-loop\""), "{loops}");

    let loop_detail = db.loop_detail("rs-loop").unwrap();
    assert!(loop_detail.contains("\"traceCount\":2"), "{loop_detail}");

    let task_traces = db
        .route_json("GET", "/v1/tasks/rs-task/traces?validationStatus=pass", "")
        .unwrap();
    assert!(task_traces.contains("\"total\":1"), "{task_traces}");
    assert!(task_traces.contains("rs-task-a"), "{task_traces}");

    let annotation = db
        .annotate_json(
            r#"{"traceId":"agent-run-rs-uuid","spanId":"agent-span-rs-uuid","label":"best_path","score":950,"reason":"human confirmed","source":"rust-test","attrs":{"project_id":"agentic-data","skill":"refund-review"}}"#,
        )
        .unwrap();
    assert!(
        annotation.contains("\"annotationId\":\"1\""),
        "{annotation}"
    );
    assert!(annotation.contains("agent-run-rs-uuid"), "{annotation}");

    let annotations = db
        .annotations_query("projectId=agentic-data&skill=refund-review&label=best_path")
        .unwrap();
    assert!(annotations.contains("\"total\":1"), "{annotations}");

    let updated = db
        .update_annotation_json(
            1,
            r#"{"status":"resolved","reviewer":"qa","attrs":{"mode":"eval"}}"#,
        )
        .unwrap();
    assert!(updated.contains("\"status\":\"resolved\""), "{updated}");
    assert!(updated.contains("\"mode\":\"eval\""), "{updated}");
    assert!(
        updated.contains("\"project_id\":\"agentic-data\""),
        "{updated}"
    );

    let deleted = db
        .delete_annotation_json(1, r#"{"reviewer":"qa","reason":"stale"}"#)
        .unwrap();
    assert!(deleted.contains("\"status\":\"deleted\""), "{deleted}");

    let active_only = db
        .annotations_query("projectId=agentic-data&label=best_path")
        .unwrap();
    assert!(active_only.contains("\"total\":0"), "{active_only}");
    let with_deleted = db
        .annotations_query("projectId=agentic-data&label=best_path&includeDeleted=true")
        .unwrap();
    assert!(
        with_deleted.contains("\"status\":\"deleted\""),
        "{with_deleted}"
    );
    assert!(with_deleted.contains("stale"), "{with_deleted}");

    let link = db
        .link_dataset_item_json(
            r#"{"datasetId":"rs-regression","itemId":"case-1","traceId":"agent-run-rs-uuid","spanId":"agent-span-rs-uuid","split":"eval","label":"pass","score":900,"attrs":{"project_id":"agentic-data","skill":"refund-review"}}"#,
        )
        .unwrap();
    assert!(link.contains("\"associationId\":\"1\""), "{link}");
    assert!(
        link.contains("\"externalSpanId\":\"agent-span-rs-uuid\""),
        "{link}"
    );

    let links = db
        .dataset_associations_query("datasetId=rs-regression&projectId=agentic-data")
        .unwrap();
    assert!(links.contains("\"total\":1"), "{links}");
    assert!(links.contains("\"itemId\":\"case-1\""), "{links}");

    db.ingest_json(
        r#"[
          {"trace_id":"rs-retention-keep","span_id":"span","ts":400,"seq":1,"event_type":2,"ext_span_id":"rs-retention-keep-span","status":0,"duration_ns":10,"attrs":{"project_id":"rs-retention","skill":"cleanup"}},
          {"trace_id":"rs-retention-delete","span_id":"span","ts":410,"seq":1,"event_type":2,"ext_span_id":"rs-retention-delete-span","status":0,"duration_ns":10,"attrs":{"project_id":"rs-retention","skill":"cleanup"}}
        ]"#,
    )
    .unwrap();
    db.flush().unwrap();
    db.ingest_json(
        r#"[
          {"trace_id":"rs-retention-hot","span_id":"span","ts":420,"seq":1,"event_type":2,"ext_span_id":"rs-retention-hot-span","status":0,"duration_ns":10,"attrs":{"project_id":"rs-retention","skill":"cleanup"}}
        ]"#,
    )
    .unwrap();
    db.annotate_json(
        r#"{"traceId":"rs-retention-keep","label":"keep","source":"retention-test","attrs":{"project_id":"rs-retention"}}"#,
    )
    .unwrap();
    let retention_query = r#"{"filter":{"projectId":"rs-retention"},"deleteBeforeTs":1000,"protect":{"annotations":true,"datasetAssociations":true,"snapshots":true,"evalLinks":true,"pathMemory":true},"requestedBy":"rust-retention","reason":"ttl"}"#;
    let plan = db.retention_plan_json(retention_query).unwrap();
    assert!(plan.contains("\"dryRun\":true"), "{plan}");
    assert!(plan.contains("\"traceCount\":3"), "{plan}");
    assert!(plan.contains("annotation"), "{plan}");

    let applied = db.apply_retention_json(retention_query).unwrap();
    assert!(applied.contains("\"applied\":true"), "{applied}");
    assert!(applied.contains("\"deletedTraceIds\""), "{applied}");
    assert!(applied.contains("\"skippedLiveTraceIds\""), "{applied}");

    let remaining = db
        .trace_search_json(r#"{"filter":{"projectId":"rs-retention"},"limit":10}"#)
        .unwrap();
    assert!(remaining.contains("\"total\":2"), "{remaining}");
    assert!(remaining.contains("rs-retention-keep"), "{remaining}");
    assert!(remaining.contains("rs-retention-hot"), "{remaining}");
    assert!(!remaining.contains("rs-retention-delete"), "{remaining}");

    let audits = db.retention_audits_query("source=rust-retention").unwrap();
    assert!(audits.contains("\"total\":1"), "{audits}");
    assert!(audits.contains("\"deletedTraceCount\":1"), "{audits}");

    let policy = db
        .create_retention_policy_json(
            r#"{"name":"rs-retention-policy","intervalNs":1000,"nextRunAtNs":1,"query":{"filter":{"projectId":"rs-retention"},"deleteBeforeTs":1000,"protect":{"annotations":true},"requestedBy":"rust-policy"},"source":"rust-policy","reason":"ttl"}"#,
        )
        .unwrap();
    assert!(policy.contains("\"policyId\":\"1\""), "{policy}");
    let due = db
        .run_retention_policies_json(r#"{"nowNs":2,"limit":1}"#)
        .unwrap();
    assert!(due.contains("\"ran\":1"), "{due}");
    assert!(due.contains("\"ok\":true"), "{due}");

    db.close().unwrap();

    let reopened = YiTraceDb::open_with_options(OpenOptions::new(&dir).tenant_id(7)).unwrap();
    let after_reopen = reopened
        .search(
            &SearchQuery::text("退款")
                .k(5)
                .attr("project_id", "agentic-data"),
        )
        .unwrap();
    assert!(after_reopen.contains("agent-run-rs-uuid"), "{after_reopen}");
    assert!(
        after_reopen.contains("agent-span-rs-uuid"),
        "{after_reopen}"
    );
    let reopened_annotations = reopened
        .annotations_query("projectId=agentic-data&includeDeleted=true")
        .unwrap();
    assert!(
        reopened_annotations.contains("\"status\":\"deleted\""),
        "{reopened_annotations}"
    );
    let reopened_links = reopened
        .dataset_associations_query("datasetId=rs-regression")
        .unwrap();
    assert!(
        reopened_links.contains("\"itemId\":\"case-1\""),
        "{reopened_links}"
    );
    let reopened_audits = reopened.retention_audits().unwrap();
    assert!(reopened_audits.contains("\"total\":2"), "{reopened_audits}");
    let reopened_policies = reopened
        .retention_policies_query("name=rs-retention-policy")
        .unwrap();
    assert!(
        reopened_policies.contains("\"lastRunAtNs\":\"2\""),
        "{reopened_policies}"
    );
}

#[test]
fn eval_rust_embedded_db_keeps_tenant_boundary_on_helpers_and_raw_route() {
    let dir = temp_dir("tenant");
    let db = YiTraceDb::open_with_options(OpenOptions::new(&dir).tenant_id(11)).unwrap();

    let mut events = SpanEventBuilder::new("tenant-run-rs");
    events
        .session_id("tenant-session-rs")
        .attr("project_id", "tenant-eval")
        .attr("skill", "isolation")
        .start_span("tenant-span-rs", "租户隔离")
        .log("tenant-span-rs", "tenant 11 only")
        .end_span("tenant-span-rs", 0);
    db.ingest_builder(&events).unwrap();

    let visible = db.search(&SearchQuery::text("tenant").k(5)).unwrap();
    assert!(visible.contains("tenant-run-rs"), "{visible}");

    let hidden = db
        .route_json_with_tenant(
            "POST",
            "/v1/search",
            &SearchQuery::text("tenant").k(5).to_json(),
            Some(12),
        )
        .unwrap();
    assert!(!hidden.contains("tenant-run-rs"), "{hidden}");

    let hidden_traces = db
        .route_json_with_tenant("GET", "/v1/traces", "", Some(12))
        .unwrap();
    assert!(!hidden_traces.contains("tenant-run-rs"), "{hidden_traces}");
}
