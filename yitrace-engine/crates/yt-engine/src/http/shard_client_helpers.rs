fn empty_replication_status() -> crate::ReplicationStatus {
    crate::ReplicationStatus {
        committed_tail: 0,
        manifest_version: 0,
        memtable_watermark: 0,
        memtable_rows: 0,
        segment_count: 0,
    }
}

fn normalize_remote_addr(addr: &str) -> String {
    let trimmed = addr.trim();
    let without_scheme = trimmed.strip_prefix("http://").unwrap_or(trimmed);
    without_scheme
        .split('/')
        .next()
        .unwrap_or(without_scheme)
        .to_string()
}

fn parse_http_response(raw: &str) -> Result<(u16, String), ShardClientError> {
    let (head, body) = raw
        .split_once("\r\n\r\n")
        .ok_or_else(|| ShardClientError::bad_gateway("remote shard returned malformed HTTP"))?;
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| ShardClientError::bad_gateway("remote shard returned bad HTTP status"))?;
    Ok((status, body.to_string()))
}

fn compact_error_body(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        "remote shard request failed".to_string()
    } else {
        trimmed.chars().take(240).collect()
    }
}

fn is_retryable_error(err: &ShardClientError) -> bool {
    err.status == 502 || err.status == 503 || err.status == 504 || err.status == 429
}

fn is_retryable_status(status: u16) -> bool {
    status == 429 || status >= 500
}

fn is_retry_safe_remote_request(method: &str, path: &str) -> bool {
    let (base, _) = path.split_once('?').unwrap_or((path, ""));
    match method {
        "GET" | "HEAD" => true,
        "POST" => matches!(
            base,
            "/v1/ingest"
                | "/v1/search"
                | "/v1/trace-search"
                | "/v1/trace-aggregate"
                | "/v1/trace-aggregates"
                | "/v1/trajectory-groups"
                | "/v1/trajectory-aggregate"
                | "/v1/trace-trajectories"
                | "/v1/trajectories"
                | "/v1/storage-stats"
                | "/v1/storage/stats"
                | "/v1/retention-plan"
                | "/v1/retention/plan"
                | "/v1/golden-path-export"
                | "/v1/golden-paths/export"
                | "/v1/golden-path-health"
                | "/v1/golden-paths/health"
                | "/v1/path-adherence"
                | "/v1/golden-path-adherence"
                | "/v1/golden-path-evidence"
                | "/v1/golden-paths/evidence"
                | "/v1/retention-audits"
        ),
        _ => false,
    }
}

fn wire_batch_json(records: &[crate::WireRecord]) -> String {
    let items: Vec<String> = records.iter().map(wire_record_json).collect();
    format!("[{}]", items.join(","))
}

