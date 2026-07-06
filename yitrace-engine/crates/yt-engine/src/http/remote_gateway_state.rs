/// 远端 shard gateway facade：负责把 HTTP JSON 请求路由到多个 shard server。
///
/// 这个类型不负责监听 socket、鉴权或限流；它只表达分布式需要的核心数据路径：
/// ingest 按 shard key 路由，查询 fanout 到所有 shard，再合并 JSON 响应和失败诊断。
#[derive(Clone, Debug)]
pub struct RemoteShardGateway {
    state: Arc<std::sync::RwLock<RemoteShardGatewayState>>,
    router: Arc<ShardRouter>,
    remote_snapshot_leases: Arc<Mutex<RemoteGatewaySnapshotLeaseBook>>,
}

#[derive(Clone, Debug)]
struct RemoteShardGatewayState {
    shards: Vec<RemoteShardClient>,
    route_table: Option<RemoteShardRouteTable>,
    health: std::collections::BTreeMap<String, RemoteReplicaHealth>,
}

#[derive(Clone, Debug)]
struct RemoteReplicaHealth {
    shard_id: String,
    replica_id: String,
    addr: String,
    health: String,
    http_status: u16,
    latency_ms: u128,
    checked_at_ns: u128,
    committed_tail: Option<u64>,
    leader_tail: Option<u64>,
    replication_lag_lsn: Option<u64>,
    readable: bool,
    reason: String,
}

#[derive(Clone)]
struct RemoteReplicaProbe {
    shard_id: String,
    replica_id: String,
    addr: String,
    role: RemoteShardRouteRole,
    readable: bool,
    writable: bool,
    max_lag_lsn: Option<u64>,
}

#[derive(Clone, Debug)]
struct RemoteReadTarget {
    index: usize,
    shard_id: String,
    replica_id: String,
    addr: String,
    role: RemoteShardRouteRole,
    health: String,
    replication_lag_lsn: Option<u64>,
    reason: String,
    client: RemoteShardClient,
}

impl RemoteShardGateway {
    pub fn new(addrs: Vec<String>) -> Result<Self, String> {
        if addrs.is_empty() {
            return Err("remote shard gateway requires at least one shard".to_string());
        }
        Ok(Self {
            state: Arc::new(std::sync::RwLock::new(RemoteShardGatewayState {
                shards: addrs.into_iter().map(RemoteShardClient::new).collect(),
                route_table: None,
                health: std::collections::BTreeMap::new(),
            })),
            router: Arc::new(ShardRouter::default()),
            remote_snapshot_leases: Arc::new(Mutex::new(RemoteGatewaySnapshotLeaseBook::default())),
        })
    }

    pub fn from_route_table_json(body: &str) -> Result<Self, String> {
        let route_table = RemoteShardRouteTable::parse_json(body)?;
        let write_addrs = route_table.write_addrs();
        if write_addrs.is_empty() {
            return Err("remote shard route table requires at least one writable shard".to_string());
        }
        Ok(Self {
            state: Arc::new(std::sync::RwLock::new(RemoteShardGatewayState {
                shards: write_addrs.into_iter().map(RemoteShardClient::new).collect(),
                route_table: Some(route_table),
                health: std::collections::BTreeMap::new(),
            })),
            router: Arc::new(ShardRouter::default()),
            remote_snapshot_leases: Arc::new(Mutex::new(RemoteGatewaySnapshotLeaseBook::default())),
        })
    }

    pub fn reload_route_table_json(&self, body: &str) -> Result<u64, String> {
        let route_table = RemoteShardRouteTable::parse_json(body)?;
        let write_addrs = route_table.write_addrs();
        if write_addrs.is_empty() {
            return Err("remote shard route table requires at least one writable shard".to_string());
        }
        let version = route_table.version();
        let mut state = self
            .state
            .write()
            .map_err(|_| "remote shard gateway route table lock poisoned".to_string())?;
        if let Some(current) = &state.route_table {
            if version < current.version() {
                return Err(format!(
                    "route table version {version} is older than current {}",
                    current.version()
                ));
            }
        }
        state.shards = write_addrs.into_iter().map(RemoteShardClient::new).collect();
        state.route_table = Some(route_table);
        state.health.clear();
        if let Ok(mut leases) = self.remote_snapshot_leases.lock() {
            leases.clear();
        }
        self.router.clear();
        Ok(version)
    }

    pub fn shard_count(&self) -> usize {
        self.shards_snapshot().len()
    }

    pub fn route_table_version(&self) -> Option<u64> {
        self.route_table_snapshot()
            .as_ref()
            .map(RemoteShardRouteTable::version)
    }

