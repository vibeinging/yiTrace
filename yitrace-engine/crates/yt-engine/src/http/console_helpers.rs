fn json_trace_search_span(s: &FoldedSpan, rank: usize) -> String {
    let kind = folded_kind(s);
    let name = folded_name(s);
    let logs = s
        .logs
        .iter()
        .take(5)
        .map(|log| json_string_value(log))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"{{"rank":{},"traceId":"{}","spanId":"{}","sessionId":{},"externalTraceId":{},"externalSpanId":{},"externalSessionId":{},"kind":"{}","name":"{}","status":{},"statusText":"{}","durationNs":{},"durMs":{},"cost":{},"costUsd":{},"costDetail":{},"usage":{},"inputTokens":{},"outputTokens":{},"agentName":{},"toolName":{},"model":{},"provider":{},"inputText":{},"outputText":{},"logsPreview":[{}],"fields":{},"attrs":{}}}"#,
        rank,
        s.trace_id,
        s.span_id,
        s.session_id
            .map_or("null".to_string(), |id| format!("\"{id}\"")),
        json_opt_str(s.external_trace_id.as_deref()),
        json_opt_str(s.external_span_id.as_deref()),
        json_opt_str(s.external_session_id.as_deref()),
        kind,
        json_escape(&name),
        s.status
            .map_or("null".to_string(), |status| status.to_string()),
        if s.status.unwrap_or(0) == 0 {
            "ok"
        } else {
            "error"
        },
        s.duration_ns.map_or("null".to_string(), |d| d.to_string()),
        s.duration_ns
            .map_or("null".to_string(), |d| (d / 1_000_000).to_string()),
        cost_num(s.input_tokens.unwrap_or(0), s.output_tokens.unwrap_or(0)),
        cost_usd_num_from_nanos(folded_cost_usd_nanos(s)),
        cost_detail_json(
            folded_cost_usd_nanos(s),
            s.cost_currency.as_deref(),
            folded_cost_source(s),
        ),
        folded_usage_json(s),
        s.input_tokens.unwrap_or(0),
        s.output_tokens.unwrap_or(0),
        json_opt_str(s.agent_name.as_deref()),
        json_opt_str(s.tool_name.as_deref()),
        json_opt_str(s.model.as_deref()),
        json_opt_str(s.provider.as_deref()),
        json_text_field(s.input_text.as_deref(), false),
        json_text_field(s.output_text.as_deref(), false),
        logs,
        json_folded_agent_fields(s),
        json_attrs(&s.attrs),
    )
}

fn span_order(spans: &[crate::ConsoleSpan]) -> std::collections::HashMap<u64, (usize, usize)> {
    let mut out = std::collections::HashMap::new();
    let mut sibling_counts: std::collections::BTreeMap<Option<u64>, usize> =
        std::collections::BTreeMap::new();
    for (idx, span) in spans.iter().enumerate() {
        let sibling = sibling_counts.entry(span.parent_span_id).or_insert(0);
        out.insert(span.span_id, (idx, *sibling));
        *sibling += 1;
    }
    out
}

fn trace_summary_json(tid: u64, spans: &[crate::ConsoleSpan]) -> String {
    let total_duration_ns: u64 = spans.iter().map(|s| s.duration_ns).sum();
    let input_tokens: u64 = spans.iter().map(|s| s.input_tokens).sum();
    let output_tokens: u64 = spans.iter().map(|s| s.output_tokens).sum();
    let cached_input_tokens: u64 = spans.iter().map(|s| s.cached_input_tokens).sum();
    let reasoning_tokens: u64 = spans.iter().map(|s| s.reasoning_tokens).sum();
    let total_tokens: u64 = spans.iter().map(|s| s.total_tokens).sum();
    let cost_usd_nanos: u64 = spans.iter().map(|s| s.cost_usd_nanos).sum();
    let any_err = spans.iter().any(|s| s.has_error);
    let name = spans.first().map(|s| s.name.clone()).unwrap_or_default();
    format!(
        r#"{{"traceId":"{}","externalTraceId":{},"name":"{}","durationNs":{},"durMs":{},"cost":{},"costUsd":{},"costDetail":{},"spanCount":{},"status":"{}","usage":{}}}"#,
        tid,
        json_opt_str(spans.iter().find_map(|s| s.external_trace_id.as_deref())),
        json_escape(&name),
        total_duration_ns,
        total_duration_ns / 1_000_000,
        cost_num(input_tokens, output_tokens),
        cost_usd_num_from_nanos(cost_usd_nanos),
        cost_detail_json(cost_usd_nanos, Some("USD"), "mixed"),
        spans.len(),
        if any_err { "error" } else { "ok" },
        usage_json(
            input_tokens,
            output_tokens,
            cached_input_tokens,
            reasoning_tokens,
            total_tokens,
        ),
    )
}

