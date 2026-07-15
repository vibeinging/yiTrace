const SEG_BLOOM_CACHE_MAGIC: u32 = 0x5954_424c; // "YTBL"
const SEG_BLOOM_CACHE_VERSION: u32 = 1;

impl WriteCoordinator {
    pub fn trace_aggregate_rollup_spans(
        &self,
        q: &TraceQuery,
        filter: &SearchFilter,
    ) -> Option<(Vec<FoldedSpan>, ReadPlanStats)> {
        self.ensure_trace_rollup_current();
        let result = self.trace_rollup.lock().unwrap().query(q, filter)?;
        if result
            .1
            .fallback_reason
            .as_deref()
            .map_or(false, |reason| reason == "rollup_dirty")
        {
            return None;
        }
        Some(result)
    }

    pub fn trace_rollup_spans_for_trace_ids(
        &self,
        trace_ids: &[u64],
        tenant: Option<u64>,
    ) -> Option<(BTreeMap<u64, Vec<FoldedSpan>>, ReadPlanStats)> {
        self.ensure_trace_rollup_current();
        let result = self
            .trace_rollup
            .lock()
            .unwrap()
            .query_trace_ids(trace_ids, tenant)?;
        if result
            .1
            .fallback_reason
            .as_deref()
            .map_or(false, |reason| reason == "rollup_dirty")
        {
            return None;
        }
        Some(result)
    }

    fn rebuild_trace_rollup_from_snapshot(&self, snap: &Snapshot) {
        let (mut records, patches) = self.collect_segment_rollup_parts(&snap.manifest);

        {
            let mt = self.memtable.lock().unwrap();
            for row in mt.read_range(snap.retained_watermark, snap.live_lsn) {
                records.push(WalRecord {
                    trace_id: row.trace_id,
                    span_id: row.span_id,
                    ts: row.ts,
                    identity: row.identity.clone(),
                    fields: row.fields.clone(),
                });
            }
        }

        self.trace_rollup.lock().unwrap().rebuild(records, patches);
    }

    fn collect_segment_rollup_parts(
        &self,
        manifest: &Manifest,
    ) -> (Vec<WalRecord>, Vec<((u64, u64), SpanFields)>) {
        let mut records = Vec::new();
        let mut patches = Vec::new();
        for entry in manifest.segments.values() {
            for (row, record) in self
                .segments
                .scan_records(entry.segment_id)
                .into_iter()
                .enumerate()
            {
                if entry.deletion_vec.is_deleted(row as u32) {
                    continue;
                }
                records.push(record);
            }
            if let Some(upgrade) = &entry.upgrade_ref {
                patches.extend(
                    upgrade.iter().map(|(&(trace_id, span_id), fields)| {
                        ((trace_id, span_id), fields.clone())
                    }),
                );
            }
        }
        (records, patches)
    }

    fn load_trace_rollup_segments(&self, manifest: &Manifest) -> bool {
        let Some(path) = &self.trace_rollup_path else {
            return false;
        };
        let Some(rollup) = TraceAggregateRollupIndex::load_cache(
            path,
            manifest.version.get(),
            manifest.memtable_watermark.get(),
        ) else {
            return false;
        };
        let row_count = rollup.len();
        *self.trace_rollup.lock().unwrap() = rollup;
        olog::log(
            olog::Level::Info,
            "trace_rollup_cache_load",
            &[
                ("rows", &row_count),
                ("version", &manifest.version.get()),
                ("watermark", &manifest.memtable_watermark.get()),
            ],
        );
        true
    }

    fn persist_trace_rollup_segments(&self) {
        if !self.read_model_load_state.lock().unwrap().rollup_ready {
            return;
        }
        let Some(path) = &self.trace_rollup_path else {
            return;
        };
        let manifest = self.current.manifest();
        let mut rollup = self.trace_rollup.lock().unwrap();
        if let Err(err) = rollup.save_cache(
            path,
            manifest.version.get(),
            manifest.memtable_watermark.get(),
        ) {
            olog::log(
                olog::Level::Warn,
                "trace_rollup_cache_save_failed",
                &[("error", &err.to_string())],
            );
        }
    }

    fn rebuild_trace_rollup_current(&self) {
        let snap = self.pin_snapshot();
        self.rebuild_trace_rollup_from_snapshot(&snap);
        self.read_model_load_state.lock().unwrap().rollup_ready = true;
    }

