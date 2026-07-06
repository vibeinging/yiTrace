/// shard 内主备复制的一批 WAL 增量。
///
/// 这是分布式 HA 的底层原语，不包含网络、Raft 或 segment 文件同步：
/// leader 通过 `export_wal_after` 导出 `(from_lsn, to_lsn, records)`，follower
/// 通过 `apply_wal_replication_batch` 按 LSN 顺序幂等重放。
#[derive(Clone)]
pub struct WalReplicationBatch {
    pub from_lsn: u64,
    pub to_lsn: u64,
    pub records: Vec<WalRecord>,
}

/// follower/leader 对外暴露的复制水位，供 gateway 或后续 shard server 判断读写新鲜度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplicationStatus {
    pub committed_tail: u64,
    pub manifest_version: u64,
    pub memtable_watermark: u64,
    pub memtable_rows: usize,
    pub segment_count: usize,
}

/// gateway 判断 follower 是否可读的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplicaReadDecision {
    pub readable: bool,
    pub sync_state: &'static str,
    pub replication_lag_lsn: u64,
    pub reason: &'static str,
}

impl ReplicationStatus {
    /// 对比 leader/follower 水位，给出 follower 是否可以承担读请求。
    ///
    /// `max_lag_lsn = 0` 表示强制读到 leader 已确认尾；更大的值允许陈旧读。
    /// 这里不做租户/权限判断，只回答“这个副本在复制水位上是否可读”。
    pub fn replica_read_decision(
        &self,
        leader: &ReplicationStatus,
        max_lag_lsn: u64,
    ) -> ReplicaReadDecision {
        if leader.memtable_watermark > leader.committed_tail {
            return ReplicaReadDecision {
                readable: false,
                sync_state: "diverged",
                replication_lag_lsn: 0,
                reason: "leader_watermark_after_tail",
            };
        }
        if self.memtable_watermark > self.committed_tail {
            return ReplicaReadDecision {
                readable: false,
                sync_state: "diverged",
                replication_lag_lsn: 0,
                reason: "replica_watermark_after_tail",
            };
        }
        if self.committed_tail > leader.committed_tail {
            return ReplicaReadDecision {
                readable: false,
                sync_state: "diverged",
                replication_lag_lsn: 0,
                reason: "replica_tail_after_leader",
            };
        }
        if self.manifest_version > leader.manifest_version {
            return ReplicaReadDecision {
                readable: false,
                sync_state: "diverged",
                replication_lag_lsn: leader.committed_tail - self.committed_tail,
                reason: "replica_manifest_after_leader",
            };
        }
        let lag = leader.committed_tail - self.committed_tail;
        if lag == 0 {
            return ReplicaReadDecision {
                readable: true,
                sync_state: "ready",
                replication_lag_lsn: 0,
                reason: "caught_up",
            };
        }
        if lag <= max_lag_lsn {
            return ReplicaReadDecision {
                readable: true,
                sync_state: "catching_up",
                replication_lag_lsn: lag,
                reason: "within_lag_budget",
            };
        }
        ReplicaReadDecision {
            readable: false,
            sync_state: "stale",
            replication_lag_lsn: lag,
            reason: "lag_exceeds_budget",
        }
    }
}

