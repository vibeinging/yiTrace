export type TenantId = string | number;
export type WireNumber = number | string;

export interface OpenOptions {
  dataDir: string;
  tenantId?: TenantId;
}

export interface TenantOptions {
  tenantId?: TenantId;
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
  agent_name?: string | null;
  tool_name?: string | null;
  model?: string | null;
  input_text?: string | null;
  output_text?: string | null;
  duration_ns?: WireNumber | null;
  input_tokens?: WireNumber | null;
  output_tokens?: WireNumber | null;
  cached_input_tokens?: WireNumber | null;
  reasoning_tokens?: WireNumber | null;
  total_tokens?: WireNumber | null;
  cost_usd?: number | null;
  cost_usd_nanos?: WireNumber | null;
  cost_currency?: string | null;
  provider?: string | null;
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
  taskFingerprint?: unknown;
  task_fingerprint?: unknown;
  loopId?: unknown;
  loop_id?: unknown;
  harnessVersion?: unknown;
  harness_version?: unknown;
  validationStatus?: unknown;
  validation_status?: unknown;
  stopReason?: unknown;
  stop_reason?: unknown;
  phase?: unknown;
  validator?: unknown;
  attrs?: Record<string, unknown>;
  [key: string]: unknown;
}

export interface SearchQuery {
  text?: string;
  textDomains?: Array<"input_text" | "output_text" | "logs" | "tool_name" | "model" | "agent_name" | string>;
  text_domains?: Array<"input_text" | "output_text" | "logs" | "tool_name" | "model" | "agent_name" | string>;
  inputTextContains?: string;
  outputContains?: string;
  logContains?: string;
  vector?: number[];
  k?: number;
  filter?: SearchFilter;
  [key: string]: unknown;
}

export interface SearchHit {
  trace_id: TenantId;
  span_id?: TenantId;
  external_trace_id?: string | null;
  external_span_id?: string | null;
  fields?: Record<string, unknown>;
  attrs?: Record<string, unknown>;
  score?: number;
  searchIndex?: string;
  textDomains?: string[];
  text?: string;
  [key: string]: unknown;
}

export type VectorNamespace = "span" | "task" | "trajectory";

export interface VectorIndexInput extends MetadataAttrs {
  namespace: VectorNamespace | string;
  key?: string;
  id?: string;
  taskFingerprint?: string;
  task_fingerprint?: string;
  trajectorySignature?: string;
  trajectory_signature?: string;
  traceId?: TenantId;
  trace_id?: TenantId;
  spanId?: TenantId;
  span_id?: TenantId;
  vector?: number[];
  embedding?: number[];
  attrs?: Record<string, unknown>;
}

export interface VectorSearchQuery extends MetadataAttrs {
  namespace?: VectorNamespace | string;
  vector?: number[];
  embedding?: number[];
  queryVector?: number[];
  query_vector?: number[];
  k?: number;
  limit?: number;
  filter?: MetadataAttrs & {
    namespace?: VectorNamespace | string;
    key?: string;
    id?: string;
    traceId?: TenantId;
    trace_id?: TenantId;
    spanId?: TenantId;
    span_id?: TenantId;
  };
}

export interface VectorSearchHit {
  namespace: VectorNamespace | string;
  key: string;
  tenantId: string | null;
  traceId: string | null;
  spanId: string | null;
  distance: number;
  score: number;
  attrs: Record<string, unknown>;
}

export interface VectorSearchPage<T = VectorSearchHit> {
  items: T[];
  total: number;
  vectorIndex: string;
}

export interface MetadataAttrs {
  projectId?: unknown;
  project_id?: unknown;
  externalRunId?: unknown;
  external_run_id?: unknown;
  skill?: unknown;
  mode?: unknown;
  callSite?: unknown;
  call_site?: unknown;
  taskFingerprint?: unknown;
  task_fingerprint?: unknown;
  loopId?: unknown;
  loop_id?: unknown;
  harnessVersion?: unknown;
  harness_version?: unknown;
  validationStatus?: unknown;
  validation_status?: unknown;
  stopReason?: unknown;
  stop_reason?: unknown;
  phase?: unknown;
  validator?: unknown;
  connectionIds?: unknown;
  connection_ids?: unknown;
  dataSourceIds?: unknown;
  data_source_ids?: unknown;
  schemaFingerprint?: unknown;
  schema_fingerprint?: unknown;
  evalProfile?: unknown;
  eval_profile?: unknown;
  toolVersion?: unknown;
  tool_version?: unknown;
  intentSignature?: unknown;
  intent_signature?: unknown;
  reviewStatus?: unknown;
  review_status?: unknown;
  evalStatus?: unknown;
  eval_status?: unknown;
  pathMemoryId?: unknown;
  path_memory_id?: unknown;
  attrs?: Record<string, unknown>;
}

export interface AnnotationInput extends MetadataAttrs {
  traceId?: TenantId;
  trace_id?: TenantId;
  spanId?: TenantId | null;
  span_id?: TenantId | null;
  target?: "trace" | "span" | string;
  targetType?: "trace" | "span" | string;
  target_type?: "trace" | "span" | string;
  label: string;
  score?: number | null;
  reason?: string | null;
  comment?: string | null;
  note?: string | null;
  source?: string | null;
  createdBy?: string | null;
  created_by?: string | null;
  status?: AnnotationStatus | string;
  reviewer?: string | null;
  reviewedBy?: string | null;
  reviewed_by?: string | null;
  evalProfile?: string | null;
  eval_profile?: string | null;
  sampleCount?: number | null;
  sample_count?: number | null;
  successRate?: number | null;
  success_rate?: number | null;
  avgCostUsdNanos?: WireNumber | null;
  avg_cost_usd_nanos?: WireNumber | null;
  p95DurationNs?: WireNumber | null;
  p95_duration_ns?: WireNumber | null;
  evidence?: Record<string, unknown>;
  evidenceSummary?: Record<string, unknown>;
  evidence_summary?: Record<string, unknown>;
}

export type AnnotationStatus = "active" | "resolved" | "rejected" | "deleted";

export interface AnnotationUpdate extends MetadataAttrs {
  label?: string;
  score?: number | null;
  reason?: string | null;
  comment?: string | null;
  note?: string | null;
  source?: string | null;
  updatedBy?: string | null;
  updated_by?: string | null;
  status?: AnnotationStatus | string;
  reviewer?: string | null;
  reviewedBy?: string | null;
  reviewed_by?: string | null;
  replaceAttrs?: boolean;
  replace_attrs?: boolean;
}

export interface AnnotationDeleteOptions extends TenantOptions {
  reason?: string | null;
  comment?: string | null;
  note?: string | null;
  reviewer?: string | null;
  reviewedBy?: string | null;
  reviewed_by?: string | null;
  source?: string | null;
}

export interface AnnotationFilter extends MetadataAttrs {
  traceId?: TenantId;
  trace_id?: TenantId;
  spanId?: TenantId;
  span_id?: TenantId;
  cursor?: number;
  offset?: number;
  limit?: number;
  target?: "trace" | "span" | string;
  targetType?: "trace" | "span" | string;
  target_type?: "trace" | "span" | string;
  label?: string;
  source?: string;
  status?: AnnotationStatus | string;
  includeDeleted?: boolean;
  include_deleted?: boolean;
}

