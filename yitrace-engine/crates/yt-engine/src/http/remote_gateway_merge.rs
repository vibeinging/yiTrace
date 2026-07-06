impl RemoteShardGateway {
    fn remote_trace_aggregate_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        let v = match crate::wire::parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, gateway_json_escape(&e))),
        };
        let policy = remote_consistency_from_json(&v);
        let remote_snapshot = match self.remote_snapshot_from_body(body) {
            Ok(snapshot) => snapshot,
            Err(resp) => return resp,
        };
        let fields = match trace_aggregate_group_fields(&v) {
            Ok(fields) => fields,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, gateway_json_escape(&e))),
        };
        let limit = json_field_alias(&v, &["limit", "k"])
            .and_then(crate::wire::Json::as_u64)
            .unwrap_or(50)
            .clamp(1, 500) as usize;
        let sort_by = json_field_alias(&v, &["sort_by", "sortBy", "sort"])
            .and_then(crate::wire::Json::as_str)
            .unwrap_or("spanCount")
            .to_string();
        let desc = json_field_alias(&v, &["order", "direction"])
            .and_then(crate::wire::Json::as_str)
            .map(|order| !order.eq_ignore_ascii_case("asc"))
            .unwrap_or(true);

        let mut merged = std::collections::BTreeMap::<String, RemoteAggregateBucket>::new();
        let mut span_total = 0usize;
        let mut failed = Vec::new();
        let mut ok_shards = 0usize;
        let mut shard_snapshots = Vec::new();
        let (read_targets, results) = match self.fanout_route_with_snapshot(
            "POST",
            "/v1/trace-aggregate",
            body,
            tenant,
            policy.strict,
            remote_snapshot.as_ref(),
        ) {
            Ok(result) => result,
            Err(resp) => return resp,
        };
        for (idx, result) in results {
            match result {
                Ok((200, response)) => {
                    ok_shards += 1;
                    if let Some(total) = remote_json_total_alias(&response, &["spanTotal"]) {
                        span_total += total;
                    }
                    if let Some(snapshot) = gateway_snapshot_from_body(&response) {
                        shard_snapshots.push((idx, snapshot));
                    }
                    for bucket in remote_aggregate_buckets_from_body(&response) {
                        merged
                            .entry(bucket.key_json.clone())
                            .or_insert_with(|| RemoteAggregateBucket::new(bucket.key_json.clone()))
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
        let mut buckets = merged.into_values().collect::<Vec<_>>();
        sort_remote_aggregate_buckets(&mut buckets, &sort_by, desc);
        buckets.truncate(limit);
        let items = buckets
            .iter()
            .map(|bucket| bucket.json(&fields))
            .collect::<Vec<_>>()
            .join(",");
        let snapshot_field = if failed.is_empty() && ok_shards == self.shard_count() {
            remote_gateway_snapshot_field(
                self.route_table_version(),
                &read_targets
                    .iter()
                    .map(|target| target.shard_id.clone())
                    .collect::<Vec<_>>(),
                &read_targets
                    .iter()
                    .map(|target| target.replica_id.clone())
                    .collect::<Vec<_>>(),
                self.shard_count(),
                shard_snapshots,
            )
        } else {
            String::new()
        };
        remote_items_response(
            items,
            buckets.len(),
            self.shard_count(),
            ok_shards,
            failed,
            policy,
            &read_targets,
            &format!(
                r#","spanTotal":{span_total},"aggregationIndex":"remote_fanout_reduce"{snapshot_field}"#
            ),
        )
    }

    fn remote_trajectory_groups_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        let v = match crate::wire::parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, gateway_json_escape(&e))),
        };
        let policy = remote_consistency_from_json(&v);
        let limit = json_field_alias(&v, &["limit", "k"])
            .and_then(crate::wire::Json::as_u64)
            .unwrap_or(50)
            .clamp(1, 500) as usize;
        let sort_by = json_field_alias(&v, &["sort_by", "sortBy", "sort"])
            .and_then(crate::wire::Json::as_str)
            .unwrap_or("best")
            .to_string();
        let desc = json_field_alias(&v, &["order", "direction"])
            .and_then(crate::wire::Json::as_str)
            .map(|order| !order.eq_ignore_ascii_case("asc"))
            .unwrap_or_else(|| trajectory_default_desc(&sort_by));
        let mut merged = std::collections::BTreeMap::<String, RemoteTrajectoryBucket>::new();
        let mut trace_total = 0usize;
        let mut span_total = 0usize;
        let mut failed = Vec::new();
        let mut ok_shards = 0usize;
        let (read_targets, results) = match self.fanout_read_route(
            "POST",
            "/v1/trajectory-groups",
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
                    trace_total += remote_json_total_alias(&response, &["traceTotal"]).unwrap_or(0);
                    span_total += remote_json_total_alias(&response, &["spanTotal"]).unwrap_or(0);
                    for bucket in remote_trajectory_buckets_from_body(&response) {
                        merged
                            .entry(bucket.signature.clone())
                            .or_insert_with(|| RemoteTrajectoryBucket::new(bucket.signature.clone()))
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
        let mut buckets = merged.into_values().collect::<Vec<_>>();
        sort_remote_trajectory_buckets(&mut buckets, &sort_by, desc);
        buckets.truncate(limit);
        let items = buckets
            .iter()
            .map(RemoteTrajectoryBucket::json)
            .collect::<Vec<_>>()
            .join(",");
        remote_items_response(
            items,
            buckets.len(),
            self.shard_count(),
            ok_shards,
            failed,
            policy,
            &read_targets,
            &format!(
                r#","traceTotal":{trace_total},"spanTotal":{span_total},"trajectoryIndex":"remote_fanout_materialized_reduce""#
            ),
        )
    }

    fn remote_trace_trajectories_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        let policy = remote_consistency_from_body(body);
        let request = match gateway_trace_search_request(body) {
            Ok(request) => request,
            Err(error) => return (400, format!(r#"{{"error":"{}"}}"#, gateway_json_escape(&error))),
        };
        let fetch_limit = request.cursor.saturating_add(request.limit).max(request.limit);
        let mut items = Vec::<RemoteTraceJsonItem>::new();
        let mut span_total = 0usize;
        let mut failed = Vec::new();
        let mut ok_shards = 0usize;
        let page_body = gateway_override_page_body(body, 0, fetch_limit)
            .unwrap_or_else(|_| body.to_string());
        let (read_targets, results) = match self.fanout_read_route(
            "POST",
            "/v1/trace-trajectories",
            &page_body,
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
                    span_total += remote_json_total_alias(&response, &["spanTotal"]).unwrap_or(0);
                    items.extend(remote_trace_items_from_body(&response, "traceId"));
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
        items.sort_by(|a, b| b.id.cmp(&a.id));
        items.dedup_by_key(|item| item.id);
        let total = items.len();
        let end = request.cursor.saturating_add(request.limit).min(total);
        let page = if request.cursor < total {
            &items[request.cursor..end]
        } else {
            &[][..]
        };
        let next = if end < total {
            end.to_string()
        } else {
            "null".to_string()
        };
        let body = page
            .iter()
            .map(|item| item.json.to_compact_json())
            .collect::<Vec<_>>()
            .join(",");
        remote_page_response(
            body,
            next,
            total,
            self.shard_count(),
            ok_shards,
            failed,
            policy,
            &read_targets,
            &format!(r#","spanTotal":{span_total},"index":"remote_fanout_materialized""#),
        )
    }

}

#[derive(Clone)]
struct RemoteTraceJsonItem {
    id: u64,
    json: crate::wire::Json,
}

fn remote_trace_items_from_body(body: &str, id_field: &str) -> Vec<RemoteTraceJsonItem> {
    let Ok(value) = crate::wire::parse(body) else {
        return Vec::new();
    };
    let Some(items) = crate::wire::field(&value, "items").map(crate::wire::Json::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let id = gateway_json_u64_alias(item, &[id_field])?;
            Some(RemoteTraceJsonItem {
                id,
                json: item.clone(),
            })
        })
        .collect()
}

fn remote_items_response(
    items: String,
    total: usize,
    shard_count: usize,
    ok_shards: usize,
    failed: Vec<String>,
    policy: RemoteConsistencyPolicy,
    read_targets: &[RemoteReadTarget],
    extra: &str,
) -> (u16, String) {
    (
        200,
        format!(
            r#"{{"items":[{}],"total":{},"queryMode":"process_gateway_fanout","shardCount":{},"okShards":{},"degraded":{},"failedShards":[{}]{},"readTargets":[{}]{} }}"#,
            items,
            total,
            shard_count,
            ok_shards,
            !failed.is_empty(),
            failed.join(","),
            policy.json_fields(),
            remote_read_targets_json(read_targets),
            extra
        )
        .replace(" }", "}"),
    )
}

fn remote_page_response(
    items: String,
    next: String,
    total: usize,
    shard_count: usize,
    ok_shards: usize,
    failed: Vec<String>,
    policy: RemoteConsistencyPolicy,
    read_targets: &[RemoteReadTarget],
    extra: &str,
) -> (u16, String) {
    (
        200,
        format!(
            r#"{{"items":[{}],"nextCursor":{},"total":{},"queryMode":"process_gateway_fanout","shardCount":{},"okShards":{},"degraded":{},"failedShards":[{}]{},"readTargets":[{}]{} }}"#,
            items,
            next,
            total,
            shard_count,
            ok_shards,
            !failed.is_empty(),
            failed.join(","),
            policy.json_fields(),
            remote_read_targets_json(read_targets),
            extra
        )
        .replace(" }", "}"),
    )
}

fn remote_all_shards_failed(shard_count: usize, failed: Vec<String>) -> (u16, String) {
    (
        503,
        format!(
            r#"{{"error":"all shards unavailable","queryMode":"process_gateway_fanout","shardCount":{},"okShards":0,"degraded":true,"failedShards":[{}],"consistencyUsed":"partial","partial":true}}"#,
            shard_count,
            failed.join(",")
        ),
    )
}

fn remote_failed_shard(idx: usize, status: u16, body: &str) -> String {
    format!(
        r#"{{"shard":{idx},"status":{status},"error":"shard query failed","body":"{}"}}"#,
        gateway_json_escape(body)
    )
}

fn remote_unreachable_shard(idx: usize, error: &str) -> String {
    format!(
        r#"{{"shard":{idx},"status":0,"error":"shard unreachable","detail":"{}"}}"#,
        gateway_json_escape(error)
    )
}

fn remote_json_total_alias(body: &str, names: &[&str]) -> Option<usize> {
    let value = crate::wire::parse(body).ok()?;
    for name in names {
        if let Some(v) = crate::wire::field(&value, name).and_then(crate::wire::Json::as_u64) {
            return Some(v as usize);
        }
    }
    None
}

#[derive(Clone, Default)]
struct RemoteAggregateBucket {
    key_json: String,
    span_count: usize,
    trace_count: usize,
    error_count: usize,
    duration_sum: u128,
    duration_max: u64,
    duration_count: usize,
    input_tokens: u64,
    output_tokens: u64,
    cached_input_tokens: u64,
    reasoning_tokens: u64,
    total_tokens: u64,
    cost_usd_nanos: u64,
    examples: Vec<String>,
}

impl RemoteAggregateBucket {
    fn new(key_json: String) -> Self {
        Self {
            key_json,
            ..Default::default()
        }
    }

    fn merge(&mut self, mut other: RemoteAggregateBucket) {
        self.span_count += other.span_count;
        self.trace_count += other.trace_count;
        self.error_count += other.error_count;
        self.duration_sum += other.duration_sum;
        self.duration_max = self.duration_max.max(other.duration_max);
        self.duration_count += other.duration_count;
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cached_input_tokens += other.cached_input_tokens;
        self.reasoning_tokens += other.reasoning_tokens;
        self.total_tokens += other.total_tokens;
        self.cost_usd_nanos += other.cost_usd_nanos;
        for example in other.examples.drain(..) {
            if self.examples.len() >= 3 {
                break;
            }
            self.examples.push(example);
        }
    }

    fn json(&self, _fields: &[TraceAggregateGroupField]) -> String {
        let avg = if self.duration_count == 0 {
            "null".to_string()
        } else {
            (self.duration_sum / self.duration_count as u128).to_string()
        };
        let max = if self.duration_count == 0 {
            "null".to_string()
        } else {
            self.duration_max.to_string()
        };
        let error_rate = if self.span_count == 0 {
            0.0
        } else {
            self.error_count as f64 / self.span_count as f64
        };
        format!(
            r#"{{"key":{},"spanCount":{},"traceCount":{},"errorCount":{},"errorRate":{:.6},"durationNs":{{"sum":{},"avg":{},"max":{},"p50":null,"p95":null,"count":{}}},"usage":{},"costUsd":{},"costDetail":{},"examples":[{}]}}"#,
            self.key_json,
            self.span_count,
            self.trace_count,
            self.error_count,
            error_rate,
            self.duration_sum,
            avg,
            max,
            self.duration_count,
            usage_json(
                self.input_tokens,
                self.output_tokens,
                self.cached_input_tokens,
                self.reasoning_tokens,
                self.total_tokens
            ),
            cost_usd_num_from_nanos(self.cost_usd_nanos),
            cost_detail_json(self.cost_usd_nanos, Some("USD"), "mixed"),
            self.examples.join(",")
        )
    }
}

fn remote_aggregate_buckets_from_body(body: &str) -> Vec<RemoteAggregateBucket> {
    let Ok(value) = crate::wire::parse(body) else {
        return Vec::new();
    };
    let Some(items) = crate::wire::field(&value, "items").map(crate::wire::Json::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .map(|item| {
            let key_json = crate::wire::field(item, "key")
                .map(crate::wire::Json::to_compact_json)
                .unwrap_or_else(|| "{}".to_string());
            let mut bucket = RemoteAggregateBucket::new(key_json);
            bucket.span_count = json_u64(item, "spanCount") as usize;
            bucket.trace_count = json_u64(item, "traceCount") as usize;
            bucket.error_count = json_u64(item, "errorCount") as usize;
            if let Some(duration) = crate::wire::field(item, "durationNs") {
                bucket.duration_sum = json_u64(duration, "sum") as u128;
                bucket.duration_max = json_u64(duration, "max");
                bucket.duration_count = json_u64(duration, "count") as usize;
            }
            if let Some(usage) = crate::wire::field(item, "usage") {
                bucket.input_tokens = json_u64(usage, "inputTokens");
                bucket.output_tokens = json_u64(usage, "outputTokens");
                bucket.cached_input_tokens = json_u64(usage, "cachedInputTokens");
                bucket.reasoning_tokens = json_u64(usage, "reasoningTokens");
                bucket.total_tokens = json_u64(usage, "totalTokens");
            }
            bucket.cost_usd_nanos = remote_cost_nanos(item);
            bucket.examples = json_array_items(item, "examples");
            bucket
        })
        .collect()
}

fn sort_remote_aggregate_buckets(
    buckets: &mut [RemoteAggregateBucket],
    sort_by: &str,
    desc: bool,
) {
    let sort = sort_by.to_ascii_lowercase().replace(['_', '-'], "");
    buckets.sort_by(|a, b| {
        let ord = match sort.as_str() {
            "tracecount" | "traces" => a.trace_count.cmp(&b.trace_count),
            "errorcount" | "errors" => a.error_count.cmp(&b.error_count),
            "duration" | "durationns" | "durationsum" => a.duration_sum.cmp(&b.duration_sum),
            "cost" | "costusd" => a.cost_usd_nanos.cmp(&b.cost_usd_nanos),
            "tokens" | "totaltokens" => a.total_tokens.cmp(&b.total_tokens),
            _ => a.span_count.cmp(&b.span_count),
        };
        let ord = if desc { ord.reverse() } else { ord };
        ord.then_with(|| a.key_json.cmp(&b.key_json))
    });
}

#[derive(Clone, Default)]
struct RemoteTrajectoryBucket {
    signature: String,
    steps_json: String,
    step_count: usize,
    trace_count: usize,
    span_count: usize,
    success_count: usize,
    error_trace_count: usize,
    error_span_count: usize,
    duration_sum: u128,
    duration_max: u64,
    duration_count: usize,
    input_tokens: u64,
    output_tokens: u64,
    cached_input_tokens: u64,
    reasoning_tokens: u64,
    total_tokens: u64,
    cost_usd_nanos: u64,
    quality_score_sum: u64,
    quality_score_count: usize,
    examples: Vec<String>,
}

impl RemoteTrajectoryBucket {
    fn new(signature: String) -> Self {
        Self {
            signature,
            ..Default::default()
        }
    }

    fn merge(&mut self, mut other: RemoteTrajectoryBucket) {
        if self.steps_json.is_empty() {
            self.steps_json = other.steps_json.clone();
            self.step_count = other.step_count;
        }
        self.trace_count += other.trace_count;
        self.span_count += other.span_count;
        self.success_count += other.success_count;
        self.error_trace_count += other.error_trace_count;
        self.error_span_count += other.error_span_count;
        self.duration_sum += other.duration_sum;
        self.duration_max = self.duration_max.max(other.duration_max);
        self.duration_count += other.duration_count;
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cached_input_tokens += other.cached_input_tokens;
        self.reasoning_tokens += other.reasoning_tokens;
        self.total_tokens += other.total_tokens;
        self.cost_usd_nanos += other.cost_usd_nanos;
        self.quality_score_sum += other.quality_score_sum;
        self.quality_score_count += other.quality_score_count;
        for example in other.examples.drain(..) {
            if self.examples.len() >= 3 {
                break;
            }
            self.examples.push(example);
        }
    }

    fn quality_score(&self) -> u32 {
        if self.quality_score_count == 0 {
            0
        } else {
            (self.quality_score_sum / self.quality_score_count as u64) as u32
        }
    }

    fn json(&self) -> String {
        let success_rate = ratio_f64(self.success_count, self.trace_count);
        let error_rate = ratio_f64(self.error_trace_count, self.trace_count);
        let avg = if self.duration_count == 0 {
            "null".to_string()
        } else {
            (self.duration_sum / self.duration_count as u128).to_string()
        };
        let max = if self.duration_count == 0 {
            "null".to_string()
        } else {
            self.duration_max.to_string()
        };
        let steps = if self.steps_json.is_empty() {
            "[]".to_string()
        } else {
            self.steps_json.clone()
        };
        format!(
            r#"{{"signature":"{}","stepCount":{},"steps":{},"traceCount":{},"spanCount":{},"successCount":{},"errorTraceCount":{},"errorSpanCount":{},"successRate":{:.6},"errorRate":{:.6},"qualityScore":{},"durationNs":{{"sum":{},"avg":{},"max":{},"p50":null,"p95":null,"count":{}}},"usage":{},"costUsd":{},"costDetail":{},"scores":{{"eval":{{"count":0,"avg":null,"min":null,"max":null}},"annotation":{{"count":0,"avg":null,"min":null,"max":null}},"dataset":{{"count":0,"avg":null,"min":null,"max":null}}}},"examples":[{}]}}"#,
            gateway_json_escape(&self.signature),
            self.step_count,
            steps,
            self.trace_count,
            self.span_count,
            self.success_count,
            self.error_trace_count,
            self.error_span_count,
            success_rate,
            error_rate,
            self.quality_score(),
            self.duration_sum,
            avg,
            max,
            self.duration_count,
            usage_json(
                self.input_tokens,
                self.output_tokens,
                self.cached_input_tokens,
                self.reasoning_tokens,
                self.total_tokens
            ),
            cost_usd_num_from_nanos(self.cost_usd_nanos),
            cost_detail_json(self.cost_usd_nanos, Some("USD"), "mixed"),
            self.examples.join(",")
        )
    }
}

fn remote_trajectory_buckets_from_body(body: &str) -> Vec<RemoteTrajectoryBucket> {
    let Ok(value) = crate::wire::parse(body) else {
        return Vec::new();
    };
    let Some(items) = crate::wire::field(&value, "items").map(crate::wire::Json::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .map(|item| {
            let signature = crate::wire::field(item, "signature")
                .and_then(crate::wire::Json::as_str)
                .unwrap_or("")
                .to_string();
            let mut bucket = RemoteTrajectoryBucket::new(signature);
            bucket.steps_json = crate::wire::field(item, "steps")
                .map(crate::wire::Json::to_compact_json)
                .unwrap_or_else(|| "[]".to_string());
            bucket.step_count = json_u64(item, "stepCount") as usize;
            bucket.trace_count = json_u64(item, "traceCount") as usize;
            bucket.span_count = json_u64(item, "spanCount") as usize;
            bucket.success_count = json_u64(item, "successCount") as usize;
            bucket.error_trace_count = json_u64(item, "errorTraceCount") as usize;
            bucket.error_span_count = json_u64(item, "errorSpanCount") as usize;
            bucket.quality_score_sum = json_u64(item, "qualityScore");
            bucket.quality_score_count = usize::from(bucket.quality_score_sum > 0);
            if let Some(duration) = crate::wire::field(item, "durationNs") {
                bucket.duration_sum = json_u64(duration, "sum") as u128;
                bucket.duration_max = json_u64(duration, "max");
                bucket.duration_count = json_u64(duration, "count") as usize;
            }
            if let Some(usage) = crate::wire::field(item, "usage") {
                bucket.input_tokens = json_u64(usage, "inputTokens");
                bucket.output_tokens = json_u64(usage, "outputTokens");
                bucket.cached_input_tokens = json_u64(usage, "cachedInputTokens");
                bucket.reasoning_tokens = json_u64(usage, "reasoningTokens");
                bucket.total_tokens = json_u64(usage, "totalTokens");
            }
            bucket.cost_usd_nanos = remote_cost_nanos(item);
            bucket.examples = json_array_items(item, "examples");
            bucket
        })
        .filter(|bucket| !bucket.signature.is_empty())
        .collect()
}

fn sort_remote_trajectory_buckets(
    buckets: &mut [RemoteTrajectoryBucket],
    sort_by: &str,
    desc: bool,
) {
    let sort = sort_by.to_ascii_lowercase().replace(['_', '-'], "");
    buckets.sort_by(|a, b| {
        let ord = match sort.as_str() {
            "tracecount" | "traces" | "count" => a.trace_count.cmp(&b.trace_count),
            "spancount" | "spans" => a.span_count.cmp(&b.span_count),
            "errorcount" | "errors" => a.error_trace_count.cmp(&b.error_trace_count),
            "successrate" | "success" => {
                (a.success_count as u128 * b.trace_count.max(1) as u128)
                    .cmp(&(b.success_count as u128 * a.trace_count.max(1) as u128))
            }
            "duration" | "durationns" => a.duration_sum.cmp(&b.duration_sum),
            "cost" | "avgcost" => a.cost_usd_nanos.cmp(&b.cost_usd_nanos),
            "tokens" | "totaltokens" => a.total_tokens.cmp(&b.total_tokens),
            _ => a.quality_score().cmp(&b.quality_score()),
        };
        let ord = if desc { ord.reverse() } else { ord };
        ord.then_with(|| a.signature.cmp(&b.signature))
    });
}
