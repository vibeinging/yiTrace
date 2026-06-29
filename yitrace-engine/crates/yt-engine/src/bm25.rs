//! bm25.rs —— **真的 BM25 中文倒排索引**（替掉 `InMemoryBm25` 的子串匹配占位），验证自研路线里
//! "原生中文检索" 这条差异化能不能立住。
//!
//! 三件事是真的（不是占位）：
//! 1. **分词可替换**：分词从索引里解耦成 [`Tokenizer`] 接缝。默认 [`CjkBigramTokenizer`]（无词典 CJK
//!    bigram，零依赖 std-only，验证级正路）；接团队 jieba 词级分词 = 实现一个 `Tokenizer` 注入进来，
//!    **索引/评分这套自有逻辑一行不动**。这是「FFI 复用分词、自有倒排」分工的落点。
//! 2. **BM25 打分**：真倒排（token → 每文档词频）+ idf + 文档长度归一。按相关性排序，不是子串"有/无"。
//! 3. **bigram 召回正确**：相邻汉字两两成词（"疑似盗刷" → 疑似/似盗/盗刷），是 Elasticsearch CJK
//!    analyzer 同款；接 jieba 是把词切得更准的**升级**，不是召回前置（bigram 已能正确召回+排序）。
//!
//! 为什么这比子串强（模块自带会失败的测试证明）：查 "盗刷风控" 这种**非连续多概念**中文串，子串占位
//! （`InMemoryBm25` 按空白切，整串当一个 token）要求文档里出现连续 "盗刷风控" 才命中 → 一条都召不回；
//! BM25 按 bigram 把它拆成 盗刷/刷风/风控，命中"盗刷"和"风控"两概念的文档排第一，按 tf-idf 给出相关性序。
#![allow(dead_code)]

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::sync::Mutex;

use crate::Bm25Index;

const K1: f32 = 1.5;
const B: f32 = 0.75;

/// f32 全序包装（NaN 也定序），WAND 的 top-k 阈值堆用。
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

/// **分词接缝**：把一段文本切成检索词。索引与评分对分词只认这个 trait —— 换分词器（bigram → 团队 jieba
/// 词级）只换实现、不动倒排逻辑。实现方负责大小写归一、标点处理等；返回的每个 token 原样进倒排。
pub trait Tokenizer: Send + Sync {
    fn tokenize(&self, text: &str) -> Vec<String>;
}

/// 默认分词器：无词典 CJK bigram + ASCII/数字按串小写化。零依赖、std-only，接 jieba 前的验证级正路。
#[derive(Default)]
pub struct CjkBigramTokenizer;

impl Tokenizer for CjkBigramTokenizer {
    fn tokenize(&self, text: &str) -> Vec<String> {
        tokenize(text)
    }
}

/// CJK 统一表意文字主区（验证够用；扩展区/标点另算）。
fn is_cjk(c: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&c)
}

/// 一串连续汉字 → 相邻 bigram；单字则保留单字。
fn push_cjk_bigrams(run: &[char], out: &mut Vec<String>) {
    match run.len() {
        0 => {}
        1 => out.push(run[0].to_string()),
        _ => {
            for w in run.windows(2) {
                out.push(w.iter().collect());
            }
        }
    }
}

/// 分词：连续汉字走 bigram，ASCII/数字按串成词并小写化，其余字符当分隔。
pub fn tokenize(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cjk: Vec<char> = Vec::new();
    let mut ascii = String::new();
    for c in text.chars() {
        if is_cjk(c) {
            if !ascii.is_empty() {
                out.push(std::mem::take(&mut ascii).to_lowercase());
            }
            cjk.push(c);
        } else if c.is_alphanumeric() {
            if !cjk.is_empty() {
                push_cjk_bigrams(&cjk, &mut out);
                cjk.clear();
            }
            ascii.push(c);
        } else {
            if !ascii.is_empty() {
                out.push(std::mem::take(&mut ascii).to_lowercase());
            }
            if !cjk.is_empty() {
                push_cjk_bigrams(&cjk, &mut out);
                cjk.clear();
            }
        }
    }
    if !ascii.is_empty() {
        out.push(ascii.to_lowercase());
    }
    push_cjk_bigrams(&cjk, &mut out);
    out
}

