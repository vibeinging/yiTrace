"""yitrace SDK —— 给 Agent 打点，产出与 yiTrace 引擎一致的 trace 事件。

核心保证：event_id 与引擎逐字节一致（同一套 FNV 哈希），所以 SDK 产生的事件灌进引擎后，
去重、崩溃重放幂等全都对得上。
"""
from ._snowflake import Snowflake
from .client import YiTraceClient, connect
from .event import EventType, SpanEvent, event_id
from .exporter import (
    BatchExporter,
    BufferedDbExporter,
    CollectingExporter,
    ConsoleExporter,
    DbExporter,
    Exporter,
    HttpExporter,
    NoopExporter,
    SpoolConsumer,
    SpoolDbExporter,
)
from .service import YiTraceRuntime, get_yitrace_runtime, init_yitrace, shutdown_yitrace
from .tracer import Span, Trace, Tracer

__all__ = [
    "Snowflake",
    "YiTraceClient",
    "connect",
    "EventType",
    "SpanEvent",
    "event_id",
    "Exporter",
    "ConsoleExporter",
    "CollectingExporter",
    "DbExporter",
    "BufferedDbExporter",
    "SpoolDbExporter",
    "SpoolConsumer",
    "BatchExporter",
    "HttpExporter",
    "NoopExporter",
    "YiTraceRuntime",
    "init_yitrace",
    "shutdown_yitrace",
    "get_yitrace_runtime",
    "Tracer",
    "Trace",
    "Span",
]
