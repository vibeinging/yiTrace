struct TraceSearchSpec {
    session_id: Option<u64>,
    span_id: Option<u64>,
    external_trace_id: Option<String>,
    external_span_id: Option<String>,
    external_session_id: Option<String>,
    status: Option<u8>,
    kind: Option<String>,
    agent_name: Option<String>,
    tool_name: Option<String>,
    model: Option<String>,
    text: Option<String>,
    input_contains: Option<String>,
    output_contains: Option<String>,
    log_contains: Option<String>,
    min_cost_usd_nanos: Option<u64>,
    max_cost_usd_nanos: Option<u64>,
    min_total_tokens: Option<u64>,
    max_total_tokens: Option<u64>,
    attrs: std::collections::BTreeMap<String, String>,
}

#[derive(Default)]
struct TraceSearchAnnotationSpec {
    active: bool,
    target: Option<AnnotationTarget>,
    label: Option<String>,
    source: Option<String>,
    status: Option<AnnotationStatus>,
    include_deleted: bool,
    score_min: Option<u32>,
    score_max: Option<u32>,
    attrs: std::collections::BTreeMap<String, String>,
}

#[derive(Default)]
struct TraceSearchDatasetSpec {
    active: bool,
    dataset_id: Option<String>,
    item_id: Option<String>,
    eval_run_id: Option<String>,
    split: Option<String>,
    label: Option<String>,
    score_min: Option<u32>,
    score_max: Option<u32>,
    attrs: std::collections::BTreeMap<String, String>,
}

#[derive(Default)]
struct TraceSearchMetadataMatches {
    need_annotation: bool,
    annotation_candidate_traces: std::collections::HashSet<u64>,
    annotation_traces: std::collections::HashSet<u64>,
    annotation_spans: std::collections::HashSet<(u64, u64)>,
    need_dataset: bool,
    dataset_candidate_traces: std::collections::HashSet<u64>,
    dataset_traces: std::collections::HashSet<u64>,
    dataset_spans: std::collections::HashSet<(u64, u64)>,
}

struct TraceSearchRequest {
    query: TraceQuery,
    spec: TraceSearchSpec,
    annotation: TraceSearchAnnotationSpec,
    dataset: TraceSearchDatasetSpec,
}

fn trace_search_index_label(request: &TraceSearchRequest) -> &'static str {
    if !request.spec.attrs.is_empty() {
        let indexed = request
            .spec
            .attrs
            .keys()
            .filter(|key| trace_search_attr_uses_postings(key))
            .count();
        if indexed == request.spec.attrs.len() {
            "attrs_postings+folded_verify"
        } else if indexed == 0 {
            "attrs_folded_scan"
        } else {
            "attrs_mixed_postings+folded_verify"
        }
    } else if request.annotation.active || request.dataset.active {
        "metadata_filter+folded_scan"
    } else {
        "folded_scan"
    }
}

fn trace_search_attr_uses_postings(key: &str) -> bool {
    matches!(
        key,
        "project_id"
            | "skill"
            | "mode"
            | "call_site"
            | "task_fingerprint"
            | "loop_id"
            | "harness_version"
            | "validation_status"
            | "stop_reason"
            | "phase"
            | "validator"
            | "connection_ids"
            | "data_source_ids"
            | "schema_fingerprint"
            | "eval_profile"
            | "tool_version"
            | "model"
            | "provider"
            | "intent_signature"
            | "review_status"
            | "eval_status"
    )
}

#[derive(Clone)]
struct TraceAggregateGroupField {
    output_key: String,
    kind: TraceAggregateGroupKind,
}

#[derive(Clone)]
enum TraceAggregateGroupKind {
    Attr(String),
    AgentName,
    ToolName,
    Model,
    Provider,
    Kind,
    Status,
}

#[derive(Clone)]
struct TraceAggregateExample {
    trace_id: u64,
    span_id: u64,
    external_trace_id: Option<String>,
    external_span_id: Option<String>,
    name: String,
}

struct StorageMetadata {
    annotations: Vec<crate::TraceAnnotation>,
    dataset_associations: Vec<crate::DatasetAssociation>,
    golden_paths: Vec<crate::GoldenPathCandidate>,
}

#[derive(Clone, Default)]
struct StorageStatsBucket {
    key: std::collections::BTreeMap<String, String>,
    trace_ids: std::collections::BTreeSet<u64>,
    session_ids: std::collections::BTreeSet<u64>,
    span_count: usize,
    event_count: usize,
    error_span_count: usize,
    first_ts: Option<i64>,
    last_ts: Option<i64>,
    input_text_bytes: u64,
    output_text_bytes: u64,
    log_bytes: u64,
    attr_bytes: u64,
    external_id_bytes: u64,
    field_bytes: u64,
    estimated_bytes: u64,
    annotation_count: usize,
    dataset_association_count: usize,
    golden_path_count: usize,
    snapshot_ref_count: usize,
    eval_link_count: usize,
    path_memory_ref_count: usize,
}

