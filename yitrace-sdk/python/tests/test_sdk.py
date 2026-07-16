"""SDK 测试。可直接 `python3 tests/test_sdk.py` 跑，也兼容 pytest。"""
import builtins
import os
import sys
import tempfile
import types

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from yitrace import (  # noqa: E402
    BufferedDbExporter,
    CollectingExporter,
    DbExporter,
    EventType,
    HttpExporter,
    NoopExporter,
    SpoolConsumer,
    SpoolDbExporter,
    Tracer,
    YiTraceClient,
    connect,
    event_id,
    get_yitrace_runtime,
    init_yitrace,
    shutdown_yitrace,
)

# 引擎基准值：cargo run -p yt-core --example print_event_id
ENGINE_BASELINE = {
    ("demo-span", 7, EventType.SPAN_END): 16098495313036060864,
    ("1002-1", 1, EventType.SPAN_START): 3941713543033365492,
    ("反洗钱-1", 3, EventType.ATTR): 13462389519714918643,
}


def test_event_id_matches_engine_byte_for_byte():
    # 与 Rust 引擎逐字节一致（含中文）——SDK↔引擎去重对得上的根。
    for (ext, seq, et), expect in ENGINE_BASELINE.items():
        assert event_id(ext, seq, et) == expect, f"{ext}|{seq}|{et.name}"


def test_event_id_is_deterministic_and_sensitive():
    assert event_id("s", 7, EventType.SPAN_END) == event_id("s", 7, EventType.SPAN_END)
    assert event_id("s", 7, EventType.SPAN_END) != event_id("s", 8, EventType.SPAN_END)  # seq
    assert event_id("s", 7, EventType.SPAN_END) != event_id("s", 7, EventType.SPAN_START)  # 类型
    assert event_id("s", 7, EventType.SPAN_END) != event_id("t", 7, EventType.SPAN_END)  # 身份


def test_span_produces_start_log_end():
    exp = CollectingExporter()
    tr = Tracer(exporter=exp, node_id=1)
    with tr.trace("反洗钱筛查") as t:
        with t.span("调用LLM研判") as s:
            s.log("研判结论 需人工复核")
            s.set_status(0)

    evs = exp.events
    assert [e.event_type for e in evs] == [EventType.SPAN_START, EventType.LOG, EventType.SPAN_END]
    assert [e.seq for e in evs] == [1, 2, 3], "seq 在 span 内单调递增"
    assert all(e.ext_span_id == evs[0].ext_span_id for e in evs), "同一 span 身份一致"
    assert evs[0].span_name == "调用LLM研判" and evs[0].logs == [], "start 用独立字段带 span 名"
    assert all(e.span_name is None for e in evs[1:]), "名字只在 start 上报"
    assert evs[1].logs == ["研判结论 需人工复核"]
    assert evs[2].status == 0 and evs[2].duration_ns is not None and evs[2].duration_ns >= 0
    assert len({e.event_id() for e in evs}) == 3, "三个事件 event_id 互不相同"


def test_nested_spans_set_parent():
    exp = CollectingExporter()
    tr = Tracer(exporter=exp, node_id=1)
    with tr.trace("反洗钱筛查") as t:
        with t.span("root") as root:
            with root.span("child"):
                pass

    starts = [e for e in exp.events if e.event_type == EventType.SPAN_START]
    root_start = next(e for e in starts if e.span_name == "root")
    child_start = next(e for e in starts if e.span_name == "child")
    assert root_start.parent_span_id is None, "根 span 无父"
    assert child_start.parent_span_id == root_start.span_id, "子 span 的父是 root"
    assert "parent_span_id" in child_start.to_wire(), "线格式带 parent_span_id"


def test_set_tokens_emits_and_wires():
    exp = CollectingExporter()
    tr = Tracer(exporter=exp, node_id=1)
    with tr.trace("x") as t:
        with t.span("llm") as s:
            s.set_tokens(input_tokens=1200, output_tokens=340)
    end = next(e for e in exp.events if e.event_type == EventType.SPAN_END)
    assert end.input_tokens == 1200 and end.output_tokens == 340
    assert end.to_wire()["input_tokens"] == 1200, "token 进线格式"


