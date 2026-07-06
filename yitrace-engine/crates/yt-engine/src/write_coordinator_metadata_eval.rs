impl WriteCoordinator {

    /// 按 agent 的成本归因（per-agent 成本下钻）：按 agent_name 聚合 token。按 agent 名升序。
    pub fn cost_by_agent(&self, snap: &Snapshot, q: &TraceQuery) -> Vec<AgentCost> {
        // 按 agent 归因 token —— 只读 agent_name + token,跳过文本（成本下钻是典型的"只数不读原文"）。
        let proj = Projection::of(
            Projection::AGENT_NAME
                | Projection::INPUT_TOKENS
                | Projection::OUTPUT_TOKENS
                | Projection::USAGE_COST,
        );
        let (spans, _) = self.fold_query(snap, q, None, proj);
        let mut acc: BTreeMap<String, (usize, u64, u64, u64, u64, u64, u64)> = BTreeMap::new();
        for s in spans {
            if let Some(a) = &s.agent_name {
                let e = acc.entry(a.clone()).or_default();
                e.0 += 1;
                e.1 += s.input_tokens.unwrap_or(0);
                e.2 += s.output_tokens.unwrap_or(0);
                e.3 += s.cached_input_tokens.unwrap_or(0);
                e.4 += s.reasoning_tokens.unwrap_or(0);
                e.5 += usage_total_tokens(
                    s.input_tokens.unwrap_or(0),
                    s.output_tokens.unwrap_or(0),
                    s.cached_input_tokens.unwrap_or(0),
                    s.reasoning_tokens.unwrap_or(0),
                    s.total_tokens,
                );
                e.6 += usage_cost_usd_nanos_for_model(
                    s.input_tokens.unwrap_or(0),
                    s.output_tokens.unwrap_or(0),
                    s.cached_input_tokens.unwrap_or(0),
                    s.reasoning_tokens.unwrap_or(0),
                    s.cost_usd_nanos,
                    s.provider.as_deref(),
                    s.model.as_deref(),
                );
            }
        }
        acc.into_iter()
            .map(
                |(
                    agent_name,
                    (
                        span_count,
                        input_tokens,
                        output_tokens,
                        cached_input_tokens,
                        reasoning_tokens,
                        total_tokens,
                        cost_usd_nanos,
                    ),
                )| AgentCost {
                    agent_name,
                    span_count,
                    input_tokens,
                    output_tokens,
                    cached_input_tokens,
                    reasoning_tokens,
                    total_tokens,
                    cost_usd_nanos,
                },
            )
            .collect()
    }

    /// eval 闭环：用 `scorer` 给命中 `q` 的每条 span 打分，分数**走 upgrade（晚到补写）通道写回**。
    /// 返回打了分的 span。读回时分数被折叠进对应 span 的 `eval_score`/`eval_label`。
    ///
    /// 把产品从"看 trace"推到"评 trace"。这里的妙处：评测分本质就是一种"trace 事后才有的字段"，
    /// 与晚到属性补写同构 —— 直接复用 upgrade 王牌，不需要给评测另起一套存储。
    /// 先 flush 内存表（让被评 span 都进段、upgrade 有落点），再按 (trace,span)→段 映射把分写回所在段。
    /// scorer 现在是不依赖 LLM 的规则版；换成 LLM-judge / 本地小模型裁判时，这条闭环骨架不变。
    pub fn eval_and_writeback(&self, scorer: &dyn Scorer, q: &TraceQuery) -> Vec<ScoredSpan> {
        // 1) 先封段：被评 span 都落进段，output_text 也随段持久化，upgrade 才有段可落。
        self.flush_memtable();

        // 2) 读出待评 span（此刻 output_text 来自段）。
        let snap = self.pin_snapshot();
        let (spans, _) = self.read_spans_query(&snap, q);

        // 3) 建 (trace,span) → 所在段 映射：分数写回该段（多段命中取最小段号，稳定）。
        // 与读路径同口径做 zone-map 时间窗 + trace_id 剪枝：只扫 q 命中的段,不扫全库
        //（否则按单条 trace 评测也要扫遍所有段）。
        let mut span_seg: HashMap<(u64, u64), SegmentId> = HashMap::new();
        for entry in snap.manifest.segments.values() {
            if entry.max_ts < q.time_from || entry.min_ts > q.time_to {
                continue; // 时间窗外，整段跳过
            }
            for (_row, fi) in self.segments.scan_fold_inputs(entry.segment_id) {
                if q.trace_id.map_or(false, |tid| fi.trace_id != tid) {
                    continue; // trace_id 不匹配（行级）
                }
                span_seg
                    .entry((fi.trace_id, fi.span_id))
                    .or_insert(entry.segment_id);
            }
        }
        drop(snap);

        // 4) 逐条打分并写回（scorer 返回 None 的 span 跳过、不写）。
        let mut out = Vec::new();
        for sp in spans {
            let Some(outcome) = scorer.score(&sp) else {
                continue;
            };
            if let Some(&seg) = span_seg.get(&(sp.trace_id, sp.span_id)) {
                self.commit_upgrade(
                    seg,
                    sp.trace_id,
                    sp.span_id,
                    SpanFields {
                        eval_score: Some(outcome.score),
                        eval_label: Some(outcome.label.clone()),
                        ..Default::default()
                    },
                );
                out.push(ScoredSpan {
                    trace_id: sp.trace_id,
                    span_id: sp.span_id,
                    outcome,
                });
            }
        }
        out
    }

    /// 评测看板：把已打分的 span 聚合成 通过率/均分 —— 整体一行 +（有 agent 名的）每 agent 一行。
    /// `pass_threshold` 千分制，分数 ≥ 它算通过。这是 eval 的产品出口:回归视图("哪个 agent 退步了")。
    /// 输出第 0 行恒为整体(agent_name=None),其后按 agent 名升序。
    pub fn eval_summary(
        &self,
        snap: &Snapshot,
        q: &TraceQuery,
        pass_threshold: u32,
    ) -> Vec<EvalSummary> {
        // 看板只看分数 + agent 名 —— 不读被评的原文（原文在打分时已用过、写回成了分数）。
        let proj = Projection::of(
            Projection::EVAL_SCORE | Projection::EVAL_LABEL | Projection::AGENT_NAME,
        );
        let (spans, _) = self.fold_query(snap, q, None, proj);
        // 只取已打分的 span（无 eval_score 的不计），喂进共用聚合口径。
        let scored = spans
            .into_iter()
            .filter_map(|s| s.eval_score.map(|sc| (s.agent_name, sc)));
        aggregate_eval(scored, pass_threshold)
    }

    /// 建一个空数据集（已存在则不动）。返回是否新建。
    pub fn create_dataset(&self, name: &str) -> bool {
        let mut ds = self.datasets.lock().unwrap();
        if ds.contains_key(name) {
            return false;
        }
        ds.insert(
            name.to_string(),
            Dataset {
                name: name.to_string(),
                examples: Vec::new(),
            },
        );
        true
    }

    /// 把命中 `q` 且通过 `pred` 的 span 采集进数据集（不存在则自动建）。返回新增样本数。
    /// 典型用法:`pred = |s| s.eval_score == Some(0)` 把失败样本收集成回归集;
    /// 或配合 `search_similar` 先捞"相似失败 trace"再传它们的 span 进来(中文/语义召回的差异化用法)。
    /// 按 (trace_id, span_id) 去重:已在集里的不重复加。存的是 span 快照,底层 trace 后续被合并/回收也不影响。
    pub fn collect_into_dataset(
        &self,
        name: &str,
        snap: &Snapshot,
        q: &TraceQuery,
        pred: &dyn Fn(&FoldedSpan) -> bool,
    ) -> usize {
        let (spans, _) = self.read_spans_query(snap, q);
        let mut ds = self.datasets.lock().unwrap();
        let entry = ds.entry(name.to_string()).or_insert_with(|| Dataset {
            name: name.to_string(),
            examples: Vec::new(),
        });
        let mut existing: std::collections::HashSet<(u64, u64)> = entry
            .examples
            .iter()
            .map(|e| (e.span.trace_id, e.span.span_id))
            .collect();
        let mut added = 0;
        for s in spans {
            if !pred(&s) {
                continue;
            }
            if existing.insert((s.trace_id, s.span_id)) {
                entry.examples.push(DatasetExample {
                    span: s,
                    expected: None,
                });
                added += 1;
            }
        }
        added
    }

    /// 取一个数据集的副本（检视/导出用）。
    pub fn dataset(&self, name: &str) -> Option<Dataset> {
        self.datasets.lock().unwrap().get(name).cloned()
    }

    /// 列出所有数据集摘要,按名升序。
    pub fn list_datasets(&self) -> Vec<DatasetSummary> {
        self.datasets
            .lock()
            .unwrap()
            .values()
            .map(|d| DatasetSummary {
                name: d.name.clone(),
                example_count: d.examples.len(),
            })
            .collect()
    }

    /// 给 trace/span 追加一条业务 annotation（人工 review、自动评估、最佳路径标记都走这里）。
    /// 这层元数据独立于 trace 主存储；它记录的是“后验判断”，不参与 WAL 去重和 span 折叠。
    pub fn add_annotation(
        &self,
        input: NewTraceAnnotation,
        tenant_id: Option<u64>,
    ) -> TraceAnnotation {
        self.add_annotation_with_id_base(input, tenant_id, 0)
    }

    /// 给 cluster gateway 使用的 metadata id 命名空间。
    ///
    /// 单机模式仍使用从 1 开始的本地自增 id；cluster mode 给每个 shard 分配高位前缀，
    /// 避免不同 shard 的 annotation_id 在全局查询和 update/delete 时撞号。
    pub(crate) fn add_annotation_with_id_base(
        &self,
        input: NewTraceAnnotation,
        tenant_id: Option<u64>,
        id_base: u64,
    ) -> TraceAnnotation {
        let _guard = self.write_lock.lock().unwrap();
        let mut next = self.next_annotation_id.lock().unwrap();
        let now = unix_now_ns_u64();
        let annotation_id = id_base.saturating_add(*next);
        let annotation = TraceAnnotation {
            annotation_id,
            tenant_id,
            target: input.target.unwrap_or_else(|| {
                if input.span_id.is_some() {
                    AnnotationTarget::Span
                } else {
                    AnnotationTarget::Trace
                }
            }),
            trace_id: input.trace_id,
            span_id: input.span_id,
            external_trace_id: input.external_trace_id,
            external_span_id: input.external_span_id,
            label: input.label,
            score: input.score,
            reason: input.reason,
            source: input.source,
            created_at_ns: now,
            updated_at_ns: now,
            status: input.status.unwrap_or(AnnotationStatus::Active),
            reviewer: input.reviewer,
            attrs: input.attrs,
        };
        *next += 1;
        drop(next);
        self.annotations.lock().unwrap().push(annotation.clone());
        self.persist_metadata();
        annotation
    }

    /// 查询 annotation。tenant_id 放在 filter 上，由 HTTP/Node 从鉴权上下文注入。
    pub fn annotations(&self, filter: &TraceAnnotationFilter) -> Vec<TraceAnnotation> {
        let candidates = self
            .metadata_index
            .lock()
            .unwrap()
            .annotation_candidates(filter);
        self.annotations
            .lock()
            .unwrap()
            .iter()
            .filter(|a| {
                candidates.contains(&a.annotation_id) && annotation_matches(a, filter)
            })
            .cloned()
            .collect()
    }

    /// 更新 annotation 的 review 状态或业务字段。删除也是状态变更，不物理移除记录。
    pub fn update_annotation(
        &self,
        annotation_id: u64,
        tenant_id: Option<u64>,
        input: UpdateTraceAnnotation,
    ) -> Option<TraceAnnotation> {
        let _guard = self.write_lock.lock().unwrap();
        let mut annotations = self.annotations.lock().unwrap();
        let item = annotations.iter_mut().find(|a| {
            a.annotation_id == annotation_id && tenant_id.map_or(true, |t| a.tenant_id == Some(t))
        })?;
        if let Some(label) = input.label {
            item.label = label;
        }
        if let Some(score) = input.score {
            item.score = score;
        }
        if let Some(reason) = input.reason {
            item.reason = reason;
        }
        if let Some(source) = input.source {
            item.source = source;
        }
        if let Some(status) = input.status {
            item.status = status;
        }
        if let Some(reviewer) = input.reviewer {
            item.reviewer = reviewer;
        }
        if let Some(attrs) = input.attrs {
            if input.merge_attrs {
                for (key, value) in attrs {
                    item.attrs.insert(key, value);
                }
            } else {
                item.attrs = attrs;
            }
        }
        item.updated_at_ns = unix_now_ns_u64();
        let out = item.clone();
        drop(annotations);
        self.persist_metadata();
        Some(out)
    }

    /// 软删除 annotation：进入 deleted 状态，默认查询和反向过滤不再命中。
    pub fn delete_annotation(
        &self,
        annotation_id: u64,
        tenant_id: Option<u64>,
        reviewer: Option<String>,
        reason: Option<String>,
    ) -> Option<TraceAnnotation> {
        self.update_annotation(
            annotation_id,
            tenant_id,
            UpdateTraceAnnotation {
                status: Some(AnnotationStatus::Deleted),
                reviewer: Some(reviewer),
                reason: reason.map(Some),
                merge_attrs: true,
                ..Default::default()
            },
        )
    }

    /// 把外部 dataset item 关联到 trace/span。item 本体仍由业务系统或评测平台管理，yiTrace 只保存引用。
    pub fn add_dataset_association(
        &self,
        input: NewDatasetAssociation,
        tenant_id: Option<u64>,
    ) -> DatasetAssociation {
        self.add_dataset_association_with_id_base(input, tenant_id, 0)
    }

    /// 给 cluster gateway 使用的 metadata id 命名空间，理由同 `add_annotation_with_id_base`。
    pub(crate) fn add_dataset_association_with_id_base(
        &self,
        input: NewDatasetAssociation,
        tenant_id: Option<u64>,
        id_base: u64,
    ) -> DatasetAssociation {
        let _guard = self.write_lock.lock().unwrap();
        let mut next = self.next_dataset_association_id.lock().unwrap();
        let association_id = id_base.saturating_add(*next);
        let assoc = DatasetAssociation {
            association_id,
            tenant_id,
            dataset_id: input.dataset_id,
            item_id: input.item_id,
            trace_id: input.trace_id,
            span_id: input.span_id,
            external_trace_id: input.external_trace_id,
            external_span_id: input.external_span_id,
            snapshot_id: input.snapshot_id,
            snapshot_hash: input.snapshot_hash,
            eval_run_id: input.eval_run_id,
            split: input.split,
            label: input.label,
            score: input.score,
            created_at_ns: unix_now_ns_u64(),
            attrs: input.attrs,
        };
        *next += 1;
        drop(next);
        self.dataset_associations
            .lock()
            .unwrap()
            .push(assoc.clone());
        self.persist_metadata();
        assoc
    }

    /// 查询外部 dataset item 与 trace/span 的关联。用于“从数据集样本反查 trace”和“从 trace 找训练/回归身份”。
    pub fn dataset_associations(
        &self,
        filter: &DatasetAssociationFilter,
    ) -> Vec<DatasetAssociation> {
        let candidates = self
            .metadata_index
            .lock()
            .unwrap()
            .dataset_candidates(filter);
        self.dataset_associations
            .lock()
            .unwrap()
            .iter()
            .filter(|a| candidates.contains(&a.association_id) && dataset_association_matches(a, filter))
            .cloned()
            .collect()
    }

    /// 保存一条 golden path 候选。这里只存源 trace/snapshot 引用，不复制 trace payload。
    pub fn add_golden_path(
        &self,
        input: NewGoldenPathCandidate,
        tenant_id: Option<u64>,
    ) -> GoldenPathCandidate {
        self.add_golden_path_with_id_base(input, tenant_id, 0)
    }

    /// 给 cluster gateway 使用的 Golden Path id 命名空间。
    ///
    /// 单机模式仍使用从 1 开始的本地自增 id；cluster mode 给每个 shard 分配高位前缀，
    /// 避免不同 shard 的 golden_path_id 在全局查询和状态更新时撞号。
    pub(crate) fn add_golden_path_with_id_base(
        &self,
        input: NewGoldenPathCandidate,
        tenant_id: Option<u64>,
        id_base: u64,
    ) -> GoldenPathCandidate {
        let _guard = self.write_lock.lock().unwrap();
        let mut next = self.next_golden_path_id.lock().unwrap();
        let now = unix_now_ns_u64();
        let golden_path_id = id_base.saturating_add(*next);
        let candidate = GoldenPathCandidate {
            golden_path_id,
            tenant_id,
            task_fingerprint: input.task_fingerprint,
            trajectory_signature: input.trajectory_signature,
            source_trace_id: input.source_trace_id,
            external_source_trace_id: input.external_source_trace_id,
            snapshot_id: input.snapshot_id,
            snapshot_hash: input.snapshot_hash,
            status: input.status.unwrap_or(GoldenPathStatus::Candidate),
            score: input.score,
            label: input.label,
            reason: input.reason,
            source: input.source,
            created_at_ns: now,
            updated_at_ns: now,
            attrs: input.attrs,
            source_trajectory_steps: input.source_trajectory_steps,
            evidence: input.evidence,
            challenger_of: input.challenger_of,
            eval_profile: input.eval_profile,
            min_sample_count: input.min_sample_count,
            margin_score: input.margin_score,
            comparison_window_ns: input.comparison_window_ns,
            promoted_from: input.promoted_from,
            deprecation_reason: input.deprecation_reason,
            stale_reasons: input.stale_reasons,
        };
        *next += 1;
        drop(next);
        self.golden_paths.lock().unwrap().push(candidate.clone());
        self.persist_metadata();
        candidate
    }

    /// 查询 golden path 候选/已确认路径。tenant_id 放在 filter 上，由 HTTP/Node 注入。
    pub fn golden_paths(&self, filter: &GoldenPathFilter) -> Vec<GoldenPathCandidate> {
        self.golden_paths
            .lock()
            .unwrap()
            .iter()
            .filter(|g| golden_path_matches(g, filter))
            .cloned()
            .collect()
    }

    /// 更新 golden path 状态，用于 candidate -> confirmed/rejected/deprecated。
    pub fn update_golden_path_status(
        &self,
        golden_path_id: u64,
        tenant_id: Option<u64>,
        status: GoldenPathStatus,
        score: Option<u32>,
        reason: Option<String>,
        source: Option<String>,
    ) -> Option<GoldenPathCandidate> {
        let _guard = self.write_lock.lock().unwrap();
        let mut paths = self.golden_paths.lock().unwrap();
        let path = paths
            .iter_mut()
            .find(|g| g.golden_path_id == golden_path_id && g.tenant_id == tenant_id)?;
        path.status = status;
        if score.is_some() {
            path.score = score;
        }
        if reason.is_some() {
            path.reason = reason;
        }
        if source.is_some() {
            path.source = source;
        }
        path.updated_at_ns = unix_now_ns_u64();
        let out = path.clone();
        drop(paths);
        self.persist_metadata();
        Some(out)
    }

    /// 记录一次 retention/apply 的执行审计。审计只保存轻量摘要和 trace id 样本，不复制 trace payload。
    pub fn add_retention_audit(
        &self,
        input: NewRetentionAuditRecord,
        tenant_id: Option<u64>,
    ) -> RetentionAuditRecord {
        self.add_retention_audit_with_id_base(input, tenant_id, 0)
    }

    /// 记录一次 retention/apply 的执行审计，并允许调用方给公开 id 加 shard 前缀。
    ///
    /// 单机路径必须使用 base=0；cluster mode 用 shard 前缀避免不同 shard 的 audit_id 撞号。
    pub(crate) fn add_retention_audit_with_id_base(
        &self,
        input: NewRetentionAuditRecord,
        tenant_id: Option<u64>,
        id_base: u64,
    ) -> RetentionAuditRecord {
        let _guard = self.write_lock.lock().unwrap();
        let mut next = self.next_retention_audit_id.lock().unwrap();
        let audit = RetentionAuditRecord {
            audit_id: id_base.saturating_add(*next),
            tenant_id,
            created_at_ns: unix_now_ns_u64(),
            source: input.source,
            reason: input.reason,
            delete_before_ts: input.delete_before_ts,
            query_json: input.query_json,
            protect_golden_paths: input.protect_golden_paths,
            protect_annotations: input.protect_annotations,
            protect_dataset_associations: input.protect_dataset_associations,
            protect_snapshots: input.protect_snapshots,
            protect_eval_links: input.protect_eval_links,
            protect_path_memory: input.protect_path_memory,
            compact_requested: input.compact_requested,
            compact_reclaim: input.compact_reclaim,
            candidate_trace_count: input.candidate_trace_count,
            protected_trace_count: input.protected_trace_count,
            deletable_trace_count: input.deletable_trace_count,
            requested_trace_count: input.requested_trace_count,
            deleted_trace_count: input.deleted_trace_count,
            deleted_segment_row_count: input.deleted_segment_row_count,
            skipped_live_trace_count: input.skipped_live_trace_count,
            compacted_segment_count: input.compacted_segment_count,
            reclaimed_segment_count: input.reclaimed_segment_count,
            dropped_deleted_row_count: input.dropped_deleted_row_count,
            rewritten_live_row_count: input.rewritten_live_row_count,
            deletable_trace_ids: input.deletable_trace_ids,
            deleted_trace_ids: input.deleted_trace_ids,
            skipped_live_trace_ids: input.skipped_live_trace_ids,
            trace_id_sample_truncated: input.trace_id_sample_truncated,
        };
        *next += 1;
        drop(next);
        self.retention_audits.lock().unwrap().push(audit.clone());
        self.persist_metadata();
        audit
    }

    /// 查询 retention 执行审计。tenant_id 放在 filter 上，由 HTTP/Node 从鉴权上下文注入。
    pub fn retention_audits(&self, filter: &RetentionAuditFilter) -> Vec<RetentionAuditRecord> {
        self.retention_audits
            .lock()
            .unwrap()
            .iter()
            .filter(|a| retention_audit_matches(a, filter))
            .cloned()
            .collect()
    }

    /// 保存一条 retention policy。policy 只保存查询和调度元数据；真正执行仍走 retention/apply。
    pub fn add_retention_policy(
        &self,
        input: NewRetentionPolicy,
        tenant_id: Option<u64>,
    ) -> RetentionPolicy {
        let _guard = self.write_lock.lock().unwrap();
        let mut next = self.next_retention_policy_id.lock().unwrap();
        let now = unix_now_ns_u64();
        let policy = RetentionPolicy {
            policy_id: *next,
            tenant_id,
            name: input.name,
            enabled: input.enabled,
            created_at_ns: now,
            updated_at_ns: now,
            last_run_at_ns: None,
            next_run_at_ns: input.next_run_at_ns,
            interval_ns: input.interval_ns,
            source: input.source,
            reason: input.reason,
            query_json: input.query_json,
        };
        *next += 1;
        drop(next);
        self.retention_policies.lock().unwrap().push(policy.clone());
        self.persist_metadata();
        policy
    }

    /// 查询 retention policies。tenant_id 放在 filter 上，由 HTTP/Node 注入。
    pub fn retention_policies(&self, filter: &RetentionPolicyFilter) -> Vec<RetentionPolicy> {
        self.retention_policies
            .lock()
            .unwrap()
            .iter()
            .filter(|p| retention_policy_matches(p, filter))
            .cloned()
            .collect()
    }

    /// 记录 policy 运行成功后的水位。失败不推进 next_run，避免静默跳过清理窗口。
    pub fn mark_retention_policy_ran(
        &self,
        policy_id: u64,
        tenant_id: Option<u64>,
        ran_at_ns: u64,
    ) -> Option<RetentionPolicy> {
        let _guard = self.write_lock.lock().unwrap();
        let mut policies = self.retention_policies.lock().unwrap();
        let policy = policies
            .iter_mut()
            .find(|p| p.policy_id == policy_id && p.tenant_id == tenant_id)?;
        policy.last_run_at_ns = Some(ran_at_ns);
        policy.next_run_at_ns = Some(ran_at_ns.saturating_add(policy.interval_ns.max(1)));
        policy.updated_at_ns = unix_now_ns_u64();
        let out = policy.clone();
        drop(policies);
        self.persist_metadata();
        Some(out)
    }
}
