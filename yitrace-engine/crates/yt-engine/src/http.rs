//! 极小 HTTP/1.1 摄入+查询服务（只用 std::net，零依赖、离线可编译）。
//!
//! 路由：
//!   POST /v1/ingest  —— body 是 SDK 线格式 JSON 批 → `parse_wire_batch` → `ingest_wire`
//!   POST /v1/traces  —— OTLP/HTTP 标准端点：OTLP/OpenInference trace → `ingest_otlp`（生态入口）
//!   GET  /v1/traces  —— 返回 trace 列表（JSON）
//!   POST /v1/search  —— 中文检索 + 可选属性过滤(agent/状态/时间) → `search_text_attr`（产品差异化出口）
//!
//! 这是 SDK→引擎跨进程的最后一层。真要上量/上 TLS，换 axum/hyper 即可，路由逻辑（`route`）不变。
//! OTLP 走「OTLP→WireRecord 适配器」（`otlp.rs`）接到同一个 `ingest_wire` 边界。
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use crate::{parse_wire_batch, TraceQuery, WriteCoordinator};

pub struct HttpIngestServer {
    coord: Arc<WriteCoordinator>,
    /// 鉴权 token。None = 不鉴权（仅限本机开发）。Some = 要求 `Authorization: Bearer <token>`。
    auth_token: Option<String>,
    /// 请求体上限（字节）。超了直接 413，**绝不按 Content-Length 预分配** —— 堵 OOM 拒绝服务。
    max_body: usize,
}

impl HttpIngestServer {
    pub fn new(coord: Arc<WriteCoordinator>) -> Self {
        Self { coord, auth_token: None, max_body: 16 << 20 } // 默认 16 MiB
    }

    /// 要求 Bearer token 鉴权（金融政企私有化最低门槛）。
    pub fn with_auth_token(mut self, token: impl Into<String>) -> Self {
        self.auth_token = Some(token.into());
        self
    }

    pub fn with_max_body(mut self, bytes: usize) -> Self {
        self.max_body = bytes;
        self
    }

    /// 鉴权判定：未配 token 则放行；配了则要求 `Authorization: Bearer <token>` 精确匹配。
    fn authorized(&self, auth_header: Option<&str>) -> bool {
        match &self.auth_token {
            None => true,
            Some(tok) => auth_header
                .and_then(|h| h.trim().strip_prefix("Bearer "))
                .map_or(false, |got| got.trim() == tok),
        }
    }

    /// 永久 accept 循环（给二进制用）。
    pub fn serve(&self, listener: &TcpListener) {
        for stream in listener.incoming().flatten() {
            self.handle(stream);
        }
    }

    /// 只处理 n 个连接后返回（给测试用，可 join）。
    pub fn serve_n(&self, listener: &TcpListener, n: usize) {
        for _ in 0..n {
            if let Ok((stream, _)) = listener.accept() {
                self.handle(stream);
            }
        }
    }

    /// 固定大小线程池 accept（生产用）：`workers` 个工作线程从 channel 取连接处理，
    /// accept 循环在调用线程。线程数有界 → 不会被高并发连接打爆（无界 spawn 本身是 DoS 面）。
    pub fn serve_pool(self: Arc<Self>, listener: TcpListener, workers: usize) {
        let (tx, rx) = mpsc::channel::<TcpStream>();
        let rx = Arc::new(Mutex::new(rx));
        for _ in 0..workers.max(1) {
            let rx = Arc::clone(&rx);
            let me = Arc::clone(&self);
            thread::spawn(move || loop {
                let next = rx.lock().unwrap().recv();
                match next {
                    Ok(stream) => me.handle(stream),
                    Err(_) => break, // 发送端关闭
                }
            });
        }
        for stream in listener.incoming().flatten() {
            if tx.send(stream).is_err() {
                break;
            }
        }
    }