def test_session_agent_and_eval_io_fields_wire_through():
    # 会话 id 从 trace 透传到所有 span；agent/tool/model + eval 输入输出文本都进线格式。
    exp = CollectingExporter()
    tr = Tracer(exporter=exp, node_id=1)
    with tr.trace("多轮对话", session_id=9000) as t:
        with t.span("规划") as s:
            s.set_agent("规划")
            s.set_model("qwen3")
            s.set_io(input_text="请研判这笔交易", output_text="判定为疑似盗刷")
            with s.span("查工具") as tool:
                tool.set_tool("kb_lookup")

    starts = [e for e in exp.events if e.event_type == EventType.SPAN_START]
    # 会话 id 透传到嵌套 span
    assert all(e.session_id == 9000 for e in exp.events), "会话 id 透传到本 trace 全部事件（含嵌套）"
    end = next(e for e in exp.events if e.event_type == EventType.SPAN_END and e.model == "qwen3")
    assert end.model == "qwen3"
    assert end.input_text == "请研判这笔交易"
    assert end.output_text == "判定为疑似盗刷"
    w = end.to_wire()
    assert w["session_id"] == 9000 and w["agent_name"] == "规划" and w["output_text"] == "判定为疑似盗刷"
    # 子 span 带 tool_name
    tool_end = next(e for e in exp.events if e.event_type == EventType.SPAN_END and e.tool_name == "kb_lookup")
    assert tool_end.session_id == 9000, "子 span 也继承会话 id"


def test_display_name_is_optional_and_agent_context_is_inherited():
    exp = CollectingExporter()
    tr = Tracer(exporter=exp, node_id=1, agent_name="planner_agent")
    with tr.trace("x") as t:
        with t.span("planner.route", display_name="  规划下一步  "):
            pass
        with t.span("普通名字"):
            pass

    starts = [e for e in exp.events if e.event_type == EventType.SPAN_START]
    advanced = next(e for e in starts if e.span_name == "planner.route")
    simple = next(e for e in starts if e.span_name == "普通名字")
    assert advanced.display_name == "规划下一步"
    assert simple.display_name is None
    assert all(e.agent_name == "planner_agent" for e in exp.events)
    assert advanced.to_wire()["display_name"] == "规划下一步"


def test_exception_marks_error_status():
    exp = CollectingExporter()
    tr = Tracer(exporter=exp, node_id=1)
    try:
        with tr.trace("x") as t:
            with t.span("y"):
                raise ValueError("boom")
    except ValueError:
        pass
    end = [e for e in exp.events if e.event_type == EventType.SPAN_END][0]
    assert end.status == 1, "异常退出 → 状态非0"


def test_batch_exporter_hands_off_whole_batch_once():
    # BatchExporter 攒够一批 → 整批一次交下游 export_batch（不是逐条 export）。
    from yitrace.event import SpanEvent  # noqa: E402
    from yitrace.exporter import BatchExporter, Exporter  # noqa: E402

    class RecordingSink(Exporter):
        def __init__(self):
            self.batches = []
            self.single = 0

        def export(self, e):
            self.single += 1

        def export_batch(self, events):
            self.batches.append(len(events))

    sink = RecordingSink()
    be = BatchExporter(sink, max_batch=3)
    for i in range(7):
        be.export(SpanEvent(trace_id=1, span_id=i, parent_span_id=None, seq=1,
                            event_type=EventType.SPAN_START, ext_span_id=f"s{i}", ts=i))
    be.close()  # flush 余下的
    assert sink.single == 0, "整批走 export_batch,不逐条 export"
    assert sink.batches == [3, 3, 1], "攒满 3 各发一批,剩 1 在 close 时发"


