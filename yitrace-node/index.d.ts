export type TenantId = string | number;
export type WireNumber = number | string;
export type EmbeddingVector = ReadonlyArray<number> | Float32Array | Float64Array;
export type EmbedText = (text: string) => EmbeddingVector | Promise<EmbeddingVector>;

export interface Embedder {
  model?: string;
  dimensions?: number;
  dimension?: number;
  dim?: number;
  embedQuery?: EmbedText;
  embed?: EmbedText;
  embedDocuments?: (texts: string[]) => EmbeddingVector[] | Promise<EmbeddingVector[]>;
  embedBatch?: (texts: string[]) => EmbeddingVector[] | Promise<EmbeddingVector[]>;
}

export interface OpenOptions {
  dataDir: string;
  tenantId?: TenantId;
  embedder?: Embedder | EmbedText;
  embedding?: Embedder | EmbedText;
  dimensions?: number;
  embeddingDimensions?: number;
  autoIndexEmbeddings?: boolean;
}

export interface TenantOptions {
  tenantId?: TenantId;
}

export interface IngestOptions extends TenantOptions {
  indexEmbeddings?: boolean;
}

export type TraceAttrKey =
  | "project_id"
  | "skill"
  | "mode"
  | "call_site"
  | "task_fingerprint"
  | "loop_id"
  | "harness_version"
  | "schema_fingerprint"
  | "intent_signature"
  | "validation_status"
  | "review_status"
  | "eval_status"
  | "path_memory_id"
  | "stop_reason"
  | "phase"
  | "validator";

export interface ReadModelOptions extends TenantOptions {
  cursor?: number;
  limit?: number;
  projectId?: unknown;
  project_id?: unknown;
  skill?: unknown;
  mode?: unknown;
  callSite?: unknown;
  call_site?: unknown;
  taskFingerprint?: unknown;
  task_fingerprint?: unknown;
  loopId?: unknown;
  loop_id?: unknown;
  validationStatus?: unknown;
  validation_status?: unknown;
  reviewStatus?: unknown;
  review_status?: unknown;
  evalStatus?: unknown;
  eval_status?: unknown;
  attrs?: Partial<Record<TraceAttrKey, unknown>>;
  [key: string]: unknown;
}

export interface SpanEvent {
  trace_id: TenantId;
  span_id: TenantId;
  ts: WireNumber;
  seq: number;
  event_type: number;
  ext_span_id: string;
  external_trace_id?: string | null;
  external_span_id?: string | null;
  parent_span_id?: TenantId | null;
  external_parent_span_id?: string | null;
  session_id?: TenantId | null;
  external_session_id?: string | null;
  turn_id?: TenantId | null;
  span_name?: string | null;
  display_name?: string | null;
  agent_name?: string | null;
  tool_name?: string | null;
  model?: string | null;
  input_text?: string | null;
  output_text?: string | null;
  duration_ns?: WireNumber | null;
  input_tokens?: WireNumber | null;
  output_tokens?: WireNumber | null;
  cost_usd?: number | null;
  status?: number | null;
  messages?: unknown[];
  tool_calls?: unknown[];
  logs?: string[];
  attrs?: Record<string, unknown>;
  tenant_id?: TenantId;
  [key: string]: unknown;
}

export interface IngestResult {
  ingested: number;
  [key: string]: unknown;
}

export interface SearchFilter {
  traceId?: TenantId;
  trace_id?: TenantId;
  agentName?: string;
  agent_name?: string;
  toolName?: string;
  tool_name?: string;
  model?: string;
  status?: number;
  timeFrom?: number;
  time_from?: number;
  timeTo?: number;
  time_to?: number;
  projectId?: unknown;
  project_id?: unknown;
  skill?: unknown;
  mode?: unknown;
  callSite?: unknown;
  call_site?: unknown;
  attrs?: Partial<Record<TraceAttrKey, unknown>>;
  [key: string]: unknown;
}

export interface SearchQuery {
  text?: string;
  q?: string;
  vector?: EmbeddingVector | "auto" | true;
  mode?: "bm25" | "keyword" | "semantic" | "vector" | "hybrid" | string;
  searchMode?: "bm25" | "keyword" | "semantic" | "vector" | "hybrid" | string;
  hybrid?: boolean;
  semantic?: boolean;
  k?: number;
  filter?: SearchFilter;
  [key: string]: unknown;
}

