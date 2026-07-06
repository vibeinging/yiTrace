fn loop_summary_buckets(spans: &[FoldedSpan]) -> Vec<LoopSummaryBucket> {
    let mut by_loop: std::collections::BTreeMap<String, LoopSummaryBucket> =
        std::collections::BTreeMap::new();
    for s in spans {
        let Some(loop_value) = crate::folded_span_attr_value(s, "loop_id") else {
            continue;
        };
        let bucket = by_loop
            .entry(loop_value.to_string())
            .or_insert_with(|| LoopSummaryBucket::new(loop_value.to_string()));
        bucket.span_count += 1;
        bucket.trace_ids.insert(s.trace_id);
        if let Some(session_id) = s.session_id {
            bucket.session_ids.insert(session_id);
        }
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
        bucket.first_trace_id = bucket.first_trace_id.min(s.trace_id);
        bucket.last_trace_id = bucket.last_trace_id.max(s.trace_id);
        collect_agent_fields_from_span(s, &mut bucket.fields);
        if let Some(phase) = crate::folded_span_attr_value(s, "phase") {
            bucket.phases.insert(json_compact_label(phase));
        }
        if let Some(validator) = crate::folded_span_attr_value(s, "validator") {
            bucket.validators.insert(json_compact_label(validator));
        }
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
    by_loop.into_values().collect()
}

fn collect_agent_fields_from_span(
    s: &FoldedSpan,
    fields: &mut std::collections::BTreeMap<String, String>,
) {
    for key in agent_field_keys() {
        if let Some(value) = crate::folded_span_attr_value(s, key) {
            fields
                .entry((*key).to_string())
                .or_insert_with(|| first_class_agent_field_json(key, value));
        }
    }
}

fn collect_agent_fields_from_rollup_row(
    row: &crate::TraceAggregateRollupRow,
    fields: &mut std::collections::BTreeMap<String, String>,
) {
    for key in agent_field_keys() {
        if let Some(value) = row.attr_value(key) {
            fields
                .entry((*key).to_string())
                .or_insert_with(|| first_class_agent_field_json(key, value));
        }
    }
}

fn loop_summary_buckets_from_rollup_rows(
    rows: &[crate::TraceAggregateRollupRow],
) -> Vec<LoopSummaryBucket> {
    let mut by_loop: std::collections::BTreeMap<String, LoopSummaryBucket> =
        std::collections::BTreeMap::new();
    for row in rows {
        let Some(loop_value) = row.attr_value("loop_id") else {
            continue;
        };
        let bucket = by_loop
            .entry(loop_value.to_string())
            .or_insert_with(|| LoopSummaryBucket::new(loop_value.to_string()));
        bucket.span_count += 1;
        bucket.trace_ids.insert(row.trace_id);
        if let Some(session_id) = row.session_id {
            bucket.session_ids.insert(session_id);
        }
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
        bucket.first_trace_id = bucket.first_trace_id.min(row.trace_id);
        bucket.last_trace_id = bucket.last_trace_id.max(row.trace_id);
        collect_agent_fields_from_rollup_row(row, &mut bucket.fields);
        if let Some(phase) = row.attr_value("phase") {
            bucket.phases.insert(json_compact_label(phase));
        }
        if let Some(validator) = row.attr_value("validator") {
            bucket.validators.insert(json_compact_label(validator));
        }
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
    by_loop.into_values().collect()
}

fn trace_summary_buckets_from_spans(spans: &[FoldedSpan]) -> Vec<TaskTraceSummaryBucket> {
    let mut by_trace: std::collections::BTreeMap<u64, TaskTraceSummaryBucket> =
        std::collections::BTreeMap::new();
    for s in spans {
        let bucket = by_trace
            .entry(s.trace_id)
            .or_insert_with(|| TaskTraceSummaryBucket::new(s));
        if bucket.external_trace_id.is_none() {
            bucket.external_trace_id = s.external_trace_id.clone();
        }
        bucket.span_count += 1;
        if s.status.unwrap_or(0) != 0 {
            bucket.error_count += 1;
        }
        if let Some(duration) = s.duration_ns {
            bucket.duration_sum_ns += duration as u128;
            bucket.duration_max_ns = bucket.duration_max_ns.max(duration);
        }
        bucket.input_tokens += s.input_tokens.unwrap_or(0);
        bucket.output_tokens += s.output_tokens.unwrap_or(0);
        bucket.cached_input_tokens += s.cached_input_tokens.unwrap_or(0);
        bucket.reasoning_tokens += s.reasoning_tokens.unwrap_or(0);
        bucket.total_tokens += folded_total_tokens(s);
        bucket.cost_usd_nanos += folded_cost_usd_nanos(s);
        collect_agent_fields_from_span(s, &mut bucket.fields);
    }
    by_trace.into_values().collect()
}

fn trace_summary_buckets_from_rollup_rows(
    rows: &[crate::TraceAggregateRollupRow],
) -> Vec<TaskTraceSummaryBucket> {
    let mut by_trace: std::collections::BTreeMap<u64, TaskTraceSummaryBucket> =
        std::collections::BTreeMap::new();
    for row in rows {
        let bucket = by_trace
            .entry(row.trace_id)
            .or_insert_with(|| TaskTraceSummaryBucket {
                trace_id: row.trace_id,
                external_trace_id: row.external_trace_id.clone(),
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
            });
        if bucket.external_trace_id.is_none() {
            bucket.external_trace_id = row.external_trace_id.clone();
        }
        bucket.span_count += 1;
        if row.status.unwrap_or(0) != 0 {
            bucket.error_count += 1;
        }
        if let Some(duration) = row.duration_ns {
            bucket.duration_sum_ns += duration as u128;
            bucket.duration_max_ns = bucket.duration_max_ns.max(duration);
        }
        bucket.input_tokens += row.input_tokens;
        bucket.output_tokens += row.output_tokens;
        bucket.cached_input_tokens += row.cached_input_tokens;
        bucket.reasoning_tokens += row.reasoning_tokens;
        bucket.total_tokens += row.total_tokens;
        bucket.cost_usd_nanos += row.cost_usd_nanos;
        collect_agent_fields_from_rollup_row(row, &mut bucket.fields);
    }
    by_trace.into_values().collect()
}

fn json_loop_summary_bucket(bucket: &LoopSummaryBucket) -> String {
    let error_rate = if bucket.span_count == 0 {
        0.0
    } else {
        bucket.error_count as f64 / bucket.span_count as f64
    };
    let phases = bucket
        .phases
        .iter()
        .map(|v| json_string_value(v))
        .collect::<Vec<_>>()
        .join(",");
    let validators = bucket
        .validators
        .iter()
        .map(|v| json_string_value(v))
        .collect::<Vec<_>>()
        .join(",");
    let examples = bucket
        .examples
        .iter()
        .map(trace_aggregate_example_json)
        .collect::<Vec<_>>()
        .join(",");
    let task = bucket
        .fields
        .get("task_fingerprint")
        .map(|v| json_string_value(&json_compact_label(v)))
        .unwrap_or_else(|| "null".to_string());
    format!(
        r#"{{"loopId":"{}","loopValue":{},"taskFingerprint":{},"status":"{}","spanCount":{},"traceCount":{},"sessionCount":{},"errorCount":{},"errorRate":{:.6},"firstTraceId":"{}","lastTraceId":"{}","durationNs":{},"usage":{},"costUsd":{},"costDetail":{},"phases":[{}],"validators":[{}],"fields":{},"examples":[{}]}}"#,
        json_escape(&bucket.loop_id),
        bucket.loop_value_json,
        task,
        if bucket.error_count > 0 {
            "error"
        } else {
            "ok"
        },
        bucket.span_count,
        bucket.trace_ids.len(),
        bucket.session_ids.len(),
        bucket.error_count,
        error_rate,
        if bucket.first_trace_id == u64::MAX {
            0
        } else {
            bucket.first_trace_id
        },
        bucket.last_trace_id,
        loop_duration_json(bucket),
        usage_json(
            bucket.input_tokens,
            bucket.output_tokens,
            bucket.cached_input_tokens,
            bucket.reasoning_tokens,
            bucket.total_tokens,
        ),
        cost_usd_num_from_nanos(bucket.cost_usd_nanos),
        cost_detail_json(bucket.cost_usd_nanos, Some("USD"), "mixed"),
        phases,
        validators,
        json_attrs(&bucket.fields),
        examples,
    )
}

fn loop_duration_json(bucket: &LoopSummaryBucket) -> String {
    let mut durations = bucket.durations_ns.clone();
    durations.sort_unstable();
    let count = durations.len();
    let avg = if count == 0 {
        "null".to_string()
    } else {
        (bucket.duration_sum_ns / count as u128).to_string()
    };
    let max = if count == 0 {
        "null".to_string()
    } else {
        bucket.duration_max_ns.to_string()
    };
    format!(
        r#"{{"sum":{},"avg":{},"max":{},"p50":{},"p95":{},"count":{}}}"#,
        bucket.duration_sum_ns,
        avg,
        max,
        percentile_json(&durations, 50),
        percentile_json(&durations, 95),
        count,
    )
}

fn json_task_trace_summary_bucket(bucket: &TaskTraceSummaryBucket) -> String {
    format!(
        r#"{{"traceId":"{}","externalTraceId":{},"spanCount":{},"errorCount":{},"status":"{}","durationNs":{{"sum":{},"max":{}}},"usage":{},"costUsd":{},"costDetail":{},"fields":{}}}"#,
        bucket.trace_id,
        json_opt_str(bucket.external_trace_id.as_deref()),
        bucket.span_count,
        bucket.error_count,
        if bucket.error_count > 0 {
            "error"
        } else {
            "ok"
        },
        bucket.duration_sum_ns,
        bucket.duration_max_ns,
        usage_json(
            bucket.input_tokens,
            bucket.output_tokens,
            bucket.cached_input_tokens,
            bucket.reasoning_tokens,
            bucket.total_tokens,
        ),
        cost_usd_num_from_nanos(bucket.cost_usd_nanos),
        cost_detail_json(bucket.cost_usd_nanos, Some("USD"), "mixed"),
        json_attrs(&bucket.fields),
    )
}

fn loop_task_index_label(
    stats: Option<&crate::TraceAggregateRollupStats>,
    segment_label: &'static str,
    tail_label: &'static str,
    fallback_label: &'static str,
) -> &'static str {
    match stats {
        Some(stats) if stats.used_segment_rollup => segment_label,
        Some(_) => tail_label,
        None => fallback_label,
    }
}

fn loop_task_read_plan_fields_json(
    stats: Option<&crate::TraceAggregateRollupStats>,
    fallback_reason: Option<&str>,
) -> String {
    if let Some(stats) = stats {
        return format!(
            r#","readPlan":{{"spanReadIndex":"{}","usedSegmentRollup":{},"segmentRollupSegments":{},"segmentRollupRows":{},"tailFoldedSpanCount":{},"rollupFallbackReason":null}}"#,
            if stats.used_segment_rollup {
                "loop_task_sidecar"
            } else {
                "tail_folded_scan"
            },
            stats.used_segment_rollup,
            stats.segment_rollup_segments,
            stats.segment_rollup_rows,
            stats.tail_folded_span_count,
        );
    }
    let fallback = fallback_reason
        .map(json_string_value)
        .unwrap_or_else(|| "null".to_string());
    format!(
        r#","readPlan":{{"spanReadIndex":"folded_scan","usedSegmentRollup":false,"segmentRollupSegments":0,"segmentRollupRows":0,"tailFoldedSpanCount":0,"rollupFallbackReason":{fallback}}}"#
    )
}

fn loop_span_contains(s: &FoldedSpan, needle: &str) -> bool {
    folded_contains(s, needle)
        || crate::folded_span_attr_value(s, "loop_id")
            .map(|v| json_compact_label(v).contains(needle))
            .unwrap_or(false)
        || crate::folded_span_attr_value(s, "task_fingerprint")
            .map(|v| json_compact_label(v).contains(needle))
            .unwrap_or(false)
}

fn json_compact_label(value: &str) -> String {
    match crate::wire::parse(value) {
        Ok(crate::wire::Json::Str(s)) => s,
        Ok(crate::wire::Json::Num(s)) => s,
        Ok(crate::wire::Json::Bool(v)) => v.to_string(),
        Ok(crate::wire::Json::Null) => "null".to_string(),
        Ok(other) => other.to_compact_json(),
        Err(_) => value.to_string(),
    }
}
