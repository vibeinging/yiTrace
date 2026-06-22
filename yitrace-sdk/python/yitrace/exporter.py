"""事件导出。把 SDK 产生的 span 事件送出去（控制台 / 批量到引擎摄入端）。"""
from __future__ import annotations

import abc
import json
import urllib.request

from .event import SpanEvent


class Exporter(abc.ABC):
    @abc.abstractmethod
    def export(self, event: SpanEvent) -> None:
        ...

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
    """攒批再发（真实部署：批量 POST 到引擎摄入端）。这里只攒批 + 留 `_send` 钩子。"""

    def __init__(self, sink: Exporter, max_batch: int = 256) -> None:
        self._sink = sink
        self._max = max_batch
        self._buf: list[SpanEvent] = []

    def export(self, event: SpanEvent) -> None:
        self._buf.append(event)
        if len(self._buf) >= self._max:
            self.flush()

    def flush(self) -> None:
        for e in self._buf:
            self._sink.export(e)  # TODO: 真实实现这里改成一次 HTTP/OTLP 批量请求
        self._buf.clear()

    def close(self) -> None:
        self.flush()
        self._sink.close()


class HttpExporter(Exporter):
    """攒批并 POST 到引擎摄入端 /v1/ingest（线格式 JSON 数组）。"""

    def __init__(self, url: str = "http://127.0.0.1:7878/v1/ingest", max_batch: int = 256, timeout: float = 5.0) -> None:
        self.url = url
        self.max = max_batch
        self.timeout = timeout
        self._buf: list[SpanEvent] = []

    def export(self, event: SpanEvent) -> None:
        self._buf.append(event)
        if len(self._buf) >= self.max:
            self.flush()

    def flush(self) -> None:
        if not self._buf:
            return
        body = json.dumps([e.to_wire() for e in self._buf]).encode("utf-8")
        req = urllib.request.Request(self.url, data=body, method="POST", headers={"Content-Type": "application/json"})
        urllib.request.urlopen(req, timeout=self.timeout).read()
        self._buf.clear()

    def close(self) -> None:
        self.flush()
