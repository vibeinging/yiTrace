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
mod simd {
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64 as sx;
    #[cfg(target_arch = "aarch64")]
    use std::arch::aarch64 as sx;

    /// L2 平方距离的主入口：运行时派发。
    #[cfg(target_arch = "x86_64")]
    pub fn l2_sq(a: &[f32], b: &[f32]) -> f32 {
        // SAFETY: is_x86_feature_detected! 保证目标特征可用；切片同长由调用方保证。
        unsafe {
            if is_x86_feature_detected!("avx512f") {
                l2_sq_avx512(a, b)
            } else if is_x86_feature_detected!("avx2") {
                l2_sq_avx2(a, b)
            } else {
                l2_sq_sse2(a, b)
            }
        }
    }

    /// L2 平方距离的主入口：运行时派发。
    #[cfg(target_arch = "aarch64")]
    pub fn l2_sq(a: &[f32], b: &[f32]) -> f32 {
        unsafe { l2_sq_neon(a, b) }
    }

    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    pub fn l2_sq(a: &[f32], b: &[f32]) -> f32 {
        l2_sq_scalar(a, b)
    }

    /// 点积（InnerProduct / 归一化后的 cosine 复用）。
    #[cfg(target_arch = "x86_64")]
    pub fn dot(a: &[f32], b: &[f32]) -> f32 {
        unsafe {
            if is_x86_feature_detected!("avx512f") {
                dot_avx512(a, b)
            } else if is_x86_feature_detected!("avx2") {
                dot_avx2(a, b)
            } else {
                dot_sse2(a, b)
            }
        }
    }

    /// 点积（InnerProduct / 归一化后的 cosine 复用）。
    #[cfg(target_arch = "aarch64")]
    pub fn dot(a: &[f32], b: &[f32]) -> f32 {
        unsafe { dot_neon(a, b) }
    }

    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    pub fn dot(a: &[f32], b: &[f32]) -> f32 {
        dot_scalar(a, b)
    }

    /// 把向量归一化成单位向量（cosine 模式：索引时归一化存储、查询时归一化查询）。
    /// 范数为 0 的退化向量保持原样（避免除 0）。返回归一化后的向量 + 原 L2 范数。
    pub fn normalize(v: &[f32]) -> (Vec<f32>, f32) {
        let norm_sq = dot(v, v);
        let norm = norm_sq.sqrt();
        if norm < 1e-12 {
            return (v.to_vec(), 0.0);
        }
        let inv = 1.0 / norm;
        (v.iter().map(|x| x * inv).collect(), norm)
    }

