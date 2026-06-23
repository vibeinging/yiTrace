import { useEffect, useState } from 'react'
import { SessionList } from './components/SessionList'
import { Waterfall } from './components/Waterfall'
import { SpanDetail } from './components/SpanDetail'

export default function App() {
  const [filter, setFilter] = useState('')
  const [q, setQ] = useState('')
  const [traceId, setTraceId] = useState<string | null>(null)
  const [spanId, setSpanId] = useState<string | null>(null)

  // 搜索防抖：输入停 250ms 再下推到列表查询（真实场景走服务端过滤）。
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
          <input placeholder="搜索会话标题 / session id…" value={q} onChange={(e) => setQ(e.target.value)} />
        </div>
        <div className="ctx">
          <span className="pill">租户 <b>招商银行·风控</b></span>
          <span className="pill">项目 <b>data-agent</b></span>
        </div>
      </div>
      <div className="main">
        <SessionList
          selectedTrace={traceId}
          filter={filter}
          onSelect={(id) => { setTraceId(id); setSpanId(null) }}
        />
        <Waterfall traceId={traceId} selectedSpan={spanId} onSelectSpan={setSpanId} />
        <SpanDetail traceId={traceId} spanId={spanId} />
      </div>
    </div>
  )
}
