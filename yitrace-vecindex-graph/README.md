# yt-vecindex-graph

yiTrace 的 **graph_index FFI 向量索引** —— 通过 C ABI 接团队自有 graph_index 库,实现引擎的 `GraphIndex` trait。

> 状态:**FFI 接缝就绪**(含跨边界进图过滤回调),默认离线 mock。团队真库到位后 `--features link` 链接真 graph_index。

## 为什么有这个

引擎默认向量索引是**自研磁盘型多层 HNSW**(`yt-engine` 的 `DiskGraphIndex`,落盘 + 缓冲池 + 进图过滤)——开箱即用、零依赖。这个 crate 提供**接团队生产级 graph_index** 的可选路径。

## 关键设计:跨边界进图过滤

带过滤召回的护城河是"进图过滤"(ACORN 式:导航穿过不满足谓词的点当跳板)。FFI 的硬骨头:**C 图遍历过程中要回调 Rust 谓词**。本 crate 把 `&dyn Fn` 包成 thin 指针 + `extern "C"` trampoline(`catch_unwind` 兜住 panic 不穿 C 边界)传进去。

## 两条向量路(同一 trait,二选一)

| 向量索引 | 在哪 | 何时用 |
|---|---|---|
| `DiskGraphIndex`(自研 HNSW) | `yt-engine` | 默认,零依赖,落盘 |
| 团队 graph_index(FFI,本 crate) | 本 crate | 团队真库到位、想用生产级索引时 |

引擎通过 `CoordinatorBuilder::new().with_graph(...)` 注入。

## 构建

```bash
cargo test                    # 默认离线 mock,4 测试(验证 FFI 编组 + 回调)
# 接真库:cargo build --features link  (需要团队 graph_index 库在场)
```

## ABI 契约

见 `src/lib.rs` 顶部注释(`graph_open` / `graph_insert` / `graph_search` / `graph_close` 的 C 签名,含过滤回调函数指针)。团队真库符号若不同,调整 `extern "C"` 块。

## 许可证

MIT。
