#[derive(Clone, Debug, Default)]
pub struct AttrIndexedReadStats {
    pub used_attr_postings: bool,
    pub candidate_span_keys: Option<usize>,
    pub scanned_segments: usize,
    pub unsupported_attr_keys: Vec<String>,
}

impl AttrIndexedReadStats {
    pub fn add_shard(&mut self, other: &AttrIndexedReadStats) {
        self.used_attr_postings |= other.used_attr_postings;
        self.scanned_segments += other.scanned_segments;
        self.candidate_span_keys = match (self.candidate_span_keys, other.candidate_span_keys) {
            (Some(left), Some(right)) => Some(left + right),
            (Some(left), None) => Some(left),
            (None, Some(right)) => Some(right),
            (None, None) => None,
        };
        for key in &other.unsupported_attr_keys {
            if !self.unsupported_attr_keys.contains(key) {
                self.unsupported_attr_keys.push(key.clone());
            }
        }
    }
}

impl WriteCoordinator {

    /// 读 MemTable 源：某快照可见的半开区间 `(retained_watermark, live_lsn]`（测试/折叠用）。
    pub fn read_memtable_lsns(&self, snap: &Snapshot) -> Vec<u64> {
        self.memtable
            .lock()
            .unwrap()
            .read_range(snap.retained_watermark, snap.live_lsn)
            .map(|r| r.commit_lsn)
            .collect()
    }

    /// 读路径：在固定快照上跨四源折叠出可见的所有 span（草案 2 §D2.2 端到端，全开窗）。
    pub fn read_spans(&self, snap: &Snapshot) -> Vec<FoldedSpan> {
        self.read_spans_query(snap, &TraceQuery::all()).0
    }

    /// 取某段的折叠缓存（不可变段，首次解码全列 + 建 (trace,span)→行号 索引，之后命中直接用）。
    fn seg_fold(&self, seg: SegmentId) -> Arc<SegFold> {
        {
            let mut c = self.seg_fold_cache.lock().unwrap();
            c.tick += 1;
            let t = c.tick;
            if let Some(e) = c.map.get_mut(&seg.get()) {
                e.1 = t;
                return e.0.clone();
            }
        }
        // 未命中：在锁外解码整段一次（之后所有查询命中缓存）。
        let raw = self.segments.scan_fold_inputs(seg);
        let mut rows = Vec::with_capacity(raw.len());
        let mut by_key: HashMap<(u64, u64), Vec<u32>> = HashMap::new();
        for (row, fi) in raw {
            by_key
                .entry((fi.trace_id, fi.span_id))
                .or_default()
                .push(row);
            rows.push(fi);
        }
        let n = rows.len();
        let sf = Arc::new(SegFold { rows, by_key });
        let mut c = self.seg_fold_cache.lock().unwrap();
        c.tick += 1;
        let tk = c.tick;
        if let Some((old, _)) = c.map.insert(seg.get(), (sf.clone(), tk)) {
            c.cur_rows -= old.rows.len();
        }
        c.cur_rows += n;
        if c.cur_rows > c.cap_rows {
            c.evict();
        }
        sf
    }

    fn register_segment_attr_sidecar(
        &self,
        seg: SegmentId,
        sidecar: Arc<SegmentAttrSidecar>,
        cache: bool,
    ) {
        self.seg_attr_directory
            .lock()
            .unwrap()
            .add_segment(seg, &sidecar);
        if cache {
            self.seg_attr_cache.lock().unwrap().insert(seg, sidecar);
        }
    }

    fn install_segment_attr_sidecar(&self, seg: SegmentId, records: &[WalRecord], cache: bool) {
        let sidecar = Arc::new(SegmentAttrSidecar::build(records));
        if let Some(dir) = &self.attr_sidecar_dir {
            let _ = write_segment_attr_sidecar_file(dir, seg, &sidecar);
        }
        self.register_segment_attr_sidecar(seg, sidecar, cache);
    }

    fn segment_attr_sidecar(&self, seg: SegmentId) -> Arc<SegmentAttrSidecar> {
        if let Some(sidecar) = self.seg_attr_cache.lock().unwrap().get(seg) {
            return sidecar;
        }
        let loaded = self
            .attr_sidecar_dir
            .as_ref()
            .and_then(|dir| read_segment_attr_sidecar_file(dir, seg))
            .unwrap_or_else(|| {
                let records = self.segments.scan_records(seg);
                let sidecar = SegmentAttrSidecar::build(&records);
                if let Some(dir) = &self.attr_sidecar_dir {
                    let _ = write_segment_attr_sidecar_file(dir, seg, &sidecar);
                }
                sidecar
            });
        let sidecar = Arc::new(loaded);
        self.register_segment_attr_sidecar(seg, sidecar.clone(), false);
        self.seg_attr_cache.lock().unwrap().insert(seg, sidecar)
    }

