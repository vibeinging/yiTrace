const { NativeYiTraceDB } = require("./native.cjs");

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

const ATTR_ALIASES = {
  projectId: "project_id",
  callSite: "call_site",
  taskFingerprint: "task_fingerprint",
  loopId: "loop_id",
  harnessVersion: "harness_version",
  schemaFingerprint: "schema_fingerprint",
  intentSignature: "intent_signature",
  validationStatus: "validation_status",
  reviewStatus: "review_status",
  evalStatus: "eval_status",
  pathMemoryId: "path_memory_id",
  stopReason: "stop_reason",
};

const ATTR_KEYS = [
  "project_id",
  "skill",
  "mode",
  "call_site",
  "task_fingerprint",
  "loop_id",
  "harness_version",
  "schema_fingerprint",
  "intent_signature",
  "validation_status",
  "review_status",
  "eval_status",
  "path_memory_id",
  "stop_reason",
  "phase",
  "validator",
];

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

function normalizeAttrs(options = {}) {
  const attrs = {};
  const source = plainObject(options.attrs) ? options.attrs : {};
  for (const key of ATTR_KEYS) {
    if (source[key] !== undefined) attrs[key] = source[key];
    if (options[key] !== undefined) attrs[key] = options[key];
  }
  for (const [camel, snake] of Object.entries(ATTR_ALIASES)) {
    if (source[camel] !== undefined) attrs[snake] = source[camel];
    if (options[camel] !== undefined) attrs[snake] = options[camel];
  }
  return Object.keys(attrs).length > 0 ? attrs : undefined;
}

function normalizeMetadataBody(input = {}) {
  const out = { ...input };
  const attrs = { ...(plainObject(input.attrs) ? input.attrs : {}) };
  for (const key of ATTR_KEYS) {
    if (input[key] !== undefined) attrs[key] = input[key];
  }
  for (const [camel, snake] of Object.entries(ATTR_ALIASES)) {
    if (input[camel] !== undefined) attrs[snake] = input[camel];
  }
  if (Object.keys(attrs).length > 0) out.attrs = attrs;
  return out;
}

function metadataQueryString(options = {}) {
  const params = new URLSearchParams();
  for (const key of [
    "cursor",
    "offset",
    "limit",
    "target",
    "label",
    "name",
    "source",
    "status",
    "split",
    "enabled",
  ]) {
    if (options[key] !== undefined && options[key] !== null) params.set(key, String(options[key]));
  }
  const aliases = {
    traceId: "traceId",
    trace_id: "trace_id",
    spanId: "spanId",
    span_id: "span_id",
    includeDeleted: "includeDeleted",
    include_deleted: "include_deleted",
    datasetId: "datasetId",
    dataset_id: "dataset_id",
    itemId: "itemId",
    item_id: "item_id",
    datasetItemId: "datasetItemId",
    dataset_item_id: "dataset_item_id",
    evalRunId: "evalRunId",
    eval_run_id: "eval_run_id",
    auditId: "auditId",
    audit_id: "audit_id",
    policyId: "policyId",
    policy_id: "policy_id",
    createdAfterNs: "createdAfterNs",
    created_after_ns: "created_after_ns",
    createdBeforeNs: "createdBeforeNs",
    created_before_ns: "created_before_ns",
    minCreatedAtNs: "minCreatedAtNs",
    maxCreatedAtNs: "maxCreatedAtNs",
  };
  for (const [from, to] of Object.entries(aliases)) {
    if (options[from] !== undefined && options[from] !== null) params.set(to, String(options[from]));
  }
  const attrs = normalizeMetadataBody(options).attrs;
  if (attrs && Object.keys(attrs).length > 0) params.set("attrs", JSON.stringify(attrs));
  return params.toString();
}

class SpanEventBuilder {
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

function createSpanEventBuilder(defaults = {}) {
  return new SpanEventBuilder(defaults);
}

class YiTraceDB {
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

  async traceSearch(query = {}, options = {}) {
    this.#ensureOpen();
    const response = this.#native.traceSearchJson(JSON.stringify(normalizeSearchQuery(query)), tenantId(options) ?? this.#tenantId);
    return parseJson(response);
  }

  async traceAggregate(query = {}, options = {}) {
    this.#ensureOpen();
    const response = this.#native.traceAggregateJson(JSON.stringify(query), tenantId(options) ?? this.#tenantId);
    return parseJson(response);
  }

  async storageStats(query = {}, options = {}) {
    this.#ensureOpen();
    const response = this.#native.storageStatsJson(JSON.stringify(query), tenantId(options) ?? this.#tenantId);
    return parseJson(response);
  }

  async retentionPlan(query = {}, options = {}) {
    this.#ensureOpen();
    const response = this.#native.retentionPlanJson(JSON.stringify(query), tenantId(options) ?? this.#tenantId);
    return parseJson(response);
  }

  async applyRetention(query = {}, options = {}) {
    this.#ensureOpen();
    const response = this.#native.applyRetentionJson(JSON.stringify(query), tenantId(options) ?? this.#tenantId);
    return parseJson(response);
  }

  async retentionAudits(options = {}) {
    this.#ensureOpen();
    const response = this.#native.retentionAuditsJson(metadataQueryString(options), tenantId(options) ?? this.#tenantId);
    return parseJson(response);
  }

  async createRetentionPolicy(policy, options = {}) {
    this.#ensureOpen();
    const response = this.#native.createRetentionPolicyJson(JSON.stringify(policy), tenantId(options) ?? this.#tenantId);
    return parseJson(response);
  }

