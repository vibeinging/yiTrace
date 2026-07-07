fn trace_search_request_from_json(
    v: &crate::wire::Json,
    tenant: Option<u64>,
) -> TraceSearchRequest {
    use crate::wire::{field, Json};
    let f = field(v, "filter").unwrap_or(v);
    let mut q = TraceQuery::all();
    q.tenant_id = tenant;
    q.trace_id = json_field_alias(f, &["trace_id", "traceId"]).and_then(json_id_or_hash);
    if let Some(from) =
        json_field_alias(f, &["time_from", "timeFrom", "created_from", "createdFrom"])
            .and_then(Json::as_i64)
    {
        q.time_from = from;
    }
    if let Some(to) = json_field_alias(f, &["time_to", "timeTo", "created_to", "createdTo"])
        .and_then(Json::as_i64)
    {
        q.time_to = to;
    }

    let mut attrs = std::collections::BTreeMap::new();
    collect_attr_map(f, &mut attrs);
    let spec = TraceSearchSpec {
        session_id: json_field_alias(f, &["session_id", "sessionId"]).and_then(json_id_or_hash),
        span_id: json_field_alias(f, &["span_id", "spanId"]).and_then(json_id_or_hash),
        external_trace_id: json_field_alias(f, &["external_trace_id", "externalTraceId"])
            .and_then(Json::as_str)
            .map(|s| s.to_string()),
        external_span_id: json_field_alias(f, &["external_span_id", "externalSpanId"])
            .and_then(Json::as_str)
            .map(|s| s.to_string()),
        external_session_id: json_field_alias(f, &["external_session_id", "externalSessionId"])
            .and_then(Json::as_str)
            .map(|s| s.to_string()),
        status: field(f, "status").and_then(Json::as_u64).map(|x| x as u8),
        kind: json_field_alias(f, &["span_kind", "spanKind", "kind"])
            .and_then(Json::as_str)
            .map(|s| s.to_string()),
        agent_name: json_field_alias(f, &["agent_name", "agentName"])
            .and_then(Json::as_str)
            .map(|s| s.to_string()),
        tool_name: json_field_alias(f, &["tool_name", "toolName"])
            .and_then(Json::as_str)
            .map(|s| s.to_string()),
        model: field(f, "model")
            .and_then(Json::as_str)
            .map(|s| s.to_string()),
        text: json_field_alias(v, &["text", "q"])
            .or_else(|| json_field_alias(f, &["text", "q"]))
            .and_then(Json::as_str)
            .map(|s| s.to_string()),
        input_contains: json_field_alias(f, &["input_text", "inputText", "inputContains"])
            .and_then(Json::as_str)
            .map(|s| s.to_string()),
        output_contains: json_field_alias(f, &["output_text", "outputText", "outputContains"])
            .and_then(Json::as_str)
            .map(|s| s.to_string()),
        log_contains: json_field_alias(f, &["log_text", "logText", "logContains"])
            .and_then(Json::as_str)
            .map(|s| s.to_string()),
        min_cost_usd_nanos: json_cost_nanos_alias(
            f,
            &["min_cost_usd_nanos", "minCostUsdNanos", "costUsdNanosMin"],
            &["min_cost_usd", "minCostUsd", "costUsdMin"],
        )
        .or_else(|| {
            json_cost_nanos_alias(
                v,
                &["min_cost_usd_nanos", "minCostUsdNanos", "costUsdNanosMin"],
                &["min_cost_usd", "minCostUsd", "costUsdMin"],
            )
        }),
        max_cost_usd_nanos: json_cost_nanos_alias(
            f,
            &["max_cost_usd_nanos", "maxCostUsdNanos", "costUsdNanosMax"],
            &["max_cost_usd", "maxCostUsd", "costUsdMax"],
        )
        .or_else(|| {
            json_cost_nanos_alias(
                v,
                &["max_cost_usd_nanos", "maxCostUsdNanos", "costUsdNanosMax"],
                &["max_cost_usd", "maxCostUsd", "costUsdMax"],
            )
        }),
        min_total_tokens: json_field_alias(
            f,
            &[
                "min_total_tokens",
                "minTotalTokens",
                "totalTokensMin",
                "minTokens",
            ],
        )
        .or_else(|| {
            json_field_alias(
                v,
                &[
                    "min_total_tokens",
                    "minTotalTokens",
                    "totalTokensMin",
                    "minTokens",
                ],
            )
        })
        .and_then(Json::as_u64),
        max_total_tokens: json_field_alias(
            f,
            &[
                "max_total_tokens",
                "maxTotalTokens",
                "totalTokensMax",
                "maxTokens",
            ],
        )
        .or_else(|| {
            json_field_alias(
                v,
                &[
                    "max_total_tokens",
                    "maxTotalTokens",
                    "totalTokensMax",
                    "maxTokens",
                ],
            )
        })
        .and_then(Json::as_u64),
        attrs,
    };
    TraceSearchRequest {
        query: q,
        spec,
        annotation: trace_search_annotation_spec(f),
        dataset: trace_search_dataset_spec(f),
    }
}