def test_http_exporter_sends_auth_and_tenant_headers():
    from yitrace.event import SpanEvent  # noqa: E402
    import urllib.request  # noqa: E402

    captured = {}
    old_urlopen = urllib.request.urlopen

    class Resp:
        def read(self):
            return b"{}"

    def fake_urlopen(req, timeout):
        captured["timeout"] = timeout
        captured["headers"] = dict(req.header_items())
        return Resp()

    try:
        urllib.request.urlopen = fake_urlopen
        exp = HttpExporter("http://example.invalid/v1/ingest", token="secret", tenant_id=7, timeout=1.5)
        exp.export_batch([
            SpanEvent(trace_id=1, span_id=1, parent_span_id=None, seq=1,
                      event_type=EventType.SPAN_START, ext_span_id="s1", ts=1)
        ])
    finally:
        urllib.request.urlopen = old_urlopen

    assert captured["timeout"] == 1.5
    headers = {k.lower(): v for k, v in captured["headers"].items()}
    assert headers["authorization"] == "Bearer secret"
    assert headers["x-tenant-id"] == "7"


def test_http_exporter_buffers_failed_batches_and_retries():
    from yitrace.event import SpanEvent  # noqa: E402
    import urllib.request  # noqa: E402

    calls = {"n": 0}
    bodies = []
    errors = []
    old_urlopen = urllib.request.urlopen

    class Resp:
        def read(self):
            return b"{}"

    def fake_urlopen(req, timeout):
        calls["n"] += 1
        bodies.append(req.data)
        if calls["n"] == 1:
            raise OSError("network down")
        return Resp()

    ev = SpanEvent(trace_id=1, span_id=1, parent_span_id=None, seq=1,
                   event_type=EventType.SPAN_START, ext_span_id="s1", ts=1)
    try:
        urllib.request.urlopen = fake_urlopen
        exp = HttpExporter("http://example.invalid/v1/ingest", max_batch=10, on_error=lambda err, dropped: errors.append((str(err), dropped)))
        exp.export_batch([ev])
        assert exp.buffered_count() == 1
        assert errors == [("network down", 0)]
        exp.flush()
        assert exp.buffered_count() == 0
        assert exp.sent_count() == 1
        assert calls["n"] == 2
        assert bodies[0] == bodies[1], "失败批次应原样重试"
    finally:
        urllib.request.urlopen = old_urlopen


def test_http_exporter_caps_buffer_and_reports_dropped():
    from yitrace.event import SpanEvent  # noqa: E402
    import urllib.request  # noqa: E402

    errors = []
    old_urlopen = urllib.request.urlopen

    def fake_urlopen(req, timeout):
        raise OSError("still down")

    def ev(i):
        return SpanEvent(trace_id=1, span_id=i, parent_span_id=None, seq=1,
                         event_type=EventType.SPAN_START, ext_span_id=f"s{i}", ts=i)

    try:
        urllib.request.urlopen = fake_urlopen
        exp = HttpExporter(
            "http://example.invalid/v1/ingest",
            max_batch=10,
            max_buffered=2,
            on_error=lambda err, dropped: errors.append(dropped),
        )
        exp.export_batch([ev(1), ev(2), ev(3)])
        assert exp.buffered_count() == 2
        assert exp.dropped_count() == 1
        assert errors == [1], "超过上限应丢最老事件并上报 dropped"
    finally:
        urllib.request.urlopen = old_urlopen


def test_db_exporter_writes_tracer_events_to_embedded_db_handle():
    class FakeDb:
        def __init__(self):
            self.calls = []

        def ingest(self, events, tenant_id=None):
            self.calls.append((events, tenant_id))
            return {"ingested": len(events)}

    db = FakeDb()
    exporter = DbExporter(db, tenant_id=7)
    tracer = Tracer(exporter=exporter, node_id=1)
    with tracer.trace("local", tenant_id=7) as trace:
        with trace.span("span") as span:
            span.log("hello")
    tracer.close()

    assert exporter.sent_count() == 3
    assert len(db.calls) == 3
    assert all(call[1] == 7 for call in db.calls)
    assert [call[0][0]["event_type"] for call in db.calls] == [
        EventType.SPAN_START.value,
        EventType.LOG.value,
        EventType.SPAN_END.value,
    ]