    fn handle(&self, mut stream: TcpStream) {
        let Ok(clone) = stream.try_clone() else { return };
        let mut reader = BufReader::new(clone);

        let mut line = String::new();
        if reader.read_line(&mut line).is_err() {
            return;
        }
        let mut parts = line.split_whitespace();
        let method = parts.next().unwrap_or("").to_string();
        let path = parts.next().unwrap_or("").to_string();

        let mut content_length = 0usize;
        let mut auth: Option<String> = None;
        let mut encoding: Option<String> = None;
        loop {
            let mut h = String::new();
            if reader.read_line(&mut h).unwrap_or(0) == 0 {
                break;
            }
            if h == "\r\n" || h == "\n" {
                break;
            }
            let hl = h.to_ascii_lowercase();
            if let Some(v) = hl.strip_prefix("content-length:") {
                content_length = v.trim().parse().unwrap_or(0);
            } else if hl.starts_with("authorization:") {
                // 取原始大小写的值（token 大小写敏感）
                auth = h.splitn(2, ':').nth(1).map(|s| s.trim().to_string());
            } else if let Some(v) = hl.strip_prefix("content-encoding:") {
                encoding = Some(v.trim().to_string());
            }
        }

        // ① 请求体上限：超了直接 413，**绝不按 Content-Length 预分配** → 堵 OOM 拒绝服务。
        if content_length > self.max_body {
            self.respond(&mut stream, 413, r#"{"error":"body too large"}"#);
            self.audit(&method, &path, 413, content_length);
            return;
        }
        // ② 鉴权：未带/错 token → 401，且不读 body。
        if !self.authorized(auth.as_deref()) {
            self.respond(&mut stream, 401, r#"{"error":"unauthorized"}"#);
            self.audit(&method, &path, 401, content_length);
            return;
        }

        let mut body_buf = vec![0u8; content_length];
        if content_length > 0 && reader.read_exact(&mut body_buf).is_err() {
            return;
        }
        // gzip 解压（带防炸弹上限）。未开 gzip feature 且 body 是 gzip → 415。
        let body_bytes = match self.decode_body(encoding.as_deref(), body_buf) {
            Ok(b) => b,
            Err(code) => {
                self.respond(&mut stream, code, r#"{"error":"bad or unsupported body encoding"}"#);
                self.audit(&method, &path, code, content_length);
                return;
            }
        };
        let body = String::from_utf8_lossy(&body_bytes).into_owned();

        let (status, resp_body) = self.route(&method, &path, &body);
        self.respond(&mut stream, status, &resp_body);
        self.audit(&method, &path, status, content_length);
    }

    fn respond(&self, stream: &mut TcpStream, status: u16, body: &str) {
        let reason = match status {
            200 => "OK",
            400 => "Bad Request",
            401 => "Unauthorized",
            404 => "Not Found",
            413 => "Payload Too Large",
            415 => "Unsupported Media Type",
            _ => "Error",
        };
        let resp = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = stream.write_all(resp.as_bytes());
        let _ = stream.flush();
    }

    /// gzip 解压（feature = gzip）。带防炸弹上限：解压后超 max_body → 413。
    #[cfg(feature = "gzip")]
    fn decode_body(&self, encoding: Option<&str>, raw: Vec<u8>) -> Result<Vec<u8>, u16> {
        if encoding.map_or(false, |e| e.eq_ignore_ascii_case("gzip")) {
            use std::io::Read;
            let mut out = Vec::new();
            // take(max_body+1)：限制解压输出,防小包炸大（gzip bomb）。
            let mut dec = flate2::read::GzDecoder::new(&raw[..]).take(self.max_body as u64 + 1);
            if dec.read_to_end(&mut out).is_err() {
                return Err(400);
            }
            if out.len() > self.max_body {
                return Err(413);
            }
            return Ok(out);
        }
        Ok(raw)
    }

    /// 未编译 gzip feature：gzip body 直接 415（不静默当原文，避免误判）。
    #[cfg(not(feature = "gzip"))]
    fn decode_body(&self, encoding: Option<&str>, raw: Vec<u8>) -> Result<Vec<u8>, u16> {
        if encoding.map_or(false, |e| e.eq_ignore_ascii_case("gzip")) {
            return Err(415);
        }
        Ok(raw)
    }

    /// ③ 审计留痕（等保三级硬要求）。骨架打到 stderr；真实实现落持久、防篡改的审计日志
    /// （含主体身份/源 IP/时间戳/操作/结果），并接入 SIEM。
    fn audit(&self, method: &str, path: &str, status: u16, body_len: usize) {
        eprintln!("[AUDIT] {method} {path} -> {status} ({body_len}B)");
    }

    /// 纯路由（无 socket，便于单测）。返回 (status, json_body)。
    pub fn route(&self, method: &str, path: &str, body: &str) -> (u16, String) {
        match (method, path) {
            ("POST", "/v1/ingest") => match parse_wire_batch(body) {
                Ok(recs) => {
                    let n = recs.len();
                    self.coord.ingest_wire(recs);
                    (200, format!(r#"{{"ingested":{n}}}"#))
                }
                Err(e) => (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
            },
            // OTLP/HTTP 标准 trace 端点（生态入口）：OpenTelemetry / OpenInference 埋点直接 POST 到这里。
            // 与下面 GET /v1/traces 同路径不同方法,各管摄入/查询。
            ("POST", "/v1/traces") => match self.coord.ingest_otlp(body) {
                Ok(_) => (200, r#"{"partialSuccess":{}}"#.to_string()), // OTLP 约定的成功响应体
                Err(e) => (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
            },
            ("GET", "/v1/traces") => (200, self.traces_json()),
            // 检索端点（产品差异化的出口）：中文 BM25 + 可选属性过滤(agent/状态/时间/trace)。
            ("POST", "/v1/search") => self.search_json(body),
            _ => (404, r#"{"error":"not found"}"#.to_string()),
        }
    }

    /// 处理 `POST /v1/search`：body = `{"text":"盗刷","vector":[..],"k":10,"filter":{"agent_name":"风控"}}`。
    /// 按给了什么自动选检索路:只 text→中文检索;只 vector→找相似;两个都给→混合(RRF)。都按 filter 过滤。
    fn search_json(&self, body: &str) -> (u16, String) {
        use crate::wire::{field, parse, Json};
        let v = match parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let text = field(&v, "text").and_then(Json::as_str).unwrap_or("");
        let k = field(&v, "k").and_then(Json::as_u64).unwrap_or(10) as usize;
        let vector: Vec<f32> = field(&v, "vector")
            .map(|j| j.as_array().iter().filter_map(Json::as_f32).collect())
            .unwrap_or_default();
        let mut filter = crate::SearchFilter::default();
        if let Some(f) = field(&v, "filter") {
            filter.trace_id = field(f, "trace_id").and_then(Json::as_u64);
            filter.agent_name = field(f, "agent_name").and_then(Json::as_str).map(|s| s.to_string());
            filter.status = field(f, "status").and_then(Json::as_u64).map(|x| x as u8);
            filter.time_from = field(f, "time_from").and_then(Json::as_i64);
            filter.time_to = field(f, "time_to").and_then(Json::as_i64);
        }

        let snap = self.coord.pin_snapshot();
        let hits = match (!text.is_empty(), !vector.is_empty()) {
            (true, true) => self.coord.search_hybrid_attr(&snap, text, &vector, k, &filter), // 混合
            (false, true) => self.coord.search_similar_attr(&snap, &vector, k, &filter),     // 找相似
            _ => self.coord.search_text_attr(&snap, text, k, &filter),                       // 中文检索
        };
        let items: Vec<String> = hits
            .iter()
            .map(|(s, score)| {
                let logs: Vec<String> = s.logs.iter().map(|l| format!("\"{}\"", json_escape(l))).collect();
                format!(
                    r#"{{"trace_id":{},"span_id":{},"score":{:.4},"status":{},"duration_ns":{},"agent_name":{},"logs":[{}]}}"#,
                    s.trace_id,
                    s.span_id,
                    score,
                    s.status.map_or("null".to_string(), |x| x.to_string()),
                    s.duration_ns.map_or("null".to_string(), |x| x.to_string()),
                    s.agent_name.as_ref().map_or("null".to_string(), |a| format!("\"{}\"", json_escape(a))),
                    logs.join(",")
                )
            })
            .collect();
        (200, format!("[{}]", items.join(",")))
    }

    fn traces_json(&self) -> String {
        let snap = self.coord.pin_snapshot();
        let traces = self.coord.list_traces(&snap, &TraceQuery::all());
        let items: Vec<String> = traces
            .iter()
            .map(|t| {
                format!(
                    r#"{{"trace_id":{},"span_count":{},"total_duration_ns":{},"max_duration_ns":{},"error_count":{},"total_input_tokens":{},"total_output_tokens":{}}}"#,
                    t.trace_id,
                    t.span_count,
                    t.total_duration_ns,
                    t.max_duration_ns,
                    t.error_count,
                    t.total_input_tokens,
                    t.total_output_tokens
                )
            })
            .collect();
        format!("[{}]", items.join(","))
    }
}

/// 极小 JSON 字符串转义（响应里嵌中文日志/agent 名时用）。中文 UTF-8 原样,只转义 `"` `\` 和控制符。
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InMemorySegmentStore;

    fn server() -> HttpIngestServer {
        HttpIngestServer::new(WriteCoordinator::new(Arc::new(InMemorySegmentStore::default())))
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
        assert!(body.contains("\"trace_id\":99"), "traceId 0x63=99 低位 {body}");
        assert!(body.contains("\"total_input_tokens\":900"));
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
        assert!(body.contains("\"trace_id\":1") && body.contains("\"trace_id\":2"), "{body}");

        // 加 agent 过滤:只剩风控那条。
        let (st2, body2) = s.route("POST", "/v1/search", r#"{"text":"盗刷","k":10,"filter":{"agent_name":"风控"}}"#);
        assert_eq!(st2, 200);
        assert!(body2.contains("\"trace_id\":1"), "{body2}");
        assert!(!body2.contains("\"trace_id\":2"), "agent 过滤掉人工那条: {body2}");
        assert!(body2.contains("风控"), "响应带 agent 名");

        // 坏 body → 400。
        assert_eq!(s.route("POST", "/v1/search", "not json").0, 400);
    }

    #[test]
    fn route_search_vector_and_hybrid() {
        // 检索端点的向量 / 混合路:body 带 vector 走找相似,text+vector 走混合。
        let s = server();
        assert_eq!(s.route("POST", "/v1/ingest", SEARCH_BATCH).0, 200);
        s.coord.index_embedding(1, 10, vec![0.0, 0.0]); // 风控/盗刷,离 query 近
        s.coord.index_embedding(2, 20, vec![5.0, 5.0]); // 人工,远

        // 只给 vector → 找相似,最近的是 span(1,10)。
        let (st, body) = s.route("POST", "/v1/search", r#"{"vector":[0.1,0.1],"k":5}"#);
        assert_eq!(st, 200, "{body}");
        assert!(body.contains("\"trace_id\":1"), "向量找相似命中近邻: {body}");

        // text + vector → 混合(RRF):盗刷两条都关键词命中,(1,10) 又被向量命中 → 排更前。
        let (st2, body2) = s.route("POST", "/v1/search", r#"{"text":"盗刷","vector":[0.1,0.1],"k":5}"#);
        assert_eq!(st2, 200);
        assert!(body2.starts_with("[{\"trace_id\":1"), "混合里双命中的 (1,10) 居首: {body2}");

        // 向量 + agent 过滤:只剩风控那条。
        let (st3, body3) = s.route("POST", "/v1/search", r#"{"vector":[0.1,0.1],"k":5,"filter":{"agent_name":"风控"}}"#);
        assert_eq!(st3, 200);
        assert!(body3.contains("\"trace_id\":1") && !body3.contains("\"trace_id\":2"), "{body3}");
    }

    #[test]
    fn route_otlp_rejects_bad_body() {
        let s = server();
        assert_eq!(s.route("POST", "/v1/traces", "garbage").0, 400);
        assert_eq!(s.route("POST", "/v1/traces", r#"{"foo":1}"#).0, 400, "缺 resourceSpans → 400");
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
        c.write_all(b"GET /v1/traces HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").unwrap();
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
                c.write_all(b"GET /v1/traces HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").unwrap();
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
        assert!(resp.contains("200 OK") && resp.contains("\"ingested\":2"), "{resp}");

        // GET
        let mut c2 = TcpStream::connect(addr).unwrap();
        c2.write_all(b"GET /v1/traces HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").unwrap();
        let mut resp2 = String::new();
        c2.read_to_string(&mut resp2).unwrap();
        assert!(resp2.contains("\"trace_id\":7"), "{resp2}");

        handle.join().unwrap();
    }
}
