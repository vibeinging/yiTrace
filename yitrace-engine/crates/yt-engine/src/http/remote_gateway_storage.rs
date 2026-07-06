impl RemoteShardGateway {
    fn remote_storage_stats_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        let v = match crate::wire::parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, gateway_json_escape(&e))),
        };
        let policy = remote_consistency_from_json(&v);
        let group_by = storage_group_by_from_json(&v);
        let mut total = RemoteStorageBucket::default();
        let mut groups = std::collections::BTreeMap::<String, RemoteStorageBucket>::new();
        let mut failed = Vec::new();
        let mut ok_shards = 0usize;
        let (read_targets, results) = match self.fanout_read_route(
            "POST",
            "/v1/storage-stats",
            body,
            tenant,
            policy.strict,
        ) {
            Ok(result) => result,
            Err(resp) => return resp,
        };
        for (idx, result) in results {
            match result {
                Ok((200, response)) => {
                    ok_shards += 1;
                    if let Some(bucket) = remote_storage_total_from_body(&response) {
                        total.merge(bucket);
                    }
                    for bucket in remote_storage_groups_from_body(&response) {
                        groups
                            .entry(bucket.key_json.clone())
                            .or_insert_with(|| RemoteStorageBucket::with_key(bucket.key_json.clone()))
                            .merge(bucket);
                    }
                }
                Ok((status, response)) => failed.push(remote_failed_shard(idx, status, &response)),
                Err(error) => failed.push(remote_unreachable_shard(idx, &error)),
            }
        }
        if let Some(resp) = policy.reject_degraded(self.shard_count(), ok_shards, &failed) {
            return resp;
        }
        if ok_shards == 0 {
            return remote_all_shards_failed(self.shard_count(), failed);
        }
        let group_json = groups
            .into_values()
            .map(|bucket| bucket.json(true))
            .collect::<Vec<_>>()
            .join(",");
        (
            200,
            format!(
                r#"{{"groupBy":{},"total":{},"groups":[{}],"queryMode":"process_gateway_fanout","shardCount":{},"okShards":{},"degraded":{},"failedShards":[{}]{},"readTargets":[{}],"storageIndex":"remote_fanout_reduce"}}"#,
                json_string_array(&group_by),
                total.json(false),
                group_json,
                self.shard_count(),
                ok_shards,
                !failed.is_empty(),
                failed.join(","),
                policy.json_fields(),
                remote_read_targets_json(&read_targets)
            ),
        )
    }

    fn remote_retention_fanout_json(
        &self,
        method: &str,
        path: &str,
        body: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let mut shards = Vec::new();
        let mut failed = Vec::new();
        let mut ok_shards = 0usize;
        for (idx, result) in self.fanout_route(method, path, body, tenant) {
            match result {
                Ok((200, response)) => {
                    ok_shards += 1;
                    shards.push(format!(r#"{{"shard":{idx},"ok":true,"result":{response}}}"#));
                }
                Ok((status, response)) => failed.push(remote_failed_shard(idx, status, &response)),
                Err(error) => failed.push(remote_unreachable_shard(idx, &error)),
            }
        }
        let status = if ok_shards == 0 { 503 } else { 200 };
        (
            status,
            format!(
                r#"{{"queryMode":"process_gateway_fanout","shardCount":{},"okShards":{},"degraded":{},"partialSuccess":{},"failedShards":[{}],"shards":[{}],"retrySafe":"idempotent_retention_policy_or_segment_delete"}}"#,
                self.shard_count(),
                ok_shards,
                !failed.is_empty(),
                ok_shards > 0 && !failed.is_empty(),
                failed.join(","),
                shards.join(",")
            ),
        )
    }
}

#[derive(Clone, Default)]
struct RemoteStorageBucket {
    key_json: String,
    trace_count: usize,
    span_count: usize,
    session_count: usize,
    event_count: usize,
    error_span_count: usize,
    first_ts: Option<i64>,
    last_ts: Option<i64>,
    input_text_bytes: u64,
    output_text_bytes: u64,
    log_bytes: u64,
    attr_bytes: u64,
    external_id_bytes: u64,
    field_bytes: u64,
    estimated_bytes: u64,
    annotations: usize,
    dataset_associations: usize,
    golden_paths: usize,
    snapshot_refs: usize,
    eval_links: usize,
    path_memory_refs: usize,
}

impl RemoteStorageBucket {
    fn with_key(key_json: String) -> Self {
        Self {
            key_json,
            ..Default::default()
        }
    }

    fn merge(&mut self, other: RemoteStorageBucket) {
        if self.key_json.is_empty() {
            self.key_json = other.key_json;
        }
        self.trace_count += other.trace_count;
        self.span_count += other.span_count;
        self.session_count += other.session_count;
        self.event_count += other.event_count;
        self.error_span_count += other.error_span_count;
        self.first_ts = match (self.first_ts, other.first_ts) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (None, Some(b)) => Some(b),
            (a, None) => a,
        };
        self.last_ts = match (self.last_ts, other.last_ts) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (None, Some(b)) => Some(b),
            (a, None) => a,
        };
        self.input_text_bytes += other.input_text_bytes;
        self.output_text_bytes += other.output_text_bytes;
        self.log_bytes += other.log_bytes;
        self.attr_bytes += other.attr_bytes;
        self.external_id_bytes += other.external_id_bytes;
        self.field_bytes += other.field_bytes;
        self.estimated_bytes += other.estimated_bytes;
        self.annotations += other.annotations;
        self.dataset_associations += other.dataset_associations;
        self.golden_paths += other.golden_paths;
        self.snapshot_refs += other.snapshot_refs;
        self.eval_links += other.eval_links;
        self.path_memory_refs += other.path_memory_refs;
    }

    fn json(&self, include_key: bool) -> String {
        let key = if include_key {
            format!(r#""key":{},"#, self.key_json)
        } else {
            String::new()
        };
        let payload = self.input_text_bytes + self.output_text_bytes + self.log_bytes;
        format!(
            r#"{{{}"traceCount":{},"spanCount":{},"sessionCount":{},"eventCount":{},"errorSpanCount":{},"firstTs":{},"lastTs":{},"bytes":{{"inputText":{},"outputText":{},"logs":{},"payload":{},"attrs":{},"externalIds":{},"fields":{},"estimated":{},"estimatedBytes":{}}},"metadata":{{"annotations":{},"datasetAssociations":{},"goldenPaths":{},"snapshotRefs":{},"evalLinks":{},"pathMemoryRefs":{}}}}}"#,
            key,
            self.trace_count,
            self.span_count,
            self.session_count,
            self.event_count,
            self.error_span_count,
            opt_i64_json(self.first_ts),
            opt_i64_json(self.last_ts),
            self.input_text_bytes,
            self.output_text_bytes,
            self.log_bytes,
            payload,
            self.attr_bytes,
            self.external_id_bytes,
            self.field_bytes,
            self.estimated_bytes,
            self.estimated_bytes,
            self.annotations,
            self.dataset_associations,
            self.golden_paths,
            self.snapshot_refs,
            self.eval_links,
            self.path_memory_refs,
        )
    }
}

