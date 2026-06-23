// Mock 数据层：确定性生成「上千会话」+ 大 trace，证明虚拟滚动 + 游标分页扛得住量。
// 真实部署把这个换成 httpApi（见 http.ts），上层组件/hook 不动。

import type { SearchHit, Span, SpanDetail, SpanKind, Status, Step, TraceApi, TraceSummary, SessionSummary } from './types'

const SESSION_COUNT = 4000 // 故意上千：试虚拟滚动与分页
const PAGE_LIMIT_DEFAULT = 50

// 确定性伪随机（splitmix32 变体），同 seed 可复现。
function rng(seed: number) {
  let s = seed >>> 0
  return () => {
    s = (s + 0x9e3779b9) >>> 0
    let t = s
    t = Math.imul(t ^ (t >>> 15), t | 1)
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61)
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296
  }
}

const KINDS: SpanKind[] = ['llm', 'tool', 'chain', 'retriever', 'agent']
const TOPICS = [
  '反洗钱可疑交易排查', '信用卡盗刷共性分析', '理财合规话术检查', '对公账户结构对比',
  '本月GMV查询', '逾期率风险分级', '渠道流失归因', '反欺诈规则回测',
  '客诉根因聚类', '高净值客户资产诊断', '营销ROI归因', '现金流需求预测',
]
const MODELS = ['Qwen2.5-72B', 'Qwen2.5-7B']

function pick<T>(r: () => number, xs: T[]): T {
  return xs[Math.floor(r() * xs.length)]
}
function statusOf(r: () => number): Status {
  const v = r()
  return v < 0.08 ? 'error' : v < 0.11 ? 'run' : 'ok'
}

// 会话元数据（轮数、标题、时间）由 sessionId 确定性派生。
function sessionMeta(i: number): SessionSummary {
  const r = rng(i * 2654435761)
  const turnCount = r() < 0.18 ? 2 + Math.floor(r() * (r() < 0.1 ? 48 : 6)) : 1 // ~18% 多轮，少数会话几十轮
  const title = pick(r, TOPICS)
  const status = statusOf(r)
  const startedAt = 1_750_000_000_000 - i * 37_000 - Math.floor(r() * 30_000)
  let totalCost = 0
  for (let t = 0; t < turnCount; t++) totalCost += 0.005 + r() * 0.06
  return {
    sessionId: `sess-${(100000 + i).toString(36)}`,
    title,
    turnCount,
    totalCost: Math.round(totalCost * 1000) / 1000,
    status,
    startedAt,
    firstTraceId: `tr-${i}-0`,
  }
}

function turnsOf(sessionId: string, i: number, turnCount: number): TraceSummary[] {
  const r = rng(i * 40503 + 7)
  return Array.from({ length: turnCount }, (_, t) => {
    const status = t === turnCount - 1 ? 'ok' : statusOf(r)
    const spanCount = 4 + Math.floor(r() * (r() < 0.05 ? 1200 : 40)) // 个别 trace 上千 span
    return {
      traceId: `tr-${i}-${t}`,
      sessionId,
      turnIndex: t,
      name: `${pick(r, TOPICS)}${turnCount > 1 ? ` · 第${t + 1}步` : ''}`,
      durMs: 800 + Math.floor(r() * 18000),
      cost: Math.round((0.005 + r() * 0.06) * 1000) / 1000,
      spanCount,
      status,
    }
  })
}

