impl WriteCoordinator {

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
                | Projection::OUTPUT_TOKENS
                | Projection::USAGE_COST,
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
        // 节点聚合：actor → (kind, span_count, in_tok, out_tok, cached, reasoning, total, cost).
        let mut nodes: BTreeMap<String, (ActorKind, usize, u64, u64, u64, u64, u64, u64)> =
            BTreeMap::new();
        for s in &spans {
            let (name, kind) = actor_of(s);
            span_actor.insert(s.span_id, name.clone());
            let e = nodes.entry(name).or_insert((kind, 0, 0, 0, 0, 0, 0, 0));
            e.1 += 1;
            e.2 += s.input_tokens.unwrap_or(0);
            e.3 += s.output_tokens.unwrap_or(0);
            e.4 += s.cached_input_tokens.unwrap_or(0);
            e.5 += s.reasoning_tokens.unwrap_or(0);
            e.6 += usage_total_tokens(
                s.input_tokens.unwrap_or(0),
                s.output_tokens.unwrap_or(0),
                s.cached_input_tokens.unwrap_or(0),
                s.reasoning_tokens.unwrap_or(0),
                s.total_tokens,
            );
            e.7 += usage_cost_usd_nanos_for_model(
                s.input_tokens.unwrap_or(0),
                s.output_tokens.unwrap_or(0),
                s.cached_input_tokens.unwrap_or(0),
                s.reasoning_tokens.unwrap_or(0),
                s.cost_usd_nanos,
                s.provider.as_deref(),
                s.model.as_deref(),
            );
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
                    |(
                        actor,
                        (
                            kind,
                            span_count,
                            input_tokens,
                            output_tokens,
                            cached_input_tokens,
                            reasoning_tokens,
                            total_tokens,
                            cost_usd_nanos,
                        ),
                    )| AgentGraphNode {
                        actor,
                        kind,
                        span_count,
                        input_tokens,
                        output_tokens,
                        cached_input_tokens,
                        reasoning_tokens,
                        total_tokens,
                        cost_usd_nanos,
                    },
                )
                .collect(),
            edges: edges
                .into_iter()
                .map(|((from, to), count)| AgentGraphEdge { from, to, count })
                .collect(),
        }
    }
}
