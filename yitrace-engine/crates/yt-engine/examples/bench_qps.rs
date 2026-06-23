//! 真实 QPS 基准：用**生产路径**的真实组件测吞吐——
//! 落盘引擎（open_durable）+ ChineseTokenizer 全量词典 BM25 + 磁盘多层 HNSW 向量索引。
//!
//! 跑：`cargo run --release -p yt-engine --example bench_qps [N] [dim] [queries]`
//! 默认 N=20000 span、dim=128、queries=2000。务必 `--release`（debug 慢几十倍，数字没意义）。
use std::time::Instant;
use yt_engine::{WireRecord, WriteCoordinator};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 16
    }
    fn vec(&mut self, dim: usize) -> Vec<f32> {
        (0..dim).map(|_| (self.next() % 1000) as f32 / 1000.0).collect()
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let n: usize = a.get(1).and_then(|s| s.parse().ok()).unwrap_or(20_000);
    let dim: usize = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(128);
    let queries: usize = a.get(3).and_then(|s| s.parse().ok()).unwrap_or(2_000);

    let dir = std::env::temp_dir().join(format!("yt_bench_qps_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let wc = WriteCoordinator::open_durable(&dir).unwrap();

    // 真实中文短语，拼出有词可分的文本（BM25 走全量词典分词）。
    let phrases = [
        "用户登录", "风控系统", "实时拦截", "疑似盗刷", "转账成功", "账户异常", "大模型调用", "工具超时",
        "会话重试", "余额不足", "反欺诈研判", "可疑交易", "智能体规划", "提示词注入", "调用链追踪",
    ];
    let mut rng = Rng(0x9E3779B97F4A7C15);

    // ───── 摄入吞吐（落盘 WAL + 全量词典 BM25 索引）─────
    let t = Instant::now();
    let batch = 256;
    let mut i = 0usize;
    while i < n {
        let mut recs = Vec::with_capacity(batch);
        for _ in 0..batch.min(n - i) {
            let text = format!(
                "{} {} {}",
                phrases[rng.next() as usize % phrases.len()],
                phrases[rng.next() as usize % phrases.len()],
                phrases[rng.next() as usize % phrases.len()],
            );
            recs.push(WireRecord {
                trace_id: i as u64,
                span_id: 1,
                ts: i as i64,
                seq: 1,
                event_type_tag: 2, // SpanEnd
                ext_span_id: format!("{i}-1"),
                parent_span_id: None,
                status: Some(0),
                duration_ns: Some(100),
                input_tokens: Some(10),
                output_tokens: Some(20),
                session_id: None,
                tenant_id: None,
                agent_name: Some("风控".into()),
                tool_name: None,
                model: None,
                input_text: None,
                output_text: Some(text),
                logs: vec![],
            });
            i += 1;
        }
        wc.ingest_wire(recs);
    }
    wc.flush_memtable();
    let ingest_s = t.elapsed().as_secs_f64();

    // ───── 向量建图（磁盘多层 HNSW，每点一条 dim 维向量）─────
    let tv = Instant::now();
    for id in 0..n {
        wc.index_embedding(id as u64, 1, rng.vec(dim));
    }
    wc.flush_memtable(); // 触发 graph.flush（上层图快照 + fsync）
    let vbuild_s = tv.elapsed().as_secs_f64();

    let snap = wc.pin_snapshot();

    // ───── BM25 检索 QPS（中文全量词典分词 + 倒排 + 候选折叠）─────
    let tb = Instant::now();
    let mut bm_hits = 0usize;
    for _ in 0..queries {
        let q = phrases[rng.next() as usize % phrases.len()];
        bm_hits += wc.search_text(&snap, q, 10).len();
    }
    let bm_s = tb.elapsed().as_secs_f64();

    // ───── 向量检索 QPS（磁盘 HNSW 顶层下沉 + 底层 beam + 按需读向量）─────
    let ts = Instant::now();
    let mut v_hits = 0usize;
    for _ in 0..queries {
        let q = rng.vec(dim);
        v_hits += wc.search_similar(&snap, &q, 10).len();
    }
    let vs_s = ts.elapsed().as_secs_f64();

    println!("\n=== 真实 QPS 基准（落盘引擎 / 全量词典 BM25 / 磁盘多层 HNSW）===");
    println!("规模: {n} span, 向量 {dim} 维, 各 {queries} 次查询  | 本机单机, release");
    println!("---");
    println!("摄入吞吐:   {:>10.0} span/s   ({n} span / {:.2}s, 含 WAL 落盘 + BM25 全量词典索引)", n as f64 / ingest_s, ingest_s);
    println!("向量建图:   {:>10.0} 点/s     ({n} 点 / {:.2}s, 磁盘多层 HNSW 插入)", n as f64 / vbuild_s, vbuild_s);
    println!("BM25 QPS:   {:>10.0} q/s      (avg {:.3} ms/q, 命中 {} )", queries as f64 / bm_s, bm_s * 1000.0 / queries as f64, bm_hits);
    println!("向量 QPS:   {:>10.0} q/s      (avg {:.3} ms/q, 命中 {} )", queries as f64 / vs_s, vs_s * 1000.0 / queries as f64, v_hits);

    let _ = std::fs::remove_dir_all(&dir);
}
