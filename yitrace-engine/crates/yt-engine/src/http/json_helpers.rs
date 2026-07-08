/// 极小 URL 解码（只处理 %XX 与 +）：会话过滤词可能是中文 → 解 percent-encoding。
fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let h = |c: u8| (c as char).to_digit(16);
                if let (Some(hi), Some(lo)) = (h(b[i + 1]), h(b[i + 2])) {
                    out.push((hi * 16 + lo) as u8);
                    i += 3;
                    continue;
                }
                out.push(b[i]);
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 千分制成本（与 SDK/前端 mock 同口径）：输入 8e-7、输出 4e-6 每 token。输出 JSON number。
fn cost_num(in_tok: u64, out_tok: u64) -> String {
    format!("{:.3}", in_tok as f64 * 8e-7 + out_tok as f64 * 4e-6)
}

fn parse_id_or_hash(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        None
    } else {
        Some(
            s.parse::<u64>()
                .unwrap_or_else(|_| yt_core::event::fnv1a64(s.as_bytes())),
        )
    }
}

fn json_id_or_hash(v: &crate::wire::Json) -> Option<u64> {
    v.as_u64()
        .or_else(|| v.as_str().map(|s| yt_core::event::fnv1a64(s.as_bytes())))
}

fn json_id_with_external(v: &crate::wire::Json) -> Option<(u64, Option<String>)> {
    match v {
        crate::wire::Json::Num(s) => s.parse::<u64>().ok().map(|id| (id, None)),
        crate::wire::Json::Str(s) => match s.parse::<u64>() {
            Ok(id) => Some((id, None)),
            Err(_) => Some((yt_core::event::fnv1a64(s.as_bytes()), Some(s.clone()))),
        },
        _ => None,
    }
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

fn json_log_events(events: &[crate::SpanLogEvent]) -> String {
    if events.is_empty() {
        return "[]".to_string();
    }
    format!(
        "[{}]",
        events
            .iter()
            .map(|ev| {
                let messages = ev
                    .messages
                    .iter()
                    .map(|m| json_string_value(m))
                    .collect::<Vec<_>>()
                    .join(",");
                format!(
                    r#"{{"eventId":"{}","ts":{},"seq":{},"eventType":{},"messages":[{}],"attrs":{}}}"#,
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

fn json_read_plan(plan: &ReadPlanStats) -> String {
    let unsupported = plan
        .unsupported_attr_keys
        .iter()
        .map(|key| json_string_value(key))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"{{"source":"{}","usedFilterIndex":{},"candidateSpanKeys":{},"scannedSegments":{},"matchedSpans":{},"fallbackReason":{},"unsupportedAttrKeys":[{}],"traceFetchSource":{},"traceFetchSpanCount":{},"traceFetchFallbackReason":{}}}"#,
        plan.source.as_deref().unwrap_or(if plan.used_filter_index {
            "filter_index"
        } else {
            "scan"
        }),
        plan.used_filter_index,
        plan.candidate_span_keys
            .map_or("null".to_string(), |value| value.to_string()),
        plan.scanned_segments,
        plan.matched_spans,
        json_opt_str(plan.fallback_reason.as_deref()),
        unsupported,
        json_opt_str(plan.trace_fetch_source.as_deref()),
        plan.trace_fetch_span_count
            .map_or("null".to_string(), |value| value.to_string()),
        json_opt_str(plan.trace_fetch_fallback_reason.as_deref()),
    )
}

fn json_string_value(s: &str) -> String {
    format!("\"{}\"", json_escape(s))
}
