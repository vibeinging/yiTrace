fn storage_group_by_from_json(v: &crate::wire::Json) -> Vec<String> {
    let Some(raw) = json_field_alias(v, &["groupBy", "group_by", "groups"]) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut push_key = |raw: &str| {
        let key = normalize_storage_group_key(raw);
        if !key.is_empty() && !out.contains(&key) {
            out.push(key);
        }
    };
    match raw {
        crate::wire::Json::Str(s) => {
            for part in s.split(',') {
                push_key(part);
            }
        }
        crate::wire::Json::Arr(items) => {
            for item in items {
                match item {
                    crate::wire::Json::Str(s) | crate::wire::Json::Num(s) => push_key(s),
                    _ => {}
                }
            }
        }
        _ => {}
    }
    out
}

fn storage_stats_projection() -> crate::Projection {
    crate::Projection::of(
        crate::Projection::STATUS
            | crate::Projection::DURATION_NS
            | crate::Projection::INPUT_TOKENS
            | crate::Projection::OUTPUT_TOKENS
            | crate::Projection::USAGE_COST
            | crate::Projection::SESSION_ID
            | crate::Projection::AGENT_NAME
            | crate::Projection::TOOL_NAME
            | crate::Projection::MODEL
            | crate::Projection::INPUT_TEXT
            | crate::Projection::OUTPUT_TEXT
            | crate::Projection::LOGS
            | crate::Projection::EXTERNAL_IDS
            | crate::Projection::ATTRS
            | crate::Projection::AGENTIC_FIELDS,
    )
}

fn normalize_storage_group_key(raw: &str) -> String {
    let lower = raw.trim().replace('-', "_").to_ascii_lowercase();
    let compact = lower.replace('_', "");
    match compact.as_str() {
        "projectid" => "project_id".to_string(),
        "taskfingerprint" => "task_fingerprint".to_string(),
        "callsite" => "call_site".to_string(),
        "loopid" => "loop_id".to_string(),
        "harnessversion" => "harness_version".to_string(),
        "schemafingerprint" => "schema_fingerprint".to_string(),
        "intentsignature" => "intent_signature".to_string(),
        "validationstatus" => "validation_status".to_string(),
        "reviewstatus" => "review_status".to_string(),
        "evalstatus" => "eval_status".to_string(),
        "pathmemoryid" => "path_memory_id".to_string(),
        "stopreason" => "stop_reason".to_string(),
        "sessionid" => "session_id".to_string(),
        "traceid" => "trace_id".to_string(),
        "spanid" => "span_id".to_string(),
        "agentname" => "agent_name".to_string(),
        "toolname" => "tool_name".to_string(),
        "timebucket" | "day" | "time" => "time".to_string(),
        _ => lower,
    }
}

fn storage_stats_report(
    spans: &[FoldedSpan],
    bounds: &std::collections::BTreeMap<u64, (i64, i64)>,
    metadata: &StorageMetadata,
    group_by: &[String],
    time_bucket_ns: u64,
) -> StorageStatsReport {
    let mut total = StorageStatsBucket::default();
    let mut groups: std::collections::BTreeMap<
        std::collections::BTreeMap<String, String>,
        StorageStatsBucket,
    > = std::collections::BTreeMap::new();

    for span in spans {
        storage_bucket_add_span(&mut total, span, bounds);
        if !group_by.is_empty() {
            let mut key = std::collections::BTreeMap::new();
            for field in group_by {
                key.insert(
                    field.clone(),
                    storage_group_value_json(span, field, bounds, time_bucket_ns),
                );
            }
            let bucket = groups
                .entry(key.clone())
                .or_insert_with(|| StorageStatsBucket {
                    key,
                    ..StorageStatsBucket::default()
                });
            storage_bucket_add_span(bucket, span, bounds);
        }
    }

    storage_bucket_apply_metadata_counts(&mut total, metadata);
    let mut groups: Vec<StorageStatsBucket> = groups.into_values().collect();
    for bucket in &mut groups {
        storage_bucket_apply_metadata_counts(bucket, metadata);
    }
    groups.sort_by(|a, b| {
        b.estimated_bytes
            .cmp(&a.estimated_bytes)
            .then_with(|| b.trace_ids.len().cmp(&a.trace_ids.len()))
            .then_with(|| a.key.cmp(&b.key))
    });
    StorageStatsReport { total, groups }
}