    fn shards_snapshot(&self) -> Vec<RemoteShardClient> {
        self.state
            .read()
            .map(|state| state.shards.clone())
            .unwrap_or_default()
    }

    fn route_table_snapshot(&self) -> Option<RemoteShardRouteTable> {
        self.state
            .read()
            .ok()
            .and_then(|state| state.route_table.clone())
    }

    fn health_snapshot(&self) -> std::collections::BTreeMap<String, RemoteReplicaHealth> {
        self.state
            .read()
            .map(|state| state.health.clone())
            .unwrap_or_default()
    }

    fn replica_probe_snapshot(&self) -> Vec<RemoteReplicaProbe> {
        if let Some(route_table) = self.route_table_snapshot() {
            return route_table
                .shards()
                .iter()
                .map(|route| RemoteReplicaProbe {
                    shard_id: route.id().to_string(),
                    replica_id: route.replica_id().to_string(),
                    addr: route.addr().to_string(),
                    role: route.role(),
                    readable: route.readable(),
                    writable: route.writable(),
                    max_lag_lsn: route.max_lag_lsn(),
                })
                .collect();
        }
        self.shards_snapshot()
            .into_iter()
            .enumerate()
            .map(|(idx, client)| {
                let id = format!("process-shard-{idx}");
                RemoteReplicaProbe {
                    shard_id: id.clone(),
                    replica_id: id,
                    addr: client.addr().to_string(),
                    role: RemoteShardRouteRole::Leader,
                    readable: true,
                    writable: true,
                    max_lag_lsn: Some(0),
                }
            })
            .collect()
    }

    fn cluster_health_json(&self) -> String {
        let health = self.health_snapshot();
        let items = health
            .values()
            .map(RemoteReplicaHealth::json)
            .collect::<Vec<_>>()
            .join(",");
        format!(
            r#"{{"routeTableVersion":{},"replicaCount":{},"replicas":[{}]}}"#,
            self.route_table_version()
                .map(|version| version.to_string())
                .unwrap_or_else(|| "null".to_string()),
            health.len(),
            items
        )
    }

    fn refresh_cluster_health_json(&self) -> String {
        let probes = self.replica_probe_snapshot();
        let mut health: Vec<RemoteReplicaHealth> =
            probes.iter().map(probe_remote_replica).collect();
        apply_replication_lag(&mut health, &probes);
        let mut map = std::collections::BTreeMap::new();
        for item in health {
            map.insert(replica_health_key(&item.shard_id, &item.replica_id), item);
        }
        if let Ok(mut state) = self.state.write() {
            state.health = map;
        }
        self.cluster_health_json()
    }

    fn route_shard_ids_snapshot(&self) -> Vec<String> {
        if let Some(route_table) = self.route_table_snapshot() {
            return route_table
                .write_routes()
                .into_iter()
                .map(|route| route.id().to_string())
                .collect();
        }
        (0..self.shard_count())
            .map(|idx| format!("process-shard-{idx}"))
            .collect()
    }

    fn read_targets_snapshot(
        &self,
        force_leader: bool,
        snapshot: Option<&RemoteGatewaySnapshot>,
    ) -> Result<Vec<RemoteReadTarget>, (u16, String)> {
        let Some(route_table) = self.route_table_snapshot() else {
            return Ok(self
                .shards_snapshot()
                .into_iter()
                .enumerate()
                .map(|(idx, client)| {
                    let shard_id = format!("process-shard-{idx}");
                    RemoteReadTarget::from_client(idx, shard_id.clone(), shard_id, client)
                })
                .collect());
        };
        let health = self.health_snapshot();
        let write_routes = route_table.write_routes();
        let mut targets = Vec::with_capacity(write_routes.len());
        for (idx, writer) in write_routes.into_iter().enumerate() {
            let pinned_replica = snapshot.and_then(|snapshot| snapshot.replica_for(idx));
            let selected = if let Some(replica_id) = pinned_replica {
                let route = route_table
                    .replicas_for_shard_id(writer.id())
                    .into_iter()
                    .find(|route| route.replica_id() == replica_id)
                    .ok_or_else(|| {
                        remote_snapshot_error(
                            409,
                            "route_table_expired",
                            "remote gateway snapshot replica id no longer matches current route table",
                        )
                    })?;
                RemoteReadTarget::from_route(idx, route, &health, "snapshot_pinned")
            } else if force_leader {
                RemoteReadTarget::from_route(idx, writer, &health, "leader_required")
            } else {
                choose_bounded_stale_read_target(&route_table, writer, idx, &health)
            };
            targets.push(selected);
        }
        Ok(targets)
    }

