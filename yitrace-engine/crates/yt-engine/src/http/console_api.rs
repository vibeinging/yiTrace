use super::*;

impl EngineJsonApi {
    pub(super) fn turns_json(&self, id: &str, tenant: Option<u64>) -> (u16, String) {
        let Some(sid) = parse_id_or_hash(id) else {
            return (400, r#"{"error":"bad session id"}"#.to_string());
        };
        let snap = self.coord().pin_snapshot();
        let mut q = TraceQuery::all();
        q.tenant_id = tenant;
        let tl = self.coord().load_session_timeline_query(&snap, sid, &q);
        let items: Vec<String> = tl
            .turns
            .iter()
            .map(|t| {
                // 真实耗时：对该轮 trace 求 span 时长之和（毫秒）。
                let spans = self.coord().console_trace_spans_for_tenant(&snap, t.trace_id, tenant);
                let dur_ms = spans.iter().map(|s| s.duration_ns).sum::<u64>() / 1_000_000;
                let name = t.user_input.as_deref().map(trunc).unwrap_or_else(|| format!("第{}轮", t.turn_index + 1));
                format!(
                    r#"{{"traceId":"{}","sessionId":"{}","turnIndex":{},"name":"{}","durMs":{},"cost":{},"costUsd":{},"costDetail":{},"usage":{},"inTok":{},"outTok":{},"spanCount":{},"status":"{}"}}"#,
                    t.trace_id,
                    sid,
                    t.turn_index,
                    json_escape(&name),
                    dur_ms,
                    cost_num(t.input_tokens, t.output_tokens),
                    cost_usd_num_from_nanos(t.cost_usd_nanos),
                    cost_detail_json(t.cost_usd_nanos, Some("USD"), "mixed"),
                    usage_json(
                        t.input_tokens,
                        t.output_tokens,
                        t.cached_input_tokens,
                        t.reasoning_tokens,
                        t.total_tokens,
                    ),
                    t.input_tokens,
                    t.output_tokens,
                    t.span_count,
                    if t.error_count > 0 { "error" } else { "ok" },
                )
            })
            .collect();
        (200, format!("[{}]", items.join(",")))
    }

    /// GET /v1/traces/:id：一条 trace 的折叠 span（瀑布）+ 摘要。
    pub(super) fn trace_json(&self, id: &str, tenant: Option<u64>) -> (u16, String) {
        let Some(tid) = parse_id_or_hash(id) else {
            return (400, r#"{"error":"bad trace id"}"#.to_string());
        };
        let snap = self.coord().pin_snapshot();
        let spans = self
            .coord()
            .console_trace_spans_for_tenant(&snap, tid, tenant);
        if spans.is_empty() {
            return (404, r#"{"error":"trace not found"}"#.to_string());
        }
        // 深度：顺父指针数（用 span_id→parent 映射 + 记忆化）。
        let parent: std::collections::HashMap<u64, Option<u64>> = spans
            .iter()
            .map(|s| (s.span_id, s.parent_span_id))
            .collect();
        let depth_of = |mut id: u64| -> usize {
            let mut d = 0;
            while let Some(Some(p)) = parent.get(&id) {
                d += 1;
                if d > 64 {
                    break;
                }
                id = *p;
            }
            d
        };
        let total_dur_ms = spans.iter().map(|s| s.duration_ns).sum::<u64>() / 1_000_000;
        let (in_tok, out_tok): (u64, u64) = spans.iter().fold((0, 0), |(i, o), s| {
            (i + s.input_tokens, o + s.output_tokens)
        });
        let cached_tok: u64 = spans.iter().map(|s| s.cached_input_tokens).sum();
        let reasoning_tok: u64 = spans.iter().map(|s| s.reasoning_tokens).sum();
        let total_tokens: u64 = spans.iter().map(|s| s.total_tokens).sum();
        let cost_usd_nanos: u64 = spans.iter().map(|s| s.cost_usd_nanos).sum();
        let any_err = spans.iter().any(|s| s.has_error);
        let name = spans.first().map(|s| s.name.clone()).unwrap_or_default();
        let visible_keys: std::collections::HashSet<(u64, u64)> =
            spans.iter().map(|s| (tid, s.span_id)).collect();
        let log_events_by_span = self
            .coord()
            .log_events_for_trace_keys(&snap, tid, &visible_keys);
        let order = span_order(&spans);
        let span_items: Vec<String> = spans
            .iter()
            .map(|s| {
                let log_events = log_events_by_span
                    .get(&s.span_id)
                    .map(|events| json_log_events(events))
                    .unwrap_or_else(|| "[]".to_string());
                let (span_ordinal, sibling_ordinal) =
                    order.get(&s.span_id).copied().unwrap_or((0, 0));
                format!(
                    r#"{{"id":"{}","parentId":{},"externalTraceId":{},"externalSpanId":{},"externalParentSpanId":{},"externalSessionId":{},"kind":"{}","name":"{}","spanOrdinal":{},"siblingOrdinal":{},"sortKey":"{:020}:{:020}","startMs":{},"durMs":{},"status":"{}","cost":{},"costUsd":{},"costDetail":{},"usage":{},"inTok":{},"outTok":{},"model":{},"provider":{},"depth":{},"fields":{},"attrs":{},"logEvents":{}}}"#,
                    s.span_id,
                    s.parent_span_id.map_or("null".to_string(), |p| format!("\"{p}\"")),
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
                    s.start_ns / 1_000_000,
                    s.duration_ns / 1_000_000,
                    if s.has_error { "error" } else { "ok" },
                    cost_num(s.input_tokens, s.output_tokens),
                    cost_usd_num_from_nanos(s.cost_usd_nanos),
                    cost_detail_json(
                        s.cost_usd_nanos,
                        s.cost_currency.as_deref(),
                        "mixed"
                    ),
                    console_usage_json(s),
                    s.input_tokens,
                    s.output_tokens,
                    s.model.as_ref().map_or("null".to_string(), |m| format!("\"{}\"", json_escape(m))),
                    json_opt_str(s.provider.as_deref()),
                    depth_of(s.span_id),
                    json_console_agent_fields(s),
                    json_attrs(&s.attrs),
                    log_events,
                )
            })
            .collect();
        let summary = format!(
            r#"{{"traceId":"{}","externalTraceId":{},"name":"{}","durMs":{},"cost":{},"costUsd":{},"costDetail":{},"usage":{},"spanCount":{},"status":"{}"}}"#,
            tid,
            json_opt_str(spans.iter().find_map(|s| s.external_trace_id.as_deref())),
            json_escape(&name),
            total_dur_ms,
            cost_num(in_tok, out_tok),
            cost_usd_num_from_nanos(cost_usd_nanos),
            cost_detail_json(cost_usd_nanos, Some("USD"), "mixed"),
            usage_json(in_tok, out_tok, cached_tok, reasoning_tok, total_tokens),
            spans.len(),
            if any_err { "error" } else { "ok" },
        );
        (
            200,
            format!(
                r#"{{"summary":{},"spans":[{}]}}"#,
                summary,
                span_items.join(",")
            ),
        )
    }

    /// GET /v1/traces/:id/steps：步骤流视图 —— 每个 span 连同输入/输出大文本一次给全。
    /// 与瀑布的晚物化相反：步骤流的本意就是看每一步的输入→输出，故在此端点物化。
    pub(super) fn steps_json(&self, id: &str, tenant: Option<u64>) -> (u16, String) {
        let Some(tid) = parse_id_or_hash(id) else {
            return (400, r#"{"error":"bad trace id"}"#.to_string());
        };
        let snap = self.coord().pin_snapshot();
        let spans = self
            .coord()
            .console_trace_spans_for_tenant(&snap, tid, tenant);
        if spans.is_empty() {
            return (404, r#"{"error":"trace not found"}"#.to_string());
        }
        let items: Vec<String> = spans
            .iter()
            .map(|s| {
                format!(
                    r#"{{"id":"{}","externalTraceId":{},"externalSpanId":{},"kind":"{}","name":"{}","status":"{}","durMs":{},"cost":{},"costUsd":{},"costDetail":{},"usage":{},"inTok":{},"outTok":{},"model":{},"provider":{},"input":{},"output":{},"fields":{},"attrs":{}}}"#,
                    s.span_id,
                    json_opt_str(s.external_trace_id.as_deref()),
                    json_opt_str(s.external_span_id.as_deref()),
                    s.kind,
                    json_escape(&s.name),
                    if s.has_error { "error" } else { "ok" },
                    s.duration_ns / 1_000_000,
                    cost_num(s.input_tokens, s.output_tokens),
                    cost_usd_num_from_nanos(s.cost_usd_nanos),
                    cost_detail_json(s.cost_usd_nanos, s.cost_currency.as_deref(), "mixed"),
                    console_usage_json(s),
                    s.input_tokens,
                    s.output_tokens,
                    s.model.as_ref().map_or("null".to_string(), |m| format!("\"{}\"", json_escape(m))),
                    json_opt_str(s.provider.as_deref()),
                    s.input_text.as_ref().map_or("null".to_string(), |t| format!("\"{}\"", json_escape(t))),
                    s.output_text.as_ref().map_or("null".to_string(), |t| format!("\"{}\"", json_escape(t))),
                    json_console_agent_fields(s),
                    json_attrs(&s.attrs),
                )
            })
            .collect();
        (200, format!("[{}]", items.join(",")))
    }

    /// GET /v1/traces/:id/spans/:spanId：单个 span 的大字段（晚物化）。
    pub(super) fn span_detail_json(
        &self,
        id: &str,
        span_id: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let (Some(tid), Some(sid)) = (parse_id_or_hash(id), parse_id_or_hash(span_id)) else {
            return (400, r#"{"error":"bad id"}"#.to_string());
        };
        let snap = self.coord().pin_snapshot();
        let spans = self
            .coord()
            .console_trace_spans_for_tenant(&snap, tid, tenant);
        match spans.into_iter().find(|s| s.span_id == sid) {
            Some(s) => {
                let mut keys = std::collections::HashSet::new();
                keys.insert((tid, sid));
                let log_events_by_span = self.coord().log_events_for_trace_keys(&snap, tid, &keys);
                let log_events = log_events_by_span
                    .get(&sid)
                    .map(|events| json_log_events(events))
                    .unwrap_or_else(|| "[]".to_string());
                (
                    200,
                    format!(
                        r#"{{"id":"{}","externalTraceId":{},"externalSpanId":{},"externalParentSpanId":{},"externalSessionId":{},"input":{},"output":{},"fields":{},"attrs":{},"logEvents":{}}}"#,
                        sid,
                        json_opt_str(s.external_trace_id.as_deref()),
                        json_opt_str(s.external_span_id.as_deref()),
                        json_opt_str(s.external_parent_span_id.as_deref()),
                        json_opt_str(s.external_session_id.as_deref()),
                        s.input_text
                            .as_ref()
                            .map_or("null".to_string(), |t| format!("\"{}\"", json_escape(t))),
                        s.output_text
                            .as_ref()
                            .map_or("null".to_string(), |t| format!("\"{}\"", json_escape(t))),
                        json_console_agent_fields(&s),
                        json_attrs(&s.attrs),
                        log_events,
                    ),
                )
            }
            None => (404, r#"{"error":"span not found"}"#.to_string()),
        }
    }

    /// GET /v1/traces/:id/snapshot：导出一条 trace 的稳定 JSON 快照，供 eval draft / 回归样本使用。
    pub(super) fn trace_snapshot_json(&self, id: &str, tenant: Option<u64>) -> (u16, String) {
        let Some(tid) = parse_id_or_hash(id) else {
            return (400, r#"{"error":"bad trace id"}"#.to_string());
        };
        let snap = self.coord().pin_snapshot();
        let spans = self
            .coord()
            .console_trace_spans_for_tenant(&snap, tid, tenant);
        if spans.is_empty() {
            return (404, r#"{"error":"trace not found"}"#.to_string());
        }
        let visible_keys: std::collections::HashSet<(u64, u64)> =
            spans.iter().map(|s| (tid, s.span_id)).collect();
        let log_events_by_span = self
            .coord()
            .log_events_for_trace_keys(&snap, tid, &visible_keys);
        let order = span_order(&spans);
        let span_items: Vec<String> = spans
            .iter()
            .map(|s| {
                let events = log_events_by_span
                    .get(&s.span_id)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                let (span_ordinal, sibling_ordinal) =
                    order.get(&s.span_id).copied().unwrap_or((0, 0));
                json_console_span_export(tid, s, span_ordinal, sibling_ordinal, events, true)
            })
            .collect();
        let summary = trace_summary_json(tid, &spans);
        let payload = format!(
            r#"{{"summary":{},"spans":[{}]}}"#,
            summary,
            span_items.join(",")
        );
        let hash = yt_core::event::fnv1a64(payload.as_bytes());
        (
            200,
            format!(
                r#"{{"snapshotId":"trace-{}-{:016x}","snapshotHash":"fnv1a64:{:016x}","createdAt":{},"trace":{}}}"#,
                tid,
                hash,
                hash,
                unix_now_ns(),
                payload
            ),
        )
    }

    /// GET /v1/traces/:id/spans?cursor=&limit=&includeFull=：分页批量取 span 详情。
    pub(super) fn spans_page_json(
        &self,
        id: &str,
        query: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let Some(tid) = parse_id_or_hash(id) else {
            return (400, r#"{"error":"bad trace id"}"#.to_string());
        };
        let (cursor, limit, include_full) = span_page_query(query);
        let snap = self.coord().pin_snapshot();
        let spans = self
            .coord()
            .console_trace_spans_for_tenant(&snap, tid, tenant);
        if spans.is_empty() {
            return (404, r#"{"error":"trace not found"}"#.to_string());
        }
        let total = spans.len();
        let end = (cursor + limit).min(total);
        let page = if cursor < total {
            &spans[cursor..end]
        } else {
            &[][..]
        };
        let keys: std::collections::HashSet<(u64, u64)> =
            page.iter().map(|s| (tid, s.span_id)).collect();
        let log_events_by_span = self.coord().log_events_for_trace_keys(&snap, tid, &keys);
        let order = span_order(&spans);
        let items: Vec<String> = page
            .iter()
            .map(|s| {
                let events = log_events_by_span
                    .get(&s.span_id)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                let (span_ordinal, sibling_ordinal) =
                    order.get(&s.span_id).copied().unwrap_or((0, 0));
                json_console_span_export(
                    tid,
                    s,
                    span_ordinal,
                    sibling_ordinal,
                    events,
                    include_full,
                )
            })
            .collect();
        let next = if end < total {
            end.to_string()
        } else {
            "null".to_string()
        };
        (
            200,
            format!(
                r#"{{"items":[{}],"nextCursor":{},"total":{}}}"#,
                items.join(","),
                next,
                total
            ),
        )
    }

    /// POST /v1/traces/:id/spans/batch：按 span id 批量取详情，避免业务侧 N 次晚物化。
    pub(super) fn spans_batch_json(
        &self,
        id: &str,
        body: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        use crate::wire::parse;
        let Some(tid) = parse_id_or_hash(id) else {
            return (400, r#"{"error":"bad trace id"}"#.to_string());
        };
        let v = match parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let include_full = json_field_alias(&v, &["include_full", "includeFull", "full"])
            .map(json_truthy)
            .unwrap_or(false);
        let wanted: std::collections::HashSet<u64> = json_field_alias(&v, &["span_ids", "spanIds"])
            .map(|arr| arr.as_array().iter().filter_map(json_id_or_hash).collect())
            .unwrap_or_default();
        if wanted.is_empty() {
            return (400, r#"{"error":"spanIds required"}"#.to_string());
        }
        let snap = self.coord().pin_snapshot();
        let spans = self
            .coord()
            .console_trace_spans_for_tenant(&snap, tid, tenant);
        if spans.is_empty() {
            return (404, r#"{"error":"trace not found"}"#.to_string());
        }
        let selected: Vec<_> = spans
            .iter()
            .filter(|s| wanted.contains(&s.span_id))
            .collect();
        let keys: std::collections::HashSet<(u64, u64)> =
            selected.iter().map(|s| (tid, s.span_id)).collect();
        let log_events_by_span = self.coord().log_events_for_trace_keys(&snap, tid, &keys);
        let order = span_order(&spans);
        let items: Vec<String> = selected
            .iter()
            .map(|s| {
                let events = log_events_by_span
                    .get(&s.span_id)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                let (span_ordinal, sibling_ordinal) =
                    order.get(&s.span_id).copied().unwrap_or((0, 0));
                json_console_span_export(
                    tid,
                    s,
                    span_ordinal,
                    sibling_ordinal,
                    events,
                    include_full,
                )
            })
            .collect();
        (200, format!(r#"{{"items":[{}]}}"#, items.join(",")))
    }
}