fn storage_rollup_blockers(group_by: &[String], request: &TraceSearchRequest) -> Vec<String> {
    let mut blockers = Vec::new();
    if request.annotation.active || request.dataset.active {
        blockers.push("metadata_filter".to_string());
    }
    let spec = &request.spec;
    if spec.text.is_some()
        || spec.input_contains.is_some()
        || spec.output_contains.is_some()
        || spec.log_contains.is_some()
    {
        blockers.push("text_contains_filter".to_string());
    }
    if spec.session_id.is_some()
        || spec.span_id.is_some()
        || spec.external_trace_id.is_some()
        || spec.external_span_id.is_some()
        || spec.external_session_id.is_some()
    {
        blockers.push("identity_filter".to_string());
    }
    if spec.min_cost_usd_nanos.is_some()
        || spec.max_cost_usd_nanos.is_some()
        || spec.min_total_tokens.is_some()
        || spec.max_total_tokens.is_some()
    {
        blockers.push("row_metric_range_filter".to_string());
    }
    if request.query.trace_id.is_some()
        || request.query.time_from != i64::MIN
        || request.query.time_to != i64::MAX
    {
        blockers.push("trace_or_time_filter".to_string());
    }
    for field in group_by {
        if !storage_rollup_group_supported(field) {
            blockers.push(format!("unsupported_group_by:{field}"));
        }
    }
    blockers
}

fn storage_rollup_group_supported(field: &str) -> bool {
    matches!(
        field,
        "project_id"
            | "skill"
            | "mode"
            | "call_site"
            | "task_fingerprint"
            | "loop_id"
            | "harness_version"
            | "schema_fingerprint"
            | "intent_signature"
            | "validation_status"
            | "review_status"
            | "eval_status"
            | "stop_reason"
            | "phase"
            | "validator"
            | "eval_profile"
            | "tool_version"
            | "trace_id"
            | "span_id"
            | "session_id"
            | "agent_name"
            | "tool_name"
            | "kind"
            | "status"
    )
}

fn storage_preaggregate_fields(
    group_by: &[String],
    request: &TraceSearchRequest,
) -> Option<Vec<String>> {
    let mut fields = std::collections::BTreeSet::new();
    for field in group_by {
        if !storage_preaggregate_field_supported(field) {
            return None;
        }
        fields.insert(field.clone());
    }
    let spec = &request.spec;
    if spec.status.is_some() {
        fields.insert("status".to_string());
    }
    if spec.kind.is_some() {
        fields.insert("kind".to_string());
    }
    if spec.agent_name.is_some() {
        fields.insert("agent_name".to_string());
    }
    if spec.tool_name.is_some() {
        fields.insert("tool_name".to_string());
    }
    if spec.model.is_some() {
        fields.insert("model".to_string());
    }
    for key in spec.attrs.keys() {
        if !storage_preaggregate_field_supported(key) {
            return None;
        }
        fields.insert(key.clone());
    }
    let fields: Vec<String> = fields.into_iter().collect();
    if crate::trace_aggregate_storage_profile_supported(&fields) {
        Some(fields)
    } else {
        None
    }
}

fn storage_preaggregate_field_supported(field: &str) -> bool {
    matches!(
        field,
        "project_id"
            | "task_fingerprint"
            | "validation_status"
            | "skill"
            | "mode"
            | "agent_name"
            | "tool_name"
            | "kind"
            | "status"
    )
}

fn storage_stats_report_from_rollup_rows(
    rows: &[crate::TraceAggregateRollupRow],
    trace_bounds: &std::collections::BTreeMap<u64, (i64, i64)>,
    metadata: &StorageMetadata,
    group_by: &[String],
    time_bucket_ns: u64,
) -> StorageStatsReport {
    let mut total = StorageStatsBucket::default();
    let mut groups: std::collections::BTreeMap<
        std::collections::BTreeMap<String, String>,
        StorageStatsBucket,
    > = std::collections::BTreeMap::new();

    for row in rows {
        storage_bucket_add_rollup_row(&mut total, row, trace_bounds);
        if !group_by.is_empty() {
            let mut key = std::collections::BTreeMap::new();
            for field in group_by {
                key.insert(
                    field.clone(),
                    storage_group_value_json_rollup(row, field, trace_bounds, time_bucket_ns),
                );
            }
            let bucket = groups
                .entry(key.clone())
                .or_insert_with(|| StorageStatsBucket {
                    key,
                    ..StorageStatsBucket::default()
                });
            storage_bucket_add_rollup_row(bucket, row, trace_bounds);
        }
    }

    storage_bucket_apply_metadata_counts(&mut total, metadata);
    let mut groups: Vec<StorageStatsBucket> = groups.into_values().collect();
    for bucket in &mut groups {
        storage_bucket_apply_metadata_counts(bucket, metadata);
    }
    groups.sort_by(|a, b| {
        b.estimated_bytes
            .cmp(&a.estimated_bytes)
            .then_with(|| b.trace_ids.len().cmp(&a.trace_ids.len()))
            .then_with(|| a.key.cmp(&b.key))
    });
    StorageStatsReport { total, groups }
}