    fn ensure_trace_rollup_current(&self) {
        if self.read_model_load_state.lock().unwrap().rollup_ready {
            return;
        }
        let _process = self.acquire_process_lock("write");
        let _local = self.write_lock.lock().unwrap();
        self.refresh_from_disk_locked();
        self.ensure_trace_rollup_current_locked();
    }

    fn ensure_trace_rollup_current_locked(&self) {
        if self.read_model_load_state.lock().unwrap().rollup_ready {
            return;
        }
        let manifest = self.current.manifest();
        let derived_dirty = manifest
            .segments
            .values()
            .any(|entry| entry.deletion_seq > 0 || entry.upgrade_ref.is_some());
        let loaded = !derived_dirty && self.load_trace_rollup_segments(&manifest);
        drop(manifest);
        if !loaded {
            let snap = self.current.pin_snapshot();
            self.rebuild_trace_rollup_from_snapshot(&snap);
        }
        self.read_model_load_state.lock().unwrap().rollup_ready = true;
        if !loaded {
            self.persist_trace_rollup_segments();
        }
    }

    fn rebuild_filter_attrs_from_snapshot(&self, snap: &Snapshot) {
        let (mut records, patches) = self.collect_segment_rollup_parts(&snap.manifest);
        {
            let mt = self.memtable.lock().unwrap();
            for row in mt.read_range(snap.retained_watermark, snap.live_lsn) {
                records.push(WalRecord {
                    trace_id: row.trace_id,
                    span_id: row.span_id,
                    ts: row.ts,
                    identity: row.identity.clone(),
                    fields: row.fields.clone(),
                });
            }
        }
        self.filter_attrs.lock().unwrap().rebuild(records, patches);
    }

    fn load_filter_attrs_segments(&self, manifest: &Manifest) -> bool {
        let Some(path) = &self.filter_attrs_path else {
            return false;
        };
        let Some(index) = FilterAttrsIndex::load_cache(
            path,
            manifest.version.get(),
            manifest.memtable_watermark.get(),
        ) else {
            return false;
        };
        let row_count = index.len();
        let posting_count = index.posting_count();
        *self.filter_attrs.lock().unwrap() = index;
        olog::log(
            olog::Level::Info,
            "filter_attrs_cache_load",
            &[
                ("rows", &row_count),
                ("postings", &posting_count),
                ("version", &manifest.version.get()),
                ("watermark", &manifest.memtable_watermark.get()),
            ],
        );
        true
    }

    fn load_bm25_segments(&self, manifest: &Manifest) -> bool {
        let Some(path) = &self.bm25_path else {
            return false;
        };
        if !self
            .bm25
            .load_cache(path, manifest.version.get(), manifest.memtable_watermark.get())
        {
            return false;
        }
        olog::log(
            olog::Level::Info,
            "bm25_cache_load",
            &[
                ("version", &manifest.version.get()),
                ("watermark", &manifest.memtable_watermark.get()),
            ],
        );
        true
    }

    fn persist_bm25_segments(&self) {
        let Some(path) = &self.bm25_path else {
            return;
        };
        if *self.segment_scan_indexes_stale.lock().unwrap() {
            return;
        }
        let manifest = self.current.manifest();
        match self
            .bm25
            .save_cache(path, manifest.version.get(), manifest.memtable_watermark.get())
        {
            Ok(true) | Ok(false) => {}
            Err(err) => olog::log(
                olog::Level::Warn,
                "bm25_cache_save_failed",
                &[("error", &err.to_string())],
            ),
        }
    }

    fn rebuild_bm25_from_snapshot(&self, snap: &Snapshot) {
        let (spans, _) = self.read_spans_query(snap, &TraceQuery::all());
        self.bm25.clear();
        for span in spans {
            self.index_folded_span_text(&span);
        }
        // delete/upgrade 后文本按折叠 span 重建，但幂等键仍来自原始事件。补齐 event_id 表，
        // 否则一次维护操作后，迟到的 SDK retry 会重新增加词频。
        for entry in snap.manifest.segments.values() {
            for record in self.segments.scan_records(entry.segment_id) {
                self.bm25.mark_event(record.identity.event_id().0);
            }
        }
        let memtable = self.memtable.lock().unwrap();
        for row in memtable.read_range(snap.retained_watermark, snap.live_lsn) {
            self.bm25.mark_event(row.identity.event_id().0);
        }
    }

