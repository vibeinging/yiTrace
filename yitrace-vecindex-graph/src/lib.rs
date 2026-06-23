//! yt-vecindex-graph —— 团队 **graph_index**（图式向量 ANN）接入引擎的 [`yt_engine::GraphIndex`] 接缝。
//!
//! 设计同 Vortex / jieba：外部 C/C++ 依赖关在工作区外独立 crate，引擎骨架保持零依赖、离线可编。
//! 这里整套 ANN（建图/搜索/过滤）都是团队库，本 crate 只做编组 + 把护城河接好：
//!
//! **进图过滤（护城河）跨 FFI**：带过滤近邻搜索要在图遍历过程中回调 Rust 谓词决定收哪些点
//! （不是搜完再筛——那样选择性谓词下召回会崩，见 `yt-engine/graph.rs` 的实证对照）。本 crate 把引擎给的
//! `&dyn Fn(u64,u64)->bool` 闭包包成 `(filter_ctx, filter_fn)` 一对传进 C，C 遍历时回调它。见 `ABI.md`。
//!
//! ```ignore
//! use yt_engine::CoordinatorBuilder;
//! use yt_vecindex_graph::GraphAnnFfi;
//! let eng = CoordinatorBuilder::new()
//!     .with_graph(std::sync::Arc::new(GraphAnnFfi::open(768, 16, 64)?))
//!     .open_durable("/data/trace")?;
//! ```
//!
//! 默认 `mock` feature 用 crate 内 Rust 桩提供 C 符号（精确过滤 top-k），离线可编可测，
//! 验证**回调跨 FFI 真被调用** + 结果过滤排序正确；生产用 `--features link` 接真库。

use std::os::raw::{c_int, c_void};
use std::panic::{catch_unwind, AssertUnwindSafe};

use yt_engine::GraphIndex;

/// 进图过滤回调类型：`filter_fn(ctx, trace_id, span_id) -> 非0=满足谓词`。
type FilterFn = unsafe extern "C" fn(*mut c_void, u64, u64) -> c_int;

// 团队 graph_index 的 C ABI（契约见 ABI.md）。mock 下由本 crate Rust 桩提供；link 下由真库提供。
extern "C" {
    fn vexgraph_open(dim: u32, m: u32, ef: u32) -> *mut c_void;
    fn vexgraph_close(handle: *mut c_void);
    fn vexgraph_add(handle: *mut c_void, trace_id: u64, span_id: u64, vec: *const f32, dim: u32);
    fn vexgraph_search(
        handle: *mut c_void,
        query: *const f32,
        dim: u32,
        k: u32,
        filter_ctx: *mut c_void,
        filter_fn: FilterFn,
        out_trace: *mut u64,
        out_span: *mut u64,
        out_dist: *mut f32,
    ) -> u32;
}

/// 建索引失败（`vexgraph_open` 返回 NULL，通常是维度非法或内存不足）。
#[derive(Debug)]
pub struct GraphOpenError;
impl std::fmt::Display for GraphOpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "vexgraph_open 建索引失败")
    }
}
impl std::error::Error for GraphOpenError {}

/// 团队 graph_index 的图式向量 ANN。持有 C 句柄，实现引擎的 `GraphIndex`。
pub struct GraphAnnFfi {
    handle: *mut c_void,
}

// 同一句柄上 search 只读并发安全（见 ABI.md）；加点在引擎单写者下串行。
unsafe impl Send for GraphAnnFfi {}
unsafe impl Sync for GraphAnnFfi {}

impl GraphAnnFfi {
    /// 建索引。`dim`=向量维度，`m`=每点邻居数，`ef`=搜索 beam 宽度。
    pub fn open(dim: usize, m: usize, ef: usize) -> Result<Self, GraphOpenError> {
        // SAFETY: 纯值参数；失败返回 NULL，下面判空。
        let handle = unsafe { vexgraph_open(dim as u32, m as u32, ef as u32) };
        if handle.is_null() {
            return Err(GraphOpenError);
        }
        Ok(Self { handle })
    }
}

impl Drop for GraphAnnFfi {
    fn drop(&mut self) {
        // SAFETY: handle 来自 open 且非空，只在此释放一次。
        unsafe { vexgraph_close(self.handle) };
    }
}

