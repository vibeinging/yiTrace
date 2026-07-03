//! metadata.rs —— 业务侧轻量元数据：annotation、dataset 关联、golden path 候选和治理审计。
//!
//! 这层不参与 trace 折叠、WAL 重放和列式段生命周期；它是产品层需要的“小账本”：
//! 哪条 trace/span 被判定为什么、它对应哪个回归样本、哪条路径被确认可复用、哪次
//! retention 真正清理过数据。单独持久化可以避免污染承重 trace 格式。

use std::collections::BTreeMap;
use std::path::Path;

use crate::olog;

const MAGIC: u32 = 0x5954_4D44; // "YTMD"
const FORMAT_VER: u32 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    pub attrs: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TraceAnnotationFilter {
    pub tenant_id: Option<u64>,
    pub target: Option<AnnotationTarget>,
    pub trace_id: Option<u64>,
    pub span_id: Option<u64>,
    pub label: Option<String>,
    pub source: Option<String>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoldenPathStatus {
    Candidate,
    Confirmed,
    Rejected,
    Deprecated,
}

impl GoldenPathStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            GoldenPathStatus::Candidate => "candidate",
            GoldenPathStatus::Confirmed => "confirmed",
            GoldenPathStatus::Rejected => "rejected",
            GoldenPathStatus::Deprecated => "deprecated",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().replace(['_', '-'], "").as_str() {
            "candidate" | "pending" => Some(GoldenPathStatus::Candidate),
            "confirmed" | "active" | "accepted" => Some(GoldenPathStatus::Confirmed),
            "rejected" | "declined" => Some(GoldenPathStatus::Rejected),
            "deprecated" | "retired" | "disabled" => Some(GoldenPathStatus::Deprecated),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoldenPathCandidate {
    pub golden_path_id: u64,
    pub tenant_id: Option<u64>,
    pub task_fingerprint: String,
    pub trajectory_signature: String,
    pub source_trace_id: u64,
    pub external_source_trace_id: Option<String>,
    pub snapshot_id: Option<String>,
    pub snapshot_hash: Option<String>,
    pub status: GoldenPathStatus,
    pub score: Option<u32>,
    pub label: Option<String>,
    pub reason: Option<String>,
    pub source: Option<String>,
    pub created_at_ns: u64,
    pub updated_at_ns: u64,
    pub attrs: BTreeMap<String, String>,
    pub source_trajectory_steps: Vec<String>,
    pub evidence: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NewGoldenPathCandidate {
    pub task_fingerprint: String,
    pub trajectory_signature: String,
    pub source_trace_id: u64,
    pub external_source_trace_id: Option<String>,
    pub snapshot_id: Option<String>,
    pub snapshot_hash: Option<String>,
    pub status: Option<GoldenPathStatus>,
    pub score: Option<u32>,
    pub label: Option<String>,
    pub reason: Option<String>,
    pub source: Option<String>,
    pub attrs: BTreeMap<String, String>,
    pub source_trajectory_steps: Vec<String>,
    pub evidence: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GoldenPathFilter {
    pub tenant_id: Option<u64>,
    pub golden_path_id: Option<u64>,
    pub task_fingerprint: Option<String>,
    pub trajectory_signature: Option<String>,
    pub source_trace_id: Option<u64>,
    pub status: Option<GoldenPathStatus>,
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
    pub protect_golden_paths: bool,
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
    pub protect_golden_paths: bool,
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

#[derive(Debug, Clone, Default)]
pub(crate) struct MetadataState {
    pub annotations: Vec<TraceAnnotation>,
    pub dataset_associations: Vec<DatasetAssociation>,
    pub golden_paths: Vec<GoldenPathCandidate>,
    pub retention_audits: Vec<RetentionAuditRecord>,
    pub retention_policies: Vec<RetentionPolicy>,
    pub next_annotation_id: u64,
    pub next_dataset_association_id: u64,
    pub next_golden_path_id: u64,
    pub next_retention_audit_id: u64,
    pub next_retention_policy_id: u64,
}

// ───────────────────────── 字节读写 ─────────────────────────

fn put_u8(b: &mut Vec<u8>, v: u8) {
    b.push(v);
}
fn put_u32(b: &mut Vec<u8>, v: u32) {
    b.extend_from_slice(&v.to_le_bytes());
}
fn put_i64(b: &mut Vec<u8>, v: i64) {
    b.extend_from_slice(&v.to_le_bytes());
}
fn put_u64(b: &mut Vec<u8>, v: u64) {
    b.extend_from_slice(&v.to_le_bytes());
}
fn put_bool(b: &mut Vec<u8>, v: bool) {
    put_u8(b, if v { 1 } else { 0 });
}
fn put_opt_i64(b: &mut Vec<u8>, v: Option<i64>) {
    match v {
        Some(x) => {
            put_u8(b, 1);
            put_i64(b, x);
        }
        None => put_u8(b, 0),
    }
}
fn put_opt_u64(b: &mut Vec<u8>, v: Option<u64>) {
    match v {
        Some(x) => {
            put_u8(b, 1);
            put_u64(b, x);
        }
        None => put_u8(b, 0),
    }
}
fn put_opt_u32(b: &mut Vec<u8>, v: Option<u32>) {
    match v {
        Some(x) => {
            put_u8(b, 1);
            put_u32(b, x);
        }
        None => put_u8(b, 0),
    }
}
fn put_str(b: &mut Vec<u8>, s: &str) {
    put_u64(b, s.len() as u64);
    b.extend_from_slice(s.as_bytes());
}
fn put_opt_str(b: &mut Vec<u8>, s: Option<&str>) {
    match s {
        Some(v) => {
            put_u8(b, 1);
            put_str(b, v);
        }
        None => put_u8(b, 0),
    }
}
fn put_map(b: &mut Vec<u8>, m: &BTreeMap<String, String>) {
    put_u64(b, m.len() as u64);
    for (k, v) in m {
        put_str(b, k);
        put_str(b, v);
    }
}
fn put_str_vec(b: &mut Vec<u8>, items: &[String]) {
    put_u64(b, items.len() as u64);
    for item in items {
        put_str(b, item);
    }
}
fn put_u64_vec(b: &mut Vec<u8>, items: &[u64]) {
    put_u64(b, items.len() as u64);
    for item in items {
        put_u64(b, *item);
    }
}

struct Cur<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Cur<'a> {
    fn u8(&mut self) -> Option<u8> {
        let v = *self.b.get(self.i)?;
        self.i += 1;
        Some(v)
    }
    fn u32(&mut self) -> Option<u32> {
        let e = self.i + 4;
        let s = self.b.get(self.i..e)?;
        self.i = e;
        Some(u32::from_le_bytes(s.try_into().ok()?))
    }
    fn i64(&mut self) -> Option<i64> {
        let e = self.i + 8;
        let s = self.b.get(self.i..e)?;
        self.i = e;
        Some(i64::from_le_bytes(s.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        let e = self.i + 8;
        let s = self.b.get(self.i..e)?;
        self.i = e;
        Some(u64::from_le_bytes(s.try_into().ok()?))
    }
    fn bool(&mut self) -> Option<bool> {
        Some(self.u8()? != 0)
    }
    fn opt_i64(&mut self) -> Option<Option<i64>> {
        match self.u8()? {
            0 => Some(None),
            _ => Some(Some(self.i64()?)),
        }
    }
    fn opt_u64(&mut self) -> Option<Option<u64>> {
        match self.u8()? {
            0 => Some(None),
            _ => Some(Some(self.u64()?)),
        }
    }
    fn opt_u32(&mut self) -> Option<Option<u32>> {
        match self.u8()? {
            0 => Some(None),
            _ => Some(Some(self.u32()?)),
        }
    }
    fn str(&mut self) -> Option<String> {
        let n = self.u64()? as usize;
        let e = self.i.checked_add(n)?;
        let s = self.b.get(self.i..e)?;
        self.i = e;
        String::from_utf8(s.to_vec()).ok()
    }
    fn opt_str(&mut self) -> Option<Option<String>> {
        match self.u8()? {
            0 => Some(None),
            _ => Some(Some(self.str()?)),
        }
    }
    fn map(&mut self) -> Option<BTreeMap<String, String>> {
        let n = self.u64()? as usize;
        let mut out = BTreeMap::new();
        for _ in 0..n {
            out.insert(self.str()?, self.str()?);
        }
        Some(out)
    }
    fn str_vec(&mut self) -> Option<Vec<String>> {
        let n = self.u64()? as usize;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            out.push(self.str()?);
        }
        Some(out)
    }
    fn u64_vec(&mut self) -> Option<Vec<u64>> {
        let n = self.u64()? as usize;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            out.push(self.u64()?);
        }
        Some(out)
    }
}

// ───────────────────────── 编码 ─────────────────────────

pub(crate) fn encode(state: &MetadataState) -> Vec<u8> {
    let mut b = Vec::new();
    put_u32(&mut b, MAGIC);
    put_u32(&mut b, FORMAT_VER);
    put_u64(&mut b, state.next_annotation_id);
    put_u64(&mut b, state.next_dataset_association_id);
    put_u64(&mut b, state.next_golden_path_id);
    put_u64(&mut b, state.next_retention_audit_id);
    put_u64(&mut b, state.next_retention_policy_id);
    put_u64(&mut b, state.annotations.len() as u64);
    for a in &state.annotations {
        put_u64(&mut b, a.annotation_id);
        put_opt_u64(&mut b, a.tenant_id);
        put_u8(
            &mut b,
            match a.target {
                AnnotationTarget::Trace => 0,
                AnnotationTarget::Span => 1,
            },
        );
        put_u64(&mut b, a.trace_id);
        put_opt_u64(&mut b, a.span_id);
        put_opt_str(&mut b, a.external_trace_id.as_deref());
        put_opt_str(&mut b, a.external_span_id.as_deref());
        put_str(&mut b, &a.label);
        put_opt_u32(&mut b, a.score);
        put_opt_str(&mut b, a.reason.as_deref());
        put_opt_str(&mut b, a.source.as_deref());
        put_u64(&mut b, a.created_at_ns);
        put_map(&mut b, &a.attrs);
    }
    put_u64(&mut b, state.dataset_associations.len() as u64);
    for d in &state.dataset_associations {
        put_u64(&mut b, d.association_id);
        put_opt_u64(&mut b, d.tenant_id);
        put_str(&mut b, &d.dataset_id);
        put_str(&mut b, &d.item_id);
        put_u64(&mut b, d.trace_id);
        put_opt_u64(&mut b, d.span_id);
        put_opt_str(&mut b, d.external_trace_id.as_deref());
        put_opt_str(&mut b, d.external_span_id.as_deref());
        put_opt_str(&mut b, d.snapshot_id.as_deref());
        put_opt_str(&mut b, d.snapshot_hash.as_deref());
        put_opt_str(&mut b, d.eval_run_id.as_deref());
        put_opt_str(&mut b, d.split.as_deref());
        put_opt_str(&mut b, d.label.as_deref());
        put_opt_u32(&mut b, d.score);
        put_u64(&mut b, d.created_at_ns);
        put_map(&mut b, &d.attrs);
    }
    put_u64(&mut b, state.golden_paths.len() as u64);
    for g in &state.golden_paths {
        put_u64(&mut b, g.golden_path_id);
        put_opt_u64(&mut b, g.tenant_id);
        put_str(&mut b, &g.task_fingerprint);
        put_str(&mut b, &g.trajectory_signature);
        put_u64(&mut b, g.source_trace_id);
        put_opt_str(&mut b, g.external_source_trace_id.as_deref());
        put_opt_str(&mut b, g.snapshot_id.as_deref());
        put_opt_str(&mut b, g.snapshot_hash.as_deref());
        put_u8(
            &mut b,
            match g.status {
                GoldenPathStatus::Candidate => 0,
                GoldenPathStatus::Confirmed => 1,
                GoldenPathStatus::Rejected => 2,
                GoldenPathStatus::Deprecated => 3,
            },
        );
        put_opt_u32(&mut b, g.score);
        put_opt_str(&mut b, g.label.as_deref());
        put_opt_str(&mut b, g.reason.as_deref());
        put_opt_str(&mut b, g.source.as_deref());
        put_u64(&mut b, g.created_at_ns);
        put_u64(&mut b, g.updated_at_ns);
        put_map(&mut b, &g.attrs);
        put_str_vec(&mut b, &g.source_trajectory_steps);
        put_map(&mut b, &g.evidence);
    }
    put_u64(&mut b, state.retention_audits.len() as u64);
    for a in &state.retention_audits {
        put_u64(&mut b, a.audit_id);
        put_opt_u64(&mut b, a.tenant_id);
        put_u64(&mut b, a.created_at_ns);
        put_opt_str(&mut b, a.source.as_deref());
        put_opt_str(&mut b, a.reason.as_deref());
        put_opt_i64(&mut b, a.delete_before_ts);
        put_str(&mut b, &a.query_json);
        put_bool(&mut b, a.protect_golden_paths);
        put_bool(&mut b, a.protect_annotations);
        put_bool(&mut b, a.protect_dataset_associations);
        put_bool(&mut b, a.protect_snapshots);
        put_bool(&mut b, a.protect_eval_links);
        put_bool(&mut b, a.protect_path_memory);
        put_bool(&mut b, a.compact_requested);
        put_bool(&mut b, a.compact_reclaim);
        put_u64(&mut b, a.candidate_trace_count);
        put_u64(&mut b, a.protected_trace_count);
        put_u64(&mut b, a.deletable_trace_count);
        put_u64(&mut b, a.requested_trace_count);
        put_u64(&mut b, a.deleted_trace_count);
        put_u64(&mut b, a.deleted_segment_row_count);
        put_u64(&mut b, a.skipped_live_trace_count);
        put_u64(&mut b, a.compacted_segment_count);
        put_u64(&mut b, a.reclaimed_segment_count);
        put_u64(&mut b, a.dropped_deleted_row_count);
        put_u64(&mut b, a.rewritten_live_row_count);
        put_u64_vec(&mut b, &a.deletable_trace_ids);
        put_u64_vec(&mut b, &a.deleted_trace_ids);
        put_u64_vec(&mut b, &a.skipped_live_trace_ids);
        put_bool(&mut b, a.trace_id_sample_truncated);
    }
    put_u64(&mut b, state.retention_policies.len() as u64);
    for p in &state.retention_policies {
        put_u64(&mut b, p.policy_id);
        put_opt_u64(&mut b, p.tenant_id);
        put_str(&mut b, &p.name);
        put_bool(&mut b, p.enabled);
        put_u64(&mut b, p.created_at_ns);
        put_u64(&mut b, p.updated_at_ns);
        put_opt_u64(&mut b, p.last_run_at_ns);
        put_opt_u64(&mut b, p.next_run_at_ns);
        put_u64(&mut b, p.interval_ns);
        put_opt_str(&mut b, p.source.as_deref());
        put_opt_str(&mut b, p.reason.as_deref());
        put_str(&mut b, &p.query_json);
    }
    b
}

pub(crate) fn decode(bytes: &[u8]) -> Option<MetadataState> {
    let mut c = Cur { b: bytes, i: 0 };
    let magic = c.u32()?;
    let ver = c.u32()?;
    if magic != MAGIC {
        olog::log(
            olog::Level::Error,
            "metadata_decode",
            &[("reason", &"bad magic")],
        );
        return None;
    }
    if ver == 0 || ver > FORMAT_VER {
        olog::log(
            olog::Level::Error,
            "metadata_decode",
            &[
                ("reason", &"unsupported version"),
                ("found", &ver),
                ("supported", &FORMAT_VER),
            ],
        );
        return None;
    }
    let mut state = MetadataState {
        next_annotation_id: c.u64()?,
        next_dataset_association_id: c.u64()?,
        next_golden_path_id: if ver >= 2 { c.u64()? } else { 1 },
        next_retention_audit_id: if ver >= 4 { c.u64()? } else { 1 },
        next_retention_policy_id: if ver >= 5 { c.u64()? } else { 1 },
        ..Default::default()
    };
    let ann_n = c.u64()? as usize;
    for _ in 0..ann_n {
        let annotation_id = c.u64()?;
        let tenant_id = c.opt_u64()?;
        let target = match c.u8()? {
            0 => AnnotationTarget::Trace,
            1 => AnnotationTarget::Span,
            _ => return None,
        };
        state.annotations.push(TraceAnnotation {
            annotation_id,
            tenant_id,
            target,
            trace_id: c.u64()?,
            span_id: c.opt_u64()?,
            external_trace_id: c.opt_str()?,
            external_span_id: c.opt_str()?,
            label: c.str()?,
            score: c.opt_u32()?,
            reason: c.opt_str()?,
            source: c.opt_str()?,
            created_at_ns: c.u64()?,
            attrs: c.map()?,
        });
    }
    let assoc_n = c.u64()? as usize;
    for _ in 0..assoc_n {
        state.dataset_associations.push(DatasetAssociation {
            association_id: c.u64()?,
            tenant_id: c.opt_u64()?,
            dataset_id: c.str()?,
            item_id: c.str()?,
            trace_id: c.u64()?,
            span_id: c.opt_u64()?,
            external_trace_id: c.opt_str()?,
            external_span_id: c.opt_str()?,
            snapshot_id: c.opt_str()?,
            snapshot_hash: c.opt_str()?,
            eval_run_id: c.opt_str()?,
            split: c.opt_str()?,
            label: c.opt_str()?,
            score: c.opt_u32()?,
            created_at_ns: c.u64()?,
            attrs: c.map()?,
        });
    }
    if ver >= 2 {
        let gp_n = c.u64()? as usize;
        for _ in 0..gp_n {
            let golden_path_id = c.u64()?;
            let tenant_id = c.opt_u64()?;
            let task_fingerprint = c.str()?;
            let trajectory_signature = c.str()?;
            let source_trace_id = c.u64()?;
            let external_source_trace_id = c.opt_str()?;
            let snapshot_id = c.opt_str()?;
            let snapshot_hash = c.opt_str()?;
            let status = match c.u8()? {
                0 => GoldenPathStatus::Candidate,
                1 => GoldenPathStatus::Confirmed,
                2 => GoldenPathStatus::Rejected,
                3 => GoldenPathStatus::Deprecated,
                _ => return None,
            };
            let score = c.opt_u32()?;
            let label = c.opt_str()?;
            let reason = c.opt_str()?;
            let source = c.opt_str()?;
            let created_at_ns = c.u64()?;
            let updated_at_ns = c.u64()?;
            let attrs = c.map()?;
            let source_trajectory_steps = if ver >= 3 { c.str_vec()? } else { Vec::new() };
            let evidence = if ver >= 3 { c.map()? } else { BTreeMap::new() };
            state.golden_paths.push(GoldenPathCandidate {
                golden_path_id,
                tenant_id,
                task_fingerprint,
                trajectory_signature,
                source_trace_id,
                external_source_trace_id,
                snapshot_id,
                snapshot_hash,
                status,
                score,
                label,
                reason,
                source,
                created_at_ns,
                updated_at_ns,
                attrs,
                source_trajectory_steps,
                evidence,
            });
        }
    }
    if ver >= 4 {
        let audit_n = c.u64()? as usize;
        for _ in 0..audit_n {
            let audit_id = c.u64()?;
            let tenant_id = c.opt_u64()?;
            let created_at_ns = c.u64()?;
            let source = c.opt_str()?;
            let reason = c.opt_str()?;
            let delete_before_ts = c.opt_i64()?;
            let query_json = c.str()?;
            let protect_golden_paths = c.bool()?;
            let protect_annotations = c.bool()?;
            let protect_dataset_associations = c.bool()?;
            let (protect_snapshots, protect_eval_links, protect_path_memory) = if ver >= 6 {
                (c.bool()?, c.bool()?, c.bool()?)
            } else {
                (false, false, false)
            };
            state.retention_audits.push(RetentionAuditRecord {
                audit_id,
                tenant_id,
                created_at_ns,
                source,
                reason,
                delete_before_ts,
                query_json,
                protect_golden_paths,
                protect_annotations,
                protect_dataset_associations,
                protect_snapshots,
                protect_eval_links,
                protect_path_memory,
                compact_requested: c.bool()?,
                compact_reclaim: c.bool()?,
                candidate_trace_count: c.u64()?,
                protected_trace_count: c.u64()?,
                deletable_trace_count: c.u64()?,
                requested_trace_count: c.u64()?,
                deleted_trace_count: c.u64()?,
                deleted_segment_row_count: c.u64()?,
                skipped_live_trace_count: c.u64()?,
                compacted_segment_count: c.u64()?,
                reclaimed_segment_count: c.u64()?,
                dropped_deleted_row_count: c.u64()?,
                rewritten_live_row_count: c.u64()?,
                deletable_trace_ids: c.u64_vec()?,
                deleted_trace_ids: c.u64_vec()?,
                skipped_live_trace_ids: c.u64_vec()?,
                trace_id_sample_truncated: c.bool()?,
            });
        }
    }
    if ver >= 5 {
        let policy_n = c.u64()? as usize;
        for _ in 0..policy_n {
            state.retention_policies.push(RetentionPolicy {
                policy_id: c.u64()?,
                tenant_id: c.opt_u64()?,
                name: c.str()?,
                enabled: c.bool()?,
                created_at_ns: c.u64()?,
                updated_at_ns: c.u64()?,
                last_run_at_ns: c.opt_u64()?,
                next_run_at_ns: c.opt_u64()?,
                interval_ns: c.u64()?,
                source: c.opt_str()?,
                reason: c.opt_str()?,
                query_json: c.str()?,
            });
        }
    }
    state.next_annotation_id = state.next_annotation_id.max(
        state
            .annotations
            .iter()
            .map(|a| a.annotation_id)
            .max()
            .unwrap_or(0)
            + 1,
    );
    state.next_dataset_association_id = state.next_dataset_association_id.max(
        state
            .dataset_associations
            .iter()
            .map(|a| a.association_id)
            .max()
            .unwrap_or(0)
            + 1,
    );
    state.next_golden_path_id = state.next_golden_path_id.max(
        state
            .golden_paths
            .iter()
            .map(|g| g.golden_path_id)
            .max()
            .unwrap_or(0)
            + 1,
    );
    state.next_retention_audit_id = state.next_retention_audit_id.max(
        state
            .retention_audits
            .iter()
            .map(|a| a.audit_id)
            .max()
            .unwrap_or(0)
            + 1,
    );
    state.next_retention_policy_id = state.next_retention_policy_id.max(
        state
            .retention_policies
            .iter()
            .map(|p| p.policy_id)
            .max()
            .unwrap_or(0)
            + 1,
    );
    Some(state)
}

pub(crate) fn save(path: impl AsRef<Path>, state: &MetadataState) -> std::io::Result<()> {
    use std::io::Write;
    let payload = encode(state);
    let mut buf = Vec::with_capacity(payload.len() + 4);
    buf.extend_from_slice(&yt_wal::crc32(&payload).to_le_bytes());
    buf.extend_from_slice(&payload);

    let path = path.as_ref();
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp)?;
        f.write_all(&buf)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

pub(crate) fn load(path: impl AsRef<Path>) -> Option<MetadataState> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() < 4 {
        return None;
    }
    let crc = u32::from_le_bytes(bytes[0..4].try_into().ok()?);
    let payload = &bytes[4..];
    if crc != yt_wal::crc32(payload) {
        return None;
    }
    decode(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_roundtrip() {
        let mut attrs = BTreeMap::new();
        attrs.insert("project_id".to_string(), "\"agentic-data\"".to_string());
        let state = MetadataState {
            annotations: vec![TraceAnnotation {
                annotation_id: 7,
                tenant_id: Some(1),
                target: AnnotationTarget::Span,
                trace_id: 10,
                span_id: Some(2),
                external_trace_id: Some("run-a".to_string()),
                external_span_id: Some("span-a".to_string()),
                label: "best_path".to_string(),
                score: Some(920),
                reason: Some("人工确认".to_string()),
                source: Some("human".to_string()),
                created_at_ns: 123,
                attrs: attrs.clone(),
            }],
            dataset_associations: vec![DatasetAssociation {
                association_id: 9,
                tenant_id: Some(1),
                dataset_id: "regression".to_string(),
                item_id: "case-1".to_string(),
                trace_id: 10,
                span_id: Some(2),
                external_trace_id: Some("run-a".to_string()),
                external_span_id: Some("span-a".to_string()),
                snapshot_id: Some("snap-1".to_string()),
                snapshot_hash: Some("fnv1a64:abc".to_string()),
                eval_run_id: Some("eval-1".to_string()),
                split: Some("train".to_string()),
                label: Some("pass".to_string()),
                score: Some(900),
                created_at_ns: 456,
                attrs,
            }],
            golden_paths: vec![GoldenPathCandidate {
                golden_path_id: 11,
                tenant_id: Some(1),
                task_fingerprint: "npm-native-packaging".to_string(),
                trajectory_signature: "fnv1a64:abc".to_string(),
                source_trace_id: 10,
                external_source_trace_id: Some("run-a".to_string()),
                snapshot_id: Some("snap-1".to_string()),
                snapshot_hash: Some("fnv1a64:snap".to_string()),
                status: GoldenPathStatus::Confirmed,
                score: Some(960),
                label: Some("fast path".to_string()),
                reason: Some("稳定通过".to_string()),
                source: Some("human".to_string()),
                created_at_ns: 789,
                updated_at_ns: 790,
                attrs: BTreeMap::new(),
                source_trajectory_steps: vec!["tool:npm|phase:verify".to_string()],
                evidence: BTreeMap::from([
                    ("sample_count".to_string(), "5".to_string()),
                    ("success_rate".to_string(), "0.800000".to_string()),
                ]),
            }],
            retention_audits: vec![RetentionAuditRecord {
                audit_id: 13,
                tenant_id: Some(1),
                created_at_ns: 1000,
                source: Some("retention-policy".to_string()),
                reason: Some("older than 30d".to_string()),
                delete_before_ts: Some(999),
                query_json: r#"{"filter":{"projectId":"agentic-data"}}"#.to_string(),
                protect_golden_paths: true,
                protect_annotations: true,
                protect_dataset_associations: true,
                protect_snapshots: true,
                protect_eval_links: true,
                protect_path_memory: true,
                compact_requested: true,
                compact_reclaim: true,
                candidate_trace_count: 3,
                protected_trace_count: 1,
                deletable_trace_count: 2,
                requested_trace_count: 2,
                deleted_trace_count: 2,
                deleted_segment_row_count: 4,
                skipped_live_trace_count: 0,
                compacted_segment_count: 1,
                reclaimed_segment_count: 1,
                dropped_deleted_row_count: 4,
                rewritten_live_row_count: 0,
                deletable_trace_ids: vec![10, 11],
                deleted_trace_ids: vec![10, 11],
                skipped_live_trace_ids: Vec::new(),
                trace_id_sample_truncated: false,
            }],
            retention_policies: vec![RetentionPolicy {
                policy_id: 17,
                tenant_id: Some(1),
                name: "nightly-retention".to_string(),
                enabled: true,
                created_at_ns: 1100,
                updated_at_ns: 1200,
                last_run_at_ns: Some(1300),
                next_run_at_ns: Some(1400),
                interval_ns: 86_400_000_000_000,
                source: Some("scheduler".to_string()),
                reason: Some("ttl cleanup".to_string()),
                query_json: r#"{"filter":{"projectId":"agentic-data"},"olderThanNs":1000}"#
                    .to_string(),
            }],
            next_annotation_id: 8,
            next_dataset_association_id: 10,
            next_golden_path_id: 12,
            next_retention_audit_id: 14,
            next_retention_policy_id: 18,
        };
        let back = decode(&encode(&state)).unwrap();
        assert_eq!(back.annotations, state.annotations);
        assert_eq!(back.dataset_associations, state.dataset_associations);
        assert_eq!(back.golden_paths, state.golden_paths);
        assert_eq!(back.retention_audits, state.retention_audits);
        assert_eq!(back.retention_policies, state.retention_policies);
        assert_eq!(back.next_annotation_id, 8);
        assert_eq!(back.next_dataset_association_id, 10);
        assert_eq!(back.next_golden_path_id, 12);
        assert_eq!(back.next_retention_audit_id, 14);
        assert_eq!(back.next_retention_policy_id, 18);
    }
}
