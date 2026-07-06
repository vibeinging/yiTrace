impl RemoteShardGateway {
    pub fn route_with_tenant(
        &self,
        method: &str,
        path: &str,
        body: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let (base, _) = path.split_once('?').unwrap_or((path, ""));
        match (method, base) {
            ("GET", "/v1/cluster") | ("GET", "/v1/cluster/shards") | ("GET", "/v1/shards") => {
                (200, self.cluster_json())
            }
            ("GET", "/v1/cluster/health") | ("GET", "/v1/cluster/heartbeat") => {
                (200, self.cluster_health_json())
            }
            ("POST", "/v1/cluster/health/refresh") | ("POST", "/v1/cluster/heartbeat") => {
                (200, self.refresh_cluster_health_json())
            }
            ("POST", "/v1/cluster/route-table/reload")
            | ("POST", "/v1/route-table/reload") => match self.reload_route_table_json(body) {
                Ok(version) => (
                    200,
                    format!(r#"{{"ok":true,"routeTableVersion":{version}}}"#),
                ),
                Err(error) => (
                    400,
                    format!(r#"{{"error":"{}"}}"#, gateway_json_escape(&error)),
                ),
            },
            ("POST", "/v1/ingest") => self.ingest_json(body, tenant),
            ("POST", "/v1/snapshots/lease") => self.remote_snapshot_lease_json(body, tenant),
            ("POST", "/v1/snapshots/renew") => self.remote_snapshot_renew_json(body, tenant),
            ("POST", "/v1/vector-index") => self.vector_index_json(body, tenant),
            ("POST", "/v1/vector-search") => self.vector_search_json(body, tenant),
            ("POST", "/v1/trace-search") => self.trace_search_json(body, tenant),
            ("POST", "/v1/search") => self.search_json(body, tenant),
            ("POST", "/v1/trace-aggregate") | ("POST", "/v1/trace-aggregates") => {
                self.remote_trace_aggregate_json(body, tenant)
            }
            ("POST", "/v1/trajectory-groups")
            | ("POST", "/v1/trajectory-aggregate")
            | ("POST", "/v1/best-paths") => self.remote_trajectory_groups_json(body, tenant),
            ("POST", "/v1/trace-trajectories") | ("POST", "/v1/trajectories") => {
                self.remote_trace_trajectories_json(body, tenant)
            }
            ("POST", "/v1/storage-stats") | ("POST", "/v1/storage/stats") => {
                self.remote_storage_stats_json(body, tenant)
            }
            ("POST", "/v1/retention-plan")
            | ("POST", "/v1/retention/plan")
            | ("POST", "/v1/retention/apply")
            | ("POST", "/v1/retention-policies/run-due")
            | ("POST", "/v1/retention/policies/run-due")
            | ("POST", "/v1/retention/run-due") => {
                self.remote_retention_fanout_json(method, base, body, tenant)
            }
            ("POST", "/v1/annotations") => {
                self.remote_create_trace_metadata_json(base, body, tenant, "annotationId")
            }
            ("POST", "/v1/dataset-associations") | ("POST", "/v1/dataset-links") => {
                self.remote_create_trace_metadata_json(base, body, tenant, "associationId")
            }
            ("POST", "/v1/golden-paths") => {
                self.remote_create_trace_metadata_json(base, body, tenant, "goldenPathId")
            }
            ("GET", "/v1/annotations") => {
                self.remote_metadata_items_json(method, path, body, tenant, "annotationId")
            }
            ("GET", "/v1/dataset-associations")
            | ("GET", "/v1/dataset-links")
            | ("GET", "/v1/golden-paths")
            | ("GET", "/v1/retention-audits")
            | ("POST", "/v1/retention-audits")
            | ("GET", "/v1/retention-policies") => {
                self.remote_metadata_items_json(method, path, body, tenant, "")
            }
            ("POST", "/v1/retention-policies") => {
                self.remote_retention_policy_create_json(body, tenant)
            }
            ("POST", "/v1/golden-path-export") | ("POST", "/v1/golden-paths/export") => {
                self.remote_golden_path_export_json(body, tenant)
            }
            ("POST", "/v1/golden-path-health") | ("POST", "/v1/golden-paths/health") => {
                self.remote_golden_path_owner_route_json(base, body, tenant)
            }
            ("POST", "/v1/path-adherence")
            | ("POST", "/v1/golden-path-adherence")
            | ("POST", "/v1/golden-path-evidence")
            | ("POST", "/v1/golden-paths/evidence") => {
                self.remote_golden_path_owner_route_json(base, body, tenant)
            }
            _ => self.remote_dynamic_route_json(method, path, body, tenant),
        }
    }

    fn ingest_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        let shards = self.shards_snapshot();
        let records = match crate::parse_wire_batch(body) {
            Ok(records) => records,
            Err(e) => {
                return (
                    400,
                    format!(
                        r#"{{"error":"{}"}}"#,
                        gateway_json_escape(&e.replace('"', "'"))
                    ),
                )
            }
        };
        let mut grouped: Vec<Vec<crate::WireRecord>> =
            (0..shards.len()).map(|_| Vec::new()).collect();
        for record in records {
            let idx = self.shard_index_for_record(&record, tenant, shards.len());
            self.router.remember_owner(
                tenant,
                record.trace_id,
                record.session_id,
                idx,
                shards.len(),
            );
            grouped[idx].push(record);
        }

        let mut ingested = 0usize;
        let mut failed = Vec::new();
        let mut ok_shards = 0usize;
        let mut handles = Vec::new();
        for (idx, batch) in grouped.into_iter().enumerate() {
            if batch.is_empty() {
                continue;
            }
            let count = batch.len();
            let shard = shards[idx].clone();
            handles.push(std::thread::spawn(move || {
                let result = shard.ingest_records_for_tenant(batch, tenant);
                (idx, count, result)
            }));
        }
        for handle in handles {
            let (idx, count, result) = match handle.join() {
                Ok(result) => result,
                Err(_) => {
                    failed.push(r#"{"shard":0,"status":0,"error":"shard ingest panicked"}"#.to_string());
                    continue;
                }
            };
            if let Err(error) = result {
                failed.push(format!(
                    r#"{{"shard":{idx},"status":0,"error":"shard ingest failed","detail":"{}","attempted":{count}}}"#,
                    gateway_json_escape(&error)
                ));
                continue;
            }
            ok_shards += 1;
            ingested += count;
        }

        if !failed.is_empty() {
            let status = if ok_shards == 0 { 503 } else { 502 };
            return (
                status,
                format!(
                    r#"{{"error":"shard ingest failed","partialSuccess":{},"ingested":{ingested},"queryMode":"process_gateway_route","shardCount":{},"okShards":{ok_shards},"degraded":true,"failedShards":[{}],"retrySafe":"event_id_dedup"}}"#,
                    ok_shards > 0,
                    shards.len(),
                    failed.join(",")
                ),
            );
        }
        (
            200,
            format!(
                r#"{{"ingested":{ingested},"queryMode":"process_gateway_route","shardCount":{}}}"#,
                shards.len()
            ),
        )
    }

    fn search_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        let request = match gateway_search_request(body) {
            Ok(request) => request,
            Err(error) => {
                return (
                    400,
                    format!(r#"{{"error":"{}"}}"#, gateway_json_escape(&error)),
                )
            }
        };
        let (status, envelope) = self.fanout_search_items_json(body, tenant, request.k);
        if status != 200 || request.include_fanout || envelope.contains(r#""degraded":true"#) {
            return (status, envelope);
        }
        let items = json_items_from_body(&envelope, "items").unwrap_or_default();
        (200, format!("[{}]", items.join(",")))
    }

    fn trace_search_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        let policy = remote_consistency_from_body(body);
        let request = match gateway_trace_search_request(body) {
            Ok(request) => request,
            Err(error) => {
                return (
                    400,
                    format!(r#"{{"error":"{}"}}"#, gateway_json_escape(&error)),
                )
            }
        };
        let remote_snapshot = match self.remote_snapshot_from_body(body) {
            Ok(snapshot) => snapshot,
            Err(resp) => return resp,
        };
        let fetch_limit = request.cursor.saturating_add(request.limit).max(request.limit);
        let (read_targets, pages) = match self.fanout_trace_search_pages(
            body,
            tenant,
            fetch_limit,
            policy.strict,
            remote_snapshot.as_ref(),
        ) {
            Ok(result) => result,
            Err(resp) => return resp,
        };
        let mut items = Vec::<GatewayTraceItem>::new();
        let mut total = 0usize;
        let mut failed = Vec::new();
        let mut ok_shards = 0usize;
        let mut shard_snapshots = Vec::new();
        for (idx, result) in pages {
            match result {
                Ok(page) => {
                    ok_shards += 1;
                    total += page.total;
                    if let Some(snapshot) = page.snapshot {
                        shard_snapshots.push((idx, snapshot));
                    }
                    items.extend(page.items);
                }
                Err(GatewayShardQueryError { status, detail }) if status != 0 => failed.push(format!(
                    r#"{{"shard":{idx},"status":{status},"error":"shard query failed","body":"{}"}}"#,
                    gateway_json_escape(&detail)
                )),
                Err(GatewayShardQueryError { detail, .. }) => failed.push(format!(
                    r#"{{"shard":{idx},"status":0,"error":"shard unreachable","detail":"{}"}}"#,
                    gateway_json_escape(&detail)
                )),
            }
        }
        if let Some(resp) = policy.reject_degraded(self.shard_count(), ok_shards, &failed) {
            return resp;
        }
        if ok_shards == 0 {
            return (
                503,
                format!(
                    r#"{{"error":"all shards unavailable","queryMode":"process_gateway_fanout","shardCount":{},"okShards":0,"degraded":true,"failedShards":[{}],"consistencyUsed":"partial","partial":true}}"#,
                    self.shard_count(),
                    failed.join(",")
                ),
            );
        }

        sort_gateway_trace_items(&mut items, &request.sort_by, request.desc);
        let end = request.cursor.saturating_add(request.limit).min(total);
        let page = if request.cursor < total {
            &items[request.cursor..end]
        } else {
            &[][..]
        };
        let merged: Vec<String> = page
            .iter()
            .enumerate()
            .map(|(idx, item)| item.with_rank(request.cursor + idx))
            .collect();
        let next = if end < total {
            end.to_string()
        } else {
            "null".to_string()
        };
        let degraded = !failed.is_empty();
        let snapshot_field = if !degraded && ok_shards == self.shard_count() {
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
        (
            200,
            format!(
                r#"{{"items":[{}],"nextCursor":{next},"total":{total},"queryMode":"process_gateway_fanout","shardCount":{},"okShards":{ok_shards},"degraded":{degraded},"failedShards":[{}]{},"readTargets":[{}]{} }}"#,
                merged.join(","),
                self.shard_count(),
                failed.join(","),
                policy.json_fields(),
                remote_read_targets_json(&read_targets),
                snapshot_field,
            )
            .replace(" }", "}"),
        )
    }

    fn fanout_search_items_json(&self, body: &str, tenant: Option<u64>, k: usize) -> (u16, String) {
        let policy = remote_consistency_from_body(body);
        let (read_targets, results) =
            match self.fanout_read_route("POST", "/v1/search", body, tenant, policy.strict) {
                Ok(result) => result,
                Err(resp) => return resp,
            };
        let mut items = Vec::<GatewaySearchItem>::new();
        let mut failed = Vec::new();
        let mut ok_shards = 0usize;
        for (idx, result) in results {
            match result {
                Ok((200, response)) => {
                    ok_shards += 1;
                    if let Some(shard_items) = gateway_search_items_from_body(&response) {
                        items.extend(shard_items);
                    }
                }
                Ok((status, response)) => failed.push(format!(
                    r#"{{"shard":{idx},"status":{status},"error":"shard query failed","body":"{}"}}"#,
                    gateway_json_escape(&response)
                )),
                Err(error) => failed.push(format!(
                    r#"{{"shard":{idx},"status":0,"error":"shard unreachable","detail":"{}"}}"#,
                    gateway_json_escape(&error)
                )),
            }
        }
        if let Some(resp) = policy.reject_degraded(self.shard_count(), ok_shards, &failed) {
            return resp;
        }
        if ok_shards == 0 {
            return (
                503,
                format!(
                    r#"{{"error":"all shards unavailable","queryMode":"process_gateway_fanout","shardCount":{},"okShards":0,"degraded":true,"failedShards":[{}],"consistencyUsed":"partial","partial":true}}"#,
                    self.shard_count(),
                    failed.join(",")
                ),
            );
        }
        items.sort_by(|left, right| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.trace_id.cmp(&right.trace_id))
                .then_with(|| left.span_id.cmp(&right.span_id))
        });
        let mut seen = std::collections::HashSet::new();
        items.retain(|item| seen.insert((item.trace_id, item.span_id)));
        items.truncate(k);
        let merged: Vec<String> = items.into_iter().map(|item| item.json).collect();
        let degraded = !failed.is_empty();
        (
            200,
            format!(
                r#"{{"items":[{}],"total":{},"queryMode":"process_gateway_fanout","shardCount":{},"okShards":{ok_shards},"degraded":{degraded},"failedShards":[{}]{},"readTargets":[{}] }}"#,
                merged.join(","),
                merged.len(),
                self.shard_count(),
                failed.join(","),
                policy.json_fields(),
                remote_read_targets_json(&read_targets)
            )
            .replace(" }", "}"),
        )
    }

    fn fanout_read_route(
        &self,
        method: &str,
        path: &str,
        body: &str,
        tenant: Option<u64>,
        force_leader: bool,
    ) -> Result<
        (
            Vec<RemoteReadTarget>,
            Vec<(usize, Result<(u16, String), String>)>,
        ),
        (u16, String),
    > {
        let targets = self.read_targets_snapshot(force_leader, None)?;
        let results = fanout_read_targets_route(&targets, method, path, body, tenant);
        Ok((targets, results))
    }

    fn fanout_route(
        &self,
        method: &str,
        path: &str,
        body: &str,
        tenant: Option<u64>,
    ) -> Vec<(usize, Result<(u16, String), String>)> {
        let mut handles = Vec::new();
        for (idx, shard) in self.shards_snapshot().into_iter().enumerate() {
            let method = method.to_string();
            let path = path.to_string();
            let body = body.to_string();
            handles.push(std::thread::spawn(move || {
                let result = shard.route_json_with_tenant(&method, &path, &body, tenant);
                (idx, result)
            }));
        }
        handles
            .into_iter()
            .map(|handle| {
                handle.join().unwrap_or_else(|_| {
                    (
                        0,
                        Err("remote shard fanout worker panicked".to_string()),
                    )
                })
            })
            .collect()
    }

    fn shard_index_for_record(
        &self,
        record: &crate::WireRecord,
        tenant: Option<u64>,
        shard_count: usize,
    ) -> usize {
        self.router.shard_index_for_record(
            tenant,
            record.session_id,
            record.trace_id,
            shard_count,
        )
    }
}

fn fanout_read_targets_route(
    targets: &[RemoteReadTarget],
    method: &str,
    path: &str,
    body: &str,
    tenant: Option<u64>,
) -> Vec<(usize, Result<(u16, String), String>)> {
    let mut handles = Vec::new();
    for target in targets.iter().cloned() {
        let method = method.to_string();
        let path = path.to_string();
        let body = body.to_string();
        handles.push(std::thread::spawn(move || {
            let result = target
                .client
                .route_json_with_tenant(&method, &path, &body, tenant);
            (target.index, result)
        }));
    }
    handles
        .into_iter()
        .map(|handle| {
            handle
                .join()
                .unwrap_or_else(|_| (0, Err("remote shard fanout worker panicked".to_string())))
        })
        .collect()
}

fn json_items_from_body(body: &str, field: &str) -> Option<Vec<String>> {
    let value = crate::wire::parse(body).ok()?;
    match &value {
        crate::wire::Json::Arr(items) => {
            Some(items.iter().map(crate::wire::Json::to_compact_json).collect())
        }
        crate::wire::Json::Obj(_) => {
            let items = crate::wire::field(&value, field)?.as_array();
            Some(
                items
                    .iter()
                    .map(crate::wire::Json::to_compact_json)
                    .collect(),
            )
        }
        _ => None,
    }
}

#[derive(Clone)]
struct GatewaySearchRequest {
    k: usize,
    include_fanout: bool,
}

fn gateway_search_request(body: &str) -> Result<GatewaySearchRequest, String> {
    let value = crate::wire::parse(body)?;
    Ok(GatewaySearchRequest {
        k: crate::wire::field(&value, "k")
            .and_then(crate::wire::Json::as_u64)
            .unwrap_or(10) as usize,
        include_fanout: json_bool_alias(&value, &["includeFanout", "include_fanout"])
            .unwrap_or(false),
    })
}

#[derive(Clone)]
struct GatewayTraceSearchRequest {
    cursor: usize,
    limit: usize,
    sort_by: String,
    desc: bool,
}

fn gateway_trace_search_request(body: &str) -> Result<GatewayTraceSearchRequest, String> {
    let value = crate::wire::parse(body)?;
    let cursor = json_field_alias(&value, &["cursor", "offset"])
        .and_then(crate::wire::Json::as_u64)
        .unwrap_or(0) as usize;
    let limit = json_field_alias(&value, &["limit", "k"])
        .and_then(crate::wire::Json::as_u64)
        .unwrap_or(50)
        .clamp(1, 500) as usize;
    let sort_by = json_field_alias(&value, &["sort_by", "sortBy", "sort"])
        .and_then(crate::wire::Json::as_str)
        .unwrap_or("created")
        .to_string();
    let order = json_field_alias(&value, &["order", "direction"])
        .and_then(crate::wire::Json::as_str)
        .unwrap_or("desc");
    Ok(GatewayTraceSearchRequest {
        cursor,
        limit,
        sort_by,
        desc: !order.eq_ignore_ascii_case("asc"),
    })
}

#[derive(Clone)]
struct GatewaySearchItem {
    json: String,
    trace_id: u64,
    span_id: u64,
    score: f32,
}

fn gateway_search_items_from_body(body: &str) -> Option<Vec<GatewaySearchItem>> {
    let value = crate::wire::parse(body).ok()?;
    let items = match &value {
        crate::wire::Json::Arr(items) => items.as_slice(),
        crate::wire::Json::Obj(_) => crate::wire::field(&value, "items")?.as_array(),
        _ => return None,
    };
    Some(
        items
            .iter()
            .filter_map(|item| {
                let trace_id = gateway_json_u64_alias(item, &["trace_id", "traceId"])?;
                let span_id = gateway_json_u64_alias(item, &["span_id", "spanId"])?;
                let score = json_field_alias(item, &["score"])
                    .and_then(crate::wire::Json::as_f32)
                    .unwrap_or(0.0);
                Some(GatewaySearchItem {
                    json: item.to_compact_json(),
                    trace_id,
                    span_id,
                    score,
                })
            })
            .collect(),
    )
}

#[derive(Clone, Debug)]
struct GatewayTraceItem {
    json: crate::wire::Json,
    trace_id: u64,
    span_id: u64,
}

impl GatewayTraceItem {
    fn compact_json(&self) -> String {
        self.json.to_compact_json()
    }

    fn with_rank(&self, rank: usize) -> String {
        let crate::wire::Json::Obj(kvs) = &self.json else {
            return self.compact_json();
        };
        let mut fields = vec![format!(r#""rank":{rank}"#)];
        for (key, value) in kvs {
            if key == "rank" {
                continue;
            }
            fields.push(format!(
                r#""{}":{}"#,
                gateway_json_escape(key),
                value.to_compact_json()
            ));
        }
        format!("{{{}}}", fields.join(","))
    }
}

fn gateway_trace_items_from_body(body: &str) -> Option<Vec<GatewayTraceItem>> {
    let value = crate::wire::parse(body).ok()?;
    let items = crate::wire::field(&value, "items")?.as_array();
    Some(
        items
            .iter()
            .filter_map(|item| {
                let trace_id = gateway_json_u64_alias(item, &["traceId", "trace_id"])?;
                let span_id = gateway_json_u64_alias(item, &["spanId", "span_id"])?;
                Some(GatewayTraceItem {
                    json: item.clone(),
                    trace_id,
                    span_id,
                })
            })
            .collect(),
    )
}

fn gateway_next_cursor_from_body(body: &str) -> Option<usize> {
    let value = crate::wire::parse(body).ok()?;
    crate::wire::field(&value, "nextCursor")
        .or_else(|| crate::wire::field(&value, "next_cursor"))
        .and_then(crate::wire::Json::as_u64)
        .map(|value| value as usize)
}

fn gateway_total_from_body(body: &str) -> Option<usize> {
    let value = crate::wire::parse(body).ok()?;
    crate::wire::field(&value, "total")
        .and_then(crate::wire::Json::as_u64)
        .map(|value| value as usize)
}

fn sort_gateway_trace_items(items: &mut [GatewayTraceItem], sort_by: &str, desc: bool) {
    let sort = sort_by.to_ascii_lowercase();
    items.sort_by(|a, b| {
        let ord = match sort.as_str() {
            "duration" | "duration_ns" | "durationns" => {
                gateway_item_u64(&a.json, &["durationNs", "duration_ns"])
                    .cmp(&gateway_item_u64(&b.json, &["durationNs", "duration_ns"]))
            }
            "cost" | "cost_usd" | "costusd" => gateway_item_cost_nanos(&a.json)
                .cmp(&gateway_item_cost_nanos(&b.json)),
            "tokens" | "token_count" | "tokencount" => {
                gateway_item_total_tokens(&a.json).cmp(&gateway_item_total_tokens(&b.json))
            }
            "status" => gateway_item_u64(&a.json, &["status"])
                .cmp(&gateway_item_u64(&b.json, &["status"])),
            "span" | "span_id" | "spanid" => a.span_id.cmp(&b.span_id),
            _ => a
                .trace_id
                .cmp(&b.trace_id)
                .then_with(|| a.span_id.cmp(&b.span_id)),
        };
        let ord = if desc { ord.reverse() } else { ord };
        ord.then_with(|| a.trace_id.cmp(&b.trace_id))
            .then_with(|| a.span_id.cmp(&b.span_id))
    });
}

fn gateway_item_u64(item: &crate::wire::Json, names: &[&str]) -> u64 {
    gateway_json_u64_alias(item, names).unwrap_or(0)
}

fn gateway_item_total_tokens(item: &crate::wire::Json) -> u64 {
    if let Some(usage) = json_field_alias(item, &["usage"]) {
        return gateway_json_u64_alias(usage, &["totalTokens", "total_tokens"]).unwrap_or(0);
    }
    gateway_json_u64_alias(item, &["totalTokens", "total_tokens"]).unwrap_or(0)
}

fn gateway_item_cost_nanos(item: &crate::wire::Json) -> u64 {
    if let Some(detail) = json_field_alias(item, &["costDetail", "cost_detail"]) {
        if let Some(nanos) = gateway_json_u64_alias(detail, &["costUsdNanos", "cost_usd_nanos"]) {
            return nanos;
        }
    }
    json_field_alias(item, &["costUsd", "cost_usd"])
        .and_then(crate::wire::Json::as_f64)
        .map(|cost| (cost * 1_000_000_000.0).max(0.0) as u64)
        .unwrap_or(0)
}

fn gateway_json_u64_alias(obj: &crate::wire::Json, names: &[&str]) -> Option<u64> {
    json_field_alias(obj, names).and_then(crate::wire::Json::as_u64)
}

fn gateway_json_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}
