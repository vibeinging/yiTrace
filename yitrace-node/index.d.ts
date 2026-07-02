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
  attrs?: Partial<Record<"project_id" | "skill" | "mode" | "call_site", unknown>>;
  [key: string]: unknown;
}

export interface SearchQuery {
  text?: string;
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

export interface SessionsOptions extends TenantOptions {
  cursor?: number;
  limit?: number;
  filter?: string;
  projectId?: unknown;
  project_id?: unknown;
  skill?: unknown;
  mode?: unknown;
  callSite?: unknown;
  call_site?: unknown;
  attrs?: Partial<Record<"project_id" | "skill" | "mode" | "call_site", unknown>>;
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
  spans?: unknown[];
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
  attrs?: Record<string, unknown>;
  [key: string]: unknown;
}

export interface SpanBuilderDefaults {
  traceId?: TenantId;
  trace_id?: TenantId;
  sessionId?: TenantId;
  session_id?: TenantId;
  tenantId?: TenantId;
  tenant_id?: TenantId;
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
  traces<T = TraceSummary>(options?: TenantOptions): Promise<T[]>;
  sessions<T = SessionPage>(options?: SessionsOptions): Promise<T>;
  trace<T = TraceDetail>(traceId: TenantId, options?: TenantOptions): Promise<T | null>;
  span<T = SpanDetail>(traceId: TenantId, spanId: TenantId, options?: TenantOptions): Promise<T | null>;
  flush(): Promise<void>;
  close(): Promise<void>;
}