impl WriteCoordinator {
    /// 崩溃恢复：从 WAL 重放重建 MemTable（§M.6）+ **重建派生检索索引**。
    /// 重放点 = 当前 manifest 的 memtable_watermark（已吸收进段的最大 LSN）。
    /// 重放只取 watermark 之后的记录；即便段与重放有重叠（崩溃窗口里段已落、水位未推进），
    /// 读时的确定性 event_id 去重也保证不重复折叠 —— 这正是「seq 原样持久化、不重补」的意义。
    ///
    /// 检索索引(BM25/属性边车/向量)是内存态,重启全空,这里一并重建,否则重启后"按内容搜/找相似"返回空:
    /// - BM25 + 属性边车是**派生数据**：扫持久段(水位之前)+ 重放的 WAL 尾(水位之后)各喂一次,合起来覆盖全部、不重不漏。
    /// - 向量**段里推不出来**：从独立向量文件重载,喂回图索引(后写覆盖先写)。
    pub fn recover(&self) {
        olog::log(
            olog::Level::Info,
            "recover_start",
            &[("version", &self.current.version())],
        );
        *self.attr_postings.lock().unwrap() = AttrPostings::default();
        *self.seg_attr_directory.lock().unwrap() = SegmentAttrDirectory::default();
        *self.seg_attr_cache.lock().unwrap() =
            SegmentAttrSidecarCache::new(ATTR_SIDECAR_CACHE_MAX_BYTES);
        self.text_domains.lock().unwrap().reset();
        self.trace_aggregate_rollups.lock().unwrap().clear();
        self.trace_trajectory_idx.lock().unwrap().clear();
        // 1) 派生索引：扫所有持久段(水位之前的数据)喂回 BM25 + 属性边车；顺带重建段级 key bloom。
        let m = self.current.manifest();
        let seg_count = m.segments.len();
        for entry in m.segments.values() {
            let recs = self.segments.scan_records(entry.segment_id);
            self.install_segment_attr_sidecar(entry.segment_id, &recs, false);
            self.install_trace_aggregate_segment_rollup(entry.segment_id, &recs, false);
            let bloom = KeyBloom::build(recs.iter().map(|r| (r.trace_id, r.span_id)), recs.len());
            self.seg_key_bloom
                .lock()
                .unwrap()
                .insert(entry.segment_id.get(), Arc::new(bloom));
            for r in &recs {
                self.index_record_inner(r, false);
            }
        }
        drop(m);
        // 2) 向量：从独立向量文件重载,喂回图索引。
        let mut vec_count = 0u64;
        if let Some(p) = &self.vector_path {
            for ((t, s), v) in vecstore::load(p) {
                self.graph.index_embedding(t, s, v);
                vec_count += 1;
            }
        }
        self.load_named_vectors_from_disk();
        // 3) WAL 重放：水位之后的尾巴进 MemTable,并喂派生索引(与段不重叠,因 manifest 水位与段同事务持久)。
        let wal = self.wal.lock().unwrap();
        let mut mt = self.memtable.lock().unwrap();
        let mut wal_count = 0u64;
        for (lsn, r) in wal.replay_after(WalLsn::new(self.current.memtable_watermark())) {
            self.index_record(&r);
            mt.append(MemRow {
                commit_lsn: lsn,
                trace_id: r.trace_id,
                span_id: r.span_id,
                ts: r.ts,
                identity: r.identity.clone(), // seq 来自 WAL 原值，绝不重补
                fields: r.fields.clone(),
            });
            wal_count += 1;
        }
        // 已提交尾从 WAL 恢复（重启后 committed_tail 不是持久态，由 WAL 重新确定）。
        let tail = wal.committed_tail();
        self.current.advance_committed_tail(tail);
        olog::log(
            olog::Level::Info,
            "recover_done",
            &[
                ("segs_scanned", &seg_count),
                ("vectors_reloaded", &vec_count),
                ("wal_replayed", &wal_count),
                ("committed_tail", &tail.get()),
            ],
        );
    }

    /// 当前复制/读取水位。多进程 shard server 后续可以把它挂到 health/ready 或 shard status。
    pub fn replication_status(&self) -> ReplicationStatus {
        let manifest = self.current.manifest();
        ReplicationStatus {
            committed_tail: self.current.committed_tail(),
            manifest_version: manifest.version.get(),
            memtable_watermark: manifest.memtable_watermark.get(),
            memtable_rows: self.memtable_len(),
            segment_count: manifest.segments.len(),
        }
    }

    /// 高频 read model 的失效水位：trace 写入、manifest 更新或 metadata 更新任一变化都会改变。
    pub fn read_model_revision(&self) -> u64 {
        let status = self.replication_status();
        status
            .committed_tail
            .wrapping_mul(31)
            .wrapping_add(status.manifest_version.wrapping_mul(17))
            .wrapping_add(self.metadata_epoch.load(Ordering::Acquire))
    }