def test_buffered_db_exporter_writes_from_background_thread():
    from yitrace.event import SpanEvent  # noqa: E402

    class FakeDb:
        def __init__(self):
            self.calls = []

        def ingest(self, events, tenant_id=None):
            self.calls.append((events, tenant_id))
            return {"ingested": len(events)}

    db = FakeDb()
    exporter = BufferedDbExporter(
        db,
        tenant_id=7,
        max_batch=10,
        flush_interval=0.01,
        register_atexit=False,
    )
    events = [
        SpanEvent(trace_id=1, span_id=i, parent_span_id=None, seq=1,
                  event_type=EventType.SPAN_START, ext_span_id=f"s{i}", ts=i)
        for i in range(3)
    ]
    exporter.export_batch(events)
    assert exporter.flush(timeout=2.0)
    exporter.close()

    written = [event for batch, _tenant in db.calls for event in batch]
    assert len(written) == 3
    assert all(tenant == 7 for _batch, tenant in db.calls)
    assert exporter.sent_count() == 3
    assert exporter.dropped_count() == 0


def test_buffered_db_exporter_drops_after_retry_budget():
    from yitrace.event import SpanEvent  # noqa: E402

    class FailingDb:
        def ingest(self, events, tenant_id=None):
            raise RuntimeError("db down")

    errors = []
    exporter = BufferedDbExporter(
        FailingDb(),
        max_batch=10,
        flush_interval=0.01,
        retry_interval=0.01,
        max_retries=1,
        on_error=lambda err, dropped: errors.append((str(err), dropped)),
        register_atexit=False,
    )
    exporter.export(
        SpanEvent(trace_id=1, span_id=1, parent_span_id=None, seq=1,
                  event_type=EventType.SPAN_START, ext_span_id="s1", ts=1)
    )
    assert exporter.flush(timeout=2.0)
    exporter.close()

    assert exporter.sent_count() == 0
    assert exporter.dropped_count() == 1
    assert exporter.write_error_count() == 2
    assert errors[-1] == ("db down", 1)


def test_buffered_db_exporter_survives_native_base_exception_and_keeps_consuming():
    from yitrace.event import SpanEvent  # noqa: E402

    class RecoveringDb:
        def __init__(self):
            self.calls = 0
            self.written = []

        def ingest(self, events, tenant_id=None):
            self.calls += 1
            if self.calls == 1:
                raise SystemExit("native write exit")
            self.written.extend(events)
            return {"ingested": len(events)}

    db = RecoveringDb()
    exporter = BufferedDbExporter(
        db,
        max_batch=1,
        flush_interval=0.01,
        retry_interval=0.01,
        max_retries=1,
        register_atexit=False,
    )
    events = [
        SpanEvent(trace_id=1, span_id=i, parent_span_id=None, seq=1,
                  event_type=EventType.SPAN_START, ext_span_id=f"s{i}", ts=i)
        for i in range(2)
    ]

    exporter.export_batch(events)
    assert exporter.flush(timeout=2.0)
    health = exporter.health()
    exporter.close()

    assert health["thread_alive"] is True
    assert exporter.sent_count() == 2
    assert exporter.dropped_count() == 0
    assert exporter.write_error_count() == 1
    assert len(db.written) == 2


def test_buffered_db_exporter_keeps_consuming_after_drop_and_callback_error():
    from yitrace.event import SpanEvent  # noqa: E402

    class RecoveringDb:
        def __init__(self):
            self.calls = 0
            self.written = []

        def ingest(self, events, tenant_id=None):
            self.calls += 1
            if self.calls <= 2:
                raise RuntimeError("db down")
            self.written.extend(events)
            return {"ingested": len(events)}

    def failing_error_callback(_err, _dropped):
        raise SystemExit("callback exit")

    db = RecoveringDb()
    exporter = BufferedDbExporter(
        db,
        max_batch=1,
        flush_interval=0.01,
        retry_interval=0.01,
        max_retries=1,
        on_error=failing_error_callback,
        register_atexit=False,
    )
    events = [
        SpanEvent(trace_id=1, span_id=i, parent_span_id=None, seq=1,
                  event_type=EventType.SPAN_START, ext_span_id=f"s{i}", ts=i)
        for i in range(2)
    ]

    exporter.export_batch(events)
    assert exporter.flush(timeout=2.0)
    health = exporter.health()
    exporter.close()

    assert health["thread_alive"] is True
    assert exporter.sent_count() == 1
    assert exporter.dropped_count() == 1
    assert exporter.write_error_count() == 2
    assert len(db.written) == 1


