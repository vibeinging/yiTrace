import { useInfiniteQuery, useQuery } from '@tanstack/react-query'
import { api } from '../api'

/** 会话列表：游标无限分页。滚到底自动拉下一页（千会话只拉看过的那几页）。 */
export function useSessions(filter: string) {
  return useInfiniteQuery({
    queryKey: ['sessions', filter],
    queryFn: ({ pageParam }) => api.listSessions({ cursor: pageParam, limit: 50, filter }),
    initialPageParam: null as string | null,
    getNextPageParam: (last) => last.nextCursor,
  })
}

/** 一个会话的轮次：展开多轮会话时才拉。 */
export function useTurns(sessionId: string | null, enabled: boolean) {
  return useQuery({
    queryKey: ['turns', sessionId],
    queryFn: () => api.listTurns(sessionId!),
    enabled: enabled && !!sessionId,
    staleTime: 60_000,
  })
}

/** 一条 trace 的 span（瀑布）：选中才拉。 */
export function useTrace(traceId: string | null) {
  return useQuery({
    queryKey: ['trace', traceId],
    queryFn: () => api.getTrace(traceId!),
    enabled: !!traceId,
    staleTime: 60_000,
  })
}

/** span 大字段：选中某个 span 才拉（晚物化）。 */
export function useSpanDetail(traceId: string | null, spanId: string | null) {
  return useQuery({
    queryKey: ['span', traceId, spanId],
    queryFn: () => api.getSpanDetail(traceId!, spanId!),
    enabled: !!traceId && !!spanId,
    staleTime: 60_000,
  })
}

/** 步骤流：切到步骤流视图才拉（含每步输入/输出文本）。 */
export function useSteps(traceId: string | null, enabled: boolean) {
  return useQuery({
    queryKey: ['steps', traceId],
    queryFn: () => api.getSteps(traceId!),
    enabled: enabled && !!traceId,
    staleTime: 60_000,
  })
}

/** 语义检索：提交查询（回车）才拉。 */
export function useSearch(query: string) {
  return useQuery({
    queryKey: ['search', query],
    queryFn: () => api.searchSpans(query, 50),
    enabled: !!query,
    staleTime: 30_000,
  })
}