    /// 导出 `from_lsn` 之后的已提交 WAL 记录。
    ///
    /// 这个方法只读 WAL，不 pin manifest；它适合复制未 flush 的 WAL tail。sealed segment 文件和
    /// manifest 同步仍是下一层能力，不能只靠这个方法完成完整 HA。
    pub fn export_wal_after(&self, from_lsn: u64) -> WalReplicationBatch {
        let wal = self.wal.lock().unwrap();
        let rows = wal.replay_after(WalLsn::new(from_lsn));
        let mut records = Vec::with_capacity(rows.len());
        let mut to_lsn = from_lsn;
        for (lsn, record) in rows {
            to_lsn = lsn;
            records.push(record);
        }
        WalReplicationBatch {
            from_lsn,
            to_lsn,
            records,
        }
    }

    /// follower 按 LSN 顺序应用 leader 导出的 WAL 增量。
    ///
    /// 语义：
    /// - 完整重复批次：幂等 no-op。
    /// - 部分重叠批次：跳过已应用前缀，只追加缺失后缀。
    /// - 当前 follower tail 小于 `from_lsn`：说明缺了一段 WAL，拒绝应用。
    /// - 当前 follower tail 大于 `to_lsn`：旧批次 no-op。
    pub fn apply_wal_replication_batch(
        &self,
        batch: &WalReplicationBatch,
    ) -> Result<ReplicationStatus, String> {
        if batch.to_lsn < batch.from_lsn {
            return Err(format!(
                "invalid replication batch: to_lsn {} is before from_lsn {}",
                batch.to_lsn, batch.from_lsn
            ));
        }
        let expected_to = batch.from_lsn + batch.records.len() as u64;
        if expected_to != batch.to_lsn {
            return Err(format!(
                "invalid replication batch: from_lsn {} + records {} != to_lsn {}",
                batch.from_lsn,
                batch.records.len(),
                batch.to_lsn
            ));
        }
        if batch.records.is_empty() {
            return Ok(self.replication_status());
        }

        let _w = self.write_lock.lock().unwrap();
        let current_tail = self.current.committed_tail();
        if current_tail >= batch.to_lsn {
            return Ok(self.replication_status());
        }
        if current_tail < batch.from_lsn {
            return Err(format!(
                "replication gap: follower tail {} is before batch from_lsn {}",
                current_tail, batch.from_lsn
            ));
        }

        let skip = (current_tail - batch.from_lsn) as usize;
        let records: Vec<WalRecord> = batch.records[skip..].to_vec();
        let dirty_traces: HashSet<u64> = records.iter().map(|r| r.trace_id).collect();
        let mut wal = self.wal.lock().unwrap();
        let wal_tail = wal.committed_tail().get();
        if wal_tail != current_tail {
            return Err(format!(
                "replication tail mismatch: manifest tail {} != wal tail {}",
                current_tail, wal_tail
            ));
        }
        let first = wal_tail + 1;
        {
            let mut mt = self.memtable.lock().unwrap();
            for (i, r) in records.iter().enumerate() {
                self.index_record(r);
                mt.append(MemRow {
                    commit_lsn: first + i as u64,
                    trace_id: r.trace_id,
                    span_id: r.span_id,
                    ts: r.ts,
                    identity: r.identity.clone(),
                    fields: r.fields.clone(),
                });
            }
        }
        let last = wal.append_committed(records);
        drop(wal);
        if last.get() != batch.to_lsn {
            return Err(format!(
                "replication apply ended at lsn {}, expected {}",
                last.get(),
                batch.to_lsn
            ));
        }
        if !dirty_traces.is_empty() {
            self.trace_trajectory_idx
                .lock()
                .unwrap()
                .retain(|(_, trace_id), _| !dirty_traces.contains(trace_id));
        }
        self.current.advance_committed_tail(last);
        if self.memtable.lock().unwrap().len() >= self.flush_threshold.load(Ordering::Relaxed) {
            self.flush_memtable_locked();
        }
        Ok(self.replication_status())
    }