fn trace_search_scan_projection(
    request: &TraceSearchRequest,
    sort_by: &str,
) -> crate::Projection {
    let mut cols = crate::Projection::STATUS
        | crate::Projection::DURATION_NS
        | crate::Projection::INPUT_TOKENS
        | crate::Projection::OUTPUT_TOKENS
        | crate::Projection::USAGE_COST
        | crate::Projection::SESSION_ID
        | crate::Projection::TENANT_ID
        | crate::Projection::AGENT_NAME
        | crate::Projection::TOOL_NAME
        | crate::Projection::MODEL
        | crate::Projection::EXTERNAL_IDS
        | crate::Projection::ATTRS
        | crate::Projection::AGENTIC_FIELDS;

    if request.spec.text.is_some() {
        cols |= crate::Projection::INPUT_TEXT | crate::Projection::OUTPUT_TEXT | crate::Projection::LOGS;
    }
    if request.spec.input_contains.is_some() {
        cols |= crate::Projection::INPUT_TEXT;
    }
    if request.spec.output_contains.is_some() {
        cols |= crate::Projection::OUTPUT_TEXT;
    }
    if request.spec.log_contains.is_some() {
        cols |= crate::Projection::LOGS;
    }

    match sort_by.to_ascii_lowercase().as_str() {
        "duration" | "duration_ns" | "durationns" => cols |= crate::Projection::DURATION_NS,
        "cost" | "cost_usd" | "costusd" => cols |= crate::Projection::USAGE_COST,
        "tokens" | "token_count" | "tokencount" => {
            cols |= crate::Projection::INPUT_TOKENS
                | crate::Projection::OUTPUT_TOKENS
                | crate::Projection::USAGE_COST;
        }
        "status" => cols |= crate::Projection::STATUS,
        _ => {}
    }

    crate::Projection::of(cols)
}

fn trace_search_match(
    s: &FoldedSpan,
    spec: &TraceSearchSpec,
    metadata: &TraceSearchMetadataMatches,
) -> bool {
    if let Some(session_id) = spec.session_id {
        if s.session_id != Some(session_id) {
            return false;
        }
    }
    if let Some(span_id) = spec.span_id {
        if s.span_id != span_id {
            return false;
        }
    }
    if let Some(expected) = &spec.external_trace_id {
        if s.external_trace_id.as_deref() != Some(expected.as_str()) {
            return false;
        }
    }
    if let Some(expected) = &spec.external_span_id {
        if s.external_span_id.as_deref() != Some(expected.as_str()) {
            return false;
        }
    }
    if let Some(expected) = &spec.external_session_id {
        if s.external_session_id.as_deref() != Some(expected.as_str()) {
            return false;
        }
    }
    if let Some(status) = spec.status {
        if s.status != Some(status) {
            return false;
        }
    }
    if let Some(kind) = &spec.kind {
        if folded_kind(s) != kind {
            return false;
        }
    }
    if let Some(agent) = &spec.agent_name {
        if s.agent_name.as_deref() != Some(agent.as_str()) {
            return false;
        }
    }
    if let Some(tool) = &spec.tool_name {
        if s.tool_name.as_deref() != Some(tool.as_str()) {
            return false;
        }
    }
    if let Some(model) = &spec.model {
        if s.model.as_deref() != Some(model.as_str()) {
            return false;
        }
    }
    for (key, expected) in &spec.attrs {
        if !crate::folded_span_attr_value(s, key)
            .map(|actual| crate::attr_json_matches(actual, expected))
            .unwrap_or(false)
        {
            return false;
        }
    }
    if let Some(text) = &spec.text {
        if !folded_contains(s, text) {
            return false;
        }
    }
    if let Some(text) = &spec.input_contains {
        if !s
            .input_text
            .as_deref()
            .map(|v| v.contains(text))
            .unwrap_or(false)
        {
            return false;
        }
    }
    if let Some(text) = &spec.output_contains {
        if !s
            .output_text
            .as_deref()
            .map(|v| v.contains(text))
            .unwrap_or(false)
        {
            return false;
        }
    }
    if let Some(text) = &spec.log_contains {
        if !s.logs.iter().any(|log| log.contains(text)) {
            return false;
        }
    }
    let cost_usd_nanos = folded_cost_usd_nanos(s);
    if let Some(min) = spec.min_cost_usd_nanos {
        if cost_usd_nanos < min {
            return false;
        }
    }
    if let Some(max) = spec.max_cost_usd_nanos {
        if cost_usd_nanos > max {
            return false;
        }
    }
    let total_tokens = folded_total_tokens(s);
    if let Some(min) = spec.min_total_tokens {
        if total_tokens < min {
            return false;
        }
    }
    if let Some(max) = spec.max_total_tokens {
        if total_tokens > max {
            return false;
        }
    }
    trace_search_metadata_match(s, metadata)
}