/// 把 Rust 闭包指针 + 维度不符等异常挡在 C 边界外的 trampoline：C 回调它，它转调真闭包。
/// 闭包 panic 不能穿过 C 边界（UB），catch_unwind 兜住、当作"不满足谓词"（保守排除）。
unsafe extern "C" fn filter_trampoline(ctx: *mut c_void, trace_id: u64, span_id: u64) -> c_int {
    let res = catch_unwind(AssertUnwindSafe(|| {
        // ctx 指向调用方栈上的 `&dyn Fn(u64,u64)->bool`（thin → fat 指针）。
        let f = &*(ctx as *const &dyn Fn(u64, u64) -> bool);
        f(trace_id, span_id)
    }));
    match res {
        Ok(true) => 1,
        _ => 0,
    }
}

impl GraphIndex for GraphAnnFfi {
    fn index_embedding(&self, trace_id: u64, span_id: u64, embedding: Vec<f32>) {
        // SAFETY: embedding.as_ptr() 指向 embedding.len() 个 f32，库在调用内拷走；维度不符库按 ABI 忽略。
        unsafe { vexgraph_add(self.handle, trace_id, span_id, embedding.as_ptr(), embedding.len() as u32) };
    }

    fn search(&self, query: &[f32], k: usize, filter: &dyn Fn(u64, u64) -> bool) -> Vec<(u64, u64, f32)> {
        if k == 0 || query.is_empty() {
            return Vec::new();
        }
        // 闭包打包：filter_ref 是 fat 指针，取它的地址得 thin 指针当 ctx；filter_ref 在本次调用内一直活着。
        let filter_ref: &dyn Fn(u64, u64) -> bool = filter;
        let ctx = &filter_ref as *const &dyn Fn(u64, u64) -> bool as *mut c_void;

        let mut out_t = vec![0u64; k];
        let mut out_s = vec![0u64; k];
        let mut out_d = vec![0f32; k];
        // SAFETY: 三个输出缓冲长度都是 k；query 长度作 dim 传（库按 open 的 dim 校验）。
        let n = unsafe {
            vexgraph_search(
                self.handle,
                query.as_ptr(),
                query.len() as u32,
                k as u32,
                ctx,
                filter_trampoline,
                out_t.as_mut_ptr(),
                out_s.as_mut_ptr(),
                out_d.as_mut_ptr(),
            )
        } as usize;
        let n = n.min(k); // 防御：库返回数超 k 时不越界读
        (0..n).map(|i| (out_t[i], out_s[i], out_d[i])).collect()
    }
}

// ───────────────────────── 离线 Rust 桩（mock feature） ─────────────────────────
// 提供上面 extern 块声明的 C 符号，使本 crate 在没有团队真库时也能编译/测试。
// 桩不建图，直接对满足谓词的点暴力精确 top-k —— 重点是**走真回调 filter_fn 跨 FFI 调 Rust 谓词**、
// 验证编组与进图过滤的语义契约（结果只含满足谓词的点、按距离升序）。不是生产图式 ANN。
#[cfg(feature = "mock")]
mod mock {
    use super::*;

    struct MockGraph {
        dim: u32,
        pts: Vec<(u64, u64, Vec<f32>)>,
    }

    #[no_mangle]
    unsafe extern "C" fn vexgraph_open(dim: u32, _m: u32, _ef: u32) -> *mut c_void {
        Box::into_raw(Box::new(MockGraph { dim, pts: Vec::new() })) as *mut c_void
    }

    #[no_mangle]
    unsafe extern "C" fn vexgraph_close(handle: *mut c_void) {
        if !handle.is_null() {
            drop(Box::from_raw(handle as *mut MockGraph));
        }
    }

    #[no_mangle]
    unsafe extern "C" fn vexgraph_add(handle: *mut c_void, trace_id: u64, span_id: u64, vec: *const f32, dim: u32) {
        let g = &mut *(handle as *mut MockGraph);
        if dim != g.dim || vec.is_null() {
            return; // 维度不符按 ABI 保守忽略
        }
        let v = std::slice::from_raw_parts(vec, dim as usize).to_vec();
        g.pts.push((trace_id, span_id, v));
    }

