impl WriteCoordinator {

    /// 给某 span 加向量（向量由外部 embedder 算，不是每个 span 都建）。
    /// 开了持久化(`open_durable`)则**先追加写盘再进内存图** —— 向量段里推不出来,必须单独落盘,
    /// 否则重启后"找相似"全空。
    pub fn index_embedding(&self, trace_id: u64, span_id: u64, embedding: Vec<f32>) {
        if let Some(p) = &self.vector_path {
            let _ = vecstore::append(p, trace_id, span_id, &embedding);
        }
        self.graph.index_embedding(trace_id, span_id, embedding);
    }

    /// 中文检索：BM25 找到候选 span，再折叠成完整 span 返回（带分，按相关性序）。
    /// 这是产品噱头之一「按内容搜 trace」。真实实现把检索下推、只折叠命中行。
    pub fn search_text(&self, snap: &Snapshot, query: &str, k: usize) -> Vec<(FoldedSpan, f32)> {
        self.search_text_filtered(snap, query, k, &|_, _| true)
    }

    /// 带过滤的中文检索：谓词限定 (trace_id, span_id)（如只搜某些 trace）。BM25 无图可下推，过滤后置 +
    /// 过取候选兜住截断。
    pub fn search_text_filtered(
        &self,
        snap: &Snapshot,
        query: &str,
        k: usize,
        filter: &dyn Fn(u64, u64) -> bool,
    ) -> Vec<(FoldedSpan, f32)> {
        let mut cands = self.bm25.search(query, k.max(50));
        cands.retain(|&(t, s, _)| filter(t, s));
        cands.truncate(k);
        self.join_folded(snap, cands)
    }

    /// 找相似：graph_index 向量近邻找到候选 span，再折叠返回（带距离，按相似度序）。
    pub fn search_similar(
        &self,
        snap: &Snapshot,
        query: &[f32],
        k: usize,
    ) -> Vec<(FoldedSpan, f32)> {
        self.search_similar_filtered(snap, query, k, &|_, _| true)
    }

    /// **带过滤找相似**：谓词**下推进图搜索**（`graph.search` 走进图过滤）—— 这正是验证过的 in-graph 过滤
    /// 在引擎层真正用起来（选择性谓词下召回不塌，见 `graph.rs` 的实测）。`filter` 按 (trace_id, span_id) 判。
    /// 快照可见性仍由 `join_folded` 自然裁（不在快照里的 span 折叠不出来）。
    pub fn search_similar_filtered(
        &self,
        snap: &Snapshot,
        query: &[f32],
        k: usize,
        filter: &dyn Fn(u64, u64) -> bool,
    ) -> Vec<(FoldedSpan, f32)> {
        let cands = self.graph.search(query, k, filter);
        self.join_folded(snap, cands)
    }

    /// 混合检索：BM25 关键词命中 + 向量语义相似，用 RRF 融合成一路排序，再折叠返回。
    /// 同时被关键词和语义命中的 span 排更前 —— 「关键词 + 语义混合召回」，单走一路给不出这个排序。
    pub fn search_hybrid(
        &self,
        snap: &Snapshot,
        text: &str,
        query_vec: &[f32],
        k: usize,
    ) -> Vec<(FoldedSpan, f32)> {
        self.search_hybrid_filtered(snap, text, query_vec, k, &|_, _| true)
    }

    /// 带过滤的混合检索：向量侧谓词**下推进图搜索**（in-graph 过滤），关键词侧过滤后置（BM25 无图），
    /// 再 RRF 融合。两路都只在满足谓词的 span 上召回。
    pub fn search_hybrid_filtered(
        &self,
        snap: &Snapshot,
        text: &str,
        query_vec: &[f32],
        k: usize,
        filter: &dyn Fn(u64, u64) -> bool,
    ) -> Vec<(FoldedSpan, f32)> {
        let pool = k.max(10);
        let mut bm = self.bm25.search(text, pool);
        bm.retain(|&(t, s, _)| filter(t, s)); // 关键词侧：后置过滤
        let vec = self.graph.search(query_vec, pool, filter); // 向量侧：下推进图过滤
        let r1: Vec<(u64, u64)> = bm.iter().map(|&(t, s, _)| (t, s)).collect();
        let r2: Vec<(u64, u64)> = vec.iter().map(|&(t, s, _)| (t, s)).collect();
        let fused = rrf_fuse(&[r1, r2], 60.0);
        let cands: Vec<(u64, u64, f32)> = fused
            .into_iter()
            .take(k)
            .map(|((t, s), sc)| (t, s, sc))
            .collect();
        self.join_folded(snap, cands)
    }