struct StorageStatsReport {
    total: StorageStatsBucket,
    groups: Vec<StorageStatsBucket>,
}

#[derive(Clone)]
struct RetentionPlanConfig {
    apply: bool,
    cutoff: Option<i64>,
    protect_golden_paths: bool,
    protect_annotations: bool,
    protect_dataset_associations: bool,
    protect_snapshots: bool,
    protect_eval_links: bool,
    protect_path_memory: bool,
    example_limit: usize,
    compact_after_apply: bool,
    compact_min_deleted_rows: u32,
    compact_min_deleted_percent: u32,
    compact_max_segments: usize,
    reclaim_after_compact: bool,
    audit_source: Option<String>,
    audit_reason: Option<String>,
    query_json: String,
}

struct RetentionPlanOutcome {
    candidate_stats: StorageStatsBucket,
    protected_stats: StorageStatsBucket,
    deletable_stats: StorageStatsBucket,
    protected: std::collections::BTreeMap<u64, Vec<String>>,
    deletable_trace_ids: std::collections::HashSet<u64>,
    applied: Option<crate::RetentionDeleteResult>,
    compacted: Option<crate::RetentionCompactResult>,
    audit: Option<crate::RetentionAuditRecord>,
}

struct ShardRetentionPlanOutcome {
    shard_id: ShardId,
    outcome: RetentionPlanOutcome,
}

pub(super) struct SearchJsonRequest {
    raw_body: String,
    text: String,
    text_domains: Vec<crate::TextDomain>,
    vector: Vec<f32>,
    k: usize,
    filter: crate::SearchFilter,
    include_fanout: bool,
}

struct TraceAggregateBucket {
    values: Vec<String>,
    span_count: usize,
    trace_ids: std::collections::HashSet<u64>,
    error_count: usize,
    duration_sum_ns: u128,
    duration_max_ns: u64,
    durations_ns: Vec<u64>,
    input_tokens: u64,
    output_tokens: u64,
    cached_input_tokens: u64,
    reasoning_tokens: u64,
    total_tokens: u64,
    cost_usd_nanos: u64,
    examples: Vec<TraceAggregateExample>,
}

#[derive(Clone, Default)]
struct ScoreStats {
    count: usize,
    sum: u64,
    min: u32,
    max: u32,
}

impl ScoreStats {
    fn add(&mut self, score: u32) {
        if self.count == 0 {
            self.min = score;
            self.max = score;
        } else {
            self.min = self.min.min(score);
            self.max = self.max.max(score);
        }
        self.count += 1;
        self.sum += score as u64;
    }

    fn avg(&self) -> u32 {
        if self.count == 0 {
            0
        } else {
            (self.sum / self.count as u64) as u32
        }
    }

    fn merge(&mut self, other: &ScoreStats) {
        if other.count == 0 {
            return;
        }
        if self.count == 0 {
            self.min = other.min;
            self.max = other.max;
        } else {
            self.min = self.min.min(other.min);
            self.max = self.max.max(other.max);
        }
        self.count += other.count;
        self.sum += other.sum;
    }
}

#[derive(Clone)]
struct TrajectoryTraceExample {
    trace_id: u64,
    external_trace_id: Option<String>,
    status: String,
    duration_sum_ns: u128,
    duration_max_ns: u64,
    input_tokens: u64,
    output_tokens: u64,
    cached_input_tokens: u64,
    reasoning_tokens: u64,
    total_tokens: u64,
    cost_usd_nanos: u64,
    score: u32,
    fields: std::collections::BTreeMap<String, String>,
}

struct TrajectoryGroupBucket {
    signature: u64,
    steps: Vec<String>,
    trace_ids: std::collections::BTreeSet<u64>,
    span_count: usize,
    error_trace_count: usize,
    error_span_count: usize,
    duration_sum_ns: u128,
    duration_max_ns: u64,
    durations_ns: Vec<u64>,
    input_tokens: u64,
    output_tokens: u64,
    cached_input_tokens: u64,
    reasoning_tokens: u64,
    total_tokens: u64,
    cost_usd_nanos: u64,
    eval_scores: ScoreStats,
    annotation_scores: ScoreStats,
    dataset_scores: ScoreStats,
    examples: Vec<TrajectoryTraceExample>,
}