#[derive(Default)]
struct Bm25State {
    /// token → (文档 → 词频)。增量建图用（HashMap 插入快）。
    postings: HashMap<String, HashMap<(u64, u64), u32>>,
    /// 文档 → 词数（BM25 长度归一用）。
    doc_len: HashMap<(u64, u64), u32>,
    /// 所有文档词数之和（算 avgdl）。
    total_len: u64,
    /// WAND 用：token → 分块的有序 postings（脏时从 `postings` 重建、查询期缓存，build-then-query 摊销）。
    sorted: HashMap<String, Postings>,
    dirty: bool,
}

/// 每块 128 篇文档，存 max_tf/min_dl 算块上界（block-max-WAND：块上界 < 阈值 → 整块跳）。
const BLOCK_SIZE: usize = 128;

/// 一个 token 的分块有序 postings。
struct Postings {
    docs: Vec<((u64, u64), u32)>, // 按 doc 升序
    blocks: Vec<BlockMeta>,
}
struct BlockMeta {
    end: usize,  // 该块覆盖 docs[start..end]（end 为下个块起点）
    max_tf: u32, // 块内最大词频
    min_dl: u32, // 块内最短文档长度（norm 在 tf 大、dl 小时最大 → (max_tf,min_dl) 给块上界）
}

impl Bm25State {
    /// 脏了就重建分块有序 postings（DAAT 要求 cursor 按 doc 推进；block-max 要每块的 max_tf/min_dl）。
    fn ensure_sorted(&mut self) {
        if !self.dirty {
            return;
        }
        self.sorted.clear();
        for (tok, plist) in &self.postings {
            let mut docs: Vec<((u64, u64), u32)> = plist.iter().map(|(&d, &tf)| (d, tf)).collect();
            docs.sort_unstable_by_key(|&(d, _)| d);
            let mut blocks = Vec::new();
            let mut i = 0;
            while i < docs.len() {
                let end = (i + BLOCK_SIZE).min(docs.len());
                let mut max_tf = 0u32;
                let mut min_dl = u32::MAX;
                for &(d, tf) in &docs[i..end] {
                    max_tf = max_tf.max(tf);
                    min_dl = min_dl.min(self.doc_len[&d]);
                }
                blocks.push(BlockMeta { end, max_tf, min_dl });
                i = end;
            }
            self.sorted.insert(tok.clone(), Postings { docs, blocks });
        }
        self.dirty = false;
    }
}

/// BM25 词频长度归一（tf·(k1+1) / (tf + k1·(1-b+b·dl/avgdl))）。上确界 = k1+1（tf→∞）。
fn bm25_norm(tf: f32, dl: f32, avgdl: f32) -> f32 {
    tf * (K1 + 1.0) / (tf + K1 * (1.0 - B + B * dl / avgdl))
}

/// 真 BM25 中文倒排索引。实现引擎的 `Bm25Index` trait，可直接替掉 `InMemoryBm25`。
/// 分词器可注入：`new()` 用默认 bigram，`with_tokenizer` 换团队 jieba（同一套倒排/评分）。
pub struct Bm25TextIndex {
    state: Mutex<Bm25State>,
    tokenizer: Box<dyn Tokenizer>,
}

impl Default for Bm25TextIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl Bm25TextIndex {
    /// 默认 bigram 分词。
    pub fn new() -> Self {
        Self::with_tokenizer(Box::new(CjkBigramTokenizer))
    }

    /// 注入自定义分词器（如团队 jieba 词级分词的 FFI 实现）。倒排与 BM25 评分不变。
    pub fn with_tokenizer(tokenizer: Box<dyn Tokenizer>) -> Self {
        Self { state: Mutex::new(Bm25State::default()), tokenizer }
    }
}

impl Bm25Index for Bm25TextIndex {
    fn index_text(&self, trace_id: u64, span_id: u64, text: &str) {
        let toks = self.tokenizer.tokenize(text);
        if toks.is_empty() {
            return;
        }
        let mut st = self.state.lock().unwrap();
        let doc = (trace_id, span_id);
        st.total_len += toks.len() as u64;
        *st.doc_len.entry(doc).or_insert(0) += toks.len() as u32;
        for t in toks {
            *st.postings.entry(t).or_default().entry(doc).or_insert(0) += 1;
        }
        st.dirty = true;
    }