fn remote_storage_total_from_body(body: &str) -> Option<RemoteStorageBucket> {
    let value = crate::wire::parse(body).ok()?;
    crate::wire::field(&value, "total").map(|v| remote_storage_bucket_from_json(v, None))
}

fn remote_storage_groups_from_body(body: &str) -> Vec<RemoteStorageBucket> {
    let Ok(value) = crate::wire::parse(body) else {
        return Vec::new();
    };
    let Some(groups) = crate::wire::field(&value, "groups").map(crate::wire::Json::as_array)
    else {
        return Vec::new();
    };
    groups
        .iter()
        .map(|item| {
            let key = crate::wire::field(item, "key")
                .map(crate::wire::Json::to_compact_json)
                .unwrap_or_else(|| "{}".to_string());
            remote_storage_bucket_from_json(item, Some(key))
        })
        .collect()
}

fn remote_storage_bucket_from_json(
    item: &crate::wire::Json,
    key_json: Option<String>,
) -> RemoteStorageBucket {
    let bytes = crate::wire::field(item, "bytes");
    let metadata = crate::wire::field(item, "metadata");
    RemoteStorageBucket {
        key_json: key_json.unwrap_or_default(),
        trace_count: json_u64(item, "traceCount") as usize,
        span_count: json_u64(item, "spanCount") as usize,
        session_count: json_u64(item, "sessionCount") as usize,
        event_count: json_u64(item, "eventCount") as usize,
        error_span_count: json_u64(item, "errorSpanCount") as usize,
        first_ts: json_i64(item, "firstTs"),
        last_ts: json_i64(item, "lastTs"),
        input_text_bytes: bytes.map_or(0, |v| json_u64(v, "inputText")),
        output_text_bytes: bytes.map_or(0, |v| json_u64(v, "outputText")),
        log_bytes: bytes.map_or(0, |v| json_u64(v, "logs")),
        attr_bytes: bytes.map_or(0, |v| json_u64(v, "attrs")),
        external_id_bytes: bytes.map_or(0, |v| json_u64(v, "externalIds")),
        field_bytes: bytes.map_or(0, |v| json_u64(v, "fields")),
        estimated_bytes: bytes
            .map_or(0, |v| json_u64(v, "estimatedBytes").max(json_u64(v, "estimated"))),
        annotations: metadata.map_or(0, |v| json_u64(v, "annotations") as usize),
        dataset_associations: metadata.map_or(0, |v| json_u64(v, "datasetAssociations") as usize),
        golden_paths: metadata.map_or(0, |v| json_u64(v, "goldenPaths") as usize),
        snapshot_refs: metadata.map_or(0, |v| json_u64(v, "snapshotRefs") as usize),
        eval_links: metadata.map_or(0, |v| json_u64(v, "evalLinks") as usize),
        path_memory_refs: metadata.map_or(0, |v| json_u64(v, "pathMemoryRefs") as usize),
    }
}
