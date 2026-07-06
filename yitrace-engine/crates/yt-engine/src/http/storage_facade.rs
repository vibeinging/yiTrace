use super::*;

#[derive(Clone)]
pub(super) struct ShardReplicaBackend {
    pub(super) id: String,
    pub(super) coord: Arc<WriteCoordinator>,
    pub(super) client: Arc<dyn ShardClient>,
    pub(super) max_lag_lsn: u64,
}

#[derive(Clone)]
pub(super) struct ShardBackend {
    pub(super) id: ShardId,
    pub(super) coord: Arc<WriteCoordinator>,
    pub(super) client: Arc<dyn ShardClient>,
    pub(super) replicas: Vec<ShardReplicaBackend>,
}

type OwnerKey = (Option<u64>, u64);

pub(super) trait TraceStorage: Send + Sync {
    fn mode(&self) -> StorageMode;
    fn primary_coord(&self) -> &Arc<WriteCoordinator>;
    fn shards(&self) -> &[ShardBackend];
    fn ingest_records_for_tenant(&self, recs: Vec<crate::WireRecord>, tenant: Option<u64>);
    fn shard_index_for_record(&self, tenant: Option<u64>, rec: &crate::WireRecord) -> usize;
    fn remember_owner(
        &self,
        tenant: Option<u64>,
        trace_id: u64,
        session_id: Option<u64>,
        idx: usize,
    );
    fn trace_owner_index(&self, tenant: Option<u64>, trace_id: u64) -> Option<usize>;
    fn session_owner_index(&self, tenant: Option<u64>, session_id: u64) -> Option<usize>;
    fn trace_detail_owner_index(&self, tenant: Option<u64>, trace_id: u64) -> Option<usize>;
    fn session_detail_owner_index(&self, tenant: Option<u64>, session_id: u64) -> Option<usize>;

    fn shard_count(&self) -> usize {
        self.shards().len()
    }

    fn metadata_owner_index_for_trace(&self, tenant: Option<u64>, trace_id: u64) -> usize {
        let len = self.shard_count().max(1);
        self.trace_detail_owner_index(tenant, trace_id)
            .unwrap_or_else(|| (route_hash(tenant, None, trace_id) as usize) % len)
            .min(len - 1)
    }
}

pub(super) struct LocalTraceStorage {
    mode: StorageMode,
    shards: Vec<ShardBackend>,
    router: ShardRouter,
}

impl LocalTraceStorage {
    pub(super) fn new_single(coord: Arc<WriteCoordinator>, shard_id: ShardId) -> Self {
        let client: Arc<dyn ShardClient> = Arc::new(LocalShardClient::new(Arc::clone(&coord)));
        Self {
            mode: StorageMode::SingleNode,
            shards: vec![ShardBackend {
                id: shard_id,
                client,
                coord,
                replicas: Vec::new(),
            }],
            router: ShardRouter::default(),
        }
    }

    pub(super) fn new_in_process_cluster(
        shards: Vec<(ShardId, Arc<WriteCoordinator>)>,
    ) -> Result<Self, String> {
        if shards.is_empty() {
            return Err("in-process cluster requires at least one shard".to_string());
        }
        Ok(Self {
            mode: StorageMode::InProcessCluster,
            shards: shards
                .into_iter()
                .map(|(id, coord)| {
                    let client: Arc<dyn ShardClient> =
                        Arc::new(LocalShardClient::new(Arc::clone(&coord)));
                    ShardBackend {
                        id,
                        client,
                        coord,
                        replicas: Vec::new(),
                    }
                })
                .collect(),
            router: ShardRouter::default(),
        })
    }

    pub(super) fn new_in_process_cluster_with_replicas(
        shards: Vec<InProcessShardSpec>,
    ) -> Result<Self, String> {
        if shards.is_empty() {
            return Err("in-process cluster requires at least one shard".to_string());
        }
        Ok(Self {
            mode: StorageMode::InProcessCluster,
            shards: shards
                .into_iter()
                .map(|spec| {
                    let shard_id = spec.shard_id;
                    let leader = spec.leader;
                    let client: Arc<dyn ShardClient> =
                        Arc::new(LocalShardClient::new(Arc::clone(&leader)));
                    let replicas = spec
                        .replicas
                        .into_iter()
                        .enumerate()
                        .map(|(idx, replica)| {
                            let id = if replica.replica_id.trim().is_empty() {
                                format!("{}-replica-{idx}", shard_id.as_str())
                            } else {
                                replica.replica_id
                            };
                            ShardReplicaBackend {
                                id,
                                client: Arc::new(LocalShardClient::new(Arc::clone(&replica.coord))),
                                coord: replica.coord,
                                max_lag_lsn: replica.max_lag_lsn,
                            }
                        })
                        .collect();
                    ShardBackend {
                        id: shard_id,
                        client,
                        coord: leader,
                        replicas,
                    }
                })
                .collect(),
            router: ShardRouter::default(),
        })
    }
}

impl TraceStorage for LocalTraceStorage {
    fn mode(&self) -> StorageMode {
        self.mode
    }

    fn primary_coord(&self) -> &Arc<WriteCoordinator> {
        &self.shards[0].coord
    }

    fn shards(&self) -> &[ShardBackend] {
        &self.shards
    }

