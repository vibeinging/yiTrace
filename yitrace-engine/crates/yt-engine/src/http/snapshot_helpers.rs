use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

const MAX_CLUSTER_SNAPSHOT_LEASES: usize = 64;
pub(super) const DEFAULT_SNAPSHOT_LEASE_TTL_NS: u128 = 300_000_000_000;

#[derive(Clone, Debug, PartialEq, Eq)]
struct ClusterSnapshotShard {
    shard_id: String,
    read_target: Option<String>,
    manifest_version: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ClusterSnapshot {
    lease_id: Option<String>,
    shards: Vec<ClusterSnapshotShard>,
}

impl ClusterSnapshot {
    fn to_json(&self) -> String {
        let lease = self
            .lease_id
            .as_deref()
            .map(|id| format!(r#","leaseId":"{}""#, json_escape(id)))
            .unwrap_or_default();
        let shards = self
            .shards
            .iter()
            .map(|shard| {
                let target = shard
                    .read_target
                    .as_deref()
                    .map(|target| format!(r#","readTarget":"{}""#, json_escape(target)))
                    .unwrap_or_default();
                format!(
                    r#"{{"shardId":"{}"{},"manifestVersion":{}}}"#,
                    json_escape(&shard.shard_id),
                    target,
                    shard.manifest_version
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        format!(
            r#"{{"mode":"in_process_cluster"{},"shards":[{}]}}"#,
            lease, shards
        )
    }

    fn same_shards(&self, other: &Self) -> bool {
        self.shards == other.shards
    }
}

#[derive(Clone)]
struct ClusterSnapshotRead {
    coord: Arc<WriteCoordinator>,
    snapshot: Arc<yt_manifest::Snapshot>,
}

#[derive(Clone)]
pub(super) struct ClusterSnapshotReadSet {
    token: ClusterSnapshot,
    reads: Vec<ClusterSnapshotRead>,
}

impl ClusterSnapshotReadSet {
    pub(super) fn snapshot_at(&self, idx: usize) -> &yt_manifest::Snapshot {
        &self.reads[idx].snapshot
    }

    pub(super) fn coord_at(&self, idx: usize) -> &Arc<WriteCoordinator> {
        &self.reads[idx].coord
    }

    pub(super) fn snapshot_field(&self) -> String {
        format!(r#","snapshot":{}"#, self.token.to_json())
    }

    pub(super) fn cache_fingerprint(&self) -> String {
        self.token
            .shards
            .iter()
            .zip(self.reads.iter())
            .map(|(shard, read)| {
                format!(
                    "{}@{}@{}@{}",
                    shard.shard_id,
                    shard.read_target.as_deref().unwrap_or("leader"),
                    shard.manifest_version,
                    read.coord.read_model_revision()
                )
            })
            .collect::<Vec<_>>()
            .join("|")
    }
}

pub(super) struct SnapshotLeaseBook {
    next_seq: AtomicU64,
    leases: Mutex<std::collections::BTreeMap<String, SnapshotLeaseEntry>>,
    max_entries: usize,
}

struct SnapshotLeaseEntry {
    token: ClusterSnapshot,
    reads: Vec<ClusterSnapshotRead>,
    last_used_seq: u64,
    expires_at_ns: u128,
}

impl Default for SnapshotLeaseBook {
    fn default() -> Self {
        Self {
            next_seq: AtomicU64::new(1),
            leases: Mutex::new(std::collections::BTreeMap::new()),
            max_entries: MAX_CLUSTER_SNAPSHOT_LEASES,
        }
    }
}

impl SnapshotLeaseBook {
    fn insert(
        &self,
        mut token: ClusterSnapshot,
        reads: Vec<ClusterSnapshotRead>,
    ) -> ClusterSnapshotReadSet {
        let seq = self.next_seq.fetch_add(1, Ordering::AcqRel);
        let lease_id = format!("lease-{seq}");
        token.lease_id = Some(lease_id.clone());
        let read_set = ClusterSnapshotReadSet {
            token: token.clone(),
            reads: reads.clone(),
        };
        let mut leases = self.leases.lock().unwrap();
        let expires_at_ns = snapshot_lease_deadline_ns(DEFAULT_SNAPSHOT_LEASE_TTL_NS);
        leases.insert(
            lease_id.clone(),
            SnapshotLeaseEntry {
                token,
                reads,
                last_used_seq: seq,
                expires_at_ns,
            },
        );
        while leases.len() > self.max_entries.max(1) {
            let Some(evict_id) = leases
                .iter()
                .filter(|(id, _)| id.as_str() != lease_id)
                .min_by_key(|(_, entry)| entry.last_used_seq)
                .map(|(id, _)| id.clone())
            else {
                break;
            };
            leases.remove(&evict_id);
        }
        read_set
    }

    fn lookup(&self, lease_id: &str) -> Option<ClusterSnapshotReadSet> {
        let seq = self.next_seq.fetch_add(1, Ordering::AcqRel);
        let mut leases = self.leases.lock().unwrap();
        purge_expired_snapshot_leases(&mut leases);
        let entry = leases.get_mut(lease_id)?;
        entry.last_used_seq = seq;
        Some(ClusterSnapshotReadSet {
            token: entry.token.clone(),
            reads: entry.reads.clone(),
        })
    }

    fn renew(&self, lease_id: &str, ttl_ns: u128) -> Option<(ClusterSnapshotReadSet, u128)> {
        let seq = self.next_seq.fetch_add(1, Ordering::AcqRel);
        let mut leases = self.leases.lock().unwrap();
        purge_expired_snapshot_leases(&mut leases);
        let entry = leases.get_mut(lease_id)?;
        entry.last_used_seq = seq;
        entry.expires_at_ns = snapshot_lease_deadline_ns(ttl_ns);
        Some((
            ClusterSnapshotReadSet {
                token: entry.token.clone(),
                reads: entry.reads.clone(),
            },
            entry.expires_at_ns,
        ))
    }

    fn release(&self, lease_id: &str) -> bool {
        let mut leases = self.leases.lock().unwrap();
        purge_expired_snapshot_leases(&mut leases);
        leases.remove(lease_id).is_some()
    }
}

impl EngineJsonApi {
    pub(super) fn snapshot_lease_json(&self, body: &str) -> (u16, String) {
        let ttl_ns = snapshot_lease_ttl_ns_from_body(body).unwrap_or(DEFAULT_SNAPSHOT_LEASE_TTL_NS);
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
        let read_set = if eventual {
            self.pin_eventually_consistent_cluster_snapshot_read_set()
        } else {
            self.pin_cluster_snapshot_read_set()
        };
        let expires_at_ns = read_set
            .token
            .lease_id
            .as_deref()
            .and_then(|lease_id| self.snapshot_leases.renew(lease_id, ttl_ns))
            .map(|(_, expires_at_ns)| expires_at_ns)
            .unwrap_or_else(|| snapshot_lease_deadline_ns(ttl_ns));
        (
            200,
            format!(
                r#"{{"snapshot":{},"leaseId":{},"leaseState":"active","expiresAtNs":"{}"}}"#,
                read_set.token.to_json(),
                json_opt_str(read_set.token.lease_id.as_deref()),
                expires_at_ns
            ),
        )
    }

    pub(super) fn snapshot_renew_json(&self, body: &str) -> (u16, String) {
        let lease_id = match lease_id_from_body(body) {
            Some(lease_id) => lease_id,
            None => {
                return (
                    400,
                    r#"{"error":"leaseId is required","code":"bad_snapshot_lease"}"#.to_string(),
                )
            }
        };
        let ttl_ns = snapshot_lease_ttl_ns_from_body(body).unwrap_or(DEFAULT_SNAPSHOT_LEASE_TTL_NS);
        let Some((read_set, expires_at_ns)) = self.snapshot_leases.renew(&lease_id, ttl_ns) else {
            return snapshot_expired_error(
                &ClusterSnapshot {
                    lease_id: Some(lease_id),
                    shards: Vec::new(),
                },
                &self.current_cluster_snapshot(),
            );
        };
        (
            200,
            format!(
                r#"{{"snapshot":{},"leaseId":{},"leaseState":"active","expiresAtNs":"{}"}}"#,
                read_set.token.to_json(),
                json_opt_str(read_set.token.lease_id.as_deref()),
                expires_at_ns
            ),
        )
    }

    pub(super) fn snapshot_release_json(&self, lease_id: &str) -> (u16, String) {
        let released = self.snapshot_leases.release(lease_id);
        (
            200,
            format!(
                r#"{{"released":{},"leaseId":{},"leaseState":"released"}}"#,
                released,
                json_opt_str(Some(lease_id))
            ),
        )
    }

    pub(super) fn current_cluster_snapshot(&self) -> ClusterSnapshot {
        let shards = self
            .shards()
            .iter()
            .map(|shard| {
                let snap = shard.coord.pin_snapshot();
                ClusterSnapshotShard {
                    shard_id: shard.id.as_str().to_string(),
                    read_target: None,
                    manifest_version: snap.manifest.version.get(),
                }
            })
            .collect();
        ClusterSnapshot {
            lease_id: None,
            shards,
        }
    }

    pub(super) fn cluster_snapshot_read_set_from_query(
        &self,
        query: &str,
    ) -> Result<ClusterSnapshotReadSet, (u16, String)> {
        let requested = self.cluster_snapshot_from_query(query)?;
        self.resolve_cluster_snapshot_read_set(requested)
    }

    pub(super) fn cluster_snapshot_read_set_from_body(
        &self,
        value: &crate::wire::Json,
    ) -> Result<ClusterSnapshotReadSet, (u16, String)> {
        let requested = self.cluster_snapshot_from_body(value)?;
        self.resolve_cluster_snapshot_read_set(requested)
    }

    pub(super) fn local_snapshot_read_set_from_body(
        &self,
        value: &crate::wire::Json,
    ) -> Result<ClusterSnapshotReadSet, (u16, String)> {
        self.cluster_snapshot_read_set_from_body(value)
    }

    pub(super) fn eventually_consistent_cluster_snapshot_read_set_from_body(
        &self,
        value: &crate::wire::Json,
    ) -> Result<ClusterSnapshotReadSet, (u16, String)> {
        let requested = self.cluster_snapshot_from_body(value)?;
        self.resolve_eventually_consistent_cluster_snapshot_read_set(requested)
    }

    fn resolve_cluster_snapshot_read_set(
        &self,
        requested: Option<ClusterSnapshot>,
    ) -> Result<ClusterSnapshotReadSet, (u16, String)> {
        if let Some(snapshot) = requested {
            if let Some(lease_id) = snapshot.lease_id.as_deref() {
                let Some(read_set) = self.snapshot_leases.lookup(lease_id) else {
                    return Err(snapshot_expired_error(
                        &snapshot,
                        &self.current_cluster_snapshot(),
                    ));
                };
                if snapshot.same_shards(&read_set.token) {
                    return Ok(read_set);
                }
                return Err(snapshot_mismatch_error(&snapshot, &read_set.token));
            }

            let read_set = self.pin_cluster_snapshot_read_set();
            if snapshot.same_shards(&read_set.token) {
                return Ok(read_set);
            }
            return Err(snapshot_mismatch_error(&snapshot, &read_set.token));
        }
        Ok(self.pin_cluster_snapshot_read_set())
    }

    fn resolve_eventually_consistent_cluster_snapshot_read_set(
        &self,
        requested: Option<ClusterSnapshot>,
    ) -> Result<ClusterSnapshotReadSet, (u16, String)> {
        if let Some(snapshot) = requested {
            if let Some(lease_id) = snapshot.lease_id.as_deref() {
                let Some(read_set) = self.snapshot_leases.lookup(lease_id) else {
                    return Err(snapshot_expired_error(
                        &snapshot,
                        &self.current_cluster_snapshot(),
                    ));
                };
                if snapshot.same_shards(&read_set.token) {
                    return Ok(read_set);
                }
                return Err(snapshot_mismatch_error(&snapshot, &read_set.token));
            }

            let read_set = self.pin_eventually_consistent_cluster_snapshot_read_set();
            if snapshot.same_shards(&read_set.token) {
                return Ok(read_set);
            }
            return Err(snapshot_mismatch_error(&snapshot, &read_set.token));
        }
        Ok(self.pin_eventually_consistent_cluster_snapshot_read_set())
    }

    pub(super) fn pin_cluster_snapshot_read_set(&self) -> ClusterSnapshotReadSet {
        let mut shards = Vec::new();
        let mut reads = Vec::new();
        for shard in self.shards().iter() {
            let snap = Arc::new(shard.coord.pin_snapshot());
            shards.push(ClusterSnapshotShard {
                shard_id: shard.id.as_str().to_string(),
                read_target: None,
                manifest_version: snap.manifest.version.get(),
            });
            reads.push(ClusterSnapshotRead {
                coord: Arc::clone(&shard.coord),
                snapshot: snap,
            });
        }
        self.snapshot_leases.insert(
            ClusterSnapshot {
                lease_id: None,
                shards,
            },
            reads,
        )
    }

    pub(super) fn pin_eventually_consistent_cluster_snapshot_read_set(
        &self,
    ) -> ClusterSnapshotReadSet {
        let mut shards = Vec::new();
        let mut reads = Vec::new();
        for shard in self.shards().iter() {
            let (coord, read_target) = self.eventually_consistent_read_target_for_shard(shard);
            let snap = Arc::new(coord.pin_snapshot());
            shards.push(ClusterSnapshotShard {
                shard_id: shard.id.as_str().to_string(),
                read_target,
                manifest_version: snap.manifest.version.get(),
            });
            reads.push(ClusterSnapshotRead {
                coord: Arc::clone(coord),
                snapshot: snap,
            });
        }
        self.snapshot_leases.insert(
            ClusterSnapshot {
                lease_id: None,
                shards,
            },
            reads,
        )
    }

    fn cluster_snapshot_from_query(
        &self,
        query: &str,
    ) -> Result<Option<ClusterSnapshot>, (u16, String)> {
        for (key, value) in query_pairs(query) {
            if matches!(
                key.as_str(),
                "snapshot" | "snapshotToken" | "snapshot_token"
            ) {
                return parse_cluster_snapshot_str(&value)
                    .map(Some)
                    .map_err(|e| snapshot_parse_error(&e));
            }
        }
        Ok(None)
    }

    fn cluster_snapshot_from_body(
        &self,
        value: &crate::wire::Json,
    ) -> Result<Option<ClusterSnapshot>, (u16, String)> {
        let Some(snapshot_value) =
            json_field_alias(value, &["snapshot", "snapshotToken", "snapshot_token"])
        else {
            return Ok(None);
        };
        match snapshot_value {
            crate::wire::Json::Str(s) => parse_cluster_snapshot_str(s)
                .map(Some)
                .map_err(|e| snapshot_parse_error(&e)),
            other => parse_cluster_snapshot_value(other)
                .map(Some)
                .map_err(|e| snapshot_parse_error(&e)),
        }
    }
}

fn lease_id_from_body(body: &str) -> Option<String> {
    crate::wire::parse(body).ok().and_then(|value| {
        json_field_alias(&value, &["leaseId", "lease_id", "id"])
            .and_then(crate::wire::Json::as_str)
            .map(ToString::to_string)
    })
}

pub(super) fn snapshot_lease_ttl_ns_from_body(body: &str) -> Option<u128> {
    let value = crate::wire::parse(body).ok()?;
    json_field_alias(&value, &["ttlNs", "ttl_ns", "leaseTtlNs", "lease_ttl_ns"])
        .and_then(crate::wire::Json::as_u64)
        .map(|value| value.max(1) as u128)
        .or_else(|| {
            json_field_alias(&value, &["ttlMs", "ttl_ms", "leaseTtlMs", "lease_ttl_ms"])
                .and_then(crate::wire::Json::as_u64)
                .map(|value| (value.max(1) as u128).saturating_mul(1_000_000))
        })
}

pub(super) fn snapshot_lease_deadline_ns(ttl_ns: u128) -> u128 {
    snapshot_now_ns().saturating_add(ttl_ns.max(1))
}

pub(super) fn snapshot_now_ns() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

fn purge_expired_snapshot_leases(
    leases: &mut std::collections::BTreeMap<String, SnapshotLeaseEntry>,
) {
    let now = snapshot_now_ns();
    leases.retain(|_, entry| entry.expires_at_ns > now);
}

fn parse_cluster_snapshot_str(value: &str) -> Result<ClusterSnapshot, String> {
    let parsed = crate::wire::parse(value)?;
    parse_cluster_snapshot_value(&parsed)
}

fn parse_cluster_snapshot_value(value: &crate::wire::Json) -> Result<ClusterSnapshot, String> {
    let source = json_field_alias(value, &["snapshot"]).unwrap_or(value);
    let lease_id = json_field_alias(source, &["leaseId", "lease_id"])
        .and_then(crate::wire::Json::as_str)
        .map(str::to_string);
    let Some(shards_value) = json_field_alias(source, &["shards"]) else {
        return Err("snapshot.shards is required".to_string());
    };
    let crate::wire::Json::Arr(items) = shards_value else {
        return Err("snapshot.shards must be an array".to_string());
    };
    let mut shards = Vec::with_capacity(items.len());
    for (idx, item) in items.iter().enumerate() {
        let shard_id = json_field_alias(item, &["shardId", "shard_id"])
            .and_then(crate::wire::Json::as_str)
            .ok_or_else(|| format!("snapshot.shards[{idx}].shardId is required"))?;
        let manifest_version = json_field_alias(item, &["manifestVersion", "manifest_version"])
            .and_then(crate::wire::Json::as_u64)
            .ok_or_else(|| format!("snapshot.shards[{idx}].manifestVersion is required"))?;
        let read_target = json_field_alias(
            item,
            &["readTarget", "read_target", "replicaId", "replica_id"],
        )
        .and_then(crate::wire::Json::as_str)
        .filter(|target| !target.eq_ignore_ascii_case("leader"))
        .map(str::to_string);
        shards.push(ClusterSnapshotShard {
            shard_id: shard_id.to_string(),
            read_target,
            manifest_version,
        });
    }
    Ok(ClusterSnapshot { lease_id, shards })
}

fn snapshot_parse_error(message: &str) -> (u16, String) {
    (
        400,
        format!(
            r#"{{"error":"bad snapshot token","code":"bad_snapshot","message":"{}"}}"#,
            json_escape(message)
        ),
    )
}

fn snapshot_mismatch_error(got: &ClusterSnapshot, expected: &ClusterSnapshot) -> (u16, String) {
    (
        409,
        format!(
            r#"{{"error":"snapshot mismatch","code":"snapshot_mismatch","snapshot":{},"got":{}}}"#,
            expected.to_json(),
            got.to_json()
        ),
    )
}

fn snapshot_expired_error(got: &ClusterSnapshot, current: &ClusterSnapshot) -> (u16, String) {
    (
        409,
        format!(
            r#"{{"error":"snapshot expired","code":"snapshot_expired","snapshot":{},"got":{}}}"#,
            current.to_json(),
            got.to_json()
        ),
    )
}
