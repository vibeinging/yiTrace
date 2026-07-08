#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AnnotationTarget {
    Trace,
    Span,
}

impl AnnotationTarget {
    pub fn as_str(self) -> &'static str {
        match self {
            AnnotationTarget::Trace => "trace",
            AnnotationTarget::Span => "span",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "trace" => Some(AnnotationTarget::Trace),
            "span" => Some(AnnotationTarget::Span),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AnnotationStatus {
    Active,
    Resolved,
    Rejected,
    Deleted,
}

impl AnnotationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            AnnotationStatus::Active => "active",
            AnnotationStatus::Resolved => "resolved",
            AnnotationStatus::Rejected => "rejected",
            AnnotationStatus::Deleted => "deleted",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "active" | "open" => Some(AnnotationStatus::Active),
            "resolved" | "accepted" | "done" => Some(AnnotationStatus::Resolved),
            "rejected" | "dismissed" => Some(AnnotationStatus::Rejected),
            "deleted" | "removed" | "archived" => Some(AnnotationStatus::Deleted),
            _ => None,
        }
    }

    fn code(self) -> u8 {
        match self {
            AnnotationStatus::Active => 0,
            AnnotationStatus::Resolved => 1,
            AnnotationStatus::Rejected => 2,
            AnnotationStatus::Deleted => 3,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(AnnotationStatus::Active),
            1 => Some(AnnotationStatus::Resolved),
            2 => Some(AnnotationStatus::Rejected),
            3 => Some(AnnotationStatus::Deleted),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceAnnotation {
    pub annotation_id: u64,
    pub tenant_id: Option<u64>,
    pub target: AnnotationTarget,
    pub trace_id: u64,
    pub span_id: Option<u64>,
    pub external_trace_id: Option<String>,
    pub external_span_id: Option<String>,
    pub label: String,
    pub score: Option<u32>,
    pub reason: Option<String>,
    pub source: Option<String>,
    pub created_at_ns: u64,
    pub updated_at_ns: u64,
    pub status: AnnotationStatus,
    pub reviewer: Option<String>,
    pub attrs: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NewTraceAnnotation {
    pub target: Option<AnnotationTarget>,
    pub trace_id: u64,
    pub span_id: Option<u64>,
    pub external_trace_id: Option<String>,
    pub external_span_id: Option<String>,
    pub label: String,
    pub score: Option<u32>,
    pub reason: Option<String>,
    pub source: Option<String>,
    pub status: Option<AnnotationStatus>,
    pub reviewer: Option<String>,
    pub attrs: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UpdateTraceAnnotation {
    pub label: Option<String>,
    pub score: Option<Option<u32>>,
    pub reason: Option<Option<String>>,
    pub source: Option<Option<String>>,
    pub status: Option<AnnotationStatus>,
    pub reviewer: Option<Option<String>>,
    pub attrs: Option<BTreeMap<String, String>>,
    pub merge_attrs: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TraceAnnotationFilter {
    pub tenant_id: Option<u64>,
    pub target: Option<AnnotationTarget>,
    pub trace_id: Option<u64>,
    pub span_id: Option<u64>,
    pub label: Option<String>,
    pub source: Option<String>,
    pub status: Option<AnnotationStatus>,
    pub include_deleted: bool,
    pub attrs: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatasetAssociation {
    pub association_id: u64,
    pub tenant_id: Option<u64>,
    pub dataset_id: String,
    pub item_id: String,
    pub trace_id: u64,
    pub span_id: Option<u64>,
    pub external_trace_id: Option<String>,
    pub external_span_id: Option<String>,
    pub snapshot_id: Option<String>,
    pub snapshot_hash: Option<String>,
    pub eval_run_id: Option<String>,
    pub split: Option<String>,
    pub label: Option<String>,
    pub score: Option<u32>,
    pub created_at_ns: u64,
    pub attrs: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NewDatasetAssociation {
    pub dataset_id: String,
    pub item_id: String,
    pub trace_id: u64,
    pub span_id: Option<u64>,
    pub external_trace_id: Option<String>,
    pub external_span_id: Option<String>,
    pub snapshot_id: Option<String>,
    pub snapshot_hash: Option<String>,
    pub eval_run_id: Option<String>,
    pub split: Option<String>,
    pub label: Option<String>,
    pub score: Option<u32>,
    pub attrs: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DatasetAssociationFilter {
    pub tenant_id: Option<u64>,
    pub dataset_id: Option<String>,
    pub item_id: Option<String>,
    pub trace_id: Option<u64>,
    pub span_id: Option<u64>,
    pub eval_run_id: Option<String>,
    pub split: Option<String>,
    pub label: Option<String>,
    pub attrs: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionAuditRecord {
    pub audit_id: u64,
    pub tenant_id: Option<u64>,
    pub created_at_ns: u64,
    pub source: Option<String>,
    pub reason: Option<String>,
    pub delete_before_ts: Option<i64>,
    pub query_json: String,
    pub protect_annotations: bool,
    pub protect_dataset_associations: bool,
    pub protect_snapshots: bool,
    pub protect_eval_links: bool,
    pub protect_path_memory: bool,
    pub compact_requested: bool,
    pub compact_reclaim: bool,
    pub candidate_trace_count: u64,
    pub protected_trace_count: u64,
    pub deletable_trace_count: u64,
    pub requested_trace_count: u64,
    pub deleted_trace_count: u64,
    pub deleted_segment_row_count: u64,
    pub skipped_live_trace_count: u64,
    pub compacted_segment_count: u64,
    pub reclaimed_segment_count: u64,
    pub dropped_deleted_row_count: u64,
    pub rewritten_live_row_count: u64,
    pub deletable_trace_ids: Vec<u64>,
    pub deleted_trace_ids: Vec<u64>,
    pub skipped_live_trace_ids: Vec<u64>,
    pub trace_id_sample_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NewRetentionAuditRecord {
    pub source: Option<String>,
    pub reason: Option<String>,
    pub delete_before_ts: Option<i64>,
    pub query_json: String,
    pub protect_annotations: bool,
    pub protect_dataset_associations: bool,
    pub protect_snapshots: bool,
    pub protect_eval_links: bool,
    pub protect_path_memory: bool,
    pub compact_requested: bool,
    pub compact_reclaim: bool,
    pub candidate_trace_count: u64,
    pub protected_trace_count: u64,
    pub deletable_trace_count: u64,
    pub requested_trace_count: u64,
    pub deleted_trace_count: u64,
    pub deleted_segment_row_count: u64,
    pub skipped_live_trace_count: u64,
    pub compacted_segment_count: u64,
    pub reclaimed_segment_count: u64,
    pub dropped_deleted_row_count: u64,
    pub rewritten_live_row_count: u64,
    pub deletable_trace_ids: Vec<u64>,
    pub deleted_trace_ids: Vec<u64>,
    pub skipped_live_trace_ids: Vec<u64>,
    pub trace_id_sample_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RetentionAuditFilter {
    pub tenant_id: Option<u64>,
    pub audit_id: Option<u64>,
    pub source: Option<String>,
    pub min_created_at_ns: Option<u64>,
    pub max_created_at_ns: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionPolicy {
    pub policy_id: u64,
    pub tenant_id: Option<u64>,
    pub name: String,
    pub enabled: bool,
    pub created_at_ns: u64,
    pub updated_at_ns: u64,
    pub last_run_at_ns: Option<u64>,
    pub next_run_at_ns: Option<u64>,
    pub interval_ns: u64,
    pub source: Option<String>,
    pub reason: Option<String>,
    pub query_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NewRetentionPolicy {
    pub name: String,
    pub enabled: bool,
    pub next_run_at_ns: Option<u64>,
    pub interval_ns: u64,
    pub source: Option<String>,
    pub reason: Option<String>,
    pub query_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RetentionPolicyFilter {
    pub tenant_id: Option<u64>,
    pub policy_id: Option<u64>,
    pub name: Option<String>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone)]
pub(crate) struct MetadataState {
    pub annotations: Vec<TraceAnnotation>,
    pub dataset_associations: Vec<DatasetAssociation>,
    pub retention_audits: Vec<RetentionAuditRecord>,
    pub retention_policies: Vec<RetentionPolicy>,
    pub next_annotation_id: u64,
    pub next_dataset_association_id: u64,
    pub next_retention_audit_id: u64,
    pub next_retention_policy_id: u64,
}

impl Default for MetadataState {
    fn default() -> Self {
        Self {
            annotations: Vec::new(),
            dataset_associations: Vec::new(),
            retention_audits: Vec::new(),
            retention_policies: Vec::new(),
            next_annotation_id: 1,
            next_dataset_association_id: 1,
            next_retention_audit_id: 1,
            next_retention_policy_id: 1,
        }
    }
}
