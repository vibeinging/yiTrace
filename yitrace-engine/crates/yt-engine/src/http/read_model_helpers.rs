fn trace_search_matches(span: &FoldedSpan, spec: &TraceSearchSpec) -> bool {
    if let Some(session_id) = spec.session_id {
        if span.session_id != Some(session_id) {
            return false;
        }
    }
    if let Some(span_id) = spec.span_id {
        if span.span_id != span_id {
            return false;
        }
    }
    if let Some(expected) = &spec.external_trace_id {
        if span.external_trace_id.as_deref() != Some(expected.as_str()) {
            return false;
        }
    }
    if let Some(expected) = &spec.external_span_id {
        if span.external_span_id.as_deref() != Some(expected.as_str()) {
            return false;
        }
    }
    if let Some(expected) = &spec.external_session_id {
        if span.external_session_id.as_deref() != Some(expected.as_str()) {
            return false;
        }
    }
    if let Some(status) = spec.status {
        if span.status != Some(status) {
            return false;
        }
    }
    if let Some(agent) = &spec.agent_name {
        if span.agent_name.as_deref() != Some(agent.as_str()) {
            return false;
        }
    }
    if let Some(tool) = &spec.tool_name {
        if span.tool_name.as_deref() != Some(tool.as_str()) {
            return false;
        }
    }
    if let Some(model) = &spec.model {
        if span.model.as_deref() != Some(model.as_str()) {
            return false;
        }
    }
    for (key, expected) in &spec.attrs {
        if span.attrs.get(key) != Some(expected) {
            return false;
        }
    }
    if let Some(text) = &spec.text {
        let text = text.as_str();
        let hit = span
            .input_text
            .as_deref()
            .map(|v| v.contains(text))
            .unwrap_or(false)
            || span
                .output_text
                .as_deref()
                .map(|v| v.contains(text))
                .unwrap_or(false)
            || span.logs.iter().any(|log| log.contains(text));
        if !hit {
            return false;
        }
    }
    true
}

fn sort_trace_search_spans(spans: &mut [FoldedSpan], sort_by: &str) {
    match sort_by
        .to_ascii_lowercase()
        .replace(['_', '-'], "")
        .as_str()
    {
        "duration" | "durationns" => spans.sort_by(|a, b| {
            b.duration_ns
                .cmp(&a.duration_ns)
                .then_with(|| a.trace_id.cmp(&b.trace_id))
        }),
        "tokens" | "totaltokens" => spans.sort_by(|a, b| {
            let at = a.input_tokens.unwrap_or(0) + a.output_tokens.unwrap_or(0);
            let bt = b.input_tokens.unwrap_or(0) + b.output_tokens.unwrap_or(0);
            bt.cmp(&at).then_with(|| a.trace_id.cmp(&b.trace_id))
        }),
        "status" => spans.sort_by(|a, b| {
            b.status
                .unwrap_or(0)
                .cmp(&a.status.unwrap_or(0))
                .then_with(|| a.trace_id.cmp(&b.trace_id))
        }),
        _ => spans.sort_by_key(|s| (s.trace_id, s.span_id)),
    }
}

fn trace_search_item_json(s: &FoldedSpan) -> String {
    format!(
        r#"{{"traceId":"{}","spanId":"{}","externalTraceId":{},"externalSpanId":{},"externalSessionId":{},"sessionId":{},"spanName":{},"displayName":{},"status":{},"durationNs":{},"inputTokens":{},"outputTokens":{},"agentName":{},"toolName":{},"model":{},"inputText":{},"outputText":{},"logs":[{}],"attrs":{}}}"#,
        s.trace_id,
        s.span_id,
        json_opt_str(s.external_trace_id.as_deref()),
        json_opt_str(s.external_span_id.as_deref()),
        json_opt_str(s.external_session_id.as_deref()),
        s.session_id.map_or("null".to_string(), |v| v.to_string()),
        json_opt_str(s.span_name.as_deref()),
        json_opt_str(s.display_name.as_deref()),
        s.status.map_or("null".to_string(), |v| v.to_string()),
        s.duration_ns.map_or("null".to_string(), |v| v.to_string()),
        s.input_tokens.map_or("null".to_string(), |v| v.to_string()),
        s.output_tokens
            .map_or("null".to_string(), |v| v.to_string()),
        json_opt_str(s.agent_name.as_deref()),
        json_opt_str(s.tool_name.as_deref()),
        json_opt_str(s.model.as_deref()),
        s.input_text
            .as_ref()
            .map_or("null".to_string(), |v| json_string_value(v)),
        s.output_text
            .as_ref()
            .map_or("null".to_string(), |v| json_string_value(v)),
        s.logs
            .iter()
            .map(|v| json_string_value(v))
            .collect::<Vec<_>>()
            .join(","),
        json_attrs(&s.attrs),
    )
}

fn group_by_fields(v: &crate::wire::Json) -> Result<Vec<String>, String> {
    let fields = group_by_fields_optional(v);
    if fields.is_empty() {
        Err("groupBy required".to_string())
    } else {
        Ok(fields)
    }
}

fn group_by_fields_optional(v: &crate::wire::Json) -> Vec<String> {
    use crate::wire::Json;
    let Some(raw) = json_field_alias(v, &["groupBy", "group_by", "by"]) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut push = |s: &str| {
        let key = normalize_group_key(s);
        if !key.is_empty() && !out.contains(&key) {
            out.push(key);
        }
    };
    match raw {
        Json::Str(s) | Json::Num(s) => {
            for part in s.split(',') {
                push(part);
            }
        }
        Json::Arr(items) => {
            for item in items {
                if let Some(s) = item.as_str() {
                    push(s);
                }
            }
        }
        _ => {}
    }
    out
}

