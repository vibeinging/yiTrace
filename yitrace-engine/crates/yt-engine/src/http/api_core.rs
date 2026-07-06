use super::*;

impl EngineJsonApi {
    pub(super) fn coord(&self) -> &Arc<WriteCoordinator> {
        self.storage.primary_coord()
    }

    pub(super) fn shards(&self) -> &[ShardBackend] {
        self.storage.shards()
    }

    pub(super) fn shard_count(&self) -> usize {
        self.storage.shard_count()
    }

    pub(super) fn storage_mode(&self) -> StorageMode {
        self.storage.mode()
    }

    pub(super) fn is_in_process_cluster(&self) -> bool {
        matches!(self.storage.mode(), StorageMode::InProcessCluster)
    }

    pub(super) fn ingest_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        match parse_wire_batch(body) {
            Ok(recs) => {
                let n = recs.len();
                self.ingest_records_for_tenant(recs, tenant);
                (200, format!(r#"{{"ingested":{n}}}"#))
            }
            Err(e) => (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        }
    }

    pub(super) fn ingest_otlp_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        match crate::parse_otlp_traces(body) {
            Ok(recs) => {
                self.ingest_records_for_tenant(recs, tenant);
                (200, r#"{"partialSuccess":{}}"#.to_string())
            }
            Err(e) => (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        }
    }

    pub(super) fn ingest_records_for_tenant(
        &self,
        recs: Vec<crate::WireRecord>,
        tenant: Option<u64>,
    ) {
        self.storage.ingest_records_for_tenant(recs, tenant);
        self.invalidate_read_model_cache();
    }

    pub(super) fn shard_index_for_record(
        &self,
        tenant: Option<u64>,
        rec: &crate::WireRecord,
    ) -> usize {
        self.storage.shard_index_for_record(tenant, rec)
    }

    pub(super) fn remember_owner(
        &self,
        tenant: Option<u64>,
        trace_id: u64,
        session_id: Option<u64>,
        idx: usize,
    ) {
        self.storage
            .remember_owner(tenant, trace_id, session_id, idx);
    }

    pub(super) fn trace_owner_index(&self, tenant: Option<u64>, trace_id: u64) -> Option<usize> {
        self.storage.trace_owner_index(tenant, trace_id)
    }

    pub(super) fn session_owner_index(
        &self,
        tenant: Option<u64>,
        session_id: u64,
    ) -> Option<usize> {
        self.storage.session_owner_index(tenant, session_id)
    }

    pub(super) fn single_shard_api_at(&self, idx: usize) -> Self {
        let shard = &self.shards()[idx];
        Self::new_single_shard(Arc::clone(&shard.coord), shard.id.clone())
    }

    pub(super) fn read_model_revision(&self) -> u64 {
        let mut hash = 0xcbf29ce484222325u64;
        for shard in self.shards() {
            hash = read_model_mix(hash, shard.coord.read_model_revision());
        }
        hash
    }

    pub(super) fn read_model_cache_get(
        &self,
        family: &str,
        tenant: Option<u64>,
        input: &str,
    ) -> Option<String> {
        let key = self.read_model_cache_key(family, tenant, input);
        let hit = self.read_model_cache.lock().unwrap().get(&key);
        hit.map(|body| mark_read_model_cache_state(&body, "hit"))
    }

    pub(super) fn read_model_cache_put(
        &self,
        family: &str,
        tenant: Option<u64>,
        input: &str,
        body: String,
    ) -> String {
        let key = self.read_model_cache_key(family, tenant, input);
        self.read_model_cache.lock().unwrap().put(key, body.clone());
        mark_read_model_cache_state(&body, "miss")
    }

    fn read_model_cache_key(&self, family: &str, tenant: Option<u64>, input: &str) -> String {
        format!(
            "{}|{}|{}",
            family,
            tenant
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string()),
            input
        )
    }

    pub(super) fn invalidate_read_model_cache(&self) {
        self.read_model_cache.lock().unwrap().map.clear();
    }

    /// 给 eventually-consistent fanout 查询选择读目标。
    ///
    /// 目前只用于 `/v1/search` 这类召回型接口；带 snapshot lease 的分页/聚合接口仍读 leader，
    /// 避免 leader snapshot token 被错误套到 follower 上。后续做远程 lease/version pin 后再扩展。
    pub(super) fn eventually_consistent_read_coord_for_shard<'a>(
        &self,
        shard: &'a ShardBackend,
    ) -> &'a Arc<WriteCoordinator> {
        self.eventually_consistent_read_target_for_shard(shard).0
    }