    /// **block-max-WAND**（DAAT + 上界剪枝 + 块跳过）。剪枝用**严格 `<` 阈值**（θ = 当前第 k 高分）：
    /// 被剪文档/块的上界严格小于 θ → 实际分严格小于最终第 k 高分 → 绝不进 top-k（含同分边界安全）。
    /// 候选全量打分后**终排（分降序、(trace,span) 升序）取 top-k**，与暴力逐位一致（有测试钉死）。
    /// 单词查询走块跳过（块上界 = idf·norm(max_tf,min_dl) < θ → 整块跳）；多词查询走 term 级 WAND（剪掉只命中弱词的文档）。
    fn search(&self, query: &str, k: usize) -> Vec<(u64, u64, f32)> {
        let mut st = self.state.lock().unwrap();
        let n = st.doc_len.len();
        if n == 0 || k == 0 {
            return Vec::new();
        }
        st.ensure_sorted();
        let avgdl = st.total_len as f32 / n as f32;

        // 查询词去重 + 排序（确定性求和顺序：按 token 序加各词贡献，与暴力一致）。
        let mut toks: Vec<String> = self.tokenizer.tokenize(query);
        toks.sort_unstable();
        toks.dedup();

        // 命中词：(idf, &分块postings)。
        let mut hits: Vec<(f32, &Postings)> = Vec::new();
        for tok in &toks {
            if let Some(pp) = st.sorted.get(tok) {
                let df = pp.docs.len() as f32;
                let idf = (1.0 + (n as f32 - df + 0.5) / (df + 0.5)).ln();
                hits.push((idf, pp));
            }
        }
        if hits.is_empty() {
            return Vec::new();
        }

        let mut topk: BinaryHeap<Reverse<OrdF32>> = BinaryHeap::new();
        let mut scored: Vec<(u64, u64, f32)> = Vec::new();
        let theta = |h: &BinaryHeap<Reverse<OrdF32>>| if h.len() >= k { h.peek().unwrap().0 .0 } else { f32::NEG_INFINITY };

        if hits.len() == 1 {
            // 单词：block-max 块跳过。块上界 < θ → 整块不打分。
            let (idf, pp) = hits[0];
            let mut i = 0usize;
            for blk in &pp.blocks {
                let bmax = idf * bm25_norm(blk.max_tf as f32, blk.min_dl as f32, avgdl);
                if bmax < theta(&topk) {
                    i = blk.end;
                    continue; // 整块跳
                }
                for &(doc, tf) in &pp.docs[i..blk.end] {
                    let dl = st.doc_len[&doc] as f32;
                    let sc = idf * bm25_norm(tf as f32, dl, avgdl);
                    scored.push((doc.0, doc.1, sc));
                    topk.push(Reverse(OrdF32(sc)));
                    if topk.len() > k {
                        topk.pop();
                    }
                }
                i = blk.end;
            }
        } else {
            // 多词：term 级 WAND（DAAT，上界 = idf·(k1+1)，按 doc 序选 pivot、剪枝）。
            struct Cur<'a> {
                docs: &'a [((u64, u64), u32)],
                idf: f32,
                maxi: f32,
                pos: usize,
            }
            let mut curs: Vec<Cur> =
                hits.iter().map(|&(idf, pp)| Cur { docs: &pp.docs, idf, maxi: idf * (K1 + 1.0), pos: 0 }).collect();
            loop {
                curs.retain(|c| c.pos < c.docs.len());
                if curs.is_empty() {
                    break;
                }
                let mut order: Vec<usize> = (0..curs.len()).collect();
                order.sort_by_key(|&i| curs[i].docs[curs[i].pos].0);
                let th = theta(&topk);
                let mut acc = 0.0f32;
                let mut pivot: Option<usize> = None;
                for (oi, &ci) in order.iter().enumerate() {
                    acc += curs[ci].maxi;
                    if acc >= th {
                        pivot = Some(oi);
                        break;
                    }
                }
                let Some(poi) = pivot else { break };
                let pivot_doc = curs[order[poi]].docs[curs[order[poi]].pos].0;
                let first_doc = curs[order[0]].docs[curs[order[0]].pos].0;
                if first_doc == pivot_doc {
                    let mut sc = 0.0f32;
                    let dl = st.doc_len[&pivot_doc] as f32;
                    for c in curs.iter() {
                        if c.pos < c.docs.len() && c.docs[c.pos].0 == pivot_doc {
                            sc += c.idf * bm25_norm(c.docs[c.pos].1 as f32, dl, avgdl);
                        }
                    }
                    scored.push((pivot_doc.0, pivot_doc.1, sc));
                    topk.push(Reverse(OrdF32(sc)));
                    if topk.len() > k {
                        topk.pop();
                    }
                    for c in curs.iter_mut() {
                        if c.pos < c.docs.len() && c.docs[c.pos].0 == pivot_doc {
                            c.pos += 1;
                        }
                    }
                } else {
                    for &ci in order.iter().take(poi + 1) {
                        if curs[ci].docs[curs[ci].pos].0 < pivot_doc {
                            let c = &mut curs[ci];
                            while c.pos < c.docs.len() && c.docs[c.pos].0 < pivot_doc {
                                c.pos += 1;
                            }
                            break;
                        }
                    }
                }
            }
        }

