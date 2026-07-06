use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;

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

fn lock_data_dir(dir: &Path) -> PyResult<(PathBuf, File)> {
    std::fs::create_dir_all(dir)
        .map_err(|e| py_runtime_err(format!("create data dir failed: {e}")))?;
    let lock_path = dir.join(".yitrace.lock");
    let lock_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)
        .map_err(|e| {
            py_runtime_err(format!(
                "data dir is already open or locked: {} ({e})",
                lock_path.display()
            ))
        })?;
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
    pub fn new(data_dir: String) -> PyResult<Self> {
        let dir = PathBuf::from(data_dir);
        let (lock_path, lock_file) = lock_data_dir(&dir)?;
        let coord = WriteCoordinator::open_durable(&dir)
            .map_err(|e| py_runtime_err(format!("open yiTrace data dir failed: {e}")))?;
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

    #[pyo3(signature = (method, path, body = "", tenant_id = None))]
    pub fn route_json(
        &self,
        method: &str,
        path: &str,
        body: &str,
        tenant_id: Option<String>,
    ) -> PyResult<String> {
        self.ensure_open()?;
        let tenant = parse_tenant_id(tenant_id)?;
        let (status, response) = self.api.route_with_tenant(method, path, body, tenant);
        if (200..300).contains(&status) {
            Ok(response)
        } else {
            Err(py_runtime_err(format!(
                "yiTrace request failed: status={status} body={response}"
            )))
        }
    }

    pub fn flush(&self) -> PyResult<()> {
        self.ensure_open()?;
        self.coord.flush_memtable();
        Ok(())
    }

    pub fn close(&mut self) -> PyResult<()> {
        if self.closed {
            return Ok(());
        }
        self.coord.flush_memtable();
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
