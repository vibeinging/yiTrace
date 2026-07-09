impl EngineJsonApi {
    fn loops_page_json(&self, query: &str, tenant: Option<u64>) -> String {
        let mut attrs = attrs_from_query(query);
        let (cursor, limit) = cursor_limit_from_query(query);
        let (spans, read_plan) = self.indexed_spans_for_attrs(&attrs, tenant);
        let mut buckets: std::collections::BTreeMap<String, LoopBucket> =
            std::collections::BTreeMap::new();
        for span in spans {
            if !span_attrs_match(&span, &attrs) {
                continue;
            }
            let Some(loop_id) = attr_label(&span, "loop_id") else {
                continue;
            };
            buckets
                .entry(loop_id.clone())
                .or_insert_with(|| LoopBucket::new(loop_id))
                .add(&span);
        }
        attrs.remove("loop_id");
        let mut items: Vec<LoopBucket> = buckets.into_values().collect();
        items.sort_by(|a, b| {
            b.trace_ids
                .len()
                .cmp(&a.trace_ids.len())
                .then_with(|| a.loop_id.cmp(&b.loop_id))
        });
        let total = items.len();
        let item_json: Vec<String> = items
            .into_iter()
            .skip(cursor)
            .take(limit)
            .map(LoopBucket::to_json)
            .collect();
        format!(
            r#"{{"items":[{}],"total":{},"cursor":{},"limit":{},"scannedSpans":{},"readPlan":{}}}"#,
            item_json.join(","),
            total,
            cursor,
            limit,
            read_plan.scanned_segments,
            json_read_plan(&read_plan),
        )
    }

    /// GET /v1/loops/:loopId：单个 loop 的 trace 和 span 明细。
    fn loop_detail_json(&self, loop_id: &str, query: &str, tenant: Option<u64>) -> (u16, String) {
        let decoded = url_decode(loop_id);
        let mut attrs = attrs_from_query(query);
        attrs.insert("loop_id".to_string(), json_string_value(&decoded));
        let (spans, mut read_plan) = self.indexed_spans_for_attrs(&attrs, tenant);
        if spans.is_empty() {
            return (404, r#"{"error":"loop not found"}"#.to_string());
        }
        let mut bucket = LoopBucket::new(decoded);
        for span in &spans {
            bucket.add(span);
        }
        let trace_ids = unique_trace_ids(&spans);
        let (mut by_trace, fetch_plan) = self.read_model_spans_by_trace_ids_with_plan(&trace_ids, tenant);
        merge_trace_fetch_plan(&mut read_plan, &fetch_plan);
        let traces: Vec<String> = trace_ids
            .iter()
            .filter_map(|trace_id| {
                by_trace.remove(trace_id).map(|mut full| {
                    sort_spans_for_trajectory(&mut full);
                    trace_trajectory_json(&full)
                })
            })
            .collect();
        let span_items: Vec<String> = spans.iter().map(trace_search_item_json).collect();
        (
            200,
            format!(
                r#"{{"summary":{},"traces":[{}],"spans":[{}],"scannedSpans":{},"readPlan":{}}}"#,
                bucket.to_json(),
                traces.join(","),
                span_items.join(","),
                read_plan.scanned_segments,
                json_read_plan(&read_plan),
            ),
        )
    }

    /// GET /v1/tasks/:fingerprint/traces：同类 task 的 trace 列表。
    fn task_traces_json(&self, fingerprint: &str, query: &str, tenant: Option<u64>) -> String {
        let decoded = url_decode(fingerprint);
        let mut attrs = attrs_from_query(query);
        attrs.insert("task_fingerprint".to_string(), json_string_value(&decoded));
        let (cursor, limit) = cursor_limit_from_query(query);
        let mut index_attrs = std::collections::BTreeMap::new();
        index_attrs.insert("task_fingerprint".to_string(), json_string_value(&decoded));
        let (candidate_spans, mut read_plan) = self.indexed_spans_for_attrs(&index_attrs, tenant);
        let candidate_trace_ids = unique_trace_ids(&candidate_spans);
        read_plan.matched_spans = candidate_spans.len();
        let (mut by_trace, fetch_plan) =
            self.read_model_spans_by_trace_ids_with_plan(&candidate_trace_ids, tenant);
        merge_trace_fetch_plan(&mut read_plan, &fetch_plan);
        let trace_ids: Vec<u64> = candidate_trace_ids
            .into_iter()
            .filter(|trace_id| {
                by_trace
                    .get(trace_id)
                    .map(|spans| trace_attrs_match(spans, &attrs))
                    .unwrap_or(false)
            })
            .collect();
        let total = trace_ids.len();
        let items: Vec<String> = trace_ids
            .into_iter()
            .skip(cursor)
            .take(limit)
            .filter_map(|trace_id| by_trace.remove(&trace_id))
            .map(|mut spans| {
                sort_spans_for_trajectory(&mut spans);
                trace_trajectory_json(&spans)
            })
            .collect();
        format!(
            r#"{{"items":[{}],"total":{},"cursor":{},"limit":{},"scannedSpans":{},"readPlan":{}}}"#,
            items.join(","),
            total,
            cursor,
            limit,
            read_plan.scanned_segments,
            json_read_plan(&read_plan),
        )
    }

    fn indexed_spans_for_attrs(
        &self,
        attrs: &std::collections::BTreeMap<String, String>,
        tenant: Option<u64>,
    ) -> (Vec<FoldedSpan>, ReadPlanStats) {
        let mut query = TraceQuery::all();
        query.tenant_id = tenant;
        let filter = SearchFilter {
            attrs: attrs.clone(),
            tenant_id: tenant,
            ..Default::default()
        };
        if let Some((spans, mut read_plan)) =
            self.coord.trace_aggregate_rollup_spans(&query, &filter)
        {
            let spans = spans
                .into_iter()
                .filter(|span| span_attrs_match(span, attrs))
                .collect::<Vec<_>>();
            read_plan.source = Some("trajectory_rollup".to_string());
            read_plan.matched_spans = spans.len();
            return (spans, read_plan);
        }

        let snap = self.coord.pin_snapshot();
        let (spans, mut read_plan) =
            self.coord
                .read_spans_query_indexed(&snap, &query, &filter, Projection::ALL);
        let spans = spans
            .into_iter()
            .filter(|span| span_attrs_match(span, attrs))
            .collect::<Vec<_>>();
        read_plan.matched_spans = spans.len();
        (spans, read_plan)
    }

    fn read_model_spans_by_trace(
        &self,
        tenant: Option<u64>,
    ) -> std::collections::BTreeMap<u64, Vec<FoldedSpan>> {
        let mut query = TraceQuery::all();
        query.tenant_id = tenant;
        let filter = SearchFilter {
            tenant_id: tenant,
            ..Default::default()
        };
        if let Some((spans, _)) = self.coord.trace_aggregate_rollup_spans(&query, &filter) {
            return group_spans_by_trace(spans);
        }
        self.all_spans_by_trace(tenant)
    }

    fn read_model_spans_by_trace_ids(
        &self,
        trace_ids: &[u64],
        tenant: Option<u64>,
    ) -> std::collections::BTreeMap<u64, Vec<FoldedSpan>> {
        self.read_model_spans_by_trace_ids_with_plan(trace_ids, tenant)
            .0
    }

    fn read_model_spans_by_trace_ids_with_plan(
        &self,
        trace_ids: &[u64],
        tenant: Option<u64>,
    ) -> (
        std::collections::BTreeMap<u64, Vec<FoldedSpan>>,
        ReadPlanStats,
    ) {
        if let Some((by_trace, _)) = self.coord.trace_rollup_spans_for_trace_ids(trace_ids, tenant)
        {
            let mut plan = ReadPlanStats {
                source: Some("trajectory_rollup".to_string()),
                matched_spans: by_trace.values().map(Vec::len).sum(),
                candidate_span_keys: Some(by_trace.values().map(Vec::len).sum()),
                ..Default::default()
            };
            plan.trace_fetch_source = Some("trajectory_rollup".to_string());
            plan.trace_fetch_span_count = Some(plan.matched_spans);
            return (by_trace, plan);
        }
        let wanted = trace_ids
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        let by_trace = self
            .all_spans_by_trace(tenant)
            .into_iter()
            .filter(|(trace_id, _)| wanted.contains(trace_id))
            .collect::<std::collections::BTreeMap<_, _>>();
        let span_count = by_trace.values().map(Vec::len).sum();
        let plan = ReadPlanStats {
            source: Some("scan".to_string()),
            matched_spans: span_count,
            candidate_span_keys: Some(span_count),
            trace_fetch_source: Some("scan".to_string()),
            trace_fetch_span_count: Some(span_count),
            trace_fetch_fallback_reason: Some("trace_rollup_unavailable".to_string()),
            ..Default::default()
        };
        (by_trace, plan)
    }

    fn full_trace_spans(&self, trace_id: u64, tenant: Option<u64>) -> Vec<FoldedSpan> {
        if let Some(mut spans) = self
            .read_model_spans_by_trace_ids(&[trace_id], tenant)
            .remove(&trace_id)
        {
            sort_spans_for_trajectory(&mut spans);
            return spans;
        }
        let snap = self.coord.pin_snapshot();
        let mut q = TraceQuery::all();
        q.trace_id = Some(trace_id);
        q.tenant_id = tenant;
        let (mut spans, _) = self.coord.read_spans_query(&snap, &q);
        sort_spans_for_trajectory(&mut spans);
        spans
    }

    fn all_spans_by_trace(
        &self,
        tenant: Option<u64>,
    ) -> std::collections::BTreeMap<u64, Vec<FoldedSpan>> {
        let snap = self.coord.pin_snapshot();
        let mut q = TraceQuery::all();
        q.tenant_id = tenant;
        let (spans, _) = self.coord.read_spans_query(&snap, &q);
        group_spans_by_trace(spans)
    }

    fn parse_trace_search_value(
        &self,
        v: &crate::wire::Json,
        tenant: Option<u64>,
    ) -> TraceSearchParsed {
        use crate::wire::{field, Json};
        let f = field(v, "filter").unwrap_or(v);
        let mut q = TraceQuery::all();
        q.tenant_id = tenant;
        let trace_id_value = json_field_alias(f, &["trace_id", "traceId"]);
        if let Some(from) =
            json_field_alias(f, &["time_from", "timeFrom", "createdFrom"]).and_then(Json::as_i64)
        {
            q.time_from = from;
        }
        if let Some(to) =
            json_field_alias(f, &["time_to", "timeTo", "createdTo"]).and_then(Json::as_i64)
        {
            q.time_to = to;
        }
        let spec = TraceSearchSpec {
            session_id: json_field_alias(f, &["session_id", "sessionId"]).and_then(json_id_or_hash),
            span_id: json_field_alias(f, &["span_id", "spanId"]).and_then(json_id_or_hash),
            external_trace_id: json_field_alias(f, &["external_trace_id", "externalTraceId"])
                .and_then(Json::as_str)
                .map(str::to_string)
                .or_else(|| trace_id_value.and_then(json_id_text)),
            external_span_id: json_field_alias(f, &["external_span_id", "externalSpanId"])
                .and_then(Json::as_str)
                .map(str::to_string),
            external_session_id: json_field_alias(f, &["external_session_id", "externalSessionId"])
                .and_then(Json::as_str)
                .map(str::to_string),
            status: field(f, "status").and_then(Json::as_u64).map(|x| x as u8),
            agent_name: json_field_alias(f, &["agent_name", "agentName"])
                .and_then(Json::as_str)
                .map(str::to_string),
            tool_name: json_field_alias(f, &["tool_name", "toolName"])
                .and_then(Json::as_str)
                .map(str::to_string),
            model: field(f, "model").and_then(Json::as_str).map(str::to_string),
            text: json_field_alias(v, &["text", "q"])
                .or_else(|| json_field_alias(f, &["text", "q"]))
                .and_then(Json::as_str)
                .map(str::to_string),
            attrs: collect_trace_search_attrs(f),
        };
        let cursor = json_field_alias(v, &["cursor", "offset"])
            .and_then(Json::as_u64)
            .unwrap_or(0) as usize;
        let limit = json_field_alias(v, &["limit"])
            .and_then(Json::as_u64)
            .unwrap_or(50)
            .clamp(1, 500) as usize;
        let sort_by = json_field_alias(v, &["sort_by", "sortBy", "sort"])
            .and_then(Json::as_str)
            .unwrap_or("trace")
            .to_string();
        let index_filter = SearchFilter {
            trace_id: q.trace_id,
            external_trace_id: spec.external_trace_id.clone(),
            agent_name: spec.agent_name.clone(),
            tool_name: spec.tool_name.clone(),
            model: spec.model.clone(),
            status: spec.status,
            time_from: if q.time_from == i64::MIN {
                None
            } else {
                Some(q.time_from)
            },
            time_to: if q.time_to == i64::MAX {
                None
            } else {
                Some(q.time_to)
            },
            attrs: spec.attrs.clone(),
            tenant_id: tenant,
        };
        TraceSearchParsed {
            query: q,
            spec,
            index_filter,
            cursor,
            limit,
            sort_by,
        }
    }

    fn trace_search_spans(
        &self,
        body: &str,
        tenant: Option<u64>,
    ) -> Result<TraceSearchRead, String> {
        let v = crate::wire::parse(body)?;
        let parsed = self.parse_trace_search_value(&v, tenant);
        if parsed.spec.text.is_none() {
            if let Some((spans, mut read_plan)) = self
                .coord
                .trace_aggregate_rollup_spans(&parsed.query, &parsed.index_filter)
            {
                let spans = spans
                    .into_iter()
                    .filter(|span| trace_search_matches(span, &parsed.spec))
                    .collect::<Vec<_>>();
                read_plan.source = Some("trajectory_rollup".to_string());
                read_plan.matched_spans = spans.len();
                return Ok(TraceSearchRead {
                    spans,
                    read_plan,
                    cursor: parsed.cursor,
                    limit: parsed.limit,
                    sort_by: parsed.sort_by,
                });
            }
        }
        let snap = self.coord.pin_snapshot();
        let (spans, mut read_plan) = self.coord.read_spans_query_indexed(
            &snap,
            &parsed.query,
            &parsed.index_filter,
            Projection::ALL,
        );
        let spans = spans
            .into_iter()
            .filter(|span| trace_search_matches(span, &parsed.spec))
            .collect::<Vec<_>>();
        read_plan.matched_spans = spans.len();
        Ok(TraceSearchRead {
            spans,
            read_plan,
            cursor: parsed.cursor,
            limit: parsed.limit,
            sort_by: parsed.sort_by,
        })
    }

    fn trace_read_model_spans(
        &self,
        body: &str,
        tenant: Option<u64>,
    ) -> Result<TraceSearchRead, String> {
        let v = crate::wire::parse(body)?;
        let parsed = self.parse_trace_search_value(&v, tenant);
        if parsed.spec.text.is_none() {
            if let Some((spans, mut read_plan)) = self
                .coord
                .trace_aggregate_rollup_spans(&parsed.query, &parsed.index_filter)
            {
                let spans = spans
                    .into_iter()
                    .filter(|span| trace_search_matches(span, &parsed.spec))
                    .collect::<Vec<_>>();
                read_plan.source = Some("trajectory_rollup".to_string());
                read_plan.matched_spans = spans.len();
                return Ok(TraceSearchRead {
                    spans,
                    read_plan,
                    cursor: parsed.cursor,
                    limit: parsed.limit,
                    sort_by: parsed.sort_by,
                });
            }
        }
        self.trace_search_spans(body, tenant)
    }

}

fn merge_trace_fetch_plan(read_plan: &mut ReadPlanStats, fetch_plan: &ReadPlanStats) {
    read_plan.trace_fetch_source = fetch_plan.source.clone();
    read_plan.trace_fetch_span_count = Some(fetch_plan.matched_spans);
    read_plan.trace_fetch_fallback_reason = fetch_plan.trace_fetch_fallback_reason.clone();
}