fn trace_search_metadata_match(s: &FoldedSpan, metadata: &TraceSearchMetadataMatches) -> bool {
    if metadata.need_annotation
        && !metadata.annotation_traces.contains(&s.trace_id)
        && !metadata.annotation_spans.contains(&(s.trace_id, s.span_id))
    {
        return false;
    }
    if metadata.need_dataset
        && !metadata.dataset_traces.contains(&s.trace_id)
        && !metadata.dataset_spans.contains(&(s.trace_id, s.span_id))
    {
        return false;
    }
    true
}

fn trace_id_metadata_match(trace_id: u64, metadata: &TraceSearchMetadataMatches) -> bool {
    if metadata.need_annotation && !metadata.annotation_candidate_traces.contains(&trace_id) {
        return false;
    }
    if metadata.need_dataset && !metadata.dataset_candidate_traces.contains(&trace_id) {
        return false;
    }
    true
}

fn metadata_candidate_trace_ids(
    metadata: &TraceSearchMetadataMatches,
) -> std::collections::HashSet<u64> {
    let mut out = std::collections::HashSet::new();
    if metadata.need_annotation {
        out.extend(metadata.annotation_candidate_traces.iter().copied());
    }
    if metadata.need_dataset {
        if out.is_empty() {
            out.extend(metadata.dataset_candidate_traces.iter().copied());
        } else {
            out.retain(|trace_id| metadata.dataset_candidate_traces.contains(trace_id));
        }
    }
    out
}

fn trace_search_annotation_spec(f: &crate::wire::Json) -> TraceSearchAnnotationSpec {
    let nested = json_field_alias(f, &["annotation", "annotations", "annotationFilter"]);
    let obj = nested.unwrap_or(f);
    let mut attrs = std::collections::BTreeMap::new();
    if nested.is_some() {
        collect_attr_map(obj, &mut attrs);
    }
    let target = json_field_alias(obj, &["target", "target_type", "targetType"])
        .or_else(|| json_field_alias(f, &["annotation_target", "annotationTarget"]))
        .and_then(crate::wire::Json::as_str)
        .and_then(AnnotationTarget::parse);
    let label = json_field_alias(obj, &["label"])
        .or_else(|| json_field_alias(f, &["annotation_label", "annotationLabel"]))
        .and_then(crate::wire::Json::as_str)
        .map(ToString::to_string);
    let source = json_field_alias(obj, &["source"])
        .or_else(|| json_field_alias(f, &["annotation_source", "annotationSource"]))
        .and_then(crate::wire::Json::as_str)
        .map(ToString::to_string);
    let status = json_field_alias(obj, &["status"])
        .or_else(|| json_field_alias(f, &["annotation_status", "annotationStatus"]))
        .and_then(crate::wire::Json::as_str)
        .and_then(AnnotationStatus::parse);
    let include_deleted = json_bool_alias(obj, &["includeDeleted", "include_deleted"])
        .or_else(|| {
            json_bool_alias(
                f,
                &["annotation_include_deleted", "annotationIncludeDeleted"],
            )
        })
        .unwrap_or(false);
    let score_min = json_field_alias(obj, &["score_min", "scoreMin", "minScore"])
        .or_else(|| json_field_alias(f, &["annotation_score_min", "annotationScoreMin"]))
        .and_then(crate::wire::Json::as_u64)
        .map(score_u64);
    let score_max = json_field_alias(obj, &["score_max", "scoreMax", "maxScore"])
        .or_else(|| json_field_alias(f, &["annotation_score_max", "annotationScoreMax"]))
        .and_then(crate::wire::Json::as_u64)
        .map(score_u64);
    let active = nested.is_some()
        || target.is_some()
        || label.is_some()
        || source.is_some()
        || status.is_some()
        || include_deleted
        || score_min.is_some()
        || score_max.is_some()
        || !attrs.is_empty();
    TraceSearchAnnotationSpec {
        active,
        target,
        label,
        source,
        status,
        include_deleted,
        score_min,
        score_max,
        attrs,
    }
}