// 由 traceId 解析回 (i, t)，再确定性生成 spans。
function spansOf(traceId: string): { summary: TraceSummary; spans: Span[] } {
  const m = /^tr-(\d+)-(\d+)$/.exec(traceId)
  const i = m ? +m[1] : 0
  const t = m ? +m[2] : 0
  const meta = sessionMeta(i)
  const summary = turnsOf(meta.sessionId, i, meta.turnCount)[t]
  const r = rng((i * 131 + t) * 2246822519)
  const n = summary.spanCount
  const spans: Span[] = []
  // 一棵随时间推进的调用树：root(agent) → 若干子步。
  let clock = 0
  const root: Span = { id: `${traceId}-s0`, parentId: null, kind: 'agent', name: 'agent.workflow', startMs: 0, durMs: summary.durMs, status: summary.status, cost: summary.cost, depth: 0 }
  spans.push(root)
  const stack: { id: string; depth: number; end: number }[] = [{ id: root.id, depth: 0, end: summary.durMs }]
  for (let k = 1; k < n; k++) {
    while (stack.length > 1 && r() < 0.35) stack.pop()
    const parent = stack[stack.length - 1]
    const kind = pick(r, KINDS)
    const dur = Math.max(20, Math.floor((parent.end / n) * (0.5 + r() * 2)))
    clock = Math.min(parent.end - dur, clock + Math.floor(r() * (parent.end / n)))
    const start = Math.max(0, clock)
    const st: Status = r() < 0.04 ? 'error' : 'ok'
    const inTok = kind === 'llm' ? 200 + Math.floor(r() * 1800) : undefined
    const outTok = kind === 'llm' ? 40 + Math.floor(r() * 500) : undefined
    const sp: Span = {
      id: `${traceId}-s${k}`,
      parentId: parent.id,
      kind,
      name: `${kind === 'llm' ? 'LLM 调用' : kind === 'tool' ? '工具调用' : kind === 'retriever' ? '向量检索' : kind === 'chain' ? '推理链' : '子 agent'} #${k}`,
      startMs: start,
      durMs: dur,
      status: st,
      cost: Math.round(r() * 0.02 * 1000) / 1000,
      inTok,
      outTok,
      model: kind === 'llm' ? pick(r, MODELS) : undefined,
      depth: parent.depth + 1,
    }
    spans.push(sp)
    if (r() < 0.5 && stack.length < 8) stack.push({ id: sp.id, depth: sp.depth, end: start + dur })
  }
  return { summary, spans }
}

function delay<T>(v: T, ms = 80): Promise<T> {
  return new Promise((res) => setTimeout(() => res(v), ms))
}

export const mockApi: TraceApi = {
  async listSessions({ cursor, limit = PAGE_LIMIT_DEFAULT, filter }) {
    const start = cursor ? parseInt(cursor, 10) : 0
    const items: SessionSummary[] = []
    let i = start
    while (items.length < limit && i < SESSION_COUNT) {
      const s = sessionMeta(i)
      if (!filter || s.title.includes(filter) || s.sessionId.includes(filter)) items.push(s)
      i++
    }
    const nextCursor = i < SESSION_COUNT ? String(i) : null
    return delay({ items, nextCursor, total: SESSION_COUNT })
  },
  async listTurns(sessionId) {
    const m = /^sess-([0-9a-z]+)$/.exec(sessionId)
    const i = m ? parseInt(m[1], 36) - 100000 : 0
    const meta = sessionMeta(i)
    return delay(turnsOf(sessionId, i, meta.turnCount))
  },
  async getTrace(traceId) {
    return delay(spansOf(traceId), 120)
  },
  async getSpanDetail(traceId, spanId) {
    const r = rng(spanId.length * 99 + traceId.length)
    const d: SpanDetail = {
      id: spanId,
      input: `用户/上游输入（${spanId}）：` + '对该批可疑账户做资金链路追踪，给出研判结论。'.repeat(1 + Math.floor(r() * 3)),
      output: '研判结论：触发规则 R12，存在盗刷风险，建议人工复核并暂缓放款。'.repeat(1 + Math.floor(r() * 4)),
    }
    return delay(d, 60)
  },
  async getSteps(traceId) {
    const { spans } = spansOf(traceId)
    const steps: Step[] = spans.map((s, i) => ({
      id: s.id,
      kind: s.kind,
      name: s.name,
      status: s.status,
      durMs: s.durMs,
      inTok: s.inTok ?? 0,
      outTok: s.outTok ?? 0,
      model: s.model,
      input: `第 ${i + 1} 步输入：` + '对该批可疑账户做资金链路追踪。',
      output: s.status === 'error' ? '执行报错：KeyError 列名拼写' : '已完成，返回观察结果并更新状态。',
    }))
    return delay(steps, 100)
  },
  async searchSpans(query, k) {
    // 简化的中文召回：扫前若干会话，标题命中 query 的当命中，按相关度（命中位置）排序。
    const hits: SearchHit[] = []
    const q = query.trim()
    for (let i = 0; i < 600 && hits.length < k; i++) {
      const s = sessionMeta(i)
      if (q && !s.title.includes(q)) continue
      hits.push({
        traceId: s.firstTraceId,
        spanId: `${s.firstTraceId}-s0`,
        score: Math.round((0.95 - hits.length * 0.03) * 100) / 100,
        status: s.status === 'error' ? 'error' : 'ok',
        agentName: s.title,
        snippet: `${s.title} … 命中「${q}」`,
      })
    }
    return delay(hits, 120)
  },
}