    /// 测试/演示：模拟崩溃，丢弃易失的 MemTable。WAL 与 manifest 是持久的，保留不动。
    pub fn simulate_crash_lose_memtable(&self) {
        *self.memtable.lock().unwrap() = MemTable::new();
    }

    fn alloc_segment_id(&self) -> SegmentId {
        let mut g = self.next_segment_id.lock().unwrap();
        let id = *g;
        *g += 1;
        SegmentId::new(id)
    }

    fn alloc_chunk_id(&self) -> yt_core::ids::ChunkId {
        let mut g = self.next_chunk_id.lock().unwrap();
        let id = *g;
        *g += 1;
        yt_core::ids::ChunkId::new(id)
    }

    /// flush 提交（sealed → live）：把一批已 ack 事件封段，新段 Live 进新版本，watermark 推进。
    /// 段加入 + watermark 推进必须在**同一次** commit 里原子生效（堵「既不在 memtable 又不在段」空窗）。
    pub fn commit_flush(&self, records: &[WalRecord], up_to_lsn: WalLsn) {
        let _w = self.write_lock.lock().unwrap();
        let seg = self.alloc_segment_id();
        self.segments.flush_to_segment(seg, records); // building→sealed（写完 fsync）
        self.install_segment_attr_sidecar(seg, records, true);
        self.install_trace_aggregate_segment_rollup(seg, records, true);
        let bloom = KeyBloom::build(
            records.iter().map(|r| (r.trace_id, r.span_id)),
            records.len(),
        );
        self.seg_key_bloom
            .lock()
            .unwrap()
            .insert(seg.get(), Arc::new(bloom));
        let (min_ts, max_ts) = ts_range(records);
        let mut draft = self.current.cow_next();
        draft.memtable_watermark = up_to_lsn; // 与下面加段同事务
        draft.segments.insert(
            seg.get(),
            SegmentEntry {
                segment_id: seg,
                level: 0,
                state: SegState::Live,
                min_ts, // zone-map：读路径据此做时间窗剪枝
                max_ts,
                deletion_vec: Arc::new(DeletionVec::empty()),
                deletion_seq: 0,
                upgrade_ref: None,
                upgrade_seq: 0,
            },
        );
        self.commit_and_persist(draft); // 原子换指针：sealed→live + watermark 同时生效;并落盘 manifest

        // 提交后按 gate 回收 MemTable 被吸收前缀。gate 必须取「所有活跃读者下界的最小值」，
        // 绝不能直接用 up_to_lsn —— 否则就是 flush-evict 漏行 bug。仍有旧读者时此值更小、不删其行。
        let gate = WalLsn::new(self.current.min_retained_watermark());
        self.memtable.lock().unwrap().evict_up_to(gate);
        self.rebuild_live_attr_postings_from_memtable();
    }

    /// 删除提交：给某段换一个新的 deletion 块（deletion_seq+1），绝不原地改旧块。
    pub fn commit_delete(&self, seg: SegmentId, row: u32) {
        let _w = self.write_lock.lock().unwrap();
        let chunk_id = self.alloc_chunk_id();
        let mut draft = self.current.cow_next();
        if let Some(entry) = draft.segments.get_mut(&seg.get()) {
            let new_dv = entry.deletion_vec.with_deleted(row, chunk_id);
            entry.deletion_vec = Arc::new(new_dv);
            entry.deletion_seq += 1;
        }
        self.commit_and_persist(draft);
        self.session_idx.lock().unwrap().dirty = true; // 删除改了段，边车下次读重建
    }

