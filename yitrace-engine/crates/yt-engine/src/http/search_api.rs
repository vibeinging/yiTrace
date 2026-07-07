use super::*;

impl EngineJsonApi {
    pub(super) fn search_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        let request = match search_json_request(body, tenant) {
            Ok(request) => request,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let hits = self.search_hits_for_coord(self.coord(), &request);
        (200, json_search_hits(&hits, &request))
    }

    pub(super) fn cluster_search_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        let request = match search_json_request(body, tenant) {
            Ok(request) => request,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let mut hits = Vec::new();
        let mut failures = Vec::new();
        let mut ok_shards = 0usize;
        for shard in self.shards().iter() {
            let (client, _) = self.eventually_consistent_read_client_for_shard(shard);
            match client.search_hits(&request) {
                Ok(shard_hits) => {
                    ok_shards += 1;
                    hits.extend(shard_hits);
                }
                Err(err) => failures.push(FanoutShardFailure {
                    shard_id: shard.id.as_str().to_string(),
                    status: err.status,
                    error: err.message,
                }),
            }
        }
        if ok_shards == 0 && !failures.is_empty() {
            let report = FanoutReport::from_parts(self.shards().len(), ok_shards, failures);
            return (
                503,
                format!(
                    r#"{{"error":"all shards unavailable","queryMode":"fanout_merge"{}}}"#,
                    report.json_fields()
                ),
            );
        }
        hits.sort_by(|(left_span, left_score), (right_span, right_score)| {
            right_score
                .partial_cmp(left_score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left_span.trace_id.cmp(&right_span.trace_id))
                .then_with(|| left_span.span_id.cmp(&right_span.span_id))
        });
        let mut seen = std::collections::HashSet::new();
        hits.retain(|(span, _)| seen.insert((span.trace_id, span.span_id)));
        hits.truncate(request.k);
        let report = FanoutReport::from_parts(self.shards().len(), ok_shards, failures);
        if request.include_fanout {
            (200, json_search_hits_with_fanout(&hits, &request, &report))
        } else {
            (200, json_search_hits(&hits, &request))
        }
    }

    pub(super) fn search_hits_for_coord(
        &self,
        coord: &WriteCoordinator,
        request: &SearchJsonRequest,
    ) -> Vec<(FoldedSpan, f32)> {
        let snap = coord.pin_snapshot();
        match (!request.text.is_empty(), !request.vector.is_empty()) {
            (true, true) if request.text_domains.is_empty() => coord.search_hybrid_attr(
                &snap,
                &request.text,
                &request.vector,
                request.k,
                &request.filter,
            ),
            (true, true) => {
                let text_hits = coord.search_text_domains_attr(
                    &snap,
                    &request.text,
                    &request.text_domains,
                    request.k.max(10),
                    &request.filter,
                );
                let vec_hits = coord.search_similar_attr(
                    &snap,
                    &request.vector,
                    request.k.max(10),
                    &request.filter,
                );
                fuse_search_hit_rows(text_hits, vec_hits, request.k)
            }
            (false, true) => {
                coord.search_similar_attr(&snap, &request.vector, request.k, &request.filter)
            }
            _ if request.text_domains.is_empty() => {
                coord.search_text_attr(&snap, &request.text, request.k, &request.filter)
            }
            _ => coord.search_text_domains_attr(
                &snap,
                &request.text,
                &request.text_domains,
                request.k,
                &request.filter,
            ),
        }
    }

