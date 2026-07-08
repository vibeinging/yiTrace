impl WriteCoordinator {
    /// 内存 WAL（测试/开发，不落盘）。
    pub fn new(segments: Arc<dyn SegmentStore>) -> Arc<Self> {
        Self::build(segments, Wal::new())
    }

    /// 文件 WAL（真落盘）：重启后用同一路径 `open` + `recover()` 可从盘上重放(WAL 持久化)。
    /// 注意：段/manifest 不持久化,崩溃后靠 WAL 全量重放进 MemTable 恢复。要"flush 后重启不丢"用 `open_durable`。
    pub fn open(
        segments: Arc<dyn SegmentStore>,
        wal_path: impl AsRef<std::path::Path>,
    ) -> std::io::Result<Arc<Self>> {
        Ok(Self::build_full(
            segments,
            Wal::open(wal_path)?,
            Manifest::empty(),
            1,
            1,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        ))
    }

    /// **全持久化引擎**：一个目录下放段(`segments/`)+ WAL(`wal.log`)+ manifest(`manifest.dat`)。
    /// 重启用同一目录 `open_durable` + `recover()`：先从 manifest 重建段集合(指向盘上段文件)、再 WAL 重放
    /// 水位之后的尾巴 —— **flush 过的数据(水位之前、WAL 不再重放)从持久段读回,真正重启不丢**。
    pub fn open_durable(dir: impl AsRef<std::path::Path>) -> std::io::Result<Arc<Self>> {
        Self::open_durable_inner(dir, None, None, None)
    }

    /// open_durable 的内部实现，多收可选索引覆盖 + 磁盘向量索引参数（[`CoordinatorBuilder`] 用它注入）。
    fn open_durable_inner(
        dir: impl AsRef<std::path::Path>,
        bm25: Option<Arc<dyn Bm25Index>>,
        graph: Option<Arc<dyn GraphIndex>>,
        vec_cfg: Option<DiskGraphConfig>,
    ) -> std::io::Result<Arc<Self>> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir)?;
        let segments = Arc::new(FileSegmentStore::open(dir.join("segments"))?);
        let wal = Wal::open(dir.join("wal.log"))?;
        let manifest_path = dir.join("manifest.dat");
        let metadata_path = dir.join("metadata.dat");
        let gc_log_path = dir.join("gc.log");
        // 有持久 manifest 就从它恢复段集合与 id 计数器；否则从空开始。
        let (manifest, next_seg, next_chunk) = match persist::load(&manifest_path) {
            Some(s) => (s.manifest, s.next_segment_id, s.next_chunk_id),
            None => (Manifest::empty(), 1, 1),
        };
        // 默认向量索引 = **磁盘图索引**（向量+图都落盘、重启不 rebuild、append 友好），不用 vecstore。
        // 注入了自定义 graph（可能内存型）则保留 vecstore 重建路径（向后兼容）。
        let (graph, vector_path): (Option<Arc<dyn GraphIndex>>, Option<std::path::PathBuf>) =
            match graph {
                Some(g) => (Some(g), Some(dir.join("vectors.dat"))),
                None => {
                    let disk =
                        DurableGraphIndex::open(dir.join("vecindex"), vec_cfg.unwrap_or_default());
                    (Some(Arc::new(disk) as Arc<dyn GraphIndex>), None)
                }
            };
        let coord = Self::build_full(
            segments,
            wal,
            manifest,
            next_seg,
            next_chunk,
            Some(manifest_path),
            vector_path,
            bm25,
            graph,
            metadata::load(&metadata_path),
            Some(metadata_path),
            Some(dir.to_path_buf()),
        );
        // 打开 GC 日志，先补删上次崩溃残留的"MARK 没 DONE"段（崩溃安全），再装上。
        let entries = gc_log::GcLog::scan(&gc_log_path).unwrap_or_default();
        for seg in gc_log::pending_deletions(&entries) {
            // 段文件可能已删了一半（崩溃在 unlink 中）；补删幂等（不存在就跳过）。
            coord.segments.unlink_segment(SegmentId(seg));
            // 这些段上次崩溃前 manifest 已不引用（reclaim 前提），不用动 manifest。
            // 段 id 不复用、dead_set 是内存态重启后清空，所以不用动 dead_set。
        }
        // 重置 gc.log：已补删的不再记；之后 reclaim 重新记新意图。truncate 即可。
        let _ = std::fs::write(&gc_log_path, b"");
        // GC 日志和 WAL/manifest 同等重要（崩溃安全的承重组件）——打开失败必须 fail-fast，
        // 不能静默降级成"无 GC 日志、reclaim 直接删"（那样崩溃恢复失效且无人知晓）。
        let log = gc_log::GcLog::open(&gc_log_path)?;
        *coord.gc_log.lock().unwrap() = Some(log);
        Ok(coord)
    }

    fn build(segments: Arc<dyn SegmentStore>, wal: Wal) -> Arc<Self> {
        Self::build_full(
            segments,
            wal,
            Manifest::empty(),
            1,
            1,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build_full(
        segments: Arc<dyn SegmentStore>,
        wal: Wal,
        manifest: Manifest,
        next_segment_id: u64,
        next_chunk_id: u64,
        manifest_path: Option<std::path::PathBuf>,
        vector_path: Option<std::path::PathBuf>,
        bm25: Option<Arc<dyn Bm25Index>>,
        graph: Option<Arc<dyn GraphIndex>>,
        metadata_state: Option<metadata::MetadataState>,
        metadata_path: Option<std::path::PathBuf>,
        dir: Option<std::path::PathBuf>,
    ) -> Arc<Self> {
        let metadata_state = metadata_state.unwrap_or_default();
        let filter_attrs_path = dir.as_ref().map(|dir| dir.join("filter_attrs.dat"));
        let trace_rollup_path = dir.as_ref().map(|dir| dir.join("trace_rollup.dat"));
        let metadata_index = MetadataIndex::build(
            &metadata_state.annotations,
            &metadata_state.dataset_associations,
            &metadata_state.retention_audits,
            &metadata_state.retention_policies,
        );
        Arc::new(Self {
            write_lock: Mutex::new(()),
            current: Current::new(manifest),
            wal: Mutex::new(wal),
            memtable: Mutex::new(MemTable::new()),
            segments,
            dead_set: Mutex::new(Vec::new()),
            buffer_pins: BufferPins::default(),
            // 默认 BM25 用纯 Rust 中文词级分词（jieba 全量词典，开箱即生产级）/ 图式 ANN；
            // 可被 builder 注入覆盖（团队 jieba FFI、bigram、或叠了自有词典的 ChineseTokenizer）。
            bm25: bm25.unwrap_or_else(|| {
                Arc::new(Bm25TextIndex::with_tokenizer(Box::new(
                    ChineseTokenizer::full(),
                )))
            }),
            graph: graph.unwrap_or_else(|| Arc::new(GraphAnnIndex::default())),
            flush_threshold: AtomicUsize::new(4096),
            next_segment_id: Mutex::new(next_segment_id),
            next_chunk_id: Mutex::new(next_chunk_id),
            datasets: Mutex::new(BTreeMap::new()),
            annotations: Mutex::new(metadata_state.annotations),
            dataset_associations: Mutex::new(metadata_state.dataset_associations),
            metadata_index: Mutex::new(metadata_index),
            retention_audits: Mutex::new(metadata_state.retention_audits),
            retention_policies: Mutex::new(metadata_state.retention_policies),
            next_annotation_id: Mutex::new(metadata_state.next_annotation_id),
            next_dataset_association_id: Mutex::new(metadata_state.next_dataset_association_id),
            next_retention_audit_id: Mutex::new(metadata_state.next_retention_audit_id),
            next_retention_policy_id: Mutex::new(metadata_state.next_retention_policy_id),
            manifest_path,
            metadata_path,
            vector_path,
            filter_attrs_path,
            trace_rollup_path,
            filter_attrs: Mutex::new(FilterAttrsIndex::default()),
            trace_rollup: Mutex::new(TraceAggregateRollupIndex::default()),
            session_idx: Mutex::new(SessionIndex::default()),
            seg_fold_cache: Mutex::new(SegFoldCache::new(2_000_000)), // 缓存上限 ~200 万行
            seg_key_bloom: Mutex::new(HashMap::new()),
            gc_log: Mutex::new(None), // open_durable 设成 Some；非持久模式保持 None
            dir,
        })
    }

    /// commit 后若开了持久化,原子写 manifest（含 id 计数器）。崩溃在写 manifest 前 = 退回上个 manifest
    /// （那次 commit 的段文件成孤儿,无害,等回收或忽略）；写后 = 新状态生效。两边都不脏读。
    fn persist_manifest(&self) {
        let Some(path) = &self.manifest_path else {
            return;
        };
        let state = persist::PersistedState {
            manifest: (*self.current.manifest()).clone(),
            next_segment_id: *self.next_segment_id.lock().unwrap(),
            next_chunk_id: *self.next_chunk_id.lock().unwrap(),
        };
        let _ = persist::save(path, &state);
        // 提交点：向量索引批量刷盘（append 期间只写不刷，靠这里持久；删除少、append 多场景的吞吐取舍）。
        self.graph.flush();
    }

    /// 元数据账本独立落盘。失败不影响 trace 写入主链路，但下一次接口返回前会保留在内存中。
    fn persist_metadata(&self) {
        let Some(path) = &self.metadata_path else {
            return;
        };
        let state = metadata::MetadataState {
            annotations: self.annotations.lock().unwrap().clone(),
            dataset_associations: self.dataset_associations.lock().unwrap().clone(),
            retention_audits: self.retention_audits.lock().unwrap().clone(),
            retention_policies: self.retention_policies.lock().unwrap().clone(),
            next_annotation_id: *self.next_annotation_id.lock().unwrap(),
            next_dataset_association_id: *self.next_dataset_association_id.lock().unwrap(),
            next_retention_audit_id: *self.next_retention_audit_id.lock().unwrap(),
            next_retention_policy_id: *self.next_retention_policy_id.lock().unwrap(),
        };
        let _ = metadata::save(path, &state);
    }

    fn rebuild_metadata_index(&self) {
        let annotations = self.annotations.lock().unwrap().clone();
        let dataset_associations = self.dataset_associations.lock().unwrap().clone();
        let retention_audits = self.retention_audits.lock().unwrap().clone();
        let retention_policies = self.retention_policies.lock().unwrap().clone();
        *self.metadata_index.lock().unwrap() = MetadataIndex::build(
            &annotations,
            &dataset_associations,
            &retention_audits,
            &retention_policies,
        );
    }

    /// 提交新 manifest 版本并（若开了持久化）落盘。所有 commit 走这里,保证段集合改动都持久。
    fn commit_and_persist(&self, draft: Manifest) {
        self.current.commit(draft);
        self.persist_manifest();
    }

    /// 读者入口：pin 一个一致快照（委托给 yt-manifest）。
    pub fn pin_snapshot(&self) -> Snapshot {
        self.current.pin_snapshot()
    }
}
