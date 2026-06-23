//! tokenizer_cn.rs —— **纯 Rust 中文词级分词**（jieba 默认模式等价），替掉 bigram 占位，std-only 零依赖。
//!
//! 团队 cppjieba 真库不在本环境，索性自己用 Rust 写一套生产级的。算法 = jieba 默认 `cut` 的核心两步：
//! 1. **词典 DAG**：对一段中文，列出每个起点能成的所有词典词（有向无环图）。
//! 2. **最大概率路径（DP）**：用词频把句子切成"整体概率最大"的一种分法 —— 按词频解决切词歧义，
//!    而不是 bigram 盲目两两成词。经典例子 "研究生命" → 研究/生命（不是 研究生/命），靠的就是
//!    P(研究)·P(生命) > P(研究生)·P(命)。
//!
//! **词典默认装满**：jieba 全量 `dict.txt`（34.9 万词，MIT）**嵌进二进制**（`include_str!`），
//! `ChineseTokenizer` 默认就用它——开箱即生产级，且契合"单二进制私有化部署"（不外挂词典文件）。
//! 在此之上再叠一层**领域词**（盗刷/风控/会话/超时… jieba 标准词典里没有的），金融/Agent 黑话也认。
//!
//! **自有词典导入**：[`ChineseTokenizer::with_user_dict`] 在全量词典上叠加用户词（同 jieba 格式
//! `词 频 [词性]`，可覆盖词频、可加生词），或 [`ChineseTokenizer::with_dict`] 完全自定义词典。
//!
//! **未登录词（OOV）**：仍不在词典里的字按单字切（DP 里单字概率低、自然让位给词典词）。jieba 用 HMM
//! （BMES + Viterbi）救 OOV 是下一步增强；接口已留 [`ChineseTokenizer`] 内的切分函数好挂。

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use crate::bm25::Tokenizer;

/// jieba 全量词典（MIT，fxsjy/jieba），编进二进制。每行 `词 频 [词性]`。
const EMBEDDED_JIEBA_DICT: &str = include_str!("../data/jieba_dict.txt");

/// CJK 统一表意文字主区（与 bigram 分词器同口径）。
fn is_cjk(c: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&c)
}

/// 词典：词 → 频次，加上算最大概率路径要的总频次与最长词长。
#[derive(Clone)]
pub struct Dict {
    freq: HashMap<String, u64>,
    total: u64,
    log_total: f64,
    /// 最长词的字数（DAG 扫描上界，避免每个起点试到句尾）。
    max_word_len: usize,
}

impl Dict {
    /// 空词典（退化成单字切分；一般用 `full` 或 `load_str`）。
    pub fn new() -> Self {
        Self { freq: HashMap::new(), total: 0, log_total: 0.0, max_word_len: 0 }
    }

    /// 加一个词。重复加同词**覆盖**频次（用户词典可借此提权某词）。
    pub fn add(&mut self, word: &str, freq: u64) {
        let chars = word.chars().count();
        if chars == 0 {
            return;
        }
        if let Some(old) = self.freq.insert(word.to_string(), freq) {
            self.total -= old;
        }
        self.total += freq;
        self.log_total = (self.total.max(1) as f64).ln();
        self.max_word_len = self.max_word_len.max(chars);
    }

    /// 加一个词，**仅当不存在**（叠领域词到全量词典时用，不覆盖 jieba 原有词频）。
    fn add_absent(&mut self, word: &str, freq: u64) {
        if !self.freq.contains_key(word) {
            self.add(word, freq);
        }
    }

    /// 把 jieba 格式词典文本叠进当前词典：每行 `词 频 [词性]`，空行/井号注释跳过，频次缺省 1，同词覆盖。
    pub fn extend_str(&mut self, text: &str) {
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut it = line.split_whitespace();
            let Some(word) = it.next() else { continue };
            let freq = it.next().and_then(|f| f.parse::<u64>().ok()).unwrap_or(1);
            self.add(word, freq);
        }
    }

    /// 从 jieba 格式词典文本新建词典。
    pub fn load_str(text: &str) -> Self {
        let mut d = Self::new();
        d.extend_str(text);
        d
    }

    fn contains(&self, word: &str) -> bool {
        self.freq.contains_key(word)
    }

    /// 词的对数概率 `ln(freq/total)`；未登录词按频次 1（很小，DP 里自然不优先）。
    fn logprob(&self, word: &str) -> f64 {
        let f = self.freq.get(word).copied().unwrap_or(1);
        (f as f64).ln() - self.log_total
    }

    /// 全量词典 = jieba 嵌入词典 + 叠加领域词（不覆盖 jieba 原词频）。解析一次、进程内共享。
    pub fn full() -> Arc<Dict> {
        static BASE: OnceLock<Arc<Dict>> = OnceLock::new();
        BASE.get_or_init(|| {
            let mut d = Dict::load_str(EMBEDDED_JIEBA_DICT);
            for &(w, f) in DOMAIN_DICT {
                d.add_absent(w, f);
            }
            Arc::new(d)
        })
        .clone()
    }
}

