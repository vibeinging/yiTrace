"""事件模型 + 确定性 event_id。

关键：event_id 与 Rust 引擎 `yt-core::event` **逐字节一致**（同一套 FNV-1a 哈希、同样的字段顺序）。
这样 SDK 这边算出的 event_id 和引擎那边算出的相同 —— 同一条 span 事件无论重传几次、跨 SDK/引擎，
去重都对得上，崩溃重放也幂等。基准值见引擎 `cargo run -p yt-core --example print_event_id`。
"""
from __future__ import annotations

import enum
from dataclasses import dataclass, field

_MASK = 0xFFFFFFFFFFFFFFFF
_FNV_OFFSET = 0xCBF29CE484222325
_FNV_PRIME = 0x100000001B3


class EventType(enum.Enum):
    """事件类型。tag 进 event_id 哈希，必须与引擎 `EventType::tag()` 完全一致、永不改。"""

    SPAN_START = 1
    SPAN_END = 2
    ATTR = 3
    LOG = 4
    ERROR = 5

    def tag(self) -> int:
        return self.value  # 1..5，与 Rust 对齐


def _fnv1a64(data: bytes) -> int:
    h = _FNV_OFFSET
    for b in data:
        h ^= b
        h = (h * _FNV_PRIME) & _MASK
    return h


def event_id(ext_span_id: str, seq: int, event_type: EventType) -> int:
    """= fnv1a64(ext_span_id(utf-8) ++ seq(8字节小端) ++ [type_tag])。与引擎逐字节一致。"""
    data = ext_span_id.encode("utf-8") + (seq & _MASK).to_bytes(8, "little") + bytes([event_type.tag()])
    return _fnv1a64(data)


@dataclass
class SpanEvent:
    """一个 span 事件 = 引擎 WalRecord 的 SDK 侧对应物。"""

    trace_id: int
    span_id: int
    ts: int  # 纳秒
    seq: int  # 上报序：客户端给，原样进引擎，绝不被引擎重补
    event_type: EventType
    ext_span_id: str  # 跨进程稳定的 span 身份（进 event_id）
    parent_span_id: int | None = None  # 父 span（trace 是棵树）
    status: int | None = None
    duration_ns: int | None = None
    input_tokens: int | None = None  # LLM 输入 token（成本核心）
    output_tokens: int | None = None
    session_id: int | None = None  # 会话 id（多轮对话/agent 会话，串起多条 trace）
    tenant_id: int | None = None  # 租户 id（逻辑隔离维度；多租户共享索引、查询强制按 tenant 过滤）
    agent_name: str | None = None  # agent 名（成本/可观测按 agent 下钻）
    tool_name: str | None = None  # 工具名（tool/function call span）
    model: str | None = None  # 模型名（成本按模型归因）
    input_text: str | None = None  # LLM 输入文本（prompt）—— eval 的评测上文
    output_text: str | None = None  # LLM 输出文本（答案）—— eval 打分对象
    logs: list[str] = field(default_factory=list)

    def event_id(self) -> int:
        return event_id(self.ext_span_id, self.seq, self.event_type)

    def to_wire(self) -> dict:
        """灌进引擎摄入端的 JSON 载荷（字段对齐引擎 WalRecord）。"""
        return {
            "trace_id": self.trace_id,
            "span_id": self.span_id,
            "ts": self.ts,
            "seq": self.seq,
            "event_type": self.event_type.value,
            "ext_span_id": self.ext_span_id,
            "parent_span_id": self.parent_span_id,
            "event_id": self.event_id(),
            "status": self.status,
            "duration_ns": self.duration_ns,
            "input_tokens": self.input_tokens,
            "output_tokens": self.output_tokens,
            "session_id": self.session_id,
            "tenant_id": self.tenant_id,
            "agent_name": self.agent_name,
            "tool_name": self.tool_name,
            "model": self.model,
            "input_text": self.input_text,
            "output_text": self.output_text,
            "logs": list(self.logs),
        }
