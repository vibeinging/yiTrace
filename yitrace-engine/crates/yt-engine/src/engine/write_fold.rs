impl WriteCoordinator {
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
            match keys {
                // ★ 检索快路：已知候选 key → 段级 bloom 跳段，只解码命中 key 的行。
                Some(ks) => {
                    // 这个段肯定没有任何候选 key → 整段跳过折叠定位（upgrade 仍在下面照常处理）。
                    let bloom_skip = self
                        .seg_key_bloom
                        .lock()
                        .unwrap()
                        .get(&entry.segment_id.get())
                        .map_or(false, |b| !ks.iter().any(|&k| b.maybe_contains(k)));
                    if !bloom_skip {
                        scanned += 1;
                        if let Some(rows) = self
                            .segments
                            .scan_fold_inputs_for_keys(entry.segment_id, ks)
                        {
                            for (row, fi) in rows {
                                if entry.deletion_vec.is_deleted(row) {
                                    continue; // 删除位图按行号照查
                                }
                                inputs.push(fi);
                            }
                        } else {
                            // 内存/测试段的兼容回退：仍使用段折叠缓存，但默认文件段不会走这里。
                            let sf = self.seg_fold(entry.segment_id);
                            for &(t, s) in ks {
                                if q.trace_id.map_or(false, |tid| t != tid) {
                                    continue;
                                }
                                let Some(rowlist) = sf.by_key.get(&(t, s)) else {
                                    continue;
                                };
                                for &row in rowlist {
                                    if entry.deletion_vec.is_deleted(row) {
                                        continue;
                                    }
                                    inputs.push(sf.rows[row as usize].clone());
                                }
                            }
                        }
                    }
                }
                // 普通读/聚合：三条扫描路（投影 `proj` 贯穿——列式段据此只解码命中列）：
                //   ① 段无删除 + 有真实时间窗 → 时间下推 + 投影（丢行号，段无删除用不到）。
                //   ② 否则纯投影下推：只裁列、不丢行 → 行号完整，删除位图照行号生效。
                //   ③ 都不支持 → 回退 `scan_fold_inputs` 读全列。
                None => {
                    scanned += 1;
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
