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
import { EventType, type SpanEvent } from "./event.js";
import { ConsoleExporter, type Exporter } from "./exporter.js";
import { Snowflake } from "./snowflake.js";

function nowNs(): bigint {
  return BigInt(Date.now()) * 1_000_000n;
}

export interface SpanOptions {
  displayName?: string;
  agentName?: string;
}

export interface TokenUsageOptions {
  inputTokens?: bigint | number | null;
  outputTokens?: bigint | number | null;
  cacheReadTokens?: bigint | number | null;
  cacheWriteTokens?: bigint | number | null;
}

type AttributeValue = string | number | boolean;

export class Span {
  tracer: Tracer;
  traceId: bigint;
  spanId: bigint;
  name: string;
  private displayNameV: string | null;
  parentSpanId: bigint | null;
  extSpanId: string;
  private seqN = 0n;
  private statusV: number | null = null;
  private inputTokensV: bigint | null = null;
  private outputTokensV: bigint | null = null;
  private cacheReadTokensV: bigint | null = null;
  private cacheWriteTokensV: bigint | null = null;
  private sessionIdV: bigint | null = null; // 会话 id：从 trace 透传下来
  private tenantIdV: bigint | null = null; // 租户 id：从 trace 透传下来（隔离维度）
  private agentNameV: string | null = null;
  private toolNameV: string | null = null;
  private modelV: string | null = null;
  private inputTextV: string | null = null;
  private outputTextV: string | null = null;
  private attrsV: Record<string, AttributeValue> = {};
  private startNs: bigint | null = null;

  constructor(
    tracer: Tracer,
    traceId: bigint,
    spanId: bigint,
    name: string,
    parentSpanId: bigint | null = null,
    sessionId: bigint | null = null,
    tenantId: bigint | null = null,
    options: SpanOptions = {},
  ) {
    this.tracer = tracer;
    this.traceId = traceId;
    this.spanId = spanId;
    this.name = name;
    this.displayNameV = options.displayName?.trim() || null;
    this.parentSpanId = parentSpanId;
    this.sessionIdV = sessionId;
    this.tenantIdV = tenantId;
    this.agentNameV = options.agentName ?? null;
    this.extSpanId = `${traceId}-${spanId}`; // 跨进程稳定身份,与引擎一致
  }

  // 嵌套子 span：自动以当前 span 为父，并继承会话 id / 租户 id。
  span<T>(name: string, fn: (s: Span) => T): T;
  span<T>(name: string, options: SpanOptions, fn: (s: Span) => T): T;
  span<T>(name: string, optionsOrFn: SpanOptions | ((s: Span) => T), maybeFn?: (s: Span) => T): T {
    const options = typeof optionsOrFn === "function" ? {} : optionsOrFn;
    const fn = typeof optionsOrFn === "function" ? optionsOrFn : maybeFn;
    if (!fn) throw new TypeError("span callback is required");
    return runSpan(
      this.tracer,
      this.traceId,
      name,
      this.spanId,
      fn,
      this.sessionIdV,
      this.tenantIdV,
      { ...options, agentName: options.agentName ?? this.agentNameV ?? undefined },
    );
  }

  private nextSeq(): bigint {
    this.seqN += 1n;
    return this.seqN;
  }

