// ───────────────────────── 评测数据集（Datasets） ─────────────────────────

/// 数据集的一条样本 = 采集时的 span 快照（含 input/output 文本、agent 名）+ 可选参考答案（人工标注）。
/// 存 span 快照而非引用:数据集是"冻结的回归基准",底层 trace 被合并/回收也不影响它。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatasetExample {
    pub span: FoldedSpan,
    /// 参考答案/期望输出（人工标注，可选）。给"对照参考答案打分"的 scorer 用。
    pub expected: Option<String>,
}

/// 一个命名评测数据集。eval 的燃料:把生产里的（失败/低分）trace 收集成固定集，反复回归重跑。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Dataset {
    pub name: String,
    pub examples: Vec<DatasetExample>,
}

/// 数据集摘要（列表视图）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatasetSummary {
    pub name: String,
    pub example_count: usize,
}

/// SDK 打点的线格式（对齐 Python / TS 的 `to_wire()` 字段）。
///
/// 摄入端据 `(ext_span_id, seq, event_type_tag)` **自己重算 event_id** —— 契约是这三个身份字段，
/// 不信任 SDK 传来的 event_id（SDK 算的与引擎一致是为了客户端去重/调试，引擎以自己算的为准）。
pub struct WireRecord {
    pub trace_id: u64,
    pub span_id: u64,
    pub ts: i64,
    pub seq: u64,
    pub event_type_tag: u8,
    pub ext_span_id: String,
    pub parent_span_id: Option<u64>,
    pub status: Option<u8>,
    pub duration_ns: Option<u64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub session_id: Option<u64>,
    pub tenant_id: Option<u64>,
    pub external_trace_id: Option<String>,
    pub external_span_id: Option<String>,
    pub external_parent_span_id: Option<String>,
    pub external_session_id: Option<String>,
    pub agent_name: Option<String>,
    pub tool_name: Option<String>,
    pub model: Option<String>,
    pub input_text: Option<String>,
    pub output_text: Option<String>,
    pub logs: Vec<String>,
    pub attrs: BTreeMap<String, String>,
}

impl WireRecord {
    fn into_wal_record(self) -> WalRecord {
        WalRecord {
            trace_id: self.trace_id,
            span_id: self.span_id,
            ts: self.ts,
            identity: EventIdentity {
                ext_span_id: self.ext_span_id,
                seq: self.seq,
                event_type: EventType::from_tag(self.event_type_tag),
            },
            fields: SpanFields {
                status: self.status,
                duration_ns: self.duration_ns,
                parent_span_id: self.parent_span_id,
                input_tokens: self.input_tokens,
                output_tokens: self.output_tokens,
                session_id: self.session_id,
                tenant_id: self.tenant_id,
                external_trace_id: self.external_trace_id,
                external_span_id: self.external_span_id,
                external_parent_span_id: self.external_parent_span_id,
                external_session_id: self.external_session_id,
                agent_name: self.agent_name,
                tool_name: self.tool_name,
                model: self.model,
                input_text: self.input_text,
                output_text: self.output_text,
                eval_score: None, // 分数由 scorer 事后算、走 upgrade 补写，不从线上摄入
                eval_label: None,
                logs: self.logs,
                attrs: self.attrs,
            },
        }
    }
}