fn storage_stats_report_from_preaggregate_buckets(
    buckets: &[crate::TraceAggregateStorageBucket],
    metadata: &StorageMetadata,
    group_by: &[String],
) -> StorageStatsReport {
    let mut total = StorageStatsBucket::default();
    let mut groups: std::collections::BTreeMap<
        std::collections::BTreeMap<String, String>,
        StorageStatsBucket,
    > = std::collections::BTreeMap::new();

    for source in buckets {
        merge_storage_preaggregate_bucket(&mut total, source);
        if !group_by.is_empty() {
            let mut key = std::collections::BTreeMap::new();
            for field in group_by {
                key.insert(
                    field.clone(),
                    source
                        .key
                        .get(field)
                        .cloned()
                        .unwrap_or_else(|| "null".to_string()),
                );
            }
            let bucket = groups
                .entry(key.clone())
                .or_insert_with(|| StorageStatsBucket {
                    key,
                    ..StorageStatsBucket::default()
                });
            merge_storage_preaggregate_bucket(bucket, source);
        }
    }

    storage_bucket_apply_metadata_counts(&mut total, metadata);
    let mut groups: Vec<StorageStatsBucket> = groups.into_values().collect();
    for bucket in &mut groups {
        storage_bucket_apply_metadata_counts(bucket, metadata);
    }
    groups.sort_by(|a, b| {
        b.estimated_bytes
            .cmp(&a.estimated_bytes)
            .then_with(|| b.trace_ids.len().cmp(&a.trace_ids.len()))
            .then_with(|| a.key.cmp(&b.key))
    });
    StorageStatsReport { total, groups }
}

fn merge_storage_bucket(target: &mut StorageStatsBucket, mut source: StorageStatsBucket) {
    target.trace_ids.append(&mut source.trace_ids);
    target.session_ids.append(&mut source.session_ids);
    target.span_count += source.span_count;
    target.event_count += source.event_count;
    target.error_span_count += source.error_span_count;
    if let Some(first_ts) = source.first_ts {
        target.first_ts = Some(target.first_ts.map_or(first_ts, |v| v.min(first_ts)));
    }
    if let Some(last_ts) = source.last_ts {
        target.last_ts = Some(target.last_ts.map_or(last_ts, |v| v.max(last_ts)));
    }
    target.input_text_bytes += source.input_text_bytes;
    target.output_text_bytes += source.output_text_bytes;
    target.log_bytes += source.log_bytes;
    target.attr_bytes += source.attr_bytes;
    target.external_id_bytes += source.external_id_bytes;
    target.field_bytes += source.field_bytes;
    target.estimated_bytes += source.estimated_bytes;
    target.annotation_count += source.annotation_count;
    target.dataset_association_count += source.dataset_association_count;
    target.golden_path_count += source.golden_path_count;
    target.snapshot_ref_count += source.snapshot_ref_count;
    target.eval_link_count += source.eval_link_count;
    target.path_memory_ref_count += source.path_memory_ref_count;
}

