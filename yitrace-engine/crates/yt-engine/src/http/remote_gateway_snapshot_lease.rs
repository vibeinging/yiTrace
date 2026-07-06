use self::snapshot_helpers::{
    snapshot_lease_deadline_ns, snapshot_lease_ttl_ns_from_body, snapshot_now_ns,
    DEFAULT_SNAPSHOT_LEASE_TTL_NS,
};

#[derive(Debug)]
struct RemoteGatewaySnapshotLeaseBook {
    next_seq: u64,
    leases: std::collections::BTreeMap<String, RemoteGatewaySnapshotLeaseEntry>,
    max_entries: usize,
}

#[derive(Clone, Debug)]
struct RemoteGatewaySnapshotLeaseEntry {
    snapshot: RemoteGatewaySnapshot,
    last_used_seq: u64,
    expires_at_ns: u128,
}

impl Default for RemoteGatewaySnapshotLeaseBook {
    fn default() -> Self {
        Self {
            next_seq: 1,
            leases: std::collections::BTreeMap::new(),
            max_entries: 64,
        }
    }
}

impl RemoteGatewaySnapshotLeaseBook {
    fn insert(
        &mut self,
        mut snapshot: RemoteGatewaySnapshot,
        ttl_ns: u128,
    ) -> (RemoteGatewaySnapshot, u128) {
        let seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);
        let lease_id = format!("remote-lease-{seq}");
        snapshot.lease_id = Some(lease_id.clone());
        let expires_at_ns = snapshot_lease_deadline_ns(ttl_ns);
        self.leases.insert(
            lease_id.clone(),
            RemoteGatewaySnapshotLeaseEntry {
                snapshot: snapshot.clone(),
                last_used_seq: seq,
                expires_at_ns,
            },
        );
        while self.leases.len() > self.max_entries.max(1) {
            let Some(evict_id) = self
                .leases
                .iter()
                .filter(|(id, _)| id.as_str() != lease_id)
                .min_by_key(|(_, entry)| entry.last_used_seq)
                .map(|(id, _)| id.clone())
            else {
                break;
            };
            self.leases.remove(&evict_id);
        }
        (snapshot, expires_at_ns)
    }

    fn lookup(&mut self, lease_id: &str) -> Option<RemoteGatewaySnapshot> {
        let seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);
        self.purge_expired();
        let entry = self.leases.get_mut(lease_id)?;
        entry.last_used_seq = seq;
        Some(entry.snapshot.clone())
    }

    fn replace(
        &mut self,
        lease_id: &str,
        mut snapshot: RemoteGatewaySnapshot,
        ttl_ns: u128,
    ) -> Option<u128> {
        let seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);
        self.purge_expired();
        snapshot.lease_id = Some(lease_id.to_string());
        let Some(entry) = self.leases.get_mut(lease_id) else {
            return None;
        };
        let expires_at_ns = snapshot_lease_deadline_ns(ttl_ns);
        entry.snapshot = snapshot;
        entry.last_used_seq = seq;
        entry.expires_at_ns = expires_at_ns;
        Some(expires_at_ns)
    }

    fn release(&mut self, lease_id: &str) -> Option<RemoteGatewaySnapshot> {
        self.purge_expired();
        self.leases.remove(lease_id).map(|entry| entry.snapshot)
    }

    fn clear(&mut self) {
        self.leases.clear();
    }

    fn purge_expired(&mut self) {
        let now = snapshot_now_ns();
        self.leases
            .retain(|_, entry| entry.expires_at_ns > now);
    }
}

impl RemoteShardGateway {
    fn remote_snapshot_lookup(
        &self,
        lease_id: &str,
    ) -> Result<RemoteGatewaySnapshot, (u16, String)> {
        self.remote_snapshot_leases
            .lock()
            .ok()
            .and_then(|mut leases| leases.lookup(lease_id))
            .ok_or_else(|| {
                remote_snapshot_error(
                    409,
                    "snapshot_expired",
                    "remote gateway snapshot lease is expired or released",
                )
            })
    }

