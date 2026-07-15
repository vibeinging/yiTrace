use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

use yitrace::{
    event_id, CollectingExporter, EventType, Exporter, HttpExporter, SpanEvent, SpanOptions,
    TraceOptions, Tracer, YiTraceError,
};

#[test]
fn event_id_matches_engine_fixtures() {
    assert_eq!(
        event_id("demo-span", 7, EventType::SpanEnd),
        16098495313036060864
    );
    assert_eq!(
        event_id("1002-1", 1, EventType::SpanStart),
        3941713543033365492
    );
    assert_eq!(
        event_id("反洗钱-1", 3, EventType::Attr),
        13462389519714918643
    );
}

#[test]
fn tracer_emits_start_log_end_and_parent_child() {
    let exporter = CollectingExporter::default();
    let mut tracer = Tracer::with_exporter(exporter, 1);
    tracer
        .trace_with_result(
            "反洗钱筛查",
            TraceOptions::default().session_id(9000).tenant_id(7),
            |trace| {
                trace.span_result("root", |root| {
                    root.set_agent("规划");
                    root.set_model("qwen3");
                    root.set_io(Some("请研判这笔交易".to_string()), None);
                    root.log("开始")?;
                    root.span_result("child", |child| {
                        child.set_tool("kb_lookup");
                        child.log("疑似盗刷")
                    })?;
                    root.set_tokens(Some(1200), Some(340));
                    root.set_io(None, Some("判定为疑似盗刷".to_string()));
                    root.set_status(0);
                    Ok(())
                })
            },
        )
        .unwrap();
    tracer.close().unwrap();

    let events = tracer.into_exporter().into_events();
    assert_eq!(events.len(), 6);
    assert_eq!(events[0].event_type, EventType::SpanStart);
    assert_eq!(events[1].event_type, EventType::Log);
    assert!(events.iter().all(|event| event.session_id == Some(9000)));
    assert!(events.iter().all(|event| event.tenant_id == Some(7)));

    let root_start = events
        .iter()
        .find(|event| {
            event.event_type == EventType::SpanStart
                && event.span_name.as_deref() == Some("root")
        })
        .unwrap();
    let child_start = events
        .iter()
        .find(|event| {
            event.event_type == EventType::SpanStart
                && event.span_name.as_deref() == Some("child")
        })
        .unwrap();
    assert_eq!(root_start.parent_span_id, None);
    assert_eq!(child_start.parent_span_id, Some(root_start.span_id));

    let root_end = events
        .iter()
        .find(|event| event.event_type == EventType::SpanEnd && event.span_id == root_start.span_id)
        .unwrap();
    assert_eq!(root_end.status, Some(0));
    assert_eq!(root_end.input_tokens, Some(1200));
    assert_eq!(root_end.output_tokens, Some(340));
    assert_eq!(root_end.agent_name.as_deref(), Some("规划"));
    assert_eq!(root_end.model.as_deref(), Some("qwen3"));
    assert_eq!(root_end.output_text.as_deref(), Some("判定为疑似盗刷"));
}

#[test]
fn display_name_is_optional_and_agent_context_is_inherited() {
    let exporter = CollectingExporter::default();
    let mut tracer = Tracer::with_exporter(exporter, 1);
    tracer
        .trace_with_result(
            "x",
            TraceOptions::default().agent_name("planner_agent"),
            |trace| {
                trace.span_with_result(
                    "planner.route",
                    SpanOptions::default().display_name("  规划下一步  "),
                    |_| Ok(()),
                )
            },
        )
        .unwrap();
    let events = tracer.into_exporter().into_events();
    let start = events
        .iter()
        .find(|event| event.event_type == EventType::SpanStart)
        .unwrap();
    assert_eq!(start.span_name.as_deref(), Some("planner.route"));
    assert_eq!(start.display_name.as_deref(), Some("规划下一步"));
    assert!(events
        .iter()
        .all(|event| event.agent_name.as_deref() == Some("planner_agent")));
    assert!(events
        .iter()
        .filter(|event| event.event_type != EventType::SpanStart)
        .all(|event| event.span_name.is_none() && event.display_name.is_none()));
}

