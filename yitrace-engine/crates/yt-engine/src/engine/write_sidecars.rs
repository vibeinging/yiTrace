impl WriteCoordinator {
    pub fn trace_aggregate_rollup_spans(
        &self,
        q: &TraceQuery,
        filter: &SearchFilter,
    ) -> Option<(Vec<FoldedSpan>, ReadPlanStats)> {
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
        let Some(path) = &self.trace_rollup_path else {
            return;
        };
        let manifest = self.current.manifest();
        let (records, patches) = self.collect_segment_rollup_parts(&manifest);
        let rollup = TraceAggregateRollupIndex::from_records(records, patches);
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

    fn persist_filter_attrs_segments(&self) {
        let Some(path) = &self.filter_attrs_path else {
            return;
        };
        let manifest = self.current.manifest();
        let (records, patches) = self.collect_segment_rollup_parts(&manifest);
        let index = FilterAttrsIndex::from_records(records, patches);
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
    }

    fn persist_read_model_sidecars(&self) {
        self.persist_trace_rollup_segments();
        self.persist_filter_attrs_segments();
    }

    fn filter_candidate_span_keys(&self, filter: &SearchFilter) -> HashSet<(u64, u64)> {
        self.filter_attrs
            .lock()
            .unwrap()
            .candidate_span_keys(filter)
    }
}