        // 候选全量打分完 → 终排取 top-k（与暴力一致）。
        scored.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap().then((a.0, a.1).cmp(&(b.0, b.1))));
        scored.truncate(k);
        scored
    }
}

impl Bm25TextIndex {
    /// 暴力全量打分（**测试基准**）：对命中词的所有文档逐一打分（无剪枝），求和顺序与 WAND 一致
    /// （按 token 序：外层 token、内层 doc 累加）。WAND 的结果必须与它逐位一致。
    #[cfg(test)]
    fn search_exhaustive(&self, query: &str, k: usize) -> Vec<(u64, u64, f32)> {
        let mut st = self.state.lock().unwrap();
        let n = st.doc_len.len();
        if n == 0 || k == 0 {
            return Vec::new();
        }
        st.ensure_sorted();
        let avgdl = st.total_len as f32 / n as f32;
        let mut toks: Vec<String> = self.tokenizer.tokenize(query);
        toks.sort_unstable();
        toks.dedup();
        let mut scores: HashMap<(u64, u64), f32> = HashMap::new();
        for tok in &toks {
            if let Some(pp) = st.sorted.get(tok) {
                let df = pp.docs.len() as f32;
                let idf = (1.0 + (n as f32 - df + 0.5) / (df + 0.5)).ln();
                for &(doc, tf) in &pp.docs {
                    let dl = st.doc_len[&doc] as f32;
                    *scores.entry(doc).or_insert(0.0) += idf * bm25_norm(tf as f32, dl, avgdl);
                }
            }
        }
        let mut scored: Vec<(u64, u64, f32)> = scores.into_iter().map(|((t, s), sc)| (t, s, sc)).collect();
        scored.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap().then((a.0, a.1).cmp(&(b.0, b.1))));
        scored.truncate(k);
        scored
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InMemoryBm25;

    #[test]
    fn cjk_bigram_tokenizer() {
        assert_eq!(tokenize("疑似盗刷"), vec!["疑似", "似盗", "盗刷"]);
        assert_eq!(tokenize("好"), vec!["好"]); // 单字保留
        // 中英混排 + 标点切分
        assert_eq!(tokenize("风控 GPT4"), vec!["风控", "gpt4"]);
        assert_eq!(tokenize("盗刷,转账"), vec!["盗刷", "转账"]);
    }

    #[test]
    fn bm25_ranks_by_relevance_where_substring_returns_nothing() {
        // 验证核心:非连续多概念中文查询,真 BM25 能召回并排序,子串占位一条都召不回。
        let bm = Bm25TextIndex::new();
        bm.index_text(1, 1, "风控系统实时拦截了一笔疑似盗刷的交易"); // 含 盗刷 + 风控 两概念
        bm.index_text(2, 2, "用户正常登录并完成转账"); // 都不含
        bm.index_text(3, 3, "这是一笔疑似盗刷"); // 只含 盗刷
        bm.index_text(4, 4, "风控规则已更新"); // 只含 风控

        // 查 "盗刷风控"(非连续):bigram = 盗刷/刷风/风控。
        let hits = bm.search("盗刷风控", 10);
        assert_eq!((hits[0].0, hits[0].1), (1, 1), "两概念都命中的文档排第一");
        // (2,2) 都不含 → 不出现。
        assert!(!hits.iter().any(|&(t, _, _)| t == 2), "无关文档不召回");
        // 只含单概念的 (3,3)/(4,4) 排在后面、分更低。
        assert!(hits[0].2 > hits[1].2, "多概念命中分更高");

        // 对照:子串占位查同一串 —— 没有文档含连续"盗刷风控" → 召回为空。
        let sub = InMemoryBm25::default();
        sub.index_text(1, 1, "风控系统实时拦截了一笔疑似盗刷的交易");
        sub.index_text(3, 3, "这是一笔疑似盗刷");
        sub.index_text(4, 4, "风控规则已更新");
        assert!(sub.search("盗刷风控", 10).is_empty(), "子串匹配召不回非连续多概念查询");
    }