fn trace_search_dataset_spec(f: &crate::wire::Json) -> TraceSearchDatasetSpec {
    let nested = json_field_alias(
        f,
        &[
            "dataset",
            "datasetAssociation",
            "dataset_association",
            "datasetLink",
            "dataset_link",
        ],
    );
    let obj = nested.unwrap_or(f);
    let mut attrs = std::collections::BTreeMap::new();
    if nested.is_some() {
        collect_attr_map(obj, &mut attrs);
    }
    let dataset_id = json_field_alias(obj, &["dataset_id", "datasetId", "dataset"])
        .or_else(|| json_field_alias(f, &["dataset_id", "datasetId"]))
        .and_then(crate::wire::Json::as_str)
        .map(ToString::to_string);
    let item_id = json_field_alias(
        obj,
        &["item_id", "itemId", "dataset_item_id", "datasetItemId"],
    )
    .or_else(|| {
        json_field_alias(
            f,
            &["item_id", "itemId", "dataset_item_id", "datasetItemId"],
        )
    })
    .and_then(crate::wire::Json::as_str)
    .map(ToString::to_string);
    let eval_run_id = json_field_alias(obj, &["eval_run_id", "evalRunId"])
        .or_else(|| json_field_alias(f, &["eval_run_id", "evalRunId"]))
        .and_then(crate::wire::Json::as_str)
        .map(ToString::to_string);
    let split = json_field_alias(obj, &["split"])
        .or_else(|| json_field_alias(f, &["dataset_split", "datasetSplit"]))
        .and_then(crate::wire::Json::as_str)
        .map(ToString::to_string);
    let label = json_field_alias(obj, &["label"])
        .or_else(|| json_field_alias(f, &["dataset_label", "datasetLabel"]))
        .and_then(crate::wire::Json::as_str)
        .map(ToString::to_string);
    let score_min = json_field_alias(obj, &["score_min", "scoreMin", "minScore"])
        .or_else(|| json_field_alias(f, &["dataset_score_min", "datasetScoreMin"]))
        .and_then(crate::wire::Json::as_u64)
        .map(score_u64);
    let score_max = json_field_alias(obj, &["score_max", "scoreMax", "maxScore"])
        .or_else(|| json_field_alias(f, &["dataset_score_max", "datasetScoreMax"]))
        .and_then(crate::wire::Json::as_u64)
        .map(score_u64);
    let active = nested.is_some()
        || dataset_id.is_some()
        || item_id.is_some()
        || eval_run_id.is_some()
        || split.is_some()
        || label.is_some()
        || score_min.is_some()
        || score_max.is_some()
        || !attrs.is_empty();
    TraceSearchDatasetSpec {
        active,
        dataset_id,
        item_id,
        eval_run_id,
        split,
        label,
        score_min,
        score_max,
        attrs,
    }
}