fn merge_storage_preaggregate_bucket(
    target: &mut StorageStatsBucket,
    source: &crate::TraceAggregateStorageBucket,
) {
    target.trace_ids.extend(source.trace_ids.iter().copied());
    target.session_ids.extend(source.session_ids.iter().copied());
    target.span_count += source.span_count;
    target.event_count += source.event_count;
    target.error_span_count += source.error_span_count;
    if let Some(first_ts) = source.first_ts {
        target.first_ts = Some(target.first_ts.map_or(first_ts, |v| v.min(first_ts)));
    }
    if let Some(last_ts) = source.last_ts {
        target.last_ts = Some(target.last_ts.map_or(last_ts, |v| v.max(last_ts)));
    }
    target.input_text_bytes += source.input_text_bytes;
    target.output_text_bytes += source.output_text_bytes;
    target.log_bytes += source.log_bytes;
    target.attr_bytes += source.attr_bytes;
    target.external_id_bytes += source.external_id_bytes;
    target.field_bytes += source.field_bytes;
    target.estimated_bytes += source.estimated_bytes;
}

fn merge_storage_stats_reports(reports: Vec<StorageStatsReport>) -> StorageStatsReport {
    let mut total = StorageStatsBucket::default();
    let mut groups: std::collections::BTreeMap<
        std::collections::BTreeMap<String, String>,
        StorageStatsBucket,
    > = std::collections::BTreeMap::new();
    for mut report in reports {
        merge_storage_bucket(&mut total, report.total);
        for group in report.groups.drain(..) {
            let key = group.key.clone();
            let bucket = groups
                .entry(key.clone())
                .or_insert_with(|| StorageStatsBucket {
                    key,
                    ..StorageStatsBucket::default()
                });
            merge_storage_bucket(bucket, group);
        }
    }
    let mut groups: Vec<StorageStatsBucket> = groups.into_values().collect();
    groups.sort_by(|a, b| {
        b.estimated_bytes
            .cmp(&a.estimated_bytes)
            .then_with(|| b.trace_ids.len().cmp(&a.trace_ids.len()))
            .then_with(|| a.key.cmp(&b.key))
    });
    StorageStatsReport { total, groups }
}

fn merge_retention_plan_outcomes(shards: &[ShardRetentionPlanOutcome]) -> RetentionPlanOutcome {
    let mut candidate_stats = StorageStatsBucket::default();
    let mut protected_stats = StorageStatsBucket::default();
    let mut deletable_stats = StorageStatsBucket::default();
    let mut protected = std::collections::BTreeMap::<u64, Vec<String>>::new();
    let mut deletable_trace_ids = std::collections::HashSet::<u64>::new();
    let mut applied: Option<crate::RetentionDeleteResult> = None;
    let mut compacted: Option<crate::RetentionCompactResult> = None;

    for shard in shards {
        let outcome = &shard.outcome;
        merge_storage_bucket(&mut candidate_stats, outcome.candidate_stats.clone());
        merge_storage_bucket(&mut protected_stats, outcome.protected_stats.clone());
        merge_storage_bucket(&mut deletable_stats, outcome.deletable_stats.clone());
        for (trace_id, reasons) in &outcome.protected {
            let entry = protected.entry(*trace_id).or_default();
            for reason in reasons {
                if !entry.iter().any(|existing| existing == reason) {
                    entry.push(reason.clone());
                }
            }
        }
        deletable_trace_ids.extend(outcome.deletable_trace_ids.iter().copied());
        if let Some(result) = &outcome.applied {
            let target = applied.get_or_insert_with(crate::RetentionDeleteResult::default);
            target.requested_trace_count += result.requested_trace_count;
            target.deleted_trace_count += result.deleted_trace_count;
            target.deleted_segment_row_count += result.deleted_segment_row_count;
            target.skipped_live_trace_count += result.skipped_live_trace_count;
            target
                .deleted_trace_ids
                .extend(result.deleted_trace_ids.iter().copied());
            target
                .skipped_live_trace_ids
                .extend(result.skipped_live_trace_ids.iter().copied());
        }
        if let Some(result) = &outcome.compacted {
            let target = compacted.get_or_insert_with(crate::RetentionCompactResult::default);
            target.before_live_segment_count += result.before_live_segment_count;
            target.after_live_segment_count += result.after_live_segment_count;
            target.before_dead_segment_count += result.before_dead_segment_count;
            target.after_dead_segment_count += result.after_dead_segment_count;
            target.selected_segment_count += result.selected_segment_count;
            target.compacted_segment_count += result.compacted_segment_count;
            target.reclaimed_segment_count += result.reclaimed_segment_count;
            target.dropped_deleted_row_count += result.dropped_deleted_row_count;
            target.rewritten_live_row_count += result.rewritten_live_row_count;
            target
                .selected_segment_ids
                .extend(result.selected_segment_ids.iter().copied());
        }
    }

    if let Some(result) = &mut applied {
        result.deleted_trace_ids.sort_unstable();
        result.deleted_trace_ids.dedup();
        result.skipped_live_trace_ids.sort_unstable();
        result.skipped_live_trace_ids.dedup();
    }
    if let Some(result) = &mut compacted {
        result.selected_segment_ids.sort_unstable();
    }

    RetentionPlanOutcome {
        candidate_stats,
        protected_stats,
        deletable_stats,
        protected,
        deletable_trace_ids,
        applied,
        compacted,
        audit: None,
    }
}

