# 团队 graph_index C ABI 契约（提案 — 按真实符号调整）

本 crate 按下面这套 C ABI 调用团队 graph_index 库。**这是提案契约**：真符号不同就改 `src/lib.rs` 的
`extern "C"` 块与 `GraphAnnFfi`，trait 接缝（`yt_engine::GraphIndex`）不变。

```c
// 建索引。dim=向量维度，m=每点邻居数，ef=搜索 beam 宽度。失败返回 NULL。
void* vexgraph_open(uint32_t dim, uint32_t m, uint32_t ef);

// 释放索引。
void  vexgraph_close(void* handle);

// 加一个向量点。vec 指向 dim 个 float（拷贝进库，调用返回后调用方可释放）。
void  vexgraph_add(void* handle, uint64_t trace_id, uint64_t span_id, const float* vec, uint32_t dim);

// 带过滤近邻搜索（进图过滤）。返回实际写入的结果数（≤ k）。
//
//   filter_fn(filter_ctx, trace_id, span_id) -> int   // 非 0 = 该点满足谓词、可进结果
//
// ★关键：filter_fn 在**图遍历过程中**被回调（不是搜完再筛）——库导航时穿过不满足谓词的点当跳板，
//   只把 filter_fn 返回非 0 的点收进结果，停止条件只看"收够满足谓词的点没有"。这正是 ACORN 式
//   进图过滤，把选择性谓词下的召回救回来（见 yt-engine/graph.rs 的实证对照）。
//   filter_ctx 原样透传给 filter_fn，库不解释其内容（本 crate 用它携带 Rust 闭包指针）。
//
// 结果写进调用方预分配的三个长度 ≥ k 的数组，按 L2 距离升序：
uint32_t vexgraph_search(
    void* handle, const float* query, uint32_t dim, uint32_t k,
    void* filter_ctx, int (*filter_fn)(void*, uint64_t, uint64_t),
    uint64_t* out_trace, uint64_t* out_span, float* out_dist);
```

## 约定要点

- **距离**：`out_dist` 是 L2 距离（与引擎 `GraphIndex::search` 返回约定一致），升序。
- **进图过滤**：`filter_fn` 必须在遍历中调用以驱动收点/停止；若库只支持事后过滤（post-filter），
  选择性谓词下召回会崩——那不是本契约要的实现，需要 ACORN/进图过滤改造（决策文档列为 3-5 人月项）。
- **回调安全**：`filter_fn` 由本 crate 提供，内部调 Rust 闭包；库须在 `vexgraph_search` 返回前完成全部
  回调（不得把 `filter_ctx` 存下来异步用）——闭包指针只在该次调用内有效。
- **线程**：同一 handle 上 `vexgraph_search` 可并发只读调用（本 crate 据此 `Send+Sync`）；`vexgraph_add`
  与 search 是否可并发由库决定，本 crate 当前在引擎单写者下串行加点，读写不重叠。
- **维度**：`query`/`add` 的 dim 必须等于 open 时的 dim；不等时库应忽略或返回 0（本 crate 的 mock 取这个保守语义）。

## 构建

- 离线/CI（默认）：`cargo test`（mock feature，Rust 桩提供符号，验证回调跨 FFI 编组）。
- 生产：`cargo build --no-default-features --features link`，设 `VEXGRAPH_LIB_DIR` 指向库目录。