export interface EmbeddingInput {
  traceId?: TenantId;
  trace_id?: TenantId;
  spanId?: TenantId;
  span_id?: TenantId;
  text?: string;
  inputText?: string;
  input_text?: string;
  outputText?: string;
  output_text?: string;
  vector?: EmbeddingVector | "auto";
  embedding?: EmbeddingVector | "auto";
  [key: string]: unknown;
}

export interface IndexEmbeddingsResult {
  indexed: number;
}

export interface TraceSearchQuery {
  text?: string;
  q?: string;
  cursor?: number;
  offset?: number;
  limit?: number;
  sortBy?: "trace" | "duration" | "tokens" | "status" | string;
  sort_by?: string;
  filter?: SearchFilter & {
    sessionId?: TenantId;
    session_id?: TenantId;
    spanId?: TenantId;
    span_id?: TenantId;
    externalTraceId?: string;
    external_trace_id?: string;
    externalSpanId?: string;
    external_span_id?: string;
    externalSessionId?: string;
    external_session_id?: string;
    toolName?: string;
    tool_name?: string;
    model?: string;
  };
  [key: string]: unknown;
}

export interface ReadPlan {
  source: "filter_index" | "aggregate_rollup" | "scan" | string;
  usedFilterIndex: boolean;
  candidateSpanKeys: number | null;
  scannedSegments: number;
  matchedSpans: number;
  fallbackReason: string | null;
  unsupportedAttrKeys: string[];
  traceFetchSource: "trajectory_rollup" | "scan" | string | null;
  traceFetchSpanCount: number | null;
  traceFetchFallbackReason: string | null;
  [key: string]: unknown;
}

export interface TraceSearchPage<T = Record<string, unknown>> {
  items: T[];
  total: number;
  cursor: number;
  limit: number;
  scannedSpans: number;
  readPlan?: ReadPlan;
  [key: string]: unknown;
}

export interface TraceAggregateQuery extends TraceSearchQuery {
  groupBy?: string | string[];
  group_by?: string | string[];
}

export interface TraceAggregateResult<T = Record<string, unknown>> {
  items: T[];
  total: number;
  groupBy: string[];
  scannedSpans: number;
  readPlan?: ReadPlan;
  [key: string]: unknown;
}

export interface StorageStatsResult<T = Record<string, unknown>> {
  total: T;
  groups: T[];
  groupBy: string[];
  scannedSpans: number;
  readPlan?: ReadPlan;
  [key: string]: unknown;
}

export interface TraceTrajectoryPage<T = Record<string, unknown>> {
  items: T[];
  total: number;
  cursor: number;
  limit: number;
  scannedSpans: number;
  readPlan?: ReadPlan;
  [key: string]: unknown;
}

export interface TrajectoryGroupPage<T = Record<string, unknown>> {
  items: T[];
  total: number;
  scannedSpans: number;
  readPlan?: ReadPlan;
  [key: string]: unknown;
}

export interface TraceDiffResult {
  sameSignature: boolean;
  commonPrefix: number;
  left: Record<string, unknown>;
  right: Record<string, unknown>;
  delta: Record<string, unknown>;
  missingSteps: string[];
  extraSteps: string[];
  [key: string]: unknown;
}

export interface MetadataPage<T = Record<string, unknown>> {
  items: T[];
  count: number;
  total: number;
  pageCount: number;
  nextCursor: number | null;
  metadataIndex?: string;
  [key: string]: unknown;
}

export interface AnnotationInput extends ReadModelOptions {
  traceId?: TenantId;
  trace_id?: TenantId;
  spanId?: TenantId;
  span_id?: TenantId;
  target?: "trace" | "span" | string;
  label: string;
  score?: number | null;
  reason?: string | null;
  source?: string | null;
  status?: "active" | "resolved" | "rejected" | "deleted" | string;
  reviewer?: string | null;
  attrs?: Record<string, unknown>;
}

export interface AnnotationPatch extends Partial<Omit<AnnotationInput, "traceId" | "trace_id" | "spanId" | "span_id" | "target">> {
  replaceAttrs?: boolean;
  replace_attrs?: boolean;
}

export interface AnnotationQuery extends ReadModelOptions {
  traceId?: TenantId;
  trace_id?: TenantId;
  spanId?: TenantId;
  span_id?: TenantId;
  target?: "trace" | "span" | string;
  label?: string;
  source?: string;
  status?: "active" | "resolved" | "rejected" | "deleted" | string;
  includeDeleted?: boolean;
  include_deleted?: boolean;
}