def test_spool_exporter_writes_files_and_consumer_ingests_once():
    from pathlib import Path  # noqa: E402
    from yitrace.event import SpanEvent  # noqa: E402

    class FakeDb:
        def __init__(self):
            self.calls = []

        def ingest(self, events, tenant_id=None):
            self.calls.append((events, tenant_id))
            return {"ingested": len(events)}

    with tempfile.TemporaryDirectory() as tmp:
        exporter = SpoolDbExporter(tmp, tenant_id=3, max_batch=10, fsync=False)
        exporter.export_batch([
            SpanEvent(trace_id=1, span_id=1, parent_span_id=None, seq=1,
                      event_type=EventType.SPAN_START, ext_span_id="s1", ts=1),
            SpanEvent(trace_id=1, span_id=1, parent_span_id=None, seq=2,
                      event_type=EventType.LOG, ext_span_id="s1", ts=2, logs=["hello"]),
        ])
        exporter.close()

        assert exporter.written_count() == 2
        assert len(list((Path(tmp) / "ready").iterdir())) == 1

        db = FakeDb()
        consumer = SpoolConsumer(db, tmp)
        assert consumer.consume_once() == 2
        assert consumer.consumed_count() == 2
        assert not list((Path(tmp) / "ready").iterdir())
        assert db.calls[0][1] == "3"
        assert [event["event_type"] for event in db.calls[0][0]] == [
            EventType.SPAN_START.value,
            EventType.LOG.value,
        ]


def test_spool_consumer_keeps_ready_file_when_db_write_fails():
    from pathlib import Path  # noqa: E402
    from yitrace.event import SpanEvent  # noqa: E402

    class FlakyDb:
        def __init__(self):
            self.calls = 0

        def ingest(self, events, tenant_id=None):
            self.calls += 1
            if self.calls == 1:
                raise RuntimeError("temporary down")
            return {"ingested": len(events)}

    with tempfile.TemporaryDirectory() as tmp:
        exporter = SpoolDbExporter(tmp, fsync=False)
        exporter.export(
            SpanEvent(trace_id=1, span_id=1, parent_span_id=None, seq=1,
                      event_type=EventType.SPAN_START, ext_span_id="s1", ts=1)
        )
        exporter.close()

        errors = []
        db = FlakyDb()
        consumer = SpoolConsumer(db, tmp, on_error=lambda err, path: errors.append((str(err), path.name)))
        assert consumer.consume_once() == 0
        assert errors[0][0] == "temporary down"
        assert len(list((Path(tmp) / "ready").iterdir())) == 1

        assert consumer.consume_once() == 1
        assert not list((Path(tmp) / "ready").iterdir())
        assert db.calls == 2


