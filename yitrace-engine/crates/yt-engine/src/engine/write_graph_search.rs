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
    /// 角色判定:有 agent_name → Agent;否则有 tool_name → Tool;都没有 → `span:<id>`(Other)。
    /// 边 = 父 span 的角色 → 子 span 的角色(同角色自环剔除,只留跨角色调用/移交),按出现次数聚合。
    /// 节点带聚合统计(span 数、token)。节点/边都确定排序,可复算。
    pub fn agent_graph(&self, snap: &Snapshot, trace_id: u64) -> AgentGraph {
        // 执行图按 agent/工具/父子连边 + 聚合 token —— 只读这些维度,不读原文。
        let proj = Projection::of(
            Projection::AGENT_NAME
                | Projection::TOOL_NAME
                | Projection::PARENT_SPAN_ID
                | Projection::INPUT_TOKENS
                | Projection::OUTPUT_TOKENS,
        );
        let (spans, _) = self.fold_query(
            snap,
            &TraceQuery::trace(trace_id, i64::MIN, i64::MAX),
            None,
            proj,
        );

        // 角色判定（返回 (名字, 类型)）。
        let actor_of = |s: &FoldedSpan| -> (String, ActorKind) {
            if let Some(a) = &s.agent_name {
                (a.clone(), ActorKind::Agent)
            } else if let Some(t) = &s.tool_name {
                (t.clone(), ActorKind::Tool)
            } else {
                (format!("span:{}", s.span_id), ActorKind::Other)
            }
        };

        // span_id → 角色名，供连边时查父角色。
        let mut span_actor: HashMap<u64, String> = HashMap::new();
        // 节点聚合：actor → (kind, span_count, in_tok, out_tok)。
        let mut nodes: BTreeMap<String, (ActorKind, usize, u64, u64)> = BTreeMap::new();
        for s in &spans {
            let (name, kind) = actor_of(s);
            span_actor.insert(s.span_id, name.clone());
            let e = nodes.entry(name).or_insert((kind, 0, 0, 0));
            e.1 += 1;
            e.2 += s.input_tokens.unwrap_or(0);
            e.3 += s.output_tokens.unwrap_or(0);
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
                    |(actor, (kind, span_count, input_tokens, output_tokens))| AgentGraphNode {
                        actor,
                        kind,
                        span_count,
                        input_tokens,
                        output_tokens,
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
        f: &SearchFilter,
        body: impl FnOnce(&dyn Fn(u64, u64) -> bool) -> R,
    ) -> R {
        let attrs = self.filter_attrs.lock().unwrap();
        let need_attrs = f.needs_attrs();
        let pred = move |t: u64, s: u64| -> bool {
            if let Some(tid) = f.trace_id {
                if t != tid {
                    return false;
                }
            }
            if !need_attrs {
                return true; // 仅 trace_id 约束（或无约束），不必查边车
            }
            attrs.matches_key(t, s, f)
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
        let cands = self.with_filter_pred(filter, |pred| {
            let mut c = self.bm25.search(query, k.max(50));
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
        let (bm, vec) = self.with_filter_pred(filter, |pred| {
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
