import { NativeYiTraceDB } from "./native.js";

function tenantId(options) {
  const value = options?.tenantId;
  if (value === undefined || value === null) return undefined;
  return String(value);
}

function parseJson(text) {
  return JSON.parse(text);
}

function optionValue(options, snake, camel = snake) {
  if (options?.[snake] !== undefined) return options[snake];
  if (options?.[camel] !== undefined) return options[camel];
  return undefined;
}

function defaultTimestampNs() {
  return (BigInt(Date.now()) * 1_000_000n).toString();
}

function setIfDefined(target, key, value) {
  if (value !== undefined && value !== null) target[key] = value;
}

function plainObject(value) {
  return value && typeof value === "object" && !Array.isArray(value);
}

function normalizeSearchQuery(query = {}) {
  const out = { ...query };
  if (query.filter) {
    out.filter = { ...query.filter };
    if (out.filter.traceId !== undefined && out.filter.trace_id === undefined) out.filter.trace_id = out.filter.traceId;
    if (out.filter.agentName !== undefined && out.filter.agent_name === undefined) out.filter.agent_name = out.filter.agentName;
    if (out.filter.timeFrom !== undefined && out.filter.time_from === undefined) out.filter.time_from = out.filter.timeFrom;
    if (out.filter.timeTo !== undefined && out.filter.time_to === undefined) out.filter.time_to = out.filter.timeTo;
    delete out.filter.traceId;
    delete out.filter.agentName;
    delete out.filter.timeFrom;
    delete out.filter.timeTo;
  }
  return out;
}

function normalizeSessionAttrs(options = {}) {
  const attrs = {};
  const source = plainObject(options.attrs) ? options.attrs : {};
  for (const key of ["project_id", "skill", "mode", "call_site"]) {
    if (source[key] !== undefined) attrs[key] = source[key];
    if (options[key] !== undefined) attrs[key] = options[key];
  }
  if (options.projectId !== undefined) attrs.project_id = options.projectId;
  if (options.callSite !== undefined) attrs.call_site = options.callSite;
  return Object.keys(attrs).length > 0 ? attrs : undefined;
}

export class SpanEventBuilder {
  #defaults;
  #events = [];
  #seqBySpan = new Map();

  constructor(defaults = {}) {
    this.#defaults = { ...defaults };
  }

  startSpan(options = {}) {
    const event = this.#baseEvent(options, 1);
    this.#copyCommonFields(event, options);
    const name = optionValue(options, "name");
    const logs = [...(Array.isArray(options.logs) ? options.logs : [])];
    if (name !== undefined && name !== null) logs.unshift(String(name));
    if (logs.length > 0) event.logs = logs;
    this.#events.push(event);
    return event;
  }

  log(options = {}) {
    const event = this.#baseEvent(options, 4);
    this.#copyCommonFields(event, options);
    const logs = [];
    const message = optionValue(options, "message");
    const log = optionValue(options, "log");
    if (message !== undefined && message !== null) logs.push(String(message));
    if (log !== undefined && log !== null) logs.push(String(log));
    if (Array.isArray(options.logs)) logs.push(...options.logs.map(String));
    if (logs.length > 0) event.logs = logs;
    this.#events.push(event);
    return event;
  }

  endSpan(options = {}) {
    const event = this.#baseEvent(options, 2);
    this.#copyCommonFields(event, options);
    setIfDefined(event, "status", optionValue(options, "status") ?? 0);
    setIfDefined(event, "duration_ns", optionValue(options, "duration_ns", "durationNs"));
    setIfDefined(event, "output_text", optionValue(options, "output_text", "outputText"));
    this.#events.push(event);
    return event;
  }

  events() {
    return this.#events.map((event) => ({ ...event, attrs: event.attrs ? { ...event.attrs } : undefined }));
  }

  clear() {
    this.#events = [];
    this.#seqBySpan.clear();
  }

  async ingest(db, options = {}) {
    return db.ingest(this.events(), options);
  }

