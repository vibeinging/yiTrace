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

const ATTR_ALIASES = {
  projectId: "project_id",
  callSite: "call_site",
  taskFingerprint: "task_fingerprint",
  loopId: "loop_id",
  harnessVersion: "harness_version",
  validationStatus: "validation_status",
  stopReason: "stop_reason",
  externalRunId: "external_run_id",
  connectionIds: "connection_ids",
  dataSourceIds: "data_source_ids",
  schemaFingerprint: "schema_fingerprint",
  intentSignature: "intent_signature",
  reviewStatus: "review_status",
  evalStatus: "eval_status",
  pathMemoryId: "path_memory_id",
};

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
  for (const key of Object.keys(source)) {
    if (source[key] !== undefined) attrs[key] = source[key];
  }
  for (const key of [
    "project_id",
    "external_run_id",
    "skill",
    "mode",
    "call_site",
    "task_fingerprint",
    "loop_id",
    "harness_version",
    "validation_status",
    "stop_reason",
    "phase",
    "validator",
    "connection_ids",
    "data_source_ids",
    "schema_fingerprint",
    "intent_signature",
    "review_status",
    "eval_status",
    "path_memory_id",
  ]) {
    if (options[key] !== undefined) attrs[key] = options[key];
  }
  for (const [camel, snake] of Object.entries(ATTR_ALIASES)) {
    if (options[camel] !== undefined) attrs[snake] = options[camel];
  }
  return Object.keys(attrs).length > 0 ? attrs : undefined;
}

const METADATA_QUERY_ALIASES = {
  traceId: "trace_id",
  spanId: "span_id",
  targetType: "target_type",
  datasetId: "dataset_id",
  itemId: "item_id",
  datasetItemId: "dataset_item_id",
  evalRunId: "eval_run_id",
  ...ATTR_ALIASES,
};

const LIST_METADATA_KEYS = new Set([
  "annotation",
  "annotations",
  "annotationLabel",
  "annotation_label",
  "annotationSource",
  "annotation_source",
  "annotationStatus",
  "annotation_status",
  "annotationIncludeDeleted",
  "annotation_include_deleted",
  "annotationTarget",
  "annotation_target",
  "annotationScoreMin",
  "annotation_score_min",
  "annotationScoreMax",
  "annotation_score_max",
  "dataset",
  "datasetAssociation",
  "dataset_association",
  "datasetLink",
  "dataset_link",
  "datasetId",
  "dataset_id",
  "itemId",
  "item_id",
  "datasetItemId",
  "dataset_item_id",
  "evalRunId",
  "eval_run_id",
  "datasetSplit",
  "dataset_split",
  "datasetLabel",
  "dataset_label",
  "datasetScoreMin",
  "dataset_score_min",
  "datasetScoreMax",
  "dataset_score_max",
]);

function setNestedMetadataParam(params, group, key, value) {
  if (value === undefined || value === null) return;
  const prefix = group === "annotation" ? "annotation" : "dataset";
  if (key === "attrs" && plainObject(value)) {
    params.set(`${prefix}Attrs`, JSON.stringify(value));
    return;
  }
  const map =
    group === "annotation"
      ? {
          target: "annotationTarget",
          targetType: "annotationTarget",
          target_type: "annotationTarget",
          label: "annotationLabel",
          source: "annotationSource",
          status: "annotationStatus",
          includeDeleted: "annotationIncludeDeleted",
          include_deleted: "annotationIncludeDeleted",
          scoreMin: "annotationScoreMin",
          score_min: "annotationScoreMin",
          minScore: "annotationScoreMin",
          scoreMax: "annotationScoreMax",
          score_max: "annotationScoreMax",
          maxScore: "annotationScoreMax",
        }
      : {
          datasetId: "datasetId",
          dataset_id: "datasetId",
          dataset: "datasetId",
          itemId: "itemId",
          item_id: "itemId",
          datasetItemId: "itemId",
          dataset_item_id: "itemId",
          evalRunId: "evalRunId",
          eval_run_id: "evalRunId",
          split: "datasetSplit",
          label: "datasetLabel",
          scoreMin: "datasetScoreMin",
          score_min: "datasetScoreMin",
          minScore: "datasetScoreMin",
          scoreMax: "datasetScoreMax",
          score_max: "datasetScoreMax",
          maxScore: "datasetScoreMax",
        };
  const wireKey = map[key] ?? key;
  params.set(wireKey, String(value));
}

