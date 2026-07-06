    #[test]
    fn route_loop_and_task_read_models() {
        let s = server();
        let batch = r#"[
          {
            "trace_id":201,
            "span_id":1,
            "ts":100,
            "seq":1,
            "event_type":2,
            "ext_span_id":"201-1",
            "session_id":8101,
            "status":0,
            "duration_ns":10,
            "tool_name":"builder",
            "input_tokens":5,
            "output_tokens":10,
            "total_tokens":15,
            "cost_usd_nanos":1000,
            "attrs":{"project_id":"agentic-data","skill":"builder","mode":"auto","task_fingerprint":"npm-native-packaging","loop_id":"loop-a","harness_version":"h1","validation_status":"pass","stop_reason":"goal_met","phase":"verify","validator":"npm test"}
          },
          {
            "trace_id":202,
            "span_id":1,
            "ts":110,
            "seq":1,
            "event_type":2,
            "ext_span_id":"202-1",
            "session_id":8102,
            "status":1,
            "duration_ns":20,
            "tool_name":"builder",
            "input_tokens":7,
            "output_tokens":8,
            "total_tokens":15,
            "cost_usd_nanos":2000,
            "attrs":{"project_id":"agentic-data","skill":"builder","mode":"auto","task_fingerprint":"npm-native-packaging","loop_id":"loop-a","harness_version":"h1","validation_status":"fail","stop_reason":"error","phase":"verify","validator":"npm test"}
          },
          {
            "trace_id":203,
            "span_id":1,
            "ts":120,
            "seq":1,
            "event_type":2,
            "ext_span_id":"203-1",
            "session_id":8103,
            "status":0,
            "duration_ns":30,
            "tool_name":"planner",
            "attrs":{"project_id":"agentic-data","skill":"review","mode":"manual","task_fingerprint":"other-task","loop_id":"loop-b","validation_status":"pass","phase":"plan"}
          }
        ]"#;
        let (status, body) = s.route_with_tenant("POST", "/v1/ingest", batch, Some(1));
        assert_eq!(status, 200, "{body}");

        let (status, loops) = s.route_with_tenant(
            "GET",
            "/v1/loops?taskFingerprint=npm-native-packaging",
            "",
            Some(1),
        );
        assert_eq!(status, 200, "{loops}");
        assert!(loops.contains(r#""total":1"#), "{loops}");
        assert!(loops.contains(r#""loopId":"loop-a""#), "{loops}");
        assert!(loops.contains(r#""traceCount":2"#), "{loops}");
        assert!(loops.contains(r#""errorCount":1"#), "{loops}");
        assert!(
            loops.contains(r#""taskFingerprint":"npm-native-packaging""#),
            "{loops}"
        );
        assert!(loops.contains(r#""phases":["verify"]"#), "{loops}");

        let (status, loop_detail) = s.route_with_tenant("GET", "/v1/loops/loop-a", "", Some(1));
        assert_eq!(status, 200, "{loop_detail}");
        assert!(
            loop_detail.contains(r#""summary":{"loopId":"loop-a""#),
            "{loop_detail}"
        );
        assert!(loop_detail.contains(r#""traces":["#), "{loop_detail}");
        assert!(loop_detail.contains(r#""spans":["#), "{loop_detail}");
        assert!(loop_detail.contains(r#""traceId":"201""#), "{loop_detail}");
        assert!(loop_detail.contains(r#""traceId":"202""#), "{loop_detail}");

        let (status, hidden) = s.route_with_tenant("GET", "/v1/loops/loop-a", "", Some(2));
        assert_eq!(status, 404, "{hidden}");

        let (status, task_traces) = s.route_with_tenant(
            "GET",
            "/v1/tasks/npm-native-packaging/traces?validationStatus=pass",
            "",
            Some(1),
        );
        assert_eq!(status, 200, "{task_traces}");
        assert!(task_traces.contains(r#""total":1"#), "{task_traces}");
        assert!(task_traces.contains(r#""traceId":"201""#), "{task_traces}");
        assert!(!task_traces.contains(r#""traceId":"202""#), "{task_traces}");
        assert!(
            task_traces.contains(r#""validation_status":"pass""#),
            "{task_traces}"
        );
    }
    #[test]
    fn route_trace_diff_compares_trajectories() {
        // Trace diff 是底层证据 API：比较 route、逐步变化和成本/时延 delta，不替业务自动判优。
        let s = server();
        let batch = r#"[
          {
            "trace_id":301,
            "span_id":1,
            "ts":100,
            "seq":1,
            "event_type":2,
            "ext_span_id":"301-1",
            "status":0,
            "duration_ns":10,
            "tool_name":"planner",
            "input_tokens":5,
            "output_tokens":10,
            "total_tokens":15,
            "cost_usd_nanos":1000,
            "input_text":"先读 package",
            "output_text":"只跑相关测试",
            "attrs":{"project_id":"agentic-data","skill":"review","mode":"auto","task_fingerprint":"diff-task","loop_id":"loop-diff","phase":"plan","validation_status":"pass"}
          },
          {
            "trace_id":302,
            "span_id":1,
            "ts":100,
            "seq":1,
            "event_type":2,
            "ext_span_id":"302-1",
            "status":0,
            "duration_ns":8,
            "tool_name":"planner",
            "input_tokens":4,
            "output_tokens":8,
            "total_tokens":12,
            "cost_usd_nanos":500,
            "input_text":"先读 package",
            "output_text":"只跑相关测试",
            "attrs":{"project_id":"agentic-data","skill":"review","mode":"auto","task_fingerprint":"diff-task","loop_id":"loop-diff","phase":"plan","validation_status":"pass"}
          },
          {
            "trace_id":302,
            "span_id":2,
            "ts":120,
            "seq":1,
            "event_type":2,
            "ext_span_id":"302-2",
            "status":1,
            "duration_ns":20,
            "tool_name":"tester",
            "input_tokens":2,
            "output_tokens":3,
            "total_tokens":5,
            "cost_usd_nanos":2000,
            "output_text":"npm test failed",
            "attrs":{"project_id":"agentic-data","skill":"review","mode":"auto","task_fingerprint":"diff-task","loop_id":"loop-diff","phase":"verify","validation_status":"fail"}
          }
        ]"#;
        let (status, body) = s.route_with_tenant("POST", "/v1/ingest", batch, Some(1));
        assert_eq!(status, 200, "{body}");

        let (status, diff) = s.route_with_tenant(
            "POST",
            "/v1/traces/diff",
            r#"{"leftTraceId":301,"rightTraceId":302}"#,
            Some(1),
        );
        assert_eq!(status, 200, "{diff}");
        assert!(diff.contains(r#""left":{"traceId":"301""#), "{diff}");
        assert!(diff.contains(r#""right":{"traceId":"302""#), "{diff}");
        assert!(diff.contains(r#""delta":{"spanCount":1"#), "{diff}");
        assert!(diff.contains(r#""errorCount":1"#), "{diff}");
        assert!(diff.contains(r#""costUsdNanos":1500"#), "{diff}");
        assert!(
            diff.contains(r#""trajectory":{"left":{"signature":"fnv1a64:"#),
            "{diff}"
        );
        assert!(diff.contains(r#""same":false"#), "{diff}");
        assert!(diff.contains(r#""tool:planner|phase:plan""#), "{diff}");
        assert!(diff.contains(r#""tool:tester|phase:verify""#), "{diff}");
        assert!(diff.contains(r#""routes":{"left":["#), "{diff}");
        assert!(diff.contains(r#""steps":["#), "{diff}");
        assert!(diff.contains(r#""status":"changed""#), "{diff}");
        assert!(diff.contains(r#""status":"right_only""#), "{diff}");
        assert!(diff.contains(r#""durationNs""#), "{diff}");
        assert!(diff.contains(r#""toolName":"tester""#), "{diff}");
        assert!(
            diff.contains(r#""outputPreview":"npm test failed""#),
            "{diff}"
        );

        let (hidden_status, hidden) = s.route_with_tenant(
            "POST",
            "/v1/traces/diff",
            r#"{"leftTraceId":301,"rightTraceId":302}"#,
            Some(2),
        );
        assert_eq!(hidden_status, 404, "{hidden}");
    }
    #[test]
    fn route_trajectory_groups_rank_stable_successful_paths() {
        let s = server();
        let batch = r#"[
          {
            "trace_id":401,
            "span_id":1,
            "ts":100,
            "seq":1,
            "event_type":2,
            "ext_span_id":"401-1",
            "status":0,
            "duration_ns":10,
            "tool_name":"planner",
            "input_tokens":10,
            "output_tokens":5,
            "total_tokens":15,
            "cost_usd_nanos":1000,
            "attrs":{"project_id":"agentic-data","skill":"review","mode":"auto","task_fingerprint":"trajectory-task","phase":"plan"}
          },
          {
            "trace_id":401,
            "span_id":2,
            "ts":120,
            "seq":1,
            "event_type":2,
            "ext_span_id":"401-2",
            "status":0,
            "duration_ns":20,
            "tool_name":"tester",
            "input_tokens":20,
            "output_tokens":10,
            "total_tokens":30,
            "cost_usd_nanos":2000,
            "attrs":{"project_id":"agentic-data","skill":"review","mode":"auto","task_fingerprint":"trajectory-task","phase":"verify","validator":"npm test"}
          },
          {
            "trace_id":402,
            "span_id":1,
            "ts":200,
            "seq":1,
            "event_type":2,
            "ext_span_id":"402-1",
            "status":0,
            "duration_ns":8,
            "tool_name":"planner",
            "input_tokens":8,
            "output_tokens":4,
            "total_tokens":12,
            "cost_usd_nanos":800,
            "attrs":{"project_id":"agentic-data","skill":"review","mode":"auto","task_fingerprint":"trajectory-task","phase":"plan"}
          },
          {
            "trace_id":402,
            "span_id":2,
            "ts":220,
            "seq":1,
            "event_type":2,
            "ext_span_id":"402-2",
            "status":0,
            "duration_ns":16,
            "tool_name":"tester",
            "input_tokens":16,
            "output_tokens":8,
            "total_tokens":24,
            "cost_usd_nanos":1600,
            "attrs":{"project_id":"agentic-data","skill":"review","mode":"auto","task_fingerprint":"trajectory-task","phase":"verify","validator":"npm test"}
          },
          {
            "trace_id":403,
            "span_id":1,
            "ts":300,
            "seq":1,
            "event_type":2,
            "ext_span_id":"403-1",
            "status":1,
            "duration_ns":50,
            "tool_name":"planner",
            "input_tokens":50,
            "output_tokens":5,
            "total_tokens":55,
            "cost_usd_nanos":5000,
            "attrs":{"project_id":"agentic-data","skill":"review","mode":"auto","task_fingerprint":"trajectory-task","phase":"plan","validation_status":"fail"}
          }
        ]"#;
        let (status, body) = s.route_with_tenant("POST", "/v1/ingest", batch, Some(1));
        assert_eq!(status, 200, "{body}");

        for (trace_id, annotation_score, dataset_score) in [(401, 960, 950), (402, 920, 930)] {
            let annotation = format!(
                r#"{{"traceId":{},"label":"best_path","score":{},"source":"human","projectId":"agentic-data"}}"#,
                trace_id, annotation_score
            );
            let (status, body) =
                s.route_with_tenant("POST", "/v1/annotations", &annotation, Some(1));
            assert_eq!(status, 200, "{body}");
            let dataset = format!(
                r#"{{"datasetId":"best-path-regression","itemId":"case-{}","traceId":{},"label":"pass","score":{},"projectId":"agentic-data"}}"#,
                trace_id, trace_id, dataset_score
            );
            let (status, body) =
                s.route_with_tenant("POST", "/v1/dataset-associations", &dataset, Some(1));
            assert_eq!(status, 200, "{body}");
        }

        let (status, groups) = s.route_with_tenant(
            "POST",
            "/v1/trajectory-groups",
            r#"{"filter":{"taskFingerprint":"trajectory-task"},"sort":"best","limit":10}"#,
            Some(1),
        );
        assert_eq!(status, 200, "{groups}");
        assert!(groups.contains(r#""total":2"#), "{groups}");
        assert!(groups.contains(r#""traceTotal":3"#), "{groups}");
        assert!(groups.contains(r#""spanTotal":5"#), "{groups}");
        assert!(groups.contains(r#""traceCount":2"#), "{groups}");
        assert!(groups.contains(r#""successCount":2"#), "{groups}");
        assert!(groups.contains(r#""successRate":1.000000"#), "{groups}");
        assert!(groups.contains(r#""qualityScore":960"#), "{groups}");
        assert!(
            groups.contains(r#""steps":["tool:planner|phase:plan","tool:tester|phase:verify|validator:npm_test"]"#),
            "{groups}"
        );
        assert!(
            groups.contains(r#""annotation":{"count":2,"avg":940"#),
            "{groups}"
        );
        assert!(
            groups.contains(r#""dataset":{"count":2,"avg":940"#),
            "{groups}"
        );
        assert!(
            groups.contains(r#""examples":[{"traceId":"401""#),
            "{groups}"
        );

        let (status, hidden) = s.route_with_tenant(
            "POST",
            "/v1/trajectory-groups",
            r#"{"filter":{"taskFingerprint":"trajectory-task"}}"#,
            Some(2),
        );
        assert_eq!(status, 200, "{hidden}");
        assert!(hidden.contains(r#""traceTotal":0"#), "{hidden}");
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
