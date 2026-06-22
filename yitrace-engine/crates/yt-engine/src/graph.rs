//! graph.rs —— **真的图式向量 ANN**（替掉 `InMemoryGraphIndex` 的暴力 L2 占位），用来**验证**整个自研
//! 路线里红队点名的最大翻车点：**带过滤的近邻搜索能不能把召回拉回来**。
//!
//! 这不是生产级 HNSW（没分层、没量化、没 SIMD），是一个**可测量**的 NSW（navigable small-world）图：
//! 每个点连最近的 M 个邻居，搜索用带访问集的 beam search。重点全在两种过滤策略的对比：
//!
//! - **post-filter（事后过滤）**：先按向量搜出 ef 个近邻、**再**用谓词筛 —— 谓词选择性一高（命中的点稀疏），
//!   近邻里能活下来的寥寥无几，召回崩。这正是当前 `search_similar` 里 `|_,_| true` 全放行所掩盖的问题。
//! - **in-graph（进图过滤）**：导航时**穿过**不满足谓词的点当路由跳板，只把满足谓词的点收进结果，
//!   停止条件只看"已收够满足谓词的点没有" —— 于是会一直往图深处探到命中的点。这是 ACORN 思路的最小版。
//!
//! 模块自带一个**会失败的测试**：在选择性谓词下实测 in-graph 召回 ≫ post-filter 召回。这就是给红队那条
//! "拉不回 → 3-5 人月变 8-10" 风险的**实证答复**：进图过滤确实把召回救得回来。真实实现换团队自有 graph_index
//! 的 C ABI（同一套 algorithm/distance/PQ），这里先用 Rust 把"带过滤召回"这件事在一个窄场景跑通。
#![allow(dead_code)]

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::Mutex;

use crate::GraphIndex;

/// f32 的全序包装（NaN 也定序），好进二叉堆。
#[derive(Clone, Copy, PartialEq)]
struct OrdF32(f32);
impl Eq for OrdF32 {}
impl PartialOrd for OrdF32 {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for OrdF32 {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&o.0)
    }
}

struct Node {
    key: (u64, u64),
    vec: Vec<f32>,
    /// 邻居在 nodes 里的下标。
    adj: Vec<usize>,
}

struct GraphState {
    nodes: Vec<Node>,
    /// 每点保留的最大邻居数。
    m: usize,
}

/// 图式向量 ANN（NSW）。`index_embedding` 增点连边，`search` 默认走 in-graph 过滤。
pub struct GraphAnnIndex {
    state: Mutex<GraphState>,
    /// 搜索默认 beam 宽度。
    ef: usize,
}

impl GraphAnnIndex {
    /// `m` = 每点邻居数（建议 8~16）；`ef` = 搜索 beam 宽度（越大越准越慢）。
    pub fn new(m: usize, ef: usize) -> Self {
        Self { state: Mutex::new(GraphState { nodes: Vec::new(), m: m.max(2) }), ef: ef.max(1) }
    }
}

impl Default for GraphAnnIndex {
    fn default() -> Self {
        Self::new(8, 32)
    }
}

fn l2_sq(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).sum()
}

impl GraphState {
    /// 插入一点并和已有点里最近的 m 个互相连边（建图 O(n²)，验证够用）。
    fn insert(&mut self, key: (u64, u64), vec: Vec<f32>) {
        let new_idx = self.nodes.len();
        // 找已有点里最近的 m 个。
        let mut near: Vec<(OrdF32, usize)> =
            self.nodes.iter().enumerate().map(|(i, n)| (OrdF32(l2_sq(&vec, &n.vec)), i)).collect();
        near.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        near.truncate(self.m);

        self.nodes.push(Node { key, vec, adj: near.iter().map(|&(_, i)| i).collect() });

        // 反向连边，并把每个被连点的度数剪到 2*m（保留最近的）。
        let m2 = self.m * 2;
        for &(_, i) in &near {
            self.nodes[i].adj.push(new_idx);
            if self.nodes[i].adj.len() > m2 {
                let base = self.nodes[i].vec.clone();
                let mut neigh = std::mem::take(&mut self.nodes[i].adj);
                neigh.sort_unstable_by(|&a, &b| {
                    OrdF32(l2_sq(&base, &self.nodes[a].vec)).cmp(&OrdF32(l2_sq(&base, &self.nodes[b].vec)))
                });
                neigh.truncate(m2);
                self.nodes[i].adj = neigh;
            }
        }
    }