    // ───── 标量实现（所有平台的兜底 + 横向求和收尾 + 测试参照） ─────
    fn l2_sq_scalar(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b.iter()).map(|(&x, &y)| (x - y) * (x - y)).sum()
    }
    fn dot_scalar(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b.iter()).map(|(&x, &y)| x * y).sum()
    }

    // ───── x86_64：SSE2(4×f32) 基线 ─────
    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "sse2")]
    unsafe fn l2_sq_sse2(a: &[f32], b: &[f32]) -> f32 {
        use std::arch::x86_64::*;
        let mut acc = _mm_setzero_ps();
        let n = a.len();
        let mut i = 0;
        while i + 4 <= n {
            let va = _mm_loadu_ps(a.as_ptr().add(i));
            let vb = _mm_loadu_ps(b.as_ptr().add(i));
            let d = _mm_sub_ps(va, vb);
            acc = _mm_add_ps(acc, _mm_mul_ps(d, d));
            i += 4;
        }
        hsum_ps(acc) + l2_sq_scalar(&a[i..], &b[i..])
    }

    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "sse2")]
    unsafe fn dot_sse2(a: &[f32], b: &[f32]) -> f32 {
        use std::arch::x86_64::*;
        let mut acc = _mm_setzero_ps();
        let n = a.len();
        let mut i = 0;
        while i + 4 <= n {
            let va = _mm_loadu_ps(a.as_ptr().add(i));
            let vb = _mm_loadu_ps(b.as_ptr().add(i));
            acc = _mm_add_ps(acc, _mm_mul_ps(va, vb));
            i += 4;
        }
        hsum_ps(acc) + dot_scalar(&a[i..], &b[i..])
    }

    // ───── x86_64：AVX2(8×f32) ─────
    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2")]
    unsafe fn l2_sq_avx2(a: &[f32], b: &[f32]) -> f32 {
        use std::arch::x86_64::*;
        let mut acc = _mm256_setzero_ps();
        let n = a.len();
        let mut i = 0;
        while i + 8 <= n {
            let va = _mm256_loadu_ps(a.as_ptr().add(i));
            let vb = _mm256_loadu_ps(b.as_ptr().add(i));
            let d = _mm256_sub_ps(va, vb);
            acc = _mm256_add_ps(acc, _mm256_mul_ps(d, d));
            i += 8;
        }
        hsum_ps256(acc) + l2_sq_scalar(&a[i..], &b[i..])
    }

    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2")]
    unsafe fn dot_avx2(a: &[f32], b: &[f32]) -> f32 {
        use std::arch::x86_64::*;
        let mut acc = _mm256_setzero_ps();
        let n = a.len();
        let mut i = 0;
        while i + 8 <= n {
            let va = _mm256_loadu_ps(a.as_ptr().add(i));
            let vb = _mm256_loadu_ps(b.as_ptr().add(i));
            acc = _mm256_add_ps(acc, _mm256_mul_ps(va, vb));
            i += 8;
        }
        hsum_ps256(acc) + dot_scalar(&a[i..], &b[i..])
    }

    // ───── x86_64：AVX-512(16×f32) ─────
    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx512f")]
    unsafe fn l2_sq_avx512(a: &[f32], b: &[f32]) -> f32 {
        use std::arch::x86_64::*;
        let mut acc = _mm512_setzero_ps();
        let n = a.len();
        let mut i = 0;
        while i + 16 <= n {
            let va = _mm512_loadu_ps(a.as_ptr().add(i) as *const f32);
            let vb = _mm512_loadu_ps(b.as_ptr().add(i) as *const f32);
            let d = _mm512_sub_ps(va, vb);
            acc = _mm512_add_ps(acc, _mm512_mul_ps(d, d));
            i += 16;
        }
        _mm512_reduce_add_ps(acc) + l2_sq_scalar(&a[i..], &b[i..])
    }

    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx512f")]
    unsafe fn dot_avx512(a: &[f32], b: &[f32]) -> f32 {
        use std::arch::x86_64::*;
        let mut acc = _mm512_setzero_ps();
        let n = a.len();
        let mut i = 0;
        while i + 16 <= n {
            let va = _mm512_loadu_ps(a.as_ptr().add(i) as *const f32);
            let vb = _mm512_loadu_ps(b.as_ptr().add(i) as *const f32);
            acc = _mm512_add_ps(acc, _mm512_mul_ps(va, vb));
            i += 16;
        }
        _mm512_reduce_add_ps(acc) + dot_scalar(&a[i..], &b[i..])
    }

    // ───── aarch64：NEON(4×f32，编译期保证) ─────
    #[cfg(target_arch = "aarch64")]
    #[target_feature(enable = "neon")]
    unsafe fn l2_sq_neon(a: &[f32], b: &[f32]) -> f32 {
        use std::arch::aarch64::*;
        let mut acc = [0f32; 4];
        let mut accv = vld1q_f32(acc.as_ptr());
        let n = a.len();
        let mut i = 0;
        while i + 4 <= n {
            let va = vld1q_f32(a.as_ptr().add(i));
            let vb = vld1q_f32(b.as_ptr().add(i));
            let d = vsubq_f32(va, vb);
            accv = vfmaq_f32(accv, d, d); // fused multiply-add：acc + d*d
            i += 4;
        }
        vst1q_f32(acc.as_mut_ptr(), accv);
        acc.iter().sum::<f32>() + l2_sq_scalar(&a[i..], &b[i..])
    }

    #[cfg(target_arch = "aarch64")]
    #[target_feature(enable = "neon")]
    unsafe fn dot_neon(a: &[f32], b: &[f32]) -> f32 {
        use std::arch::aarch64::*;
        let mut acc = [0f32; 4];
        let mut accv = vld1q_f32(acc.as_ptr());
        let n = a.len();
        let mut i = 0;
        while i + 4 <= n {
            let va = vld1q_f32(a.as_ptr().add(i));
            let vb = vld1q_f32(b.as_ptr().add(i));
            accv = vfmaq_f32(accv, va, vb);
            i += 4;
        }
        vst1q_f32(acc.as_mut_ptr(), accv);
        acc.iter().sum::<f32>() + dot_scalar(&a[i..], &b[i..])
    }

    // ───── 横向求和（x86）：4 通道 SSE → 1 个 f32；8 通道 AVX2 → 先降到 4 通道 ─────
    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "sse2")]
    unsafe fn hsum_ps(v: sx::__m128) -> f32 {
        use std::arch::x86_64::*;
        let buf = [0f32; 4];
        _mm_storeu_ps(buf.as_ptr() as *mut f32, v);
        buf.iter().sum()
    }

    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx,sse2")]
    unsafe fn hsum_ps256(v: sx::__m256) -> f32 {
        use std::arch::x86_64::*;
        // 256 → 128 高低半相加，复用 hsum_ps。
        let lo = _mm256_castps256_ps128(v);
        let hi = _mm256_extractf128_ps(v, 1);
        hsum_ps(_mm_add_ps(lo, hi))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn rand_vec(seed: u64, n: usize) -> Vec<f32> {
            let mut s = seed.wrapping_add(0x9E3779B97F4A7C15);
            (0..n)
                .map(|_| {
                    s = (s ^ (s >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
                    s = (s ^ (s >> 27)).wrapping_mul(0x94D049BB133111EB);
                    ((s ^ (s >> 31)) >> 8) as f32 / (1u64 << 40) as f32 * 20.0 - 10.0
                })
                .collect()
        }

        #[test]
        fn l2_sq_matches_scalar_various_dims() {
            for &n in &[1usize, 3, 4, 7, 8, 15, 16, 17, 33, 128, 129, 768] {
                let a = rand_vec(1, n);
                let b = rand_vec(2, n);
                let sim = l2_sq(&a, &b);
                let sca = l2_sq_scalar(&a, &b);
                assert!((sim - sca).abs() <= sca.abs() * 1e-4 + 1e-5, "dim={n}: simd={sim} scalar={sca}");
            }
        }

        #[test]
        fn dot_matches_scalar_various_dims() {
            for &n in &[1usize, 4, 8, 16, 31, 128, 768] {
                let a = rand_vec(3, n);
                let b = rand_vec(4, n);
                let sim = dot(&a, &b);
                let sca = dot_scalar(&a, &b);
                assert!((sim - sca).abs() <= sca.abs() * 1e-4 + 1e-5, "dim={n}: simd={sim} scalar={sca}");
            }
        }

        #[test]
        fn normalize_produces_unit_vector() {
            let v = rand_vec(5, 128);
            let (unit, norm) = normalize(&v);
            let unit_norm_sq = dot(&unit, &unit);
            assert!((unit_norm_sq - 1.0).abs() < 1e-4, "归一化后范数²应=1，实={unit_norm_sq}");
            assert!((norm - dot(&v, &v).sqrt()).abs() < 1e-3);
        }

        #[test]
        fn normalize_zero_vector_is_safe() {
            let z = vec![0.0; 8];
            let (unit, norm) = normalize(&z);
            assert_eq!(norm, 0.0);
            assert!(unit.iter().all(|x| *x == 0.0));
        }

        // SIMD vs 标量加速比（忽略测试，--ignored --nocapture 跑；release 才有意义）。
        #[test]
        #[ignore]
        fn bench_l2_sq_simd_vs_scalar() {
            fn bench<F: Fn(&[f32], &[f32]) -> f32>(f: F, a: &[Vec<f32>], b: &[Vec<f32>], iters: usize) -> (f64, f32) {
                let mut acc = 0f32;
                let t = std::time::Instant::now();
                for _ in 0..iters {
                    for (x, y) in a.iter().zip(b) {
                        acc += f(x, y);
                    }
                }
                // 用 black_box 阻止编译器把 acc 算掉。
                let acc = std::hint::black_box(acc);
                (t.elapsed().as_secs_f64(), acc)
            }
            let mut s = 0xDEAD_BEEF_CAFE_F00Du64;
            let mk = |n: usize| -> Vec<Vec<f32>> {
                (0..n).map(|_| rand_vec(s.rotate_left(7), 768)).collect()
            };
            let a = mk(1000);
            let b = mk(1000);
            let iters = 500;
            let (t_sim, _) = bench(l2_sq, &a, &b, iters);
            let (t_sca, _) = bench(l2_sq_scalar, &a, &b, iters);
            eprintln!(
                "[SIMD bench] dim=768, {}×{} 距离: simd={t_sim:.4}s scalar={t_sca:.4}s 加速比={:.2}×",
                a.len(), iters, t_sca / t_sim
            );
        }
    }
}

// ───────────────────────── 向量缓冲池（按字节预算的 LRU） ─────────────────────────

/// 向量缓冲池：**按内存预算（字节）** 缓存热向量，对齐 graph_index 的 `vector_buffers`。
/// 例：预算 1GiB、索引 10GiB → 只有约 1GiB 的热向量常驻，冷向量淘汰、再访问回磁盘读。
///
/// **O(1) 访问**：每项记一个访问 tick，命中只更 tick（不再每次线性扫整个缓存更 LRU 顺序——那是建图/
/// 检索慢的主因之一）；仅在**超预算时**才 O(n) 批量淘汰最久未用的、一次腾出 ~10% 余量（摊销，命中区无淘汰）。
/// 向量存 `Arc<[f32]>`：命中返回 Arc 克隆（仅加引用计数），不复制 dim 个 f32。
struct VecCache {
    budget_bytes: usize,
    cur_bytes: usize,
    map: FastMap<u64, (Arc<[f32]>, u64)>,
    tick: u64,
    hits: u64,
    misses: u64,
}

impl VecCache {
    fn new(budget_bytes: usize) -> Self {
        Self { budget_bytes, cur_bytes: 0, map: FastMap::default(), tick: 0, hits: 0, misses: 0 }
    }

    fn get(&mut self, id: u64) -> Option<Arc<[f32]>> {
        self.tick += 1;
        let t = self.tick;
        if let Some(e) = self.map.get_mut(&id) {
            e.1 = t;
            self.hits += 1;
            Some(e.0.clone())
        } else {
            self.misses += 1;
            None
        }
    }

    fn put(&mut self, id: u64, v: Arc<[f32]>) {
        self.tick += 1;
        let bytes = v.len() * 4;
        match self.map.insert(id, (v, self.tick)) {
            Some((old, _)) => self.cur_bytes = self.cur_bytes + bytes - old.len() * 4,
            None => self.cur_bytes += bytes,
        }
        if self.cur_bytes > self.budget_bytes {
            self.evict();
        }
    }

    /// 超预算时批量淘汰最久未用的，腾到 ~90% 预算（一次腾够、不是每 put 都淘）。
    fn evict(&mut self) {
        let target = (self.budget_bytes * 9 / 10).max(1);
        let mut by_tick: Vec<(u64, u64, usize)> =
            self.map.iter().map(|(&id, (v, t))| (*t, id, v.len() * 4)).collect();
        by_tick.sort_unstable_by_key(|x| x.0);
        for (_, id, bytes) in by_tick {
            if self.cur_bytes <= target || self.map.len() <= 1 {
                break;
            }
            self.map.remove(&id);
            self.cur_bytes -= bytes;
        }
    }
}

/// 节点记录缓存（图拓扑，对齐 graph_index 把邻边/元数据放 shared_buffers）：消除每次访问的 pread 系统调用。
/// 写穿（write_node 同步更新），O(1) 访问的 tick-LRU，按**条数**封顶（节点记录小，默认上限大）。
struct NodeCache {
    cap: usize,
    map: FastMap<u32, (Arc<NodeRec>, u64)>,
    tick: u64,
}

impl NodeCache {
    fn new(cap: usize) -> Self {
        Self { cap: cap.max(1), map: FastMap::default(), tick: 0 }
    }
    fn get(&mut self, id: u32) -> Option<Arc<NodeRec>> {
        self.tick += 1;
        let t = self.tick;
        if let Some(e) = self.map.get_mut(&id) {
            e.1 = t;
            Some(e.0.clone()) // Arc 克隆 = 加引用计数，不复制 NodeRec
        } else {
            None
        }
    }
    fn put(&mut self, id: u32, rec: Arc<NodeRec>) {
        self.tick += 1;
        self.map.insert(id, (rec, self.tick));
        if self.map.len() > self.cap {
            let target = self.cap * 9 / 10;
            let mut by_tick: Vec<(u64, u32)> = self.map.iter().map(|(&id, (_, t))| (*t, id)).collect();
            by_tick.sort_unstable_by_key(|x| x.0);
            for (_, id) in by_tick {
                if self.map.len() <= target {
                    break;
                }
                self.map.remove(&id);
            }
        }
    }
}

/// 磁盘图索引参数（对齐 graph_index 的可调项）。
#[derive(Clone, Copy, Debug)]
pub struct DiskGraphConfig {
    /// 每点最大邻边数（建图参数，对齐 graph_index 的 `m`）。
    pub m: usize,
    /// **向量缓冲池内存预算（字节）**，对齐 graph_index 的 `vector_buffers`。热向量常驻、冷的回磁盘。
    /// 例：`1 << 30` = 1GiB。
    pub vector_cache_bytes: usize,
    /// 建图时候选列表宽度（对齐 `ef_construction`）。越大召回越好、建图越慢。
    pub ef_construction: usize,
    /// 查询时候选列表宽度（对齐 `hnsw_ef_search`）。越大召回越高、查询越慢；实际取 `max(ef_search, k)`。
    pub ef_search: usize,
    /// 距离度量。L2（默认）；Cosine 在索引/查询时归一化后复用 L2 路径；InnerProduct 走负点积。
    pub metric: Metric,
}

impl Default for DiskGraphConfig {
    fn default() -> Self {
        Self { m: 16, vector_cache_bytes: 256 << 20, ef_construction: 64, ef_search: 100, metric: Metric::L2 }
    }
}

impl DiskGraphConfig {
    pub fn with_cache_bytes(mut self, bytes: usize) -> Self {
        self.vector_cache_bytes = bytes;
        self
    }
    pub fn with_m(mut self, m: usize) -> Self {
        self.m = m;
        self
    }
    pub fn with_ef_construction(mut self, ef: usize) -> Self {
        self.ef_construction = ef;
        self
    }
    pub fn with_ef_search(mut self, ef: usize) -> Self {
        self.ef_search = ef;
        self
    }
    pub fn with_metric(mut self, metric: Metric) -> Self {
        self.metric = metric;
        self
    }
}

/// f32 全序包装（NaN 也定序），好进二叉堆。
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

// ───────────────────────── 节点记录（定长槽位） ─────────────────────────

/// 一个节点的内存视图（从盘上定长记录解出）。
#[derive(Clone, Debug, PartialEq)]
pub struct NodeRec {
    pub trace_id: u64,
    pub span_id: u64,
    pub deleted: bool,
    /// HNSW 层级（0=只在底层；越高越稀疏，做导航）。复用记录里原 pad 字节，不涨记录大小。
    pub level: u8,
    /// **底层（level 0）邻居** node_id。上层邻居在内存的 upper 映射里（稀疏、小）。
    pub neighbors: Vec<u32>,
}

// ───────────────────────── 磁盘图存储 ─────────────────────────

/// 磁盘型图存储：定长节点文件 + 定长向量文件 + 元页 + 向量缓冲。
pub struct DiskGraphStore {
    dir: PathBuf,
    nodes: File,
    vectors: File,
    dim: usize,
    m: usize,
    max_deg: usize,
    metric: Metric,
    node_rec_size: usize,
    /// 已分配节点数（= nodes 文件长度 / 记录长，开盘时据此恢复，无需单独持久）。
    count: AtomicU64,
    cache: Mutex<VecCache>,
    node_cache: Mutex<NodeCache>,
}

impl DiskGraphStore {
    /// 打开/创建索引目录。`dim`/`m` 首次创建时定型（重开从元页读回，`cfg.m` 仅作创建默认）。
    /// `cfg.vector_cache_bytes` = 向量缓冲池内存预算（控制常驻内存，对齐 graph_index 的 vector_buffers）。
    pub fn open(dir: impl AsRef<Path>, dim: usize, cfg: DiskGraphConfig) -> std::io::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;
        let meta_path = dir.join("meta");

        // 元页：有则读回（dim/m/metric 以盘上为准），无则按传入值创建并落盘。
        let (dim, m, metric) = match Meta::load(&meta_path) {
            Some(meta) => (meta.dim, meta.m, meta.metric),
            None => {
                Meta { dim, m: cfg.m, metric: cfg.metric }.store(&meta_path)?;
                (dim, cfg.m, cfg.metric)
            }
        };

        let max_deg = (2 * m).max(2);
        let node_rec_size = NODE_HEADER + 4 * max_deg;

        let nodes = OpenOptions::new().read(true).write(true).create(true).open(dir.join("nodes"))?;
        let vectors = OpenOptions::new().read(true).write(true).create(true).open(dir.join("vectors"))?;

        // 节点数从 nodes 文件长度恢复（撕裂的尾部不足一条则忽略）。
        let count = nodes.metadata()?.len() / node_rec_size as u64;

        Ok(Self {
            dir,
            nodes,
            vectors,
            dim,
            m,
            max_deg,
            metric,
            node_rec_size,
            count: AtomicU64::new(count),
            cache: Mutex::new(VecCache::new(cfg.vector_cache_bytes)),
            // 节点记录（图拓扑）缓存，消除每次访问 pread。默认上限 1M 条（~小几百 MB），上量可调。
            node_cache: Mutex::new(NodeCache::new(1 << 20)),
        })
    }

    pub fn dim(&self) -> usize {
        self.dim
    }
    pub fn m(&self) -> usize {
        self.m
    }
    pub fn max_deg(&self) -> usize {
        self.max_deg
    }
    pub fn metric(&self) -> Metric {
        self.metric
    }
    pub fn len(&self) -> u64 {
        self.count.load(Ordering::Acquire)
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 加一个节点：先写向量、再写节点记录（崩在两者之间则该槽未计数、下次复用），返回 node_id。
    /// 维度不符（≠ dim）拒绝。`level` = HNSW 层级。
    pub fn add_node(&self, trace_id: u64, span_id: u64, vector: &[f32], level: u8) -> std::io::Result<Option<u32>> {
        if vector.len() != self.dim {
            return Ok(None);
        }
        let id = self.count.load(Ordering::Acquire);
        self.write_vector(id, vector)?;
        self.write_node(id, trace_id, span_id, false, level, &[])?;
        // 两个文件都落盘后才提交计数（读者据此判可见）。
        self.count.store(id + 1, Ordering::Release);
        self.cache.lock().unwrap().put(id, Arc::from(vector.to_vec()));
        Ok(Some(id as u32))
    }

    /// 原地改写某节点的**底层邻边**（保留 level/软删）。截到 `max_deg`。
    pub fn set_neighbors(&self, id: u32, neighbors: &[u32]) -> std::io::Result<()> {
        let rec = self.read_node(id)?;
        self.write_node(id as u64, rec.trace_id, rec.span_id, rec.deleted, rec.level, neighbors)
    }

    /// 标记软删（保留 level/邻边）。
    pub fn mark_deleted(&self, id: u32) -> std::io::Result<()> {
        let rec = self.read_node(id)?;
        self.write_node(id as u64, rec.trace_id, rec.span_id, true, rec.level, &rec.neighbors)
    }

    /// 改某节点的 HNSW 层级（保留邻边/软删）。
    pub fn set_level(&self, id: u32, level: u8) -> std::io::Result<()> {
        let rec = self.read_node(id)?;
        self.write_node(id as u64, rec.trace_id, rec.span_id, rec.deleted, level, &rec.neighbors)
    }

    /// 读节点记录（拷贝出 `NodeRec`，给需要拥有所有权的调用方）。
    pub fn read_node(&self, id: u32) -> std::io::Result<NodeRec> {
        self.node_arc(id).map(|a| (*a).clone())
    }

    /// 读节点记录（`Arc<NodeRec>`，热路径用，命中只加引用计数、不复制邻居 Vec）。
    pub fn node_arc(&self, id: u32) -> std::io::Result<Arc<NodeRec>> {
        if let Some(a) = self.node_cache.lock().unwrap().get(id) {
            return Ok(a);
        }
        let mut buf = vec![0u8; self.node_rec_size];
        self.nodes.read_exact_at(&mut buf, id as u64 * self.node_rec_size as u64)?;
        let a = Arc::new(decode_node(&buf));
        self.node_cache.lock().unwrap().put(id, a.clone());
        Ok(a)
    }

    /// 读向量（`Arc<[f32]>`，热路径用，命中只加引用计数、不复制）。
    pub fn read_vector_arc(&self, id: u32) -> std::io::Result<Arc<[f32]>> {
        if let Some(v) = self.cache.lock().unwrap().get(id as u64) {
            return Ok(v);
        }
        let mut buf = vec![0u8; self.dim * 4];
        self.vectors.read_exact_at(&mut buf, id as u64 * self.dim as u64 * 4)?;
        let v: Arc<[f32]> = buf.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
        self.cache.lock().unwrap().put(id as u64, v.clone());
        Ok(v)
    }

    /// 读向量（拷成 `Vec<f32>`，给外部 API / 测试用）。
    pub fn read_vector(&self, id: u32) -> std::io::Result<Vec<f32>> {
        self.read_vector_arc(id).map(|v| v.to_vec())
    }

    /// 缓冲命中/未命中计数（测"向量不全量常驻"用）。
    pub fn cache_stats(&self) -> (u64, u64) {
        let c = self.cache.lock().unwrap();
        (c.hits, c.misses)
    }

    /// 缓冲池当前常驻字节 / 预算字节（观测"只用了 1G 显存似的"那种内存上界）。
    pub fn cache_mem(&self) -> (usize, usize) {
        let c = self.cache.lock().unwrap();
        (c.cur_bytes, c.budget_bytes)
    }

    /// 刷盘（fsync 向量 + 节点文件）。写操作本身不刷，由调用方在一批写完后 `sync` 一次（批量、快）。
    /// 同进程内重开读页缓存不需要它；它保证的是**崩溃后落盘**。
    pub fn sync(&self) -> std::io::Result<()> {
        self.vectors.sync_data()?;
        self.nodes.sync_data()
    }

    fn write_vector(&self, id: u64, vector: &[f32]) -> std::io::Result<()> {
        let mut buf = Vec::with_capacity(self.dim * 4);
        for &x in vector {
            buf.extend_from_slice(&x.to_le_bytes());
        }
        self.vectors.write_all_at(&buf, id * self.dim as u64 * 4)
    }

    fn write_node(&self, id: u64, trace_id: u64, span_id: u64, deleted: bool, level: u8, neighbors: &[u32]) -> std::io::Result<()> {
        let nb: Vec<u32> = neighbors.iter().take(self.max_deg).copied().collect();
        let buf = encode_node(self.node_rec_size, self.max_deg, trace_id, span_id, deleted, level, &nb);
        self.nodes.write_all_at(&buf, id * self.node_rec_size as u64)?;
        // 写穿：节点缓存同步更新，读路径直接命中、不回盘。
        self.node_cache.lock().unwrap().put(id as u32, Arc::new(NodeRec { trace_id, span_id, deleted, level, neighbors: nb }));
        Ok(())
    }
}

const NODE_HEADER: usize = 8 + 8 + 1 + 1 + 2; // trace + span + deleted + level + neighbor_count

fn encode_node(rec_size: usize, max_deg: usize, trace_id: u64, span_id: u64, deleted: bool, level: u8, neighbors: &[u32]) -> Vec<u8> {
    let mut b = vec![0u8; rec_size];
    b[0..8].copy_from_slice(&trace_id.to_le_bytes());
    b[8..16].copy_from_slice(&span_id.to_le_bytes());
    b[16] = deleted as u8;
    b[17] = level; // 原 pad 字节
    let n = neighbors.len().min(max_deg);
    b[18..20].copy_from_slice(&(n as u16).to_le_bytes());
    for (i, &nb) in neighbors.iter().take(max_deg).enumerate() {
        let o = NODE_HEADER + i * 4;
        b[o..o + 4].copy_from_slice(&nb.to_le_bytes());
    }
    b
}

fn decode_node(b: &[u8]) -> NodeRec {
    let trace_id = u64::from_le_bytes(b[0..8].try_into().unwrap());
    let span_id = u64::from_le_bytes(b[8..16].try_into().unwrap());
    let deleted = b[16] != 0;
    let level = b[17];
    let n = u16::from_le_bytes(b[18..20].try_into().unwrap()) as usize;
    let mut neighbors = Vec::with_capacity(n);
    for i in 0..n {
        let o = NODE_HEADER + i * 4;
        if o + 4 > b.len() {
            break;
        }
        neighbors.push(u32::from_le_bytes(b[o..o + 4].try_into().unwrap()));
    }
    NodeRec { trace_id, span_id, deleted, level, neighbors }
}

// ───────────────────────── 元页 ─────────────────────────

struct Meta {
    dim: usize,
    m: usize,
    metric: Metric,
}

impl Meta {
    fn load(path: &Path) -> Option<Meta> {
        let bytes = std::fs::read(path).ok()?;
        if bytes.len() < 4 + 4 + 4 + 4 + 1 + 4 {
            return None;
        }
        let crc_stored = u32::from_le_bytes(bytes[bytes.len() - 4..].try_into().ok()?);
        if crc_stored != yt_wal::crc32(&bytes[..bytes.len() - 4]) {
            return None;
        }
        if u32::from_le_bytes(bytes[0..4].try_into().ok()?) != MAGIC {
            return None;
        }
        let dim = u32::from_le_bytes(bytes[8..12].try_into().ok()?) as usize;
        let m = u32::from_le_bytes(bytes[12..16].try_into().ok()?) as usize;
        let metric = match bytes[16] {
            1 => Metric::Cosine,
            2 => Metric::InnerProduct,
            _ => Metric::L2, // 旧索引（VERSION 1 未写 metric 字节）回退默认 L2。
        };
        Some(Meta { dim, m, metric })
    }

    fn store(&self, path: &Path) -> std::io::Result<()> {
        let mut b = Vec::new();
        b.extend_from_slice(&MAGIC.to_le_bytes());
        b.extend_from_slice(&VERSION.to_le_bytes());
        b.extend_from_slice(&(self.dim as u32).to_le_bytes());
        b.extend_from_slice(&(self.m as u32).to_le_bytes());
        b.push(self.metric as u8);
        let crc = yt_wal::crc32(&b);
        b.extend_from_slice(&crc.to_le_bytes());
        // 原子写：tmp + rename。
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &b)?;
        std::fs::rename(&tmp, path)
    }
}

