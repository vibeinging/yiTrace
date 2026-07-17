"""服务端接入 helper。"""
from __future__ import annotations

import atexit
import threading
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Mapping

from .client import connect
from .exporter import BufferedDbExporter, DbExporter, Exporter, HttpExporter, NoopExporter, SpoolDbExporter
from .tracer import Tracer


@dataclass
class YiTraceRuntime:
    """`init_yitrace` 返回的运行时句柄。"""

    tracer: Tracer
    exporter: Exporter
    db: Any | None = None
    enabled: bool = True
    error: Exception | None = None
    owns_db: bool = False
    mode: str = "unknown"
    data_dir: str | None = None
    spool_dir: str | None = None
    url: str | None = None
    requested_mode: str | None = None

    def close(self, timeout: float | None = 5.0) -> None:
        try:
            close = getattr(self.exporter, "close")
            try:
                close(timeout=timeout)
            except TypeError:
                close()
        finally:
            if self.owns_db and self.db is not None:
                self.db.close()

    def health(self) -> dict[str, Any]:
        exporter = _exporter_health(self.exporter)
        lock = _db_lock_health(self.db)
        last_error = str(self.error) if self.error is not None else exporter.get("last_error")
        return {
            "enabled": self.enabled,
            "mode": self.mode,
            "requested_mode": self.requested_mode,
            "data_dir": self.data_dir,
            "spool_dir": self.spool_dir,
            "url": self.url,
            "queue": exporter.get("queue", {"queued": None, "max": None}),
            "sent": exporter.get("sent"),
            "written": exporter.get("written"),
            "dropped": exporter.get("dropped", 0),
            "write_errors": exporter.get("write_errors", 0),
            "last_error": last_error,
            "lock": lock,
            "exporter": exporter,
        }


def _path_text(value: str | Path | None) -> str | None:
    return None if value is None else str(value)


def _requested_mode(
    *,
    url: str | None,
    path: str | Path | None,
    data_dir: str | Path | None,
    spool_dir: str | Path | None,
    mode: str,
) -> str:
    if url is not None:
        return "http"
    normalized = mode.replace("-", "_")
    if normalized == "spool" or spool_dir is not None:
        return "spool"
    if path is not None or data_dir is not None:
        return normalized
    return normalized


def _exporter_health(exporter: Exporter) -> dict[str, Any]:
    health = getattr(exporter, "health", None)
    if callable(health):
        value = health()
        if isinstance(value, dict):
            return value
    state: dict[str, Any] = {"type": exporter.__class__.__name__, "queue": {"queued": None, "max": None}}
    for key, method_name in (
        ("sent", "sent_count"),
        ("dropped", "dropped_count"),
        ("written", "written_count"),
        ("write_errors", "write_error_count"),
        ("last_error", "last_error"),
    ):
        method = getattr(exporter, method_name, None)
        if callable(method):
            try:
                state[key] = method()
            except Exception as err:
                state.setdefault("last_error", str(err))
    queued = getattr(exporter, "queued_count", None)
    if callable(queued):
        try:
            state["queue"] = {"queued": queued(), "max": None}
        except Exception as err:
            state.setdefault("last_error", str(err))
    return state


def _db_lock_health(db: Any | None) -> dict[str, Any]:
    if db is None:
        return {"enabled": False}
    lock_metrics = getattr(db, "lock_metrics", None)
    if not callable(lock_metrics):
        return {"enabled": None}
    try:
        metrics = lock_metrics()
    except Exception as err:
        return {"enabled": True, "error": str(err)}
    if isinstance(metrics, dict):
        return metrics
    return {"enabled": True, "raw": metrics}


_runtime_lock = threading.Lock()
_runtime: YiTraceRuntime | None = None