fn normalize_group_key(raw: &str) -> String {
    let lower = raw.trim().replace('-', "_").to_ascii_lowercase();
    let compact = lower.replace('_', "");
    match compact.as_str() {
        "projectid" => "project_id".to_string(),
        "callsite" => "call_site".to_string(),
        "taskfingerprint" => "task_fingerprint".to_string(),
        "loopid" => "loop_id".to_string(),
        "validationstatus" => "validation_status".to_string(),
        "reviewstatus" => "review_status".to_string(),
        "evalstatus" => "eval_status".to_string(),
        "sessionid" => "session_id".to_string(),
        "traceid" => "trace_id".to_string(),
        "spanid" => "span_id".to_string(),
        "agentname" => "agent_name".to_string(),
        "toolname" => "tool_name".to_string(),
        _ => lower,
    }
}

fn span_group_value_json(span: &FoldedSpan, field: &str) -> String {
    match field {
        "trace_id" => span.trace_id.to_string(),
        "span_id" => span.span_id.to_string(),
        "session_id" => span
            .session_id
            .map_or("null".to_string(), |v| v.to_string()),
        "status" => span.status.map_or("null".to_string(), |v| v.to_string()),
        "agent_name" => span
            .agent_name
            .as_ref()
            .map_or("null".to_string(), |v| json_string_value(v)),
        "tool_name" => span
            .tool_name
            .as_ref()
            .map_or("null".to_string(), |v| json_string_value(v)),
        "model" => span
            .model
            .as_ref()
            .map_or("null".to_string(), |v| json_string_value(v)),
        other => span
            .attrs
            .get(other)
            .cloned()
            .unwrap_or_else(|| "null".to_string()),
    }
}

struct TraceAggregateBucket {
    values: Vec<String>,
    span_count: usize,
    trace_ids: std::collections::BTreeSet<u64>,
    error_count: usize,
    duration_sum_ns: u128,
    duration_max_ns: u64,
    input_tokens: u64,
    output_tokens: u64,
}

impl TraceAggregateBucket {
    fn new(values: Vec<String>) -> Self {
        Self {
            values,
            span_count: 0,
            trace_ids: std::collections::BTreeSet::new(),
            error_count: 0,
            duration_sum_ns: 0,
            duration_max_ns: 0,
            input_tokens: 0,
            output_tokens: 0,
        }
    }

    fn add(&mut self, span: &FoldedSpan) {
        self.span_count += 1;
        self.trace_ids.insert(span.trace_id);
        if span.status.unwrap_or(0) != 0 {
            self.error_count += 1;
        }
        if let Some(duration) = span.duration_ns {
            self.duration_sum_ns += duration as u128;
            self.duration_max_ns = self.duration_max_ns.max(duration);
        }
        self.input_tokens += span.input_tokens.unwrap_or(0);
        self.output_tokens += span.output_tokens.unwrap_or(0);
    }

    fn to_json(self, group_by: &[String]) -> String {
        let key = group_by
            .iter()
            .zip(self.values.iter())
            .map(|(field, value)| format!("{}:{}", json_string_value(field), value))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            r#"{{"key":{{{}}},"spanCount":{},"traceCount":{},"errorCount":{},"durationSumNs":"{}","durationMaxNs":{},"inputTokens":{},"outputTokens":{}}}"#,
            key,
            self.span_count,
            self.trace_ids.len(),
            self.error_count,
            self.duration_sum_ns,
            self.duration_max_ns,
            self.input_tokens,
            self.output_tokens,
        )
    }
}

#[derive(Clone)]
struct StorageStatsBucket {
    values: Vec<String>,
    trace_ids: std::collections::BTreeSet<u64>,
    span_count: usize,
    event_count: usize,
    estimated_bytes: usize,
}

impl StorageStatsBucket {
    fn from_spans(spans: &[FoldedSpan], values: &[String]) -> Self {
        let mut out = Self {
            values: values.to_vec(),
            trace_ids: std::collections::BTreeSet::new(),
            span_count: 0,
            event_count: 0,
            estimated_bytes: 0,
        };
        for span in spans {
            out.trace_ids.insert(span.trace_id);
            out.span_count += 1;
            out.event_count += span.event_count;
            out.estimated_bytes += estimate_span_bytes(span);
        }
        out
    }

    fn to_json(self, group_by: &[String]) -> String {
        let key = group_by
            .iter()
            .zip(self.values.iter())
            .map(|(field, value)| format!("{}:{}", json_string_value(field), value))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            r#"{{"key":{{{}}},"traceCount":{},"spanCount":{},"eventCount":{},"estimatedBytes":{}}}"#,
            key,
            self.trace_ids.len(),
            self.span_count,
            self.event_count,
            self.estimated_bytes,
        )
    }
}

fn estimate_span_bytes(span: &FoldedSpan) -> usize {
    let fixed = 96usize;
    fixed
        + span.external_trace_id.as_ref().map_or(0, String::len)
        + span.external_span_id.as_ref().map_or(0, String::len)
        + span.external_parent_span_id.as_ref().map_or(0, String::len)
        + span.external_session_id.as_ref().map_or(0, String::len)
        + span.agent_name.as_ref().map_or(0, String::len)
        + span.tool_name.as_ref().map_or(0, String::len)
        + span.model.as_ref().map_or(0, String::len)
        + span.input_text.as_ref().map_or(0, String::len)
        + span.output_text.as_ref().map_or(0, String::len)
        + span.logs.iter().map(String::len).sum::<usize>()
        + span
            .attrs
            .iter()
            .map(|(k, v)| k.len() + v.len())
            .sum::<usize>()
}
