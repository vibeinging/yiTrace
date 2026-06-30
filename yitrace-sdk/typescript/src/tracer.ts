// 打点 API：Tracer / Trace / Span。
//
// JS 没有 Python 的 with,用回调式作用域：
//
//   tracer.trace("反洗钱筛查", (t) => {
//     t.span("调用LLM研判", (s) => {
//       s.log("研判结论 需人工复核");
//       s.setStatus(0);
//     });
//   });
//
// 每个 span 产出 SpanStart（带名）+ 若干 Log + SpanEnd（带状态+耗时），seq 在 span 内单调递增。
import { EventType, type SpanEvent } from "./event";
import { ConsoleExporter, type Exporter } from "./exporter";
import { Snowflake } from "./snowflake";

function nowNs(): bigint {
  return BigInt(Date.now()) * 1_000_000n;
}

export class Span {
  tracer: Tracer;
  traceId: bigint;
  spanId: bigint;
  name: string;
  parentSpanId: bigint | null;
  extSpanId: string;
  private seqN = 0n;
  private statusV: number | null = null;
  private inputTokensV: bigint | null = null;
  private outputTokensV: bigint | null = null;
  private sessionIdV: bigint | null = null; // 会话 id：从 trace 透传下来
  private tenantIdV: bigint | null = null; // 租户 id：从 trace 透传下来（隔离维度）
  private agentNameV: string | null = null;
  private toolNameV: string | null = null;
  private modelV: string | null = null;
  private inputTextV: string | null = null;
  private outputTextV: string | null = null;
  private startNs: bigint | null = null;

  constructor(
    tracer: Tracer,
    traceId: bigint,
    spanId: bigint,
    name: string,
    parentSpanId: bigint | null = null,
    sessionId: bigint | null = null,
    tenantId: bigint | null = null,
  ) {
    this.tracer = tracer;
    this.traceId = traceId;
    this.spanId = spanId;
    this.name = name;
    this.parentSpanId = parentSpanId;
    this.sessionIdV = sessionId;
    this.tenantIdV = tenantId;
    this.extSpanId = `${traceId}-${spanId}`; // 跨进程稳定身份,与引擎一致
  }

  // 嵌套子 span：自动以当前 span 为父，并继承会话 id / 租户 id。
  span<T>(name: string, fn: (s: Span) => T): T {
    return runSpan(this.tracer, this.traceId, name, this.spanId, fn, this.sessionIdV, this.tenantIdV);
  }

  private nextSeq(): bigint {
    this.seqN += 1n;
    return this.seqN;
  }

  private emit(
    eventType: EventType,
    opts: { status?: number | null; durationNs?: bigint | null; logs?: string[] } = {},
  ): void {
    this.tracer.emitEvent({
      traceId: this.traceId,
      spanId: this.spanId,
      ts: nowNs(),
      seq: this.nextSeq(),
      eventType,
      extSpanId: this.extSpanId,
      parentSpanId: this.parentSpanId,
      status: opts.status ?? null,
      durationNs: opts.durationNs ?? null,
      inputTokens: this.inputTokensV,
      outputTokens: this.outputTokensV,
      sessionId: this.sessionIdV,
      tenantId: this.tenantIdV,
      agentName: this.agentNameV,
      toolName: this.toolNameV,
      model: this.modelV,
      inputText: this.inputTextV,
      outputText: this.outputTextV,
      logs: opts.logs ?? [],
    });
  }

  log(...msgs: string[]): void {
    this.emit(EventType.Log, { logs: msgs });
  }

  setStatus(status: number): void {
    this.statusV = status;
  }

  // 记 LLM token 用量（成本核心）。在后续事件上报，引擎按 trace 汇总。
  setTokens(inputTokens?: bigint | number, outputTokens?: bigint | number): void {
    if (inputTokens !== undefined) this.inputTokensV = BigInt(inputTokens);
    if (outputTokens !== undefined) this.outputTokensV = BigInt(outputTokens);
  }

  // 标记本 span 属于哪个 agent（成本/可观测按 agent 下钻）。
  setAgent(agentName: string): void {
    this.agentNameV = agentName;
  }

  // 标记本 span 是哪个工具/函数调用。
  setTool(toolName: string): void {
    this.toolNameV = toolName;
  }

  // 标记本 span 用的模型（成本按模型归因）。
  setModel(model: string): void {
    this.modelV = model;
  }

  // 记 LLM 输入/输出文本 —— eval 的评测对象（judge 据此打分）。
  setIo(inputText?: string, outputText?: string): void {
    if (inputText !== undefined) this.inputTextV = inputText;
    if (outputText !== undefined) this.outputTextV = outputText;
  }

  start(): void {
    this.startNs = nowNs();
    this.emit(EventType.SpanStart, { logs: [this.name] });
  }

  end(): void {
    const e = nowNs();
    const dur = e - (this.startNs ?? e);
    this.emit(EventType.SpanEnd, { status: this.statusV, durationNs: dur });
  }
}

// 跑一个作用域 span（根 span 或子 span 共用）。
function runSpan<T>(
  tracer: Tracer,
  traceId: bigint,
  name: string,
  parentSpanId: bigint | null,
  fn: (s: Span) => T,
  sessionId: bigint | null = null,
  tenantId: bigint | null = null,
): T {
  const spanId = tracer.sf.next();
  const sp = new Span(tracer, traceId, spanId, name, parentSpanId, sessionId, tenantId);
  sp.start();
  try {
    return fn(sp);
  } catch (err) {
    sp.setStatus(1); // 异常 → 状态非0
    throw err;
  } finally {
    sp.end();
  }
}

export class Trace {
  tracer: Tracer;
  traceId: bigint;
  name: string;
  sessionId: bigint | null; // 会话 id：多轮对话/agent 会话，串起多条 trace
  tenantId: bigint | null; // 租户 id：逻辑隔离维度，本 trace 全部 span 都带它

  constructor(tracer: Tracer, traceId: bigint, name: string, sessionId: bigint | null = null, tenantId: bigint | null = null) {
    this.tracer = tracer;
    this.traceId = traceId;
    this.name = name;
    this.sessionId = sessionId;
    this.tenantId = tenantId;
  }

  // 根 span（无父），继承本 trace 的会话 id / 租户 id。
  span<T>(name: string, fn: (s: Span) => T): T {
    return runSpan(this.tracer, this.traceId, name, null, fn, this.sessionId, this.tenantId);
  }
}

export class Tracer {
  exporter: Exporter;
  sf: Snowflake;

  constructor(exporter?: Exporter, nodeId?: number) {
    this.exporter = exporter ?? new ConsoleExporter();
    this.sf = new Snowflake(nodeId);
  }

  // 开一条 trace。sessionId 归会话；tenantId 标租户（隔离维度，该 trace 全部 span 都带）。
  trace<T>(name: string, fn: (t: Trace) => T, sessionId?: bigint | number, tenantId?: bigint | number): T {
    const traceId = this.sf.next();
    const sid = sessionId === undefined ? null : BigInt(sessionId);
    const tid = tenantId === undefined ? null : BigInt(tenantId);
    return fn(new Trace(this, traceId, name, sid, tid));
  }

  emitEvent(e: SpanEvent): void {
    this.exporter.export(e);
  }

  close(): void {
    this.exporter.close?.();
  }
}
