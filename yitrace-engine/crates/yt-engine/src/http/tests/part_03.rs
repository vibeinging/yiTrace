    #[test]
    fn route_trace_product_apis_support_attrs_snapshot_and_batch_spans() {
        let s = server();
        let batch = r#"[
          {"trace_id":31,"span_id":1,"ts":1,"seq":1,"event_type":1,"ext_span_id":"31-1","session_id":123,"agent_name":"planner","input_text":"用户反复问同一个问题"},
          {"trace_id":31,"span_id":1,"ts":2,"seq":2,"event_type":2,"ext_span_id":"31-1","session_id":123,"status":0,"duration_ns":1000,"output_text":"拆出候选路径"},
          {"trace_id":31,"span_id":2,"parent_span_id":1,"ts":3,"seq":1,"event_type":1,"ext_span_id":"31-2","session_id":123,"tool_name":"planner","model":"qwen","input_text":"最优路径输入","attrs":{"project_id":"agentic-data","connection_ids":["conn-a","conn-b"],"path_memory_id":"pm-1"}},
          {"trace_id":31,"span_id":2,"parent_span_id":1,"ts":4,"seq":2,"event_type":4,"ext_span_id":"31-2","session_id":123,"logs":["选择最优路径"],"attrs":{"call_site":"planner.ts:9"}},
          {"trace_id":31,"span_id":2,"parent_span_id":1,"ts":5,"seq":3,"event_type":2,"ext_span_id":"31-2","session_id":123,"status":0,"duration_ns":2000,"output_text":"最优路径输出"}
        ]"#;
        assert_eq!(s.route("POST", "/v1/ingest", batch).0, 200);

        let (st_search, search) = s.route(
            "POST",
            "/v1/search",
            r#"{"text":"最优路径","filter":{"attrs":{"connection_ids":"conn-a"}}}"#,
        );
        assert_eq!(st_search, 200, "{search}");
        assert!(search.contains("\"trace_id\":31"), "{search}");

        let (st_trace_search, trace_search) = s.route(
            "POST",
            "/v1/trace-search",
            r#"{"text":"最优","limit":1,"sort":"duration","order":"desc","filter":{"tool_name":"planner","attrs":{"connection_ids":"conn-a"}}}"#,
        );
        assert_eq!(st_trace_search, 200, "{trace_search}");
        assert!(trace_search.contains("\"total\":1"), "{trace_search}");
        assert!(trace_search.contains("\"spanId\":\"2\""), "{trace_search}");
        assert!(
            trace_search.contains("\"inputText\":{\"preview\""),
            "{trace_search}"
        );
        assert!(
            trace_search.contains("\"fields\":{\"project_id\":\"agentic-data\""),
            "{trace_search}"
        );

        let (st_traces, traces) = s.route(
            "GET",
            "/v1/traces?attrs=%7B%22connection_ids%22%3A%22conn-a%22%7D",
            "",
        );
        assert_eq!(st_traces, 200, "{traces}");
        assert!(traces.contains("\"trace_id\":31"), "{traces}");
        assert!(traces.contains("\"fields\":{"), "{traces}");
        assert!(
            traces.contains("\"project_id\":\"agentic-data\""),
            "{traces}"
        );
        assert!(
            traces.contains("\"connection_ids\":[\"conn-a\",\"conn-b\"]"),
            "{traces}"
        );

        let (st_trace, trace) = s.route("GET", "/v1/traces/31", "");
        assert_eq!(st_trace, 200, "{trace}");
        assert!(trace.contains("\"spanOrdinal\":0"), "{trace}");
        assert!(trace.contains("\"siblingOrdinal\":0"), "{trace}");
        assert!(trace.contains("\"eventOrdinal\":0"), "{trace}");

        let (st_page, page) = s.route("GET", "/v1/traces/31/spans?cursor=0&limit=1", "");
        assert_eq!(st_page, 200, "{page}");
        assert!(page.contains("\"total\":2"), "{page}");
        assert!(page.contains("\"nextCursor\":1"), "{page}");
        assert!(page.contains("\"full\":null"), "{page}");

        let (st_batch, batch_detail) = s.route(
            "POST",
            "/v1/traces/31/spans/batch",
            r#"{"spanIds":[2],"includeFull":true}"#,
        );
        assert_eq!(st_batch, 200, "{batch_detail}");
        assert!(batch_detail.contains("\"spanId\":\"2\""), "{batch_detail}");
        assert!(
            batch_detail.contains("\"full\":\"最优路径输入\""),
            "{batch_detail}"
        );
        assert!(
            batch_detail.contains("\"contentHash\":\"fnv1a64:"),
            "{batch_detail}"
        );

        let (st_snapshot, snapshot) = s.route("GET", "/v1/traces/31/snapshot", "");
        assert_eq!(st_snapshot, 200, "{snapshot}");
        assert!(
            snapshot.contains("\"snapshotHash\":\"fnv1a64:"),
            "{snapshot}"
        );
        assert!(snapshot.contains("\"full\":\"最优路径输出\""), "{snapshot}");
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

    #[test]
    fn remote_shard_client_reads_status_and_search_hits_over_socket() {
        let coord = WriteCoordinator::new(Arc::new(InMemorySegmentStore::default()));
        let records = parse_wire_batch(
            r#"[
          {"trace_id":701,"span_id":1,"ts":100,"seq":1,"event_type":1,"ext_span_id":"701-1","agent_name":"remote-agent","input_text":"疑似盗刷，需要检查","attrs":{"project_id":"remote-client","skill":"review"}},
          {"trace_id":701,"span_id":1,"ts":120,"seq":2,"event_type":2,"ext_span_id":"701-1","status":0,"duration_ns":20,"output_text":"通过"}
        ]"#,
        )
        .unwrap();

        let server = HttpIngestServer::new(Arc::clone(&coord));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || server.serve_n(&listener, 4));

        let client = RemoteShardClient::new(format!("http://{addr}"))
            .with_timeout(std::time::Duration::from_secs(2));
        client
            .ingest_records_for_tenant(records, Some(7))
            .expect("remote ingest should use shard HTTP API");
        let status = client.replication_status();
        assert!(
            status.committed_tail > 0 || status.memtable_rows > 0,
            "remote status should reflect the shard state: {:?}",
            status
        );

        let request = search_json_request(
            r#"{"text":"盗刷","k":10,"filter":{"attrs":{"project_id":"remote-client","skill":"review"}}}"#,
            Some(7),
        )
        .unwrap();
        let hits = client.search_hits(&request).unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
        let (span, score) = &hits[0];
        assert_eq!(span.trace_id, 701);
        assert_eq!(span.span_id, 1);
        assert!(*score > 0.0, "score should be preserved from remote hit");
        assert_eq!(span.status, Some(0));
        assert_eq!(span.duration_ns, Some(20));
        assert_eq!(span.agent_name.as_deref(), Some("remote-agent"));
        assert_eq!(span.project_id.as_deref(), Some("\"remote-client\""));
        assert_eq!(span.attrs.get("skill").map(String::as_str), Some("\"review\""));

        let hidden = search_json_request(r#"{"text":"盗刷","k":10}"#, Some(8)).unwrap();
        let misses = client.search_hits(&hidden).unwrap();
        assert!(misses.is_empty(), "{misses:?}");

        handle.join().unwrap();
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
    fn remote_gateway_server_routes_real_socket_requests() {
        let shard = Arc::new(server());
        let shard_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let shard_addr = shard_listener.local_addr().unwrap();
        let shard_server = Arc::clone(&shard);
        std::thread::spawn(move || shard_server.serve_pool(shard_listener, 4));

        let gateway = RemoteShardGateway::new(vec![format!("http://{shard_addr}")]).unwrap();
        let gateway = RemoteGatewayServer::new(gateway);
        let gateway_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let gateway_addr = gateway_listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || gateway.serve_n(&gateway_listener, 3));

        let (status, cluster) =
            gateway_socket_request(gateway_addr, "GET", "/v1/cluster/shards", "", None, None);
        assert_eq!(status, 200, "{cluster}");
        assert!(cluster.contains(r#""mode":"process_gateway""#), "{cluster}");
        assert!(cluster.contains(r#""shardCount":1"#), "{cluster}");

        let batch = r#"[
          {"trace_id":8701,"span_id":1,"session_id":8701,"ts":1,"seq":1,"event_type":1,"ext_span_id":"8701-1","input_text":"gateway production server smoke","attrs":{"project_id":"gateway-entry","skill":"deploy"}},
          {"trace_id":8701,"span_id":1,"session_id":8701,"ts":2,"seq":2,"event_type":2,"ext_span_id":"8701-1","status":0,"duration_ns":10,"output_text":"gateway server ok"}
        ]"#;
        let (status, body) =
            gateway_socket_request(gateway_addr, "POST", "/v1/ingest", batch, Some(3), None);
        assert_eq!(status, 200, "{body}");
        assert!(body.contains(r#""ingested":2"#), "{body}");

        let query = r#"{"text":"production server","k":5,"filter":{"attrs":{"project_id":"gateway-entry"}}}"#;
        let (status, body) =
            gateway_socket_request(gateway_addr, "POST", "/v1/search", query, Some(3), None);
        assert_eq!(status, 200, "{body}");
        assert!(body.contains(r#""trace_id":8701"#), "{body}");
        handle.join().unwrap();
    }

    #[test]
    fn remote_gateway_server_enforces_auth_and_body_limit() {
        let gateway = RemoteShardGateway::new(vec!["127.0.0.1:1".to_string()]).unwrap();
        let gateway = RemoteGatewayServer::new(gateway)
            .with_auth_token("secret")
            .with_max_body(4);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || gateway.serve_n(&listener, 2));

        let (status, body) =
            gateway_socket_request(addr, "GET", "/v1/cluster/shards", "", None, None);
        assert_eq!(status, 401, "{body}");
        assert!(body.contains("unauthorized"), "{body}");

        let (status, body) = gateway_socket_request(
            addr,
            "POST",
            "/v1/ingest",
            r#"{"too":"large"}"#,
            None,
            Some("secret"),
        );
        assert_eq!(status, 413, "{body}");
        assert!(body.contains("body too large"), "{body}");
        handle.join().unwrap();
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

    fn gateway_socket_request(
        addr: std::net::SocketAddr,
        method: &str,
        path: &str,
        body: &str,
        tenant: Option<u64>,
        token: Option<&str>,
    ) -> (u16, String) {
        let mut stream = TcpStream::connect(addr).unwrap();
        let tenant_header = tenant
            .map(|id| format!("X-Tenant-Id: {id}\r\n"))
            .unwrap_or_default();
        let auth_header = token
            .map(|token| format!("Authorization: Bearer {token}\r\n"))
            .unwrap_or_default();
        let req = format!(
            "{method} {path} HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{tenant_header}{auth_header}Connection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(req.as_bytes()).unwrap();
        let mut resp = String::new();
        stream.read_to_string(&mut resp).unwrap();
        let status = resp
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse::<u16>().ok())
            .unwrap_or(0);
        let body = resp
            .split_once("\r\n\r\n")
            .map(|(_, body)| body.to_string())
            .unwrap_or_default();
        (status, body)
    }