fn trace_search_annotation_spec_from_query(
    pairs: &[(String, String)],
) -> TraceSearchAnnotationSpec {
    let mut spec = TraceSearchAnnotationSpec::default();
    for (k, v) in pairs {
        match k.as_str() {
            "annotation_target" | "annotationTarget" => {
                spec.target = AnnotationTarget::parse(v);
                spec.active = true;
            }
            "annotation_label" | "annotationLabel" => {
                spec.label = Some(v.clone());
                spec.active = true;
            }
            "annotation_source" | "annotationSource" => {
                spec.source = Some(v.clone());
                spec.active = true;
            }
            "annotation_status" | "annotationStatus" => {
                spec.status = AnnotationStatus::parse(v);
                spec.active = spec.status.is_some();
            }
            "annotation_include_deleted" | "annotationIncludeDeleted" => {
                spec.include_deleted = query_bool(v);
                spec.active = true;
            }
            "annotation_score_min" | "annotationScoreMin" => {
                spec.score_min = v.parse::<u64>().ok().map(score_u64);
                spec.active = true;
            }
            "annotation_score_max" | "annotationScoreMax" => {
                spec.score_max = v.parse::<u64>().ok().map(score_u64);
                spec.active = true;
            }
            "annotation_attrs" | "annotationAttrs" => {
                collect_attr_query_json(v, &mut spec.attrs);
                spec.active = true;
            }
            _ => {}
        }
    }
    if !spec.attrs.is_empty() {
        spec.active = true;
    }
    spec
}

fn trace_search_dataset_spec_from_query(pairs: &[(String, String)]) -> TraceSearchDatasetSpec {
    let mut spec = TraceSearchDatasetSpec::default();
    for (k, v) in pairs {
        match k.as_str() {
            "dataset_id" | "datasetId" => {
                spec.dataset_id = Some(v.clone());
                spec.active = true;
            }
            "item_id" | "itemId" | "dataset_item_id" | "datasetItemId" => {
                spec.item_id = Some(v.clone());
                spec.active = true;
            }
            "eval_run_id" | "evalRunId" => {
                spec.eval_run_id = Some(v.clone());
                spec.active = true;
            }
            "dataset_split" | "datasetSplit" => {
                spec.split = Some(v.clone());
                spec.active = true;
            }
            "dataset_label" | "datasetLabel" => {
                spec.label = Some(v.clone());
                spec.active = true;
            }
            "dataset_score_min" | "datasetScoreMin" => {
                spec.score_min = v.parse::<u64>().ok().map(score_u64);
                spec.active = true;
            }
            "dataset_score_max" | "datasetScoreMax" => {
                spec.score_max = v.parse::<u64>().ok().map(score_u64);
                spec.active = true;
            }
            "dataset_attrs" | "datasetAttrs" => {
                collect_attr_query_json(v, &mut spec.attrs);
                spec.active = true;
            }
            _ => {}
        }
    }
    if !spec.attrs.is_empty() {
        spec.active = true;
    }
    spec
}

fn score_u64(n: u64) -> u32 {
    n.min(u32::MAX as u64) as u32
}

fn score_in_range(score: Option<u32>, min: Option<u32>, max: Option<u32>) -> bool {
    if min.is_none() && max.is_none() {
        return true;
    }
    let Some(score) = score else {
        return false;
    };
    if min.map(|m| score < m).unwrap_or(false) {
        return false;
    }
    if max.map(|m| score > m).unwrap_or(false) {
        return false;
    }
    true
}

fn folded_contains(s: &FoldedSpan, needle: &str) -> bool {
    needle.is_empty()
        || s.input_text
            .as_deref()
            .map(|v| v.contains(needle))
            .unwrap_or(false)
        || s.output_text
            .as_deref()
            .map(|v| v.contains(needle))
            .unwrap_or(false)
        || s.logs.iter().any(|log| log.contains(needle))
        || s.agent_name
            .as_deref()
            .map(|v| v.contains(needle))
            .unwrap_or(false)
        || s.tool_name
            .as_deref()
            .map(|v| v.contains(needle))
            .unwrap_or(false)
        || s.model
            .as_deref()
            .map(|v| v.contains(needle))
            .unwrap_or(false)
        || [
            "project_id",
            "skill",
            "mode",
            "call_site",
            "task_fingerprint",
            "loop_id",
            "harness_version",
            "schema_fingerprint",
            "intent_signature",
            "validation_status",
            "review_status",
            "eval_status",
            "path_memory_id",
            "stop_reason",
            "phase",
            "validator",
        ]
        .iter()
        .filter_map(|key| crate::first_class_span_attr_value(s, key))
        .any(|value| value.contains(needle))
        || s.attrs
            .iter()
            .any(|(k, v)| k.contains(needle) || v.contains(needle))
}

