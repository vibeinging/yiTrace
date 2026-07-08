"""Client helpers for talking to yiTrace locally or over HTTP."""
from __future__ import annotations

import json
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any, Mapping
from urllib.parse import quote, urlencode

from .event import SpanEvent

TenantId = str | int


def _json_dumps(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"))


def _json_loads(value: bytes) -> Any:
    text = value.decode("utf-8") if value else ""
    return json.loads(text) if text else None


def _query_string(params: Mapping[str, Any] | None = None) -> str:
    if not params:
        return ""
    out: dict[str, str] = {}
    for key, value in params.items():
        if value is None or key in {"tenant_id", "tenantId"}:
            continue
        if isinstance(value, (dict, list)):
            out[key] = _json_dumps(value)
        else:
            out[key] = str(value)
    return urlencode(out)


def _append_query(path: str, query: str | None) -> str:
    if not query:
        return path
    return f"{path}{'&' if '?' in path else '?'}{query}"


def _wire_events(events: list[Mapping[str, Any] | SpanEvent]) -> list[dict[str, Any]]:
    out: list[dict[str, Any]] = []
    for event in events:
        if isinstance(event, SpanEvent):
            out.append(event.to_wire())
        elif hasattr(event, "to_wire"):
            out.append(event.to_wire())  # type: ignore[no-any-return]
        else:
            out.append(dict(event))
    return out


class YiTraceClient:
    """Small HTTP client for a running yiTrace service."""

    def __init__(
        self,
        url: str = "http://127.0.0.1:7878",
        *,
        token: str | None = None,
        tenant_id: TenantId | None = None,
        headers: Mapping[str, str] | None = None,
        timeout: float = 5.0,
    ) -> None:
        if not url:
            raise ValueError("YiTraceClient requires a url")
        self.url = url.rstrip("/")
        self.timeout = timeout
        self.headers = dict(headers or {})
        if token is not None:
            self.headers["Authorization"] = f"Bearer {token}"
        if tenant_id is not None:
            self.headers["X-Tenant-Id"] = str(tenant_id)

    def route_json(
        self,
        method: str,
        path: str,
        body: Any = "",
        *,
        tenant_id: TenantId | None = None,
    ) -> Any:
        req_headers = {"Accept": "application/json", **self.headers}
        if tenant_id is not None:
            req_headers["X-Tenant-Id"] = str(tenant_id)
        data: bytes | None
        if body is None or body == "":
            data = None
        elif isinstance(body, str):
            data = body.encode("utf-8")
            req_headers["Content-Type"] = "application/json"
        else:
            data = _json_dumps(body).encode("utf-8")
            req_headers["Content-Type"] = "application/json"
        req = urllib.request.Request(self._url_for(path), data=data, method=method.upper(), headers=req_headers)
        try:
            with urllib.request.urlopen(req, timeout=self.timeout) as resp:
                return _json_loads(resp.read())
        except urllib.error.HTTPError as err:
            detail = err.read().decode("utf-8", errors="replace")
            raise RuntimeError(f"yiTrace request failed: status={err.code} body={detail}") from err

    def ingest(self, events: list[Mapping[str, Any] | SpanEvent], *, tenant_id: TenantId | None = None) -> Any:
        return self.route_json("POST", "/v1/ingest", _wire_events(events), tenant_id=tenant_id)

    def ingest_otlp(self, body: Mapping[str, Any] | str, *, tenant_id: TenantId | None = None) -> Any:
        return self.route_json("POST", "/v1/traces", body, tenant_id=tenant_id)

    def search(self, query: Mapping[str, Any] | None = None, *, tenant_id: TenantId | None = None, **kwargs: Any) -> Any:
        body = dict(query or {})
        body.update(kwargs)
        return self.route_json("POST", "/v1/search", body, tenant_id=tenant_id)

    def trace_search(self, query: Mapping[str, Any] | None = None, *, tenant_id: TenantId | None = None, **kwargs: Any) -> Any:
        body = dict(query or {})
        body.update(kwargs)
        return self.route_json("POST", "/v1/trace-search", body, tenant_id=tenant_id)

    def trace_aggregate(self, query: Mapping[str, Any] | None = None, *, tenant_id: TenantId | None = None, **kwargs: Any) -> Any:
        body = dict(query or {})
        body.update(kwargs)
        return self.route_json("POST", "/v1/trace-aggregate", body, tenant_id=tenant_id)

    def storage_stats(self, query: Mapping[str, Any] | None = None, *, tenant_id: TenantId | None = None, **kwargs: Any) -> Any:
        body = dict(query or {})
        body.update(kwargs)
        return self.route_json("POST", "/v1/storage-stats", body, tenant_id=tenant_id)

    def traces(self, *, tenant_id: TenantId | None = None) -> Any:
        return self.route_json("GET", "/v1/traces", tenant_id=tenant_id)

    def sessions(self, cursor: int = 0, limit: int = 50, *, tenant_id: TenantId | None = None, **options: Any) -> Any:
        query = _query_string({"cursor": cursor, "limit": limit, **options})
        return self.route_json("GET", _append_query("/v1/sessions", query), tenant_id=tenant_id)

    def trace(self, trace_id: TenantId, *, tenant_id: TenantId | None = None) -> Any:
        return self.route_json("GET", f"/v1/traces/{quote(str(trace_id), safe='')}", tenant_id=tenant_id)

    def span(self, trace_id: TenantId, span_id: TenantId, *, tenant_id: TenantId | None = None) -> Any:
        trace_part = quote(str(trace_id), safe="")
        span_part = quote(str(span_id), safe="")
        return self.route_json("GET", f"/v1/traces/{trace_part}/spans/{span_part}", tenant_id=tenant_id)

    def tracer(self, *, node_id: int | None = None):
        from .exporter import HttpExporter
        from .tracer import Tracer

        return Tracer(exporter=HttpExporter(self._url_for("/v1/ingest"), headers=self.headers, timeout=self.timeout), node_id=node_id)

    def close(self) -> None:
        pass

    def _url_for(self, path: str) -> str:
        if path.startswith("http://") or path.startswith("https://"):
            return path
        normalized = path if path.startswith("/") else f"/{path}"
        if self.url.endswith("/v1") and normalized.startswith("/v1/"):
            normalized = normalized[3:]
        return f"{self.url}{normalized}"


def connect(
    target: str | Path | None = None,
    *,
    url: str | None = None,
    path: str | Path | None = None,
    data_dir: str | Path | None = None,
    tenant_id: TenantId | None = None,
    token: str | None = None,
    headers: Mapping[str, str] | None = None,
    timeout: float = 5.0,
    **options: Any,
) -> Any:
    """Open a yiTrace connection.

    Use ``connect(url="http://localhost:7878")`` for a running server, or
    ``connect(path="./data")`` for an embedded DB when ``yitrace-db`` is
    installed.
    """

    if target is not None:
        target_text = str(target)
        if target_text.startswith(("http://", "https://")):
            if url is not None:
                raise ValueError("connect received both target URL and url")
            url = target_text
        else:
            if path is not None or data_dir is not None:
                raise ValueError("connect received both target path and path/data_dir")
            path = target
    if url is not None and (path is not None or data_dir is not None):
        raise ValueError("connect accepts either url or path, not both")
    if url is not None:
        return YiTraceClient(url, token=token, tenant_id=tenant_id, headers=headers, timeout=timeout)
    local_path = data_dir if data_dir is not None else path
    if local_path is None:
        raise ValueError("connect requires url or path")
    try:
        from yitrace_db import YiTraceDB
    except ImportError as err:
        raise RuntimeError("local yiTrace connections require the optional yitrace-db package") from err
    return YiTraceDB.open(local_path, tenant_id=tenant_id, **options)


__all__ = ["YiTraceClient", "connect"]