fn wire_record_json(record: &crate::WireRecord) -> String {
    let mut fields = vec![
        format!(r#""trace_id":{}"#, record.trace_id),
        format!(r#""span_id":{}"#, record.span_id),
        format!(r#""ts":{}"#, record.ts),
        format!(r#""seq":{}"#, record.seq),
        format!(r#""event_type":{}"#, record.event_type_tag),
        format!(
            r#""ext_span_id":"{}""#,
            http_json_escape(&record.ext_span_id)
        ),
    ];
    wire_opt_u64("parent_span_id", record.parent_span_id, &mut fields);
    wire_opt_u64("session_id", record.session_id, &mut fields);
    wire_opt_u64("tenant_id", record.tenant_id, &mut fields);
    wire_opt_u64("status", record.status.map(u64::from), &mut fields);
    wire_opt_u64("duration_ns", record.duration_ns, &mut fields);
    wire_opt_u64("input_tokens", record.input_tokens, &mut fields);
    wire_opt_u64("output_tokens", record.output_tokens, &mut fields);
    wire_opt_u64(
        "cached_input_tokens",
        record.cached_input_tokens,
        &mut fields,
    );
    wire_opt_u64("reasoning_tokens", record.reasoning_tokens, &mut fields);
    wire_opt_u64("total_tokens", record.total_tokens, &mut fields);
    wire_opt_u64("cost_usd_nanos", record.cost_usd_nanos, &mut fields);
    wire_opt_string("cost_currency", &record.cost_currency, &mut fields);
    wire_opt_string("provider", &record.provider, &mut fields);
    wire_opt_string("external_trace_id", &record.external_trace_id, &mut fields);
    wire_opt_string("external_span_id", &record.external_span_id, &mut fields);
    wire_opt_string(
        "external_parent_span_id",
        &record.external_parent_span_id,
        &mut fields,
    );
    wire_opt_string(
        "external_session_id",
        &record.external_session_id,
        &mut fields,
    );
    wire_opt_string("agent_name", &record.agent_name, &mut fields);
    wire_opt_string("tool_name", &record.tool_name, &mut fields);
    wire_opt_string("model", &record.model, &mut fields);
    wire_opt_string("input_text", &record.input_text, &mut fields);
    wire_opt_string("output_text", &record.output_text, &mut fields);
    if !record.logs.is_empty() {
        let logs: Vec<String> = record
            .logs
            .iter()
            .map(|log| format!(r#""{}""#, http_json_escape(log)))
            .collect();
        fields.push(format!(r#""logs":[{}]"#, logs.join(",")));
    }
    if !record.attrs.is_empty() {
        let attrs: Vec<String> = record
            .attrs
            .iter()
            .map(|(key, value)| format!(r#""{}":{}"#, http_json_escape(key), value))
            .collect();
        fields.push(format!(r#""attrs":{{{}}}"#, attrs.join(",")));
    }
    format!("{{{}}}", fields.join(","))
}

fn wire_opt_u64(key: &str, value: Option<u64>, fields: &mut Vec<String>) {
    if let Some(value) = value {
        fields.push(format!(r#""{key}":{value}"#));
    }
}

fn wire_opt_string(key: &str, value: &Option<String>, fields: &mut Vec<String>) {
    if let Some(value) = value {
        fields.push(format!(r#""{key}":"{}""#, http_json_escape(value)));
    }
}

fn http_json_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
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

fn parse_remote_replication_status(body: &str) -> Option<crate::ReplicationStatus> {
    let root = crate::wire::parse(body).ok()?;
    let shard = json_field_alias(&root, &["shards"])?.as_array().first()?;
    Some(crate::ReplicationStatus {
        committed_tail: json_field_alias(shard, &["committedTail", "committed_tail"])?.as_u64()?,
        manifest_version: json_field_alias(shard, &["manifestVersion", "manifest_version"])?
            .as_u64()?,
        memtable_watermark: json_field_alias(shard, &["memtableWatermark", "memtable_watermark"])?
            .as_u64()?,
        memtable_rows: json_field_alias(shard, &["memtableRows", "memtable_rows"])?.as_u64()?
            as usize,
        segment_count: json_field_alias(shard, &["segmentCount", "segment_count"])?.as_u64()?
            as usize,
    })
}

fn parse_remote_search_hits(body: &str) -> Result<Vec<(FoldedSpan, f32)>, String> {
    let root = crate::wire::parse(body)?;
    let items: Vec<&crate::wire::Json> = match &root {
        crate::wire::Json::Arr(items) => items.iter().collect(),
        crate::wire::Json::Obj(_) => json_field_alias(&root, &["items"])
            .map(|items| items.as_array().iter().collect())
            .unwrap_or_default(),
        _ => return Err("remote search response must be an array or object".to_string()),
    };
    items
        .iter()
        .map(|item| remote_search_hit_from_json(item))
        .collect()
}

fn remote_search_hit_from_json(item: &crate::wire::Json) -> Result<(FoldedSpan, f32), String> {
    let trace_id = json_field_alias(item, &["trace_id", "traceId"])
        .and_then(crate::wire::Json::as_u64)
        .ok_or_else(|| "remote search hit missing trace_id".to_string())?;
    let span_id = json_field_alias(item, &["span_id", "spanId"])
        .and_then(crate::wire::Json::as_u64)
        .ok_or_else(|| "remote search hit missing span_id".to_string())?;
    let score = json_field_alias(item, &["score"])
        .and_then(crate::wire::Json::as_f32)
        .unwrap_or(0.0);
    let fields = json_field_alias(item, &["fields"]);
    let mut attrs = remote_json_object_map(json_field_alias(item, &["attrs"]));
    if let Some(crate::wire::Json::Obj(kvs)) = fields {
        for (key, value) in kvs {
            attrs
                .entry(key.clone())
                .or_insert_with(|| value.to_compact_json());
        }
    }
    let span = FoldedSpan {
        trace_id,
        span_id,
        parent_span_id: json_field_alias(item, &["parent_span_id", "parentSpanId"])
            .and_then(crate::wire::Json::as_u64),
        status: json_field_alias(item, &["status"])
            .and_then(crate::wire::Json::as_u64)
            .map(|value| value as u8),
        duration_ns: json_field_alias(item, &["duration_ns", "durationNs"])
            .and_then(crate::wire::Json::as_u64),
        input_tokens: json_field_alias(item, &["input_tokens", "inputTokens"])
            .and_then(crate::wire::Json::as_u64),
        output_tokens: json_field_alias(item, &["output_tokens", "outputTokens"])
            .and_then(crate::wire::Json::as_u64),
        cached_input_tokens: json_field_alias(item, &["cached_input_tokens", "cachedInputTokens"])
            .and_then(crate::wire::Json::as_u64),
        reasoning_tokens: json_field_alias(item, &["reasoning_tokens", "reasoningTokens"])
            .and_then(crate::wire::Json::as_u64),
        total_tokens: json_field_alias(item, &["total_tokens", "totalTokens"])
            .and_then(crate::wire::Json::as_u64),
        cost_usd_nanos: json_field_alias(item, &["cost_usd_nanos", "costUsdNanos"])
            .and_then(crate::wire::Json::as_u64),
        cost_currency: remote_string_alias(item, &["cost_currency", "costCurrency"]),
        provider: remote_raw_field_string(fields, "provider").or_else(|| {
            remote_attr_json_string(&attrs, "provider")
                .or_else(|| remote_string_alias(item, &["provider"]))
        }),
        session_id: json_field_alias(item, &["session_id", "sessionId"])
            .and_then(crate::wire::Json::as_u64),
        tenant_id: json_field_alias(item, &["tenant_id", "tenantId"])
            .and_then(crate::wire::Json::as_u64),
        external_trace_id: remote_string_alias(item, &["external_trace_id", "externalTraceId"]),
        external_span_id: remote_string_alias(item, &["external_span_id", "externalSpanId"]),
        external_parent_span_id: remote_string_alias(
            item,
            &["external_parent_span_id", "externalParentSpanId"],
        ),
        external_session_id: remote_string_alias(
            item,
            &["external_session_id", "externalSessionId"],
        ),
        project_id: remote_compact_field_or_attr(fields, &attrs, "project_id"),
        skill: remote_compact_field_or_attr(fields, &attrs, "skill"),
        mode: remote_compact_field_or_attr(fields, &attrs, "mode"),
        call_site: remote_compact_field_or_attr(fields, &attrs, "call_site"),
        task_fingerprint: remote_compact_field_or_attr(fields, &attrs, "task_fingerprint"),
        loop_id: remote_compact_field_or_attr(fields, &attrs, "loop_id"),
        harness_version: remote_compact_field_or_attr(fields, &attrs, "harness_version"),
        schema_fingerprint: remote_compact_field_or_attr(fields, &attrs, "schema_fingerprint"),
        intent_signature: remote_compact_field_or_attr(fields, &attrs, "intent_signature"),
        validation_status: remote_compact_field_or_attr(fields, &attrs, "validation_status"),
        review_status: remote_compact_field_or_attr(fields, &attrs, "review_status"),
        eval_status: remote_compact_field_or_attr(fields, &attrs, "eval_status"),
        path_memory_id: remote_compact_field_or_attr(fields, &attrs, "path_memory_id"),
        stop_reason: remote_compact_field_or_attr(fields, &attrs, "stop_reason"),
        phase: remote_compact_field_or_attr(fields, &attrs, "phase"),
        validator: remote_compact_field_or_attr(fields, &attrs, "validator"),
        agent_name: remote_string_alias(item, &["agent_name", "agentName"]),
        tool_name: remote_string_alias(item, &["tool_name", "toolName"]),
        model: remote_raw_field_string(fields, "model")
            .or_else(|| remote_attr_json_string(&attrs, "model"))
            .or_else(|| remote_string_alias(item, &["model"])),
        input_text: remote_string_alias(item, &["input_text", "inputText"]),
        output_text: remote_string_alias(item, &["output_text", "outputText"]),
        eval_score: json_field_alias(item, &["eval_score", "evalScore"])
            .and_then(crate::wire::Json::as_u64)
            .map(|value| value.min(u32::MAX as u64) as u32),
        eval_label: remote_string_alias(item, &["eval_label", "evalLabel"]),
        logs: remote_string_array_alias(item, &["logs"]),
        attrs,
        event_count: json_field_alias(item, &["event_count", "eventCount"])
            .and_then(crate::wire::Json::as_u64)
            .unwrap_or(0) as usize,
    };
    Ok((span, score))
}

fn remote_json_object_map(
    value: Option<&crate::wire::Json>,
) -> std::collections::BTreeMap<String, String> {
    match value {
        Some(crate::wire::Json::Obj(kvs)) => kvs
            .iter()
            .map(|(key, value)| (key.clone(), value.to_compact_json()))
            .collect(),
        _ => std::collections::BTreeMap::new(),
    }
}

fn remote_string_alias(obj: &crate::wire::Json, names: &[&str]) -> Option<String> {
    json_field_alias(obj, names)
        .and_then(crate::wire::Json::as_str)
        .map(ToString::to_string)
}

fn remote_string_array_alias(obj: &crate::wire::Json, names: &[&str]) -> Vec<String> {
    let Some(value) = json_field_alias(obj, names) else {
        return Vec::new();
    };
    value
        .as_array()
        .iter()
        .filter_map(crate::wire::Json::as_str)
        .map(ToString::to_string)
        .collect()
}

fn remote_raw_field_string(fields: Option<&crate::wire::Json>, key: &str) -> Option<String> {
    fields?
        .get(key)
        .and_then(crate::wire::Json::as_str)
        .map(ToString::to_string)
}

fn remote_compact_field_or_attr(
    fields: Option<&crate::wire::Json>,
    attrs: &std::collections::BTreeMap<String, String>,
    key: &str,
) -> Option<String> {
    fields
        .and_then(|fields| fields.get(key))
        .map(crate::wire::Json::to_compact_json)
        .or_else(|| attrs.get(key).cloned())
}

fn remote_attr_json_string(
    attrs: &std::collections::BTreeMap<String, String>,
    key: &str,
) -> Option<String> {
    let value = attrs.get(key)?;
    match crate::wire::parse(value).ok()? {
        crate::wire::Json::Str(s) => Some(s),
        crate::wire::Json::Num(s) => Some(s),
        crate::wire::Json::Bool(v) => Some(v.to_string()),
        _ => None,
    }
}
