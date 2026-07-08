use std::time::{SystemTime, UNIX_EPOCH};

use yitrace_db::{OpenOptions, SearchQuery, SpanEndOptions, SpanEventBuilder, YiTraceDb};

fn temp_dir(name: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("yitrace_db_rs_{name}_{nanos}"))
}

#[test]
fn rust_embedded_db_ingests_searches_and_reads_details() {
    let dir = temp_dir("roundtrip");
    let db = YiTraceDb::open_with_options(OpenOptions::new(&dir).tenant_id(1)).unwrap();

    let mut events = SpanEventBuilder::new("run-rs-uuid");
    events
        .session_id("session-rs-uuid")
        .attr("project_id", "agentic-data")
        .attr("skill", "review")
        .start_span("span-rs-uuid", "风控研判")
        .log("span-rs-uuid", "疑似盗刷")
        .end_span_with(
            "span-rs-uuid",
            SpanEndOptions::ok()
                .duration_ns(12_000_000)
                .input_tokens(80)
                .output_tokens(20)
                .output_text("需要人工复核"),
        );

    let ingest = db.ingest_builder(&events).unwrap();
    assert!(ingest.contains("\"ingested\""));

    let search = db
        .search(
            &SearchQuery::text("盗刷")
                .k(10)
                .attr("project_id", "agentic-data")
                .attr("skill", "review"),
        )
        .unwrap();
    assert!(search.contains("疑似盗刷"), "{search}");
    assert!(search.contains("run-rs-uuid"), "{search}");

    let traces = db.traces().unwrap();
    assert!(traces.contains("run-rs-uuid"), "{traces}");

    let spans = db.trace("run-rs-uuid").unwrap();
    assert!(spans.contains("风控研判"), "{spans}");

    let span = db.span("run-rs-uuid", "span-rs-uuid").unwrap();
    assert!(span.contains("\"logEvents\""), "{span}");
    assert!(span.contains("疑似盗刷"), "{span}");
}

#[test]
fn rust_embedded_db_rejects_second_writer_lock() {
    let dir = temp_dir("lock");
    let mut first = YiTraceDb::open(&dir).unwrap();
    let lock_text = std::fs::read_to_string(dir.join(".yitrace.lock")).unwrap();
    assert!(lock_text.contains("\"pid\""), "{lock_text}");
    assert!(lock_text.contains(&dir.display().to_string()), "{lock_text}");
    let second = YiTraceDb::open(&dir);
    let err = match second {
        Ok(_) => panic!("second writer should fail"),
        Err(err) => err.to_string(),
    };
    assert!(err.contains("already open or locked"), "{err}");
    assert!(err.contains("existing lock owner"), "{err}");
    assert!(err.contains("\"pid\""), "{err}");
    first.close().unwrap();
    YiTraceDb::open(&dir).unwrap();
}

#[test]
fn rust_embedded_db_route_json_reports_closed_and_request_errors() {
    let dir = temp_dir("route");
    let mut db = YiTraceDb::open(&dir).unwrap();
    let bad = db.route_json("POST", "/v1/search", "not json").unwrap_err();
    assert!(bad.to_string().contains("status=400"), "{bad}");

    db.close().unwrap();
    let closed = db.traces().unwrap_err();
    assert!(closed.to_string().contains("closed"), "{closed}");
}