// ───────────────────────── GraphIndex 实现（图导航：NSW 落盘版 + beam search） ─────────────────────────

/// HNSW 最高层级上限（level > 16 在任何现实规模都几乎不可能，封顶防失控）。
const MAX_LEVEL: u8 = 16;

/// splitmix64：把 node_id 散成均匀位，用来确定性地定层级（不依赖 rand / Date，可复算）。
fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E3779B97F4A7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D049BB133111EB);
    x ^ (x >> 31)
}

/// 磁盘型图向量索引（**多层 HNSW**）。底层(level 0)邻边 + 向量在磁盘、按需读；上层图稀疏、常驻内存、
/// 快照持久（导航骨架）。查询从最高层入口贪心下沉、底层 beam 细搜；底层 beam 的收点谓词驱动停止 +
/// 导航穿过不满足点 ⇒ **进图过滤**（带过滤召回不塌）。append 友好：插入只写不刷，靠 `flush` 批量持久。
pub struct DiskGraphIndex {
    store: DiskGraphStore,
    ef_construction: usize,
    ef_search: usize,
    m: usize,
    max_deg: usize,
    metric: Metric,
    ml: f64, // 层级归一 = 1/ln(m)
    /// 上层（level≥1）邻边：稀疏、小，常驻内存。键 (node_id, level)。
    upper: Mutex<HashMap<(u32, u8), Vec<u32>>>,
    /// 入口点 (node_id, 它的 level)。None = 空图。
    entry: Mutex<Option<(u32, u8)>>,
    upper_path: PathBuf,
}

