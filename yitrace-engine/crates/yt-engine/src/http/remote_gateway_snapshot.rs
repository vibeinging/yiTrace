#[derive(Clone, Debug)]
struct RemoteGatewaySnapshot {
    lease_id: Option<String>,
    route_table_version: Option<u64>,
    shards: Vec<RemoteGatewaySnapshotShard>,
}

#[derive(Clone, Debug)]
struct RemoteGatewaySnapshotShard {
    index: usize,
    shard_id: Option<String>,
    replica_id: Option<String>,
    snapshot: crate::wire::Json,
}

impl RemoteGatewaySnapshot {
    fn snapshot_for(&self, index: usize) -> Option<&crate::wire::Json> {
        self.shards
            .iter()
            .find(|shard| shard.index == index)
            .map(|shard| &shard.snapshot)
    }

    fn replica_for(&self, index: usize) -> Option<&str> {
        self.shards
            .iter()
            .find(|shard| shard.index == index)
            .and_then(|shard| shard.replica_id.as_deref())
    }
}

impl RemoteShardGateway {
    fn remote_snapshot_from_body(
        &self,
        body: &str,
    ) -> Result<Option<RemoteGatewaySnapshot>, (u16, String)> {
        let Some(mut snapshot) = parse_remote_gateway_snapshot_from_body(body)? else {
            return Ok(None);
        };
        if let Some(lease_id) = snapshot.lease_id.as_deref() {
            snapshot = self.remote_snapshot_lookup(lease_id)?;
        }
        self.validate_remote_snapshot(&snapshot)?;
        Ok(Some(snapshot))
    }

    fn validate_remote_snapshot(
        &self,
        snapshot: &RemoteGatewaySnapshot,
    ) -> Result<(), (u16, String)> {
        let current_version = self.route_table_version();
        if snapshot.route_table_version != current_version {
            return Err(remote_snapshot_error(
                409,
                "route_table_expired",
                "remote gateway snapshot route table version no longer matches current gateway",
            ));
        }
        let shard_count = self.shard_count();
        if snapshot.shards.len() != shard_count {
            return Err(remote_snapshot_error(
                409,
                "snapshot_mismatch",
                "remote gateway snapshot shard count does not match current gateway",
            ));
        }
        let route_ids = self.route_shard_ids_snapshot();
        let mut seen = vec![false; shard_count];
        for shard in &snapshot.shards {
            if shard.index >= shard_count || seen[shard.index] {
                return Err(remote_snapshot_error(
                    409,
                    "snapshot_mismatch",
                    "remote gateway snapshot contains an invalid shard index",
                ));
            }
            seen[shard.index] = true;
            if let Some(id) = shard.shard_id.as_deref() {
                if route_ids
                    .get(shard.index)
                    .map(String::as_str)
                    .filter(|current| *current == id)
                    .is_none()
                {
                    return Err(remote_snapshot_error(
                        409,
                        "route_table_expired",
                        "remote gateway snapshot shard id no longer matches current route table",
                    ));
                }
            }
            if let Some(replica_id) = shard.replica_id.as_deref() {
                if let Some(route_table) = self.route_table_snapshot() {
                    let shard_id = route_ids
                        .get(shard.index)
                        .map(String::as_str)
                        .unwrap_or("");
                    let replica_still_exists = route_table
                        .replicas_for_shard_id(shard_id)
                        .into_iter()
                        .any(|route| route.replica_id() == replica_id);
                    if !replica_still_exists {
                        return Err(remote_snapshot_error(
                            409,
                            "route_table_expired",
                            "remote gateway snapshot replica id no longer matches current route table",
                        ));
                    }
                }
            }
        }
        if seen.iter().any(|hit| !*hit) {
            return Err(remote_snapshot_error(
                409,
                "snapshot_mismatch",
                "remote gateway snapshot is missing a shard",
            ));
        }
        Ok(())
    }

