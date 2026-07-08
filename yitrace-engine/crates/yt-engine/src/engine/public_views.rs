#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReadPlanStats {
    pub source: Option<String>,
    pub used_filter_index: bool,
    pub candidate_span_keys: Option<usize>,
    pub scanned_segments: usize,
    pub matched_spans: usize,
    pub fallback_reason: Option<String>,
    pub unsupported_attr_keys: Vec<String>,
    pub trace_fetch_source: Option<String>,
    pub trace_fetch_span_count: Option<usize>,
    pub trace_fetch_fallback_reason: Option<String>,
}

/// 一条 trace 的摘要（web 控制台列表视图用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceSummary {
    pub trace_id: u64,
    pub external_trace_id: Option<String>,
    pub span_count: usize,
    pub total_duration_ns: u64,
    pub max_duration_ns: u64,
    /// 状态非 0 的 span 数（报错）。
    pub error_count: usize,
    /// 全 trace 输入/输出 token 汇总（成本指标）。
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
}

/// retention apply 的物理删除结果。
///
/// 只软删除已经 flush 到 segment 的行；仍在 MemTable/WAL tail 的热 trace 会整条跳过。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RetentionDeleteResult {
    pub requested_trace_count: usize,
    pub deleted_trace_count: usize,
    pub deleted_segment_row_count: usize,
    pub skipped_live_trace_count: usize,
    pub deleted_trace_ids: Vec<u64>,
    pub skipped_live_trace_ids: Vec<u64>,
}

/// retention 后可选 compaction 的结果。
///
/// compaction 只是把 deletion vector 物化进新段；真正释放磁盘还要看快照读者和 buffer pin 水位。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RetentionCompactResult {
    pub before_live_segment_count: usize,
    pub after_live_segment_count: usize,
    pub before_dead_segment_count: usize,
    pub after_dead_segment_count: usize,
    pub selected_segment_count: usize,
    pub compacted_segment_count: usize,
    pub reclaimed_segment_count: usize,
    pub dropped_deleted_row_count: usize,
    pub rewritten_live_row_count: usize,
    pub selected_segment_ids: Vec<u64>,
}

/// trace 树的一个节点 = 折叠出的 span + 它的孩子 span_id。
#[derive(Debug, Clone)]
pub struct TraceNode {
    pub span: FoldedSpan,
    pub children: Vec<u64>,
}

/// 一条 trace 的父子树（树+瀑布视图直接渲染）。
#[derive(Debug, Clone)]
pub struct TraceTree {
    pub trace_id: u64,
    /// 无父（或父不在本 trace 内）的 span_id，升序。
    pub roots: Vec<u64>,
    pub nodes: BTreeMap<u64, TraceNode>,
}

impl TraceTree {
    /// 深度优先顺序的 span_id（瀑布视图按此从上到下排）。孩子按 span_id 升序。
    pub fn dfs_order(&self) -> Vec<u64> {
        let mut out = Vec::new();
        let mut stack: Vec<u64> = self.roots.iter().rev().copied().collect();
        while let Some(id) = stack.pop() {
            out.push(id);
            if let Some(n) = self.nodes.get(&id) {
                for &c in n.children.iter().rev() {
                    stack.push(c);
                }
            }
        }
        out
    }
}

/// agent 执行图里一个节点的角色类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorKind {
    /// 有 agent_name 的 span。
    Agent,
    /// 无 agent_name 但有 tool_name 的 span。
    Tool,
    /// 两者都无（用 span:<id> 占位）。
    Other,
}

/// agent 执行图的一个节点 = 一个"角色"（agent / 工具），带聚合统计。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentGraphNode {
    pub actor: String,
    pub kind: ActorKind,
    pub span_count: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// agent 执行图的一条边 = 父 span 的角色"调用/移交给"子 span 的角色（聚合次数）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentGraphEdge {
    pub from: String,
    pub to: String,
    pub count: usize,
}

/// 一条 trace 的 agent 执行图（DAG）：谁调用了谁。
/// 把"span 父子树"按 agent/工具维度收拢成"角色调用图"——dogfood 自家 SuperAgent 最想看的视图。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentGraph {
    pub trace_id: u64,
    /// 按 actor 名升序。
    pub nodes: Vec<AgentGraphNode>,
    /// 按 (from, to) 升序。已剔除同角色自环（只留跨角色的调用/移交）。
    pub edges: Vec<AgentGraphEdge>,
}

/// 多轮对话里的**一轮** = 会话内的一条 trace，抽成「用户问 → agent 答」的对子 + 该轮统计。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionTurn {
    pub trace_id: u64,
    /// 轮次序号（0 起）。按 trace_id 升序定序 —— trace id 单调下发，是对话时间序的可靠代理
    /// （折叠后的 span 不保留 ts，故不按 ts 排）。
    pub turn_index: usize,
    /// 该轮输入：span_id 最小的、带 input_text 的 span（通常是编排根 span 上的提示词）。
    pub user_input: Option<String>,
    /// 该轮最终答复：span_id 最大的、带 output_text 的 span（最末一步的作答）。
    pub agent_output: Option<String>,
    /// 该轮参与的 agent（去重升序）。
    pub agents: Vec<String>,
    pub span_count: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// 该轮 status≠0 的 span 数（这一轮有没有出错）。
    pub error_count: usize,
    /// 该轮答复 span 的评测分（若已 eval 写回）。
    pub eval_score: Option<u32>,
}

/// 一个会话的**多轮对话流**（多轮会话视图直接渲染）：把会话内多条 trace 按时间序拼成对话。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionTimeline {
    pub session_id: u64,
    /// 按 turn_index 升序。
    pub turns: Vec<SessionTurn>,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
}

/// 控制台会话行（一次扫描聚合）。比 `SessionSummary` 多了标题/状态/首 trace，给前端列表直接用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleSession {
    pub session_id: u64,
    pub external_session_id: Option<String>,
    pub title: String,
    pub turn_count: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub has_error: bool,
    pub first_trace_id: u64,
}

/// 控制台瀑布的一行 span（kind/name/起始时刻为派生值，见 `console_trace_spans`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleSpan {
    pub span_id: u64,
    pub parent_span_id: Option<u64>,
    pub external_trace_id: Option<String>,
    pub external_span_id: Option<String>,
    pub external_parent_span_id: Option<String>,
    pub external_session_id: Option<String>,
    pub kind: &'static str,
    pub name: String,
    pub start_ns: u64,
    pub duration_ns: u64,
    pub has_error: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub model: Option<String>,
    pub input_text: Option<String>,
    pub output_text: Option<String>,
    pub attrs: BTreeMap<String, String>,
}

/// span 内的原始日志事件视图。它不是折叠后的 `logs` 字符串并集，而是保留事件顺序与 attrs 的明细，
/// 供 trace/span 详情页还原执行现场，避免业务侧把日志镜像进 `attrs.event_logs`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpanLogEvent {
    pub ts: i64,
    pub seq: u64,
    pub event_type: u8,
    pub event_id: u64,
    pub messages: Vec<String>,
    pub attrs: BTreeMap<String, String>,
}

/// 一个会话的摘要（多轮对话/agent 会话视图）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    pub session_id: u64,
    pub trace_count: usize,
    pub span_count: usize,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
}

/// 按 agent 的成本归因（per-agent 成本下钻）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCost {
    pub agent_name: String,
    pub span_count: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
}