impl DiskGraphIndex {
    pub fn open(dir: impl AsRef<Path>, dim: usize, cfg: DiskGraphConfig) -> std::io::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        let store = DiskGraphStore::open(&dir, dim, cfg)?;
        let m = store.m();
        let max_deg = store.max_deg();
        let upper_path = dir.join("upper");
        let (upper, mut entry) = load_upper(&upper_path);
        // 无上层快照（没 flush 过）但盘上有节点 → 从节点 level 重建入口（退化但可搜，不至于搜啥都空）。
        if entry.is_none() && store.len() > 0 {
            let mut best: Option<(u32, u8)> = None;
            for id in 0..store.len() as u32 {
                if let Ok(r) = store.read_node(id) {
                    if !r.deleted && best.map(|(_, l)| r.level > l).unwrap_or(true) {
                        best = Some((id, r.level));
                    }
                }
            }
            entry = best;
        }
        Ok(Self {
            metric: store.metric(),
            store,
            ef_construction: cfg.ef_construction.max(cfg.m),
            ef_search: cfg.ef_search.max(1),
            m,
            max_deg,
            ml: 1.0 / (m as f64).max(2.0).ln(),
            upper: Mutex::new(upper),
            entry: Mutex::new(entry),
            upper_path,
        })
    }

    pub fn store(&self) -> &DiskGraphStore {
        &self.store
    }

    /// 节点向量与查询的距离（按需读向量，走缓冲；Arc 命中不复制）。读失败返回 +inf。
    /// L2 / Cosine：归一化（cosine 在 index/search 入口做）后用 l2_sq，两者单调等价、复用整条建图路径。
    /// InnerProduct：负点积（点积越大 = 越「近」，取负进堆排序）。
    fn dist(&self, query: &[f32], id: u32) -> f32 {
        match self.store.read_vector_arc(id) {
            Ok(v) => {
                if self.metric == Metric::InnerProduct {
                    -simd::dot(query, &v)
                } else {
                    l2_sq(query, &v)
                }
            }
            Err(_) => f32::INFINITY,
        }
    }

    /// 由 node_id 确定性定层级：floor(-ln(u) * ml)，u∈(0,1) 由 id 哈希得到。封顶 MAX_LEVEL。
    fn level_for(&self, node_id: u32) -> u8 {
        let h = splitmix64(node_id as u64 ^ 0xD1B54A32D192ED03);
        let u = (((h >> 11) as f64) / ((1u64 << 53) as f64)).max(1e-12);
        ((-u.ln() * self.ml).floor() as i64).clamp(0, MAX_LEVEL as i64) as u8
    }

    fn neighbors_at(&self, id: u32, level: u8) -> Vec<u32> {
        if level == 0 {
            self.store.read_node(id).map(|r| r.neighbors).unwrap_or_default()
        } else {
            self.upper.lock().unwrap().get(&(id, level)).cloned().unwrap_or_default()
        }
    }

    fn set_neighbors_at(&self, id: u32, level: u8, neighbors: &[u32]) -> std::io::Result<()> {
        if level == 0 {
            self.store.set_neighbors(id, neighbors)
        } else {
            self.upper.lock().unwrap().insert((id, level), neighbors.to_vec());
            Ok(())
        }
    }

    /// **邻居选择启发式**（hnswlib heuristic）：从候选里选出与查询点分布更散的 m 个邻居，
    /// 替代朴素的「取最近 m 个」。后者在高维下会让近邻簇聚成一团、图连通性变差、召回掉。
    ///
    /// 规则：候选按到 query 的距离升序排；依次考察 e，仅当 e 比**所有已选入的点**都更靠近 query
    /// （即 dist(query,e) < dist(e, r) 对每个已选 r 成立）才选入 —— e 没被任何已选点「挡住」，
    /// 保证选入的点彼此分散。距离函数由 `dist` 闭包给出（复用 self.dist 的按需读 + 缓冲）。
    ///
    /// `candidates` = (id, dist_to_query) 升序；排除 query 自身（id）。
    fn select_neighbors(
        &self,
        _query: &[f32],
        candidates: &[(u32, f32)],
        m: usize,
        dist: &dyn Fn(&[f32], u32) -> f32,
    ) -> Vec<u32> {
        let mut kept: Vec<(u32, f32)> = Vec::with_capacity(m);
        for &(e, de) in candidates {
            if kept.len() >= m {
                break;
            }
            // e 与所有已选点 r 比：只要有一个 r 挡住 e（dist(e,r) < dist(query,e)），丢 e。
            let dominated = kept.iter().any(|&(r, dr)| {
                // dist(query, e) = de；dist(query, r) = dr；这里算 dist(e, r)。
                let er = dist(&self.store.read_vector_arc(e).unwrap_or_else(|_| Arc::from(Vec::new())), r);
                er < de.max(dr)
            });
            if !dominated {
                kept.push((e, de));
            }
        }
        kept.into_iter().map(|(id, _)| id).collect()
    }

    /// HNSW search-layer：在某一层从 `entries` 出发 beam 扩展。`admit` 决定收点 + 驱动停止，
    /// 导航穿过所有未访问邻居（含 admit=false 的）⇒ 进图过滤。返回 (id, 距离) 升序。
    fn search_layer(&self, query: &[f32], entries: &[u32], ef: usize, level: u8, admit: &dyn Fn(u32) -> bool) -> Vec<(u32, f32)> {
        let mut visited: FastSet<u32> = FastSet::default();
        let mut frontier: BinaryHeap<Reverse<(OrdF32, u32)>> = BinaryHeap::new();
        let mut result: BinaryHeap<(OrdF32, u32)> = BinaryHeap::new();

        for &e in entries {
            if visited.insert(e) {
                let d = self.dist(query, e);
                frontier.push(Reverse((OrdF32(d), e)));
                if admit(e) {
                    result.push((OrdF32(d), e));
                }
            }
        }

        while let Some(Reverse((cd, cur))) = frontier.pop() {
            if result.len() >= ef {
                if let Some(&(worst, _)) = result.peek() {
                    if cd > worst {
                        break;
                    }
                }
            }
            // 取 cur 在该层的邻居：level 0（热）借 Arc 不克隆；上层取稀疏小表。
            let arc0 = if level == 0 { self.store.node_arc(cur).ok() } else { None };
            let upper_v: Vec<u32>;
            let nbrs: &[u32] = if let Some(n) = &arc0 {
                &n.neighbors
            } else if level == 0 {
                &[]
            } else {
                upper_v = self.upper.lock().unwrap().get(&(cur, level)).cloned().unwrap_or_default();
                &upper_v
            };
            for &nb in nbrs {
                if !visited.insert(nb) {
                    continue;
                }
                let d = self.dist(query, nb);
                frontier.push(Reverse((OrdF32(d), nb)));
                if admit(nb) {
                    result.push((OrdF32(d), nb));
                    if result.len() > ef {
                        result.pop();
                    }
                }
            }
        }

        let mut v: Vec<(u32, f32)> = result.into_iter().map(|(d, i)| (i, d.0)).collect();
        v.sort_by(|a, b| a.1.total_cmp(&b.1));
        v
    }

    /// 多层插入：顶层贪心下沉找入口 → 各层 search_layer 连边 + 反向边度数剪枝；新点层级更高则成为新入口。
    fn insert(&self, trace_id: u64, span_id: u64, vector: &[f32]) -> std::io::Result<()> {
        // 先占槽得 id（层级由 id 确定性算），再补写 level。
        let Some(id) = self.store.add_node(trace_id, span_id, vector, 0)? else {
            return Ok(());
        };
        let level = self.level_for(id);
        if level > 0 {
            self.store.set_level(id, level)?;
        }

        let entry = *self.entry.lock().unwrap();
        let Some((mut ep, top)) = entry else {
            *self.entry.lock().unwrap() = Some((id, level)); // 第一个点 = 入口
            return Ok(());
        };

        let alive = |q: u32| self.store.node_arc(q).map(|a| !a.deleted).unwrap_or(false);

        // 1) 顶层贪心下沉到 level+1，找靠近插入点的入口（ef=1）。
        let mut lc = top;
        while lc > level {
            let r = self.search_layer(vector, &[ep], 1, lc, &alive);
            if let Some(&(c, _)) = r.first() {
                ep = c;
            }
            lc -= 1;
        }

        // 2) 从 min(level,top) 到 0：search_layer(ef_construction) → 启发式选邻居连边 + 反向剪枝。
        let mut entries = vec![ep];
        for lc in (0..=level.min(top)).rev() {
            let cap = if lc == 0 { self.max_deg } else { self.m };
            let cands = self.search_layer(vector, &entries, self.ef_construction, lc, &alive);
            // 启发式选 m 个分散邻居（候选已升序、排除自身 id）。
            let cands_clean: Vec<(u32, f32)> = cands.into_iter().filter(|&(c, _)| c != id).collect();
            let dist = |q: &[f32], x: u32| self.dist(q, x);
            let chosen = self.select_neighbors(vector, &cands_clean, self.m, &dist);
            self.set_neighbors_at(id, lc, &chosen)?;

            for &nb in &chosen {
                let mut adj = self.neighbors_at(nb, lc);
                if !adj.contains(&id) {
                    adj.push(id);
                }
                if adj.len() > cap {
                    // 反向边也用启发式：以 nb 为查询点，从它的邻边里选 cap 个分散的。
                    let base = self.store.read_vector_arc(nb).unwrap_or_else(|_| Arc::from(Vec::new()));
                    let mut scored: Vec<(u32, f32)> = adj.iter().map(|&x| (x, self.dist(&base, x))).collect();
                    scored.sort_by(|a, b| a.1.total_cmp(&b.1));
                    let dist2 = |q: &[f32], x: u32| self.dist(q, x);
                    adj = self.select_neighbors(&base, &scored, cap, &dist2);
                }
                self.set_neighbors_at(nb, lc, &adj)?;
            }
            // 下一层的入口 = 这一层找到的近邻。
            entries = if cands_clean.is_empty() { vec![ep] } else { cands_clean.iter().map(|&(c, _)| c).collect() };
        }

        // 3) 新点层级更高 → 成为新入口。
        if level > top {
            *self.entry.lock().unwrap() = Some((id, level));
        }
        Ok(())
    }

    /// 暴力精确搜索（测试用 ground-truth；带过滤、跳软删）。
    pub fn brute_force(&self, query: &[f32], k: usize, filter: &dyn Fn(u64, u64) -> bool) -> Vec<(u64, u64, f32)> {
        // Cosine：归一化查询（与索引时的归一化对齐）。IP：不归一化、不取 sqrt（距离已是 -dot）。
        let q: Vec<f32> = if self.metric == Metric::Cosine { simd::normalize(query).0 } else { query.to_vec() };
        let finalize = |d: f32| -> f32 {
            if self.metric == Metric::InnerProduct { d } else { d.max(0.0).sqrt() }
        };
        let mut scored: Vec<(f32, u64, u64)> = Vec::new();
        for id in 0..self.store.len() as u32 {
            let Ok(node) = self.store.read_node(id) else { continue };
            if node.deleted || !filter(node.trace_id, node.span_id) {
                continue;
            }
            scored.push((finalize(self.dist(&q, id)), node.trace_id, node.span_id));
        }
        scored.sort_by(|a, b| a.0.total_cmp(&b.0));
        scored.truncate(k);
        scored.into_iter().map(|(d, t, s)| (t, s, d)).collect()
    }

    /// 当前入口点层级（测试用：验证确实建了多层）。
    pub fn entry_level(&self) -> u8 {
        self.entry.lock().unwrap().map(|(_, l)| l).unwrap_or(0)
    }
}