  async retentionPolicies(options = {}) {
    this.#ensureOpen();
    const response = this.#native.retentionPoliciesJson(metadataQueryString(options), tenantId(options) ?? this.#tenantId);
    return parseJson(response);
  }

  async runRetentionPolicies(query = {}, options = {}) {
    this.#ensureOpen();
    const response = this.#native.runRetentionPoliciesJson(JSON.stringify(query), tenantId(options) ?? this.#tenantId);
    return parseJson(response);
  }

  async traceTrajectories(query = {}, options = {}) {
    this.#ensureOpen();
    const response = this.#native.traceTrajectoriesJson(JSON.stringify(normalizeSearchQuery(query)), tenantId(options) ?? this.#tenantId);
    return parseJson(response);
  }

  async trajectoryGroups(query = {}, options = {}) {
    this.#ensureOpen();
    const response = this.#native.trajectoryGroupsJson(JSON.stringify(normalizeSearchQuery(query)), tenantId(options) ?? this.#tenantId);
    return parseJson(response);
  }

  async traceDiff(leftOrQuery, rightTraceId, options = {}) {
    this.#ensureOpen();
    const query = plainObject(leftOrQuery)
      ? leftOrQuery
      : { baseTraceId: leftOrQuery, candidateTraceId: rightTraceId };
    const response = this.#native.traceDiffJson(JSON.stringify(query), tenantId(options) ?? this.#tenantId);
    return parseJson(response);
  }

  async annotate(annotation, options = {}) {
    this.#ensureOpen();
    const response = this.#native.annotateJson(JSON.stringify(normalizeMetadataBody(annotation)), tenantId(options) ?? this.#tenantId);
    return parseJson(response);
  }

  async annotations(options = {}) {
    this.#ensureOpen();
    const response = this.#native.annotationsJson(metadataQueryString(options), tenantId(options) ?? this.#tenantId);
    return parseJson(response);
  }

  async updateAnnotation(annotationId, update = {}, options = {}) {
    this.#ensureOpen();
    const response = this.#native.updateAnnotationJson(String(annotationId), JSON.stringify(normalizeMetadataBody(update)), tenantId(options) ?? this.#tenantId);
    return parseJson(response);
  }

  async deleteAnnotation(annotationId, deleteInfo = {}, options = {}) {
    this.#ensureOpen();
    const response = this.#native.deleteAnnotationJson(String(annotationId), JSON.stringify(deleteInfo ?? {}), tenantId(options) ?? this.#tenantId);
    return parseJson(response);
  }

  async linkDatasetItem(association, options = {}) {
    this.#ensureOpen();
    const response = this.#native.linkDatasetItemJson(JSON.stringify(normalizeMetadataBody(association)), tenantId(options) ?? this.#tenantId);
    return parseJson(response);
  }

  async datasetAssociations(options = {}) {
    this.#ensureOpen();
    const response = this.#native.datasetAssociationsJson(metadataQueryString(options), tenantId(options) ?? this.#tenantId);
    return parseJson(response);
  }

  async traces(options = {}) {
    this.#ensureOpen();
    return parseJson(this.#native.tracesJson(tenantId(options) ?? this.#tenantId));
  }

  async sessions(options = {}) {
    this.#ensureOpen();
    const attrs = normalizeAttrs(options);
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

  async loops(options = {}) {
    this.#ensureOpen();
    const attrs = normalizeAttrs(options);
    return parseJson(
      this.#native.loopsJson(
        options.cursor ?? 0,
        options.limit ?? 50,
        attrs ? JSON.stringify(attrs) : undefined,
        tenantId(options) ?? this.#tenantId,
      ),
    );
  }

  async loop(loopId, options = {}) {
    this.#ensureOpen();
    try {
      return parseJson(this.#native.loopJson(String(loopId), tenantId(options) ?? this.#tenantId));
    } catch (error) {
      if (String(error?.message ?? error).includes("status=404")) return null;
      throw error;
    }
  }

  async taskTraces(fingerprint, options = {}) {
    this.#ensureOpen();
    const attrs = normalizeAttrs(options);
    return parseJson(
      this.#native.taskTracesJson(
        String(fingerprint),
        options.cursor ?? 0,
        options.limit ?? 50,
        attrs ? JSON.stringify(attrs) : undefined,
        tenantId(options) ?? this.#tenantId,
      ),
    );
  }

  async trace(traceId, options = {}) {
    this.#ensureOpen();
    try {
      return parseJson(this.#native.traceJson(String(traceId), tenantId(options) ?? this.#tenantId));
    } catch (error) {
      if (String(error?.message ?? error).includes("status=404")) return null;
      throw error;
    }
  }

  async span(traceId, spanId, options = {}) {
    this.#ensureOpen();
    try {
      return parseJson(this.#native.spanJson(String(traceId), String(spanId), tenantId(options) ?? this.#tenantId));
    } catch (error) {
      if (String(error?.message ?? error).includes("status=404")) return null;
      throw error;
    }
  }

  async flush() {
    this.#ensureOpen();
    this.#native.flush();
  }

  async close() {
    if (!this.#closed) {
      this.#native.close();
      this.#closed = true;
    }
  }

  #ensureOpen() {
    if (this.#closed) {
      throw new Error("YiTraceDB is closed");
    }
  }
}

module.exports = { YiTraceDB, SpanEventBuilder, createSpanEventBuilder };