export interface TraceAnnotation {
  annotationId: string;
  tenantId: string | null;
  target: "trace" | "span" | string;
  traceId: string;
  spanId: string | null;
  externalTraceId?: string | null;
  externalSpanId?: string | null;
  label: string;
  score?: number | null;
  reason?: string | null;
  source?: string | null;
  status: AnnotationStatus | string;
  reviewer?: string | null;
  createdAtNs: string;
  updatedAtNs: string;
  attrs: Record<string, unknown>;
  [key: string]: unknown;
}

export interface AnnotationPage<T = TraceAnnotation> {
  items: T[];
  total?: number;
  count: number;
  pageCount?: number;
  nextCursor?: number | null;
  [key: string]: unknown;
}

export interface DatasetAssociationInput extends MetadataAttrs {
  datasetId?: string;
  dataset_id?: string;
  dataset?: string;
  itemId?: string;
  item_id?: string;
  datasetItemId?: string;
  dataset_item_id?: string;
  traceId?: TenantId;
  trace_id?: TenantId;
  spanId?: TenantId | null;
  span_id?: TenantId | null;
  snapshotId?: string | null;
  snapshot_id?: string | null;
  snapshotHash?: string | null;
  snapshot_hash?: string | null;
  evalRunId?: string | null;
  eval_run_id?: string | null;
  split?: string | null;
  label?: string | null;
  score?: number | null;
}

export interface DatasetAssociationFilter extends MetadataAttrs {
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
  cursor?: number;
  offset?: number;
  limit?: number;
  evalRunId?: string;
  eval_run_id?: string;
  split?: string;
  label?: string;
}

export interface DatasetAssociation {
  associationId: string;
  tenantId: string | null;
  datasetId: string;
  itemId: string;
  traceId: string;
  spanId: string | null;
  externalTraceId?: string | null;
  externalSpanId?: string | null;
  snapshotId?: string | null;
  snapshotHash?: string | null;
  evalRunId?: string | null;
  split?: string | null;
  label?: string | null;
  score?: number | null;
  createdAtNs: string;
  attrs: Record<string, unknown>;
  [key: string]: unknown;
}

export interface DatasetAssociationPage<T = DatasetAssociation> {
  items: T[];
  total?: number;
  count: number;
  pageCount?: number;
  nextCursor?: number | null;
  [key: string]: unknown;
}

export type GoldenPathStatus = "candidate" | "confirmed" | "rejected" | "deprecated" | string;

export interface GoldenPathCandidateInput extends MetadataAttrs {
  sourceTraceId?: TenantId;
  source_trace_id?: TenantId;
  traceId?: TenantId;
  trace_id?: TenantId;
  externalSourceTraceId?: string | null;
  external_source_trace_id?: string | null;
  taskFingerprint?: string;
  task_fingerprint?: string;
  task?: string;
  taskId?: string;
  trajectorySignature?: string;
  trajectory_signature?: string;
  signature?: string;
  pathSignature?: string;
  snapshotId?: string | null;
  snapshot_id?: string | null;
  snapshotHash?: string | null;
  snapshot_hash?: string | null;
  status?: GoldenPathStatus;
  score?: number | null;
  qualityScore?: number | null;
  label?: string | null;
  name?: string | null;
  reason?: string | null;
  comment?: string | null;
  note?: string | null;
  source?: string | null;
  createdBy?: string | null;
  created_by?: string | null;
  challengerOf?: TenantId | null;
  challenger_of?: TenantId | null;
  baselineGoldenPathId?: TenantId | null;
  baseline_golden_path_id?: TenantId | null;
  evalProfile?: string | null;
  eval_profile?: string | null;
  minSampleCount?: number | null;
  min_sample_count?: number | null;
  minSamples?: number | null;
  min_samples?: number | null;
  marginScore?: number | null;
  margin_score?: number | null;
  margin?: number | null;
  comparisonWindowNs?: WireNumber | null;
  comparison_window_ns?: WireNumber | null;
  windowNs?: WireNumber | null;
  window_ns?: WireNumber | null;
  promotedFrom?: TenantId | null;
  promoted_from?: TenantId | null;
  deprecationReason?: string | null;
  deprecation_reason?: string | null;
  staleReasons?: string[] | string;
  stale_reasons?: string[] | string;
}

export interface GoldenPathFilter extends MetadataAttrs {
  goldenPathId?: TenantId;
  golden_path_id?: TenantId;
  id?: TenantId;
  taskFingerprint?: string;
  task_fingerprint?: string;
  task?: string;
  trajectorySignature?: string;
  trajectory_signature?: string;
  signature?: string;
  pathSignature?: string;
  sourceTraceId?: TenantId;
  source_trace_id?: TenantId;
  traceId?: TenantId;
  trace_id?: TenantId;
  challengerOf?: TenantId;
  challenger_of?: TenantId;
  baselineGoldenPathId?: TenantId;
  baseline_golden_path_id?: TenantId;
  evalProfile?: string;
  eval_profile?: string;
  status?: GoldenPathStatus;
}

export interface GoldenPathGovernance {
  challengerOf?: string | null;
  evalProfile?: string | null;
  minSampleCount?: string | null;
  marginScore?: number | null;
  comparisonWindowNs?: string | null;
  promotedFrom?: string | null;
  deprecationReason?: string | null;
  stale?: boolean;
  staleReasons: string[];
  [key: string]: unknown;
}

export interface GoldenPathCandidate {
  goldenPathId: string;
  tenantId: string | null;
  taskFingerprint: string;
  trajectorySignature: string;
  sourceTraceId: string;
  externalSourceTraceId?: string | null;
  snapshotId?: string | null;
  snapshotHash?: string | null;
  status: GoldenPathStatus;
  score?: number | null;
  label?: string | null;
  reason?: string | null;
  source?: string | null;
  challengerOf?: string | null;
  evalProfile?: string | null;
  minSampleCount?: string | null;
  marginScore?: number | null;
  comparisonWindowNs?: string | null;
  promotedFrom?: string | null;
  deprecationReason?: string | null;
  staleReasons?: string[];
  governance?: GoldenPathGovernance;
  createdAtNs: string;
  updatedAtNs: string;
  attrs: Record<string, unknown>;
  sourceTrajectory: TraceDiffTrajectorySide;
  evidenceSummary: Record<string, unknown>;
  [key: string]: unknown;
}

export interface GoldenPathStatusUpdate {
  status: GoldenPathStatus;
  score?: number | null;
  qualityScore?: number | null;
  reason?: string | null;
  comment?: string | null;
  note?: string | null;
  source?: string | null;
  updatedBy?: string | null;
  updated_by?: string | null;
}

export interface GoldenPathPage<T = GoldenPathCandidate> {
  items: T[];
  count: number;
  [key: string]: unknown;
}

export interface PathAdherenceQuery {
  goldenPathId?: TenantId;
  golden_path_id?: TenantId;
  id?: TenantId;
  traceId?: TenantId;
  trace_id?: TenantId;
  candidateTraceId?: TenantId;
  candidate_trace_id?: TenantId;
  candidate?: TenantId;
}

