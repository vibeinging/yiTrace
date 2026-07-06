fn trace_diff_delta_json(
    left: Option<&TaskTraceSummaryBucket>,
    right: Option<&TaskTraceSummaryBucket>,
) -> String {
    let li = left.map(|b| b.input_tokens).unwrap_or(0);
    let lo = left.map(|b| b.output_tokens).unwrap_or(0);
    let lt = left.map(|b| b.total_tokens).unwrap_or(0);
    let lc = left.map(|b| b.cost_usd_nanos).unwrap_or(0);
    let ld = left.map(|b| b.duration_sum_ns).unwrap_or(0);
    let le = left.map(|b| b.error_count).unwrap_or(0);
    let ls = left.map(|b| b.span_count).unwrap_or(0);
    let ri = right.map(|b| b.input_tokens).unwrap_or(0);
    let ro = right.map(|b| b.output_tokens).unwrap_or(0);
    let rt = right.map(|b| b.total_tokens).unwrap_or(0);
    let rc = right.map(|b| b.cost_usd_nanos).unwrap_or(0);
    let rd = right.map(|b| b.duration_sum_ns).unwrap_or(0);
    let re = right.map(|b| b.error_count).unwrap_or(0);
    let rs = right.map(|b| b.span_count).unwrap_or(0);
    format!(
        r#"{{"spanCount":{},"errorCount":{},"durationNs":{},"inputTokens":{},"outputTokens":{},"totalTokens":{},"costUsdNanos":{},"costUsd":{}}}"#,
        rs as i128 - ls as i128,
        re as i128 - le as i128,
        rd as i128 - ld as i128,
        ri as i128 - li as i128,
        ro as i128 - lo as i128,
        rt as i128 - lt as i128,
        rc as i128 - lc as i128,
        format!("{:.6}", (rc as i128 - lc as i128) as f64 / 1_000_000_000.0),
    )
}

fn trace_diff_trajectory_json(left: &[FoldedSpan], right: &[FoldedSpan]) -> String {
    let left_steps = trajectory_steps(left);
    let right_steps = trajectory_steps(right);
    let left_sig = trajectory_signature(&left_steps);
    let right_sig = trajectory_signature(&right_steps);
    format!(
        r#"{{"left":{},"right":{},"same":{}}}"#,
        trajectory_summary_json(&left_steps, left_sig),
        trajectory_summary_json(&right_steps, right_sig),
        if left_sig == right_sig {
            "true"
        } else {
            "false"
        },
    )
}

fn trajectory_summary_json(steps: &[String], signature: u64) -> String {
    trajectory_summary_json_with_signature(steps, &format!("fnv1a64:{signature:016x}"))
}

fn trajectory_summary_json_with_signature(steps: &[String], signature: &str) -> String {
    let steps_json = steps
        .iter()
        .map(|s| json_string_value(s))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"{{"signature":"{}","stepCount":{},"steps":[{}]}}"#,
        json_escape(signature),
        steps.len(),
        steps_json,
    )
}

fn trajectory_steps(spans: &[FoldedSpan]) -> Vec<String> {
    crate::trajectory_steps_for_spans(spans)
}

fn trajectory_signature(steps: &[String]) -> u64 {
    crate::trajectory_signature_value(steps)
}

fn trajectory_signature_string(steps: &[String]) -> String {
    crate::trajectory_signature_label(steps)
}

fn ordered_step_diff(
    golden_steps: &[String],
    trace_steps: &[String],
) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut common = Vec::new();
    let mut missing = Vec::new();
    let mut trace_cursor = 0usize;
    for golden_step in golden_steps {
        if trace_cursor >= trace_steps.len() {
            missing.push(golden_step.clone());
            continue;
        }
        if let Some(offset) = trace_steps[trace_cursor..]
            .iter()
            .position(|trace_step| trace_step == golden_step)
        {
            common.push(golden_step.clone());
            trace_cursor += offset + 1;
        } else {
            missing.push(golden_step.clone());
        }
    }

    let mut extra = Vec::new();
    let mut common_iter = common.iter();
    let mut next_common = common_iter.next();
    for trace_step in trace_steps {
        if next_common.map(|step| step == trace_step).unwrap_or(false) {
            next_common = common_iter.next();
        } else {
            extra.push(trace_step.clone());
        }
    }
    (common, missing, extra)
}