    fn live_span_keys(&self) -> HashSet<SpanKey> {
        self.memtable
            .lock()
            .unwrap()
            .iter()
            .map(|r| (r.trace_id, r.span_id))
            .collect()
    }

    fn rebuild_live_attr_postings_from_memtable(&self) {
        let records: Vec<WalRecord> = self
            .memtable
            .lock()
            .unwrap()
            .iter()
            .map(|r| WalRecord {
                trace_id: r.trace_id,
                span_id: r.span_id,
                ts: r.ts,
                identity: r.identity.clone(),
                fields: r.fields.clone(),
            })
            .collect();
        let mut by_span: HashMap<SpanKey, BTreeMap<String, String>> = HashMap::new();
        let mut postings = AttrPostings::default();
        for r in &records {
            let span_key = (r.trace_id, r.span_id);
            let attrs = by_span.entry(span_key).or_default();
            emit_indexable_attrs(&r.fields, |attr_key, new| {
                if !is_filter_attr_key(attr_key) {
                    return;
                }
                let old = attrs.insert(attr_key.to_string(), new.to_string());
                postings.update(span_key, attr_key, old.as_deref(), new);
            });
        }
        *self.attr_postings.lock().unwrap() = postings;
    }

    /// 带剪枝的读路径。按时间窗（段 zone-map）+ trace_id 剪枝，减少触及的段数（活 trace 读扇出上界）。
    /// 返回 (折叠出的 span, 实际扫描的段数)。所有判定只用快照里钉死的版本。
    pub fn read_spans_query(&self, snap: &Snapshot, q: &TraceQuery) -> (Vec<FoldedSpan>, usize) {
        // 普通读 / trace 详情要原文,读全列。
        self.fold_query(snap, q, None, Projection::ALL)
    }

    fn attr_matching_span_keys(
        &self,
        snap: &Snapshot,
        attrs: &BTreeMap<String, String>,
    ) -> Option<HashSet<(u64, u64)>> {
        if attrs.is_empty() {
            return None;
        }
        let mut out: Option<HashSet<SpanKey>> = None;
        let mut used_index = false;
        for (attr_key, expected) in attrs {
            if !is_postings_attr_key(attr_key) {
                continue;
            }
            used_index = true;
            let mut one = self.segment_attr_candidates_for_attr(snap, attr_key, expected);
            if let Some(live) = self.live_attr_candidates_for_attr(attr_key, expected) {
                one.extend(live);
            }
            out = Some(match out {
                None => one,
                Some(prev) => prev.intersection(&one).copied().collect(),
            });
            if out.as_ref().map_or(false, HashSet::is_empty) {
                break;
            }
        }
        if used_index {
            Some(out.unwrap_or_default())
        } else {
            None
        }
    }

    fn segment_attr_candidates_for_attr(
        &self,
        snap: &Snapshot,
        attr_key: &str,
        expected: &str,
    ) -> HashSet<SpanKey> {
        let mut out = HashSet::new();
        let seg_ids = self
            .seg_attr_directory
            .lock()
            .unwrap()
            .candidate_segments_for_attr(attr_key, expected)
            .unwrap_or_default();
        for seg_id in seg_ids {
            let Some(entry) = snap.manifest.segments.get(&seg_id) else {
                continue;
            };
            let sidecar = self.segment_attr_sidecar(entry.segment_id);
            out.extend(sidecar.candidates_for_attr(attr_key, expected));
        }
        for entry in snap.manifest.segments.values() {
            let Some(upgrade) = &entry.upgrade_ref else {
                continue;
            };
            for (&span_key, fields) in upgrade.iter() {
                if fields_attr_value(fields, attr_key)
                    .map(|actual| attr_json_matches(actual, expected))
                    .unwrap_or(false)
                {
                    out.insert(span_key);
                }
            }
        }
        out
    }

    fn live_attr_candidates_for_attr(
        &self,
        attr_key: &str,
        expected: &str,
    ) -> Option<HashSet<SpanKey>> {
        if !is_postings_attr_key(attr_key) {
            return None;
        }
        let postings = self.attr_postings.lock().unwrap();
        if postings.key_is_complete(attr_key) {
            Some(postings.candidates_for_attr(attr_key, expected))
        } else {
            drop(postings);
            Some(self.live_span_keys())
        }
    }