    /// 用 (trace,span) 谓词回调跑一段逻辑，谓词由 `SearchFilter` + 属性边车构造（在锁内有效）。
    /// 把"按产品维度（agent/状态/时间）过滤"翻译成 `graph.search` 认的 `Fn(u64,u64)->bool`。
    fn with_filter_pred<R>(
        &self,
        snap: &Snapshot,
        f: &SearchFilter,
        body: impl FnOnce(&dyn Fn(u64, u64) -> bool) -> R,
    ) -> R {
        let attr_candidates = if f.attrs.is_empty() {
            None
        } else {
            self.attr_matching_span_keys(snap, &f.attrs)
        };
        let attrs = self.filter_attrs.lock().unwrap();
        let need_attrs = f.needs_attrs();
        let pred = move |t: u64, s: u64| -> bool {
            if let Some(tid) = f.trace_id {
                if t != tid {
                    return false;
                }
            }
            if let Some(keys) = &attr_candidates {
                if !keys.contains(&(t, s)) {
                    return false;
                }
            }
            if !need_attrs {
                return true; // 仅 trace_id 约束（或无约束），不必查边车
            }
            match attrs.get(&(t, s)) {
                Some(a) => f.attrs_match(a),
                None => false, // 有属性约束但无元数据 → 不命中
            }
        };
        body(&pred)
    }

    /// **按产品维度过滤的找相似**：`SearchFilter`（agent/状态/时间/trace）翻成谓词，下推进图搜索。
    /// 这才是"带过滤 ANN"在真实查询里的样子 —— "找 agent X 报错的相似 span"。
    pub fn search_similar_attr(
        &self,
        snap: &Snapshot,
        query: &[f32],
        k: usize,
        filter: &SearchFilter,
    ) -> Vec<(FoldedSpan, f32)> {
        let cands = self.with_filter_pred(snap, filter, |pred| self.graph.search(query, k, pred));
        self.join_folded(snap, cands)
    }

    /// 按产品维度过滤的**中文检索**：BM25 命中后按 `SearchFilter`（agent/状态/时间/trace）后置过滤。
    /// "搜『盗刷』里 agent=风控、报错的那些 span" —— HTTP 检索端点用这个。
    pub fn search_text_attr(
        &self,
        snap: &Snapshot,
        query: &str,
        k: usize,
        filter: &SearchFilter,
    ) -> Vec<(FoldedSpan, f32)> {
        let cands = self.with_filter_pred(snap, filter, |pred| {
            let mut c = self.bm25.search(query, k.max(50));
            c.retain(|&(t, s, _)| pred(t, s));
            c.truncate(k);
            c
        });
        self.join_folded(snap, cands)
    }

    /// 分域全文检索：input/output/log/tool/model/agent 各有独立 BM25，适合 Trace Inbox 做精准域检索。
    /// attrs/tenant/status 等过滤仍复用同一个谓词边界，保证不会因为换索引而破坏隔离语义。
    pub(crate) fn search_text_domains_attr(
        &self,
        snap: &Snapshot,
        query: &str,
        domains: &[TextDomain],
        k: usize,
        filter: &SearchFilter,
    ) -> Vec<(FoldedSpan, f32)> {
        if domains.is_empty() {
            return self.search_text_attr(snap, query, k, filter);
        }
        let cands = self.with_filter_pred(snap, filter, |pred| {
            let mut c = self.text_domains.lock().unwrap().search(query, domains, k.max(50));
            c.retain(|&(t, s, _)| pred(t, s));
            c.truncate(k);
            c
        });
        self.join_folded(snap, cands)
    }

    /// 按产品维度过滤的混合检索（向量侧下推进图、关键词侧后置过滤）。
    pub fn search_hybrid_attr(
        &self,
        snap: &Snapshot,
        text: &str,
        query_vec: &[f32],
        k: usize,
        filter: &SearchFilter,
    ) -> Vec<(FoldedSpan, f32)> {
        let pool = k.max(10);
        let (bm, vec) = self.with_filter_pred(snap, filter, |pred| {
            let mut bm = self.bm25.search(text, pool);
            bm.retain(|&(t, s, _)| pred(t, s));
            let vec = self.graph.search(query_vec, pool, pred);
            (bm, vec)
        });
        let r1: Vec<(u64, u64)> = bm.iter().map(|&(t, s, _)| (t, s)).collect();
        let r2: Vec<(u64, u64)> = vec.iter().map(|&(t, s, _)| (t, s)).collect();
        let fused = rrf_fuse(&[r1, r2], 60.0);
        let cands: Vec<(u64, u64, f32)> = fused
            .into_iter()
            .take(k)
            .map(|((t, s), sc)| (t, s, sc))
            .collect();
        self.join_folded(snap, cands)
    }

    /// 把检索候选 (trace, span, 分) join 上「在快照里折叠出的完整 span」，保持检索的排序。
    /// **只折叠命中行**：把候选 key 集喂给 `fold_query`，不折叠全库（大数据下检索不再为几条命中折叠整库）。
    fn join_folded(&self, snap: &Snapshot, cands: Vec<(u64, u64, f32)>) -> Vec<(FoldedSpan, f32)> {
        let keys: std::collections::HashSet<(u64, u64)> =
            cands.iter().map(|&(t, s, _)| (t, s)).collect();
        // 检索结果要展示原文（命中片段），读全列。
        let (hits, _) = self.fold_query(snap, &TraceQuery::all(), Some(&keys), Projection::ALL);
        let map: HashMap<(u64, u64), FoldedSpan> = hits
            .into_iter()
            .map(|s| ((s.trace_id, s.span_id), s))
            .collect();
        cands
            .into_iter()
            .filter_map(|(t, s, score)| map.get(&(t, s)).cloned().map(|sp| (sp, score)))
            .collect()
    }
}