function metadataQueryString(filter = {}) {
  const params = new URLSearchParams();
  for (const [key, raw] of Object.entries(filter ?? {})) {
    if (raw === undefined || raw === null || key === "tenantId") continue;
    if ((key === "annotation" || key === "annotations") && plainObject(raw)) {
      for (const [childKey, childValue] of Object.entries(raw)) {
        setNestedMetadataParam(params, "annotation", childKey, childValue);
      }
      continue;
    }
    if ((key === "dataset" || key === "datasetAssociation" || key === "dataset_association" || key === "datasetLink" || key === "dataset_link") && plainObject(raw)) {
      for (const [childKey, childValue] of Object.entries(raw)) {
        setNestedMetadataParam(params, "dataset", childKey, childValue);
      }
      continue;
    }
    if (key === "attrs" && plainObject(raw)) {
      params.set("attrs", JSON.stringify(raw));
      continue;
    }
    const wireKey = METADATA_QUERY_ALIASES[key] ?? key;
    params.set(wireKey, String(raw));
  }
  const out = params.toString();
  return out.length > 0 ? out : undefined;
}

function metadataListQueryString(options = {}) {
  const picked = {};
  for (const [key, value] of Object.entries(options ?? {})) {
    if (LIST_METADATA_KEYS.has(key)) picked[key] = value;
  }
  return metadataQueryString(picked);
}

const GOLDEN_PATH_QUERY_ALIASES = {
  goldenPathId: "goldenPathId",
  golden_path_id: "golden_path_id",
  taskFingerprint: "taskFingerprint",
  task_fingerprint: "task_fingerprint",
  trajectorySignature: "trajectorySignature",
  trajectory_signature: "trajectory_signature",
  pathSignature: "pathSignature",
  sourceTraceId: "sourceTraceId",
  source_trace_id: "source_trace_id",
  traceId: "traceId",
  trace_id: "trace_id",
  ...ATTR_ALIASES,
};

function goldenPathQueryString(filter = {}) {
  const params = new URLSearchParams();
  for (const [key, raw] of Object.entries(filter ?? {})) {
    if (raw === undefined || raw === null || key === "tenantId") continue;
    if (key === "attrs" && plainObject(raw)) {
      params.set("attrs", JSON.stringify(raw));
      continue;
    }
    const wireKey = GOLDEN_PATH_QUERY_ALIASES[key] ?? key;
    params.set(wireKey, String(raw));
  }
  const out = params.toString();
  return out.length > 0 ? out : undefined;
}

const RETENTION_POLICY_QUERY_ALIASES = {
  policyId: "policyId",
  policy_id: "policy_id",
  policyName: "policyName",
  policy_name: "policy_name",
};

