//! 极小 HTTP/1.1 摄入+查询服务（只用 std::net，零依赖、离线可编译）。
//!
//! 路由：
//!   POST /v1/ingest  —— body 是 SDK 线格式 JSON 批 → `parse_wire_batch` → `ingest_wire`
//!   POST /v1/traces  —— OTLP/HTTP 标准端点：OTLP/OpenInference trace → `ingest_otlp`（生态入口）
//!   GET  /v1/traces  —— 返回 trace 列表（JSON）
//!   POST /v1/search  —— 中文检索 + 可选属性过滤(agent/状态/时间) → `search_text_attr`（产品差异化出口）
//! 这是 SDK→引擎跨进程的最后一层。真要上量/上 TLS，换 axum/hyper 即可，路由逻辑（`route`）不变。
//! OTLP 走「OTLP→WireRecord 适配器」（`otlp.rs`）接到同一个 `ingest_wire` 边界。
#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use crate::{
    parse_wire_batch, AnnotationStatus, AnnotationTarget, AttrIndexedReadStats, ConsoleSession,
    DatasetAssociation, DatasetAssociationFilter, GoldenPathCandidate, GoldenPathFilter,
    GoldenPathStatus, NewDatasetAssociation, NewGoldenPathCandidate, NewRetentionAuditRecord,
    NewRetentionPolicy, NewTraceAnnotation, RetentionAuditFilter, RetentionPolicyFilter,
    TraceAnnotation, TraceAnnotationFilter, TraceQuery, TraceSummary, UpdateTraceAnnotation,
    WriteCoordinator,
};
use yt_core::fold::FoldedSpan;

mod api_core;
mod console_api;
mod diff_loop_task_api;
mod golden_path_api;
mod golden_path_export_health_api;
mod lists_api;
mod metadata_api;
mod replication_api;
mod retention_api;
mod search_api;
mod snapshot_helpers;
mod storage_api;
mod storage_facade;
mod trajectory_api;
mod vector_api;

use storage_facade::{
    cluster_metadata_id_base, LocalTraceStorage, ShardBackend, ShardRouter, TraceStorage,
};

/// 进程内 JSON API 边界。
///
/// 它复用 HTTP 的 path/body JSON 契约，但不负责 socket、鉴权头解析、body limit、gzip
/// 或静态资源。HTTP server 和 Node/Electron N-API 都走这里，从而共享同一套路由语义。
#[derive(Clone)]
pub struct EngineJsonApi {
    storage: Arc<dyn TraceStorage>,
    snapshot_leases: Arc<snapshot_helpers::SnapshotLeaseBook>,
    read_model_cache: Arc<Mutex<ReadModelCache>>,
}

#[derive(Default)]
struct ReadModelCache {
    map: HashMap<String, String>,
}

impl ReadModelCache {
    fn get(&self, key: &str) -> Option<String> {
        self.map.get(key).cloned()
    }

    fn put(&mut self, key: String, value: String) {
        if self.map.len() >= 256 {
            self.map.clear();
        }
        self.map.insert(key, value);
    }
}

/// yiTrace API 当前挂载的存储模式。
///
/// 第一阶段只暴露 single-node/single-shard，后续 cluster mode 会在这个边界下扩展，
/// 避免上层 API 直接绑定单个 `WriteCoordinator`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageMode {
    SingleNode,
    InProcessCluster,
}

impl StorageMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SingleNode => "single_node",
            Self::InProcessCluster => "in_process_cluster",
        }
    }
}

/// 稳定 shard 身份。单机模式也显式使用 `shard-0`，让 API/测试先形成分片语义。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ShardId(String);

