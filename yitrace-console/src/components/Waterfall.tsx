import { useMemo, useRef, useState } from 'react'
import { useVirtualizer } from '@tanstack/react-virtual'
import { useTrace } from '../hooks/queries'
import type { Span } from '../api'
import { Flame } from './Flame'
import { AgentGraph } from './AgentGraph'
import { StepStream } from './StepStream'

type View = 'wf' | 'flame' | 'steps' | 'graph'

const fmtDur = (ms: number) => (ms >= 1000 ? (ms / 1000).toFixed(2) + 's' : ms + 'ms')
const KIND_LABEL: Record<string, string> = { llm: 'LLM', tool: 'TOOL', chain: 'CHAIN', retriever: 'RETR', agent: 'AGENT' }

export function Waterfall({
  traceId,
  selectedSpan,
  onSelectSpan,
}: {
  traceId: string | null
  selectedSpan: string | null
  onSelectSpan: (id: string) => void
}) {
  const { data, isLoading } = useTrace(traceId)
  const spans = data?.spans ?? []
  const summary = data?.summary
  const [view, setView] = useState<View>('wf')

  const maxEnd = useMemo(() => Math.max(1, ...spans.map((s) => s.startMs + s.durMs)), [spans])
  const agg = useMemo(() => {
    let inTok = 0, outTok = 0, cost = 0
    for (const s of spans) { inTok += s.inTok ?? 0; outTok += s.outTok ?? 0; cost += s.cost }
    return { inTok, outTok, cost, tot: inTok + outTok }
  }, [spans])

  const parentRef = useRef<HTMLDivElement>(null)
  const virt = useVirtualizer({
    count: spans.length,
    getScrollElement: () => parentRef.current,
    estimateSize: () => 30,
    overscan: 20,
  })

  if (!traceId) return <div className="center"><div className="empty">← 从左侧选一条会话 / 轮次</div></div>

  return (
    <div className="center">
      <div className="toolbar">
        <div>
          <div className="tt">{summary?.name ?? '加载中…'}</div>
          <div className="sub">{traceId}{summary ? ` · ${summary.spanCount} spans · ${fmtDur(summary.durMs)}` : ''}</div>
        </div>
        <div className="seg">
          <button className={view === 'wf' ? 'on' : ''} onClick={() => setView('wf')}>瀑布</button>
          <button className={view === 'flame' ? 'on' : ''} onClick={() => setView('flame')}>火焰图</button>
          <button className={view === 'steps' ? 'on' : ''} onClick={() => setView('steps')}>步骤流</button>
          <button className={view === 'graph' ? 'on' : ''} onClick={() => setView('graph')}>Agent图</button>
        </div>
      </div>
      <div className="summary">
        <div className="blk"><span className="lab">总 Token</span><span className="big">{agg.tot.toLocaleString()}</span></div>
        <div className="blk"><span className="lab">成本</span><span className="big cost">${agg.cost.toFixed(3)}</span></div>
        <div className="blk"><span className="lab">Span</span><span className="big">{spans.length.toLocaleString()}</span></div>
      </div>
      {view === 'wf' && (
        <div className="ruler">
          {Array.from({ length: 6 }, (_, i) => (
            <span key={i}>{((maxEnd / 6) * i / 1000).toFixed(1)}s</span>
          ))}
        </div>
      )}
      {isLoading ? (
        <div className="spin">加载 span…</div>
      ) : view === 'wf' ? (
        <div className="wfwrap" ref={parentRef}>
          <div className="vinner" style={{ height: virt.getTotalSize() }}>
            {virt.getVirtualItems().map((vi) => (
              <div key={vi.key} className="vrow" style={{ transform: `translateY(${vi.start}px)`, height: 30 }}>
                <SpanRow s={spans[vi.index]} maxEnd={maxEnd} sel={selectedSpan === spans[vi.index].id} onClick={onSelectSpan} />
              </div>
            ))}
          </div>
        </div>
      ) : view === 'flame' ? (
        <div className="viewscroll"><Flame spans={spans} selectedSpan={selectedSpan} onSelectSpan={onSelectSpan} /></div>
      ) : view === 'steps' ? (
        <StepStream traceId={traceId} active={view === 'steps'} />
      ) : (
        <div className="viewscroll"><AgentGraph spans={spans} /></div>
      )}
      {(view === 'wf' || view === 'flame') && spans.length > 1 && <Insight spans={spans} onSelect={onSelectSpan} />}
    </div>
  )
}

// 本 trace 洞察：耗时 / 成本 Top5（从内存里全部 span 算，与虚拟化渲染无关）。点行选中。
function Insight({ spans, onSelect }: { spans: Span[]; onSelect: (id: string) => void }) {
  const byDur = [...spans].sort((a, b) => b.durMs - a.durMs).slice(0, 5)
  const byCost = [...spans].sort((a, b) => b.cost - a.cost).slice(0, 5)
  const maxDur = Math.max(1, ...byDur.map((s) => s.durMs))
  const maxCost = Math.max(1e-6, ...byCost.map((s) => s.cost))
  const row = (s: Span, i: number, frac: number, val: string, color: string) => (
    <div className="irow" key={s.id} onClick={() => onSelect(s.id)} title={s.name}>
      <span className="irank">{i + 1}</span>
      <span className="iname">{s.name}</span>
      <span className="imeter"><i style={{ width: Math.max(frac * 100, 5) + '%', background: color }} /></span>
      <span className="ival">{val}</span>
    </div>
  )
  return (
    <div className="insight">
      <div className="icard">
        <h5>⏱ 耗时 Top 5</h5>
        {byDur.map((s, i) => row(s, i, s.durMs / maxDur, fmtDur(s.durMs), `var(--${s.kind === 'retriever' ? 'retr' : s.kind})`))}
      </div>
      <div className="icard">
        <h5>💰 成本 Top 5</h5>
        {byCost.map((s, i) => row(s, i, s.cost / maxCost, '$' + s.cost.toFixed(3), 'var(--ok)'))}
      </div>
    </div>
  )
}

function SpanRow({ s, maxEnd, sel, onClick }: { s: Span; maxEnd: number; sel: boolean; onClick: (id: string) => void }) {
  const left = (s.startMs / maxEnd) * 100
  const w = Math.max((s.durMs / maxEnd) * 100, 0.4)
  const barcls = s.status === 'error' ? 'b-err' : 'b-' + s.kind
  const tot = (s.inTok ?? 0) + (s.outTok ?? 0)
  return (
    <div className={'srow' + (sel ? ' sel' : '')} onClick={() => onClick(s.id)}>
      <div className="sname" style={{ paddingLeft: 14 + s.depth * 12 }}>
        <span className={'kind k-' + s.kind}>{KIND_LABEL[s.kind]}</span>
        <span className="slabel">{s.name}{s.status === 'error' && <span className="errchip"> ERR</span>}</span>
      </div>
      <div className="wf"><div className={'bar ' + barcls} style={{ left: left + '%', width: w + '%' }} /></div>
      <div className="scost">{tot ? (tot / 1000 >= 1 ? (tot / 1000).toFixed(1) + 'k' : tot) : '·'}</div>
      <div className="sdur">{fmtDur(s.durMs)}</div>
    </div>
  )
}