fn storage_group_value_json(
    s: &FoldedSpan,
    key: &str,
    bounds: &std::collections::BTreeMap<u64, (i64, i64)>,
    time_bucket_ns: u64,
) -> String {
    match key {
        "time" => {
            let Some((first_ts, _)) = bounds.get(&s.trace_id) else {
                return "null".to_string();
            };
            let width = (time_bucket_ns.max(1).min(i64::MAX as u64)) as i64;
            let bucket = first_ts.div_euclid(width) * width;
            json_string_value(&bucket.to_string())
        }
        "trace_id" => json_string_value(&s.trace_id.to_string()),
        "span_id" => json_string_value(&s.span_id.to_string()),
        "session_id" => s
            .external_session_id
            .as_deref()
            .map(json_string_value)
            .or_else(|| s.session_id.map(|id| json_string_value(&id.to_string())))
            .unwrap_or_else(|| "null".to_string()),
        "agent_name" => s
            .agent_name
            .as_deref()
            .map(json_string_value)
            .unwrap_or_else(|| "null".to_string()),
        "tool_name" => s
            .tool_name
            .as_deref()
            .map(json_string_value)
            .unwrap_or_else(|| "null".to_string()),
        "kind" => json_string_value(folded_kind(s)),
        "status" => s
            .status
            .map(|status| status.to_string())
            .unwrap_or_else(|| "null".to_string()),
        _ => crate::folded_span_attr_value(s, key)
            .map(storage_compact_or_string_json)
            .unwrap_or_else(|| "null".to_string()),
    }
}

fn storage_group_value_json_rollup(
    row: &crate::TraceAggregateRollupRow,
    key: &str,
    trace_bounds: &std::collections::BTreeMap<u64, (i64, i64)>,
    time_bucket_ns: u64,
) -> String {
    match key {
        "time" => {
            let Some((first_ts, _)) = trace_bounds.get(&row.trace_id) else {
                return "null".to_string();
            };
            let width = (time_bucket_ns.max(1).min(i64::MAX as u64)) as i64;
            let bucket = first_ts.div_euclid(width) * width;
            json_string_value(&bucket.to_string())
        }
        "trace_id" => json_string_value(&row.trace_id.to_string()),
        "span_id" => json_string_value(&row.span_id.to_string()),
        "session_id" => row
            .external_session_id
            .as_deref()
            .map(json_string_value)
            .or_else(|| row.session_id.map(|id| json_string_value(&id.to_string())))
            .unwrap_or_else(|| "null".to_string()),
        "agent_name" => row
            .agent_name
            .as_deref()
            .map(json_string_value)
            .unwrap_or_else(|| "null".to_string()),
        "tool_name" => row
            .tool_name
            .as_deref()
            .map(json_string_value)
            .unwrap_or_else(|| "null".to_string()),
        "kind" => json_string_value(row.kind()),
        "status" => row
            .status
            .map(|status| status.to_string())
            .unwrap_or_else(|| "null".to_string()),
        _ => row
            .attr_value(key)
            .map(storage_compact_or_string_json)
            .unwrap_or_else(|| "null".to_string()),
    }
}

fn storage_compact_or_string_json(value: &str) -> String {
    match crate::wire::parse(value) {
        Ok(v) => v.to_compact_json(),
        Err(_) => json_string_value(value),
    }
}