    fn rebuild_bm25_current(&self) {
        let snap = self.pin_snapshot();
        self.rebuild_bm25_from_snapshot(&snap);
    }

    fn index_folded_span_text(&self, span: &FoldedSpan) {
        let mut parts: Vec<&str> = Vec::new();
        if let Some(text) = span.input_text.as_deref() {
            parts.push(text);
        }
        if let Some(text) = span.output_text.as_deref() {
            parts.push(text);
        }
        if let Some(text) = span.span_name.as_deref() {
            parts.push(text);
        }
        for field in [&span.agent_name, &span.tool_name, &span.model] {
            if let Some(text) = field.as_deref() {
                parts.push(text);
            }
        }
        for log in &span.logs {
            parts.push(log);
        }
        if !parts.is_empty() {
            self.bm25
                .index_text(span.trace_id, span.span_id, &parts.join(" "));
        }
    }

    fn load_seg_key_bloom_segments(&self, manifest: &Manifest) -> bool {
        let Some(path) = &self.seg_key_bloom_path else {
            return false;
        };
        let Some(blooms) = load_seg_key_bloom_cache(
            path,
            manifest.version.get(),
            manifest.memtable_watermark.get(),
            manifest,
        ) else {
            return false;
        };
        let count = blooms.len();
        *self.seg_key_bloom.lock().unwrap() = blooms;
        olog::log(
            olog::Level::Info,
            "segment_bloom_cache_load",
            &[
                ("segments", &count),
                ("version", &manifest.version.get()),
                ("watermark", &manifest.memtable_watermark.get()),
            ],
        );
        true
    }

    fn persist_seg_key_bloom_segments(&self) {
        let Some(path) = &self.seg_key_bloom_path else {
            return;
        };
        if *self.segment_scan_indexes_stale.lock().unwrap() {
            return;
        }
        let manifest = self.current.manifest();
        let blooms = self.seg_key_bloom.lock().unwrap();
        if let Err(err) = save_seg_key_bloom_cache(
            path,
            manifest.version.get(),
            manifest.memtable_watermark.get(),
            &manifest,
            &blooms,
        ) {
            olog::log(
                olog::Level::Warn,
                "segment_bloom_cache_save_failed",
                &[("error", &err.to_string())],
            );
        }
    }

    fn persist_filter_attrs_segments(&self) {
        if !self
            .read_model_load_state
            .lock()
            .unwrap()
            .filter_attrs_ready
        {
            return;
        }
        let Some(path) = &self.filter_attrs_path else {
            return;
        };
        let manifest = self.current.manifest();
        let index = self.filter_attrs.lock().unwrap();
        if let Err(err) = index.save_cache(
            path,
            manifest.version.get(),
            manifest.memtable_watermark.get(),
        ) {
            olog::log(
                olog::Level::Warn,
                "filter_attrs_cache_save_failed",
                &[("error", &err.to_string())],
            );
        }
    }

    fn rebuild_filter_attrs_current(&self) {
        let snap = self.pin_snapshot();
        self.rebuild_filter_attrs_from_snapshot(&snap);
        self.read_model_load_state
            .lock()
            .unwrap()
            .filter_attrs_ready = true;
    }

    fn ensure_filter_attrs_current(&self) {
        if self
            .read_model_load_state
            .lock()
            .unwrap()
            .filter_attrs_ready
        {
            return;
        }
        let _process = self.acquire_process_lock("write");
        let _local = self.write_lock.lock().unwrap();
        self.refresh_from_disk_locked();
        self.ensure_filter_attrs_current_locked();
    }

    fn ensure_filter_attrs_current_locked(&self) {
        if self
            .read_model_load_state
            .lock()
            .unwrap()
            .filter_attrs_ready
        {
            return;
        }
        let manifest = self.current.manifest();
        let derived_dirty = manifest
            .segments
            .values()
            .any(|entry| entry.deletion_seq > 0 || entry.upgrade_ref.is_some());
        let loaded = !derived_dirty && self.load_filter_attrs_segments(&manifest);
        drop(manifest);
        if !loaded {
            let snap = self.current.pin_snapshot();
            self.rebuild_filter_attrs_from_snapshot(&snap);
        }
        self.read_model_load_state
            .lock()
            .unwrap()
            .filter_attrs_ready = true;
        if !loaded {
            self.persist_filter_attrs_segments();
        }
    }