    pub(super) fn eventually_consistent_read_target_for_shard<'a>(
        &self,
        shard: &'a ShardBackend,
    ) -> (&'a Arc<WriteCoordinator>, Option<String>) {
        let leader_status = shard.coord.replication_status();
        let mut best = None;
        for replica in &shard.replicas {
            let status = replica.coord.replication_status();
            let decision = status.replica_read_decision(&leader_status, replica.max_lag_lsn);
            if !decision.readable {
                continue;
            }
            let candidate = (
                &replica.coord,
                Some(replica.id.clone()),
                decision.replication_lag_lsn,
            );
            match best {
                Some((_, _, best_lag)) if best_lag <= candidate.2 => {}
                _ => best = Some(candidate),
            }
        }
        best.map(|(coord, target, _)| (coord, target))
            .unwrap_or((&shard.coord, None))
    }

    pub(super) fn eventually_consistent_read_client_for_shard<'a>(
        &self,
        shard: &'a ShardBackend,
    ) -> (&'a dyn ShardClient, Option<String>) {
        let leader_status = shard.client.replication_status();
        let mut best = None;
        for replica in &shard.replicas {
            let status = replica.client.replication_status();
            let decision = status.replica_read_decision(&leader_status, replica.max_lag_lsn);
            if !decision.readable {
                continue;
            }
            let candidate = (
                replica.client.as_ref(),
                Some(replica.id.clone()),
                decision.replication_lag_lsn,
            );
            match best {
                Some((_, _, best_lag)) if best_lag <= candidate.2 => {}
                _ => best = Some(candidate),
            }
        }
        best.map(|(client, target, _)| (client, target))
            .unwrap_or((shard.client.as_ref(), None))
    }

    pub(super) fn trace_detail_owner_index(
        &self,
        tenant: Option<u64>,
        trace_id: u64,
    ) -> Option<usize> {
        self.storage.trace_detail_owner_index(tenant, trace_id)
    }

    pub(super) fn metadata_owner_index_for_trace(
        &self,
        tenant: Option<u64>,
        trace_id: u64,
    ) -> usize {
        self.storage
            .metadata_owner_index_for_trace(tenant, trace_id)
    }

    pub(super) fn session_detail_owner_index(
        &self,
        tenant: Option<u64>,
        session_id: u64,
    ) -> Option<usize> {
        self.storage.session_detail_owner_index(tenant, session_id)
    }

    pub(super) fn cluster_shards_json(&self) -> String {
        let mode = self.storage.mode().as_str();
        let routing = if self.is_in_process_cluster() {
            "hash_tenant_session_trace"
        } else {
            "single_shard"
        };
        let shards = self
            .shards()
            .iter()
            .map(|shard| {
                let status = shard.client.replication_status();
                let replicas = shard
                    .replicas
                    .iter()
                    .map(|replica| {
                        let replica_status = replica.client.replication_status();
                        let decision =
                            replica_status.replica_read_decision(&status, replica.max_lag_lsn);
                        format!(
                            r#"{{"replicaId":"{}","role":"follower","writable":false,"readable":{},"local":true,"storageMode":"{}","manifestVersion":{},"committedTail":{},"memtableWatermark":{},"segmentCount":{},"memtableRows":{},"syncState":"{}","replicationLagLsn":{},"reason":"{}","maxLagLsn":{}}}"#,
                            json_escape(&replica.id),
                            decision.readable,
                            mode,
                            replica_status.manifest_version,
                            replica_status.committed_tail,
                            replica_status.memtable_watermark,
                            replica_status.segment_count,
                            replica_status.memtable_rows,
                            decision.sync_state,
                            decision.replication_lag_lsn,
                            decision.reason,
                            replica.max_lag_lsn
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                format!(
                    r#"{{"shardId":"{}","role":"leader","writable":true,"readable":true,"local":true,"storageMode":"{}","manifestVersion":{},"committedTail":{},"memtableWatermark":{},"segmentCount":{},"memtableRows":{},"syncState":"leader","replicationLagLsn":0,"replicaCount":{},"replicas":[{}]}}"#,
                    json_escape(shard.id.as_str()),
                    mode,
                    status.manifest_version,
                    status.committed_tail,
                    status.memtable_watermark,
                    status.segment_count,
                    status.memtable_rows,
                    shard.replicas.len(),
                    replicas
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let replica_count: usize = self.shards().iter().map(|shard| shard.replicas.len()).sum();
        format!(
            r#"{{"mode":"{}","writeModel":"single_writer_per_shard","routing":"{}","shardCount":{},"replicaCount":{},"shardKey":["tenant_id","session_id","trace_id"],"shards":[{}]}}"#,
            mode,
            routing,
            self.shard_count(),
            replica_count,
            shards
        )
    }
}

fn read_model_mix(mut hash: u64, value: u64) -> u64 {
    const FNV_PRIME: u64 = 0x100000001b3;
    for byte in value.to_le_bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn mark_read_model_cache_state(body: &str, state: &str) -> String {
    if body.contains(r#""readModelCache":"miss""#) {
        return body.replace(
            r#""readModelCache":"miss""#,
            &format!(r#""readModelCache":"{state}""#),
        );
    }
    if body.contains(r#""readModelCache":"hit""#) {
        return body.replace(
            r#""readModelCache":"hit""#,
            &format!(r#""readModelCache":"{state}""#),
        );
    }
    if let Some(stripped) = body.strip_suffix('}') {
        format!(r#"{stripped},"readModelCache":"{state}"}}"#)
    } else {
        body.to_string()
    }
}