    fn remote_snapshot_lease_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        let ttl_ns = snapshot_lease_ttl_ns_from_body(body).unwrap_or(DEFAULT_SNAPSHOT_LEASE_TTL_NS);
        let force_leader = remote_snapshot_lease_requires_leader(body);
        let targets = match self.read_targets_snapshot(force_leader, None) {
            Ok(targets) => targets,
            Err(error) => return error,
        };
        let mut shards = Vec::new();
        let mut failed = Vec::new();
        for target in &targets {
            match target.client.route_json_with_tenant(
                "POST",
                "/v1/snapshots/lease",
                body,
                tenant,
            ) {
                Ok((200, response)) => {
                    let Some(snapshot) = gateway_snapshot_from_body(&response) else {
                        failed.push(format!(
                            r#"{{"shard":{},"error":"missing shard snapshot"}}"#,
                            target.index
                        ));
                        continue;
                    };
                    shards.push(RemoteGatewaySnapshotShard {
                        index: target.index,
                        shard_id: Some(target.shard_id.clone()),
                        replica_id: Some(target.replica_id.clone()),
                        snapshot,
                    });
                }
                Ok((status, response)) => failed.push(format!(
                    r#"{{"shard":{},"status":{},"error":{}}}"#,
                    target.index,
                    status,
                    json_string_value(&response)
                )),
                Err(error) => failed.push(format!(
                    r#"{{"shard":{},"status":0,"error":{}}}"#,
                    target.index,
                    json_string_value(&error)
                )),
            }
        }
        if !failed.is_empty() || shards.len() != targets.len() {
            remote_release_local_snapshot_leases(&targets, &shards, tenant);
            return (
                503,
                format!(
                    r#"{{"error":"remote snapshot lease failed","failedShards":[{}]}}"#,
                    failed.join(",")
                ),
            );
        }
        shards.sort_by_key(|shard| shard.index);
        let snapshot = RemoteGatewaySnapshot {
            lease_id: None,
            route_table_version: self.route_table_version(),
            shards,
        };
        let (leased, expires_at_ns) = match self.remote_snapshot_leases.lock() {
            Ok(mut leases) => leases.insert(snapshot, ttl_ns),
            Err(_) => {
                return (
                    503,
                    r#"{"error":"remote snapshot lease book unavailable"}"#.to_string(),
                )
            }
        };
        (
            200,
            format!(
                r#"{{"snapshot":{},"leaseId":{},"leaseState":"active","expiresAtNs":"{}","readTargets":[{}]}}"#,
                remote_gateway_snapshot_json(&leased),
                json_opt_str(leased.lease_id.as_deref()),
                expires_at_ns,
                remote_read_targets_json(&targets)
            ),
        )
    }

    fn remote_snapshot_renew_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        let Some(lease_id) = remote_snapshot_lease_id_from_body(body) else {
            return (
                400,
                r#"{"error":"leaseId is required","code":"bad_snapshot_lease"}"#.to_string(),
            );
        };
        let ttl_ns = snapshot_lease_ttl_ns_from_body(body).unwrap_or(DEFAULT_SNAPSHOT_LEASE_TTL_NS);
        let snapshot = match self.remote_snapshot_lookup(&lease_id) {
            Ok(snapshot) => snapshot,
            Err(error) => return error,
        };
        if let Err(error) = self.validate_remote_snapshot(&snapshot) {
            return error;
        }
        let targets = match self.read_targets_snapshot(false, Some(&snapshot)) {
            Ok(targets) => targets,
            Err(error) => return error,
        };
        let mut renewed_shards = Vec::new();
        let mut failed = Vec::new();
        for target in &targets {
            let Some(local_snapshot) = snapshot.snapshot_for(target.index) else {
                failed.push(format!(
                    r#"{{"shard":{},"error":"missing local snapshot"}}"#,
                    target.index
                ));
                continue;
            };
            let Some(local_lease_id) = remote_local_lease_id(local_snapshot) else {
                failed.push(format!(
                    r#"{{"shard":{},"error":"missing local leaseId"}}"#,
                    target.index
                ));
                continue;
            };
            let renew_body = format!(
                r#"{{"leaseId":"{}","ttlNs":{}}}"#,
                gateway_json_escape(&local_lease_id),
                ttl_ns
            );
            match target.client.route_json_with_tenant(
                "POST",
                "/v1/snapshots/renew",
                &renew_body,
                tenant,
            ) {
                Ok((200, response)) => {
                    let snapshot =
                        gateway_snapshot_from_body(&response).unwrap_or_else(|| local_snapshot.clone());
                    renewed_shards.push(RemoteGatewaySnapshotShard {
                        index: target.index,
                        shard_id: Some(target.shard_id.clone()),
                        replica_id: Some(target.replica_id.clone()),
                        snapshot,
                    });
                }
                Ok((409, response)) => {
                    if let Ok(mut leases) = self.remote_snapshot_leases.lock() {
                        leases.release(&lease_id);
                    }
                    return (
                        409,
                        format!(
                            r#"{{"error":"remote snapshot error","code":"snapshot_expired","message":{}}}"#,
                            json_string_value(&response)
                        ),
                    );
                }
                Ok((status, response)) => failed.push(format!(
                    r#"{{"shard":{},"status":{},"error":{}}}"#,
                    target.index,
                    status,
                    json_string_value(&response)
                )),
                Err(error) => failed.push(format!(
                    r#"{{"shard":{},"status":0,"error":{}}}"#,
                    target.index,
                    json_string_value(&error)
                )),
            }
        }
        if !failed.is_empty() || renewed_shards.len() != targets.len() {
            return (
                503,
                format!(
                    r#"{{"error":"remote snapshot renew failed","failedShards":[{}]}}"#,
                    failed.join(",")
                ),
            );
        }
        renewed_shards.sort_by_key(|shard| shard.index);
        let renewed = RemoteGatewaySnapshot {
            lease_id: Some(lease_id.clone()),
            route_table_version: self.route_table_version(),
            shards: renewed_shards,
        };
        let expires_at_ns = match self.remote_snapshot_leases.lock() {
            Ok(mut leases) => match leases.replace(&lease_id, renewed.clone(), ttl_ns) {
                Some(expires_at_ns) => expires_at_ns,
                None => {
                    return remote_snapshot_error(
                        409,
                        "snapshot_expired",
                        "remote gateway snapshot lease is expired or released",
                    )
                }
            },
            Err(_) => snapshot_lease_deadline_ns(ttl_ns),
        };
        (
            200,
            format!(
                r#"{{"snapshot":{},"leaseId":{},"leaseState":"active","expiresAtNs":"{}","readTargets":[{}]}}"#,
                remote_gateway_snapshot_json(&renewed),
                json_opt_str(Some(&lease_id)),
                expires_at_ns,
                remote_read_targets_json(&targets)
            ),
        )
    }

    fn remote_snapshot_release_json(&self, lease_id: &str, tenant: Option<u64>) -> (u16, String) {
        let snapshot = self
            .remote_snapshot_leases
            .lock()
            .ok()
            .and_then(|mut leases| leases.release(lease_id));
        let Some(snapshot) = snapshot else {
            return (
                200,
                format!(
                    r#"{{"released":false,"leaseId":{},"leaseState":"released"}}"#,
                    json_opt_str(Some(lease_id))
                ),
            );
        };
        let targets = self
            .read_targets_snapshot(false, Some(&snapshot))
            .unwrap_or_default();
        let mut released = 0usize;
        for target in &targets {
            let Some(local_snapshot) = snapshot.snapshot_for(target.index) else {
                continue;
            };
            let Some(local_lease_id) = remote_local_lease_id(local_snapshot) else {
                continue;
            };
            let path = format!("/v1/snapshots/{local_lease_id}");
            if matches!(
                target
                    .client
                    .route_json_with_tenant("DELETE", &path, "", tenant),
                Ok((200, _))
            ) {
                released += 1;
            }
        }
        (
            200,
            format!(
                r#"{{"released":true,"leaseId":{},"leaseState":"released","releasedShards":{},"shardCount":{}}}"#,
                json_opt_str(Some(lease_id)),
                released,
                snapshot.shards.len()
            ),
        )
    }
}

