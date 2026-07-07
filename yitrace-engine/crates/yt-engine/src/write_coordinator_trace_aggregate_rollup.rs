#[derive(Clone, Debug, Default)]
pub(crate) struct TraceAggregateRollupFilters {
    pub status: Option<u8>,
    pub kind: Option<String>,
    pub agent_name: Option<String>,
    pub tool_name: Option<String>,
    pub model: Option<String>,
    pub attrs: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct TraceAggregateRollupStats {
    pub used_segment_rollup: bool,
    pub segment_rollup_segments: usize,
    pub segment_rollup_rows: usize,
    pub tail_folded_span_count: usize,
}

impl TraceAggregateRollupStats {
    pub(crate) fn add_shard(&mut self, other: &TraceAggregateRollupStats) {
        self.used_segment_rollup |= other.used_segment_rollup;
        self.segment_rollup_segments += other.segment_rollup_segments;
        self.segment_rollup_rows += other.segment_rollup_rows;
        self.tail_folded_span_count += other.tail_folded_span_count;
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct TraceAggregateRollupProfileStats {
    pub cached_segments: usize,
    pub cached_rows: usize,
    pub storage_profile_families: usize,
    pub storage_profile_buckets: usize,
    pub aggregate_profile_families: usize,
    pub aggregate_profile_buckets: usize,
}

impl TraceAggregateRollupProfileStats {
    fn add_rollup(&mut self, rollup: &TraceAggregateSegmentRollup) {
        self.cached_segments += 1;
        self.cached_rows += rollup.rows.len();
        self.storage_profile_families += rollup.storage_profiles.len();
        self.storage_profile_buckets += rollup
            .storage_profiles
            .values()
            .map(Vec::len)
            .sum::<usize>();
        self.aggregate_profile_families += rollup.aggregate_profiles.len();
        self.aggregate_profile_buckets += rollup
            .aggregate_profiles
            .values()
            .map(Vec::len)
            .sum::<usize>();
    }
}

#[derive(Clone, Debug)]
pub(crate) struct TraceAggregateRollupRead {
    pub rows: Vec<TraceAggregateRollupRow>,
    pub trace_bounds: BTreeMap<u64, (i64, i64)>,
    pub stats: TraceAggregateRollupStats,
}

#[derive(Clone, Debug)]
pub(crate) struct TraceAggregateStorageRollupRead {
    pub buckets: Vec<TraceAggregateStorageBucket>,
    pub stats: TraceAggregateRollupStats,
}

#[derive(Clone, Debug)]
pub(crate) struct TraceAggregatePreaggregateRead {
    pub buckets: Vec<TraceAggregatePreaggregateBucket>,
    pub stats: TraceAggregateRollupStats,
}

#[derive(Clone, Debug)]
pub(crate) struct TraceSearchRollupPageRead {
    pub keys: Vec<(u64, u64)>,
    pub total: usize,
    pub stats: TraceAggregateRollupStats,
}

#[derive(Clone, Debug)]
struct TraceSearchRollupCandidate {
    trace_id: u64,
    span_id: u64,
    duration_ns: u64,
    cost_usd_nanos: u64,
    total_tokens: u64,
    status: u8,
}

impl TraceSearchRollupCandidate {
    fn from_row(row: &TraceAggregateRollupRow) -> Self {
        Self {
            trace_id: row.trace_id,
            span_id: row.span_id,
            duration_ns: row.duration_ns.unwrap_or(0),
            cost_usd_nanos: row.cost_usd_nanos,
            total_tokens: row.total_tokens,
            status: row.status.unwrap_or(0),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct TraceAggregatePreaggregateBucket {
    pub key: BTreeMap<String, String>,
    pub trace_ids: BTreeSet<u64>,
    pub tenant_ids: BTreeSet<u64>,
    pub span_count: usize,
    pub error_count: usize,
    pub duration_sum_ns: u128,
    pub duration_max_ns: u64,
    pub durations_ns: Vec<u64>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    pub reasoning_tokens: u64,
    pub total_tokens: u64,
    pub cost_usd_nanos: u64,
    pub examples: Vec<TraceAggregatePreaggregateExample>,
}

#[derive(Clone, Debug)]
pub(crate) struct TraceAggregatePreaggregateExample {
    pub trace_id: u64,
    pub span_id: u64,
    pub external_trace_id: Option<String>,
    pub external_span_id: Option<String>,
    pub name: String,
}

impl TraceAggregatePreaggregateBucket {
    fn add_row(&mut self, row: &TraceAggregateRollupRow) {
        self.trace_ids.insert(row.trace_id);
        if let Some(tenant_id) = row.tenant_id {
            self.tenant_ids.insert(tenant_id);
        }
        self.span_count += 1;
        if row.status.unwrap_or(0) != 0 {
            self.error_count += 1;
        }
        if let Some(duration) = row.duration_ns {
            self.duration_sum_ns += duration as u128;
            self.duration_max_ns = self.duration_max_ns.max(duration);
            self.durations_ns.push(duration);
        }
        self.input_tokens += row.input_tokens;
        self.output_tokens += row.output_tokens;
        self.cached_input_tokens += row.cached_input_tokens;
        self.reasoning_tokens += row.reasoning_tokens;
        self.total_tokens += row.total_tokens;
        self.cost_usd_nanos += row.cost_usd_nanos;
        if self.examples.len() < 3 {
            self.examples.push(TraceAggregatePreaggregateExample {
                trace_id: row.trace_id,
                span_id: row.span_id,
                external_trace_id: row.external_trace_id.clone(),
                external_span_id: row.external_span_id.clone(),
                name: row.name(),
            });
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct TraceAggregateStorageBucket {
    pub key: BTreeMap<String, String>,
    pub trace_ids: BTreeSet<u64>,
    pub session_ids: BTreeSet<u64>,
    pub tenant_ids: BTreeSet<u64>,
    pub span_count: usize,
    pub event_count: usize,
    pub error_span_count: usize,
    pub first_ts: Option<i64>,
    pub last_ts: Option<i64>,
    pub input_text_bytes: u64,
    pub output_text_bytes: u64,
    pub log_bytes: u64,
    pub attr_bytes: u64,
    pub external_id_bytes: u64,
    pub field_bytes: u64,
    pub estimated_bytes: u64,
}

impl TraceAggregateStorageBucket {
    fn add_row(&mut self, row: &TraceAggregateRollupRow, trace_bounds: &BTreeMap<u64, (i64, i64)>) {
        self.trace_ids.insert(row.trace_id);
        if let Some(session_id) = row.session_id {
            self.session_ids.insert(session_id);
        }
        if let Some(tenant_id) = row.tenant_id {
            self.tenant_ids.insert(tenant_id);
        }
        self.span_count += 1;
        self.event_count += row.event_count;
        if row.status.unwrap_or(0) != 0 {
            self.error_span_count += 1;
        }
        if let Some((first_ts, last_ts)) = trace_bounds.get(&row.trace_id) {
            self.first_ts = Some(self.first_ts.map_or(*first_ts, |v| v.min(*first_ts)));
            self.last_ts = Some(self.last_ts.map_or(*last_ts, |v| v.max(*last_ts)));
        }
        self.input_text_bytes += row.input_text_bytes;
        self.output_text_bytes += row.output_text_bytes;
        self.log_bytes += row.log_bytes;
        self.attr_bytes += row.attr_bytes;
        self.external_id_bytes += row.external_id_bytes;
        self.field_bytes += row.field_bytes;
        self.estimated_bytes += row.estimated_bytes;
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct TraceAggregateRollupRow {
    pub trace_id: u64,
    pub span_id: u64,
    pub session_id: Option<u64>,
    pub tenant_id: Option<u64>,
    pub external_trace_id: Option<String>,
    pub external_span_id: Option<String>,
    pub external_session_id: Option<String>,
    pub first_ts: i64,
    pub last_ts: i64,
    pub status: Option<u8>,
    pub duration_ns: Option<u64>,
    pub event_count: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    pub reasoning_tokens: u64,
    pub total_tokens: u64,
    pub cost_usd_nanos: u64,
    pub provider: Option<String>,
    pub agent_name: Option<String>,
    pub tool_name: Option<String>,
    pub model: Option<String>,
    pub attrs: BTreeMap<String, String>,
    pub input_text_bytes: u64,
    pub output_text_bytes: u64,
    pub log_bytes: u64,
    pub attr_bytes: u64,
    pub external_id_bytes: u64,
    pub field_bytes: u64,
    pub estimated_bytes: u64,
}

impl TraceAggregateRollupRow {
    fn from_folded_span(s: FoldedSpan, first_ts: i64, last_ts: i64) -> Self {
        let input_text_bytes = s.input_text.as_deref().map(str::len).unwrap_or(0) as u64;
        let output_text_bytes = s.output_text.as_deref().map(str::len).unwrap_or(0) as u64;
        let log_bytes = s.logs.iter().map(|log| log.len() as u64).sum::<u64>();
        let attr_bytes = s
            .attrs
            .iter()
            .map(|(k, v)| k.len() as u64 + v.len() as u64 + 4)
            .sum::<u64>();
        let external_id_bytes = [
            s.external_trace_id.as_deref(),
            s.external_span_id.as_deref(),
            s.external_parent_span_id.as_deref(),
            s.external_session_id.as_deref(),
        ]
        .into_iter()
        .flatten()
        .map(|value| value.len() as u64)
        .sum::<u64>();
        let field_bytes = [
            s.agent_name.as_deref(),
            s.tool_name.as_deref(),
            s.model.as_deref(),
            s.provider.as_deref(),
            s.project_id.as_deref(),
            s.skill.as_deref(),
            s.mode.as_deref(),
            s.call_site.as_deref(),
            s.task_fingerprint.as_deref(),
            s.loop_id.as_deref(),
        ]
        .into_iter()
        .flatten()
        .map(|value| value.len() as u64)
        .sum::<u64>()
            + 8 * [
                s.duration_ns,
                s.input_tokens,
                s.output_tokens,
                s.cached_input_tokens,
                s.reasoning_tokens,
                s.total_tokens,
                s.cost_usd_nanos,
            ]
            .into_iter()
            .flatten()
            .count() as u64;
        let estimated_bytes = input_text_bytes
            + output_text_bytes
            + log_bytes
            + attr_bytes
            + external_id_bytes
            + field_bytes
            + (s.event_count as u64 * 64)
            + 128;
        let mut attrs = s.attrs.clone();
        for key in first_class_agentic_attr_keys() {
            if let Some(value) = first_class_span_attr_value(&s, key) {
                attrs.insert(
                    (*key).to_string(),
                    first_class_agentic_attr_json(key, value),
                );
            }
        }
        let input_tokens = s.input_tokens.unwrap_or(0);
        let output_tokens = s.output_tokens.unwrap_or(0);
        let cached_input_tokens = s.cached_input_tokens.unwrap_or(0);
        let reasoning_tokens = s.reasoning_tokens.unwrap_or(0);
        let total_tokens = usage_total_tokens(
            input_tokens,
            output_tokens,
            cached_input_tokens,
            reasoning_tokens,
            s.total_tokens,
        );
        let cost_usd_nanos = usage_cost_usd_nanos_for_model(
            input_tokens,
            output_tokens,
            cached_input_tokens,
            reasoning_tokens,
            s.cost_usd_nanos,
            s.provider.as_deref(),
            s.model.as_deref(),
        );
        Self {
            trace_id: s.trace_id,
            span_id: s.span_id,
            session_id: s.session_id,
            tenant_id: s.tenant_id,
            external_trace_id: s.external_trace_id,
            external_span_id: s.external_span_id,
            external_session_id: s.external_session_id,
            first_ts,
            last_ts,
            status: s.status,
            duration_ns: s.duration_ns,
            event_count: s.event_count,
            input_tokens,
            output_tokens,
            cached_input_tokens,
            reasoning_tokens,
            total_tokens,
            cost_usd_nanos,
            provider: s.provider,
            agent_name: s.agent_name,
            tool_name: s.tool_name,
            model: s.model,
            attrs,
            input_text_bytes,
            output_text_bytes,
            log_bytes,
            attr_bytes,
            external_id_bytes,
            field_bytes,
            estimated_bytes,
        }
    }

    pub(crate) fn attr_value(&self, key: &str) -> Option<&str> {
        self.attrs.get(key).map(String::as_str)
    }

    pub(crate) fn kind(&self) -> &'static str {
        if self.agent_name.is_some() {
            "agent"
        } else if self.tool_name.is_some() {
            "tool"
        } else if self.model.is_some() {
            "llm"
        } else {
            "other"
        }
    }

    pub(crate) fn name(&self) -> String {
        self.agent_name
            .as_ref()
            .or(self.tool_name.as_ref())
            .or(self.model.as_ref())
            .cloned()
            .unwrap_or_else(|| format!("span {}", self.span_id))
    }
}

#[derive(Clone, Debug, Default)]
struct TraceAggregateSegmentRollup {
    rows: Vec<TraceAggregateRollupRow>,
    trace_min: Option<u64>,
    trace_max: Option<u64>,
    storage_profiles: BTreeMap<String, Vec<TraceAggregateStorageBucket>>,
    aggregate_profiles: BTreeMap<String, Vec<TraceAggregatePreaggregateBucket>>,
}

impl TraceAggregateSegmentRollup {
    fn build(records: &[WalRecord], config: &TraceRollupProfileConfig) -> Self {
        let inputs: Vec<FoldInput> = records.iter().map(WalRecord::to_fold_input).collect();
        let bounds =
            span_time_bounds_from_records(records.iter().map(|r| (r.trace_id, r.span_id, r.ts)));
        let rows = fold_events(inputs)
            .into_iter()
            .map(|span| {
                let (first_ts, last_ts) = bounds
                    .get(&(span.trace_id, span.span_id))
                    .copied()
                    .unwrap_or((0, 0));
                TraceAggregateRollupRow::from_folded_span(span, first_ts, last_ts)
            })
            .collect();
        Self::from_rows_with_config(rows, config)
    }

    fn from_rows(rows: Vec<TraceAggregateRollupRow>) -> Self {
        Self::from_rows_with_config(rows, &TraceRollupProfileConfig::full())
    }

    fn from_rows_with_config(
        rows: Vec<TraceAggregateRollupRow>,
        config: &TraceRollupProfileConfig,
    ) -> Self {
        let trace_min = rows.iter().map(|row| row.trace_id).min();
        let trace_max = rows.iter().map(|row| row.trace_id).max();
        let trace_bounds = trace_time_bounds_from_rollup_rows(&rows);
        let storage_profiles = trace_aggregate_storage_profiles(&rows, &trace_bounds, config);
        let aggregate_profiles = trace_aggregate_preaggregate_profiles(&rows, config);
        Self {
            rows,
            trace_min,
            trace_max,
            storage_profiles,
            aggregate_profiles,
        }
    }
}

fn span_time_bounds_from_records(
    records: impl IntoIterator<Item = (u64, u64, i64)>,
) -> BTreeMap<(u64, u64), (i64, i64)> {
    let mut bounds: BTreeMap<(u64, u64), (i64, i64)> = BTreeMap::new();
    for (trace_id, span_id, ts) in records {
        bounds
            .entry((trace_id, span_id))
            .and_modify(|(first, last)| {
                *first = (*first).min(ts);
                *last = (*last).max(ts);
            })
            .or_insert((ts, ts));
    }
    bounds
}

fn trace_time_bounds_from_rollup_rows<'a>(
    rows: impl IntoIterator<Item = &'a TraceAggregateRollupRow>,
) -> BTreeMap<u64, (i64, i64)> {
    let mut bounds: BTreeMap<u64, (i64, i64)> = BTreeMap::new();
    for row in rows {
        bounds
            .entry(row.trace_id)
            .and_modify(|(first, last)| {
                *first = (*first).min(row.first_ts);
                *last = (*last).max(row.last_ts);
            })
            .or_insert((row.first_ts, row.last_ts));
    }
    bounds
}

const STORAGE_PREAGG_PROFILES: &[&[&str]] = &[
    &[],
    &["project_id"],
    &["task_fingerprint"],
    &["validation_status"],
    &["skill"],
    &["mode"],
    &["agent_name"],
    &["tool_name"],
    &["kind"],
    &["status"],
    &["project_id", "validation_status"],
    &["project_id", "task_fingerprint"],
    &["project_id", "skill"],
    &["project_id", "mode"],
    &["project_id", "tool_name"],
    &["project_id", "status"],
    &["task_fingerprint", "validation_status"],
    &["task_fingerprint", "skill"],
    &["task_fingerprint", "mode"],
    &["task_fingerprint", "status"],
];

pub(crate) fn trace_aggregate_storage_profile_supported(fields: &[String]) -> bool {
    let key = trace_aggregate_storage_profile_key(fields);
    STORAGE_PREAGG_PROFILES
        .iter()
        .any(|profile| trace_aggregate_storage_profile_key_strs(profile) == key)
}

fn trace_aggregate_storage_profiles(
    rows: &[TraceAggregateRollupRow],
    trace_bounds: &BTreeMap<u64, (i64, i64)>,
    config: &TraceRollupProfileConfig,
) -> BTreeMap<String, Vec<TraceAggregateStorageBucket>> {
    let mut profiles = BTreeMap::new();
    let profile_limit = config
        .storage_profile_limit()
        .unwrap_or(STORAGE_PREAGG_PROFILES.len())
        .min(STORAGE_PREAGG_PROFILES.len());
    for profile in STORAGE_PREAGG_PROFILES.iter().copied().take(profile_limit) {
        let key = trace_aggregate_storage_profile_key_strs(profile);
        let mut buckets: BTreeMap<BTreeMap<String, String>, TraceAggregateStorageBucket> =
            BTreeMap::new();
        let mut overflowed = false;
        for row in rows {
            let mut bucket_key = BTreeMap::new();
            for field in profile {
                bucket_key.insert(
                    (*field).to_string(),
                    trace_aggregate_storage_value_json(row, field),
                );
            }
            if trace_aggregate_profile_bucket_overflows(&buckets, &bucket_key, config) {
                overflowed = true;
                break;
            }
            let bucket =
                buckets
                    .entry(bucket_key.clone())
                    .or_insert_with(|| TraceAggregateStorageBucket {
                        key: bucket_key,
                        ..TraceAggregateStorageBucket::default()
                    });
            bucket.add_row(row, trace_bounds);
        }
        if !overflowed {
            profiles.insert(key, buckets.into_values().collect());
        }
    }
    profiles
}

fn trace_aggregate_profile_bucket_overflows<T>(
    buckets: &BTreeMap<BTreeMap<String, String>, T>,
    bucket_key: &BTreeMap<String, String>,
    config: &TraceRollupProfileConfig,
) -> bool {
    !buckets.contains_key(bucket_key)
        && config
            .max_buckets_per_profile()
            .is_some_and(|limit| buckets.len() >= limit)
}

fn trace_aggregate_storage_profile_key(fields: &[String]) -> String {
    let mut fields = fields.to_vec();
    fields.sort();
    fields.dedup();
    fields.join("\u{1f}")
}

fn trace_aggregate_storage_profile_key_strs(fields: &[&str]) -> String {
    let mut fields = fields.to_vec();
    fields.sort();
    fields.dedup();
    fields.join("\u{1f}")
}

fn trace_aggregate_storage_value_json(row: &TraceAggregateRollupRow, field: &str) -> String {
    match field {
        "trace_id" => row.trace_id.to_string(),
        "span_id" => row.span_id.to_string(),
        "session_id" => row
            .external_session_id
            .as_deref()
            .map(storage_json_string_value)
            .or_else(|| row.session_id.map(|id| id.to_string()))
            .unwrap_or_else(|| "null".to_string()),
        "agent_name" => row
            .agent_name
            .as_deref()
            .map(storage_json_string_value)
            .unwrap_or_else(|| "null".to_string()),
        "tool_name" => row
            .tool_name
            .as_deref()
            .map(storage_json_string_value)
            .unwrap_or_else(|| "null".to_string()),
        "model" => row
            .model
            .as_deref()
            .map(storage_json_string_value)
            .unwrap_or_else(|| "null".to_string()),
        "kind" => storage_json_string_value(row.kind()),
        "status" => row
            .status
            .map(|status| status.to_string())
            .unwrap_or_else(|| "null".to_string()),
        _ => row
            .attr_value(field)
            .map(storage_compact_or_json_string_value)
            .unwrap_or_else(|| "null".to_string()),
    }
}

fn storage_compact_or_json_string_value(value: &str) -> String {
    match crate::wire::parse(value) {
        Ok(v) => v.to_compact_json(),
        Err(_) => storage_json_string_value(value),
    }
}

fn storage_json_string_value(value: &str) -> String {
    crate::wire::Json::Str(value.to_string()).to_compact_json()
}

fn trace_aggregate_storage_bucket_matches(
    bucket: &TraceAggregateStorageBucket,
    q: &TraceQuery,
    filters: &TraceAggregateRollupFilters,
    profile_fields: &[String],
) -> Result<bool, &'static str> {
    if let Some(tenant_id) = q.tenant_id {
        if !bucket.tenant_ids.contains(&tenant_id) {
            return Ok(false);
        }
        if bucket.tenant_ids.len() != 1 {
            return Err("mixed_tenant_bucket");
        }
    }
    if let Some(status) = filters.status {
        if !trace_aggregate_storage_bucket_value_matches(
            bucket,
            profile_fields,
            "status",
            &status.to_string(),
        )? {
            return Ok(false);
        }
    }
    if let Some(kind) = &filters.kind {
        if !trace_aggregate_storage_bucket_value_matches(
            bucket,
            profile_fields,
            "kind",
            &storage_json_string_value(kind),
        )? {
            return Ok(false);
        }
    }
    if let Some(agent_name) = &filters.agent_name {
        if !trace_aggregate_storage_bucket_value_matches(
            bucket,
            profile_fields,
            "agent_name",
            &storage_json_string_value(agent_name),
        )? {
            return Ok(false);
        }
    }
    if let Some(tool_name) = &filters.tool_name {
        if !trace_aggregate_storage_bucket_value_matches(
            bucket,
            profile_fields,
            "tool_name",
            &storage_json_string_value(tool_name),
        )? {
            return Ok(false);
        }
    }
    if let Some(model) = &filters.model {
        if !trace_aggregate_storage_bucket_value_matches(
            bucket,
            profile_fields,
            "model",
            &storage_json_string_value(model),
        )? {
            return Ok(false);
        }
    }
    for (key, expected) in &filters.attrs {
        if !trace_aggregate_storage_bucket_value_matches(bucket, profile_fields, key, expected)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn trace_aggregate_storage_bucket_value_matches(
    bucket: &TraceAggregateStorageBucket,
    profile_fields: &[String],
    key: &str,
    expected: &str,
) -> Result<bool, &'static str> {
    if !profile_fields.iter().any(|field| field == key) {
        return Err("storage_preaggregate_missing_filter_field");
    }
    let Some(actual) = bucket.key.get(key) else {
        return Ok(false);
    };
    Ok(attr_json_matches(actual, expected))
}

fn trace_aggregate_storage_ranges_disjoint(
    ranges: &[(u64, u64)],
) -> Result<(), &'static str> {
    let mut ranges = ranges.to_vec();
    ranges.sort_unstable();
    let mut last_max: Option<u64> = None;
    for (min_trace, max_trace) in ranges {
        if let Some(last) = last_max {
            if min_trace <= last {
                return Err("trace_range_overlap");
            }
        }
        last_max = Some(max_trace);
    }
    Ok(())
}

const TRACE_AGGREGATE_PREAGG_PROFILES: &[&[&str]] = &[
    &["project_id"],
    &["task_fingerprint"],
    &["validation_status"],
    &["tool_name"],
    &["agent_name"],
    &["skill"],
    &["mode"],
    &["status"],
    &["kind"],
    &["project_id", "validation_status"],
    &["project_id", "tool_name"],
    &["project_id", "task_fingerprint"],
    &["project_id", "skill"],
    &["task_fingerprint", "validation_status"],
    &["validation_status", "tool_name"],
    &["project_id", "validation_status", "tool_name"],
    &["project_id", "task_fingerprint", "validation_status"],
    &["project_id", "task_fingerprint", "tool_name"],
    &["project_id", "skill", "mode"],
    &["project_id", "status", "tool_name"],
];

pub(crate) fn trace_aggregate_preaggregate_profile_supported(fields: &[String]) -> bool {
    let key = trace_aggregate_preaggregate_profile_key(fields);
    TRACE_AGGREGATE_PREAGG_PROFILES
        .iter()
        .any(|profile| trace_aggregate_storage_profile_key_strs(profile) == key)
}

fn trace_aggregate_preaggregate_profiles(
    rows: &[TraceAggregateRollupRow],
    config: &TraceRollupProfileConfig,
) -> BTreeMap<String, Vec<TraceAggregatePreaggregateBucket>> {
    let mut profiles = BTreeMap::new();
    let profile_limit = config
        .aggregate_profile_limit()
        .unwrap_or(TRACE_AGGREGATE_PREAGG_PROFILES.len())
        .min(TRACE_AGGREGATE_PREAGG_PROFILES.len());
    for profile in TRACE_AGGREGATE_PREAGG_PROFILES
        .iter()
        .copied()
        .take(profile_limit)
    {
        let key = trace_aggregate_storage_profile_key_strs(profile);
        let mut buckets: BTreeMap<BTreeMap<String, String>, TraceAggregatePreaggregateBucket> =
            BTreeMap::new();
        let mut overflowed = false;
        for row in rows {
            let mut bucket_key = BTreeMap::new();
            for field in profile {
                bucket_key.insert(
                    (*field).to_string(),
                    trace_aggregate_storage_value_json(row, field),
                );
            }
            if trace_aggregate_profile_bucket_overflows(&buckets, &bucket_key, config) {
                overflowed = true;
                break;
            }
            let bucket =
                buckets
                    .entry(bucket_key.clone())
                    .or_insert_with(|| TraceAggregatePreaggregateBucket {
                        key: bucket_key,
                        ..TraceAggregatePreaggregateBucket::default()
                    });
            bucket.add_row(row);
        }
        if !overflowed {
            profiles.insert(key, buckets.into_values().collect());
        }
    }
    profiles
}

fn trace_aggregate_preaggregate_profile_key(fields: &[String]) -> String {
    trace_aggregate_storage_profile_key(fields)
}

fn trace_aggregate_preaggregate_bucket_matches(
    bucket: &TraceAggregatePreaggregateBucket,
    q: &TraceQuery,
    filters: &TraceAggregateRollupFilters,
    profile_fields: &[String],
) -> Result<bool, &'static str> {
    if let Some(tenant_id) = q.tenant_id {
        if !bucket.tenant_ids.contains(&tenant_id) {
            return Ok(false);
        }
        if bucket.tenant_ids.len() != 1 {
            return Err("mixed_tenant_bucket");
        }
    }
    if let Some(status) = filters.status {
        if !trace_aggregate_preaggregate_bucket_value_matches(
            bucket,
            profile_fields,
            "status",
            &status.to_string(),
        )? {
            return Ok(false);
        }
    }
    if let Some(kind) = &filters.kind {
        if !trace_aggregate_preaggregate_bucket_value_matches(
            bucket,
            profile_fields,
            "kind",
            &storage_json_string_value(kind),
        )? {
            return Ok(false);
        }
    }
    if let Some(agent_name) = &filters.agent_name {
        if !trace_aggregate_preaggregate_bucket_value_matches(
            bucket,
            profile_fields,
            "agent_name",
            &storage_json_string_value(agent_name),
        )? {
            return Ok(false);
        }
    }
    if let Some(tool_name) = &filters.tool_name {
        if !trace_aggregate_preaggregate_bucket_value_matches(
            bucket,
            profile_fields,
            "tool_name",
            &storage_json_string_value(tool_name),
        )? {
            return Ok(false);
        }
    }
    if let Some(model) = &filters.model {
        if !trace_aggregate_preaggregate_bucket_value_matches(
            bucket,
            profile_fields,
            "model",
            &storage_json_string_value(model),
        )? {
            return Ok(false);
        }
    }
    for (key, expected) in &filters.attrs {
        if !trace_aggregate_preaggregate_bucket_value_matches(
            bucket,
            profile_fields,
            key,
            expected,
        )? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn trace_aggregate_preaggregate_bucket_value_matches(
    bucket: &TraceAggregatePreaggregateBucket,
    profile_fields: &[String],
    key: &str,
    expected: &str,
) -> Result<bool, &'static str> {
    if !profile_fields.iter().any(|field| field == key) {
        return Err("trace_aggregate_preaggregate_missing_filter_field");
    }
    let Some(actual) = bucket.key.get(key) else {
        return Ok(false);
    };
    Ok(attr_json_matches(actual, expected))
}

fn sort_trace_search_rollup_candidates(
    candidates: &mut [TraceSearchRollupCandidate],
    sort_by: &str,
    desc: bool,
) {
    let sort = sort_by.to_ascii_lowercase();
    candidates.sort_by(|a, b| {
        let ord = match sort.as_str() {
            "duration" | "duration_ns" | "durationns" => a.duration_ns.cmp(&b.duration_ns),
            "cost" | "cost_usd" | "costusd" => a.cost_usd_nanos.cmp(&b.cost_usd_nanos),
            "tokens" | "token_count" | "tokencount" => a.total_tokens.cmp(&b.total_tokens),
            "status" => a.status.cmp(&b.status),
            "span" | "span_id" | "spanid" => a.span_id.cmp(&b.span_id),
            _ => a
                .trace_id
                .cmp(&b.trace_id)
                .then_with(|| a.span_id.cmp(&b.span_id)),
        };
        let ord = if desc { ord.reverse() } else { ord };
        ord.then_with(|| a.trace_id.cmp(&b.trace_id))
            .then_with(|| a.span_id.cmp(&b.span_id))
    });
}

impl WriteCoordinator {
    fn install_trace_aggregate_segment_rollup(
        &self,
        seg: SegmentId,
        records: &[WalRecord],
        cache: bool,
    ) {
        let rollup = Arc::new(TraceAggregateSegmentRollup::build(
            records,
            &self.trace_rollup_profile_config,
        ));
        if let Some(dir) = &self.trace_aggregate_rollup_dir {
            let _ = write_trace_aggregate_rollup_file(dir, seg, &rollup);
        }
        if cache {
            self.trace_aggregate_rollups
                .lock()
                .unwrap()
                .insert(seg.get(), rollup);
        }
    }

    fn trace_aggregate_segment_rollup(&self, seg: SegmentId) -> Arc<TraceAggregateSegmentRollup> {
        if let Some(rollup) = self
            .trace_aggregate_rollups
            .lock()
            .unwrap()
            .get(&seg.get())
            .cloned()
        {
            return rollup;
        }
        let loaded = self
            .trace_aggregate_rollup_dir
            .as_ref()
            .and_then(|dir| {
                read_trace_aggregate_rollup_file(dir, seg, &self.trace_rollup_profile_config)
            })
            .unwrap_or_else(|| {
                let records = self.segments.scan_records(seg);
                let rollup = TraceAggregateSegmentRollup::build(
                    &records,
                    &self.trace_rollup_profile_config,
                );
                if let Some(dir) = &self.trace_aggregate_rollup_dir {
                    let _ = write_trace_aggregate_rollup_file(dir, seg, &rollup);
                }
                rollup
            });
        let rollup = Arc::new(loaded);
        self.trace_aggregate_rollups
            .lock()
            .unwrap()
            .insert(seg.get(), rollup.clone());
        rollup
    }

    pub(crate) fn trace_aggregate_rollup_profile_stats(
        &self,
    ) -> TraceAggregateRollupProfileStats {
        let rollups = self.trace_aggregate_rollups.lock().unwrap();
        let mut stats = TraceAggregateRollupProfileStats::default();
        for rollup in rollups.values() {
            stats.add_rollup(rollup);
        }
        stats
    }

    pub(crate) fn trace_search_rollup_page_read(
        &self,
        snap: &Snapshot,
        q: &TraceQuery,
        filters: &TraceAggregateRollupFilters,
        sort_by: &str,
        desc: bool,
        cursor: usize,
        limit: usize,
    ) -> Result<TraceSearchRollupPageRead, &'static str> {
        if q.trace_id.is_some() {
            return Err("trace_id_filter");
        }
        if q.time_from != i64::MIN || q.time_to != i64::MAX {
            return Err("time_window_filter");
        }

        let mut seen = HashSet::new();
        let mut candidates = Vec::new();
        let mut stats = TraceAggregateRollupStats::default();
        for entry in snap.manifest.segments.values() {
            if entry.deletion_seq != 0 || entry.deletion_vec.count() != 0 {
                return Err("segment_deletion_vector");
            }
            if entry.upgrade_seq != 0 || entry.upgrade_ref.is_some() {
                return Err("segment_upgrade_patch");
            }
            let rollup = self.trace_aggregate_segment_rollup(entry.segment_id);
            stats.segment_rollup_segments += 1;
            stats.segment_rollup_rows += rollup.rows.len();
            for row in &rollup.rows {
                if !seen.insert((row.trace_id, row.span_id)) {
                    return Err("span_crosses_multiple_segments");
                }
                if trace_aggregate_rollup_row_matches(row, q, filters) {
                    candidates.push(TraceSearchRollupCandidate::from_row(row));
                }
            }
        }

        let tail_rows: Vec<(u64, u64, i64, FoldInput)> = self
            .memtable
            .lock()
            .unwrap()
            .read_range(snap.retained_watermark, snap.live_lsn)
            .map(|row| (row.trace_id, row.span_id, row.ts, row.to_fold_input()))
            .collect();
        let tail_bounds = span_time_bounds_from_records(
            tail_rows
                .iter()
                .map(|(trace_id, span_id, ts, _)| (*trace_id, *span_id, *ts)),
        );
        let tail_inputs: Vec<FoldInput> = tail_rows
            .into_iter()
            .map(|(_, _, _, input)| input)
            .collect();
        let tail_spans = fold_events(tail_inputs);
        stats.tail_folded_span_count = tail_spans.len();
        for span in tail_spans {
            let (first_ts, last_ts) = tail_bounds
                .get(&(span.trace_id, span.span_id))
                .copied()
                .unwrap_or((0, 0));
            let row = TraceAggregateRollupRow::from_folded_span(span, first_ts, last_ts);
            if seen.contains(&(row.trace_id, row.span_id)) {
                return Err("tail_overlaps_segment_span");
            }
            if trace_aggregate_rollup_row_matches(&row, q, filters) {
                candidates.push(TraceSearchRollupCandidate::from_row(&row));
            }
        }

        stats.used_segment_rollup = stats.segment_rollup_segments > 0;
        sort_trace_search_rollup_candidates(&mut candidates, sort_by, desc);
        let total = candidates.len();
        let end = cursor.saturating_add(limit).min(total);
        let keys = if cursor < total {
            candidates[cursor..end]
                .iter()
                .map(|candidate| (candidate.trace_id, candidate.span_id))
                .collect()
        } else {
            Vec::new()
        };
        Ok(TraceSearchRollupPageRead { keys, total, stats })
    }

    pub(crate) fn trace_aggregate_rollup_read(
        &self,
        snap: &Snapshot,
        q: &TraceQuery,
        filters: &TraceAggregateRollupFilters,
    ) -> Result<TraceAggregateRollupRead, &'static str> {
        if q.trace_id.is_some() {
            return Err("trace_id_filter");
        }
        if q.time_from != i64::MIN || q.time_to != i64::MAX {
            return Err("time_window_filter");
        }

        let mut seen = HashSet::new();
        let mut rows = Vec::new();
        let mut all_rows = Vec::new();
        let mut stats = TraceAggregateRollupStats::default();
        for entry in snap.manifest.segments.values() {
            if entry.deletion_seq != 0 || entry.deletion_vec.count() != 0 {
                return Err("segment_deletion_vector");
            }
            if entry.upgrade_seq != 0 || entry.upgrade_ref.is_some() {
                return Err("segment_upgrade_patch");
            }
            let rollup = self.trace_aggregate_segment_rollup(entry.segment_id);
            stats.segment_rollup_segments += 1;
            stats.segment_rollup_rows += rollup.rows.len();
            for row in &rollup.rows {
                if !seen.insert((row.trace_id, row.span_id)) {
                    return Err("span_crosses_multiple_segments");
                }
                all_rows.push(row.clone());
                if trace_aggregate_rollup_row_matches(row, q, filters) {
                    rows.push(row.clone());
                }
            }
        }

        let tail_rows: Vec<(u64, u64, i64, FoldInput)> = self
            .memtable
            .lock()
            .unwrap()
            .read_range(snap.retained_watermark, snap.live_lsn)
            .map(|row| (row.trace_id, row.span_id, row.ts, row.to_fold_input()))
            .collect();
        let tail_bounds = span_time_bounds_from_records(
            tail_rows
                .iter()
                .map(|(trace_id, span_id, ts, _)| (*trace_id, *span_id, *ts)),
        );
        let tail_inputs: Vec<FoldInput> = tail_rows
            .into_iter()
            .map(|(_, _, _, input)| input)
            .collect();
        let tail_spans = fold_events(tail_inputs);
        stats.tail_folded_span_count = tail_spans.len();
        for span in tail_spans {
            let (first_ts, last_ts) = tail_bounds
                .get(&(span.trace_id, span.span_id))
                .copied()
                .unwrap_or((0, 0));
            let row = TraceAggregateRollupRow::from_folded_span(span, first_ts, last_ts);
            if seen.contains(&(row.trace_id, row.span_id)) {
                return Err("tail_overlaps_segment_span");
            }
            all_rows.push(row.clone());
            if trace_aggregate_rollup_row_matches(&row, q, filters) {
                rows.push(row);
            }
        }
        stats.used_segment_rollup = stats.segment_rollup_segments > 0;
        let trace_bounds = trace_time_bounds_from_rollup_rows(&all_rows);
        Ok(TraceAggregateRollupRead {
            rows,
            trace_bounds,
            stats,
        })
    }

    pub(crate) fn trace_aggregate_storage_rollup_read(
        &self,
        snap: &Snapshot,
        q: &TraceQuery,
        filters: &TraceAggregateRollupFilters,
        profile_fields: &[String],
    ) -> Result<TraceAggregateStorageRollupRead, &'static str> {
        if q.trace_id.is_some() {
            return Err("trace_id_filter");
        }
        if q.time_from != i64::MIN || q.time_to != i64::MAX {
            return Err("time_window_filter");
        }
        if !trace_aggregate_storage_profile_supported(profile_fields) {
            return Err("unsupported_storage_preaggregate_profile");
        }

        let profile_key = trace_aggregate_storage_profile_key(profile_fields);
        let mut buckets = Vec::new();
        let mut stats = TraceAggregateRollupStats::default();
        let mut trace_ranges = Vec::new();
        for entry in snap.manifest.segments.values() {
            if entry.deletion_seq != 0 || entry.deletion_vec.count() != 0 {
                return Err("segment_deletion_vector");
            }
            if entry.upgrade_seq != 0 || entry.upgrade_ref.is_some() {
                return Err("segment_upgrade_patch");
            }
            let rollup = self.trace_aggregate_segment_rollup(entry.segment_id);
            if let (Some(min_trace), Some(max_trace)) = (rollup.trace_min, rollup.trace_max) {
                trace_ranges.push((min_trace, max_trace));
            }
            let Some(segment_buckets) = rollup.storage_profiles.get(&profile_key) else {
                return Err("missing_storage_preaggregate_profile");
            };
            stats.segment_rollup_segments += 1;
            stats.segment_rollup_rows += rollup.rows.len();
            for bucket in segment_buckets {
                if trace_aggregate_storage_bucket_matches(bucket, q, filters, profile_fields)? {
                    buckets.push(bucket.clone());
                }
            }
        }
        trace_aggregate_storage_ranges_disjoint(&trace_ranges)?;

        let tail_rows: Vec<(u64, u64, i64, FoldInput)> = self
            .memtable
            .lock()
            .unwrap()
            .read_range(snap.retained_watermark, snap.live_lsn)
            .map(|row| (row.trace_id, row.span_id, row.ts, row.to_fold_input()))
            .collect();
        let tail_bounds = span_time_bounds_from_records(
            tail_rows
                .iter()
                .map(|(trace_id, span_id, ts, _)| (*trace_id, *span_id, *ts)),
        );
        let tail_inputs: Vec<FoldInput> = tail_rows
            .into_iter()
            .map(|(_, _, _, input)| input)
            .collect();
        let tail_spans = fold_events(tail_inputs);
        stats.tail_folded_span_count = tail_spans.len();
        if !tail_spans.is_empty() {
            let tail_rows = tail_spans
                .into_iter()
                .map(|span| {
                    let (first_ts, last_ts) = tail_bounds
                        .get(&(span.trace_id, span.span_id))
                        .copied()
                        .unwrap_or((0, 0));
                    TraceAggregateRollupRow::from_folded_span(span, first_ts, last_ts)
                })
                .collect::<Vec<_>>();
            let tail_rollup = TraceAggregateSegmentRollup::from_rows_with_config(
                tail_rows,
                &self.trace_rollup_profile_config,
            );
            if let (Some(min_trace), Some(max_trace)) = (tail_rollup.trace_min, tail_rollup.trace_max)
            {
                trace_ranges.push((min_trace, max_trace));
                trace_aggregate_storage_ranges_disjoint(&trace_ranges)?;
            }
            let Some(tail_buckets) = tail_rollup.storage_profiles.get(&profile_key) else {
                return Err("missing_tail_storage_preaggregate_profile");
            };
            for bucket in tail_buckets {
                if trace_aggregate_storage_bucket_matches(bucket, q, filters, profile_fields)? {
                    buckets.push(bucket.clone());
                }
            }
        }
        stats.used_segment_rollup = stats.segment_rollup_segments > 0;
        Ok(TraceAggregateStorageRollupRead { buckets, stats })
    }

    pub(crate) fn trace_aggregate_preaggregate_read(
        &self,
        snap: &Snapshot,
        q: &TraceQuery,
        filters: &TraceAggregateRollupFilters,
        profile_fields: &[String],
    ) -> Result<TraceAggregatePreaggregateRead, &'static str> {
        if q.trace_id.is_some() {
            return Err("trace_id_filter");
        }
        if q.time_from != i64::MIN || q.time_to != i64::MAX {
            return Err("time_window_filter");
        }
        if !trace_aggregate_preaggregate_profile_supported(profile_fields) {
            return Err("unsupported_trace_aggregate_preaggregate_profile");
        }

        let profile_key = trace_aggregate_preaggregate_profile_key(profile_fields);
        let mut buckets = Vec::new();
        let mut stats = TraceAggregateRollupStats::default();
        let mut trace_ranges = Vec::new();
        for entry in snap.manifest.segments.values() {
            if entry.deletion_seq != 0 || entry.deletion_vec.count() != 0 {
                return Err("segment_deletion_vector");
            }
            if entry.upgrade_seq != 0 || entry.upgrade_ref.is_some() {
                return Err("segment_upgrade_patch");
            }
            let rollup = self.trace_aggregate_segment_rollup(entry.segment_id);
            if let (Some(min_trace), Some(max_trace)) = (rollup.trace_min, rollup.trace_max) {
                trace_ranges.push((min_trace, max_trace));
            }
            let Some(segment_buckets) = rollup.aggregate_profiles.get(&profile_key) else {
                return Err("missing_trace_aggregate_preaggregate_profile");
            };
            stats.segment_rollup_segments += 1;
            stats.segment_rollup_rows += rollup.rows.len();
            for bucket in segment_buckets {
                if trace_aggregate_preaggregate_bucket_matches(
                    bucket,
                    q,
                    filters,
                    profile_fields,
                )? {
                    buckets.push(bucket.clone());
                }
            }
        }
        trace_aggregate_storage_ranges_disjoint(&trace_ranges)?;

        let tail_rows: Vec<(u64, u64, i64, FoldInput)> = self
            .memtable
            .lock()
            .unwrap()
            .read_range(snap.retained_watermark, snap.live_lsn)
            .map(|row| (row.trace_id, row.span_id, row.ts, row.to_fold_input()))
            .collect();
        let tail_bounds = span_time_bounds_from_records(
            tail_rows
                .iter()
                .map(|(trace_id, span_id, ts, _)| (*trace_id, *span_id, *ts)),
        );
        let tail_inputs: Vec<FoldInput> = tail_rows
            .into_iter()
            .map(|(_, _, _, input)| input)
            .collect();
        let tail_spans = fold_events(tail_inputs);
        stats.tail_folded_span_count = tail_spans.len();
        if !tail_spans.is_empty() {
            let tail_rows = tail_spans
                .into_iter()
                .map(|span| {
                    let (first_ts, last_ts) = tail_bounds
                        .get(&(span.trace_id, span.span_id))
                        .copied()
                        .unwrap_or((0, 0));
                    TraceAggregateRollupRow::from_folded_span(span, first_ts, last_ts)
                })
                .collect::<Vec<_>>();
            let tail_rollup = TraceAggregateSegmentRollup::from_rows_with_config(
                tail_rows,
                &self.trace_rollup_profile_config,
            );
            if let (Some(min_trace), Some(max_trace)) = (tail_rollup.trace_min, tail_rollup.trace_max)
            {
                trace_ranges.push((min_trace, max_trace));
                trace_aggregate_storage_ranges_disjoint(&trace_ranges)?;
            }
            let Some(tail_buckets) = tail_rollup.aggregate_profiles.get(&profile_key) else {
                return Err("missing_tail_trace_aggregate_preaggregate_profile");
            };
            for bucket in tail_buckets {
                if trace_aggregate_preaggregate_bucket_matches(
                    bucket,
                    q,
                    filters,
                    profile_fields,
                )? {
                    buckets.push(bucket.clone());
                }
            }
        }
        stats.used_segment_rollup = stats.segment_rollup_segments > 0;
        Ok(TraceAggregatePreaggregateRead { buckets, stats })
    }
}

fn trace_aggregate_rollup_row_matches(
    row: &TraceAggregateRollupRow,
    q: &TraceQuery,
    filters: &TraceAggregateRollupFilters,
) -> bool {
    if let Some(tenant_id) = q.tenant_id {
        if row.tenant_id != Some(tenant_id) {
            return false;
        }
    }
    if let Some(status) = filters.status {
        if row.status != Some(status) {
            return false;
        }
    }
    if let Some(kind) = &filters.kind {
        if row.kind() != kind {
            return false;
        }
    }
    if let Some(agent_name) = &filters.agent_name {
        if row.agent_name.as_deref() != Some(agent_name.as_str()) {
            return false;
        }
    }
    if let Some(tool_name) = &filters.tool_name {
        if row.tool_name.as_deref() != Some(tool_name.as_str()) {
            return false;
        }
    }
    if let Some(model) = &filters.model {
        if row.model.as_deref() != Some(model.as_str()) {
            return false;
        }
    }
    filters.attrs.iter().all(|(key, expected)| {
        row.attr_value(key)
            .map(|actual| attr_json_matches(actual, expected))
            .unwrap_or(false)
    })
}

const TRACE_AGGREGATE_ROLLUP_MAGIC: &[u8; 8] = b"YTAR2\0\0\0";

fn trace_aggregate_rollup_path(dir: &std::path::Path, seg: SegmentId) -> std::path::PathBuf {
    dir.join(format!("seg-{}.agg", seg.get()))
}

fn write_trace_aggregate_rollup_file(
    dir: &std::path::Path,
    seg: SegmentId,
    rollup: &TraceAggregateSegmentRollup,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = trace_aggregate_rollup_path(dir, seg);
    let tmp = dir.join(format!("seg-{}.agg.tmp", seg.get()));
    std::fs::write(&tmp, encode_trace_aggregate_rollup(rollup))?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

fn read_trace_aggregate_rollup_file(
    dir: &std::path::Path,
    seg: SegmentId,
    config: &TraceRollupProfileConfig,
) -> Option<TraceAggregateSegmentRollup> {
    let bytes = std::fs::read(trace_aggregate_rollup_path(dir, seg)).ok()?;
    decode_trace_aggregate_rollup(&bytes, config).ok()
}

fn encode_trace_aggregate_rollup(rollup: &TraceAggregateSegmentRollup) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(TRACE_AGGREGATE_ROLLUP_MAGIC);
    write_u32(&mut out, rollup.rows.len() as u32);
    for row in &rollup.rows {
        write_trace_aggregate_rollup_row(&mut out, row);
    }
    out
}

fn decode_trace_aggregate_rollup(
    bytes: &[u8],
    config: &TraceRollupProfileConfig,
) -> Result<TraceAggregateSegmentRollup, String> {
    let mut pos = 0usize;
    if bytes.len() < TRACE_AGGREGATE_ROLLUP_MAGIC.len()
        || &bytes[..TRACE_AGGREGATE_ROLLUP_MAGIC.len()] != TRACE_AGGREGATE_ROLLUP_MAGIC
    {
        return Err("bad trace aggregate rollup magic".to_string());
    }
    pos += TRACE_AGGREGATE_ROLLUP_MAGIC.len();
    let count = read_u32(bytes, &mut pos)? as usize;
    let mut rows = Vec::with_capacity(count);
    for _ in 0..count {
        rows.push(read_trace_aggregate_rollup_row(bytes, &mut pos)?);
    }
    Ok(TraceAggregateSegmentRollup::from_rows_with_config(
        rows, config,
    ))
}

fn write_trace_aggregate_rollup_row(out: &mut Vec<u8>, row: &TraceAggregateRollupRow) {
    write_u64(out, row.trace_id);
    write_u64(out, row.span_id);
    write_trace_aggregate_optional_u64(out, row.session_id);
    write_trace_aggregate_optional_u64(out, row.tenant_id);
    write_trace_aggregate_optional_string(out, row.external_trace_id.as_deref());
    write_trace_aggregate_optional_string(out, row.external_span_id.as_deref());
    write_trace_aggregate_optional_string(out, row.external_session_id.as_deref());
    write_i64(out, row.first_ts);
    write_i64(out, row.last_ts);
    write_trace_aggregate_optional_u8(out, row.status);
    write_trace_aggregate_optional_u64(out, row.duration_ns);
    write_u64(out, row.event_count as u64);
    write_u64(out, row.input_tokens);
    write_u64(out, row.output_tokens);
    write_u64(out, row.cached_input_tokens);
    write_u64(out, row.reasoning_tokens);
    write_u64(out, row.total_tokens);
    write_u64(out, row.cost_usd_nanos);
    write_trace_aggregate_optional_string(out, row.provider.as_deref());
    write_trace_aggregate_optional_string(out, row.agent_name.as_deref());
    write_trace_aggregate_optional_string(out, row.tool_name.as_deref());
    write_trace_aggregate_optional_string(out, row.model.as_deref());
    write_u64(out, row.input_text_bytes);
    write_u64(out, row.output_text_bytes);
    write_u64(out, row.log_bytes);
    write_u64(out, row.attr_bytes);
    write_u64(out, row.external_id_bytes);
    write_u64(out, row.field_bytes);
    write_u64(out, row.estimated_bytes);
    write_u32(out, row.attrs.len() as u32);
    for (key, value) in &row.attrs {
        write_string(out, key);
        write_string(out, value);
    }
}

fn read_trace_aggregate_rollup_row(
    bytes: &[u8],
    pos: &mut usize,
) -> Result<TraceAggregateRollupRow, String> {
    let trace_id = read_u64(bytes, pos)?;
    let span_id = read_u64(bytes, pos)?;
    let session_id = read_trace_aggregate_optional_u64(bytes, pos)?;
    let tenant_id = read_trace_aggregate_optional_u64(bytes, pos)?;
    let external_trace_id = read_trace_aggregate_optional_string(bytes, pos)?;
    let external_span_id = read_trace_aggregate_optional_string(bytes, pos)?;
    let external_session_id = read_trace_aggregate_optional_string(bytes, pos)?;
    let first_ts = read_i64(bytes, pos)?;
    let last_ts = read_i64(bytes, pos)?;
    let status = read_trace_aggregate_optional_u8(bytes, pos)?;
    let duration_ns = read_trace_aggregate_optional_u64(bytes, pos)?;
    let event_count = read_u64(bytes, pos)? as usize;
    let input_tokens = read_u64(bytes, pos)?;
    let output_tokens = read_u64(bytes, pos)?;
    let cached_input_tokens = read_u64(bytes, pos)?;
    let reasoning_tokens = read_u64(bytes, pos)?;
    let total_tokens = read_u64(bytes, pos)?;
    let cost_usd_nanos = read_u64(bytes, pos)?;
    let provider = read_trace_aggregate_optional_string(bytes, pos)?;
    let agent_name = read_trace_aggregate_optional_string(bytes, pos)?;
    let tool_name = read_trace_aggregate_optional_string(bytes, pos)?;
    let model = read_trace_aggregate_optional_string(bytes, pos)?;
    let input_text_bytes = read_u64(bytes, pos)?;
    let output_text_bytes = read_u64(bytes, pos)?;
    let log_bytes = read_u64(bytes, pos)?;
    let attr_bytes = read_u64(bytes, pos)?;
    let external_id_bytes = read_u64(bytes, pos)?;
    let field_bytes = read_u64(bytes, pos)?;
    let estimated_bytes = read_u64(bytes, pos)?;
    let attr_count = read_u32(bytes, pos)? as usize;
    let mut attrs = BTreeMap::new();
    for _ in 0..attr_count {
        attrs.insert(read_string(bytes, pos)?, read_string(bytes, pos)?);
    }
    Ok(TraceAggregateRollupRow {
        trace_id,
        span_id,
        session_id,
        tenant_id,
        external_trace_id,
        external_span_id,
        external_session_id,
        first_ts,
        last_ts,
        status,
        duration_ns,
        event_count,
        input_tokens,
        output_tokens,
        cached_input_tokens,
        reasoning_tokens,
        total_tokens,
        cost_usd_nanos,
        provider,
        agent_name,
        tool_name,
        model,
        attrs,
        input_text_bytes,
        output_text_bytes,
        log_bytes,
        attr_bytes,
        external_id_bytes,
        field_bytes,
        estimated_bytes,
    })
}

fn write_i64(out: &mut Vec<u8>, value: i64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn read_i64(bytes: &[u8], pos: &mut usize) -> Result<i64, String> {
    let end = pos.saturating_add(8);
    let Some(slice) = bytes.get(*pos..end) else {
        return Err("truncated i64".into());
    };
    *pos = end;
    Ok(i64::from_le_bytes(slice.try_into().unwrap()))
}

fn write_trace_aggregate_optional_u8(out: &mut Vec<u8>, value: Option<u8>) {
    match value {
        Some(v) => {
            out.push(1);
            out.push(v);
        }
        None => out.push(0),
    }
}

fn read_trace_aggregate_optional_u8(bytes: &[u8], pos: &mut usize) -> Result<Option<u8>, String> {
    let tag = *bytes.get(*pos).ok_or_else(|| "truncated option tag".to_string())?;
    *pos += 1;
    if tag == 0 {
        Ok(None)
    } else {
        let value = *bytes.get(*pos).ok_or_else(|| "truncated u8".to_string())?;
        *pos += 1;
        Ok(Some(value))
    }
}

fn write_trace_aggregate_optional_u64(out: &mut Vec<u8>, value: Option<u64>) {
    match value {
        Some(v) => {
            out.push(1);
            write_u64(out, v);
        }
        None => out.push(0),
    }
}

fn read_trace_aggregate_optional_u64(bytes: &[u8], pos: &mut usize) -> Result<Option<u64>, String> {
    let tag = *bytes.get(*pos).ok_or_else(|| "truncated option tag".to_string())?;
    *pos += 1;
    if tag == 0 {
        Ok(None)
    } else {
        Ok(Some(read_u64(bytes, pos)?))
    }
}

fn write_trace_aggregate_optional_string(out: &mut Vec<u8>, value: Option<&str>) {
    match value {
        Some(v) => {
            out.push(1);
            write_string(out, v);
        }
        None => out.push(0),
    }
}

fn read_trace_aggregate_optional_string(
    bytes: &[u8],
    pos: &mut usize,
) -> Result<Option<String>, String> {
    let tag = *bytes.get(*pos).ok_or_else(|| "truncated option tag".to_string())?;
    *pos += 1;
    if tag == 0 {
        Ok(None)
    } else {
        Ok(Some(read_string(bytes, pos)?))
    }
}
