"""事件导出。把 SDK 产生的 span 事件送出去（控制台 / 批量到引擎摄入端）。"""
from __future__ import annotations

import abc
import atexit
import json
import os
import queue
import sys
import threading
import time
import urllib.request
from pathlib import Path
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


class NoopExporter(Exporter):
    """静默丢弃事件。用于 fail-open，保证 trace 故障不影响主业务。"""

    def __init__(self) -> None:
        self._dropped = 0

    def export(self, event: SpanEvent) -> None:
        self._dropped += 1

    def export_batch(self, events: list[SpanEvent]) -> None:
        self._dropped += len(events)

    def dropped_count(self) -> int:
        return self._dropped


class DbExporter(Exporter):
    """把 SDK 事件直接写进 embedded YiTraceDB。"""

    def __init__(self, db, *, tenant_id: int | str | None = None) -> None:
        self.db = db
        self.tenant_id = tenant_id
        self._sent = 0

    def export(self, event: SpanEvent) -> None:
        self.export_batch([event])

    def export_batch(self, events: list[SpanEvent]) -> None:
        if not events:
            return
        self.db.ingest([event.to_wire() for event in events], tenant_id=self.tenant_id)
        self._sent += len(events)

    def sent_count(self) -> int:
        return self._sent


class BufferedDbExporter(Exporter):
    """服务端 embedded 默认写法：业务线程入队，后台单写线程串行写 YiTraceDB。"""

    def __init__(
        self,
        db,
        *,
        tenant_id: int | str | None = None,
        max_batch: int = 256,
        flush_interval: float = 0.25,
        max_queue: int = 8192,
        drop_when_full: bool = True,
        max_retries: int = 3,
        retry_interval: float = 0.1,
        on_error: Callable[[Exception, int], None] | None = None,
        register_atexit: bool = True,
    ) -> None:
        if max_batch <= 0:
            raise ValueError("max_batch must be > 0")
        if max_queue <= 0:
            raise ValueError("max_queue must be > 0")
        self.db = db
        self.tenant_id = tenant_id
        self.max_batch = max_batch
        self.flush_interval = flush_interval
        self.max_retries = max_retries
        self.retry_interval = retry_interval
        self.drop_when_full = drop_when_full
        self.on_error = on_error if on_error is not None else self._default_on_error
        self._queue: queue.Queue[SpanEvent] = queue.Queue(maxsize=max_queue)
        self._closed = threading.Event()
        self._lock = threading.Lock()
        self._sent = 0
        self._dropped = 0
        self._write_errors = 0
        self._last_error: str | None = None
        self._thread = threading.Thread(target=self._run, name="yitrace-db-writer", daemon=True)
        self._thread.start()
        if register_atexit:
            atexit.register(self.close)

    def export(self, event: SpanEvent) -> None:
        self.export_batch([event])

    def export_batch(self, events: list[SpanEvent]) -> None:
        if not events:
            return
        for event in events:
            if self._closed.is_set():
                self._record_drop(1)
                continue
            try:
                if self.drop_when_full:
                    self._queue.put_nowait(event)
                else:
                    self._queue.put(event)
            except queue.Full:
                self._record_drop(1)

    def flush(self, timeout: float | None = None) -> bool:
        """等待当前队列写完。返回 False 表示超时。"""
        if timeout is None:
            self._queue.join()
            return True
        deadline = time.monotonic() + timeout
        with self._queue.all_tasks_done:
            while self._queue.unfinished_tasks:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    return False
                self._queue.all_tasks_done.wait(remaining)
        return True

    def close(self, timeout: float | None = 5.0) -> None:
        if self._closed.is_set():
            return
        self._closed.set()
        self.flush(timeout=timeout)
        self._thread.join(timeout=timeout)

    def sent_count(self) -> int:
        with self._lock:
            return self._sent

    def dropped_count(self) -> int:
        with self._lock:
            return self._dropped

    def write_error_count(self) -> int:
        with self._lock:
            return self._write_errors

    def last_error(self) -> str | None:
        with self._lock:
            return self._last_error

    def queued_count(self) -> int:
        return self._queue.qsize()

    def _run(self) -> None:
        while not self._closed.is_set() or not self._queue.empty():
            batch: list[SpanEvent] = []
            try:
                batch.append(self._queue.get(timeout=self.flush_interval))
            except queue.Empty:
                continue
            while len(batch) < self.max_batch:
                try:
                    batch.append(self._queue.get_nowait())
                except queue.Empty:
                    break
            self._write_batch(batch)
            for _ in batch:
                self._queue.task_done()

    def _write_batch(self, batch: list[SpanEvent]) -> None:
        attempts = 0
        while True:
            try:
                self.db.ingest([event.to_wire() for event in batch], tenant_id=self.tenant_id)
                with self._lock:
                    self._sent += len(batch)
                return
            except Exception as err:
                attempts += 1
                with self._lock:
                    self._write_errors += 1
                    self._last_error = str(err)
                if attempts > self.max_retries:
                    self._record_drop(len(batch))
                    self.on_error(err, len(batch))
                    return
                self.on_error(err, 0)
                time.sleep(self.retry_interval)

    def _record_drop(self, n: int) -> None:
        if n <= 0:
            return
        with self._lock:
            self._dropped += n

    @staticmethod
    def _default_on_error(err: Exception, dropped: int) -> None:
        msg = f"[yitrace] embedded 写入失败: {err}"
        if dropped:
            msg += f" (dropped={dropped})"
        print(msg, file=sys.stderr)


