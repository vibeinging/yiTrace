import { useTrace, useSpanDetail } from '../hooks/queries'

const fmtDur = (ms: number) => (ms >= 1000 ? (ms / 1000).toFixed(2) + 's' : ms + 'ms')

export function SpanDetail({ traceId, spanId }: { traceId: string | null; spanId: string | null }) {
  const { data: trace } = useTrace(traceId)
  const span = trace?.spans.find((s) => s.id === spanId)
  const { data: detail, isLoading } = useSpanDetail(traceId, spanId)

  return (
    <div className="pane detail">
      <div className="ph">Span 详情 <small>大字段晚物化</small></div>
      <div className="dwrap">
        {!span ? (
          <div className="empty">选中一个 span 看输入 / 输出</div>
        ) : (
          <>
            <div className="dsec">
              <div className="kv"><span className="k">名称</span><span className="v">{span.name}</span></div>
              <div className="kv"><span className="k">类型</span><span className="v">{span.kind}</span></div>
              <div className="kv"><span className="k">状态</span><span className="v">{span.status}</span></div>
              <div className="kv"><span className="k">耗时</span><span className="v">{fmtDur(span.durMs)}</span></div>
              <div className="kv"><span className="k">成本</span><span className="v">${span.cost.toFixed(3)}</span></div>
              {span.model && <div className="kv"><span className="k">模型</span><span className="v">{span.model}</span></div>}
              {span.inTok != null && <div className="kv"><span className="k">Token</span><span className="v">{span.inTok} → {span.outTok}</span></div>}
            </div>
            <div className="dsec">
              <h4>输入</h4>
              <div className="io in">{isLoading ? '加载中…' : detail?.input ?? '—'}</div>
            </div>
            <div className="dsec">
              <h4>输出</h4>
              <div className="io out">{isLoading ? '加载中…' : detail?.output ?? '—'}</div>
            </div>
          </>
        )}
      </div>
    </div>
  )
}
