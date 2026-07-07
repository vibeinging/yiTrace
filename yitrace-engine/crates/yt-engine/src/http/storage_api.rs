use super::*;

impl EngineJsonApi {
    pub(super) fn storage_stats_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        if let Some(cached) = self.read_model_cache_get("storage_stats", tenant, body) {
            return (200, cached);
        }
        use crate::wire::{parse, Json};
        let v = match parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let group_by = storage_group_by_from_json(&v);
        let time_bucket_ns = json_field_alias(
            &v,
            &["time_bucket_ns", "timeBucketNs", "bucket_ns", "bucketNs"],
        )
        .and_then(Json::as_u64)
        .unwrap_or(86_400_000_000_000);

        let request = trace_search_request_from_json(&v, tenant);
        let blockers = storage_rollup_blockers(&group_by, &request);
        if blockers.is_empty() {
            let snap = self.coord().pin_snapshot();
            if let Some(profile_fields) = storage_preaggregate_fields(&group_by, &request) {
                if let Ok(read) = self.coord().trace_aggregate_storage_rollup_read(
                    &snap,
                    &request.query,
                    &trace_aggregate_rollup_filters(&request),
                    &profile_fields,
                ) {
                    let metadata = self.storage_metadata_for_tenant(tenant);
                    let report = storage_stats_report_from_preaggregate_buckets(
                        &read.buckets,
                        &metadata,
                        &group_by,
                    );
                    let read_plan = format!(
                        r#"{{"spanReadIndex":"storage_preaggregate","usedSegmentRollup":{},"segmentRollupSegments":{},"segmentRollupRows":{},"tailFoldedSpanCount":{},"storagePreaggregateBuckets":{},"storagePreaggregateProfile":{},"rollupFallbackReason":null}}"#,
                        read.stats.used_segment_rollup,
                        read.stats.segment_rollup_segments,
                        read.stats.segment_rollup_rows,
                        read.stats.tail_folded_span_count,
                        read.buckets.len(),
                        json_string_array(&profile_fields),
                    );
                    let response =
                        json_storage_stats_report_with_read_plan(&report, &group_by, &read_plan);
                    return (
                        200,
                        self.read_model_cache_put("storage_stats", tenant, body, response),
                    );
                }
            }
            if let Ok(read) = self.coord().trace_aggregate_rollup_read(
                &snap,
                &request.query,
                &trace_aggregate_rollup_filters(&request),
            ) {
                let metadata = self.storage_metadata_for_tenant(tenant);
                let report = storage_stats_report_from_rollup_rows(
                    &read.rows,
                    &read.trace_bounds,
                    &metadata,
                    &group_by,
                    time_bucket_ns,
                );
                let read_plan = format!(
                    r#"{{"spanReadIndex":"storage_segment_rollup","usedSegmentRollup":{},"segmentRollupSegments":{},"segmentRollupRows":{},"tailFoldedSpanCount":{},"rollupFallbackReason":null}}"#,
                    read.stats.used_segment_rollup,
                    read.stats.segment_rollup_segments,
                    read.stats.segment_rollup_rows,
                    read.stats.tail_folded_span_count,
                );
                let response =
                    json_storage_stats_report_with_read_plan(&report, &group_by, &read_plan);
                return (
                    200,
                    self.read_model_cache_put("storage_stats", tenant, body, response),
                );
            }
        }

        let (snap, spans) =
            match self.filtered_spans_for_storage_projected(&v, tenant, storage_stats_projection())
            {
                Ok(v) => v,
                Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
            };
        let trace_ids: std::collections::HashSet<u64> = spans.iter().map(|s| s.trace_id).collect();
        let bounds = self.coord().trace_time_bounds(&snap, &trace_ids);
        let metadata = self.storage_metadata_for_tenant(tenant);
        let report = storage_stats_report(&spans, &bounds, &metadata, &group_by, time_bucket_ns);
        let fallback_reason = if blockers.is_empty() {
            "rollup_runtime_fallback"
        } else {
            "rollup_blocked"
        };
        let read_plan = format!(
            r#"{{"spanReadIndex":"folded_scan","usedSegmentRollup":false,"segmentRollupSegments":0,"segmentRollupRows":0,"tailFoldedSpanCount":0,"rollupFallbackReason":"{}"}}"#,
            fallback_reason,
        );
        let response = json_storage_stats_report_with_read_plan(&report, &group_by, &read_plan);
        (
            200,
            self.read_model_cache_put("storage_stats", tenant, body, response),
        )
    }

    pub(super) fn cluster_storage_stats_json(
        &self,
        body: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let cache_input = format!("{}|{}", self.read_model_revision(), body);
        if let Some(cached) =
            self.read_model_cache_get("cluster_storage_stats", tenant, &cache_input)
        {
            return (200, cached);
        }
        use crate::wire::{parse, Json};
        let v = match parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let read_set = match self.cluster_snapshot_read_set_from_body(&v) {
            Ok(read_set) => read_set,
            Err(resp) => return resp,
        };
        let group_by = storage_group_by_from_json(&v);
        let time_bucket_ns = json_field_alias(
            &v,
            &["time_bucket_ns", "timeBucketNs", "bucket_ns", "bucketNs"],
        )
        .and_then(Json::as_u64)
        .unwrap_or(86_400_000_000_000);

        let mut reports = Vec::new();
        for (idx, shard) in self.shards().iter().enumerate() {
            let snap = read_set.snapshot_at(idx);
            let spans = match self.filtered_spans_for_storage_for_coord_snapshot_projected(
                &shard.coord,
                snap,
                &v,
                tenant,
                storage_stats_projection(),
            ) {
                Ok(v) => v,
                Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
            };
            let trace_ids: std::collections::HashSet<u64> =
                spans.iter().map(|s| s.trace_id).collect();
            let bounds = shard.coord.trace_time_bounds(snap, &trace_ids);
            let metadata = self.storage_metadata_for_coord(&shard.coord, tenant);
            reports.push(storage_stats_report(
                &spans,
                &bounds,
                &metadata,
                &group_by,
                time_bucket_ns,
            ));
        }
        let report = merge_storage_stats_reports(reports);
        (
            200,
            self.read_model_cache_put(
                "cluster_storage_stats",
                tenant,
                &cache_input,
                json_storage_stats_report_with_cluster(
                    &report,
                    &group_by,
                    self.shards().len(),
                    &read_set.snapshot_field(),
                ),
            ),
        )
    }
}