    /// beam search 核心。`admit` 决定一个点能否进结果集（同时驱动停止条件）；导航**穿过**所有未访问邻居
    /// （含 admit=false 的，当路由跳板）。返回被收进结果的点下标，按距离升序。
    fn beam(&self, query: &[f32], ef: usize, admit: &dyn Fn(usize) -> bool) -> Vec<usize> {
        if self.nodes.is_empty() {
            return Vec::new();
        }
        let mut visited = vec![false; self.nodes.len()];
        // 待扩展前沿：最近的先扩展。
        let mut frontier: BinaryHeap<Reverse<(OrdF32, usize)>> = BinaryHeap::new();
        // 结果集：堆顶是最远的（满了丢最远），只装 admit 通过的点。
        let mut result: BinaryHeap<(OrdF32, usize)> = BinaryHeap::new();

        let start = 0usize; // 入口点
        visited[start] = true;
        let ds = OrdF32(l2_sq(query, &self.nodes[start].vec));
        frontier.push(Reverse((ds, start)));
        if admit(start) {
            result.push((ds, start));
        }

        while let Some(Reverse((cd, cur))) = frontier.pop() {
            // 停止：已收够 ef 个满足谓词的点,且前沿里最近的也比结果里最远的还远 → 收敛。
            if result.len() >= ef {
                if let Some(&(worst, _)) = result.peek() {
                    if cd > worst {
                        break;
                    }
                }
            }
            for &nb in &self.nodes[cur].adj {
                if visited[nb] {
                    continue;
                }
                visited[nb] = true;
                let d = OrdF32(l2_sq(query, &self.nodes[nb].vec));
                frontier.push(Reverse((d, nb))); // 穿过任何点做路由
                if admit(nb) {
                    result.push((d, nb));
                    if result.len() > ef {
                        result.pop(); // 丢最远，保 ef 个最近
                    }
                }
            }
        }

        let mut v: Vec<(OrdF32, usize)> = result.into_vec();
        v.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        v.into_iter().map(|(_, i)| i).collect()
    }

    fn key_filter<'a>(&'a self, filter: &'a dyn Fn(u64, u64) -> bool) -> impl Fn(usize) -> bool + 'a {
        move |idx: usize| {
            let (t, s) = self.nodes[idx].key;
            filter(t, s)
        }
    }
}

impl GraphAnnIndex {
    /// 进图过滤搜索（导航穿过不满足谓词的点，只收满足的）。返回 (trace, span, L2 距离)，距离升序、取前 k。
    pub fn search_ingraph(&self, query: &[f32], k: usize, ef: usize, filter: &dyn Fn(u64, u64) -> bool) -> Vec<(u64, u64, f32)> {
        let st = self.state.lock().unwrap();
        let pred = st.key_filter(filter);
        let mut out: Vec<(u64, u64, f32)> = st
            .beam(query, ef.max(k), &pred)
            .into_iter()
            .map(|i| (st.nodes[i].key.0, st.nodes[i].key.1, l2_sq(query, &st.nodes[i].vec).sqrt()))
            .collect();
        out.truncate(k);
        out
    }

    /// 事后过滤搜索（先按向量搜 ef 个近邻，再用谓词筛）。选择性谓词下召回会崩 —— 用来做对照。
    pub fn search_postfilter(&self, query: &[f32], k: usize, ef: usize, filter: &dyn Fn(u64, u64) -> bool) -> Vec<(u64, u64, f32)> {
        let st = self.state.lock().unwrap();
        let all = st.beam(query, ef.max(k), &|_| true); // 先不带过滤搜近邻
        let mut out: Vec<(u64, u64, f32)> = all
            .into_iter()
            .filter(|&i| {
                let (t, s) = st.nodes[i].key;
                filter(t, s)
            })
            .map(|i| (st.nodes[i].key.0, st.nodes[i].key.1, l2_sq(query, &st.nodes[i].vec).sqrt()))
            .collect();
        out.truncate(k);
        out
    }

    /// 暴力精确 top-k（满足谓词的点里），给测试算召回 ground-truth。
    pub fn exact_filtered_topk(&self, query: &[f32], k: usize, filter: &dyn Fn(u64, u64) -> bool) -> Vec<(u64, u64)> {
        let st = self.state.lock().unwrap();
        let mut scored: Vec<(OrdF32, (u64, u64))> = st
            .nodes
            .iter()
            .filter(|n| filter(n.key.0, n.key.1))
            .map(|n| (OrdF32(l2_sq(query, &n.vec)), n.key))
            .collect();
        scored.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        scored.truncate(k);
        scored.into_iter().map(|(_, key)| key).collect()
    }
}

impl GraphIndex for GraphAnnIndex {
    fn index_embedding(&self, trace_id: u64, span_id: u64, embedding: Vec<f32>) {
        self.state.lock().unwrap().insert((trace_id, span_id), embedding);
    }

