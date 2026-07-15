fn unique_trace_ids(spans: &[FoldedSpan]) -> Vec<u64> {
    spans
        .iter()
        .map(|s| s.trace_id)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn group_spans_by_trace(
    spans: Vec<FoldedSpan>,
) -> std::collections::BTreeMap<u64, Vec<FoldedSpan>> {
    let mut by_trace: std::collections::BTreeMap<u64, Vec<FoldedSpan>> =
        std::collections::BTreeMap::new();
    for span in spans {
        by_trace.entry(span.trace_id).or_default().push(span);
    }
    by_trace
}

fn sort_spans_for_trajectory(spans: &mut [FoldedSpan]) {
    let parents: std::collections::BTreeMap<u64, Option<u64>> = spans
        .iter()
        .map(|s| (s.span_id, s.parent_span_id))
        .collect();
    let depth_of = |mut id: u64| -> usize {
        let mut depth = 0usize;
        while let Some(Some(parent)) = parents.get(&id) {
            depth += 1;
            if depth > 64 {
                break;
            }
            id = *parent;
        }
        depth
    };
    spans.sort_by_key(|s| (depth_of(s.span_id), s.span_id));
}

fn folded_kind(span: &FoldedSpan) -> &'static str {
    if span.tool_name.is_some() {
        "tool"
    } else if span.model.is_some() {
        "llm"
    } else if span.agent_name.is_some() {
        "agent"
    } else {
        "span"
    }
}

fn folded_name(span: &FoldedSpan) -> String {
    span.tool_name
        .clone()
        .or_else(|| span.agent_name.clone())
        .or_else(|| span.model.clone())
        .or_else(|| span.logs.first().cloned())
        .unwrap_or_else(|| format!("span-{}", span.span_id))
}

fn attr_json<'a>(span: &'a FoldedSpan, key: &str) -> Option<&'a str> {
    span.attrs.get(key).map(String::as_str)
}

fn attr_label(span: &FoldedSpan, key: &str) -> Option<String> {
    attr_json(span, key).map(json_scalar_label)
}

fn attr_json_or_null(spans: &[FoldedSpan], key: &str) -> String {
    spans
        .iter()
        .find_map(|span| span.attrs.get(key).cloned())
        .unwrap_or_else(|| "null".to_string())
}

