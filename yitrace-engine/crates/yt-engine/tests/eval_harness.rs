//! eval 测试框架的集成测试：用 evalkit 自造多场景数据、真实摄入、跑 eval 闭环，断言不变量。
//! 验证「框架真把数据灌进去了、eval 真还原了注入的失败、回归机制真能检出退步」。

use std::sync::Arc;

use yt_engine::evalkit;
use yt_engine::{InMemorySegmentStore, TraceQuery, WriteCoordinator};

fn fresh() -> Arc<WriteCoordinator> {
    WriteCoordinator::new(Arc::new(InMemorySegmentStore::default()))
}

/// eval 应当**精确还原**每个场景注入的失败数：
/// 每条 trace 恰有一个被打分的 answer span，通过数 = trace 数 − 注入失败数。
#[test]
fn eval_recovers_injected_failures_per_scenario() {
    let coord = fresh();
    let report = evalkit::run_harness(&coord, 30, 7);
    assert_eq!(report.scenarios.len(), 4);
    for s in &report.scenarios {
        let overall = &s.summary[0];
        assert_eq!(overall.scored_spans, s.traces, "场景[{}]：每条 trace 应恰好一个被评 span", s.key);
        assert_eq!(
            overall.pass_count,
            s.traces - s.injected_failures,
            "场景[{}]：通过数应等于 trace 数减去注入失败数",
            s.key
        );
        // 既有通过也有失败，eval 才有意义（注入比例都在 (0,1) 内）。
        assert!(s.injected_failures > 0, "场景[{}]：应注入了一些失败", s.key);
        assert!(s.injected_failures < s.traces, "场景[{}]：不应全失败", s.key);
    }
}

/// 多 agent 场景（风控研判）应能看出 per-agent 通过率差异：
/// 「风控研判」(低失败权重) 通过率应高于「反洗钱核查」(高失败权重)。
#[test]
fn per_agent_pass_rate_differs_in_multi_agent_scenario() {
    let coord = fresh();
    let report = evalkit::run_harness(&coord, 80, 42);
    let risk = report.scenarios.iter().find(|s| s.key == "风控研判").expect("有风控场景");

    let rate_of = |agent: &str| -> f32 {
        risk.summary
            .iter()
            .find(|r| r.agent_name.as_deref() == Some(agent))
            .map(|r| r.pass_rate())
            .unwrap_or(0.0)
    };
    let good_agent = rate_of("风控研判");
    let bad_agent = rate_of("反洗钱核查");
    assert!(
        good_agent > bad_agent,
        "表现好的 agent 通过率应更高：风控研判={good_agent:.2} 反洗钱核查={bad_agent:.2}"
    );
}

/// 回归机制：同一冻结数据集，评判标准收紧后通过率应下降（检出退步）。
#[test]
fn dataset_regression_drops_under_stricter_scorer() {
    let coord = fresh();
    let report = evalkit::run_harness(&coord, 80, 11);
    assert!(report.dataset_size > 0, "应采集到回归样本");
    let base = report.dataset_baseline[0].pass_rate();
    let strict = report.dataset_stricter[0].pass_rate();
    assert!(strict < base, "更严评判通过率应低于基准：基准={base:.2} 更严={strict:.2}");
}

/// 数据是**真灌进引擎**的：摄入后能从 trace 列表读出来。
#[test]
fn ingested_data_is_visible_in_trace_list() {
    let coord = fresh();
    let report = evalkit::run_harness(&coord, 20, 3);
    let total_traces: usize = report.scenarios.iter().map(|s| s.traces).sum();

    let snap = coord.pin_snapshot();
    let traces = coord.list_traces(&snap, &TraceQuery::all());
    assert_eq!(traces.len(), total_traces, "trace 列表条数应等于灌入的 trace 总数");
    // 每条 trace 三个 span（root/tool/answer），且有 token 成本。
    assert!(traces.iter().all(|t| t.span_count == 3), "每条 trace 应有 3 个 span");
    assert!(traces.iter().any(|t| t.total_input_tokens > 0), "应有输入 token 成本");
}

// ───────────────────────── 会话级（多轮）评测 ─────────────────────────

/// 会话级评测应把多轮对话准确分成「一次到位 / 绕圈后解决 / 未解决」三类，
/// 且分类与生成时注入的会话弧线一一对账（生成什么弧线，就该评成什么类）。
#[test]
fn session_eval_classifies_multi_turn_conversations() {
    let coord = fresh();
    let r = evalkit::run_session_harness(&coord, 60, 99);
    // 三类是一个划分（互斥且周全）。
    assert_eq!(r.efficient + r.looped_resolved + r.unresolved, r.evals.len());
    // 与生成弧线对账：一次到位=resolved_fast；绕圈后解决=重试+重复问；未解决=始终失败。
    assert_eq!(r.efficient, r.gen.resolved_fast, "一次到位");
    assert_eq!(r.looped_resolved, r.gen.resolved_after_retry + r.gen.repeat_question, "绕圈后解决");
    assert_eq!(r.unresolved, r.gen.unresolved, "未解决");
    // 各类都得有样本，演示才立得住。
    assert!(r.efficient > 0 && r.looped_resolved > 0 && r.unresolved > 0, "三类应都有样本");
    // 确实是多轮（平均 > 1 轮）。
    assert!(r.avg_turns > 1.0, "应是多轮会话");
}

/// 绕圈检测要双管齐下：连续失败、重复问，都应被判 looped。
#[test]
fn looping_is_detected_for_both_retry_and_repeat() {
    let coord = fresh();
    let r = evalkit::run_session_harness(&coord, 80, 7);
    // 被判 looped 的会话数应 ≥ 重试类（连续失败必触发 looped）。
    let looped = r.evals.iter().filter(|e| e.looped).count();
    assert!(looped >= r.gen.resolved_after_retry, "连续失败的重试会话都应被判绕圈");
    // 「绕圈后解决」的会话：既 resolved 又 looped。
    assert!(r.evals.iter().any(|e| e.resolved && e.looped), "应有绕圈后解决的会话");
    // 未解决的会话最后一轮一定是失败的。
    assert!(r.evals.iter().filter(|e| !e.resolved).all(|e| e.failed_turns > 0));
}