  #baseEvent(options, eventType) {
    const traceId = optionValue(options, "trace_id", "traceId") ?? optionValue(this.#defaults, "trace_id", "traceId");
    const spanId = optionValue(options, "span_id", "spanId");
    if (traceId === undefined || traceId === null) throw new Error("SpanEventBuilder requires traceId");
    if (spanId === undefined || spanId === null) throw new Error("SpanEventBuilder requires spanId");
    const extSpanId = optionValue(options, "ext_span_id", "extSpanId") ?? String(spanId);
    const key = `${String(traceId)}\u0000${String(extSpanId)}`;
    const seq = optionValue(options, "seq") ?? this.#nextSeq(key);
    const event = {
      trace_id: traceId,
      span_id: spanId,
      ts: optionValue(options, "ts") ?? defaultTimestampNs(),
      seq,
      event_type: eventType,
      ext_span_id: extSpanId,
    };
    setIfDefined(event, "parent_span_id", optionValue(options, "parent_span_id", "parentSpanId"));
    setIfDefined(event, "session_id", optionValue(options, "session_id", "sessionId") ?? optionValue(this.#defaults, "session_id", "sessionId"));
    setIfDefined(event, "tenant_id", optionValue(options, "tenant_id", "tenantId") ?? optionValue(this.#defaults, "tenant_id", "tenantId"));
    setIfDefined(event, "external_trace_id", optionValue(options, "external_trace_id", "externalTraceId"));
    setIfDefined(event, "external_span_id", optionValue(options, "external_span_id", "externalSpanId"));
    setIfDefined(event, "external_parent_span_id", optionValue(options, "external_parent_span_id", "externalParentSpanId"));
    setIfDefined(event, "external_session_id", optionValue(options, "external_session_id", "externalSessionId"));
    const attrs = {
      ...(plainObject(this.#defaults.attrs) ? this.#defaults.attrs : {}),
      ...(plainObject(options.attrs) ? options.attrs : {}),
    };
    if (Object.keys(attrs).length > 0) {
      event.attrs = attrs;
    }
    return event;
  }

  #copyCommonFields(event, options) {
    setIfDefined(event, "agent_name", optionValue(options, "agent_name", "agentName"));
    setIfDefined(event, "tool_name", optionValue(options, "tool_name", "toolName"));
    setIfDefined(event, "model", optionValue(options, "model"));
    setIfDefined(event, "input_text", optionValue(options, "input_text", "inputText"));
    setIfDefined(event, "output_text", optionValue(options, "output_text", "outputText"));
    setIfDefined(event, "input_tokens", optionValue(options, "input_tokens", "inputTokens"));
    setIfDefined(event, "output_tokens", optionValue(options, "output_tokens", "outputTokens"));
  }

  #nextSeq(key) {
    const next = (this.#seqBySpan.get(key) ?? 0) + 1;
    this.#seqBySpan.set(key, next);
    return next;
  }
}

export function createSpanEventBuilder(defaults = {}) {
  return new SpanEventBuilder(defaults);
}

export class YiTraceDB {
  #native;
  #tenantId;
  #closed = false;

  constructor(native, options = {}) {
    this.#native = native;
    this.#tenantId = tenantId(options);
  }

  static async open(pathOrOptions) {
    const options = typeof pathOrOptions === "string" ? { dataDir: pathOrOptions } : pathOrOptions;
    if (!options?.dataDir) {
      throw new Error("YiTraceDB.open requires a dataDir");
    }
    if (Object.prototype.hasOwnProperty.call(options, "readOnly")) {
      throw new Error("OpenOptions.readOnly is not supported yet; omit it to open a writable embedded database");
    }
    return new YiTraceDB(new NativeYiTraceDB(options.dataDir), options);
  }

  async ingest(events, options = {}) {
    this.#ensureOpen();
    const response = this.#native.ingestJson(JSON.stringify(events), tenantId(options) ?? this.#tenantId);
    return parseJson(response);
  }

  async ingestOtlp(body, options = {}) {
    this.#ensureOpen();
    const response = this.#native.ingestOtlpJson(
      typeof body === "string" ? body : JSON.stringify(body),
      tenantId(options) ?? this.#tenantId,
    );
    return parseJson(response);
  }

  async search(query, options = {}) {
    this.#ensureOpen();
    const response = this.#native.searchJson(JSON.stringify(normalizeSearchQuery(query)), tenantId(options) ?? this.#tenantId);
    return parseJson(response);
  }

  async traces(options = {}) {
    this.#ensureOpen();
    return parseJson(this.#native.tracesJson(tenantId(options) ?? this.#tenantId));
  }

  async sessions(options = {}) {
    this.#ensureOpen();
    const attrs = normalizeSessionAttrs(options);
    return parseJson(
      this.#native.sessionsJson(
        options.cursor ?? 0,
        options.limit ?? 50,
        options.filter,
        attrs ? JSON.stringify(attrs) : undefined,
        tenantId(options) ?? this.#tenantId,
      ),
    );
  }

  async trace(traceId, options = {}) {
    this.#ensureOpen();
    try {
      return parseJson(this.#native.traceJson(String(traceId), tenantId(options) ?? this.#tenantId));
    } catch (err) {
      if (String(err?.message ?? err).includes("status=404")) return null;
      throw err;
    }
  }

  async span(traceId, spanId, options = {}) {
    this.#ensureOpen();
    try {
      return parseJson(this.#native.spanJson(String(traceId), String(spanId), tenantId(options) ?? this.#tenantId));
    } catch (err) {
      if (String(err?.message ?? err).includes("status=404")) return null;
      throw err;
    }
  }

  async flush() {
    this.#ensureOpen();
    this.#native.flush();
  }

  async close() {
    if (this.#closed) return;
    this.#native.close();
    this.#closed = true;
  }

  #ensureOpen() {
    if (this.#closed) {
      throw new Error("YiTraceDB is closed");
    }
  }
}

export default YiTraceDB;