fn json_scalar_label(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        let inner = &trimmed[1..trimmed.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut escaped = false;
        for c in inner.chars() {
            if escaped {
                match c {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    other => out.push(other),
                }
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else {
                out.push(c);
            }
        }
        out
    } else {
        trimmed.to_string()
    }
}

fn attr_json_matches(actual: &str, expected: &str) -> bool {
    actual == expected || json_scalar_label(actual) == json_scalar_label(expected)
}

fn span_attrs_match(span: &FoldedSpan, attrs: &std::collections::BTreeMap<String, String>) -> bool {
    attrs.iter().all(|(key, expected)| {
        span.attrs
            .get(key)
            .map(|v| attr_json_matches(v, expected))
            .unwrap_or(false)
    })
}

fn trace_attrs_match(
    spans: &[FoldedSpan],
    attrs: &std::collections::BTreeMap<String, String>,
) -> bool {
    attrs.iter().all(|(key, expected)| {
        spans.iter().any(|span| {
            span.attrs
                .get(key)
                .map(|v| attr_json_matches(v, expected))
                .unwrap_or(false)
        })
    })
}

fn attrs_from_query(query: &str) -> std::collections::BTreeMap<String, String> {
    let mut attrs = std::collections::BTreeMap::new();
    for kv in query.split('&') {
        if kv.is_empty() {
            continue;
        }
        let (raw_key, raw_value) = kv.split_once('=').unwrap_or((kv, ""));
        let key = normalize_group_key(&url_decode(raw_key));
        let value = url_decode(raw_value);
        match key.as_str() {
            "cursor" | "limit" | "filter" => {}
            "attrs" => collect_attr_query_json(&value, &mut attrs),
            "project_id" | "skill" | "mode" | "call_site" | "task_fingerprint" | "loop_id"
            | "harness_version" | "schema_fingerprint" | "intent_signature"
            | "validation_status" | "review_status" | "eval_status" | "path_memory_id"
            | "stop_reason" | "phase" | "validator" => {
                attrs.insert(key, json_string_value(&value));
            }
            _ => {}
        }
    }
    attrs
}

fn cursor_limit_from_query(query: &str) -> (usize, usize) {
    let mut cursor = 0usize;
    let mut limit = 50usize;
    for kv in query.split('&') {
        if let Some((k, v)) = kv.split_once('=') {
            match k {
                "cursor" | "offset" => cursor = v.parse().unwrap_or(0),
                "limit" => limit = v.parse::<usize>().unwrap_or(50).clamp(1, 500),
                _ => {}
            }
        }
    }
    (cursor, limit)
}

#[derive(Default)]
struct TraceMetrics {
    span_count: usize,
    event_count: usize,
    duration_ns: u128,
    input_tokens: u64,
    output_tokens: u64,
    has_error: bool,
}

fn trace_metrics(spans: &[FoldedSpan]) -> TraceMetrics {
    let mut metrics = TraceMetrics::default();
    for span in spans {
        metrics.span_count += 1;
        metrics.event_count += span.event_count;
        metrics.duration_ns += span.duration_ns.unwrap_or(0) as u128;
        metrics.input_tokens += span.input_tokens.unwrap_or(0);
        metrics.output_tokens += span.output_tokens.unwrap_or(0);
        if span.status.unwrap_or(0) != 0 {
            metrics.has_error = true;
        }
    }
    metrics
}

fn trace_success(spans: &[FoldedSpan]) -> bool {
    let mut saw_pass = false;
    for span in spans {
        if span.status.unwrap_or(0) != 0 {
            return false;
        }
        if attr_json(span, "validation_status")
            .map(json_scalar_label)
            .map(|v| v == "fail" || v == "failed" || v == "error")
            .unwrap_or(false)
        {
            return false;
        }
        if attr_json(span, "validation_status")
            .map(json_scalar_label)
            .map(|v| v == "pass" || v == "passed" || v == "ok")
            .unwrap_or(false)
        {
            saw_pass = true;
        }
    }
    saw_pass || !spans.is_empty()
}

fn trajectory_step_key(span: &FoldedSpan) -> String {
    [
        folded_kind(span).to_string(),
        folded_name(span),
        span.tool_name.clone().unwrap_or_default(),
        span.model.clone().unwrap_or_default(),
        span.status.map_or("".to_string(), |v| v.to_string()),
    ]
    .into_iter()
    .map(|part| part.replace(['|', '>'], "/"))
    .collect::<Vec<_>>()
    .join("|")
}

fn trajectory_signature(spans: &[FoldedSpan]) -> String {
    spans
        .iter()
        .map(trajectory_step_key)
        .collect::<Vec<_>>()
        .join(">")
}

fn trajectory_steps(spans: &[FoldedSpan]) -> Vec<String> {
    spans.iter().map(trajectory_step_key).collect()
}

fn trajectory_steps_json(spans: &[FoldedSpan]) -> String {
    let items: Vec<String> = spans
        .iter()
        .enumerate()
        .map(|(index, span)| {
            format!(
                r#"{{"index":{},"spanId":"{}","externalSpanId":{},"parentSpanId":{},"kind":"{}","name":"{}","spanName":{},"displayName":{},"agentName":{},"toolName":{},"model":{},"status":{},"durationNs":{},"key":"{}"}}"#,
                index,
                span.span_id,
                json_opt_str(span.external_span_id.as_deref()),
                span.parent_span_id.map_or("null".to_string(), |v| json_string_value(&v.to_string())),
                folded_kind(span),
                json_escape(&folded_name(span)),
                json_opt_str(span.span_name.as_deref()),
                json_opt_str(span.display_name.as_deref()),
                json_opt_str(span.agent_name.as_deref()),
                json_opt_str(span.tool_name.as_deref()),
                json_opt_str(span.model.as_deref()),
                span.status.map_or("null".to_string(), |v| v.to_string()),
                span.duration_ns.map_or("null".to_string(), |v| v.to_string()),
                json_escape(&trajectory_step_key(span)),
            )
        })
        .collect();
    format!("[{}]", items.join(","))
}

fn trace_trajectory_summary_json(spans: &[FoldedSpan]) -> String {
    if spans.is_empty() {
        return "{}".to_string();
    }
    let metrics = trace_metrics(spans);
    let first = &spans[0];
    format!(
        r#"{{"traceId":"{}","externalTraceId":{},"sessionId":{},"externalSessionId":{},"status":"{}","spanCount":{},"eventCount":{},"durationNs":"{}","inputTokens":{},"outputTokens":{},"projectId":{},"taskFingerprint":{},"loopId":{},"skill":{},"mode":{},"validationStatus":{},"signature":"{}"}}"#,
        first.trace_id,
        json_opt_str(spans.iter().find_map(|s| s.external_trace_id.as_deref())),
        spans
            .iter()
            .find_map(|s| s.session_id)
            .map_or("null".to_string(), |v| v.to_string()),
        json_opt_str(spans.iter().find_map(|s| s.external_session_id.as_deref())),
        if metrics.has_error { "error" } else { "ok" },
        metrics.span_count,
        metrics.event_count,
        metrics.duration_ns,
        metrics.input_tokens,
        metrics.output_tokens,
        attr_json_or_null(spans, "project_id"),
        attr_json_or_null(spans, "task_fingerprint"),
        attr_json_or_null(spans, "loop_id"),
        attr_json_or_null(spans, "skill"),
        attr_json_or_null(spans, "mode"),
        attr_json_or_null(spans, "validation_status"),
        json_escape(&trajectory_signature(spans)),
    )
}

fn trace_trajectory_json(spans: &[FoldedSpan]) -> String {
    format!(
        r#"{{"summary":{},"steps":{}}}"#,
        trace_trajectory_summary_json(spans),
        trajectory_steps_json(spans),
    )
}

struct TrajectoryGroupBucket {
    signature: String,
    steps_json: String,
    trace_count: usize,
    span_count: usize,
    success_count: usize,
    error_count: usize,
    duration_ns: u128,
    input_tokens: u64,
    output_tokens: u64,
    examples: Vec<String>,
}

impl TrajectoryGroupBucket {
    fn new(signature: String, steps_json: String) -> Self {
        Self {
            signature,
            steps_json,
            trace_count: 0,
            span_count: 0,
            success_count: 0,
            error_count: 0,
            duration_ns: 0,
            input_tokens: 0,
            output_tokens: 0,
            examples: Vec::new(),
        }
    }

    fn add(&mut self, spans: &[FoldedSpan]) {
        let metrics = trace_metrics(spans);
        self.trace_count += 1;
        self.span_count += metrics.span_count;
        if trace_success(spans) {
            self.success_count += 1;
        }
        if metrics.has_error {
            self.error_count += 1;
        }
        self.duration_ns += metrics.duration_ns;
        self.input_tokens += metrics.input_tokens;
        self.output_tokens += metrics.output_tokens;
        if self.examples.len() < 3 {
            self.examples.push(trace_trajectory_summary_json(spans));
        }
    }

    fn to_json(self) -> String {
        format!(
            r#"{{"signature":"{}","traceCount":{},"spanCount":{},"successCount":{},"errorCount":{},"durationNs":"{}","inputTokens":{},"outputTokens":{},"steps":{},"examples":[{}]}}"#,
            json_escape(&self.signature),
            self.trace_count,
            self.span_count,
            self.success_count,
            self.error_count,
            self.duration_ns,
            self.input_tokens,
            self.output_tokens,
            self.steps_json,
            self.examples.join(","),
        )
    }
}

fn trace_diff_result_json(
    left: &[FoldedSpan],
    right: &[FoldedSpan],
    include_steps: bool,
) -> String {
    let left_steps = trajectory_steps(left);
    let right_steps = trajectory_steps(right);
    let mut common_prefix = 0usize;
    while common_prefix < left_steps.len()
        && common_prefix < right_steps.len()
        && left_steps[common_prefix] == right_steps[common_prefix]
    {
        common_prefix += 1;
    }
    let left_metrics = trace_metrics(left);
    let right_metrics = trace_metrics(right);
    let missing: Vec<String> = left_steps
        .iter()
        .skip(common_prefix)
        .map(|v| json_string_value(v))
        .collect();
    let extra: Vec<String> = right_steps
        .iter()
        .skip(common_prefix)
        .map(|v| json_string_value(v))
        .collect();
    let left_json = if include_steps {
        trace_trajectory_json(left)
    } else {
        trace_trajectory_summary_json(left)
    };
    let right_json = if include_steps {
        trace_trajectory_json(right)
    } else {
        trace_trajectory_summary_json(right)
    };
    format!(
        r#"{{"sameSignature":{},"commonPrefix":{},"left":{},"right":{},"delta":{{"durationNs":"{}","inputTokens":{},"outputTokens":{},"spanCount":{}}},"missingSteps":[{}],"extraSteps":[{}]}}"#,
        trajectory_signature(left) == trajectory_signature(right),
        common_prefix,
        left_json,
        right_json,
        (right_metrics.duration_ns as i128) - (left_metrics.duration_ns as i128),
        (right_metrics.input_tokens as i128) - (left_metrics.input_tokens as i128),
        (right_metrics.output_tokens as i128) - (left_metrics.output_tokens as i128),
        (right_metrics.span_count as i128) - (left_metrics.span_count as i128),
        missing.join(","),
        extra.join(","),
    )
}

struct LoopBucket {
    loop_id: String,
    trace_ids: std::collections::BTreeSet<u64>,
    span_count: usize,
    error_count: usize,
    duration_ns: u128,
    input_tokens: u64,
    output_tokens: u64,
    project_id: String,
    task_fingerprint: String,
    validation_status: String,
}

impl LoopBucket {
    fn new(loop_id: String) -> Self {
        Self {
            loop_id,
            trace_ids: std::collections::BTreeSet::new(),
            span_count: 0,
            error_count: 0,
            duration_ns: 0,
            input_tokens: 0,
            output_tokens: 0,
            project_id: "null".to_string(),
            task_fingerprint: "null".to_string(),
            validation_status: "null".to_string(),
        }
    }

    fn add(&mut self, span: &FoldedSpan) {
        self.trace_ids.insert(span.trace_id);
        self.span_count += 1;
        if span.status.unwrap_or(0) != 0 {
            self.error_count += 1;
        }
        self.duration_ns += span.duration_ns.unwrap_or(0) as u128;
        self.input_tokens += span.input_tokens.unwrap_or(0);
        self.output_tokens += span.output_tokens.unwrap_or(0);
        if self.project_id == "null" {
            self.project_id = span
                .attrs
                .get("project_id")
                .cloned()
                .unwrap_or_else(|| "null".to_string());
        }
        if self.task_fingerprint == "null" {
            self.task_fingerprint = span
                .attrs
                .get("task_fingerprint")
                .cloned()
                .unwrap_or_else(|| "null".to_string());
        }
        if self.validation_status == "null" {
            self.validation_status = span
                .attrs
                .get("validation_status")
                .cloned()
                .unwrap_or_else(|| "null".to_string());
        }
    }

    fn to_json(self) -> String {
        format!(
            r#"{{"loopId":"{}","traceCount":{},"spanCount":{},"errorCount":{},"durationNs":"{}","inputTokens":{},"outputTokens":{},"projectId":{},"taskFingerprint":{},"validationStatus":{}}}"#,
            json_escape(&self.loop_id),
            self.trace_ids.len(),
            self.span_count,
            self.error_count,
            self.duration_ns,
            self.input_tokens,
            self.output_tokens,
            self.project_id,
            self.task_fingerprint,
            self.validation_status,
        )
    }
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
