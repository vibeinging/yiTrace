// 事件模型 + 确定性 event_id。
//
// 关键：event_id 与 Rust 引擎 yt-core::event 以及 Python SDK **逐字节一致**
//（同一套 FNV-1a 64 哈希、同样字段顺序、UTF-8 编码、u64 小端）。u64 必须用 BigInt 才精确
//（JS number 是 f64,装不下 64 位整数）。基准值见引擎 cargo run -p yt-core --example print_event_id。

const MASK = (1n << 64n) - 1n;
const FNV_OFFSET = 0xcbf29ce484222325n;
const FNV_PRIME = 0x100000001b3n;

// 事件类型。用 const 对象 + 联合类型（不用 enum —— Node 类型剥离要求可擦除语法）。
export const EventType = {
  SpanStart: 1,
  SpanEnd: 2,
  Attr: 3,
  Log: 4,
  Error: 5,
} as const;
export type EventType = (typeof EventType)[keyof typeof EventType];

function fnv1a64(data: Uint8Array): bigint {
  let h = FNV_OFFSET;
  for (const b of data) {
    h ^= BigInt(b);
    h = (h * FNV_PRIME) & MASK;
  }
  return h;
}

// u64 → 8 字节小端（对齐 Rust seq.to_le_bytes()）。
function u64le(v: bigint): Uint8Array {
  const out = new Uint8Array(8);
  let x = v & MASK;
  for (let i = 0; i < 8; i++) {
    out[i] = Number(x & 0xffn);
    x >>= 8n;
  }
  return out;
}

// = fnv1a64(ext_span_id(utf-8) ++ seq(8字节小端) ++ [type_tag])。与引擎逐字节一致。
export function eventId(extSpanId: string, seq: bigint, eventType: EventType): bigint {
  const name = new TextEncoder().encode(extSpanId);
  const seqBytes = u64le(seq);
  const data = new Uint8Array(name.length + 8 + 1);
  data.set(name, 0);
  data.set(seqBytes, name.length);
  data[name.length + 8] = eventType; // tag = 1..5
  return fnv1a64(data);
}

export interface SpanEvent {
  traceId: bigint;
  spanId: bigint;
  ts: bigint; // 纳秒
  seq: bigint; // 上报序：客户端给,原样进引擎
  eventType: EventType;
  extSpanId: string;
  parentSpanId: bigint | null; // 父 span（trace 是棵树）
  status: number | null;
  durationNs: bigint | null;
  inputTokens: bigint | null; // LLM 输入 token（成本核心）
  outputTokens: bigint | null;
  sessionId: bigint | null; // 会话 id（多轮对话/agent 会话，串起多条 trace）
  agentName: string | null; // agent 名（成本/可观测按 agent 下钻）
  toolName: string | null; // 工具名（tool/function call span）
  model: string | null; // 模型名（成本按模型归因）
  inputText: string | null; // LLM 输入文本（prompt）—— eval 的评测上文
  outputText: string | null; // LLM 输出文本（答案）—— eval 打分对象
  logs: string[];
}

// 灌进引擎摄入端的 JSON 载荷（BigInt 转字符串,避免精度丢失）。
export function toWire(e: SpanEvent): Record<string, unknown> {
  return {
    trace_id: e.traceId.toString(),
    span_id: e.spanId.toString(),
    ts: e.ts.toString(),
    seq: e.seq.toString(),
    event_type: e.eventType,
    ext_span_id: e.extSpanId,
    parent_span_id: e.parentSpanId === null ? null : e.parentSpanId.toString(),
    event_id: eventId(e.extSpanId, e.seq, e.eventType).toString(),
    status: e.status,
    duration_ns: e.durationNs === null ? null : e.durationNs.toString(),
    input_tokens: e.inputTokens === null ? null : e.inputTokens.toString(),
    output_tokens: e.outputTokens === null ? null : e.outputTokens.toString(),
    session_id: e.sessionId === null ? null : e.sessionId.toString(),
    agent_name: e.agentName,
    tool_name: e.toolName,
    model: e.model,
    input_text: e.inputText,
    output_text: e.outputText,
    logs: e.logs,
  };
}