export type PathAdherenceStatus =
  | "followed"
  | "extended"
  | "partial"
  | "deviated"
  | "unknown"
  | string;

export interface PathAdherenceScores {
  commonStepCount: number;
  goldenStepCount: number;
  traceStepCount: number;
  goldenCoverage: number | null;
  traceCoverage: number | null;
  [key: string]: unknown;
}

export interface PathAdherenceResult {
  goldenPath: GoldenPathCandidate;
  trace: TraceDiffSide;
  adherence: PathAdherenceStatus;
  sameSignature: boolean;
  sourceAvailable: boolean;
  sourceRetained: boolean;
  storedSignatureMatchesSource: boolean | null;
  goldenTrajectory: TraceDiffTrajectorySide;
  sourceTrajectory: TraceDiffTrajectorySide | null;
  traceTrajectory: TraceDiffTrajectorySide;
  scores: PathAdherenceScores;
  commonSteps: string[];
  missingSteps: string[];
  extraSteps: string[];
  [key: string]: unknown;
}

export interface GoldenPathEvidenceQuery {
  goldenPathId?: TenantId;
  golden_path_id?: TenantId;
  id?: TenantId;
  candidateTraceId?: TenantId;
  candidate_trace_id?: TenantId;
  traceId?: TenantId;
  trace_id?: TenantId;
  candidate?: TenantId;
}

export interface TraceEvidenceBundle {
  available: boolean;
  trace: TraceDiffSide;
  trajectory: TraceDiffTrajectorySide | null;
  annotations: TraceAnnotation[];
  annotationCount: number;
  datasetAssociations: DatasetAssociation[];
  datasetAssociationCount: number;
  [key: string]: unknown;
}

export interface GoldenPathEvidenceCandidate {
  evidence: TraceEvidenceBundle;
  pathAdherence: PathAdherenceResult;
  traceDiff: TraceDiffResult | null;
  [key: string]: unknown;
}

export interface GoldenPathEvidenceResult {
  goldenPath: GoldenPathCandidate;
  source: TraceEvidenceBundle;
  candidate: GoldenPathEvidenceCandidate | null;
  [key: string]: unknown;
}

export interface GoldenPathExportQuery extends GoldenPathFilter {
  filter?: GoldenPathFilter;
  limit?: number;
  k?: number;
}

export interface GoldenPathExportRecord {
  schemaVersion: "yitrace.golden_path_export.v1" | string;
  recordType: "golden_path" | string;
  goldenPath: GoldenPathCandidate;
  source: TraceEvidenceBundle;
  exportedAtNs: string;
  [key: string]: unknown;
}

export interface GoldenPathExportResult {
  schemaVersion: "yitrace.golden_path_export.v1" | string;
  format: "jsonl" | string;
  count: number;
  items: GoldenPathExportRecord[];
  jsonl: string;
  [key: string]: unknown;
}

export interface GoldenPathHealthQuery extends Omit<TraceSearchQuery, "cursor" | "offset" | "sort" | "sortBy" | "sort_by" | "order" | "direction"> {
  goldenPathId?: TenantId;
  golden_path_id?: TenantId;
  id?: TenantId;
  includeSource?: boolean;
  include_source?: boolean;
  exampleLimit?: number;
  example_limit?: number;
  examples?: number;
}

export interface GoldenPathHealthCounts {
  total: number;
  followed: number;
  extended: number;
  partial: number;
  deviated: number;
  unknown: number;
  [key: string]: unknown;
}

export interface GoldenPathHealthRates {
  followed: number | null;
  usable: number | null;
  deviated: number | null;
  unknown: number | null;
  [key: string]: unknown;
}

export interface GoldenPathHealthCoverage {
  commonStepCount: number;
  goldenStepCount: number;
  traceStepCount: number;
  goldenCoverage: number | null;
  traceCoverage: number | null;
  [key: string]: unknown;
}

export interface GoldenPathHealthWindow {
  limit: number;
  includeSource: boolean;
  spanTotal: number;
  matchingTraceTotal: number;
  analyzedTraceTotal: number;
  [key: string]: unknown;
}

export interface GoldenPathHealthExample {
  trace: TraceDiffSide;
  adherence: PathAdherenceStatus;
  sameSignature: boolean;
  scores: PathAdherenceScores;
  traceTrajectory: TraceDiffTrajectorySide;
  [key: string]: unknown;
}

export interface GoldenPathHealthResult {
  goldenPath: GoldenPathCandidate;
  sourceAvailable: boolean;
  sourceRetained: boolean;
  storedSignatureMatchesSource: boolean | null;
  goldenTrajectory: TraceDiffTrajectorySide;
  sourceTrajectory: TraceDiffTrajectorySide | null;
  window: GoldenPathHealthWindow;
  counts: GoldenPathHealthCounts;
  rates: GoldenPathHealthRates;
  coverage: GoldenPathHealthCoverage;
  governance: GoldenPathGovernance;
  examples: GoldenPathHealthExample[];
  [key: string]: unknown;
}

export interface TraceSummary {
  traceId?: TenantId;
  trace_id?: TenantId;
  externalTraceId?: string | null;
  external_trace_id?: string | null;
  usage?: UsageSummary;
  costUsd?: number;
  costDetail?: CostDetail;
  fields?: Record<string, unknown>;
  [key: string]: unknown;
}

export interface UsageSummary {
  inputTokens: number;
  outputTokens: number;
  cachedInputTokens: number;
  reasoningTokens: number;
  totalTokens: number;
  [key: string]: unknown;
}

export interface CostDetail {
  costUsd: number;
  costUsdNanos: number | string;
  currency: string;
  source: "explicit" | "estimated" | "estimated_model_price" | "estimated_default" | "mixed" | string;
  [key: string]: unknown;
}

export interface SpanLogEvent {
  eventId: string;
  eventOrdinal?: number;
  sortKey?: string;
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
  spanOrdinal?: number;
  siblingOrdinal?: number;
  sortKey?: string;
  inputText?: TextField | null;
  outputText?: TextField | null;
  usage?: UsageSummary;
  costUsd?: number;
  costDetail?: CostDetail;
  provider?: string | null;
  fields?: Record<string, unknown>;
  attrs?: Record<string, unknown>;
  logEvents?: SpanLogEvent[];
  [key: string]: unknown;
}

export interface TextField {
  preview: string;
  full: string | null;
  contentHash: string;
  byteLength: number;
  truncated: boolean;
  blobRef?: string | null;
}

