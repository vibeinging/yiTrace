use super::snapshot_helpers::ClusterSnapshotReadSet;
use super::*;

impl EngineJsonApi {
    pub(super) fn trace_diff_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        use crate::wire::parse;
        let v = match parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let Some(left_id) = json_field_alias(
            &v,
            &[
                "left_trace_id",
                "leftTraceId",
                "left",
                "base_trace_id",
                "baseTraceId",
                "a",
            ],
        )
        .and_then(json_id_or_hash) else {
            return (400, r#"{"error":"missing leftTraceId"}"#.to_string());
        };
        let Some(right_id) = json_field_alias(
            &v,
            &[
                "right_trace_id",
                "rightTraceId",
                "right",
                "candidate_trace_id",
                "candidateTraceId",
                "b",
            ],
        )
        .and_then(json_id_or_hash) else {
            return (400, r#"{"error":"missing rightTraceId"}"#.to_string());
        };

        let snap = self.coord().pin_snapshot();
        let left = self.trace_folded_spans(&snap, left_id, tenant);
        if left.is_empty() {
            return (404, r#"{"error":"left trace not found"}"#.to_string());
        }
        let right = self.trace_folded_spans(&snap, right_id, tenant);
        if right.is_empty() {
            return (404, r#"{"error":"right trace not found"}"#.to_string());
        }
        (200, json_trace_diff(left_id, right_id, &left, &right))
    }

    pub(super) fn trace_folded_spans(
        &self,
        snap: &yt_manifest::Snapshot,
        trace_id: u64,
        tenant: Option<u64>,
    ) -> Vec<FoldedSpan> {
        self.trace_folded_spans_for_coord(self.coord(), snap, trace_id, tenant)
    }

    pub(super) fn trace_folded_spans_for_coord(
        &self,
        coord: &WriteCoordinator,
        snap: &yt_manifest::Snapshot,
        trace_id: u64,
        tenant: Option<u64>,
    ) -> Vec<FoldedSpan> {
        let mut q = TraceQuery::trace(trace_id, i64::MIN, i64::MAX);
        q.tenant_id = tenant;
        let mut spans = coord.read_spans_query(snap, &q).0;
        spans.sort_by_key(|s| s.span_id);
        spans
    }

    pub(super) fn trace_folded_spans_any_shard(
        &self,
        trace_id: u64,
        tenant: Option<u64>,
    ) -> Vec<FoldedSpan> {
        if self.is_in_process_cluster() {
            let Some(idx) = self.trace_detail_owner_index(tenant, trace_id) else {
                return Vec::new();
            };
            let coord = &self.shards()[idx].coord;
            let snap = coord.pin_snapshot();
            self.trace_folded_spans_for_coord(coord, &snap, trace_id, tenant)
        } else {
            let snap = self.coord().pin_snapshot();
            self.trace_folded_spans(&snap, trace_id, tenant)
        }
    }

    pub(super) fn materialized_trace_trajectory_any_shard(
        &self,
        trace_id: u64,
        tenant: Option<u64>,
    ) -> Option<crate::TraceTrajectorySummary> {
        if self.is_in_process_cluster() {
            let idx = self.trace_detail_owner_index(tenant, trace_id)?;
            let coord = &self.shards()[idx].coord;
            let snap = coord.pin_snapshot();
            coord.materialized_trace_trajectory(&snap, trace_id, tenant)
        } else {
            let snap = self.coord().pin_snapshot();
            self.coord()
                .materialized_trace_trajectory(&snap, trace_id, tenant)
        }
    }

    /// GET /v1/loops：按 `loop_id` 聚合出 agent loop 摘要。
    ///
    /// 这是轻量读模型，不做自动诊断；它只把一等 task/loop/validation 字段折叠成稳定分页结果。
    pub(super) fn loops_page_json(&self, query: &str, tenant: Option<u64>) -> String {
        if let Some(cached) = self.read_model_cache_get("loops", tenant, query) {
            return cached;
        }
        let parts = product_query_parts(query, 50);
        let snap = self.coord().pin_snapshot();
        let mut rollup_stats = None;
        let mut rollup_fallback = None;
        let mut loops = if parts.filter.is_empty() {
            match self.product_query_rollup_rows_for_coord(
                self.coord(),
                &snap,
                tenant,
                &parts.attrs,
                &parts.annotation,
                &parts.dataset,
            ) {
                Ok(read) => {
                    let buckets = loop_summary_buckets_from_rollup_rows(&read.rows);
                    rollup_stats = Some(read.stats);
                    buckets
                }
                Err(reason) => {
                    rollup_fallback = Some(reason);
                    let spans = self.product_query_spans(
                        &snap,
                        tenant,
                        &parts.attrs,
                        &parts.annotation,
                        &parts.dataset,
                    );
                    loop_summary_buckets(&spans)
                }
            }
        } else {
            rollup_fallback = Some("text_filter");
            let mut spans = self.product_query_spans(
                &snap,
                tenant,
                &parts.attrs,
                &parts.annotation,
                &parts.dataset,
            );
            spans.retain(|s| loop_span_contains(s, &parts.filter));
            loop_summary_buckets(&spans)
        };
        loops.sort_by(|a, b| {
            b.last_trace_id
                .cmp(&a.last_trace_id)
                .then_with(|| a.loop_id.cmp(&b.loop_id))
        });
        let total = loops.len();
        let end = (parts.cursor + parts.limit).min(total);
        let page = if parts.cursor < total {
            &loops[parts.cursor..end]
        } else {
            &[][..]
        };
        let items = page
            .iter()
            .map(json_loop_summary_bucket)
            .collect::<Vec<_>>()
            .join(",");
        let next = if end < total {
            end.to_string()
        } else {
            "null".to_string()
        };
        let loop_index = loop_task_index_label(
            rollup_stats.as_ref(),
            "loop_task_sidecar+tail_overlay",
            "loop_task_tail_folded_scan",
            "loop_folded_scan",
        );
        let response = format!(
            r#"{{"items":[{}],"nextCursor":{},"total":{},"loopIndex":"{}"{} }}"#,
            items,
            next,
            total,
            loop_index,
            loop_task_read_plan_fields_json(rollup_stats.as_ref(), rollup_fallback),
        )
        .replace(" }", "}");
        self.read_model_cache_put("loops", tenant, query, response)
    }

    pub(super) fn cluster_loops_page_json(
        &self,
        query: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let read_set = match self.cluster_snapshot_read_set_from_query(query) {
            Ok(read_set) => read_set,
            Err(resp) => return resp,
        };
        let cache_input = format!("{}|{}", query, read_set.cache_fingerprint());
        if let Some(cached) = self.read_model_cache_get("cluster_loops", tenant, &cache_input) {
            return (200, cached);
        }
        let parts = product_query_parts(query, 50);
        let mut rollup_stats = None;
        let mut rollup_fallback = None;
        let mut loops = Vec::new();
        if parts.filter.is_empty() {
            let mut rows = Vec::new();
            let mut stats = crate::TraceAggregateRollupStats::default();
            let mut failed = None;
            for idx in 0..self.shards().len() {
                match self.product_query_rollup_rows_for_coord(
                    read_set.coord_at(idx),
                    read_set.snapshot_at(idx),
                    tenant,
                    &parts.attrs,
                    &parts.annotation,
                    &parts.dataset,
                ) {
                    Ok(read) => {
                        stats.add_shard(&read.stats);
                        rows.extend(read.rows);
                    }
                    Err(reason) => {
                        failed = Some(reason);
                        break;
                    }
                }
            }
            if let Some(reason) = failed {
                rollup_fallback = Some(reason);
            } else {
                loops = loop_summary_buckets_from_rollup_rows(&rows);
                rollup_stats = Some(stats);
            }
        } else {
            rollup_fallback = Some("text_filter");
        }
        if rollup_stats.is_none() {
            let mut spans = self.cluster_product_query_spans_with_read_set(
                &read_set,
                tenant,
                &parts.attrs,
                &parts.annotation,
                &parts.dataset,
            );
            if !parts.filter.is_empty() {
                spans.retain(|s| loop_span_contains(s, &parts.filter));
            }
            loops = loop_summary_buckets(&spans);
        }
        loops.sort_by(|a, b| {
            b.last_trace_id
                .cmp(&a.last_trace_id)
                .then_with(|| a.loop_id.cmp(&b.loop_id))
        });
        let total = loops.len();
        let end = (parts.cursor + parts.limit).min(total);
        let page = if parts.cursor < total {
            &loops[parts.cursor..end]
        } else {
            &[][..]
        };
        let items = page
            .iter()
            .map(json_loop_summary_bucket)
            .collect::<Vec<_>>()
            .join(",");
        let next = if end < total {
            end.to_string()
        } else {
            "null".to_string()
        };
        let loop_index = loop_task_index_label(
            rollup_stats.as_ref(),
            "fanout_loop_task_sidecar+tail_overlay",
            "fanout_loop_task_tail_folded_scan",
            "fanout_loop_folded_scan",
        );
        let response = format!(
            r#"{{"items":[{}],"nextCursor":{},"total":{},"loopIndex":"{}","queryMode":"fanout_merge","shardCount":{}{}{} }}"#,
            items,
            next,
            total,
            loop_index,
            self.shards().len(),
            read_set.snapshot_field(),
            loop_task_read_plan_fields_json(rollup_stats.as_ref(), rollup_fallback),
        )
        .replace(" }", "}");
        (
            200,
            self.read_model_cache_put("cluster_loops", tenant, &cache_input, response),
        )
    }

    /// GET /v1/loops/:id：返回一个 loop 的摘要、trace 列表和 span 列表。
    pub(super) fn loop_detail_json(
        &self,
        id: &str,
        query: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let mut parts = product_query_parts(query, 200);
        let loop_id = url_decode(id);
        parts
            .attrs
            .insert("loop_id".to_string(), json_string_value(&loop_id));
        let snap = self.coord().pin_snapshot();
        let mut spans = self.product_query_spans(
            &snap,
            tenant,
            &parts.attrs,
            &parts.annotation,
            &parts.dataset,
        );
        if !parts.filter.is_empty() {
            spans.retain(|s| loop_span_contains(s, &parts.filter));
        }
        if spans.is_empty() {
            return (404, r#"{"error":"loop not found"}"#.to_string());
        }
        spans.sort_by_key(|s| (s.trace_id, s.span_id));
        let mut loops = loop_summary_buckets(&spans);
        let Some(summary) = loops.pop() else {
            return (404, r#"{"error":"loop not found"}"#.to_string());
        };
        let traces = trace_summary_buckets_from_spans(&spans)
            .iter()
            .map(json_task_trace_summary_bucket)
            .collect::<Vec<_>>()
            .join(",");
        let span_items = spans
            .iter()
            .enumerate()
            .map(|(rank, span)| json_trace_search_span(span, rank))
            .collect::<Vec<_>>()
            .join(",");
        (
            200,
            format!(
                r#"{{"summary":{},"traces":[{}],"spans":[{}]}}"#,
                json_loop_summary_bucket(&summary),
                traces,
                span_items
            ),
        )
    }

    pub(super) fn cluster_loop_detail_json(
        &self,
        id: &str,
        query: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let mut parts = product_query_parts(query, 200);
        let loop_id = url_decode(id);
        parts
            .attrs
            .insert("loop_id".to_string(), json_string_value(&loop_id));
        let mut spans = self.cluster_product_query_spans(
            tenant,
            &parts.attrs,
            &parts.annotation,
            &parts.dataset,
        );
        if !parts.filter.is_empty() {
            spans.retain(|s| loop_span_contains(s, &parts.filter));
        }
        if spans.is_empty() {
            return (404, r#"{"error":"loop not found"}"#.to_string());
        }
        spans.sort_by_key(|s| (s.trace_id, s.span_id));
        let mut loops = loop_summary_buckets(&spans);
        let Some(summary) = loops.pop() else {
            return (404, r#"{"error":"loop not found"}"#.to_string());
        };
        let traces = trace_summary_buckets_from_spans(&spans)
            .iter()
            .map(json_task_trace_summary_bucket)
            .collect::<Vec<_>>()
            .join(",");
        let span_items = spans
            .iter()
            .enumerate()
            .map(|(rank, span)| json_trace_search_span(span, rank))
            .collect::<Vec<_>>()
            .join(",");
        (
            200,
            format!(
                r#"{{"summary":{},"traces":[{}],"spans":[{}],"queryMode":"fanout_merge","shardCount":{}}}"#,
                json_loop_summary_bucket(&summary),
                traces,
                span_items,
                self.shards().len()
            ),
        )
    }

    /// GET /v1/tasks/:fingerprint/traces：列出同类任务的 trace 摘要。
    pub(super) fn task_traces_json(
        &self,
        fingerprint: &str,
        query: &str,
        tenant: Option<u64>,
    ) -> String {
        let cache_input = format!("{fingerprint}?{query}");
        if let Some(cached) = self.read_model_cache_get("task_traces", tenant, &cache_input) {
            return cached;
        }
        let mut parts = product_query_parts(query, 50);
        let task_fingerprint = url_decode(fingerprint);
        parts.attrs.insert(
            "task_fingerprint".to_string(),
            json_string_value(&task_fingerprint),
        );
        let snap = self.coord().pin_snapshot();
        let mut rollup_stats = None;
        let mut rollup_fallback = None;
        let mut traces = if parts.filter.is_empty() {
            match self.product_query_rollup_rows_for_coord(
                self.coord(),
                &snap,
                tenant,
                &parts.attrs,
                &parts.annotation,
                &parts.dataset,
            ) {
                Ok(read) => {
                    let buckets = trace_summary_buckets_from_rollup_rows(&read.rows);
                    rollup_stats = Some(read.stats);
                    buckets
                }
                Err(reason) => {
                    rollup_fallback = Some(reason);
                    let spans = self.product_query_spans(
                        &snap,
                        tenant,
                        &parts.attrs,
                        &parts.annotation,
                        &parts.dataset,
                    );
                    trace_summary_buckets_from_spans(&spans)
                }
            }
        } else {
            rollup_fallback = Some("text_filter");
            let mut spans = self.product_query_spans(
                &snap,
                tenant,
                &parts.attrs,
                &parts.annotation,
                &parts.dataset,
            );
            spans.retain(|s| folded_contains(s, &parts.filter));
            trace_summary_buckets_from_spans(&spans)
        };
        traces.sort_by(|a, b| b.trace_id.cmp(&a.trace_id));
        let total = traces.len();
        let end = (parts.cursor + parts.limit).min(total);
        let page = if parts.cursor < total {
            &traces[parts.cursor..end]
        } else {
            &[][..]
        };
        let items = page
            .iter()
            .map(json_task_trace_summary_bucket)
            .collect::<Vec<_>>()
            .join(",");
        let next = if end < total {
            end.to_string()
        } else {
            "null".to_string()
        };
        let task_index = loop_task_index_label(
            rollup_stats.as_ref(),
            "loop_task_sidecar+tail_overlay",
            "loop_task_tail_folded_scan",
            "task_folded_scan",
        );
        let response = format!(
            r#"{{"items":[{}],"nextCursor":{},"total":{},"taskIndex":"{}"{} }}"#,
            items,
            next,
            total,
            task_index,
            loop_task_read_plan_fields_json(rollup_stats.as_ref(), rollup_fallback),
        )
        .replace(" }", "}");
        self.read_model_cache_put("task_traces", tenant, &cache_input, response)
    }

    pub(super) fn cluster_task_traces_json(
        &self,
        fingerprint: &str,
        query: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let read_set = match self.cluster_snapshot_read_set_from_query(query) {
            Ok(read_set) => read_set,
            Err(resp) => return resp,
        };
        let cache_input = format!("{}?{}|{}", fingerprint, query, read_set.cache_fingerprint());
        if let Some(cached) = self.read_model_cache_get("cluster_task_traces", tenant, &cache_input)
        {
            return (200, cached);
        }
        let mut parts = product_query_parts(query, 50);
        let task_fingerprint = url_decode(fingerprint);
        parts.attrs.insert(
            "task_fingerprint".to_string(),
            json_string_value(&task_fingerprint),
        );
        let mut rollup_stats = None;
        let mut rollup_fallback = None;
        let mut traces = Vec::new();
        if parts.filter.is_empty() {
            let mut rows = Vec::new();
            let mut stats = crate::TraceAggregateRollupStats::default();
            let mut failed = None;
            for idx in 0..self.shards().len() {
                match self.product_query_rollup_rows_for_coord(
                    read_set.coord_at(idx),
                    read_set.snapshot_at(idx),
                    tenant,
                    &parts.attrs,
                    &parts.annotation,
                    &parts.dataset,
                ) {
                    Ok(read) => {
                        stats.add_shard(&read.stats);
                        rows.extend(read.rows);
                    }
                    Err(reason) => {
                        failed = Some(reason);
                        break;
                    }
                }
            }
            if let Some(reason) = failed {
                rollup_fallback = Some(reason);
            } else {
                traces = trace_summary_buckets_from_rollup_rows(&rows);
                rollup_stats = Some(stats);
            }
        } else {
            rollup_fallback = Some("text_filter");
        }
        if rollup_stats.is_none() {
            let mut spans = self.cluster_product_query_spans_with_read_set(
                &read_set,
                tenant,
                &parts.attrs,
                &parts.annotation,
                &parts.dataset,
            );
            if !parts.filter.is_empty() {
                spans.retain(|s| folded_contains(s, &parts.filter));
            }
            traces = trace_summary_buckets_from_spans(&spans);
        }
        traces.sort_by(|a, b| b.trace_id.cmp(&a.trace_id));
        let total = traces.len();
        let end = (parts.cursor + parts.limit).min(total);
        let page = if parts.cursor < total {
            &traces[parts.cursor..end]
        } else {
            &[][..]
        };
        let items = page
            .iter()
            .map(json_task_trace_summary_bucket)
            .collect::<Vec<_>>()
            .join(",");
        let next = if end < total {
            end.to_string()
        } else {
            "null".to_string()
        };
        let task_index = loop_task_index_label(
            rollup_stats.as_ref(),
            "fanout_loop_task_sidecar+tail_overlay",
            "fanout_loop_task_tail_folded_scan",
            "fanout_task_folded_scan",
        );
        let response = format!(
            r#"{{"items":[{}],"nextCursor":{},"total":{},"taskIndex":"{}","queryMode":"fanout_merge","shardCount":{}{}{} }}"#,
            items,
            next,
            total,
            task_index,
            self.shards().len(),
            read_set.snapshot_field(),
            loop_task_read_plan_fields_json(rollup_stats.as_ref(), rollup_fallback),
        )
        .replace(" }", "}");
        (
            200,
            self.read_model_cache_put("cluster_task_traces", tenant, &cache_input, response),
        )
    }

    pub(super) fn product_query_spans(
        &self,
        snap: &yt_manifest::Snapshot,
        tenant: Option<u64>,
        attr_filter: &std::collections::BTreeMap<String, String>,
        annotation_spec: &TraceSearchAnnotationSpec,
        dataset_spec: &TraceSearchDatasetSpec,
    ) -> Vec<FoldedSpan> {
        self.product_query_spans_for_coord(
            self.coord(),
            snap,
            tenant,
            attr_filter,
            annotation_spec,
            dataset_spec,
        )
    }

    pub(super) fn product_query_spans_for_coord(
        &self,
        coord: &WriteCoordinator,
        snap: &yt_manifest::Snapshot,
        tenant: Option<u64>,
        attr_filter: &std::collections::BTreeMap<String, String>,
        annotation_spec: &TraceSearchAnnotationSpec,
        dataset_spec: &TraceSearchDatasetSpec,
    ) -> Vec<FoldedSpan> {
        let metadata_matches = self.trace_search_metadata_matches_for_coord(
            coord,
            annotation_spec,
            dataset_spec,
            tenant,
        );
        let mut q = TraceQuery::all();
        q.tenant_id = tenant;
        let mut spans = if attr_filter.is_empty() {
            coord.read_spans_query(snap, &q).0
        } else {
            coord.read_spans_query_for_attrs(snap, &q, attr_filter)
        };
        spans.retain(|s| trace_search_metadata_match(s, &metadata_matches));
        spans
    }

    fn product_query_rollup_rows_for_coord(
        &self,
        coord: &WriteCoordinator,
        snap: &yt_manifest::Snapshot,
        tenant: Option<u64>,
        attr_filter: &std::collections::BTreeMap<String, String>,
        annotation_spec: &TraceSearchAnnotationSpec,
        dataset_spec: &TraceSearchDatasetSpec,
    ) -> Result<crate::TraceAggregateRollupRead, &'static str> {
        if annotation_spec.active || dataset_spec.active {
            return Err("metadata_filter");
        }
        let mut q = TraceQuery::all();
        q.tenant_id = tenant;
        coord.trace_aggregate_rollup_read(
            snap,
            &q,
            &crate::TraceAggregateRollupFilters {
                attrs: attr_filter.clone(),
                ..Default::default()
            },
        )
    }

    pub(super) fn cluster_product_query_spans(
        &self,
        tenant: Option<u64>,
        attr_filter: &std::collections::BTreeMap<String, String>,
        annotation_spec: &TraceSearchAnnotationSpec,
        dataset_spec: &TraceSearchDatasetSpec,
    ) -> Vec<FoldedSpan> {
        let read_set = self.pin_cluster_snapshot_read_set();
        self.cluster_product_query_spans_with_read_set(
            &read_set,
            tenant,
            attr_filter,
            annotation_spec,
            dataset_spec,
        )
    }

    pub(super) fn cluster_product_query_spans_with_read_set(
        &self,
        read_set: &ClusterSnapshotReadSet,
        tenant: Option<u64>,
        attr_filter: &std::collections::BTreeMap<String, String>,
        annotation_spec: &TraceSearchAnnotationSpec,
        dataset_spec: &TraceSearchDatasetSpec,
    ) -> Vec<FoldedSpan> {
        let mut spans = Vec::new();
        for (idx, shard) in self.shards().iter().enumerate() {
            spans.extend(self.product_query_spans_for_coord(
                &shard.coord,
                read_set.snapshot_at(idx),
                tenant,
                attr_filter,
                annotation_spec,
                dataset_spec,
            ));
        }
        spans
    }
}