    fn ingest_records_for_tenant(&self, recs: Vec<crate::WireRecord>, tenant: Option<u64>) {
        if !matches!(self.mode, StorageMode::InProcessCluster) {
            self.primary_coord().ingest_wire_for_tenant(recs, tenant);
            return;
        }

        let mut grouped: Vec<Vec<_>> = (0..self.shards().len()).map(|_| Vec::new()).collect();
        for rec in recs {
            let idx = self.shard_index_for_record(tenant, &rec);
            self.remember_owner(tenant, rec.trace_id, rec.session_id, idx);
            grouped[idx].push(rec);
        }
        for (idx, batch) in grouped.into_iter().enumerate() {
            if !batch.is_empty() {
                self.shards()[idx]
                    .client
                    .ingest_wire_for_tenant(batch, tenant)
                    .expect("local shard ingest should not fail");
            }
        }
    }

    fn shard_index_for_record(&self, tenant: Option<u64>, rec: &crate::WireRecord) -> usize {
        self.router.shard_index_for_record(
            tenant,
            rec.session_id,
            rec.trace_id,
            self.shards().len(),
        )
    }

    fn remember_owner(
        &self,
        tenant: Option<u64>,
        trace_id: u64,
        session_id: Option<u64>,
        idx: usize,
    ) {
        self.router
            .remember_owner(tenant, trace_id, session_id, idx, self.shards().len());
    }

    fn trace_owner_index(&self, tenant: Option<u64>, trace_id: u64) -> Option<usize> {
        self.router
            .trace_owner_index(tenant, trace_id, self.shards().len())
    }

    fn session_owner_index(&self, tenant: Option<u64>, session_id: u64) -> Option<usize> {
        self.router
            .session_owner_index(tenant, session_id, self.shards().len())
    }

    fn trace_detail_owner_index(&self, tenant: Option<u64>, trace_id: u64) -> Option<usize> {
        if let Some(idx) = self.trace_owner_index(tenant, trace_id) {
            return Some(idx);
        }
        for (idx, shard) in self.shards().iter().enumerate() {
            let snap = shard.coord.pin_snapshot();
            let spans = shard
                .coord
                .console_trace_spans_for_tenant(&snap, trace_id, tenant);
            if !spans.is_empty() {
                self.remember_owner(tenant, trace_id, None, idx);
                return Some(idx);
            }
        }
        None
    }

    fn session_detail_owner_index(&self, tenant: Option<u64>, session_id: u64) -> Option<usize> {
        if let Some(idx) = self.session_owner_index(tenant, session_id) {
            return Some(idx);
        }
        for (idx, shard) in self.shards().iter().enumerate() {
            let snap = shard.coord.pin_snapshot();
            let mut q = TraceQuery::all();
            q.tenant_id = tenant;
            let tl = shard
                .coord
                .load_session_timeline_query(&snap, session_id, &q);
            if !tl.turns.is_empty() {
                for turn in &tl.turns {
                    self.remember_owner(tenant, turn.trace_id, Some(session_id), idx);
                }
                return Some(idx);
            }
        }
        None
    }
}

#[derive(Default, Debug)]
pub(super) struct ShardRouter {
    trace_owner: Mutex<std::collections::HashMap<OwnerKey, usize>>,
    session_owner: Mutex<std::collections::HashMap<OwnerKey, usize>>,
}

impl ShardRouter {
    pub(super) fn shard_index_for_record(
        &self,
        tenant: Option<u64>,
        session_id: Option<u64>,
        trace_id: u64,
        shard_count: usize,
    ) -> usize {
        let len = shard_count.max(1);
        if let Some(idx) = self.trace_owner_index(tenant, trace_id, len) {
            return idx.min(len - 1);
        }
        if let Some(session_id) = session_id {
            if let Some(idx) = self.session_owner_index(tenant, session_id, len) {
                return idx.min(len - 1);
            }
        }
        (route_hash(tenant, session_id, trace_id) as usize) % len
    }

    pub(super) fn remember_owner(
        &self,
        tenant: Option<u64>,
        trace_id: u64,
        session_id: Option<u64>,
        idx: usize,
        shard_count: usize,
    ) {
        if idx >= shard_count {
            return;
        }
        self.trace_owner
            .lock()
            .unwrap()
            .insert((tenant, trace_id), idx);
        if let Some(session_id) = session_id {
            self.session_owner
                .lock()
                .unwrap()
                .insert((tenant, session_id), idx);
        }
    }

    pub(super) fn clear(&self) {
        self.trace_owner.lock().unwrap().clear();
        self.session_owner.lock().unwrap().clear();
    }

    pub(super) fn trace_owner_index(
        &self,
        tenant: Option<u64>,
        trace_id: u64,
        shard_count: usize,
    ) -> Option<usize> {
        self.trace_owner
            .lock()
            .unwrap()
            .get(&(tenant, trace_id))
            .copied()
            .filter(|idx| *idx < shard_count)
    }

    pub(super) fn session_owner_index(
        &self,
        tenant: Option<u64>,
        session_id: u64,
        shard_count: usize,
    ) -> Option<usize> {
        self.session_owner
            .lock()
            .unwrap()
            .get(&(tenant, session_id))
            .copied()
            .filter(|idx| *idx < shard_count)
    }
}

fn route_hash(tenant: Option<u64>, session_id: Option<u64>, trace_id: u64) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    fn mix(mut hash: u64, value: u64) -> u64 {
        for b in value.to_le_bytes() {
            hash ^= b as u64;
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        hash
    }
    let mut hash = FNV_OFFSET;
    hash = mix(hash, tenant.unwrap_or(0));
    match session_id {
        Some(session) => {
            hash = mix(hash, 1);
            mix(hash, session)
        }
        None => {
            hash = mix(hash, 0);
            mix(hash, trace_id)
        }
    }
}

pub(super) fn cluster_metadata_id_base(shard_idx: usize) -> u64 {
    ((shard_idx as u64) + 1) << 56
}
