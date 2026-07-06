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
                .total_tokens(165)
                .cost_usd_nanos(420_000)
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

    let aggregate = db
        .trace_aggregate_json(
            r#"{"filter":{"projectId":"agentic-data","skill":"refund-review"},"groupBy":["validationStatus"],"limit":10}"#,
        )
        .unwrap();
    assert!(aggregate.contains("validation_status"), "{aggregate}");
    assert!(aggregate.contains("pass"), "{aggregate}");
    assert!(aggregate.contains("\"spanCount\":1"), "{aggregate}");

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
