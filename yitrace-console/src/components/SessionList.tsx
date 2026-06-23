import { useEffect, useMemo, useRef, useState } from 'react'
import { useVirtualizer } from '@tanstack/react-virtual'
import { useSessions } from '../hooks/queries'
import { api } from '../api'
import type { SessionSummary, TraceSummary } from '../api'

const fmtDur = (ms: number) => (ms >= 1000 ? (ms / 1000).toFixed(2) + 's' : ms + 'ms')

// 把"会话 + 展开的轮次"摊平成一维 row 列表 —— 虚拟滚动只认一维。
type Row =
  | { k: 'single'; s: SessionSummary }
  | { k: 'group'; s: SessionSummary; open: boolean }
  | { k: 'turn'; t: TraceSummary; idx: number }
  | { k: 'turnsLoading'; id: string }
  | { k: 'loader' }

export function SessionList({
  selectedTrace,
  filter,
  onSelect,
}: {
  selectedTrace: string | null
  filter: string
  onSelect: (traceId: string) => void
}) {
  const { data, fetchNextPage, hasNextPage, isFetchingNextPage, isLoading } = useSessions(filter)
  const [expanded, setExpanded] = useState<Set<string>>(new Set())
  const [turns, setTurns] = useState<Record<string, TraceSummary[]>>({})

  const sessions = useMemo(() => data?.pages.flatMap((p) => p.items) ?? [], [data])

  // 加载完默认选中第一条会话的首轮，中间区不至于空着。
  const didInit = useRef(false)
  useEffect(() => {
    if (!didInit.current && !selectedTrace && sessions.length) {
      didInit.current = true
      onSelect(sessions[0].firstTraceId)
    }
  }, [sessions, selectedTrace, onSelect])

  function toggle(s: SessionSummary) {
    if (s.turnCount <= 1) {
      onSelect(s.firstTraceId)
      return
    }
    setExpanded((prev) => {
      const next = new Set(prev)
      if (next.has(s.sessionId)) next.delete(s.sessionId)
      else {
        next.add(s.sessionId)
        if (!turns[s.sessionId]) api.listTurns(s.sessionId).then((ts) => setTurns((m) => ({ ...m, [s.sessionId]: ts })))
      }
      return next
    })
  }

  // 摊平。
  const rows = useMemo<Row[]>(() => {
    const out: Row[] = []
    for (const s of sessions) {
      if (s.turnCount <= 1) {
        out.push({ k: 'single', s })
      } else {
        const open = expanded.has(s.sessionId)
        out.push({ k: 'group', s, open })
        if (open) {
          const ts = turns[s.sessionId]
          if (ts) ts.forEach((t, idx) => out.push({ k: 'turn', t, idx }))
          else out.push({ k: 'turnsLoading', id: s.sessionId })
        }
      }
    }
    if (hasNextPage) out.push({ k: 'loader' })
    return out
  }, [sessions, expanded, turns, hasNextPage])

  const parentRef = useRef<HTMLDivElement>(null)
  const virt = useVirtualizer({
    count: rows.length,
    getScrollElement: () => parentRef.current,
    estimateSize: (i) => {
      const r = rows[i]
      return r.k === 'group' ? 56 : r.k === 'single' ? 62 : r.k === 'turn' ? 33 : 40
    },
    overscan: 12,
  })

  // 滚到底自动拉下一页。
  const items = virt.getVirtualItems()
  useEffect(() => {
    const last = items[items.length - 1]
    if (last && last.index >= rows.length - 1 && hasNextPage && !isFetchingNextPage) fetchNextPage()
  }, [items, rows.length, hasNextPage, isFetchingNextPage, fetchNextPage])

  return (
    <div className="pane list">
      <div className="ph">
        会话列表
        <small>{isLoading ? '加载中…' : `${sessions.length} / ${data?.pages[0].total ?? '…'} 会话`}</small>
      </div>
      <div className="vlist" ref={parentRef}>
        <div className="vinner" style={{ height: virt.getTotalSize() }}>
          {items.map((vi) => {
            const r = rows[vi.index]
            return (
              <div
                key={vi.key}
                className="vrow"
                data-index={vi.index}
                ref={virt.measureElement}
                style={{ transform: `translateY(${vi.start}px)` }}
              >
                {renderRow(r, selectedTrace, toggle, onSelect)}
              </div>
            )
          })}
        </div>
      </div>
    </div>
  )
}

function renderRow(
  r: Row,
  sel: string | null,
  toggle: (s: SessionSummary) => void,
  onSelect: (id: string) => void,
) {
  if (r.k === 'loader') return <div className="rowloading">加载更多会话…</div>
  if (r.k === 'turnsLoading') return <div className="rowloading" style={{ marginLeft: 21 }}>加载轮次…</div>
  if (r.k === 'single') {
    const s = r.s
    return (
      <div className={'titem' + (sel === s.firstTraceId ? ' sel' : '')} onClick={() => onSelect(s.firstTraceId)}>
        <div className="tname"><span className={'dot ' + s.status} />{s.title}</div>
        <div className="tmeta"><span className="thsingle">🧵 单轮</span><span>${s.totalCost.toFixed(3)}</span><span className="thsingle">{s.sessionId}</span></div>
      </div>
    )
  }
  if (r.k === 'group') {
    const s = r.s
    return (
      <div>
        <div className="shead" onClick={() => toggle(s)}>
          <span className="scaret">{r.open ? '▾' : '▸'}</span>
          <span className={'dot ' + s.status} />
          <span className="sttl">{s.title}</span>
          <span className="mt">🧵 {s.turnCount} 轮</span>
        </div>
        <div className="ssub">🧵 {s.sessionId} · {s.turnCount} 轮对话 · 合计 ${s.totalCost.toFixed(3)}</div>
      </div>
    )
  }
  // turn
  const t = r.t
  return (
    <div className={'tturn' + (sel === t.traceId ? ' sel' : '')} onClick={() => onSelect(t.traceId)}>
      <span className="ti">第{t.turnIndex + 1}轮</span>
      <span className="tn">{t.name}</span>
      <span className="tmm">{fmtDur(t.durMs)} · ${t.cost.toFixed(3)}</span>
    </div>
  )
}
