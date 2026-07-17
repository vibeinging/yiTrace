/// 查询期间允许临时物化的过滤 key 上限。按每个 HashSet key 约 48 字节估算，超过预算
/// 就回到磁盘逐条校验，避免低选择性属性把进程内存顶满。
const DEFAULT_BM25_FILTER_SET_BUDGET_BYTES: usize = 64 * 1024 * 1024;
const BM25_FILTER_KEY_ESTIMATED_BYTES: usize = 48;

fn bm25_filter_set_budget_bytes() -> usize {
    std::env::var("YT_BM25_FILTER_SET_BUDGET_BYTES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(DEFAULT_BM25_FILTER_SET_BUDGET_BYTES)
}

impl WriteCoordinator {
    /// 装一条 trace 的父子树（树+瀑布视图用）：读出该 trace 的 span，按 parent_span_id 连成树。
    /// 父不在本 trace 内的 span 当根（容错：丢了 root 事件也能渲染）。
    pub fn load_trace_tree(&self, snap: &Snapshot, trace_id: u64) -> TraceTree {
        let (spans, _) =
            self.read_spans_query(snap, &TraceQuery::trace(trace_id, i64::MIN, i64::MAX));
        let mut nodes: BTreeMap<u64, TraceNode> = BTreeMap::new();
        for s in spans {
            nodes.insert(
                s.span_id,
                TraceNode {
                    span: s,
                    children: Vec::new(),
                },
            );
        }
        let mut roots = Vec::new();
        let ids: Vec<u64> = nodes.keys().copied().collect();
        for id in ids {
            let parent = nodes[&id].span.parent_span_id;
            match parent {
                Some(p) if nodes.contains_key(&p) => nodes.get_mut(&p).unwrap().children.push(id),
                _ => roots.push(id),
            }
        }
        for n in nodes.values_mut() {
            n.children.sort_unstable(); // 确定序
        }
        roots.sort_unstable();
        TraceTree {
            trace_id,
            roots,
            nodes,
        }
    }

    /// 一条 trace 的 **agent 执行图（DAG）**：把 span 父子树按 agent/工具维度收拢成"谁调用了谁"。
    /// 角色判定:有 tool_name → Tool;否则有 agent_name → Agent;都没有 → `span:<id>`(Other)。
    /// 边 = 父 span 的角色 → 子 span 的角色(同角色自环剔除,只留跨角色调用/移交),按出现次数聚合。
    /// 节点带聚合统计(span 数、token)。节点/边都确定排序,可复算。
    pub fn agent_graph(&self, snap: &Snapshot, trace_id: u64) -> AgentGraph {
        // 执行图按 agent/工具/父子连边 + 聚合 token —— 只读这些维度,不读原文。
        let proj = Projection::of(
            Projection::AGENT_NAME
                | Projection::TOOL_NAME
                | Projection::PARENT_SPAN_ID
                | Projection::INPUT_TOKENS
                | Projection::OUTPUT_TOKENS
                | Projection::CACHE_READ_TOKENS
                | Projection::CACHE_WRITE_TOKENS,
        );
        let (spans, _) = self.fold_query(
            snap,
            &TraceQuery::trace(trace_id, i64::MIN, i64::MAX),
            None,
            proj,
        );

        // 角色判定（返回 (名字, 类型)）。
        let actor_of = |s: &FoldedSpan| -> (String, ActorKind) {
            if let Some(t) = &s.tool_name {
                (t.clone(), ActorKind::Tool)
            } else if let Some(a) = &s.agent_name {
                (a.clone(), ActorKind::Agent)
            } else {
                (format!("span:{}", s.span_id), ActorKind::Other)
            }
        };

        // span_id → 角色名，供连边时查父角色。
        let mut span_actor: HashMap<u64, String> = HashMap::new();
        // 节点聚合：actor → (kind, span_count, in_tok, out_tok)。
        let mut nodes: BTreeMap<String, (ActorKind, usize, u64, u64, u64, u64, usize, usize)> =
            BTreeMap::new();
        for s in &spans {
            let (name, kind) = actor_of(s);
            span_actor.insert(s.span_id, name.clone());
            let e = nodes.entry(name).or_insert((kind, 0, 0, 0, 0, 0, 0, 0));
            e.1 += 1;
            e.2 += s.input_tokens.unwrap_or(0);
            e.3 += s.output_tokens.unwrap_or(0);
            if let Some(tokens) = s.cache_read_tokens {
                e.4 += tokens;
                e.6 += 1;
            }
            if let Some(tokens) = s.cache_write_tokens {
                e.5 += tokens;
                e.7 += 1;
            }
        }

        // 边聚合：父角色 → 子角色（跳过父不在本 trace 内 / 同角色自环）。
        let mut edges: BTreeMap<(String, String), usize> = BTreeMap::new();
        for s in &spans {
            let Some(parent_id) = s.parent_span_id else {
                continue;
            };
            let Some(from) = span_actor.get(&parent_id) else {
                continue;
            };
            let to = &span_actor[&s.span_id];
            if from == to {
                continue; // 同角色多步,不算一次调用/移交
            }
            *edges.entry((from.clone(), to.clone())).or_insert(0) += 1;
        }

        AgentGraph {
            trace_id,
            nodes: nodes
                .into_iter()
                .map(
                    |(actor, (kind, span_count, input_tokens, output_tokens, read, write, read_n, write_n))| AgentGraphNode {
                        actor,
                        kind,
                        span_count,
                        input_tokens,
                        output_tokens,
                        cache_read_tokens: (read_n > 0).then_some(read),
                        cache_write_tokens: (write_n > 0).then_some(write),
                    },
                )
                .collect(),
            edges: edges
                .into_iter()
                .map(|((from, to), count)| AgentGraphEdge { from, to, count })
                .collect(),
        }
    }

    /// 给某 span 加向量（向量由外部 embedder 算，不是每个 span 都建）。
    /// 开了持久化(`open_durable`)则**先追加写盘再进内存图** —— 向量段里推不出来,必须单独落盘,
    /// 否则重启后"找相似"全空。
    pub fn index_embedding(&self, trace_id: u64, span_id: u64, embedding: Vec<f32>) {
        let _process = self.acquire_process_lock("write");
        let _local = self.write_lock.lock().unwrap();
        self.refresh_from_disk_locked();
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
        self.ensure_segment_scan_indexes_current();
        let cands = self.search_bm25_with_filter(query, k, filter);
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
        self.ensure_segment_scan_indexes_current();
        let pool = k.max(10);
        let bm = self.search_bm25_with_filter(text, pool, filter);
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
        f: &SearchFilter,
        body: impl FnOnce(&dyn Fn(u64, u64) -> bool) -> R,
    ) -> R {
        let need_attrs = f.needs_attrs();
        if !need_attrs {
            let pred = |trace_id: u64, _: u64| f.trace_id.map_or(true, |id| id == trace_id);
            return body(&pred);
        }

        // 精确字段先通过磁盘 postings 求出候选集合。后续 BM25/ANN 只做 HashSet contains，
        // 不再为每一个候选 span 随机读取属性行。
        let candidates = self.filter_candidate_span_keys(f);
        let pred = move |t: u64, s: u64| -> bool {
            if let Some(tid) = f.trace_id {
                if t != tid {
                    return false;
                }
            }
            candidates.contains(&(t, s))
        };
        body(&pred)
    }

    /// 超过查询期集合预算时的保底路径：按相关性逐步扩大候选窗，再从磁盘点查属性。
    /// 原生 BM25 在预算内走 `search_filtered`，不会进入这里；外部适配器也可继续复用此回退。
    fn search_bm25_with_filter(
        &self,
        query: &str,
        k: usize,
        filter: &dyn Fn(u64, u64) -> bool,
    ) -> Vec<(u64, u64, f32)> {
        self.bm25.search_filtered(query, k, filter)
    }

    fn search_bm25_post_filter_mut(
        &self,
        query: &str,
        k: usize,
        filter: &mut dyn FnMut(u64, u64) -> bool,
    ) -> Vec<(u64, u64, f32)> {
        if k == 0 {
            return Vec::new();
        }
        let mut pool = k.max(50);
        loop {
            let mut candidates = self.bm25.search(query, pool);
            let exhausted = candidates.len() < pool;
            candidates.retain(|&(trace_id, span_id, _)| filter(trace_id, span_id));
            if candidates.len() >= k || exhausted {
                candidates.truncate(k);
                return candidates;
            }
            let next = pool.saturating_mul(4);
            if next == pool {
                return candidates;
            }
            pool = next;
        }
    }

    fn search_bm25_with_attrs(
        &self,
        query: &str,
        k: usize,
        filter: &SearchFilter,
    ) -> Vec<(u64, u64, f32)> {
        if !filter.needs_attrs() {
            let predicate = |trace_id: u64, _: u64| {
                filter.trace_id.is_none_or(|expected| expected == trace_id)
            };
            return self.search_bm25_with_filter(query, k, &predicate);
        }
        self.ensure_filter_attrs_current();
        let mut index = self.filter_attrs.lock().unwrap();
        if index.filter_matches_all(filter) {
            // 单租户本地库里，tenant posting 覆盖全部 span。后续 BM25 已不需要属性索引，
            // 先放锁，否则所有同租户全文查询仍会被这把外层锁串行化。
            drop(index);
            return self.bm25.search(query, k);
        }
        let mut predicate = |trace_id, span_id| index.span_matches((trace_id, span_id), filter);
        self.search_bm25_post_filter_mut(query, k, &mut predicate)
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
        let cands = self.with_filter_pred(filter, |pred| self.graph.search(query, k, pred));
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
        let cands = self.search_text_attr_candidates(query, k, filter);
        self.join_folded(snap, cands)
    }

    /// 与 `search_text_attr` 相同，但同时返回 segment 点查证据，供 HTTP readPlan 和性能回归使用。
    pub fn search_text_attr_with_read_plan(
        &self,
        snap: &Snapshot,
        query: &str,
        k: usize,
        filter: &SearchFilter,
    ) -> (Vec<(FoldedSpan, f32)>, ReadPlanStats) {
        let cands = self.search_text_attr_candidates(query, k, filter);
        let candidate_span_keys = cands
            .iter()
            .map(|&(trace_id, span_id, _)| (trace_id, span_id))
            .collect::<HashSet<_>>()
            .len();
        let (hits, scan) = self.join_folded_with_stats(snap, cands);
        (
            hits,
            ReadPlanStats {
                used_filter_index: filter.needs_indexed_filter(),
                candidate_span_keys: Some(candidate_span_keys),
                scanned_segments: scan.scanned_segments,
                point_lookup_segments: scan.point_lookup_segments,
                decoded_segment_rows: scan.decoded_segment_rows,
                index_bytes_read: scan.index_bytes_read,
                data_bytes_read: scan.data_bytes_read,
                indexes_validated: scan.indexes_validated,
                indexes_rebuilt: scan.indexes_rebuilt,
                ..ReadPlanStats::default()
            },
        )
    }

    fn search_text_attr_candidates(
        &self,
        query: &str,
        k: usize,
        filter: &SearchFilter,
    ) -> Vec<(u64, u64, f32)> {
        self.ensure_segment_scan_indexes_current();
        self.ensure_filter_attrs_current();
        let exceeds_candidate_budget = self
            .filter_attrs
            .lock()
            .unwrap()
            .candidate_materialization_key_hint(filter)
            .is_some_and(|count| {
                count.saturating_mul(BM25_FILTER_KEY_ESTIMATED_BYTES)
                    > bm25_filter_set_budget_bytes()
            });
        let cands = if filter.has_non_tenant_constraints() && !exceeds_candidate_budget {
            self.with_filter_pred(filter, |pred| self.search_bm25_with_filter(query, k, pred))
        } else {
            self.search_bm25_with_attrs(query, k, filter)
        };
        cands
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
        self.ensure_segment_scan_indexes_current();
        let pool = k.max(10);
        let (bm, vec) = self.with_filter_pred(filter, |pred| {
            let bm = self.search_bm25_with_filter(text, pool, pred);
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
        self.join_folded_with_stats(snap, cands).0
    }

    fn join_folded_with_stats(
        &self,
        snap: &Snapshot,
        cands: Vec<(u64, u64, f32)>,
    ) -> (Vec<(FoldedSpan, f32)>, FoldQueryStats) {
        let mut seen = std::collections::HashSet::new();
        let cands: Vec<(u64, u64, f32)> = cands
            .into_iter()
            .filter(|(t, s, _)| seen.insert((*t, *s)))
            .collect();
        let keys: std::collections::HashSet<(u64, u64)> =
            cands.iter().map(|&(t, s, _)| (t, s)).collect();
        // 检索结果要展示原文（命中片段），读全列。
        let (hits, stats) =
            self.fold_query(snap, &TraceQuery::all(), Some(&keys), Projection::ALL);
        let map: HashMap<(u64, u64), FoldedSpan> = hits
            .into_iter()
            .map(|s| ((s.trace_id, s.span_id), s))
            .collect();
        let hits = cands
            .into_iter()
            .filter_map(|(t, s, score)| map.get(&(t, s)).cloned().map(|sp| (sp, score)))
            .collect();
        (hits, stats)
    }
}
