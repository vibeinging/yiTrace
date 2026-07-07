fn trace_aggregate_group_fields(
    v: &crate::wire::Json,
) -> Result<Vec<TraceAggregateGroupField>, String> {
    let Some(raw) = json_field_alias(v, &["group_by", "groupBy", "by"]) else {
        return Err("groupBy required".to_string());
    };
    let names: Vec<String> = match raw {
        crate::wire::Json::Str(s) => vec![s.clone()],
        crate::wire::Json::Arr(items) => items
            .iter()
            .filter_map(|item| item.as_str().map(ToString::to_string))
            .collect(),
        _ => Vec::new(),
    };
    let fields: Vec<TraceAggregateGroupField> = names
        .iter()
        .filter_map(|name| trace_aggregate_group_field(name))
        .collect();
    if fields.is_empty() {
        Err("groupBy must include at least one supported field".to_string())
    } else {
        Ok(fields)
    }
}

fn trace_aggregate_group_field(name: &str) -> Option<TraceAggregateGroupField> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    let no_sep = lower.replace(['_', '-', '.'], "");
    let (output_key, kind) = match no_sep.as_str() {
        "projectid" => (
            "project_id".to_string(),
            TraceAggregateGroupKind::Attr("project_id".to_string()),
        ),
        "skill" => (
            "skill".to_string(),
            TraceAggregateGroupKind::Attr("skill".to_string()),
        ),
        "mode" => (
            "mode".to_string(),
            TraceAggregateGroupKind::Attr("mode".to_string()),
        ),
        "callsite" => (
            "call_site".to_string(),
            TraceAggregateGroupKind::Attr("call_site".to_string()),
        ),
        "taskfingerprint" => (
            "task_fingerprint".to_string(),
            TraceAggregateGroupKind::Attr("task_fingerprint".to_string()),
        ),
        "loopid" => (
            "loop_id".to_string(),
            TraceAggregateGroupKind::Attr("loop_id".to_string()),
        ),
        "harnessversion" => (
            "harness_version".to_string(),
            TraceAggregateGroupKind::Attr("harness_version".to_string()),
        ),
        "schemafingerprint" => (
            "schema_fingerprint".to_string(),
            TraceAggregateGroupKind::Attr("schema_fingerprint".to_string()),
        ),
        "intentsignature" => (
            "intent_signature".to_string(),
            TraceAggregateGroupKind::Attr("intent_signature".to_string()),
        ),
        "validationstatus" => (
            "validation_status".to_string(),
            TraceAggregateGroupKind::Attr("validation_status".to_string()),
        ),
        "reviewstatus" => (
            "review_status".to_string(),
            TraceAggregateGroupKind::Attr("review_status".to_string()),
        ),
        "evalstatus" => (
            "eval_status".to_string(),
            TraceAggregateGroupKind::Attr("eval_status".to_string()),
        ),
        "pathmemoryid" => (
            "path_memory_id".to_string(),
            TraceAggregateGroupKind::Attr("path_memory_id".to_string()),
        ),
        "stopreason" => (
            "stop_reason".to_string(),
            TraceAggregateGroupKind::Attr("stop_reason".to_string()),
        ),
        "phase" => (
            "phase".to_string(),
            TraceAggregateGroupKind::Attr("phase".to_string()),
        ),
        "validator" => (
            "validator".to_string(),
            TraceAggregateGroupKind::Attr("validator".to_string()),
        ),
        "agentname" => ("agentName".to_string(), TraceAggregateGroupKind::AgentName),
        "toolname" => ("toolName".to_string(), TraceAggregateGroupKind::ToolName),
        "model" => ("model".to_string(), TraceAggregateGroupKind::Model),
        "provider" => ("provider".to_string(), TraceAggregateGroupKind::Provider),
        "kind" | "spankind" => ("kind".to_string(), TraceAggregateGroupKind::Kind),
        "status" => ("status".to_string(), TraceAggregateGroupKind::Status),
        _ => {
            let attr = trimmed
                .strip_prefix("attrs.")
                .or_else(|| trimmed.strip_prefix("attr."))
                .unwrap_or(trimmed)
                .to_string();
            (attr.clone(), TraceAggregateGroupKind::Attr(attr))
        }
    };
    Some(TraceAggregateGroupField { output_key, kind })
}