  private emit(
    eventType: EventType,
    opts: { status?: number | null; durationNs?: bigint | null; logs?: string[] } = {},
  ): void {
    const isEnd = eventType === EventType.SpanEnd;
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
      inputTokens: isEnd ? this.inputTokensV : null,
      outputTokens: isEnd ? this.outputTokensV : null,
      cacheReadTokens: isEnd ? this.cacheReadTokensV : null,
      cacheWriteTokens: isEnd ? this.cacheWriteTokensV : null,
      sessionId: this.sessionIdV,
      tenantId: this.tenantIdV,
      spanName: eventType === EventType.SpanStart ? this.name : null,
      displayName: eventType === EventType.SpanStart ? this.displayNameV : null,
      agentName: this.agentNameV,
      toolName: this.toolNameV,
      model: this.modelV,
      inputText: this.inputTextV,
      outputText: this.outputTextV,
      logs: opts.logs ?? [],
      attrs: isEnd ? { ...this.attrsV } : {},
    });
  }

  log(...msgs: string[]): void {
    this.emit(EventType.Log, { logs: msgs });
  }

  setStatus(status: number): void {
    this.statusV = status;
  }

  // 记 LLM token 用量（成本核心）。在后续事件上报，引擎按 trace 汇总。
  setTokens(options: TokenUsageOptions): void;
  setTokens(inputTokens?: bigint | number, outputTokens?: bigint | number): void;
  setTokens(
    optionsOrInput?: TokenUsageOptions | bigint | number,
    outputTokens?: bigint | number,
  ): void {
    const options: TokenUsageOptions =
      typeof optionsOrInput === "object" && optionsOrInput !== null
        ? optionsOrInput
        : { inputTokens: optionsOrInput, outputTokens };
    const convert = (name: string, value: bigint | number | null | undefined): bigint | null | undefined => {
      if (value === undefined || value === null) return value;
      if (typeof value === "number" && (!Number.isSafeInteger(value) || value < 0)) {
        throw new RangeError(`${name} must be a non-negative safe integer or bigint`);
      }
      const converted = BigInt(value);
      if (converted < 0n) throw new RangeError(`${name} must be non-negative`);
      return converted;
    };
    const input = convert("inputTokens", options.inputTokens);
    const output = convert("outputTokens", options.outputTokens);
    const cacheRead = convert("cacheReadTokens", options.cacheReadTokens);
    const cacheWrite = convert("cacheWriteTokens", options.cacheWriteTokens);
    if (input !== undefined && input !== null) this.inputTokensV = input;
    if (output !== undefined && output !== null) this.outputTokensV = output;
    if (cacheRead !== undefined && cacheRead !== null) this.cacheReadTokensV = cacheRead;
    if (cacheWrite !== undefined && cacheWrite !== null) this.cacheWriteTokensV = cacheWrite;
  }

  setAttribute(key: string, value: AttributeValue | null): void {
    if (value === null) return;
    if (typeof key !== "string" || key.trim().length === 0) {
      throw new TypeError("attribute key must be a non-empty string");
    }
    if (new TextEncoder().encode(key).length > 128) {
      throw new RangeError("attribute key must be at most 128 UTF-8 bytes");
    }
    if (!["string", "number", "boolean"].includes(typeof value)) {
      throw new TypeError("attribute value must be a JSON scalar or null");
    }
    if (typeof value === "number" && !Number.isFinite(value)) {
      throw new RangeError("attribute number must be finite");
    }
    if (new TextEncoder().encode(JSON.stringify(value)).length > 4096) {
      throw new RangeError("attribute value must be at most 4096 UTF-8 bytes");
    }
    const next = { ...this.attrsV, [key]: value };
    if (Object.keys(next).length > 64) throw new RangeError("a span can have at most 64 attributes");
    if (new TextEncoder().encode(JSON.stringify(next)).length > 16384) {
      throw new RangeError("span attributes must be at most 16384 UTF-8 bytes");
    }
    this.attrsV = next;
  }

  setAttributes(attrs: Record<string, AttributeValue | null>): void {
    if (attrs === null || typeof attrs !== "object" || Array.isArray(attrs)) {
      throw new TypeError("attrs must be an object");
    }
    const original = { ...this.attrsV };
    try {
      for (const [key, value] of Object.entries(attrs)) this.setAttribute(key, value);
    } catch (error) {
      this.attrsV = original;
      throw error;
    }
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
    this.emit(EventType.SpanStart);
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
  options: SpanOptions = {},
): T {
  const spanId = tracer.sf.next();
  const sp = new Span(tracer, traceId, spanId, name, parentSpanId, sessionId, tenantId, options);
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
  agentName: string | null;

  constructor(
    tracer: Tracer,
    traceId: bigint,
    name: string,
    sessionId: bigint | null = null,
    tenantId: bigint | null = null,
    agentName: string | null = null,
  ) {
    this.tracer = tracer;
    this.traceId = traceId;
    this.name = name;
    this.sessionId = sessionId;
    this.tenantId = tenantId;
    this.agentName = agentName;
  }

  // 根 span（无父），继承本 trace 的会话 id / 租户 id。
  span<T>(name: string, fn: (s: Span) => T): T;
  span<T>(name: string, options: SpanOptions, fn: (s: Span) => T): T;
  span<T>(name: string, optionsOrFn: SpanOptions | ((s: Span) => T), maybeFn?: (s: Span) => T): T {
    const options = typeof optionsOrFn === "function" ? {} : optionsOrFn;
    const fn = typeof optionsOrFn === "function" ? optionsOrFn : maybeFn;
    if (!fn) throw new TypeError("span callback is required");
    return runSpan(
      this.tracer,
      this.traceId,
      name,
      null,
      fn,
      this.sessionId,
      this.tenantId,
      { ...options, agentName: options.agentName ?? this.agentName ?? undefined },
    );
  }
}

export class Tracer {
  exporter: Exporter;
  sf: Snowflake;
  agentName: string | null;

  constructor(exporter?: Exporter, nodeId?: number, agentName?: string) {
    this.exporter = exporter ?? new ConsoleExporter();
    this.sf = new Snowflake(nodeId);
    this.agentName = agentName ?? null;
  }

  // 开一条 trace。sessionId 归会话；tenantId 标租户（隔离维度，该 trace 全部 span 都带）。
  trace<T>(name: string, fn: (t: Trace) => T, sessionId?: bigint | number, tenantId?: bigint | number): T {
    const traceId = this.sf.next();
    const sid = sessionId === undefined ? null : BigInt(sessionId);
    const tid = tenantId === undefined ? null : BigInt(tenantId);
    return fn(new Trace(this, traceId, name, sid, tid, this.agentName));
  }

  emitEvent(e: SpanEvent): void {
    this.exporter.export(e);
  }

  async close(): Promise<void> {
    await this.exporter.close?.();
  }
}
