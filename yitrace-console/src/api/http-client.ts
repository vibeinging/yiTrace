// 可测试的 HTTP 数据层。Vite 环境变量只在 http.ts 里读取，这里保持纯函数。

import type { Page, SpanDetail, Step, TraceApi, TraceSummary, SessionSummary } from './types'

type FetchLike = (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>

export interface HttpApiOptions {
  base?: string
  apiToken?: string
  tenantId?: string | (() => string | undefined)
  fetchImpl?: FetchLike
}

// 引擎 /v1/search 返回的命中行（蛇形字段）→ 控制台 SearchHit。
interface RawHit { trace_id: number; span_id: number; score: number; status: number | null; agent_name: string | null; logs: string[] }

function resolveTenantId(tenantId: HttpApiOptions['tenantId']): string | undefined {
  return typeof tenantId === 'function' ? tenantId() : tenantId
}

function authHeaders(apiToken: string | undefined, tenantId: HttpApiOptions['tenantId'], extra: Record<string, string> = {}): Record<string, string> {
  const headers: Record<string, string> = { ...extra }
  if (apiToken) headers.authorization = `Bearer ${apiToken}`
  const tenant = resolveTenantId(tenantId)
  if (tenant) headers['x-tenant-id'] = tenant
  return headers
}

export function createHttpApi(options: HttpApiOptions = {}): TraceApi {
  const base = options.base ?? '/v1'
  const fetcher = options.fetchImpl ?? fetch

  async function get<T>(path: string): Promise<T> {
    const res = await fetcher(base + path, { headers: authHeaders(options.apiToken, options.tenantId, { accept: 'application/json' }) })
    if (!res.ok) throw new Error(`${res.status} ${res.statusText}`)
    return res.json() as Promise<T>
  }

  async function post<T>(path: string, body: unknown): Promise<T> {
    const res = await fetcher(base + path, {
      method: 'POST',
      headers: authHeaders(options.apiToken, options.tenantId, { 'content-type': 'application/json' }),
      body: JSON.stringify(body),
    })
    if (!res.ok) throw new Error(`${res.status} ${res.statusText}`)
    return res.json() as Promise<T>
  }

  return {
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
}