fn trace_aggregate_buckets(
    spans: &[FoldedSpan],
    fields: &[TraceAggregateGroupField],
) -> Vec<TraceAggregateBucket> {
    let mut by_key: std::collections::BTreeMap<Vec<String>, TraceAggregateBucket> =
        std::collections::BTreeMap::new();
    for s in spans {
        let values: Vec<String> = fields
            .iter()
            .map(|field| trace_aggregate_value_json(s, &field.kind))
            .collect();
        let bucket = by_key
            .entry(values.clone())
            .or_insert_with(|| TraceAggregateBucket {
                values,
                span_count: 0,
                trace_ids: std::collections::HashSet::new(),
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
                examples: Vec::new(),
            });
        bucket.span_count += 1;
        bucket.trace_ids.insert(s.trace_id);
        if s.status.unwrap_or(0) != 0 {
            bucket.error_count += 1;
        }
        if let Some(duration) = s.duration_ns {
            bucket.duration_sum_ns += duration as u128;
            bucket.duration_max_ns = bucket.duration_max_ns.max(duration);
            bucket.durations_ns.push(duration);
        }
        bucket.input_tokens += s.input_tokens.unwrap_or(0);
        bucket.output_tokens += s.output_tokens.unwrap_or(0);
        bucket.cached_input_tokens += s.cached_input_tokens.unwrap_or(0);
        bucket.reasoning_tokens += s.reasoning_tokens.unwrap_or(0);
        bucket.total_tokens += folded_total_tokens(s);
        bucket.cost_usd_nanos += folded_cost_usd_nanos(s);
        if bucket.examples.len() < 3 {
            bucket.examples.push(TraceAggregateExample {
                trace_id: s.trace_id,
                span_id: s.span_id,
                external_trace_id: s.external_trace_id.clone(),
                external_span_id: s.external_span_id.clone(),
                name: folded_name(s),
            });
        }
    }
    by_key.into_values().collect()
}

fn trace_aggregate_buckets_from_rollup_rows(
    rows: &[crate::TraceAggregateRollupRow],
    fields: &[TraceAggregateGroupField],
) -> Vec<TraceAggregateBucket> {
    let mut by_key: std::collections::BTreeMap<Vec<String>, TraceAggregateBucket> =
        std::collections::BTreeMap::new();
    for row in rows {
        let values: Vec<String> = fields
            .iter()
            .map(|field| trace_aggregate_rollup_value_json(row, &field.kind))
            .collect();
        let bucket = by_key
            .entry(values.clone())
            .or_insert_with(|| TraceAggregateBucket {
                values,
                span_count: 0,
                trace_ids: std::collections::HashSet::new(),
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
                examples: Vec::new(),
            });
        bucket.span_count += 1;
        bucket.trace_ids.insert(row.trace_id);
        if row.status.unwrap_or(0) != 0 {
            bucket.error_count += 1;
        }
        if let Some(duration) = row.duration_ns {
            bucket.duration_sum_ns += duration as u128;
            bucket.duration_max_ns = bucket.duration_max_ns.max(duration);
            bucket.durations_ns.push(duration);
        }
        bucket.input_tokens += row.input_tokens;
        bucket.output_tokens += row.output_tokens;
        bucket.cached_input_tokens += row.cached_input_tokens;
        bucket.reasoning_tokens += row.reasoning_tokens;
        bucket.total_tokens += row.total_tokens;
        bucket.cost_usd_nanos += row.cost_usd_nanos;
        if bucket.examples.len() < 3 {
            bucket.examples.push(TraceAggregateExample {
                trace_id: row.trace_id,
                span_id: row.span_id,
                external_trace_id: row.external_trace_id.clone(),
                external_span_id: row.external_span_id.clone(),
                name: row.name(),
            });
        }
    }
    by_key.into_values().collect()
}