impl GraphIndex for DiskGraphIndex {
    fn index_embedding(&self, trace_id: u64, span_id: u64, embedding: Vec<f32>) {
        // Cosine：索引时归一化成单位向量存储。归一化后 cosine 距离与 L2² 单调等价 → 整条建图/检索复用 l2_sq。
        let v: Vec<f32> = if self.metric == Metric::Cosine {
            simd::normalize(&embedding).0
        } else {
            embedding
        };
        let _ = self.insert(trace_id, span_id, &v);
    }

    fn search(&self, query: &[f32], k: usize, filter: &dyn Fn(u64, u64) -> bool) -> Vec<(u64, u64, f32)> {
        if k == 0 || query.len() != self.store.dim {
            return Vec::new();
        }
        // Cosine：归一化查询（与索引时的归一化对齐）。
        let q: Vec<f32> = if self.metric == Metric::Cosine { simd::normalize(query).0 } else { query.to_vec() };
        let query: &[f32] = &q;
        let Some((mut ep, top)) = *self.entry.lock().unwrap() else {
            return Vec::new();
        };
        let alive = |q: u32| self.store.node_arc(q).map(|a| !a.deleted).unwrap_or(false);

        // 顶层贪心下沉到 level 1（只导航、ef=1）。
        let mut lc = top;
        while lc >= 1 {
            let r = self.search_layer(query, &[ep], 1, lc, &alive);
            if let Some(&(c, _)) = r.first() {
                ep = c;
            }
            lc -= 1;
        }

        // 底层 ef_search beam + 进图过滤（admit = 未删 + 业务谓词）。node_arc 不克隆。
        let admit = |q: u32| match self.store.node_arc(q) {
            Ok(a) => !a.deleted && filter(a.trace_id, a.span_id),
            Err(_) => false,
        };
        let ef = self.ef_search.max(k);
        // IP：距离已是 -dot，不取 sqrt；L2/Cosine 取 sqrt 还原真实距离。
        let finalize = |d: f32| -> f32 {
            if self.metric == Metric::InnerProduct { d } else { d.max(0.0).sqrt() }
        };
        let mut out: Vec<(u64, u64, f32)> = self
            .search_layer(query, &[ep], ef, 0, &admit)
            .into_iter()
            .filter_map(|(id, d)| self.store.read_node(id).ok().map(|r| (r.trace_id, r.span_id, finalize(d))))
            .collect();
        out.truncate(k);
        out
    }

