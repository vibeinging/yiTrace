use std::path::PathBuf;
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

#[pyclass(name = "NativeYiTraceDB")]
pub struct NativeYiTraceDb {
    coord: Arc<WriteCoordinator>,
    api: EngineJsonApi,
    closed: bool,
}

#[pymethods]
impl NativeYiTraceDb {
    #[new]
    pub fn new(py: Python<'_>, data_dir: String) -> PyResult<Self> {
        let dir = PathBuf::from(data_dir);
        let coord = py.detach(move || {
            std::fs::create_dir_all(&dir)
                .map_err(|e| py_runtime_err(format!("create data dir failed: {e}")))?;
            let coord = WriteCoordinator::open_durable(&dir)
                .map_err(|e| py_runtime_err(format!("open yiTrace data dir failed: {e}")))?;
            coord.recover();
            Ok::<_, PyErr>(coord)
        })?;
        let api = EngineJsonApi::new(Arc::clone(&coord));
        Ok(Self {
            coord,
            api,
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
        let (status, response) =
            py.detach(move || api.route_with_tenant(&method, &path, &body, tenant));
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
            self.closed = true;
        }
    }
}

#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<NativeYiTraceDb>()?;
    Ok(())
}