def init_yitrace(
    *,
    url: str | None = None,
    path: str | Path | None = None,
    data_dir: str | Path | None = None,
    spool_dir: str | Path | None = None,
    mode: str = "buffered",
    tenant_id: int | str | None = None,
    node_id: int | None = None,
    token: str | None = None,
    headers: Mapping[str, str] | None = None,
    fail_open: bool = True,
    register_atexit: bool = True,
    **options: Any,
) -> YiTraceRuntime:
    """初始化服务端 tracer。

    - `url=...`：发到独立 yiTrace server，使用 `HttpExporter`。
    - `path=...` / `data_dir=...`：打开 embedded DB，默认使用 `BufferedDbExporter`。
    - `mode="spool"` + `spool_dir=...`：只写本地 spool，不打开 embedded DB。
    - `fail_open=True`：初始化失败时返回 `NoopExporter`，主服务继续启动。
    """

    global _runtime
    try:
        runtime = _init_yitrace_strict(
            url=url,
            path=path,
            data_dir=data_dir,
            spool_dir=spool_dir,
            mode=mode,
            tenant_id=tenant_id,
            node_id=node_id,
            token=token,
            headers=headers,
            register_atexit=register_atexit,
            **options,
        )
    except Exception as err:
        if not fail_open:
            raise
        exporter = NoopExporter()
        runtime = YiTraceRuntime(
            tracer=Tracer(exporter=exporter, node_id=node_id),
            exporter=exporter,
            enabled=False,
            error=err,
            mode="noop",
            requested_mode=_requested_mode(
                url=url,
                path=path,
                data_dir=data_dir,
                spool_dir=spool_dir,
                mode=mode,
            ),
            data_dir=_path_text(data_dir if data_dir is not None else path),
            spool_dir=_path_text(spool_dir),
            url=url,
        )
    with _runtime_lock:
        _runtime = runtime
    if register_atexit:
        atexit.register(shutdown_yitrace)
    return runtime


def _init_yitrace_strict(
    *,
    url: str | None,
    path: str | Path | None,
    data_dir: str | Path | None,
    spool_dir: str | Path | None,
    mode: str,
    tenant_id: int | str | None,
    node_id: int | None,
    token: str | None,
    headers: Mapping[str, str] | None,
    register_atexit: bool,
    **options: Any,
) -> YiTraceRuntime:
    if url is not None:
        if path is not None or data_dir is not None or spool_dir is not None:
            raise ValueError("url mode cannot be combined with path/data_dir/spool_dir")
        base_url = url.rstrip("/")
        if base_url.endswith("/v1/ingest"):
            ingest_url = base_url
        elif base_url.endswith("/v1"):
            ingest_url = f"{base_url}/ingest"
        else:
            ingest_url = f"{base_url}/v1/ingest"
        exporter = HttpExporter(
            ingest_url,
            token=token,
            tenant_id=tenant_id,
            headers=dict(headers or {}),
            timeout=options.pop("timeout", 5.0),
            max_batch=options.pop("max_batch", 256),
        )
        return YiTraceRuntime(
            tracer=Tracer(exporter=exporter, node_id=node_id),
            exporter=exporter,
            mode="http",
            requested_mode="http",
            url=ingest_url,
        )

    normalized_mode = mode.replace("-", "_")
    if normalized_mode == "spool":
        if spool_dir is None:
            raise ValueError('mode="spool" requires spool_dir')
        exporter = SpoolDbExporter(
            spool_dir,
            tenant_id=tenant_id,
            max_batch=options.pop("max_batch", 256),
            fsync=options.pop("fsync", True),
        )
        return YiTraceRuntime(
            tracer=Tracer(exporter=exporter, node_id=node_id),
            exporter=exporter,
            mode="spool",
            requested_mode="spool",
            spool_dir=_path_text(spool_dir),
        )

    local_path = data_dir if data_dir is not None else path
    if local_path is None:
        raise ValueError("init_yitrace requires url, path/data_dir, or mode='spool' with spool_dir")
    db = connect(path=local_path, tenant_id=tenant_id, **options.pop("connect_options", {}))
    if normalized_mode == "direct":
        exporter = DbExporter(db, tenant_id=tenant_id)
    elif normalized_mode == "buffered":
        exporter = BufferedDbExporter(
            db,
            tenant_id=tenant_id,
            max_batch=options.pop("max_batch", 256),
            flush_interval=options.pop("flush_interval", 1.0),
            max_queue=options.pop("max_queue", 8192),
            drop_when_full=options.pop("drop_when_full", True),
            max_retries=options.pop("max_retries", 3),
            retry_interval=options.pop("retry_interval", 0.1),
            register_atexit=register_atexit,
        )
    else:
        db.close()
        raise ValueError(f"unknown yitrace mode: {mode}")
    return YiTraceRuntime(
        tracer=Tracer(exporter=exporter, node_id=node_id),
        exporter=exporter,
        db=db,
        owns_db=True,
        mode=normalized_mode,
        requested_mode=normalized_mode,
        data_dir=_path_text(local_path),
    )


def get_yitrace_runtime() -> YiTraceRuntime | None:
    with _runtime_lock:
        return _runtime


def shutdown_yitrace(timeout: float | None = 5.0) -> None:
    global _runtime
    with _runtime_lock:
        runtime = _runtime
        _runtime = None
    if runtime is not None:
        runtime.close(timeout=timeout)


__all__ = ["YiTraceRuntime", "get_yitrace_runtime", "init_yitrace", "shutdown_yitrace"]
