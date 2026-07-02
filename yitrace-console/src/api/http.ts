// 真实 HTTP 实现：对接引擎的 HTTP 网关。
//
// 控制台数据端点已由引擎 HTTP 网关提供。启用鉴权/租户隔离时，用 VITE_API_TOKEN、
// VITE_TENANT_ID，或在浏览器 localStorage 写入 yitrace.tenantId。
//
//   GET /v1/sessions?cursor=&limit=&filter=     → Page<SessionSummary>
//   GET /v1/sessions/:id/turns                  → TraceSummary[]
//   GET /v1/traces/:id                          → { summary, spans }
//   GET /v1/traces/:id/spans/:spanId            → SpanDetail   （大字段晚物化）

import type { Page, SpanDetail, Step, TraceApi, TraceSummary, SessionSummary } from './types'

const BASE = (import.meta.env.VITE_API_BASE as string | undefined) ?? '/v1'
const API_TOKEN = import.meta.env.VITE_API_TOKEN as string | undefined
const ENV_TENANT_ID = import.meta.env.VITE_TENANT_ID as string | undefined

function tenantId(): string | undefined {
  if (ENV_TENANT_ID) return ENV_TENANT_ID
  if (typeof window === 'undefined') return undefined
  return window.localStorage.getItem('yitrace.tenantId') ?? undefined
}

function authHeaders(extra: Record<string, string> = {}): Record<string, string> {
  const headers: Record<string, string> = { ...extra }
  if (API_TOKEN) headers.authorization = `Bearer ${API_TOKEN}`
  const tenant = tenantId()
  if (tenant) headers['x-tenant-id'] = tenant
  return headers
}

async function get<T>(path: string): Promise<T> {
  const res = await fetch(BASE + path, { headers: authHeaders({ accept: 'application/json' }) })
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`)
  return res.json() as Promise<T>
}

async function post<T>(path: string, body: unknown): Promise<T> {
  const res = await fetch(BASE + path, {
    method: 'POST',
    headers: authHeaders({ 'content-type': 'application/json' }),
    body: JSON.stringify(body),
  })
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`)
  return res.json() as Promise<T>
}

// 引擎 /v1/search 返回的命中行（蛇形字段）→ 控制台 SearchHit。
interface RawHit { trace_id: number; span_id: number; score: number; status: number | null; agent_name: string | null; logs: string[] }

export const httpApi: TraceApi = {
  listSessions: ({ cursor, limit, filter }) => {
    const q = new URLSearchParams()
    if (cursor) q.set('cursor', cursor)
    q.set('limit', String(limit))
    if (filter) q.set('filter', filter)
    return get<Page<SessionSummary>>(`/sessions?${q}`)
  },
  listTurns: (sessionId) => get<TraceSummary[]>(`/sessions/${encodeURIComponent(sessionId)}/turns`),
  getTrace: (traceId) => get(`/traces/${encodeURIComponent(traceId)}`),
  getSpanDetail: (traceId, spanId) => get<SpanDetail>(`/traces/${encodeURIComponent(traceId)}/spans/${encodeURIComponent(spanId)}`),
  getSteps: (traceId) => get<Step[]>(`/traces/${encodeURIComponent(traceId)}/steps`),
  searchSpans: async (query, k) => {
    const hits = await post<RawHit[]>('/search', { text: query, k })
    return hits.map((h) => ({
      traceId: String(h.trace_id),
      spanId: String(h.span_id),
      score: h.score,
      status: h.status === null ? 'ok' : h.status === 0 ? 'ok' : 'error',
      agentName: h.agent_name ?? undefined,
      snippet: h.logs?.[0],
    }))
  },
}