    /// 属性补写（upgrade）提交：给某段 (trace_id, span_id) 补写**非身份属性**，与 delete 完全对称——
    /// 写时复制出新 upgrade 块（upgrade_seq+1），绝不原地改旧块（旧版本读者读旧块）。
    /// 身份字段冻结（M.7），由上层 schema 保证不进 `fields`。
    pub fn commit_upgrade(
        &self,
        seg: SegmentId,
        trace_id: u64,
        span_id: u64,
        fields: yt_core::fold::SpanFields,
    ) {
        let _w = self.write_lock.lock().unwrap();
        let chunk_id = self.alloc_chunk_id();
        let mut draft = self.current.cow_next();
        if let Some(entry) = draft.segments.get_mut(&seg.get()) {
            let base = entry
                .upgrade_ref
                .as_deref()
                .cloned()
                .unwrap_or_else(UpgradeColChunk::empty);
            let new_chunk = base.with_patch(trace_id, span_id, fields, chunk_id);
            entry.upgrade_ref = Some(Arc::new(new_chunk));
            entry.upgrade_seq += 1;
        }
        self.commit_and_persist(draft);
        self.session_idx.lock().unwrap().dirty = true; // 补写改了段，边车下次读重建
    }

    /// compaction 第 1 步：选段，记录选段瞬间各输入段的 (deletion_seq, upgrade_seq)。
    /// 返回的 plan 交给调用方在**锁外**做昂贵的段重建，再用 `compaction_finish` 提交。
    pub fn compaction_begin(&self, inputs: &[SegmentId]) -> CompactionPlan {
        let _w = self.write_lock.lock().unwrap();
        let m = self.current.manifest();
        let seqs_at_select = inputs
            .iter()
            .filter_map(|s| {
                m.segments
                    .get(&s.get())
                    .map(|e| (s.get(), (e.deletion_seq, e.upgrade_seq)))
            })
            .collect();
        CompactionPlan {
            inputs: inputs.to_vec(),
            seqs_at_select,
        }
    }

    /// compaction 第 3 步：提交（草案 1 §D1.3 / OPEN-3）。
    /// 在 write_lock 下**重读输入段当前状态**重建新段 —— 这样选段后、提交前并发打到输入段的
    /// 删除/补写**不会丢**：当前 deletion_vec 把后到的删除也滤掉，当前 upgrade 块也并进新段。
    /// 返回是否发生了重读合并（输入段 seq 变了），便于观测/测试。
    pub fn compaction_finish(&self, plan: &CompactionPlan) -> bool {
        if plan.inputs.is_empty() {
            return false;
        }
        let _w = self.write_lock.lock().unwrap();
        let m = self.current.manifest();

        let mut reconciled = false;
        let mut merged: Vec<WalRecord> = Vec::new();
        let mut merged_upgrade = UpgradeColChunk::empty();
        let up_chunk_id = self.alloc_chunk_id();

        for &seg in &plan.inputs {
            let Some(entry) = m.segments.get(&seg.get()) else {
                continue;
            };
            // 选段以来 seq 涨了 = 期间有并发删除/补写打到这个输入段 → 触发重读合并
            if plan.seqs_at_select.get(&seg.get()) != Some(&(entry.deletion_seq, entry.upgrade_seq))
            {
                reconciled = true;
            }
            // 用「当前」deletion_vec 过滤（含选段后新增的删除）→ 删除不丢
            for (row, rec) in self.segments.scan_records(seg).into_iter().enumerate() {
                if !entry.deletion_vec.is_deleted(row as u32) {
                    merged.push(rec);
                }
            }
            // 把「当前」upgrade 块并进新段（按 (trace,span) 键，行号变了也不影响）→ 补写不丢
            if let Some(up) = &entry.upgrade_ref {
                for (&(t, s), fields) in up.iter() {
                    merged_upgrade = merged_upgrade.with_patch(t, s, fields.clone(), up_chunk_id);
                }
            }
        }

        let (min_ts, max_ts) = ts_range(&merged);
        let has_upgrade = merged_upgrade.iter().next().is_some();

        let mut draft = self.current.cow_next();
        let v_dead = draft.version.get();
        for s in &plan.inputs {
            draft.segments.remove(&s.get());
        }
        if !merged.is_empty() {
            let new_seg = self.alloc_segment_id();
            self.segments.flush_to_segment(new_seg, &merged);
            self.install_segment_attr_sidecar(new_seg, &merged, true);
            self.install_trace_aggregate_segment_rollup(new_seg, &merged, true);
            let bloom =
                KeyBloom::build(merged.iter().map(|r| (r.trace_id, r.span_id)), merged.len());
            self.seg_key_bloom
                .lock()
                .unwrap()
                .insert(new_seg.get(), Arc::new(bloom));
            draft.segments.insert(
                new_seg.get(),
                SegmentEntry {
                    segment_id: new_seg,
                    level: 1,
                    state: SegState::Live,
                    min_ts,
                    max_ts,
                    deletion_vec: Arc::new(DeletionVec::empty()), // 删除已物化进 merged，新段从干净开始
                    deletion_seq: 0,
                    upgrade_ref: has_upgrade.then(|| Arc::new(merged_upgrade)),
                    upgrade_seq: 0,
                },
            );
        }
        self.commit_and_persist(draft);

        let mut dead = self.dead_set.lock().unwrap();
        for s in &plan.inputs {
            dead.push(DeadResource { seg: *s, v_dead });
        }
        reconciled
    }