    #[test]
    fn bm25_term_frequency_and_length_norm() {
        // 同一查询词,词频高的文档排前(且长度归一:短文档同 tf 占便宜)。
        let bm = Bm25TextIndex::new();
        bm.index_text(1, 1, "盗刷盗刷盗刷"); // 盗刷 出现多次,文档短
        bm.index_text(2, 2, "盗刷 以及一大段无关的正常交易日志内容填充长度"); // 一次,文档长
        let hits = bm.search("盗刷", 10);
        assert_eq!((hits[0].0, hits[0].1), (1, 1), "高词频+短文档排第一");
        assert_eq!(hits.len(), 2);
        assert!(hits[0].2 > hits[1].2);
    }

    #[test]
    fn wand_matches_exhaustive_on_random_corpus() {
        // WAND 必须与暴力全量打分**逐位一致**（剪枝只跳掉绝不进 top-k 的文档）。
        // 随机语料 + 多词查询，扫多个 k 对比。确定性 LCG，不依赖 rand。
        let words = ["盗刷", "风控", "交易", "转账", "登录", "异常", "拦截", "模型", "会话", "超时"];
        let mut seed = 0x1234_5678u64;
        let mut next = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) as usize
        };
        let bm = Bm25TextIndex::new();
        // 800 文档，每篇 3~7 个随机词（空格分隔，bigram 分词会再切，但稳定可复算）。
        for i in 0..800u64 {
            let len = 3 + next() % 5;
            let text: Vec<&str> = (0..len).map(|_| words[next() % words.len()]).collect();
            bm.index_text(i / 10, i, &text.join(" "));
        }
        // 多组随机多词查询 × 多个 k。
        for _ in 0..60 {
            let qlen = 1 + next() % 3;
            let q: Vec<&str> = (0..qlen).map(|_| words[next() % words.len()]).collect();
            let query = q.join(" ");
            for &k in &[1usize, 5, 10, 50] {
                let wand = bm.search(&query, k);
                let exhaustive = bm.search_exhaustive(&query, k);
                assert_eq!(wand, exhaustive, "WAND≠暴力: query={query:?} k={k}");
            }
        }
    }

    #[test]
    fn empty_index_returns_nothing() {
        let bm = Bm25TextIndex::new();
        assert!(bm.search("盗刷", 5).is_empty());
    }

    /// 接缝验证：注入一个"只认整词、不切 bigram"的分词器，索引/评分逻辑照旧走，
    /// 但召回行为随分词器改变 —— 证明换分词器（→ jieba）只换这一层。
    #[test]
    fn injected_tokenizer_changes_segmentation_only() {
        struct WordTokenizer; // 按空白切，整段中文当一个词（模拟"词级"的极端：不拆 bigram）
        impl Tokenizer for WordTokenizer {
            fn tokenize(&self, text: &str) -> Vec<String> {
                text.split_whitespace().map(|w| w.to_lowercase()).collect()
            }
        }

        let bm = Bm25TextIndex::with_tokenizer(Box::new(WordTokenizer));
        bm.index_text(1, 1, "盗刷 风控");
        bm.index_text(2, 2, "盗刷风控"); // 无空格 → 在该分词器下是一个整词

        // 查 "风控"：只有 (1,1) 把它切成独立词 → 命中；(2,2) 整串是一个词，不含 "风控" 这个 token。
        let hits = bm.search("风控", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!((hits[0].0, hits[0].1), (1, 1), "分词器决定切分，索引只认 token");

        // 同一份数据走默认 bigram：两条都把 风控 切出来 → 都召回（对照，证明只有分词层变了）。
        let bg = Bm25TextIndex::new();
        bg.index_text(1, 1, "盗刷 风控");
        bg.index_text(2, 2, "盗刷风控");
        assert_eq!(bg.search("风控", 10).len(), 2, "bigram 下两条都含 风控");
    }
}