impl TrajectoryGroupBucket {
    fn new(signature: u64, steps: Vec<String>) -> Self {
        Self {
            signature,
            steps,
            trace_ids: std::collections::BTreeSet::new(),
            span_count: 0,
            error_trace_count: 0,
            error_span_count: 0,
            duration_sum_ns: 0,
            duration_max_ns: 0,
            durations_ns: Vec::new(),
            input_tokens: 0,
            output_tokens: 0,
            cached_input_tokens: 0,
            reasoning_tokens: 0,
            total_tokens: 0,
            cost_usd_nanos: 0,
            eval_scores: ScoreStats::default(),
            annotation_scores: ScoreStats::default(),
            dataset_scores: ScoreStats::default(),
            examples: Vec::new(),
        }
    }

    fn add_trace(
        &mut self,
        spans: &[FoldedSpan],
        summary: Option<&TaskTraceSummaryBucket>,
        annotation_scores: Option<&[u32]>,
        dataset_scores: Option<&[u32]>,
        example_limit: usize,
    ) {
        let Some(summary) = summary else {
            return;
        };
        self.trace_ids.insert(summary.trace_id);
        self.span_count += summary.span_count;
        self.error_span_count += summary.error_count;
        if summary.error_count > 0 {
            self.error_trace_count += 1;
        }
        let trace_duration = summary.duration_sum_ns.min(u64::MAX as u128) as u64;
        self.duration_sum_ns += summary.duration_sum_ns;
        self.duration_max_ns = self.duration_max_ns.max(summary.duration_max_ns);
        self.durations_ns.push(trace_duration);
        self.input_tokens += summary.input_tokens;
        self.output_tokens += summary.output_tokens;
        self.cached_input_tokens += summary.cached_input_tokens;
        self.reasoning_tokens += summary.reasoning_tokens;
        self.total_tokens += summary.total_tokens;
        self.cost_usd_nanos += summary.cost_usd_nanos;
        for score in spans.iter().filter_map(|s| s.eval_score) {
            self.eval_scores.add(score);
        }
        if let Some(scores) = annotation_scores {
            for score in scores {
                self.annotation_scores.add(*score);
            }
        }
        if let Some(scores) = dataset_scores {
            for score in scores {
                self.dataset_scores.add(*score);
            }
        }
        if self.examples.len() < example_limit {
            self.examples.push(TrajectoryTraceExample {
                trace_id: summary.trace_id,
                external_trace_id: summary.external_trace_id.clone(),
                status: if summary.error_count > 0 {
                    "error".to_string()
                } else {
                    "ok".to_string()
                },
                duration_sum_ns: summary.duration_sum_ns,
                duration_max_ns: summary.duration_max_ns,
                input_tokens: summary.input_tokens,
                output_tokens: summary.output_tokens,
                cached_input_tokens: summary.cached_input_tokens,
                reasoning_tokens: summary.reasoning_tokens,
                total_tokens: summary.total_tokens,
                cost_usd_nanos: summary.cost_usd_nanos,
                score: trajectory_trace_quality_score(
                    summary.error_count == 0,
                    spans,
                    annotation_scores,
                    dataset_scores,
                ),
                fields: summary.fields.clone(),
            });
        }
    }

    fn trace_count(&self) -> usize {
        self.trace_ids.len()
    }

    fn success_count(&self) -> usize {
        self.trace_count().saturating_sub(self.error_trace_count)
    }

    fn avg_duration_ns(&self) -> u128 {
        if self.durations_ns.is_empty() {
            0
        } else {
            self.duration_sum_ns / self.durations_ns.len() as u128
        }
    }

    fn avg_cost_usd_nanos(&self) -> u64 {
        if self.trace_count() == 0 {
            0
        } else {
            self.cost_usd_nanos / self.trace_count() as u64
        }
    }

    fn quality_score(&self) -> u32 {
        let mut sum = self.success_score() as u64;
        let mut count = 1u64;
        for stats in [
            &self.eval_scores,
            &self.annotation_scores,
            &self.dataset_scores,
        ] {
            if stats.count > 0 {
                sum += stats.avg() as u64;
                count += 1;
            }
        }
        (sum / count) as u32
    }

    fn success_score(&self) -> u32 {
        if self.trace_count() == 0 {
            0
        } else {
            ((self.success_count() as u128 * 1000) / self.trace_count() as u128) as u32
        }
    }
}

