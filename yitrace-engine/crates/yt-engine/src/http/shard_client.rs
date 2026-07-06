#[derive(Debug, Clone)]
pub(super) struct ShardClientError {
    pub(super) status: u16,
    pub(super) message: String,
}

impl ShardClientError {
    pub(super) fn unavailable(message: impl Into<String>) -> Self {
        Self {
            status: 503,
            message: message.into(),
        }
    }

    fn bad_gateway(message: impl Into<String>) -> Self {
        Self {
            status: 502,
            message: message.into(),
        }
    }
}

pub(super) trait ShardClient: Send + Sync {
    fn route_with_tenant(
        &self,
        method: &str,
        path: &str,
        body: &str,
        tenant: Option<u64>,
    ) -> Result<(u16, String), ShardClientError>;

    fn ingest_wire_for_tenant(
        &self,
        records: Vec<crate::WireRecord>,
        tenant: Option<u64>,
    ) -> Result<(), ShardClientError>;

    fn search_hits(
        &self,
        request: &SearchJsonRequest,
    ) -> Result<Vec<(FoldedSpan, f32)>, ShardClientError>;

    fn replication_status(&self) -> crate::ReplicationStatus;
}

pub(super) struct LocalShardClient {
    coord: Arc<WriteCoordinator>,
}

impl LocalShardClient {
    pub(super) fn new(coord: Arc<WriteCoordinator>) -> Self {
        Self { coord }
    }
}

impl ShardClient for LocalShardClient {
    fn route_with_tenant(
        &self,
        method: &str,
        path: &str,
        body: &str,
        tenant: Option<u64>,
    ) -> Result<(u16, String), ShardClientError> {
        Ok(EngineJsonApi::new(Arc::clone(&self.coord))
            .route_with_tenant(method, path, body, tenant))
    }

    fn ingest_wire_for_tenant(
        &self,
        records: Vec<crate::WireRecord>,
        tenant: Option<u64>,
    ) -> Result<(), ShardClientError> {
        self.coord.ingest_wire_for_tenant(records, tenant);
        Ok(())
    }

    fn search_hits(
        &self,
        request: &SearchJsonRequest,
    ) -> Result<Vec<(FoldedSpan, f32)>, ShardClientError> {
        let snap = self.coord.pin_snapshot();
        let hits = match (!request.text.is_empty(), !request.vector.is_empty()) {
            (true, true) if request.text_domains.is_empty() => self.coord.search_hybrid_attr(
                &snap,
                &request.text,
                &request.vector,
                request.k,
                &request.filter,
            ),
            (true, true) => {
                let text_hits = self.coord.search_text_domains_attr(
                    &snap,
                    &request.text,
                    &request.text_domains,
                    request.k.max(10),
                    &request.filter,
                );
                let vec_hits = self.coord.search_similar_attr(
                    &snap,
                    &request.vector,
                    request.k.max(10),
                    &request.filter,
                );
                fuse_search_hit_rows(text_hits, vec_hits, request.k)
            }
            (false, true) => {
                self.coord
                    .search_similar_attr(&snap, &request.vector, request.k, &request.filter)
            }
            _ if request.text_domains.is_empty() => self.coord.search_text_attr(
                &snap,
                &request.text,
                request.k,
                &request.filter,
            ),
            _ => self.coord.search_text_domains_attr(
                &snap,
                &request.text,
                &request.text_domains,
                request.k,
                &request.filter,
            ),
        };
        Ok(hits)
    }

    fn replication_status(&self) -> crate::ReplicationStatus {
        self.coord.replication_status()
    }
}

/// 远端 shard 的 std-only HTTP 客户端。
///
/// 它不是临时 smoke test helper，而是分布式 gateway 访问 shard server 的共享边界：
/// - 写入：`ingest_records_for_tenant()` 序列化 `WireRecord` 并 POST `/v1/ingest`。
/// - 查询/管理：`route_json_with_tenant()` 透传 shard-local HTTP JSON API。
/// - 状态：`replication_status_snapshot()` 读取 `/v1/cluster/shards`。
///
/// 引擎本体仍保持零外部依赖，所以这里直接用 `TcpStream` 写 HTTP/1.1。
#[derive(Clone, Debug)]
pub struct RemoteShardClient {
    addr: String,
    timeout: std::time::Duration,
    retry: RemoteShardRetryConfig,
    circuit_breaker: Option<RemoteShardCircuitBreakerConfig>,
    circuit_state: std::sync::Arc<std::sync::Mutex<RemoteShardCircuitBreakerState>>,
}