fn trace_aggregate_buckets_from_preaggregate_buckets(
    buckets: &[crate::TraceAggregatePreaggregateBucket],
    fields: &[TraceAggregateGroupField],
) -> Vec<TraceAggregateBucket> {
    let mut by_key: std::collections::BTreeMap<Vec<String>, TraceAggregateBucket> =
        std::collections::BTreeMap::new();
    for source in buckets {
        let values: Vec<String> = fields
            .iter()
            .map(|field| {
                trace_aggregate_preaggregate_field_name(&field.kind)
                    .and_then(|name| source.key.get(&name).cloned())
                    .unwrap_or_else(|| "null".to_string())
            })
            .collect();
        let bucket = by_key
            .entry(values.clone())
            .or_insert_with(|| TraceAggregateBucket {
                values,
                span_count: 0,
                trace_ids: std::collections::HashSet::new(),
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
                examples: Vec::new(),
            });
        bucket.trace_ids.extend(source.trace_ids.iter().copied());
        bucket.span_count += source.span_count;
        bucket.error_count += source.error_count;
        bucket.duration_sum_ns += source.duration_sum_ns;
        bucket.duration_max_ns = bucket.duration_max_ns.max(source.duration_max_ns);
        bucket.durations_ns.extend(source.durations_ns.iter().copied());
        bucket.input_tokens += source.input_tokens;
        bucket.output_tokens += source.output_tokens;
        bucket.cached_input_tokens += source.cached_input_tokens;
        bucket.reasoning_tokens += source.reasoning_tokens;
        bucket.total_tokens += source.total_tokens;
        bucket.cost_usd_nanos += source.cost_usd_nanos;
        for example in &source.examples {
            if bucket.examples.len() >= 3 {
                break;
            }
            bucket.examples.push(TraceAggregateExample {
                trace_id: example.trace_id,
                span_id: example.span_id,
                external_trace_id: example.external_trace_id.clone(),
                external_span_id: example.external_span_id.clone(),
                name: example.name.clone(),
            });
        }
    }
    by_key.into_values().collect()
}

fn trace_aggregate_value_json(s: &FoldedSpan, kind: &TraceAggregateGroupKind) -> String {
    match kind {
        TraceAggregateGroupKind::Attr(key) => crate::folded_span_attr_value(s, key)
            .map(ToString::to_string)
            .unwrap_or_else(|| "null".to_string()),
        TraceAggregateGroupKind::AgentName => s
            .agent_name
            .as_deref()
            .map(json_string_value)
            .unwrap_or_else(|| "null".to_string()),
        TraceAggregateGroupKind::ToolName => s
            .tool_name
            .as_deref()
            .map(json_string_value)
            .unwrap_or_else(|| "null".to_string()),
        TraceAggregateGroupKind::Model => s
            .model
            .as_deref()
            .map(json_string_value)
            .unwrap_or_else(|| "null".to_string()),
        TraceAggregateGroupKind::Provider => s
            .provider
            .as_deref()
            .map(json_string_value)
            .unwrap_or_else(|| "null".to_string()),
        TraceAggregateGroupKind::Kind => json_string_value(folded_kind(s)),
        TraceAggregateGroupKind::Status => s
            .status
            .map(|status| status.to_string())
            .unwrap_or_else(|| "null".to_string()),
    }
}

fn trace_aggregate_rollup_value_json(
    row: &crate::TraceAggregateRollupRow,
    kind: &TraceAggregateGroupKind,
) -> String {
    match kind {
        TraceAggregateGroupKind::Attr(key) => row
            .attr_value(key)
            .map(ToString::to_string)
            .unwrap_or_else(|| "null".to_string()),
        TraceAggregateGroupKind::AgentName => row
            .agent_name
            .as_deref()
            .map(json_string_value)
            .unwrap_or_else(|| "null".to_string()),
        TraceAggregateGroupKind::ToolName => row
            .tool_name
            .as_deref()
            .map(json_string_value)
            .unwrap_or_else(|| "null".to_string()),
        TraceAggregateGroupKind::Model => row
            .model
            .as_deref()
            .map(json_string_value)
            .unwrap_or_else(|| "null".to_string()),
        TraceAggregateGroupKind::Provider => row
            .provider
            .as_deref()
            .map(json_string_value)
            .unwrap_or_else(|| "null".to_string()),
        TraceAggregateGroupKind::Kind => json_string_value(row.kind()),
        TraceAggregateGroupKind::Status => row
            .status
            .map(|status| status.to_string())
            .unwrap_or_else(|| "null".to_string()),
    }
}