fn merge_trajectory_group_buckets(
    buckets: Vec<TrajectoryGroupBucket>,
    example_limit: usize,
) -> Vec<TrajectoryGroupBucket> {
    let mut by_signature: std::collections::BTreeMap<u64, TrajectoryGroupBucket> =
        std::collections::BTreeMap::new();
    for mut bucket in buckets {
        match by_signature.get_mut(&bucket.signature) {
            Some(existing) => {
                existing.trace_ids.append(&mut bucket.trace_ids);
                existing.span_count += bucket.span_count;
                existing.error_trace_count += bucket.error_trace_count;
                existing.error_span_count += bucket.error_span_count;
                existing.duration_sum_ns += bucket.duration_sum_ns;
                existing.duration_max_ns = existing.duration_max_ns.max(bucket.duration_max_ns);
                existing.durations_ns.append(&mut bucket.durations_ns);
                existing.input_tokens += bucket.input_tokens;
                existing.output_tokens += bucket.output_tokens;
                existing.cached_input_tokens += bucket.cached_input_tokens;
                existing.reasoning_tokens += bucket.reasoning_tokens;
                existing.total_tokens += bucket.total_tokens;
                existing.cost_usd_nanos += bucket.cost_usd_nanos;
                existing.eval_scores.merge(&bucket.eval_scores);
                existing.annotation_scores.merge(&bucket.annotation_scores);
                existing.dataset_scores.merge(&bucket.dataset_scores);
                for example in bucket.examples {
                    if existing.examples.len() >= example_limit {
                        break;
                    }
                    existing.examples.push(example);
                }
            }
            None => {
                if bucket.examples.len() > example_limit {
                    bucket.examples.truncate(example_limit);
                }
                by_signature.insert(bucket.signature, bucket);
            }
        }
    }
    by_signature.into_values().collect()
}

struct ProductQueryParts {
    cursor: usize,
    limit: usize,
    filter: String,
    attrs: std::collections::BTreeMap<String, String>,
    annotation: TraceSearchAnnotationSpec,
    dataset: TraceSearchDatasetSpec,
}

struct PathAdherenceFacts {
    source_available: bool,
    source_retained: bool,
    source_steps: Vec<String>,
    source_signature: Option<String>,
    trace_steps: Vec<String>,
    trace_signature: String,
    same_signature: bool,
    stored_signature_matches_source: Option<bool>,
    common_steps: Vec<String>,
    missing_steps: Vec<String>,
    extra_steps: Vec<String>,
}

impl PathAdherenceFacts {
    fn adherence(&self) -> &'static str {
        if self.same_signature {
            "followed"
        } else if self.source_steps.is_empty() {
            "unknown"
        } else if self.common_steps.is_empty() {
            "deviated"
        } else if self.missing_steps.is_empty() {
            "extended"
        } else {
            "partial"
        }
    }
}

struct LoopSummaryBucket {
    loop_id: String,
    loop_value_json: String,
    trace_ids: std::collections::HashSet<u64>,
    session_ids: std::collections::HashSet<u64>,
    span_count: usize,
    error_count: usize,
    duration_sum_ns: u128,
    duration_max_ns: u64,
    durations_ns: Vec<u64>,
    input_tokens: u64,
    output_tokens: u64,
    cached_input_tokens: u64,
    reasoning_tokens: u64,
    total_tokens: u64,
    cost_usd_nanos: u64,
    first_trace_id: u64,
    last_trace_id: u64,
    fields: std::collections::BTreeMap<String, String>,
    phases: std::collections::BTreeSet<String>,
    validators: std::collections::BTreeSet<String>,
    examples: Vec<TraceAggregateExample>,
}

impl LoopSummaryBucket {
    fn new(loop_value_json: String) -> Self {
        Self {
            loop_id: json_compact_label(&loop_value_json),
            loop_value_json,
            trace_ids: std::collections::HashSet::new(),
            session_ids: std::collections::HashSet::new(),
            span_count: 0,
            error_count: 0,
            duration_sum_ns: 0,
            duration_max_ns: 0,
            durations_ns: Vec::new(),
            input_tokens: 0,
            output_tokens: 0,
            cached_input_tokens: 0,
            reasoning_tokens: 0,
            total_tokens: 0,
            cost_usd_nanos: 0,
            first_trace_id: u64::MAX,
            last_trace_id: 0,
            fields: std::collections::BTreeMap::new(),
            phases: std::collections::BTreeSet::new(),
            validators: std::collections::BTreeSet::new(),
            examples: Vec::new(),
        }
    }
}

struct TaskTraceSummaryBucket {
    trace_id: u64,
    external_trace_id: Option<String>,
    span_count: usize,
    error_count: usize,
    duration_sum_ns: u128,
    duration_max_ns: u64,
    input_tokens: u64,
    output_tokens: u64,
    cached_input_tokens: u64,
    reasoning_tokens: u64,
    total_tokens: u64,
    cost_usd_nanos: u64,
    fields: std::collections::BTreeMap<String, String>,
}

impl TaskTraceSummaryBucket {
    fn new(s: &FoldedSpan) -> Self {
        Self {
            trace_id: s.trace_id,
            external_trace_id: s.external_trace_id.clone(),
            span_count: 0,
            error_count: 0,
            duration_sum_ns: 0,
            duration_max_ns: 0,
            input_tokens: 0,
            output_tokens: 0,
            cached_input_tokens: 0,
            reasoning_tokens: 0,
            total_tokens: 0,
            cost_usd_nanos: 0,
            fields: std::collections::BTreeMap::new(),
        }
    }
}