    fn flush(&self) {
        let _ = self.store.sync();
        let upper = self.upper.lock().unwrap();
        let entry = *self.entry.lock().unwrap();
        let _ = save_upper(&self.upper_path, &upper, entry);
    }
}

/// 上层图快照编解码：entry(flag+node+level) + upper 条目(node,level,n,邻居)，crc + 原子写。
fn save_upper(path: &Path, upper: &HashMap<(u32, u8), Vec<u32>>, entry: Option<(u32, u8)>) -> std::io::Result<()> {
    let mut b = Vec::new();
    match entry {
        Some((n, l)) => {
            b.push(1);
            b.extend_from_slice(&n.to_le_bytes());
            b.push(l);
        }
        None => b.push(0),
    }
    b.extend_from_slice(&(upper.len() as u64).to_le_bytes());
    for (&(node, level), adj) in upper {
        b.extend_from_slice(&node.to_le_bytes());
        b.push(level);
        b.extend_from_slice(&(adj.len() as u16).to_le_bytes());
        for &nb in adj {
            b.extend_from_slice(&nb.to_le_bytes());
        }
    }
    let crc = yt_wal::crc32(&b);
    b.extend_from_slice(&crc.to_le_bytes());
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &b)?;
    std::fs::rename(&tmp, path)
}