    fn persist_read_model_sidecars(&self) {
        self.persist_trace_rollup_segments();
        self.persist_filter_attrs_segments();
        self.persist_bm25_segments();
        self.persist_seg_key_bloom_segments();
    }

    fn filter_candidate_span_keys(&self, filter: &SearchFilter) -> HashSet<(u64, u64)> {
        self.ensure_filter_attrs_current();
        let mut index = self.filter_attrs.lock().unwrap();
        index.candidate_span_keys(filter)
    }
}

fn save_seg_key_bloom_cache(
    path: &std::path::Path,
    manifest_version: u64,
    memtable_watermark: u64,
    manifest: &Manifest,
    blooms: &HashMap<u64, Arc<KeyBloom>>,
) -> std::io::Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    let mut out = Vec::new();
    sidecar_put_u32(&mut out, SEG_BLOOM_CACHE_MAGIC);
    sidecar_put_u32(&mut out, SEG_BLOOM_CACHE_VERSION);
    sidecar_put_u64(&mut out, manifest_version);
    sidecar_put_u64(&mut out, memtable_watermark);
    sidecar_put_u64(&mut out, manifest.segments.len() as u64);
    for &seg_id in manifest.segments.keys() {
        let Some(bloom) = blooms.get(&seg_id) else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("missing segment bloom for segment {seg_id}"),
            ));
        };
        sidecar_put_u64(&mut out, seg_id);
        sidecar_put_u32(&mut out, bloom.k);
        sidecar_put_u64(&mut out, bloom.bits.len() as u64);
        for &word in &bloom.bits {
            sidecar_put_u64(&mut out, word);
        }
    }
    std::fs::create_dir_all(parent)?;
    let tmp = path.with_extension("tmp");
    let mut file = std::fs::File::create(&tmp)?;
    std::io::Write::write_all(&mut file, &out)?;
    file.sync_all()?;
    drop(file);
    crate::test_failpoints::before_sidecar_rename("segment_bloom", path);
    std::fs::rename(tmp, path)
}

fn load_seg_key_bloom_cache(
    path: &std::path::Path,
    manifest_version: u64,
    memtable_watermark: u64,
    manifest: &Manifest,
) -> Option<HashMap<u64, Arc<KeyBloom>>> {
    let bytes = std::fs::read(path).ok()?;
    let mut cur = SidecarCursor { bytes: &bytes, pos: 0 };
    if cur.u32()? != SEG_BLOOM_CACHE_MAGIC || cur.u32()? != SEG_BLOOM_CACHE_VERSION {
        return None;
    }
    if cur.u64()? != manifest_version || cur.u64()? != memtable_watermark {
        return None;
    }
    let count = cur.u64()? as usize;
    let mut out = HashMap::with_capacity(count);
    for _ in 0..count {
        let seg_id = cur.u64()?;
        let k = cur.u32()?;
        let bit_words = cur.u64()? as usize;
        let mut bits = Vec::with_capacity(bit_words);
        for _ in 0..bit_words {
            bits.push(cur.u64()?);
        }
        let bloom = KeyBloom::from_bits(bits, k)?;
        out.insert(seg_id, Arc::new(bloom));
    }
    if cur.pos != bytes.len() || out.len() != manifest.segments.len() {
        return None;
    }
    if manifest
        .segments
        .keys()
        .any(|seg_id| !out.contains_key(seg_id))
    {
        return None;
    }
    Some(out)
}

fn sidecar_put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn sidecar_put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

struct SidecarCursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> SidecarCursor<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        if end > self.bytes.len() {
            return None;
        }
        let out = &self.bytes[self.pos..end];
        self.pos = end;
        Some(out)
    }

    fn u32(&mut self) -> Option<u32> {
        let mut b = [0u8; 4];
        b.copy_from_slice(self.take(4)?);
        Some(u32::from_le_bytes(b))
    }

    fn u64(&mut self) -> Option<u64> {
        let mut b = [0u8; 8];
        b.copy_from_slice(self.take(8)?);
        Some(u64::from_le_bytes(b))
    }
}
