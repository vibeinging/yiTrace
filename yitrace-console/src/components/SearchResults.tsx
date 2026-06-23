import { useSearch } from '../hooks/queries'

// 语义检索结果面板（左栏）：中文 BM25 召回命中的 span，按分数排。点结果跳到该 trace+span。
export function SearchResults({
  query,
  selectedTrace,
  onSelect,
  onClear,
}: {
  query: string
  selectedTrace: string | null
  onSelect: (traceId: string, spanId: string) => void
  onClear: () => void
}) {
  const { data: hits, isLoading } = useSearch(query)
  return (
    <div className="pane list">
      <div className="ph">
        <span>搜索结果 <span className="hlq">「{query}」</span></span>
        <span className="clearbtn" onClick={onClear} title="返回会话列表">✕ 返回会话</span>
      </div>
      <div className="vlist">
        {isLoading ? (
          <div className="rowloading">语义检索中…</div>
        ) : !hits || hits.length === 0 ? (
          <div className="empty">无命中。换个词试试（中文 BM25 召回）。</div>
        ) : (
          hits.map((h) => (
            <div
              key={h.traceId + h.spanId}
              className={'hititem' + (selectedTrace === h.traceId ? ' sel' : '')}
              onClick={() => onSelect(h.traceId, h.spanId)}
            >
              <span className="score">{h.score.toFixed(2)}</span>
              <div className="hitbody">
                <div className="hitname">
                  <span className={'dot ' + h.status} />
                  {h.agentName ?? '(span)'}
                </div>
                {h.snippet && <div className="hitsnip">{h.snippet}</div>}
              </div>
            </div>
          ))
        )}
      </div>
    </div>
  )
}