impl ShardId {
    pub fn new(id: impl Into<String>) -> Self {
        let id = id.into();
        if id.trim().is_empty() {
            Self::default()
        } else {
            Self(id)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for ShardId {
    fn default() -> Self {
        Self("shard-0".to_string())
    }
}

/// 进程内 cluster 原型的 follower 副本配置。
///
/// 这只是分布式升级的本地测试/嵌入式形态：它把 follower `WriteCoordinator`
/// 挂到同一个 shard 拓扑里，供状态展示和后续读副本路由使用；不表示已经有网络复制或自动故障切换。
#[derive(Clone)]
pub struct InProcessReplicaSpec {
    pub replica_id: String,
    pub coord: Arc<WriteCoordinator>,
    pub max_lag_lsn: u64,
}

impl InProcessReplicaSpec {
    pub fn new(
        replica_id: impl Into<String>,
        coord: Arc<WriteCoordinator>,
        max_lag_lsn: u64,
    ) -> Self {
        let replica_id = replica_id.into();
        Self {
            replica_id: if replica_id.trim().is_empty() {
                "replica-0".to_string()
            } else {
                replica_id
            },
            coord,
            max_lag_lsn,
        }
    }
}

/// 进程内 cluster 原型的单个 shard 配置。
#[derive(Clone)]
pub struct InProcessShardSpec {
    pub shard_id: ShardId,
    pub leader: Arc<WriteCoordinator>,
    pub replicas: Vec<InProcessReplicaSpec>,
}

impl InProcessShardSpec {
    pub fn new(shard_id: ShardId, leader: Arc<WriteCoordinator>) -> Self {
        Self {
            shard_id,
            leader,
            replicas: Vec::new(),
        }
    }

    pub fn with_replica(mut self, replica: InProcessReplicaSpec) -> Self {
        self.replicas.push(replica);
        self
    }
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
        Self::new_single_shard(coord, ShardId::default())
    }

    pub fn new_single_shard(coord: Arc<WriteCoordinator>, shard_id: ShardId) -> Self {
        let storage: Arc<dyn TraceStorage> =
            Arc::new(LocalTraceStorage::new_single(coord, shard_id));
        Self {
            storage,
            snapshot_leases: Arc::new(snapshot_helpers::SnapshotLeaseBook::default()),
            read_model_cache: Arc::new(Mutex::new(ReadModelCache::default())),
        }
    }

    pub fn new_in_process_cluster(
        shards: Vec<(ShardId, Arc<WriteCoordinator>)>,
    ) -> Result<Self, String> {
        let storage: Arc<dyn TraceStorage> =
            Arc::new(LocalTraceStorage::new_in_process_cluster(shards)?);
        Ok(Self {
            storage,
            snapshot_leases: Arc::new(snapshot_helpers::SnapshotLeaseBook::default()),
            read_model_cache: Arc::new(Mutex::new(ReadModelCache::default())),
        })
    }

    pub fn new_in_process_cluster_with_replicas(
        shards: Vec<InProcessShardSpec>,
    ) -> Result<Self, String> {
        let storage: Arc<dyn TraceStorage> = Arc::new(
            LocalTraceStorage::new_in_process_cluster_with_replicas(shards)?,
        );
        Ok(Self {
            storage,
            snapshot_leases: Arc::new(snapshot_helpers::SnapshotLeaseBook::default()),
            read_model_cache: Arc::new(Mutex::new(ReadModelCache::default())),
        })
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
        if read_model_mutating_route(method, base) {
            self.invalidate_read_model_cache();
        }
        match (method, base) {
            ("POST", "/v1/ingest") => self.ingest_json(body, tenant),
            // OTLP/HTTP 标准 trace 端点（生态入口）：OpenTelemetry / OpenInference 埋点直接 POST 到这里。
            ("POST", "/v1/traces") => self.ingest_otlp_json(body, tenant),
            ("GET", "/v1/traces") => {
                if self.is_in_process_cluster() {
                    (200, self.cluster_traces_json(query, tenant))
                } else {
                    (200, self.traces_json(query, tenant))
                }
            }
            // 检索端点（产品差异化的出口）：中文 BM25 + 可选属性过滤(agent/状态/时间/trace) + 租户隔离。
            ("POST", "/v1/search") => {
                if self.is_in_process_cluster() {
                    self.cluster_search_json(body, tenant)
                } else {
                    self.search_json(body, tenant)
                }
            }
            ("POST", "/v1/vector-index") => self.vector_index_json(body, tenant),
            ("POST", "/v1/vector-search") => self.vector_search_json(body, tenant),
            ("POST", "/v1/snapshots/lease") => self.snapshot_lease_json(body),
            ("POST", "/v1/snapshots/renew") => self.snapshot_renew_json(body),
            // 产品化 trace/span 搜索：跨 session 扫描折叠 span，支持 attrs、文本 contains、分页和排序。
            ("POST", "/v1/trace-search") => {
                if self.is_in_process_cluster() {
                    self.cluster_trace_search_json(body, tenant)
                } else {
                    self.trace_search_json(body, tenant)
                }
            }
            // 产品化聚合：按 skill/mode/tool/model/attrs 等字段做 group-by，用于 trace inbox 和路径挖掘。
            ("POST", "/v1/trace-aggregate") | ("POST", "/v1/trace-aggregates") => {
                if self.is_in_process_cluster() {
                    self.cluster_trace_aggregate_json(body, tenant)
                } else {
                    self.trace_aggregate_json(body, tenant)
                }
            }
            // Trajectory 聚合：按完整 trace 路径签名分桶，给 golden path mining 提供候选证据。
            ("POST", "/v1/trajectory-groups")
            | ("POST", "/v1/trajectory-aggregate")
            | ("POST", "/v1/best-paths") => {
                if self.is_in_process_cluster() {
                    self.cluster_trajectory_groups_json(body, tenant)
                } else {
                    self.trajectory_groups_json(body, tenant)
                }
            }
            // 物化 trajectory read model：按 traceSearch 过滤返回每条 trace 的路径摘要。
            ("POST", "/v1/trace-trajectories") | ("POST", "/v1/trajectories") => {
                if self.is_in_process_cluster() {
                    self.cluster_trace_trajectories_json(body, tenant)
                } else {
                    self.trace_trajectories_json(body, tenant)
                }
            }
            // Storage/retention：统计 trace 存储占用，或按策略 dry-run / apply 安全清理。
            ("POST", "/v1/storage-stats") | ("POST", "/v1/storage/stats") => {
                if self.is_in_process_cluster() {
                    self.cluster_storage_stats_json(body, tenant)
                } else {
                    self.storage_stats_json(body, tenant)
                }
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
            // Shard 内网络复制底座：leader 导出 WAL 增量，follower 幂等应用。
            ("GET", "/v1/replication/status") => (200, self.replication_status_json()),
            ("GET", "/v1/replication/wal") => self.replication_wal_json(query),
            ("POST", "/v1/replication/wal")
            | ("POST", "/v1/replication/apply")
            | ("POST", "/v1/replication/apply-wal") => self.apply_replication_wal_json(body),
            ("POST", "/v1/replication/pull")
            | ("POST", "/v1/replication/pull-once")
            | ("POST", "/v1/replication/worker/run-once") => {
                self.replication_pull_once_json(body, tenant)
            }
            // Trace trajectory diff：比较两次尝试的路线、工具、状态、耗时和成本差异。
            ("POST", "/v1/traces/diff") | ("POST", "/v1/trace-diff") => {
                self.trace_diff_json(body, tenant)
            }
            // Agent loop/task 读模型：基于 P0.5 一等字段产出稳定摘要，业务侧不必自己扫 spans 拼。
            ("GET", "/v1/loops") => {
                if self.is_in_process_cluster() {
                    self.cluster_loops_page_json(query, tenant)
                } else {
                    (200, self.loops_page_json(query, tenant))
                }
            }
            // 业务元数据：给 trace/span 打后验 annotation，并把 trace/span 关联到外部 dataset item。
            ("POST", "/v1/annotations") => self.create_annotation_json(body, tenant),
            ("GET", "/v1/annotations") => {
                if self.is_in_process_cluster() {
                    self.cluster_annotations_json(query, tenant)
                } else {
                    self.annotations_json(query, tenant)
                }
            }
            ("POST", "/v1/dataset-associations") | ("POST", "/v1/dataset-links") => {
                self.create_dataset_association_json(body, tenant)
            }
            ("GET", "/v1/dataset-associations") | ("GET", "/v1/dataset-links") => {
                if self.is_in_process_cluster() {
                    self.cluster_dataset_associations_json(query, tenant)
                } else {
                    self.dataset_associations_json(query, tenant)
                }
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
            // 分布式升级前置：即便单机模式也显式暴露 shard/storage 形态，便于上层先按可分片 API 接。
            ("GET", "/v1/cluster") | ("GET", "/v1/cluster/shards") | ("GET", "/v1/shards") => {
                (200, self.cluster_shards_json())
            }
            // 生产可观测（§3.1）：Prometheus 文本格式，无需租户隔离（全局指标）。
            ("GET", "/v1/metrics") => (200, self.coord().metrics()),
            // 控制台数据端点（前端 yitrace-console 对接）：会话游标分页 / 轮次 / trace span / span 详情。
            ("GET", "/v1/sessions") => {
                if self.is_in_process_cluster() {
                    self.cluster_sessions_page_json(query, tenant)
                } else {
                    (200, self.sessions_page_json(query, tenant))
                }
            }
            _ => self.route_console(method, base, query, body, tenant),
        }
    }

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
        if self.is_in_process_cluster() {
            match (method, segs.as_slice()) {
                ("GET", ["v1", "sessions", id, "turns"]) => {
                    let Some(session_id) = parse_id_or_hash(id) else {
                        return (400, r#"{"error":"bad session id"}"#.to_string());
                    };
                    let Some(idx) = self.session_detail_owner_index(tenant, session_id) else {
                        return (200, "[]".to_string());
                    };
                    return self
                        .single_shard_api_at(idx)
                        .route_console(method, base, query, body, tenant);
                }
                ("GET", ["v1", "traces", id])
                | ("GET", ["v1", "traces", id, "snapshot"])
                | ("GET", ["v1", "traces", id, "steps"])
                | ("GET", ["v1", "traces", id, "spans"])
                | ("POST", ["v1", "traces", id, "spans", "batch"])
                | ("GET", ["v1", "traces", id, "spans", _]) => {
                    let Some(trace_id) = parse_id_or_hash(id) else {
                        return (400, r#"{"error":"bad trace id"}"#.to_string());
                    };
                    let Some(idx) = self.trace_detail_owner_index(tenant, trace_id) else {
                        return (404, r#"{"error":"trace not found"}"#.to_string());
                    };
                    return self
                        .single_shard_api_at(idx)
                        .route_console(method, base, query, body, tenant);
                }
                _ => {}
            }
        }
        match (method, segs.as_slice()) {
            ("GET", ["v1", "sessions", id, "turns"]) => self.turns_json(id, tenant),
            ("GET", ["v1", "loops", id]) => {
                if self.is_in_process_cluster() {
                    self.cluster_loop_detail_json(id, query, tenant)
                } else {
                    self.loop_detail_json(id, query, tenant)
                }
            }
            ("GET", ["v1", "tasks", fingerprint, "traces"])
            | ("GET", ["v1", "task", fingerprint, "traces"]) => {
                if self.is_in_process_cluster() {
                    self.cluster_task_traces_json(fingerprint, query, tenant)
                } else {
                    (200, self.task_traces_json(fingerprint, query, tenant))
                }
            }
            ("PATCH", ["v1", "annotations", id])
            | ("POST", ["v1", "annotations", id, "status"]) => {
                if self.is_in_process_cluster() {
                    self.cluster_update_annotation_json(id, body, tenant)
                } else {
                    self.update_annotation_json(id, body, tenant)
                }
            }
            ("DELETE", ["v1", "annotations", id]) => {
                if self.is_in_process_cluster() {
                    self.cluster_delete_annotation_json(id, body, tenant)
                } else {
                    self.delete_annotation_json(id, body, tenant)
                }
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
            ("DELETE", ["v1", "snapshots", id]) => self.snapshot_release_json(id),
            _ => (404, r#"{"error":"not found"}"#.to_string()),
        }
    }
}

include!("http/helper_types.rs");
include!("http/mutation_helpers.rs");
include!("http/json_parse_helpers.rs");
include!("http/fanout_helpers.rs");
include!("http/shard_client.rs");
include!("http/shard_client_helpers.rs");
include!("http/route_table.rs");
include!("http/remote_gateway_state.rs");
include!("http/remote_gateway_snapshot.rs");
include!("http/remote_gateway_snapshot_lease.rs");
include!("http/remote_gateway_consistency.rs");
include!("http/remote_gateway.rs");
include!("http/remote_gateway_vector.rs");
include!("http/remote_gateway_server.rs");
include!("http/remote_gateway_helpers.rs");
include!("http/remote_gateway_merge.rs");
include!("http/remote_gateway_storage.rs");
include!("http/remote_gateway_metadata.rs");
include!("http/trace_search_helpers.rs");
include!("http/aggregate_helpers.rs");
include!("http/trajectory_helpers.rs");
include!("http/trace_diff_json_helpers.rs");
include!("http/storage_helpers.rs");
include!("http/retention_helpers.rs");
include!("http/trace_diff_helpers.rs");
include!("http/metadata_helpers.rs");
include!("http/loop_task_helpers.rs");
include!("http/console_helpers.rs");
include!("http/json_misc_helpers.rs");
include!("http/golden_path_json_helpers.rs");
include!("http/search_json_helpers.rs");
include!("http/attr_helpers.rs");

#[cfg(test)]
mod tests;
