use super::*;

impl EngineJsonApi {
    pub(super) fn storage_stats_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
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

        let (snap, spans) = match self.filtered_spans_for_storage(&v, tenant) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let trace_ids: std::collections::HashSet<u64> = spans.iter().map(|s| s.trace_id).collect();
        let bounds = self.coord().trace_time_bounds(&snap, &trace_ids);
        let metadata = self.storage_metadata_for_tenant(tenant);
        let report = storage_stats_report(&spans, &bounds, &metadata, &group_by, time_bucket_ns);
        (200, json_storage_stats_report(&report, &group_by))
    }

    pub(super) fn cluster_storage_stats_json(
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
            let spans = match self.filtered_spans_for_storage_for_coord_snapshot(
                &shard.coord,
                snap,
                &v,
                tenant,
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
            json_storage_stats_report_with_cluster(
                &report,
                &group_by,
                self.shards().len(),
                &read_set.snapshot_field(),
            ),
        )
    }
}
