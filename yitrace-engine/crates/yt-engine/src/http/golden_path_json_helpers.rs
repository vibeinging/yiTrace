fn golden_path_filter_from_json(
    f: &crate::wire::Json,
    tenant: Option<u64>,
) -> Result<(GoldenPathFilter, bool), String> {
    let mut filter = GoldenPathFilter {
        tenant_id: tenant,
        ..Default::default()
    };
    filter.golden_path_id =
        json_field_alias(f, &["golden_path_id", "goldenPathId", "id"]).and_then(json_internal_id);
    filter.task_fingerprint = json_field_alias(
        f,
        &["task_fingerprint", "taskFingerprint", "task", "taskId"],
    )
    .and_then(crate::wire::Json::as_str)
    .map(ToString::to_string);
    filter.trajectory_signature = json_field_alias(
        f,
        &[
            "trajectory_signature",
            "trajectorySignature",
            "signature",
            "pathSignature",
        ],
    )
    .and_then(crate::wire::Json::as_str)
    .map(ToString::to_string);
    filter.source_trace_id = json_field_alias(
        f,
        &["source_trace_id", "sourceTraceId", "trace_id", "traceId"],
    )
    .and_then(json_id_with_external)
    .map(|(id, _)| id);
    filter.challenger_of = json_field_alias(
        f,
        &[
            "challenger_of",
            "challengerOf",
            "baselineGoldenPathId",
            "baseline_golden_path_id",
        ],
    )
    .and_then(json_internal_id);
    filter.eval_profile = json_field_alias(f, &["eval_profile", "evalProfile"])
        .and_then(crate::wire::Json::as_str)
        .map(ToString::to_string);
    let status_value = json_field_alias(f, &["status"]);
    let explicit_status = status_value.is_some();
    if let Some(value) = status_value {
        let Some(status) = value.as_str().and_then(GoldenPathStatus::parse) else {
            return Err("bad status".to_string());
        };
        filter.status = Some(status);
    }
    collect_attr_map(f, &mut filter.attrs);
    remove_top_level_golden_path_governance_attrs(f, &mut filter.attrs);
    for key in ["model", "provider"] {
        if let Some(value) = crate::wire::field(f, key).and_then(crate::wire::Json::as_str) {
            filter
                .attrs
                .insert(key.to_string(), json_string_value(value));
        }
    }
    Ok((filter, explicit_status))
}

fn json_opt_str(s: Option<&str>) -> String {
    s.map_or("null".to_string(), |v| format!("\"{}\"", json_escape(v)))
}

fn json_opt_u64_string(v: Option<u64>) -> String {
    v.map_or("null".to_string(), |id| format!("\"{id}\""))
}