    fn fanout_route_with_snapshot(
        &self,
        method: &str,
        path: &str,
        body: &str,
        tenant: Option<u64>,
        force_leader: bool,
        snapshot: Option<&RemoteGatewaySnapshot>,
    ) -> Result<
        (
            Vec<RemoteReadTarget>,
            Vec<(usize, Result<(u16, String), String>)>,
        ),
        (u16, String),
    > {
        let targets = self.read_targets_snapshot(force_leader, snapshot)?;
        let mut handles = Vec::new();
        for target in targets.iter().cloned() {
            let method = method.to_string();
            let path = path.to_string();
            let body = match gateway_replace_snapshot_body(
                body,
                snapshot.and_then(|snap| snap.snapshot_for(target.index)),
            ) {
                Ok(body) => body,
                Err(error) => {
                    handles.push(std::thread::spawn(move || (target.index, Err(error))));
                    continue;
                }
            };
            handles.push(std::thread::spawn(move || {
                let result =
                    target
                        .client
                        .route_json_with_tenant(&method, &path, &body, tenant);
                (target.index, result)
            }));
        }
        let results = handles
            .into_iter()
            .map(|handle| {
                handle.join().unwrap_or_else(|_| {
                    (
                        0,
                        Err("remote shard fanout worker panicked".to_string()),
                    )
                })
            })
            .collect();
        Ok((targets, results))
    }

    fn fanout_trace_search_pages(
        &self,
        body: &str,
        tenant: Option<u64>,
        fetch_limit: usize,
        force_leader: bool,
        snapshot: Option<&RemoteGatewaySnapshot>,
    ) -> Result<
        (
            Vec<RemoteReadTarget>,
            Vec<(usize, Result<GatewayTracePage, GatewayShardQueryError>)>,
        ),
        (u16, String),
    > {
        let targets = self.read_targets_snapshot(force_leader, snapshot)?;
        let mut handles = Vec::new();
        for target in targets.iter().cloned() {
            let body = body.to_string();
            let shard_snapshot = snapshot.and_then(|snap| snap.snapshot_for(target.index).cloned());
            handles.push(std::thread::spawn(move || {
                let result = gateway_fetch_trace_search_pages(
                    &target.client,
                    &body,
                    tenant,
                    fetch_limit,
                    shard_snapshot,
                );
                (target.index, result)
            }));
        }
        let results = handles
            .into_iter()
            .map(|handle| {
                handle.join().unwrap_or_else(|_| {
                    (
                        0,
                        Err(GatewayShardQueryError {
                            status: 0,
                            detail: "remote shard trace-search worker panicked".to_string(),
                        }),
                    )
                })
            })
            .collect();
        Ok((targets, results))
    }
}

#[derive(Clone, Debug)]
struct GatewayShardQueryError {
    status: u16,
    detail: String,
}

#[derive(Clone, Debug)]
struct GatewayTracePage {
    items: Vec<GatewayTraceItem>,
    total: usize,
    snapshot: Option<crate::wire::Json>,
}

fn gateway_fetch_trace_search_pages(
    shard: &RemoteShardClient,
    body: &str,
    tenant: Option<u64>,
    fetch_limit: usize,
    initial_snapshot: Option<crate::wire::Json>,
) -> Result<GatewayTracePage, GatewayShardQueryError> {
    let mut out = Vec::new();
    let mut total = None;
    let mut cursor = 0usize;
    let mut pinned_snapshot = initial_snapshot;
    loop {
        let remaining = fetch_limit.saturating_sub(out.len());
        if remaining == 0 {
            break;
        }
        let request_limit = remaining.min(500);
        let shard_body = gateway_override_page_body_with_snapshot(
            body,
            cursor,
            request_limit,
            pinned_snapshot.as_ref(),
        )
        .map_err(|detail| GatewayShardQueryError {
            status: 400,
            detail,
        })?;
        let (status, response) = shard
            .route_json_with_tenant("POST", "/v1/trace-search", &shard_body, tenant)
            .map_err(|detail| GatewayShardQueryError { status: 0, detail })?;
        if status != 200 {
            return Err(GatewayShardQueryError {
                status,
                detail: response,
            });
        }
        if total.is_none() {
            total = gateway_total_from_body(&response);
        }
        if pinned_snapshot.is_none() {
            pinned_snapshot = gateway_snapshot_from_body(&response);
        }
        let page_items = gateway_trace_items_from_body(&response).unwrap_or_default();
        let page_len = page_items.len();
        out.extend(page_items);
        let Some(next) = gateway_next_cursor_from_body(&response) else {
            break;
        };
        if page_len == 0 || next <= cursor {
            break;
        }
        cursor = next;
    }
    let total = total.unwrap_or(out.len());
    Ok(GatewayTracePage {
        items: out,
        total,
        snapshot: pinned_snapshot,
    })
}