    /// 引擎默认走 **in-graph** 过滤（好的那条）。
    fn search(&self, query: &[f32], k: usize, filter: &dyn Fn(u64, u64) -> bool) -> Vec<(u64, u64, f32)> {
        self.search_ingraph(query, k, self.ef, filter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 确定性伪随机（LCG），不依赖 rand、可复算。
    struct Lcg(u64);
    impl Lcg {
        fn next_f32(&mut self) -> f32 {
            // 经典 LCG 常数
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((self.0 >> 33) as f32) / (1u64 << 31) as f32
        }
        fn vec(&mut self, dim: usize) -> Vec<f32> {
            (0..dim).map(|_| self.next_f32()).collect()
        }
    }

    fn recall(got: &[(u64, u64, f32)], truth: &[(u64, u64)]) -> f32 {
        if truth.is_empty() {
            return 1.0;
        }
        let hit = got.iter().filter(|(t, s, _)| truth.contains(&(*t, *s))).count();
        hit as f32 / truth.len() as f32
    }

    #[test]
    fn in_graph_filter_recovers_recall_that_post_filter_loses() {
        // 红队最大翻车点的实证:选择性谓词下,进图过滤的召回 ≫ 事后过滤。
        let idx = GraphAnnIndex::new(12, 48);
        let mut rng = Lcg(0x1234_5678);
        let dim = 12;
        let n = 800u64;

        // 800 个点;约 8% 打上 label=1（稀疏谓词:只搜这部分）。label 用 span_id 的奇偶编码:
        // 这里直接把"命中"编进 trace_id：trace_id=1 表示命中、=0 表示不命中。
        let mut matching: Vec<(u64, u64)> = Vec::new();
        for i in 0..n {
            let v = rng.vec(dim);
            let is_match = i % 12 == 0; // ~8.3% 命中
            let trace_id = if is_match { 1 } else { 0 };
            idx.index_embedding(trace_id, i, v.clone());
            if is_match {
                matching.push((trace_id, i));
            }
        }
        assert!(matching.len() > 30, "命中集要够大才有统计意义");

        // 谓词:只要命中的点（trace_id==1）。
        let filter = |t: u64, _s: u64| t == 1;

        // 查询点取某个命中点附近（加点扰动），这样它的图邻域里多是不命中的点 —— 正是 post-filter 崩的场景。
        let probe_key = matching[matching.len() / 2];
        let probe_vec = {
            let st = idx.state.lock().unwrap();
            let base = st.nodes.iter().find(|node| node.key == probe_key).unwrap().vec.clone();
            base
        };

        let k = 10;
        let truth = idx.exact_filtered_topk(&probe_vec, k, &filter);
        let post = idx.search_postfilter(&probe_vec, k, 48, &filter);
        let ingraph = idx.search_ingraph(&probe_vec, k, 48, &filter);

        let r_post = recall(&post, &truth);
        let r_in = recall(&ingraph, &truth);
        eprintln!("[带过滤ANN召回] post-filter={r_post:.2}  in-graph={r_in:.2}  (命中集={})", matching.len());

        // 实测结论:进图过滤召回明显高于事后过滤,且自身召回够用。
        assert!(r_in > r_post, "in-graph({r_in}) 必须高于 post-filter({r_post})");
        assert!(r_in >= 0.8, "in-graph 召回应 ≥0.8,实测 {r_in}");
        // post-filter 在这个稀疏谓词下明显偏低（留余量,不卡死具体值）。
        assert!(r_post <= 0.6, "post-filter 在稀疏谓词下召回应明显偏低,实测 {r_post}");

        // 返回的确实都满足谓词、且按距离升序。
        assert!(ingraph.iter().all(|&(t, _, _)| t == 1), "in-graph 结果都满足谓词");
        assert!(ingraph.windows(2).all(|w| w[0].2 <= w[1].2), "按距离升序");
    }

    #[test]
    fn finds_near_neighbor_without_filter() {
        // 无过滤时,图搜索能找到最近点（基本正确性）。
        let idx = GraphAnnIndex::new(8, 32);
        let mut rng = Lcg(99);
        for i in 0..200u64 {
            idx.index_embedding(0, i, rng.vec(8));
        }
        // 插一个已知点,查它自己 → 应排第一（距离 0）。
        let target = vec![0.5f32; 8];
        idx.index_embedding(7, 9999, target.clone());
        let hits = idx.search(&target, 5, &|_, _| true);
        assert_eq!((hits[0].0, hits[0].1), (7, 9999), "查询点自身应排第一");
        assert!(hits[0].2 < 1e-6, "自身距离 ~0");
    }
}
