"""打点 API：Tracer / Trace / Span。

用法::

    tracer = Tracer(exporter=ConsoleExporter(), node_id=1)
    with tracer.trace("反洗钱筛查") as t:
        with t.span("调用LLM研判") as s:
            s.log("研判结论 需人工复核")
            s.set_status(0)

每个 span 产出 SPAN_START（带 span 名）+ 若干 LOG + SPAN_END（带状态+耗时）三类事件，
seq 在 span 内单调递增、客户端给定 —— 进引擎后按 (trace,span) 折叠成一条完整 span。
"""
from __future__ import annotations

import time
from contextlib import contextmanager
from typing import Iterator

from ._snowflake import Snowflake
from .event import EventType, SpanEvent
from .exporter import ConsoleExporter, Exporter


class Span:
    def __init__(
        self,
        tracer: "Tracer",
        trace_id: int,
        span_id: int,
        name: str,
        parent_span_id: int | None = None,
        session_id: int | None = None,
        tenant_id: int | None = None,
    ) -> None:
        self.tracer = tracer
        self.trace_id = trace_id
        self.span_id = span_id
        self.name = name
        self.parent_span_id = parent_span_id
        self.ext_span_id = f"{trace_id}-{span_id}"  # 跨进程稳定身份，与引擎 demo 一致
        self._seq = 0
        self._status: int | None = None
        self._input_tokens: int | None = None
        self._output_tokens: int | None = None
        self._session_id = session_id  # 会话 id：从 trace 透传下来
        self._tenant_id = tenant_id  # 租户 id：从 trace 透传下来（隔离维度）
        self._agent_name: str | None = None
        self._tool_name: str | None = None
        self._model: str | None = None
        self._input_text: str | None = None
        self._output_text: str | None = None
        self._start_ns: int | None = None

    def _next_seq(self) -> int:
        self._seq += 1
        return self._seq

    def _emit(self, event_type: EventType, *, status: int | None = None, duration_ns: int | None = None, logs: list[str] | None = None) -> None:
        self.tracer._emit(
            SpanEvent(
                trace_id=self.trace_id,
                span_id=self.span_id,
                ts=time.time_ns(),
                seq=self._next_seq(),
                event_type=event_type,
                ext_span_id=self.ext_span_id,
                parent_span_id=self.parent_span_id,
                status=status,
                duration_ns=duration_ns,
                input_tokens=self._input_tokens,
                output_tokens=self._output_tokens,
                session_id=self._session_id,
                tenant_id=self._tenant_id,
                agent_name=self._agent_name,
                tool_name=self._tool_name,
                model=self._model,
                input_text=self._input_text,
                output_text=self._output_text,
                logs=logs or [],
            )
        )

    def log(self, *msgs: str) -> None:
        """记一条/多条日志（进折叠后的 logs 并集）。"""
        self._emit(EventType.LOG, logs=list(msgs))

    def set_status(self, status: int) -> None:
        """设状态（0=正常，非0=异常等）。在 SPAN_END 时上报，last-non-null 胜出。"""
        self._status = status

    def set_tokens(self, input_tokens: int | None = None, output_tokens: int | None = None) -> None:
        """记 LLM token 用量（成本核心）。在后续事件上报，引擎按 trace 汇总。"""
        if input_tokens is not None:
            self._input_tokens = input_tokens
        if output_tokens is not None:
            self._output_tokens = output_tokens

    def set_agent(self, agent_name: str) -> None:
        """标记本 span 属于哪个 agent（成本/可观测按 agent 下钻）。"""
        self._agent_name = agent_name

    def set_tool(self, tool_name: str) -> None:
        """标记本 span 是哪个工具/函数调用。"""
        self._tool_name = tool_name

    def set_model(self, model: str) -> None:
        """标记本 span 用的模型（成本按模型归因）。"""
        self._model = model

    def set_io(self, input_text: str | None = None, output_text: str | None = None) -> None:
        """记 LLM 输入/输出文本 —— eval 的评测对象（judge 据此打分）。"""
        if input_text is not None:
            self._input_text = input_text
        if output_text is not None:
            self._output_text = output_text

    def span(self, name: str):
        """嵌套子 span：自动以当前 span 为父，并继承会话 id / 租户 id。"""
        return _scoped_span(self.tracer, self.trace_id, name, self.span_id, self._session_id, self._tenant_id)

    # —— 上下文管理 ——
    def _start(self) -> None:
        self._start_ns = time.time_ns()
        self._emit(EventType.SPAN_START, logs=[self.name])

    def _end(self) -> None:
        end = time.time_ns()
        dur = end - (self._start_ns if self._start_ns is not None else end)
        self._emit(EventType.SPAN_END, status=self._status, duration_ns=dur)


@contextmanager
def _scoped_span(
    tracer: "Tracer", trace_id: int, name: str, parent_span_id: int | None, session_id: int | None = None, tenant_id: int | None = None
) -> Iterator[Span]:
    span_id = tracer._sf.next()
    sp = Span(tracer, trace_id, span_id, name, parent_span_id, session_id, tenant_id)
    sp._start()
    try:
        yield sp
    except Exception:
        sp.set_status(1)  # 异常 → 状态非0
        raise
    finally:
        sp._end()


class Trace:
    def __init__(self, tracer: "Tracer", trace_id: int, name: str, session_id: int | None = None, tenant_id: int | None = None) -> None:
        self.tracer = tracer
        self.trace_id = trace_id
        self.name = name
        self.session_id = session_id  # 会话 id：多轮对话/agent 会话，串起多条 trace
        self.tenant_id = tenant_id  # 租户 id：逻辑隔离维度，本 trace 的所有 span 都带它

    def span(self, name: str):
        """根 span（无父），继承本 trace 的会话 id / 租户 id。"""
        return _scoped_span(self.tracer, self.trace_id, name, None, self.session_id, self.tenant_id)


class Tracer:
    def __init__(self, exporter: Exporter | None = None, node_id: int | None = None) -> None:
        self.exporter: Exporter = exporter or ConsoleExporter()
        self._sf = Snowflake(node_id)

    @contextmanager
    def trace(self, name: str, session_id: int | None = None, tenant_id: int | None = None) -> Iterator[Trace]:
        """开一条 trace。session_id 归会话；tenant_id 标租户（隔离维度，该 trace 全部 span 都带）。"""
        trace_id = self._sf.next()
        yield Trace(self, trace_id, name, session_id, tenant_id)

    def _emit(self, event: SpanEvent) -> None:
        self.exporter.export(event)

    def close(self) -> None:
        self.exporter.close()
