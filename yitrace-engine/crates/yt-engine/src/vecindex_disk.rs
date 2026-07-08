//! vecindex_disk.rs —— **磁盘型图向量索引**（可落盘、重启不全量 rebuild），参考 yiTrace graph_index 落盘设计。
//!
//! 现有 `graph.rs` 的 NSW 是**内存型**：图结构只在内存、重启靠重放向量全量 rebuild，且整图常驻。
//! graph_index 的改良（解决 HNSW 内存占用过高）核心三招，本模块照搬其**思路**（不搬 openGauss 页式重件）：
//!
//! 1. **定长槽位节点存储**（`nodes` 文件）：`node_id` 即槽位下标 = 文件偏移。每个节点定长记录 =
//!    外部 id(trace,span) + 软删标记 + 邻边表。邻边可**原地改写**（HNSW 建图频繁更新邻边，不靠追加避免膨胀）。
//! 2. **向量单独定长存储**（`vectors` 文件）：`node_id` → `f32[dim]`，按偏移 **O(1) 随机读**。
//!    向量是大头（1024 维=4KB/点），**单独存 + 按需读**，遍历图只碰邻边(小、热)、向量(大、冷)按需取。
//! 3. **缓冲池**（[`VecCache`] LRU）：向量不全量常驻，热向量留缓存、冷的读盘。这就是 graph_index 比
//!    原生 HNSW（向量内联、整图常驻）省内存的关键。
//!
//! 已落地的能力（不再只是持久化基座）：
//! - **多层 HNSW 导航**：顶层贪心下沉 + 底层 beam search（按需读页），重启不 rebuild。
//! - **进图过滤**：导航穿过不满足谓词的点当路由跳板、只收满足的，选择性谓词下召回不塌。
//! - **邻居选择启发式**（hnswlib heuristic）：选分布更散的邻居，高维连通性好、召回高（替代朴素「取最近 m 个」）。
//! - **SIMD 距离内核**（[`simd`] 子模块）：std::arch 运行时派发，x86_64 走 AVX-512/AVX2/SSE2、
//!   aarch64 走 NEON，零外部依赖。768 维实测加速 ~5.5×。
//! - **多度量**（[`Metric`]）：L2 / Cosine（索引+查询归一化后复用 L2 路径）/ InnerProduct。
//! - 定长存储 + 元页 + 缓冲 + 软删 + append 友好（只写不刷、批量 fsync）。
//!
//! 待升级：向量量化（PQ/SQ 省内存+IO）、并发多线程建图、大规模召回对标。
#![allow(dead_code)]

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::fs::{File, OpenOptions};
use std::os::unix::fs::FileExt; // 定位读写（read_at/write_at），无文件游标 → 并发只读安全
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::GraphIndex;

const MAGIC: u32 = 0x56474958; // "VGIX"
const VERSION: u32 = 1;

// ───── 快哈希（整数键内部索引用，无依赖）：默认 HashMap 走抗 DoS 的 SipHash，对 visited/缓存这类
// 整数键热结构太慢；这里乘移位的廉价哈希快 3-5×，建图/检索全程受益。仅用于内部、非对外暴露的 key。
#[derive(Default)]
struct FastHasher(u64);
impl std::hash::Hasher for FastHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0.rotate_left(5) ^ b as u64).wrapping_mul(0x51_7C_C1_B7_27_22_0A_95);
        }
    }
    fn write_u8(&mut self, i: u8) {
        self.write_u64(i as u64);
    }
    fn write_u32(&mut self, i: u32) {
        self.write_u64(i as u64);
    }
    fn write_u64(&mut self, i: u64) {
        self.0 = (self.0 ^ i).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
}
type FastBuild = std::hash::BuildHasherDefault<FastHasher>;
type FastMap<K, V> = HashMap<K, V, FastBuild>;
type FastSet<K> = std::collections::HashSet<K, FastBuild>;

/// 距离度量（索引级配置）。归一化存储后 cosine 与 L2² 单调等价，整条建图/检索路径复用 L2；
/// InnerProduct 单独走负点积。持久化时存成 1 字节（见 `Meta`），旧索引读回默认 L2。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Metric {
    L2 = 0,
    Cosine = 1,
    InnerProduct = 2,
}

impl Default for Metric {
    fn default() -> Self {
        Metric::L2
    }
}

fn l2_sq(a: &[f32], b: &[f32]) -> f32 {
    simd::l2_sq(a, b)
}

// ───────────────────────── SIMD 距离内核（std::arch，运行时派发，零外部依赖） ─────────────────────────
//
// 距离是建图（search_layer 每次 dist）+ 检索的主成本。按 CPU 特征运行时派发到最快的向量化实现：
// x86_64：AVX-512(16×f32) → AVX2(8×) → SSE2(4×，基线保证)；aarch64：NEON(4×，编译期保证)；
// 其余退化标量。横向求和顺序与标量不同，故有 ~1e-5 级浮点误差（测试用容差断言）。
// unsafe 仅在各 #[target_feature] 实现内部；intrinsic 名在 Rust 1.9x 不自动可见，故各 fn 内 `use`。

mod simd;

include!("vecindex_disk/cache_types.rs");
include!("vecindex_disk/store.rs");
include!("vecindex_disk/hnsw_index.rs");
include!("vecindex_disk/durable_index.rs");

#[cfg(test)]
mod tests;
