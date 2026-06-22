//! bm25.rs —— **真的 BM25 中文倒排索引**（替掉 `InMemoryBm25` 的子串匹配占位），验证自研路线里
//! "原生中文检索" 这条差异化能不能立住。
//!
//! 两件事是真的（不是占位）：
//! 1. **中文分词** = 无词典的 **CJK bigram**（相邻汉字两两成词，"疑似盗刷" → 疑似/似盗/盗刷）。
//!    这是 Elasticsearch CJK analyzer 同款做法，**零词典、std-only**，是验证级正路；接 jieba 词级分词是
//!    升级、不是前置（bigram 已能正确召回+排序，jieba 只是把词切得更准）。ASCII/数字按空白与标点切词、小写化。
//! 2. **BM25 打分**：真倒排（token → 每文档词频）+ idf + 文档长度归一。按相关性排序，不是子串"有/无"。
//!
//! 为什么这比子串强（模块自带会失败的测试证明）：查 "盗刷风控" 这种**非连续多概念**中文串，子串占位
//! （`InMemoryBm25` 按空白切，整串当一个 token）要求文档里出现连续 "盗刷风控" 才命中 → 一条都召不回；
//! BM25 按 bigram 把它拆成 盗刷/刷风/风控，命中"盗刷"和"风控"两概念的文档排第一，按 tf-idf 给出相关性序。
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Mutex;

use crate::Bm25Index;

const K1: f32 = 1.5;
const B: f32 = 0.75;

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
    /// token → (文档 → 词频)。
    postings: HashMap<String, HashMap<(u64, u64), u32>>,
    /// 文档 → 词数（BM25 长度归一用）。
    doc_len: HashMap<(u64, u64), u32>,
    /// 所有文档词数之和（算 avgdl）。
    total_len: u64,
}

/// 真 BM25 中文倒排索引。实现引擎的 `Bm25Index` trait，可直接替掉 `InMemoryBm25`。
#[derive(Default)]
pub struct Bm25TextIndex {
    state: Mutex<Bm25State>,
}

impl Bm25TextIndex {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Bm25Index for Bm25TextIndex {
    fn index_text(&self, trace_id: u64, span_id: u64, text: &str) {
        let toks = tokenize(text);
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
    }

    fn search(&self, query: &str, k: usize) -> Vec<(u64, u64, f32)> {
        let st = self.state.lock().unwrap();
        let n = st.doc_len.len();
        if n == 0 {
            return Vec::new();
        }
        let avgdl = st.total_len as f32 / n as f32;

        let mut scores: HashMap<(u64, u64), f32> = HashMap::new();
        // 查询词去重（同一 token 重复不重复加 idf）。
        let mut seen = std::collections::HashSet::new();
        for tok in tokenize(query) {
            if !seen.insert(tok.clone()) {
                continue;
            }
            let Some(plist) = st.postings.get(&tok) else { continue };
            let df = plist.len() as f32;
            // idf = ln(1 + (N - df + 0.5)/(df + 0.5))，恒正（BM25+ 形式）。
            let idf = (1.0 + (n as f32 - df + 0.5) / (df + 0.5)).ln();
            for (&doc, &tf) in plist {
                let dl = st.doc_len[&doc] as f32;
                let tf = tf as f32;
                let norm = tf * (K1 + 1.0) / (tf + K1 * (1.0 - B + B * dl / avgdl));
                *scores.entry(doc).or_insert(0.0) += idf * norm;
            }
        }

        let mut scored: Vec<(u64, u64, f32)> = scores.into_iter().map(|((t, s), sc)| (t, s, sc)).collect();
        // 分降序；同分按 (trace,span) 升序定序（确定可复算）。
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
    fn empty_index_returns_nothing() {
        let bm = Bm25TextIndex::new();
        assert!(bm.search("盗刷", 5).is_empty());
    }
}