fn sort_trace_aggregate_buckets(buckets: &mut [TraceAggregateBucket], sort_by: &str, desc: bool) {
    let sort = sort_by.to_ascii_lowercase().replace(['_', '-'], "");
    buckets.sort_by(|a, b| {
        let ord = match sort.as_str() {
            "tracecount" | "traces" => a.trace_ids.len().cmp(&b.trace_ids.len()),
            "errorcount" | "errors" => a.error_count.cmp(&b.error_count),
            "errorrate" => (a.error_count as u128 * b.span_count as u128)
                .cmp(&(b.error_count as u128 * a.span_count as u128)),
            "duration" | "durationns" | "durationsum" => a.duration_sum_ns.cmp(&b.duration_sum_ns),
            "avgduration" | "durationavg" => {
                aggregate_avg_duration_ns(a).cmp(&aggregate_avg_duration_ns(b))
            }
            "maxduration" | "durationmax" => a.duration_max_ns.cmp(&b.duration_max_ns),
            "cost" | "costusd" => a.cost_usd_nanos.cmp(&b.cost_usd_nanos),
            "tokens" | "totaltokens" => a.total_tokens.cmp(&b.total_tokens),
            _ => a.span_count.cmp(&b.span_count),
        };
        let ord = if desc { ord.reverse() } else { ord };
        ord.then_with(|| a.values.cmp(&b.values))
    });
}

fn aggregate_avg_duration_ns(bucket: &TraceAggregateBucket) -> u128 {
    if bucket.durations_ns.is_empty() {
        0
    } else {
        bucket.duration_sum_ns / bucket.durations_ns.len() as u128
    }
}

fn trace_aggregate_planner_fields_json(
    fields: &[TraceAggregateGroupField],
    request: &TraceSearchRequest,
    stats: &AttrIndexedReadStats,
    folded_span_count: usize,
    rollup_stats: Option<&crate::TraceAggregateRollupStats>,
    rollup_fallback_reason: Option<&str>,
    preaggregate_profile: Option<&[String]>,
) -> String {
    let blockers = trace_aggregate_rollup_blockers(fields, request);
    let eligible = blockers.is_empty();
    let planner = if preaggregate_profile.is_some() {
        "aggregate_preaggregate_tail_overlay"
    } else if let Some(stats) = rollup_stats {
        if stats.used_segment_rollup {
            "segment_rollup_tail_overlay"
        } else {
            "tail_only_query_time_reduce"
        }
    } else if eligible && rollup_fallback_reason.is_some() {
        "rollup_safety_fallback_folded_scan"
    } else if eligible {
        "rollup_candidate_folded_scan"
    } else {
        "query_time_reduce"
    };
    let blockers = blockers
        .iter()
        .map(|s| json_string_value(s))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#","aggregationPlanner":"{}","rollupEligible":{},"rollupBlockedBy":[{}],"readPlan":{}"#,
        planner,
        eligible,
        blockers,
        trace_aggregate_read_plan_json(
            stats,
            folded_span_count,
            rollup_stats,
            rollup_fallback_reason,
            preaggregate_profile,
        )
    )
}

