use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use yt_engine::{EngineJsonApi, WriteCoordinator};

fn py_runtime_err(message: impl Into<String>) -> PyErr {
    PyRuntimeError::new_err(message.into())
}

fn py_value_err(message: impl Into<String>) -> PyErr {
    PyValueError::new_err(message.into())
}

fn parse_tenant_id(tenant_id: Option<String>) -> PyResult<Option<u64>> {
    match tenant_id {
        None => Ok(None),
        Some(s) if s.trim().is_empty() => Ok(None),
        Some(s) => s
            .trim()
            .parse::<u64>()
            .map(Some)
            .map_err(|_| py_value_err(format!("bad tenant_id: {s}"))),
    }
}

fn json_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn host_name() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "unknown".to_string())
}

fn lock_metadata(dir: &Path) -> String {
    let created_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let executable = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    format!(
        "{{\"pid\":{},\"host\":\"{}\",\"created_unix_ms\":{},\"data_dir\":\"{}\",\"executable\":\"{}\"}}\n",
        std::process::id(),
        json_escape(&host_name()),
        created_unix_ms,
        json_escape(&dir.display().to_string()),
        json_escape(&executable),
    )
}

fn existing_lock_owner(lock_path: &Path) -> String {
    match std::fs::read_to_string(lock_path) {
        Ok(text) if !text.trim().is_empty() => format!("; existing lock owner: {}", text.trim()),
        Ok(_) => "; existing lock owner: <empty lock file>".to_string(),
        Err(e) => format!("; existing lock owner: unreadable ({e})"),
    }
}

fn lock_data_dir(dir: &Path) -> PyResult<(PathBuf, File)> {
    std::fs::create_dir_all(dir)
        .map_err(|e| py_runtime_err(format!("create data dir failed: {e}")))?;
    let lock_path = dir.join(".yitrace.lock");
    let mut lock_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)
        .map_err(|e| {
            py_runtime_err(format!(
                "data dir is already open or locked: {} ({e}){}",
                lock_path.display(),
                existing_lock_owner(&lock_path),
            ))
        })?;
    let metadata = lock_metadata(dir);
    if let Err(e) = lock_file
        .write_all(metadata.as_bytes())
        .and_then(|_| lock_file.sync_all())
    {
        let _ = std::fs::remove_file(&lock_path);
        return Err(py_runtime_err(format!("write lock metadata failed: {e}")));
    }
    Ok((lock_path, lock_file))
}

#[pyclass(name = "NativeYiTraceDB")]
pub struct NativeYiTraceDb {
    coord: Arc<WriteCoordinator>,
    api: EngineJsonApi,
    lock_path: PathBuf,
    lock_file: Option<File>,
    closed: bool,
}

#[pymethods]
impl NativeYiTraceDb {
    #[new]
    pub fn new(py: Python<'_>, data_dir: String) -> PyResult<Self> {
        let dir = PathBuf::from(data_dir);
        let (lock_path, lock_file, coord) = py.detach(move || {
            let (lock_path, lock_file) = lock_data_dir(&dir)?;
            let coord = WriteCoordinator::open_durable(&dir)
                .map_err(|e| py_runtime_err(format!("open yiTrace data dir failed: {e}")))?;
            coord.recover();
            Ok::<_, PyErr>((lock_path, lock_file, coord))
        })?;
        let api = EngineJsonApi::new(Arc::clone(&coord));
        Ok(Self {
            coord,
            api,
            lock_path,
            lock_file: Some(lock_file),
            closed: false,
        })
    }

    #[pyo3(signature = (method, path, body = "", tenant_id = None))]
    pub fn route_json(
        &self,
        py: Python<'_>,
        method: &str,
        path: &str,
        body: &str,
        tenant_id: Option<String>,
    ) -> PyResult<String> {
        self.ensure_open()?;
        let tenant = parse_tenant_id(tenant_id)?;
        let api = self.api.clone();
        let method = method.to_string();
        let path = path.to_string();
        let body = body.to_string();
        let (status, response) = py.detach(move || api.route_with_tenant(&method, &path, &body, tenant));
        if (200..300).contains(&status) {
            Ok(response)
        } else {
            Err(py_runtime_err(format!(
                "yiTrace request failed: status={status} body={response}"
            )))
        }
    }

    pub fn flush(&self, py: Python<'_>) -> PyResult<()> {
        self.ensure_open()?;
        let coord = Arc::clone(&self.coord);
        py.detach(move || coord.flush_memtable());
        Ok(())
    }

    pub fn close(&mut self, py: Python<'_>) -> PyResult<()> {
        if self.closed {
            return Ok(());
        }
        let coord = Arc::clone(&self.coord);
        py.detach(move || coord.flush_memtable());
        self.lock_file.take();
        let _ = std::fs::remove_file(&self.lock_path);
        self.closed = true;
        Ok(())
    }

    fn ensure_open(&self) -> PyResult<()> {
        if self.closed {
            Err(py_runtime_err("YiTraceDB is closed"))
        } else {
            Ok(())
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

#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<NativeYiTraceDb>()?;
    Ok(())
}