export interface MetadataReverseFilterOptions {
  annotation?: AnnotationFilter & {
    scoreMin?: number;
    score_min?: number;
    scoreMax?: number;
    score_max?: number;
    minScore?: number;
    maxScore?: number;
  };
  annotations?: AnnotationFilter & {
    scoreMin?: number;
    score_min?: number;
    scoreMax?: number;
    score_max?: number;
    minScore?: number;
    maxScore?: number;
  };
  annotationLabel?: string;
  annotation_label?: string;
  annotationSource?: string;
  annotation_source?: string;
  annotationTarget?: "trace" | "span" | string;
  annotation_target?: "trace" | "span" | string;
  annotationScoreMin?: number;
  annotation_score_min?: number;
  annotationScoreMax?: number;
  annotation_score_max?: number;
  dataset?: DatasetAssociationFilter & {
    scoreMin?: number;
    score_min?: number;
    scoreMax?: number;
    score_max?: number;
    minScore?: number;
    maxScore?: number;
  };
  datasetAssociation?: DatasetAssociationFilter;
  dataset_association?: DatasetAssociationFilter;
  datasetLink?: DatasetAssociationFilter;
  dataset_link?: DatasetAssociationFilter;
  datasetId?: string;
  dataset_id?: string;
  itemId?: string;
  item_id?: string;
  datasetItemId?: string;
  dataset_item_id?: string;
  evalRunId?: string;
  eval_run_id?: string;
  datasetSplit?: string;
  dataset_split?: string;
  datasetLabel?: string;
  dataset_label?: string;
  datasetScoreMin?: number;
  dataset_score_min?: number;
  datasetScoreMax?: number;
  dataset_score_max?: number;
}

export interface SessionsOptions extends TenantOptions, MetadataReverseFilterOptions {
  cursor?: number;
  limit?: number;
  filter?: string;
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
  harnessVersion?: unknown;
  harness_version?: unknown;
  validationStatus?: unknown;
  validation_status?: unknown;
  stopReason?: unknown;
  stop_reason?: unknown;
  phase?: unknown;
  validator?: unknown;
  attrs?: Record<string, unknown>;
  externalRunId?: unknown;
  external_run_id?: unknown;
  connectionIds?: unknown;
  connection_ids?: unknown;
  dataSourceIds?: unknown;
  data_source_ids?: unknown;
  schemaFingerprint?: unknown;
  schema_fingerprint?: unknown;
  intentSignature?: unknown;
  intent_signature?: unknown;
  reviewStatus?: unknown;
  review_status?: unknown;
  evalStatus?: unknown;
  eval_status?: unknown;
  pathMemoryId?: unknown;
  path_memory_id?: unknown;
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
  usage?: UsageSummary;
  costUsd?: number;
  costDetail?: CostDetail;
  provider?: string | null;
  fields?: Record<string, unknown>;
  attrs?: Record<string, unknown>;
  logEvents?: SpanLogEvent[];
  [key: string]: unknown;
}

export interface TraceListOptions extends TenantOptions, MetadataReverseFilterOptions {
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
  harnessVersion?: unknown;
  harness_version?: unknown;
  validationStatus?: unknown;
  validation_status?: unknown;
  stopReason?: unknown;
  stop_reason?: unknown;
  phase?: unknown;
  validator?: unknown;
  attrs?: Record<string, unknown>;
  externalRunId?: unknown;
  external_run_id?: unknown;
  connectionIds?: unknown;
  connection_ids?: unknown;
  dataSourceIds?: unknown;
  data_source_ids?: unknown;
  schemaFingerprint?: unknown;
  schema_fingerprint?: unknown;
  intentSignature?: unknown;
  intent_signature?: unknown;
  reviewStatus?: unknown;
  review_status?: unknown;
  evalStatus?: unknown;
  eval_status?: unknown;
  pathMemoryId?: unknown;
  path_memory_id?: unknown;
}

export interface TraceSearchQuery {
  text?: string;
  q?: string;
  cursor?: number;
  offset?: number;
  limit?: number;
  sort?: "created" | "duration" | "cost" | "tokens" | "status" | "span" | string;
  sortBy?: string;
  sort_by?: string;
  order?: "asc" | "desc" | string;
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
    kind?: string;
    spanKind?: string;
    span_kind?: string;
    toolName?: string;
    tool_name?: string;
    model?: string;
    inputContains?: string;
    outputContains?: string;
    logContains?: string;
    minCostUsdNanos?: WireNumber;
    min_cost_usd_nanos?: WireNumber;
    costUsdNanosMin?: WireNumber;
    maxCostUsdNanos?: WireNumber;
    max_cost_usd_nanos?: WireNumber;
    costUsdNanosMax?: WireNumber;
    minCostUsd?: number;
    min_cost_usd?: number;
    costUsdMin?: number;
    maxCostUsd?: number;
    max_cost_usd?: number;
    costUsdMax?: number;
    minTotalTokens?: number;
    min_total_tokens?: number;
    totalTokensMin?: number;
    minTokens?: number;
    maxTotalTokens?: number;
    max_total_tokens?: number;
    totalTokensMax?: number;
    maxTokens?: number;
    annotation?: AnnotationFilter & {
      scoreMin?: number;
      score_min?: number;
      scoreMax?: number;
      score_max?: number;
      minScore?: number;
      maxScore?: number;
    };
    annotations?: AnnotationFilter & {
      scoreMin?: number;
      score_min?: number;
      scoreMax?: number;
      score_max?: number;
      minScore?: number;
      maxScore?: number;
    };
    annotationLabel?: string;
    annotation_label?: string;
    annotationSource?: string;
    annotation_source?: string;
    annotationTarget?: "trace" | "span" | string;
    annotation_target?: "trace" | "span" | string;
    annotationScoreMin?: number;
    annotation_score_min?: number;
    annotationScoreMax?: number;
    annotation_score_max?: number;
    dataset?: DatasetAssociationFilter & {
      scoreMin?: number;
      score_min?: number;
      scoreMax?: number;
      score_max?: number;
      minScore?: number;
      maxScore?: number;
    };
    datasetAssociation?: DatasetAssociationFilter;
    dataset_association?: DatasetAssociationFilter;
    datasetId?: string;
    dataset_id?: string;
    itemId?: string;
    item_id?: string;
    datasetItemId?: string;
    dataset_item_id?: string;
    evalRunId?: string;
    eval_run_id?: string;
    split?: string;
    datasetSplit?: string;
    dataset_split?: string;
    datasetLabel?: string;
    dataset_label?: string;
    datasetScoreMin?: number;
    dataset_score_min?: number;
    datasetScoreMax?: number;
    dataset_score_max?: number;
  };
  [key: string]: unknown;
}

export interface TraceSearchPage<T = TraceSpan> {
  items: T[];
  nextCursor: number | null;
  total: number;
  index?: string;
  [key: string]: unknown;
}

export type TraceAggregateGroupBy =
  | "project_id"
  | "projectId"
  | "skill"
  | "mode"
  | "call_site"
  | "callSite"
  | "task_fingerprint"
  | "taskFingerprint"
  | "loop_id"
  | "loopId"
  | "harness_version"
  | "harnessVersion"
  | "validation_status"
  | "validationStatus"
  | "stop_reason"
  | "stopReason"
  | "phase"
  | "validator"
  | "agentName"
  | "agent_name"
  | "toolName"
  | "tool_name"
  | "model"
  | "provider"
  | "kind"
  | "spanKind"
  | "status"
  | `attrs.${string}`
  | string;

