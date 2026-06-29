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
        // 租户来自**鉴权上下文**（X-Tenant-Id 头），不信任请求体——客户端不能自选租户。
        let mut tenant: Option<u64> = None;
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
            } else if let Some(v) = hl.strip_prefix("x-tenant-id:") {
                tenant = v.trim().parse().ok();
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

        // ③ 静态资源（内嵌的控制台前端）：GET 且非 /v1/* → 从编译期内嵌资源服务（无 body）。
        if method == "GET" && !path.starts_with("/v1") {
            let p = path.split('?').next().unwrap_or("/");
            if self.serve_static(&mut stream, p) {
                self.audit(&method, &path, 200, 0);
                return;
            }
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

        let (status, resp_body) = self.route_with_tenant(&method, &path, &body, tenant);
        self.respond(&mut stream, status, &resp_body);
        self.audit(&method, &path, status, content_length);
    }

    /// 从内嵌资源服务静态文件。`/` → index.html；未知无扩展名路径 → 回退 index.html（SPA 前端路由）。
    /// 返回是否命中（命中已写响应）。console_dist 未构建时 ASSETS 为空 → 一律 miss。
    fn serve_static(&self, stream: &mut TcpStream, path: &str) -> bool {
        let want = if path == "/" { "/index.html" } else { path };
        for (url, ct, bytes) in crate::assets::ASSETS {
            if *url == want {
                self.respond_bytes(stream, ct, bytes);
                return true;
            }
        }
        // SPA 回退：无扩展名的路径当前端路由，回 index.html。
        if !path.contains('.') {
            for (url, ct, bytes) in crate::assets::ASSETS {
                if *url == "/index.html" {
                    self.respond_bytes(stream, ct, bytes);
                    return true;
                }
            }
        }
        false
    }

    fn respond_bytes(&self, stream: &mut TcpStream, content_type: &str, body: &[u8]) {
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(body);
        let _ = stream.flush();
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
        self.route_with_tenant(method, path, body, None)
    }

    /// 带租户上下文的路由（`tenant` 来自 X-Tenant-Id 头）：检索/列表端点据此强制隔离。
    pub fn route_with_tenant(&self, method: &str, path: &str, body: &str, tenant: Option<u64>) -> (u16, String) {
        // 切掉查询串：精确路由按 base 匹配，查询参数（分页 cursor/limit）单独解析。
        let (base, query) = path.split_once('?').unwrap_or((path, ""));
        match (method, base) {
            ("POST", "/v1/ingest") => match parse_wire_batch(body) {
                Ok(recs) => {
                    let n = recs.len();
                    self.coord.ingest_wire(recs);
                    (200, format!(r#"{{"ingested":{n}}}"#))
                }
                Err(e) => (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
            },
            // OTLP/HTTP 标准 trace 端点（生态入口）：OpenTelemetry / OpenInference 埋点直接 POST 到这里。
            ("POST", "/v1/traces") => match self.coord.ingest_otlp(body) {
                Ok(_) => (200, r#"{"partialSuccess":{}}"#.to_string()), // OTLP 约定的成功响应体
                Err(e) => (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
            },
            ("GET", "/v1/traces") => (200, self.traces_json(tenant)),
            // 检索端点（产品差异化的出口）：中文 BM25 + 可选属性过滤(agent/状态/时间/trace) + 租户隔离。
            ("POST", "/v1/search") => self.search_json(body, tenant),
            // 生产可观测（§3.1）：Prometheus 文本格式，无需租户隔离（全局指标）。
            ("GET", "/v1/metrics") => (200, self.coord.metrics()),
            // 控制台数据端点（前端 yitrace-console 对接）：会话游标分页 / 轮次 / trace span / span 详情。
            ("GET", "/v1/sessions") => (200, self.sessions_page_json(query)),
            _ => self.route_console(method, base),
        }
    }

    /// 带路径参数的控制台路由（/v1/sessions/:id/turns 等）。
    fn route_console(&self, method: &str, base: &str) -> (u16, String) {
        let segs: Vec<&str> = base.trim_start_matches('/').split('/').filter(|s| !s.is_empty()).collect();
        match (method, segs.as_slice()) {
            ("GET", ["v1", "sessions", id, "turns"]) => self.turns_json(id),
            ("GET", ["v1", "traces", id]) => self.trace_json(id),
            ("GET", ["v1", "traces", id, "steps"]) => self.steps_json(id),
            ("GET", ["v1", "traces", id, "spans", sid]) => self.span_detail_json(id, sid),
            _ => (404, r#"{"error":"not found"}"#.to_string()),
        }
    }

    /// 处理 `POST /v1/search`：body = `{"text":"盗刷","vector":[..],"k":10,"filter":{"agent_name":"风控"}}`。
    /// 按给了什么自动选检索路:只 text→中文检索;只 vector→找相似;两个都给→混合(RRF)。都按 filter 过滤。
    fn search_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
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
        // 租户来自鉴权头（X-Tenant-Id），覆盖请求体——客户端不能越权查别的租户。
        filter.tenant_id = tenant;

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

    fn traces_json(&self, tenant: Option<u64>) -> String {
        let snap = self.coord.pin_snapshot();
        let mut q = TraceQuery::all();
        q.tenant_id = tenant; // 租户隔离：只列本租户的 trace
        let traces = self.coord.list_traces(&snap, &q);
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

    // ───────────────────── 控制台数据端点（游标分页 / 轮次 / span / 详情） ─────────────────────

    /// GET /v1/sessions?cursor=&limit=：会话列表，offset 游标分页。
    /// `console_sessions` 走增量边车索引（摄入时 O(1) 维护），分页不全扫（见引擎实现）。
    fn sessions_page_json(&self, query: &str) -> String {
        let (mut offset, mut limit, mut filter) = (0usize, 50usize, String::new());
        for kv in query.split('&') {
            if let Some((k, v)) = kv.split_once('=') {
                match k {
                    "cursor" => offset = v.parse().unwrap_or(0),
                    "limit" => limit = v.parse().unwrap_or(50).clamp(1, 500),
                    "filter" => filter = url_decode(v),
                    _ => {}
                }
            }
        }
        let snap = self.coord.pin_snapshot();
        let mut all = self.coord.console_sessions(&snap);
        if !filter.is_empty() {
            all.retain(|s| s.title.contains(&filter) || s.session_id.to_string().contains(&filter));
        }
        let total = all.len();
        let end = (offset + limit).min(total);
        let page = if offset < total { &all[offset..end] } else { &[][..] };
        let items: Vec<String> = page
            .iter()
            .map(|s| {
                format!(
                    r#"{{"sessionId":"{}","title":"{}","turnCount":{},"totalCost":{},"status":"{}","startedAt":{},"firstTraceId":"{}"}}"#,
                    s.session_id,
                    json_escape(&s.title),
                    s.turn_count,
                    cost_num(s.input_tokens, s.output_tokens),
                    if s.has_error { "error" } else { "ok" },
                    s.session_id,
                    s.first_trace_id,
                )
            })
            .collect();
        let next = if end < total { end.to_string() } else { "null".to_string() };
        format!(r#"{{"items":[{}],"nextCursor":{},"total":{}}}"#, items.join(","), next, total)
    }

    /// GET /v1/sessions/:id/turns：一个会话的轮次（按时序）。
    fn turns_json(&self, id: &str) -> (u16, String) {
        let Ok(sid) = id.parse::<u64>() else { return (400, r#"{"error":"bad session id"}"#.to_string()) };
        let snap = self.coord.pin_snapshot();
        let tl = self.coord.load_session_timeline(&snap, sid);
        let items: Vec<String> = tl
            .turns
            .iter()
            .map(|t| {
                // 真实耗时：对该轮 trace 求 span 时长之和（毫秒）。
                let spans = self.coord.console_trace_spans(&snap, t.trace_id);
                let dur_ms = spans.iter().map(|s| s.duration_ns).sum::<u64>() / 1_000_000;
                let name = t.user_input.as_deref().map(trunc).unwrap_or_else(|| format!("第{}轮", t.turn_index + 1));
                format!(
                    r#"{{"traceId":"{}","sessionId":"{}","turnIndex":{},"name":"{}","durMs":{},"cost":{},"inTok":{},"outTok":{},"spanCount":{},"status":"{}"}}"#,
                    t.trace_id,
                    sid,
                    t.turn_index,
                    json_escape(&name),
                    dur_ms,
                    cost_num(t.input_tokens, t.output_tokens),
                    t.input_tokens,
                    t.output_tokens,
                    t.span_count,
                    if t.error_count > 0 { "error" } else { "ok" },
                )
            })
            .collect();
        (200, format!("[{}]", items.join(",")))
    }

    /// GET /v1/traces/:id：一条 trace 的折叠 span（瀑布）+ 摘要。
    fn trace_json(&self, id: &str) -> (u16, String) {
        let Ok(tid) = id.parse::<u64>() else { return (400, r#"{"error":"bad trace id"}"#.to_string()) };
        let snap = self.coord.pin_snapshot();
        let spans = self.coord.console_trace_spans(&snap, tid);
        if spans.is_empty() {
            return (404, r#"{"error":"trace not found"}"#.to_string());
        }
        // 深度：顺父指针数（用 span_id→parent 映射 + 记忆化）。
        let parent: std::collections::HashMap<u64, Option<u64>> = spans.iter().map(|s| (s.span_id, s.parent_span_id)).collect();
        let depth_of = |mut id: u64| -> usize {
            let mut d = 0;
            while let Some(Some(p)) = parent.get(&id) {
                d += 1;
                if d > 64 {
                    break;
                }
                id = *p;
            }
            d
        };
        let total_dur_ms = spans.iter().map(|s| s.duration_ns).sum::<u64>() / 1_000_000;
        let (in_tok, out_tok): (u64, u64) = spans.iter().fold((0, 0), |(i, o), s| (i + s.input_tokens, o + s.output_tokens));
        let any_err = spans.iter().any(|s| s.has_error);
        let name = spans.first().map(|s| s.name.clone()).unwrap_or_default();
        let span_items: Vec<String> = spans
            .iter()
            .map(|s| {
                format!(
                    r#"{{"id":"{}","parentId":{},"kind":"{}","name":"{}","startMs":{},"durMs":{},"status":"{}","cost":{},"inTok":{},"outTok":{},"model":{},"depth":{}}}"#,
                    s.span_id,
                    s.parent_span_id.map_or("null".to_string(), |p| format!("\"{p}\"")),
                    s.kind,
                    json_escape(&s.name),
                    s.start_ns / 1_000_000,
                    s.duration_ns / 1_000_000,
                    if s.has_error { "error" } else { "ok" },
                    cost_num(s.input_tokens, s.output_tokens),
                    s.input_tokens,
                    s.output_tokens,
                    s.model.as_ref().map_or("null".to_string(), |m| format!("\"{}\"", json_escape(m))),
                    depth_of(s.span_id),
                )
            })
            .collect();
        let summary = format!(
            r#"{{"traceId":"{}","name":"{}","durMs":{},"cost":{},"spanCount":{},"status":"{}"}}"#,
            tid,
            json_escape(&name),
            total_dur_ms,
            cost_num(in_tok, out_tok),
            spans.len(),
            if any_err { "error" } else { "ok" },
        );
        (200, format!(r#"{{"summary":{},"spans":[{}]}}"#, summary, span_items.join(",")))
    }

    /// GET /v1/traces/:id/steps：步骤流视图 —— 每个 span 连同输入/输出大文本一次给全。
    /// 与瀑布的晚物化相反：步骤流的本意就是看每一步的输入→输出，故在此端点物化。
    fn steps_json(&self, id: &str) -> (u16, String) {
        let Ok(tid) = id.parse::<u64>() else { return (400, r#"{"error":"bad trace id"}"#.to_string()) };
        let snap = self.coord.pin_snapshot();
        let spans = self.coord.console_trace_spans(&snap, tid);
        if spans.is_empty() {
            return (404, r#"{"error":"trace not found"}"#.to_string());
        }
        let items: Vec<String> = spans
            .iter()
            .map(|s| {
                format!(
                    r#"{{"id":"{}","kind":"{}","name":"{}","status":"{}","durMs":{},"inTok":{},"outTok":{},"model":{},"input":{},"output":{}}}"#,
                    s.span_id,
                    s.kind,
                    json_escape(&s.name),
                    if s.has_error { "error" } else { "ok" },
                    s.duration_ns / 1_000_000,
                    s.input_tokens,
                    s.output_tokens,
                    s.model.as_ref().map_or("null".to_string(), |m| format!("\"{}\"", json_escape(m))),
                    s.input_text.as_ref().map_or("null".to_string(), |t| format!("\"{}\"", json_escape(t))),
                    s.output_text.as_ref().map_or("null".to_string(), |t| format!("\"{}\"", json_escape(t))),
                )
            })
            .collect();
        (200, format!("[{}]", items.join(",")))
    }

    /// GET /v1/traces/:id/spans/:spanId：单个 span 的大字段（晚物化）。
    fn span_detail_json(&self, id: &str, span_id: &str) -> (u16, String) {
        let (Ok(tid), Ok(sid)) = (id.parse::<u64>(), span_id.parse::<u64>()) else {
            return (400, r#"{"error":"bad id"}"#.to_string());
        };
        let snap = self.coord.pin_snapshot();
        let spans = self.coord.console_trace_spans(&snap, tid);
        match spans.into_iter().find(|s| s.span_id == sid) {
            Some(s) => (
                200,
                format!(
                    r#"{{"id":"{}","input":{},"output":{}}}"#,
                    sid,
                    s.input_text.as_ref().map_or("null".to_string(), |t| format!("\"{}\"", json_escape(t))),
                    s.output_text.as_ref().map_or("null".to_string(), |t| format!("\"{}\"", json_escape(t))),
                ),
            ),
            None => (404, r#"{"error":"span not found"}"#.to_string()),
        }
    }
}

/// 极小 URL 解码（只处理 %XX 与 +）：会话过滤词可能是中文 → 解 percent-encoding。
fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let h = |c: u8| (c as char).to_digit(16);
                if let (Some(hi), Some(lo)) = (h(b[i + 1]), h(b[i + 2])) {
                    out.push((hi * 16 + lo) as u8);
                    i += 3;
                    continue;
                }
                out.push(b[i]);
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 千分制成本（与 SDK/前端 mock 同口径）：输入 8e-7、输出 4e-6 每 token。输出 JSON number。
fn cost_num(in_tok: u64, out_tok: u64) -> String {
    format!("{:.3}", in_tok as f64 * 8e-7 + out_tok as f64 * 4e-6)
}

/// 截断长文本当标题（按字符，不切坏 UTF-8）。
fn trunc(s: &str) -> String {
    let max = 40;
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>() + "…"
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
        assert!(body.contains("yt_manifest_version"), "缺 manifest 版本:\n{body}");
        assert!(body.contains("yt_memtable_rows"), "缺内存表行数:\n{body}");
        assert!(body.contains("yt_wal_committed_tail"), "缺 WAL 尾:\n{body}");
        assert!(body.contains("yt_segments_live"), "缺活跃段数:\n{body}");
        assert!(body.contains("yt_readers_active"), "缺活跃读者:\n{body}");
        // 灌过数据 → committed_tail > 0。
        assert!(
            body.lines().any(|l| l.starts_with("yt_wal_committed_tail ") && !l.ends_with(" 0")),
            "灌数据后 committed_tail 应 > 0:\n{body}"
        );
    }

    #[test]
    fn http_tenant_header_isolates_traces_and_search() {
        // HTTP 端到端租户隔离：摄入两租户，GET /v1/traces 与 POST /v1/search 带 X-Tenant-Id 头 → 只见本租户。
        let s = server();
        let batch = r#"[
          {"trace_id":1,"span_id":1,"ts":100,"seq":1,"event_type":2,"ext_span_id":"1-1","tenant_id":1,"duration_ns":10,"logs":["盗刷"]},
          {"trace_id":2,"span_id":1,"ts":100,"seq":1,"event_type":2,"ext_span_id":"2-1","tenant_id":2,"duration_ns":20,"logs":["盗刷"]}
        ]"#;
        assert_eq!(s.route("POST", "/v1/ingest", batch).0, 200);

        // 不带租户：两条都列。
        let all = s.route("GET", "/v1/traces", "").1;
        assert!(all.contains("\"trace_id\":1") && all.contains("\"trace_id\":2"));
        // 带租户 1：只见 trace 1。
        let t1 = s.route_with_tenant("GET", "/v1/traces", "", Some(1)).1;
        assert!(t1.contains("\"trace_id\":1") && !t1.contains("\"trace_id\":2"), "列表按租户头隔离: {t1}");
        // 检索同样隔离：查"盗刷"租户 1 只回 trace 1。
        let r1 = s.route_with_tenant("POST", "/v1/search", r#"{"text":"盗刷","k":10}"#, Some(1)).1;
        assert!(r1.contains("\"trace_id\":1") && !r1.contains("\"trace_id\":2"), "检索按租户头隔离: {r1}");
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
    fn route_console_sessions_turns_trace_detail() {
        // 控制台数据端点端到端：灌 1 个会话(2 轮) → 会话分页 → 轮次 → trace span → span 详情。
        let s = server();
        let batch = r#"[
          {"trace_id":11,"span_id":1,"ts":1,"seq":1,"event_type":1,"ext_span_id":"11-1","session_id":900,"agent_name":"风控研判","input_tokens":500,"input_text":"对账户A做研判"},
          {"trace_id":11,"span_id":1,"ts":2,"seq":2,"event_type":2,"ext_span_id":"11-1","session_id":900,"status":0,"duration_ns":2000000,"output_tokens":120,"output_text":"触发规则R12"},
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

        // 轮次：2 轮，首轮名取 input_text。
        let (st2, turns) = s.route("GET", "/v1/sessions/900/turns", "");
        assert_eq!(st2, 200, "{turns}");
        assert!(turns.contains("\"turnIndex\":0") && turns.contains("\"turnIndex\":1"), "{turns}");
        assert!(turns.contains("对账户A做研判"), "{turns}");
        assert!(turns.contains("\"durMs\":2"), "首轮 2ms: {turns}");

        // trace span：trace 11 有 span，kind=agent。
        let (st3, trace) = s.route("GET", "/v1/traces/11", "");
        assert_eq!(st3, 200, "{trace}");
        assert!(trace.contains("\"kind\":\"agent\"") && trace.contains("风控研判"), "{trace}");
        assert!(trace.contains("\"summary\""), "{trace}");

        // span 详情：晚物化大字段。
        let (st4, detail) = s.route("GET", "/v1/traces/11/spans/1", "");
        assert_eq!(st4, 200, "{detail}");
        assert!(detail.contains("触发规则R12"), "{detail}");

        // 步骤流：带输入/输出文本一次给全。
        let (st5, steps) = s.route("GET", "/v1/traces/11/steps", "");
        assert_eq!(st5, 200, "{steps}");
        assert!(steps.contains("对账户A做研判") && steps.contains("触发规则R12"), "{steps}");

        // 不存在的 trace → 404。
        assert_eq!(s.route("GET", "/v1/traces/999", "").0, 404);
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