    #[no_mangle]
    #[allow(clippy::too_many_arguments)]
    unsafe extern "C" fn vexgraph_search(
        handle: *mut c_void,
        query: *const f32,
        dim: u32,
        k: u32,
        filter_ctx: *mut c_void,
        filter_fn: FilterFn,
        out_trace: *mut u64,
        out_span: *mut u64,
        out_dist: *mut f32,
    ) -> u32 {
        let g = &*(handle as *mut MockGraph);
        if dim != g.dim {
            return 0;
        }
        let q = std::slice::from_raw_parts(query, dim as usize);
        let mut scored: Vec<(f32, u64, u64)> = Vec::new();
        for (t, s, v) in &g.pts {
            // ★ 跨 FFI 回调 Rust 谓词（进图过滤的语义：只收满足谓词的点）。
            if filter_fn(filter_ctx, *t, *s) == 0 {
                continue;
            }
            let d: f32 = q.iter().zip(v).map(|(a, b)| (a - b) * (a - b)).sum::<f32>().sqrt();
            scored.push((d, *t, *s));
        }
        scored.sort_by(|a, b| a.0.total_cmp(&b.0));
        let n = (k as usize).min(scored.len());
        for (i, &(d, t, s)) in scored.iter().take(n).enumerate() {
            *out_trace.add(i) = t;
            *out_span.add(i) = s;
            *out_dist.add(i) = d;
        }
        n as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(dim: usize) -> GraphAnnFfi {
        GraphAnnFfi::open(dim, 16, 64).expect("mock open")
    }

    // 核心：带过滤搜索时 filter_fn 跨 FFI 被真调用，结果只含满足谓词的点、按距离升序。
    #[test]
    fn filter_callback_crosses_ffi_and_constrains_results() {
        let idx = build(3);
        // trace_id=1 的是"目标域"，trace_id=0 是干扰；放一个 trace 1 的点正好等于查询向量。
        idx.index_embedding(0, 100, vec![9.0, 9.0, 9.0]);
        idx.index_embedding(1, 200, vec![0.0, 0.0, 0.0]); // 命中谓词且离查询最近
        idx.index_embedding(1, 201, vec![1.0, 0.0, 0.0]);
        idx.index_embedding(0, 101, vec![0.0, 0.0, 0.1]); // 离查询很近但不满足谓词 → 必须被排除

        let hits = idx.search(&[0.0, 0.0, 0.0], 5, &|t, _| t == 1);
        assert!(hits.iter().all(|&(t, _, _)| t == 1), "结果都满足谓词（回调真生效）");
        assert_eq!((hits[0].0, hits[0].1), (1, 200), "最近的满足点排第一");
        assert!(hits.windows(2).all(|w| w[0].2 <= w[1].2), "按距离升序");
        // 离查询更近的 (0,101) 被谓词挡掉，没出现 —— 证明不是"搜完再筛"也能挡，而是回调在选点时就挡了。
        assert!(!hits.iter().any(|&(t, s, _)| (t, s) == (0, 101)));
    }

    // 闭包捕获外部状态也能跨边界（ctx 真带着闭包环境）。
    #[test]
    fn capturing_closure_filter_works() {
        let idx = build(2);
        for i in 0..20u64 {
            idx.index_embedding(0, i, vec![i as f32, 0.0]);
        }
        let allow: std::collections::HashSet<u64> = [3u64, 7, 11].into_iter().collect();
        let hits = idx.search(&[0.0, 0.0], 10, &|_, s| allow.contains(&s));
        let got: std::collections::HashSet<u64> = hits.iter().map(|&(_, s, _)| s).collect();
        assert_eq!(got, allow, "只返回闭包放行的 span（闭包环境跨 FFI 携带正确）");
    }

    // 作为 Box<dyn GraphIndex> 用（引擎 with_graph 注入口要的就是这个）。
    #[test]
    fn usable_as_boxed_graphindex() {
        let idx: std::sync::Arc<dyn GraphIndex> = std::sync::Arc::new(build(2));
        idx.index_embedding(1, 1, vec![1.0, 1.0]);
        let hits = idx.search(&[1.0, 1.0], 1, &|_, _| true);
        assert_eq!((hits[0].0, hits[0].1), (1, 1));
    }

    // 维度不符不崩、空查询不过 FFI。
    #[test]
    fn dim_mismatch_and_empty_are_safe() {
        let idx = build(4);
        idx.index_embedding(1, 1, vec![1.0, 2.0]); // dim=2 ≠ 4 → 被忽略
        assert!(idx.search(&[1.0, 2.0, 3.0, 4.0], 3, &|_, _| true).is_empty(), "没有点");
        assert!(idx.search(&[], 3, &|_, _| true).is_empty(), "空查询直接空");
    }
}
