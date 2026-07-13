use std::collections::{HashMap, HashSet};

use crate::{Bm25Index, Bm25TextIndex, ChineseTokenizer};

pub type SearchDocId = (u64, u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RelevanceJudgment {
    pub doc: SearchDocId,
    /// 0 表示不相关；1～3 表示相关程度逐步提高。
    pub grade: u8,
}

#[derive(Clone, Debug)]
pub struct RetrievalCase {
    pub name: String,
    pub query: String,
    pub k: usize,
    pub judgments: Vec<RelevanceJudgment>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RetrievalMetrics {
    pub recall_at_k: f64,
    pub mrr_at_k: f64,
    pub ndcg_at_k: f64,
}

#[derive(Clone, Debug)]
pub struct RetrievalCaseResult {
    pub name: String,
    pub query: String,
    pub k: usize,
    pub returned: usize,
    pub relevant: usize,
    pub metrics: RetrievalMetrics,
}

#[derive(Clone, Debug, Default)]
pub struct RetrievalEvalReport {
    pub cases: Vec<RetrievalCaseResult>,
    pub macro_recall_at_k: f64,
    pub macro_mrr_at_k: f64,
    pub macro_ndcg_at_k: f64,
}

/// 用固定相关性标签计算检索指标。重复结果只计一次，避免重复 doc 抬高 Recall 或 NDCG。
pub fn retrieval_metrics(
    ranked: &[SearchDocId],
    judgments: &[RelevanceJudgment],
    k: usize,
) -> RetrievalMetrics {
    if k == 0 {
        return RetrievalMetrics::default();
    }
    let mut grades = HashMap::new();
    for judgment in judgments.iter().filter(|judgment| judgment.grade > 0) {
        grades
            .entry(judgment.doc)
            .and_modify(|grade: &mut u8| *grade = (*grade).max(judgment.grade))
            .or_insert(judgment.grade);
    }
    if grades.is_empty() {
        return RetrievalMetrics::default();
    }

    let mut seen = HashSet::new();
    let unique_ranked = ranked
        .iter()
        .copied()
        .filter(|doc| seen.insert(*doc))
        .take(k)
        .collect::<Vec<_>>();
    let relevant_hits = unique_ranked
        .iter()
        .filter(|doc| grades.contains_key(doc))
        .count();
    let recall_at_k = relevant_hits as f64 / grades.len() as f64;
    let mrr_at_k = unique_ranked
        .iter()
        .position(|doc| grades.contains_key(doc))
        .map_or(0.0, |rank| 1.0 / (rank + 1) as f64);

    let gain = |grade: u8| 2f64.powi(grade as i32) - 1.0;
    let discount = |rank: usize| ((rank + 2) as f64).log2();
    let dcg = unique_ranked
        .iter()
        .enumerate()
        .map(|(rank, doc)| gain(grades.get(doc).copied().unwrap_or(0)) / discount(rank))
        .sum::<f64>();
    let mut ideal_grades = grades.values().copied().collect::<Vec<_>>();
    ideal_grades.sort_unstable_by(|left, right| right.cmp(left));
    let idcg = ideal_grades
        .into_iter()
        .take(k)
        .enumerate()
        .map(|(rank, grade)| gain(grade) / discount(rank))
        .sum::<f64>();

    RetrievalMetrics {
        recall_at_k,
        mrr_at_k,
        ndcg_at_k: if idcg > 0.0 { dcg / idcg } else { 0.0 },
    }
}

pub fn evaluate_retrieval(
    cases: &[RetrievalCase],
    mut search: impl FnMut(&str, usize) -> Vec<SearchDocId>,
) -> RetrievalEvalReport {
    let results = cases
        .iter()
        .map(|case| {
            let ranked = search(&case.query, case.k);
            let relevant = case
                .judgments
                .iter()
                .filter(|judgment| judgment.grade > 0)
                .map(|judgment| judgment.doc)
                .collect::<HashSet<_>>()
                .len();
            RetrievalCaseResult {
                name: case.name.clone(),
                query: case.query.clone(),
                k: case.k,
                returned: ranked.len(),
                relevant,
                metrics: retrieval_metrics(&ranked, &case.judgments, case.k),
            }
        })
        .collect::<Vec<_>>();
    let count = results.len().max(1) as f64;
    RetrievalEvalReport {
        macro_recall_at_k: results
            .iter()
            .map(|case| case.metrics.recall_at_k)
            .sum::<f64>()
            / count,
        macro_mrr_at_k: results
            .iter()
            .map(|case| case.metrics.mrr_at_k)
            .sum::<f64>()
            / count,
        macro_ndcg_at_k: results
            .iter()
            .map(|case| case.metrics.ndcg_at_k)
            .sum::<f64>()
            / count,
        cases: results,
    }
}

/// 固定的 Agent trace 检索质量集。它不代替真实用户标注，但能在分词、字段拼接或排序
/// 改动后稳定检出中文、英文技术词和长尾场景的明显退步。
pub fn run_search_quality_harness() -> RetrievalEvalReport {
    let index = Bm25TextIndex::with_tokenizer(Box::new(ChineseTokenizer::full()));
    let docs = [
        (1, "交易风控系统发现同设备多卡支付，疑似盗刷，需要人工复核"),
        (2, "登录风险检测发现异地登录，但交易行为正常"),
        (3, "支付风控检查完成，风险证据已按交易和设备归类"),
        (4, "Node 应用启动失败，darwin arm64 native binding 无法加载"),
        (5, "Windows x64 平台包安装成功，ESM CJS native binding 均可加载"),
        (6, "恢复失败，manifest 快照版本与 WAL 水位不一致"),
        (7, "重启后从写前日志恢复，快照版本校验通过"),
        (8, "找到相似历史问题，沿用成功处理路径后完成任务"),
        (9, "没有可信历史案例，需要转人工处理"),
        (10, "候选提示词准确率提高，token 成本下降"),
        (11, "评测输出格式错误，无法比较提示词准确率"),
        (12, "数据库查询耗时下降，但 token 消耗没有变化"),
    ];
    for (trace_id, text) in docs {
        index.index_text(trace_id, 1, text);
    }

    let judgment = |trace_id, grade| RelevanceJudgment {
        doc: (trace_id, 1),
        grade,
    };
    let cases = vec![
        RetrievalCase {
            name: "fraud-review".to_string(),
            query: "疑似盗刷 人工复核".to_string(),
            k: 3,
            judgments: vec![judgment(1, 3)],
        },
        RetrievalCase {
            name: "risk-evidence".to_string(),
            query: "支付风控 交易设备证据".to_string(),
            k: 3,
            judgments: vec![judgment(3, 3), judgment(1, 2)],
        },
        RetrievalCase {
            name: "native-binding".to_string(),
            query: "darwin arm64 native binding 加载失败".to_string(),
            k: 3,
            judgments: vec![judgment(4, 3), judgment(5, 1)],
        },
        RetrievalCase {
            name: "wal-recovery".to_string(),
            query: "WAL 水位 快照版本 恢复".to_string(),
            k: 3,
            judgments: vec![judgment(6, 3), judgment(7, 2)],
        },
        RetrievalCase {
            name: "historical-path".to_string(),
            query: "相似历史问题 成功处理路径".to_string(),
            k: 3,
            judgments: vec![judgment(8, 3)],
        },
        RetrievalCase {
            name: "eval-cost".to_string(),
            query: "提示词 准确率 token 成本下降".to_string(),
            k: 3,
            judgments: vec![judgment(10, 3), judgment(11, 1), judgment(12, 1)],
        },
    ];

    evaluate_retrieval(&cases, |query, k| {
        index
            .search(query, k)
            .into_iter()
            .map(|(trace_id, span_id, _)| (trace_id, span_id))
            .collect()
    })
}

pub fn print_search_quality_report(report: &RetrievalEvalReport) {
    println!("\n=== Search quality eval ===");
    for case in &report.cases {
        println!(
            "{} recall@{}={:.3} mrr@{}={:.3} ndcg@{}={:.3}",
            case.name,
            case.k,
            case.metrics.recall_at_k,
            case.k,
            case.metrics.mrr_at_k,
            case.k,
            case.metrics.ndcg_at_k
        );
    }
    println!(
        "macro recall={:.3} mrr={:.3} ndcg={:.3}",
        report.macro_recall_at_k, report.macro_mrr_at_k, report.macro_ndcg_at_k
    );
}

#[cfg(test)]
mod search_quality_tests {
    use super::*;

    #[test]
    fn metrics_handle_grades_duplicates_and_cutoff() {
        let judgments = [
            RelevanceJudgment {
                doc: (1, 1),
                grade: 3,
            },
            RelevanceJudgment {
                doc: (2, 1),
                grade: 1,
            },
        ];
        let metrics = retrieval_metrics(&[(9, 1), (1, 1), (1, 1), (2, 1)], &judgments, 3);
        assert_eq!(metrics.recall_at_k, 1.0);
        assert_eq!(metrics.mrr_at_k, 0.5);
        assert!(metrics.ndcg_at_k > 0.6 && metrics.ndcg_at_k < 1.0);
    }

    #[test]
    fn metrics_are_zero_without_labels_or_budget() {
        assert_eq!(retrieval_metrics(&[(1, 1)], &[], 10).recall_at_k, 0.0);
        assert_eq!(
            retrieval_metrics(
                &[(1, 1)],
                &[RelevanceJudgment {
                    doc: (1, 1),
                    grade: 3
                }],
                0
            )
            .ndcg_at_k,
            0.0
        );
    }
}
