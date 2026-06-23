import { useSteps } from '../hooks/queries'

const fmtDur = (ms: number) => (ms >= 1000 ? (ms / 1000).toFixed(2) + 's' : ms + 'ms')
const KIND_LABEL: Record<string, string> = { llm: 'LLM', tool: 'TOOL', chain: 'CHAIN', retriever: 'RETR', agent: 'AGENT', other: '·' }

// 步骤流：每一步「输入 → 输出」并排卡片。切到此视图才拉文本（与瀑布的晚物化分开）。
export function StepStream({ traceId, active }: { traceId: string | null; active: boolean }) {
  const { data: steps, isLoading } = useSteps(traceId, active)
  if (isLoading) return <div className="spin">加载步骤…</div>
  if (!steps) return null
  return (
    <div className="viewscroll">
      <div className="stream">
        {steps.map((s) => {
          const tot = s.inTok + s.outTok
          return (
            <div className="scard" key={s.id}>
              <span className={'sdot k-' + s.kind} />
              <div className="shd">
                <span className={'kind k-' + s.kind}>{KIND_LABEL[s.kind] ?? s.kind}</span>
                <span className="snm">{s.name}</span>
                <span className="smeta">{fmtDur(s.durMs)}{tot ? ` · ${tot} tok` : ''}{s.model ? ` · ${s.model}` : ''}{s.status === 'error' ? ' · 出错' : ''}</span>
              </div>
              <div className="iorow">
                <div className="iobox in">
                  <div className="cap">输入</div>
                  {s.input ?? '—'}
                </div>
                <div className="ioarrow">→</div>
                <div className={'iobox ' + (s.status === 'error' ? 'err' : 'out')}>
                  <div className="cap">{s.status === 'error' ? '错误' : '输出'}</div>
                  {s.output ?? '—'}
                </div>
              </div>
            </div>
          )
        })}
      </div>
    </div>
  )
}
