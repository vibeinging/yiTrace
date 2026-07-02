use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use napi::{Error, Result, Status};
use napi_derive::napi;
use yt_engine::{EngineJsonApi, WriteCoordinator};

fn napi_err(message: impl Into<String>) -> Error {
    Error::new(Status::GenericFailure, message.into())
}

fn parse_tenant_id(tenant_id: Option<String>) -> Result<Option<u64>> {
    match tenant_id {
        None => Ok(None),
        Some(s) if s.trim().is_empty() => Ok(None),
        Some(s) => s
            .trim()
            .parse::<u64>()
            .map(Some)
            .map_err(|_| napi_err(format!("bad tenantId: {s}"))),
    }
}

fn status_error(status: u16, body: String) -> Error {
    napi_err(format!(
        "yiTrace request failed: status={status} body={body}"
    ))
}

fn lock_data_dir(dir: &Path) -> Result<(PathBuf, File)> {
    std::fs::create_dir_all(dir).map_err(|e| napi_err(format!("create data dir failed: {e}")))?;
    let lock_path = dir.join(".yitrace.lock");
    let lock_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)
        .map_err(|e| {
            napi_err(format!(
                "data dir is already open or locked: {} ({e})",
                lock_path.display()
            ))
        })?;
    Ok((lock_path, lock_file))
}

#[napi(js_name = "NativeYiTraceDB")]
pub struct NativeYiTraceDb {
    coord: Arc<WriteCoordinator>,
    api: EngineJsonApi,
    lock_path: PathBuf,
    lock_file: Option<File>,
    closed: bool,
}

#[napi]
impl NativeYiTraceDb {
    #[napi(constructor)]
    pub fn new(data_dir: String) -> Result<Self> {
        let dir = PathBuf::from(data_dir);
        let (lock_path, lock_file) = lock_data_dir(&dir)?;
        let coord = WriteCoordinator::open_durable(&dir)
            .map_err(|e| napi_err(format!("open yiTrace data dir failed: {e}")))?;
        coord.recover();
        let api = EngineJsonApi::new(Arc::clone(&coord));
        Ok(Self {
            coord,
            api,
            lock_path,
            lock_file: Some(lock_file),
            closed: false,
        })
    }

    #[napi(js_name = "ingestJson")]
    pub fn ingest_json(&self, events_json: String, tenant_id: Option<String>) -> Result<String> {
        self.route("POST", "/v1/ingest", &events_json, tenant_id)
    }

    #[napi(js_name = "ingestOtlpJson")]
    pub fn ingest_otlp_json(&self, otlp_json: String, tenant_id: Option<String>) -> Result<String> {
        self.route("POST", "/v1/traces", &otlp_json, tenant_id)
    }

    #[napi(js_name = "searchJson")]
    pub fn search_json(&self, query_json: String, tenant_id: Option<String>) -> Result<String> {
        self.route("POST", "/v1/search", &query_json, tenant_id)
    }

    #[napi(js_name = "tracesJson")]
    pub fn traces_json(&self, tenant_id: Option<String>) -> Result<String> {
        self.route("GET", "/v1/traces", "", tenant_id)
    }

    #[napi(js_name = "sessionsJson")]
    pub fn sessions_json(
        &self,
        cursor: Option<u32>,
        limit: Option<u32>,
        filter: Option<String>,
        attrs_json: Option<String>,
        tenant_id: Option<String>,
    ) -> Result<String> {
        let mut path = format!(
            "/v1/sessions?cursor={}&limit={}",
            cursor.unwrap_or(0),
            limit.unwrap_or(50)
        );
        if let Some(f) = filter {
            if !f.is_empty() {
                path.push_str("&filter=");
                path.push_str(&url_encode_component(&f));
            }
        }
        if let Some(attrs) = attrs_json {
            if !attrs.is_empty() {
                path.push_str("&attrs=");
                path.push_str(&url_encode_component(&attrs));
            }
        }
        self.route("GET", &path, "", tenant_id)
    }

    #[napi(js_name = "traceJson")]
    pub fn trace_json(&self, trace_id: String, tenant_id: Option<String>) -> Result<String> {
        let path = format!("/v1/traces/{}", trace_id.trim());
        self.route("GET", &path, "", tenant_id)
    }

    #[napi(js_name = "spanJson")]
    pub fn span_json(
        &self,
        trace_id: String,
        span_id: String,
        tenant_id: Option<String>,
    ) -> Result<String> {
        let path = format!("/v1/traces/{}/spans/{}", trace_id.trim(), span_id.trim());
        self.route("GET", &path, "", tenant_id)
    }

    #[napi]
    pub fn flush(&self) -> Result<()> {
        self.ensure_open()?;
        self.coord.flush_memtable();
        Ok(())
    }

    #[napi]
    pub fn close(&mut self) -> Result<()> {
        if self.closed {
            return Ok(());
        }
        self.coord.flush_memtable();
        self.lock_file.take();
        let _ = std::fs::remove_file(&self.lock_path);
        self.closed = true;
        Ok(())
    }

    fn ensure_open(&self) -> Result<()> {
        if self.closed {
            Err(napi_err("YiTraceDB is closed"))
        } else {
            Ok(())
        }
    }

    fn route(
        &self,
        method: &str,
        path: &str,
        body: &str,
        tenant_id: Option<String>,
    ) -> Result<String> {
        self.ensure_open()?;
        let tenant = parse_tenant_id(tenant_id)?;
        let (status, response) = self.api.route_with_tenant(method, path, body, tenant);
        if (200..300).contains(&status) {
            Ok(response)
        } else {
            Err(status_error(status, response))
        }
    }
}

impl Drop for NativeYiTraceDb {
    fn drop(&mut self) {
        if !self.closed {
            self.coord.flush_memtable();
            self.lock_file.take();
            let _ = std::fs::remove_file(&self.lock_path);
            self.closed = true;
        }
    }
}

fn url_encode_component(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