impl Default for Dict {
    fn default() -> Self {
        (*Dict::full()).clone()
    }
}

/// 纯 Rust 中文词级分词器，实现引擎的 `Tokenizer`。
/// 默认装满 jieba 全量词典：`with_tokenizer(Box::new(ChineseTokenizer::default()))`。
pub struct ChineseTokenizer {
    dict: Arc<Dict>,
}

impl ChineseTokenizer {
    /// 用 jieba 全量词典（+ 领域词），开箱即生产级。
    pub fn full() -> Self {
        Self { dict: Dict::full() }
    }

    /// 在全量词典上**叠加自有词典**（jieba 格式文本：每行 `词 频 [词性]`）。用户词覆盖词频、可加生词。
    pub fn with_user_dict(text: &str) -> Self {
        let mut d = (*Dict::full()).clone();
        d.extend_str(text);
        Self { dict: Arc::new(d) }
    }

    /// 用完全自定义词典（不含 jieba 全量；多用于测试或特殊场景）。
    pub fn with_dict(dict: Dict) -> Self {
        Self { dict: Arc::new(dict) }
    }

    /// 对一段**纯中文**做词典 DAG + 最大概率路径切分。
    fn cut(&self, sentence: &str) -> Vec<String> {
        let chars: Vec<char> = sentence.chars().collect();
        let n = chars.len();
        if n == 0 {
            return Vec::new();
        }
        // 字下标 → 字节偏移，便于直接切 &str（避免每个子串都新建 String 去查词典）。
        let mut byte_at = Vec::with_capacity(n + 1);
        let mut b = 0;
        for c in &chars {
            byte_at.push(b);
            b += c.len_utf8();
        }
        byte_at.push(b);

        // DAG[i] = 从 i 起能成的词的结束字下标（含单字 i+1 兜底，保证每点可达）。
        let dag: Vec<Vec<usize>> = (0..n)
            .map(|i| {
                let mut ends = Vec::new();
                let jmax = (i + self.dict.max_word_len.max(1)).min(n);
                for j in (i + 1)..=jmax {
                    if j == i + 1 || self.dict.contains(&sentence[byte_at[i]..byte_at[j]]) {
                        ends.push(j);
                    }
                }
                if ends.is_empty() {
                    ends.push(i + 1);
                }
                ends
            })
            .collect();

        // 从句尾倒推每个起点的最大概率后继：route[i] = (最大对数概率, 选的结束下标)。
        let mut route: Vec<(f64, usize)> = vec![(0.0, n); n + 1];
        for i in (0..n).rev() {
            let mut best = (f64::NEG_INFINITY, i + 1);
            for &j in &dag[i] {
                let lp = self.dict.logprob(&sentence[byte_at[i]..byte_at[j]]) + route[j].0;
                if lp > best.0 {
                    best = (lp, j);
                }
            }
            route[i] = best;
        }

        // 从头按 route 走，切出词。
        let mut out = Vec::new();
        let mut i = 0;
        while i < n {
            let j = route[i].1;
            out.push(sentence[byte_at[i]..byte_at[j]].to_string());
            i = j;
        }
        out
    }
}

impl Default for ChineseTokenizer {
    fn default() -> Self {
        Self::full()
    }
}

impl Tokenizer for ChineseTokenizer {
    fn tokenize(&self, text: &str) -> Vec<String> {
        // 按 中文run / ASCII数字run / 其余分隔 切块；中文 run 走词典 DP，ASCII 串小写成词。
        let mut out = Vec::new();
        let mut cjk = String::new();
        let mut ascii = String::new();
        for c in text.chars() {
            if is_cjk(c) {
                if !ascii.is_empty() {
                    out.push(std::mem::take(&mut ascii).to_lowercase());
                }
                cjk.push(c);
            } else if c.is_alphanumeric() {
                if !cjk.is_empty() {
                    out.extend(self.cut(&std::mem::take(&mut cjk)));
                }
                ascii.push(c);
            } else {
                if !ascii.is_empty() {
                    out.push(std::mem::take(&mut ascii).to_lowercase());
                }
                if !cjk.is_empty() {
                    out.extend(self.cut(&std::mem::take(&mut cjk)));
                }
            }
        }
        if !ascii.is_empty() {
            out.push(ascii.to_lowercase());
        }
        if !cjk.is_empty() {
            out.extend(self.cut(&cjk));
        }
        out
    }
}