export interface TraceAggregateQuery {
  groupBy?: TraceAggregateGroupBy | TraceAggregateGroupBy[];
  group_by?: TraceAggregateGroupBy | TraceAggregateGroupBy[];
  by?: TraceAggregateGroupBy | TraceAggregateGroupBy[];
  text?: string;
  q?: string;
  limit?: number;
  k?: number;
  sort?: "count" | "traceCount" | "errorCount" | "errorRate" | "duration" | "avgDuration" | "maxDuration" | "cost" | "tokens" | string;
  sortBy?: string;
  sort_by?: string;
  order?: "asc" | "desc" | string;
  filter?: TraceSearchQuery["filter"];
  [key: string]: unknown;
}

export interface TraceAggregateDuration {
  sum: number | string;
  avg: number | string | null;
  max: number | string | null;
  p50: number | string | null;
  p95: number | string | null;
  count: number;
  [key: string]: unknown;
}

export interface TraceAggregateExample {
  traceId: TenantId;
  spanId: TenantId;
  externalTraceId?: string | null;
  externalSpanId?: string | null;
  name?: string;
  [key: string]: unknown;
}

export interface TraceAggregateBucket {
  key: Record<string, unknown>;
  spanCount: number;
  traceCount: number;
  errorCount: number;
  errorRate: number;
  durationNs: TraceAggregateDuration;
  usage: UsageSummary;
  costUsd: number;
  costDetail: CostDetail;
  examples: TraceAggregateExample[];
  [key: string]: unknown;
}

export interface TraceAggregatePage<T = TraceAggregateBucket> {
  items: T[];
  total: number;
  spanTotal: number;
  index?: string;
  aggregationIndex?: string;
  [key: string]: unknown;
}

export type StorageGroupBy =
  | TraceAggregateGroupBy
  | "time"
  | "timeBucket"
  | "day"
  | "session_id"
  | "sessionId"
  | "trace_id"
  | "traceId"
  | string;

export interface StorageStatsQuery extends TraceSearchQuery {
  groupBy?: StorageGroupBy | StorageGroupBy[];
  group_by?: StorageGroupBy | StorageGroupBy[];
  groups?: StorageGroupBy | StorageGroupBy[];
  timeBucketNs?: WireNumber;
  time_bucket_ns?: WireNumber;
  bucketNs?: WireNumber;
  bucket_ns?: WireNumber;
}

export interface StorageByteBreakdown {
  inputText: number;
  outputText: number;
  logs: number;
  payload: number;
  attrs: number;
  externalIds: number;
  fields: number;
  estimated: number;
  estimatedBytes: number;
  [key: string]: unknown;
}

export interface StorageMetadataCounts {
  annotations: number;
  datasetAssociations: number;
  goldenPaths: number;
  snapshotRefs: number;
  evalLinks: number;
  pathMemoryRefs: number;
  [key: string]: unknown;
}

export interface StorageStatsBucket {
  key?: Record<string, unknown>;
  traceCount: number;
  spanCount: number;
  sessionCount: number;
  eventCount: number;
  errorSpanCount: number;
  firstTs: number | null;
  lastTs: number | null;
  bytes: StorageByteBreakdown;
  metadata: StorageMetadataCounts;
  [key: string]: unknown;
}

export interface StorageStatsReport {
  groupBy: string[];
  total: StorageStatsBucket;
  groups: StorageStatsBucket[];
  [key: string]: unknown;
}

export interface RetentionProtectOptions {
  goldenPaths?: boolean;
  golden_paths?: boolean;
  annotations?: boolean;
  datasetAssociations?: boolean;
  dataset_associations?: boolean;
  snapshots?: boolean;
  evalLinks?: boolean;
  eval_links?: boolean;
  pathMemory?: boolean;
  path_memory?: boolean;
  [key: string]: unknown;
}

export interface RetentionPlanQuery extends StorageStatsQuery {
  deleteBeforeTs?: WireNumber;
  delete_before_ts?: WireNumber;
  olderThanTs?: WireNumber;
  older_than_ts?: WireNumber;
  timeTo?: WireNumber;
  time_to?: WireNumber;
  apply?: boolean;
  execute?: boolean;
  delete?: boolean;
  protect?: RetentionProtectOptions;
  protectGoldenPaths?: boolean;
  protect_golden_paths?: boolean;
  protectAnnotations?: boolean;
  protect_annotations?: boolean;
  protectDatasetAssociations?: boolean;
  protect_dataset_associations?: boolean;
  compact?: boolean;
  compactAfterApply?: boolean;
  compact_after_apply?: boolean;
  compactMinDeletedRows?: number;
  compact_min_deleted_rows?: number;
  minDeletedRows?: number;
  min_deleted_rows?: number;
  compactMinDeletedPercent?: number;
  compact_min_deleted_percent?: number;
  minDeletedPercent?: number;
  min_deleted_percent?: number;
  compactMaxSegments?: number;
  compact_max_segments?: number;
  maxSegments?: number;
  max_segments?: number;
  reclaim?: boolean;
  reclaimAfterCompact?: boolean;
  reclaim_after_compact?: boolean;
  source?: string;
  requestedBy?: string;
  requested_by?: string;
  actor?: string;
  reason?: string;
  comment?: string;
  note?: string;
  exampleLimit?: number;
  examples?: number;
}

export interface RetentionApplyResult {
  requestedTraceCount: number;
  deletedTraceCount: number;
  deletedSegmentRowCount: number;
  skippedLiveTraceCount: number;
  deletedTraceIds: TenantId[];
  skippedLiveTraceIds: TenantId[];
  [key: string]: unknown;
}

export interface RetentionCompactOptions {
  requested: boolean;
  minDeletedRows: number;
  minDeletedPercent: number;
  maxSegments: number;
  reclaim: boolean;
  [key: string]: unknown;
}

export interface RetentionCompactResult {
  beforeLiveSegmentCount: number;
  afterLiveSegmentCount: number;
  beforeDeadSegmentCount: number;
  afterDeadSegmentCount: number;
  selectedSegmentCount: number;
  compactedSegmentCount: number;
  reclaimedSegmentCount: number;
  droppedDeletedRowCount: number;
  rewrittenLiveRowCount: number;
  selectedSegmentIds: TenantId[];
  [key: string]: unknown;
}

export interface RetentionAuditRecord {
  auditId: TenantId;
  tenantId: TenantId | null;
  createdAtNs: TenantId;
  source: string | null;
  reason: string | null;
  deleteBeforeTs: number | null;
  query: Record<string, unknown>;
  protect: {
    goldenPaths: boolean;
    annotations: boolean;
    datasetAssociations: boolean;
    snapshots: boolean;
    evalLinks: boolean;
    pathMemory: boolean;
    [key: string]: unknown;
  };
  compact: {
    requested: boolean;
    reclaim: boolean;
    compactedSegmentCount: number;
    reclaimedSegmentCount: number;
    droppedDeletedRowCount: number;
    rewrittenLiveRowCount: number;
    [key: string]: unknown;
  };
  counts: {
    candidateTraceCount: number;
    protectedTraceCount: number;
    deletableTraceCount: number;
    requestedTraceCount: number;
    deletedTraceCount: number;
    deletedSegmentRowCount: number;
    skippedLiveTraceCount: number;
    [key: string]: unknown;
  };
  traceIds: {
    deletable: TenantId[];
    deleted: TenantId[];
    skippedLive: TenantId[];
    sampleTruncated: boolean;
    [key: string]: unknown;
  };
  [key: string]: unknown;
}