    /// 便捷：无并发窗口的一次性 compaction（begin + finish 连续）。
    pub fn commit_compaction(&self, inputs: &[SegmentId]) {
        let n_in = inputs.len();
        let plan = self.compaction_begin(inputs);
        self.compaction_finish(&plan);
        olog::log(
            olog::Level::Info,
            "compaction",
            &[("inputs", &n_in), ("version", &self.current.version())],
        );
    }

    /// 取 / 放一个段文件的 buffer pin（读路径扫段字节时持有，用完释放）。
    pub fn pin_buffer(&self, seg: SegmentId) {
        self.buffer_pins.pin(seg);
    }
    pub fn unpin_buffer(&self, seg: SegmentId) {
        self.buffer_pins.unpin(seg);
    }

    /// 段回收线程的一轮（草案 1 §D1.4）。对 dead_set 里每个资源，三条同真才物理删除：
    ///   (1) v_dead ≤ safe_version   (没有读者还 pin 在它 dead 之前的版本)
    ///   (2) ∧ 无未释放的 buffer pin  (字节级最后保险)
    ///   (3) ∧ 不被当前 manifest 引用 (防崩溃竞态)
    /// 返回这一轮回收了多少个段。真实实现是后台线程 + IO 限速。
    ///
    /// **崩溃安全**：持久模式（gc.log 存在）下，每个可删段走 MARK→fsync→unlink→DONE→fsync。
    /// 崩溃在 unlink 前（只写了 MARK）：重启据 gc.log 补删（文件还在 → 删）；崩溃在 unlink 后 DONE 前
    /// （文件已没）：重启据 gc.log 判定文件可能已删，幂等补删（不存在跳过）。两边都不留"删一半 +
    /// manifest 没更新"的不一致。
    ///
    /// **非持久模式**（gc.log 不存在）：reclaim 走旧的"直接删"路径——仅靠"段 id 永不复用 + compaction
    /// 只产新段"这两个不变量兜底，没有崩溃恢复。这是纯内存 / 测试场景可接受的退化。
    pub fn reclaim(&self) -> usize {
        let safe = self.current.safe_version();
        let mut freed = 0;
        let mut dead = self.dead_set.lock().unwrap();
        let mut gc = self.gc_log.lock().unwrap();
        dead.retain(|r| {
            let ok = r.v_dead <= safe
                && !self.buffer_pins.is_pinned(r.seg)
                && !self.current.contains_segment(r.seg);
            if !ok {
                return true; // 留着，下一轮再看
            }
            // 崩溃安全路径：MARK → fsync → unlink → DONE → fsync。
            if let Some(log) = gc.as_mut() {
                // MARK 写失败 = 意图没落盘，不能进 unlink（否则崩溃后无法恢复）。保守不删，留下轮。
                if log.mark(r.seg.get()).is_err() {
                    return true;
                }
            }
            self.segments.unlink_segment(r.seg);
            self.seg_fold_cache.lock().unwrap().remove(r.seg.get()); // 段没了，缓存失效
            self.seg_key_bloom.lock().unwrap().remove(&r.seg.get()); // bloom 同失效
            self.seg_attr_cache.lock().unwrap().remove(r.seg);
            self.seg_attr_directory
                .lock()
                .unwrap()
                .remove_segment(r.seg);
            self.trace_aggregate_rollups
                .lock()
                .unwrap()
                .remove(&r.seg.get());
            if let Some(dir) = &self.attr_sidecar_dir {
                let _ = std::fs::remove_file(attr_sidecar_path(dir, r.seg));
            }
            if let Some(dir) = &self.trace_aggregate_rollup_dir {
                let _ = std::fs::remove_file(trace_aggregate_rollup_path(dir, r.seg));
            }
            if let Some(log) = gc.as_mut() {
                // DONE 写失败：文件已删但完成标记没落盘。重启时会当成"MARK 没 DONE"补删——
                // 文件不存在了，unlink 幂等（store 实现容忍），正确。所以这里不回滚、继续。
                let _ = log.done(r.seg.get());
            }
            freed += 1;
            false // 出 dead_set
        });
        if freed > 0 {
            olog::log(
                olog::Level::Info,
                "reclaim",
                &[("freed", &freed), ("remaining_dead", &dead.len())],
            );
        }
        freed
    }