fn trace_aggregate_read_plan_json(
    stats: &AttrIndexedReadStats,
    folded_span_count: usize,
    rollup_stats: Option<&crate::TraceAggregateRollupStats>,
    rollup_fallback_reason: Option<&str>,
    preaggregate_profile: Option<&[String]>,
) -> String {
    if let Some(rollup) = rollup_stats {
        let span_read_index = if preaggregate_profile.is_some() {
            "aggregate_preaggregate"
        } else if rollup.used_segment_rollup {
            "segment_rollup"
        } else {
            "tail_folded_scan"
        };
        let preaggregate_profile = preaggregate_profile
            .map(json_string_array)
            .unwrap_or_else(|| "null".to_string());
        let verification = if span_read_index == "aggregate_preaggregate" {
            "preaggregate_scope_safety"
        } else {
            "rollup_scope_safety"
        };
        return format!(
            r#"{{"spanReadIndex":"{}","usedSegmentRollup":{},"segmentRollupSegments":{},"segmentRollupRows":{},"tailFoldedSpanCount":{},"aggregatePreaggregateProfile":{},"usedAttrPostings":false,"candidateSpanKeys":null,"scannedSegments":0,"foldedSpanCount":{},"unsupportedAttrKeys":[],"verification":"{}","rollupFallbackReason":null}}"#,
            span_read_index,
            rollup.used_segment_rollup,
            rollup.segment_rollup_segments,
            rollup.segment_rollup_rows,
            rollup.tail_folded_span_count,
            preaggregate_profile,
            folded_span_count,
            verification,
        );
    }
    let span_read_index = if stats.used_attr_postings {
        "attrs_postings"
    } else {
        "folded_scan"
    };
    let candidate_span_keys = stats
        .candidate_span_keys
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_string());
    let unsupported_attr_keys = stats
        .unsupported_attr_keys
        .iter()
        .map(|key| json_string_value(key))
        .collect::<Vec<_>>()
        .join(",");
    let verification = if stats.used_attr_postings {
        "folded_attrs_verify"
    } else {
        "none"
    };
    let fallback = rollup_fallback_reason
        .map(json_string_value)
        .unwrap_or_else(|| "null".to_string());
    format!(
        r#"{{"spanReadIndex":"{}","usedSegmentRollup":false,"segmentRollupSegments":0,"segmentRollupRows":0,"tailFoldedSpanCount":0,"usedAttrPostings":{},"candidateSpanKeys":{},"scannedSegments":{},"foldedSpanCount":{},"unsupportedAttrKeys":[{}],"verification":"{}","rollupFallbackReason":{}}}"#,
        span_read_index,
        stats.used_attr_postings,
        candidate_span_keys,
        stats.scanned_segments,
        folded_span_count,
        unsupported_attr_keys,
        verification,
        fallback
    )
}

fn trace_aggregate_preaggregate_fields(
    fields: &[TraceAggregateGroupField],
    request: &TraceSearchRequest,
) -> Option<Vec<String>> {
    let mut profile = std::collections::BTreeSet::new();
    for field in fields {
        let name = trace_aggregate_preaggregate_field_name(&field.kind)?;
        if !trace_aggregate_preaggregate_field_supported(&name) {
            return None;
        }
        profile.insert(name);
    }
    let spec = &request.spec;
    if spec.status.is_some() {
        profile.insert("status".to_string());
    }
    if spec.kind.is_some() {
        profile.insert("kind".to_string());
    }
    if spec.agent_name.is_some() {
        profile.insert("agent_name".to_string());
    }
    if spec.tool_name.is_some() {
        profile.insert("tool_name".to_string());
    }
    if spec.model.is_some() {
        profile.insert("model".to_string());
    }
    for key in spec.attrs.keys() {
        if !trace_aggregate_preaggregate_field_supported(key) {
            return None;
        }
        profile.insert(key.clone());
    }
    let profile: Vec<String> = profile.into_iter().collect();
    if crate::trace_aggregate_preaggregate_profile_supported(&profile) {
        Some(profile)
    } else {
        None
    }
}

fn trace_aggregate_preaggregate_field_name(kind: &TraceAggregateGroupKind) -> Option<String> {
    match kind {
        TraceAggregateGroupKind::Attr(key) => Some(key.clone()),
        TraceAggregateGroupKind::AgentName => Some("agent_name".to_string()),
        TraceAggregateGroupKind::ToolName => Some("tool_name".to_string()),
        TraceAggregateGroupKind::Model => Some("model".to_string()),
        TraceAggregateGroupKind::Provider => Some("provider".to_string()),
        TraceAggregateGroupKind::Kind => Some("kind".to_string()),
        TraceAggregateGroupKind::Status => Some("status".to_string()),
    }
}

fn trace_aggregate_preaggregate_field_supported(field: &str) -> bool {
    matches!(
        field,
        "project_id"
            | "task_fingerprint"
            | "validation_status"
            | "tool_name"
            | "agent_name"
            | "skill"
            | "mode"
            | "status"
            | "kind"
    )
}