    fn cluster_json(&self) -> String {
        let shards = self.shards_snapshot();
        let route_table = self.route_table_snapshot();
        let health = self.health_snapshot();
        let writable_routes: Vec<RemoteShardRoute> = route_table
            .as_ref()
            .map(|table| table.write_routes().into_iter().cloned().collect())
            .unwrap_or_default();
        let shard_items: Vec<String> = shards
            .iter()
            .enumerate()
            .map(|(idx, client)| {
                let status = client
                    .route_json_with_tenant("GET", "/v1/cluster/shards", "", None)
                    .map(|(status, _)| status)
                    .unwrap_or(0);
                let shard_id = writable_routes
                    .get(idx)
                    .map(|route| route.id().to_string())
                    .unwrap_or_else(|| format!("process-shard-{idx}"));
                let route_fields = writable_routes
                    .get(idx)
                    .map(|route| {
                        let key = replica_health_key(route.id(), route.replica_id());
                        let lag = route
                            .max_lag_lsn()
                            .map(|value| format!(r#","maxLagLsn":{value}"#))
                            .unwrap_or_default();
                        let replicas = route_table
                            .as_ref()
                            .map(|table| route_replicas_json(table, route.id(), &health))
                            .unwrap_or_default();
                        let health_fields = route_health_fields(health.get(&key));
                        format!(
                            r#","replicaId":"{}","role":"{}","readable":{},"writable":{},"weight":{},"priority":{}{}{}{}"#,
                            gateway_json_escape(route.replica_id()),
                            route.role().as_str(),
                            route.readable(),
                            route.writable(),
                            route.weight(),
                            route.priority(),
                            lag,
                            health_fields,
                            replicas
                        )
                    })
                    .unwrap_or_default();
                format!(
                    r#"{{"shardId":"{}","addr":"{}","httpStatus":{status}{route_fields}}}"#,
                    gateway_json_escape(&shard_id),
                    gateway_json_escape(client.addr()),
                )
            })
            .collect();
        let route_table_fields = route_table
            .as_ref()
            .map(|table| {
                format!(
                    r#","routeTableVersion":{},"routeTableFingerprint":"{}""#,
                    table.version(),
                    gateway_json_escape(&table.fingerprint())
                )
            })
            .unwrap_or_default();
        format!(
            r#"{{"mode":"process_gateway","routing":"hash_tenant_session_trace","shardCount":{}{},"shards":[{}]}}"#,
            shards.len(),
            route_table_fields,
            shard_items.join(",")
        )
    }
}

fn route_replicas_json(
    table: &RemoteShardRouteTable,
    shard_id: &str,
    health: &std::collections::BTreeMap<String, RemoteReplicaHealth>,
) -> String {
    let replicas = table
        .replicas_for_shard_id(shard_id)
        .into_iter()
        .map(|route| route_replica_json(route, health))
        .collect::<Vec<_>>()
        .join(",");
    if replicas.is_empty() {
        String::new()
    } else {
        format!(r#","replicas":[{replicas}]"#)
    }
}

fn route_replica_json(
    route: &RemoteShardRoute,
    health: &std::collections::BTreeMap<String, RemoteReplicaHealth>,
) -> String {
    let lag = route
        .max_lag_lsn()
        .map(|value| format!(r#","maxLagLsn":{value}"#))
        .unwrap_or_default();
    let key = replica_health_key(route.id(), route.replica_id());
    let health_fields = route_health_fields(health.get(&key));
    format!(
        r#"{{"replicaId":"{}","addr":"{}","role":"{}","readable":{},"writable":{},"weight":{},"priority":{}{}{}}}"#,
        gateway_json_escape(route.replica_id()),
        gateway_json_escape(route.addr()),
        route.role().as_str(),
        route.readable(),
        route.writable(),
        route.weight(),
        route.priority(),
        lag,
        health_fields
    )
}

impl RemoteReplicaHealth {
    fn json(&self) -> String {
        format!(
            r#"{{"shardId":"{}","replicaId":"{}","addr":"{}","health":"{}","httpStatus":{},"latencyMs":{},"checkedAtNs":"{}","committedTail":{},"leaderTail":{},"replicationLagLsn":{},"readable":{},"reason":"{}"}}"#,
            gateway_json_escape(&self.shard_id),
            gateway_json_escape(&self.replica_id),
            gateway_json_escape(&self.addr),
            gateway_json_escape(&self.health),
            self.http_status,
            self.latency_ms,
            self.checked_at_ns,
            opt_u64_json(self.committed_tail),
            opt_u64_json(self.leader_tail),
            opt_u64_json(self.replication_lag_lsn),
            self.readable,
            gateway_json_escape(&self.reason)
        )
    }
}

impl RemoteReadTarget {
    fn from_client(
        index: usize,
        shard_id: String,
        replica_id: String,
        client: RemoteShardClient,
    ) -> Self {
        Self {
            index,
            shard_id,
            replica_id,
            addr: client.addr().to_string(),
            role: RemoteShardRouteRole::Leader,
            health: "unknown".to_string(),
            replication_lag_lsn: None,
            reason: "static_writer".to_string(),
            client,
        }
    }

    fn from_route(
        index: usize,
        route: &RemoteShardRoute,
        health: &std::collections::BTreeMap<String, RemoteReplicaHealth>,
        reason: &str,
    ) -> Self {
        let key = replica_health_key(route.id(), route.replica_id());
        let snapshot = health.get(&key);
        Self {
            index,
            shard_id: route.id().to_string(),
            replica_id: route.replica_id().to_string(),
            addr: route.addr().to_string(),
            role: route.role(),
            health: snapshot
                .map(|health| health.health.clone())
                .unwrap_or_else(|| "unknown".to_string()),
            replication_lag_lsn: snapshot.and_then(|health| health.replication_lag_lsn),
            reason: reason.to_string(),
            client: RemoteShardClient::new(route.addr().to_string()),
        }
    }

    fn json(&self) -> String {
        format!(
            r#"{{"shard":{},"shardId":"{}","replicaId":"{}","addr":"{}","role":"{}","health":"{}","replicationLagLsn":{},"reason":"{}"}}"#,
            self.index,
            gateway_json_escape(&self.shard_id),
            gateway_json_escape(&self.replica_id),
            gateway_json_escape(&self.addr),
            self.role.as_str(),
            gateway_json_escape(&self.health),
            opt_u64_json(self.replication_lag_lsn),
            gateway_json_escape(&self.reason)
        )
    }
}

fn choose_bounded_stale_read_target(
    table: &RemoteShardRouteTable,
    writer: &RemoteShardRoute,
    idx: usize,
    health: &std::collections::BTreeMap<String, RemoteReplicaHealth>,
) -> RemoteReadTarget {
    let mut candidates = table
        .replicas_for_shard_id(writer.id())
        .into_iter()
        .filter(|route| route.readable() && !route.writable())
        .filter_map(|route| {
            let key = replica_health_key(route.id(), route.replica_id());
            let h = health.get(&key)?;
            if h.health != "healthy" || !h.readable {
                return None;
            }
            let lag = h.replication_lag_lsn.unwrap_or(0);
            let max_lag = route.max_lag_lsn().unwrap_or(0);
            if lag > max_lag {
                return None;
            }
            Some((route, lag))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|(left, left_lag), (right, right_lag)| {
        left.writable()
            .cmp(&right.writable())
            .then_with(|| right.priority().cmp(&left.priority()))
            .then_with(|| left_lag.cmp(right_lag))
            .then_with(|| left.replica_id().cmp(right.replica_id()))
    });
    if let Some((route, _)) = candidates.first() {
        return RemoteReadTarget::from_route(idx, route, health, "bounded_stale_follower");
    }
    RemoteReadTarget::from_route(idx, writer, health, "leader_fallback")
}

fn remote_read_targets_json(targets: &[RemoteReadTarget]) -> String {
    targets
        .iter()
        .map(RemoteReadTarget::json)
        .collect::<Vec<_>>()
        .join(",")
}

fn probe_remote_replica(probe: &RemoteReplicaProbe) -> RemoteReplicaHealth {
    let client =
        RemoteShardClient::new(probe.addr.clone()).with_timeout(std::time::Duration::from_millis(300));
    let started = std::time::Instant::now();
    let checked_at_ns = unix_now_ns();
    let first = client.route_json_with_tenant("GET", "/v1/replication/status", "", None);
    let result = match first {
        Ok((404, _)) => client.route_json_with_tenant("GET", "/v1/cluster/shards", "", None),
        other => other,
    };
    let latency_ms = started.elapsed().as_millis();
    match result {
        Ok((200, body)) => {
            let status = remote_replication_status_from_body(&body);
            RemoteReplicaHealth {
                shard_id: probe.shard_id.clone(),
                replica_id: probe.replica_id.clone(),
                addr: probe.addr.clone(),
                health: "healthy".to_string(),
                http_status: 200,
                latency_ms,
                checked_at_ns,
                committed_tail: status.map(|s| s.committed_tail),
                leader_tail: None,
                replication_lag_lsn: None,
                readable: probe.readable,
                reason: "ok".to_string(),
            }
        }
        Ok((status, body)) => RemoteReplicaHealth {
            shard_id: probe.shard_id.clone(),
            replica_id: probe.replica_id.clone(),
            addr: probe.addr.clone(),
            health: "suspect".to_string(),
            http_status: status,
            latency_ms,
            checked_at_ns,
            committed_tail: None,
            leader_tail: None,
            replication_lag_lsn: None,
            readable: false,
            reason: compact_error_body(&body),
        },
        Err(error) => RemoteReplicaHealth {
            shard_id: probe.shard_id.clone(),
            replica_id: probe.replica_id.clone(),
            addr: probe.addr.clone(),
            health: "unreachable".to_string(),
            http_status: 0,
            latency_ms,
            checked_at_ns,
            committed_tail: None,
            leader_tail: None,
            replication_lag_lsn: None,
            readable: false,
            reason: error,
        },
    }
}

fn apply_replication_lag(health: &mut [RemoteReplicaHealth], probes: &[RemoteReplicaProbe]) {
    let mut leader_tail_by_shard = std::collections::BTreeMap::<String, u64>::new();
    for item in health.iter() {
        let Some(tail) = item.committed_tail else {
            continue;
        };
        let Some(probe) = probes.iter().find(|probe| {
            probe.shard_id == item.shard_id && probe.replica_id == item.replica_id
        }) else {
            continue;
        };
        if probe.writable || matches!(probe.role, RemoteShardRouteRole::Leader) {
            leader_tail_by_shard.insert(item.shard_id.clone(), tail);
        }
    }
    for item in health.iter_mut() {
        let Some(leader_tail) = leader_tail_by_shard.get(&item.shard_id).copied() else {
            continue;
        };
        item.leader_tail = Some(leader_tail);
        if let Some(tail) = item.committed_tail {
            if tail > leader_tail {
                item.replication_lag_lsn = Some(0);
                item.health = "diverged".to_string();
                item.readable = false;
                item.reason = "replica_tail_after_leader".to_string();
                continue;
            }
            let lag = leader_tail - tail;
            item.replication_lag_lsn = Some(lag);
            let Some(probe) = probes.iter().find(|probe| {
                probe.shard_id == item.shard_id && probe.replica_id == item.replica_id
            }) else {
                continue;
            };
            let max_lag = probe.max_lag_lsn.unwrap_or(0);
            if item.health == "healthy" && lag > max_lag && !probe.writable {
                item.health = "stale".to_string();
                item.readable = false;
                item.reason = "lag_exceeds_budget".to_string();
            }
        }
    }
}

fn remote_replication_status_from_body(body: &str) -> Option<crate::ReplicationStatus> {
    let root = crate::wire::parse(body).ok()?;
    if let Some(committed_tail) =
        json_field_alias(&root, &["committedTail", "committed_tail"]).and_then(crate::wire::Json::as_u64)
    {
        return Some(crate::ReplicationStatus {
            committed_tail,
            manifest_version: json_field_alias(&root, &["manifestVersion", "manifest_version"])
                .and_then(crate::wire::Json::as_u64)
                .unwrap_or(0),
            memtable_watermark: json_field_alias(&root, &["memtableWatermark", "memtable_watermark"])
                .and_then(crate::wire::Json::as_u64)
                .unwrap_or(0),
            memtable_rows: json_field_alias(&root, &["memtableRows", "memtable_rows"])
                .and_then(crate::wire::Json::as_u64)
                .unwrap_or(0) as usize,
            segment_count: json_field_alias(&root, &["segmentCount", "segment_count"])
                .and_then(crate::wire::Json::as_u64)
                .unwrap_or(0) as usize,
        });
    }
    parse_remote_replication_status(body)
}

fn route_health_fields(health: Option<&RemoteReplicaHealth>) -> String {
    let Some(health) = health else {
        return r#","health":"unknown""#.to_string();
    };
    format!(
        r#","health":"{}","lastHttpStatus":{},"latencyMs":{},"replicationLagLsn":{},"leaderTail":{},"healthReason":"{}""#,
        gateway_json_escape(&health.health),
        health.http_status,
        health.latency_ms,
        opt_u64_json(health.replication_lag_lsn),
        opt_u64_json(health.leader_tail),
        gateway_json_escape(&health.reason)
    )
}

fn replica_health_key(shard_id: &str, replica_id: &str) -> String {
    format!("{shard_id}/{replica_id}")
}

fn opt_u64_json(value: Option<u64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_string())
}