/// 叠加在 jieba 全量词典之上的**领域词**（金融/风控/Agent 黑话，jieba 标准词典里多半没有）。
/// 用 `add_absent` 叠加：jieba 已有的词不动，只补缺。频次量级参考 jieba（相对大小决定切词倾向）。
#[rustfmt::skip]
const DOMAIN_DICT: &[(&str, u64)] = &[
    // 风控/金融
    ("风控", 8_000), ("盗刷", 4_000), ("反欺诈", 2_000), ("洗钱", 2_000), ("黑产", 1_500),
    ("拦截", 9_000), ("疑似", 7_000), ("可疑", 4_000), ("风险", 30_000), ("额度", 3_000),
    // Agent/可观测性
    ("会话", 5_000), ("超时", 4_000), ("重试", 3_000), ("智能体", 2_000), ("大模型", 3_000),
    ("提示词", 1_500), ("工具调用", 1_500), ("链路", 2_000), ("追踪", 3_000), ("埋点", 1_500),
    ("可观测", 1_000), ("调用链", 1_500),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_known_words_at_word_level() {
        let t = ChineseTokenizer::full();
        // 词级切分：四个词，不是 bigram 的 用户/户登/登录…
        assert_eq!(t.tokenize("用户登录风控系统"), vec!["用户", "登录", "风控", "系统"]);
    }

    #[test]
    fn resolves_ambiguity_by_word_frequency() {
        let t = ChineseTokenizer::full();
        // 招牌歧义：研究生命 → 研究/生命（P(研究)P(生命) > P(研究生)P(命)），bigram 给不出这种判断。
        assert_eq!(t.tokenize("研究生命"), vec!["研究", "生命"]);
    }

    #[test]
    fn mixed_cjk_ascii_and_punctuation() {
        let t = ChineseTokenizer::full();
        assert_eq!(t.tokenize("风控GPT4"), vec!["风控", "gpt4"]);
        assert_eq!(t.tokenize("盗刷,转账"), vec!["盗刷", "转账"]);
        // 整句带功能词
        assert_eq!(
            t.tokenize("风控系统实时拦截了一笔疑似盗刷的交易"),
            vec!["风控", "系统", "实时", "拦截", "了", "一笔", "疑似", "盗刷", "的", "交易"]
        );
    }

    #[test]
    fn oov_runs_fall_back_to_single_chars() {
        let t = ChineseTokenizer::full();
        // 未登录的生造词按单字切（不崩、可被 BM25 索引）。
        let toks = t.tokenize("烎槑");
        assert_eq!(toks, vec!["烎", "槑"]);
    }

    #[test]
    fn plugs_into_real_bm25_and_ranks_at_word_level() {
        use crate::{Bm25Index, Bm25TextIndex};
        // 生产分词器接进真 BM25 倒排：按词级 token 建索引、按相关性排序。
        let bm = Bm25TextIndex::with_tokenizer(Box::new(ChineseTokenizer::full()));
        bm.index_text(1, 1, "风控系统实时拦截了一笔疑似盗刷的交易"); // 含 风控 + 盗刷
        bm.index_text(2, 2, "用户登录并完成转账"); // 都不含
        bm.index_text(3, 3, "这是一笔疑似盗刷"); // 只含 盗刷
        bm.index_text(4, 4, "风控规则已更新"); // 只含 风控

        // 查 "盗刷风控" → 词级切成 盗刷/风控 两词，两概念都命中的排第一。
        let hits = bm.search("盗刷风控", 10);
        assert_eq!((hits[0].0, hits[0].1), (1, 1), "两概念都命中的文档排第一");
        assert!(!hits.iter().any(|&(t, _, _)| t == 2), "无关文档不召回");
        assert!(hits[0].2 > hits[1].2, "多概念命中分更高");
    }

    #[test]
    fn full_dict_loaded_by_default_and_domain_words_recognized() {
        let t = ChineseTokenizer::full();
        // 全量 jieba 词典里的普通词
        assert_eq!(t.tokenize("自然语言处理"), vec!["自然语言", "处理"]);
        // jieba 标准词典没有、靠领域词补的（风控/盗刷）
        assert_eq!(t.tokenize("风控盗刷"), vec!["风控", "盗刷"]);
    }

    #[test]
    fn user_dict_overlays_on_full_dict() {
        // 自有词典导入：在全量词典之上加一个 jieba 不认的专名，分词随之把它当整词。
        let t = ChineseTokenizer::with_user_dict("玄武风控引擎 100000 nz\n");
        assert_eq!(t.tokenize("玄武风控引擎拦截了请求"), vec!["玄武风控引擎", "拦截", "了", "请求"]);
        // 不加用户词时，这个生造专名会被切碎（证明确实是用户词起的作用）。
        let base = ChineseTokenizer::full();
        assert!(base.tokenize("玄武风控引擎").len() > 1);
    }

    #[test]
    fn loadable_dict_overrides_segmentation() {
        // 加载外部词典：把"自然语言处理"当一个词，切分随词典变（证明词典可插拔=能换 jieba 全量）。
        let dict = Dict::load_str("自然语言处理 100000\n# 注释行\n");
        let t = ChineseTokenizer::with_dict(dict);
        assert_eq!(t.tokenize("自然语言处理"), vec!["自然语言处理"]);
        // 内置词典没有这个长词 → 切成更短的词
        let b = ChineseTokenizer::full();
        assert!(b.tokenize("自然语言处理").len() > 1);
    }
}
