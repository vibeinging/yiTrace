// 控制台数据模型。字段对齐引擎已有的输出（SessionSummary / TraceSummary / FoldedSpan）。

export type SpanKind = 'llm' | 'tool' | 'chain' | 'retriever' | 'agent'
export type Status = 'ok' | 'error' | 'run'

/** 会话摘要 = 引擎 list_sessions 的一行（多轮串起的一组 trace）。 */
export interface SessionSummary {
  sessionId: string
  title: string // 会话标题（取首轮 trace 名）
  turnCount: number // 几轮 = 几条 trace
  totalCost: number
  status: Status
  startedAt: number // 排序/游标用
  firstTraceId: string // 单轮会话直接选它；多轮展开再拉全部轮次
}

/** trace 摘要 = 会话里的一轮。 */
export interface TraceSummary {
  traceId: string
  sessionId: string
  turnIndex: number // 第几轮（0 起）
  name: string
  durMs: number
  cost: number
  spanCount: number
  status: Status
}

/** 折叠后的 span（瀑布一行）。大文本字段不在这里，选中才单独拉。 */
export interface Span {
  id: string
  parentId: string | null
  kind: SpanKind
  name: string
  startMs: number
  durMs: number
  status: Status
  cost: number
  inTok?: number
  outTok?: number
  model?: string
  depth: number
}

/** span 详情：大字段晚物化（选中才拉）。 */
export interface SpanDetail {
  id: string
  input?: string
  output?: string
  error?: string
}

/** 语义检索命中（POST /v1/search 的一行）。 */
export interface SearchHit {
  traceId: string
  spanId: string
  score: number
  status: Status
  agentName?: string
  snippet?: string
}

/** 步骤流的一步 = 一个 span 连同输入/输出文本（步骤流视图专用，一次物化）。 */
export interface Step {
  id: string
  kind: SpanKind
  name: string
  status: Status
  durMs: number
  inTok: number
  outTok: number
  model?: string
  input?: string
  output?: string
}

/** 游标分页页。 */
export interface Page<T> {
  items: T[]
  nextCursor: string | null
  total: number
}

/** 数据访问接口。mock 与真实 HTTP 实现同一套契约，切换不动上层。 */
export interface TraceApi {
  /** 会话列表，按时间游标分页（千会话只拉可视区那几页）。 */
  listSessions(params: { cursor?: string | null; limit: number; filter?: string }): Promise<Page<SessionSummary>>
  /** 一个会话的全部轮次（多轮会话展开时拉）。 */
  listTurns(sessionId: string): Promise<TraceSummary[]>
  /** 一条 trace 的折叠 span 列表（瀑布，可能上千行）。 */
  getTrace(traceId: string): Promise<{ summary: TraceSummary; spans: Span[] }>
  /** 单个 span 的大字段（选中才拉）。 */
  getSpanDetail(traceId: string, spanId: string): Promise<SpanDetail>
  /** 步骤流：每步连同输入/输出文本（切到步骤流视图才拉）。 */
  getSteps(traceId: string): Promise<Step[]>
  /** 语义检索：中文 BM25 召回命中的 span（回车触发）。 */
  searchSpans(query: string, k: number): Promise<SearchHit[]>
}
