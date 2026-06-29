"""事件导出。把 SDK 产生的 span 事件送出去（控制台 / 批量到引擎摄入端）。"""
import abc
import json
import urllib.request

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

    def __init__(self, url: str = "http://127.0.0.1:7878/v1/ingest", max_batch: int = 256, timeout: float = 5.0) -> None:
        self.url = url
        self.max = max_batch
        self.timeout = timeout
        self._buf: list[SpanEvent] = []

    def export(self, event: SpanEvent) -> None:
        self._buf.append(event)
        if len(self._buf) >= self.max:
            self.flush()

    def export_batch(self, events: list[SpanEvent]) -> None:
        """整批一次 POST（覆盖默认逐条）——这是真正的批量传输。"""
        self._post(events)

    def flush(self) -> None:
        if not self._buf:
            return
        batch, self._buf = self._buf, []
        self._post(batch)

    def _post(self, events: list[SpanEvent]) -> None:
        if not events:
            return
        body = json.dumps([e.to_wire() for e in events]).encode("utf-8")
        req = urllib.request.Request(self.url, data=body, method="POST", headers={"Content-Type": "application/json"})
        urllib.request.urlopen(req, timeout=self.timeout).read()

    def close(self) -> None:
        self.flush()
