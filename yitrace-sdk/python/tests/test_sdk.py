"""SDK 测试。可直接 `python3 tests/test_sdk.py` 跑，也兼容 pytest。"""
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from yitrace import CollectingExporter, EventType, HttpExporter, Tracer, event_id  # noqa: E402

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
    assert evs[0].logs == ["调用LLM研判"], "start 带 span 名"
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
    root_start = next(e for e in starts if e.logs == ["root"])
    child_start = next(e for e in starts if e.logs == ["child"])
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
    end = next(e for e in exp.events if e.event_type == EventType.SPAN_END and e.agent_name == "规划")
    assert end.model == "qwen3"
    assert end.input_text == "请研判这笔交易"
    assert end.output_text == "判定为疑似盗刷"
    w = end.to_wire()
    assert w["session_id"] == 9000 and w["agent_name"] == "规划" and w["output_text"] == "判定为疑似盗刷"
    # 子 span 带 tool_name
    tool_end = next(e for e in exp.events if e.event_type == EventType.SPAN_END and e.tool_name == "kb_lookup")
    assert tool_end.session_id == 9000, "子 span 也继承会话 id"


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


if __name__ == "__main__":
    fns = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    for fn in fns:
        fn()
        print(f"OK  {fn.__name__}")
    print(f"\n{len(fns)} passed")
