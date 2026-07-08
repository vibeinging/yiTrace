// ───────────────────────── 评测（eval 闭环） ─────────────────────────

/// 一个 scorer 对一条 span 的产出：千分制分数 + 标签。
///
/// 这是 eval 闭环的"评"那一步的结果。分数用千分制整数（保住可比/可持久化且不引入 f32 的 Eq 麻烦），
/// 展示层除以 10 得百分。label 给人看（"通过"/"未通过"/scorer 名）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalOutcome {
    pub score: u32,
    pub label: String,
}

impl EvalOutcome {
    pub fn new(score: u32, label: impl Into<String>) -> Self {
        Self {
            score: score.min(1000),
            label: label.into(),
        }
    }
}

/// scorer：看一条折叠出的 span，给个分。
///
/// 先做**不依赖 LLM 的规则 scorer**（关键词/正则/非空/无错），把"存→评→写回→读分"主链跑通；
/// LLM-judge 只是换一个 impl（异步调模型、本地小模型当裁判），闭环骨架不变。
/// 返回 None = 这条 span 不适用此 scorer（跳过，不写回）。
pub trait Scorer: Send + Sync {
    fn score(&self, span: &FoldedSpan) -> Option<EvalOutcome>;
}

/// 关键词规则 scorer：output_text 命中任一"坏词"判未通过(0)，否则通过(1000)。
/// 反洗钱/风控场景的探路用法：答案里出现"无法/抱歉/未知"等即判不合格。
pub struct KeywordScorer {
    bad_words: Vec<String>,
}

impl KeywordScorer {
    pub fn new(bad_words: &[&str]) -> Self {
        Self {
            bad_words: bad_words.iter().map(|s| s.to_string()).collect(),
        }
    }
}

impl Scorer for KeywordScorer {
    fn score(&self, span: &FoldedSpan) -> Option<EvalOutcome> {
        let text = span.output_text.as_deref()?; // 没有输出文本 → 不评
        let hit = self.bad_words.iter().any(|w| text.contains(w));
        Some(if hit {
            EvalOutcome::new(0, "未通过")
        } else {
            EvalOutcome::new(1000, "通过")
        })
    }
}

/// 一条 span 的评测记录（eval_and_writeback 的返回，便于观测/断言）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScoredSpan {
    pub trace_id: u64,
    pub span_id: u64,
    pub outcome: EvalOutcome,
}

/// 评测汇总的一行（整体一行 + 每个 agent 一行）。通过率/均分用于"哪个 agent 退步了"的回归视图。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalSummary {
    /// None = 整体；Some(name) = 该 agent。
    pub agent_name: Option<String>,
    /// 有分的 span 数（无 eval_score 的不计）。
    pub scored_spans: usize,
    /// 分数 ≥ 阈值的 span 数（通过）。
    pub pass_count: usize,
    /// 千分制平均分（scored_spans=0 时为 0）。
    pub avg_score: u32,
}

impl EvalSummary {
    /// 通过率（0.0..=1.0）。无打分 span 时为 0。
    pub fn pass_rate(&self) -> f32 {
        if self.scored_spans == 0 {
            0.0
        } else {
            self.pass_count as f32 / self.scored_spans as f32
        }
    }
}

/// 把一串 (可选 agent 名, 千分制分数) 聚合成评测看板：第 0 行恒为整体，其后按 agent 名升序。
/// `eval_summary`（线上已打分的 span）和 `eval_dataset`（对数据集现跑 scorer）共用这套口径。
fn aggregate_eval(
    scored: impl Iterator<Item = (Option<String>, u32)>,
    pass_threshold: u32,
) -> Vec<EvalSummary> {
    let mut overall = (0usize, 0usize, 0u64);
    let mut by_agent: BTreeMap<String, (usize, usize, u64)> = BTreeMap::new();
    for (agent, score) in scored {
        let pass = (score >= pass_threshold) as usize;
        overall.0 += 1;
        overall.1 += pass;
        overall.2 += score as u64;
        if let Some(a) = agent {
            let e = by_agent.entry(a).or_default();
            e.0 += 1;
            e.1 += pass;
            e.2 += score as u64;
        }
    }
    let mk = |agent_name: Option<String>, (scored, pass, sum): (usize, usize, u64)| EvalSummary {
        agent_name,
        scored_spans: scored,
        pass_count: pass,
        avg_score: if scored == 0 {
            0
        } else {
            (sum / scored as u64) as u32
        },
    };
    let mut out = vec![mk(None, overall)]; // 第 0 行恒为整体
    for (agent, acc) in by_agent {
        out.push(mk(Some(agent), acc));
    }
    out
}