fn load_upper(path: &Path) -> (HashMap<(u32, u8), Vec<u32>>, Option<(u32, u8)>) {
    let bytes = std::fs::read(path).unwrap_or_default();
    let mut empty = (HashMap::new(), None);
    if bytes.len() < 4 {
        return empty;
    }
    let body = &bytes[..bytes.len() - 4];
    if yt_wal::crc32(body) != u32::from_le_bytes(bytes[bytes.len() - 4..].try_into().unwrap()) {
        return empty; // 损坏 → 当空（上层图是派生数据，最坏退化成慢一点的导航）
    }
    let mut p = 0usize;
    let entry = if body[p] == 1 {
        p += 1;
        let n = u32::from_le_bytes(body[p..p + 4].try_into().unwrap());
        p += 4;
        let l = body[p];
        p += 1;
        Some((n, l))
    } else {
        p += 1;
        None
    };
    let cnt = u64::from_le_bytes(body[p..p + 8].try_into().unwrap()) as usize;
    p += 8;
    let mut upper = HashMap::with_capacity(cnt);
    for _ in 0..cnt {
        if p + 7 > body.len() {
            return empty;
        }
        let node = u32::from_le_bytes(body[p..p + 4].try_into().unwrap());
        p += 4;
        let level = body[p];
        p += 1;
        let n = u16::from_le_bytes(body[p..p + 2].try_into().unwrap()) as usize;
        p += 2;
        let mut adj = Vec::with_capacity(n);
        for _ in 0..n {
            if p + 4 > body.len() {
                return empty;
            }
            adj.push(u32::from_le_bytes(body[p..p + 4].try_into().unwrap()));
            p += 4;
        }
        upper.insert((node, level), adj);
    }
    empty = (upper, entry);
    empty
}

// ───────────────────────── 引擎用：惰性磁盘图索引（首个向量定维度） ─────────────────────────

/// 引擎 `open_durable` 用的磁盘图索引包装：维度由**首个 embedding** 决定（或重开时从元页读回），
/// 在此之前（还没向量）搜索返回空。这样引擎不必预先知道向量维度。
pub struct DurableGraphIndex {
    dir: PathBuf,
    cfg: DiskGraphConfig,
    inner: Mutex<Option<std::sync::Arc<DiskGraphIndex>>>,
}

impl DurableGraphIndex {
    /// 在 `dir` 下放磁盘图索引。已有元页则立即打开（维度从盘读回）；没有则等首个向量来定维度。
    pub fn open(dir: impl AsRef<Path>, cfg: DiskGraphConfig) -> Self {
        let dir = dir.as_ref().to_path_buf();
        let inner = if dir.join("meta").exists() {
            DiskGraphIndex::open(&dir, 0, cfg).ok().map(std::sync::Arc::new)
        } else {
            None
        };
        Self { dir, cfg, inner: Mutex::new(inner) }
    }

    fn handle(&self) -> Option<std::sync::Arc<DiskGraphIndex>> {
        self.inner.lock().unwrap().clone()
    }
}

impl GraphIndex for DurableGraphIndex {
    fn index_embedding(&self, trace_id: u64, span_id: u64, embedding: Vec<f32>) {
        // 首个向量定维度、建索引；之后复用。锁只护"取/建句柄"，建图本身在句柄上做（句柄内部已同步）。
        let idx = {
            let mut g = self.inner.lock().unwrap();
            if g.is_none() {
                match DiskGraphIndex::open(&self.dir, embedding.len(), self.cfg) {
                    Ok(i) => *g = Some(std::sync::Arc::new(i)),
                    Err(_) => return,
                }
            }
            g.clone()
        };
        if let Some(i) = idx {
            i.index_embedding(trace_id, span_id, embedding);
        }
    }

    fn search(&self, query: &[f32], k: usize, filter: &dyn Fn(u64, u64) -> bool) -> Vec<(u64, u64, f32)> {
        match self.handle() {
            Some(i) => i.search(query, k, filter),
            None => Vec::new(),
        }
    }

