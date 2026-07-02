"""事件导出。把 SDK 产生的 span 事件送出去（控制台 / 批量到引擎摄入端）。"""
from __future__ import annotations

import abc
import json
import sys
import urllib.request
from typing import Callable

from .event import SpanEvent


class Exporter(abc.ABC):
    @abc.abstractmethod
    def export(self, event: SpanEvent) -> None:
        ...

    def export_batch(self, events: list[SpanEvent]) -> None:
        """一次收一批。默认逐条转 `export`；能真正批量的传输（HttpExporter）覆盖成单次请求。"""
        for e in events:
            self.export(e)

    def close(self) -> None:
        pass


class ConsoleExporter(Exporter):
    """打印成 JSON 行（开发/调试用）。"""

    def export(self, event: SpanEvent) -> None:
        print(json.dumps(event.to_wire(), ensure_ascii=False))


class CollectingExporter(Exporter):
    """收集到内存（测试用）。"""

    def __init__(self) -> None:
        self.events: list[SpanEvent] = []

    def export(self, event: SpanEvent) -> None:
        self.events.append(event)


class BatchExporter(Exporter):
    """攒批缓冲装饰器：攒够一批就**整批**交给下游 sink 的 `export_batch`（一次请求/一次落盘），
    不再逐条转。要批量 HTTP 直接用 `HttpExporter`（它本身就攒批）；要给任意 sink 加攒批语义才套这个。"""

    def __init__(self, sink: Exporter, max_batch: int = 256) -> None:
        self._sink = sink
        self._max = max_batch
        self._buf: list[SpanEvent] = []

    def export(self, event: SpanEvent) -> None:
        self._buf.append(event)
        if len(self._buf) >= self._max:
            self.flush()

    def flush(self) -> None:
        if not self._buf:
            return
        batch, self._buf = self._buf, []
        self._sink.export_batch(batch)  # 整批一次交下游（sink 能批就批）

    def close(self) -> None:
        self.flush()
        self._sink.close()


class HttpExporter(Exporter):
    """攒批并 POST 到引擎摄入端 /v1/ingest（线格式 JSON 数组）。"""

    def __init__(
        self,
        url: str = "http://127.0.0.1:7878/v1/ingest",
        max_batch: int = 256,
        timeout: float = 5.0,
        headers: dict[str, str] | None = None,
        token: str | None = None,
        tenant_id: int | str | None = None,
        max_buffered: int | None = None,
        on_error: Callable[[Exception, int], None] | None = None,
    ) -> None:
        self.url = url
        self.max = max_batch
        self.timeout = timeout
        self.max_buffered = max_buffered if max_buffered is not None else max_batch * 16
        self.on_error = on_error if on_error is not None else self._default_on_error
        self.headers = dict(headers or {})
        if token is not None:
            self.headers["Authorization"] = f"Bearer {token}"
        if tenant_id is not None:
            self.headers["X-Tenant-Id"] = str(tenant_id)
        self._buf: list[SpanEvent] = []
        self._sent = 0
        self._dropped = 0

    def export(self, event: SpanEvent) -> None:
        self._buf.append(event)
        if len(self._buf) >= self.max:
            self.flush()

    def export_batch(self, events: list[SpanEvent]) -> None:
        """整批一次 POST（覆盖默认逐条）——这是真正的批量传输。"""
        self._post_or_buffer(events)

    def flush(self) -> None:
        if not self._buf:
            return
        batch, self._buf = self._buf, []
        self._post_or_buffer(batch)

    def buffered_count(self) -> int:
        """当前等待重试的事件数（监控/测试用）。"""
        return len(self._buf)

    def sent_count(self) -> int:
        """已确认成功 POST 的事件数。"""
        return self._sent

    def dropped_count(self) -> int:
        """因超过最大缓冲上限而丢弃的事件数。"""
        return self._dropped

    def _post(self, events: list[SpanEvent]) -> None:
        if not events:
            return
        body = json.dumps([e.to_wire() for e in events]).encode("utf-8")
        req_headers = {"Content-Type": "application/json", **self.headers}
        req = urllib.request.Request(self.url, data=body, method="POST", headers=req_headers)
        urllib.request.urlopen(req, timeout=self.timeout).read()
        self._sent += len(events)

    def _post_or_buffer(self, events: list[SpanEvent]) -> None:
        if not events:
            return
        try:
            self._post(events)
        except Exception as err:
            # POST 是批级别的全有或全无。失败时退回队首；引擎端用确定性 event_id 去重，
            # 所以网络"已达但响应丢"时重试仍是安全的 at-least-once 语义。
            self._buf = list(events) + self._buf
            dropped = 0
            if len(self._buf) > self.max_buffered:
                dropped = len(self._buf) - self.max_buffered
                self._buf = self._buf[dropped:]
                self._dropped += dropped
            self.on_error(err, dropped)

    @staticmethod
    def _default_on_error(err: Exception, dropped: int) -> None:
        msg = f"[yitrace] 上报失败: {err}"
        if dropped:
            msg += f" (dropped={dropped})"
        print(msg, file=sys.stderr)

    def close(self) -> None:
        self.flush()