fn trajectory_step(s: &FoldedSpan) -> String {
    let (kind, name) = trajectory_step_kind_name(s);
    let mut out = format!(
        "{}:{}",
        normalize_trajectory_part(kind),
        normalize_trajectory_part(&name)
    );
    for key in ["phase", "validator"] {
        if let Some(value) = crate::folded_span_attr_value(s, key) {
            out.push('|');
            out.push_str(key);
            out.push(':');
            out.push_str(&normalize_trajectory_part(&json_compact_label(value)));
        }
    }
    out
}

fn trajectory_step_kind_name(s: &FoldedSpan) -> (&'static str, String) {
    if let Some(tool) = &s.tool_name {
        ("tool", tool.clone())
    } else if let Some(agent) = &s.agent_name {
        ("agent", agent.clone())
    } else if let Some(model) = &s.model {
        ("llm", model.clone())
    } else {
        ("other", format!("span {}", s.span_id))
    }
}

fn normalize_trajectory_part(value: &str) -> String {
    value
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_whitespace() || matches!(c, '|' | ':' | '\0') {
                '_'
            } else {
                c
            }
        })
        .collect()
}

fn trace_diff_route_json(spans: &[FoldedSpan]) -> String {
    format!(
        "[{}]",
        spans
            .iter()
            .enumerate()
            .map(|(idx, span)| trace_diff_route_step_json(idx, span))
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn trace_diff_route_step_json(index: usize, s: &FoldedSpan) -> String {
    format!(
        r#"{{"spanId":"{}","externalSpanId":{},"kind":"{}","name":"{}","spanOrdinal":{},"sortKey":"{:020}:{:020}","agentName":{},"toolName":{},"model":{},"status":{},"statusText":"{}","fields":{}}}"#,
        s.span_id,
        json_opt_str(s.external_span_id.as_deref()),
        folded_kind(s),
        json_escape(&folded_name(s)),
        index,
        index,
        s.span_id,
        json_opt_str(s.agent_name.as_deref()),
        json_opt_str(s.tool_name.as_deref()),
        json_opt_str(s.model.as_deref()),
        s.status
            .map_or("null".to_string(), |status| status.to_string()),
        if s.status.unwrap_or(0) == 0 {
            "ok"
        } else {
            "error"
        },
        json_folded_agent_fields(s),
    )
}

fn trace_diff_steps_json(left: &[FoldedSpan], right: &[FoldedSpan]) -> String {
    let len = left.len().max(right.len());
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        out.push(trace_diff_step_json(i, left.get(i), right.get(i)));
    }
    format!("[{}]", out.join(","))
}

fn trace_diff_step_json(
    index: usize,
    left: Option<&FoldedSpan>,
    right: Option<&FoldedSpan>,
) -> String {
    let changes = trace_diff_step_changes(left, right);
    let status = match (left, right, changes.is_empty()) {
        (Some(_), Some(_), true) => "same",
        (Some(_), Some(_), false) => "changed",
        (Some(_), None, _) => "left_only",
        (None, Some(_), _) => "right_only",
        (None, None, _) => "same",
    };
    let changes_json = changes
        .iter()
        .map(|c| json_string_value(c))
        .collect::<Vec<_>>()
        .join(",");
    let duration_delta = right.and_then(|s| s.duration_ns).unwrap_or(0) as i128
        - left.and_then(|s| s.duration_ns).unwrap_or(0) as i128;
    let token_delta = right.map(folded_total_tokens).unwrap_or(0) as i128
        - left.map(folded_total_tokens).unwrap_or(0) as i128;
    let cost_delta = right.map(folded_cost_usd_nanos).unwrap_or(0) as i128
        - left.map(folded_cost_usd_nanos).unwrap_or(0) as i128;
    format!(
        r#"{{"index":{},"status":"{}","changes":[{}],"left":{},"right":{},"delta":{{"durationNs":{},"totalTokens":{},"costUsdNanos":{},"costUsd":{}}}}}"#,
        index,
        status,
        changes_json,
        left.map(trace_diff_span_json)
            .unwrap_or_else(|| "null".to_string()),
        right
            .map(trace_diff_span_json)
            .unwrap_or_else(|| "null".to_string()),
        duration_delta,
        token_delta,
        cost_delta,
        format!("{:.6}", cost_delta as f64 / 1_000_000_000.0),
    )
}

fn trace_diff_span_json(s: &FoldedSpan) -> String {
    format!(
        r#"{{"traceId":"{}","spanId":"{}","externalTraceId":{},"externalSpanId":{},"kind":"{}","name":"{}","status":{},"statusText":"{}","durationNs":{},"usage":{},"costUsd":{},"costDetail":{},"evalScore":{},"evalLabel":{},"agentName":{},"toolName":{},"model":{},"provider":{},"inputPreview":{},"outputPreview":{},"fields":{}}}"#,
        s.trace_id,
        s.span_id,
        json_opt_str(s.external_trace_id.as_deref()),
        json_opt_str(s.external_span_id.as_deref()),
        folded_kind(s),
        json_escape(&folded_name(s)),
        s.status
            .map_or("null".to_string(), |status| status.to_string()),
        if s.status.unwrap_or(0) == 0 {
            "ok"
        } else {
            "error"
        },
        s.duration_ns.map_or("null".to_string(), |d| d.to_string()),
        folded_usage_json(s),
        cost_usd_num_from_nanos(folded_cost_usd_nanos(s)),
        cost_detail_json(
            folded_cost_usd_nanos(s),
            s.cost_currency.as_deref(),
            folded_cost_source(s),
        ),
        s.eval_score
            .map_or("null".to_string(), |score| score.to_string()),
        json_opt_str(s.eval_label.as_deref()),
        json_opt_str(s.agent_name.as_deref()),
        json_opt_str(s.tool_name.as_deref()),
        json_opt_str(s.model.as_deref()),
        json_opt_str(s.provider.as_deref()),
        json_opt_preview(s.input_text.as_deref()),
        json_opt_preview(s.output_text.as_deref()),
        json_folded_agent_fields(s),
    )
}

fn trace_diff_step_changes(left: Option<&FoldedSpan>, right: Option<&FoldedSpan>) -> Vec<String> {
    let (Some(left), Some(right)) = (left, right) else {
        return Vec::new();
    };
    let mut changes = Vec::new();
    if folded_kind(left) != folded_kind(right) {
        changes.push("kind".to_string());
    }
    if folded_name(left) != folded_name(right) {
        changes.push("name".to_string());
    }
    if left.status != right.status {
        changes.push("status".to_string());
    }
    if left.agent_name != right.agent_name {
        changes.push("agentName".to_string());
    }
    if left.tool_name != right.tool_name {
        changes.push("toolName".to_string());
    }
    if left.model != right.model {
        changes.push("model".to_string());
    }
    for key in [
        "skill",
        "mode",
        "call_site",
        "task_fingerprint",
        "loop_id",
        "phase",
        "validation_status",
        "stop_reason",
        "validator",
    ] {
        if crate::folded_span_attr_value(left, key) != crate::folded_span_attr_value(right, key) {
            changes.push(key.to_string());
        }
    }
    if left.duration_ns != right.duration_ns {
        changes.push("durationNs".to_string());
    }
    if folded_total_tokens(left) != folded_total_tokens(right) {
        changes.push("totalTokens".to_string());
    }
    if folded_cost_usd_nanos(left) != folded_cost_usd_nanos(right) {
        changes.push("costUsd".to_string());
    }
    if left.eval_score != right.eval_score {
        changes.push("evalScore".to_string());
    }
    if left.eval_label != right.eval_label {
        changes.push("evalLabel".to_string());
    }
    if left.output_text != right.output_text {
        changes.push("outputText".to_string());
    }
    changes
}
