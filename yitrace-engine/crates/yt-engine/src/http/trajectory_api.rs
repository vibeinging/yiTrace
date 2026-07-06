use super::*;

impl EngineJsonApi {
    pub(super) fn trajectory_group_buckets_for_coord(
        &self,
        coord: &WriteCoordinator,
        request: &TraceSearchRequest,
        tenant: Option<u64>,
        example_limit: usize,
    ) -> (Vec<TrajectoryGroupBucket>, usize, usize) {
        let snap = coord.pin_snapshot();
        self.trajectory_group_buckets_for_coord_snapshot(
            coord,
            &snap,
            request,
            tenant,
            example_limit,
        )
    }

    pub(super) fn trajectory_group_buckets_for_coord_snapshot(
        &self,
        coord: &WriteCoordinator,
        snap: &yt_manifest::Snapshot,
        request: &TraceSearchRequest,
        tenant: Option<u64>,
        example_limit: usize,
    ) -> (Vec<TrajectoryGroupBucket>, usize, usize) {
        let metadata_matches = self.trace_search_metadata_matches_for_coord(
            coord,
            &request.annotation,
            &request.dataset,
            tenant,
        );
        let mut matching_spans = if request.spec.attrs.is_empty() {
            coord.read_spans_query(snap, &request.query).0
        } else {
            coord.read_spans_query_for_attrs(snap, &request.query, &request.spec.attrs)
        };
        matching_spans.retain(|s| trace_search_match(s, &request.spec, &metadata_matches));
        let span_total = matching_spans.len();
        let trace_ids: std::collections::BTreeSet<u64> =
            matching_spans.iter().map(|s| s.trace_id).collect();

        let annotation_scores =
            trace_annotation_score_map(coord.annotations(&TraceAnnotationFilter {
                tenant_id: tenant,
                ..Default::default()
            }));
        let dataset_scores =
            trace_dataset_score_map(coord.dataset_associations(&DatasetAssociationFilter {
                tenant_id: tenant,
                ..Default::default()
            }));

        let mut by_signature: std::collections::BTreeMap<u64, TrajectoryGroupBucket> =
            std::collections::BTreeMap::new();
        let mut trace_total = 0usize;
        for trace_id in trace_ids {
            let spans = self.trace_folded_spans_for_coord(coord, snap, trace_id, tenant);
            if spans.is_empty() {
                continue;
            }
            trace_total += 1;
            let steps = trajectory_steps(&spans);
            let signature = trajectory_signature(&steps);
            let summary = trace_summary_buckets_from_spans(&spans).into_iter().next();
            let bucket = by_signature
                .entry(signature)
                .or_insert_with(|| TrajectoryGroupBucket::new(signature, steps));
            bucket.add_trace(
                &spans,
                summary.as_ref(),
                annotation_scores.get(&trace_id).map(Vec::as_slice),
                dataset_scores.get(&trace_id).map(Vec::as_slice),
                example_limit,
            );
        }
        (
            by_signature.into_values().collect(),
            trace_total,
            span_total,
        )
    }

    pub(super) fn trajectory_groups_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        use crate::wire::{parse, Json};
        let v = match parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let limit = json_field_alias(&v, &["limit", "k"])
            .and_then(Json::as_u64)
            .unwrap_or(50)
            .clamp(1, 500) as usize;
        let example_limit = json_field_alias(&v, &["example_limit", "exampleLimit", "examples"])
            .and_then(Json::as_u64)
            .unwrap_or(3)
            .clamp(0, 20) as usize;
        let sort_by = json_field_alias(&v, &["sort_by", "sortBy", "sort"])
            .and_then(Json::as_str)
            .unwrap_or("best")
            .to_string();
        let desc = json_field_alias(&v, &["order", "direction"])
            .and_then(Json::as_str)
            .map(|order| !order.eq_ignore_ascii_case("asc"))
            .unwrap_or_else(|| trajectory_default_desc(&sort_by));
        if let Some(cached) = self.read_model_cache_get("trajectory_groups", tenant, body) {
            return (200, cached);
        }