export interface TraceAnnotation {
  annotationId: TenantId;
  tenantId?: TenantId | null;
  target: "trace" | "span" | string;
  traceId: TenantId;
  spanId?: TenantId | null;
  externalTraceId?: string | null;
  externalSpanId?: string | null;
  label: string;
  score?: number | null;
  reason?: string | null;
  source?: string | null;
  status: string;
  reviewer?: string | null;
  attrs?: Record<string, unknown>;
  [key: string]: unknown;
}

export interface DatasetAssociationInput extends ReadModelOptions {
  datasetId?: string;
  dataset_id?: string;
  dataset?: string;
  itemId?: string;
  item_id?: string;
  datasetItemId?: string;
  dataset_item_id?: string;
  traceId?: TenantId;
  trace_id?: TenantId;
  spanId?: TenantId;
  span_id?: TenantId;
  snapshotId?: string | null;
  snapshot_id?: string | null;
  snapshotHash?: string | null;
  snapshot_hash?: string | null;
  evalRunId?: string | null;
  eval_run_id?: string | null;
  split?: string | null;
  label?: string | null;
  score?: number | null;
  attrs?: Record<string, unknown>;
}

export interface DatasetAssociationQuery extends ReadModelOptions {
  datasetId?: string;
  dataset_id?: string;
  dataset?: string;
  itemId?: string;
  item_id?: string;
  datasetItemId?: string;
  dataset_item_id?: string;
  traceId?: TenantId;
  trace_id?: TenantId;
  spanId?: TenantId;
  span_id?: TenantId;
  evalRunId?: string;
  eval_run_id?: string;
  split?: string;
  label?: string;
}

export interface DatasetAssociation {
  associationId: TenantId;
  tenantId?: TenantId | null;
  datasetId: string;
  itemId: string;
  traceId: TenantId;
  spanId?: TenantId | null;
  externalTraceId?: string | null;
  externalSpanId?: string | null;
  snapshotId?: string | null;
  snapshotHash?: string | null;
  evalRunId?: string | null;
  split?: string | null;
  label?: string | null;
  score?: number | null;
  attrs?: Record<string, unknown>;
  [key: string]: unknown;
}

export interface RetentionPlanQuery extends TraceSearchQuery {
  deleteBeforeTs?: WireNumber;
  delete_before_ts?: WireNumber;
  olderThanTs?: WireNumber;
  older_than_ts?: WireNumber;
  protect?: {
    annotations?: boolean;
    datasetAssociations?: boolean;
    dataset_associations?: boolean;
    snapshots?: boolean;
    evalLinks?: boolean;
    eval_links?: boolean;
    pathMemory?: boolean;
    path_memory?: boolean;
    [key: string]: unknown;
  };
  compact?: boolean;
  requestedBy?: string;
  requested_by?: string;
  reason?: string;
  [key: string]: unknown;
}

export interface RetentionAuditQuery extends TenantOptions {
  cursor?: number;
  offset?: number;
  limit?: number;
  auditId?: TenantId;
  audit_id?: TenantId;
  source?: string;
  createdAfterNs?: WireNumber;
  created_after_ns?: WireNumber;
  createdBeforeNs?: WireNumber;
  created_before_ns?: WireNumber;
  minCreatedAtNs?: WireNumber;
  maxCreatedAtNs?: WireNumber;
}

export interface RetentionPolicyInput extends TenantOptions {
  name: string;
  intervalNs?: WireNumber;
  interval_ns?: WireNumber;
  nextRunAtNs?: WireNumber;
  next_run_at_ns?: WireNumber;
  enabled?: boolean;
  query: RetentionPlanQuery;
  source?: string;
  reason?: string;
  [key: string]: unknown;
}

export interface RetentionPolicyQuery extends TenantOptions {
  cursor?: number;
  offset?: number;
  limit?: number;
  policyId?: TenantId;
  policy_id?: TenantId;
  name?: string;
  enabled?: boolean;
}

export interface RetentionRunDueQuery extends TenantOptions {
  nowNs?: WireNumber;
  now_ns?: WireNumber;
  limit?: number;
  includeDisabled?: boolean;
  include_disabled?: boolean;
  policyId?: TenantId;
  policy_id?: TenantId;
  name?: string;
}

