use super::*;

impl EngineJsonApi {
    pub(super) fn trace_search_metadata_matches(
        &self,
        annotation_spec: &TraceSearchAnnotationSpec,
        dataset_spec: &TraceSearchDatasetSpec,
        tenant: Option<u64>,
    ) -> TraceSearchMetadataMatches {
        self.trace_search_metadata_matches_for_coord(
            self.coord(),
            annotation_spec,
            dataset_spec,
            tenant,
        )
    }

    pub(super) fn trace_search_metadata_matches_for_coord(
        &self,
        coord: &WriteCoordinator,
        annotation_spec: &TraceSearchAnnotationSpec,
        dataset_spec: &TraceSearchDatasetSpec,
        tenant: Option<u64>,
    ) -> TraceSearchMetadataMatches {
        let mut matches = TraceSearchMetadataMatches {
            need_annotation: annotation_spec.active,
            need_dataset: dataset_spec.active,
            ..Default::default()
        };
        if annotation_spec.active {
            let items = coord.annotations(&TraceAnnotationFilter {
                tenant_id: tenant,
                target: annotation_spec.target,
                label: annotation_spec.label.clone(),
                source: annotation_spec.source.clone(),
                status: annotation_spec.status,
                include_deleted: annotation_spec.include_deleted,
                attrs: annotation_spec.attrs.clone(),
                ..Default::default()
            });
            for a in items.into_iter().filter(|a| {
                score_in_range(
                    a.score,
                    annotation_spec.score_min,
                    annotation_spec.score_max,
                )
            }) {
                matches.annotation_candidate_traces.insert(a.trace_id);
                match (a.target, a.span_id) {
                    (AnnotationTarget::Span, Some(span_id)) => {
                        matches.annotation_spans.insert((a.trace_id, span_id));
                    }
                    _ => {
                        matches.annotation_traces.insert(a.trace_id);
                    }
                }
            }
        }
        if dataset_spec.active {
            let items = coord.dataset_associations(&DatasetAssociationFilter {
                tenant_id: tenant,
                dataset_id: dataset_spec.dataset_id.clone(),
                item_id: dataset_spec.item_id.clone(),
                eval_run_id: dataset_spec.eval_run_id.clone(),
                split: dataset_spec.split.clone(),
                label: dataset_spec.label.clone(),
                attrs: dataset_spec.attrs.clone(),
                ..Default::default()
            });
            for d in items
                .into_iter()
                .filter(|d| score_in_range(d.score, dataset_spec.score_min, dataset_spec.score_max))
            {
                matches.dataset_candidate_traces.insert(d.trace_id);
                if let Some(span_id) = d.span_id {
                    matches.dataset_spans.insert((d.trace_id, span_id));
                } else {
                    matches.dataset_traces.insert(d.trace_id);
                }
            }
        }
        matches
    }

    pub(super) fn trace_search_spans_for_coord(
        &self,
        coord: &WriteCoordinator,
        request: &TraceSearchRequest,
        tenant: Option<u64>,
    ) -> Vec<FoldedSpan> {
        let snap = coord.pin_snapshot();
        self.trace_search_spans_for_coord_snapshot(coord, &snap, request, tenant)
    }

    pub(super) fn trace_search_spans_for_coord_snapshot(
        &self,
        coord: &WriteCoordinator,
        snap: &yt_manifest::Snapshot,
        request: &TraceSearchRequest,
        tenant: Option<u64>,
    ) -> Vec<FoldedSpan> {
        self.trace_search_spans_for_coord_snapshot_with_stats(coord, snap, request, tenant)
            .0
    }

    pub(super) fn trace_search_spans_for_coord_snapshot_with_stats(
        &self,
        coord: &WriteCoordinator,
        snap: &yt_manifest::Snapshot,
        request: &TraceSearchRequest,
        tenant: Option<u64>,
    ) -> (Vec<FoldedSpan>, AttrIndexedReadStats) {
        let metadata_matches = self.trace_search_metadata_matches_for_coord(
            coord,
            &request.annotation,
            &request.dataset,
            tenant,
        );
        let (mut spans, stats) = if request.spec.attrs.is_empty() {
            let (spans, scanned) = coord.read_spans_query(snap, &request.query);
            (
                spans,
                AttrIndexedReadStats {
                    scanned_segments: scanned,
                    ..Default::default()
                },
            )
        } else {
            coord.read_spans_query_for_attrs_with_stats(snap, &request.query, &request.spec.attrs)
        };
        spans.retain(|s| trace_search_match(s, &request.spec, &metadata_matches));
        (spans, stats)
    }

    pub(super) fn metadata_matching_session_ids(
        &self,
        snap: &yt_manifest::Snapshot,
        metadata: &TraceSearchMetadataMatches,
        tenant: Option<u64>,
    ) -> std::collections::HashSet<u64> {
        self.metadata_matching_session_ids_for_coord(self.coord(), snap, metadata, tenant)
    }

