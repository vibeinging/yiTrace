import { useMemo } from 'react'
import type { Span } from '../api'

const fmtDur = (ms: number) => (ms >= 1000 ? (ms / 1000).toFixed(2) + 's' : ms + 'ms')

// 火焰图：深度 × 时间的嵌套时间块。横=时间轴，纵=调用深度。一眼看时间花在哪条子树。
export function Flame({ spans, selectedSpan, onSelectSpan }: { spans: Span[]; selectedSpan: string | null; onSelectSpan: (id: string) => void }) {
  const maxEnd = useMemo(() => Math.max(1, ...spans.map((s) => s.startMs + s.durMs)), [spans])
  const maxDepth = useMemo(() => Math.max(0, ...spans.map((s) => s.depth)), [spans])
  const levels = useMemo(() => {
    const out: Span[][] = Array.from({ length: maxDepth + 1 }, () => [])
    for (const s of spans) out[s.depth].push(s)
    return out
  }, [spans, maxDepth])

  return (
    <div className="flamewrap">
      <div className="fhint">深度 × 时间的嵌套时间块：横=时间轴，纵=调用深度。点块选中。</div>
      <div className="flame">
        {levels.map((lvl, d) => (
          <div className="flevel" key={d}>
            {lvl.map((s) => {
              const left = (s.startMs / maxEnd) * 100
              const w = Math.max((s.durMs / maxEnd) * 100, 0.3)
              const cls = s.status === 'error' ? 'b-err' : 'b-' + s.kind
              return (
                <div
                  key={s.id}
                  className={'fblk ' + cls + (selectedSpan === s.id ? ' fsel' : '')}
                  style={{ left: left + '%', width: w + '%' }}
                  title={`${s.name} · ${fmtDur(s.durMs)}`}
                  onClick={() => onSelectSpan(s.id)}
                >
                  <span className="flbl">{s.name}</span>
                </div>
              )
            })}
          </div>
        ))}
      </div>
    </div>
  )
}