    /// 待回收 dead 资源数（可观测 / 测试用）。
    pub fn dead_count(&self) -> usize {
        self.dead_set.lock().unwrap().len()
    }

    /// **在线快照备份**（§3.3 数据安全底线）。
    ///
    /// 走 pin 协议拿一致快照（持有的版本不会被 GC），把所有持久文件拷到目标目录，
    /// 得到一个可独立 `open_durable` 恢复的一致快照。备份期间读写不阻塞（snapshot 隔离）。
    ///
    /// 拷的文件：`segments/`（目录）+ `attr_postings/`（目录）+ `wal.log` + `manifest.dat`
    /// + `metadata.dat` + `vecindex/`（或 `vectors.dat`）+ `named_vectors.dat` + `gc.log`。
    /// 段文件是不可变的、manifest 是当前版本快照——拷的是那一刻的一致态。
    /// WAL 可能比 manifest 新（有未 flush 的事务），recover 时重放水位之后的尾巴,幂等（确定性 event_id）。
    pub fn backup_snapshot(&self, dest: impl AsRef<std::path::Path>) -> std::io::Result<()> {
        let dest = dest.as_ref();
        let src = self.dir.as_ref().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "backup 需要 open_durable 的数据目录",
            )
        })?;
        // pin 住当前版本——拷贝期间 reclaim 不会删这个版本引用的段文件。
        let _snap = self.current.pin_snapshot();
        let version = self.current.version();
        olog::log(
            olog::Level::Info,
            "backup_start",
            &[
                ("dest", &dest.to_string_lossy().to_string()),
                ("version", &version),
            ],
        );

        std::fs::create_dir_all(dest)?;
        // 拷贝数据文件/目录。segments/、vecindex/、attr_postings/ 是目录,其余是文件。
        for name in ["segments", "vecindex", "attr_postings"] {
            let s = src.join(name);
            if s.exists() {
                copy_dir_recursive(&s, &dest.join(name))?;
            }
        }
        for name in [
            "wal.log",
            "manifest.dat",
            "metadata.dat",
            "vectors.dat",
            "named_vectors.dat",
            "gc.log",
        ] {
            let s = src.join(name);
            if s.exists() {
                std::fs::copy(&s, dest.join(name))?;
            }
        }
        olog::log(
            olog::Level::Info,
            "backup_done",
            &[
                ("dest", &dest.to_string_lossy().to_string()),
                ("version", &version),
            ],
        );
        Ok(())
    }
}