fn remote_gateway_snapshot_json(snapshot: &RemoteGatewaySnapshot) -> String {
    let lease = snapshot
        .lease_id
        .as_deref()
        .map(|lease_id| format!(r#","leaseId":"{}""#, gateway_json_escape(lease_id)))
        .unwrap_or_default();
    let version = snapshot
        .route_table_version
        .map(|version| format!(r#","routeTableVersion":{version}"#))
        .unwrap_or_default();
    let shards = snapshot
        .shards
        .iter()
        .map(|shard| {
            let shard_id = shard
                .shard_id
                .as_deref()
                .map(|id| format!(r#","shardId":"{}""#, gateway_json_escape(id)))
                .unwrap_or_default();
            let replica_id = shard
                .replica_id
                .as_deref()
                .map(|id| format!(r#","replicaId":"{}""#, gateway_json_escape(id)))
                .unwrap_or_default();
            format!(
                r#"{{"shard":{}{}{},"snapshot":{}}}"#,
                shard.index,
                shard_id,
                replica_id,
                shard.snapshot.to_compact_json()
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(r#"{{"mode":"remote_gateway"{lease}{version},"shards":[{shards}]}}"#)
}

fn remote_snapshot_lease_id_from_body(body: &str) -> Option<String> {
    crate::wire::parse(body).ok().and_then(|value| {
        json_field_alias(&value, &["leaseId", "lease_id", "id"])
            .and_then(crate::wire::Json::as_str)
            .map(ToString::to_string)
    })
}

fn remote_snapshot_lease_requires_leader(body: &str) -> bool {
    let eventual = crate::wire::parse(body)
        .ok()
        .and_then(|value| {
            json_field_alias(&value, &["consistency", "readConsistency"])
                .and_then(crate::wire::Json::as_str)
                .map(|value| {
                    value.eq_ignore_ascii_case("eventual")
                        || value.eq_ignore_ascii_case("bounded_stale")
                        || value.eq_ignore_ascii_case("bounded-stale")
                })
        })
        .unwrap_or(false);
    !eventual
}

fn remote_local_lease_id(snapshot: &crate::wire::Json) -> Option<String> {
    let source = json_field_alias(snapshot, &["snapshot"]).unwrap_or(snapshot);
    json_field_alias(source, &["leaseId", "lease_id"])
        .and_then(crate::wire::Json::as_str)
        .map(ToString::to_string)
}

fn remote_release_local_snapshot_leases(
    targets: &[RemoteReadTarget],
    shards: &[RemoteGatewaySnapshotShard],
    tenant: Option<u64>,
) {
    for shard in shards {
        let Some(local_lease_id) = remote_local_lease_id(&shard.snapshot) else {
            continue;
        };
        let Some(target) = targets.iter().find(|target| target.index == shard.index) else {
            continue;
        };
        let path = format!("/v1/snapshots/{local_lease_id}");
        let _ = target
            .client
            .route_json_with_tenant("DELETE", &path, "", tenant);
    }
}