    fn span_keys_for_trace_ids(&self, trace_ids: &HashSet<u64>) -> HashSet<(u64, u64)> {
        if trace_ids.is_empty() {
            return HashSet::new();
        }
        let by_trace = self.trace_span_keys.lock().unwrap();
        let mut out = HashSet::new();
        for trace_id in trace_ids {
            if let Some(keys) = by_trace.get(trace_id) {
                out.extend(keys.iter().copied());
            }
        }
        out
    }

    /// 带 attrs 候选集的结构化查询：先用 attrs postings 缩小 span key，再走折叠读路径校验。
    /// postings 可能因删除/compaction 返回陈旧超集，最终语义仍以 snapshot 折叠结果为准。
    pub fn read_spans_query_for_attrs(
        &self,
        snap: &Snapshot,
        q: &TraceQuery,
        attrs: &BTreeMap<String, String>,
    ) -> Vec<FoldedSpan> {
        self.read_spans_query_for_attrs_with_stats(snap, q, attrs).0
    }

    pub fn read_spans_query_for_attrs_with_stats(
        &self,
        snap: &Snapshot,
        q: &TraceQuery,
        attrs: &BTreeMap<String, String>,
    ) -> (Vec<FoldedSpan>, AttrIndexedReadStats) {
        let mut stats = AttrIndexedReadStats::default();
        if attrs.is_empty() {
            let (spans, scanned) = self.read_spans_query(snap, q);
            stats.scanned_segments = scanned;
            return (spans, stats);
        }
        for attr_key in attrs.keys() {
            if is_postings_attr_key(attr_key) {
                stats.used_attr_postings = true;
            } else {
                stats.unsupported_attr_keys.push(attr_key.clone());
            }
        }
        let candidate_keys = self.attr_matching_span_keys(snap, attrs);
        stats.candidate_span_keys = candidate_keys.as_ref().map(HashSet::len);
        if matches!(candidate_keys.as_ref(), Some(keys) if keys.is_empty()) {
            return (Vec::new(), stats);
        }
        let (mut spans, scanned) = match candidate_keys {
            Some(keys) => self.fold_query(snap, q, Some(&keys), Projection::ALL),
            None => self.read_spans_query(snap, q),
        };
        stats.scanned_segments = scanned;
        spans.retain(|s| folded_span_attrs_match(s, attrs));
        (spans, stats)
    }

