import { useTurns } from '../hooks/queries'

const fmtDur = (ms: number) => (ms >= 1000 ? (ms / 1000).toFixed(2) + 's' : ms + 'ms')

// 多轮时间线：选中会话的各轮垂直排列。复用 useTurns（切会话自动复用缓存）。
// 纯内容渲染（无 pane 外壳），由宿主决定布局位置——当前嵌在中栏左侧。
export function TurnTimeline({
  sessionId,
  selectedTrace,
  onSelect,
}: {
  sessionId: string | null
  selectedTrace: string | null
  onSelect: (traceId: string) => void
}) {
  const { data: turns, isLoading } = useTurns(sessionId, !!sessionId)

  if (!sessionId) return <div className="empty">← 从左侧选一条会话</div>
  if (isLoading) return <div className="spin">加载轮次…</div>
  if (!turns || turns.length === 0) return <div className="empty">该会话无轮次数据</div>

  return (
    <div className="tlbody">
      {turns.map((t, i) => {
        const tot = (t.inTok ?? 0) + (t.outTok ?? 0)
        const sel = selectedTrace === t.traceId
        return (
          <div
            key={t.traceId}
            className={'tlnode' + (sel ? ' sel' : '')}
            onClick={() => onSelect(t.traceId)}
          >
            <span className="tldotwrap">
              <span className={'dot ' + t.status} />
              {i < turns.length - 1 && <span className="tlline" />}
            </span>
            <span className="tlmain">
              <span className="tlhead">
                <span className="tlno">第{t.turnIndex + 1}轮</span>
                <span className="tlmeta">{fmtDur(t.durMs)}{tot ? ` · ${tot} tok` : ''}{t.status === 'error' ? ' · 出错' : ''}</span>
              </span>
              <span className="tlname">{t.name}</span>
            </span>
          </div>
        )
      })}
    </div>
  )
}