fn storage_bucket_add_span(
    bucket: &mut StorageStatsBucket,
    s: &FoldedSpan,
    bounds: &std::collections::BTreeMap<u64, (i64, i64)>,
) {
    bucket.span_count += 1;
    bucket.event_count += s.event_count;
    bucket.trace_ids.insert(s.trace_id);
    if let Some(session_id) = s.session_id {
        bucket.session_ids.insert(session_id);
    }
    if s.status.unwrap_or(0) != 0 {
        bucket.error_span_count += 1;
    }
    if let Some((first_ts, last_ts)) = bounds.get(&s.trace_id) {
        bucket.first_ts = Some(bucket.first_ts.map_or(*first_ts, |v| v.min(*first_ts)));
        bucket.last_ts = Some(bucket.last_ts.map_or(*last_ts, |v| v.max(*last_ts)));
    }

    let input_bytes = s.input_text.as_deref().map(str::len).unwrap_or(0) as u64;
    let output_bytes = s.output_text.as_deref().map(str::len).unwrap_or(0) as u64;
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

    bucket.input_text_bytes += input_bytes;
    bucket.output_text_bytes += output_bytes;
    bucket.log_bytes += log_bytes;
    bucket.attr_bytes += attr_bytes;
    bucket.external_id_bytes += external_id_bytes;
    bucket.field_bytes += field_bytes;
    bucket.estimated_bytes += input_bytes
        + output_bytes
        + log_bytes
        + attr_bytes
        + external_id_bytes
        + field_bytes
        + (s.event_count as u64 * 64)
        + 128;
}

fn storage_bucket_add_rollup_row(
    bucket: &mut StorageStatsBucket,
    row: &crate::TraceAggregateRollupRow,
    trace_bounds: &std::collections::BTreeMap<u64, (i64, i64)>,
) {
    bucket.span_count += 1;
    bucket.event_count += row.event_count;
    bucket.trace_ids.insert(row.trace_id);
    if let Some(session_id) = row.session_id {
        bucket.session_ids.insert(session_id);
    }
    if row.status.unwrap_or(0) != 0 {
        bucket.error_span_count += 1;
    }
    if let Some((first_ts, last_ts)) = trace_bounds.get(&row.trace_id) {
        bucket.first_ts = Some(bucket.first_ts.map_or(*first_ts, |v| v.min(*first_ts)));
        bucket.last_ts = Some(bucket.last_ts.map_or(*last_ts, |v| v.max(*last_ts)));
    }
    bucket.input_text_bytes += row.input_text_bytes;
    bucket.output_text_bytes += row.output_text_bytes;
    bucket.log_bytes += row.log_bytes;
    bucket.attr_bytes += row.attr_bytes;
    bucket.external_id_bytes += row.external_id_bytes;
    bucket.field_bytes += row.field_bytes;
    bucket.estimated_bytes += row.estimated_bytes;
}

fn storage_bucket_apply_metadata_counts(
    bucket: &mut StorageStatsBucket,
    metadata: &StorageMetadata,
) {
    bucket.annotation_count = metadata
        .annotations
        .iter()
        .filter(|a| bucket.trace_ids.contains(&a.trace_id))
        .count();
    bucket.dataset_association_count = metadata
        .dataset_associations
        .iter()
        .filter(|a| bucket.trace_ids.contains(&a.trace_id))
        .count();
    bucket.golden_path_count = metadata
        .golden_paths
        .iter()
        .filter(|g| bucket.trace_ids.contains(&g.source_trace_id))
        .count();
    bucket.snapshot_ref_count = metadata
        .dataset_associations
        .iter()
        .filter(|a| bucket.trace_ids.contains(&a.trace_id) && dataset_association_has_snapshot(a))
        .count()
        + metadata
            .golden_paths
            .iter()
            .filter(|g| {
                bucket.trace_ids.contains(&g.source_trace_id) && golden_path_has_snapshot(g)
            })
            .count();
    bucket.eval_link_count = metadata
        .dataset_associations
        .iter()
        .filter(|a| bucket.trace_ids.contains(&a.trace_id) && dataset_association_is_eval_link(a))
        .count()
        + metadata
            .annotations
            .iter()
            .filter(|a| {
                bucket.trace_ids.contains(&a.trace_id) && metadata_attrs_have_eval_link(&a.attrs)
            })
            .count()
        + metadata
            .golden_paths
            .iter()
            .filter(|g| {
                bucket.trace_ids.contains(&g.source_trace_id)
                    && metadata_attrs_have_eval_link(&g.attrs)
            })
            .count();
    bucket.path_memory_ref_count = metadata
        .dataset_associations
        .iter()
        .filter(|a| {
            bucket.trace_ids.contains(&a.trace_id) && metadata_attrs_have_path_memory(&a.attrs)
        })
        .count()
        + metadata
            .annotations
            .iter()
            .filter(|a| {
                bucket.trace_ids.contains(&a.trace_id) && metadata_attrs_have_path_memory(&a.attrs)
            })
            .count()
        + metadata
            .golden_paths
            .iter()
            .filter(|g| {
                bucket.trace_ids.contains(&g.source_trace_id)
                    && metadata_attrs_have_path_memory(&g.attrs)
            })
            .count();
}