    /// 折叠核心。`keys=Some(集合)` 时**只折叠命中这些 (trace,span) 的行**（检索用：先由索引拿到命中 key,
    /// 只折叠它们,不折叠全库）；`None` = 折叠全部（普通读）。`proj` 声明要读哪些可折叠值列——列式段据此
    /// 跳过不读的列（尤其大文本列），行式/内存源忽略它（无列 I/O 可省）。段扫描仍是全段（行级行指针待真实
    /// 索引），但折叠/克隆只发生在候选行上。
    fn fold_query(
        &self,
        snap: &Snapshot,
        q: &TraceQuery,
        keys: Option<&std::collections::HashSet<(u64, u64)>>,
        proj: Projection,
    ) -> (Vec<FoldedSpan>, usize) {
        // 租户隔离时，强制把 tenant_id 列纳入投影（否则列式段窄投影读不到 tenant，过滤会误删全部）。
        let proj = if q.tenant_id.is_some() {
            Projection::of(proj.bits() | Projection::TENANT_ID)
        } else {
            proj
        };
        let mut inputs: Vec<FoldInput> = Vec::new();
        let mut scanned = 0usize;
        let in_keys = |t: u64, s: u64| keys.map_or(true, |ks| ks.contains(&(t, s)));

        // 段源：先用段 zone-map(min_ts/max_ts) 做时间窗剪枝 —— 不重叠的段整段跳过、不扫。
        let mut upgrades: std::collections::BTreeMap<(u64, u64), SpanFields> =
            std::collections::BTreeMap::new();
        for entry in snap.manifest.segments.values() {
            if entry.max_ts < q.time_from || entry.min_ts > q.time_to {
                continue; // 时间窗外，整段剪掉
            }
            scanned += 1;
            match keys {
                // ★ 检索快路：已知候选 key → 段折叠缓存 + 段内 key→行号 索引，**只取候选行**、不扫全段。
                //   首次解码该段后缓存，之后所有查询命中缓存（这是把检索 QPS 从"每查全段扫"解放出来的关键）。
                Some(ks) => {
                    // 段级 bloom：这个段肯定没有任何候选 key → 整段跳过折叠定位（upgrade 仍在下面照常处理）。
                    let bloom_skip = self
                        .seg_key_bloom
                        .lock()
                        .unwrap()
                        .get(&entry.segment_id.get())
                        .map_or(false, |b| !ks.iter().any(|&k| b.maybe_contains(k)));
                    let sf = if bloom_skip {
                        None
                    } else {
                        Some(self.seg_fold(entry.segment_id))
                    };
                    for &(t, s) in ks {
                        let Some(sf) = &sf else { break };
                        if q.trace_id.map_or(false, |tid| t != tid) {
                            continue;
                        }
                        let Some(rowlist) = sf.by_key.get(&(t, s)) else {
                            continue;
                        };
                        for &row in rowlist {
                            if entry.deletion_vec.is_deleted(row) {
                                continue; // 删除位图按行号照查
                            }
                            // 时间窗已由段 zone-map 整段剪枝（FoldInput 不带行级 ts，与投影路一致）。
                            inputs.push(sf.rows[row as usize].clone()); // 只克隆候选行（极少）
                        }
                    }
                }
                // 普通读/聚合：三条扫描路（投影 `proj` 贯穿——列式段据此只解码命中列）：
                //   ① 段无删除 + 有真实时间窗 → 时间下推 + 投影（丢行号，段无删除用不到）。
                //   ② 否则纯投影下推：只裁列、不丢行 → 行号完整，删除位图照行号生效。
                //   ③ 都不支持 → 回退 `scan_fold_inputs` 读全列。
                None => {
                    let time_pushed = if entry.deletion_seq == 0
                        && (q.time_from != i64::MIN || q.time_to != i64::MAX)
                    {
                        self.segments.scan_fold_inputs_in_time(
                            entry.segment_id,
                            q.time_from,
                            q.time_to,
                            proj,
                        )
                    } else {
                        None
                    };
                    match time_pushed {
                        Some(folds) => {
                            for fi in folds {
                                if q.trace_id.map_or(false, |tid| fi.trace_id != tid) {
                                    continue;
                                }
                                inputs.push(fi);
                            }
                        }
                        None => {
                            let rows = self
                                .segments
                                .scan_fold_inputs_projected(entry.segment_id, proj)
                                .unwrap_or_else(|| {
                                    self.segments.scan_fold_inputs(entry.segment_id)
                                });
                            for (row, fi) in rows {
                                if entry.deletion_vec.is_deleted(row) {
                                    continue;
                                }
                                if let Some(tid) = q.trace_id {
                                    if fi.trace_id != tid {
                                        continue;
                                    }
                                }
                                inputs.push(fi);
                            }
                        }
                    }
                }
            }
            if let Some(up) = &entry.upgrade_ref {
                for (&(t, s), patch) in up.iter() {
                    if q.trace_id.map_or(false, |tid| t != tid) {
                        continue;
                    }
                    if !in_keys(t, s) {
                        continue;
                    }
                    // 同一 span 跨段的多份 upgrade 也按 last-non-null + logs 并集合一起。
                    upgrades.entry((t, s)).or_default().merge_from(patch);
                }
            }
        }

        // MemTable 源：半开区间 (retained_watermark, live_lsn]，再按时间窗 + trace_id 行级过滤。
        {
            let mt = self.memtable.lock().unwrap();
            for r in mt.read_range(snap.retained_watermark, snap.live_lsn) {
                if r.ts < q.time_from || r.ts > q.time_to {
                    continue;
                }
                if let Some(tid) = q.trace_id {
                    if r.trace_id != tid {
                        continue;
                    }
                }
                if !in_keys(r.trace_id, r.span_id) {
                    continue;
                }
                inputs.push(r.to_fold_input());
            }
        }

        // 四源 k 路归并折叠：event_id 去重、last-non-null-wins、logs union。
        let mut spans = fold_events(inputs);

        // upgrade 校正：晚到属性补写盖到对应 span 上（只覆盖非身份属性，非空才覆盖）。
        for sp in &mut spans {
            if let Some(patch) = upgrades.get(&(sp.trace_id, sp.span_id)) {
                sp.apply_patch(patch);
            }
        }
        // 租户隔离：只留本租户的 span（列表/读路径与检索路径一致地强制过滤）。
        if let Some(t) = q.tenant_id {
            spans.retain(|sp| sp.tenant_id == Some(t));
        }
        (spans, scanned)
    }
}