export interface RetentionAuditQuery {
  auditId?: TenantId;
  audit_id?: TenantId;
  id?: TenantId;
  source?: string;
  requestedBy?: string;
  requested_by?: string;
  actor?: string;
  createdAfterNs?: WireNumber;
  created_after_ns?: WireNumber;
  minCreatedAtNs?: WireNumber;
  createdBeforeNs?: WireNumber;
  created_before_ns?: WireNumber;
  maxCreatedAtNs?: WireNumber;
  filter?: Omit<RetentionAuditQuery, "filter" | "limit" | "cursor" | "offset">;
  limit?: number;
  cursor?: number;
  offset?: number;
  [key: string]: unknown;
}

export interface RetentionAuditPage {
  items: RetentionAuditRecord[];
  nextCursor: string | null;
  total: number;
  [key: string]: unknown;
}

export interface RetentionPolicyInput {
  name?: string;
  policyName?: string;
  policy_name?: string;
  enabled?: boolean;
  intervalNs?: WireNumber;
  interval_ns?: WireNumber;
  everyNs?: WireNumber;
  every_ns?: WireNumber;
  nextRunAtNs?: WireNumber;
  next_run_at_ns?: WireNumber;
  source?: string;
  requestedBy?: string;
  requested_by?: string;
  actor?: string;
  createdBy?: string;
  reason?: string;
  comment?: string;
  note?: string;
  query?: RetentionPlanQuery;
  retention?: RetentionPlanQuery;
  retentionQuery?: RetentionPlanQuery;
  retention_query?: RetentionPlanQuery;
  [key: string]: unknown;
}

export interface RetentionPolicy {
  policyId: TenantId;
  tenantId: TenantId | null;
  name: string;
  enabled: boolean;
  createdAtNs: TenantId;
  updatedAtNs: TenantId;
  lastRunAtNs: TenantId | null;
  nextRunAtNs: TenantId | null;
  intervalNs: TenantId;
  source: string | null;
  reason: string | null;
  query: RetentionPlanQuery;
  [key: string]: unknown;
}

export interface RetentionPolicyFilter {
  policyId?: TenantId;
  policy_id?: TenantId;
  id?: TenantId;
  name?: string;
  policyName?: string;
  policy_name?: string;
  enabled?: boolean;
  cursor?: number;
  offset?: number;
  limit?: number;
  [key: string]: unknown;
}

export interface RetentionPolicyPage {
  items: RetentionPolicy[];
  nextCursor: string | null;
  total: number;
  [key: string]: unknown;
}

export interface RetentionPlanResult {
  dryRun: boolean;
  applied: boolean;
  deleteBeforeTs: number | null;
  protect: {
    goldenPaths: boolean;
    annotations: boolean;
    datasetAssociations: boolean;
    snapshots: boolean;
    evalLinks: boolean;
    pathMemory: boolean;
    [key: string]: unknown;
  };
  candidates: StorageStatsBucket;
  protected: StorageStatsBucket;
  deletable: StorageStatsBucket;
  protectedReasons: Record<string, string[]>;
  deletableTraceIds: TenantId[];
  applyResult: RetentionApplyResult | null;
  compact: RetentionCompactOptions;
  compactResult: RetentionCompactResult | null;
  audit: RetentionAuditRecord | null;
  [key: string]: unknown;
}

export interface RunRetentionPoliciesQuery {
  nowNs?: WireNumber;
  now_ns?: WireNumber;
  limit?: number;
  maxPolicies?: number;
  max_policies?: number;
  includeDisabled?: boolean;
  include_disabled?: boolean;
  policyId?: TenantId;
  policy_id?: TenantId;
  id?: TenantId;
  name?: string;
  policyName?: string;
  policy_name?: string;
  [key: string]: unknown;
}

export interface RetentionPolicyRunItem {
  policy: RetentionPolicy;
  ok: boolean;
  statusCode: number;
  result?: RetentionPlanResult;
  error?: { error?: string; [key: string]: unknown };
  [key: string]: unknown;
}

export interface RunRetentionPoliciesResult {
  nowNs: TenantId;
  ran: number;
  failed: number;
  skipped: number;
  items: RetentionPolicyRunItem[];
  [key: string]: unknown;
}

export interface TrajectoryGroupsQuery {
  text?: string;
  q?: string;
  limit?: number;
  k?: number;
  exampleLimit?: number;
  example_limit?: number;
  examples?: number;
  sort?: "best" | "traceCount" | "successRate" | "evalScore" | "annotationScore" | "datasetScore" | "duration" | "cost" | "tokens" | string;
  sortBy?: string;
  sort_by?: string;
  order?: "asc" | "desc" | string;
  filter?: TraceSearchQuery["filter"];
  [key: string]: unknown;
}

export interface TrajectoryScoreStats {
  count: number;
  avg: number | null;
  min: number | null;
  max: number | null;
  [key: string]: unknown;
}

export interface TrajectoryGroupExample {
  traceId: TenantId;
  externalTraceId?: string | null;
  status: "ok" | "error" | string;
  durationNs: {
    sum: WireNumber;
    max: WireNumber;
    [key: string]: unknown;
  };
  usage: UsageSummary;
  costUsd: number;
  costDetail: CostDetail;
  qualityScore: number;
  fields: Record<string, unknown>;
  [key: string]: unknown;
}

export interface TrajectoryGroupBucket {
  signature: string;
  stepCount: number;
  steps: string[];
  traceCount: number;
  spanCount: number;
  successCount: number;
  errorTraceCount: number;
  errorSpanCount: number;
  successRate: number;
  errorRate: number;
  qualityScore: number;
  durationNs: TraceAggregateDuration;
  usage: UsageSummary;
  costUsd: number;
  costDetail: CostDetail;
  scores: {
    eval: TrajectoryScoreStats;
    annotation: TrajectoryScoreStats;
    dataset: TrajectoryScoreStats;
    [key: string]: unknown;
  };
  examples: TrajectoryGroupExample[];
  [key: string]: unknown;
}

export interface TrajectoryGroupPage<T = TrajectoryGroupBucket> {
  items: T[];
  total: number;
  traceTotal: number;
  spanTotal: number;
  index?: string;
  trajectoryIndex?: string;
  [key: string]: unknown;
}

export interface TraceTrajectoriesQuery {
  text?: string;
  q?: string;
  cursor?: number;
  offset?: number;
  limit?: number;
  k?: number;
  filter?: TraceSearchQuery["filter"];
  [key: string]: unknown;
}

export interface TraceTrajectoryItem {
  trace: TraceDiffSide;
  trajectory: TraceDiffTrajectorySide;
  index: "materialized" | string;
  [key: string]: unknown;
}

export interface TraceTrajectoryPage<T = TraceTrajectoryItem> {
  items: T[];
  nextCursor: string | number | null;
  total: number;
  spanTotal: number;
  index: "materialized" | string;
  [key: string]: unknown;
}

