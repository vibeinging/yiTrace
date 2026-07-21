impl EngineJsonApi {
    /// GET /v1/sessions?cursor=&limit=：会话列表，offset 游标分页。
    /// `console_sessions` 走增量边车索引（摄入时 O(1) 维护），分页不全扫（见引擎实现）。
    fn sessions_page_json(&self, query: &str, tenant: Option<u64>) -> String {
        let (mut offset, mut limit, mut filter) = (0usize, 50usize, String::new());
        let mut attr_filter = std::collections::BTreeMap::new();
        for kv in query.split('&') {
            if let Some((k, v)) = kv.split_once('=') {
                match k {
                    "cursor" => offset = v.parse().unwrap_or(0),
                    "limit" => limit = v.parse().unwrap_or(50).clamp(1, 500),
                    "filter" => filter = url_decode(v),
                    "attrs" => collect_attr_query_json(&url_decode(v), &mut attr_filter),
                    "project_id" | "skill" | "mode" | "call_site" => {
                        attr_filter.insert(k.to_string(), json_string_value(&url_decode(v)));
                    }
                    "projectId" => {
                        attr_filter
                            .insert("project_id".to_string(), json_string_value(&url_decode(v)));
                    }
                    "callSite" => {
                        attr_filter
                            .insert("call_site".to_string(), json_string_value(&url_decode(v)));
                    }
                    _ => {}
                }
            }
        }
        let snap = self.coord.pin_snapshot();
        let mut all = if attr_filter.is_empty() {
            self.coord.console_sessions_for_tenant(&snap, tenant)
        } else {
            self.coord
                .console_sessions_for_tenant_and_attrs(&snap, tenant, &attr_filter)
        };
        if !filter.is_empty() {
            all.retain(|s| s.title.contains(&filter) || s.session_id.to_string().contains(&filter));
        }
        let total = all.len();
        let end = (offset + limit).min(total);
        let page = if offset < total {
            &all[offset..end]
        } else {
            &[][..]
        };
        let items: Vec<String> = page
            .iter()
            .map(|s| {
                format!(
                    r#"{{"sessionId":"{}","externalSessionId":{},"title":"{}","turnCount":{},"totalCost":{},"inputTokens":{},"outputTokens":{},"totalCacheReadTokens":{},"totalCacheWriteTokens":{},"cacheReadReportedSpans":{},"cacheWriteReportedSpans":{},"totalLlmSpans":{},"status":"{}","startedAt":{},"firstTraceId":"{}"}}"#,
                    s.session_id,
                    json_opt_str(s.external_session_id.as_deref()),
                    json_escape(&s.title),
                    s.turn_count,
                    cost_num(s.input_tokens, s.output_tokens),
                    s.input_tokens,
                    s.output_tokens,
                    s.cache_read_tokens.map_or("null".to_string(), |v| v.to_string()),
                    s.cache_write_tokens.map_or("null".to_string(), |v| v.to_string()),
                    s.cache_read_reported_spans,
                    s.cache_write_reported_spans,
                    s.total_llm_spans,
                    if s.has_error { "error" } else if s.has_running { "run" } else { "ok" },
                    s.session_id,
                    s.first_trace_id,
                )
            })
            .collect();
        let next = if end < total {
            end.to_string()
        } else {
            "null".to_string()
        };
        format!(
            r#"{{"items":[{}],"nextCursor":{},"total":{}}}"#,
            items.join(","),
            next,
            total
        )
    }

    /// GET /v1/sessions/:id/turns：一个会话的轮次（按时序）。
    fn turns_json(&self, id: &str, tenant: Option<u64>) -> (u16, String) {
        let Some(sid) = parse_id_or_hash(id) else {
            return (400, r#"{"error":"bad session id"}"#.to_string());
        };
        let snap = self.coord.pin_snapshot();
        let mut q = TraceQuery::all();
        q.tenant_id = tenant;
        let tl = self.coord.load_session_timeline_query(&snap, sid, &q);
        let items: Vec<String> = tl
            .turns
            .iter()
            .map(|t| {
                // 真实耗时：对该轮 trace 求 span 时长之和（毫秒）。
                let spans = self.coord.console_trace_spans_for_tenant(&snap, t.trace_id, tenant);
                let dur_ms = spans.iter().map(|s| s.duration_ns).sum::<u64>() / 1_000_000;
                let lifecycle_status = aggregate_span_status(&spans);
                let name = t.user_input.as_deref().map(trunc).unwrap_or_else(|| format!("第{}轮", t.turn_index + 1));
                format!(
                    r#"{{"traceId":"{}","sessionId":"{}","turnIndex":{},"name":"{}","durMs":{},"cost":{},"inTok":{},"outTok":{},"cacheReadTokens":{},"cacheWriteTokens":{},"cacheReadReportedSpans":{},"cacheWriteReportedSpans":{},"totalLlmSpans":{},"spanCount":{},"status":"{}"}}"#,
                    t.trace_id,
                    sid,
                    t.turn_index,
                    json_escape(&name),
                    dur_ms,
                    cost_num(t.input_tokens, t.output_tokens),
                    t.input_tokens,
                    t.output_tokens,
                    t.cache_read_tokens.map_or("null".to_string(), |v| v.to_string()),
                    t.cache_write_tokens.map_or("null".to_string(), |v| v.to_string()),
                    t.cache_read_reported_spans,
                    t.cache_write_reported_spans,
                    t.total_llm_spans,
                    t.span_count,
                    if t.error_count > 0 { "error" } else { lifecycle_status },
                )
            })
            .collect();
        (200, format!("[{}]", items.join(",")))
    }

    /// GET /v1/traces/:id：一条 trace 的折叠 span（瀑布）+ 摘要。
    fn trace_json(&self, id: &str, tenant: Option<u64>) -> (u16, String) {
        let Some(tid) = parse_id_or_hash(id) else {
            return (400, r#"{"error":"bad trace id"}"#.to_string());
        };
        let snap = self.coord.pin_snapshot();
        let spans = self
            .coord
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
        let any_err = spans.iter().any(|s| s.has_error);
        let mut cache = crate::CacheTokenCoverage::default();
        for span in &spans {
            cache.observe_values(
                span.model.is_some(),
                (span.input_tokens > 0).then_some(span.input_tokens),
                (span.output_tokens > 0).then_some(span.output_tokens),
                span.cache_read_tokens,
                span.cache_write_tokens,
            );
        }
        let lifecycle_status = aggregate_span_status(&spans);
        let name = spans
            .first()
            .map(|s| {
                s.display_name
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
                    .or_else(|| {
                        s.span_name
                            .as_deref()
                            .filter(|value| !value.trim().is_empty())
                    })
                    .unwrap_or(&s.name)
                    .to_string()
            })
            .unwrap_or_default();
        let visible_keys: std::collections::HashSet<(u64, u64)> =
            spans.iter().map(|s| (tid, s.span_id)).collect();
        let log_events_by_span = self
            .coord
            .log_events_for_trace_keys(&snap, tid, &visible_keys);
        let span_items: Vec<String> = spans
            .iter()
            .map(|s| {
                let log_events = log_events_by_span
                    .get(&s.span_id)
                    .map(|events| json_log_events(events))
                    .unwrap_or_else(|| "[]".to_string());
                format!(
                    r#"{{"id":"{}","parentId":{},"externalTraceId":{},"externalSpanId":{},"externalParentSpanId":{},"externalSessionId":{},"kind":"{}","name":"{}","spanName":{},"displayName":{},"actorId":"{}","agentName":{},"toolName":{},"startMs":{},"durMs":{},"status":"{}","lifecycle":"{}","cost":{},"inTok":{},"outTok":{},"cacheReadTokens":{},"cacheWriteTokens":{},"model":{},"depth":{},"attrs":{},"logEvents":{}}}"#,
                    s.span_id,
                    s.parent_span_id.map_or("null".to_string(), |p| format!("\"{p}\"")),
                    json_opt_str(s.external_trace_id.as_deref()),
                    json_opt_str(s.external_span_id.as_deref()),
                    json_opt_str(s.external_parent_span_id.as_deref()),
                    json_opt_str(s.external_session_id.as_deref()),
                    s.kind,
                    json_escape(&s.name),
                    json_opt_str(s.span_name.as_deref()),
                    json_opt_str(s.display_name.as_deref()),
                    json_escape(&s.actor_id),
                    json_opt_str(s.agent_name.as_deref()),
                    json_opt_str(s.tool_name.as_deref()),
                    s.start_ns / 1_000_000,
                    s.duration_ns / 1_000_000,
                    s.status_label(),
                    s.lifecycle(),
                    cost_num(s.input_tokens, s.output_tokens),
                    s.input_tokens,
                    s.output_tokens,
                    s.cache_read_tokens.map_or("null".to_string(), |v| v.to_string()),
                    s.cache_write_tokens.map_or("null".to_string(), |v| v.to_string()),
                    s.model.as_ref().map_or("null".to_string(), |m| format!("\"{}\"", json_escape(m))),
                    depth_of(s.span_id),
                    json_attrs(&s.attrs),
                    log_events,
                )
            })
            .collect();
        let summary = format!(
            r#"{{"traceId":"{}","externalTraceId":{},"name":"{}","durMs":{},"cost":{},"totalCacheReadTokens":{},"totalCacheWriteTokens":{},"cacheReadReportedSpans":{},"cacheWriteReportedSpans":{},"totalLlmSpans":{},"spanCount":{},"status":"{}"}}"#,
            tid,
            json_opt_str(spans.iter().find_map(|s| s.external_trace_id.as_deref())),
            json_escape(&name),
            total_dur_ms,
            cost_num(in_tok, out_tok),
            cache.read_total().map_or("null".to_string(), |v| v.to_string()),
            cache.write_total().map_or("null".to_string(), |v| v.to_string()),
            cache.read_reported_spans,
            cache.write_reported_spans,
            cache.total_llm_spans,
            spans.len(),
            if any_err { "error" } else { lifecycle_status },
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
    fn steps_json(&self, id: &str, tenant: Option<u64>) -> (u16, String) {
        let Some(tid) = parse_id_or_hash(id) else {
            return (400, r#"{"error":"bad trace id"}"#.to_string());
        };
        let snap = self.coord.pin_snapshot();
        let spans = self
            .coord
            .console_trace_spans_for_tenant(&snap, tid, tenant);
        if spans.is_empty() {
            return (404, r#"{"error":"trace not found"}"#.to_string());
        }
        let items: Vec<String> = spans
            .iter()
            .map(|s| {
                format!(
                    r#"{{"id":"{}","externalTraceId":{},"externalSpanId":{},"kind":"{}","name":"{}","spanName":{},"displayName":{},"actorId":"{}","agentName":{},"toolName":{},"status":"{}","lifecycle":"{}","durMs":{},"inTok":{},"outTok":{},"cacheReadTokens":{},"cacheWriteTokens":{},"model":{},"input":{},"output":{},"attrs":{}}}"#,
                    s.span_id,
                    json_opt_str(s.external_trace_id.as_deref()),
                    json_opt_str(s.external_span_id.as_deref()),
                    s.kind,
                    json_escape(&s.name),
                    json_opt_str(s.span_name.as_deref()),
                    json_opt_str(s.display_name.as_deref()),
                    json_escape(&s.actor_id),
                    json_opt_str(s.agent_name.as_deref()),
                    json_opt_str(s.tool_name.as_deref()),
                    s.status_label(),
                    s.lifecycle(),
                    s.duration_ns / 1_000_000,
                    s.input_tokens,
                    s.output_tokens,
                    s.cache_read_tokens.map_or("null".to_string(), |v| v.to_string()),
                    s.cache_write_tokens.map_or("null".to_string(), |v| v.to_string()),
                    s.model.as_ref().map_or("null".to_string(), |m| format!("\"{}\"", json_escape(m))),
                    s.input_text.as_ref().map_or("null".to_string(), |t| format!("\"{}\"", json_escape(t))),
                    s.output_text.as_ref().map_or("null".to_string(), |t| format!("\"{}\"", json_escape(t))),
                    json_attrs(&s.attrs),
                )
            })
            .collect();
        (200, format!("[{}]", items.join(",")))
    }

    /// GET /v1/traces/:id/spans/:spanId：单个 span 的大字段（晚物化）。
    fn span_detail_json(&self, id: &str, span_id: &str, tenant: Option<u64>) -> (u16, String) {
        let (Some(tid), Some(sid)) = (parse_id_or_hash(id), parse_id_or_hash(span_id)) else {
            return (400, r#"{"error":"bad id"}"#.to_string());
        };
        let snap = self.coord.pin_snapshot();
        let (span, _) = self
            .coord
            .console_span_for_tenant(&snap, tid, sid, tenant);
        match span {
            Some(s) => {
                let mut keys = std::collections::HashSet::new();
                keys.insert((tid, sid));
                let log_events_by_span = self.coord.log_events_for_trace_keys(&snap, tid, &keys);
                let log_events = log_events_by_span
                    .get(&sid)
                    .map(|events| json_log_events(events))
                    .unwrap_or_else(|| "[]".to_string());
                (
                    200,
                    format!(
                        r#"{{"id":"{}","externalTraceId":{},"externalSpanId":{},"externalParentSpanId":{},"externalSessionId":{},"name":"{}","spanName":{},"displayName":{},"actorId":"{}","agentName":{},"toolName":{},"inputTokens":{},"outputTokens":{},"cacheReadTokens":{},"cacheWriteTokens":{},"model":{},"input":{},"output":{},"attrs":{},"logEvents":{}}}"#,
                        sid,
                        json_opt_str(s.external_trace_id.as_deref()),
                        json_opt_str(s.external_span_id.as_deref()),
                        json_opt_str(s.external_parent_span_id.as_deref()),
                        json_opt_str(s.external_session_id.as_deref()),
                        json_escape(&s.name),
                        json_opt_str(s.span_name.as_deref()),
                        json_opt_str(s.display_name.as_deref()),
                        json_escape(&s.actor_id),
                        json_opt_str(s.agent_name.as_deref()),
                        json_opt_str(s.tool_name.as_deref()),
                        s.input_tokens,
                        s.output_tokens,
                        s.cache_read_tokens.map_or("null".to_string(), |v| v.to_string()),
                        s.cache_write_tokens.map_or("null".to_string(), |v| v.to_string()),
                        json_opt_str(s.model.as_deref()),
                        s.input_text
                            .as_ref()
                            .map_or("null".to_string(), |t| format!("\"{}\"", json_escape(t))),
                        s.output_text
                            .as_ref()
                            .map_or("null".to_string(), |t| format!("\"{}\"", json_escape(t))),
                        json_attrs(&s.attrs),
                        log_events,
                    ),
                )
            }
            None => (404, r#"{"error":"span not found"}"#.to_string()),
        }
    }
}

/// trace/轮次摘要的状态优先级：错误 > 运行中 > 数据不完整 > 完成。
fn aggregate_span_status(spans: &[crate::ConsoleSpan]) -> &'static str {
    if spans.iter().any(|span| span.has_error) {
        "error"
    } else if spans.iter().any(|span| span.lifecycle() == "running") {
        "run"
    } else if spans.iter().any(|span| span.lifecycle() != "completed") {
        "incomplete"
    } else {
        "ok"
    }
}