def _ensure_spool_dirs(spool_dir: Path) -> dict[str, Path]:
    dirs = {
        "root": spool_dir,
        "tmp": spool_dir / "tmp",
        "ready": spool_dir / "ready",
        "inflight": spool_dir / "inflight",
        "done": spool_dir / "done",
        "dead": spool_dir / "dead",
    }
    for path in dirs.values():
        path.mkdir(parents=True, exist_ok=True)
    return dirs


def _fsync_dir(path: Path) -> None:
    if not hasattr(os, "O_DIRECTORY"):
        return
    try:
        fd = os.open(str(path), os.O_RDONLY | os.O_DIRECTORY)
    except OSError:
        return
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


class SpoolDbExporter(Exporter):
    """多 worker 本地写日志：worker 只写 spool 文件，不直接打开 YiTraceDB。"""

    def __init__(
        self,
        spool_dir: str | Path,
        *,
        tenant_id: int | str | None = None,
        max_batch: int = 256,
        fsync: bool = True,
        on_error: Callable[[Exception, int], None] | None = None,
    ) -> None:
        if max_batch <= 0:
            raise ValueError("max_batch must be > 0")
        self.spool_dir = Path(spool_dir)
        self.tenant_id = tenant_id
        self.max_batch = max_batch
        self.fsync = fsync
        self.on_error = on_error if on_error is not None else self._default_on_error
        self._dirs = _ensure_spool_dirs(self.spool_dir)
        self._lock = threading.Lock()
        self._buf: list[SpanEvent] = []
        self._counter = 0
        self._written = 0
        self._dropped = 0

    def export(self, event: SpanEvent) -> None:
        with self._lock:
            self._buf.append(event)
            if len(self._buf) < self.max_batch:
                return
            batch, self._buf = self._buf, []
        self._write_batch(batch)

    def export_batch(self, events: list[SpanEvent]) -> None:
        if not events:
            return
        self._write_batch(events)

    def flush(self) -> None:
        with self._lock:
            if not self._buf:
                return
            batch, self._buf = self._buf, []
        self._write_batch(batch)

    def close(self) -> None:
        self.flush()

    def written_count(self) -> int:
        with self._lock:
            return self._written

    def dropped_count(self) -> int:
        with self._lock:
            return self._dropped

    def _next_name(self) -> str:
        with self._lock:
            self._counter += 1
            counter = self._counter
        return f"{time.time_ns()}-{os.getpid()}-{counter}.json"

    def _write_batch(self, batch: list[SpanEvent]) -> None:
        if not batch:
            return
        name = self._next_name()
        tmp_path = self._dirs["tmp"] / f"{name}.tmp"
        ready_path = self._dirs["ready"] / name
        payload = {
            "version": 1,
            "created_ns": time.time_ns(),
            "pid": os.getpid(),
            "tenant_id": None if self.tenant_id is None else str(self.tenant_id),
            "events": [event.to_wire() for event in batch],
        }
        try:
            with tmp_path.open("w", encoding="utf-8") as f:
                json.dump(payload, f, ensure_ascii=False, separators=(",", ":"))
                f.write("\n")
                f.flush()
                if self.fsync:
                    os.fsync(f.fileno())
            os.replace(tmp_path, ready_path)
            if self.fsync:
                _fsync_dir(self._dirs["ready"])
            with self._lock:
                self._written += len(batch)
        except Exception as err:
            try:
                tmp_path.unlink(missing_ok=True)
            except Exception:
                pass
            with self._lock:
                self._dropped += len(batch)
            self.on_error(err, len(batch))

    @staticmethod
    def _default_on_error(err: Exception, dropped: int) -> None:
        msg = f"[yitrace] spool 写入失败: {err}"
        if dropped:
            msg += f" (dropped={dropped})"
        print(msg, file=sys.stderr)


