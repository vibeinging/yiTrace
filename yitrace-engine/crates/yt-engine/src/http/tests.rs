use super::*;
use crate::InMemorySegmentStore;

fn server() -> HttpIngestServer {
    HttpIngestServer::new(WriteCoordinator::new(Arc::new(
        InMemorySegmentStore::default(),
    )))
}

const BATCH: &str = r#"[
  {"trace_id":7,"span_id":1,"ts":100,"seq":1,"event_type":1,"ext_span_id":"7-1","status":0,"input_tokens":900,"logs":["开始"]},
  {"trace_id":7,"span_id":1,"ts":150,"seq":2,"event_type":2,"ext_span_id":"7-1","duration_ns":50,"output_tokens":150,"logs":["结束"]}
]"#;

#[test]
fn route_ingest_then_query() {
    let s = server();
    let (status, body) = s.route("POST", "/v1/ingest", BATCH);
    assert_eq!(status, 200);
    assert!(body.contains("\"ingested\":2"));

    let (status, body) = s.route("GET", "/v1/traces", "");
    assert_eq!(status, 200);
    assert!(body.contains("\"trace_id\":7"), "{body}");
    assert!(body.contains("\"total_input_tokens\":900"));
}

#[test]
fn route_ingest_accepts_external_ids_and_attrs() {
    let s = server();
    let batch = r#"[
    {
      "trace_id":"run-uuid",
      "span_id":"span-uuid",
      "session_id":"session-uuid",
      "ts":100,
      "seq":1,
      "event_type":2,
      "ext_span_id":"span-uuid",
      "status":0,
      "duration_ns":50,
      "agent_name":"risk",
      "input_text":"疑似盗刷",
      "attrs":{"external_run_id":"run-uuid","project_id":"agentic-data","skill":"review","mode":"auto","call_site":"worker.ts:10"}
    },
    {
      "trace_id":"123456",
      "span_id":"numeric-business-span",
      "ts":120,
      "seq":1,
      "event_type":2,
      "ext_span_id":"numeric-business-span",
      "status":0,
      "input_text":"数字业务主键",
      "attrs":{"project_id":"agentic-data","skill":"review"}
    }]"#;
    let (status, body) = s.route("POST", "/v1/ingest", batch);
    assert_eq!(status, 200, "{body}");

    let (status, body) = s.route("GET", "/v1/traces/run-uuid", "");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""externalTraceId":"run-uuid""#), "{body}");
    assert!(body.contains(r#""externalSpanId":"span-uuid""#), "{body}");
    assert!(body.contains(r#""project_id":"agentic-data""#), "{body}");

    let (status, body) = s.route("GET", "/v1/traces/run-uuid/spans/span-uuid", "");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""externalSpanId":"span-uuid""#), "{body}");
    assert!(body.contains(r#""call_site":"worker.ts:10""#), "{body}");

    let (status, body) = s.route(
        "POST",
        "/v1/search",
        r#"{"text":"盗刷","filter":{"trace_id":"run-uuid"}}"#,
    );
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""external_trace_id":"run-uuid""#), "{body}");
    assert!(body.contains(r#""skill":"review""#), "{body}");

    let (status, body) = s.route(
        "POST",
        "/v1/search",
        r#"{"text":"盗刷","filter":{"attrs":{"project_id":"agentic-data","skill":"review"}}}"#,
    );
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""external_trace_id":"run-uuid""#), "{body}");

    let (status, body) = s.route(
        "POST",
        "/v1/trace-search",
        r#"{"filter":{"externalTraceId":"run-uuid"},"limit":1}"#,
    );
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""total":1"#), "{body}");
    assert!(body.contains(r#""externalTraceId":"run-uuid""#), "{body}");
    assert!(body.contains(r#""usedFilterIndex":true"#), "{body}");
    assert!(body.contains(r#""candidateSpanKeys":1"#), "{body}");

    let (status, body) = s.route(
        "POST",
        "/v1/trace-search",
        r#"{"filter":{"traceId":"123456"},"limit":1}"#,
    );
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""total":1"#), "{body}");
    assert!(body.contains(r#""externalTraceId":"123456""#), "{body}");
    assert!(body.contains(r#""usedFilterIndex":true"#), "{body}");
    assert!(body.contains(r#""candidateSpanKeys":1"#), "{body}");

    let (status, body) = s.route(
        "POST",
        "/v1/trace-search",
        r#"{"filter":{"traceId":123456},"limit":1}"#,
    );
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""total":1"#), "{body}");
    assert!(body.contains(r#""externalTraceId":"123456""#), "{body}");
    assert!(body.contains(r#""usedFilterIndex":true"#), "{body}");
    assert!(body.contains(r#""candidateSpanKeys":1"#), "{body}");

    let (status, body) = s.route(
        "POST",
        "/v1/trace-search",
        r#"{"filter":{"externalTraceId":"run-missing"},"limit":1}"#,
    );
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""total":0"#), "{body}");
    assert!(body.contains(r#""usedFilterIndex":true"#), "{body}");
    assert!(body.contains(r#""candidateSpanKeys":0"#), "{body}");

    let (status, body) = s.route(
        "POST",
        "/v1/search",
        r#"{"text":"盗刷","filter":{"project_id":"agentic-data","skill":"other"}}"#,
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(body, "[]");
}

#[test]
fn route_metrics_reports_prometheus_format() {
    // §3.1：/v1/metrics 输出 Prometheus 文本格式，含关键运行态指标。
    let s = server();
    // 灌点数据，让 memtable_rows > 0、committed_tail 推进。
    s.route("POST", "/v1/ingest", BATCH);
    let (status, body) = s.route("GET", "/v1/metrics", "");
    assert_eq!(status, 200);
    // Prometheus 格式特征：有 # HELP / # TYPE 注释、metric 行。
    assert!(body.contains("# HELP "), "应有 HELP 注释:\n{body}");
    assert!(body.contains("# TYPE "), "应有 TYPE 注释:\n{body}");
    // 关键指标都在。
    assert!(
        body.contains("yt_manifest_version"),
        "缺 manifest 版本:\n{body}"
    );
    assert!(body.contains("yt_memtable_rows"), "缺内存表行数:\n{body}");
    assert!(body.contains("yt_wal_committed_tail"), "缺 WAL 尾:\n{body}");
    assert!(body.contains("yt_segments_live"), "缺活跃段数:\n{body}");
    assert!(body.contains("yt_readers_active"), "缺活跃读者:\n{body}");
    assert!(
        body.contains("yt_read_model_rollup_ready")
            && body.contains("yt_read_model_filter_ready")
            && body.contains("yt_read_model_search_ready"),
        "缺惰性读模型就绪指标:\n{body}"
    );
    assert!(
        body.contains("yt_filter_attr_disabled_postings"),
        "缺过滤 postings 预算指标:\n{body}"
    );
    // 灌过数据 → committed_tail > 0。
    assert!(
        body.lines()
            .any(|l| l.starts_with("yt_wal_committed_tail ") && !l.ends_with(" 0")),
        "灌数据后 committed_tail 应 > 0:\n{body}"
    );
}

#[test]
fn route_health_and_ready_are_ok() {
    let s = server();
    assert_eq!(s.route("GET", "/v1/healthz", "").1, r#"{"ok":true}"#);
    assert_eq!(s.route("GET", "/v1/readyz", "").1, r#"{"ok":true}"#);
}

#[test]
fn http_tenant_header_isolates_traces_and_search() {
    // HTTP 端到端租户隔离：摄入时 tenant 来自 X-Tenant-Id，body tenant_id 被覆盖；
    // GET /v1/traces 与 POST /v1/search 带 X-Tenant-Id 头 → 只见本租户。
    let s = server();
    let batch1 = r#"[
      {"trace_id":1,"span_id":1,"ts":100,"seq":1,"event_type":2,"ext_span_id":"1-1","tenant_id":999,"duration_ns":10,"logs":["盗刷"]}
    ]"#;
    let batch2 = r#"[
      {"trace_id":2,"span_id":1,"ts":100,"seq":1,"event_type":2,"ext_span_id":"2-1","tenant_id":999,"duration_ns":20,"logs":["盗刷"]}
    ]"#;
    assert_eq!(
        s.route_with_tenant("POST", "/v1/ingest", batch1, Some(1)).0,
        200
    );
    assert_eq!(
        s.route_with_tenant("POST", "/v1/ingest", batch2, Some(2)).0,
        200
    );

    // 不带租户：两条都列。
    let all = s.route("GET", "/v1/traces", "").1;
    assert!(all.contains("\"trace_id\":1") && all.contains("\"trace_id\":2"));
    // 带租户 1：只见 trace 1。
    let t1 = s.route_with_tenant("GET", "/v1/traces", "", Some(1)).1;
    assert!(
        t1.contains("\"trace_id\":1") && !t1.contains("\"trace_id\":2"),
        "列表按租户头隔离: {t1}"
    );
    // 检索同样隔离：查"盗刷"租户 1 只回 trace 1。
    let r1 = s
        .route_with_tenant("POST", "/v1/search", r#"{"text":"盗刷","k":10}"#, Some(1))
        .1;
    assert!(
        r1.contains("\"trace_id\":1") && !r1.contains("\"trace_id\":2"),
        "检索按租户头隔离: {r1}"
    );
    let spoofed = s.route_with_tenant("GET", "/v1/traces", "", Some(999)).1;
    assert!(
        !spoofed.contains("\"trace_id\":"),
        "body tenant_id 不应生效: {spoofed}"
    );
}

#[test]
fn route_otlp_ingest_then_query() {
    // 生态入口:OTLP/HTTP JSON POST 到标准 /v1/traces → 摄入 → GET 查回。
    let s = server();
    let otlp = r#"{"resourceSpans":[{"scopeSpans":[{"spans":[{
        "traceId":"00000000000000000000000000000063","spanId":"0000000000000001",
        "name":"chat","startTimeUnixNano":"100","endTimeUnixNano":"150",
        "status":{"code":1},
        "attributes":[{"key":"gen_ai.usage.input_tokens","value":{"intValue":"900"}}]
    }]}]}]}"#;
    let (status, body) = s.route("POST", "/v1/traces", otlp);
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("partialSuccess"));

    let (status, body) = s.route("GET", "/v1/traces", "");
    assert_eq!(status, 200);
    assert!(
        body.contains("\"trace_id\":99"),
        "traceId 0x63=99 低位 {body}"
    );
    assert!(body.contains("\"total_input_tokens\":900"));
}

#[test]
fn route_otlp_tenant_header_overrides_body_tenant_attr() {
    // OTLP body 里的 yitrace.tenant_id 只是普通输入属性；HTTP 安全边界仍是 X-Tenant-Id。
    let s = server();
    let otlp = r#"{"resourceSpans":[{"scopeSpans":[{"spans":[{
        "traceId":"00000000000000000000000000000064","spanId":"0000000000000001",
        "name":"chat","startTimeUnixNano":"100","endTimeUnixNano":"150",
        "attributes":[
          {"key":"yitrace.tenant_id","value":{"stringValue":"999"}},
          {"key":"yitrace.session_id","value":{"stringValue":"777"}},
          {"key":"input.value","value":{"stringValue":"租户隔离测试"}}
        ]
    }]}]}]}"#;
    assert_eq!(
        s.route_with_tenant("POST", "/v1/traces", otlp, Some(1)).0,
        200
    );

    let t1 = s.route_with_tenant("GET", "/v1/traces", "", Some(1)).1;
    assert!(
        t1.contains("\"trace_id\":100"),
        "租户头 1 应能看到 trace: {t1}"
    );
    let spoofed = s.route_with_tenant("GET", "/v1/traces", "", Some(999)).1;
    assert!(
        !spoofed.contains("\"trace_id\":100"),
        "body tenant_id 不能越权: {spoofed}"
    );

    let sessions = s
        .route_with_tenant("GET", "/v1/sessions?cursor=0&limit=50", "", Some(1))
        .1;
    assert!(
        sessions.contains("\"sessionId\":\"777\""),
        "yitrace.session_id 应进入控制台会话: {sessions}"
    );
}

// 两条带 agent 的中文 span(走 wire 摄入 → 自动喂 BM25 + 属性边车)。
const SEARCH_BATCH: &str = r#"[
  {"trace_id":1,"span_id":10,"ts":1,"seq":1,"event_type":2,"ext_span_id":"1-10","status":1,"duration_ns":100,"agent_name":"风控","logs":["疑似盗刷 已拦截"]},
  {"trace_id":2,"span_id":20,"ts":1,"seq":1,"event_type":2,"ext_span_id":"2-20","status":0,"duration_ns":50,"agent_name":"人工","logs":["盗刷误报 复核通过"]}
]"#;

#[test]
fn route_search_text_and_filter() {
    // 检索端点:灌数据 → POST /v1/search 中文搜 → 带 agent 过滤再搜。
    let s = server();
    assert_eq!(s.route("POST", "/v1/ingest", SEARCH_BATCH).0, 200);

    // 纯文本搜"盗刷":两条都命中。
    let (st, body) = s.route("POST", "/v1/search", r#"{"text":"盗刷","k":10}"#);
    assert_eq!(st, 200, "{body}");
    assert!(
        body.contains("\"trace_id\":1") && body.contains("\"trace_id\":2"),
        "{body}"
    );

    // 加 agent 过滤:只剩风控那条。
    let (st2, body2) = s.route(
        "POST",
        "/v1/search",
        r#"{"text":"盗刷","k":10,"filter":{"agent_name":"风控"}}"#,
    );
    assert_eq!(st2, 200);
    assert!(body2.contains("\"trace_id\":1"), "{body2}");
    assert!(
        !body2.contains("\"trace_id\":2"),
        "agent 过滤掉人工那条: {body2}"
    );
    assert!(body2.contains("风控"), "响应带 agent 名");

    // 坏 body → 400。
    assert_eq!(s.route("POST", "/v1/search", "not json").0, 400);
}

#[test]
fn route_search_vector_and_hybrid() {
    // 检索端点的向量 / 混合路:body 带 vector 走找相似,text+vector 走混合。
    let coord = WriteCoordinator::new(Arc::new(InMemorySegmentStore::default()));
    let s = HttpIngestServer::new(Arc::clone(&coord));
    assert_eq!(s.route("POST", "/v1/ingest", SEARCH_BATCH).0, 200);
    coord.index_embedding(1, 10, vec![0.0, 0.0]); // 风控/盗刷,离 query 近
    coord.index_embedding(2, 20, vec![5.0, 5.0]); // 人工,远

    // 只给 vector → 找相似,最近的是 span(1,10)。
    let (st, body) = s.route("POST", "/v1/search", r#"{"vector":[0.1,0.1],"k":5}"#);
    assert_eq!(st, 200, "{body}");
    assert!(
        body.contains("\"trace_id\":1"),
        "向量找相似命中近邻: {body}"
    );

    // text + vector → 混合(RRF):盗刷两条都关键词命中,(1,10) 又被向量命中 → 排更前。
    let (st2, body2) = s.route(
        "POST",
        "/v1/search",
        r#"{"text":"盗刷","vector":[0.1,0.1],"k":5}"#,
    );
    assert_eq!(st2, 200);
    assert!(
        body2.starts_with("[{\"trace_id\":1"),
        "混合里双命中的 (1,10) 居首: {body2}"
    );

    // 向量 + agent 过滤:只剩风控那条。
    let (st3, body3) = s.route(
        "POST",
        "/v1/search",
        r#"{"vector":[0.1,0.1],"k":5,"filter":{"agent_name":"风控"}}"#,
    );
    assert_eq!(st3, 200);
    assert!(
        body3.contains("\"trace_id\":1") && !body3.contains("\"trace_id\":2"),
        "{body3}"
    );
}

#[test]
fn route_console_sessions_turns_trace_detail() {
    // 控制台数据端点端到端：灌 1 个会话(2 轮) → 会话分页 → 轮次 → trace span → span 详情。
    let s = server();
    let batch = r#"[
      {"trace_id":11,"span_id":1,"ts":1,"seq":1,"event_type":1,"ext_span_id":"11-1","session_id":900,"agent_name":"风控研判","input_tokens":500,"input_text":"对账户A做研判","attrs":{"project_id":"agentic-data","skill":"review","mode":"auto"}},
      {"trace_id":11,"span_id":1,"ts":2,"seq":2,"event_type":4,"ext_span_id":"11-1","session_id":900,"logs":["读取 package.json"],"attrs":{"call_site":"package-json"}},
      {"trace_id":11,"span_id":1,"ts":3,"seq":3,"event_type":2,"ext_span_id":"11-1","session_id":900,"status":0,"duration_ns":2000000,"output_tokens":120,"output_text":"触发规则R12"},
      {"trace_id":12,"span_id":1,"ts":3,"seq":1,"event_type":1,"ext_span_id":"12-1","session_id":900,"agent_name":"风控研判","input_tokens":300,"input_text":"继续核查"},
      {"trace_id":12,"span_id":1,"ts":4,"seq":2,"event_type":2,"ext_span_id":"12-1","session_id":900,"status":0,"duration_ns":1000000,"output_tokens":80}
    ]"#;
    assert_eq!(s.route("POST", "/v1/ingest", batch).0, 200);

    // 会话分页：1 个会话、2 轮、标题取 agent。
    let (st, body) = s.route("GET", "/v1/sessions?cursor=0&limit=50", "");
    assert_eq!(st, 200, "{body}");
    assert!(body.contains("\"sessionId\":\"900\""), "{body}");
    assert!(body.contains("\"turnCount\":2"), "{body}");
    assert!(body.contains("\"title\":\"风控研判\""), "{body}");
    assert!(body.contains("\"total\":1"), "{body}");
    assert!(body.contains("\"nextCursor\":null"), "{body}");

    let (st_attr, body_attr) = s.route(
        "GET",
        "/v1/sessions?attrs=%7B%22project_id%22%3A%22agentic-data%22%2C%22skill%22%3A%22review%22%7D",
        "",
    );
    assert_eq!(st_attr, 200, "{body_attr}");
    assert!(body_attr.contains("\"sessionId\":\"900\""), "{body_attr}");
    assert!(body_attr.contains("\"total\":1"), "{body_attr}");

    let (st_miss, body_miss) = s.route(
        "GET",
        "/v1/sessions?project_id=agentic-data&skill=other",
        "",
    );
    assert_eq!(st_miss, 200, "{body_miss}");
    assert!(body_miss.contains("\"items\":[]"), "{body_miss}");
    assert!(body_miss.contains("\"total\":0"), "{body_miss}");

    // 轮次：2 轮，首轮名取 input_text。
    let (st2, turns) = s.route("GET", "/v1/sessions/900/turns", "");
    assert_eq!(st2, 200, "{turns}");
    assert!(
        turns.contains("\"turnIndex\":0") && turns.contains("\"turnIndex\":1"),
        "{turns}"
    );
    assert!(turns.contains("对账户A做研判"), "{turns}");
    assert!(turns.contains("\"durMs\":2"), "首轮 2ms: {turns}");

    // trace span：trace 11 有 span，kind=agent。
    let (st3, trace) = s.route("GET", "/v1/traces/11", "");
    assert_eq!(st3, 200, "{trace}");
    assert!(
        trace.contains("\"kind\":\"agent\"") && trace.contains("风控研判"),
        "{trace}"
    );
    assert!(trace.contains("\"summary\""), "{trace}");
    assert!(
        trace.contains("\"logEvents\"")
            && trace.contains("读取 package.json")
            && trace.contains("\"eventType\":4")
            && trace.contains("\"call_site\":\"package-json\""),
        "{trace}"
    );

    // span 详情：晚物化大字段。
    let (st4, detail) = s.route("GET", "/v1/traces/11/spans/1", "");
    assert_eq!(st4, 200, "{detail}");
    assert!(detail.contains("触发规则R12"), "{detail}");
    assert!(
        detail.contains("\"logEvents\"") && detail.contains("读取 package.json"),
        "{detail}"
    );

    // 步骤流：带输入/输出文本一次给全。
    let (st5, steps) = s.route("GET", "/v1/traces/11/steps", "");
    assert_eq!(st5, 200, "{steps}");
    assert!(
        steps.contains("对账户A做研判") && steps.contains("触发规则R12"),
        "{steps}"
    );

    // 不存在的 trace → 404。
    assert_eq!(s.route("GET", "/v1/traces/999", "").0, 404);
}

#[test]
fn route_console_endpoints_are_tenant_isolated() {
    // 控制台详情端点也必须按 X-Tenant-Id 隔离，尤其是 input/output 大文本。
    let s = server();
    let t1 = r#"[
      {"trace_id":11,"span_id":1,"ts":1,"seq":1,"event_type":1,"ext_span_id":"11-1","session_id":900,"tenant_id":999,"agent_name":"租户一","input_text":"租户一问题"},
      {"trace_id":11,"span_id":1,"ts":2,"seq":2,"event_type":2,"ext_span_id":"11-1","session_id":900,"tenant_id":999,"status":0,"duration_ns":1000000,"output_text":"租户一答案"}
    ]"#;
    let t2 = r#"[
      {"trace_id":22,"span_id":1,"ts":1,"seq":1,"event_type":1,"ext_span_id":"22-1","session_id":900,"tenant_id":999,"agent_name":"租户二","input_text":"租户二机密"},
      {"trace_id":22,"span_id":1,"ts":2,"seq":2,"event_type":2,"ext_span_id":"22-1","session_id":900,"tenant_id":999,"status":0,"duration_ns":2000000,"output_text":"租户二答案"}
    ]"#;
    assert_eq!(
        s.route_with_tenant("POST", "/v1/ingest", t1, Some(1)).0,
        200
    );
    assert_eq!(
        s.route_with_tenant("POST", "/v1/ingest", t2, Some(2)).0,
        200
    );

    let sessions1 = s
        .route_with_tenant("GET", "/v1/sessions?cursor=0&limit=50", "", Some(1))
        .1;
    assert!(sessions1.contains("\"firstTraceId\":\"11\""), "{sessions1}");
    assert!(
        !sessions1.contains("\"firstTraceId\":\"22\""),
        "{sessions1}"
    );

    let turns1 = s
        .route_with_tenant("GET", "/v1/sessions/900/turns", "", Some(1))
        .1;
    assert!(
        turns1.contains("\"traceId\":\"11\"") && turns1.contains("租户一问题"),
        "{turns1}"
    );
    assert!(!turns1.contains("租户二机密"), "{turns1}");

    let (st_cross, body_cross) = s.route_with_tenant("GET", "/v1/traces/22", "", Some(1));
    assert_eq!(st_cross, 404, "tenant1 不能读 tenant2 trace: {body_cross}");
    assert_eq!(
        s.route_with_tenant("GET", "/v1/traces/22/spans/1", "", Some(1))
            .0,
        404
    );
    assert_eq!(
        s.route_with_tenant("GET", "/v1/traces/22/steps", "", Some(1))
            .0,
        404
    );

    let (st2, trace2) = s.route_with_tenant("GET", "/v1/traces/22", "", Some(2));
    assert_eq!(st2, 200, "{trace2}");
    let detail2 = s
        .route_with_tenant("GET", "/v1/traces/22/spans/1", "", Some(2))
        .1;
    assert!(
        detail2.contains("租户二答案") && !detail2.contains("租户一答案"),
        "{detail2}"
    );
}

#[test]
fn route_otlp_rejects_bad_body() {
    let s = server();
    assert_eq!(s.route("POST", "/v1/traces", "garbage").0, 400);
    assert_eq!(
        s.route("POST", "/v1/traces", r#"{"foo":1}"#).0,
        400,
        "缺 resourceSpans → 400"
    );
}

#[test]
fn route_rejects_bad_json_and_unknown() {
    let s = server();
    assert_eq!(s.route("POST", "/v1/ingest", "garbage").0, 400);
    assert_eq!(s.route("GET", "/nope", "").0, 404);
}

#[test]
fn auth_token_logic() {
    let s = server().with_auth_token("secret");
    assert!(!s.authorized(None), "无 token 拒绝");
    assert!(!s.authorized(Some("Bearer wrong")), "错 token 拒绝");
    assert!(s.authorized(Some("Bearer secret")), "对 token 放行");
    assert!(server().authorized(None), "未配置 token → 放行（开发）");
}

#[test]
fn oversized_body_rejected_without_oom() {
    // 声称 1TB body 但不发 —— 服务端必须 413,绝不去 vec![0u8; 1e12] 把自己撑死。
    let s = Arc::new(server().with_max_body(1024));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let h = std::thread::spawn(move || s.serve_n(&listener, 1));
    let mut c = TcpStream::connect(addr).unwrap();
    c.write_all(b"POST /v1/ingest HTTP/1.1\r\nHost: x\r\nContent-Length: 999999999999\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut resp = String::new();
    c.read_to_string(&mut resp).unwrap();
    assert!(resp.contains("413"), "{resp}");
    h.join().unwrap();
}

#[test]
fn auth_enforced_over_socket() {
    let s = Arc::new(server().with_auth_token("secret"));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let h = std::thread::spawn(move || s.serve_n(&listener, 2));
    // 无 token → 401
    let mut c = TcpStream::connect(addr).unwrap();
    c.write_all(b"GET /v1/traces HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut r = String::new();
    c.read_to_string(&mut r).unwrap();
    assert!(r.contains("401"), "{r}");
    // 带对 token → 200
    let mut c2 = TcpStream::connect(addr).unwrap();
    c2.write_all(b"GET /v1/traces HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer secret\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut r2 = String::new();
    c2.read_to_string(&mut r2).unwrap();
    assert!(r2.contains("200 OK"), "{r2}");
    h.join().unwrap();
}

#[cfg(feature = "gzip")]
#[test]
fn gzip_body_decompressed() {
    let s = Arc::new(server());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let h = std::thread::spawn(move || s.serve_n(&listener, 1));

    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(BATCH.as_bytes()).unwrap();
    let gz = enc.finish().unwrap();
    assert!(gz.len() < BATCH.len(), "确实压缩了");

    let mut c = TcpStream::connect(addr).unwrap();
    let header = format!(
        "POST /v1/ingest HTTP/1.1\r\nHost: x\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        gz.len()
    );
    c.write_all(header.as_bytes()).unwrap();
    c.write_all(&gz).unwrap();
    let mut resp = String::new();
    c.read_to_string(&mut resp).unwrap();
    assert!(resp.contains("\"ingested\":2"), "{resp}");
    h.join().unwrap();
}

#[test]
fn thread_pool_handles_concurrent_requests() {
    // 线程池：并发打 8 个请求,都成功(不串、不崩)。
    let s = Arc::new(server());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let me = Arc::clone(&s);
    std::thread::spawn(move || me.serve_pool(listener, 4));
    let mut handles = Vec::new();
    for _ in 0..8 {
        handles.push(std::thread::spawn(move || {
            let mut c = TcpStream::connect(addr).unwrap();
            c.write_all(b"GET /v1/traces HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
                .unwrap();
            let mut r = String::new();
            c.read_to_string(&mut r).unwrap();
            assert!(r.contains("200 OK"), "{r}");
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
}

#[test]
fn real_socket_roundtrip() {
    // 真 socket：起服务线程,客户端 POST 再 GET,验证字节真从一个连接搬到另一个。
    let s = server();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || s.serve_n(&listener, 2));

    // POST
    let mut c = TcpStream::connect(addr).unwrap();
    let req = format!(
        "POST /v1/ingest HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        BATCH.len(),
        BATCH
    );
    c.write_all(req.as_bytes()).unwrap();
    let mut resp = String::new();
    c.read_to_string(&mut resp).unwrap();
    assert!(
        resp.contains("200 OK") && resp.contains("\"ingested\":2"),
        "{resp}"
    );

    // GET
    let mut c2 = TcpStream::connect(addr).unwrap();
    c2.write_all(b"GET /v1/traces HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut resp2 = String::new();
    c2.read_to_string(&mut resp2).unwrap();
    assert!(resp2.contains("\"trace_id\":7"), "{resp2}");

    handle.join().unwrap();
}