#[derive(Clone, Copy, Debug)]
struct RemoteShardRetryConfig {
    max_attempts: usize,
    backoff: std::time::Duration,
}

impl Default for RemoteShardRetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 1,
            backoff: std::time::Duration::ZERO,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct RemoteShardCircuitBreakerConfig {
    failure_threshold: u32,
    reset_timeout: std::time::Duration,
}

#[derive(Debug, Default)]
struct RemoteShardCircuitBreakerState {
    consecutive_failures: u32,
    opened_at: Option<std::time::Instant>,
}

impl RemoteShardClient {
    pub fn new(addr: impl Into<String>) -> Self {
        Self {
            addr: normalize_remote_addr(&addr.into()),
            timeout: std::time::Duration::from_secs(3),
            retry: RemoteShardRetryConfig::default(),
            circuit_breaker: None,
            circuit_state: std::sync::Arc::new(std::sync::Mutex::new(
                RemoteShardCircuitBreakerState::default(),
            )),
        }
    }

    pub fn with_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// 配置远端 shard 调用的重试次数；默认不重试，且只重试安全重放的请求。
    pub fn with_retry(mut self, max_attempts: usize, backoff: std::time::Duration) -> Self {
        self.retry.max_attempts = max_attempts.max(1);
        self.retry.backoff = backoff;
        self
    }

    /// 配置最小熔断器；timeout 后下一次调用进入 half-open 探测。
    pub fn with_circuit_breaker(
        mut self,
        failure_threshold: u32,
        reset_timeout: std::time::Duration,
    ) -> Self {
        self.circuit_breaker = (failure_threshold > 0).then_some(RemoteShardCircuitBreakerConfig {
            failure_threshold,
            reset_timeout,
        });
        self
    }

    pub fn addr(&self) -> &str {
        &self.addr
    }

    pub fn route_json_with_tenant(
        &self,
        method: &str,
        path: &str,
        body: &str,
        tenant: Option<u64>,
    ) -> Result<(u16, String), String> {
        self.request(method, path, body, tenant)
            .map_err(|err| err.message)
    }

    pub fn ingest_records_for_tenant(
        &self,
        records: Vec<crate::WireRecord>,
        tenant: Option<u64>,
    ) -> Result<(), String> {
        self.remote_ingest_wire_for_tenant(records, tenant)
            .map_err(|err| err.message)
    }

    pub fn replication_status_snapshot(&self) -> crate::ReplicationStatus {
        self.remote_replication_status()
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        body: &str,
        tenant: Option<u64>,
    ) -> Result<(u16, String), ShardClientError> {
        self.ensure_circuit_allows()?;
        let max_attempts = self.retry_attempts_for(method, path);
        let mut attempt = 0usize;
        loop {
            attempt += 1;
            match self.request_once(method, path, body, tenant) {
                Ok((status, response)) if is_retryable_status(status) => {
                    if attempt < max_attempts {
                        self.retry_sleep();
                        continue;
                    }
                    self.record_circuit_failure();
                    return Ok((status, response));
                }
                Ok(success) => {
                    self.record_circuit_success();
                    return Ok(success);
                }
                Err(err) if is_retryable_error(&err) && attempt < max_attempts => {
                    self.retry_sleep();
                    continue;
                }
                Err(err) => {
                    self.record_circuit_failure();
                    return Err(err);
                }
            }
        }
    }

