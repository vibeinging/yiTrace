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

use crate::{
    parse_wire_batch, AnnotationTarget, DatasetAssociationFilter, GoldenPathFilter,
    GoldenPathStatus, NewDatasetAssociation, NewGoldenPathCandidate, NewRetentionAuditRecord,
    NewRetentionPolicy, NewTraceAnnotation, RetentionAuditFilter, RetentionPolicyFilter,
    TraceAnnotationFilter, TraceQuery, WriteCoordinator,
};
use yt_core::fold::FoldedSpan;

/// 进程内 JSON API 边界。
///
/// 它复用 HTTP 的 path/body JSON 契约，但不负责 socket、鉴权头解析、body limit、gzip
/// 或静态资源。HTTP server 和 Node/Electron N-API 都走这里，从而共享同一套路由语义。
#[derive(Clone)]
pub struct EngineJsonApi {
    coord: Arc<WriteCoordinator>,
}

pub struct HttpIngestServer {
    api: EngineJsonApi,
    /// 鉴权 token。None = 不鉴权（仅限本机开发）。Some = 要求 `Authorization: Bearer <token>`。
    auth_token: Option<String>,
    /// 请求体上限（字节）。超了直接 413，**绝不按 Content-Length 预分配** —— 堵 OOM 拒绝服务。
    max_body: usize,
}

impl HttpIngestServer {
    pub fn new(coord: Arc<WriteCoordinator>) -> Self {
        Self {
            api: EngineJsonApi::new(coord),
            auth_token: None,
            max_body: 16 << 20,
        } // 默认 16 MiB
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
        let Ok(clone) = stream.try_clone() else {
            return;
        };
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
        // ② 静态资源（内嵌的控制台前端）：页面本身匿名可取，API 仍按下面 Bearer 鉴权。
        // 浏览器访问 `/` 不能带 Authorization 头；把鉴权留给 /v1/* 数据请求。
        if method == "GET" && !path.starts_with("/v1") {
            let p = path.split('?').next().unwrap_or("/");
            if self.serve_static(&mut stream, p) {
                self.audit(&method, &path, 200, 0);
                return;
            }
        }

        // ③ 鉴权：未带/错 token → 401，且不读 body。
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
                self.respond(
                    &mut stream,
                    code,
                    r#"{"error":"bad or unsupported body encoding"}"#,
                );
                self.audit(&method, &path, code, content_length);
                return;
            }
        };
        let body = String::from_utf8_lossy(&body_bytes).into_owned();

        let (status, resp_body) = self.api.route_with_tenant(&method, &path, &body, tenant);
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

    /// 兼容旧测试/调用方的路由入口。真实进程内 API 请优先使用 `EngineJsonApi`。
    pub fn route(&self, method: &str, path: &str, body: &str) -> (u16, String) {
        self.api.route(method, path, body)
    }

    /// 兼容旧测试/调用方的带租户路由入口。真实进程内 API 请优先使用 `EngineJsonApi`。
    pub fn route_with_tenant(
        &self,
        method: &str,
        path: &str,
        body: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        self.api.route_with_tenant(method, path, body, tenant)
    }
}

impl EngineJsonApi {
    pub fn new(coord: Arc<WriteCoordinator>) -> Self {
        Self { coord }
    }

    /// 纯路由（无 socket，便于单测和嵌入式调用）。返回 (status, json_body)。
    pub fn route(&self, method: &str, path: &str, body: &str) -> (u16, String) {
        self.route_with_tenant(method, path, body, None)
    }

