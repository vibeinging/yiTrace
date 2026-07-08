#[cfg(target_arch = "aarch64")]
use std::arch::aarch64 as sx;
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64 as sx;

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
    a.iter()
        .zip(b.iter())
        .map(|(&x, &y)| (x - y) * (x - y))
        .sum()
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
            assert!(
                (sim - sca).abs() <= sca.abs() * 1e-4 + 1e-5,
                "dim={n}: simd={sim} scalar={sca}"
            );
        }
    }

    #[test]
    fn dot_matches_scalar_various_dims() {
        for &n in &[1usize, 4, 8, 16, 31, 128, 768] {
            let a = rand_vec(3, n);
            let b = rand_vec(4, n);
            let sim = dot(&a, &b);
            let sca = dot_scalar(&a, &b);
            assert!(
                (sim - sca).abs() <= sca.abs() * 1e-4 + 1e-5,
                "dim={n}: simd={sim} scalar={sca}"
            );
        }
    }

    #[test]
    fn normalize_produces_unit_vector() {
        let v = rand_vec(5, 128);
        let (unit, norm) = normalize(&v);
        let unit_norm_sq = dot(&unit, &unit);
        assert!(
            (unit_norm_sq - 1.0).abs() < 1e-4,
            "归一化后范数²应=1，实={unit_norm_sq}"
        );
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
        fn bench<F: Fn(&[f32], &[f32]) -> f32>(
            f: F,
            a: &[Vec<f32>],
            b: &[Vec<f32>],
            iters: usize,
        ) -> (f64, f32) {
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
        let s = 0xDEAD_BEEF_CAFE_F00Du64;
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
            a.len(),
            iters,
            t_sca / t_sim
        );
    }
}