fn storage_bucket_for_trace_ids(
    spans: &[FoldedSpan],
    bounds: &std::collections::BTreeMap<u64, (i64, i64)>,
    trace_ids: &std::collections::HashSet<u64>,
) -> StorageStatsBucket {
    let mut bucket = StorageStatsBucket::default();
    for span in spans {
        if trace_ids.contains(&span.trace_id) {
            storage_bucket_add_span(&mut bucket, span, bounds);
        }
    }
    bucket
}

fn json_storage_stats_report(report: &StorageStatsReport, group_by: &[String]) -> String {
    let group_by_json = json_string_array(group_by);
    let groups = report
        .groups
        .iter()
        .map(|bucket| json_storage_bucket(bucket, true))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"{{"groupBy":{},"total":{},"groups":[{}]}}"#,
        group_by_json,
        json_storage_bucket(&report.total, false),
        groups,
    )
}

fn json_storage_stats_report_with_read_plan(
    report: &StorageStatsReport,
    group_by: &[String],
    read_plan: &str,
) -> String {
    append_storage_read_plan(json_storage_stats_report(report, group_by), read_plan)
}

fn append_storage_read_plan(body: String, read_plan: &str) -> String {
    if let Some(stripped) = body.strip_suffix('}') {
        format!(r#"{stripped},"readPlan":{read_plan}}}"#)
    } else {
        body
    }
}

fn json_storage_stats_report_with_cluster(
    report: &StorageStatsReport,
    group_by: &[String],
    shard_count: usize,
    extra_fields: &str,
) -> String {
    let group_by_json = json_string_array(group_by);
    let groups = report
        .groups
        .iter()
        .map(|bucket| json_storage_bucket(bucket, true))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"{{"groupBy":{},"total":{},"groups":[{}],"queryMode":"fanout_merge","shardCount":{}{}}}"#,
        group_by_json,
        json_storage_bucket(&report.total, false),
        groups,
        shard_count,
        extra_fields,
    )
}

fn json_storage_bucket(bucket: &StorageStatsBucket, include_key: bool) -> String {
    let payload_bytes = bucket.input_text_bytes + bucket.output_text_bytes + bucket.log_bytes;
    let key = if include_key {
        format!(r#""key":{},"#, json_attrs(&bucket.key))
    } else {
        String::new()
    };
    format!(
        r#"{{{}"traceCount":{},"spanCount":{},"sessionCount":{},"eventCount":{},"errorSpanCount":{},"firstTs":{},"lastTs":{},"bytes":{{"inputText":{},"outputText":{},"logs":{},"payload":{},"attrs":{},"externalIds":{},"fields":{},"estimated":{},"estimatedBytes":{}}},"metadata":{{"annotations":{},"datasetAssociations":{},"goldenPaths":{},"snapshotRefs":{},"evalLinks":{},"pathMemoryRefs":{}}}}}"#,
        key,
        bucket.trace_ids.len(),
        bucket.span_count,
        bucket.session_ids.len(),
        bucket.event_count,
        bucket.error_span_count,
        json_opt_i64(bucket.first_ts),
        json_opt_i64(bucket.last_ts),
        bucket.input_text_bytes,
        bucket.output_text_bytes,
        bucket.log_bytes,
        payload_bytes,
        bucket.attr_bytes,
        bucket.external_id_bytes,
        bucket.field_bytes,
        bucket.estimated_bytes,
        bucket.estimated_bytes,
        bucket.annotation_count,
        bucket.dataset_association_count,
        bucket.golden_path_count,
        bucket.snapshot_ref_count,
        bucket.eval_link_count,
        bucket.path_memory_ref_count,
    )
}

fn json_opt_i64(value: Option<i64>) -> String {
    value
        .map(|v| v.to_string())
        .unwrap_or_else(|| "null".to_string())
}