    pub(super) fn metadata_matching_session_ids_for_coord(
        &self,
        coord: &WriteCoordinator,
        snap: &yt_manifest::Snapshot,
        metadata: &TraceSearchMetadataMatches,
        tenant: Option<u64>,
    ) -> std::collections::HashSet<u64> {
        let mut out = std::collections::HashSet::new();
        for trace_id in metadata_candidate_trace_ids(metadata) {
            let mut q = TraceQuery::trace(trace_id, i64::MIN, i64::MAX);
            q.tenant_id = tenant;
            let (spans, _) = coord.read_spans_query(snap, &q);
            for span in spans {
                if trace_search_metadata_match(&span, metadata) {
                    if let Some(session_id) = span.session_id {
                        out.insert(session_id);
                    }
                }
            }
        }
        out
    }

    pub(super) fn trace_list_attr_filter(
        &self,
        pairs: &[(String, String)],
    ) -> BTreeMap<String, String> {
        let mut attr_filter = BTreeMap::new();
        for (k, v) in pairs {
            match k.as_str() {
                "attrs" => collect_attr_query_json(v, &mut attr_filter),
                _ => {
                    if let Some((_, attr_key)) =
                        attr_aliases().iter().find(|(alias, _)| *alias == k)
                    {
                        attr_filter.insert((*attr_key).to_string(), json_string_value(v));
                    }
                }
            }
        }
        attr_filter
    }

    pub(super) fn trace_list_rows_for_coord(
        &self,
        coord: &WriteCoordinator,
        pairs: &[(String, String)],
        attr_filter: &BTreeMap<String, String>,
        tenant: Option<u64>,
    ) -> Vec<(TraceSummary, BTreeMap<String, String>)> {
        let annotation_spec = trace_search_annotation_spec_from_query(pairs);
        let dataset_spec = trace_search_dataset_spec_from_query(pairs);
        let metadata_matches = self.trace_search_metadata_matches_for_coord(
            coord,
            &annotation_spec,
            &dataset_spec,
            tenant,
        );
        let snap = coord.pin_snapshot();
        let mut traces = if attr_filter.is_empty() {
            let mut q = TraceQuery::all();
            q.tenant_id = tenant;
            coord.list_traces(&snap, &q)
        } else {
            coord.list_traces_for_tenant_and_attrs(&snap, tenant, attr_filter)
        };
        if metadata_matches.need_annotation || metadata_matches.need_dataset {
            traces.retain(|t| trace_id_metadata_match(t.trace_id, &metadata_matches));
        }
        let trace_ids: std::collections::HashSet<u64> = traces.iter().map(|t| t.trace_id).collect();
        let mut fields_by_trace =
            coord.trace_attr_fields_for_tenant_and_traces(&snap, tenant, &trace_ids);
        traces
            .into_iter()
            .map(|t| {
                let fields = fields_by_trace.remove(&t.trace_id).unwrap_or_default();
                (t, fields)
            })
            .collect()
    }

    pub(super) fn merged_trace_list_rows(
        &self,
        rows: Vec<(TraceSummary, BTreeMap<String, String>)>,
    ) -> Vec<(TraceSummary, BTreeMap<String, String>)> {
        let mut merged: BTreeMap<u64, (TraceSummary, BTreeMap<String, String>)> = BTreeMap::new();
        for (trace, fields) in rows {
            match merged.get_mut(&trace.trace_id) {
                Some((existing, existing_fields)) => {
                    if existing.external_trace_id.is_none() {
                        existing.external_trace_id = trace.external_trace_id;
                    }
                    existing.span_count += trace.span_count;
                    existing.total_duration_ns += trace.total_duration_ns;
                    existing.max_duration_ns = existing.max_duration_ns.max(trace.max_duration_ns);
                    existing.error_count += trace.error_count;
                    existing.total_input_tokens += trace.total_input_tokens;
                    existing.total_output_tokens += trace.total_output_tokens;
                    existing.total_cached_input_tokens += trace.total_cached_input_tokens;
                    existing.total_reasoning_tokens += trace.total_reasoning_tokens;
                    existing.total_tokens += trace.total_tokens;
                    existing.total_cost_usd_nanos += trace.total_cost_usd_nanos;
                    for (k, v) in fields {
                        existing_fields.entry(k).or_insert(v);
                    }
                }
                None => {
                    merged.insert(trace.trace_id, (trace, fields));
                }
            }
        }
        merged.into_values().collect()
    }