#[test]
fn result_api_marks_span_failed_when_closure_returns_error() {
    let exporter = CollectingExporter::default();
    let mut tracer = Tracer::with_exporter(exporter, 1);
    let result: yitrace::Result<()> = tracer.trace_result("risk review", |trace| {
        trace.span_result("LLM check", |span| {
            span.log("before failure")?;
            Err(YiTraceError::InvalidUrl("domain failure".to_string()))
        })
    });
    let err = result.unwrap_err();
    assert!(err.to_string().contains("domain failure"));

    let events = tracer.into_exporter().into_events();
    assert_eq!(events.len(), 3);
    let end = events
        .iter()
        .find(|event| event.event_type == EventType::SpanEnd)
        .unwrap();
    assert_eq!(end.status, Some(1));
}

#[test]
fn event_json_escapes_and_uses_wire_field_names() {
    let event = SpanEvent {
        trace_id: 1,
        span_id: 2,
        ts: 3,
        seq: 4,
        event_type: EventType::Log,
        ext_span_id: "span-1".to_string(),
        parent_span_id: None,
        status: None,
        duration_ns: None,
        input_tokens: None,
        output_tokens: None,
        session_id: None,
        tenant_id: Some(9),
        span_name: None,
        display_name: None,
        agent_name: Some("agent".to_string()),
        tool_name: None,
        model: None,
        input_text: Some("hello \"rust\"".to_string()),
        output_text: None,
        logs: vec!["line\nbreak".to_string()],
    };
    let json = event.to_json();
    assert!(json.contains("\"event_type\":4"), "{json}");
    assert!(json.contains("\"tenant_id\":9"), "{json}");
    assert!(json.contains("hello \\\"rust\\\""), "{json}");
    assert!(json.contains("line\\nbreak"), "{json}");
}

#[test]
fn http_exporter_posts_batch_with_auth_and_tenant_headers() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_http_request(&mut stream);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}")
            .unwrap();
        request
    });

    let mut exporter = HttpExporter::new(format!("http://{addr}/v1/ingest"))
        .unwrap()
        .with_token("secret")
        .with_tenant_id(42);
    exporter.export_batch(vec![sample_event()]).unwrap();
    assert_eq!(exporter.sent_count(), 1);

    let request = handle.join().unwrap();
    assert!(request.starts_with("POST /v1/ingest HTTP/1.1"), "{request}");
    assert!(
        request.contains("Authorization: Bearer secret"),
        "{request}"
    );
    assert!(request.contains("X-Tenant-Id: 42"), "{request}");
    assert!(request.contains("\"logs\":[\"疑似盗刷\"]"), "{request}");
}

#[test]
fn http_exporter_buffers_failed_batch() {
    let mut exporter = HttpExporter::new("http://127.0.0.1:9/v1/ingest")
        .unwrap()
        .with_batch_size(1)
        .with_max_buffered(1);
    let err = exporter.export(sample_event()).unwrap_err();
    assert!(err.to_string().contains("Connection") || err.to_string().contains("refused"));
    assert_eq!(exporter.buffered_count(), 1);
    assert_eq!(exporter.dropped_count(), 0);

    let _ = exporter.export(sample_event());
    assert_eq!(exporter.buffered_count(), 1);
    assert!(exporter.dropped_count() >= 1);
}

fn sample_event() -> SpanEvent {
    SpanEvent {
        trace_id: 1,
        span_id: 2,
        ts: 3,
        seq: 1,
        event_type: EventType::Log,
        ext_span_id: "span-1".to_string(),
        parent_span_id: None,
        status: None,
        duration_ns: None,
        input_tokens: None,
        output_tokens: None,
        session_id: None,
        tenant_id: None,
        span_name: None,
        display_name: None,
        agent_name: None,
        tool_name: None,
        model: None,
        input_text: None,
        output_text: None,
        logs: vec!["疑似盗刷".to_string()],
    }
}

fn read_http_request(stream: &mut std::net::TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut buf = [0u8; 512];
    loop {
        let n = stream.read(&mut buf).unwrap();
        assert!(n > 0, "connection closed before HTTP headers");
        bytes.extend_from_slice(&buf[..n]);
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let header_end = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap()
        + 4;
    let head = String::from_utf8_lossy(&bytes[..header_end]).to_string();
    let content_length = head
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    while bytes.len() - header_end < content_length {
        let n = stream.read(&mut buf).unwrap();
        assert!(n > 0, "connection closed before HTTP body");
        bytes.extend_from_slice(&buf[..n]);
    }
    String::from_utf8(bytes).unwrap()
}