export interface SearchHit {
  trace_id: TenantId;
  span_id?: TenantId;
  external_trace_id?: string | null;
  external_span_id?: string | null;
  attrs?: Record<string, unknown>;
  score?: number;
  text?: string;
  [key: string]: unknown;
}

export interface TraceSummary {
  traceId?: TenantId;
  trace_id?: TenantId;
  externalTraceId?: string | null;
  external_trace_id?: string | null;
  [key: string]: unknown;
}

export interface SpanLogEvent {
  eventId: string;
  ts: WireNumber;
  seq: number;
  eventType: number;
  messages: string[];
  attrs?: Record<string, unknown>;
  [key: string]: unknown;
}

export interface TraceSpan {
  id?: TenantId;
  spanId?: TenantId;
  span_id?: TenantId;
  parentId?: TenantId | null;
  externalTraceId?: string | null;
  externalSpanId?: string | null;
  externalParentSpanId?: string | null;
  externalSessionId?: string | null;
  kind?: string;
  name?: string;
  spanName?: string | null;
  displayName?: string | null;
  actorId?: string;
  agentName?: string | null;
  toolName?: string | null;
  attrs?: Record<string, unknown>;
  logEvents?: SpanLogEvent[];
  [key: string]: unknown;
}

export interface SessionsOptions extends ReadModelOptions {
  cursor?: number;
  limit?: number;
  filter?: string;
}

export interface SessionPage {
  sessions?: unknown[];
  nextCursor?: number | null;
  next_cursor?: number | null;
  [key: string]: unknown;
}

export interface TraceDetail {
  traceId?: TenantId;
  trace_id?: TenantId;
  externalTraceId?: string | null;
  external_trace_id?: string | null;
  spans?: TraceSpan[];
  [key: string]: unknown;
}

export interface SpanDetail {
  traceId?: TenantId;
  trace_id?: TenantId;
  spanId?: TenantId;
  span_id?: TenantId;
  externalTraceId?: string | null;
  externalSpanId?: string | null;
  externalParentSpanId?: string | null;
  externalSessionId?: string | null;
  name?: string;
  spanName?: string | null;
  displayName?: string | null;
  actorId?: string;
  agentName?: string | null;
  toolName?: string | null;
  attrs?: Record<string, unknown>;
  logEvents?: SpanLogEvent[];
  [key: string]: unknown;
}

export interface SpanBuilderDefaults {
  traceId?: TenantId;
  trace_id?: TenantId;
  sessionId?: TenantId;
  session_id?: TenantId;
  tenantId?: TenantId;
  tenant_id?: TenantId;
  agentName?: string | null;
  agent_name?: string | null;
  attrs?: Record<string, unknown>;
}

export interface SpanBuilderEventOptions extends SpanBuilderDefaults {
  spanId?: TenantId;
  span_id?: TenantId;
  parentSpanId?: TenantId | null;
  parent_span_id?: TenantId | null;
  extSpanId?: string;
  ext_span_id?: string;
  externalTraceId?: string | null;
  external_trace_id?: string | null;
  externalSpanId?: string | null;
  external_span_id?: string | null;
  externalParentSpanId?: string | null;
  external_parent_span_id?: string | null;
  externalSessionId?: string | null;
  external_session_id?: string | null;
  ts?: WireNumber;
  seq?: number;
  name?: string;
  displayName?: string | null;
  display_name?: string | null;
  message?: string;
  log?: string;
  logs?: string[];
  status?: number;
  durationNs?: WireNumber;
  duration_ns?: WireNumber;
  agentName?: string | null;
  agent_name?: string | null;
  toolName?: string | null;
  tool_name?: string | null;
  model?: string | null;
  inputText?: string | null;
  input_text?: string | null;
  outputText?: string | null;
  output_text?: string | null;
  inputTokens?: WireNumber | null;
  input_tokens?: WireNumber | null;
  outputTokens?: WireNumber | null;
  output_tokens?: WireNumber | null;
  attrs?: Record<string, unknown>;
}

export declare class SpanEventBuilder {
  constructor(defaults?: SpanBuilderDefaults);
  startSpan(options: SpanBuilderEventOptions): SpanEvent;
  log(options: SpanBuilderEventOptions): SpanEvent;
  endSpan(options: SpanBuilderEventOptions): SpanEvent;
  events(): SpanEvent[];
  clear(): void;
  ingest(db: YiTraceDB, options?: IngestOptions): Promise<IngestResult>;
}

