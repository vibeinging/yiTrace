import type { TraceApi } from './types'
import { mockApi } from './mock'
import { httpApi } from './http'

// 数据源开关：默认 mock（前端独立可跑、演示上千会话）；
// 设 VITE_API=http 走真实引擎网关。
export const api: TraceApi = import.meta.env.VITE_API === 'http' ? httpApi : mockApi

export * from './types'