    /// POST /v1/trace-search：跨 session 的结构化 span 搜索。
    ///
    /// 它和 `/v1/search` 分工不同：后者做 BM25/向量召回；这里做产品列表页需要的精确筛选、
    /// contains、分页和排序，便于 AgenticData 从 trace 数据里找可复用路径。
    pub(super) fn trace_search_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        use crate::wire::{parse, Json};
        let v = match parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };

        let cursor = json_field_alias(&v, &["cursor", "offset"])
            .and_then(Json::as_u64)
            .unwrap_or(0) as usize;
        let limit = json_field_alias(&v, &["limit", "k"])
            .and_then(Json::as_u64)
            .unwrap_or(50)
            .clamp(1, 500) as usize;
        let sort_by = json_field_alias(&v, &["sort_by", "sortBy", "sort"])
            .and_then(Json::as_str)
            .unwrap_or("created")
            .to_string();
        let order = json_field_alias(&v, &["order", "direction"])
            .and_then(Json::as_str)
            .unwrap_or("desc");
        let desc = !order.eq_ignore_ascii_case("asc");

        let request = trace_search_request_from_json(&v, tenant);
        let read_set = match self.local_snapshot_read_set_from_body(&v) {
            Ok(read_set) => read_set,
            Err(resp) => return resp,
        };
        let cache_input = format!("{}|{}", body, read_set.cache_fingerprint());
        if let Some(cached) = self.read_model_cache_get("trace_search", tenant, &cache_input) {
            return (200, cached);
        }
        let metadata_matches =
            self.trace_search_metadata_matches(&request.annotation, &request.dataset, tenant);

        let rollup_blockers = trace_search_rollup_blockers(&request);
        let mut rollup_fallback_reason: Option<&'static str> = None;
        if rollup_blockers.is_empty() {
            let filters = trace_aggregate_rollup_filters(&request);
            match read_set.coord_at(0).trace_search_rollup_page_read(
                read_set.snapshot_at(0),
                &request.query,
                &filters,
                &sort_by,
                desc,
                cursor,
                limit,
            ) {
                Ok(page_read) => {
                    let keys: std::collections::HashSet<(u64, u64)> =
                        page_read.keys.iter().copied().collect();
                    let full_spans = if page_read.keys.is_empty() {
                        Vec::new()
                    } else {
                        read_set
                            .coord_at(0)
                            .read_spans_query_for_keys_projected(
                                read_set.snapshot_at(0),
                                &request.query,
                                &keys,
                                crate::Projection::ALL,
                            )
                            .0
                    };
                    let mut by_key: std::collections::HashMap<(u64, u64), FoldedSpan> = full_spans
                        .into_iter()
                        .map(|span| ((span.trace_id, span.span_id), span))
                        .collect();
                    let full_page = page_read
                        .keys
                        .iter()
                        .filter_map(|key| by_key.remove(key))
                        .collect::<Vec<_>>();
                    if full_page.len() == page_read.keys.len() {
                        let items: Vec<String> = full_page
                            .iter()
                            .enumerate()
                            .map(|(i, s)| json_trace_search_span(s, cursor + i))
                            .collect();
                        let end = cursor.saturating_add(limit).min(page_read.total);
                        let next = if end < page_read.total {
                            end.to_string()
                        } else {
                            "null".to_string()
                        };
                        let read_plan = trace_search_read_plan_json(
                            "trace_search_rollup",
                            &AttrIndexedReadStats::default(),
                            Some(&page_read.stats),
                            page_read.total,
                            page_read.keys.len(),
                            &rollup_blockers,
                            None,
                        );
                        let response = format!(
                            r#"{{"items":[{}],"nextCursor":{},"total":{},"index":"{}","readPlan":{}{} }}"#,
                            items.join(","),
                            next,
                            page_read.total,
                            trace_search_index_label(&request),
                            read_plan,
                            read_set.snapshot_field(),
                        );
                        return (
                            200,
                            self.read_model_cache_put(
                                "trace_search",
                                tenant,
                                &cache_input,
                                response,
                            ),
                        );
                    }
                    rollup_fallback_reason = Some("page_hydrate_miss");
                }
                Err(reason) => {
                    rollup_fallback_reason = Some(reason);
                }
            }
        }

        let scan_projection = trace_search_scan_projection(&request, &sort_by);
        let (mut spans, attr_stats) = if request.spec.attrs.is_empty() {
            let (spans, scanned_segments) = read_set.coord_at(0).read_spans_query_projected(
                read_set.snapshot_at(0),
                &request.query,
                scan_projection,
            );
            (
                spans,
                AttrIndexedReadStats {
                    scanned_segments,
                    ..Default::default()
                },
            )
        } else {
            read_set
                .coord_at(0)
                .read_spans_query_for_attrs_projected_with_stats(
                    read_set.snapshot_at(0),
                    &request.query,
                    &request.spec.attrs,
                    scan_projection,
                )
        };
        spans.retain(|s| trace_search_match(s, &request.spec, &metadata_matches));
        sort_trace_search_spans(&mut spans, &sort_by, desc);

        let total = spans.len();
        let end = cursor.saturating_add(limit).min(total);
        let page = if cursor < total {
            &spans[cursor..end]
        } else {
            &[][..]
        };
        let full_page;
        let page = if scan_projection == crate::Projection::ALL || page.is_empty() {
            page
        } else {
            let keys: std::collections::HashSet<(u64, u64)> =
                page.iter().map(|s| (s.trace_id, s.span_id)).collect();
            let full_spans = read_set
                .coord_at(0)
                .read_spans_query_for_keys_projected(
                    read_set.snapshot_at(0),
                    &request.query,
                    &keys,
                    crate::Projection::ALL,
                )
                .0;
            let mut by_key: std::collections::HashMap<(u64, u64), FoldedSpan> = full_spans
                .into_iter()
                .map(|span| ((span.trace_id, span.span_id), span))
                .collect();
            full_page = page
                .iter()
                .filter_map(|span| by_key.remove(&(span.trace_id, span.span_id)))
                .collect::<Vec<_>>();
            &full_page
        };
        let items: Vec<String> = page
            .iter()
            .enumerate()
            .map(|(i, s)| json_trace_search_span(s, cursor + i))
            .collect();
        let next = if end < total {
            end.to_string()
        } else {
            "null".to_string()
        };
        let read_plan = trace_search_read_plan_json(
            "folded_scan",
            &attr_stats,
            None,
            total,
            page.len(),
            &rollup_blockers,
            rollup_fallback_reason,
        );
        let response = format!(
            r#"{{"items":[{}],"nextCursor":{},"total":{},"index":"{}","readPlan":{}{} }}"#,
            items.join(","),
            next,
            total,
            trace_search_index_label(&request),
            read_plan,
            read_set.snapshot_field(),
        );
        (
            200,
            self.read_model_cache_put("trace_search", tenant, &cache_input, response),
        )
    }

    pub(super) fn cluster_trace_search_json(
        &self,
        body: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        use crate::wire::{parse, Json};
        let v = match parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let read_set = match self.eventually_consistent_cluster_snapshot_read_set_from_body(&v) {
            Ok(read_set) => read_set,
            Err(resp) => return resp,
        };
        let cache_input = format!("{}|{}", body, read_set.cache_fingerprint());
        if let Some(cached) =
            self.read_model_cache_get("cluster_trace_search", tenant, &cache_input)
        {
            return (200, cached);
        }

        let cursor = json_field_alias(&v, &["cursor", "offset"])
            .and_then(Json::as_u64)
            .unwrap_or(0) as usize;
        let limit = json_field_alias(&v, &["limit", "k"])
            .and_then(Json::as_u64)
            .unwrap_or(50)
            .clamp(1, 500) as usize;
        let sort_by = json_field_alias(&v, &["sort_by", "sortBy", "sort"])
            .and_then(Json::as_str)
            .unwrap_or("created")
            .to_string();
        let order = json_field_alias(&v, &["order", "direction"])
            .and_then(Json::as_str)
            .unwrap_or("desc");
        let desc = !order.eq_ignore_ascii_case("asc");

        let request = trace_search_request_from_json(&v, tenant);
        let mut spans = Vec::new();
        for idx in 0..self.shards().len() {
            spans.extend(self.trace_search_spans_for_coord_snapshot(
                read_set.coord_at(idx),
                read_set.snapshot_at(idx),
                &request,
                tenant,
            ));
        }
        sort_trace_search_spans(&mut spans, &sort_by, desc);

        let total = spans.len();
        let end = cursor.saturating_add(limit).min(total);
        let page = if cursor < total {
            &spans[cursor..end]
        } else {
            &[][..]
        };
        let items: Vec<String> = page
            .iter()
            .enumerate()
            .map(|(i, s)| json_trace_search_span(s, cursor + i))
            .collect();
        let next = if end < total {
            end.to_string()
        } else {
            "null".to_string()
        };
        let report = FanoutReport::all_ok(self.shards().len());
        let response = format!(
            r#"{{"items":[{}],"nextCursor":{},"total":{},"index":"{}","queryMode":"fanout_merge"{}{}}}"#,
            items.join(","),
            next,
            total,
            trace_search_index_label(&request),
            report.json_fields(),
            read_set.snapshot_field(),
        );
        (
            200,
            self.read_model_cache_put("cluster_trace_search", tenant, &cache_input, response),
        )
    }

    /// POST /v1/trace-aggregate：对结构化 trace/span 搜索结果做 group-by 聚合。
    ///
    /// 用于产品侧从 trace 数据里看出“哪个 skill/mode/tool 路径最常见、最贵、最容易失败”。
    /// 过滤语义完全复用 `/v1/trace-search`，避免搜索页和聚合页看到不同的数据集。
    pub(super) fn trace_aggregate_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        use crate::wire::{parse, Json};
        let v = match parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let group_fields = match trace_aggregate_group_fields(&v) {
            Ok(fields) => fields,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let limit = json_field_alias(&v, &["limit", "k"])
            .and_then(Json::as_u64)
            .unwrap_or(100)
            .clamp(1, 500) as usize;
        let sort_by = json_field_alias(&v, &["sort_by", "sortBy", "sort"])
            .and_then(Json::as_str)
            .unwrap_or("count")
            .to_string();
        let order = json_field_alias(&v, &["order", "direction"])
            .and_then(Json::as_str)
            .unwrap_or("desc");
        let desc = !order.eq_ignore_ascii_case("asc");
        let read_set = match self.local_snapshot_read_set_from_body(&v) {
            Ok(read_set) => read_set,
            Err(resp) => return resp,
        };
        let cache_input = format!("{}|{}", body, read_set.cache_fingerprint());
        if let Some(cached) = self.read_model_cache_get("trace_aggregate", tenant, &cache_input) {
            return (200, cached);
        }

        let request = trace_search_request_from_json(&v, tenant);
        let blockers = trace_aggregate_rollup_blockers(&group_fields, &request);
        let mut read_stats = AttrIndexedReadStats::default();
        let mut rollup_stats = None;
        let mut rollup_fallback_reason = None;
        let mut preaggregate_profile = None;
        let mut used_segment_rollup = false;
        let (mut buckets, span_total) = if blockers.is_empty() {
            if let Some(profile_fields) =
                trace_aggregate_preaggregate_fields(&group_fields, &request)
            {
                match read_set.coord_at(0).trace_aggregate_preaggregate_read(
                    read_set.snapshot_at(0),
                    &request.query,
                    &trace_aggregate_rollup_filters(&request),
                    &profile_fields,
                ) {
                    Ok(read) => {
                        let span_total = read.buckets.iter().map(|bucket| bucket.span_count).sum();
                        used_segment_rollup = read.stats.used_segment_rollup;
                        let buckets = trace_aggregate_buckets_from_preaggregate_buckets(
                            &read.buckets,
                            &group_fields,
                        );
                        preaggregate_profile = Some(profile_fields);
                        rollup_stats = Some(read.stats);
                        (buckets, span_total)
                    }
                    Err(reason) => {
                        rollup_fallback_reason = Some(reason);
                        match read_set.coord_at(0).trace_aggregate_rollup_read(
                            read_set.snapshot_at(0),
                            &request.query,
                            &trace_aggregate_rollup_filters(&request),
                        ) {
                            Ok(read) => {
                                let span_total = read.rows.len();
                                used_segment_rollup = read.stats.used_segment_rollup;
                                let buckets = trace_aggregate_buckets_from_rollup_rows(
                                    &read.rows,
                                    &group_fields,
                                );
                                rollup_stats = Some(read.stats);
                                (buckets, span_total)
                            }
                            Err(row_reason) => {
                                rollup_fallback_reason = Some(row_reason);
                                let (spans, stats) = self
                                    .trace_search_spans_for_coord_snapshot_with_stats(
                                        read_set.coord_at(0),
                                        read_set.snapshot_at(0),
                                        &request,
                                        tenant,
                                    );
                                let span_total = spans.len();
                                read_stats = stats;
                                (trace_aggregate_buckets(&spans, &group_fields), span_total)
                            }
                        }
                    }
                }
            } else {
                match read_set.coord_at(0).trace_aggregate_rollup_read(
                    read_set.snapshot_at(0),
                    &request.query,
                    &trace_aggregate_rollup_filters(&request),
                ) {
                    Ok(read) => {
                        let span_total = read.rows.len();
                        used_segment_rollup = read.stats.used_segment_rollup;
                        let buckets =
                            trace_aggregate_buckets_from_rollup_rows(&read.rows, &group_fields);
                        rollup_stats = Some(read.stats);
                        (buckets, span_total)
                    }
                    Err(reason) => {
                        rollup_fallback_reason = Some(reason);
                        let (spans, stats) = self.trace_search_spans_for_coord_snapshot_with_stats(
                            read_set.coord_at(0),
                            read_set.snapshot_at(0),
                            &request,
                            tenant,
                        );
                        let span_total = spans.len();
                        read_stats = stats;
                        (trace_aggregate_buckets(&spans, &group_fields), span_total)
                    }
                }
            }
        } else {
            let (spans, stats) = self.trace_search_spans_for_coord_snapshot_with_stats(
                read_set.coord_at(0),
                read_set.snapshot_at(0),
                &request,
                tenant,
            );
            let span_total = spans.len();
            read_stats = stats;
            (trace_aggregate_buckets(&spans, &group_fields), span_total)
        };
        sort_trace_aggregate_buckets(&mut buckets, &sort_by, desc);
        let total = buckets.len();
        let items: Vec<String> = buckets
            .iter()
            .take(limit)
            .map(|bucket| trace_aggregate_bucket_json(bucket, &group_fields))
            .collect();
        let aggregation_index = if preaggregate_profile.is_some() {
            "aggregate_preaggregate_tail_overlay"
        } else if used_segment_rollup {
            "segment_rollup_tail_overlay"
        } else if rollup_stats.is_some() {
            "tail_folded_scan"
        } else {
            "folded_query_time_reduce"
        };
        let response = format!(
            r#"{{"items":[{}],"total":{},"spanTotal":{},"index":"{}","aggregationIndex":"{}"{}{} }}"#,
            items.join(","),
            total,
            span_total,
            trace_search_index_label(&request),
            aggregation_index,
            read_set.snapshot_field(),
            trace_aggregate_planner_fields_json(
                &group_fields,
                &request,
                &read_stats,
                span_total,
                rollup_stats.as_ref(),
                rollup_fallback_reason,
                preaggregate_profile.as_deref(),
            ),
        )
        .replace(" }", "}");
        (
            200,
            self.read_model_cache_put("trace_aggregate", tenant, &cache_input, response),
        )
    }

    pub(super) fn cluster_trace_aggregate_json(
        &self,
        body: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        use crate::wire::{parse, Json};
        let v = match parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let read_set = match self.eventually_consistent_cluster_snapshot_read_set_from_body(&v) {
            Ok(read_set) => read_set,
            Err(resp) => return resp,
        };
        let group_fields = match trace_aggregate_group_fields(&v) {
            Ok(fields) => fields,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let limit = json_field_alias(&v, &["limit", "k"])
            .and_then(Json::as_u64)
            .unwrap_or(100)
            .clamp(1, 500) as usize;
        let sort_by = json_field_alias(&v, &["sort_by", "sortBy", "sort"])
            .and_then(Json::as_str)
            .unwrap_or("count")
            .to_string();
        let order = json_field_alias(&v, &["order", "direction"])
            .and_then(Json::as_str)
            .unwrap_or("desc");
        let desc = !order.eq_ignore_ascii_case("asc");
        let cache_input = format!("{}|{}", body, read_set.cache_fingerprint());
        if let Some(cached) =
            self.read_model_cache_get("cluster_trace_aggregate", tenant, &cache_input)
        {
            return (200, cached);
        }

        let request = trace_search_request_from_json(&v, tenant);
        let mut read_stats = AttrIndexedReadStats::default();
        let blockers = trace_aggregate_rollup_blockers(&group_fields, &request);
        let mut rollup_stats = None;
        let mut rollup_fallback_reason = None;
        let mut span_total = 0usize;
        let mut used_segment_rollup = false;
        let mut preaggregate_profile = None;
        let mut buckets = Vec::new();
        if blockers.is_empty() {
            if let Some(profile_fields) =
                trace_aggregate_preaggregate_fields(&group_fields, &request)
            {
                let mut preaggregate_buckets = Vec::new();
                let mut stats = crate::TraceAggregateRollupStats::default();
                let mut failed_reason = None;
                for idx in 0..self.shards().len() {
                    match read_set.coord_at(idx).trace_aggregate_preaggregate_read(
                        read_set.snapshot_at(idx),
                        &request.query,
                        &trace_aggregate_rollup_filters(&request),
                        &profile_fields,
                    ) {
                        Ok(read) => {
                            stats.add_shard(&read.stats);
                            preaggregate_buckets.extend(read.buckets);
                        }
                        Err(reason) => {
                            failed_reason = Some(reason);
                            break;
                        }
                    }
                }
                if let Some(reason) = failed_reason {
                    rollup_fallback_reason = Some(reason);
                } else {
                    span_total = preaggregate_buckets
                        .iter()
                        .map(|bucket| bucket.span_count)
                        .sum();
                    used_segment_rollup = stats.used_segment_rollup;
                    buckets = trace_aggregate_buckets_from_preaggregate_buckets(
                        &preaggregate_buckets,
                        &group_fields,
                    );
                    preaggregate_profile = Some(profile_fields);
                    rollup_stats = Some(stats);
                }
            } else {
                let mut rollup_rows = Vec::new();
                let mut stats = crate::TraceAggregateRollupStats::default();
                let mut failed_reason = None;
                for idx in 0..self.shards().len() {
                    match read_set.coord_at(idx).trace_aggregate_rollup_read(
                        read_set.snapshot_at(idx),
                        &request.query,
                        &trace_aggregate_rollup_filters(&request),
                    ) {
                        Ok(read) => {
                            stats.add_shard(&read.stats);
                            rollup_rows.extend(read.rows);
                        }
                        Err(reason) => {
                            failed_reason = Some(reason);
                            break;
                        }
                    }
                }
                if let Some(reason) = failed_reason {
                    rollup_fallback_reason = Some(reason);
                } else {
                    span_total = rollup_rows.len();
                    used_segment_rollup = stats.used_segment_rollup;
                    buckets = trace_aggregate_buckets_from_rollup_rows(&rollup_rows, &group_fields);
                    rollup_stats = Some(stats);
                }
            }
        }
        if rollup_stats.is_none() {
            let mut spans = Vec::new();
            for idx in 0..self.shards().len() {
                let (shard_spans, shard_stats) = self
                    .trace_search_spans_for_coord_snapshot_with_stats(
                        read_set.coord_at(idx),
                        read_set.snapshot_at(idx),
                        &request,
                        tenant,
                    );
                read_stats.add_shard(&shard_stats);
                spans.extend(shard_spans);
            }
            span_total = spans.len();
            buckets = trace_aggregate_buckets(&spans, &group_fields);
        }
        sort_trace_aggregate_buckets(&mut buckets, &sort_by, desc);
        let total = buckets.len();
        let items: Vec<String> = buckets
            .iter()
            .take(limit)
            .map(|bucket| trace_aggregate_bucket_json(bucket, &group_fields))
            .collect();
        let report = FanoutReport::all_ok(self.shards().len());
        let aggregation_index = if preaggregate_profile.is_some() {
            "fanout_aggregate_preaggregate_tail_overlay"
        } else if used_segment_rollup {
            "fanout_segment_rollup_tail_overlay"
        } else if rollup_stats.is_some() {
            "fanout_tail_folded_scan"
        } else {
            "fanout_folded_query_time_reduce"
        };
        let mut response = format!(
            r#"{{"items":[{}],"total":{},"spanTotal":{},"index":"{}","aggregationIndex":"{}","queryMode":"fanout_merge"{}{}"#,
            items.join(","),
            total,
            span_total,
            trace_search_index_label(&request),
            aggregation_index,
            report.json_fields(),
            read_set.snapshot_field(),
        );
        response.push_str(&trace_aggregate_planner_fields_json(
            &group_fields,
            &request,
            &read_stats,
            span_total,
            rollup_stats.as_ref(),
            rollup_fallback_reason,
            preaggregate_profile.as_deref(),
        ));
        response.push('}');
        (
            200,
            self.read_model_cache_put("cluster_trace_aggregate", tenant, &cache_input, response),
        )
    }
}