fn folded_kind(s: &FoldedSpan) -> &'static str {
    if s.agent_name.is_some() {
        "agent"
    } else if s.tool_name.is_some() {
        "tool"
    } else if s.model.is_some() {
        "llm"
    } else {
        "other"
    }
}

fn folded_name(s: &FoldedSpan) -> String {
    s.agent_name
        .as_ref()
        .or(s.tool_name.as_ref())
        .or(s.model.as_ref())
        .cloned()
        .unwrap_or_else(|| format!("span {}", s.span_id))
}

fn sort_trace_search_spans(spans: &mut [FoldedSpan], sort_by: &str, desc: bool) {
    let sort = sort_by.to_ascii_lowercase();
    spans.sort_by(|a, b| {
        let ord = match sort.as_str() {
            "duration" | "duration_ns" | "durationns" => {
                a.duration_ns.unwrap_or(0).cmp(&b.duration_ns.unwrap_or(0))
            }
            "cost" | "cost_usd" | "costusd" => cost_sort_key(a).cmp(&cost_sort_key(b)),
            "tokens" | "token_count" | "tokencount" => token_sort_key(a).cmp(&token_sort_key(b)),
            "status" => a.status.unwrap_or(0).cmp(&b.status.unwrap_or(0)),
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

fn trace_search_rollup_blockers(request: &TraceSearchRequest) -> Vec<String> {
    let mut blockers = trace_aggregate_rollup_blockers(&[], request);
    if request.query.trace_id.is_some() {
        blockers.push("trace_id_filter".to_string());
    }
    if request.query.time_from != i64::MIN || request.query.time_to != i64::MAX {
        blockers.push("time_window_filter".to_string());
    }
    blockers
}

fn trace_search_read_plan_json(
    span_read_index: &str,
    attr_stats: &AttrIndexedReadStats,
    rollup_stats: Option<&crate::TraceAggregateRollupStats>,
    folded_span_count: usize,
    page_hydrate_keys: usize,
    rollup_blockers: &[String],
    fallback_reason: Option<&str>,
) -> String {
    let candidate_span_keys = attr_stats
        .candidate_span_keys
        .map_or("null".to_string(), |n| n.to_string());
    let unsupported = attr_stats
        .unsupported_attr_keys
        .iter()
        .map(|key| format!(r#""{}""#, json_escape(key)))
        .collect::<Vec<_>>()
        .join(",");
    let rollup_blockers = rollup_blockers
        .iter()
        .map(|key| format!(r#""{}""#, json_escape(key)))
        .collect::<Vec<_>>()
        .join(",");
    let fallback_reason = fallback_reason.map_or("null".to_string(), json_string_value);
    let (used_segment_rollup, segment_rollup_segments, segment_rollup_rows, tail_folded_span_count) =
        rollup_stats.map_or((false, 0, 0, 0), |stats| {
            (
                stats.used_segment_rollup,
                stats.segment_rollup_segments,
                stats.segment_rollup_rows,
                stats.tail_folded_span_count,
            )
        });
    format!(
        r#"{{"spanReadIndex":"{}","usedSegmentRollup":{},"segmentRollupSegments":{},"segmentRollupRows":{},"tailFoldedSpanCount":{},"usedAttrPostings":{},"candidateSpanKeys":{},"scannedSegments":{},"foldedSpanCount":{},"pageHydrateKeys":{},"unsupportedAttrKeys":[{}],"rollupBlockedBy":[{}],"rollupFallbackReason":{}}}"#,
        span_read_index,
        used_segment_rollup,
        segment_rollup_segments,
        segment_rollup_rows,
        tail_folded_span_count,
        attr_stats.used_attr_postings,
        candidate_span_keys,
        attr_stats.scanned_segments,
        folded_span_count,
        page_hydrate_keys,
        unsupported,
        rollup_blockers,
        fallback_reason,
    )
}

fn cost_sort_key(s: &FoldedSpan) -> u128 {
    folded_cost_usd_nanos(s) as u128
}

fn token_sort_key(s: &FoldedSpan) -> u64 {
    folded_total_tokens(s)
}