export interface TraceDiffQuery {
  leftTraceId?: TenantId;
  left_trace_id?: TenantId;
  left?: TenantId;
  baseTraceId?: TenantId;
  base_trace_id?: TenantId;
  a?: TenantId;
  rightTraceId?: TenantId;
  right_trace_id?: TenantId;
  right?: TenantId;
  candidateTraceId?: TenantId;
  candidate_trace_id?: TenantId;
  b?: TenantId;
  [key: string]: unknown;
}

export interface TraceDiffSide {
  traceId: TenantId;
  externalTraceId?: string | null;
  spanCount: number;
  errorCount: number;
  status: "ok" | "error" | "missing" | string;
  durationNs: {
    sum: WireNumber;
    max: WireNumber;
    [key: string]: unknown;
  };
  usage: UsageSummary;
  costUsd: number;
  costDetail: CostDetail;
  fields: Record<string, unknown>;
  [key: string]: unknown;
}

export interface TraceDiffDelta {
  spanCount: number;
  errorCount: number;
  durationNs: WireNumber;
  inputTokens: number;
  outputTokens: number;
  totalTokens: number;
  costUsdNanos: WireNumber;
  costUsd: number;
  [key: string]: unknown;
}

export interface TraceDiffRouteStep {
  spanId: TenantId;
  externalSpanId?: string | null;
  kind: string;
  name: string;
  spanOrdinal?: number;
  sortKey?: string;
  agentName?: string | null;
  toolName?: string | null;
  model?: string | null;
  status?: number | null;
  statusText: "ok" | "error" | string;
  fields: Record<string, unknown>;
  [key: string]: unknown;
}

export interface TraceDiffSpan {
  traceId: TenantId;
  spanId: TenantId;
  externalTraceId?: string | null;
  externalSpanId?: string | null;
  kind: string;
  name: string;
  status?: number | null;
  statusText: "ok" | "error" | string;
  durationNs?: WireNumber | null;
  usage: UsageSummary;
  costUsd: number;
  costDetail: CostDetail;
  evalScore?: number | null;
  evalLabel?: string | null;
  agentName?: string | null;
  toolName?: string | null;
  model?: string | null;
  provider?: string | null;
  inputPreview?: string | null;
  outputPreview?: string | null;
  fields: Record<string, unknown>;
  [key: string]: unknown;
}

export interface TraceDiffStep {
  index: number;
  status: "same" | "changed" | "left_only" | "right_only" | string;
  changes: string[];
  left: TraceDiffSpan | null;
  right: TraceDiffSpan | null;
  delta: {
    durationNs: WireNumber;
    totalTokens: number;
    costUsdNanos: WireNumber;
    costUsd: number;
    [key: string]: unknown;
  };
  [key: string]: unknown;
}

export interface TraceDiffTrajectorySide {
  signature: string;
  stepCount: number;
  steps: string[];
  [key: string]: unknown;
}

export interface TraceDiffTrajectory {
  left: TraceDiffTrajectorySide;
  right: TraceDiffTrajectorySide;
  same: boolean;
  [key: string]: unknown;
}

export interface TraceDiffResult {
  left: TraceDiffSide;
  right: TraceDiffSide;
  delta: TraceDiffDelta;
  trajectory: TraceDiffTrajectory;
  routes: {
    left: TraceDiffRouteStep[];
    right: TraceDiffRouteStep[];
    [key: string]: unknown;
  };
  steps: TraceDiffStep[];
  [key: string]: unknown;
}

export interface LoopListOptions extends TraceListOptions {
  cursor?: number;
  limit?: number;
  filter?: string;
  text?: string;
  q?: string;
}

export interface LoopDetailOptions extends TenantOptions, MetadataReverseFilterOptions {
  filter?: string;
  text?: string;
  q?: string;
}

export interface TaskTracesOptions extends TraceListOptions {
  cursor?: number;
  limit?: number;
  filter?: string;
  text?: string;
  q?: string;
}

export interface TaskTraceSummary {
  traceId: TenantId;
  externalTraceId?: string | null;
  spanCount: number;
  errorCount: number;
  status: "ok" | "error" | string;
  durationNs: {
    sum: WireNumber;
    max: WireNumber;
    [key: string]: unknown;
  };
  usage: UsageSummary;
  costUsd: number;
  costDetail: CostDetail;
  fields: Record<string, unknown>;
  [key: string]: unknown;
}

export interface LoopSummary {
  loopId: string;
  loopValue: unknown;
  taskFingerprint?: string | null;
  status: "ok" | "error" | string;
  spanCount: number;
  traceCount: number;
  sessionCount: number;
  errorCount: number;
  errorRate: number;
  firstTraceId: TenantId;
  lastTraceId: TenantId;
  durationNs: TraceAggregateDuration;
  usage: UsageSummary;
  costUsd: number;
  costDetail: CostDetail;
  phases: string[];
  validators: string[];
  fields: Record<string, unknown>;
  examples: TraceAggregateExample[];
  [key: string]: unknown;
}

export interface LoopPage<T = LoopSummary> {
  items: T[];
  nextCursor: number | null;
  total: number;
  [key: string]: unknown;
}

export interface LoopDetail {
  summary: LoopSummary;
  traces: TaskTraceSummary[];
  spans: TraceSpan[];
  [key: string]: unknown;
}

export interface TaskTracePage<T = TaskTraceSummary> {
  items: T[];
  nextCursor: number | null;
  total: number;
  [key: string]: unknown;
}

export interface SpanPage<T = TraceSpan> {
  items: T[];
  nextCursor: number | null;
  total: number;
  [key: string]: unknown;
}

export interface SpanBatchOptions extends TenantOptions {
  spanIds?: TenantId[];
  span_ids?: TenantId[];
  includeFull?: boolean;
  include_full?: boolean;
  full?: boolean;
  [key: string]: unknown;
}

export interface TraceSnapshot {
  snapshotId: string;
  snapshotHash: string;
  createdAt: WireNumber;
  trace: TraceDetail;
  [key: string]: unknown;
}

export interface SpanBuilderDefaults {
  traceId?: TenantId;
  trace_id?: TenantId;
  sessionId?: TenantId;
  session_id?: TenantId;
  tenantId?: TenantId;
  tenant_id?: TenantId;
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
  harnessVersion?: unknown;
  harness_version?: unknown;
  validationStatus?: unknown;
  validation_status?: unknown;
  stopReason?: unknown;
  stop_reason?: unknown;
  phase?: unknown;
  validator?: unknown;
  externalRunId?: unknown;
  external_run_id?: unknown;
  connectionIds?: unknown;
  connection_ids?: unknown;
  dataSourceIds?: unknown;
  data_source_ids?: unknown;
  schemaFingerprint?: unknown;
  schema_fingerprint?: unknown;
  intentSignature?: unknown;
  intent_signature?: unknown;
  reviewStatus?: unknown;
  review_status?: unknown;
  evalStatus?: unknown;
  eval_status?: unknown;
  pathMemoryId?: unknown;
  path_memory_id?: unknown;
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
  cachedInputTokens?: WireNumber | null;
  cached_input_tokens?: WireNumber | null;
  reasoningTokens?: WireNumber | null;
  reasoning_tokens?: WireNumber | null;
  totalTokens?: WireNumber | null;
  total_tokens?: WireNumber | null;
  costUsd?: number | null;
  cost_usd?: number | null;
  costUsdNanos?: WireNumber | null;
  cost_usd_nanos?: WireNumber | null;
  costCurrency?: string | null;
  cost_currency?: string | null;
  provider?: string | null;
  llmProvider?: string | null;
  attrs?: Record<string, unknown>;
}

