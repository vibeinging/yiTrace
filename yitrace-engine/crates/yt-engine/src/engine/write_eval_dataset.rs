impl WriteCoordinator {
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

    /// 对一个数据集**现跑 scorer**,聚合成通过率/均分看板(整体 + per-agent)——回归基准:
    /// 同一数据集 + 同一 scorer 反复跑,通过率掉了就是 agent/prompt 退步了。返回 None=无此数据集。
    /// 注意:这里直接对数据集里**冻结的 span 快照**评分,不走 upgrade 写回(那是线上 trace 的事)。
    pub fn eval_dataset(
        &self,
        name: &str,
        scorer: &dyn Scorer,
        pass_threshold: u32,
    ) -> Option<Vec<EvalSummary>> {
        let ds = self.datasets.lock().unwrap().get(name).cloned()?;
        let scored = ds.examples.iter().filter_map(|ex| {
            scorer
                .score(&ex.span)
                .map(|o| (ex.span.agent_name.clone(), o.score))
        });
        Some(aggregate_eval(scored, pass_threshold))
    }
}