fn json_attrs(attrs: &std::collections::BTreeMap<String, String>) -> String {
    if attrs.is_empty() {
        return "{}".to_string();
    }
    format!(
        "{{{}}}",
        attrs
            .iter()
            .map(|(k, v)| format!("\"{}\":{}", json_escape(k), v))
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn json_annotation(a: &crate::TraceAnnotation) -> String {
    format!(
        r#"{{"annotationId":"{}","tenantId":{},"target":"{}","traceId":"{}","spanId":{},"externalTraceId":{},"externalSpanId":{},"label":"{}","score":{},"reason":{},"source":{},"status":"{}","reviewer":{},"createdAtNs":"{}","updatedAtNs":"{}","attrs":{}}}"#,
        a.annotation_id,
        json_opt_u64_string(a.tenant_id),
        a.target.as_str(),
        a.trace_id,
        json_opt_u64_string(a.span_id),
        json_opt_str(a.external_trace_id.as_deref()),
        json_opt_str(a.external_span_id.as_deref()),
        json_escape(&a.label),
        a.score.map_or("null".to_string(), |s| s.to_string()),
        json_opt_str(a.reason.as_deref()),
        json_opt_str(a.source.as_deref()),
        a.status.as_str(),
        json_opt_str(a.reviewer.as_deref()),
        a.created_at_ns,
        a.updated_at_ns,
        json_attrs(&a.attrs),
    )
}

fn json_dataset_association(a: &crate::DatasetAssociation) -> String {
    format!(
        r#"{{"associationId":"{}","tenantId":{},"datasetId":"{}","itemId":"{}","traceId":"{}","spanId":{},"externalTraceId":{},"externalSpanId":{},"snapshotId":{},"snapshotHash":{},"evalRunId":{},"split":{},"label":{},"score":{},"createdAtNs":"{}","attrs":{}}}"#,
        a.association_id,
        json_opt_u64_string(a.tenant_id),
        json_escape(&a.dataset_id),
        json_escape(&a.item_id),
        a.trace_id,
        json_opt_u64_string(a.span_id),
        json_opt_str(a.external_trace_id.as_deref()),
        json_opt_str(a.external_span_id.as_deref()),
        json_opt_str(a.snapshot_id.as_deref()),
        json_opt_str(a.snapshot_hash.as_deref()),
        json_opt_str(a.eval_run_id.as_deref()),
        json_opt_str(a.split.as_deref()),
        json_opt_str(a.label.as_deref()),
        a.score.map_or("null".to_string(), |s| s.to_string()),
        a.created_at_ns,
        json_attrs(&a.attrs),
    )
}

fn json_golden_path(g: &crate::GoldenPathCandidate) -> String {
    format!(
        r#"{{"goldenPathId":"{}","tenantId":{},"taskFingerprint":"{}","trajectorySignature":"{}","sourceTraceId":"{}","externalSourceTraceId":{},"snapshotId":{},"snapshotHash":{},"status":"{}","score":{},"label":{},"reason":{},"source":{},"challengerOf":{},"evalProfile":{},"minSampleCount":{},"marginScore":{},"comparisonWindowNs":{},"promotedFrom":{},"deprecationReason":{},"staleReasons":{},"governance":{{"challengerOf":{},"evalProfile":{},"minSampleCount":{},"marginScore":{},"comparisonWindowNs":{},"promotedFrom":{},"deprecationReason":{},"staleReasons":{}}},"createdAtNs":"{}","updatedAtNs":"{}","attrs":{},"sourceTrajectory":{},"evidenceSummary":{}}}"#,
        g.golden_path_id,
        json_opt_u64_string(g.tenant_id),
        json_escape(&g.task_fingerprint),
        json_escape(&g.trajectory_signature),
        g.source_trace_id,
        json_opt_str(g.external_source_trace_id.as_deref()),
        json_opt_str(g.snapshot_id.as_deref()),
        json_opt_str(g.snapshot_hash.as_deref()),
        g.status.as_str(),
        g.score.map_or("null".to_string(), |s| s.to_string()),
        json_opt_str(g.label.as_deref()),
        json_opt_str(g.reason.as_deref()),
        json_opt_str(g.source.as_deref()),
        json_opt_u64_string(g.challenger_of),
        json_opt_str(g.eval_profile.as_deref()),
        json_opt_u64_string(g.min_sample_count),
        g.margin_score.map_or("null".to_string(), |s| s.to_string()),
        json_opt_u64_string(g.comparison_window_ns),
        json_opt_u64_string(g.promoted_from),
        json_opt_str(g.deprecation_reason.as_deref()),
        json_string_array(&g.stale_reasons),
        json_opt_u64_string(g.challenger_of),
        json_opt_str(g.eval_profile.as_deref()),
        json_opt_u64_string(g.min_sample_count),
        g.margin_score.map_or("null".to_string(), |s| s.to_string()),
        json_opt_u64_string(g.comparison_window_ns),
        json_opt_u64_string(g.promoted_from),
        json_opt_str(g.deprecation_reason.as_deref()),
        json_string_array(&g.stale_reasons),
        g.created_at_ns,
        g.updated_at_ns,
        json_attrs(&g.attrs),
        trajectory_summary_json_with_signature(&g.source_trajectory_steps, &g.trajectory_signature),
        json_attrs(&g.evidence),
    )
}

fn golden_path_stale_reasons(
    g: &crate::GoldenPathCandidate,
    stored_signature_matches_source: Option<bool>,
    analyzed_trace_total: usize,
    usable_trace_total: usize,
) -> Vec<String> {
    let mut reasons = g.stale_reasons.clone();
    if matches!(g.status, GoldenPathStatus::Deprecated) {
        push_unique_reason(&mut reasons, "deprecated");
    }
    if g.deprecation_reason.is_some() {
        push_unique_reason(&mut reasons, "deprecation_reason");
    }
    if stored_signature_matches_source == Some(false) {
        push_unique_reason(&mut reasons, "source_signature_changed");
    }
    if let Some(min_sample) = g.min_sample_count {
        if analyzed_trace_total < min_sample as usize {
            push_unique_reason(&mut reasons, "insufficient_samples");
        }
    }
    if analyzed_trace_total > 0 {
        let usable_score = (usable_trace_total as u128 * 1000) / analyzed_trace_total as u128;
        let threshold = g.margin_score.unwrap_or(800) as u128;
        if usable_score < threshold {
            push_unique_reason(&mut reasons, "health_below_margin");
        }
    }
    reasons
}

fn push_unique_reason(reasons: &mut Vec<String>, reason: &str) {
    if !reasons.iter().any(|item| item == reason) {
        reasons.push(reason.to_string());
    }
}

fn json_path_adherence(
    golden_path: &crate::GoldenPathCandidate,
    trace_id: u64,
    trace_spans: &[FoldedSpan],
    source_spans: &[FoldedSpan],
) -> String {
    let facts = path_adherence_facts(golden_path, trace_spans, source_spans);
    let golden_coverage = ratio_json(facts.common_steps.len(), facts.source_steps.len());
    let trace_coverage = ratio_json(facts.common_steps.len(), facts.trace_steps.len());
    let trace_summary = trace_summary_buckets_from_spans(trace_spans);
    format!(
        r#"{{"goldenPath":{},"trace":{},"adherence":"{}","sameSignature":{},"sourceAvailable":{},"sourceRetained":{},"storedSignatureMatchesSource":{},"goldenTrajectory":{},"sourceTrajectory":{},"traceTrajectory":{},"scores":{{"commonStepCount":{},"goldenStepCount":{},"traceStepCount":{},"goldenCoverage":{},"traceCoverage":{}}},"commonSteps":{},"missingSteps":{},"extraSteps":{}}}"#,
        json_golden_path(golden_path),
        trace_diff_side_json(trace_id, trace_summary.first()),
        facts.adherence(),
        json_bool(facts.same_signature),
        json_bool(facts.source_available),
        json_bool(facts.source_retained),
        json_opt_bool(facts.stored_signature_matches_source),
        trajectory_summary_json_with_signature(
            &facts.source_steps,
            &golden_path.trajectory_signature
        ),
        facts
            .source_signature
            .as_ref()
            .map(|signature| trajectory_summary_json_with_signature(&facts.source_steps, signature))
            .unwrap_or_else(|| "null".to_string()),
        trajectory_summary_json_with_signature(&facts.trace_steps, &facts.trace_signature),
        facts.common_steps.len(),
        facts.source_steps.len(),
        facts.trace_steps.len(),
        golden_coverage,
        trace_coverage,
        json_string_array(&facts.common_steps),
        json_string_array(&facts.missing_steps),
        json_string_array(&facts.extra_steps),
    )
}

fn path_adherence_facts(
    golden_path: &crate::GoldenPathCandidate,
    trace_spans: &[FoldedSpan],
    source_spans: &[FoldedSpan],
) -> PathAdherenceFacts {
    path_adherence_facts_from_steps(golden_path, trajectory_steps(trace_spans), source_spans)
}

fn path_adherence_facts_from_steps(
    golden_path: &crate::GoldenPathCandidate,
    trace_steps: Vec<String>,
    source_spans: &[FoldedSpan],
) -> PathAdherenceFacts {
    let source_available = !source_spans.is_empty();
    let source_retained = !golden_path.source_trajectory_steps.is_empty();
    let source_steps = if source_available {
        trajectory_steps(source_spans)
    } else if source_retained {
        golden_path.source_trajectory_steps.clone()
    } else {
        Vec::new()
    };
    let source_signature =
        (!source_steps.is_empty()).then(|| trajectory_signature_string(&source_steps));
    let trace_signature = trajectory_signature_string(&trace_steps);
    let same_signature = trace_signature == golden_path.trajectory_signature;
    let stored_signature_matches_source = source_signature
        .as_ref()
        .map(|signature| signature == &golden_path.trajectory_signature);

    let (common_steps, missing_steps, extra_steps) = if source_available {
        ordered_step_diff(&source_steps, &trace_steps)
    } else if source_retained {
        ordered_step_diff(&source_steps, &trace_steps)
    } else {
        (Vec::new(), Vec::new(), Vec::new())
    };
    PathAdherenceFacts {
        source_available,
        source_retained,
        source_steps,
        source_signature,
        trace_steps,
        trace_signature,
        same_signature,
        stored_signature_matches_source,
        common_steps,
        missing_steps,
        extra_steps,
    }
}

fn path_adherence_health_example_json(
    summary: &crate::TraceTrajectorySummary,
    facts: &PathAdherenceFacts,
) -> String {
    let golden_coverage = ratio_json(facts.common_steps.len(), facts.source_steps.len());
    let trace_coverage = ratio_json(facts.common_steps.len(), facts.trace_steps.len());
    format!(
        r#"{{"trace":{},"adherence":"{}","sameSignature":{},"scores":{{"commonStepCount":{},"goldenStepCount":{},"traceStepCount":{},"goldenCoverage":{},"traceCoverage":{}}},"traceTrajectory":{}}}"#,
        trace_trajectory_side_json(summary),
        facts.adherence(),
        json_bool(facts.same_signature),
        facts.common_steps.len(),
        facts.source_steps.len(),
        facts.trace_steps.len(),
        golden_coverage,
        trace_coverage,
        trajectory_summary_json_with_signature(&facts.trace_steps, &facts.trace_signature),
    )
}

fn json_string_array(items: &[String]) -> String {
    format!(
        "[{}]",
        items
            .iter()
            .map(|s| json_string_value(s))
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn json_bool(value: bool) -> &'static str {
    if value {
        "true"
    } else {
        "false"
    }
}

fn json_opt_bool(value: Option<bool>) -> String {
    value.map(json_bool).unwrap_or("null").to_string()
}

fn ratio_json(numerator: usize, denominator: usize) -> String {
    if denominator == 0 {
        "null".to_string()
    } else {
        format!("{:.6}", numerator as f64 / denominator as f64)
    }
}

fn json_agent_fields(attrs: &std::collections::BTreeMap<String, String>) -> String {
    if attrs.is_empty() {
        return "{}".to_string();
    }
    let fields: Vec<String> = agent_field_keys()
        .iter()
        .filter_map(|key| {
            attrs
                .get(*key)
                .map(|value| format!("\"{}\":{}", json_escape(key), value))
        })
        .collect();
    if fields.is_empty() {
        "{}".to_string()
    } else {
        format!("{{{}}}", fields.join(","))
    }
}

fn json_folded_agent_fields(s: &FoldedSpan) -> String {
    json_agent_fields_with_lookup(&s.attrs, |key| crate::first_class_span_attr_value(s, key))
}

fn json_console_agent_fields(s: &crate::ConsoleSpan) -> String {
    json_agent_fields_with_lookup(&s.attrs, |key| {
        crate::first_class_console_attr_value(s, key)
    })
}

fn json_agent_fields_with_lookup<'a>(
    attrs: &'a std::collections::BTreeMap<String, String>,
    first_class: impl Fn(&str) -> Option<&'a str>,
) -> String {
    let fields: Vec<String> = agent_field_keys()
        .iter()
        .filter_map(|key| {
            if let Some(value) = first_class(key) {
                Some(format!(
                    "\"{}\":{}",
                    json_escape(key),
                    first_class_agent_field_json(key, value)
                ))
            } else {
                attrs
                    .get(*key)
                    .map(|value| format!("\"{}\":{}", json_escape(key), value))
            }
        })
        .collect();
    if fields.is_empty() {
        "{}".to_string()
    } else {
        format!("{{{}}}", fields.join(","))
    }
}

fn first_class_agent_field_json(key: &str, value: &str) -> String {
    match key {
        // model/provider are native string fields; the other promoted agentic
        // dimensions are attrs values and already use compact JSON.
        "model" | "provider" => json_string_value(value),
        _ => value.to_string(),
    }
}

fn json_log_events(events: &[crate::SpanLogEvent]) -> String {
    if events.is_empty() {
        return "[]".to_string();
    }
    format!(
        "[{}]",
        events
            .iter()
            .enumerate()
            .map(|(idx, ev)| {
                let messages = ev
                    .messages
                    .iter()
                    .map(|m| json_string_value(m))
                    .collect::<Vec<_>>()
                    .join(",");
                format!(
                    r#"{{"eventId":"{}","eventOrdinal":{},"sortKey":"{:020}:{:020}:{:020}","ts":{},"seq":{},"eventType":{},"messages":[{}],"attrs":{}}}"#,
                    ev.event_id,
                    idx,
                    ev.ts,
                    ev.seq,
                    ev.event_id,
                    ev.ts,
                    ev.seq,
                    ev.event_type,
                    messages,
                    json_attrs(&ev.attrs),
                )
            })
            .collect::<Vec<_>>()
            .join(",")
    )
}