    fn request_once(
        &self,
        method: &str,
        path: &str,
        body: &str,
        tenant: Option<u64>,
    ) -> Result<(u16, String), ShardClientError> {
        if self.addr.starts_with("https://") {
            return Err(ShardClientError::unavailable(
                "https remote shard addresses are not supported by the std-only client",
            ));
        }
        let mut stream = TcpStream::connect(self.addr.as_str())
            .map_err(|e| ShardClientError::unavailable(format!("connect remote shard: {e}")))?;
        let _ = stream.set_read_timeout(Some(self.timeout));
        let _ = stream.set_write_timeout(Some(self.timeout));

        let tenant_header = tenant
            .map(|id| format!("X-Tenant-Id: {id}\r\n"))
            .unwrap_or_default();
        let content_type = if body.is_empty() {
            ""
        } else {
            "Content-Type: application/json; charset=utf-8\r\n"
        };
        let req = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\n{}{}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
            self.addr,
            tenant_header,
            content_type,
            body.len(),
            body
        );
        stream.write_all(req.as_bytes()).map_err(|e| {
            ShardClientError::unavailable(format!("write remote shard request: {e}"))
        })?;
        let mut raw = String::new();
        stream.read_to_string(&mut raw).map_err(|e| {
            ShardClientError::unavailable(format!("read remote shard response: {e}"))
        })?;
        parse_http_response(&raw)
    }

    fn retry_attempts_for(&self, method: &str, path: &str) -> usize {
        if is_retry_safe_remote_request(method, path) {
            self.retry.max_attempts
        } else {
            1
        }
    }

    fn retry_sleep(&self) {
        if !self.retry.backoff.is_zero() {
            std::thread::sleep(self.retry.backoff);
        }
    }

    fn ensure_circuit_allows(&self) -> Result<(), ShardClientError> {
        let Some(config) = self.circuit_breaker else {
            return Ok(());
        };
        let mut state = self
            .circuit_state
            .lock()
            .map_err(|_| ShardClientError::unavailable("remote shard circuit breaker poisoned"))?;
        let Some(opened_at) = state.opened_at else {
            return Ok(());
        };
        if opened_at.elapsed() < config.reset_timeout {
            return Err(ShardClientError::unavailable(format!(
                "remote shard circuit breaker open for {}",
                self.addr
            )));
        }
        state.opened_at = None;
        Ok(())
    }

    fn record_circuit_success(&self) {
        if self.circuit_breaker.is_none() {
            return;
        }
        if let Ok(mut state) = self.circuit_state.lock() {
            state.consecutive_failures = 0;
            state.opened_at = None;
        }
    }

    fn record_circuit_failure(&self) {
        let Some(config) = self.circuit_breaker else {
            return;
        };
        if let Ok(mut state) = self.circuit_state.lock() {
            state.consecutive_failures = state.consecutive_failures.saturating_add(1);
            if state.consecutive_failures >= config.failure_threshold {
                state.opened_at = Some(std::time::Instant::now());
            }
        }
    }

    fn remote_ingest_wire_for_tenant(
        &self,
        records: Vec<crate::WireRecord>,
        tenant: Option<u64>,
    ) -> Result<(), ShardClientError> {
        let count = records.len();
        let body = wire_batch_json(&records);
        let (status, response) = self.request("POST", "/v1/ingest", &body, tenant)?;
        if status != 200 {
            return Err(ShardClientError {
                status,
                message: compact_error_body(&response),
            });
        }
        if !response.contains(&format!(r#""ingested":{count}"#)) {
            return Err(ShardClientError::bad_gateway(format!(
                "remote shard ingest count mismatch: expected {count}, body {response}"
            )));
        }
        Ok(())
    }

    fn remote_replication_status(&self) -> crate::ReplicationStatus {
        let Ok((200, body)) = self.request("GET", "/v1/cluster/shards", "", None) else {
            return empty_replication_status();
        };
        parse_remote_replication_status(&body).unwrap_or_else(empty_replication_status)
    }
}

impl ShardClient for RemoteShardClient {
    fn route_with_tenant(
        &self,
        method: &str,
        path: &str,
        body: &str,
        tenant: Option<u64>,
    ) -> Result<(u16, String), ShardClientError> {
        self.request(method, path, body, tenant)
    }

    fn ingest_wire_for_tenant(
        &self,
        records: Vec<crate::WireRecord>,
        tenant: Option<u64>,
    ) -> Result<(), ShardClientError> {
        self.remote_ingest_wire_for_tenant(records, tenant)
    }

    fn search_hits(
        &self,
        request: &SearchJsonRequest,
    ) -> Result<Vec<(FoldedSpan, f32)>, ShardClientError> {
        let (status, body) = self.request(
            "POST",
            "/v1/search",
            &request.raw_body,
            request.filter.tenant_id,
        )?;
        if status != 200 {
            return Err(ShardClientError {
                status,
                message: compact_error_body(&body),
            });
        }
        parse_remote_search_hits(&body).map_err(ShardClientError::bad_gateway)
    }

    fn replication_status(&self) -> crate::ReplicationStatus {
        self.remote_replication_status()
    }
}