        let request = trace_search_request_from_json(&v, tenant);
        let (mut buckets, trace_total, span_total) =
            self.trajectory_group_buckets_for_coord(self.coord(), &request, tenant, example_limit);
        sort_trajectory_group_buckets(&mut buckets, &sort_by, desc);
        let total = buckets.len();
        let items = buckets
            .iter()
            .take(limit)
            .map(json_trajectory_group_bucket)
            .collect::<Vec<_>>()
            .join(",");
        let response = format!(
            r#"{{"items":[{}],"total":{},"traceTotal":{},"spanTotal":{},"index":"{}","trajectoryIndex":"materialized_trajectory_cache"}}"#,
            items,
            total,
            trace_total,
            span_total,
            trace_search_index_label(&request),
        );
        (
            200,
            self.read_model_cache_put("trajectory_groups", tenant, body, response),
        )
    }

    pub(super) fn cluster_trajectory_groups_json(
        &self,
        body: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        use crate::wire::{parse, Json};
        let v = match parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let read_set = match self.cluster_snapshot_read_set_from_body(&v) {
            Ok(read_set) => read_set,
            Err(resp) => return resp,
        };
        let limit = json_field_alias(&v, &["limit", "k"])
            .and_then(Json::as_u64)
            .unwrap_or(50)
            .clamp(1, 500) as usize;
        let example_limit = json_field_alias(&v, &["example_limit", "exampleLimit", "examples"])
            .and_then(Json::as_u64)
            .unwrap_or(3)
            .clamp(0, 20) as usize;
        let sort_by = json_field_alias(&v, &["sort_by", "sortBy", "sort"])
            .and_then(Json::as_str)
            .unwrap_or("best")
            .to_string();
        let desc = json_field_alias(&v, &["order", "direction"])
            .and_then(Json::as_str)
            .map(|order| !order.eq_ignore_ascii_case("asc"))
            .unwrap_or_else(|| trajectory_default_desc(&sort_by));
        let cache_input = format!("{}|{}", body, read_set.cache_fingerprint());
        if let Some(cached) =
            self.read_model_cache_get("cluster_trajectory_groups", tenant, &cache_input)
        {
            return (200, cached);
        }

        let request = trace_search_request_from_json(&v, tenant);
        let mut buckets = Vec::new();
        let mut trace_total = 0usize;
        let mut span_total = 0usize;
        for (idx, shard) in self.shards().iter().enumerate() {
            let (local, local_traces, local_spans) = self
                .trajectory_group_buckets_for_coord_snapshot(
                    &shard.coord,
                    read_set.snapshot_at(idx),
                    &request,
                    tenant,
                    example_limit,
                );
            buckets.extend(local);
            trace_total += local_traces;
            span_total += local_spans;
        }
        let mut buckets = merge_trajectory_group_buckets(buckets, example_limit);
        sort_trajectory_group_buckets(&mut buckets, &sort_by, desc);
        let total = buckets.len();
        let items = buckets
            .iter()
            .take(limit)
            .map(json_trajectory_group_bucket)
            .collect::<Vec<_>>()
            .join(",");
        let response = format!(
            r#"{{"items":[{}],"total":{},"traceTotal":{},"spanTotal":{},"index":"{}","trajectoryIndex":"fanout_materialized_trajectory_cache","queryMode":"fanout_merge","shardCount":{}{}}}"#,
            items,
            total,
            trace_total,
            span_total,
            trace_search_index_label(&request),
            self.shards().len(),
            read_set.snapshot_field(),
        );
        (
            200,
            self.read_model_cache_put("cluster_trajectory_groups", tenant, &cache_input, response),
        )
    }

    /// POST /v1/trace-trajectories：按 traceSearch 过滤返回每条 trace 的物化 trajectory 摘要。
    pub(super) fn trace_trajectory_summaries_for_coord(
        &self,
        coord: &WriteCoordinator,
        request: &TraceSearchRequest,
        tenant: Option<u64>,
    ) -> (Vec<crate::TraceTrajectorySummary>, usize) {
        let snap = coord.pin_snapshot();
        self.trace_trajectory_summaries_for_coord_snapshot(coord, &snap, request, tenant)
    }

    pub(super) fn trace_trajectory_summaries_for_coord_snapshot(
        &self,
        coord: &WriteCoordinator,
        snap: &yt_manifest::Snapshot,
        request: &TraceSearchRequest,
        tenant: Option<u64>,
    ) -> (Vec<crate::TraceTrajectorySummary>, usize) {
        let metadata_matches = self.trace_search_metadata_matches_for_coord(
            coord,
            &request.annotation,
            &request.dataset,
            tenant,
        );
        let mut matching_spans = if request.spec.attrs.is_empty() {
            coord.read_spans_query(snap, &request.query).0
        } else {
            coord.read_spans_query_for_attrs(snap, &request.query, &request.spec.attrs)
        };
        matching_spans.retain(|s| trace_search_match(s, &request.spec, &metadata_matches));
        let trace_ids: Vec<u64> = matching_spans
            .iter()
            .map(|s| s.trace_id)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let summaries = trace_ids
            .iter()
            .filter_map(|trace_id| coord.materialized_trace_trajectory(snap, *trace_id, tenant))
            .collect();
        (summaries, matching_spans.len())
    }

    pub(super) fn trace_trajectories_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
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
        if let Some(cached) = self.read_model_cache_get("trace_trajectories", tenant, body) {
            return (200, cached);
        }
        let request = trace_search_request_from_json(&v, tenant);
        let (mut summaries, span_total) =
            self.trace_trajectory_summaries_for_coord(self.coord(), &request, tenant);
        summaries.sort_by(|a, b| b.trace_id.cmp(&a.trace_id));
        let total = summaries.len();
        let end = cursor.saturating_add(limit).min(total);
        let page = if cursor < total {
            &summaries[cursor..end]
        } else {
            &[][..]
        };
        let items = page
            .iter()
            .map(|summary| json_trace_trajectory_summary(summary))
            .collect::<Vec<_>>()
            .join(",");
        let next = if end < total {
            end.to_string()
        } else {
            "null".to_string()
        };
        let response = format!(
            r#"{{"items":[{}],"nextCursor":{},"total":{},"spanTotal":{},"index":"materialized_trace_trajectory_cache"}}"#,
            items, next, total, span_total,
        );
        (
            200,
            self.read_model_cache_put("trace_trajectories", tenant, body, response),
        )
    }

    pub(super) fn cluster_trace_trajectories_json(
        &self,
        body: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        use crate::wire::{parse, Json};
        let v = match parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let read_set = match self.cluster_snapshot_read_set_from_body(&v) {
            Ok(read_set) => read_set,
            Err(resp) => return resp,
        };
        let cursor = json_field_alias(&v, &["cursor", "offset"])
            .and_then(Json::as_u64)
            .unwrap_or(0) as usize;
        let limit = json_field_alias(&v, &["limit", "k"])
            .and_then(Json::as_u64)
            .unwrap_or(50)
            .clamp(1, 500) as usize;
        let cache_input = format!("{}|{}", body, read_set.cache_fingerprint());
        if let Some(cached) =
            self.read_model_cache_get("cluster_trace_trajectories", tenant, &cache_input)
        {
            return (200, cached);
        }
        let request = trace_search_request_from_json(&v, tenant);
        let mut summaries = Vec::new();
        let mut span_total = 0usize;
        for (idx, shard) in self.shards().iter().enumerate() {
            let (local, local_spans) = self.trace_trajectory_summaries_for_coord_snapshot(
                &shard.coord,
                read_set.snapshot_at(idx),
                &request,
                tenant,
            );
            summaries.extend(local);
            span_total += local_spans;
        }
        summaries.sort_by(|a, b| b.trace_id.cmp(&a.trace_id));
        summaries.dedup_by_key(|summary| summary.trace_id);
        let total = summaries.len();
        let end = cursor.saturating_add(limit).min(total);
        let page = if cursor < total {
            &summaries[cursor..end]
        } else {
            &[][..]
        };
        let items = page
            .iter()
            .map(|summary| json_trace_trajectory_summary(summary))
            .collect::<Vec<_>>()
            .join(",");
        let next = if end < total {
            end.to_string()
        } else {
            "null".to_string()
        };
        let response = format!(
            r#"{{"items":[{}],"nextCursor":{},"total":{},"spanTotal":{},"index":"fanout_materialized_trace_trajectory_cache","queryMode":"fanout_merge","shardCount":{}{}}}"#,
            items,
            next,
            total,
            span_total,
            self.shards().len(),
            read_set.snapshot_field(),
        );
        (
            200,
            self.read_model_cache_put("cluster_trace_trajectories", tenant, &cache_input, response),
        )
    }
}
