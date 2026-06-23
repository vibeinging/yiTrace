import { useEffect, useState } from 'react'
import { SessionList } from './components/SessionList'
import { SearchResults } from './components/SearchResults'
import { Waterfall } from './components/Waterfall'
import { SpanDetail } from './components/SpanDetail'

export default function App() {
  const [filter, setFilter] = useState('')
  const [q, setQ] = useState('')
  const [search, setSearch] = useState('') // 已提交的语义检索词（回车触发）
  const [traceId, setTraceId] = useState<string | null>(null)
  const [spanId, setSpanId] = useState<string | null>(null)

  // 输入停 250ms 下推为会话标题过滤（即时、轻量）；回车则跑语义检索。
  useEffect(() => {
    const id = setTimeout(() => setFilter(q.trim()), 250)
    return () => clearTimeout(id)
  }, [q])

  return (
    <div className="app">
      <div className="topbar">
        <div className="logo"><b>yiTrace</b><small>控制台</small></div>
        <div className="search">
          <span className="ic">🔍</span>
          <input
            placeholder="筛会话标题；回车做中文语义检索…"
            value={q}
            onChange={(e) => setQ(e.target.value)}
            onKeyDown={(e) => { if (e.key === 'Enter') setSearch(q.trim()) }}
          />
          <span className="searchbadge">语义召回</span>
        </div>
        <div className="ctx">
          <span className="pill">租户 <b>招商银行·风控</b></span>
          <span className="pill">项目 <b>data-agent</b></span>
        </div>
      </div>
      <div className="main">
        {search ? (
          <SearchResults
            query={search}
            selectedTrace={traceId}
            onSelect={(tid, sid) => { setTraceId(tid); setSpanId(sid) }}
            onClear={() => { setSearch(''); setQ('') }}
          />
        ) : (
          <SessionList
            selectedTrace={traceId}
            filter={filter}
            onSelect={(id) => { setTraceId(id); setSpanId(null) }}
          />
        )}
        <Waterfall traceId={traceId} selectedSpan={spanId} onSelectSpan={setSpanId} />
        <SpanDetail traceId={traceId} spanId={spanId} />
      </div>
    </div>
  )
}
