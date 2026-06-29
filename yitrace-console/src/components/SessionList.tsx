import { useEffect, useMemo, useRef } from 'react'
import { useVirtualizer } from '@tanstack/react-virtual'
import { useSessions } from '../hooks/queries'
import type { SessionSummary } from '../api'

// 会话列表：每条会话一行（单轮/多轮统一）。多轮不再在此折叠——
// 选中后由右栏的「多轮时间线」承载各轮切换。onSelect 同时回填 sessionId + 首轮 traceId。
type Row = { k: 'item'; s: SessionSummary } | { k: 'loader' }

export function SessionList({
  selectedTrace,
  filter,
  onSelect,
}: {
  selectedTrace: string | null
  filter: string
  onSelect: (sessionId: string, traceId: string) => void
}) {
  const { data, fetchNextPage, hasNextPage, isFetchingNextPage, isLoading } = useSessions(filter)

  const sessions = useMemo(() => data?.pages.flatMap((p) => p.items) ?? [], [data])

  // 加载完默认选中第一条会话的首轮，中栏 / 右栏不至于空着。
  const didInit = useRef(false)
  useEffect(() => {
    if (!didInit.current && !selectedTrace && sessions.length) {
      didInit.current = true
      onSelect(sessions[0].sessionId, sessions[0].firstTraceId)
    }
  }, [sessions, selectedTrace, onSelect])

  const rows = useMemo<Row[]>(() => {
    const out: Row[] = sessions.map((s) => ({ k: 'item', s }))
    if (hasNextPage) out.push({ k: 'loader' })
    return out
  }, [sessions, hasNextPage])

  const parentRef = useRef<HTMLDivElement>(null)
  const virt = useVirtualizer({
    count: rows.length,
    getScrollElement: () => parentRef.current,
    estimateSize: () => 62,
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
                {renderRow(r, selectedTrace, onSelect)}
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
  onSelect: (sessionId: string, traceId: string) => void,
) {
  if (r.k === 'loader') return <div className="rowloading">加载更多会话…</div>
  const s = r.s
  // 多轮：列表行高亮看会话首轮是否被选中（firstTraceId == selectedTrace 时即首轮）。
  const isSel = sel === s.firstTraceId || sel != null && s.firstTraceId === sel
  return (
    <div className={'titem' + (isSel ? ' sel' : '')} onClick={() => onSelect(s.sessionId, s.firstTraceId)}>
      <div className="tname"><span className={'dot ' + s.status} />{s.title}</div>
      <div className="tmeta">
        <span className="thsingle">{s.turnCount > 1 ? `🧵 ${s.turnCount} 轮` : '🧵 单轮'}</span>
        <span>${s.totalCost.toFixed(3)}</span>
        <span className="thsingle">{s.sessionId}</span>
      </div>
    </div>
  )
}