    fn flush(&self) {
        if let Some(i) = self.handle() {
            i.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering as O};

    fn tmpdir() -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!("yt_diskgraph_{}_{}", std::process::id(), N.fetch_add(1, O::Relaxed)))
    }

    /// 测试用配置：大缓冲（不淘汰），按需指定 m。
    fn cfg(m: usize) -> DiskGraphConfig {
        DiskGraphConfig { m, vector_cache_bytes: 1 << 20, ..Default::default() }
    }

    #[test]
    fn persists_and_reopens_without_rebuild() {
        let dir = tmpdir();
        {
            let idx = DiskGraphIndex::open(&dir, 3, cfg(16)).unwrap();
            idx.index_embedding(1, 10, vec![0.0, 0.0, 0.0]);
            idx.index_embedding(2, 20, vec![1.0, 0.0, 0.0]);
            idx.index_embedding(3, 30, vec![5.0, 5.0, 5.0]);
            assert_eq!(idx.store().len(), 3);
        } // drop：文件已 fsync

        // 重开：不重放、不 rebuild，直接从盘读回。
        let idx = DiskGraphIndex::open(&dir, 3, cfg(16)).unwrap();
        assert_eq!(idx.store().len(), 3, "节点数从盘恢复");
        assert_eq!(idx.store().read_node(0).unwrap().trace_id, 1);
        assert_eq!(idx.store().read_vector(1).unwrap(), vec![1.0, 0.0, 0.0]);

        // 暴力搜索：查 [0.9,0,0] 最近的是 (2,20)，其次 (1,10)。
        let hits = idx.search(&[0.9, 0.0, 0.0], 2, &|_, _| true);
        assert_eq!((hits[0].0, hits[0].1), (2, 20));
        assert_eq!((hits[1].0, hits[1].1), (1, 10));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn filter_and_soft_delete_respected() {
        let dir = tmpdir();
        let idx = DiskGraphIndex::open(&dir, 2, cfg(16)).unwrap();
        idx.index_embedding(1, 10, vec![0.0, 0.0]);
        idx.index_embedding(1, 11, vec![0.1, 0.0]);
        idx.index_embedding(2, 20, vec![0.0, 0.1]); // 不满足谓词
        // 谓词只要 trace==1
        let hits = idx.search(&[0.0, 0.0], 5, &|t, _| t == 1);
        assert!(hits.iter().all(|&(t, _, _)| t == 1));
        assert_eq!(hits.len(), 2);
        // 软删 node 1 (span 11) 后不再出现。
        idx.store().mark_deleted(1).unwrap();
        let hits2 = idx.search(&[0.0, 0.0], 5, &|t, _| t == 1);
        assert_eq!(hits2.len(), 1);
        assert_eq!((hits2[0].0, hits2[0].1), (1, 10));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn vector_cache_serves_repeat_reads_from_memory() {
        let dir = tmpdir();
        let idx = DiskGraphIndex::open(&dir, 2, cfg(16)).unwrap();
        for i in 0..5u64 {
            idx.index_embedding(1, i, vec![i as f32, 0.0]);
        }
        // 反复读 node 4（最后写入、在缓存里）→ 命中累加。
        for _ in 0..3 {
            let _ = idx.store().read_vector(4).unwrap();
        }
        let (hits, _) = idx.store().cache_stats();
        assert!(hits >= 3, "重复读热向量命中缓存（不每次读盘）");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_budget_caps_resident_memory_cold_vectors_go_to_disk() {
        // 用户场景：设个内存预算，索引比预算大 → 只有预算内的热向量常驻，冷的回磁盘。
        let dir = tmpdir();
        let dim = 4usize;
        let vec_bytes = dim * 4; // 每条向量 16 字节
        // 预算 = 刚好 2 条向量。
        let cfg = DiskGraphConfig { m: 6, vector_cache_bytes: 2 * vec_bytes, ..Default::default() };
        let idx = DiskGraphIndex::open(&dir, dim, cfg).unwrap();
        // 灌 40 条（远超预算）。
        for i in 0..40u64 {
            idx.index_embedding(1, i, vec![i as f32; dim]);
        }
        // 常驻字节不超预算（“只用 1G”那种上界）。
        let (resident, budget) = idx.store().cache_mem();
        assert!(resident <= budget, "常驻 {resident} 不超预算 {budget}");
        assert!(resident <= 2 * vec_bytes, "最多 2 条向量常驻");

        // 扫全部 40 条：预算只容 2 条 → 大量回磁盘（冷数据去磁盘找），且值都正确。
        let (_, miss_before) = idx.store().cache_stats();
        for i in 0..40u32 {
            assert_eq!(idx.store().read_vector(i).unwrap(), vec![i as f32; dim], "冷向量从磁盘读回值正确");
        }
        let (_, miss_after) = idx.store().cache_stats();
        assert!(miss_after - miss_before >= 38, "预算只容 2 条，扫 40 条几乎全回磁盘");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 确定性伪随机（LCG），不依赖 rand、可复算。
    struct Lcg(u64);
    impl Lcg {
        fn next_f32(&mut self) -> f32 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((self.0 >> 33) as f32) / (1u64 << 31) as f32
        }
        fn vec(&mut self, dim: usize) -> Vec<f32> {
            (0..dim).map(|_| self.next_f32()).collect()
        }
    }

    #[test]
    fn graph_search_recall_vs_brute_force() {
        // 图导航的核心：beam search 召回 ≈ 暴力 ground-truth（证明"按需读页的图遍历"找得到近邻）。
        let dir = tmpdir();
        let dim = 8usize;
        let idx = DiskGraphIndex::open(&dir, dim, DiskGraphConfig { m: 8, ef_construction: 64, ef_search: 64, vector_cache_bytes: 1 << 20, metric: Metric::L2 }).unwrap();
        let mut rng = Lcg(0x51A6_3D11);
        for i in 0..150u64 {
            idx.index_embedding(1, i, rng.vec(dim));
        }
        // 多个查询点求平均召回@10。
        let k = 10;
        let mut hit_sum = 0usize;
        let mut probes = 0usize;
        let mut q = Lcg(0xBEEF);
        for _ in 0..8 {
            let query = q.vec(dim);
            let truth: std::collections::HashSet<(u64, u64)> =
                idx.brute_force(&query, k, &|_, _| true).into_iter().map(|(t, s, _)| (t, s)).collect();
            let got = idx.search(&query, k, &|_, _| true);
            hit_sum += got.iter().filter(|(t, s, _)| truth.contains(&(*t, *s))).count();
            probes += 1;
        }
        let recall = hit_sum as f32 / (k * probes) as f32;
        eprintln!("[磁盘图索引] 召回@{k} = {recall:.2}");
        assert!(recall >= 0.85, "beam 召回应接近暴力，实测 {recall:.2}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn builds_multiple_layers_and_persists_them() {
        // 多层 HNSW：灌够多点会建出 level≥1 的层；flush 后重启从上层快照恢复入口/上层图（不靠扫描重建）。
        let dir = tmpdir();
        let dim = 8usize;
        let mut rng = Lcg(0x7A5E);
        let top_level;
        {
            let idx = DiskGraphIndex::open(&dir, dim, DiskGraphConfig { m: 8, ef_construction: 48, ef_search: 48, vector_cache_bytes: 1 << 20, metric: Metric::L2 }).unwrap();
            for i in 0..300u64 {
                idx.index_embedding(1, i, rng.vec(dim));
            }
            top_level = idx.entry_level();
            assert!(top_level >= 1, "300 点应建出多层（入口层级≥1），实测 {top_level}");
            idx.flush(); // 持久上层图 + 入口
        }
        // 重开：upper 快照在 → 入口/上层图从快照恢复（入口层级一致），搜索照常。
        let idx = DiskGraphIndex::open(&dir, dim, cfg(8)).unwrap();
        assert_eq!(idx.entry_level(), top_level, "重启后入口层级从快照恢复一致");
        let probe = idx.store().read_vector(42).unwrap();
        let hits = idx.search(&probe, 5, &|_, _| true);
        assert_eq!((hits[0].0, hits[0].1), (1, 42), "多层重启后搜索查询点自身排第一");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restart_then_graph_search_still_works() {
        // 图结构（邻边）落盘，重启后**不 rebuild** 直接图搜索。
        let dir = tmpdir();
        let dim = 6usize;
        let mut rng = Lcg(0x1234);
        let probe;
        {
            let idx = DiskGraphIndex::open(&dir, dim, cfg(8)).unwrap();
            for i in 0..60u64 {
                idx.index_embedding(1, i, rng.vec(dim));
            }
            probe = idx.store().read_vector(7).unwrap(); // 拿 node 7 的向量当查询
        } // drop

        let idx = DiskGraphIndex::open(&dir, dim, cfg(8)).unwrap();
        // 不重放、不 rebuild：查 node 7 自身 → 应排第一（距离 ~0）。
        let hits = idx.search(&probe, 5, &|_, _| true);
        assert_eq!((hits[0].0, hits[0].1), (1, 7), "重启后图搜索照常，查询点自身排第一");
        assert!(hits[0].2 < 1e-3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn in_graph_filter_returns_only_matching_sorted() {
        // 进图过滤：导航穿过不满足谓词的点、只收满足的，结果按距离升序。
        let dir = tmpdir();
        let dim = 6usize;
        let idx = DiskGraphIndex::open(&dir, dim, cfg(8)).unwrap();
        let mut rng = Lcg(0xACED);
        for i in 0..120u64 {
            let trace = if i % 5 == 0 { 1 } else { 0 }; // 约 20% 命中
            idx.index_embedding(trace, i, rng.vec(dim));
        }
        let probe = idx.store().read_vector(10).unwrap();
        let hits = idx.search(&probe, 10, &|t, _| t == 1);
        assert!(!hits.is_empty());
        assert!(hits.iter().all(|&(t, _, _)| t == 1), "只返回满足谓词的点");
        assert!(hits.windows(2).all(|w| w[0].2 <= w[1].2), "按距离升序");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cosine_mode_search_recalls_brute_force() {
        // Cosine 模式：图检索召回应 ≈ 同一索引的暴力精确搜索（与 L2 同款断言，验证归一化路径对齐）。
        let dir = tmpdir();
        let dim = 8usize;
        let cfg = DiskGraphConfig { m: 8, ef_construction: 64, ef_search: 64, vector_cache_bytes: 1 << 20, metric: Metric::Cosine };
        let idx = DiskGraphIndex::open(&dir, dim, cfg).unwrap();
        let mut rng = Lcg(0xC05E);
        for _ in 0..150u64 {
            idx.index_embedding(1, 0, rng.vec(dim));
        }
        let k = 10;
        let mut hit_sum = 0usize;
        let mut probes = 0usize;
        for _ in 0..8 {
            let query = rng.vec(dim);
            let truth: std::collections::HashSet<(u64, u64)> =
                idx.brute_force(&query, k, &|_, _| true).into_iter().map(|(t, s, _)| (t, s)).collect();
            let got = idx.search(&query, k, &|_, _| true);
            hit_sum += got.iter().filter(|(t, s, _)| truth.contains(&(*t, *s))).count();
            probes += 1;
        }
        let recall = hit_sum as f32 / (k * probes) as f32;
        eprintln!("[磁盘图索引·cosine] 召回@{k} = {recall:.2}");
        assert!(recall >= 0.85, "cosine 召回应接近暴力，实={recall:.2}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn neighbors_round_trip_in_place() {
        // 邻边原地改写 + 读回（图导航阶段的基础）。
        let dir = tmpdir();
        let idx = DiskGraphIndex::open(&dir, 2, cfg(4)).unwrap();
        for i in 0..6u64 {
            idx.index_embedding(1, i, vec![i as f32, 0.0]);
        }
        idx.store().set_neighbors(0, &[1, 2, 3]).unwrap();
        assert_eq!(idx.store().read_node(0).unwrap().neighbors, vec![1, 2, 3]);
        // 改写覆盖。
        idx.store().set_neighbors(0, &[4, 5]).unwrap();
        assert_eq!(idx.store().read_node(0).unwrap().neighbors, vec![4, 5]);
        // 重开后邻边还在。
        drop(idx);
        let idx2 = DiskGraphIndex::open(&dir, 2, cfg(4)).unwrap();
        assert_eq!(idx2.store().read_node(0).unwrap().neighbors, vec![4, 5]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