fn gateway_override_page_body(body: &str, cursor: usize, limit: usize) -> Result<String, String> {
    gateway_override_page_body_with_snapshot(body, cursor, limit, None)
}

fn gateway_override_page_body_with_snapshot(
    body: &str,
    cursor: usize,
    limit: usize,
    snapshot: Option<&crate::wire::Json>,
) -> Result<String, String> {
    let value = crate::wire::parse(body)?;
    let crate::wire::Json::Obj(kvs) = value else {
        return Err("trace-search request must be a JSON object".to_string());
    };
    let mut fields = Vec::new();
    for (key, value) in kvs {
        if matches!(
            key.as_str(),
            "cursor"
                | "offset"
                | "limit"
                | "k"
                | "includeFanout"
                | "include_fanout"
                | "snapshot"
                | "snapshotToken"
                | "snapshot_token"
        ) {
            continue;
        }
        fields.push(format!(
            r#""{}":{}"#,
            gateway_json_escape(&key),
            value.to_compact_json()
        ));
    }
    fields.push(format!(r#""cursor":{cursor}"#));
    fields.push(format!(r#""limit":{limit}"#));
    if let Some(snapshot) = snapshot {
        fields.push(format!(r#""snapshot":{}"#, snapshot.to_compact_json()));
    }
    Ok(format!("{{{}}}", fields.join(",")))
}

fn parse_remote_gateway_snapshot_from_body(
    body: &str,
) -> Result<Option<RemoteGatewaySnapshot>, (u16, String)> {
    let value = crate::wire::parse(body)
        .map_err(|e| remote_snapshot_error(400, "bad_snapshot", &e))?;
    let Some(snapshot_value) =
        json_field_alias(&value, &["snapshot", "snapshotToken", "snapshot_token"])
    else {
        return Ok(None);
    };
    parse_remote_gateway_snapshot_value(snapshot_value).map(Some)
}

fn parse_remote_gateway_snapshot_value(
    value: &crate::wire::Json,
) -> Result<RemoteGatewaySnapshot, (u16, String)> {
    let parsed;
    let source = match value {
        crate::wire::Json::Str(raw) => {
            parsed = crate::wire::parse(raw)
                .map_err(|e| remote_snapshot_error(400, "bad_snapshot", &e))?;
            &parsed
        }
        other => other,
    };
    let mode = json_field_alias(source, &["mode"])
        .and_then(crate::wire::Json::as_str)
        .unwrap_or("");
    if mode != "remote_gateway" {
        return Err(remote_snapshot_error(
            400,
            "bad_snapshot",
            "remote gateway requires a remote_gateway snapshot token",
        ));
    }
    let route_table_version =
        json_field_alias(source, &["routeTableVersion", "route_table_version"])
            .and_then(crate::wire::Json::as_u64);
    let lease_id = json_field_alias(source, &["leaseId", "lease_id"])
        .and_then(crate::wire::Json::as_str)
        .map(str::to_string);
    let Some(shards_value) = json_field_alias(source, &["shards"]) else {
        return Err(remote_snapshot_error(
            400,
            "bad_snapshot",
            "remote gateway snapshot.shards is required",
        ));
    };
    let crate::wire::Json::Arr(items) = shards_value else {
        return Err(remote_snapshot_error(
            400,
            "bad_snapshot",
            "remote gateway snapshot.shards must be an array",
        ));
    };
    let mut shards = Vec::with_capacity(items.len());
    for (fallback_idx, item) in items.iter().enumerate() {
        let index = json_field_alias(item, &["shard", "shardIndex", "shard_index"])
            .and_then(crate::wire::Json::as_u64)
            .map(|idx| idx as usize)
            .unwrap_or(fallback_idx);
        let shard_id = json_field_alias(item, &["shardId", "shard_id"])
            .and_then(crate::wire::Json::as_str)
            .map(str::to_string);
        let replica_id = json_field_alias(item, &["replicaId", "replica_id"])
            .and_then(crate::wire::Json::as_str)
            .map(str::to_string);
        let Some(local_snapshot) = json_field_alias(item, &["snapshot"]) else {
            return Err(remote_snapshot_error(
                400,
                "bad_snapshot",
                "remote gateway snapshot shard must include a local snapshot",
            ));
        };
        let parsed_local;
        let snapshot = match local_snapshot {
            crate::wire::Json::Str(raw) => {
                parsed_local = crate::wire::parse(raw)
                    .map_err(|e| remote_snapshot_error(400, "bad_snapshot", &e))?;
                parsed_local
            }
            other => other.clone(),
        };
        shards.push(RemoteGatewaySnapshotShard {
            index,
            shard_id,
            replica_id,
            snapshot,
        });
    }
    Ok(RemoteGatewaySnapshot {
        lease_id,
        route_table_version,
        shards,
    })
}

fn remote_gateway_snapshot_field(
    route_table_version: Option<u64>,
    route_ids: &[String],
    replica_ids: &[String],
    shard_count: usize,
    snapshots: Vec<(usize, crate::wire::Json)>,
) -> String {
    if snapshots.len() != shard_count || shard_count == 0 {
        return String::new();
    }
    let mut by_idx = vec![None; shard_count];
    for (idx, snapshot) in snapshots {
        if idx >= shard_count || by_idx[idx].is_some() {
            return String::new();
        }
        by_idx[idx] = Some(snapshot);
    }
    if by_idx.iter().any(Option::is_none) {
        return String::new();
    }
    let version = route_table_version
        .map(|version| format!(r#","routeTableVersion":{version}"#))
        .unwrap_or_default();
    let shards = by_idx
        .into_iter()
        .enumerate()
        .map(|(idx, snapshot)| {
            let shard_id = route_ids
                .get(idx)
                .cloned()
                .unwrap_or_else(|| format!("process-shard-{idx}"));
            let replica_field = replica_ids
                .get(idx)
                .map(|replica_id| format!(r#","replicaId":"{}""#, gateway_json_escape(replica_id)))
                .unwrap_or_default();
            format!(
                r#"{{"shard":{},"shardId":"{}"{},"snapshot":{}}}"#,
                idx,
                gateway_json_escape(&shard_id),
                replica_field,
                snapshot.expect("checked above").to_compact_json()
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#","snapshot":{{"mode":"remote_gateway"{version},"shards":[{shards}]}}"#
    )
}

fn gateway_snapshot_from_body(body: &str) -> Option<crate::wire::Json> {
    let value = crate::wire::parse(body).ok()?;
    let snapshot = json_field_alias(&value, &["snapshot", "snapshotToken", "snapshot_token"])?;
    match snapshot {
        crate::wire::Json::Str(raw) => crate::wire::parse(raw).ok(),
        other => Some(other.clone()),
    }
}

fn gateway_replace_snapshot_body(
    body: &str,
    snapshot: Option<&crate::wire::Json>,
) -> Result<String, String> {
    let value = crate::wire::parse(body)?;
    let crate::wire::Json::Obj(kvs) = value else {
        return Err("request must be a JSON object".to_string());
    };
    let mut fields = Vec::new();
    for (key, value) in kvs {
        if matches!(
            key.as_str(),
            "snapshot" | "snapshotToken" | "snapshot_token"
        ) {
            continue;
        }
        fields.push(format!(
            r#""{}":{}"#,
            gateway_json_escape(&key),
            value.to_compact_json()
        ));
    }
    if let Some(snapshot) = snapshot {
        fields.push(format!(r#""snapshot":{}"#, snapshot.to_compact_json()));
    }
    Ok(format!("{{{}}}", fields.join(",")))
}

fn remote_snapshot_error(status: u16, code: &str, message: &str) -> (u16, String) {
    (
        status,
        format!(
            r#"{{"error":"remote snapshot error","code":"{}","message":"{}"}}"#,
            gateway_json_escape(code),
            gateway_json_escape(message)
        ),
    )
}