export declare class SpanEventBuilder {
  constructor(defaults?: SpanBuilderDefaults);
  startSpan(options: SpanBuilderEventOptions): SpanEvent;
  log(options: SpanBuilderEventOptions): SpanEvent;
  endSpan(options: SpanBuilderEventOptions): SpanEvent;
  events(): SpanEvent[];
  clear(): void;
  ingest(db: YiTraceDB, options?: TenantOptions): Promise<IngestResult>;
}

export declare function createSpanEventBuilder(defaults?: SpanBuilderDefaults): SpanEventBuilder;

export declare class YiTraceDB {
  static open(pathOrOptions: string | OpenOptions): Promise<YiTraceDB>;

  ingest(events: SpanEvent[], options?: TenantOptions): Promise<IngestResult>;
  ingestOtlp(body: unknown, options?: TenantOptions): Promise<IngestResult>;
  search<T = SearchHit>(query?: SearchQuery, options?: TenantOptions): Promise<T[]>;
  indexVector<T = { ok: true; vectorIndex: string }>(vector: VectorIndexInput, options?: TenantOptions): Promise<T>;
  searchVector<T = VectorSearchPage>(query: VectorSearchQuery, options?: TenantOptions): Promise<T>;
  traceSearch<T = TraceSearchPage>(query?: TraceSearchQuery, options?: TenantOptions): Promise<T>;
  traceAggregate<T = TraceAggregatePage>(query?: TraceAggregateQuery, options?: TenantOptions): Promise<T>;
  trajectoryGroups<T = TrajectoryGroupPage>(query?: TrajectoryGroupsQuery, options?: TenantOptions): Promise<T>;
  traceTrajectories<T = TraceTrajectoryPage>(query?: TraceTrajectoriesQuery, options?: TenantOptions): Promise<T>;
  storageStats<T = StorageStatsReport>(query?: StorageStatsQuery, options?: TenantOptions): Promise<T>;
  retentionPlan<T = RetentionPlanResult>(query?: RetentionPlanQuery, options?: TenantOptions): Promise<T>;
  applyRetention<T = RetentionPlanResult>(query?: RetentionPlanQuery, options?: TenantOptions): Promise<T>;
  retentionAudits<T = RetentionAuditPage>(query?: RetentionAuditQuery, options?: TenantOptions): Promise<T>;
  createRetentionPolicy<T = RetentionPolicy>(policy: RetentionPolicyInput, options?: TenantOptions): Promise<T>;
  retentionPolicies<T = RetentionPolicyPage>(filter?: RetentionPolicyFilter, options?: TenantOptions): Promise<T>;
  runRetentionPolicies<T = RunRetentionPoliciesResult>(query?: RunRetentionPoliciesQuery, options?: TenantOptions): Promise<T>;
  createGoldenPath<T = GoldenPathCandidate>(candidate: GoldenPathCandidateInput, options?: TenantOptions): Promise<T>;
  goldenPaths<T = GoldenPathPage>(filter?: GoldenPathFilter, options?: TenantOptions): Promise<T>;
  updateGoldenPathStatus<T = GoldenPathCandidate>(goldenPathId: TenantId, update: GoldenPathStatus | GoldenPathStatusUpdate, options?: TenantOptions): Promise<T>;
  pathAdherence<T = PathAdherenceResult>(query: PathAdherenceQuery, options?: TenantOptions): Promise<T>;
  pathAdherence<T = PathAdherenceResult>(goldenPathId: TenantId, traceId: TenantId, options?: TenantOptions): Promise<T>;
  goldenPathEvidence<T = GoldenPathEvidenceResult>(query: GoldenPathEvidenceQuery, options?: TenantOptions): Promise<T>;
  goldenPathEvidence<T = GoldenPathEvidenceResult>(goldenPathId: TenantId, options?: TenantOptions): Promise<T>;
  goldenPathExport<T = GoldenPathExportResult>(query?: GoldenPathExportQuery, options?: TenantOptions): Promise<T>;
  goldenPathHealth<T = GoldenPathHealthResult>(query: GoldenPathHealthQuery, options?: TenantOptions): Promise<T>;
  goldenPathHealth<T = GoldenPathHealthResult>(goldenPathId: TenantId, query?: Omit<GoldenPathHealthQuery, "goldenPathId" | "golden_path_id" | "id">, options?: TenantOptions): Promise<T>;
  traceDiff<T = TraceDiffResult>(query: TraceDiffQuery, options?: TenantOptions): Promise<T>;
  traceDiff<T = TraceDiffResult>(leftTraceId: TenantId, rightTraceId: TenantId, options?: TenantOptions): Promise<T>;
  loops<T = LoopPage>(options?: LoopListOptions): Promise<T>;
  loop<T = LoopDetail>(loopId: string, options?: LoopDetailOptions): Promise<T | null>;
  taskTraces<T = TaskTracePage>(taskFingerprint: string, options?: TaskTracesOptions): Promise<T>;
  annotate<T = TraceAnnotation>(annotation: AnnotationInput, options?: TenantOptions): Promise<T>;
  updateAnnotation<T = TraceAnnotation>(annotationId: TenantId, update?: AnnotationUpdate, options?: TenantOptions): Promise<T>;
  deleteAnnotation<T = TraceAnnotation>(annotationId: TenantId, options?: AnnotationDeleteOptions): Promise<T>;
  annotations<T = AnnotationPage>(filter?: AnnotationFilter, options?: TenantOptions): Promise<T>;
  linkDatasetItem<T = DatasetAssociation>(association: DatasetAssociationInput, options?: TenantOptions): Promise<T>;
  datasetAssociations<T = DatasetAssociationPage>(filter?: DatasetAssociationFilter, options?: TenantOptions): Promise<T>;
  traces<T = TraceSummary>(options?: TraceListOptions): Promise<T[]>;
  sessions<T = SessionPage>(options?: SessionsOptions): Promise<T>;
  trace<T = TraceDetail>(traceId: TenantId, options?: TenantOptions): Promise<T | null>;
  traceSnapshot<T = TraceSnapshot>(traceId: TenantId, options?: TenantOptions): Promise<T | null>;
  spans<T = SpanPage>(traceId: TenantId, options?: TenantOptions & { cursor?: number; limit?: number; includeFull?: boolean; include_full?: boolean; full?: boolean }): Promise<T | null>;
  spansBatch<T = { items: TraceSpan[] }>(traceId: TenantId, spanIdsOrOptions?: TenantId[] | SpanBatchOptions, options?: TenantOptions & { includeFull?: boolean; include_full?: boolean; full?: boolean }): Promise<T | null>;
  span<T = SpanDetail>(traceId: TenantId, spanId: TenantId, options?: TenantOptions): Promise<T | null>;
  flush(): Promise<void>;
  close(): Promise<void>;
}