    pub(super) fn trace_list_json(
        &self,
        rows: &[(TraceSummary, BTreeMap<String, String>)],
    ) -> String {
        let items: Vec<String> = rows
            .iter()
            .map(|(t, fields_map)| {
                let fields = json_attrs(fields_map);
                format!(
                    r#"{{"trace_id":{},"external_trace_id":{},"span_count":{},"total_duration_ns":{},"max_duration_ns":{},"error_count":{},"total_input_tokens":{},"total_output_tokens":{},"total_cached_input_tokens":{},"total_reasoning_tokens":{},"total_tokens":{},"total_cost_usd":{},"total_cost_usd_nanos":{},"usage":{},"costDetail":{},"fields":{}}}"#,
                    t.trace_id,
                    json_opt_str(t.external_trace_id.as_deref()),
                    t.span_count,
                    t.total_duration_ns,
                    t.max_duration_ns,
                    t.error_count,
                    t.total_input_tokens,
                    t.total_output_tokens,
                    t.total_cached_input_tokens,
                    t.total_reasoning_tokens,
                    t.total_tokens,
                    cost_usd_num_from_nanos(t.total_cost_usd_nanos),
                    t.total_cost_usd_nanos,
                    usage_json(
                        t.total_input_tokens,
                        t.total_output_tokens,
                        t.total_cached_input_tokens,
                        t.total_reasoning_tokens,
                        t.total_tokens,
                    ),
                    cost_detail_json(t.total_cost_usd_nanos, Some("USD"), "mixed"),
                    fields
                )
            })
            .collect();
        format!("[{}]", items.join(","))
    }

    pub(super) fn traces_json(&self, query: &str, tenant: Option<u64>) -> String {
        let pairs = query_pairs(query);
        let attr_filter = self.trace_list_attr_filter(&pairs);
        let rows = self.trace_list_rows_for_coord(self.coord(), &pairs, &attr_filter, tenant);
        self.trace_list_json(&rows)
    }

    pub(super) fn cluster_traces_json(&self, query: &str, tenant: Option<u64>) -> String {
        let pairs = query_pairs(query);
        let attr_filter = self.trace_list_attr_filter(&pairs);
        let mut rows = Vec::new();
        for shard in self.shards().iter() {
            rows.extend(self.trace_list_rows_for_coord(&shard.coord, &pairs, &attr_filter, tenant));
        }
        let rows = self.merged_trace_list_rows(rows);
        self.trace_list_json(&rows)
    }

    pub(super) fn session_list_rows_for_coord(
        &self,
        coord: &WriteCoordinator,
        pairs: &[(String, String)],
        attr_filter: &BTreeMap<String, String>,
        filter: &str,
        tenant: Option<u64>,
    ) -> Vec<ConsoleSession> {
        let snap = coord.pin_snapshot();
        self.session_list_rows_for_coord_snapshot(coord, &snap, pairs, attr_filter, filter, tenant)
    }

    pub(super) fn session_list_rows_for_coord_snapshot(
        &self,
        coord: &WriteCoordinator,
        snap: &yt_manifest::Snapshot,
        pairs: &[(String, String)],
        attr_filter: &BTreeMap<String, String>,
        filter: &str,
        tenant: Option<u64>,
    ) -> Vec<ConsoleSession> {
        let annotation_spec = trace_search_annotation_spec_from_query(pairs);
        let dataset_spec = trace_search_dataset_spec_from_query(pairs);
        let metadata_matches = self.trace_search_metadata_matches_for_coord(
            coord,
            &annotation_spec,
            &dataset_spec,
            tenant,
        );
        let mut all = if attr_filter.is_empty() {
            coord.console_sessions_for_tenant(snap, tenant)
        } else {
            coord.console_sessions_for_tenant_and_attrs(snap, tenant, attr_filter)
        };
        if metadata_matches.need_annotation || metadata_matches.need_dataset {
            let session_ids = self.metadata_matching_session_ids_for_coord(
                coord,
                snap,
                &metadata_matches,
                tenant,
            );
            all.retain(|s| session_ids.contains(&s.session_id));
        }
        if !filter.is_empty() {
            all.retain(|s| s.title.contains(filter) || s.session_id.to_string().contains(filter));
        }
        all
    }

    pub(super) fn merged_session_list_rows(
        &self,
        rows: Vec<ConsoleSession>,
    ) -> Vec<ConsoleSession> {
        let mut merged: BTreeMap<u64, ConsoleSession> = BTreeMap::new();
        for row in rows {
            match merged.get_mut(&row.session_id) {
                Some(existing) => {
                    if existing.external_session_id.is_none() {
                        existing.external_session_id = row.external_session_id;
                    }
                    if existing.title.starts_with("会话 ") && !row.title.starts_with("会话 ") {
                        existing.title = row.title;
                    }
                    existing.turn_count += row.turn_count;
                    existing.input_tokens += row.input_tokens;
                    existing.output_tokens += row.output_tokens;
                    existing.cached_input_tokens += row.cached_input_tokens;
                    existing.reasoning_tokens += row.reasoning_tokens;
                    existing.total_tokens += row.total_tokens;
                    existing.cost_usd_nanos += row.cost_usd_nanos;
                    existing.has_error |= row.has_error;
                    existing.first_trace_id = existing.first_trace_id.min(row.first_trace_id);
                }
                None => {
                    merged.insert(row.session_id, row);
                }
            }
        }
        let mut rows: Vec<_> = merged.into_values().collect();
        rows.sort_by(|a, b| b.session_id.cmp(&a.session_id));
        rows
    }

