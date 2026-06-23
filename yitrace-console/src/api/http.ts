// 真实 HTTP 实现：对接引擎的 HTTP 网关。
//
// 现状：引擎已有 GET /v1/traces（列表）、POST /v1/search（检索），但**还没有游标分页参数**
// （list_sessions/list_traces 只收时间窗）。下列端点是按本控制台需要约定的目标形状，
// 后端补齐 limit + cursor 后即可启用（把 main.tsx 里的 api 从 mockApi 换成 httpApi）。
//
//   GET /v1/sessions?cursor=&limit=&filter=     → Page<SessionSummary>
//   GET /v1/sessions/:id/turns                  → TraceSummary[]
//   GET /v1/traces/:id                          → { summary, spans }
//   GET /v1/traces/:id/spans/:spanId            → SpanDetail   （大字段晚物化）

import type { Page, SpanDetail, Step, TraceApi, TraceSummary, SessionSummary } from './types'

const BASE = (import.meta.env.VITE_API_BASE as string | undefined) ?? '/v1'

async function get<T>(path: string): Promise<T> {
  const res = await fetch(BASE + path, { headers: { accept: 'application/json' } })
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`)
  return res.json() as Promise<T>
}

async function post<T>(path: string, body: unknown): Promise<T> {
  const res = await fetch(BASE + path, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) })
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
