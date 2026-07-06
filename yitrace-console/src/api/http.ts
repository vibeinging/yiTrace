// 真实 HTTP 实现：对接引擎的 HTTP 网关。
//
// 控制台数据端点已由引擎 HTTP 网关提供。启用鉴权/租户隔离时，用 VITE_API_TOKEN、
// VITE_TENANT_ID，或在浏览器 localStorage 写入 yitrace.tenantId。
//
//   GET /v1/sessions?cursor=&limit=&filter=     → Page<SessionSummary>
//   GET /v1/sessions/:id/turns                  → TraceSummary[]
//   GET /v1/traces/:id                          → { summary, spans }
//   GET /v1/traces/:id/spans/:spanId            → SpanDetail   （大字段晚物化）

import type { TraceApi } from './types'
import { createHttpApi } from './http-client'

const BASE = (import.meta.env.VITE_API_BASE as string | undefined) ?? '/v1'
const API_TOKEN = import.meta.env.VITE_API_TOKEN as string | undefined
const ENV_TENANT_ID = import.meta.env.VITE_TENANT_ID as string | undefined

function tenantId(): string | undefined {
  if (ENV_TENANT_ID) return ENV_TENANT_ID
  if (typeof window === 'undefined') return undefined
  return window.localStorage.getItem('yitrace.tenantId') ?? undefined
}

export const httpApi: TraceApi = createHttpApi({ base: BASE, apiToken: API_TOKEN, tenantId })