class SpoolConsumer:
    """唯一消费者：读取 spool ready 文件，串行写入 embedded YiTraceDB。"""

    def __init__(
        self,
        db,
        spool_dir: str | Path,
        *,
        tenant_id: int | str | None = None,
        keep_done: bool = False,
        on_error: Callable[[Exception, Path], None] | None = None,
    ) -> None:
        self.db = db
        self.spool_dir = Path(spool_dir)
        self.tenant_id = tenant_id
        self.keep_done = keep_done
        self.on_error = on_error if on_error is not None else self._default_on_error
        self._dirs = _ensure_spool_dirs(self.spool_dir)
        self._consumed = 0
        self._dead = 0
        self._recover_inflight()

    def consume_once(self, *, limit: int | None = None) -> int:
        """消费一轮 ready 文件，返回成功写入 DB 的事件数。"""
        consumed = 0
        files = sorted(path for path in self._dirs["ready"].iterdir() if path.is_file())
        for ready_path in files:
            if limit is not None and consumed >= limit:
                break
            inflight_path = self._dirs["inflight"] / ready_path.name
            try:
                os.replace(ready_path, inflight_path)
            except FileNotFoundError:
                continue
            try:
                payload = self._read_payload(inflight_path)
            except Exception as err:
                self._move_to_dead(inflight_path)
                self.on_error(err, inflight_path)
                continue

            events = payload["events"]
            tenant_id = payload.get("tenant_id")
            if tenant_id is None:
                tenant_id = self.tenant_id
            try:
                self.db.ingest(events, tenant_id=tenant_id)
            except Exception as err:
                self._move_back_to_ready(inflight_path)
                self.on_error(err, inflight_path)
                break

            if self.keep_done:
                os.replace(inflight_path, self._dirs["done"] / inflight_path.name)
            else:
                inflight_path.unlink(missing_ok=True)
            consumed += len(events)
            self._consumed += len(events)
        return consumed

    def consumed_count(self) -> int:
        return self._consumed

    def dead_count(self) -> int:
        return self._dead

    def _read_payload(self, path: Path) -> dict:
        with path.open("r", encoding="utf-8") as f:
            payload = json.load(f)
        if not isinstance(payload, dict) or payload.get("version") != 1:
            raise ValueError(f"bad spool payload version: {path}")
        events = payload.get("events")
        if not isinstance(events, list):
            raise ValueError(f"bad spool payload events: {path}")
        return payload

    def _recover_inflight(self) -> None:
        for path in sorted(self._dirs["inflight"].iterdir()):
            if path.is_file():
                self._move_back_to_ready(path)

    def _move_back_to_ready(self, path: Path) -> None:
        os.replace(path, self._dirs["ready"] / path.name)

    def _move_to_dead(self, path: Path) -> None:
        os.replace(path, self._dirs["dead"] / path.name)
        self._dead += 1

    @staticmethod
    def _default_on_error(err: Exception, path: Path) -> None:
        print(f"[yitrace] spool 消费失败: {path}: {err}", file=sys.stderr)


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
