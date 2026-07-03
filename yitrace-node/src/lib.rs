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

    #[napi(js_name = "traceSearchJson")]
    pub fn trace_search_json(
        &self,
        query_json: String,
        tenant_id: Option<String>,
    ) -> Result<String> {
        self.route("POST", "/v1/trace-search", &query_json, tenant_id)
    }

    #[napi(js_name = "traceAggregateJson")]
    pub fn trace_aggregate_json(
        &self,
        query_json: String,
        tenant_id: Option<String>,
    ) -> Result<String> {
        self.route("POST", "/v1/trace-aggregate", &query_json, tenant_id)
    }

    #[napi(js_name = "trajectoryGroupsJson")]
    pub fn trajectory_groups_json(
        &self,
        query_json: String,
        tenant_id: Option<String>,
    ) -> Result<String> {
        self.route("POST", "/v1/trajectory-groups", &query_json, tenant_id)
    }

    #[napi(js_name = "traceTrajectoriesJson")]
    pub fn trace_trajectories_json(
        &self,
        query_json: String,
        tenant_id: Option<String>,
    ) -> Result<String> {
        self.route("POST", "/v1/trace-trajectories", &query_json, tenant_id)
    }

    #[napi(js_name = "storageStatsJson")]
    pub fn storage_stats_json(
        &self,
        query_json: String,
        tenant_id: Option<String>,
    ) -> Result<String> {
        self.route("POST", "/v1/storage-stats", &query_json, tenant_id)
    }

    #[napi(js_name = "retentionPlanJson")]
    pub fn retention_plan_json(
        &self,
        query_json: String,
        tenant_id: Option<String>,
    ) -> Result<String> {
        self.route("POST", "/v1/retention-plan", &query_json, tenant_id)
    }

    #[napi(js_name = "applyRetentionJson")]
    pub fn apply_retention_json(
        &self,
        query_json: String,
        tenant_id: Option<String>,
    ) -> Result<String> {
        self.route("POST", "/v1/retention/apply", &query_json, tenant_id)
    }

    #[napi(js_name = "retentionAuditsJson")]
    pub fn retention_audits_json(
        &self,
        query_json: String,
        tenant_id: Option<String>,
    ) -> Result<String> {
        self.route("POST", "/v1/retention-audits", &query_json, tenant_id)
    }

    #[napi(js_name = "createRetentionPolicyJson")]
    pub fn create_retention_policy_json(
        &self,
        policy_json: String,
        tenant_id: Option<String>,
    ) -> Result<String> {
        self.route("POST", "/v1/retention-policies", &policy_json, tenant_id)
    }

    #[napi(js_name = "retentionPoliciesJson")]
    pub fn retention_policies_json(
        &self,
        query: Option<String>,
        tenant_id: Option<String>,
    ) -> Result<String> {
        let path = match query {
            Some(q) if !q.is_empty() => format!("/v1/retention-policies?{q}"),
            _ => "/v1/retention-policies".to_string(),
        };
        self.route("GET", &path, "", tenant_id)
    }

    #[napi(js_name = "runRetentionPoliciesJson")]
    pub fn run_retention_policies_json(
        &self,
        query_json: String,
        tenant_id: Option<String>,
    ) -> Result<String> {
        self.route("POST", "/v1/retention-policies/run-due", &query_json, tenant_id)
    }

    #[napi(js_name = "traceDiffJson")]
    pub fn trace_diff_json(
        &self,
        query_json: String,
        tenant_id: Option<String>,
    ) -> Result<String> {
        self.route("POST", "/v1/traces/diff", &query_json, tenant_id)
    }

    #[napi(js_name = "createGoldenPathJson")]
    pub fn create_golden_path_json(
        &self,
        candidate_json: String,
        tenant_id: Option<String>,
    ) -> Result<String> {
        self.route("POST", "/v1/golden-paths", &candidate_json, tenant_id)
    }

    #[napi(js_name = "goldenPathsJson")]
    pub fn golden_paths_json(
        &self,
        query: Option<String>,
        tenant_id: Option<String>,
    ) -> Result<String> {
        let path = match query {
            Some(q) if !q.is_empty() => format!("/v1/golden-paths?{q}"),
            _ => "/v1/golden-paths".to_string(),
        };
        self.route("GET", &path, "", tenant_id)
    }

    #[napi(js_name = "updateGoldenPathStatusJson")]
    pub fn update_golden_path_status_json(
        &self,
        golden_path_id: String,
        body_json: String,
        tenant_id: Option<String>,
    ) -> Result<String> {
        let path = format!(
            "/v1/golden-paths/{}/status",
            url_encode_component(golden_path_id.trim())
        );
        self.route("POST", &path, &body_json, tenant_id)
    }

    #[napi(js_name = "pathAdherenceJson")]
    pub fn path_adherence_json(
        &self,
        query_json: String,
        tenant_id: Option<String>,
    ) -> Result<String> {
        self.route("POST", "/v1/path-adherence", &query_json, tenant_id)
    }

    #[napi(js_name = "goldenPathEvidenceJson")]
    pub fn golden_path_evidence_json(
        &self,
        query_json: String,
        tenant_id: Option<String>,
    ) -> Result<String> {
        self.route("POST", "/v1/golden-path-evidence", &query_json, tenant_id)
    }

    #[napi(js_name = "goldenPathExportJson")]
    pub fn golden_path_export_json(
        &self,
        query_json: String,
        tenant_id: Option<String>,
    ) -> Result<String> {
        self.route("POST", "/v1/golden-path-export", &query_json, tenant_id)
    }

    #[napi(js_name = "goldenPathHealthJson")]
    pub fn golden_path_health_json(
        &self,
        query_json: String,
        tenant_id: Option<String>,
    ) -> Result<String> {
        self.route("POST", "/v1/golden-path-health", &query_json, tenant_id)
    }

    #[napi(js_name = "loopsJson")]
    pub fn loops_json(
        &self,
        cursor: Option<u32>,
        limit: Option<u32>,
        filter: Option<String>,
        attrs_json: Option<String>,
        metadata_query: Option<String>,
        tenant_id: Option<String>,
    ) -> Result<String> {
        let mut path = format!(
            "/v1/loops?cursor={}&limit={}",
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
        if let Some(metadata) = metadata_query {
            if !metadata.is_empty() {
                path.push('&');
                path.push_str(&metadata);
            }
        }
        self.route("GET", &path, "", tenant_id)
    }

    #[napi(js_name = "loopJson")]
    pub fn loop_json(
        &self,
        loop_id: String,
        filter: Option<String>,
        metadata_query: Option<String>,
        tenant_id: Option<String>,
    ) -> Result<String> {
        let mut path = format!("/v1/loops/{}", url_encode_component(loop_id.trim()));
        let mut sep = "?";
        if let Some(f) = filter {
            if !f.is_empty() {
                path.push_str(sep);
                sep = "&";
                path.push_str("filter=");
                path.push_str(&url_encode_component(&f));
            }
        }
        if let Some(metadata) = metadata_query {
            if !metadata.is_empty() {
                path.push_str(sep);
                path.push_str(&metadata);
            }
        }
        self.route("GET", &path, "", tenant_id)
    }

    #[napi(js_name = "taskTracesJson")]
    pub fn task_traces_json(
        &self,
        task_fingerprint: String,
        cursor: Option<u32>,
        limit: Option<u32>,
        filter: Option<String>,
        attrs_json: Option<String>,
        metadata_query: Option<String>,
        tenant_id: Option<String>,
    ) -> Result<String> {
        let mut path = format!(
            "/v1/tasks/{}/traces?cursor={}&limit={}",
            url_encode_component(task_fingerprint.trim()),
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
        if let Some(metadata) = metadata_query {
            if !metadata.is_empty() {
                path.push('&');
                path.push_str(&metadata);
            }
        }
        self.route("GET", &path, "", tenant_id)
    }

    #[napi(js_name = "createAnnotationJson")]
    pub fn create_annotation_json(
        &self,
        annotation_json: String,
        tenant_id: Option<String>,
    ) -> Result<String> {
        self.route("POST", "/v1/annotations", &annotation_json, tenant_id)
    }

    #[napi(js_name = "annotationsJson")]
    pub fn annotations_json(
        &self,
        query: Option<String>,
        tenant_id: Option<String>,
    ) -> Result<String> {
        let path = match query {
            Some(q) if !q.is_empty() => format!("/v1/annotations?{q}"),
            _ => "/v1/annotations".to_string(),
        };
        self.route("GET", &path, "", tenant_id)
    }

    #[napi(js_name = "createDatasetAssociationJson")]
    pub fn create_dataset_association_json(
        &self,
        association_json: String,
        tenant_id: Option<String>,
    ) -> Result<String> {
        self.route(
            "POST",
            "/v1/dataset-associations",
            &association_json,
            tenant_id,
        )
    }

    #[napi(js_name = "datasetAssociationsJson")]
    pub fn dataset_associations_json(
        &self,
        query: Option<String>,
        tenant_id: Option<String>,
    ) -> Result<String> {
        let path = match query {
            Some(q) if !q.is_empty() => format!("/v1/dataset-associations?{q}"),
            _ => "/v1/dataset-associations".to_string(),
        };
        self.route("GET", &path, "", tenant_id)
    }

    #[napi(js_name = "tracesJson")]
    pub fn traces_json(
        &self,
        attrs_json: Option<String>,
        metadata_query: Option<String>,
        tenant_id: Option<String>,
    ) -> Result<String> {
        let mut path = "/v1/traces".to_string();
        let mut sep = "?";
        if let Some(attrs) = attrs_json {
            if !attrs.is_empty() {
                path.push_str(sep);
                sep = "&";
                path.push_str("attrs=");
                path.push_str(&url_encode_component(&attrs));
            }
        }
        if let Some(metadata) = metadata_query {
            if !metadata.is_empty() {
                path.push_str(sep);
                path.push_str(&metadata);
            }
        }
        self.route("GET", &path, "", tenant_id)
    }

    #[napi(js_name = "sessionsJson")]
    pub fn sessions_json(
        &self,
        cursor: Option<u32>,
        limit: Option<u32>,
        filter: Option<String>,
        attrs_json: Option<String>,
        metadata_query: Option<String>,
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
        if let Some(metadata) = metadata_query {
            if !metadata.is_empty() {
                path.push('&');
                path.push_str(&metadata);
            }
        }
        self.route("GET", &path, "", tenant_id)
    }

    #[napi(js_name = "traceJson")]
    pub fn trace_json(&self, trace_id: String, tenant_id: Option<String>) -> Result<String> {
        let path = format!("/v1/traces/{}", trace_id.trim());
        self.route("GET", &path, "", tenant_id)
    }

    #[napi(js_name = "traceSnapshotJson")]
    pub fn trace_snapshot_json(&self, trace_id: String, tenant_id: Option<String>) -> Result<String> {
        let path = format!("/v1/traces/{}/snapshot", trace_id.trim());
        self.route("GET", &path, "", tenant_id)
    }

    #[napi(js_name = "spansJson")]
    pub fn spans_json(
        &self,
        trace_id: String,
        cursor: Option<u32>,
        limit: Option<u32>,
        include_full: Option<bool>,
        tenant_id: Option<String>,
    ) -> Result<String> {
        let path = format!(
            "/v1/traces/{}/spans?cursor={}&limit={}&includeFull={}",
            trace_id.trim(),
            cursor.unwrap_or(0),
            limit.unwrap_or(50),
            if include_full.unwrap_or(false) { 1 } else { 0 }
        );
        self.route("GET", &path, "", tenant_id)
    }

    #[napi(js_name = "spansBatchJson")]
    pub fn spans_batch_json(
        &self,
        trace_id: String,
        body_json: String,
        tenant_id: Option<String>,
    ) -> Result<String> {
        let path = format!("/v1/traces/{}/spans/batch", trace_id.trim());
        self.route("POST", &path, &body_json, tenant_id)
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
