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
        dir: Option<std::path::PathBuf>,
    ) -> Arc<Self> {
        let attr_sidecar_dir = dir.as_ref().map(|d| d.join("attr_postings"));
        if let Some(path) = &attr_sidecar_dir {
            let _ = std::fs::create_dir_all(path);
        }
        let trace_aggregate_rollup_dir = dir.as_ref().map(|d| d.join("trace_aggregate_rollups"));
        if let Some(path) = &trace_aggregate_rollup_dir {
            let _ = std::fs::create_dir_all(path);
        }
        let named_vector_path = dir.as_ref().map(|d| d.join("named_vectors.dat"));
        let metadata_path = dir.as_ref().map(|d| d.join("metadata.dat"));
        let metadata_state = metadata_path
            .as_ref()
            .and_then(metadata::load)
            .unwrap_or_else(|| metadata::MetadataState {
                next_annotation_id: 1,
                next_dataset_association_id: 1,
                next_golden_path_id: 1,
                next_retention_audit_id: 1,
                next_retention_policy_id: 1,
                ..Default::default()
            });
        let metadata_index = MetadataIndex::build(
            &metadata_state.annotations,
            &metadata_state.dataset_associations,
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
            text_domains: Mutex::new(TextDomainIndexes::default()),
            graph: graph.unwrap_or_else(|| Arc::new(GraphAnnIndex::default())),
            flush_threshold: AtomicUsize::new(4096),
            next_segment_id: Mutex::new(next_segment_id),
            next_chunk_id: Mutex::new(next_chunk_id),
            datasets: Mutex::new(BTreeMap::new()),
            annotations: Mutex::new(metadata_state.annotations),
            dataset_associations: Mutex::new(metadata_state.dataset_associations),
            golden_paths: Mutex::new(metadata_state.golden_paths),
            retention_audits: Mutex::new(metadata_state.retention_audits),
            retention_policies: Mutex::new(metadata_state.retention_policies),
            metadata_epoch: AtomicU64::new(0),
            metadata_index: Mutex::new(metadata_index),
            next_annotation_id: Mutex::new(metadata_state.next_annotation_id),
            next_dataset_association_id: Mutex::new(metadata_state.next_dataset_association_id),
            next_golden_path_id: Mutex::new(metadata_state.next_golden_path_id),
            next_retention_audit_id: Mutex::new(metadata_state.next_retention_audit_id),
            next_retention_policy_id: Mutex::new(metadata_state.next_retention_policy_id),
            manifest_path,
            metadata_path,
            vector_path,
            named_vector_path,
            named_vectors: Mutex::new(NamedVectorIndex::default()),
            filter_attrs: Mutex::new(HashMap::new()),
            attr_postings: Mutex::new(AttrPostings::default()),
            seg_attr_directory: Mutex::new(SegmentAttrDirectory::default()),
            seg_attr_cache: Mutex::new(SegmentAttrSidecarCache::new(ATTR_SIDECAR_CACHE_MAX_BYTES)),
            attr_sidecar_dir,
            trace_aggregate_rollups: Mutex::new(HashMap::new()),
            trace_aggregate_rollup_dir,
            trace_span_keys: Mutex::new(HashMap::new()),
            trace_trajectory_idx: Mutex::new(HashMap::new()),
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

    /// 持久化业务元数据。它不属于 manifest 版本链，但需要和 WAL/segment 一起备份和重启恢复。
    fn persist_metadata(&self) {
        self.metadata_epoch.fetch_add(1, Ordering::AcqRel);
        let annotations = self.annotations.lock().unwrap().clone();
        let dataset_associations = self.dataset_associations.lock().unwrap().clone();
        *self.metadata_index.lock().unwrap() =
            MetadataIndex::build(&annotations, &dataset_associations);
        let Some(path) = &self.metadata_path else {
            return;
        };
        let state = metadata::MetadataState {
            annotations,
            dataset_associations,
            golden_paths: self.golden_paths.lock().unwrap().clone(),
            retention_audits: self.retention_audits.lock().unwrap().clone(),
            retention_policies: self.retention_policies.lock().unwrap().clone(),
            next_annotation_id: *self.next_annotation_id.lock().unwrap(),
            next_dataset_association_id: *self.next_dataset_association_id.lock().unwrap(),
            next_golden_path_id: *self.next_golden_path_id.lock().unwrap(),
            next_retention_audit_id: *self.next_retention_audit_id.lock().unwrap(),
            next_retention_policy_id: *self.next_retention_policy_id.lock().unwrap(),
        };
        let _ = metadata::save(path, &state);
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

    /// 把一条记录喂进**派生检索索引**：BM25 中文倒排 + 过滤属性边车。
    /// ingest、WAL 重放、从段重建索引三处共用 —— 派生索引的喂法只此一份。
    fn index_record(&self, r: &WalRecord) {
        self.index_record_inner(r, true);
    }

    fn index_record_inner(&self, r: &WalRecord, index_live_attr_postings: bool) {
        // 中文倒排：把该 span 的**可检索文本**喂进 BM25。检索的主对象是 LLM 的输入/输出原文
        // （input_text/output_text），logs（含 span name）作补充。三者拼起来索引——真实 SDK 灌进来的
        // input/output 文本会被索引，而不是只索引 logs（否则真实数据上"中文检索"会突然失效）。
        let mut parts: Vec<&str> = Vec::new();
        if let Some(t) = r.fields.input_text.as_deref() {
            parts.push(t);
        }
        if let Some(t) = r.fields.output_text.as_deref() {
            parts.push(t);
        }
        // agent/tool/model 名也索引——用户会按"搜某个 agent/tool 的 trace"（如"搜风控 agent 的报错"）。
        for field in [&r.fields.agent_name, &r.fields.tool_name, &r.fields.model] {
            if let Some(t) = field.as_deref() {
                parts.push(t);
            }
        }
        for l in &r.fields.logs {
            parts.push(l);
        }
        if !parts.is_empty() {
            self.bm25
                .index_text(r.trace_id, r.span_id, &parts.join(" "));
        }
        self.text_domains.lock().unwrap().index_record(r);
        let span_key = (r.trace_id, r.span_id);
        self.trace_span_keys
            .lock()
            .unwrap()
            .entry(r.trace_id)
            .or_default()
            .insert(span_key);
        // 过滤属性边车：last-non-null 累积 status/agent，ts 取范围（带过滤 ANN 的 payload）。
        let mut fa = self.filter_attrs.lock().unwrap();
        let a = fa.entry(span_key).or_insert(FilterAttrs {
            min_ts: r.ts,
            max_ts: r.ts,
            ..Default::default()
        });
        if r.fields.status.is_some() {
            a.status = r.fields.status;
        }
        if r.fields.agent_name.is_some() {
            a.agent_name = r.fields.agent_name.clone();
        }
        if r.fields.tenant_id.is_some() {
            a.tenant_id = r.fields.tenant_id;
        }
        let mut attr_updates = Vec::new();
        emit_indexable_attrs(&r.fields, |k, v| {
            if is_filter_attr_key(k) {
                let old = a.attrs.get(k).cloned();
                a.attrs.insert(k.to_string(), v.to_string());
                attr_updates.push((k.to_string(), old, v.to_string()));
            }
        });
        a.min_ts = a.min_ts.min(r.ts);
        a.max_ts = a.max_ts.max(r.ts);
        drop(fa);
        if index_live_attr_postings && !attr_updates.is_empty() {
            let mut postings = self.attr_postings.lock().unwrap();
            for (attr_key, old, new) in attr_updates {
                postings.update(span_key, &attr_key, old.as_deref(), &new);
            }
        }

        // 会话边车：用 last-non-null 算出该 span 的新聚合，差量更新会话级（增量、O(1)/事件）。
        let mut idx = self.session_idx.lock().unwrap();
        let mut new = idx.span.get(&span_key).cloned().unwrap_or_default();
        new.trace = r.trace_id;
        if let Some(s) = r.fields.session_id {
            new.session = Some(s);
        }
        if let Some(s) = &r.fields.external_session_id {
            new.external_session = Some(s.clone());
        }
        if let Some(t) = r.fields.input_tokens {
            new.in_tok = t;
        }
        if let Some(t) = r.fields.output_tokens {
            new.out_tok = t;
        }
        if let Some(t) = r.fields.cached_input_tokens {
            new.cached_tok = t;
        }
        if let Some(t) = r.fields.reasoning_tokens {
            new.reasoning_tok = t;
        }
        if let Some(t) = r.fields.total_tokens {
            new.explicit_total_tok = Some(t);
        }
        if let Some(c) = r.fields.cost_usd_nanos {
            new.explicit_cost_usd_nanos = Some(c);
        }
        new.total_tok = usage_total_tokens(
            new.in_tok,
            new.out_tok,
            new.cached_tok,
            new.reasoning_tok,
            new.explicit_total_tok,
        );
        new.cost_usd_nanos = usage_cost_usd_nanos_for_model(
            new.in_tok,
            new.out_tok,
            new.cached_tok,
            new.reasoning_tok,
            new.explicit_cost_usd_nanos,
            r.fields.provider.as_deref(),
            r.fields.model.as_deref(),
        );
        if let Some(st) = r.fields.status {
            new.error = st != 0;
        }
        if new.agent.is_none() {
            if let Some(a) = &r.fields.agent_name {
                new.agent = Some(a.clone());
            }
        }
        idx.apply_span(span_key, new);
    }

    /// 写入：先进 WAL（ack 后才算持久），同步进活 MemTable，再推进已提交尾。
    /// 折叠在读时做，所以写路径不需要「脏队列」（决策文档已去掉 fold_dirty）。
    /// 整个过 write_lock 串行（单写者）。
    pub fn ingest(&self, records: Vec<WalRecord>) -> WalLsn {
        let _w = self.write_lock.lock().unwrap();
        let dirty_traces: HashSet<u64> = records.iter().map(|r| r.trace_id).collect();
        let mut wal = self.wal.lock().unwrap();
        // 这批的起始 LSN（在 append 之前确定），逐条分配 commit_lsn。
        let first = wal.committed_tail().get() + 1;
        {
            let mut mt = self.memtable.lock().unwrap();
            for (i, r) in records.iter().enumerate() {
                self.index_record(r); // 喂检索索引（BM25 + 属性边车）
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
        if !dirty_traces.is_empty() {
            self.trace_trajectory_idx
                .lock()
                .unwrap()
                .retain(|(_, trace_id), _| !dirty_traces.contains(trace_id));
        }
        // ack 之后才推进 committed_tail（读者据此取 live_lsn 上界）。
        self.current.advance_committed_tail(last);

        // 内存表超阈值就自动刷盘，兜住内存上界（OPEN-2）。仍在 write_lock 下。
        if self.memtable.lock().unwrap().len() >= self.flush_threshold.load(Ordering::Relaxed) {
            self.flush_memtable_locked();
        }
        // 会话边车已在 index_record 里逐事件增量维护，这里无需额外动作。
        let n = first;
        let cnt = last.get() - first + 1;
        let tail = last.get();
        olog::log(
            olog::Level::Debug,
            "ingest",
            &[("lsn", &n), ("count", &cnt), ("tail", &tail)],
        );
        last
    }

    /// 摄入 SDK 线格式记录：转成内部 WalRecord（引擎自算 event_id）后走正常 `ingest`。
    /// 这是「打点 → 引擎存」的数据契约入口；上面再套一层 HTTP/OTLP 网关即闭环（网关是纯管道）。
    pub fn ingest_wire(&self, records: Vec<WireRecord>) -> WalLsn {
        let recs: Vec<WalRecord> = records
            .into_iter()
            .map(WireRecord::into_wal_record)
            .collect();
        self.ingest(recs)
    }

    /// HTTP 网关专用摄入：租户来自鉴权上下文（如 `X-Tenant-Id`），覆盖 wire body 里的 tenant_id。
    /// 这是多租户安全边界；SDK/客户端可以重复发送 body，但不能自选或伪造租户。
    pub fn ingest_wire_for_tenant(
        &self,
        mut records: Vec<WireRecord>,
        tenant: Option<u64>,
    ) -> WalLsn {
        for r in &mut records {
            r.tenant_id = tenant;
        }
        self.ingest_wire(records)
    }

    /// 摄入 OTLP/OpenInference 标准 trace（OTLP/HTTP JSON）：经适配器映射成 WireRecord 后走正常摄入。
    /// 这是「生态入口」——已用 OpenTelemetry / OpenInference 埋点的 agent 应用不改打点即可灌进来。
    /// 解析失败返回 Err（调用方/HTTP 网关据此回 400）。
    pub fn ingest_otlp(&self, body: &str) -> Result<WalLsn, String> {
        let wires = parse_otlp_traces(body)?;
        Ok(self.ingest_wire(wires))
    }

    /// HTTP 网关专用 OTLP 摄入：OTLP attributes 里带的 tenant 也不作为安全边界，统一由请求上下文覆盖。
    pub fn ingest_otlp_for_tenant(
        &self,
        body: &str,
        tenant: Option<u64>,
    ) -> Result<WalLsn, String> {
        let wires = parse_otlp_traces(body)?;
        Ok(self.ingest_wire_for_tenant(wires, tenant))
    }

    /// 设置内存表自动刷盘阈值（行数）。
    pub fn set_flush_threshold(&self, n: usize) {
        self.flush_threshold.store(n.max(1), Ordering::Relaxed);
    }

    /// 当前内存表行数（可观测 / 测试用）。
    pub fn memtable_len(&self) -> usize {
        self.memtable.lock().unwrap().len()
    }

    /// 主动把内存表当前内容封成一个段（周期刷盘 / 关机前）。
    pub fn flush_memtable(&self) {
        let _w = self.write_lock.lock().unwrap();
        let before = self.memtable.lock().unwrap().len();
        let v_before = self.current.version();
        self.flush_memtable_locked();
        let seg = v_before;
        olog::log(
            olog::Level::Info,
            "flush",
            &[
                ("seg", &seg),
                ("rows", &before),
                ("version", &self.current.version()),
            ],
        );
    }

    /// 把内存表内容封段（调用方须已持 write_lock）。watermark 推进到内存表最新 LSN。
    fn flush_memtable_locked(&self) {
        let (records, max_lsn) = {
            let mt = self.memtable.lock().unwrap();
            if mt.is_empty() {
                return;
            }
            let records: Vec<WalRecord> = mt
                .iter()
                .map(|r| WalRecord {
                    trace_id: r.trace_id,
                    span_id: r.span_id,
                    ts: r.ts,
                    identity: r.identity.clone(),
                    fields: r.fields.clone(),
                })
                .collect();
            (records, mt.newest_lsn().unwrap())
        };
        let seg = self.alloc_segment_id();
        self.segments.flush_to_segment(seg, &records);
        self.install_segment_attr_sidecar(seg, &records, true);
        self.install_trace_aggregate_segment_rollup(seg, &records, true);
        // 段级 key bloom：从这批记录的 (trace,span) 建，供检索折叠定位跳过无关段。
        let bloom = KeyBloom::build(
            records.iter().map(|r| (r.trace_id, r.span_id)),
            records.len(),
        );
        self.seg_key_bloom
            .lock()
            .unwrap()
            .insert(seg.get(), Arc::new(bloom));
        let (min_ts, max_ts) = ts_range(&records);
        let mut draft = self.current.cow_next();
        draft.memtable_watermark = WalLsn::new(max_lsn);
        draft.segments.insert(
            seg.get(),
            SegmentEntry {
                segment_id: seg,
                level: 0,
                state: SegState::Live,
                min_ts,
                max_ts,
                deletion_vec: Arc::new(DeletionVec::empty()),
                deletion_seq: 0,
                upgrade_ref: None,
                upgrade_seq: 0,
            },
        );
        self.commit_and_persist(draft);
        let gate = WalLsn::new(self.current.min_retained_watermark());
        self.memtable.lock().unwrap().evict_up_to(gate);
        self.rebuild_live_attr_postings_from_memtable();
    }
}
