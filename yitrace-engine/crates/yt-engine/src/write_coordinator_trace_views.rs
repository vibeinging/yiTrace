impl WriteCoordinator {

    /// 列出 trace 摘要（web 控制台列表视图）。按 trace_id 把折叠出的 span 聚合：span 数、总/最大耗时、报错数。
    /// 输出按 trace_id 升序，确定可复算。
    pub fn list_traces(&self, snap: &Snapshot, q: &TraceQuery) -> Vec<TraceSummary> {
        // 只读 status/耗时/token —— 不碰大文本列。
        let proj = Projection::of(
            Projection::STATUS
                | Projection::DURATION_NS
                | Projection::INPUT_TOKENS
                | Projection::OUTPUT_TOKENS
                | Projection::USAGE_COST
                | Projection::EXTERNAL_IDS,
        );
        let (spans, _) = self.fold_query(snap, q, None, proj);
        summarize_trace_spans(spans)
    }

    /// 返回一条 trace 的物化 trajectory 摘要。miss 时按当前 snapshot 折叠一次并缓存。
    pub fn materialized_trace_trajectory(
        &self,
        snap: &Snapshot,
        trace_id: u64,
        tenant: Option<u64>,
    ) -> Option<TraceTrajectorySummary> {
        let key = (tenant, trace_id);
        if let Some(cached) = self.trace_trajectory_idx.lock().unwrap().get(&key).cloned() {
            return Some(cached);
        }
        let mut q = TraceQuery::trace(trace_id, i64::MIN, i64::MAX);
        q.tenant_id = tenant;
        let proj = Projection::of(
            Projection::STATUS
                | Projection::DURATION_NS
                | Projection::INPUT_TOKENS
                | Projection::OUTPUT_TOKENS
                | Projection::USAGE_COST
                | Projection::AGENT_NAME
                | Projection::TOOL_NAME
                | Projection::MODEL
                | Projection::TENANT_ID
                | Projection::EXTERNAL_IDS
                | Projection::ATTRS
                | Projection::AGENTIC_FIELDS,
        );
        let (mut spans, _) = self.fold_query(snap, &q, None, proj);
        if spans.is_empty() {
            return None;
        }
        spans.sort_by_key(|s| s.span_id);
        let summary = trace_trajectory_summary_from_spans(trace_id, tenant, &spans)?;
        self.trace_trajectory_idx
            .lock()
            .unwrap()
            .insert(key, summary.clone());
        Some(summary)
    }

    /// 返回一组 trace 在当前 snapshot 内的事件时间边界。
    ///
    /// 这是 retention/storage stats 的底座：policy 层用 folded span 先确定租户与过滤语义，
    /// 再用这里确认整条 trace 是否已经早于 cutoff。
    pub fn trace_time_bounds(
        &self,
        snap: &Snapshot,
        trace_ids: &HashSet<u64>,
    ) -> BTreeMap<u64, (i64, i64)> {
        let mut out: BTreeMap<u64, (i64, i64)> = BTreeMap::new();
        if trace_ids.is_empty() {
            return out;
        }
        let mut update = |trace_id: u64, ts: i64| {
            let e = out.entry(trace_id).or_insert((ts, ts));
            e.0 = e.0.min(ts);
            e.1 = e.1.max(ts);
        };
        for entry in snap.manifest.segments.values() {
            let recs = self.segments.scan_records(entry.segment_id);
            for (row, rec) in recs.iter().enumerate() {
                if entry.deletion_vec.is_deleted(row as u32) || !trace_ids.contains(&rec.trace_id) {
                    continue;
                }
                update(rec.trace_id, rec.ts);
            }
        }
        let mt = self.memtable.lock().unwrap();
        for row in mt.read_range(snap.retained_watermark, snap.live_lsn) {
            if trace_ids.contains(&row.trace_id) {
                update(row.trace_id, row.ts);
            }
        }
        out
    }

    /// 对一批 trace 做 segment-row 级软删除。
    ///
    /// 注意：仍在 MemTable/WAL tail 中的 trace 会整条跳过，避免 retention apply 后出现半条 trace。
    /// 调用方应先用租户过滤后的 folded spans 决定 trace 集合；这里不做业务策略判断。
    pub fn delete_segment_rows_for_traces(
        &self,
        snap: &Snapshot,
        trace_ids: &HashSet<u64>,
    ) -> RetentionDeleteResult {
        let mut result = RetentionDeleteResult {
            requested_trace_count: trace_ids.len(),
            ..RetentionDeleteResult::default()
        };
        if trace_ids.is_empty() {
            return result;
        }

        let mut live_trace_ids = HashSet::new();
        {
            let mt = self.memtable.lock().unwrap();
            for row in mt.read_range(snap.retained_watermark, snap.live_lsn) {
                if trace_ids.contains(&row.trace_id) {
                    live_trace_ids.insert(row.trace_id);
                }
            }
        }

        let deletable: HashSet<u64> = trace_ids.difference(&live_trace_ids).copied().collect();
        result.skipped_live_trace_ids = live_trace_ids.into_iter().collect();
        result.skipped_live_trace_ids.sort_unstable();
        result.skipped_live_trace_count = result.skipped_live_trace_ids.len();

        let mut row_targets = Vec::new();
        let mut deleted_trace_ids = HashSet::new();
        for entry in snap.manifest.segments.values() {
            let recs = self.segments.scan_records(entry.segment_id);
            for (row, rec) in recs.iter().enumerate() {
                if entry.deletion_vec.is_deleted(row as u32) || !deletable.contains(&rec.trace_id) {
                    continue;
                }
                row_targets.push((entry.segment_id, row as u32));
                deleted_trace_ids.insert(rec.trace_id);
            }
        }

        for (seg, row) in row_targets {
            self.commit_delete(seg, row);
            result.deleted_segment_row_count += 1;
        }
        result.deleted_trace_ids = deleted_trace_ids.into_iter().collect();
        result.deleted_trace_ids.sort_unstable();
        result.deleted_trace_count = result.deleted_trace_ids.len();
        if !result.deleted_trace_ids.is_empty() {
            let deleted: HashSet<u64> = result.deleted_trace_ids.iter().copied().collect();
            self.trace_trajectory_idx
                .lock()
                .unwrap()
                .retain(|(_, trace_id), _| !deleted.contains(trace_id));
        }
        result
    }

    /// 对带 deletion vector 的段做 retention 后压实。
    ///
    /// 这不是默认写路径：调用方通常在 retention apply 后、维护窗口或后台任务中显式触发。
    /// 阈值用“被删行数 + 被删比例”同时限制，避免为少量删除反复重写大段。
    pub fn compact_deleted_segments(
        &self,
        max_segments: usize,
        min_deleted_rows: u32,
        min_deleted_percent: u32,
        reclaim_after_compact: bool,
    ) -> RetentionCompactResult {
        let manifest = self.current.manifest();
        let mut result = RetentionCompactResult {
            before_live_segment_count: manifest.segments.len(),
            before_dead_segment_count: self.dead_count(),
            ..RetentionCompactResult::default()
        };
        if max_segments == 0 {
            result.after_live_segment_count = result.before_live_segment_count;
            result.after_dead_segment_count = result.before_dead_segment_count;
            return result;
        }

        let min_deleted_percent = min_deleted_percent.clamp(1, 100);
        let mut candidates = Vec::new();
        for entry in manifest.segments.values() {
            let rows = self.segments.scan_records(entry.segment_id);
            let row_count = rows.len();
            if row_count == 0 {
                continue;
            }
            let deleted_count = (entry.deletion_vec.count() as usize).min(row_count);
            if deleted_count == 0 || deleted_count < min_deleted_rows as usize {
                continue;
            }
            let deleted_percent = ((deleted_count as u64 * 100) / row_count as u64) as u32;
            if deleted_percent < min_deleted_percent {
                continue;
            }
            let live_count = row_count - deleted_count;
            candidates.push((
                deleted_percent,
                deleted_count,
                live_count,
                entry.segment_id.get(),
            ));
        }
        drop(manifest);

        candidates.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| b.1.cmp(&a.1))
                .then_with(|| a.3.cmp(&b.3))
        });
        for (_, deleted_count, live_count, segment_id) in candidates.into_iter().take(max_segments)
        {
            let seg = SegmentId::new(segment_id);
            self.commit_compaction(&[seg]);
            result.selected_segment_ids.push(segment_id);
            result.compacted_segment_count += 1;
            result.dropped_deleted_row_count += deleted_count;
            result.rewritten_live_row_count += live_count;
        }
        result.selected_segment_count = result.selected_segment_ids.len();
        if reclaim_after_compact {
            result.reclaimed_segment_count = self.reclaim();
        }
        let after = self.current.manifest();
        result.after_live_segment_count = after.segments.len();
        result.after_dead_segment_count = self.dead_count();
        result
    }

    /// 列出 trace 摘要并按 attrs 过滤。语义与 session attrs 过滤一致：
    /// trace 内至少一个 span 命中所有 attrs 条件，返回该 trace 的完整聚合摘要。
    pub fn list_traces_for_tenant_and_attrs(
        &self,
        snap: &Snapshot,
        tenant: Option<u64>,
        attrs: &BTreeMap<String, String>,
    ) -> Vec<TraceSummary> {
        let q = match tenant {
            Some(t) => TraceQuery::all().for_tenant(t),
            None => TraceQuery::all(),
        };
        if attrs.is_empty() {
            return self.list_traces(snap, &q);
        }
        let candidate_keys = self.attr_matching_span_keys(snap, attrs);
        if matches!(candidate_keys.as_ref(), Some(keys) if keys.is_empty()) {
            return Vec::new();
        }
        let attr_proj = Projection::of(Projection::ATTRS);
        let (matching_spans, all_spans_for_slow_path) = match candidate_keys {
            Some(keys) => (self.fold_query(snap, &q, Some(&keys), attr_proj).0, None),
            None => {
                let proj = Projection::of(
                    Projection::STATUS
                        | Projection::DURATION_NS
                        | Projection::INPUT_TOKENS
                        | Projection::OUTPUT_TOKENS
                        | Projection::USAGE_COST
                        | Projection::EXTERNAL_IDS
                        | Projection::ATTRS,
                );
                let spans = self.fold_query(snap, &q, None, proj).0;
                (spans.clone(), Some(spans))
            }
        };
        let matching_traces: HashSet<u64> = matching_spans
            .into_iter()
            .filter(|s| folded_span_attrs_match(s, attrs))
            .map(|s| s.trace_id)
            .collect();
        if matching_traces.is_empty() {
            return Vec::new();
        }
        if let Some(spans) = all_spans_for_slow_path {
            return summarize_trace_spans(
                spans
                    .into_iter()
                    .filter(|s| matching_traces.contains(&s.trace_id))
                    .collect(),
            );
        }
        let trace_keys = self.span_keys_for_trace_ids(&matching_traces);
        if trace_keys.is_empty() {
            return Vec::new();
        }
        let summary_proj = Projection::of(
            Projection::STATUS
                | Projection::DURATION_NS
                | Projection::INPUT_TOKENS
                | Projection::OUTPUT_TOKENS
                | Projection::USAGE_COST
                | Projection::EXTERNAL_IDS,
        );
        let (spans, _) = self.fold_query(snap, &q, Some(&trace_keys), summary_proj);
        summarize_trace_spans(spans)
    }

    /// 窄投影读取 trace attrs，用于 HTTP trace list 输出稳定的一等 fields，不读大文本。
    pub fn trace_attr_fields_for_tenant(
        &self,
        snap: &Snapshot,
        tenant: Option<u64>,
    ) -> BTreeMap<u64, BTreeMap<String, String>> {
        let trace_ids: HashSet<u64> = self
            .trace_span_keys
            .lock()
            .unwrap()
            .keys()
            .copied()
            .collect();
        self.trace_attr_fields_for_tenant_and_traces(snap, tenant, &trace_ids)
    }

    /// 窄投影读取指定 trace 的 attrs，用于列表页只给当前可见 trace 补稳定 fields。
    pub fn trace_attr_fields_for_tenant_and_traces(
        &self,
        snap: &Snapshot,
        tenant: Option<u64>,
        trace_ids: &HashSet<u64>,
    ) -> BTreeMap<u64, BTreeMap<String, String>> {
        if trace_ids.is_empty() {
            return BTreeMap::new();
        }
        let keys = self.span_keys_for_trace_ids(trace_ids);
        if keys.is_empty() {
            return BTreeMap::new();
        }
        let q = match tenant {
            Some(t) => TraceQuery::all().for_tenant(t),
            None => TraceQuery::all(),
        };
        let proj = Projection::of(Projection::AGENTIC_FIELDS | Projection::ATTRS);
        let (spans, _) = self.fold_query(snap, &q, Some(&keys), proj);
        let mut by_trace: BTreeMap<u64, BTreeMap<String, String>> = BTreeMap::new();
        for s in spans {
            let fields = by_trace.entry(s.trace_id).or_default();
            for key in first_class_agentic_attr_keys() {
                if let Some(value) = first_class_span_attr_value(&s, key) {
                    fields
                        .entry(key.to_string())
                        .or_insert_with(|| first_class_agentic_attr_json(key, value));
                }
            }
            for (k, v) in s.attrs {
                if is_agentic_field_key(&k) {
                    fields.entry(k).or_insert(v);
                }
            }
        }
        by_trace
    }

    /// 列出会话摘要（多轮会话视图）：按 session_id 聚合,数 trace 数/span 数/token 汇总。升序。
    pub fn list_sessions(&self, snap: &Snapshot, q: &TraceQuery) -> Vec<SessionSummary> {
        // 按 session 聚合 token —— 只读 session_id + token,跳过文本。
        let proj = Projection::of(
            Projection::SESSION_ID
                | Projection::INPUT_TOKENS
                | Projection::OUTPUT_TOKENS
                | Projection::USAGE_COST,
        );
        let (spans, _) = self.fold_query(snap, q, None, proj);
        // session_id -> (distinct traces, span_count, in_tok, out_tok)
        let mut acc: BTreeMap<u64, (std::collections::HashSet<u64>, usize, u64, u64)> =
            BTreeMap::new();
        for s in spans {
            if let Some(sid) = s.session_id {
                let e = acc.entry(sid).or_default();
                e.0.insert(s.trace_id);
                e.1 += 1;
                e.2 += s.input_tokens.unwrap_or(0);
                e.3 += s.output_tokens.unwrap_or(0);
            }
        }
        acc.into_iter()
            .map(|(session_id, (traces, span_count, i, o))| SessionSummary {
                session_id,
                trace_count: traces.len(),
                span_count,
                total_input_tokens: i,
                total_output_tokens: o,
            })
            .collect()
    }

    /// 装一个会话的**多轮对话流**：把同一 `session_id` 的多条 trace（每条=一轮）按 trace_id 升序
    /// 拼成「用户问 → agent 答」的时间线。这是多轮会话视图的渲染源，也是会话级评测的输入。
    ///
    /// 取原文要读全列。当前没有 session→trace 倒排，按 session_id **扫全量过滤**（O(全库)）——
    /// 会话视图是低频操作可接受；真要高频再加 session 边车索引。
    pub fn load_session_timeline(&self, snap: &Snapshot, session_id: u64) -> SessionTimeline {
        self.load_session_timeline_query(snap, session_id, &TraceQuery::all())
    }

    /// 带查询约束的会话时间线。控制台网关用它把 tenant 过滤压到折叠读取层。
    pub fn load_session_timeline_query(
        &self,
        snap: &Snapshot,
        session_id: u64,
        q: &TraceQuery,
    ) -> SessionTimeline {
        let (spans, _) = self.read_spans_query(snap, q);
        // 按 trace 分组本会话的 span（BTreeMap → trace_id 升序 = 轮次序）。
        let mut by_trace: BTreeMap<u64, Vec<FoldedSpan>> = BTreeMap::new();
        for s in spans {
            if s.session_id == Some(session_id) {
                by_trace.entry(s.trace_id).or_default().push(s);
            }
        }
        let mut turns = Vec::with_capacity(by_trace.len());
        let mut total_in = 0u64;
        let mut total_out = 0u64;
        for (turn_index, (trace_id, mut sps)) in by_trace.into_iter().enumerate() {
            sps.sort_by_key(|s| s.span_id);
            // 输入取最早（span_id 最小）带 input_text 的；答复取最末带 output_text 的。
            let user_input = sps
                .iter()
                .find(|s| s.input_text.is_some())
                .and_then(|s| s.input_text.clone());
            let answer = sps.iter().rev().find(|s| s.output_text.is_some());
            let agent_output = answer.and_then(|s| s.output_text.clone());
            let eval_score = answer.and_then(|s| s.eval_score);
            let mut agents: Vec<String> = sps.iter().filter_map(|s| s.agent_name.clone()).collect();
            agents.sort();
            agents.dedup();
            let input_tokens: u64 = sps.iter().map(|s| s.input_tokens.unwrap_or(0)).sum();
            let output_tokens: u64 = sps.iter().map(|s| s.output_tokens.unwrap_or(0)).sum();
            let cached_input_tokens: u64 =
                sps.iter().map(|s| s.cached_input_tokens.unwrap_or(0)).sum();
            let reasoning_tokens: u64 = sps.iter().map(|s| s.reasoning_tokens.unwrap_or(0)).sum();
            let total_tokens: u64 = sps
                .iter()
                .map(|s| {
                    usage_total_tokens(
                        s.input_tokens.unwrap_or(0),
                        s.output_tokens.unwrap_or(0),
                        s.cached_input_tokens.unwrap_or(0),
                        s.reasoning_tokens.unwrap_or(0),
                        s.total_tokens,
                    )
                })
                .sum();
            let cost_usd_nanos: u64 = sps
                .iter()
                .map(|s| {
                    usage_cost_usd_nanos_for_model(
                        s.input_tokens.unwrap_or(0),
                        s.output_tokens.unwrap_or(0),
                        s.cached_input_tokens.unwrap_or(0),
                        s.reasoning_tokens.unwrap_or(0),
                        s.cost_usd_nanos,
                        s.provider.as_deref(),
                        s.model.as_deref(),
                    )
                })
                .sum();
            let error_count = sps.iter().filter(|s| s.status.unwrap_or(0) != 0).count();
            total_in += input_tokens;
            total_out += output_tokens;
            turns.push(SessionTurn {
                trace_id,
                turn_index,
                user_input,
                agent_output,
                agents,
                input_tokens,
                output_tokens,
                cached_input_tokens,
                reasoning_tokens,
                total_tokens,
                cost_usd_nanos,
                span_count: sps.len(),
                error_count,
                eval_score,
            });
        }
        SessionTimeline {
            session_id,
            turns,
            total_input_tokens: total_in,
            total_output_tokens: total_out,
        }
    }

    /// 控制台用：会话行列表（标题/轮数/状态/token/首 trace），按 session_id 降序。
    /// 走**增量边车索引**：摄入时已逐事件 O(1) 维护，这里直接产出（带排序缓存）→ 写多读少也不全扫。
    /// 仅当 delete/upgrade 标脏时，才在此做一次全量重建（这两类不走 index_record）。
    pub fn console_sessions(&self, snap: &Snapshot) -> Vec<ConsoleSession> {
        // 先看是否标脏（不持锁去扫，避免 session_idx→memtable 的锁序反转死锁）。
        let dirty = self.session_idx.lock().unwrap().dirty;
        if dirty {
            let (spans, _) = self.read_spans_query(snap, &TraceQuery::all()); // 不持 session_idx 锁
            let mut idx = self.session_idx.lock().unwrap();
            if idx.dirty {
                idx.rebuild(&spans);
            }
        }
        self.session_idx.lock().unwrap().rows()
    }

    /// 控制台用：按请求租户隔离的会话行列表。
    /// 无租户时走增量边车；有租户时基于已过滤 span 临时聚合，避免全局 session_idx 泄露别的租户。
    pub fn console_sessions_for_tenant(
        &self,
        snap: &Snapshot,
        tenant: Option<u64>,
    ) -> Vec<ConsoleSession> {
        let Some(t) = tenant else {
            return self.console_sessions(snap);
        };
        let (spans, _) = self.read_spans_query(snap, &TraceQuery::all().for_tenant(t));
        let mut idx = SessionIndex::default();
        idx.rebuild(&spans);
        idx.rows()
    }

    /// 控制台用：按租户和 attrs 过滤会话。
    ///
    /// 语义是“会话内至少一个 span 命中所有 attrs 条件”，返回该会话的完整聚合行。attrs 命中先走
    /// postings 候选集，再用折叠结果校验；会话行仍复用现有 session 聚合口径，避免摘要被命中 span 算少。
    pub fn console_sessions_for_tenant_and_attrs(
        &self,
        snap: &Snapshot,
        tenant: Option<u64>,
        attrs: &BTreeMap<String, String>,
    ) -> Vec<ConsoleSession> {
        if attrs.is_empty() {
            return self.console_sessions_for_tenant(snap, tenant);
        }
        let q = match tenant {
            Some(t) => TraceQuery::all().for_tenant(t),
            None => TraceQuery::all(),
        };
        let candidate_keys = self.attr_matching_span_keys(snap, attrs);
        if matches!(candidate_keys.as_ref(), Some(keys) if keys.is_empty()) {
            return Vec::new();
        }
        let proj = Projection::of(Projection::SESSION_ID | Projection::ATTRS);
        let (spans, _) = match candidate_keys {
            Some(keys) => self.fold_query(snap, &q, Some(&keys), proj),
            None => self.fold_query(snap, &q, None, proj),
        };
        let mut matching_sessions = HashSet::new();
        for s in spans {
            let Some(session_id) = s.session_id else {
                continue;
            };
            if folded_span_attrs_match(&s, attrs) {
                matching_sessions.insert(session_id);
            }
        }
        if matching_sessions.is_empty() {
            return Vec::new();
        }
        let mut rows = self.console_sessions_for_tenant(snap, tenant);
        rows.retain(|s| matching_sessions.contains(&s.session_id));
        rows
    }

    /// 控制台用：一条 trace 的折叠 span（瀑布）。引擎不存 span 的 kind/name/起始时刻，这里**派生**：
    /// kind = agent>tool>model>other；name = 同源；起始时刻按 span_id 升序累加 duration 顺排（逻辑瀑布）。
    pub fn console_trace_spans(&self, snap: &Snapshot, trace_id: u64) -> Vec<ConsoleSpan> {
        self.console_trace_spans_for_tenant(snap, trace_id, None)
    }

    /// 控制台用：按请求租户隔离的一条 trace 折叠 span。
    pub fn console_trace_spans_for_tenant(
        &self,
        snap: &Snapshot,
        trace_id: u64,
        tenant: Option<u64>,
    ) -> Vec<ConsoleSpan> {
        let mut q = TraceQuery::trace(trace_id, i64::MIN, i64::MAX);
        q.tenant_id = tenant;
        let (mut spans, _) = self.read_spans_query(snap, &q);
        spans.sort_by_key(|s| s.span_id);
        let mut start = 0u64;
        spans
            .into_iter()
            .map(|s| {
                let (kind, name) = if let Some(a) = &s.agent_name {
                    ("agent", a.clone())
                } else if let Some(t) = &s.tool_name {
                    ("tool", t.clone())
                } else if let Some(m) = &s.model {
                    ("llm", m.clone())
                } else {
                    ("other", format!("span {}", s.span_id))
                };
                let dur = s.duration_ns.unwrap_or(0);
                let cs = ConsoleSpan {
                    span_id: s.span_id,
                    parent_span_id: s.parent_span_id,
                    external_trace_id: s.external_trace_id.clone(),
                    external_span_id: s.external_span_id.clone(),
                    external_parent_span_id: s.external_parent_span_id.clone(),
                    external_session_id: s.external_session_id.clone(),
                    project_id: s.project_id.clone(),
                    skill: s.skill.clone(),
                    mode: s.mode.clone(),
                    call_site: s.call_site.clone(),
                    task_fingerprint: s.task_fingerprint.clone(),
                    loop_id: s.loop_id.clone(),
                    harness_version: s.harness_version.clone(),
                    schema_fingerprint: s.schema_fingerprint.clone(),
                    intent_signature: s.intent_signature.clone(),
                    validation_status: s.validation_status.clone(),
                    review_status: s.review_status.clone(),
                    eval_status: s.eval_status.clone(),
                    path_memory_id: s.path_memory_id.clone(),
                    stop_reason: s.stop_reason.clone(),
                    phase: s.phase.clone(),
                    validator: s.validator.clone(),
                    kind,
                    name,
                    start_ns: start,
                    duration_ns: dur,
                    has_error: s.status.unwrap_or(0) != 0,
                    input_tokens: s.input_tokens.unwrap_or(0),
                    output_tokens: s.output_tokens.unwrap_or(0),
                    cached_input_tokens: s.cached_input_tokens.unwrap_or(0),
                    reasoning_tokens: s.reasoning_tokens.unwrap_or(0),
                    total_tokens: usage_total_tokens(
                        s.input_tokens.unwrap_or(0),
                        s.output_tokens.unwrap_or(0),
                        s.cached_input_tokens.unwrap_or(0),
                        s.reasoning_tokens.unwrap_or(0),
                        s.total_tokens,
                    ),
                    cost_usd_nanos: usage_cost_usd_nanos_for_model(
                        s.input_tokens.unwrap_or(0),
                        s.output_tokens.unwrap_or(0),
                        s.cached_input_tokens.unwrap_or(0),
                        s.reasoning_tokens.unwrap_or(0),
                        s.cost_usd_nanos,
                        s.provider.as_deref(),
                        s.model.as_deref(),
                    ),
                    model: s.model.clone(),
                    provider: s.provider.clone(),
                    cost_currency: s.cost_currency.clone(),
                    input_text: s.input_text.clone(),
                    output_text: s.output_text.clone(),
                    attrs: s.attrs.clone(),
                };
                start += dur;
                cs
            })
            .collect()
    }

    /// 读取一条 trace 中可见 span 的原始日志事件。调用方传入已经过租户/删除过滤的 key 集，
    /// 这里只做原始事件扫描、event_id 去重与时间排序，保证不会把不可见 span 的日志泄漏出去。
    pub fn log_events_for_trace_keys(
        &self,
        snap: &Snapshot,
        trace_id: u64,
        keys: &std::collections::HashSet<(u64, u64)>,
    ) -> BTreeMap<u64, Vec<SpanLogEvent>> {
        let mut by_event: BTreeMap<u64, (u64, SpanLogEvent)> = BTreeMap::new();

        for entry in snap.manifest.segments.values() {
            let recs = self.segments.scan_records(entry.segment_id);
            for (row, rec) in recs.iter().enumerate() {
                if entry.deletion_vec.is_deleted(row as u32) {
                    continue;
                }
                self.collect_log_event(trace_id, keys, rec, &mut by_event);
            }
        }

        {
            let mt = self.memtable.lock().unwrap();
            for row in mt.read_range(snap.retained_watermark, snap.live_lsn) {
                let rec = WalRecord {
                    trace_id: row.trace_id,
                    span_id: row.span_id,
                    ts: row.ts,
                    identity: row.identity.clone(),
                    fields: row.fields.clone(),
                };
                self.collect_log_event(trace_id, keys, &rec, &mut by_event);
            }
        }

        let mut by_span: BTreeMap<u64, Vec<SpanLogEvent>> = BTreeMap::new();
        for (_event_id, (span_id, ev)) in by_event {
            by_span.entry(span_id).or_default().push(ev);
        }
        for events in by_span.values_mut() {
            events.sort_by_key(|ev| (ev.ts, ev.seq, ev.event_id));
        }
        by_span
    }

    fn collect_log_event(
        &self,
        trace_id: u64,
        keys: &std::collections::HashSet<(u64, u64)>,
        rec: &WalRecord,
        out: &mut BTreeMap<u64, (u64, SpanLogEvent)>,
    ) {
        if rec.trace_id != trace_id
            || !keys.contains(&(rec.trace_id, rec.span_id))
            || rec.fields.logs.is_empty()
        {
            return;
        }
        let event_id = rec.identity.event_id().0;
        out.entry(event_id).or_insert_with(|| {
            (
                rec.span_id,
                SpanLogEvent {
                    ts: rec.ts,
                    seq: rec.identity.seq,
                    event_type: rec.identity.event_type.tag(),
                    event_id,
                    messages: rec.fields.logs.clone(),
                    attrs: rec.fields.attrs.clone(),
                },
            )
        });
    }
}