    pub(super) fn session_list_item_json(&self, s: &ConsoleSession) -> String {
        format!(
            r#"{{"sessionId":"{}","externalSessionId":{},"title":"{}","turnCount":{},"totalCost":{},"costUsd":{},"costDetail":{},"usage":{},"status":"{}","startedAt":{},"firstTraceId":"{}"}}"#,
            s.session_id,
            json_opt_str(s.external_session_id.as_deref()),
            json_escape(&s.title),
            s.turn_count,
            cost_num(s.input_tokens, s.output_tokens),
            cost_usd_num_from_nanos(s.cost_usd_nanos),
            cost_detail_json(s.cost_usd_nanos, Some("USD"), "mixed"),
            usage_json(
                s.input_tokens,
                s.output_tokens,
                s.cached_input_tokens,
                s.reasoning_tokens,
                s.total_tokens,
            ),
            if s.has_error { "error" } else { "ok" },
            s.session_id,
            s.first_trace_id,
        )
    }

    pub(super) fn session_page_json(
        &self,
        all: &[ConsoleSession],
        offset: usize,
        limit: usize,
    ) -> String {
        self.session_page_json_with_extra(all, offset, limit, "")
    }

    pub(super) fn session_page_json_with_extra(
        &self,
        all: &[ConsoleSession],
        offset: usize,
        limit: usize,
        extra_fields: &str,
    ) -> String {
        let total = all.len();
        let end = offset.saturating_add(limit).min(total);
        let page = if offset < total {
            &all[offset..end]
        } else {
            &[][..]
        };
        let items: Vec<String> = page
            .iter()
            .map(|s| self.session_list_item_json(s))
            .collect();
        let next = if end < total {
            end.to_string()
        } else {
            "null".to_string()
        };
        format!(
            r#"{{"items":[{}],"nextCursor":{},"total":{}{}}}"#,
            items.join(","),
            next,
            total,
            extra_fields
        )
    }

    // ───────────────────── 控制台数据端点（游标分页 / 轮次 / span / 详情） ─────────────────────

    /// GET /v1/sessions?cursor=&limit=：会话列表，offset 游标分页。
    /// `console_sessions` 走增量边车索引（摄入时 O(1) 维护），分页不全扫（见引擎实现）。
    pub(super) fn sessions_page_json(&self, query: &str, tenant: Option<u64>) -> String {
        let (mut offset, mut limit, mut filter) = (0usize, 50usize, String::new());
        let pairs = query_pairs(query);
        for (k, v) in &pairs {
            match k.as_str() {
                "cursor" => offset = v.parse().unwrap_or(0),
                "limit" => limit = v.parse().unwrap_or(50).clamp(1, 500),
                "filter" => filter = v.clone(),
                _ => {}
            }
        }
        let attr_filter = self.trace_list_attr_filter(&pairs);
        let all =
            self.session_list_rows_for_coord(self.coord(), &pairs, &attr_filter, &filter, tenant);
        self.session_page_json(&all, offset, limit)
    }

    pub(super) fn cluster_sessions_page_json(
        &self,
        query: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let read_set = match self.cluster_snapshot_read_set_from_query(query) {
            Ok(read_set) => read_set,
            Err(resp) => return resp,
        };
        let (mut offset, mut limit, mut filter) = (0usize, 50usize, String::new());
        let pairs = query_pairs(query);
        for (k, v) in &pairs {
            match k.as_str() {
                "cursor" => offset = v.parse().unwrap_or(0),
                "limit" => limit = v.parse().unwrap_or(50).clamp(1, 500),
                "filter" => filter = v.clone(),
                _ => {}
            }
        }
        let attr_filter = self.trace_list_attr_filter(&pairs);
        let mut all = Vec::new();
        for (idx, shard) in self.shards().iter().enumerate() {
            all.extend(self.session_list_rows_for_coord_snapshot(
                &shard.coord,
                read_set.snapshot_at(idx),
                &pairs,
                &attr_filter,
                &filter,
                tenant,
            ));
        }
        let all = self.merged_session_list_rows(all);
        let extra = format!(
            r#","queryMode":"fanout_merge","shardCount":{}{}"#,
            self.shards().len(),
            read_set.snapshot_field()
        );
        (
            200,
            self.session_page_json_with_extra(&all, offset, limit, &extra),
        )
    }
}
