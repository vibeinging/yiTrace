fn golden_path_evidence_summary_from_json(
    f: &crate::wire::Json,
    spans: &[FoldedSpan],
) -> std::collections::BTreeMap<String, String> {
    let mut evidence = std::collections::BTreeMap::new();
    if let Some(crate::wire::Json::Obj(kvs)) =
        json_field_alias(f, &["evidence", "evidenceSummary", "evidence_summary"])
    {
        for (k, v) in kvs {
            evidence.insert(k.clone(), v.to_compact_json());
        }
    }
    for (alias, key) in [
        ("eval_profile", "eval_profile"),
        ("evalProfile", "eval_profile"),
        ("sample_count", "sample_count"),
        ("sampleCount", "sample_count"),
        ("success_rate", "success_rate"),
        ("successRate", "success_rate"),
        ("avg_cost_usd_nanos", "avg_cost_usd_nanos"),
        ("avgCostUsdNanos", "avg_cost_usd_nanos"),
        ("p95_duration_ns", "p95_duration_ns"),
        ("p95DurationNs", "p95_duration_ns"),
    ] {
        if let Some(value) = crate::wire::field(f, alias) {
            evidence.insert(key.to_string(), value.to_compact_json());
        }
    }

    let summary = trace_summary_buckets_from_spans(spans);
    if let Some(bucket) = summary.first() {
        evidence
            .entry("source_span_count".to_string())
            .or_insert_with(|| bucket.span_count.to_string());
        evidence
            .entry("source_status".to_string())
            .or_insert_with(|| {
                json_string_value(if bucket.error_count > 0 {
                    "error"
                } else {
                    "ok"
                })
            });
        evidence
            .entry("source_duration_ns".to_string())
            .or_insert_with(|| bucket.duration_sum_ns.to_string());
        evidence
            .entry("source_total_tokens".to_string())
            .or_insert_with(|| bucket.total_tokens.to_string());
        evidence
            .entry("source_cost_usd_nanos".to_string())
            .or_insert_with(|| bucket.cost_usd_nanos.to_string());
    }
    let steps = trajectory_steps(spans);
    evidence
        .entry("source_trajectory_step_count".to_string())
        .or_insert_with(|| steps.len().to_string());
    if !steps.is_empty() {
        evidence
            .entry("source_trajectory_signature".to_string())
            .or_insert_with(|| json_string_value(&trajectory_signature_string(&steps)));
    }
    evidence
}

fn collect_attr_map(f: &crate::wire::Json, attrs: &mut std::collections::BTreeMap<String, String>) {
    use crate::wire::{field, Json};
    for (alias, key) in attr_aliases() {
        if let Some(v) = field(f, alias) {
            attrs.insert((*key).to_string(), v.to_compact_json());
        }
    }
    if let Some(Json::Obj(kvs)) = field(f, "attrs") {
        for (k, v) in kvs {
            attrs.insert(k.clone(), v.to_compact_json());
        }
    }
}

fn collect_attr_query_json(s: &str, attrs: &mut std::collections::BTreeMap<String, String>) {
    use crate::wire::Json;
    let Ok(Json::Obj(kvs)) = crate::wire::parse(s) else {
        return;
    };
    for (k, v) in kvs {
        attrs.insert(k, v.to_compact_json());
    }
}

fn collect_attr_query_pair(
    k: &str,
    v: &str,
    attrs: &mut std::collections::BTreeMap<String, String>,
) {
    if let Some((_, attr_key)) = attr_aliases().iter().find(|(alias, _)| *alias == k) {
        attrs.insert((*attr_key).to_string(), json_string_value(v));
    }
}

fn query_pairs(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            Some((url_decode(k), url_decode(v)))
        })
        .collect()
}

fn attr_aliases() -> &'static [(&'static str, &'static str)] {
    &[
        ("project_id", "project_id"),
        ("projectId", "project_id"),
        ("external_run_id", "external_run_id"),
        ("externalRunId", "external_run_id"),
        ("skill", "skill"),
        ("mode", "mode"),
        ("call_site", "call_site"),
        ("callSite", "call_site"),
        ("task_fingerprint", "task_fingerprint"),
        ("taskFingerprint", "task_fingerprint"),
        ("loop_id", "loop_id"),
        ("loopId", "loop_id"),
        ("harness_version", "harness_version"),
        ("harnessVersion", "harness_version"),
        ("validation_status", "validation_status"),
        ("validationStatus", "validation_status"),
        ("stop_reason", "stop_reason"),
        ("stopReason", "stop_reason"),
        ("phase", "phase"),
        ("validator", "validator"),
        ("connection_ids", "connection_ids"),
        ("connectionIds", "connection_ids"),
        ("data_source_ids", "data_source_ids"),
        ("dataSourceIds", "data_source_ids"),
        ("schema_fingerprint", "schema_fingerprint"),
        ("schemaFingerprint", "schema_fingerprint"),
        ("eval_profile", "eval_profile"),
        ("evalProfile", "eval_profile"),
        ("tool_version", "tool_version"),
        ("toolVersion", "tool_version"),
        ("intent_signature", "intent_signature"),
        ("intentSignature", "intent_signature"),
        ("review_status", "review_status"),
        ("reviewStatus", "review_status"),
        ("eval_status", "eval_status"),
        ("evalStatus", "eval_status"),
        ("path_memory_id", "path_memory_id"),
        ("pathMemoryId", "path_memory_id"),
    ]
}

fn agent_field_keys() -> &'static [&'static str] {
    &[
        "project_id",
        "session_id",
        "external_run_id",
        "skill",
        "mode",
        "call_site",
        "task_fingerprint",
        "loop_id",
        "harness_version",
        "validation_status",
        "stop_reason",
        "phase",
        "validator",
        "connection_ids",
        "data_source_ids",
        "schema_fingerprint",
        "eval_profile",
        "tool_version",
        "model",
        "provider",
        "intent_signature",
        "review_status",
        "eval_status",
        "path_memory_id",
    ]
}

/// 截断长文本当标题（按字符，不切坏 UTF-8）。
fn trunc(s: &str) -> String {
    let max = 40;
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>() + "…"
    }
}

/// 极小 JSON 字符串转义（响应里嵌中文日志/agent 名时用）。中文 UTF-8 原样,只转义 `"` `\` 和控制符。
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}