fn json_console_span_export(
    trace_id: u64,
    s: &crate::ConsoleSpan,
    span_ordinal: usize,
    sibling_ordinal: usize,
    events: &[crate::SpanLogEvent],
    include_full: bool,
) -> String {
    format!(
        r#"{{"traceId":"{}","id":"{}","spanId":"{}","parentId":{},"externalTraceId":{},"externalSpanId":{},"externalParentSpanId":{},"externalSessionId":{},"kind":"{}","name":"{}","spanOrdinal":{},"siblingOrdinal":{},"sortKey":"{:020}:{:020}","status":"{}","durationNs":{},"durMs":{},"cost":{},"costUsd":{},"costDetail":{},"usage":{},"model":{},"provider":{},"inputText":{},"outputText":{},"fields":{},"attrs":{},"logEvents":{}}}"#,
        trace_id,
        s.span_id,
        s.span_id,
        s.parent_span_id
            .map_or("null".to_string(), |p| format!("\"{p}\"")),
        json_opt_str(s.external_trace_id.as_deref()),
        json_opt_str(s.external_span_id.as_deref()),
        json_opt_str(s.external_parent_span_id.as_deref()),
        json_opt_str(s.external_session_id.as_deref()),
        s.kind,
        json_escape(&s.name),
        span_ordinal,
        sibling_ordinal,
        span_ordinal,
        s.span_id,
        if s.has_error { "error" } else { "ok" },
        s.duration_ns,
        s.duration_ns / 1_000_000,
        cost_num(s.input_tokens, s.output_tokens),
        cost_usd_num_from_nanos(s.cost_usd_nanos),
        cost_detail_json(s.cost_usd_nanos, s.cost_currency.as_deref(), "mixed"),
        console_usage_json(s),
        json_opt_str(s.model.as_deref()),
        json_opt_str(s.provider.as_deref()),
        json_text_field(s.input_text.as_deref(), include_full),
        json_text_field(s.output_text.as_deref(), include_full),
        json_console_agent_fields(s),
        json_attrs(&s.attrs),
        json_log_events(events),
    )
}

fn json_text_field(text: Option<&str>, include_full: bool) -> String {
    let Some(text) = text else {
        return "null".to_string();
    };
    let (preview, truncated) = preview_text(text, 280);
    let hash = yt_core::event::fnv1a64(text.as_bytes());
    format!(
        r#"{{"preview":"{}","full":{},"contentHash":"fnv1a64:{:016x}","byteLength":{},"truncated":{},"blobRef":null}}"#,
        json_escape(&preview),
        if include_full {
            json_string_value(text)
        } else {
            "null".to_string()
        },
        hash,
        text.len(),
        truncated,
    )
}

fn preview_text(text: &str, max_chars: usize) -> (String, bool) {
    if text.chars().count() <= max_chars {
        return (text.to_string(), false);
    }
    (text.chars().take(max_chars).collect(), true)
}

fn span_page_query(query: &str) -> (usize, usize, bool) {
    let mut cursor = 0usize;
    let mut limit = 50usize;
    let mut include_full = false;
    for kv in query.split('&') {
        if let Some((k, v)) = kv.split_once('=') {
            match k {
                "cursor" | "offset" => cursor = v.parse().unwrap_or(0),
                "limit" => limit = v.parse::<usize>().unwrap_or(50).clamp(1, 500),
                "includeFull" | "include_full" | "full" => {
                    include_full = matches!(url_decode(v).as_str(), "1" | "true" | "yes")
                }
                _ => {}
            }
        }
    }
    (cursor, limit, include_full)
}