def test_init_yitrace_buffered_runtime_opens_db_and_closes_it():
    class FakeDb:
        def __init__(self):
            self.calls = []
            self.closed = False

        def ingest(self, events, tenant_id=None):
            self.calls.append((events, tenant_id))
            return {"ingested": len(events)}

        def lock_metrics(self):
            return {"enabled": True, "wait_count": 2, "wait_ms": 3.5}

        def close(self):
            self.closed = True

    opened = {}

    class FakeYiTraceDB:
        @classmethod
        def open(cls, path, **options):
            opened["path"] = str(path)
            opened["options"] = options
            return FakeDb()

    old_module = sys.modules.get("yitrace_db")
    sys.modules["yitrace_db"] = types.SimpleNamespace(YiTraceDB=FakeYiTraceDB)
    try:
        runtime = init_yitrace(
            path="./data",
            tenant_id=5,
            node_id=1,
            flush_interval=0.01,
            register_atexit=False,
        )
        with runtime.tracer.trace("service", tenant_id=5) as trace:
            with trace.span("span") as span:
                span.log("hello")
        runtime.close()
    finally:
        shutdown_yitrace()
        if old_module is None:
            sys.modules.pop("yitrace_db", None)
        else:
            sys.modules["yitrace_db"] = old_module

    assert runtime.enabled is True
    assert opened == {"path": "./data", "options": {"tenant_id": 5}}
    assert runtime.db.closed is True
    assert runtime.exporter.sent_count() == 3
    assert all(tenant == 5 for _events, tenant in runtime.db.calls)
    health = runtime.health()
    assert health["enabled"] is True
    assert health["mode"] == "buffered"
    assert health["data_dir"] == "./data"
    assert health["queue"]["queued"] == 0
    assert health["sent"] == 3
    assert health["dropped"] == 0
    assert health["last_error"] is None
    assert health["lock"]["wait_count"] == 2
    assert health["lock"]["wait_ms"] == 3.5


def test_init_yitrace_fail_open_returns_noop_runtime():
    real_import = builtins.__import__

    def fake_import(name, *args, **kwargs):
        if name == "yitrace_db":
            raise ImportError("missing yitrace_db")
        return real_import(name, *args, **kwargs)

    try:
        builtins.__import__ = fake_import
        runtime = init_yitrace(path="./data", node_id=1, fail_open=True, register_atexit=False)
        with runtime.tracer.trace("service") as trace:
            with trace.span("span") as span:
                span.log("hello")
        runtime.close()
    finally:
        shutdown_yitrace()
        builtins.__import__ = real_import

    assert runtime.enabled is False
    assert isinstance(runtime.exporter, NoopExporter)
    assert runtime.exporter.dropped_count() == 3
    assert "pip install yitrace-db" in str(runtime.error)
    health = runtime.health()
    assert health["enabled"] is False
    assert health["mode"] == "noop"
    assert health["requested_mode"] == "buffered"
    assert health["data_dir"] == "./data"
    assert health["dropped"] == 3
    assert "pip install yitrace-db" in health["last_error"]


def test_init_yitrace_spool_mode_writes_ready_file():
    from pathlib import Path  # noqa: E402

    with tempfile.TemporaryDirectory() as tmp:
        runtime = init_yitrace(
            mode="spool",
            spool_dir=tmp,
            tenant_id=4,
            node_id=1,
            max_batch=10,
            fsync=False,
            register_atexit=False,
        )
        with runtime.tracer.trace("spool", tenant_id=4) as trace:
            with trace.span("span") as span:
                span.log("hello")
        runtime.close()

        ready_files = list((Path(tmp) / "ready").iterdir())
        assert len(ready_files) == 1
        assert runtime.exporter.written_count() == 3
        health = runtime.health()
        assert health["enabled"] is True
        assert health["mode"] == "spool"
        assert health["spool_dir"] == tmp
        assert health["written"] == 3
        assert health["dropped"] == 0
        assert health["lock"]["enabled"] is False


