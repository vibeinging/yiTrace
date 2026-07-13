"""Optional FastAPI adapter for yitrace-db."""
from __future__ import annotations

from contextlib import asynccontextmanager
import json
import re
from pathlib import Path
from typing import Any

from . import YiTraceDB

try:
    from fastapi import APIRouter, FastAPI, HTTPException, Request
    from fastapi.responses import JSONResponse
except ImportError:  # pragma: no cover - exercised by users without server extras
    APIRouter = None  # type: ignore[assignment]
    FastAPI = None  # type: ignore[assignment]
    HTTPException = None  # type: ignore[assignment]
    Request = None  # type: ignore[assignment]
    JSONResponse = None  # type: ignore[assignment]

_STATUS_RE = re.compile(r"status=(\d+)\s+body=(.*)", re.DOTALL)


def _require_fastapi() -> None:
    if APIRouter is None or FastAPI is None or HTTPException is None or JSONResponse is None:
        raise RuntimeError("FastAPI support requires: pip install 'yitrace-db[server]'")


def _path_with_query(api_path: str, query_string: bytes) -> str:
    path = "/" + api_path.lstrip("/")
    if query_string:
        path += "?" + query_string.decode("utf-8", errors="replace")
    return path


def _raise_http_error(err: RuntimeError) -> None:
    _require_fastapi()
    text = str(err)
    match = _STATUS_RE.search(text)
    if not match:
        raise HTTPException(status_code=500, detail=text)
    status = int(match.group(1))
    raw_body = match.group(2)
    try:
        detail: Any = json.loads(raw_body)
    except Exception:
        detail = raw_body
    raise HTTPException(status_code=status, detail=detail)


def create_yitrace_router(
    db: YiTraceDB,
    *,
    tenant_header: str = "X-Tenant-Id",
    default_tenant_id: str | int | None = None,
):
    """Expose a YiTraceDB handle through FastAPI.

    Mount it with a prefix, for example:

    ``app.include_router(create_yitrace_router(db), prefix="/yitrace")``
    """

    _require_fastapi()
    router = APIRouter()

    async def handle(api_path: str, request: Request):  # type: ignore[valid-type]
        body_bytes = await request.body()
        body = body_bytes.decode("utf-8") if body_bytes else ""
        tenant_id = request.headers.get(tenant_header)
        if tenant_id is None:
            tenant_id = default_tenant_id
        path = _path_with_query(api_path, request.scope.get("query_string", b""))
        try:
            payload = db.route_json(request.method, path, body, tenant_id=tenant_id)
        except RuntimeError as err:
            _raise_http_error(err)
        return JSONResponse(payload)

    router.add_api_route("/{api_path:path}", handle, methods=["GET", "POST", "PATCH", "DELETE"])
    return router


def create_yitrace_app(
    db_or_data_dir: YiTraceDB | str | Path,
    *,
    tenant_id: str | int | None = None,
    tenant_header: str = "X-Tenant-Id",
):
    """Create a small FastAPI app around an embedded yiTrace DB."""

    _require_fastapi()
    owns_db = not isinstance(db_or_data_dir, YiTraceDB)
    db = db_or_data_dir if isinstance(db_or_data_dir, YiTraceDB) else YiTraceDB.open(db_or_data_dir, tenant_id=tenant_id)
    if owns_db:
        @asynccontextmanager
        async def lifespan(_app):
            try:
                yield
            finally:
                db.close()

        app = FastAPI(title="yiTrace", lifespan=lifespan)
    else:
        app = FastAPI(title="yiTrace")
    app.include_router(create_yitrace_router(db, tenant_header=tenant_header, default_tenant_id=tenant_id))
    return app


__all__ = ["create_yitrace_app", "create_yitrace_router"]