export declare function createSpanEventBuilder(defaults?: SpanBuilderDefaults): SpanEventBuilder;

export declare class YiTraceDB {
  static open(pathOrOptions: string | OpenOptions): Promise<YiTraceDB>;

  ingest(events: SpanEvent[], options?: IngestOptions): Promise<IngestResult>;
  ingestOtlp(body: unknown, options?: TenantOptions): Promise<IngestResult>;
  search<T = SearchHit>(query?: SearchQuery, options?: TenantOptions): Promise<T[]>;
  indexEmbedding(input: EmbeddingInput, options?: TenantOptions): Promise<IndexEmbeddingsResult>;
  indexEmbeddings(items: EmbeddingInput[], options?: TenantOptions): Promise<IndexEmbeddingsResult>;
  traceSearch<T = Record<string, unknown>>(query?: TraceSearchQuery, options?: TenantOptions): Promise<TraceSearchPage<T>>;
  traceAggregate<T = Record<string, unknown>>(query?: TraceAggregateQuery, options?: TenantOptions): Promise<TraceAggregateResult<T>>;
  storageStats<T = Record<string, unknown>>(query?: TraceAggregateQuery, options?: TenantOptions): Promise<StorageStatsResult<T>>;
  retentionPlan<T = Record<string, unknown>>(query?: RetentionPlanQuery, options?: TenantOptions): Promise<T>;
  applyRetention<T = Record<string, unknown>>(query?: RetentionPlanQuery, options?: TenantOptions): Promise<T>;
  retentionAudits<T = Record<string, unknown>>(options?: RetentionAuditQuery): Promise<MetadataPage<T>>;
  createRetentionPolicy<T = Record<string, unknown>>(policy: RetentionPolicyInput, options?: TenantOptions): Promise<T>;
  retentionPolicies<T = Record<string, unknown>>(options?: RetentionPolicyQuery): Promise<MetadataPage<T>>;
  runRetentionPolicies<T = Record<string, unknown>>(query?: RetentionRunDueQuery, options?: TenantOptions): Promise<T>;
  traceTrajectories<T = Record<string, unknown>>(query?: TraceSearchQuery, options?: TenantOptions): Promise<TraceTrajectoryPage<T>>;
  trajectoryGroups<T = Record<string, unknown>>(query?: TraceSearchQuery, options?: TenantOptions): Promise<TrajectoryGroupPage<T>>;
  traceDiff<T = TraceDiffResult>(leftOrQuery: TenantId | Record<string, unknown>, rightTraceId?: TenantId, options?: TenantOptions): Promise<T>;
  annotate<T = TraceAnnotation>(annotation: AnnotationInput, options?: TenantOptions): Promise<T>;
  annotations<T = TraceAnnotation>(options?: AnnotationQuery): Promise<MetadataPage<T>>;
  updateAnnotation<T = TraceAnnotation>(annotationId: TenantId, update?: AnnotationPatch, options?: TenantOptions): Promise<T>;
  deleteAnnotation<T = TraceAnnotation>(annotationId: TenantId, deleteInfo?: { reviewer?: string; reason?: string; source?: string; [key: string]: unknown }, options?: TenantOptions): Promise<T>;
  linkDatasetItem<T = DatasetAssociation>(association: DatasetAssociationInput, options?: TenantOptions): Promise<T>;
  datasetAssociations<T = DatasetAssociation>(options?: DatasetAssociationQuery): Promise<MetadataPage<T>>;
  traces<T = TraceSummary>(options?: TenantOptions): Promise<T[]>;
  sessions<T = SessionPage>(options?: SessionsOptions): Promise<T>;
  loops<T = Record<string, unknown>>(options?: ReadModelOptions): Promise<TraceSearchPage<T>>;
  loop<T = Record<string, unknown>>(loopId: TenantId, options?: TenantOptions): Promise<T | null>;
  taskTraces<T = Record<string, unknown>>(fingerprint: string, options?: ReadModelOptions): Promise<TraceTrajectoryPage<T>>;
  trace<T = TraceDetail>(traceId: TenantId, options?: TenantOptions): Promise<T | null>;
  span<T = SpanDetail>(traceId: TenantId, spanId: TenantId, options?: TenantOptions): Promise<T | null>;
  flush(): Promise<void>;
  close(): Promise<void>;
}