function retentionPolicyQueryString(filter = {}) {
  const params = new URLSearchParams();
  for (const [key, raw] of Object.entries(filter ?? {})) {
    if (raw === undefined || raw === null || key === "tenantId") continue;
    const wireKey = RETENTION_POLICY_QUERY_ALIASES[key] ?? key;
    params.set(wireKey, String(raw));
  }
  const out = params.toString();
  return out.length > 0 ? out : undefined;
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
      ...(normalizeSessionAttrs(this.#defaults) ?? {}),
      ...(normalizeSessionAttrs(options) ?? {}),
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
    setIfDefined(event, "cached_input_tokens", optionValue(options, "cached_input_tokens", "cachedInputTokens"));
    setIfDefined(event, "reasoning_tokens", optionValue(options, "reasoning_tokens", "reasoningTokens"));
    setIfDefined(event, "total_tokens", optionValue(options, "total_tokens", "totalTokens"));
    setIfDefined(event, "cost_usd", optionValue(options, "cost_usd", "costUsd"));
    setIfDefined(event, "cost_usd_nanos", optionValue(options, "cost_usd_nanos", "costUsdNanos"));
    setIfDefined(event, "cost_currency", optionValue(options, "cost_currency", "costCurrency"));
    setIfDefined(event, "provider", optionValue(options, "provider", "llmProvider"));
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

  async traceSearch(query = {}, options = {}) {
    this.#ensureOpen();
    const response = this.#native.traceSearchJson(JSON.stringify(normalizeSearchQuery(query)), tenantId(options) ?? this.#tenantId);
    return parseJson(response);
  }

  async traceAggregate(query = {}, options = {}) {
    this.#ensureOpen();
    const response = this.#native.traceAggregateJson(JSON.stringify(normalizeSearchQuery(query)), tenantId(options) ?? this.#tenantId);
    return parseJson(response);
  }

  async trajectoryGroups(query = {}, options = {}) {
    this.#ensureOpen();
    const response = this.#native.trajectoryGroupsJson(JSON.stringify(normalizeSearchQuery(query)), tenantId(options) ?? this.#tenantId);
    return parseJson(response);
  }

  async traceTrajectories(query = {}, options = {}) {
    this.#ensureOpen();
    const response = this.#native.traceTrajectoriesJson(JSON.stringify(normalizeSearchQuery(query)), tenantId(options) ?? this.#tenantId);
    return parseJson(response);
  }

  async storageStats(query = {}, options = {}) {
    this.#ensureOpen();
    const response = this.#native.storageStatsJson(JSON.stringify(normalizeSearchQuery(query)), tenantId(options) ?? this.#tenantId);
    return parseJson(response);
  }

  async retentionPlan(query = {}, options = {}) {
    this.#ensureOpen();
    const response = this.#native.retentionPlanJson(JSON.stringify(normalizeSearchQuery(query)), tenantId(options) ?? this.#tenantId);
    return parseJson(response);
  }

  async applyRetention(query = {}, options = {}) {
    this.#ensureOpen();
    const response = this.#native.applyRetentionJson(JSON.stringify(normalizeSearchQuery({ ...(query ?? {}), apply: true })), tenantId(options) ?? this.#tenantId);
    return parseJson(response);
  }

  async retentionAudits(query = {}, options = {}) {
    this.#ensureOpen();
    const response = this.#native.retentionAuditsJson(JSON.stringify(query ?? {}), tenantId(options) ?? this.#tenantId);
    return parseJson(response);
  }

  async createRetentionPolicy(policy = {}, options = {}) {
    this.#ensureOpen();
    const response = this.#native.createRetentionPolicyJson(JSON.stringify(policy ?? {}), tenantId(options) ?? this.#tenantId);
    return parseJson(response);
  }

  async retentionPolicies(filter = {}, options = {}) {
    this.#ensureOpen();
    const response = this.#native.retentionPoliciesJson(retentionPolicyQueryString(filter), tenantId(options) ?? this.#tenantId);
    return parseJson(response);
  }

  async runRetentionPolicies(query = {}, options = {}) {
    this.#ensureOpen();
    const response = this.#native.runRetentionPoliciesJson(JSON.stringify(query ?? {}), tenantId(options) ?? this.#tenantId);
    return parseJson(response);
  }

  async createGoldenPath(candidate = {}, options = {}) {
    this.#ensureOpen();
    const response = this.#native.createGoldenPathJson(JSON.stringify(candidate ?? {}), tenantId(options) ?? this.#tenantId);
    return parseJson(response);
  }

  async goldenPaths(filter = {}, options = {}) {
    this.#ensureOpen();
    const response = this.#native.goldenPathsJson(goldenPathQueryString(filter), tenantId(options) ?? this.#tenantId);
    return parseJson(response);
  }

  async updateGoldenPathStatus(goldenPathId, update, options = {}) {
    this.#ensureOpen();
    const body = typeof update === "string" ? { status: update } : { ...(update ?? {}) };
    const response = this.#native.updateGoldenPathStatusJson(String(goldenPathId), JSON.stringify(body), tenantId(options) ?? this.#tenantId);
    return parseJson(response);
  }

  async pathAdherence(goldenPathIdOrQuery, traceIdOrOptions, options = {}) {
    this.#ensureOpen();
    let query;
    let opts;
    if (plainObject(goldenPathIdOrQuery)) {
      query = { ...goldenPathIdOrQuery };
      opts = traceIdOrOptions ?? {};
    } else {
      query = { goldenPathId: goldenPathIdOrQuery, traceId: traceIdOrOptions };
      opts = options ?? {};
    }
    const response = this.#native.pathAdherenceJson(JSON.stringify(query), tenantId(opts) ?? this.#tenantId);
    return parseJson(response);
  }

  async goldenPathEvidence(goldenPathIdOrQuery, options = {}) {
    this.#ensureOpen();
    const query = plainObject(goldenPathIdOrQuery)
      ? { ...goldenPathIdOrQuery }
      : { goldenPathId: goldenPathIdOrQuery };
    const response = this.#native.goldenPathEvidenceJson(JSON.stringify(query), tenantId(options) ?? this.#tenantId);
    return parseJson(response);
  }

  async goldenPathExport(query = {}, options = {}) {
    this.#ensureOpen();
    const response = this.#native.goldenPathExportJson(JSON.stringify(query ?? {}), tenantId(options) ?? this.#tenantId);
    return parseJson(response);
  }

  async goldenPathHealth(goldenPathIdOrQuery, queryOrOptions = {}, options = {}) {
    this.#ensureOpen();
    let query;
    let opts;
    if (plainObject(goldenPathIdOrQuery)) {
      query = { ...goldenPathIdOrQuery };
      opts = queryOrOptions ?? {};
    } else {
      query = { ...(plainObject(queryOrOptions) ? queryOrOptions : {}), goldenPathId: goldenPathIdOrQuery };
      opts = options ?? {};
    }
    const response = this.#native.goldenPathHealthJson(JSON.stringify(query), tenantId(opts) ?? this.#tenantId);
    return parseJson(response);
  }

  async traceDiff(leftTraceIdOrQuery, rightTraceIdOrOptions, options = {}) {
    this.#ensureOpen();
    let query;
    let opts;
    if (plainObject(leftTraceIdOrQuery)) {
      query = { ...leftTraceIdOrQuery };
      opts = rightTraceIdOrOptions ?? {};
    } else {
      query = { leftTraceId: leftTraceIdOrQuery, rightTraceId: rightTraceIdOrOptions };
      opts = options ?? {};
    }
    const response = this.#native.traceDiffJson(JSON.stringify(query), tenantId(opts) ?? this.#tenantId);
    return parseJson(response);
  }

  async loops(options = {}) {
    this.#ensureOpen();
    const attrs = normalizeSessionAttrs(options);
    return parseJson(
      this.#native.loopsJson(
        options.cursor ?? 0,
        options.limit ?? 50,
        options.filter ?? options.text ?? options.q,
        attrs ? JSON.stringify(attrs) : undefined,
        metadataListQueryString(options),
        tenantId(options) ?? this.#tenantId,
      ),
    );
  }

  async loop(loopId, options = {}) {
    this.#ensureOpen();
    try {
      return parseJson(
        this.#native.loopJson(
          String(loopId),
          options.filter ?? options.text ?? options.q,
          metadataListQueryString(options),
          tenantId(options) ?? this.#tenantId,
        ),
      );
    } catch (err) {
      if (String(err?.message ?? err).includes("status=404")) return null;
      throw err;
    }
  }

  async taskTraces(taskFingerprint, options = {}) {
    this.#ensureOpen();
    const attrs = normalizeSessionAttrs(options);
    return parseJson(
      this.#native.taskTracesJson(
        String(taskFingerprint),
        options.cursor ?? 0,
        options.limit ?? 50,
        options.filter ?? options.text ?? options.q,
        attrs ? JSON.stringify(attrs) : undefined,
        metadataListQueryString(options),
        tenantId(options) ?? this.#tenantId,
      ),
    );
  }

  async annotate(annotation, options = {}) {
    this.#ensureOpen();
    const response = this.#native.createAnnotationJson(JSON.stringify(annotation ?? {}), tenantId(options) ?? this.#tenantId);
    return parseJson(response);
  }

  async updateAnnotation(annotationId, update = {}, options = {}) {
    this.#ensureOpen();
    const response = this.#native.updateAnnotationJson(String(annotationId), JSON.stringify(update ?? {}), tenantId(options) ?? this.#tenantId);
    return parseJson(response);
  }

  async deleteAnnotation(annotationId, options = {}) {
    this.#ensureOpen();
    const response = this.#native.deleteAnnotationJson(String(annotationId), JSON.stringify(options ?? {}), tenantId(options) ?? this.#tenantId);
    return parseJson(response);
  }

  async annotations(filter = {}, options = {}) {
    this.#ensureOpen();
    const response = this.#native.annotationsJson(metadataQueryString(filter), tenantId(options) ?? this.#tenantId);
    return parseJson(response);
  }

  async linkDatasetItem(association, options = {}) {
    this.#ensureOpen();
    const response = this.#native.createDatasetAssociationJson(JSON.stringify(association ?? {}), tenantId(options) ?? this.#tenantId);
    return parseJson(response);
  }

  async datasetAssociations(filter = {}, options = {}) {
    this.#ensureOpen();
    const response = this.#native.datasetAssociationsJson(metadataQueryString(filter), tenantId(options) ?? this.#tenantId);
    return parseJson(response);
  }

  async traces(options = {}) {
    this.#ensureOpen();
    const attrs = normalizeSessionAttrs(options);
    const metadata = metadataListQueryString(options);
    return parseJson(
      this.#native.tracesJson(
        attrs ? JSON.stringify(attrs) : undefined,
        metadata,
        tenantId(options) ?? this.#tenantId,
      ),
    );
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
        metadataListQueryString(options),
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

  async traceSnapshot(traceId, options = {}) {
    this.#ensureOpen();
    try {
      return parseJson(this.#native.traceSnapshotJson(String(traceId), tenantId(options) ?? this.#tenantId));
    } catch (err) {
      if (String(err?.message ?? err).includes("status=404")) return null;
      throw err;
    }
  }

  async spans(traceId, options = {}) {
    this.#ensureOpen();
    try {
      return parseJson(
        this.#native.spansJson(
          String(traceId),
          options.cursor ?? 0,
          options.limit ?? 50,
          Boolean(options.includeFull ?? options.include_full ?? options.full),
          tenantId(options) ?? this.#tenantId,
        ),
      );
    } catch (err) {
      if (String(err?.message ?? err).includes("status=404")) return null;
      throw err;
    }
  }

  async spansBatch(traceId, spanIdsOrOptions = {}, options = {}) {
    this.#ensureOpen();
    const body = Array.isArray(spanIdsOrOptions)
      ? { spanIds: spanIdsOrOptions, includeFull: Boolean(options.includeFull ?? options.include_full ?? options.full) }
      : { ...spanIdsOrOptions };
    try {
      return parseJson(this.#native.spansBatchJson(String(traceId), JSON.stringify(body), tenantId(options) ?? this.#tenantId));
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