fn trace_aggregate_rollup_blockers(
    fields: &[TraceAggregateGroupField],
    request: &TraceSearchRequest,
) -> Vec<String> {
    let mut blockers = Vec::new();
    if !request.annotation.active && !request.dataset.active {
        // ok
    } else {
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
    for field in fields {
        if !trace_aggregate_rollup_dimension_supported(&field.kind) {
            let blocker = format!("unsupported_group_by:{}", field.output_key);
            if !blockers.contains(&blocker) {
                blockers.push(blocker);
            }
        }
    }
    blockers
}

fn trace_aggregate_rollup_filters(
    request: &TraceSearchRequest,
) -> crate::TraceAggregateRollupFilters {
    crate::TraceAggregateRollupFilters {
        status: request.spec.status,
        kind: request.spec.kind.clone(),
        agent_name: request.spec.agent_name.clone(),
        tool_name: request.spec.tool_name.clone(),
        model: request.spec.model.clone(),
        attrs: request.spec.attrs.clone(),
    }
}

fn trace_aggregate_rollup_dimension_supported(kind: &TraceAggregateGroupKind) -> bool {
    match kind {
        TraceAggregateGroupKind::Attr(key) => matches!(
            key.as_str(),
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
        ),
        TraceAggregateGroupKind::AgentName
        | TraceAggregateGroupKind::ToolName
        | TraceAggregateGroupKind::Model
        | TraceAggregateGroupKind::Provider
        | TraceAggregateGroupKind::Kind
        | TraceAggregateGroupKind::Status => true,
    }
}

fn trace_aggregate_bucket_json(
    bucket: &TraceAggregateBucket,
    fields: &[TraceAggregateGroupField],
) -> String {
    let key = fields
        .iter()
        .enumerate()
        .map(|(i, field)| {
            format!(
                r#""{}":{}"#,
                json_escape(&field.output_key),
                bucket
                    .values
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| "null".to_string())
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let examples = bucket
        .examples
        .iter()
        .map(trace_aggregate_example_json)
        .collect::<Vec<_>>()
        .join(",");
    let error_rate = if bucket.span_count == 0 {
        0.0
    } else {
        bucket.error_count as f64 / bucket.span_count as f64
    };
    format!(
        r#"{{"key":{{{key}}},"spanCount":{},"traceCount":{},"errorCount":{},"errorRate":{:.6},"durationNs":{},"usage":{},"costUsd":{},"costDetail":{},"examples":[{}]}}"#,
        bucket.span_count,
        bucket.trace_ids.len(),
        bucket.error_count,
        error_rate,
        aggregate_duration_json(bucket),
        usage_json(
            bucket.input_tokens,
            bucket.output_tokens,
            bucket.cached_input_tokens,
            bucket.reasoning_tokens,
            bucket.total_tokens,
        ),
        cost_usd_num_from_nanos(bucket.cost_usd_nanos),
        cost_detail_json(bucket.cost_usd_nanos, Some("USD"), "mixed"),
        examples,
    )
}

fn aggregate_duration_json(bucket: &TraceAggregateBucket) -> String {
    let mut durations = bucket.durations_ns.clone();
    durations.sort_unstable();
    duration_values_json(&durations, bucket.duration_sum_ns, bucket.duration_max_ns)
}

fn duration_values_json(
    sorted_durations: &[u64],
    duration_sum_ns: u128,
    duration_max_ns: u64,
) -> String {
    let count = sorted_durations.len();
    let avg = if count == 0 {
        "null".to_string()
    } else {
        (duration_sum_ns / count as u128).to_string()
    };
    let max = if count == 0 {
        "null".to_string()
    } else {
        duration_max_ns.to_string()
    };
    let p50 = percentile_json(sorted_durations, 50);
    let p95 = percentile_json(sorted_durations, 95);
    format!(
        r#"{{"sum":{},"avg":{},"max":{},"p50":{},"p95":{},"count":{}}}"#,
        duration_sum_ns, avg, max, p50, p95, count
    )
}