def test_cli_consume_spool_once_writes_embedded_db():
    import contextlib  # noqa: E402
    import io  # noqa: E402
    from pathlib import Path  # noqa: E402
    from yitrace.cli import main  # noqa: E402
    from yitrace.event import SpanEvent  # noqa: E402

    class FakeDb:
        def __init__(self):
            self.calls = []
            self.closed = False

        def ingest(self, events, tenant_id=None):
            self.calls.append((events, tenant_id))
            return {"ingested": len(events)}

        def close(self):
            self.closed = True

    fake_db = FakeDb()

    class FakeYiTraceDB:
        @classmethod
        def open(cls, path, **options):
            return fake_db

    old_module = sys.modules.get("yitrace_db")
    sys.modules["yitrace_db"] = types.SimpleNamespace(YiTraceDB=FakeYiTraceDB)
    try:
        with tempfile.TemporaryDirectory() as tmp:
            spool_dir = Path(tmp) / "spool"
            exporter = SpoolDbExporter(spool_dir, tenant_id=8, fsync=False)
            exporter.export(
                SpanEvent(trace_id=1, span_id=1, parent_span_id=None, seq=1,
                          event_type=EventType.SPAN_START, ext_span_id="s1", ts=1)
            )
            exporter.close()

            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                assert main(["consume-spool", "--data-dir", str(Path(tmp) / "db"), "--spool-dir", str(spool_dir), "--once"]) == 0
            assert out.getvalue().strip() == "consumed=1"
            assert not list((spool_dir / "ready").iterdir())
    finally:
        if old_module is None:
            sys.modules.pop("yitrace_db", None)
        else:
            sys.modules["yitrace_db"] = old_module

    assert fake_db.closed is True
    assert fake_db.calls[0][1] == "8"
    assert fake_db.calls[0][0][0]["event_type"] == EventType.SPAN_START.value


def test_yitrace_client_routes_json_with_auth_and_tenant_headers():
    import urllib.request  # noqa: E402

    captured = {}
    old_urlopen = urllib.request.urlopen

    class Resp:
        def __enter__(self):
            return self

        def __exit__(self, exc_type, exc, tb):
            return None

        def read(self):
            return b'[{"trace_id":1}]'

    def fake_urlopen(req, timeout):
        captured["url"] = req.full_url
        captured["timeout"] = timeout
        captured["headers"] = dict(req.header_items())
        captured["body"] = req.data
        return Resp()

    try:
        urllib.request.urlopen = fake_urlopen
        client = YiTraceClient("http://example.test", token="secret", tenant_id=3, timeout=1.25)
        result = client.search(text="盗刷", k=1)
    finally:
        urllib.request.urlopen = old_urlopen

    assert result == [{"trace_id": 1}]
    assert captured["url"] == "http://example.test/v1/search"
    assert captured["timeout"] == 1.25
    headers = {k.lower(): v for k, v in captured["headers"].items()}
    assert headers["authorization"] == "Bearer secret"
    assert headers["x-tenant-id"] == "3"
    assert b"\\u76d7\\u5237" not in captured["body"], "body keeps readable UTF-8 JSON"


def test_connect_selects_http_or_optional_embedded_package():
    remote = connect("http://localhost:7878", tenant_id=1)
    assert isinstance(remote, YiTraceClient)

    opened = {}

    class FakeYiTraceDB:
        @classmethod
        def open(cls, path, **options):
            opened["path"] = str(path)
            opened["options"] = options
            return "embedded-db"

    old_module = sys.modules.get("yitrace_db")
    sys.modules["yitrace_db"] = types.SimpleNamespace(YiTraceDB=FakeYiTraceDB)
    try:
        local = connect(path="./data", tenant_id=9)
    finally:
        if old_module is None:
            sys.modules.pop("yitrace_db", None)
        else:
            sys.modules["yitrace_db"] = old_module

    assert local == "embedded-db"
    assert opened == {"path": "./data", "options": {"tenant_id": 9}}


def test_connect_path_without_embedded_package_has_install_hint():
    real_import = builtins.__import__

    def fake_import(name, *args, **kwargs):
        if name == "yitrace_db":
            raise ImportError("missing yitrace_db")
        return real_import(name, *args, **kwargs)

    try:
        builtins.__import__ = fake_import
        try:
            connect(path="./data")
        except RuntimeError as err:
            message = str(err)
        else:
            raise AssertionError("connect(path=...) should fail when yitrace-db is missing")
    finally:
        builtins.__import__ = real_import

    assert "pip install yitrace-db" in message
    assert "pip install 'yitrace[db]'" in message


if __name__ == "__main__":
    fns = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    for fn in fns:
        fn()
        print(f"OK  {fn.__name__}")
    print(f"\n{len(fns)} passed")