    /// 带租户上下文的进程内路由：检索/列表端点据此强制隔离。
    pub fn route_with_tenant(
        &self,
        method: &str,
        path: &str,
        body: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        // 切掉查询串：精确路由按 base 匹配，查询参数（分页 cursor/limit）单独解析。
        let (base, query) = path.split_once('?').unwrap_or((path, ""));
        match (method, base) {
            ("POST", "/v1/ingest") => match parse_wire_batch(body) {
                Ok(recs) => {
                    let n = recs.len();
                    self.coord.ingest_wire_for_tenant(recs, tenant);
                    (200, format!(r#"{{"ingested":{n}}}"#))
                }
                Err(e) => (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
            },
            // OTLP/HTTP 标准 trace 端点（生态入口）：OpenTelemetry / OpenInference 埋点直接 POST 到这里。
            ("POST", "/v1/traces") => match self.coord.ingest_otlp_for_tenant(body, tenant) {
                Ok(_) => (200, r#"{"partialSuccess":{}}"#.to_string()), // OTLP 约定的成功响应体
                Err(e) => (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
            },
            ("GET", "/v1/traces") => (200, self.traces_json(query, tenant)),
            // 检索端点（产品差异化的出口）：中文 BM25 + 可选属性过滤(agent/状态/时间/trace) + 租户隔离。
            ("POST", "/v1/search") => self.search_json(body, tenant),
            // 产品化 trace/span 搜索：跨 session 扫描折叠 span，支持 attrs、文本 contains、分页和排序。
            ("POST", "/v1/trace-search") => self.trace_search_json(body, tenant),
            // 产品化聚合：按 skill/mode/tool/model/attrs 等字段做 group-by，用于 trace inbox 和路径挖掘。
            ("POST", "/v1/trace-aggregate") | ("POST", "/v1/trace-aggregates") => {
                self.trace_aggregate_json(body, tenant)
            }
            // Trajectory 聚合：按完整 trace 路径签名分桶，给 golden path mining 提供候选证据。
            ("POST", "/v1/trajectory-groups")
            | ("POST", "/v1/trajectory-aggregate")
            | ("POST", "/v1/best-paths") => self.trajectory_groups_json(body, tenant),
            // 物化 trajectory read model：按 traceSearch 过滤返回每条 trace 的路径摘要。
            ("POST", "/v1/trace-trajectories") | ("POST", "/v1/trajectories") => {
                self.trace_trajectories_json(body, tenant)
            }
            // Storage/retention：统计 trace 存储占用，或按策略 dry-run / apply 安全清理。
            ("POST", "/v1/storage-stats") | ("POST", "/v1/storage/stats") => {
                self.storage_stats_json(body, tenant)
            }
            ("POST", "/v1/retention-plan") | ("POST", "/v1/retention/plan") => {
                self.retention_plan_json(body, tenant, false)
            }
            ("POST", "/v1/retention/apply") => self.retention_plan_json(body, tenant, true),
            ("POST", "/v1/retention-policies") | ("POST", "/v1/retention/policies") => {
                self.create_retention_policy_json(body, tenant)
            }
            ("GET", "/v1/retention-policies") | ("GET", "/v1/retention/policies") => {
                self.retention_policies_query_json(query, tenant)
            }
            ("POST", "/v1/retention-policies/run-due")
            | ("POST", "/v1/retention/policies/run-due")
            | ("POST", "/v1/retention/run-due") => {
                self.run_due_retention_policies_json(body, tenant)
            }
            ("GET", "/v1/retention-audits") | ("GET", "/v1/retention/audits") => {
                self.retention_audits_query_json(query, tenant)
            }
            ("POST", "/v1/retention-audits") | ("POST", "/v1/retention/audits") => {
                self.retention_audits_body_json(body, tenant)
            }
            // Trace trajectory diff：比较两次尝试的路线、工具、状态、耗时和成本差异。
            ("POST", "/v1/traces/diff") | ("POST", "/v1/trace-diff") => {
                self.trace_diff_json(body, tenant)
            }
            // Agent loop/task 读模型：基于 P0.5 一等字段产出稳定摘要，业务侧不必自己扫 spans 拼。
            ("GET", "/v1/loops") => (200, self.loops_page_json(query, tenant)),
            // 业务元数据：给 trace/span 打后验 annotation，并把 trace/span 关联到外部 dataset item。
            ("POST", "/v1/annotations") => self.create_annotation_json(body, tenant),
            ("GET", "/v1/annotations") => self.annotations_json(query, tenant),
            ("POST", "/v1/dataset-associations") | ("POST", "/v1/dataset-links") => {
                self.create_dataset_association_json(body, tenant)
            }
            ("GET", "/v1/dataset-associations") | ("GET", "/v1/dataset-links") => {
                self.dataset_associations_json(query, tenant)
            }
            // Golden path 候选资产：只保存源 trace/snapshot 引用和评审状态，不复制 trace 主数据。
            ("POST", "/v1/golden-paths") => self.create_golden_path_json(body, tenant),
            ("GET", "/v1/golden-paths") => self.golden_paths_json(query, tenant),
            // Path adherence：比较一条新 trace 是否沿着某个 golden path 的轨迹执行，只返回证据不判优。
            ("POST", "/v1/path-adherence") | ("POST", "/v1/golden-path-adherence") => {
                self.path_adherence_json(body, tenant)
            }
            // Golden path evidence：把 source trace 的轨迹和元数据证据打包给上层评审/导出。
            ("POST", "/v1/golden-path-evidence") | ("POST", "/v1/golden-paths/evidence") => {
                self.golden_path_evidence_json(body, tenant)
            }
            // Golden path export：稳定 JSONL schema，供 Agent Memory / regression dataset 管线消费。
            ("POST", "/v1/golden-path-export") | ("POST", "/v1/golden-paths/export") => {
                self.golden_path_export_json(body, tenant)
            }
            // Golden path health：批量统计同 scope trace 对某条 golden path 的遵循情况，只给证据不判优。
            ("POST", "/v1/golden-path-health") | ("POST", "/v1/golden-paths/health") => {
                self.golden_path_health_json(body, tenant)
            }
            // 容器/编排系统探针：只表明进程和路由可用，不做深度数据一致性检查。
            ("GET", "/v1/healthz") | ("GET", "/v1/readyz") => (200, r#"{"ok":true}"#.to_string()),
            // 生产可观测（§3.1）：Prometheus 文本格式，无需租户隔离（全局指标）。
            ("GET", "/v1/metrics") => (200, self.coord.metrics()),
            // 控制台数据端点（前端 yitrace-console 对接）：会话游标分页 / 轮次 / trace span / span 详情。
            ("GET", "/v1/sessions") => (200, self.sessions_page_json(query, tenant)),
            _ => self.route_console(method, base, query, body, tenant),
        }
    }

    /// 带路径参数的控制台路由（/v1/sessions/:id/turns 等）。
    fn route_console(
        &self,
        method: &str,
        base: &str,
        query: &str,
        body: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let segs: Vec<&str> = base
            .trim_start_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();
        match (method, segs.as_slice()) {
            ("GET", ["v1", "sessions", id, "turns"]) => self.turns_json(id, tenant),
            ("GET", ["v1", "loops", id]) => self.loop_detail_json(id, query, tenant),
            ("GET", ["v1", "tasks", fingerprint, "traces"])
            | ("GET", ["v1", "task", fingerprint, "traces"]) => {
                (200, self.task_traces_json(fingerprint, query, tenant))
            }
            ("POST", ["v1", "golden-paths", id, "status"]) => {
                self.update_golden_path_status_json(id, body, tenant)
            }
            ("POST", ["v1", "golden-paths", id, "adherence"]) => {
                self.path_adherence_for_golden_path_json(id, body, tenant)
            }
            ("POST", ["v1", "golden-paths", id, "evidence"]) => {
                self.golden_path_evidence_for_id_json(id, body, tenant)
            }
            ("POST", ["v1", "golden-paths", id, "health"]) => {
                self.golden_path_health_for_id_json(id, body, tenant)
            }
            ("GET", ["v1", "traces", id, "snapshot"]) => self.trace_snapshot_json(id, tenant),
            ("GET", ["v1", "traces", id]) => self.trace_json(id, tenant),
            ("GET", ["v1", "traces", id, "steps"]) => self.steps_json(id, tenant),
            ("GET", ["v1", "traces", id, "spans"]) => self.spans_page_json(id, query, tenant),
            ("POST", ["v1", "traces", id, "spans", "batch"]) => {
                self.spans_batch_json(id, body, tenant)
            }
            ("GET", ["v1", "traces", id, "spans", sid]) => self.span_detail_json(id, sid, tenant),
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
            filter.trace_id = field(f, "trace_id").and_then(json_id_or_hash);
            filter.agent_name = field(f, "agent_name")
                .and_then(Json::as_str)
                .map(|s| s.to_string());
            filter.status = field(f, "status").and_then(Json::as_u64).map(|x| x as u8);
            filter.time_from = field(f, "time_from").and_then(Json::as_i64);
            filter.time_to = field(f, "time_to").and_then(Json::as_i64);
            collect_attr_filters(f, &mut filter);
        }
        // 租户来自鉴权头（X-Tenant-Id），覆盖请求体——客户端不能越权查别的租户。
        filter.tenant_id = tenant;

        let snap = self.coord.pin_snapshot();
        let hits = match (!text.is_empty(), !vector.is_empty()) {
            (true, true) => self
                .coord
                .search_hybrid_attr(&snap, text, &vector, k, &filter), // 混合
            (false, true) => self.coord.search_similar_attr(&snap, &vector, k, &filter), // 找相似
            _ => self.coord.search_text_attr(&snap, text, k, &filter),                   // 中文检索
        };
        let items: Vec<String> = hits
            .iter()
            .map(|(s, score)| {
                let logs: Vec<String> = s.logs.iter().map(|l| format!("\"{}\"", json_escape(l))).collect();
                format!(
                    r#"{{"trace_id":{},"span_id":{},"external_trace_id":{},"external_span_id":{},"score":{:.4},"status":{},"duration_ns":{},"agent_name":{},"logs":[{}],"fields":{},"attrs":{}}}"#,
                    s.trace_id,
                    s.span_id,
                    json_opt_str(s.external_trace_id.as_deref()),
                    json_opt_str(s.external_span_id.as_deref()),
                    score,
                    s.status.map_or("null".to_string(), |x| x.to_string()),
                    s.duration_ns.map_or("null".to_string(), |x| x.to_string()),
                    s.agent_name.as_ref().map_or("null".to_string(), |a| format!("\"{}\"", json_escape(a))),
                    logs.join(","),
                    json_folded_agent_fields(s),
                    json_attrs(&s.attrs),
                )
            })
            .collect();
        (200, format!("[{}]", items.join(",")))
    }

    /// POST /v1/trace-search：跨 session 的结构化 span 搜索。
    ///
    /// 它和 `/v1/search` 分工不同：后者做 BM25/向量召回；这里做产品列表页需要的精确筛选、
    /// contains、分页和排序，便于 AgenticData 从 trace 数据里找可复用路径。
    fn trace_search_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        use crate::wire::{parse, Json};
        let v = match parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };

        let cursor = json_field_alias(&v, &["cursor", "offset"])
            .and_then(Json::as_u64)
            .unwrap_or(0) as usize;
        let limit = json_field_alias(&v, &["limit", "k"])
            .and_then(Json::as_u64)
            .unwrap_or(50)
            .clamp(1, 500) as usize;
        let sort_by = json_field_alias(&v, &["sort_by", "sortBy", "sort"])
            .and_then(Json::as_str)
            .unwrap_or("created")
            .to_string();
        let order = json_field_alias(&v, &["order", "direction"])
            .and_then(Json::as_str)
            .unwrap_or("desc");
        let desc = !order.eq_ignore_ascii_case("asc");

        let request = trace_search_request_from_json(&v, tenant);
        let metadata_matches =
            self.trace_search_metadata_matches(&request.annotation, &request.dataset, tenant);

        let snap = self.coord.pin_snapshot();
        let mut spans = if request.spec.attrs.is_empty() {
            self.coord.read_spans_query(&snap, &request.query).0
        } else {
            self.coord
                .read_spans_query_for_attrs(&snap, &request.query, &request.spec.attrs)
        };
        spans.retain(|s| trace_search_match(s, &request.spec, &metadata_matches));
        sort_trace_search_spans(&mut spans, &sort_by, desc);

        let total = spans.len();
        let end = (cursor + limit).min(total);
        let page = if cursor < total {
            &spans[cursor..end]
        } else {
            &[][..]
        };
        let items: Vec<String> = page
            .iter()
            .enumerate()
            .map(|(i, s)| json_trace_search_span(s, cursor + i))
            .collect();
        let next = if end < total {
            end.to_string()
        } else {
            "null".to_string()
        };
        (
            200,
            format!(
                r#"{{"items":[{}],"nextCursor":{},"total":{}}}"#,
                items.join(","),
                next,
                total
            ),
        )
    }

    /// POST /v1/trace-aggregate：对结构化 trace/span 搜索结果做 group-by 聚合。
    ///
    /// 用于产品侧从 trace 数据里看出“哪个 skill/mode/tool 路径最常见、最贵、最容易失败”。
    /// 过滤语义完全复用 `/v1/trace-search`，避免搜索页和聚合页看到不同的数据集。
    fn trace_aggregate_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        use crate::wire::{parse, Json};
        let v = match parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let group_fields = match trace_aggregate_group_fields(&v) {
            Ok(fields) => fields,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let limit = json_field_alias(&v, &["limit", "k"])
            .and_then(Json::as_u64)
            .unwrap_or(100)
            .clamp(1, 500) as usize;
        let sort_by = json_field_alias(&v, &["sort_by", "sortBy", "sort"])
            .and_then(Json::as_str)
            .unwrap_or("count")
            .to_string();
        let order = json_field_alias(&v, &["order", "direction"])
            .and_then(Json::as_str)
            .unwrap_or("desc");
        let desc = !order.eq_ignore_ascii_case("asc");

        let request = trace_search_request_from_json(&v, tenant);
        let metadata_matches =
            self.trace_search_metadata_matches(&request.annotation, &request.dataset, tenant);

        let snap = self.coord.pin_snapshot();
        let mut spans = if request.spec.attrs.is_empty() {
            self.coord.read_spans_query(&snap, &request.query).0
        } else {
            self.coord
                .read_spans_query_for_attrs(&snap, &request.query, &request.spec.attrs)
        };
        spans.retain(|s| trace_search_match(s, &request.spec, &metadata_matches));

        let mut buckets = trace_aggregate_buckets(&spans, &group_fields);
        sort_trace_aggregate_buckets(&mut buckets, &sort_by, desc);
        let total = buckets.len();
        let items: Vec<String> = buckets
            .iter()
            .take(limit)
            .map(|bucket| trace_aggregate_bucket_json(bucket, &group_fields))
            .collect();
        (
            200,
            format!(
                r#"{{"items":[{}],"total":{},"spanTotal":{}}}"#,
                items.join(","),
                total,
                spans.len()
            ),
        )
    }

    /// POST /v1/trajectory-groups：把候选 trace 按完整 trajectory signature 分桶。
    ///
    /// 过滤语义复用 `/v1/trace-search`。实现先用 span 过滤找候选 trace，再读取每条 trace 的完整
    /// folded spans 算路径签名，避免把“命中的单个 span”误当成完整执行路径。
    fn trajectory_groups_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        use crate::wire::{parse, Json};
        let v = match parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let limit = json_field_alias(&v, &["limit", "k"])
            .and_then(Json::as_u64)
            .unwrap_or(50)
            .clamp(1, 500) as usize;
        let example_limit = json_field_alias(&v, &["example_limit", "exampleLimit", "examples"])
            .and_then(Json::as_u64)
            .unwrap_or(3)
            .clamp(0, 20) as usize;
        let sort_by = json_field_alias(&v, &["sort_by", "sortBy", "sort"])
            .and_then(Json::as_str)
            .unwrap_or("best")
            .to_string();
        let desc = json_field_alias(&v, &["order", "direction"])
            .and_then(Json::as_str)
            .map(|order| !order.eq_ignore_ascii_case("asc"))
            .unwrap_or_else(|| trajectory_default_desc(&sort_by));

        let request = trace_search_request_from_json(&v, tenant);
        let metadata_matches =
            self.trace_search_metadata_matches(&request.annotation, &request.dataset, tenant);
        let snap = self.coord.pin_snapshot();
        let mut matching_spans = if request.spec.attrs.is_empty() {
            self.coord.read_spans_query(&snap, &request.query).0
        } else {
            self.coord
                .read_spans_query_for_attrs(&snap, &request.query, &request.spec.attrs)
        };
        matching_spans.retain(|s| trace_search_match(s, &request.spec, &metadata_matches));
        let span_total = matching_spans.len();
        let trace_ids: std::collections::BTreeSet<u64> =
            matching_spans.iter().map(|s| s.trace_id).collect();

        let annotation_scores =
            trace_annotation_score_map(self.coord.annotations(&TraceAnnotationFilter {
                tenant_id: tenant,
                ..Default::default()
            }));
        let dataset_scores =
            trace_dataset_score_map(self.coord.dataset_associations(&DatasetAssociationFilter {
                tenant_id: tenant,
                ..Default::default()
            }));

        let mut by_signature: std::collections::BTreeMap<u64, TrajectoryGroupBucket> =
            std::collections::BTreeMap::new();
        let mut trace_total = 0usize;
        for trace_id in trace_ids {
            let spans = self.trace_folded_spans(&snap, trace_id, tenant);
            if spans.is_empty() {
                continue;
            }
            trace_total += 1;
            let steps = trajectory_steps(&spans);
            let signature = trajectory_signature(&steps);
            let summary = trace_summary_buckets_from_spans(&spans).into_iter().next();
            let bucket = by_signature
                .entry(signature)
                .or_insert_with(|| TrajectoryGroupBucket::new(signature, steps));
            bucket.add_trace(
                &spans,
                summary.as_ref(),
                annotation_scores.get(&trace_id).map(Vec::as_slice),
                dataset_scores.get(&trace_id).map(Vec::as_slice),
                example_limit,
            );
        }

        let mut buckets: Vec<_> = by_signature.into_values().collect();
        sort_trajectory_group_buckets(&mut buckets, &sort_by, desc);
        let total = buckets.len();
        let items = buckets
            .iter()
            .take(limit)
            .map(json_trajectory_group_bucket)
            .collect::<Vec<_>>()
            .join(",");
        (
            200,
            format!(
                r#"{{"items":[{}],"total":{},"traceTotal":{},"spanTotal":{}}}"#,
                items, total, trace_total, span_total
            ),
        )
    }

    /// POST /v1/trace-trajectories：按 traceSearch 过滤返回每条 trace 的物化 trajectory 摘要。
    fn trace_trajectories_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        use crate::wire::{parse, Json};
        let v = match parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let cursor = json_field_alias(&v, &["cursor", "offset"])
            .and_then(Json::as_u64)
            .unwrap_or(0) as usize;
        let limit = json_field_alias(&v, &["limit", "k"])
            .and_then(Json::as_u64)
            .unwrap_or(50)
            .clamp(1, 500) as usize;
        let request = trace_search_request_from_json(&v, tenant);
        let metadata_matches =
            self.trace_search_metadata_matches(&request.annotation, &request.dataset, tenant);
        let snap = self.coord.pin_snapshot();
        let mut matching_spans = if request.spec.attrs.is_empty() {
            self.coord.read_spans_query(&snap, &request.query).0
        } else {
            self.coord
                .read_spans_query_for_attrs(&snap, &request.query, &request.spec.attrs)
        };
        matching_spans.retain(|s| trace_search_match(s, &request.spec, &metadata_matches));
        let mut trace_ids: Vec<u64> = matching_spans
            .iter()
            .map(|s| s.trace_id)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        trace_ids.sort_by(|a, b| b.cmp(a));
        let total = trace_ids.len();
        let end = (cursor + limit).min(total);
        let page = if cursor < total {
            &trace_ids[cursor..end]
        } else {
            &[][..]
        };
        let items = page
            .iter()
            .filter_map(|trace_id| {
                self.coord
                    .materialized_trace_trajectory(&snap, *trace_id, tenant)
            })
            .map(|summary| json_trace_trajectory_summary(&summary))
            .collect::<Vec<_>>()
            .join(",");
        let next = if end < total {
            end.to_string()
        } else {
            "null".to_string()
        };
        (
            200,
            format!(
                r#"{{"items":[{}],"nextCursor":{},"total":{},"spanTotal":{},"index":"materialized"}}"#,
                items,
                next,
                total,
                matching_spans.len(),
            ),
        )
    }

    /// POST /v1/storage-stats：按 traceSearch 过滤统计存储占用估算。
    ///
    /// 这里返回的是可复算估算值：输入/输出/log/attrs 等 payload 字节精确按 UTF-8 计，
    /// segment/WAL 文件级空间仍以 estimatedBytes 近似表达，避免伪装成物理磁盘精确值。
    fn storage_stats_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        use crate::wire::{parse, Json};
        let v = match parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let group_by = storage_group_by_from_json(&v);
        let time_bucket_ns = json_field_alias(
            &v,
            &["time_bucket_ns", "timeBucketNs", "bucket_ns", "bucketNs"],
        )
        .and_then(Json::as_u64)
        .unwrap_or(86_400_000_000_000);

        let (snap, spans) = match self.filtered_spans_for_storage(&v, tenant) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let trace_ids: std::collections::HashSet<u64> = spans.iter().map(|s| s.trace_id).collect();
        let bounds = self.coord.trace_time_bounds(&snap, &trace_ids);
        let metadata = self.storage_metadata_for_tenant(tenant);
        let report = storage_stats_report(&spans, &bounds, &metadata, &group_by, time_bucket_ns);
        (200, json_storage_stats_report(&report, &group_by))
    }

    /// POST /v1/retention-plan / retention/apply：按策略生成或执行保留清理计划。
    fn retention_plan_json(
        &self,
        body: &str,
        tenant: Option<u64>,
        route_apply: bool,
    ) -> (u16, String) {
        use crate::wire::{parse, Json};
        let v = match parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let apply =
            route_apply || json_bool_alias(&v, &["apply", "execute", "delete"]).unwrap_or(false);
        let cutoff = json_field_alias(
            &v,
            &[
                "delete_before_ts",
                "deleteBeforeTs",
                "older_than_ts",
                "olderThanTs",
                "time_to",
                "timeTo",
            ],
        )
        .and_then(Json::as_i64);
        if apply && cutoff.is_none() {
            return (
                400,
                r#"{"error":"retention apply requires deleteBeforeTs"}"#.to_string(),
            );
        }
        let protect_golden_paths = retention_protect_bool(&v, "goldenPaths", "golden_paths", true);
        let protect_annotations = retention_protect_bool(&v, "annotations", "annotations", true);
        let protect_dataset_associations =
            retention_protect_bool(&v, "datasetAssociations", "dataset_associations", true);
        let protect_snapshots = retention_protect_bool(&v, "snapshots", "snapshots", true);
        let protect_eval_links = retention_protect_bool(&v, "evalLinks", "eval_links", true);
        let protect_path_memory = retention_protect_bool(&v, "pathMemory", "path_memory", true);
        let example_limit = json_field_alias(&v, &["exampleLimit", "examples", "limit"])
            .and_then(Json::as_u64)
            .unwrap_or(20)
            .clamp(0, 100) as usize;
        let compact_after_apply =
            json_bool_alias(&v, &["compact", "compactAfterApply", "compact_after_apply"])
                .unwrap_or(false);
        let compact_min_deleted_rows = json_field_alias(
            &v,
            &[
                "compactMinDeletedRows",
                "compact_min_deleted_rows",
                "minDeletedRows",
                "min_deleted_rows",
            ],
        )
        .and_then(Json::as_u64)
        .unwrap_or(1)
        .clamp(1, u32::MAX as u64) as u32;
        let compact_min_deleted_percent = json_field_alias(
            &v,
            &[
                "compactMinDeletedPercent",
                "compact_min_deleted_percent",
                "minDeletedPercent",
                "min_deleted_percent",
            ],
        )
        .and_then(Json::as_u64)
        .unwrap_or(1)
        .clamp(1, 100) as u32;
        let compact_max_segments = json_field_alias(
            &v,
            &[
                "compactMaxSegments",
                "compact_max_segments",
                "maxSegments",
                "max_segments",
            ],
        )
        .and_then(Json::as_u64)
        .unwrap_or(64)
        .clamp(0, 1024) as usize;
        let reclaim_after_compact = json_bool_alias(
            &v,
            &[
                "reclaim",
                "reclaimAfterCompact",
                "reclaim_after_compact",
                "compactReclaim",
                "compact_reclaim",
            ],
        )
        .unwrap_or(true);
        let audit_source = json_field_alias(
            &v,
            &[
                "source",
                "requestedBy",
                "requested_by",
                "actor",
                "createdBy",
                "created_by",
            ],
        )
        .and_then(Json::as_str)
        .map(ToString::to_string);
        let audit_reason = json_field_alias(&v, &["reason", "comment", "note"])
            .and_then(Json::as_str)
            .map(ToString::to_string);

        let (snap, spans) = match self.filtered_spans_for_storage(&v, tenant) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let all_trace_ids: std::collections::HashSet<u64> =
            spans.iter().map(|s| s.trace_id).collect();
        let bounds = self.coord.trace_time_bounds(&snap, &all_trace_ids);
        let metadata = self.storage_metadata_for_tenant(tenant);

        let mut candidate_trace_ids = std::collections::HashSet::new();
        for trace_id in &all_trace_ids {
            let Some((_, max_ts)) = bounds.get(trace_id) else {
                continue;
            };
            if cutoff.map(|cut| *max_ts <= cut).unwrap_or(true) {
                candidate_trace_ids.insert(*trace_id);
            }
        }

        let protected = protected_trace_reasons(
            &candidate_trace_ids,
            &metadata,
            protect_golden_paths,
            protect_annotations,
            protect_dataset_associations,
            protect_snapshots,
            protect_eval_links,
            protect_path_memory,
        );
        let protected_trace_ids: std::collections::HashSet<u64> =
            protected.keys().copied().collect();
        let deletable_trace_ids: std::collections::HashSet<u64> = candidate_trace_ids
            .difference(&protected_trace_ids)
            .copied()
            .collect();

        let mut candidate_stats =
            storage_bucket_for_trace_ids(&spans, &bounds, &candidate_trace_ids);
        let mut protected_stats =
            storage_bucket_for_trace_ids(&spans, &bounds, &protected_trace_ids);
        let mut deletable_stats =
            storage_bucket_for_trace_ids(&spans, &bounds, &deletable_trace_ids);
        storage_bucket_apply_metadata_counts(&mut candidate_stats, &metadata);
        storage_bucket_apply_metadata_counts(&mut protected_stats, &metadata);
        storage_bucket_apply_metadata_counts(&mut deletable_stats, &metadata);
        let applied = if apply {
            Some(
                self.coord
                    .delete_segment_rows_for_traces(&snap, &deletable_trace_ids),
            )
        } else {
            None
        };
        let compacted = if apply && compact_after_apply {
            drop(snap);
            Some(self.coord.compact_deleted_segments(
                compact_max_segments,
                compact_min_deleted_rows,
                compact_min_deleted_percent,
                reclaim_after_compact,
            ))
        } else {
            None
        };
        let audit = if apply {
            let applied_ref = applied.as_ref();
            let compacted_ref = compacted.as_ref();
            let sample_limit = 100;
            let deletable_sample = sample_u64_set(&deletable_trace_ids, sample_limit);
            let deleted_sample = applied_ref
                .map(|result| sample_u64_slice(&result.deleted_trace_ids, sample_limit))
                .unwrap_or_default();
            let skipped_sample = applied_ref
                .map(|result| sample_u64_slice(&result.skipped_live_trace_ids, sample_limit))
                .unwrap_or_default();
            let sample_truncated = deletable_trace_ids.len() > deletable_sample.len()
                || applied_ref
                    .map(|result| result.deleted_trace_ids.len() > deleted_sample.len())
                    .unwrap_or(false)
                || applied_ref
                    .map(|result| result.skipped_live_trace_ids.len() > skipped_sample.len())
                    .unwrap_or(false);
            Some(
                self.coord.add_retention_audit(
                    NewRetentionAuditRecord {
                        source: audit_source,
                        reason: audit_reason,
                        delete_before_ts: cutoff,
                        query_json: v.to_compact_json(),
                        protect_golden_paths,
                        protect_annotations,
                        protect_dataset_associations,
                        protect_snapshots,
                        protect_eval_links,
                        protect_path_memory,
                        compact_requested: compact_after_apply,
                        compact_reclaim: reclaim_after_compact,
                        candidate_trace_count: candidate_stats.trace_ids.len() as u64,
                        protected_trace_count: protected_stats.trace_ids.len() as u64,
                        deletable_trace_count: deletable_stats.trace_ids.len() as u64,
                        requested_trace_count: applied_ref
                            .map(|result| result.requested_trace_count as u64)
                            .unwrap_or(0),
                        deleted_trace_count: applied_ref
                            .map(|result| result.deleted_trace_count as u64)
                            .unwrap_or(0),
                        deleted_segment_row_count: applied_ref
                            .map(|result| result.deleted_segment_row_count as u64)
                            .unwrap_or(0),
                        skipped_live_trace_count: applied_ref
                            .map(|result| result.skipped_live_trace_count as u64)
                            .unwrap_or(0),
                        compacted_segment_count: compacted_ref
                            .map(|result| result.compacted_segment_count as u64)
                            .unwrap_or(0),
                        reclaimed_segment_count: compacted_ref
                            .map(|result| result.reclaimed_segment_count as u64)
                            .unwrap_or(0),
                        dropped_deleted_row_count: compacted_ref
                            .map(|result| result.dropped_deleted_row_count as u64)
                            .unwrap_or(0),
                        rewritten_live_row_count: compacted_ref
                            .map(|result| result.rewritten_live_row_count as u64)
                            .unwrap_or(0),
                        deletable_trace_ids: deletable_sample,
                        deleted_trace_ids: deleted_sample,
                        skipped_live_trace_ids: skipped_sample,
                        trace_id_sample_truncated: sample_truncated,
                    },
                    tenant,
                ),
            )
        } else {
            None
        };

        (
            200,
            json_retention_plan(
                apply,
                cutoff,
                protect_golden_paths,
                protect_annotations,
                protect_dataset_associations,
                protect_snapshots,
                protect_eval_links,
                protect_path_memory,
                &candidate_stats,
                &protected_stats,
                &deletable_stats,
                &protected,
                &deletable_trace_ids,
                applied.as_ref(),
                compact_after_apply,
                compact_min_deleted_rows,
                compact_min_deleted_percent,
                compact_max_segments,
                reclaim_after_compact,
                compacted.as_ref(),
                audit.as_ref(),
                example_limit,
            ),
        )
    }

    /// GET /v1/retention-audits：查询 retention/apply 审计记录。
    fn retention_audits_query_json(&self, query: &str, tenant: Option<u64>) -> (u16, String) {
        let mut filter = RetentionAuditFilter {
            tenant_id: tenant,
            ..Default::default()
        };
        let mut cursor = 0usize;
        let mut limit = 50usize;
        for (k, v) in query_pairs(query) {
            match k.as_str() {
                "audit_id" | "auditId" | "id" => filter.audit_id = parse_id_or_hash(&v),
                "source" | "requestedBy" | "requested_by" | "actor" => filter.source = Some(v),
                "created_after_ns" | "createdAfterNs" | "minCreatedAtNs" => {
                    filter.min_created_at_ns = v.parse::<u64>().ok()
                }
                "created_before_ns" | "createdBeforeNs" | "maxCreatedAtNs" => {
                    filter.max_created_at_ns = v.parse::<u64>().ok()
                }
                "cursor" | "offset" => cursor = v.parse::<usize>().unwrap_or(0),
                "limit" => limit = v.parse::<usize>().unwrap_or(50).clamp(1, 500),
                _ => {}
            }
        }
        (200, self.retention_audits_page_json(filter, cursor, limit))
    }

    /// POST /v1/retention-audits：JSON 查询审计记录，兼容 `{filter:{...},limit,cursor}`。
    fn retention_audits_body_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        let v = match parse_json_body_or_empty(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let f = crate::wire::field(&v, "filter").unwrap_or(&v);
        let mut filter = RetentionAuditFilter {
            tenant_id: tenant,
            ..Default::default()
        };
        filter.audit_id =
            json_field_alias(f, &["audit_id", "auditId", "id"]).and_then(json_internal_id);
        filter.source = json_field_alias(
            f,
            &[
                "source",
                "requestedBy",
                "requested_by",
                "actor",
                "createdBy",
            ],
        )
        .and_then(crate::wire::Json::as_str)
        .map(ToString::to_string);
        filter.min_created_at_ns =
            json_field_alias(f, &["created_after_ns", "createdAfterNs", "minCreatedAtNs"])
                .and_then(crate::wire::Json::as_u64);
        filter.max_created_at_ns = json_field_alias(
            f,
            &["created_before_ns", "createdBeforeNs", "maxCreatedAtNs"],
        )
        .and_then(crate::wire::Json::as_u64);
        let cursor = json_field_alias(&v, &["cursor", "offset"])
            .and_then(crate::wire::Json::as_u64)
            .unwrap_or(0) as usize;
        let limit = json_field_alias(&v, &["limit"])
            .and_then(crate::wire::Json::as_u64)
            .unwrap_or(50)
            .clamp(1, 500) as usize;
        (200, self.retention_audits_page_json(filter, cursor, limit))
    }

    fn retention_audits_page_json(
        &self,
        filter: RetentionAuditFilter,
        cursor: usize,
        limit: usize,
    ) -> String {
        let mut items = self.coord.retention_audits(&filter);
        items.sort_by(|a, b| {
            b.created_at_ns
                .cmp(&a.created_at_ns)
                .then_with(|| b.audit_id.cmp(&a.audit_id))
        });
        let total = items.len();
        let end = (cursor + limit).min(total);
        let page = if cursor < total {
            &items[cursor..end]
        } else {
            &[][..]
        };
        let body = page
            .iter()
            .map(json_retention_audit)
            .collect::<Vec<_>>()
            .join(",");
        let next = if end < total {
            end.to_string()
        } else {
            "null".to_string()
        };
        format!(
            r#"{{"items":[{}],"nextCursor":{},"total":{}}}"#,
            body, next, total
        )
    }

    /// POST /v1/retention-policies：保存一条可重复执行的 retention policy。
    fn create_retention_policy_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        let v = match crate::wire::parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let name = json_field_alias(&v, &["name", "policyName", "policy_name"])
            .and_then(crate::wire::Json::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if name.is_empty() {
            return (400, r#"{"error":"missing name"}"#.to_string());
        }
        let interval_ns =
            json_field_alias(&v, &["intervalNs", "interval_ns", "everyNs", "every_ns"])
                .and_then(crate::wire::Json::as_u64)
                .unwrap_or(0);
        if interval_ns == 0 {
            return (400, r#"{"error":"missing intervalNs"}"#.to_string());
        }
        let Some(query) = json_field_alias(
            &v,
            &["query", "retention", "retentionQuery", "retention_query"],
        ) else {
            return (400, r#"{"error":"missing query"}"#.to_string());
        };
        if !matches!(query, crate::wire::Json::Obj(_)) {
            return (400, r#"{"error":"query must be an object"}"#.to_string());
        }
        if !retention_policy_query_has_cutoff(query) {
            return (
                400,
                r#"{"error":"query requires deleteBeforeTs or olderThanNs"}"#.to_string(),
            );
        }
        let now = unix_now_ns_u64_for_http();
        let policy = self.coord.add_retention_policy(
            NewRetentionPolicy {
                name,
                enabled: json_bool_alias(&v, &["enabled"]).unwrap_or(true),
                next_run_at_ns: json_field_alias(&v, &["nextRunAtNs", "next_run_at_ns"])
                    .and_then(crate::wire::Json::as_u64)
                    .or(Some(now)),
                interval_ns,
                source: json_field_alias(
                    &v,
                    &[
                        "source",
                        "requestedBy",
                        "requested_by",
                        "actor",
                        "createdBy",
                    ],
                )
                .and_then(crate::wire::Json::as_str)
                .map(ToString::to_string),
                reason: json_field_alias(&v, &["reason", "comment", "note"])
                    .and_then(crate::wire::Json::as_str)
                    .map(ToString::to_string),
                query_json: query.to_compact_json(),
            },
            tenant,
        );
        (200, json_retention_policy(&policy))
    }

    /// GET /v1/retention-policies：查询已保存的 retention policies。
    fn retention_policies_query_json(&self, query: &str, tenant: Option<u64>) -> (u16, String) {
        let mut filter = RetentionPolicyFilter {
            tenant_id: tenant,
            ..Default::default()
        };
        let mut cursor = 0usize;
        let mut limit = 50usize;
        for (k, v) in query_pairs(query) {
            match k.as_str() {
                "policy_id" | "policyId" | "id" => filter.policy_id = parse_id_or_hash(&v),
                "name" | "policyName" | "policy_name" => filter.name = Some(v),
                "enabled" => filter.enabled = parse_query_bool(&v),
                "cursor" | "offset" => cursor = v.parse::<usize>().unwrap_or(0),
                "limit" => limit = v.parse::<usize>().unwrap_or(50).clamp(1, 500),
                _ => {}
            }
        }
        (
            200,
            self.retention_policies_page_json(filter, cursor, limit),
        )
    }

    fn retention_policies_page_json(
        &self,
        filter: RetentionPolicyFilter,
        cursor: usize,
        limit: usize,
    ) -> String {
        let mut items = self.coord.retention_policies(&filter);
        items.sort_by(|a, b| a.policy_id.cmp(&b.policy_id));
        let total = items.len();
        let end = (cursor + limit).min(total);
        let page = if cursor < total {
            &items[cursor..end]
        } else {
            &[][..]
        };
        let body = page
            .iter()
            .map(json_retention_policy)
            .collect::<Vec<_>>()
            .join(",");
        let next = if end < total {
            end.to_string()
        } else {
            "null".to_string()
        };
        format!(
            r#"{{"items":[{}],"nextCursor":{},"total":{}}}"#,
            body, next, total
        )
    }

    /// POST /v1/retention-policies/run-due：执行当前到期的 policies。
    fn run_due_retention_policies_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        let v = match parse_json_body_or_empty(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let now = json_field_alias(&v, &["nowNs", "now_ns"])
            .and_then(crate::wire::Json::as_u64)
            .unwrap_or_else(unix_now_ns_u64_for_http);
        let limit = json_field_alias(&v, &["limit", "maxPolicies", "max_policies"])
            .and_then(crate::wire::Json::as_u64)
            .unwrap_or(10)
            .clamp(1, 100) as usize;
        let include_disabled =
            json_bool_alias(&v, &["includeDisabled", "include_disabled"]).unwrap_or(false);
        let mut filter = RetentionPolicyFilter {
            tenant_id: tenant,
            ..Default::default()
        };
        filter.policy_id =
            json_field_alias(&v, &["policyId", "policy_id", "id"]).and_then(json_internal_id);
        filter.name = json_field_alias(&v, &["name", "policyName", "policy_name"])
            .and_then(crate::wire::Json::as_str)
            .map(ToString::to_string);
        let policies = self.coord.retention_policies(&filter);
        let mut due = Vec::new();
        let mut skipped = 0usize;
        for policy in policies {
            if !include_disabled && !policy.enabled {
                skipped += 1;
                continue;
            }
            if policy.next_run_at_ns.map(|n| n <= now).unwrap_or(false) {
                due.push(policy);
            } else {
                skipped += 1;
            }
        }
        due.sort_by(|a, b| {
            a.next_run_at_ns
                .cmp(&b.next_run_at_ns)
                .then_with(|| a.policy_id.cmp(&b.policy_id))
        });
        skipped += due.len().saturating_sub(limit);

        let mut ran = 0usize;
        let mut failed = 0usize;
        let mut items = Vec::new();
        for policy in due.into_iter().take(limit) {
            match retention_policy_effective_query(&policy, now) {
                Ok(query) => {
                    let (status, result) = self.retention_plan_json(&query, tenant, true);
                    if status == 200 {
                        ran += 1;
                        let policy = self
                            .coord
                            .mark_retention_policy_ran(policy.policy_id, tenant, now)
                            .unwrap_or(policy);
                        items.push(format!(
                            r#"{{"policy":{},"ok":true,"statusCode":{},"result":{}}}"#,
                            json_retention_policy(&policy),
                            status,
                            result
                        ));
                    } else {
                        failed += 1;
                        items.push(format!(
                            r#"{{"policy":{},"ok":false,"statusCode":{},"error":{}}}"#,
                            json_retention_policy(&policy),
                            status,
                            result
                        ));
                    }
                }
                Err(error) => {
                    failed += 1;
                    items.push(format!(
                        r#"{{"policy":{},"ok":false,"statusCode":400,"error":{{"error":"{}"}}}}"#,
                        json_retention_policy(&policy),
                        json_escape(&error)
                    ));
                }
            }
        }
        (
            200,
            format!(
                r#"{{"nowNs":"{}","ran":{},"failed":{},"skipped":{},"items":[{}]}}"#,
                now,
                ran,
                failed,
                skipped,
                items.join(",")
            ),
        )
    }

    fn filtered_spans_for_storage(
        &self,
        v: &crate::wire::Json,
        tenant: Option<u64>,
    ) -> Result<(yt_manifest::Snapshot, Vec<FoldedSpan>), String> {
        let request = trace_search_request_from_json(v, tenant);
        let metadata_matches =
            self.trace_search_metadata_matches(&request.annotation, &request.dataset, tenant);
        let snap = self.coord.pin_snapshot();
        let mut spans = if request.spec.attrs.is_empty() {
            self.coord.read_spans_query(&snap, &request.query).0
        } else {
            self.coord
                .read_spans_query_for_attrs(&snap, &request.query, &request.spec.attrs)
        };
        spans.retain(|s| trace_search_match(s, &request.spec, &metadata_matches));
        Ok((snap, spans))
    }

    fn storage_metadata_for_tenant(&self, tenant: Option<u64>) -> StorageMetadata {
        StorageMetadata {
            annotations: self.coord.annotations(&TraceAnnotationFilter {
                tenant_id: tenant,
                ..TraceAnnotationFilter::default()
            }),
            dataset_associations: self.coord.dataset_associations(&DatasetAssociationFilter {
                tenant_id: tenant,
                ..DatasetAssociationFilter::default()
            }),
            golden_paths: self.coord.golden_paths(&GoldenPathFilter {
                tenant_id: tenant,
                ..GoldenPathFilter::default()
            }),
        }
    }

    /// POST /v1/traces/diff：比较两条 trace 的 trajectory。
    ///
    /// 这是基础读模型：只做确定性结构对比，不做“哪条更好”的自动判断。上层可以用 annotation/eval
    /// 或业务规则在这个证据上继续判优。
    fn trace_diff_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        use crate::wire::parse;
        let v = match parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let Some(left_id) = json_field_alias(
            &v,
            &[
                "left_trace_id",
                "leftTraceId",
                "left",
                "base_trace_id",
                "baseTraceId",
                "a",
            ],
        )
        .and_then(json_id_or_hash) else {
            return (400, r#"{"error":"missing leftTraceId"}"#.to_string());
        };
        let Some(right_id) = json_field_alias(
            &v,
            &[
                "right_trace_id",
                "rightTraceId",
                "right",
                "candidate_trace_id",
                "candidateTraceId",
                "b",
            ],
        )
        .and_then(json_id_or_hash) else {
            return (400, r#"{"error":"missing rightTraceId"}"#.to_string());
        };

        let snap = self.coord.pin_snapshot();
        let left = self.trace_folded_spans(&snap, left_id, tenant);
        if left.is_empty() {
            return (404, r#"{"error":"left trace not found"}"#.to_string());
        }
        let right = self.trace_folded_spans(&snap, right_id, tenant);
        if right.is_empty() {
            return (404, r#"{"error":"right trace not found"}"#.to_string());
        }
        (200, json_trace_diff(left_id, right_id, &left, &right))
    }

    fn trace_folded_spans(
        &self,
        snap: &yt_manifest::Snapshot,
        trace_id: u64,
        tenant: Option<u64>,
    ) -> Vec<FoldedSpan> {
        let mut q = TraceQuery::trace(trace_id, i64::MIN, i64::MAX);
        q.tenant_id = tenant;
        let mut spans = self.coord.read_spans_query(snap, &q).0;
        spans.sort_by_key(|s| s.span_id);
        spans
    }

    /// GET /v1/loops：按 `loop_id` 聚合出 agent loop 摘要。
    ///
    /// 这是轻量读模型，不做自动诊断；它只把一等 task/loop/validation 字段折叠成稳定分页结果。
    fn loops_page_json(&self, query: &str, tenant: Option<u64>) -> String {
        let parts = product_query_parts(query, 50);
        let snap = self.coord.pin_snapshot();
        let mut spans = self.product_query_spans(
            &snap,
            tenant,
            &parts.attrs,
            &parts.annotation,
            &parts.dataset,
        );
        if !parts.filter.is_empty() {
            spans.retain(|s| loop_span_contains(s, &parts.filter));
        }
        let mut loops = loop_summary_buckets(&spans);
        loops.sort_by(|a, b| {
            b.last_trace_id
                .cmp(&a.last_trace_id)
                .then_with(|| a.loop_id.cmp(&b.loop_id))
        });
        let total = loops.len();
        let end = (parts.cursor + parts.limit).min(total);
        let page = if parts.cursor < total {
            &loops[parts.cursor..end]
        } else {
            &[][..]
        };
        let items = page
            .iter()
            .map(json_loop_summary_bucket)
            .collect::<Vec<_>>()
            .join(",");
        let next = if end < total {
            end.to_string()
        } else {
            "null".to_string()
        };
        format!(
            r#"{{"items":[{}],"nextCursor":{},"total":{}}}"#,
            items, next, total
        )
    }

    /// GET /v1/loops/:id：返回一个 loop 的摘要、trace 列表和 span 列表。
    fn loop_detail_json(&self, id: &str, query: &str, tenant: Option<u64>) -> (u16, String) {
        let mut parts = product_query_parts(query, 200);
        let loop_id = url_decode(id);
        parts
            .attrs
            .insert("loop_id".to_string(), json_string_value(&loop_id));
        let snap = self.coord.pin_snapshot();
        let mut spans = self.product_query_spans(
            &snap,
            tenant,
            &parts.attrs,
            &parts.annotation,
            &parts.dataset,
        );
        if !parts.filter.is_empty() {
            spans.retain(|s| loop_span_contains(s, &parts.filter));
        }
        if spans.is_empty() {
            return (404, r#"{"error":"loop not found"}"#.to_string());
        }
        spans.sort_by_key(|s| (s.trace_id, s.span_id));
        let mut loops = loop_summary_buckets(&spans);
        let Some(summary) = loops.pop() else {
            return (404, r#"{"error":"loop not found"}"#.to_string());
        };
        let traces = trace_summary_buckets_from_spans(&spans)
            .iter()
            .map(json_task_trace_summary_bucket)
            .collect::<Vec<_>>()
            .join(",");
        let span_items = spans
            .iter()
            .enumerate()
            .map(|(rank, span)| json_trace_search_span(span, rank))
            .collect::<Vec<_>>()
            .join(",");
        (
            200,
            format!(
                r#"{{"summary":{},"traces":[{}],"spans":[{}]}}"#,
                json_loop_summary_bucket(&summary),
                traces,
                span_items
            ),
        )
    }

    /// GET /v1/tasks/:fingerprint/traces：列出同类任务的 trace 摘要。
    fn task_traces_json(&self, fingerprint: &str, query: &str, tenant: Option<u64>) -> String {
        let mut parts = product_query_parts(query, 50);
        let task_fingerprint = url_decode(fingerprint);
        parts.attrs.insert(
            "task_fingerprint".to_string(),
            json_string_value(&task_fingerprint),
        );
        let snap = self.coord.pin_snapshot();
        let mut spans = self.product_query_spans(
            &snap,
            tenant,
            &parts.attrs,
            &parts.annotation,
            &parts.dataset,
        );
        if !parts.filter.is_empty() {
            spans.retain(|s| folded_contains(s, &parts.filter));
        }
        let mut traces = trace_summary_buckets_from_spans(&spans);
        traces.sort_by(|a, b| b.trace_id.cmp(&a.trace_id));
        let total = traces.len();
        let end = (parts.cursor + parts.limit).min(total);
        let page = if parts.cursor < total {
            &traces[parts.cursor..end]
        } else {
            &[][..]
        };
        let items = page
            .iter()
            .map(json_task_trace_summary_bucket)
            .collect::<Vec<_>>()
            .join(",");
        let next = if end < total {
            end.to_string()
        } else {
            "null".to_string()
        };
        format!(
            r#"{{"items":[{}],"nextCursor":{},"total":{}}}"#,
            items, next, total
        )
    }

    fn product_query_spans(
        &self,
        snap: &yt_manifest::Snapshot,
        tenant: Option<u64>,
        attr_filter: &std::collections::BTreeMap<String, String>,
        annotation_spec: &TraceSearchAnnotationSpec,
        dataset_spec: &TraceSearchDatasetSpec,
    ) -> Vec<FoldedSpan> {
        let metadata_matches =
            self.trace_search_metadata_matches(annotation_spec, dataset_spec, tenant);
        let mut q = TraceQuery::all();
        q.tenant_id = tenant;
        let mut spans = if attr_filter.is_empty() {
            self.coord.read_spans_query(snap, &q).0
        } else {
            self.coord.read_spans_query_for_attrs(snap, &q, attr_filter)
        };
        spans.retain(|s| trace_search_metadata_match(s, &metadata_matches));
        spans
    }

    /// POST /v1/annotations：给 trace/span 追加一条后验 annotation。
    fn create_annotation_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        let v = match crate::wire::parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let Some((trace_id, external_trace_id)) =
            json_field_alias(&v, &["trace_id", "traceId"]).and_then(json_id_with_external)
        else {
            return (400, r#"{"error":"missing trace_id"}"#.to_string());
        };
        let span = json_field_alias(&v, &["span_id", "spanId"]).and_then(json_id_with_external);
        let label = json_field_alias(&v, &["label", "name"])
            .and_then(crate::wire::Json::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if label.is_empty() {
            return (400, r#"{"error":"missing label"}"#.to_string());
        }
        let target = json_field_alias(&v, &["target", "target_type", "targetType"])
            .and_then(crate::wire::Json::as_str)
            .and_then(AnnotationTarget::parse);
        let mut attrs = std::collections::BTreeMap::new();
        collect_attr_map(&v, &mut attrs);
        let annotation = self.coord.add_annotation(
            NewTraceAnnotation {
                target,
                trace_id,
                span_id: span.as_ref().map(|(id, _)| *id),
                external_trace_id: json_field_alias(&v, &["external_trace_id", "externalTraceId"])
                    .and_then(crate::wire::Json::as_str)
                    .map(ToString::to_string)
                    .or(external_trace_id),
                external_span_id: json_field_alias(&v, &["external_span_id", "externalSpanId"])
                    .and_then(crate::wire::Json::as_str)
                    .map(ToString::to_string)
                    .or_else(|| span.and_then(|(_, ext)| ext)),
                label,
                score: json_field_alias(&v, &["score", "eval_score", "evalScore"])
                    .and_then(crate::wire::Json::as_u64)
                    .map(|n| n.min(u32::MAX as u64) as u32),
                reason: json_field_alias(&v, &["reason", "comment", "note"])
                    .and_then(crate::wire::Json::as_str)
                    .map(ToString::to_string),
                source: json_field_alias(&v, &["source", "created_by", "createdBy"])
                    .and_then(crate::wire::Json::as_str)
                    .map(ToString::to_string),
                attrs,
            },
            tenant,
        );
        (200, json_annotation(&annotation))
    }

    /// GET /v1/annotations?trace_id=...&label=...&attrs={...}
    fn annotations_json(&self, query: &str, tenant: Option<u64>) -> (u16, String) {
        let mut filter = TraceAnnotationFilter {
            tenant_id: tenant,
            ..Default::default()
        };
        for (k, v) in query_pairs(query) {
            match k.as_str() {
                "target" | "target_type" | "targetType" => {
                    filter.target = AnnotationTarget::parse(&v);
                }
                "trace_id" | "traceId" => filter.trace_id = parse_id_or_hash(&v),
                "span_id" | "spanId" => filter.span_id = parse_id_or_hash(&v),
                "label" => filter.label = Some(v),
                "source" => filter.source = Some(v),
                "attrs" => collect_attr_query_json(&v, &mut filter.attrs),
                _ => collect_attr_query_pair(&k, &v, &mut filter.attrs),
            }
        }
        let items = self.coord.annotations(&filter);
        let body = items
            .iter()
            .map(json_annotation)
            .collect::<Vec<_>>()
            .join(",");
        (
            200,
            format!(r#"{{"items":[{}],"count":{}}}"#, body, items.len()),
        )
    }

    /// POST /v1/dataset-associations：把 trace/span 绑定到外部 dataset item。
    fn create_dataset_association_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        let v = match crate::wire::parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let dataset_id = json_field_alias(&v, &["dataset_id", "datasetId", "dataset"])
            .and_then(crate::wire::Json::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if dataset_id.is_empty() {
            return (400, r#"{"error":"missing dataset_id"}"#.to_string());
        }
        let item_id = json_field_alias(
            &v,
            &["item_id", "itemId", "dataset_item_id", "datasetItemId"],
        )
        .and_then(crate::wire::Json::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
        if item_id.is_empty() {
            return (400, r#"{"error":"missing item_id"}"#.to_string());
        }
        let Some((trace_id, external_trace_id)) =
            json_field_alias(&v, &["trace_id", "traceId"]).and_then(json_id_with_external)
        else {
            return (400, r#"{"error":"missing trace_id"}"#.to_string());
        };
        let span = json_field_alias(&v, &["span_id", "spanId"]).and_then(json_id_with_external);
        let mut attrs = std::collections::BTreeMap::new();
        collect_attr_map(&v, &mut attrs);
        let assoc = self.coord.add_dataset_association(
            NewDatasetAssociation {
                dataset_id,
                item_id,
                trace_id,
                span_id: span.as_ref().map(|(id, _)| *id),
                external_trace_id: json_field_alias(&v, &["external_trace_id", "externalTraceId"])
                    .and_then(crate::wire::Json::as_str)
                    .map(ToString::to_string)
                    .or(external_trace_id),
                external_span_id: json_field_alias(&v, &["external_span_id", "externalSpanId"])
                    .and_then(crate::wire::Json::as_str)
                    .map(ToString::to_string)
                    .or_else(|| span.and_then(|(_, ext)| ext)),
                snapshot_id: json_field_alias(&v, &["snapshot_id", "snapshotId"])
                    .and_then(crate::wire::Json::as_str)
                    .map(ToString::to_string),
                snapshot_hash: json_field_alias(&v, &["snapshot_hash", "snapshotHash"])
                    .and_then(crate::wire::Json::as_str)
                    .map(ToString::to_string),
                eval_run_id: json_field_alias(&v, &["eval_run_id", "evalRunId"])
                    .and_then(crate::wire::Json::as_str)
                    .map(ToString::to_string),
                split: json_field_alias(&v, &["split"])
                    .and_then(crate::wire::Json::as_str)
                    .map(ToString::to_string),
                label: json_field_alias(&v, &["label"])
                    .and_then(crate::wire::Json::as_str)
                    .map(ToString::to_string),
                score: json_field_alias(&v, &["score", "eval_score", "evalScore"])
                    .and_then(crate::wire::Json::as_u64)
                    .map(|n| n.min(u32::MAX as u64) as u32),
                attrs,
            },
            tenant,
        );
        (200, json_dataset_association(&assoc))
    }

    /// GET /v1/dataset-associations?dataset_id=...&item_id=...&trace_id=...
    fn dataset_associations_json(&self, query: &str, tenant: Option<u64>) -> (u16, String) {
        let mut filter = DatasetAssociationFilter {
            tenant_id: tenant,
            ..Default::default()
        };
        for (k, v) in query_pairs(query) {
            match k.as_str() {
                "dataset_id" | "datasetId" | "dataset" => filter.dataset_id = Some(v),
                "item_id" | "itemId" | "dataset_item_id" | "datasetItemId" => {
                    filter.item_id = Some(v)
                }
                "trace_id" | "traceId" => filter.trace_id = parse_id_or_hash(&v),
                "span_id" | "spanId" => filter.span_id = parse_id_or_hash(&v),
                "eval_run_id" | "evalRunId" => filter.eval_run_id = Some(v),
                "split" => filter.split = Some(v),
                "label" => filter.label = Some(v),
                "attrs" => collect_attr_query_json(&v, &mut filter.attrs),
                _ => collect_attr_query_pair(&k, &v, &mut filter.attrs),
            }
        }
        let items = self.coord.dataset_associations(&filter);
        let body = items
            .iter()
            .map(json_dataset_association)
            .collect::<Vec<_>>()
            .join(",");
        (
            200,
            format!(r#"{{"items":[{}],"count":{}}}"#, body, items.len()),
        )
    }

    /// POST /v1/golden-paths：把一条 trace/trajectory 登记为 golden path 候选。
    fn create_golden_path_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        let v = match crate::wire::parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let Some((trace_id, external_trace_id)) = json_field_alias(
            &v,
            &["trace_id", "traceId", "sourceTraceId", "source_trace_id"],
        )
        .and_then(json_id_with_external) else {
            return (400, r#"{"error":"missing sourceTraceId"}"#.to_string());
        };
        let snap = self.coord.pin_snapshot();
        let spans = self.trace_folded_spans(&snap, trace_id, tenant);
        if spans.is_empty() {
            return (404, r#"{"error":"source trace not found"}"#.to_string());
        }
        let source_steps = trajectory_steps(&spans);
        let task_fingerprint = json_field_alias(
            &v,
            &["task_fingerprint", "taskFingerprint", "task", "taskId"],
        )
        .and_then(crate::wire::Json::as_str)
        .map(ToString::to_string)
        .or_else(|| {
            spans
                .iter()
                .find_map(|s| crate::folded_span_attr_value(s, "task_fingerprint"))
                .map(json_compact_label)
        });
        let Some(task_fingerprint) = task_fingerprint.filter(|s| !s.trim().is_empty()) else {
            return (400, r#"{"error":"missing taskFingerprint"}"#.to_string());
        };
        let trajectory_signature = json_field_alias(
            &v,
            &[
                "trajectory_signature",
                "trajectorySignature",
                "signature",
                "pathSignature",
            ],
        )
        .and_then(crate::wire::Json::as_str)
        .map(ToString::to_string)
        .unwrap_or_else(|| trajectory_signature_string(&source_steps));
        let status = json_field_alias(&v, &["status"])
            .and_then(crate::wire::Json::as_str)
            .and_then(GoldenPathStatus::parse);
        let mut attrs = std::collections::BTreeMap::new();
        collect_attr_map(&v, &mut attrs);
        collect_golden_path_scope_attrs(&spans, &mut attrs);
        let evidence = golden_path_evidence_summary_from_json(&v, &spans);
        let candidate = self.coord.add_golden_path(
            NewGoldenPathCandidate {
                task_fingerprint,
                trajectory_signature,
                source_trace_id: trace_id,
                external_source_trace_id: json_field_alias(
                    &v,
                    &["external_source_trace_id", "externalSourceTraceId"],
                )
                .and_then(crate::wire::Json::as_str)
                .map(ToString::to_string)
                .or(external_trace_id),
                snapshot_id: json_field_alias(&v, &["snapshot_id", "snapshotId"])
                    .and_then(crate::wire::Json::as_str)
                    .map(ToString::to_string),
                snapshot_hash: json_field_alias(&v, &["snapshot_hash", "snapshotHash"])
                    .and_then(crate::wire::Json::as_str)
                    .map(ToString::to_string),
                status,
                score: json_field_alias(&v, &["score", "qualityScore"])
                    .and_then(crate::wire::Json::as_u64)
                    .map(score_u64),
                label: json_field_alias(&v, &["label", "name"])
                    .and_then(crate::wire::Json::as_str)
                    .map(ToString::to_string),
                reason: json_field_alias(&v, &["reason", "comment", "note"])
                    .and_then(crate::wire::Json::as_str)
                    .map(ToString::to_string),
                source: json_field_alias(&v, &["source", "created_by", "createdBy"])
                    .and_then(crate::wire::Json::as_str)
                    .map(ToString::to_string),
                attrs,
                source_trajectory_steps: source_steps,
                evidence,
            },
            tenant,
        );
        (200, json_golden_path(&candidate))
    }

    /// GET /v1/golden-paths?taskFingerprint=...&status=confirmed
    fn golden_paths_json(&self, query: &str, tenant: Option<u64>) -> (u16, String) {
        let mut filter = GoldenPathFilter {
            tenant_id: tenant,
            ..Default::default()
        };
        for (k, v) in query_pairs(query) {
            match k.as_str() {
                "golden_path_id" | "goldenPathId" | "id" => {
                    filter.golden_path_id = parse_id_or_hash(&v)
                }
                "task_fingerprint" | "taskFingerprint" | "task" => {
                    filter.task_fingerprint = Some(v)
                }
                "trajectory_signature" | "trajectorySignature" | "signature" => {
                    filter.trajectory_signature = Some(v)
                }
                "trace_id" | "traceId" | "sourceTraceId" | "source_trace_id" => {
                    filter.source_trace_id = parse_id_or_hash(&v)
                }
                "status" => filter.status = GoldenPathStatus::parse(&v),
                "attrs" => collect_attr_query_json(&v, &mut filter.attrs),
                "model" | "provider" => {
                    filter.attrs.insert(k, json_string_value(&v));
                }
                _ => collect_attr_query_pair(&k, &v, &mut filter.attrs),
            }
        }
        let mut items = self.coord.golden_paths(&filter);
        items.sort_by(|a, b| {
            b.updated_at_ns
                .cmp(&a.updated_at_ns)
                .then_with(|| a.golden_path_id.cmp(&b.golden_path_id))
        });
        let body = items
            .iter()
            .map(json_golden_path)
            .collect::<Vec<_>>()
            .join(",");
        (
            200,
            format!(r#"{{"items":[{}],"count":{}}}"#, body, items.len()),
        )
    }

    /// POST /v1/golden-paths/:id/status：确认、拒绝或废弃候选路径。
    fn update_golden_path_status_json(
        &self,
        id: &str,
        body: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let Some(golden_path_id) = parse_id_or_hash(id) else {
            return (400, r#"{"error":"bad golden path id"}"#.to_string());
        };
        let v = match crate::wire::parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let Some(status) = json_field_alias(&v, &["status"])
            .and_then(crate::wire::Json::as_str)
            .and_then(GoldenPathStatus::parse)
        else {
            return (400, r#"{"error":"missing status"}"#.to_string());
        };
        let updated = self.coord.update_golden_path_status(
            golden_path_id,
            tenant,
            status,
            json_field_alias(&v, &["score", "qualityScore"])
                .and_then(crate::wire::Json::as_u64)
                .map(score_u64),
            json_field_alias(&v, &["reason", "comment", "note"])
                .and_then(crate::wire::Json::as_str)
                .map(ToString::to_string),
            json_field_alias(&v, &["source", "updated_by", "updatedBy"])
                .and_then(crate::wire::Json::as_str)
                .map(ToString::to_string),
        );
        match updated {
            Some(path) => (200, json_golden_path(&path)),
            None => (404, r#"{"error":"golden path not found"}"#.to_string()),
        }
    }

    /// POST /v1/path-adherence：比较一条 trace 是否遵循某个 golden path。
    ///
    /// 这是底座读模型：只返回 trajectory signature、共同步骤、缺失步骤和额外步骤，不替业务判断
    /// “这是不是当前最佳路径”。
    fn path_adherence_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        let v = match crate::wire::parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let Some(golden_path_id) = json_field_alias(&v, &["golden_path_id", "goldenPathId", "id"])
            .and_then(json_internal_id)
        else {
            return (400, r#"{"error":"missing goldenPathId"}"#.to_string());
        };
        let Some((trace_id, _)) = json_field_alias(
            &v,
            &["trace_id", "traceId", "candidateTraceId", "candidate"],
        )
        .and_then(json_id_with_external) else {
            return (400, r#"{"error":"missing traceId"}"#.to_string());
        };
        self.path_adherence_result_json(golden_path_id, trace_id, tenant)
    }

    /// POST /v1/golden-paths/:id/adherence：路径参数传 goldenPathId，body 只需 traceId。
    fn path_adherence_for_golden_path_json(
        &self,
        id: &str,
        body: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let Some(golden_path_id) = parse_id_or_hash(id) else {
            return (400, r#"{"error":"bad golden path id"}"#.to_string());
        };
        let v = match crate::wire::parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let Some((trace_id, _)) = json_field_alias(
            &v,
            &["trace_id", "traceId", "candidateTraceId", "candidate"],
        )
        .and_then(json_id_with_external) else {
            return (400, r#"{"error":"missing traceId"}"#.to_string());
        };
        self.path_adherence_result_json(golden_path_id, trace_id, tenant)
    }

    fn path_adherence_result_json(
        &self,
        golden_path_id: u64,
        trace_id: u64,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let filter = GoldenPathFilter {
            tenant_id: tenant,
            golden_path_id: Some(golden_path_id),
            ..Default::default()
        };
        let Some(golden_path) = self.coord.golden_paths(&filter).into_iter().next() else {
            return (404, r#"{"error":"golden path not found"}"#.to_string());
        };

        let snap = self.coord.pin_snapshot();
        let trace_spans = self.trace_folded_spans(&snap, trace_id, tenant);
        if trace_spans.is_empty() {
            return (404, r#"{"error":"trace not found"}"#.to_string());
        }
        let source_spans = self.trace_folded_spans(&snap, golden_path.source_trace_id, tenant);
        (
            200,
            json_path_adherence(&golden_path, trace_id, &trace_spans, &source_spans),
        )
    }

    /// POST /v1/golden-path-evidence：导出 Golden Path 的底层证据包。
    ///
    /// 默认只返回 source trace 的摘要/trajectory/annotation/dataset 证据。传 `candidateTraceId`
    /// 时，额外返回 pathAdherence 和 traceDiff，供上层做评审、回归集或 Agent Memory 导出。
    fn golden_path_evidence_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        let v = match crate::wire::parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let Some(golden_path_id) = json_field_alias(&v, &["golden_path_id", "goldenPathId", "id"])
            .and_then(json_internal_id)
        else {
            return (400, r#"{"error":"missing goldenPathId"}"#.to_string());
        };
        let candidate_trace_id = json_field_alias(
            &v,
            &[
                "candidate_trace_id",
                "candidateTraceId",
                "trace_id",
                "traceId",
                "candidate",
            ],
        )
        .and_then(json_id_with_external)
        .map(|(id, _)| id);
        self.golden_path_evidence_result_json(golden_path_id, candidate_trace_id, tenant)
    }

    /// POST /v1/golden-paths/:id/evidence：路径参数传 goldenPathId，body 可选 candidateTraceId。
    fn golden_path_evidence_for_id_json(
        &self,
        id: &str,
        body: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let Some(golden_path_id) = parse_id_or_hash(id) else {
            return (400, r#"{"error":"bad golden path id"}"#.to_string());
        };
        let v = match parse_json_body_or_empty(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let candidate_trace_id = json_field_alias(
            &v,
            &[
                "candidate_trace_id",
                "candidateTraceId",
                "trace_id",
                "traceId",
                "candidate",
            ],
        )
        .and_then(json_id_with_external)
        .map(|(id, _)| id);
        self.golden_path_evidence_result_json(golden_path_id, candidate_trace_id, tenant)
    }

    fn golden_path_evidence_result_json(
        &self,
        golden_path_id: u64,
        candidate_trace_id: Option<u64>,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let filter = GoldenPathFilter {
            tenant_id: tenant,
            golden_path_id: Some(golden_path_id),
            ..Default::default()
        };
        let Some(golden_path) = self.coord.golden_paths(&filter).into_iter().next() else {
            return (404, r#"{"error":"golden path not found"}"#.to_string());
        };

        let snap = self.coord.pin_snapshot();
        let source_spans = self.trace_folded_spans(&snap, golden_path.source_trace_id, tenant);
        let source = self.trace_evidence_json(golden_path.source_trace_id, &source_spans, tenant);
        let candidate = match candidate_trace_id {
            Some(trace_id) => {
                let trace_spans = self.trace_folded_spans(&snap, trace_id, tenant);
                if trace_spans.is_empty() {
                    return (404, r#"{"error":"candidate trace not found"}"#.to_string());
                }
                let evidence = self.trace_evidence_json(trace_id, &trace_spans, tenant);
                let adherence =
                    json_path_adherence(&golden_path, trace_id, &trace_spans, &source_spans);
                let diff = if source_spans.is_empty() {
                    "null".to_string()
                } else {
                    json_trace_diff(
                        golden_path.source_trace_id,
                        trace_id,
                        &source_spans,
                        &trace_spans,
                    )
                };
                format!(
                    r#"{{"evidence":{},"pathAdherence":{},"traceDiff":{}}}"#,
                    evidence, adherence, diff
                )
            }
            None => "null".to_string(),
        };
        (
            200,
            format!(
                r#"{{"goldenPath":{},"source":{},"candidate":{}}}"#,
                json_golden_path(&golden_path),
                source,
                candidate,
            ),
        )
    }

    fn trace_evidence_json(
        &self,
        trace_id: u64,
        spans: &[FoldedSpan],
        tenant: Option<u64>,
    ) -> String {
        let summary = trace_summary_buckets_from_spans(spans);
        let trajectory = if spans.is_empty() {
            "null".to_string()
        } else {
            let steps = trajectory_steps(spans);
            trajectory_summary_json_with_signature(&steps, &trajectory_signature_string(&steps))
        };
        let annotations = self.coord.annotations(&TraceAnnotationFilter {
            tenant_id: tenant,
            trace_id: Some(trace_id),
            ..Default::default()
        });
        let datasets = self.coord.dataset_associations(&DatasetAssociationFilter {
            tenant_id: tenant,
            trace_id: Some(trace_id),
            ..Default::default()
        });
        let annotations_json = annotations
            .iter()
            .map(json_annotation)
            .collect::<Vec<_>>()
            .join(",");
        let datasets_json = datasets
            .iter()
            .map(json_dataset_association)
            .collect::<Vec<_>>()
            .join(",");
        format!(
            r#"{{"available":{},"trace":{},"trajectory":{},"annotations":[{}],"annotationCount":{},"datasetAssociations":[{}],"datasetAssociationCount":{}}}"#,
            json_bool(!spans.is_empty()),
            trace_diff_side_json(trace_id, summary.first()),
            trajectory,
            annotations_json,
            annotations.len(),
            datasets_json,
            datasets.len(),
        )
    }

    /// POST /v1/golden-path-export：稳定 JSONL 导出。
    ///
    /// 默认只导出 confirmed Golden Path；显式传 `status` 可导出 candidate/rejected/deprecated。
    /// 响应同时给 items 和 jsonl，方便 Node 直接消费或写入外部管线。
    fn golden_path_export_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        let v = match parse_json_body_or_empty(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let filter_source = json_field_alias(&v, &["filter"]).unwrap_or(&v);
        let (mut filter, explicit_status) =
            match golden_path_filter_from_json(filter_source, tenant) {
                Ok(out) => out,
                Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
            };
        if !explicit_status {
            filter.status = Some(GoldenPathStatus::Confirmed);
        }
        let limit = json_field_alias(&v, &["limit", "k"])
            .and_then(crate::wire::Json::as_u64)
            .unwrap_or(100)
            .clamp(1, 500) as usize;

        let mut paths = self.coord.golden_paths(&filter);
        paths.sort_by(|a, b| {
            b.updated_at_ns
                .cmp(&a.updated_at_ns)
                .then_with(|| a.golden_path_id.cmp(&b.golden_path_id))
        });
        paths.truncate(limit);

        let snap = self.coord.pin_snapshot();
        let records = paths
            .iter()
            .map(|path| {
                let source_spans = self.trace_folded_spans(&snap, path.source_trace_id, tenant);
                self.golden_path_export_record_json(path, &source_spans, tenant)
            })
            .collect::<Vec<_>>();
        let jsonl = records.join("\n");
        (
            200,
            format!(
                r#"{{"schemaVersion":"yitrace.golden_path_export.v1","format":"jsonl","count":{},"items":[{}],"jsonl":{}}}"#,
                records.len(),
                records.join(","),
                json_string_value(&jsonl),
            ),
        )
    }

    fn golden_path_export_record_json(
        &self,
        path: &crate::GoldenPathCandidate,
        source_spans: &[FoldedSpan],
        tenant: Option<u64>,
    ) -> String {
        let evidence = self.trace_evidence_json(path.source_trace_id, source_spans, tenant);
        format!(
            r#"{{"schemaVersion":"yitrace.golden_path_export.v1","recordType":"golden_path","goldenPath":{},"source":{},"exportedAtNs":"{}"}}"#,
            json_golden_path(path),
            evidence,
            unix_now_ns(),
        )
    }

    /// POST /v1/golden-path-health：统计一批同 scope trace 对某条 Golden Path 的遵循情况。
    ///
    /// 这是底座证据模型：默认用 Golden Path 的 taskFingerprint + attrs 收窄窗口，并排除 source trace。
    /// 它只输出 followed/extended/partial/deviated 分布和覆盖率，不维护“当前最佳路径”。
    fn golden_path_health_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        let v = match parse_json_body_or_empty(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let Some(golden_path_id) = json_field_alias(&v, &["golden_path_id", "goldenPathId", "id"])
            .and_then(json_internal_id)
        else {
            return (400, r#"{"error":"missing goldenPathId"}"#.to_string());
        };
        self.golden_path_health_result_json(golden_path_id, &v, tenant)
    }

    /// POST /v1/golden-paths/:id/health：路径参数传 goldenPathId，body 可传 filter/limit。
    fn golden_path_health_for_id_json(
        &self,
        id: &str,
        body: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let Some(golden_path_id) = parse_id_or_hash(id) else {
            return (400, r#"{"error":"bad golden path id"}"#.to_string());
        };
        let v = match parse_json_body_or_empty(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        self.golden_path_health_result_json(golden_path_id, &v, tenant)
    }

    fn golden_path_health_result_json(
        &self,
        golden_path_id: u64,
        v: &crate::wire::Json,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let filter = GoldenPathFilter {
            tenant_id: tenant,
            golden_path_id: Some(golden_path_id),
            ..Default::default()
        };
        let Some(golden_path) = self.coord.golden_paths(&filter).into_iter().next() else {
            return (404, r#"{"error":"golden path not found"}"#.to_string());
        };

        let limit = json_field_alias(v, &["limit", "k"])
            .and_then(crate::wire::Json::as_u64)
            .unwrap_or(100)
            .clamp(1, 500) as usize;
        let example_limit = json_field_alias(v, &["example_limit", "exampleLimit", "examples"])
            .and_then(crate::wire::Json::as_u64)
            .unwrap_or(5)
            .clamp(0, 50) as usize;
        let include_source =
            json_bool_alias(v, &["include_source", "includeSource"]).unwrap_or(false);

        let mut request = trace_search_request_from_json(v, tenant);
        if !golden_path.task_fingerprint.is_empty() {
            request
                .spec
                .attrs
                .entry("task_fingerprint".to_string())
                .or_insert_with(|| json_string_value(&golden_path.task_fingerprint));
        }
        for (key, value) in &golden_path.attrs {
            request
                .spec
                .attrs
                .entry(key.clone())
                .or_insert_with(|| value.clone());
        }

        let metadata_matches =
            self.trace_search_metadata_matches(&request.annotation, &request.dataset, tenant);
        let snap = self.coord.pin_snapshot();
        let mut matching_spans = if request.spec.attrs.is_empty() {
            self.coord.read_spans_query(&snap, &request.query).0
        } else {
            self.coord
                .read_spans_query_for_attrs(&snap, &request.query, &request.spec.attrs)
        };
        matching_spans.retain(|s| trace_search_match(s, &request.spec, &metadata_matches));
        let span_total = matching_spans.len();
        let mut trace_ids: Vec<u64> = matching_spans
            .iter()
            .map(|s| s.trace_id)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        if !include_source {
            trace_ids.retain(|trace_id| *trace_id != golden_path.source_trace_id);
        }
        trace_ids.sort_by(|a, b| b.cmp(a));
        let matching_trace_total = trace_ids.len();
        trace_ids.truncate(limit);

        let source_spans = self.trace_folded_spans(&snap, golden_path.source_trace_id, tenant);
        let source_available = !source_spans.is_empty();
        let source_retained = !golden_path.source_trajectory_steps.is_empty();
        let source_steps = if source_available {
            trajectory_steps(&source_spans)
        } else if source_retained {
            golden_path.source_trajectory_steps.clone()
        } else {
            Vec::new()
        };
        let source_signature =
            (!source_steps.is_empty()).then(|| trajectory_signature_string(&source_steps));
        let stored_signature_matches_source = source_signature
            .as_ref()
            .map(|signature| signature == &golden_path.trajectory_signature);

        let mut analyzed_trace_total = 0usize;
        let mut followed = 0usize;
        let mut extended = 0usize;
        let mut partial = 0usize;
        let mut deviated = 0usize;
        let mut unknown = 0usize;
        let mut common_step_count = 0usize;
        let mut golden_step_count = 0usize;
        let mut trace_step_count = 0usize;
        let mut examples = Vec::new();

        for trace_id in trace_ids {
            let Some(trajectory) = self
                .coord
                .materialized_trace_trajectory(&snap, trace_id, tenant)
            else {
                continue;
            };
            let facts = path_adherence_facts_from_steps(
                &golden_path,
                trajectory.steps.clone(),
                &source_spans,
            );
            match facts.adherence() {
                "followed" => followed += 1,
                "extended" => extended += 1,
                "partial" => partial += 1,
                "deviated" => deviated += 1,
                _ => unknown += 1,
            }
            analyzed_trace_total += 1;
            common_step_count += facts.common_steps.len();
            golden_step_count += facts.source_steps.len();
            trace_step_count += facts.trace_steps.len();
            if examples.len() < example_limit {
                examples.push(path_adherence_health_example_json(&trajectory, &facts));
            }
        }

        let examples_json = examples.join(",");
        (
            200,
            format!(
                r#"{{"goldenPath":{},"sourceAvailable":{},"sourceRetained":{},"storedSignatureMatchesSource":{},"goldenTrajectory":{},"sourceTrajectory":{},"window":{{"limit":{},"includeSource":{},"spanTotal":{},"matchingTraceTotal":{},"analyzedTraceTotal":{}}},"counts":{{"total":{},"followed":{},"extended":{},"partial":{},"deviated":{},"unknown":{}}},"rates":{{"followed":{},"usable":{},"deviated":{},"unknown":{}}},"coverage":{{"commonStepCount":{},"goldenStepCount":{},"traceStepCount":{},"goldenCoverage":{},"traceCoverage":{}}},"examples":[{}]}}"#,
                json_golden_path(&golden_path),
                json_bool(source_available),
                json_bool(source_retained),
                json_opt_bool(stored_signature_matches_source),
                trajectory_summary_json_with_signature(
                    &source_steps,
                    &golden_path.trajectory_signature
                ),
                source_signature
                    .as_ref()
                    .map(|signature| trajectory_summary_json_with_signature(
                        &source_steps,
                        signature
                    ))
                    .unwrap_or_else(|| "null".to_string()),
                limit,
                json_bool(include_source),
                span_total,
                matching_trace_total,
                analyzed_trace_total,
                analyzed_trace_total,
                followed,
                extended,
                partial,
                deviated,
                unknown,
                ratio_json(followed, analyzed_trace_total),
                ratio_json(followed + extended, analyzed_trace_total),
                ratio_json(deviated, analyzed_trace_total),
                ratio_json(unknown, analyzed_trace_total),
                common_step_count,
                golden_step_count,
                trace_step_count,
                ratio_json(common_step_count, golden_step_count),
                ratio_json(common_step_count, trace_step_count),
                examples_json,
            ),
        )
    }

    fn trace_search_metadata_matches(
        &self,
        annotation_spec: &TraceSearchAnnotationSpec,
        dataset_spec: &TraceSearchDatasetSpec,
        tenant: Option<u64>,
    ) -> TraceSearchMetadataMatches {
        let mut matches = TraceSearchMetadataMatches {
            need_annotation: annotation_spec.active,
            need_dataset: dataset_spec.active,
            ..Default::default()
        };
        if annotation_spec.active {
            let items = self.coord.annotations(&TraceAnnotationFilter {
                tenant_id: tenant,
                target: annotation_spec.target,
                label: annotation_spec.label.clone(),
                source: annotation_spec.source.clone(),
                attrs: annotation_spec.attrs.clone(),
                ..Default::default()
            });
            for a in items.into_iter().filter(|a| {
                score_in_range(
                    a.score,
                    annotation_spec.score_min,
                    annotation_spec.score_max,
                )
            }) {
                matches.annotation_candidate_traces.insert(a.trace_id);
                match (a.target, a.span_id) {
                    (AnnotationTarget::Span, Some(span_id)) => {
                        matches.annotation_spans.insert((a.trace_id, span_id));
                    }
                    _ => {
                        matches.annotation_traces.insert(a.trace_id);
                    }
                }
            }
        }
        if dataset_spec.active {
            let items = self.coord.dataset_associations(&DatasetAssociationFilter {
                tenant_id: tenant,
                dataset_id: dataset_spec.dataset_id.clone(),
                item_id: dataset_spec.item_id.clone(),
                eval_run_id: dataset_spec.eval_run_id.clone(),
                split: dataset_spec.split.clone(),
                label: dataset_spec.label.clone(),
                attrs: dataset_spec.attrs.clone(),
                ..Default::default()
            });
            for d in items
                .into_iter()
                .filter(|d| score_in_range(d.score, dataset_spec.score_min, dataset_spec.score_max))
            {
                matches.dataset_candidate_traces.insert(d.trace_id);
                if let Some(span_id) = d.span_id {
                    matches.dataset_spans.insert((d.trace_id, span_id));
                } else {
                    matches.dataset_traces.insert(d.trace_id);
                }
            }
        }
        matches
    }

    fn metadata_matching_session_ids(
        &self,
        snap: &yt_manifest::Snapshot,
        metadata: &TraceSearchMetadataMatches,
        tenant: Option<u64>,
    ) -> std::collections::HashSet<u64> {
        let mut out = std::collections::HashSet::new();
        for trace_id in metadata_candidate_trace_ids(metadata) {
            let mut q = TraceQuery::trace(trace_id, i64::MIN, i64::MAX);
            q.tenant_id = tenant;
            let (spans, _) = self.coord.read_spans_query(snap, &q);
            for span in spans {
                if trace_search_metadata_match(&span, metadata) {
                    if let Some(session_id) = span.session_id {
                        out.insert(session_id);
                    }
                }
            }
        }
        out
    }

    fn traces_json(&self, query: &str, tenant: Option<u64>) -> String {
        let mut attr_filter = std::collections::BTreeMap::new();
        let pairs = query_pairs(query);
        for (k, v) in &pairs {
            match k.as_str() {
                "attrs" => collect_attr_query_json(v, &mut attr_filter),
                _ => {
                    if let Some((_, attr_key)) =
                        attr_aliases().iter().find(|(alias, _)| *alias == k)
                    {
                        attr_filter.insert((*attr_key).to_string(), json_string_value(v));
                    }
                }
            }
        }
        let annotation_spec = trace_search_annotation_spec_from_query(&pairs);
        let dataset_spec = trace_search_dataset_spec_from_query(&pairs);
        let metadata_matches =
            self.trace_search_metadata_matches(&annotation_spec, &dataset_spec, tenant);
        let snap = self.coord.pin_snapshot();
        let mut traces = if attr_filter.is_empty() {
            let mut q = TraceQuery::all();
            q.tenant_id = tenant; // 租户隔离：只列本租户的 trace
            self.coord.list_traces(&snap, &q)
        } else {
            self.coord
                .list_traces_for_tenant_and_attrs(&snap, tenant, &attr_filter)
        };
        if metadata_matches.need_annotation || metadata_matches.need_dataset {
            traces.retain(|t| trace_id_metadata_match(t.trace_id, &metadata_matches));
        }
        let trace_ids: std::collections::HashSet<u64> = traces.iter().map(|t| t.trace_id).collect();
        let fields_by_trace = self
            .coord
            .trace_attr_fields_for_tenant_and_traces(&snap, tenant, &trace_ids);
        let items: Vec<String> = traces
            .iter()
            .map(|t| {
                let fields = fields_by_trace
                    .get(&t.trace_id)
                    .map(json_attrs)
                    .unwrap_or_else(|| "{}".to_string());
                format!(
                    r#"{{"trace_id":{},"external_trace_id":{},"span_count":{},"total_duration_ns":{},"max_duration_ns":{},"error_count":{},"total_input_tokens":{},"total_output_tokens":{},"total_cached_input_tokens":{},"total_reasoning_tokens":{},"total_tokens":{},"total_cost_usd":{},"total_cost_usd_nanos":{},"usage":{},"costDetail":{},"fields":{}}}"#,
                    t.trace_id,
                    json_opt_str(t.external_trace_id.as_deref()),
                    t.span_count,
                    t.total_duration_ns,
                    t.max_duration_ns,
                    t.error_count,
                    t.total_input_tokens,
                    t.total_output_tokens,
                    t.total_cached_input_tokens,
                    t.total_reasoning_tokens,
                    t.total_tokens,
                    cost_usd_num_from_nanos(t.total_cost_usd_nanos),
                    t.total_cost_usd_nanos,
                    usage_json(
                        t.total_input_tokens,
                        t.total_output_tokens,
                        t.total_cached_input_tokens,
                        t.total_reasoning_tokens,
                        t.total_tokens,
                    ),
                    cost_detail_json(t.total_cost_usd_nanos, Some("USD"), "mixed"),
                    fields
                )
            })
            .collect();
        format!("[{}]", items.join(","))
    }

    // ───────────────────── 控制台数据端点（游标分页 / 轮次 / span / 详情） ─────────────────────

    /// GET /v1/sessions?cursor=&limit=：会话列表，offset 游标分页。
    /// `console_sessions` 走增量边车索引（摄入时 O(1) 维护），分页不全扫（见引擎实现）。
    fn sessions_page_json(&self, query: &str, tenant: Option<u64>) -> String {
        let (mut offset, mut limit, mut filter) = (0usize, 50usize, String::new());
        let mut attr_filter = std::collections::BTreeMap::new();
        let pairs = query_pairs(query);
        for (k, v) in &pairs {
            match k.as_str() {
                "cursor" => offset = v.parse().unwrap_or(0),
                "limit" => limit = v.parse().unwrap_or(50).clamp(1, 500),
                "filter" => filter = v.clone(),
                "attrs" => collect_attr_query_json(v, &mut attr_filter),
                _ => {
                    if let Some((_, attr_key)) =
                        attr_aliases().iter().find(|(alias, _)| *alias == k)
                    {
                        attr_filter.insert((*attr_key).to_string(), json_string_value(v));
                    }
                }
            }
        }
        let annotation_spec = trace_search_annotation_spec_from_query(&pairs);
        let dataset_spec = trace_search_dataset_spec_from_query(&pairs);
        let metadata_matches =
            self.trace_search_metadata_matches(&annotation_spec, &dataset_spec, tenant);
        let snap = self.coord.pin_snapshot();
        let mut all = if attr_filter.is_empty() {
            self.coord.console_sessions_for_tenant(&snap, tenant)
        } else {
            self.coord
                .console_sessions_for_tenant_and_attrs(&snap, tenant, &attr_filter)
        };
        if metadata_matches.need_annotation || metadata_matches.need_dataset {
            let session_ids = self.metadata_matching_session_ids(&snap, &metadata_matches, tenant);
            all.retain(|s| session_ids.contains(&s.session_id));
        }
        if !filter.is_empty() {
            all.retain(|s| s.title.contains(&filter) || s.session_id.to_string().contains(&filter));
        }
        let total = all.len();
        let end = (offset + limit).min(total);
        let page = if offset < total {
            &all[offset..end]
        } else {
            &[][..]
        };
        let items: Vec<String> = page
            .iter()
            .map(|s| {
                format!(
                    r#"{{"sessionId":"{}","externalSessionId":{},"title":"{}","turnCount":{},"totalCost":{},"costUsd":{},"costDetail":{},"usage":{},"status":"{}","startedAt":{},"firstTraceId":"{}"}}"#,
                    s.session_id,
                    json_opt_str(s.external_session_id.as_deref()),
                    json_escape(&s.title),
                    s.turn_count,
                    cost_num(s.input_tokens, s.output_tokens),
                    cost_usd_num_from_nanos(s.cost_usd_nanos),
                    cost_detail_json(s.cost_usd_nanos, Some("USD"), "mixed"),
                    usage_json(
                        s.input_tokens,
                        s.output_tokens,
                        s.cached_input_tokens,
                        s.reasoning_tokens,
                        s.total_tokens,
                    ),
                    if s.has_error { "error" } else { "ok" },
                    s.session_id,
                    s.first_trace_id,
                )
            })
            .collect();
        let next = if end < total {
            end.to_string()
        } else {
            "null".to_string()
        };
        format!(
            r#"{{"items":[{}],"nextCursor":{},"total":{}}}"#,
            items.join(","),
            next,
            total
        )
    }

    /// GET /v1/sessions/:id/turns：一个会话的轮次（按时序）。
    fn turns_json(&self, id: &str, tenant: Option<u64>) -> (u16, String) {
        let Some(sid) = parse_id_or_hash(id) else {
            return (400, r#"{"error":"bad session id"}"#.to_string());
        };
        let snap = self.coord.pin_snapshot();
        let mut q = TraceQuery::all();
        q.tenant_id = tenant;
        let tl = self.coord.load_session_timeline_query(&snap, sid, &q);
        let items: Vec<String> = tl
            .turns
            .iter()
            .map(|t| {
                // 真实耗时：对该轮 trace 求 span 时长之和（毫秒）。
                let spans = self.coord.console_trace_spans_for_tenant(&snap, t.trace_id, tenant);
                let dur_ms = spans.iter().map(|s| s.duration_ns).sum::<u64>() / 1_000_000;
                let name = t.user_input.as_deref().map(trunc).unwrap_or_else(|| format!("第{}轮", t.turn_index + 1));
                format!(
                    r#"{{"traceId":"{}","sessionId":"{}","turnIndex":{},"name":"{}","durMs":{},"cost":{},"costUsd":{},"costDetail":{},"usage":{},"inTok":{},"outTok":{},"spanCount":{},"status":"{}"}}"#,
                    t.trace_id,
                    sid,
                    t.turn_index,
                    json_escape(&name),
                    dur_ms,
                    cost_num(t.input_tokens, t.output_tokens),
                    cost_usd_num_from_nanos(t.cost_usd_nanos),
                    cost_detail_json(t.cost_usd_nanos, Some("USD"), "mixed"),
                    usage_json(
                        t.input_tokens,
                        t.output_tokens,
                        t.cached_input_tokens,
                        t.reasoning_tokens,
                        t.total_tokens,
                    ),
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
    fn trace_json(&self, id: &str, tenant: Option<u64>) -> (u16, String) {
        let Some(tid) = parse_id_or_hash(id) else {
            return (400, r#"{"error":"bad trace id"}"#.to_string());
        };
        let snap = self.coord.pin_snapshot();
        let spans = self
            .coord
            .console_trace_spans_for_tenant(&snap, tid, tenant);
        if spans.is_empty() {
            return (404, r#"{"error":"trace not found"}"#.to_string());
        }
        // 深度：顺父指针数（用 span_id→parent 映射 + 记忆化）。
        let parent: std::collections::HashMap<u64, Option<u64>> = spans
            .iter()
            .map(|s| (s.span_id, s.parent_span_id))
            .collect();
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
        let (in_tok, out_tok): (u64, u64) = spans.iter().fold((0, 0), |(i, o), s| {
            (i + s.input_tokens, o + s.output_tokens)
        });
        let cached_tok: u64 = spans.iter().map(|s| s.cached_input_tokens).sum();
        let reasoning_tok: u64 = spans.iter().map(|s| s.reasoning_tokens).sum();
        let total_tokens: u64 = spans.iter().map(|s| s.total_tokens).sum();
        let cost_usd_nanos: u64 = spans.iter().map(|s| s.cost_usd_nanos).sum();
        let any_err = spans.iter().any(|s| s.has_error);
        let name = spans.first().map(|s| s.name.clone()).unwrap_or_default();
        let visible_keys: std::collections::HashSet<(u64, u64)> =
            spans.iter().map(|s| (tid, s.span_id)).collect();
        let log_events_by_span = self
            .coord
            .log_events_for_trace_keys(&snap, tid, &visible_keys);
        let order = span_order(&spans);
        let span_items: Vec<String> = spans
            .iter()
            .map(|s| {
                let log_events = log_events_by_span
                    .get(&s.span_id)
                    .map(|events| json_log_events(events))
                    .unwrap_or_else(|| "[]".to_string());
                let (span_ordinal, sibling_ordinal) =
                    order.get(&s.span_id).copied().unwrap_or((0, 0));
                format!(
                    r#"{{"id":"{}","parentId":{},"externalTraceId":{},"externalSpanId":{},"externalParentSpanId":{},"externalSessionId":{},"kind":"{}","name":"{}","spanOrdinal":{},"siblingOrdinal":{},"sortKey":"{:020}:{:020}","startMs":{},"durMs":{},"status":"{}","cost":{},"costUsd":{},"costDetail":{},"usage":{},"inTok":{},"outTok":{},"model":{},"provider":{},"depth":{},"fields":{},"attrs":{},"logEvents":{}}}"#,
                    s.span_id,
                    s.parent_span_id.map_or("null".to_string(), |p| format!("\"{p}\"")),
                    json_opt_str(s.external_trace_id.as_deref()),
                    json_opt_str(s.external_span_id.as_deref()),
                    json_opt_str(s.external_parent_span_id.as_deref()),
                    json_opt_str(s.external_session_id.as_deref()),
                    s.kind,
                    json_escape(&s.name),
                    span_ordinal,
                    sibling_ordinal,
                    span_ordinal,
                    s.span_id,
                    s.start_ns / 1_000_000,
                    s.duration_ns / 1_000_000,
                    if s.has_error { "error" } else { "ok" },
                    cost_num(s.input_tokens, s.output_tokens),
                    cost_usd_num_from_nanos(s.cost_usd_nanos),
                    cost_detail_json(
                        s.cost_usd_nanos,
                        s.cost_currency.as_deref(),
                        "mixed"
                    ),
                    console_usage_json(s),
                    s.input_tokens,
                    s.output_tokens,
                    s.model.as_ref().map_or("null".to_string(), |m| format!("\"{}\"", json_escape(m))),
                    json_opt_str(s.provider.as_deref()),
                    depth_of(s.span_id),
                    json_console_agent_fields(s),
                    json_attrs(&s.attrs),
                    log_events,
                )
            })
            .collect();
        let summary = format!(
            r#"{{"traceId":"{}","externalTraceId":{},"name":"{}","durMs":{},"cost":{},"costUsd":{},"costDetail":{},"usage":{},"spanCount":{},"status":"{}"}}"#,
            tid,
            json_opt_str(spans.iter().find_map(|s| s.external_trace_id.as_deref())),
            json_escape(&name),
            total_dur_ms,
            cost_num(in_tok, out_tok),
            cost_usd_num_from_nanos(cost_usd_nanos),
            cost_detail_json(cost_usd_nanos, Some("USD"), "mixed"),
            usage_json(in_tok, out_tok, cached_tok, reasoning_tok, total_tokens),
            spans.len(),
            if any_err { "error" } else { "ok" },
        );
        (
            200,
            format!(
                r#"{{"summary":{},"spans":[{}]}}"#,
                summary,
                span_items.join(",")
            ),
        )
    }

    /// GET /v1/traces/:id/steps：步骤流视图 —— 每个 span 连同输入/输出大文本一次给全。
    /// 与瀑布的晚物化相反：步骤流的本意就是看每一步的输入→输出，故在此端点物化。
    fn steps_json(&self, id: &str, tenant: Option<u64>) -> (u16, String) {
        let Some(tid) = parse_id_or_hash(id) else {
            return (400, r#"{"error":"bad trace id"}"#.to_string());
        };
        let snap = self.coord.pin_snapshot();
        let spans = self
            .coord
            .console_trace_spans_for_tenant(&snap, tid, tenant);
        if spans.is_empty() {
            return (404, r#"{"error":"trace not found"}"#.to_string());
        }
        let items: Vec<String> = spans
            .iter()
            .map(|s| {
                format!(
                    r#"{{"id":"{}","externalTraceId":{},"externalSpanId":{},"kind":"{}","name":"{}","status":"{}","durMs":{},"cost":{},"costUsd":{},"costDetail":{},"usage":{},"inTok":{},"outTok":{},"model":{},"provider":{},"input":{},"output":{},"fields":{},"attrs":{}}}"#,
                    s.span_id,
                    json_opt_str(s.external_trace_id.as_deref()),
                    json_opt_str(s.external_span_id.as_deref()),
                    s.kind,
                    json_escape(&s.name),
                    if s.has_error { "error" } else { "ok" },
                    s.duration_ns / 1_000_000,
                    cost_num(s.input_tokens, s.output_tokens),
                    cost_usd_num_from_nanos(s.cost_usd_nanos),
                    cost_detail_json(s.cost_usd_nanos, s.cost_currency.as_deref(), "mixed"),
                    console_usage_json(s),
                    s.input_tokens,
                    s.output_tokens,
                    s.model.as_ref().map_or("null".to_string(), |m| format!("\"{}\"", json_escape(m))),
                    json_opt_str(s.provider.as_deref()),
                    s.input_text.as_ref().map_or("null".to_string(), |t| format!("\"{}\"", json_escape(t))),
                    s.output_text.as_ref().map_or("null".to_string(), |t| format!("\"{}\"", json_escape(t))),
                    json_console_agent_fields(s),
                    json_attrs(&s.attrs),
                )
            })
            .collect();
        (200, format!("[{}]", items.join(",")))
    }

    /// GET /v1/traces/:id/spans/:spanId：单个 span 的大字段（晚物化）。
    fn span_detail_json(&self, id: &str, span_id: &str, tenant: Option<u64>) -> (u16, String) {
        let (Some(tid), Some(sid)) = (parse_id_or_hash(id), parse_id_or_hash(span_id)) else {
            return (400, r#"{"error":"bad id"}"#.to_string());
        };
        let snap = self.coord.pin_snapshot();
        let spans = self
            .coord
            .console_trace_spans_for_tenant(&snap, tid, tenant);
        match spans.into_iter().find(|s| s.span_id == sid) {
            Some(s) => {
                let mut keys = std::collections::HashSet::new();
                keys.insert((tid, sid));
                let log_events_by_span = self.coord.log_events_for_trace_keys(&snap, tid, &keys);
                let log_events = log_events_by_span
                    .get(&sid)
                    .map(|events| json_log_events(events))
                    .unwrap_or_else(|| "[]".to_string());
                (
                    200,
                    format!(
                        r#"{{"id":"{}","externalTraceId":{},"externalSpanId":{},"externalParentSpanId":{},"externalSessionId":{},"input":{},"output":{},"fields":{},"attrs":{},"logEvents":{}}}"#,
                        sid,
                        json_opt_str(s.external_trace_id.as_deref()),
                        json_opt_str(s.external_span_id.as_deref()),
                        json_opt_str(s.external_parent_span_id.as_deref()),
                        json_opt_str(s.external_session_id.as_deref()),
                        s.input_text
                            .as_ref()
                            .map_or("null".to_string(), |t| format!("\"{}\"", json_escape(t))),
                        s.output_text
                            .as_ref()
                            .map_or("null".to_string(), |t| format!("\"{}\"", json_escape(t))),
                        json_console_agent_fields(&s),
                        json_attrs(&s.attrs),
                        log_events,
                    ),
                )
            }
            None => (404, r#"{"error":"span not found"}"#.to_string()),
        }
    }

    /// GET /v1/traces/:id/snapshot：导出一条 trace 的稳定 JSON 快照，供 eval draft / 回归样本使用。
    fn trace_snapshot_json(&self, id: &str, tenant: Option<u64>) -> (u16, String) {
        let Some(tid) = parse_id_or_hash(id) else {
            return (400, r#"{"error":"bad trace id"}"#.to_string());
        };
        let snap = self.coord.pin_snapshot();
        let spans = self
            .coord
            .console_trace_spans_for_tenant(&snap, tid, tenant);
        if spans.is_empty() {
            return (404, r#"{"error":"trace not found"}"#.to_string());
        }
        let visible_keys: std::collections::HashSet<(u64, u64)> =
            spans.iter().map(|s| (tid, s.span_id)).collect();
        let log_events_by_span = self
            .coord
            .log_events_for_trace_keys(&snap, tid, &visible_keys);
        let order = span_order(&spans);
        let span_items: Vec<String> = spans
            .iter()
            .map(|s| {
                let events = log_events_by_span
                    .get(&s.span_id)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                let (span_ordinal, sibling_ordinal) =
                    order.get(&s.span_id).copied().unwrap_or((0, 0));
                json_console_span_export(tid, s, span_ordinal, sibling_ordinal, events, true)
            })
            .collect();
        let summary = trace_summary_json(tid, &spans);
        let payload = format!(
            r#"{{"summary":{},"spans":[{}]}}"#,
            summary,
            span_items.join(",")
        );
        let hash = yt_core::event::fnv1a64(payload.as_bytes());
        (
            200,
            format!(
                r#"{{"snapshotId":"trace-{}-{:016x}","snapshotHash":"fnv1a64:{:016x}","createdAt":{},"trace":{}}}"#,
                tid,
                hash,
                hash,
                unix_now_ns(),
                payload
            ),
        )
    }

    /// GET /v1/traces/:id/spans?cursor=&limit=&includeFull=：分页批量取 span 详情。
    fn spans_page_json(&self, id: &str, query: &str, tenant: Option<u64>) -> (u16, String) {
        let Some(tid) = parse_id_or_hash(id) else {
            return (400, r#"{"error":"bad trace id"}"#.to_string());
        };
        let (cursor, limit, include_full) = span_page_query(query);
        let snap = self.coord.pin_snapshot();
        let spans = self
            .coord
            .console_trace_spans_for_tenant(&snap, tid, tenant);
        if spans.is_empty() {
            return (404, r#"{"error":"trace not found"}"#.to_string());
        }
        let total = spans.len();
        let end = (cursor + limit).min(total);
        let page = if cursor < total {
            &spans[cursor..end]
        } else {
            &[][..]
        };
        let keys: std::collections::HashSet<(u64, u64)> =
            page.iter().map(|s| (tid, s.span_id)).collect();
        let log_events_by_span = self.coord.log_events_for_trace_keys(&snap, tid, &keys);
        let order = span_order(&spans);
        let items: Vec<String> = page
            .iter()
            .map(|s| {
                let events = log_events_by_span
                    .get(&s.span_id)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                let (span_ordinal, sibling_ordinal) =
                    order.get(&s.span_id).copied().unwrap_or((0, 0));
                json_console_span_export(
                    tid,
                    s,
                    span_ordinal,
                    sibling_ordinal,
                    events,
                    include_full,
                )
            })
            .collect();
        let next = if end < total {
            end.to_string()
        } else {
            "null".to_string()
        };
        (
            200,
            format!(
                r#"{{"items":[{}],"nextCursor":{},"total":{}}}"#,
                items.join(","),
                next,
                total
            ),
        )
    }

    /// POST /v1/traces/:id/spans/batch：按 span id 批量取详情，避免业务侧 N 次晚物化。
    fn spans_batch_json(&self, id: &str, body: &str, tenant: Option<u64>) -> (u16, String) {
        use crate::wire::parse;
        let Some(tid) = parse_id_or_hash(id) else {
            return (400, r#"{"error":"bad trace id"}"#.to_string());
        };
        let v = match parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let include_full = json_field_alias(&v, &["include_full", "includeFull", "full"])
            .map(json_truthy)
            .unwrap_or(false);
        let wanted: std::collections::HashSet<u64> = json_field_alias(&v, &["span_ids", "spanIds"])
            .map(|arr| arr.as_array().iter().filter_map(json_id_or_hash).collect())
            .unwrap_or_default();
        if wanted.is_empty() {
            return (400, r#"{"error":"spanIds required"}"#.to_string());
        }
        let snap = self.coord.pin_snapshot();
        let spans = self
            .coord
            .console_trace_spans_for_tenant(&snap, tid, tenant);
        if spans.is_empty() {
            return (404, r#"{"error":"trace not found"}"#.to_string());
        }
        let selected: Vec<_> = spans
            .iter()
            .filter(|s| wanted.contains(&s.span_id))
            .collect();
        let keys: std::collections::HashSet<(u64, u64)> =
            selected.iter().map(|s| (tid, s.span_id)).collect();
        let log_events_by_span = self.coord.log_events_for_trace_keys(&snap, tid, &keys);
        let order = span_order(&spans);
        let items: Vec<String> = selected
            .iter()
            .map(|s| {
                let events = log_events_by_span
                    .get(&s.span_id)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                let (span_ordinal, sibling_ordinal) =
                    order.get(&s.span_id).copied().unwrap_or((0, 0));
                json_console_span_export(
                    tid,
                    s,
                    span_ordinal,
                    sibling_ordinal,
                    events,
                    include_full,
                )
            })
            .collect();
        (200, format!(r#"{{"items":[{}]}}"#, items.join(",")))
    }
}

struct TraceSearchSpec {
    session_id: Option<u64>,
    span_id: Option<u64>,
    external_trace_id: Option<String>,
    external_span_id: Option<String>,
    external_session_id: Option<String>,
    status: Option<u8>,
    kind: Option<String>,
    agent_name: Option<String>,
    tool_name: Option<String>,
    model: Option<String>,
    text: Option<String>,
    input_contains: Option<String>,
    output_contains: Option<String>,
    log_contains: Option<String>,
    attrs: std::collections::BTreeMap<String, String>,
}

#[derive(Default)]
struct TraceSearchAnnotationSpec {
    active: bool,
    target: Option<AnnotationTarget>,
    label: Option<String>,
    source: Option<String>,
    score_min: Option<u32>,
    score_max: Option<u32>,
    attrs: std::collections::BTreeMap<String, String>,
}

#[derive(Default)]
struct TraceSearchDatasetSpec {
    active: bool,
    dataset_id: Option<String>,
    item_id: Option<String>,
    eval_run_id: Option<String>,
    split: Option<String>,
    label: Option<String>,
    score_min: Option<u32>,
    score_max: Option<u32>,
    attrs: std::collections::BTreeMap<String, String>,
}

#[derive(Default)]
struct TraceSearchMetadataMatches {
    need_annotation: bool,
    annotation_candidate_traces: std::collections::HashSet<u64>,
    annotation_traces: std::collections::HashSet<u64>,
    annotation_spans: std::collections::HashSet<(u64, u64)>,
    need_dataset: bool,
    dataset_candidate_traces: std::collections::HashSet<u64>,
    dataset_traces: std::collections::HashSet<u64>,
    dataset_spans: std::collections::HashSet<(u64, u64)>,
}

struct TraceSearchRequest {
    query: TraceQuery,
    spec: TraceSearchSpec,
    annotation: TraceSearchAnnotationSpec,
    dataset: TraceSearchDatasetSpec,
}

#[derive(Clone)]
struct TraceAggregateGroupField {
    output_key: String,
    kind: TraceAggregateGroupKind,
}

#[derive(Clone)]
enum TraceAggregateGroupKind {
    Attr(String),
    AgentName,
    ToolName,
    Model,
    Provider,
    Kind,
    Status,
}

#[derive(Clone)]
struct TraceAggregateExample {
    trace_id: u64,
    span_id: u64,
    external_trace_id: Option<String>,
    external_span_id: Option<String>,
    name: String,
}

struct StorageMetadata {
    annotations: Vec<crate::TraceAnnotation>,
    dataset_associations: Vec<crate::DatasetAssociation>,
    golden_paths: Vec<crate::GoldenPathCandidate>,
}

#[derive(Clone, Default)]
struct StorageStatsBucket {
    key: std::collections::BTreeMap<String, String>,
    trace_ids: std::collections::BTreeSet<u64>,
    session_ids: std::collections::BTreeSet<u64>,
    span_count: usize,
    event_count: usize,
    error_span_count: usize,
    first_ts: Option<i64>,
    last_ts: Option<i64>,
    input_text_bytes: u64,
    output_text_bytes: u64,
    log_bytes: u64,
    attr_bytes: u64,
    external_id_bytes: u64,
    field_bytes: u64,
    estimated_bytes: u64,
    annotation_count: usize,
    dataset_association_count: usize,
    golden_path_count: usize,
    snapshot_ref_count: usize,
    eval_link_count: usize,
    path_memory_ref_count: usize,
}

struct StorageStatsReport {
    total: StorageStatsBucket,
    groups: Vec<StorageStatsBucket>,
}

struct TraceAggregateBucket {
    values: Vec<String>,
    span_count: usize,
    trace_ids: std::collections::HashSet<u64>,
    error_count: usize,
    duration_sum_ns: u128,
    duration_max_ns: u64,
    durations_ns: Vec<u64>,
    input_tokens: u64,
    output_tokens: u64,
    cached_input_tokens: u64,
    reasoning_tokens: u64,
    total_tokens: u64,
    cost_usd_nanos: u64,
    examples: Vec<TraceAggregateExample>,
}

#[derive(Clone, Default)]
struct ScoreStats {
    count: usize,
    sum: u64,
    min: u32,
    max: u32,
}

impl ScoreStats {
    fn add(&mut self, score: u32) {
        if self.count == 0 {
            self.min = score;
            self.max = score;
        } else {
            self.min = self.min.min(score);
            self.max = self.max.max(score);
        }
        self.count += 1;
        self.sum += score as u64;
    }

    fn avg(&self) -> u32 {
        if self.count == 0 {
            0
        } else {
            (self.sum / self.count as u64) as u32
        }
    }
}

#[derive(Clone)]
struct TrajectoryTraceExample {
    trace_id: u64,
    external_trace_id: Option<String>,
    status: String,
    duration_sum_ns: u128,
    duration_max_ns: u64,
    input_tokens: u64,
    output_tokens: u64,
    cached_input_tokens: u64,
    reasoning_tokens: u64,
    total_tokens: u64,
    cost_usd_nanos: u64,
    score: u32,
    fields: std::collections::BTreeMap<String, String>,
}

struct TrajectoryGroupBucket {
    signature: u64,
    steps: Vec<String>,
    trace_ids: std::collections::BTreeSet<u64>,
    span_count: usize,
    error_trace_count: usize,
    error_span_count: usize,
    duration_sum_ns: u128,
    duration_max_ns: u64,
    durations_ns: Vec<u64>,
    input_tokens: u64,
    output_tokens: u64,
    cached_input_tokens: u64,
    reasoning_tokens: u64,
    total_tokens: u64,
    cost_usd_nanos: u64,
    eval_scores: ScoreStats,
    annotation_scores: ScoreStats,
    dataset_scores: ScoreStats,
    examples: Vec<TrajectoryTraceExample>,
}

impl TrajectoryGroupBucket {
    fn new(signature: u64, steps: Vec<String>) -> Self {
        Self {
            signature,
            steps,
            trace_ids: std::collections::BTreeSet::new(),
            span_count: 0,
            error_trace_count: 0,
            error_span_count: 0,
            duration_sum_ns: 0,
            duration_max_ns: 0,
            durations_ns: Vec::new(),
            input_tokens: 0,
            output_tokens: 0,
            cached_input_tokens: 0,
            reasoning_tokens: 0,
            total_tokens: 0,
            cost_usd_nanos: 0,
            eval_scores: ScoreStats::default(),
            annotation_scores: ScoreStats::default(),
            dataset_scores: ScoreStats::default(),
            examples: Vec::new(),
        }
    }

    fn add_trace(
        &mut self,
        spans: &[FoldedSpan],
        summary: Option<&TaskTraceSummaryBucket>,
        annotation_scores: Option<&[u32]>,
        dataset_scores: Option<&[u32]>,
        example_limit: usize,
    ) {
        let Some(summary) = summary else {
            return;
        };
        self.trace_ids.insert(summary.trace_id);
        self.span_count += summary.span_count;
        self.error_span_count += summary.error_count;
        if summary.error_count > 0 {
            self.error_trace_count += 1;
        }
        let trace_duration = summary.duration_sum_ns.min(u64::MAX as u128) as u64;
        self.duration_sum_ns += summary.duration_sum_ns;
        self.duration_max_ns = self.duration_max_ns.max(summary.duration_max_ns);
        self.durations_ns.push(trace_duration);
        self.input_tokens += summary.input_tokens;
        self.output_tokens += summary.output_tokens;
        self.cached_input_tokens += summary.cached_input_tokens;
        self.reasoning_tokens += summary.reasoning_tokens;
        self.total_tokens += summary.total_tokens;
        self.cost_usd_nanos += summary.cost_usd_nanos;
        for score in spans.iter().filter_map(|s| s.eval_score) {
            self.eval_scores.add(score);
        }
        if let Some(scores) = annotation_scores {
            for score in scores {
                self.annotation_scores.add(*score);
            }
        }
        if let Some(scores) = dataset_scores {
            for score in scores {
                self.dataset_scores.add(*score);
            }
        }
        if self.examples.len() < example_limit {
            self.examples.push(TrajectoryTraceExample {
                trace_id: summary.trace_id,
                external_trace_id: summary.external_trace_id.clone(),
                status: if summary.error_count > 0 {
                    "error".to_string()
                } else {
                    "ok".to_string()
                },
                duration_sum_ns: summary.duration_sum_ns,
                duration_max_ns: summary.duration_max_ns,
                input_tokens: summary.input_tokens,
                output_tokens: summary.output_tokens,
                cached_input_tokens: summary.cached_input_tokens,
                reasoning_tokens: summary.reasoning_tokens,
                total_tokens: summary.total_tokens,
                cost_usd_nanos: summary.cost_usd_nanos,
                score: trajectory_trace_quality_score(
                    summary.error_count == 0,
                    spans,
                    annotation_scores,
                    dataset_scores,
                ),
                fields: summary.fields.clone(),
            });
        }
    }

    fn trace_count(&self) -> usize {
        self.trace_ids.len()
    }

    fn success_count(&self) -> usize {
        self.trace_count().saturating_sub(self.error_trace_count)
    }

    fn avg_duration_ns(&self) -> u128 {
        if self.durations_ns.is_empty() {
            0
        } else {
            self.duration_sum_ns / self.durations_ns.len() as u128
        }
    }

    fn avg_cost_usd_nanos(&self) -> u64 {
        if self.trace_count() == 0 {
            0
        } else {
            self.cost_usd_nanos / self.trace_count() as u64
        }
    }

    fn quality_score(&self) -> u32 {
        let mut sum = self.success_score() as u64;
        let mut count = 1u64;
        for stats in [
            &self.eval_scores,
            &self.annotation_scores,
            &self.dataset_scores,
        ] {
            if stats.count > 0 {
                sum += stats.avg() as u64;
                count += 1;
            }
        }
        (sum / count) as u32
    }

    fn success_score(&self) -> u32 {
        if self.trace_count() == 0 {
            0
        } else {
            ((self.success_count() as u128 * 1000) / self.trace_count() as u128) as u32
        }
    }
}

struct ProductQueryParts {
    cursor: usize,
    limit: usize,
    filter: String,
    attrs: std::collections::BTreeMap<String, String>,
    annotation: TraceSearchAnnotationSpec,
    dataset: TraceSearchDatasetSpec,
}

struct PathAdherenceFacts {
    source_available: bool,
    source_retained: bool,
    source_steps: Vec<String>,
    source_signature: Option<String>,
    trace_steps: Vec<String>,
    trace_signature: String,
    same_signature: bool,
    stored_signature_matches_source: Option<bool>,
    common_steps: Vec<String>,
    missing_steps: Vec<String>,
    extra_steps: Vec<String>,
}

impl PathAdherenceFacts {
    fn adherence(&self) -> &'static str {
        if self.same_signature {
            "followed"
        } else if self.source_steps.is_empty() {
            "unknown"
        } else if self.common_steps.is_empty() {
            "deviated"
        } else if self.missing_steps.is_empty() {
            "extended"
        } else {
            "partial"
        }
    }
}

struct LoopSummaryBucket {
    loop_id: String,
    loop_value_json: String,
    trace_ids: std::collections::HashSet<u64>,
    session_ids: std::collections::HashSet<u64>,
    span_count: usize,
    error_count: usize,
    duration_sum_ns: u128,
    duration_max_ns: u64,
    durations_ns: Vec<u64>,
    input_tokens: u64,
    output_tokens: u64,
    cached_input_tokens: u64,
    reasoning_tokens: u64,
    total_tokens: u64,
    cost_usd_nanos: u64,
    first_trace_id: u64,
    last_trace_id: u64,
    fields: std::collections::BTreeMap<String, String>,
    phases: std::collections::BTreeSet<String>,
    validators: std::collections::BTreeSet<String>,
    examples: Vec<TraceAggregateExample>,
}

impl LoopSummaryBucket {
    fn new(loop_value_json: String) -> Self {
        Self {
            loop_id: json_compact_label(&loop_value_json),
            loop_value_json,
            trace_ids: std::collections::HashSet::new(),
            session_ids: std::collections::HashSet::new(),
            span_count: 0,
            error_count: 0,
            duration_sum_ns: 0,
            duration_max_ns: 0,
            durations_ns: Vec::new(),
            input_tokens: 0,
            output_tokens: 0,
            cached_input_tokens: 0,
            reasoning_tokens: 0,
            total_tokens: 0,
            cost_usd_nanos: 0,
            first_trace_id: u64::MAX,
            last_trace_id: 0,
            fields: std::collections::BTreeMap::new(),
            phases: std::collections::BTreeSet::new(),
            validators: std::collections::BTreeSet::new(),
            examples: Vec::new(),
        }
    }
}

struct TaskTraceSummaryBucket {
    trace_id: u64,
    external_trace_id: Option<String>,
    span_count: usize,
    error_count: usize,
    duration_sum_ns: u128,
    duration_max_ns: u64,
    input_tokens: u64,
    output_tokens: u64,
    cached_input_tokens: u64,
    reasoning_tokens: u64,
    total_tokens: u64,
    cost_usd_nanos: u64,
    fields: std::collections::BTreeMap<String, String>,
}

impl TaskTraceSummaryBucket {
    fn new(s: &FoldedSpan) -> Self {
        Self {
            trace_id: s.trace_id,
            external_trace_id: s.external_trace_id.clone(),
            span_count: 0,
            error_count: 0,
            duration_sum_ns: 0,
            duration_max_ns: 0,
            input_tokens: 0,
            output_tokens: 0,
            cached_input_tokens: 0,
            reasoning_tokens: 0,
            total_tokens: 0,
            cost_usd_nanos: 0,
            fields: std::collections::BTreeMap::new(),
        }
    }
}

fn json_field_alias<'a>(
    obj: &'a crate::wire::Json,
    names: &[&str],
) -> Option<&'a crate::wire::Json> {
    names.iter().find_map(|name| crate::wire::field(obj, name))
}

fn json_bool_alias(obj: &crate::wire::Json, names: &[&str]) -> Option<bool> {
    json_field_alias(obj, names).and_then(|value| match value {
        crate::wire::Json::Bool(v) => Some(*v),
        crate::wire::Json::Num(s) | crate::wire::Json::Str(s) => {
            if s.eq_ignore_ascii_case("true") || s == "1" {
                Some(true)
            } else if s.eq_ignore_ascii_case("false") || s == "0" {
                Some(false)
            } else {
                None
            }
        }
        _ => None,
    })
}

fn trace_search_request_from_json(
    v: &crate::wire::Json,
    tenant: Option<u64>,
) -> TraceSearchRequest {
    use crate::wire::{field, Json};
    let f = field(v, "filter").unwrap_or(v);
    let mut q = TraceQuery::all();
    q.tenant_id = tenant;
    q.trace_id = json_field_alias(f, &["trace_id", "traceId"]).and_then(json_id_or_hash);
    if let Some(from) =
        json_field_alias(f, &["time_from", "timeFrom", "created_from", "createdFrom"])
            .and_then(Json::as_i64)
    {
        q.time_from = from;
    }
    if let Some(to) = json_field_alias(f, &["time_to", "timeTo", "created_to", "createdTo"])
        .and_then(Json::as_i64)
    {
        q.time_to = to;
    }

    let mut attrs = std::collections::BTreeMap::new();
    collect_attr_map(f, &mut attrs);
    let spec = TraceSearchSpec {
        session_id: json_field_alias(f, &["session_id", "sessionId"]).and_then(json_id_or_hash),
        span_id: json_field_alias(f, &["span_id", "spanId"]).and_then(json_id_or_hash),
        external_trace_id: json_field_alias(f, &["external_trace_id", "externalTraceId"])
            .and_then(Json::as_str)
            .map(|s| s.to_string()),
        external_span_id: json_field_alias(f, &["external_span_id", "externalSpanId"])
            .and_then(Json::as_str)
            .map(|s| s.to_string()),
        external_session_id: json_field_alias(f, &["external_session_id", "externalSessionId"])
            .and_then(Json::as_str)
            .map(|s| s.to_string()),
        status: field(f, "status").and_then(Json::as_u64).map(|x| x as u8),
        kind: json_field_alias(f, &["span_kind", "spanKind", "kind"])
            .and_then(Json::as_str)
            .map(|s| s.to_string()),
        agent_name: json_field_alias(f, &["agent_name", "agentName"])
            .and_then(Json::as_str)
            .map(|s| s.to_string()),
        tool_name: json_field_alias(f, &["tool_name", "toolName"])
            .and_then(Json::as_str)
            .map(|s| s.to_string()),
        model: field(f, "model")
            .and_then(Json::as_str)
            .map(|s| s.to_string()),
        text: json_field_alias(v, &["text", "q"])
            .or_else(|| json_field_alias(f, &["text", "q"]))
            .and_then(Json::as_str)
            .map(|s| s.to_string()),
        input_contains: json_field_alias(f, &["input_text", "inputText", "inputContains"])
            .and_then(Json::as_str)
            .map(|s| s.to_string()),
        output_contains: json_field_alias(f, &["output_text", "outputText", "outputContains"])
            .and_then(Json::as_str)
            .map(|s| s.to_string()),
        log_contains: json_field_alias(f, &["log_text", "logText", "logContains"])
            .and_then(Json::as_str)
            .map(|s| s.to_string()),
        attrs,
    };
    TraceSearchRequest {
        query: q,
        spec,
        annotation: trace_search_annotation_spec(f),
        dataset: trace_search_dataset_spec(f),
    }
}

fn trace_search_match(
    s: &FoldedSpan,
    spec: &TraceSearchSpec,
    metadata: &TraceSearchMetadataMatches,
) -> bool {
    if let Some(session_id) = spec.session_id {
        if s.session_id != Some(session_id) {
            return false;
        }
    }
    if let Some(span_id) = spec.span_id {
        if s.span_id != span_id {
            return false;
        }
    }
    if let Some(expected) = &spec.external_trace_id {
        if s.external_trace_id.as_deref() != Some(expected.as_str()) {
            return false;
        }
    }
    if let Some(expected) = &spec.external_span_id {
        if s.external_span_id.as_deref() != Some(expected.as_str()) {
            return false;
        }
    }
    if let Some(expected) = &spec.external_session_id {
        if s.external_session_id.as_deref() != Some(expected.as_str()) {
            return false;
        }
    }
    if let Some(status) = spec.status {
        if s.status != Some(status) {
            return false;
        }
    }
    if let Some(kind) = &spec.kind {
        if folded_kind(s) != kind {
            return false;
        }
    }
    if let Some(agent) = &spec.agent_name {
        if s.agent_name.as_deref() != Some(agent.as_str()) {
            return false;
        }
    }
    if let Some(tool) = &spec.tool_name {
        if s.tool_name.as_deref() != Some(tool.as_str()) {
            return false;
        }
    }
    if let Some(model) = &spec.model {
        if s.model.as_deref() != Some(model.as_str()) {
            return false;
        }
    }
    for (key, expected) in &spec.attrs {
        if !crate::folded_span_attr_value(s, key)
            .map(|actual| crate::attr_json_matches(actual, expected))
            .unwrap_or(false)
        {
            return false;
        }
    }
    if let Some(text) = &spec.text {
        if !folded_contains(s, text) {
            return false;
        }
    }
    if let Some(text) = &spec.input_contains {
        if !s
            .input_text
            .as_deref()
            .map(|v| v.contains(text))
            .unwrap_or(false)
        {
            return false;
        }
    }
    if let Some(text) = &spec.output_contains {
        if !s
            .output_text
            .as_deref()
            .map(|v| v.contains(text))
            .unwrap_or(false)
        {
            return false;
        }
    }
    if let Some(text) = &spec.log_contains {
        if !s.logs.iter().any(|log| log.contains(text)) {
            return false;
        }
    }
    trace_search_metadata_match(s, metadata)
}

fn trace_search_metadata_match(s: &FoldedSpan, metadata: &TraceSearchMetadataMatches) -> bool {
    if metadata.need_annotation
        && !metadata.annotation_traces.contains(&s.trace_id)
        && !metadata.annotation_spans.contains(&(s.trace_id, s.span_id))
    {
        return false;
    }
    if metadata.need_dataset
        && !metadata.dataset_traces.contains(&s.trace_id)
        && !metadata.dataset_spans.contains(&(s.trace_id, s.span_id))
    {
        return false;
    }
    true
}

fn trace_id_metadata_match(trace_id: u64, metadata: &TraceSearchMetadataMatches) -> bool {
    if metadata.need_annotation && !metadata.annotation_candidate_traces.contains(&trace_id) {
        return false;
    }
    if metadata.need_dataset && !metadata.dataset_candidate_traces.contains(&trace_id) {
        return false;
    }
    true
}

fn metadata_candidate_trace_ids(
    metadata: &TraceSearchMetadataMatches,
) -> std::collections::HashSet<u64> {
    let mut out = std::collections::HashSet::new();
    if metadata.need_annotation {
        out.extend(metadata.annotation_candidate_traces.iter().copied());
    }
    if metadata.need_dataset {
        if out.is_empty() {
            out.extend(metadata.dataset_candidate_traces.iter().copied());
        } else {
            out.retain(|trace_id| metadata.dataset_candidate_traces.contains(trace_id));
        }
    }
    out
}

fn trace_search_annotation_spec(f: &crate::wire::Json) -> TraceSearchAnnotationSpec {
    let nested = json_field_alias(f, &["annotation", "annotations", "annotationFilter"]);
    let obj = nested.unwrap_or(f);
    let mut attrs = std::collections::BTreeMap::new();
    if nested.is_some() {
        collect_attr_map(obj, &mut attrs);
    }
    let target = json_field_alias(obj, &["target", "target_type", "targetType"])
        .or_else(|| json_field_alias(f, &["annotation_target", "annotationTarget"]))
        .and_then(crate::wire::Json::as_str)
        .and_then(AnnotationTarget::parse);
    let label = json_field_alias(obj, &["label"])
        .or_else(|| json_field_alias(f, &["annotation_label", "annotationLabel"]))
        .and_then(crate::wire::Json::as_str)
        .map(ToString::to_string);
    let source = json_field_alias(obj, &["source"])
        .or_else(|| json_field_alias(f, &["annotation_source", "annotationSource"]))
        .and_then(crate::wire::Json::as_str)
        .map(ToString::to_string);
    let score_min = json_field_alias(obj, &["score_min", "scoreMin", "minScore"])
        .or_else(|| json_field_alias(f, &["annotation_score_min", "annotationScoreMin"]))
        .and_then(crate::wire::Json::as_u64)
        .map(score_u64);
    let score_max = json_field_alias(obj, &["score_max", "scoreMax", "maxScore"])
        .or_else(|| json_field_alias(f, &["annotation_score_max", "annotationScoreMax"]))
        .and_then(crate::wire::Json::as_u64)
        .map(score_u64);
    let active = nested.is_some()
        || target.is_some()
        || label.is_some()
        || source.is_some()
        || score_min.is_some()
        || score_max.is_some()
        || !attrs.is_empty();
    TraceSearchAnnotationSpec {
        active,
        target,
        label,
        source,
        score_min,
        score_max,
        attrs,
    }
}

fn trace_search_dataset_spec(f: &crate::wire::Json) -> TraceSearchDatasetSpec {
    let nested = json_field_alias(
        f,
        &[
            "dataset",
            "datasetAssociation",
            "dataset_association",
            "datasetLink",
            "dataset_link",
        ],
    );
    let obj = nested.unwrap_or(f);
    let mut attrs = std::collections::BTreeMap::new();
    if nested.is_some() {
        collect_attr_map(obj, &mut attrs);
    }
    let dataset_id = json_field_alias(obj, &["dataset_id", "datasetId", "dataset"])
        .or_else(|| json_field_alias(f, &["dataset_id", "datasetId"]))
        .and_then(crate::wire::Json::as_str)
        .map(ToString::to_string);
    let item_id = json_field_alias(
        obj,
        &["item_id", "itemId", "dataset_item_id", "datasetItemId"],
    )
    .or_else(|| {
        json_field_alias(
            f,
            &["item_id", "itemId", "dataset_item_id", "datasetItemId"],
        )
    })
    .and_then(crate::wire::Json::as_str)
    .map(ToString::to_string);
    let eval_run_id = json_field_alias(obj, &["eval_run_id", "evalRunId"])
        .or_else(|| json_field_alias(f, &["eval_run_id", "evalRunId"]))
        .and_then(crate::wire::Json::as_str)
        .map(ToString::to_string);
    let split = json_field_alias(obj, &["split"])
        .or_else(|| json_field_alias(f, &["dataset_split", "datasetSplit"]))
        .and_then(crate::wire::Json::as_str)
        .map(ToString::to_string);
    let label = json_field_alias(obj, &["label"])
        .or_else(|| json_field_alias(f, &["dataset_label", "datasetLabel"]))
        .and_then(crate::wire::Json::as_str)
        .map(ToString::to_string);
    let score_min = json_field_alias(obj, &["score_min", "scoreMin", "minScore"])
        .or_else(|| json_field_alias(f, &["dataset_score_min", "datasetScoreMin"]))
        .and_then(crate::wire::Json::as_u64)
        .map(score_u64);
    let score_max = json_field_alias(obj, &["score_max", "scoreMax", "maxScore"])
        .or_else(|| json_field_alias(f, &["dataset_score_max", "datasetScoreMax"]))
        .and_then(crate::wire::Json::as_u64)
        .map(score_u64);
    let active = nested.is_some()
        || dataset_id.is_some()
        || item_id.is_some()
        || eval_run_id.is_some()
        || split.is_some()
        || label.is_some()
        || score_min.is_some()
        || score_max.is_some()
        || !attrs.is_empty();
    TraceSearchDatasetSpec {
        active,
        dataset_id,
        item_id,
        eval_run_id,
        split,
        label,
        score_min,
        score_max,
        attrs,
    }
}

fn trace_search_annotation_spec_from_query(
    pairs: &[(String, String)],
) -> TraceSearchAnnotationSpec {
    let mut spec = TraceSearchAnnotationSpec::default();
    for (k, v) in pairs {
        match k.as_str() {
            "annotation_target" | "annotationTarget" => {
                spec.target = AnnotationTarget::parse(v);
                spec.active = true;
            }
            "annotation_label" | "annotationLabel" => {
                spec.label = Some(v.clone());
                spec.active = true;
            }
            "annotation_source" | "annotationSource" => {
                spec.source = Some(v.clone());
                spec.active = true;
            }
            "annotation_score_min" | "annotationScoreMin" => {
                spec.score_min = v.parse::<u64>().ok().map(score_u64);
                spec.active = true;
            }
            "annotation_score_max" | "annotationScoreMax" => {
                spec.score_max = v.parse::<u64>().ok().map(score_u64);
                spec.active = true;
            }
            "annotation_attrs" | "annotationAttrs" => {
                collect_attr_query_json(v, &mut spec.attrs);
                spec.active = true;
            }
            _ => {}
        }
    }
    if !spec.attrs.is_empty() {
        spec.active = true;
    }
    spec
}

fn trace_search_dataset_spec_from_query(pairs: &[(String, String)]) -> TraceSearchDatasetSpec {
    let mut spec = TraceSearchDatasetSpec::default();
    for (k, v) in pairs {
        match k.as_str() {
            "dataset_id" | "datasetId" => {
                spec.dataset_id = Some(v.clone());
                spec.active = true;
            }
            "item_id" | "itemId" | "dataset_item_id" | "datasetItemId" => {
                spec.item_id = Some(v.clone());
                spec.active = true;
            }
            "eval_run_id" | "evalRunId" => {
                spec.eval_run_id = Some(v.clone());
                spec.active = true;
            }
            "dataset_split" | "datasetSplit" => {
                spec.split = Some(v.clone());
                spec.active = true;
            }
            "dataset_label" | "datasetLabel" => {
                spec.label = Some(v.clone());
                spec.active = true;
            }
            "dataset_score_min" | "datasetScoreMin" => {
                spec.score_min = v.parse::<u64>().ok().map(score_u64);
                spec.active = true;
            }
            "dataset_score_max" | "datasetScoreMax" => {
                spec.score_max = v.parse::<u64>().ok().map(score_u64);
                spec.active = true;
            }
            "dataset_attrs" | "datasetAttrs" => {
                collect_attr_query_json(v, &mut spec.attrs);
                spec.active = true;
            }
            _ => {}
        }
    }
    if !spec.attrs.is_empty() {
        spec.active = true;
    }
    spec
}

fn score_u64(n: u64) -> u32 {
    n.min(u32::MAX as u64) as u32
}

fn score_in_range(score: Option<u32>, min: Option<u32>, max: Option<u32>) -> bool {
    if min.is_none() && max.is_none() {
        return true;
    }
    let Some(score) = score else {
        return false;
    };
    if min.map(|m| score < m).unwrap_or(false) {
        return false;
    }
    if max.map(|m| score > m).unwrap_or(false) {
        return false;
    }
    true
}

fn folded_contains(s: &FoldedSpan, needle: &str) -> bool {
    needle.is_empty()
        || s.input_text
            .as_deref()
            .map(|v| v.contains(needle))
            .unwrap_or(false)
        || s.output_text
            .as_deref()
            .map(|v| v.contains(needle))
            .unwrap_or(false)
        || s.logs.iter().any(|log| log.contains(needle))
        || s.agent_name
            .as_deref()
            .map(|v| v.contains(needle))
            .unwrap_or(false)
        || s.tool_name
            .as_deref()
            .map(|v| v.contains(needle))
            .unwrap_or(false)
        || s.model
            .as_deref()
            .map(|v| v.contains(needle))
            .unwrap_or(false)
        || [
            "project_id",
            "skill",
            "mode",
            "call_site",
            "task_fingerprint",
            "loop_id",
            "harness_version",
            "validation_status",
            "stop_reason",
            "phase",
            "validator",
        ]
        .iter()
        .filter_map(|key| crate::first_class_span_attr_value(s, key))
        .any(|value| value.contains(needle))
        || s.attrs
            .iter()
            .any(|(k, v)| k.contains(needle) || v.contains(needle))
}

fn folded_kind(s: &FoldedSpan) -> &'static str {
    if s.agent_name.is_some() {
        "agent"
    } else if s.tool_name.is_some() {
        "tool"
    } else if s.model.is_some() {
        "llm"
    } else {
        "other"
    }
}

fn folded_name(s: &FoldedSpan) -> String {
    s.agent_name
        .as_ref()
        .or(s.tool_name.as_ref())
        .or(s.model.as_ref())
        .cloned()
        .unwrap_or_else(|| format!("span {}", s.span_id))
}

fn sort_trace_search_spans(spans: &mut [FoldedSpan], sort_by: &str, desc: bool) {
    let sort = sort_by.to_ascii_lowercase();
    spans.sort_by(|a, b| {
        let ord = match sort.as_str() {
            "duration" | "duration_ns" | "durationns" => {
                a.duration_ns.unwrap_or(0).cmp(&b.duration_ns.unwrap_or(0))
            }
            "cost" | "cost_usd" | "costusd" => cost_sort_key(a).cmp(&cost_sort_key(b)),
            "tokens" | "token_count" | "tokencount" => token_sort_key(a).cmp(&token_sort_key(b)),
            "status" => a.status.unwrap_or(0).cmp(&b.status.unwrap_or(0)),
            "span" | "span_id" | "spanid" => a.span_id.cmp(&b.span_id),
            _ => a
                .trace_id
                .cmp(&b.trace_id)
                .then_with(|| a.span_id.cmp(&b.span_id)),
        };
        let ord = if desc { ord.reverse() } else { ord };
        ord.then_with(|| a.trace_id.cmp(&b.trace_id))
            .then_with(|| a.span_id.cmp(&b.span_id))
    });
}

fn cost_sort_key(s: &FoldedSpan) -> u128 {
    folded_cost_usd_nanos(s) as u128
}

fn token_sort_key(s: &FoldedSpan) -> u64 {
    folded_total_tokens(s)
}

fn trace_aggregate_group_fields(
    v: &crate::wire::Json,
) -> Result<Vec<TraceAggregateGroupField>, String> {
    let Some(raw) = json_field_alias(v, &["group_by", "groupBy", "by"]) else {
        return Err("groupBy required".to_string());
    };
    let names: Vec<String> = match raw {
        crate::wire::Json::Str(s) => vec![s.clone()],
        crate::wire::Json::Arr(items) => items
            .iter()
            .filter_map(|item| item.as_str().map(ToString::to_string))
            .collect(),
        _ => Vec::new(),
    };
    let fields: Vec<TraceAggregateGroupField> = names
        .iter()
        .filter_map(|name| trace_aggregate_group_field(name))
        .collect();
    if fields.is_empty() {
        Err("groupBy must include at least one supported field".to_string())
    } else {
        Ok(fields)
    }
}

fn trace_aggregate_group_field(name: &str) -> Option<TraceAggregateGroupField> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    let no_sep = lower.replace(['_', '-', '.'], "");
    let (output_key, kind) = match no_sep.as_str() {
        "projectid" => (
            "project_id".to_string(),
            TraceAggregateGroupKind::Attr("project_id".to_string()),
        ),
        "skill" => (
            "skill".to_string(),
            TraceAggregateGroupKind::Attr("skill".to_string()),
        ),
        "mode" => (
            "mode".to_string(),
            TraceAggregateGroupKind::Attr("mode".to_string()),
        ),
        "callsite" => (
            "call_site".to_string(),
            TraceAggregateGroupKind::Attr("call_site".to_string()),
        ),
        "taskfingerprint" => (
            "task_fingerprint".to_string(),
            TraceAggregateGroupKind::Attr("task_fingerprint".to_string()),
        ),
        "loopid" => (
            "loop_id".to_string(),
            TraceAggregateGroupKind::Attr("loop_id".to_string()),
        ),
        "harnessversion" => (
            "harness_version".to_string(),
            TraceAggregateGroupKind::Attr("harness_version".to_string()),
        ),
        "validationstatus" => (
            "validation_status".to_string(),
            TraceAggregateGroupKind::Attr("validation_status".to_string()),
        ),
        "stopreason" => (
            "stop_reason".to_string(),
            TraceAggregateGroupKind::Attr("stop_reason".to_string()),
        ),
        "phase" => (
            "phase".to_string(),
            TraceAggregateGroupKind::Attr("phase".to_string()),
        ),
        "validator" => (
            "validator".to_string(),
            TraceAggregateGroupKind::Attr("validator".to_string()),
        ),
        "agentname" => ("agentName".to_string(), TraceAggregateGroupKind::AgentName),
        "toolname" => ("toolName".to_string(), TraceAggregateGroupKind::ToolName),
        "model" => ("model".to_string(), TraceAggregateGroupKind::Model),
        "provider" => ("provider".to_string(), TraceAggregateGroupKind::Provider),
        "kind" | "spankind" => ("kind".to_string(), TraceAggregateGroupKind::Kind),
        "status" => ("status".to_string(), TraceAggregateGroupKind::Status),
        _ => {
            let attr = trimmed
                .strip_prefix("attrs.")
                .or_else(|| trimmed.strip_prefix("attr."))
                .unwrap_or(trimmed)
                .to_string();
            (attr.clone(), TraceAggregateGroupKind::Attr(attr))
        }
    };
    Some(TraceAggregateGroupField { output_key, kind })
}

fn trace_aggregate_buckets(
    spans: &[FoldedSpan],
    fields: &[TraceAggregateGroupField],
) -> Vec<TraceAggregateBucket> {
    let mut by_key: std::collections::BTreeMap<Vec<String>, TraceAggregateBucket> =
        std::collections::BTreeMap::new();
    for s in spans {
        let values: Vec<String> = fields
            .iter()
            .map(|field| trace_aggregate_value_json(s, &field.kind))
            .collect();
        let bucket = by_key
            .entry(values.clone())
            .or_insert_with(|| TraceAggregateBucket {
                values,
                span_count: 0,
                trace_ids: std::collections::HashSet::new(),
                error_count: 0,
                duration_sum_ns: 0,
                duration_max_ns: 0,
                durations_ns: Vec::new(),
                input_tokens: 0,
                output_tokens: 0,
                cached_input_tokens: 0,
                reasoning_tokens: 0,
                total_tokens: 0,
                cost_usd_nanos: 0,
                examples: Vec::new(),
            });
        bucket.span_count += 1;
        bucket.trace_ids.insert(s.trace_id);
        if s.status.unwrap_or(0) != 0 {
            bucket.error_count += 1;
        }
        if let Some(duration) = s.duration_ns {
            bucket.duration_sum_ns += duration as u128;
            bucket.duration_max_ns = bucket.duration_max_ns.max(duration);
            bucket.durations_ns.push(duration);
        }
        bucket.input_tokens += s.input_tokens.unwrap_or(0);
        bucket.output_tokens += s.output_tokens.unwrap_or(0);
        bucket.cached_input_tokens += s.cached_input_tokens.unwrap_or(0);
        bucket.reasoning_tokens += s.reasoning_tokens.unwrap_or(0);
        bucket.total_tokens += folded_total_tokens(s);
        bucket.cost_usd_nanos += folded_cost_usd_nanos(s);
        if bucket.examples.len() < 3 {
            bucket.examples.push(TraceAggregateExample {
                trace_id: s.trace_id,
                span_id: s.span_id,
                external_trace_id: s.external_trace_id.clone(),
                external_span_id: s.external_span_id.clone(),
                name: folded_name(s),
            });
        }
    }
    by_key.into_values().collect()
}

fn trace_aggregate_value_json(s: &FoldedSpan, kind: &TraceAggregateGroupKind) -> String {
    match kind {
        TraceAggregateGroupKind::Attr(key) => crate::folded_span_attr_value(s, key)
            .map(ToString::to_string)
            .unwrap_or_else(|| "null".to_string()),
        TraceAggregateGroupKind::AgentName => s
            .agent_name
            .as_deref()
            .map(json_string_value)
            .unwrap_or_else(|| "null".to_string()),
        TraceAggregateGroupKind::ToolName => s
            .tool_name
            .as_deref()
            .map(json_string_value)
            .unwrap_or_else(|| "null".to_string()),
        TraceAggregateGroupKind::Model => s
            .model
            .as_deref()
            .map(json_string_value)
            .unwrap_or_else(|| "null".to_string()),
        TraceAggregateGroupKind::Provider => s
            .provider
            .as_deref()
            .map(json_string_value)
            .unwrap_or_else(|| "null".to_string()),
        TraceAggregateGroupKind::Kind => json_string_value(folded_kind(s)),
        TraceAggregateGroupKind::Status => s
            .status
            .map(|status| status.to_string())
            .unwrap_or_else(|| "null".to_string()),
    }
}

fn sort_trace_aggregate_buckets(buckets: &mut [TraceAggregateBucket], sort_by: &str, desc: bool) {
    let sort = sort_by.to_ascii_lowercase().replace(['_', '-'], "");
    buckets.sort_by(|a, b| {
        let ord = match sort.as_str() {
            "tracecount" | "traces" => a.trace_ids.len().cmp(&b.trace_ids.len()),
            "errorcount" | "errors" => a.error_count.cmp(&b.error_count),
            "errorrate" => (a.error_count as u128 * b.span_count as u128)
                .cmp(&(b.error_count as u128 * a.span_count as u128)),
            "duration" | "durationns" | "durationsum" => a.duration_sum_ns.cmp(&b.duration_sum_ns),
            "avgduration" | "durationavg" => {
                aggregate_avg_duration_ns(a).cmp(&aggregate_avg_duration_ns(b))
            }
            "maxduration" | "durationmax" => a.duration_max_ns.cmp(&b.duration_max_ns),
            "cost" | "costusd" => a.cost_usd_nanos.cmp(&b.cost_usd_nanos),
            "tokens" | "totaltokens" => a.total_tokens.cmp(&b.total_tokens),
            _ => a.span_count.cmp(&b.span_count),
        };
        let ord = if desc { ord.reverse() } else { ord };
        ord.then_with(|| a.values.cmp(&b.values))
    });
}

fn aggregate_avg_duration_ns(bucket: &TraceAggregateBucket) -> u128 {
    if bucket.durations_ns.is_empty() {
        0
    } else {
        bucket.duration_sum_ns / bucket.durations_ns.len() as u128
    }
}

fn trace_aggregate_bucket_json(
    bucket: &TraceAggregateBucket,
    fields: &[TraceAggregateGroupField],
) -> String {
    let key = fields
        .iter()
        .enumerate()
        .map(|(i, field)| {
            format!(
                r#""{}":{}"#,
                json_escape(&field.output_key),
                bucket
                    .values
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| "null".to_string())
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let examples = bucket
        .examples
        .iter()
        .map(trace_aggregate_example_json)
        .collect::<Vec<_>>()
        .join(",");
    let error_rate = if bucket.span_count == 0 {
        0.0
    } else {
        bucket.error_count as f64 / bucket.span_count as f64
    };
    format!(
        r#"{{"key":{{{key}}},"spanCount":{},"traceCount":{},"errorCount":{},"errorRate":{:.6},"durationNs":{},"usage":{},"costUsd":{},"costDetail":{},"examples":[{}]}}"#,
        bucket.span_count,
        bucket.trace_ids.len(),
        bucket.error_count,
        error_rate,
        aggregate_duration_json(bucket),
        usage_json(
            bucket.input_tokens,
            bucket.output_tokens,
            bucket.cached_input_tokens,
            bucket.reasoning_tokens,
            bucket.total_tokens,
        ),
        cost_usd_num_from_nanos(bucket.cost_usd_nanos),
        cost_detail_json(bucket.cost_usd_nanos, Some("USD"), "mixed"),
        examples,
    )
}

fn aggregate_duration_json(bucket: &TraceAggregateBucket) -> String {
    let mut durations = bucket.durations_ns.clone();
    durations.sort_unstable();
    duration_values_json(&durations, bucket.duration_sum_ns, bucket.duration_max_ns)
}

fn duration_values_json(
    sorted_durations: &[u64],
    duration_sum_ns: u128,
    duration_max_ns: u64,
) -> String {
    let count = sorted_durations.len();
    let avg = if count == 0 {
        "null".to_string()
    } else {
        (duration_sum_ns / count as u128).to_string()
    };
    let max = if count == 0 {
        "null".to_string()
    } else {
        duration_max_ns.to_string()
    };
    let p50 = percentile_json(sorted_durations, 50);
    let p95 = percentile_json(sorted_durations, 95);
    format!(
        r#"{{"sum":{},"avg":{},"max":{},"p50":{},"p95":{},"count":{}}}"#,
        duration_sum_ns, avg, max, p50, p95, count
    )
}

fn trajectory_duration_json(bucket: &TrajectoryGroupBucket) -> String {
    let mut durations = bucket.durations_ns.clone();
    durations.sort_unstable();
    duration_values_json(&durations, bucket.duration_sum_ns, bucket.duration_max_ns)
}

fn score_stats_json(stats: &ScoreStats) -> String {
    if stats.count == 0 {
        r#"{"count":0,"avg":null,"min":null,"max":null}"#.to_string()
    } else {
        format!(
            r#"{{"count":{},"avg":{},"min":{},"max":{}}}"#,
            stats.count,
            stats.avg(),
            stats.min,
            stats.max,
        )
    }
}

fn trajectory_trace_quality_score(
    success: bool,
    spans: &[FoldedSpan],
    annotation_scores: Option<&[u32]>,
    dataset_scores: Option<&[u32]>,
) -> u32 {
    let mut sum = if success { 1000u64 } else { 0u64 };
    let mut count = 1u64;
    let mut eval = ScoreStats::default();
    for score in spans.iter().filter_map(|s| s.eval_score) {
        eval.add(score);
    }
    if eval.count > 0 {
        sum += eval.avg() as u64;
        count += 1;
    }
    for scores in [annotation_scores, dataset_scores].into_iter().flatten() {
        if !scores.is_empty() {
            let local_sum: u64 = scores.iter().map(|s| *s as u64).sum();
            sum += local_sum / scores.len() as u64;
            count += 1;
        }
    }
    (sum / count) as u32
}

fn trace_annotation_score_map(
    annotations: Vec<crate::TraceAnnotation>,
) -> std::collections::HashMap<u64, Vec<u32>> {
    let mut out = std::collections::HashMap::new();
    for annotation in annotations {
        if let Some(score) = annotation.score {
            out.entry(annotation.trace_id)
                .or_insert_with(Vec::new)
                .push(score);
        }
    }
    out
}

fn trace_dataset_score_map(
    associations: Vec<crate::DatasetAssociation>,
) -> std::collections::HashMap<u64, Vec<u32>> {
    let mut out = std::collections::HashMap::new();
    for assoc in associations {
        if let Some(score) = assoc.score {
            out.entry(assoc.trace_id)
                .or_insert_with(Vec::new)
                .push(score);
        }
    }
    out
}

fn trajectory_default_desc(sort_by: &str) -> bool {
    !matches!(
        sort_by
            .to_ascii_lowercase()
            .replace(['_', '-'], "")
            .as_str(),
        "duration" | "durationns" | "avgduration" | "durationavg" | "cost" | "avgcost"
    )
}

fn sort_trajectory_group_buckets(buckets: &mut [TrajectoryGroupBucket], sort_by: &str, desc: bool) {
    let sort = sort_by.to_ascii_lowercase().replace(['_', '-'], "");
    buckets.sort_by(|a, b| {
        let ord = match sort.as_str() {
            "tracecount" | "traces" | "count" => a.trace_count().cmp(&b.trace_count()),
            "spancount" | "spans" => a.span_count.cmp(&b.span_count),
            "errorcount" | "errors" => a.error_trace_count.cmp(&b.error_trace_count),
            "successrate" | "success" => trajectory_success_cmp(a, b),
            "eval" | "evalscore" | "avgeval" => a.eval_scores.avg().cmp(&b.eval_scores.avg()),
            "annotation" | "annotationscore" | "avgannotation" => {
                a.annotation_scores.avg().cmp(&b.annotation_scores.avg())
            }
            "dataset" | "datasetscore" | "avgdataset" => {
                a.dataset_scores.avg().cmp(&b.dataset_scores.avg())
            }
            "duration" | "durationns" | "avgduration" | "durationavg" => {
                a.avg_duration_ns().cmp(&b.avg_duration_ns())
            }
            "cost" | "avgcost" => a.avg_cost_usd_nanos().cmp(&b.avg_cost_usd_nanos()),
            "tokens" | "totaltokens" => a.total_tokens.cmp(&b.total_tokens),
            _ => trajectory_best_cmp(a, b),
        };
        let ord = if desc { ord.reverse() } else { ord };
        ord.then_with(|| a.signature.cmp(&b.signature))
    });
}

fn trajectory_best_cmp(a: &TrajectoryGroupBucket, b: &TrajectoryGroupBucket) -> std::cmp::Ordering {
    a.quality_score()
        .cmp(&b.quality_score())
        .then_with(|| trajectory_success_cmp(a, b))
        .then_with(|| a.eval_scores.avg().cmp(&b.eval_scores.avg()))
        .then_with(|| a.annotation_scores.avg().cmp(&b.annotation_scores.avg()))
        .then_with(|| a.dataset_scores.avg().cmp(&b.dataset_scores.avg()))
        .then_with(|| a.trace_count().cmp(&b.trace_count()))
        // 低耗时/低成本是更好的 tie-breaker：这里反向比较，让 desc 排序时更小的值靠前。
        .then_with(|| b.avg_duration_ns().cmp(&a.avg_duration_ns()))
        .then_with(|| b.avg_cost_usd_nanos().cmp(&a.avg_cost_usd_nanos()))
}

fn trajectory_success_cmp(
    a: &TrajectoryGroupBucket,
    b: &TrajectoryGroupBucket,
) -> std::cmp::Ordering {
    let left = a.success_count() as u128 * b.trace_count().max(1) as u128;
    let right = b.success_count() as u128 * a.trace_count().max(1) as u128;
    left.cmp(&right)
}

fn json_trajectory_group_bucket(bucket: &TrajectoryGroupBucket) -> String {
    let steps = bucket
        .steps
        .iter()
        .map(|s| json_string_value(s))
        .collect::<Vec<_>>()
        .join(",");
    let examples = bucket
        .examples
        .iter()
        .map(json_trajectory_trace_example)
        .collect::<Vec<_>>()
        .join(",");
    let trace_count = bucket.trace_count();
    let success_rate = if trace_count == 0 {
        0.0
    } else {
        bucket.success_count() as f64 / trace_count as f64
    };
    let error_rate = if trace_count == 0 {
        0.0
    } else {
        bucket.error_trace_count as f64 / trace_count as f64
    };
    format!(
        r#"{{"signature":"fnv1a64:{:016x}","stepCount":{},"steps":[{}],"traceCount":{},"spanCount":{},"successCount":{},"errorTraceCount":{},"errorSpanCount":{},"successRate":{:.6},"errorRate":{:.6},"qualityScore":{},"durationNs":{},"usage":{},"costUsd":{},"costDetail":{},"scores":{{"eval":{},"annotation":{},"dataset":{}}},"examples":[{}]}}"#,
        bucket.signature,
        bucket.steps.len(),
        steps,
        trace_count,
        bucket.span_count,
        bucket.success_count(),
        bucket.error_trace_count,
        bucket.error_span_count,
        success_rate,
        error_rate,
        bucket.quality_score(),
        trajectory_duration_json(bucket),
        usage_json(
            bucket.input_tokens,
            bucket.output_tokens,
            bucket.cached_input_tokens,
            bucket.reasoning_tokens,
            bucket.total_tokens,
        ),
        cost_usd_num_from_nanos(bucket.cost_usd_nanos),
        cost_detail_json(bucket.cost_usd_nanos, Some("USD"), "mixed"),
        score_stats_json(&bucket.eval_scores),
        score_stats_json(&bucket.annotation_scores),
        score_stats_json(&bucket.dataset_scores),
        examples,
    )
}

fn json_trajectory_trace_example(example: &TrajectoryTraceExample) -> String {
    format!(
        r#"{{"traceId":"{}","externalTraceId":{},"status":"{}","durationNs":{{"sum":{},"max":{}}},"usage":{},"costUsd":{},"costDetail":{},"qualityScore":{},"fields":{}}}"#,
        example.trace_id,
        json_opt_str(example.external_trace_id.as_deref()),
        json_escape(&example.status),
        example.duration_sum_ns,
        example.duration_max_ns,
        usage_json(
            example.input_tokens,
            example.output_tokens,
            example.cached_input_tokens,
            example.reasoning_tokens,
            example.total_tokens,
        ),
        cost_usd_num_from_nanos(example.cost_usd_nanos),
        cost_detail_json(example.cost_usd_nanos, Some("USD"), "mixed"),
        example.score,
        json_attrs(&example.fields),
    )
}

fn percentile_json(sorted: &[u64], percentile: usize) -> String {
    if sorted.is_empty() {
        return "null".to_string();
    }
    let idx = ((sorted.len() * percentile + 99) / 100)
        .saturating_sub(1)
        .min(sorted.len() - 1);
    sorted[idx].to_string()
}

fn trace_aggregate_example_json(example: &TraceAggregateExample) -> String {
    format!(
        r#"{{"traceId":"{}","spanId":"{}","externalTraceId":{},"externalSpanId":{},"name":"{}"}}"#,
        example.trace_id,
        example.span_id,
        json_opt_str(example.external_trace_id.as_deref()),
        json_opt_str(example.external_span_id.as_deref()),
        json_escape(&example.name),
    )
}

fn json_trace_diff(
    left_id: u64,
    right_id: u64,
    left: &[FoldedSpan],
    right: &[FoldedSpan],
) -> String {
    let left_summary = trace_summary_buckets_from_spans(left);
    let right_summary = trace_summary_buckets_from_spans(right);
    let left_bucket = left_summary.first();
    let right_bucket = right_summary.first();
    format!(
        r#"{{"left":{},"right":{},"delta":{},"trajectory":{},"routes":{{"left":{},"right":{}}},"steps":{}}}"#,
        trace_diff_side_json(left_id, left_bucket),
        trace_diff_side_json(right_id, right_bucket),
        trace_diff_delta_json(left_bucket, right_bucket),
        trace_diff_trajectory_json(left, right),
        trace_diff_route_json(left),
        trace_diff_route_json(right),
        trace_diff_steps_json(left, right),
    )
}

fn trace_diff_side_json(trace_id: u64, bucket: Option<&TaskTraceSummaryBucket>) -> String {
    if let Some(bucket) = bucket {
        format!(
            r#"{{"traceId":"{}","externalTraceId":{},"spanCount":{},"errorCount":{},"status":"{}","durationNs":{{"sum":{},"max":{}}},"usage":{},"costUsd":{},"costDetail":{},"fields":{}}}"#,
            trace_id,
            json_opt_str(bucket.external_trace_id.as_deref()),
            bucket.span_count,
            bucket.error_count,
            if bucket.error_count > 0 {
                "error"
            } else {
                "ok"
            },
            bucket.duration_sum_ns,
            bucket.duration_max_ns,
            usage_json(
                bucket.input_tokens,
                bucket.output_tokens,
                bucket.cached_input_tokens,
                bucket.reasoning_tokens,
                bucket.total_tokens,
            ),
            cost_usd_num_from_nanos(bucket.cost_usd_nanos),
            cost_detail_json(bucket.cost_usd_nanos, Some("USD"), "mixed"),
            json_attrs(&bucket.fields),
        )
    } else {
        format!(
            r#"{{"traceId":"{}","spanCount":0,"errorCount":0,"status":"missing","durationNs":{{"sum":0,"max":0}},"usage":{},"costUsd":0.000000,"costDetail":{},"fields":{{}}}}"#,
            trace_id,
            usage_json(0, 0, 0, 0, 0),
            cost_detail_json(0, Some("USD"), "mixed"),
        )
    }
}

fn trace_trajectory_side_json(summary: &crate::TraceTrajectorySummary) -> String {
    format!(
        r#"{{"traceId":"{}","externalTraceId":{},"spanCount":{},"errorCount":{},"status":"{}","durationNs":{{"sum":{},"max":{}}},"usage":{},"costUsd":{},"costDetail":{},"fields":{}}}"#,
        summary.trace_id,
        json_opt_str(summary.external_trace_id.as_deref()),
        summary.span_count,
        summary.error_count,
        if summary.error_count > 0 {
            "error"
        } else {
            "ok"
        },
        summary.duration_sum_ns,
        summary.duration_max_ns,
        usage_json(
            summary.input_tokens,
            summary.output_tokens,
            summary.cached_input_tokens,
            summary.reasoning_tokens,
            summary.total_tokens,
        ),
        cost_usd_num_from_nanos(summary.cost_usd_nanos),
        cost_detail_json(summary.cost_usd_nanos, Some("USD"), "mixed"),
        json_attrs(&summary.fields),
    )
}

fn json_trace_trajectory_summary(summary: &crate::TraceTrajectorySummary) -> String {
    format!(
        r#"{{"trace":{},"trajectory":{},"index":"materialized"}}"#,
        trace_trajectory_side_json(summary),
        trajectory_summary_json_with_signature(&summary.steps, &summary.trajectory_signature),
    )
}

fn storage_group_by_from_json(v: &crate::wire::Json) -> Vec<String> {
    let Some(raw) = json_field_alias(v, &["groupBy", "group_by", "groups"]) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut push_key = |raw: &str| {
        let key = normalize_storage_group_key(raw);
        if !key.is_empty() && !out.contains(&key) {
            out.push(key);
        }
    };
    match raw {
        crate::wire::Json::Str(s) => {
            for part in s.split(',') {
                push_key(part);
            }
        }
        crate::wire::Json::Arr(items) => {
            for item in items {
                match item {
                    crate::wire::Json::Str(s) | crate::wire::Json::Num(s) => push_key(s),
                    _ => {}
                }
            }
        }
        _ => {}
    }
    out
}

fn normalize_storage_group_key(raw: &str) -> String {
    let lower = raw.trim().replace('-', "_").to_ascii_lowercase();
    let compact = lower.replace('_', "");
    match compact.as_str() {
        "projectid" => "project_id".to_string(),
        "taskfingerprint" => "task_fingerprint".to_string(),
        "callsite" => "call_site".to_string(),
        "loopid" => "loop_id".to_string(),
        "harnessversion" => "harness_version".to_string(),
        "validationstatus" => "validation_status".to_string(),
        "stopreason" => "stop_reason".to_string(),
        "sessionid" => "session_id".to_string(),
        "traceid" => "trace_id".to_string(),
        "spanid" => "span_id".to_string(),
        "agentname" => "agent_name".to_string(),
        "toolname" => "tool_name".to_string(),
        "timebucket" | "day" | "time" => "time".to_string(),
        _ => lower,
    }
}

fn storage_stats_report(
    spans: &[FoldedSpan],
    bounds: &std::collections::BTreeMap<u64, (i64, i64)>,
    metadata: &StorageMetadata,
    group_by: &[String],
    time_bucket_ns: u64,
) -> StorageStatsReport {
    let mut total = StorageStatsBucket::default();
    let mut groups: std::collections::BTreeMap<
        std::collections::BTreeMap<String, String>,
        StorageStatsBucket,
    > = std::collections::BTreeMap::new();

    for span in spans {
        storage_bucket_add_span(&mut total, span, bounds);
        if !group_by.is_empty() {
            let mut key = std::collections::BTreeMap::new();
            for field in group_by {
                key.insert(
                    field.clone(),
                    storage_group_value_json(span, field, bounds, time_bucket_ns),
                );
            }
            let bucket = groups
                .entry(key.clone())
                .or_insert_with(|| StorageStatsBucket {
                    key,
                    ..StorageStatsBucket::default()
                });
            storage_bucket_add_span(bucket, span, bounds);
        }
    }

    storage_bucket_apply_metadata_counts(&mut total, metadata);
    let mut groups: Vec<StorageStatsBucket> = groups.into_values().collect();
    for bucket in &mut groups {
        storage_bucket_apply_metadata_counts(bucket, metadata);
    }
    groups.sort_by(|a, b| {
        b.estimated_bytes
            .cmp(&a.estimated_bytes)
            .then_with(|| b.trace_ids.len().cmp(&a.trace_ids.len()))
            .then_with(|| a.key.cmp(&b.key))
    });
    StorageStatsReport { total, groups }
}

fn storage_group_value_json(
    s: &FoldedSpan,
    key: &str,
    bounds: &std::collections::BTreeMap<u64, (i64, i64)>,
    time_bucket_ns: u64,
) -> String {
    match key {
        "time" => {
            let Some((first_ts, _)) = bounds.get(&s.trace_id) else {
                return "null".to_string();
            };
            let width = (time_bucket_ns.max(1).min(i64::MAX as u64)) as i64;
            let bucket = first_ts.div_euclid(width) * width;
            json_string_value(&bucket.to_string())
        }
        "trace_id" => json_string_value(&s.trace_id.to_string()),
        "span_id" => json_string_value(&s.span_id.to_string()),
        "session_id" => s
            .external_session_id
            .as_deref()
            .map(json_string_value)
            .or_else(|| s.session_id.map(|id| json_string_value(&id.to_string())))
            .unwrap_or_else(|| "null".to_string()),
        "agent_name" => s
            .agent_name
            .as_deref()
            .map(json_string_value)
            .unwrap_or_else(|| "null".to_string()),
        "tool_name" => s
            .tool_name
            .as_deref()
            .map(json_string_value)
            .unwrap_or_else(|| "null".to_string()),
        "kind" => json_string_value(folded_kind(s)),
        "status" => s
            .status
            .map(|status| status.to_string())
            .unwrap_or_else(|| "null".to_string()),
        _ => crate::folded_span_attr_value(s, key)
            .map(storage_compact_or_string_json)
            .unwrap_or_else(|| "null".to_string()),
    }
}

fn storage_compact_or_string_json(value: &str) -> String {
    match crate::wire::parse(value) {
        Ok(v) => v.to_compact_json(),
        Err(_) => json_string_value(value),
    }
}

fn storage_bucket_add_span(
    bucket: &mut StorageStatsBucket,
    s: &FoldedSpan,
    bounds: &std::collections::BTreeMap<u64, (i64, i64)>,
) {
    bucket.span_count += 1;
    bucket.event_count += s.event_count;
    bucket.trace_ids.insert(s.trace_id);
    if let Some(session_id) = s.session_id {
        bucket.session_ids.insert(session_id);
    }
    if s.status.unwrap_or(0) != 0 {
        bucket.error_span_count += 1;
    }
    if let Some((first_ts, last_ts)) = bounds.get(&s.trace_id) {
        bucket.first_ts = Some(bucket.first_ts.map_or(*first_ts, |v| v.min(*first_ts)));
        bucket.last_ts = Some(bucket.last_ts.map_or(*last_ts, |v| v.max(*last_ts)));
    }

    let input_bytes = s.input_text.as_deref().map(str::len).unwrap_or(0) as u64;
    let output_bytes = s.output_text.as_deref().map(str::len).unwrap_or(0) as u64;
    let log_bytes = s.logs.iter().map(|log| log.len() as u64).sum::<u64>();
    let attr_bytes = s
        .attrs
        .iter()
        .map(|(k, v)| k.len() as u64 + v.len() as u64 + 4)
        .sum::<u64>();
    let external_id_bytes = [
        s.external_trace_id.as_deref(),
        s.external_span_id.as_deref(),
        s.external_parent_span_id.as_deref(),
        s.external_session_id.as_deref(),
    ]
    .into_iter()
    .flatten()
    .map(|value| value.len() as u64)
    .sum::<u64>();
    let field_bytes = [
        s.agent_name.as_deref(),
        s.tool_name.as_deref(),
        s.model.as_deref(),
        s.provider.as_deref(),
        s.project_id.as_deref(),
        s.skill.as_deref(),
        s.mode.as_deref(),
        s.call_site.as_deref(),
        s.task_fingerprint.as_deref(),
        s.loop_id.as_deref(),
    ]
    .into_iter()
    .flatten()
    .map(|value| value.len() as u64)
    .sum::<u64>()
        + 8 * [
            s.duration_ns,
            s.input_tokens,
            s.output_tokens,
            s.cached_input_tokens,
            s.reasoning_tokens,
            s.total_tokens,
            s.cost_usd_nanos,
        ]
        .into_iter()
        .flatten()
        .count() as u64;

    bucket.input_text_bytes += input_bytes;
    bucket.output_text_bytes += output_bytes;
    bucket.log_bytes += log_bytes;
    bucket.attr_bytes += attr_bytes;
    bucket.external_id_bytes += external_id_bytes;
    bucket.field_bytes += field_bytes;
    bucket.estimated_bytes += input_bytes
        + output_bytes
        + log_bytes
        + attr_bytes
        + external_id_bytes
        + field_bytes
        + (s.event_count as u64 * 64)
        + 128;
}

fn storage_bucket_apply_metadata_counts(
    bucket: &mut StorageStatsBucket,
    metadata: &StorageMetadata,
) {
    bucket.annotation_count = metadata
        .annotations
        .iter()
        .filter(|a| bucket.trace_ids.contains(&a.trace_id))
        .count();
    bucket.dataset_association_count = metadata
        .dataset_associations
        .iter()
        .filter(|a| bucket.trace_ids.contains(&a.trace_id))
        .count();
    bucket.golden_path_count = metadata
        .golden_paths
        .iter()
        .filter(|g| bucket.trace_ids.contains(&g.source_trace_id))
        .count();
    bucket.snapshot_ref_count = metadata
        .dataset_associations
        .iter()
        .filter(|a| bucket.trace_ids.contains(&a.trace_id) && dataset_association_has_snapshot(a))
        .count()
        + metadata
            .golden_paths
            .iter()
            .filter(|g| {
                bucket.trace_ids.contains(&g.source_trace_id) && golden_path_has_snapshot(g)
            })
            .count();
    bucket.eval_link_count = metadata
        .dataset_associations
        .iter()
        .filter(|a| bucket.trace_ids.contains(&a.trace_id) && dataset_association_is_eval_link(a))
        .count()
        + metadata
            .annotations
            .iter()
            .filter(|a| {
                bucket.trace_ids.contains(&a.trace_id) && metadata_attrs_have_eval_link(&a.attrs)
            })
            .count()
        + metadata
            .golden_paths
            .iter()
            .filter(|g| {
                bucket.trace_ids.contains(&g.source_trace_id)
                    && metadata_attrs_have_eval_link(&g.attrs)
            })
            .count();
    bucket.path_memory_ref_count = metadata
        .dataset_associations
        .iter()
        .filter(|a| {
            bucket.trace_ids.contains(&a.trace_id) && metadata_attrs_have_path_memory(&a.attrs)
        })
        .count()
        + metadata
            .annotations
            .iter()
            .filter(|a| {
                bucket.trace_ids.contains(&a.trace_id) && metadata_attrs_have_path_memory(&a.attrs)
            })
            .count()
        + metadata
            .golden_paths
            .iter()
            .filter(|g| {
                bucket.trace_ids.contains(&g.source_trace_id)
                    && metadata_attrs_have_path_memory(&g.attrs)
            })
            .count();
}

fn storage_bucket_for_trace_ids(
    spans: &[FoldedSpan],
    bounds: &std::collections::BTreeMap<u64, (i64, i64)>,
    trace_ids: &std::collections::HashSet<u64>,
) -> StorageStatsBucket {
    let mut bucket = StorageStatsBucket::default();
    for span in spans {
        if trace_ids.contains(&span.trace_id) {
            storage_bucket_add_span(&mut bucket, span, bounds);
        }
    }
    bucket
}

fn json_storage_stats_report(report: &StorageStatsReport, group_by: &[String]) -> String {
    let group_by_json = json_string_array(group_by);
    let groups = report
        .groups
        .iter()
        .map(|bucket| json_storage_bucket(bucket, true))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"{{"groupBy":{},"total":{},"groups":[{}]}}"#,
        group_by_json,
        json_storage_bucket(&report.total, false),
        groups,
    )
}

fn json_storage_bucket(bucket: &StorageStatsBucket, include_key: bool) -> String {
    let payload_bytes = bucket.input_text_bytes + bucket.output_text_bytes + bucket.log_bytes;
    let key = if include_key {
        format!(r#""key":{},"#, json_attrs(&bucket.key))
    } else {
        String::new()
    };
    format!(
        r#"{{{}"traceCount":{},"spanCount":{},"sessionCount":{},"eventCount":{},"errorSpanCount":{},"firstTs":{},"lastTs":{},"bytes":{{"inputText":{},"outputText":{},"logs":{},"payload":{},"attrs":{},"externalIds":{},"fields":{},"estimated":{},"estimatedBytes":{}}},"metadata":{{"annotations":{},"datasetAssociations":{},"goldenPaths":{},"snapshotRefs":{},"evalLinks":{},"pathMemoryRefs":{}}}}}"#,
        key,
        bucket.trace_ids.len(),
        bucket.span_count,
        bucket.session_ids.len(),
        bucket.event_count,
        bucket.error_span_count,
        json_opt_i64(bucket.first_ts),
        json_opt_i64(bucket.last_ts),
        bucket.input_text_bytes,
        bucket.output_text_bytes,
        bucket.log_bytes,
        payload_bytes,
        bucket.attr_bytes,
        bucket.external_id_bytes,
        bucket.field_bytes,
        bucket.estimated_bytes,
        bucket.estimated_bytes,
        bucket.annotation_count,
        bucket.dataset_association_count,
        bucket.golden_path_count,
        bucket.snapshot_ref_count,
        bucket.eval_link_count,
        bucket.path_memory_ref_count,
    )
}

fn json_opt_i64(value: Option<i64>) -> String {
    value
        .map(|v| v.to_string())
        .unwrap_or_else(|| "null".to_string())
}

fn retention_protect_bool(v: &crate::wire::Json, camel: &str, snake: &str, default: bool) -> bool {
    let top_camel = format!("protect{}", capitalize_ascii(camel));
    let top_snake = format!("protect_{}", snake);
    if let Some(value) = json_bool_alias(v, &[top_camel.as_str(), top_snake.as_str(), camel, snake])
    {
        return value;
    }
    if let Some(protect) = crate::wire::field(v, "protect") {
        if let Some(value) = json_bool_alias(protect, &[camel, snake]) {
            return value;
        }
    }
    default
}

fn capitalize_ascii(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

fn protected_trace_reasons(
    candidate_trace_ids: &std::collections::HashSet<u64>,
    metadata: &StorageMetadata,
    protect_golden_paths: bool,
    protect_annotations: bool,
    protect_dataset_associations: bool,
    protect_snapshots: bool,
    protect_eval_links: bool,
    protect_path_memory: bool,
) -> std::collections::BTreeMap<u64, Vec<String>> {
    let mut reasons = std::collections::BTreeMap::<u64, Vec<String>>::new();
    let mut add = |trace_id: u64, reason: &str| {
        if candidate_trace_ids.contains(&trace_id) {
            let entry = reasons.entry(trace_id).or_default();
            if !entry.iter().any(|item| item == reason) {
                entry.push(reason.to_string());
            }
        }
    };
    if protect_annotations {
        for annotation in &metadata.annotations {
            add(annotation.trace_id, "annotation");
        }
    }
    if protect_dataset_associations {
        for association in &metadata.dataset_associations {
            add(association.trace_id, "datasetAssociation");
        }
    }
    if protect_golden_paths {
        for golden_path in &metadata.golden_paths {
            if matches!(
                golden_path.status,
                GoldenPathStatus::Candidate | GoldenPathStatus::Confirmed
            ) {
                add(golden_path.source_trace_id, "goldenPath");
            }
        }
    }
    if protect_snapshots {
        for association in &metadata.dataset_associations {
            if dataset_association_has_snapshot(association) {
                add(association.trace_id, "snapshot");
            }
        }
        for golden_path in &metadata.golden_paths {
            if golden_path_has_snapshot(golden_path) {
                add(golden_path.source_trace_id, "snapshot");
            }
        }
    }
    if protect_eval_links {
        for association in &metadata.dataset_associations {
            if dataset_association_is_eval_link(association) {
                add(association.trace_id, "evalLink");
            }
        }
        for annotation in &metadata.annotations {
            if metadata_attrs_have_eval_link(&annotation.attrs) {
                add(annotation.trace_id, "evalLink");
            }
        }
        for golden_path in &metadata.golden_paths {
            if metadata_attrs_have_eval_link(&golden_path.attrs) {
                add(golden_path.source_trace_id, "evalLink");
            }
        }
    }
    if protect_path_memory {
        for association in &metadata.dataset_associations {
            if metadata_attrs_have_path_memory(&association.attrs) {
                add(association.trace_id, "pathMemory");
            }
        }
        for annotation in &metadata.annotations {
            if metadata_attrs_have_path_memory(&annotation.attrs) {
                add(annotation.trace_id, "pathMemory");
            }
        }
        for golden_path in &metadata.golden_paths {
            if metadata_attrs_have_path_memory(&golden_path.attrs) {
                add(golden_path.source_trace_id, "pathMemory");
            }
        }
    }
    reasons
}

fn dataset_association_has_snapshot(a: &crate::DatasetAssociation) -> bool {
    a.snapshot_id.as_deref().is_some_and(|v| !v.is_empty())
        || a.snapshot_hash.as_deref().is_some_and(|v| !v.is_empty())
}

fn golden_path_has_snapshot(g: &crate::GoldenPathCandidate) -> bool {
    g.snapshot_id.as_deref().is_some_and(|v| !v.is_empty())
        || g.snapshot_hash.as_deref().is_some_and(|v| !v.is_empty())
}

fn dataset_association_is_eval_link(a: &crate::DatasetAssociation) -> bool {
    a.eval_run_id.as_deref().is_some_and(|v| !v.is_empty())
        || metadata_attrs_have_eval_link(&a.attrs)
}

fn metadata_attrs_have_eval_link(attrs: &std::collections::BTreeMap<String, String>) -> bool {
    attrs.contains_key("eval_run_id")
        || attrs.contains_key("evalRunId")
        || attrs.contains_key("eval_profile")
        || attrs.contains_key("evalProfile")
        || attrs.contains_key("eval_status")
        || attrs.contains_key("evalStatus")
}

fn metadata_attrs_have_path_memory(attrs: &std::collections::BTreeMap<String, String>) -> bool {
    attrs.contains_key("path_memory_id") || attrs.contains_key("pathMemoryId")
}

fn json_retention_plan(
    apply: bool,
    cutoff: Option<i64>,
    protect_golden_paths: bool,
    protect_annotations: bool,
    protect_dataset_associations: bool,
    protect_snapshots: bool,
    protect_eval_links: bool,
    protect_path_memory: bool,
    candidate_stats: &StorageStatsBucket,
    protected_stats: &StorageStatsBucket,
    deletable_stats: &StorageStatsBucket,
    protected: &std::collections::BTreeMap<u64, Vec<String>>,
    deletable_trace_ids: &std::collections::HashSet<u64>,
    applied: Option<&crate::RetentionDeleteResult>,
    compact_after_apply: bool,
    compact_min_deleted_rows: u32,
    compact_min_deleted_percent: u32,
    compact_max_segments: usize,
    reclaim_after_compact: bool,
    compacted: Option<&crate::RetentionCompactResult>,
    audit: Option<&crate::RetentionAuditRecord>,
    example_limit: usize,
) -> String {
    let protected_reasons = protected
        .iter()
        .map(|(trace_id, reasons)| format!(r#""{}":{}"#, trace_id, json_string_array(reasons)))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"{{"dryRun":{},"applied":{},"deleteBeforeTs":{},"protect":{{"goldenPaths":{},"annotations":{},"datasetAssociations":{},"snapshots":{},"evalLinks":{},"pathMemory":{}}},"compact":{{"requested":{},"minDeletedRows":{},"minDeletedPercent":{},"maxSegments":{},"reclaim":{}}},"candidates":{},"protected":{},"deletable":{},"protectedReasons":{{{}}},"deletableTraceIds":{},"applyResult":{},"compactResult":{},"audit":{}}}"#,
        json_bool(!apply),
        json_bool(apply),
        json_opt_i64(cutoff),
        json_bool(protect_golden_paths),
        json_bool(protect_annotations),
        json_bool(protect_dataset_associations),
        json_bool(protect_snapshots),
        json_bool(protect_eval_links),
        json_bool(protect_path_memory),
        json_bool(compact_after_apply),
        compact_min_deleted_rows,
        compact_min_deleted_percent,
        compact_max_segments,
        json_bool(reclaim_after_compact),
        json_storage_bucket(candidate_stats, false),
        json_storage_bucket(protected_stats, false),
        json_storage_bucket(deletable_stats, false),
        protected_reasons,
        json_u64_set_as_string_array(deletable_trace_ids, example_limit),
        applied
            .map(json_retention_delete_result)
            .unwrap_or_else(|| "null".to_string()),
        compacted
            .map(json_retention_compact_result)
            .unwrap_or_else(|| "null".to_string()),
        audit
            .map(json_retention_audit)
            .unwrap_or_else(|| "null".to_string()),
    )
}

fn sample_u64_set(items: &std::collections::HashSet<u64>, limit: usize) -> Vec<u64> {
    let mut ids = items.iter().copied().collect::<Vec<_>>();
    ids.sort_unstable();
    ids.truncate(limit);
    ids
}

fn sample_u64_slice(items: &[u64], limit: usize) -> Vec<u64> {
    let mut ids = items.to_vec();
    ids.sort_unstable();
    ids.truncate(limit);
    ids
}

fn json_u64_set_as_string_array(items: &std::collections::HashSet<u64>, limit: usize) -> String {
    let mut ids = items.iter().copied().collect::<Vec<_>>();
    ids.sort_unstable();
    let values = ids
        .into_iter()
        .take(limit)
        .map(|id| id.to_string())
        .collect::<Vec<_>>();
    json_string_array(&values)
}

fn json_u64_vec_as_string_array(items: &[u64], limit: usize) -> String {
    let values = items
        .iter()
        .take(limit)
        .map(|id| id.to_string())
        .collect::<Vec<_>>();
    json_string_array(&values)
}

fn json_u64_sample_as_string_array(items: &[u64]) -> String {
    let values = items.iter().map(|id| id.to_string()).collect::<Vec<_>>();
    json_string_array(&values)
}

fn json_retention_delete_result(result: &crate::RetentionDeleteResult) -> String {
    let limit = 100;
    format!(
        r#"{{"requestedTraceCount":{},"deletedTraceCount":{},"deletedSegmentRowCount":{},"skippedLiveTraceCount":{},"deletedTraceIds":{},"skippedLiveTraceIds":{}}}"#,
        result.requested_trace_count,
        result.deleted_trace_count,
        result.deleted_segment_row_count,
        result.skipped_live_trace_count,
        json_u64_vec_as_string_array(&result.deleted_trace_ids, limit),
        json_u64_vec_as_string_array(&result.skipped_live_trace_ids, limit),
    )
}

fn json_retention_audit(a: &crate::RetentionAuditRecord) -> String {
    format!(
        r#"{{"auditId":"{}","tenantId":{},"createdAtNs":"{}","source":{},"reason":{},"deleteBeforeTs":{},"query":{},"protect":{{"goldenPaths":{},"annotations":{},"datasetAssociations":{},"snapshots":{},"evalLinks":{},"pathMemory":{}}},"compact":{{"requested":{},"reclaim":{},"compactedSegmentCount":{},"reclaimedSegmentCount":{},"droppedDeletedRowCount":{},"rewrittenLiveRowCount":{}}},"counts":{{"candidateTraceCount":{},"protectedTraceCount":{},"deletableTraceCount":{},"requestedTraceCount":{},"deletedTraceCount":{},"deletedSegmentRowCount":{},"skippedLiveTraceCount":{}}},"traceIds":{{"deletable":{},"deleted":{},"skippedLive":{},"sampleTruncated":{}}}}}"#,
        a.audit_id,
        json_opt_u64_string(a.tenant_id),
        a.created_at_ns,
        json_opt_str(a.source.as_deref()),
        json_opt_str(a.reason.as_deref()),
        json_opt_i64(a.delete_before_ts),
        if a.query_json.trim().is_empty() {
            "{}".to_string()
        } else {
            a.query_json.clone()
        },
        json_bool(a.protect_golden_paths),
        json_bool(a.protect_annotations),
        json_bool(a.protect_dataset_associations),
        json_bool(a.protect_snapshots),
        json_bool(a.protect_eval_links),
        json_bool(a.protect_path_memory),
        json_bool(a.compact_requested),
        json_bool(a.compact_reclaim),
        a.compacted_segment_count,
        a.reclaimed_segment_count,
        a.dropped_deleted_row_count,
        a.rewritten_live_row_count,
        a.candidate_trace_count,
        a.protected_trace_count,
        a.deletable_trace_count,
        a.requested_trace_count,
        a.deleted_trace_count,
        a.deleted_segment_row_count,
        a.skipped_live_trace_count,
        json_u64_sample_as_string_array(&a.deletable_trace_ids),
        json_u64_sample_as_string_array(&a.deleted_trace_ids),
        json_u64_sample_as_string_array(&a.skipped_live_trace_ids),
        json_bool(a.trace_id_sample_truncated),
    )
}

fn json_retention_policy(p: &crate::RetentionPolicy) -> String {
    format!(
        r#"{{"policyId":"{}","tenantId":{},"name":"{}","enabled":{},"createdAtNs":"{}","updatedAtNs":"{}","lastRunAtNs":{},"nextRunAtNs":{},"intervalNs":"{}","source":{},"reason":{},"query":{}}}"#,
        p.policy_id,
        json_opt_u64_string(p.tenant_id),
        json_escape(&p.name),
        json_bool(p.enabled),
        p.created_at_ns,
        p.updated_at_ns,
        json_opt_u64_string(p.last_run_at_ns),
        json_opt_u64_string(p.next_run_at_ns),
        p.interval_ns,
        json_opt_str(p.source.as_deref()),
        json_opt_str(p.reason.as_deref()),
        if p.query_json.trim().is_empty() {
            "{}".to_string()
        } else {
            p.query_json.clone()
        },
    )
}

fn retention_policy_query_has_cutoff(v: &crate::wire::Json) -> bool {
    json_field_alias(
        v,
        &[
            "deleteBeforeTs",
            "delete_before_ts",
            "olderThanTs",
            "older_than_ts",
            "timeTo",
            "time_to",
            "olderThanNs",
            "older_than_ns",
            "ttlNs",
            "ttl_ns",
            "retentionNs",
            "retention_ns",
        ],
    )
    .is_some()
}

fn retention_policy_effective_query(
    policy: &crate::RetentionPolicy,
    now_ns: u64,
) -> Result<String, String> {
    let mut json = crate::wire::parse(&policy.query_json)?;
    let crate::wire::Json::Obj(ref mut kvs) = json else {
        return Err("policy query must be an object".to_string());
    };
    if !json_obj_has_alias(
        kvs,
        &[
            "deleteBeforeTs",
            "delete_before_ts",
            "olderThanTs",
            "older_than_ts",
            "timeTo",
            "time_to",
        ],
    ) {
        let ttl = json_obj_field_alias(
            kvs,
            &[
                "olderThanNs",
                "older_than_ns",
                "ttlNs",
                "ttl_ns",
                "retentionNs",
                "retention_ns",
            ],
        )
        .and_then(crate::wire::Json::as_u64)
        .ok_or_else(|| "policy query requires deleteBeforeTs or olderThanNs".to_string())?;
        let cutoff = now_ns.saturating_sub(ttl).min(i64::MAX as u64) as i64;
        json_obj_set(
            kvs,
            "deleteBeforeTs",
            crate::wire::Json::Num(cutoff.to_string()),
        );
    }
    json_obj_set(kvs, "apply", crate::wire::Json::Bool(true));
    if !json_obj_has_alias(
        kvs,
        &[
            "source",
            "requestedBy",
            "requested_by",
            "actor",
            "createdBy",
            "created_by",
        ],
    ) {
        if let Some(source) = &policy.source {
            json_obj_set(kvs, "requestedBy", crate::wire::Json::Str(source.clone()));
        }
    }
    if !json_obj_has_alias(kvs, &["reason", "comment", "note"]) {
        if let Some(reason) = &policy.reason {
            json_obj_set(kvs, "reason", crate::wire::Json::Str(reason.clone()));
        }
    }
    Ok(json.to_compact_json())
}

fn json_obj_field_alias<'a>(
    kvs: &'a [(String, crate::wire::Json)],
    names: &[&str],
) -> Option<&'a crate::wire::Json> {
    names.iter().find_map(|name| {
        kvs.iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value)
    })
}

fn json_obj_has_alias(kvs: &[(String, crate::wire::Json)], names: &[&str]) -> bool {
    json_obj_field_alias(kvs, names).is_some()
}

fn json_obj_set(kvs: &mut Vec<(String, crate::wire::Json)>, key: &str, value: crate::wire::Json) {
    if let Some((_, existing)) = kvs.iter_mut().find(|(name, _)| name == key) {
        *existing = value;
    } else {
        kvs.push((key.to_string(), value));
    }
}

fn parse_query_bool(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn unix_now_ns_u64_for_http() -> u64 {
    unix_now_ns().min(u64::MAX as u128) as u64
}

fn json_retention_compact_result(result: &crate::RetentionCompactResult) -> String {
    let ids = result
        .selected_segment_ids
        .iter()
        .map(|id| id.to_string())
        .collect::<Vec<_>>();
    format!(
        r#"{{"beforeLiveSegmentCount":{},"afterLiveSegmentCount":{},"beforeDeadSegmentCount":{},"afterDeadSegmentCount":{},"selectedSegmentCount":{},"compactedSegmentCount":{},"reclaimedSegmentCount":{},"droppedDeletedRowCount":{},"rewrittenLiveRowCount":{},"selectedSegmentIds":{}}}"#,
        result.before_live_segment_count,
        result.after_live_segment_count,
        result.before_dead_segment_count,
        result.after_dead_segment_count,
        result.selected_segment_count,
        result.compacted_segment_count,
        result.reclaimed_segment_count,
        result.dropped_deleted_row_count,
        result.rewritten_live_row_count,
        json_string_array(&ids),
    )
}

fn trace_diff_delta_json(
    left: Option<&TaskTraceSummaryBucket>,
    right: Option<&TaskTraceSummaryBucket>,
) -> String {
    let li = left.map(|b| b.input_tokens).unwrap_or(0);
    let lo = left.map(|b| b.output_tokens).unwrap_or(0);
    let lt = left.map(|b| b.total_tokens).unwrap_or(0);
    let lc = left.map(|b| b.cost_usd_nanos).unwrap_or(0);
    let ld = left.map(|b| b.duration_sum_ns).unwrap_or(0);
    let le = left.map(|b| b.error_count).unwrap_or(0);
    let ls = left.map(|b| b.span_count).unwrap_or(0);
    let ri = right.map(|b| b.input_tokens).unwrap_or(0);
    let ro = right.map(|b| b.output_tokens).unwrap_or(0);
    let rt = right.map(|b| b.total_tokens).unwrap_or(0);
    let rc = right.map(|b| b.cost_usd_nanos).unwrap_or(0);
    let rd = right.map(|b| b.duration_sum_ns).unwrap_or(0);
    let re = right.map(|b| b.error_count).unwrap_or(0);
    let rs = right.map(|b| b.span_count).unwrap_or(0);
    format!(
        r#"{{"spanCount":{},"errorCount":{},"durationNs":{},"inputTokens":{},"outputTokens":{},"totalTokens":{},"costUsdNanos":{},"costUsd":{}}}"#,
        rs as i128 - ls as i128,
        re as i128 - le as i128,
        rd as i128 - ld as i128,
        ri as i128 - li as i128,
        ro as i128 - lo as i128,
        rt as i128 - lt as i128,
        rc as i128 - lc as i128,
        format!("{:.6}", (rc as i128 - lc as i128) as f64 / 1_000_000_000.0),
    )
}

fn trace_diff_trajectory_json(left: &[FoldedSpan], right: &[FoldedSpan]) -> String {
    let left_steps = trajectory_steps(left);
    let right_steps = trajectory_steps(right);
    let left_sig = trajectory_signature(&left_steps);
    let right_sig = trajectory_signature(&right_steps);
    format!(
        r#"{{"left":{},"right":{},"same":{}}}"#,
        trajectory_summary_json(&left_steps, left_sig),
        trajectory_summary_json(&right_steps, right_sig),
        if left_sig == right_sig {
            "true"
        } else {
            "false"
        },
    )
}

fn trajectory_summary_json(steps: &[String], signature: u64) -> String {
    trajectory_summary_json_with_signature(steps, &format!("fnv1a64:{signature:016x}"))
}

fn trajectory_summary_json_with_signature(steps: &[String], signature: &str) -> String {
    let steps_json = steps
        .iter()
        .map(|s| json_string_value(s))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"{{"signature":"{}","stepCount":{},"steps":[{}]}}"#,
        json_escape(signature),
        steps.len(),
        steps_json,
    )
}

fn trajectory_steps(spans: &[FoldedSpan]) -> Vec<String> {
    crate::trajectory_steps_for_spans(spans)
}

fn trajectory_signature(steps: &[String]) -> u64 {
    crate::trajectory_signature_value(steps)
}

fn trajectory_signature_string(steps: &[String]) -> String {
    crate::trajectory_signature_label(steps)
}

fn ordered_step_diff(
    golden_steps: &[String],
    trace_steps: &[String],
) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut common = Vec::new();
    let mut missing = Vec::new();
    let mut trace_cursor = 0usize;
    for golden_step in golden_steps {
        if trace_cursor >= trace_steps.len() {
            missing.push(golden_step.clone());
            continue;
        }
        if let Some(offset) = trace_steps[trace_cursor..]
            .iter()
            .position(|trace_step| trace_step == golden_step)
        {
            common.push(golden_step.clone());
            trace_cursor += offset + 1;
        } else {
            missing.push(golden_step.clone());
        }
    }

    let mut extra = Vec::new();
    let mut common_iter = common.iter();
    let mut next_common = common_iter.next();
    for trace_step in trace_steps {
        if next_common.map(|step| step == trace_step).unwrap_or(false) {
            next_common = common_iter.next();
        } else {
            extra.push(trace_step.clone());
        }
    }
    (common, missing, extra)
}

fn trajectory_step(s: &FoldedSpan) -> String {
    let (kind, name) = trajectory_step_kind_name(s);
    let mut out = format!(
        "{}:{}",
        normalize_trajectory_part(kind),
        normalize_trajectory_part(&name)
    );
    for key in ["phase", "validator"] {
        if let Some(value) = crate::folded_span_attr_value(s, key) {
            out.push('|');
            out.push_str(key);
            out.push(':');
            out.push_str(&normalize_trajectory_part(&json_compact_label(value)));
        }
    }
    out
}

fn trajectory_step_kind_name(s: &FoldedSpan) -> (&'static str, String) {
    if let Some(tool) = &s.tool_name {
        ("tool", tool.clone())
    } else if let Some(agent) = &s.agent_name {
        ("agent", agent.clone())
    } else if let Some(model) = &s.model {
        ("llm", model.clone())
    } else {
        ("other", format!("span {}", s.span_id))
    }
}

fn normalize_trajectory_part(value: &str) -> String {
    value
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_whitespace() || matches!(c, '|' | ':' | '\0') {
                '_'
            } else {
                c
            }
        })
        .collect()
}

fn trace_diff_route_json(spans: &[FoldedSpan]) -> String {
    format!(
        "[{}]",
        spans
            .iter()
            .enumerate()
            .map(|(idx, span)| trace_diff_route_step_json(idx, span))
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn trace_diff_route_step_json(index: usize, s: &FoldedSpan) -> String {
    format!(
        r#"{{"spanId":"{}","externalSpanId":{},"kind":"{}","name":"{}","spanOrdinal":{},"sortKey":"{:020}:{:020}","agentName":{},"toolName":{},"model":{},"status":{},"statusText":"{}","fields":{}}}"#,
        s.span_id,
        json_opt_str(s.external_span_id.as_deref()),
        folded_kind(s),
        json_escape(&folded_name(s)),
        index,
        index,
        s.span_id,
        json_opt_str(s.agent_name.as_deref()),
        json_opt_str(s.tool_name.as_deref()),
        json_opt_str(s.model.as_deref()),
        s.status
            .map_or("null".to_string(), |status| status.to_string()),
        if s.status.unwrap_or(0) == 0 {
            "ok"
        } else {
            "error"
        },
        json_folded_agent_fields(s),
    )
}

fn trace_diff_steps_json(left: &[FoldedSpan], right: &[FoldedSpan]) -> String {
    let len = left.len().max(right.len());
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        out.push(trace_diff_step_json(i, left.get(i), right.get(i)));
    }
    format!("[{}]", out.join(","))
}

fn trace_diff_step_json(
    index: usize,
    left: Option<&FoldedSpan>,
    right: Option<&FoldedSpan>,
) -> String {
    let changes = trace_diff_step_changes(left, right);
    let status = match (left, right, changes.is_empty()) {
        (Some(_), Some(_), true) => "same",
        (Some(_), Some(_), false) => "changed",
        (Some(_), None, _) => "left_only",
        (None, Some(_), _) => "right_only",
        (None, None, _) => "same",
    };
    let changes_json = changes
        .iter()
        .map(|c| json_string_value(c))
        .collect::<Vec<_>>()
        .join(",");
    let duration_delta = right.and_then(|s| s.duration_ns).unwrap_or(0) as i128
        - left.and_then(|s| s.duration_ns).unwrap_or(0) as i128;
    let token_delta = right.map(folded_total_tokens).unwrap_or(0) as i128
        - left.map(folded_total_tokens).unwrap_or(0) as i128;
    let cost_delta = right.map(folded_cost_usd_nanos).unwrap_or(0) as i128
        - left.map(folded_cost_usd_nanos).unwrap_or(0) as i128;
    format!(
        r#"{{"index":{},"status":"{}","changes":[{}],"left":{},"right":{},"delta":{{"durationNs":{},"totalTokens":{},"costUsdNanos":{},"costUsd":{}}}}}"#,
        index,
        status,
        changes_json,
        left.map(trace_diff_span_json)
            .unwrap_or_else(|| "null".to_string()),
        right
            .map(trace_diff_span_json)
            .unwrap_or_else(|| "null".to_string()),
        duration_delta,
        token_delta,
        cost_delta,
        format!("{:.6}", cost_delta as f64 / 1_000_000_000.0),
    )
}

fn trace_diff_span_json(s: &FoldedSpan) -> String {
    format!(
        r#"{{"traceId":"{}","spanId":"{}","externalTraceId":{},"externalSpanId":{},"kind":"{}","name":"{}","status":{},"statusText":"{}","durationNs":{},"usage":{},"costUsd":{},"costDetail":{},"evalScore":{},"evalLabel":{},"agentName":{},"toolName":{},"model":{},"provider":{},"inputPreview":{},"outputPreview":{},"fields":{}}}"#,
        s.trace_id,
        s.span_id,
        json_opt_str(s.external_trace_id.as_deref()),
        json_opt_str(s.external_span_id.as_deref()),
        folded_kind(s),
        json_escape(&folded_name(s)),
        s.status
            .map_or("null".to_string(), |status| status.to_string()),
        if s.status.unwrap_or(0) == 0 {
            "ok"
        } else {
            "error"
        },
        s.duration_ns.map_or("null".to_string(), |d| d.to_string()),
        folded_usage_json(s),
        cost_usd_num_from_nanos(folded_cost_usd_nanos(s)),
        cost_detail_json(
            folded_cost_usd_nanos(s),
            s.cost_currency.as_deref(),
            if s.cost_usd_nanos.is_some() {
                "explicit"
            } else {
                "estimated"
            },
        ),
        s.eval_score
            .map_or("null".to_string(), |score| score.to_string()),
        json_opt_str(s.eval_label.as_deref()),
        json_opt_str(s.agent_name.as_deref()),
        json_opt_str(s.tool_name.as_deref()),
        json_opt_str(s.model.as_deref()),
        json_opt_str(s.provider.as_deref()),
        json_opt_preview(s.input_text.as_deref()),
        json_opt_preview(s.output_text.as_deref()),
        json_folded_agent_fields(s),
    )
}

fn trace_diff_step_changes(left: Option<&FoldedSpan>, right: Option<&FoldedSpan>) -> Vec<String> {
    let (Some(left), Some(right)) = (left, right) else {
        return Vec::new();
    };
    let mut changes = Vec::new();
    if folded_kind(left) != folded_kind(right) {
        changes.push("kind".to_string());
    }
    if folded_name(left) != folded_name(right) {
        changes.push("name".to_string());
    }
    if left.status != right.status {
        changes.push("status".to_string());
    }
    if left.agent_name != right.agent_name {
        changes.push("agentName".to_string());
    }
    if left.tool_name != right.tool_name {
        changes.push("toolName".to_string());
    }
    if left.model != right.model {
        changes.push("model".to_string());
    }
    for key in [
        "skill",
        "mode",
        "call_site",
        "task_fingerprint",
        "loop_id",
        "phase",
        "validation_status",
        "stop_reason",
        "validator",
    ] {
        if crate::folded_span_attr_value(left, key) != crate::folded_span_attr_value(right, key) {
            changes.push(key.to_string());
        }
    }
    if left.duration_ns != right.duration_ns {
        changes.push("durationNs".to_string());
    }
    if folded_total_tokens(left) != folded_total_tokens(right) {
        changes.push("totalTokens".to_string());
    }
    if folded_cost_usd_nanos(left) != folded_cost_usd_nanos(right) {
        changes.push("costUsd".to_string());
    }
    if left.eval_score != right.eval_score {
        changes.push("evalScore".to_string());
    }
    if left.eval_label != right.eval_label {
        changes.push("evalLabel".to_string());
    }
    if left.output_text != right.output_text {
        changes.push("outputText".to_string());
    }
    changes
}

fn product_query_parts(query: &str, default_limit: usize) -> ProductQueryParts {
    let mut cursor = 0usize;
    let mut limit = default_limit.clamp(1, 500);
    let mut filter = String::new();
    let mut attrs = std::collections::BTreeMap::new();
    let pairs = query_pairs(query);
    for (k, v) in &pairs {
        match k.as_str() {
            "cursor" | "offset" => cursor = v.parse().unwrap_or(0),
            "limit" | "k" => limit = v.parse().unwrap_or(default_limit).clamp(1, 500),
            "filter" | "q" | "text" => filter = v.clone(),
            "attrs" => collect_attr_query_json(v, &mut attrs),
            _ => collect_attr_query_pair(k, v, &mut attrs),
        }
    }
    ProductQueryParts {
        cursor,
        limit,
        filter,
        attrs,
        annotation: trace_search_annotation_spec_from_query(&pairs),
        dataset: trace_search_dataset_spec_from_query(&pairs),
    }
}

fn loop_summary_buckets(spans: &[FoldedSpan]) -> Vec<LoopSummaryBucket> {
    let mut by_loop: std::collections::BTreeMap<String, LoopSummaryBucket> =
        std::collections::BTreeMap::new();
    for s in spans {
        let Some(loop_value) = crate::folded_span_attr_value(s, "loop_id") else {
            continue;
        };
        let bucket = by_loop
            .entry(loop_value.to_string())
            .or_insert_with(|| LoopSummaryBucket::new(loop_value.to_string()));
        bucket.span_count += 1;
        bucket.trace_ids.insert(s.trace_id);
        if let Some(session_id) = s.session_id {
            bucket.session_ids.insert(session_id);
        }
        if s.status.unwrap_or(0) != 0 {
            bucket.error_count += 1;
        }
        if let Some(duration) = s.duration_ns {
            bucket.duration_sum_ns += duration as u128;
            bucket.duration_max_ns = bucket.duration_max_ns.max(duration);
            bucket.durations_ns.push(duration);
        }
        bucket.input_tokens += s.input_tokens.unwrap_or(0);
        bucket.output_tokens += s.output_tokens.unwrap_or(0);
        bucket.cached_input_tokens += s.cached_input_tokens.unwrap_or(0);
        bucket.reasoning_tokens += s.reasoning_tokens.unwrap_or(0);
        bucket.total_tokens += folded_total_tokens(s);
        bucket.cost_usd_nanos += folded_cost_usd_nanos(s);
        bucket.first_trace_id = bucket.first_trace_id.min(s.trace_id);
        bucket.last_trace_id = bucket.last_trace_id.max(s.trace_id);
        collect_agent_fields_from_span(s, &mut bucket.fields);
        if let Some(phase) = crate::folded_span_attr_value(s, "phase") {
            bucket.phases.insert(json_compact_label(phase));
        }
        if let Some(validator) = crate::folded_span_attr_value(s, "validator") {
            bucket.validators.insert(json_compact_label(validator));
        }
        if bucket.examples.len() < 3 {
            bucket.examples.push(TraceAggregateExample {
                trace_id: s.trace_id,
                span_id: s.span_id,
                external_trace_id: s.external_trace_id.clone(),
                external_span_id: s.external_span_id.clone(),
                name: folded_name(s),
            });
        }
    }
    by_loop.into_values().collect()
}

fn collect_agent_fields_from_span(
    s: &FoldedSpan,
    fields: &mut std::collections::BTreeMap<String, String>,
) {
    for key in agent_field_keys() {
        if let Some(value) = crate::folded_span_attr_value(s, key) {
            fields
                .entry((*key).to_string())
                .or_insert_with(|| first_class_agent_field_json(key, value));
        }
    }
}

fn trace_summary_buckets_from_spans(spans: &[FoldedSpan]) -> Vec<TaskTraceSummaryBucket> {
    let mut by_trace: std::collections::BTreeMap<u64, TaskTraceSummaryBucket> =
        std::collections::BTreeMap::new();
    for s in spans {
        let bucket = by_trace
            .entry(s.trace_id)
            .or_insert_with(|| TaskTraceSummaryBucket::new(s));
        if bucket.external_trace_id.is_none() {
            bucket.external_trace_id = s.external_trace_id.clone();
        }
        bucket.span_count += 1;
        if s.status.unwrap_or(0) != 0 {
            bucket.error_count += 1;
        }
        if let Some(duration) = s.duration_ns {
            bucket.duration_sum_ns += duration as u128;
            bucket.duration_max_ns = bucket.duration_max_ns.max(duration);
        }
        bucket.input_tokens += s.input_tokens.unwrap_or(0);
        bucket.output_tokens += s.output_tokens.unwrap_or(0);
        bucket.cached_input_tokens += s.cached_input_tokens.unwrap_or(0);
        bucket.reasoning_tokens += s.reasoning_tokens.unwrap_or(0);
        bucket.total_tokens += folded_total_tokens(s);
        bucket.cost_usd_nanos += folded_cost_usd_nanos(s);
        collect_agent_fields_from_span(s, &mut bucket.fields);
    }
    by_trace.into_values().collect()
}

fn json_loop_summary_bucket(bucket: &LoopSummaryBucket) -> String {
    let error_rate = if bucket.span_count == 0 {
        0.0
    } else {
        bucket.error_count as f64 / bucket.span_count as f64
    };
    let phases = bucket
        .phases
        .iter()
        .map(|v| json_string_value(v))
        .collect::<Vec<_>>()
        .join(",");
    let validators = bucket
        .validators
        .iter()
        .map(|v| json_string_value(v))
        .collect::<Vec<_>>()
        .join(",");
    let examples = bucket
        .examples
        .iter()
        .map(trace_aggregate_example_json)
        .collect::<Vec<_>>()
        .join(",");
    let task = bucket
        .fields
        .get("task_fingerprint")
        .map(|v| json_string_value(&json_compact_label(v)))
        .unwrap_or_else(|| "null".to_string());
    format!(
        r#"{{"loopId":"{}","loopValue":{},"taskFingerprint":{},"status":"{}","spanCount":{},"traceCount":{},"sessionCount":{},"errorCount":{},"errorRate":{:.6},"firstTraceId":"{}","lastTraceId":"{}","durationNs":{},"usage":{},"costUsd":{},"costDetail":{},"phases":[{}],"validators":[{}],"fields":{},"examples":[{}]}}"#,
        json_escape(&bucket.loop_id),
        bucket.loop_value_json,
        task,
        if bucket.error_count > 0 {
            "error"
        } else {
            "ok"
        },
        bucket.span_count,
        bucket.trace_ids.len(),
        bucket.session_ids.len(),
        bucket.error_count,
        error_rate,
        if bucket.first_trace_id == u64::MAX {
            0
        } else {
            bucket.first_trace_id
        },
        bucket.last_trace_id,
        loop_duration_json(bucket),
        usage_json(
            bucket.input_tokens,
            bucket.output_tokens,
            bucket.cached_input_tokens,
            bucket.reasoning_tokens,
            bucket.total_tokens,
        ),
        cost_usd_num_from_nanos(bucket.cost_usd_nanos),
        cost_detail_json(bucket.cost_usd_nanos, Some("USD"), "mixed"),
        phases,
        validators,
        json_attrs(&bucket.fields),
        examples,
    )
}

fn loop_duration_json(bucket: &LoopSummaryBucket) -> String {
    let mut durations = bucket.durations_ns.clone();
    durations.sort_unstable();
    let count = durations.len();
    let avg = if count == 0 {
        "null".to_string()
    } else {
        (bucket.duration_sum_ns / count as u128).to_string()
    };
    let max = if count == 0 {
        "null".to_string()
    } else {
        bucket.duration_max_ns.to_string()
    };
    format!(
        r#"{{"sum":{},"avg":{},"max":{},"p50":{},"p95":{},"count":{}}}"#,
        bucket.duration_sum_ns,
        avg,
        max,
        percentile_json(&durations, 50),
        percentile_json(&durations, 95),
        count,
    )
}

fn json_task_trace_summary_bucket(bucket: &TaskTraceSummaryBucket) -> String {
    format!(
        r#"{{"traceId":"{}","externalTraceId":{},"spanCount":{},"errorCount":{},"status":"{}","durationNs":{{"sum":{},"max":{}}},"usage":{},"costUsd":{},"costDetail":{},"fields":{}}}"#,
        bucket.trace_id,
        json_opt_str(bucket.external_trace_id.as_deref()),
        bucket.span_count,
        bucket.error_count,
        if bucket.error_count > 0 {
            "error"
        } else {
            "ok"
        },
        bucket.duration_sum_ns,
        bucket.duration_max_ns,
        usage_json(
            bucket.input_tokens,
            bucket.output_tokens,
            bucket.cached_input_tokens,
            bucket.reasoning_tokens,
            bucket.total_tokens,
        ),
        cost_usd_num_from_nanos(bucket.cost_usd_nanos),
        cost_detail_json(bucket.cost_usd_nanos, Some("USD"), "mixed"),
        json_attrs(&bucket.fields),
    )
}

fn loop_span_contains(s: &FoldedSpan, needle: &str) -> bool {
    folded_contains(s, needle)
        || crate::folded_span_attr_value(s, "loop_id")
            .map(|v| json_compact_label(v).contains(needle))
            .unwrap_or(false)
        || crate::folded_span_attr_value(s, "task_fingerprint")
            .map(|v| json_compact_label(v).contains(needle))
            .unwrap_or(false)
}

fn json_compact_label(value: &str) -> String {
    match crate::wire::parse(value) {
        Ok(crate::wire::Json::Str(s)) => s,
        Ok(crate::wire::Json::Num(s)) => s,
        Ok(crate::wire::Json::Bool(v)) => v.to_string(),
        Ok(crate::wire::Json::Null) => "null".to_string(),
        Ok(other) => other.to_compact_json(),
        Err(_) => value.to_string(),
    }
}

fn json_trace_search_span(s: &FoldedSpan, rank: usize) -> String {
    let kind = folded_kind(s);
    let name = folded_name(s);
    let logs = s
        .logs
        .iter()
        .take(5)
        .map(|log| json_string_value(log))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"{{"rank":{},"traceId":"{}","spanId":"{}","sessionId":{},"externalTraceId":{},"externalSpanId":{},"externalSessionId":{},"kind":"{}","name":"{}","status":{},"statusText":"{}","durationNs":{},"durMs":{},"cost":{},"costUsd":{},"costDetail":{},"usage":{},"inputTokens":{},"outputTokens":{},"agentName":{},"toolName":{},"model":{},"provider":{},"inputText":{},"outputText":{},"logsPreview":[{}],"fields":{},"attrs":{}}}"#,
        rank,
        s.trace_id,
        s.span_id,
        s.session_id
            .map_or("null".to_string(), |id| format!("\"{id}\"")),
        json_opt_str(s.external_trace_id.as_deref()),
        json_opt_str(s.external_span_id.as_deref()),
        json_opt_str(s.external_session_id.as_deref()),
        kind,
        json_escape(&name),
        s.status
            .map_or("null".to_string(), |status| status.to_string()),
        if s.status.unwrap_or(0) == 0 {
            "ok"
        } else {
            "error"
        },
        s.duration_ns.map_or("null".to_string(), |d| d.to_string()),
        s.duration_ns
            .map_or("null".to_string(), |d| (d / 1_000_000).to_string()),
        cost_num(s.input_tokens.unwrap_or(0), s.output_tokens.unwrap_or(0)),
        cost_usd_num_from_nanos(folded_cost_usd_nanos(s)),
        cost_detail_json(
            folded_cost_usd_nanos(s),
            s.cost_currency.as_deref(),
            if s.cost_usd_nanos.is_some() {
                "explicit"
            } else {
                "estimated"
            },
        ),
        folded_usage_json(s),
        s.input_tokens.unwrap_or(0),
        s.output_tokens.unwrap_or(0),
        json_opt_str(s.agent_name.as_deref()),
        json_opt_str(s.tool_name.as_deref()),
        json_opt_str(s.model.as_deref()),
        json_opt_str(s.provider.as_deref()),
        json_text_field(s.input_text.as_deref(), false),
        json_text_field(s.output_text.as_deref(), false),
        logs,
        json_folded_agent_fields(s),
        json_attrs(&s.attrs),
    )
}

fn span_order(spans: &[crate::ConsoleSpan]) -> std::collections::HashMap<u64, (usize, usize)> {
    let mut out = std::collections::HashMap::new();
    let mut sibling_counts: std::collections::BTreeMap<Option<u64>, usize> =
        std::collections::BTreeMap::new();
    for (idx, span) in spans.iter().enumerate() {
        let sibling = sibling_counts.entry(span.parent_span_id).or_insert(0);
        out.insert(span.span_id, (idx, *sibling));
        *sibling += 1;
    }
    out
}

fn trace_summary_json(tid: u64, spans: &[crate::ConsoleSpan]) -> String {
    let total_duration_ns: u64 = spans.iter().map(|s| s.duration_ns).sum();
    let input_tokens: u64 = spans.iter().map(|s| s.input_tokens).sum();
    let output_tokens: u64 = spans.iter().map(|s| s.output_tokens).sum();
    let cached_input_tokens: u64 = spans.iter().map(|s| s.cached_input_tokens).sum();
    let reasoning_tokens: u64 = spans.iter().map(|s| s.reasoning_tokens).sum();
    let total_tokens: u64 = spans.iter().map(|s| s.total_tokens).sum();
    let cost_usd_nanos: u64 = spans.iter().map(|s| s.cost_usd_nanos).sum();
    let any_err = spans.iter().any(|s| s.has_error);
    let name = spans.first().map(|s| s.name.clone()).unwrap_or_default();
    format!(
        r#"{{"traceId":"{}","externalTraceId":{},"name":"{}","durationNs":{},"durMs":{},"cost":{},"costUsd":{},"costDetail":{},"spanCount":{},"status":"{}","usage":{}}}"#,
        tid,
        json_opt_str(spans.iter().find_map(|s| s.external_trace_id.as_deref())),
        json_escape(&name),
        total_duration_ns,
        total_duration_ns / 1_000_000,
        cost_num(input_tokens, output_tokens),
        cost_usd_num_from_nanos(cost_usd_nanos),
        cost_detail_json(cost_usd_nanos, Some("USD"), "mixed"),
        spans.len(),
        if any_err { "error" } else { "ok" },
        usage_json(
            input_tokens,
            output_tokens,
            cached_input_tokens,
            reasoning_tokens,
            total_tokens,
        ),
    )
}

fn json_console_span_export(
    trace_id: u64,
    s: &crate::ConsoleSpan,
    span_ordinal: usize,
    sibling_ordinal: usize,
    events: &[crate::SpanLogEvent],
    include_full: bool,
) -> String {
    format!(
        r#"{{"traceId":"{}","id":"{}","spanId":"{}","parentId":{},"externalTraceId":{},"externalSpanId":{},"externalParentSpanId":{},"externalSessionId":{},"kind":"{}","name":"{}","spanOrdinal":{},"siblingOrdinal":{},"sortKey":"{:020}:{:020}","status":"{}","durationNs":{},"durMs":{},"cost":{},"costUsd":{},"costDetail":{},"usage":{},"model":{},"provider":{},"inputText":{},"outputText":{},"fields":{},"attrs":{},"logEvents":{}}}"#,
        trace_id,
        s.span_id,
        s.span_id,
        s.parent_span_id
            .map_or("null".to_string(), |p| format!("\"{p}\"")),
        json_opt_str(s.external_trace_id.as_deref()),
        json_opt_str(s.external_span_id.as_deref()),
        json_opt_str(s.external_parent_span_id.as_deref()),
        json_opt_str(s.external_session_id.as_deref()),
        s.kind,
        json_escape(&s.name),
        span_ordinal,
        sibling_ordinal,
        span_ordinal,
        s.span_id,
        if s.has_error { "error" } else { "ok" },
        s.duration_ns,
        s.duration_ns / 1_000_000,
        cost_num(s.input_tokens, s.output_tokens),
        cost_usd_num_from_nanos(s.cost_usd_nanos),
        cost_detail_json(s.cost_usd_nanos, s.cost_currency.as_deref(), "mixed"),
        console_usage_json(s),
        json_opt_str(s.model.as_deref()),
        json_opt_str(s.provider.as_deref()),
        json_text_field(s.input_text.as_deref(), include_full),
        json_text_field(s.output_text.as_deref(), include_full),
        json_console_agent_fields(s),
        json_attrs(&s.attrs),
        json_log_events(events),
    )
}

fn json_text_field(text: Option<&str>, include_full: bool) -> String {
    let Some(text) = text else {
        return "null".to_string();
    };
    let (preview, truncated) = preview_text(text, 280);
    let hash = yt_core::event::fnv1a64(text.as_bytes());
    format!(
        r#"{{"preview":"{}","full":{},"contentHash":"fnv1a64:{:016x}","byteLength":{},"truncated":{},"blobRef":null}}"#,
        json_escape(&preview),
        if include_full {
            json_string_value(text)
        } else {
            "null".to_string()
        },
        hash,
        text.len(),
        truncated,
    )
}

fn preview_text(text: &str, max_chars: usize) -> (String, bool) {
    if text.chars().count() <= max_chars {
        return (text.to_string(), false);
    }
    (text.chars().take(max_chars).collect(), true)
}

fn span_page_query(query: &str) -> (usize, usize, bool) {
    let mut cursor = 0usize;
    let mut limit = 50usize;
    let mut include_full = false;
    for kv in query.split('&') {
        if let Some((k, v)) = kv.split_once('=') {
            match k {
                "cursor" | "offset" => cursor = v.parse().unwrap_or(0),
                "limit" => limit = v.parse::<usize>().unwrap_or(50).clamp(1, 500),
                "includeFull" | "include_full" | "full" => {
                    include_full = matches!(url_decode(v).as_str(), "1" | "true" | "yes")
                }
                _ => {}
            }
        }
    }
    (cursor, limit, include_full)
}

fn json_truthy(v: &crate::wire::Json) -> bool {
    match v {
        crate::wire::Json::Bool(b) => *b,
        crate::wire::Json::Num(n) | crate::wire::Json::Str(n) => {
            matches!(n.as_str(), "1" | "true" | "yes")
        }
        _ => false,
    }
}

fn parse_json_body_or_empty(body: &str) -> Result<crate::wire::Json, String> {
    if body.trim().is_empty() {
        Ok(crate::wire::Json::Obj(Vec::new()))
    } else {
        crate::wire::parse(body)
    }
}

fn unix_now_ns() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
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

fn cost_usd_num_from_nanos(nanos: u64) -> String {
    format!("{:.6}", nanos as f64 / 1_000_000_000.0)
}

fn cost_detail_json(nanos: u64, currency: Option<&str>, source: &str) -> String {
    format!(
        r#"{{"costUsd":{},"costUsdNanos":{},"currency":"{}","source":"{}"}}"#,
        cost_usd_num_from_nanos(nanos),
        nanos,
        json_escape(currency.unwrap_or("USD")),
        source,
    )
}

fn folded_cost_usd_nanos(s: &FoldedSpan) -> u64 {
    crate::usage_cost_usd_nanos(
        s.input_tokens.unwrap_or(0),
        s.output_tokens.unwrap_or(0),
        s.cached_input_tokens.unwrap_or(0),
        s.reasoning_tokens.unwrap_or(0),
        s.cost_usd_nanos,
    )
}

fn folded_total_tokens(s: &FoldedSpan) -> u64 {
    crate::usage_total_tokens(
        s.input_tokens.unwrap_or(0),
        s.output_tokens.unwrap_or(0),
        s.cached_input_tokens.unwrap_or(0),
        s.reasoning_tokens.unwrap_or(0),
        s.total_tokens,
    )
}

fn folded_usage_json(s: &FoldedSpan) -> String {
    format!(
        r#"{{"inputTokens":{},"outputTokens":{},"cachedInputTokens":{},"reasoningTokens":{},"totalTokens":{}}}"#,
        s.input_tokens.unwrap_or(0),
        s.output_tokens.unwrap_or(0),
        s.cached_input_tokens.unwrap_or(0),
        s.reasoning_tokens.unwrap_or(0),
        folded_total_tokens(s),
    )
}

fn console_usage_json(s: &crate::ConsoleSpan) -> String {
    format!(
        r#"{{"inputTokens":{},"outputTokens":{},"cachedInputTokens":{},"reasoningTokens":{},"totalTokens":{}}}"#,
        s.input_tokens, s.output_tokens, s.cached_input_tokens, s.reasoning_tokens, s.total_tokens,
    )
}

fn usage_json(
    input_tokens: u64,
    output_tokens: u64,
    cached_input_tokens: u64,
    reasoning_tokens: u64,
    total_tokens: u64,
) -> String {
    format!(
        r#"{{"inputTokens":{},"outputTokens":{},"cachedInputTokens":{},"reasoningTokens":{},"totalTokens":{}}}"#,
        input_tokens, output_tokens, cached_input_tokens, reasoning_tokens, total_tokens
    )
}

/// 兼容旧字段：输入 8e-7、输出 4e-6 每 token。新代码优先使用 `costUsd`/`costDetail`。
fn cost_num(in_tok: u64, out_tok: u64) -> String {
    format!("{:.3}", in_tok as f64 * 8e-7 + out_tok as f64 * 4e-6)
}

fn parse_id_or_hash(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        None
    } else {
        Some(
            s.parse::<u64>()
                .unwrap_or_else(|_| yt_core::event::fnv1a64(s.as_bytes())),
        )
    }
}

fn json_id_or_hash(v: &crate::wire::Json) -> Option<u64> {
    v.as_u64()
        .or_else(|| v.as_str().map(|s| yt_core::event::fnv1a64(s.as_bytes())))
}

fn json_id_with_external(v: &crate::wire::Json) -> Option<(u64, Option<String>)> {
    match v {
        crate::wire::Json::Num(s) => s.parse::<u64>().ok().map(|id| (id, None)),
        crate::wire::Json::Str(s) => match s.parse::<u64>() {
            Ok(id) => Some((id, None)),
            Err(_) => Some((yt_core::event::fnv1a64(s.as_bytes()), Some(s.clone()))),
        },
        _ => None,
    }
}

fn json_internal_id(v: &crate::wire::Json) -> Option<u64> {
    v.as_u64().or_else(|| v.as_str().and_then(parse_id_or_hash))
}

fn golden_path_filter_from_json(
    f: &crate::wire::Json,
    tenant: Option<u64>,
) -> Result<(GoldenPathFilter, bool), String> {
    let mut filter = GoldenPathFilter {
        tenant_id: tenant,
        ..Default::default()
    };
    filter.golden_path_id =
        json_field_alias(f, &["golden_path_id", "goldenPathId", "id"]).and_then(json_internal_id);
    filter.task_fingerprint = json_field_alias(
        f,
        &["task_fingerprint", "taskFingerprint", "task", "taskId"],
    )
    .and_then(crate::wire::Json::as_str)
    .map(ToString::to_string);
    filter.trajectory_signature = json_field_alias(
        f,
        &[
            "trajectory_signature",
            "trajectorySignature",
            "signature",
            "pathSignature",
        ],
    )
    .and_then(crate::wire::Json::as_str)
    .map(ToString::to_string);
    filter.source_trace_id = json_field_alias(
        f,
        &["source_trace_id", "sourceTraceId", "trace_id", "traceId"],
    )
    .and_then(json_id_with_external)
    .map(|(id, _)| id);
    let status_value = json_field_alias(f, &["status"]);
    let explicit_status = status_value.is_some();
    if let Some(value) = status_value {
        let Some(status) = value.as_str().and_then(GoldenPathStatus::parse) else {
            return Err("bad status".to_string());
        };
        filter.status = Some(status);
    }
    collect_attr_map(f, &mut filter.attrs);
    for key in ["model", "provider"] {
        if let Some(value) = crate::wire::field(f, key).and_then(crate::wire::Json::as_str) {
            filter
                .attrs
                .insert(key.to_string(), json_string_value(value));
        }
    }
    Ok((filter, explicit_status))
}

fn json_opt_str(s: Option<&str>) -> String {
    s.map_or("null".to_string(), |v| format!("\"{}\"", json_escape(v)))
}

fn json_opt_u64_string(v: Option<u64>) -> String {
    v.map_or("null".to_string(), |id| format!("\"{id}\""))
}

fn json_attrs(attrs: &std::collections::BTreeMap<String, String>) -> String {
    if attrs.is_empty() {
        return "{}".to_string();
    }
    format!(
        "{{{}}}",
        attrs
            .iter()
            .map(|(k, v)| format!("\"{}\":{}", json_escape(k), v))
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn json_annotation(a: &crate::TraceAnnotation) -> String {
    format!(
        r#"{{"annotationId":"{}","tenantId":{},"target":"{}","traceId":"{}","spanId":{},"externalTraceId":{},"externalSpanId":{},"label":"{}","score":{},"reason":{},"source":{},"createdAtNs":"{}","attrs":{}}}"#,
        a.annotation_id,
        json_opt_u64_string(a.tenant_id),
        a.target.as_str(),
        a.trace_id,
        json_opt_u64_string(a.span_id),
        json_opt_str(a.external_trace_id.as_deref()),
        json_opt_str(a.external_span_id.as_deref()),
        json_escape(&a.label),
        a.score.map_or("null".to_string(), |s| s.to_string()),
        json_opt_str(a.reason.as_deref()),
        json_opt_str(a.source.as_deref()),
        a.created_at_ns,
        json_attrs(&a.attrs),
    )
}

fn json_dataset_association(a: &crate::DatasetAssociation) -> String {
    format!(
        r#"{{"associationId":"{}","tenantId":{},"datasetId":"{}","itemId":"{}","traceId":"{}","spanId":{},"externalTraceId":{},"externalSpanId":{},"snapshotId":{},"snapshotHash":{},"evalRunId":{},"split":{},"label":{},"score":{},"createdAtNs":"{}","attrs":{}}}"#,
        a.association_id,
        json_opt_u64_string(a.tenant_id),
        json_escape(&a.dataset_id),
        json_escape(&a.item_id),
        a.trace_id,
        json_opt_u64_string(a.span_id),
        json_opt_str(a.external_trace_id.as_deref()),
        json_opt_str(a.external_span_id.as_deref()),
        json_opt_str(a.snapshot_id.as_deref()),
        json_opt_str(a.snapshot_hash.as_deref()),
        json_opt_str(a.eval_run_id.as_deref()),
        json_opt_str(a.split.as_deref()),
        json_opt_str(a.label.as_deref()),
        a.score.map_or("null".to_string(), |s| s.to_string()),
        a.created_at_ns,
        json_attrs(&a.attrs),
    )
}

fn json_golden_path(g: &crate::GoldenPathCandidate) -> String {
    format!(
        r#"{{"goldenPathId":"{}","tenantId":{},"taskFingerprint":"{}","trajectorySignature":"{}","sourceTraceId":"{}","externalSourceTraceId":{},"snapshotId":{},"snapshotHash":{},"status":"{}","score":{},"label":{},"reason":{},"source":{},"createdAtNs":"{}","updatedAtNs":"{}","attrs":{},"sourceTrajectory":{},"evidenceSummary":{}}}"#,
        g.golden_path_id,
        json_opt_u64_string(g.tenant_id),
        json_escape(&g.task_fingerprint),
        json_escape(&g.trajectory_signature),
        g.source_trace_id,
        json_opt_str(g.external_source_trace_id.as_deref()),
        json_opt_str(g.snapshot_id.as_deref()),
        json_opt_str(g.snapshot_hash.as_deref()),
        g.status.as_str(),
        g.score.map_or("null".to_string(), |s| s.to_string()),
        json_opt_str(g.label.as_deref()),
        json_opt_str(g.reason.as_deref()),
        json_opt_str(g.source.as_deref()),
        g.created_at_ns,
        g.updated_at_ns,
        json_attrs(&g.attrs),
        trajectory_summary_json_with_signature(&g.source_trajectory_steps, &g.trajectory_signature),
        json_attrs(&g.evidence),
    )
}

fn json_path_adherence(
    golden_path: &crate::GoldenPathCandidate,
    trace_id: u64,
    trace_spans: &[FoldedSpan],
    source_spans: &[FoldedSpan],
) -> String {
    let facts = path_adherence_facts(golden_path, trace_spans, source_spans);
    let golden_coverage = ratio_json(facts.common_steps.len(), facts.source_steps.len());
    let trace_coverage = ratio_json(facts.common_steps.len(), facts.trace_steps.len());
    let trace_summary = trace_summary_buckets_from_spans(trace_spans);
    format!(
        r#"{{"goldenPath":{},"trace":{},"adherence":"{}","sameSignature":{},"sourceAvailable":{},"sourceRetained":{},"storedSignatureMatchesSource":{},"goldenTrajectory":{},"sourceTrajectory":{},"traceTrajectory":{},"scores":{{"commonStepCount":{},"goldenStepCount":{},"traceStepCount":{},"goldenCoverage":{},"traceCoverage":{}}},"commonSteps":{},"missingSteps":{},"extraSteps":{}}}"#,
        json_golden_path(golden_path),
        trace_diff_side_json(trace_id, trace_summary.first()),
        facts.adherence(),
        json_bool(facts.same_signature),
        json_bool(facts.source_available),
        json_bool(facts.source_retained),
        json_opt_bool(facts.stored_signature_matches_source),
        trajectory_summary_json_with_signature(
            &facts.source_steps,
            &golden_path.trajectory_signature
        ),
        facts
            .source_signature
            .as_ref()
            .map(|signature| trajectory_summary_json_with_signature(&facts.source_steps, signature))
            .unwrap_or_else(|| "null".to_string()),
        trajectory_summary_json_with_signature(&facts.trace_steps, &facts.trace_signature),
        facts.common_steps.len(),
        facts.source_steps.len(),
        facts.trace_steps.len(),
        golden_coverage,
        trace_coverage,
        json_string_array(&facts.common_steps),
        json_string_array(&facts.missing_steps),
        json_string_array(&facts.extra_steps),
    )
}

fn path_adherence_facts(
    golden_path: &crate::GoldenPathCandidate,
    trace_spans: &[FoldedSpan],
    source_spans: &[FoldedSpan],
) -> PathAdherenceFacts {
    path_adherence_facts_from_steps(golden_path, trajectory_steps(trace_spans), source_spans)
}

fn path_adherence_facts_from_steps(
    golden_path: &crate::GoldenPathCandidate,
    trace_steps: Vec<String>,
    source_spans: &[FoldedSpan],
) -> PathAdherenceFacts {
    let source_available = !source_spans.is_empty();
    let source_retained = !golden_path.source_trajectory_steps.is_empty();
    let source_steps = if source_available {
        trajectory_steps(source_spans)
    } else if source_retained {
        golden_path.source_trajectory_steps.clone()
    } else {
        Vec::new()
    };
    let source_signature =
        (!source_steps.is_empty()).then(|| trajectory_signature_string(&source_steps));
    let trace_signature = trajectory_signature_string(&trace_steps);
    let same_signature = trace_signature == golden_path.trajectory_signature;
    let stored_signature_matches_source = source_signature
        .as_ref()
        .map(|signature| signature == &golden_path.trajectory_signature);

    let (common_steps, missing_steps, extra_steps) = if source_available {
        ordered_step_diff(&source_steps, &trace_steps)
    } else if source_retained {
        ordered_step_diff(&source_steps, &trace_steps)
    } else {
        (Vec::new(), Vec::new(), Vec::new())
    };
    PathAdherenceFacts {
        source_available,
        source_retained,
        source_steps,
        source_signature,
        trace_steps,
        trace_signature,
        same_signature,
        stored_signature_matches_source,
        common_steps,
        missing_steps,
        extra_steps,
    }
}

fn path_adherence_health_example_json(
    summary: &crate::TraceTrajectorySummary,
    facts: &PathAdherenceFacts,
) -> String {
    let golden_coverage = ratio_json(facts.common_steps.len(), facts.source_steps.len());
    let trace_coverage = ratio_json(facts.common_steps.len(), facts.trace_steps.len());
    format!(
        r#"{{"trace":{},"adherence":"{}","sameSignature":{},"scores":{{"commonStepCount":{},"goldenStepCount":{},"traceStepCount":{},"goldenCoverage":{},"traceCoverage":{}}},"traceTrajectory":{}}}"#,
        trace_trajectory_side_json(summary),
        facts.adherence(),
        json_bool(facts.same_signature),
        facts.common_steps.len(),
        facts.source_steps.len(),
        facts.trace_steps.len(),
        golden_coverage,
        trace_coverage,
        trajectory_summary_json_with_signature(&facts.trace_steps, &facts.trace_signature),
    )
}

fn json_string_array(items: &[String]) -> String {
    format!(
        "[{}]",
        items
            .iter()
            .map(|s| json_string_value(s))
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn json_bool(value: bool) -> &'static str {
    if value {
        "true"
    } else {
        "false"
    }
}

fn json_opt_bool(value: Option<bool>) -> String {
    value.map(json_bool).unwrap_or("null").to_string()
}

fn ratio_json(numerator: usize, denominator: usize) -> String {
    if denominator == 0 {
        "null".to_string()
    } else {
        format!("{:.6}", numerator as f64 / denominator as f64)
    }
}

fn json_agent_fields(attrs: &std::collections::BTreeMap<String, String>) -> String {
    if attrs.is_empty() {
        return "{}".to_string();
    }
    let fields: Vec<String> = agent_field_keys()
        .iter()
        .filter_map(|key| {
            attrs
                .get(*key)
                .map(|value| format!("\"{}\":{}", json_escape(key), value))
        })
        .collect();
    if fields.is_empty() {
        "{}".to_string()
    } else {
        format!("{{{}}}", fields.join(","))
    }
}

fn json_folded_agent_fields(s: &FoldedSpan) -> String {
    json_agent_fields_with_lookup(&s.attrs, |key| crate::first_class_span_attr_value(s, key))
}

fn json_console_agent_fields(s: &crate::ConsoleSpan) -> String {
    json_agent_fields_with_lookup(&s.attrs, |key| {
        crate::first_class_console_attr_value(s, key)
    })
}

fn json_agent_fields_with_lookup<'a>(
    attrs: &'a std::collections::BTreeMap<String, String>,
    first_class: impl Fn(&str) -> Option<&'a str>,
) -> String {
    let fields: Vec<String> = agent_field_keys()
        .iter()
        .filter_map(|key| {
            if let Some(value) = first_class(key) {
                Some(format!(
                    "\"{}\":{}",
                    json_escape(key),
                    first_class_agent_field_json(key, value)
                ))
            } else {
                attrs
                    .get(*key)
                    .map(|value| format!("\"{}\":{}", json_escape(key), value))
            }
        })
        .collect();
    if fields.is_empty() {
        "{}".to_string()
    } else {
        format!("{{{}}}", fields.join(","))
    }
}

fn first_class_agent_field_json(key: &str, value: &str) -> String {
    match key {
        // model/provider are native string fields; the other promoted agentic
        // dimensions are attrs values and already use compact JSON.
        "model" | "provider" => json_string_value(value),
        _ => value.to_string(),
    }
}

fn json_log_events(events: &[crate::SpanLogEvent]) -> String {
    if events.is_empty() {
        return "[]".to_string();
    }
    format!(
        "[{}]",
        events
            .iter()
            .enumerate()
            .map(|(idx, ev)| {
                let messages = ev
                    .messages
                    .iter()
                    .map(|m| json_string_value(m))
                    .collect::<Vec<_>>()
                    .join(",");
                format!(
                    r#"{{"eventId":"{}","eventOrdinal":{},"sortKey":"{:020}:{:020}:{:020}","ts":{},"seq":{},"eventType":{},"messages":[{}],"attrs":{}}}"#,
                    ev.event_id,
                    idx,
                    ev.ts,
                    ev.seq,
                    ev.event_id,
                    ev.ts,
                    ev.seq,
                    ev.event_type,
                    messages,
                    json_attrs(&ev.attrs),
                )
            })
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn json_string_value(s: &str) -> String {
    format!("\"{}\"", json_escape(s))
}

fn json_opt_preview(value: Option<&str>) -> String {
    value
        .map(trunc)
        .map(|s| json_string_value(&s))
        .unwrap_or_else(|| "null".to_string())
}

fn collect_attr_filters(f: &crate::wire::Json, filter: &mut crate::SearchFilter) {
    collect_attr_map(f, &mut filter.attrs);
}

fn collect_golden_path_scope_attrs(
    spans: &[FoldedSpan],
    attrs: &mut std::collections::BTreeMap<String, String>,
) {
    for key in golden_path_scope_keys() {
        if attrs.contains_key(*key) {
            continue;
        }
        if let Some(value) = spans.iter().find_map(|s| golden_path_scope_value(s, key)) {
            attrs.insert((*key).to_string(), json_string_value(&value));
        }
    }
}

fn golden_path_scope_value(s: &FoldedSpan, key: &str) -> Option<String> {
    match key {
        "model" => s.model.clone(),
        "provider" => s.provider.clone(),
        _ => crate::folded_span_attr_value(s, key).map(json_compact_label),
    }
}

fn golden_path_scope_keys() -> &'static [&'static str] {
    &[
        "project_id",
        "task_fingerprint",
        "skill",
        "mode",
        "harness_version",
        "schema_fingerprint",
        "eval_profile",
        "model",
        "provider",
        "tool_version",
    ]
}

fn golden_path_evidence_summary_from_json(
    f: &crate::wire::Json,
    spans: &[FoldedSpan],
) -> std::collections::BTreeMap<String, String> {
    let mut evidence = std::collections::BTreeMap::new();
    if let Some(crate::wire::Json::Obj(kvs)) =
        json_field_alias(f, &["evidence", "evidenceSummary", "evidence_summary"])
    {
        for (k, v) in kvs {
            evidence.insert(k.clone(), v.to_compact_json());
        }
    }
    for (alias, key) in [
        ("eval_profile", "eval_profile"),
        ("evalProfile", "eval_profile"),
        ("sample_count", "sample_count"),
        ("sampleCount", "sample_count"),
        ("success_rate", "success_rate"),
        ("successRate", "success_rate"),
        ("avg_cost_usd_nanos", "avg_cost_usd_nanos"),
        ("avgCostUsdNanos", "avg_cost_usd_nanos"),
        ("p95_duration_ns", "p95_duration_ns"),
        ("p95DurationNs", "p95_duration_ns"),
    ] {
        if let Some(value) = crate::wire::field(f, alias) {
            evidence.insert(key.to_string(), value.to_compact_json());
        }
    }

    let summary = trace_summary_buckets_from_spans(spans);
    if let Some(bucket) = summary.first() {
        evidence
            .entry("source_span_count".to_string())
            .or_insert_with(|| bucket.span_count.to_string());
        evidence
            .entry("source_status".to_string())
            .or_insert_with(|| {
                json_string_value(if bucket.error_count > 0 {
                    "error"
                } else {
                    "ok"
                })
            });
        evidence
            .entry("source_duration_ns".to_string())
            .or_insert_with(|| bucket.duration_sum_ns.to_string());
        evidence
            .entry("source_total_tokens".to_string())
            .or_insert_with(|| bucket.total_tokens.to_string());
        evidence
            .entry("source_cost_usd_nanos".to_string())
            .or_insert_with(|| bucket.cost_usd_nanos.to_string());
    }
    let steps = trajectory_steps(spans);
    evidence
        .entry("source_trajectory_step_count".to_string())
        .or_insert_with(|| steps.len().to_string());
    if !steps.is_empty() {
        evidence
            .entry("source_trajectory_signature".to_string())
            .or_insert_with(|| json_string_value(&trajectory_signature_string(&steps)));
    }
    evidence
}

fn collect_attr_map(f: &crate::wire::Json, attrs: &mut std::collections::BTreeMap<String, String>) {
    use crate::wire::{field, Json};
    for (alias, key) in attr_aliases() {
        if let Some(v) = field(f, alias) {
            attrs.insert((*key).to_string(), v.to_compact_json());
        }
    }
    if let Some(Json::Obj(kvs)) = field(f, "attrs") {
        for (k, v) in kvs {
            attrs.insert(k.clone(), v.to_compact_json());
        }
    }
}

fn collect_attr_query_json(s: &str, attrs: &mut std::collections::BTreeMap<String, String>) {
    use crate::wire::Json;
    let Ok(Json::Obj(kvs)) = crate::wire::parse(s) else {
        return;
    };
    for (k, v) in kvs {
        attrs.insert(k, v.to_compact_json());
    }
}

fn collect_attr_query_pair(
    k: &str,
    v: &str,
    attrs: &mut std::collections::BTreeMap<String, String>,
) {
    if let Some((_, attr_key)) = attr_aliases().iter().find(|(alias, _)| *alias == k) {
        attrs.insert((*attr_key).to_string(), json_string_value(v));
    }
}

fn query_pairs(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            Some((url_decode(k), url_decode(v)))
        })
        .collect()
}

fn attr_aliases() -> &'static [(&'static str, &'static str)] {
    &[
        ("project_id", "project_id"),
        ("projectId", "project_id"),
        ("external_run_id", "external_run_id"),
        ("externalRunId", "external_run_id"),
        ("skill", "skill"),
        ("mode", "mode"),
        ("call_site", "call_site"),
        ("callSite", "call_site"),
        ("task_fingerprint", "task_fingerprint"),
        ("taskFingerprint", "task_fingerprint"),
        ("loop_id", "loop_id"),
        ("loopId", "loop_id"),
        ("harness_version", "harness_version"),
        ("harnessVersion", "harness_version"),
        ("validation_status", "validation_status"),
        ("validationStatus", "validation_status"),
        ("stop_reason", "stop_reason"),
        ("stopReason", "stop_reason"),
        ("phase", "phase"),
        ("validator", "validator"),
        ("connection_ids", "connection_ids"),
        ("connectionIds", "connection_ids"),
        ("data_source_ids", "data_source_ids"),
        ("dataSourceIds", "data_source_ids"),
        ("schema_fingerprint", "schema_fingerprint"),
        ("schemaFingerprint", "schema_fingerprint"),
        ("eval_profile", "eval_profile"),
        ("evalProfile", "eval_profile"),
        ("tool_version", "tool_version"),
        ("toolVersion", "tool_version"),
        ("intent_signature", "intent_signature"),
        ("intentSignature", "intent_signature"),
        ("review_status", "review_status"),
        ("reviewStatus", "review_status"),
        ("eval_status", "eval_status"),
        ("evalStatus", "eval_status"),
        ("path_memory_id", "path_memory_id"),
        ("pathMemoryId", "path_memory_id"),
    ]
}

fn agent_field_keys() -> &'static [&'static str] {
    &[
        "project_id",
        "session_id",
        "external_run_id",
        "skill",
        "mode",
        "call_site",
        "task_fingerprint",
        "loop_id",
        "harness_version",
        "validation_status",
        "stop_reason",
        "phase",
        "validator",
        "connection_ids",
        "data_source_ids",
        "schema_fingerprint",
        "eval_profile",
        "tool_version",
        "model",
        "provider",
        "intent_signature",
        "review_status",
        "eval_status",
        "path_memory_id",
    ]
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
        HttpIngestServer::new(WriteCoordinator::new(Arc::new(
            InMemorySegmentStore::default(),
        )))
    }

    const BATCH: &str = r#"[
      {"trace_id":7,"span_id":1,"ts":100,"seq":1,"event_type":1,"ext_span_id":"7-1","status":0,"input_tokens":900,"cached_input_tokens":100,"reasoning_tokens":20,"total_tokens":1170,"cost_usd":0.0025,"cost_currency":"USD","provider":"openai","logs":["开始"]},
      {"trace_id":7,"span_id":1,"ts":150,"seq":2,"event_type":2,"ext_span_id":"7-1","duration_ns":50,"output_tokens":150,"logs":["结束"]}
    ]"#;

    fn durable_temp_dir(name: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "yt_http_{name}_{}_{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

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
        assert!(body.contains("\"total_cached_input_tokens\":100"), "{body}");
        assert!(body.contains("\"total_reasoning_tokens\":20"), "{body}");
        assert!(body.contains("\"total_tokens\":1170"), "{body}");
        assert!(body.contains("\"total_cost_usd_nanos\":2500000"), "{body}");

        let (status, body) = s.route("GET", "/v1/traces/7", "");
        assert_eq!(status, 200, "{body}");
        assert!(body.contains(r#""provider":"openai""#), "{body}");
        assert!(body.contains(r#""cachedInputTokens":100"#), "{body}");
        assert!(body.contains(r#""reasoningTokens":20"#), "{body}");
        assert!(body.contains(r#""costUsd":0.002500"#), "{body}");
    }

    #[test]
    fn route_ingest_accepts_external_ids_and_attrs() {
        let s = server();
        let batch = r#"[{
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
        // 灌过数据 → committed_tail > 0。
        assert!(
            body.lines()
                .any(|l| l.starts_with("yt_wal_committed_tail ") && !l.ends_with(" 0")),
            "灌数据后 committed_tail 应 > 0:\n{body}"
        );
    }

    #[test]
    fn annotations_and_dataset_associations_are_tenant_isolated_and_durable() {
        let dir = durable_temp_dir("metadata");
        {
            let coord = WriteCoordinator::open_durable(&dir).unwrap();
            coord.recover();
            let api = EngineJsonApi::new(coord);
            let trace = r#"[{
              "trace_id":"run-uuid",
              "span_id":"span-uuid",
              "ts":100,
              "seq":1,
              "event_type":2,
              "ext_span_id":"span-uuid",
              "session_id":"session-uuid",
              "status":0,
              "duration_ns":42,
              "agent_name":"builder-agent",
              "input_text":"builder 输入",
              "output_text":"builder 输出",
              "attrs":{"project_id":"agentic-data","skill":"review"}
            }]"#;
            let (status, body) = api.route_with_tenant("POST", "/v1/ingest", trace, Some(1));
            assert_eq!(status, 200, "{body}");

            let annotation = r#"{
              "traceId":"run-uuid",
              "spanId":"span-uuid",
              "target":"span",
              "label":"best_path",
              "score":920,
              "reason":"人工确认这次路径最短",
              "source":"human",
              "projectId":"agentic-data",
              "skill":"review"
            }"#;
            let (status, body) =
                api.route_with_tenant("POST", "/v1/annotations", annotation, Some(1));
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""externalTraceId":"run-uuid""#), "{body}");
            assert!(body.contains(r#""externalSpanId":"span-uuid""#), "{body}");

            let link = r#"{
              "datasetId":"best-path-regression",
              "itemId":"case-1",
              "traceId":"run-uuid",
              "spanId":"span-uuid",
              "snapshotId":"snap-1",
              "snapshotHash":"fnv1a64:abc",
              "evalRunId":"eval-1",
              "split":"train",
              "label":"pass",
              "score":920,
              "projectId":"agentic-data",
              "skill":"review"
            }"#;
            let (status, body) =
                api.route_with_tenant("POST", "/v1/dataset-associations", link, Some(1));
            assert_eq!(status, 200, "{body}");
            assert!(
                body.contains(r#""datasetId":"best-path-regression""#),
                "{body}"
            );

            let other =
                r#"{"traceId":"run-uuid","label":"wrong_tenant","projectId":"agentic-data"}"#;
            assert_eq!(
                api.route_with_tenant("POST", "/v1/annotations", other, Some(2))
                    .0,
                200
            );

            let (status, body) = api.route_with_tenant(
                "GET",
                "/v1/annotations?traceId=run-uuid&projectId=agentic-data",
                "",
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""count":1"#), "{body}");
            assert!(body.contains(r#""label":"best_path""#), "{body}");
            assert!(!body.contains("wrong_tenant"), "{body}");

            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/trace-search",
                r#"{"filter":{"annotation":{"label":"best_path","source":"human","scoreMin":900}}}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""total":1"#), "{body}");
            assert!(body.contains(r#""externalSpanId":"span-uuid""#), "{body}");

            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/trace-search",
                r#"{"filter":{"dataset":{"datasetId":"best-path-regression","itemId":"case-1","evalRunId":"eval-1","scoreMin":900}}}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""total":1"#), "{body}");

            let (status, body) = api.route_with_tenant(
                "GET",
                "/v1/traces?annotationLabel=best_path&annotationScoreMin=900",
                "",
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""external_trace_id":"run-uuid""#), "{body}");

            let (status, body) = api.route_with_tenant(
                "GET",
                "/v1/sessions?datasetId=best-path-regression&datasetLabel=pass",
                "",
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(
                body.contains(r#""externalSessionId":"session-uuid""#),
                "{body}"
            );

            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/trace-search",
                r#"{"filter":{"annotationLabel":"missing"}}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""total":0"#), "{body}");
        }
        {
            let coord = WriteCoordinator::open_durable(&dir).unwrap();
            coord.recover();
            let api = EngineJsonApi::new(coord);
            let (status, body) = api.route_with_tenant(
                "GET",
                "/v1/annotations?traceId=run-uuid&label=best_path",
                "",
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""count":1"#), "{body}");

            let (status, body) = api.route_with_tenant(
                "GET",
                "/v1/dataset-associations?datasetId=best-path-regression&itemId=case-1",
                "",
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""count":1"#), "{body}");
            assert!(body.contains(r#""snapshotHash":"fnv1a64:abc""#), "{body}");
            assert!(body.contains(r#""project_id":"agentic-data""#), "{body}");

            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/trace-search",
                r#"{"filter":{"datasetId":"best-path-regression","datasetLabel":"pass"}}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""total":1"#), "{body}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn storage_stats_and_retention_plan_protect_metadata_before_apply() {
        let dir = durable_temp_dir("storage-retention");
        {
            let coord = WriteCoordinator::open_durable(&dir).unwrap();
            coord.recover();
            let api = EngineJsonApi::new(coord);
            let batch = r#"[
              {"trace_id":101,"span_id":1,"ts":10,"seq":1,"event_type":1,"ext_span_id":"101-1","input_text":"old delete","attrs":{"project_id":"retention-demo","task_fingerprint":"case-a"}},
              {"trace_id":101,"span_id":1,"ts":20,"seq":2,"event_type":2,"ext_span_id":"101-1","duration_ns":10,"output_text":"done"},
              {"trace_id":102,"span_id":1,"ts":30,"seq":1,"event_type":1,"ext_span_id":"102-1","input_text":"old protected","attrs":{"project_id":"retention-demo","task_fingerprint":"case-a"}},
              {"trace_id":102,"span_id":1,"ts":40,"seq":2,"event_type":2,"ext_span_id":"102-1","duration_ns":10,"output_text":"done"},
              {"trace_id":103,"span_id":1,"ts":200,"seq":1,"event_type":1,"ext_span_id":"103-1","input_text":"new keep","attrs":{"project_id":"retention-demo","task_fingerprint":"case-a"}},
              {"trace_id":103,"span_id":1,"ts":220,"seq":2,"event_type":2,"ext_span_id":"103-1","duration_ns":10,"output_text":"done"}
            ]"#;
            let (status, body) = api.route_with_tenant("POST", "/v1/ingest", batch, Some(1));
            assert_eq!(status, 200, "{body}");
            api.coord.flush_memtable();

            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/annotations",
                r#"{"traceId":102,"target":"trace","label":"manual_keep"}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");

            let query = r#"{"filter":{"projectId":"retention-demo"},"groupBy":["projectId"]}"#;
            let (status, stats) =
                api.route_with_tenant("POST", "/v1/storage-stats", query, Some(1));
            assert_eq!(status, 200, "{stats}");
            assert!(stats.contains(r#""traceCount":3"#), "{stats}");
            assert!(stats.contains(r#""groupBy":["project_id"]"#), "{stats}");
            assert!(
                stats.contains(r#""project_id":"retention-demo""#),
                "{stats}"
            );
            assert!(stats.contains(r#""annotations":1"#), "{stats}");

            let retention_query =
                r#"{"filter":{"projectId":"retention-demo"},"deleteBeforeTs":100}"#;
            let (status, plan) =
                api.route_with_tenant("POST", "/v1/retention-plan", retention_query, Some(1));
            assert_eq!(status, 200, "{plan}");
            assert!(plan.contains(r#""dryRun":true"#), "{plan}");
            assert!(plan.contains(r#""candidates":{"traceCount":2"#), "{plan}");
            assert!(plan.contains(r#""protected":{"traceCount":1"#), "{plan}");
            assert!(plan.contains(r#""deletable":{"traceCount":1"#), "{plan}");
            assert!(plan.contains(r#""102":["annotation"]"#), "{plan}");

            let retention_apply_query = r#"{"filter":{"projectId":"retention-demo"},"deleteBeforeTs":100,"compact":true,"requestedBy":"test-policy","reason":"ttl cleanup"}"#;
            let (status, applied) = api.route_with_tenant(
                "POST",
                "/v1/retention/apply",
                retention_apply_query,
                Some(1),
            );
            assert_eq!(status, 200, "{applied}");
            assert!(applied.contains(r#""applied":true"#), "{applied}");
            assert!(applied.contains(r#""deletedTraceCount":1"#), "{applied}");
            assert!(
                applied.contains(r#""deletedTraceIds":["101"]"#),
                "{applied}"
            );
            assert!(
                applied.contains(r#""compactResult":{"beforeLiveSegmentCount":1"#),
                "{applied}"
            );
            assert!(
                applied.contains(r#""compactedSegmentCount":1"#),
                "{applied}"
            );
            assert!(
                applied.contains(r#""droppedDeletedRowCount":2"#),
                "{applied}"
            );
            assert!(
                applied.contains(r#""rewrittenLiveRowCount":4"#),
                "{applied}"
            );
            assert!(applied.contains(r#""audit":{"auditId":"1""#), "{applied}");
            assert!(applied.contains(r#""source":"test-policy""#), "{applied}");
            assert!(applied.contains(r#""reason":"ttl cleanup""#), "{applied}");
            assert!(
                applied.contains(r#""traceIds":{"deletable":["101"],"deleted":["101"]"#),
                "{applied}"
            );

            let (status, after) = api.route_with_tenant("POST", "/v1/trace-search", query, Some(1));
            assert_eq!(status, 200, "{after}");
            assert!(after.contains(r#""total":2"#), "{after}");
            assert!(!after.contains(r#""traceId":"101""#), "{after}");
            assert!(after.contains(r#""traceId":"102""#), "{after}");
            assert!(after.contains(r#""traceId":"103""#), "{after}");

            let (status, audits) = api.route_with_tenant(
                "GET",
                "/v1/retention-audits?source=test-policy",
                "",
                Some(1),
            );
            assert_eq!(status, 200, "{audits}");
            assert!(audits.contains(r#""total":1"#), "{audits}");
            assert!(audits.contains(r#""deletedTraceCount":1"#), "{audits}");
            assert!(audits.contains(r#""sampleTruncated":false"#), "{audits}");

            let (status, audits) = api.route_with_tenant(
                "POST",
                "/v1/retention-audits",
                r#"{"filter":{"source":"test-policy"},"limit":10}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{audits}");
            assert!(audits.contains(r#""auditId":"1""#), "{audits}");

            let (status, audits) = api.route_with_tenant(
                "GET",
                "/v1/retention-audits?source=test-policy",
                "",
                Some(2),
            );
            assert_eq!(status, 200, "{audits}");
            assert!(audits.contains(r#""total":0"#), "{audits}");
        }
        {
            let coord = WriteCoordinator::open_durable(&dir).unwrap();
            coord.recover();
            let api = EngineJsonApi::new(coord);
            let (status, audits) = api.route_with_tenant(
                "GET",
                "/v1/retention-audits?source=test-policy",
                "",
                Some(1),
            );
            assert_eq!(status, 200, "{audits}");
            assert!(audits.contains(r#""total":1"#), "{audits}");
            assert!(audits.contains(r#""auditId":"1""#), "{audits}");
            assert!(audits.contains(r#""deletedTraceCount":1"#), "{audits}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn retention_plan_protects_snapshot_eval_and_path_memory_refs() {
        let dir = durable_temp_dir("retention-derived-protect");
        {
            let coord = WriteCoordinator::open_durable(&dir).unwrap();
            coord.recover();
            let api = EngineJsonApi::new(coord);
            let batch = r#"[
              {"trace_id":201,"span_id":1,"ts":10,"seq":1,"event_type":1,"ext_span_id":"201-1","input_text":"old delete a","attrs":{"project_id":"retention-derived"}},
              {"trace_id":201,"span_id":1,"ts":20,"seq":2,"event_type":2,"ext_span_id":"201-1","duration_ns":10,"output_text":"done"},
              {"trace_id":202,"span_id":1,"ts":10,"seq":1,"event_type":1,"ext_span_id":"202-1","input_text":"old delete b","attrs":{"project_id":"retention-derived"}},
              {"trace_id":202,"span_id":1,"ts":20,"seq":2,"event_type":2,"ext_span_id":"202-1","duration_ns":10,"output_text":"done"},
              {"trace_id":203,"span_id":1,"ts":10,"seq":1,"event_type":1,"ext_span_id":"203-1","input_text":"snapshot keep","attrs":{"project_id":"retention-derived"}},
              {"trace_id":203,"span_id":1,"ts":20,"seq":2,"event_type":2,"ext_span_id":"203-1","duration_ns":10,"output_text":"done"},
              {"trace_id":204,"span_id":1,"ts":10,"seq":1,"event_type":1,"ext_span_id":"204-1","input_text":"eval keep","attrs":{"project_id":"retention-derived"}},
              {"trace_id":204,"span_id":1,"ts":20,"seq":2,"event_type":2,"ext_span_id":"204-1","duration_ns":10,"output_text":"done"},
              {"trace_id":205,"span_id":1,"ts":10,"seq":1,"event_type":1,"ext_span_id":"205-1","input_text":"path memory keep","attrs":{"project_id":"retention-derived"}},
              {"trace_id":205,"span_id":1,"ts":20,"seq":2,"event_type":2,"ext_span_id":"205-1","duration_ns":10,"output_text":"done"}
            ]"#;
            let (status, body) = api.route_with_tenant("POST", "/v1/ingest", batch, Some(1));
            assert_eq!(status, 200, "{body}");
            api.coord.flush_memtable();

            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/dataset-associations",
                r#"{"datasetId":"snapshots","itemId":"snap-203","traceId":203,"snapshotId":"snap-203","snapshotHash":"fnv1a64:203"}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/dataset-associations",
                r#"{"datasetId":"eval-regression","itemId":"eval-204","traceId":204,"evalRunId":"eval-run-1"}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/annotations",
                r#"{"traceId":205,"target":"trace","label":"path_memory","pathMemoryId":"pm-1"}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");

            let query = r#"{"filter":{"projectId":"retention-derived"},"deleteBeforeTs":100,"protect":{"annotations":false,"datasetAssociations":false,"goldenPaths":false}}"#;
            let (status, plan) =
                api.route_with_tenant("POST", "/v1/retention-plan", query, Some(1));
            assert_eq!(status, 200, "{plan}");
            assert!(plan.contains(r#""candidates":{"traceCount":5"#), "{plan}");
            assert!(plan.contains(r#""protected":{"traceCount":3"#), "{plan}");
            assert!(plan.contains(r#""deletable":{"traceCount":2"#), "{plan}");
            assert!(plan.contains(r#""snapshots":true"#), "{plan}");
            assert!(plan.contains(r#""evalLinks":true"#), "{plan}");
            assert!(plan.contains(r#""pathMemory":true"#), "{plan}");
            assert!(plan.contains(r#""snapshotRefs":1"#), "{plan}");
            assert!(plan.contains(r#""evalLinks":1"#), "{plan}");
            assert!(plan.contains(r#""pathMemoryRefs":1"#), "{plan}");
            assert!(plan.contains(r#""203":["snapshot"]"#), "{plan}");
            assert!(plan.contains(r#""204":["evalLink"]"#), "{plan}");
            assert!(plan.contains(r#""205":["pathMemory"]"#), "{plan}");
            assert!(
                plan.contains(r#""deletableTraceIds":["201","202"]"#),
                "{plan}"
            );

            let apply_query = r#"{"filter":{"projectId":"retention-derived"},"deleteBeforeTs":100,"protect":{"annotations":false,"datasetAssociations":false,"goldenPaths":false},"requestedBy":"derived-protect-test"}"#;
            let (status, applied) =
                api.route_with_tenant("POST", "/v1/retention/apply", apply_query, Some(1));
            assert_eq!(status, 200, "{applied}");
            assert!(applied.contains(r#""deletedTraceCount":2"#), "{applied}");
            assert!(
                applied.contains(r#""deletedTraceIds":["201","202"]"#),
                "{applied}"
            );
            assert!(applied.contains(r#""snapshots":true"#), "{applied}");
            assert!(applied.contains(r#""evalLinks":true"#), "{applied}");
            assert!(applied.contains(r#""pathMemory":true"#), "{applied}");

            let (status, remaining) = api.route_with_tenant(
                "POST",
                "/v1/trace-search",
                r#"{"filter":{"projectId":"retention-derived"}}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{remaining}");
            assert!(remaining.contains(r#""total":3"#), "{remaining}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn retention_policies_run_due_and_persist() {
        let dir = durable_temp_dir("retention-policy");
        {
            let coord = WriteCoordinator::open_durable(&dir).unwrap();
            coord.recover();
            let api = EngineJsonApi::new(coord);
            let batch = r#"[
              {"trace_id":201,"span_id":1,"ts":10,"seq":1,"event_type":1,"ext_span_id":"201-1","input_text":"old policy delete","attrs":{"project_id":"policy-demo"}},
              {"trace_id":201,"span_id":1,"ts":20,"seq":2,"event_type":2,"ext_span_id":"201-1","duration_ns":10,"output_text":"done"},
              {"trace_id":202,"span_id":1,"ts":200,"seq":1,"event_type":1,"ext_span_id":"202-1","input_text":"new policy keep","attrs":{"project_id":"policy-demo"}},
              {"trace_id":202,"span_id":1,"ts":220,"seq":2,"event_type":2,"ext_span_id":"202-1","duration_ns":10,"output_text":"done"}
            ]"#;
            let (status, body) = api.route_with_tenant("POST", "/v1/ingest", batch, Some(1));
            assert_eq!(status, 200, "{body}");
            api.coord.flush_memtable();

            let policy = r#"{
              "name":"daily-policy",
              "intervalNs":1000,
              "nextRunAtNs":100,
              "source":"policy-test",
              "reason":"ttl cleanup",
              "query":{"filter":{"projectId":"policy-demo"},"olderThanNs":50,"compact":true}
            }"#;
            let (status, body) =
                api.route_with_tenant("POST", "/v1/retention-policies", policy, Some(1));
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""policyId":"1""#), "{body}");
            assert!(body.contains(r#""nextRunAtNs":"100""#), "{body}");

            let (status, body) = api.route_with_tenant(
                "GET",
                "/v1/retention-policies?name=daily-policy",
                "",
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""total":1"#), "{body}");

            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/retention-policies/run-due",
                r#"{"nowNs":100,"limit":10}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""ran":1"#), "{body}");
            assert!(body.contains(r#""failed":0"#), "{body}");
            assert!(body.contains(r#""deletedTraceCount":1"#), "{body}");
            assert!(body.contains(r#""source":"policy-test""#), "{body}");
            assert!(body.contains(r#""nextRunAtNs":"1100""#), "{body}");

            let query = r#"{"filter":{"projectId":"policy-demo"}}"#;
            let (status, after) = api.route_with_tenant("POST", "/v1/trace-search", query, Some(1));
            assert_eq!(status, 200, "{after}");
            assert!(after.contains(r#""total":1"#), "{after}");
            assert!(!after.contains(r#""traceId":"201""#), "{after}");
            assert!(after.contains(r#""traceId":"202""#), "{after}");

            let (status, audits) = api.route_with_tenant(
                "GET",
                "/v1/retention-audits?source=policy-test",
                "",
                Some(1),
            );
            assert_eq!(status, 200, "{audits}");
            assert!(audits.contains(r#""total":1"#), "{audits}");
            assert!(audits.contains(r#""deletedTraceCount":1"#), "{audits}");
        }
        {
            let coord = WriteCoordinator::open_durable(&dir).unwrap();
            coord.recover();
            let api = EngineJsonApi::new(coord);
            let (status, policies) = api.route_with_tenant(
                "GET",
                "/v1/retention-policies?name=daily-policy",
                "",
                Some(1),
            );
            assert_eq!(status, 200, "{policies}");
            assert!(policies.contains(r#""total":1"#), "{policies}");
            assert!(policies.contains(r#""lastRunAtNs":"100""#), "{policies}");
            assert!(policies.contains(r#""nextRunAtNs":"1100""#), "{policies}");

            let (status, audits) = api.route_with_tenant(
                "GET",
                "/v1/retention-audits?source=policy-test",
                "",
                Some(1),
            );
            assert_eq!(status, 200, "{audits}");
            assert!(audits.contains(r#""total":1"#), "{audits}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn golden_paths_are_tenant_isolated_and_durable() {
        let dir = durable_temp_dir("golden-paths");
        {
            let coord = WriteCoordinator::open_durable(&dir).unwrap();
            coord.recover();
            let api = EngineJsonApi::new(coord);
            let batch = r#"[
              {
                "trace_id":"gold-run-1",
                "span_id":"gold-span-1",
                "ts":100,
                "seq":1,
                "event_type":2,
                "ext_span_id":"gold-span-1",
                "status":0,
                "duration_ns":12,
                "tool_name":"planner",
                "model":"qwen",
                "provider":"openai",
                "attrs":{"project_id":"agentic-data","task_fingerprint":"refund-dispute","phase":"plan"}
              },
              {
                "trace_id":"gold-run-2",
                "span_id":"gold-span-2",
                "ts":200,
                "seq":1,
                "event_type":2,
                "ext_span_id":"gold-span-2",
                "status":0,
                "duration_ns":11,
                "tool_name":"planner",
                "model":"qwen",
                "provider":"openai",
                "attrs":{"project_id":"agentic-data","task_fingerprint":"refund-dispute","phase":"plan"}
              },
              {
                "trace_id":"gold-run-3",
                "span_id":"gold-span-3a",
                "ts":300,
                "seq":1,
                "event_type":2,
                "ext_span_id":"gold-span-3a",
                "status":0,
                "duration_ns":9,
                "tool_name":"planner",
                "model":"qwen",
                "provider":"openai",
                "attrs":{"project_id":"agentic-data","task_fingerprint":"refund-dispute","phase":"plan"}
              },
              {
                "trace_id":"gold-run-3",
                "span_id":"gold-span-3b",
                "ts":310,
                "seq":1,
                "event_type":2,
                "ext_span_id":"gold-span-3b",
                "status":0,
                "duration_ns":8,
                "tool_name":"tester",
                "model":"qwen",
                "provider":"openai",
                "attrs":{"project_id":"agentic-data","task_fingerprint":"refund-dispute","phase":"verify","validator":"unit"}
              }
            ]"#;
            let (status, body) = api.route_with_tenant("POST", "/v1/ingest", batch, Some(1));
            assert_eq!(status, 200, "{body}");

            let create = r#"{
              "sourceTraceId":"gold-run-1",
              "taskFingerprint":"refund-dispute",
              "score":960,
              "label":"fast path",
              "reason":"stable winner",
              "source":"human",
              "projectId":"agentic-data"
            }"#;
            let (status, body) = api.route_with_tenant("POST", "/v1/golden-paths", create, Some(1));
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""goldenPathId":"1""#), "{body}");
            assert!(body.contains(r#""status":"candidate""#), "{body}");
            assert!(
                body.contains(r#""trajectorySignature":"fnv1a64:"#),
                "{body}"
            );
            assert!(
                body.contains(r#""externalSourceTraceId":"gold-run-1""#),
                "{body}"
            );
            assert!(body.contains(r#""sourceTrajectory":{"#), "{body}");
            assert!(
                body.contains(r#""evidenceSummary":{"source_cost_usd_nanos""#)
                    || body.contains(r#""evidenceSummary":{"source_duration_ns""#),
                "{body}"
            );
            assert!(
                body.contains(r#""source_trajectory_step_count":1"#),
                "{body}"
            );
            assert!(body.contains(r#""project_id":"agentic-data""#), "{body}");
            assert!(body.contains(r#""model":"qwen""#), "{body}");
            assert!(body.contains(r#""provider":"openai""#), "{body}");

            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/golden-paths/1/status",
                r#"{"status":"confirmed","reason":"manual accept","source":"reviewer"}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""status":"confirmed""#), "{body}");
            assert!(body.contains(r#""reason":"manual accept""#), "{body}");

            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/annotations",
                r#"{"traceId":"gold-run-1","label":"best_path","score":960,"source":"human","projectId":"agentic-data"}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/dataset-associations",
                r#"{"datasetId":"golden-regression","itemId":"case-1","traceId":"gold-run-1","label":"pass","score":950,"projectId":"agentic-data"}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");

            let (status, body) = api.route_with_tenant(
                "GET",
                "/v1/golden-paths?taskFingerprint=refund-dispute&status=confirmed&projectId=agentic-data",
                "",
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""count":1"#), "{body}");

            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/trace-trajectories",
                r#"{"filter":{"taskFingerprint":"refund-dispute","projectId":"agentic-data"},"limit":10}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""index":"materialized""#), "{body}");
            assert!(body.contains(r#""total":3"#), "{body}");
            assert!(
                body.contains(r#""trajectory":{"signature":"fnv1a64:"#),
                "{body}"
            );

            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/trace-trajectories",
                r#"{"filter":{"taskFingerprint":"refund-dispute","attrs":{"model":"qwen","provider":"openai"}},"limit":10}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""total":3"#), "{body}");
            assert!(body.contains(r#""model":"qwen""#), "{body}");

            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/path-adherence",
                r#"{"goldenPathId":"1","traceId":"gold-run-2"}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""adherence":"followed""#), "{body}");
            assert!(body.contains(r#""sameSignature":true"#), "{body}");
            assert!(body.contains(r#""sourceAvailable":true"#), "{body}");

            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/golden-paths/1/adherence",
                r#"{"traceId":"gold-run-3"}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""adherence":"extended""#), "{body}");
            assert!(body.contains(r#""sameSignature":false"#), "{body}");
            assert!(
                body.contains(r#""extraSteps":["tool:tester|phase:verify|validator:unit"]"#),
                "{body}"
            );

            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/golden-path-evidence",
                r#"{"goldenPathId":"1","candidateTraceId":"gold-run-3"}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""source":{"available":true"#), "{body}");
            assert!(body.contains(r#""annotationCount":1"#), "{body}");
            assert!(body.contains(r#""datasetAssociationCount":1"#), "{body}");
            assert!(body.contains(r#""pathAdherence":{"goldenPath""#), "{body}");
            assert!(body.contains(r#""traceDiff":{"left""#), "{body}");
            assert!(body.contains(r#""adherence":"extended""#), "{body}");

            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/golden-path-export",
                r#"{"filter":{"taskFingerprint":"refund-dispute","projectId":"agentic-data"},"limit":10}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(
                body.contains(r#""schemaVersion":"yitrace.golden_path_export.v1""#),
                "{body}"
            );
            assert!(body.contains(r#""format":"jsonl""#), "{body}");
            assert!(body.contains(r#""count":1"#), "{body}");
            assert!(body.contains(r#""recordType":"golden_path""#), "{body}");
            assert!(body.contains(r#""annotationCount":1"#), "{body}");
            assert!(body.contains(r#""datasetAssociationCount":1"#), "{body}");
            assert!(
                body.contains(r#""jsonl":"{\"schemaVersion\":\"yitrace.golden_path_export.v1\""#),
                "{body}"
            );

            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/golden-path-health",
                r#"{"goldenPathId":"1","filter":{"projectId":"agentic-data"},"limit":10,"examples":10}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""includeSource":false"#), "{body}");
            assert!(body.contains(r#""matchingTraceTotal":2"#), "{body}");
            assert!(body.contains(r#""analyzedTraceTotal":2"#), "{body}");
            assert!(body.contains(r#""followed":1"#), "{body}");
            assert!(body.contains(r#""extended":1"#), "{body}");
            assert!(body.contains(r#""usable":1.000000"#), "{body}");
            assert!(body.contains(r#""adherence":"extended""#), "{body}");

            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/golden-paths/1/health",
                r#"{"filter":{"projectId":"agentic-data"},"includeSource":true,"limit":10}"#,
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""includeSource":true"#), "{body}");
            assert!(body.contains(r#""matchingTraceTotal":3"#), "{body}");
            assert!(body.contains(r#""followed":2"#), "{body}");

            let (status, body) =
                api.route_with_tenant("POST", "/v1/golden-paths/1/evidence", "", Some(1));
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""candidate":null"#), "{body}");

            let (status, body) = api.route_with_tenant(
                "GET",
                "/v1/golden-paths?taskFingerprint=refund-dispute",
                "",
                Some(2),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""count":0"#), "{body}");
            assert_eq!(
                api.route_with_tenant(
                    "POST",
                    "/v1/golden-paths/1/status",
                    r#"{"status":"rejected"}"#,
                    Some(2),
                )
                .0,
                404
            );
            assert_eq!(
                api.route_with_tenant(
                    "POST",
                    "/v1/path-adherence",
                    r#"{"goldenPathId":"1","traceId":"gold-run-2"}"#,
                    Some(2),
                )
                .0,
                404
            );
            assert_eq!(
                api.route_with_tenant(
                    "POST",
                    "/v1/golden-path-evidence",
                    r#"{"goldenPathId":"1","candidateTraceId":"gold-run-3"}"#,
                    Some(2),
                )
                .0,
                404
            );
            let (status, body) = api.route_with_tenant(
                "POST",
                "/v1/golden-path-export",
                r#"{"filter":{"taskFingerprint":"refund-dispute"}}"#,
                Some(2),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""count":0"#), "{body}");
            assert_eq!(
                api.route_with_tenant(
                    "POST",
                    "/v1/golden-path-health",
                    r#"{"goldenPathId":"1"}"#,
                    Some(2),
                )
                .0,
                404
            );
        }
        {
            let coord = WriteCoordinator::open_durable(&dir).unwrap();
            coord.recover();
            let api = EngineJsonApi::new(coord);
            let (status, body) = api.route_with_tenant(
                "GET",
                "/v1/golden-paths?taskFingerprint=refund-dispute&status=confirmed",
                "",
                Some(1),
            );
            assert_eq!(status, 200, "{body}");
            assert!(body.contains(r#""count":1"#), "{body}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn trace_aggregate_groups_filtered_spans() {
        let s = server();
        let batch = r#"[
          {
            "trace_id":101,
            "span_id":1,
            "ts":100,
            "seq":1,
            "event_type":2,
            "ext_span_id":"101-1",
            "session_id":9001,
            "status":0,
            "duration_ns":10,
            "tool_name":"planner",
            "input_tokens":5,
            "output_tokens":10,
            "total_tokens":15,
            "cost_usd_nanos":1000,
            "attrs":{"project_id":"agentic-data","skill":"review","mode":"auto"}
          },
          {
            "trace_id":102,
            "span_id":1,
            "ts":110,
            "seq":1,
            "event_type":2,
            "ext_span_id":"102-1",
            "session_id":9002,
            "status":1,
            "duration_ns":20,
            "tool_name":"planner",
            "input_tokens":7,
            "output_tokens":8,
            "total_tokens":15,
            "cost_usd_nanos":2000,
            "attrs":{"project_id":"agentic-data","skill":"review","mode":"auto"}
          },
          {
            "trace_id":103,
            "span_id":1,
            "ts":120,
            "seq":1,
            "event_type":2,
            "ext_span_id":"103-1",
            "session_id":9003,
            "status":0,
            "duration_ns":30,
            "tool_name":"builder",
            "input_tokens":100,
            "output_tokens":100,
            "total_tokens":200,
            "cost_usd_nanos":9999,
            "attrs":{"project_id":"other","skill":"build","mode":"auto"}
          }
        ]"#;
        let (status, body) = s.route_with_tenant("POST", "/v1/ingest", batch, Some(1));
        assert_eq!(status, 200, "{body}");

        let (status, body) = s.route_with_tenant(
            "POST",
            "/v1/trace-aggregate",
            r#"{"groupBy":["skill","mode"],"filter":{"attrs":{"project_id":"agentic-data"}},"sort":"count","order":"desc"}"#,
            Some(1),
        );
        assert_eq!(status, 200, "{body}");
        assert!(body.contains(r#""total":1"#), "{body}");
        assert!(body.contains(r#""spanTotal":2"#), "{body}");
        assert!(body.contains(r#""skill":"review""#), "{body}");
        assert!(body.contains(r#""mode":"auto""#), "{body}");
        assert!(body.contains(r#""spanCount":2"#), "{body}");
        assert!(body.contains(r#""traceCount":2"#), "{body}");
        assert!(body.contains(r#""errorCount":1"#), "{body}");
        assert!(body.contains(r#""sum":30"#), "{body}");
        assert!(body.contains(r#""totalTokens":30"#), "{body}");
        assert!(body.contains(r#""costUsdNanos":3000"#), "{body}");

        let (status, body) = s.route_with_tenant(
            "POST",
            "/v1/trace-aggregate",
            r#"{"groupBy":["toolName"],"filter":{"status":1}}"#,
            Some(1),
        );
        assert_eq!(status, 200, "{body}");
        assert!(body.contains(r#""toolName":"planner""#), "{body}");
        assert!(body.contains(r#""spanTotal":1"#), "{body}");
    }

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
}
