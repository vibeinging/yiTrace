//! evalkit —— eval 测试框架 / 场景模拟器。
//!
//! 目的：把 eval 闭环从「单元测试里手搓几条 span」升级成「**自造多种 agent 场景的合成 trace、
//! 经真实摄入路径灌进引擎、跑完整评测闭环**」，既当端到端验证、也当可跑的演示。
//!
//! 它真做了什么（不是 mock）：
//! 1. **自产测试数据**：4 类内置 agent 场景（客服问答 / 风控研判多 agent / 代码助手 / 数据分析），
//!    每条 trace 拆成 root(编排) + tool(工具调用) + answer(模型作答) 三个 span，带中文 input/output、
//!    token、agent/工具/模型标注、状态/耗时。失败答案里埋「坏词」，给 scorer 留信号。
//! 2. **走真实摄入**：所有数据经 `WriteCoordinator::ingest_wire`（SDK 线格式同一入口）灌进去，
//!    不是直接塞内存表 —— 确定性 event_id、折叠、落盘全都真实经过。
//! 3. **跑完整 eval 闭环**：`eval_and_writeback` 打分走 upgrade 写回 → `eval_summary` 出 per-agent
//!    通过率看板 → `collect_into_dataset` 把答案 span 冻成回归数据集 → `eval_dataset` 用更严 scorer
//!    重跑，演示「评判标准变严 → 通过率下降」的回归检出。
//!
//! 确定性：用 std-only 的 xorshift 伪随机（同 seed 完全可复现），不碰 `rand` / 时钟，契合零依赖骨架。

use std::sync::Arc;

use yt_core::fold::FoldedSpan;

use crate::{AgentCost, EvalSummary, KeywordScorer, SessionTimeline, SessionTurn, TraceQuery, WireRecord, WriteCoordinator};

// ───────────────────────── 确定性伪随机（std-only） ─────────────────────────

/// splitmix64 —— 同 seed 可复现。选它而非 xorshift：定步长（每条 trace 固定消耗若干个数）
/// 采样下 xorshift 的低维相关性会让 `below(p)` 偏离 p；splitmix64 的强 finalizer 没这毛病，
/// 注入比例能贴合配置。仅用于造测试数据，不要求密码学强度。
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// [0,1) 均匀。
    fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// 以概率 p 返回 true。
    fn below(&mut self, p: f32) -> bool {
        self.unit() < p
    }

    /// 从切片里挑一个（切片非空）。
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[(self.next_u64() as usize) % xs.len()]
    }

    /// 闭区间 [lo, hi] 内的整数。
    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next_u64() % (hi - lo + 1)
    }
}

// ───────────────────────── 场景定义 ─────────────────────────

/// 失败答案里都会出现的「坏词」。`KeywordScorer` 命中任一即判未通过。
pub const BAD_WORDS: &[&str] = &["抱歉", "无法", "未知", "失败", "错误", "不确定"];

/// 更严评判时额外加入的词（出现在部分**合格**答案里）—— 用来演示评判标准收紧后通过率下降。
pub const STRICTER_EXTRA: &[&str] = &["暂缓"];

/// 一个 agent 场景的定义：参与的 agent / 工具 / 模型 + 提示词与好/坏答案样本 + 失败注入比例。
pub struct Scenario {
    pub key: &'static str,
    pub agents: &'static [&'static str],
    pub tools: &'static [&'static str],
    pub model: &'static str,
    pub prompts: &'static [&'static str],
    pub good: &'static [&'static str],
    pub bad: &'static [&'static str],
    /// 基准失败比例（多 agent 场景再按 agent 加权，见 `generate_scenario`）。
    pub fail_ratio: f32,
}

/// 4 类内置场景。中文内容让 BM25 检索与 eval 都有真实信号。
pub fn builtin_scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            key: "客服问答",
            agents: &["客服助手"],
            tools: &["知识库检索"],
            model: "qwen-max",
            prompts: &["如何修改预留手机号", "信用卡额度怎么提升", "账户被冻结了怎么办", "怎么查询交易明细"],
            good: &[
                "您可以在手机银行『我的-安全中心』修改预留手机号，需短信验证码确认。",
                "额度提升可在App信用卡页发起申请，系统将根据您的用信与还款情况评估。",
                "账户冻结通常因风控触发，请携带身份证到柜面或联系客服核实解冻。",
            ],
            bad: &["抱歉，我无法回答这个问题。", "这个问题我不确定，请联系人工客服。", "查询失败，请稍后再试。"],
            fail_ratio: 0.25,
        },
        Scenario {
            key: "风控研判",
            agents: &["风控研判", "反洗钱核查"],
            tools: &["规则引擎", "交易查询"],
            model: "qwen-max",
            prompts: &["对账户A近30天大额交易做风险研判", "核查该笔交易是否涉及可疑资金往来", "评估这笔跨境汇款的洗钱风险"],
            // 第一条含「暂缓」，会被更严 scorer 判掉 → 用于回归演示。
            good: &[
                "研判结论：交易触发规则R12，存在盗刷风险，建议人工复核并暂缓放款。",
                "经核查未发现可疑资金链路，交易模式正常，可予以放行。",
            ],
            bad: &["抱歉，规则引擎调用失败，无法给出研判结论。", "数据缺失，本次核查无法完成。"],
            fail_ratio: 0.40,
        },
        Scenario {
            key: "代码助手",
            agents: &["代码助手"],
            tools: &["代码执行", "单元测试"],
            model: "qwen-coder",
            prompts: &["实现一个快速排序函数", "修复这段空指针异常", "给登录接口加上限流"],
            good: &[
                "已生成快速排序实现并通过全部单元测试，平均时间复杂度O(nlogn)。",
                "已定位空指针来源并加上判空保护，回归测试全部通过。",
            ],
            bad: &["生成的代码运行报错，无法通过测试。", "抱歉，未能理解你的需求。"],
            fail_ratio: 0.50,
        },
        Scenario {
            key: "数据分析",
            agents: &["数据分析师"],
            tools: &["SQL执行", "图表生成"],
            model: "qwen-max",
            prompts: &["统计上季度各分行交易额", "分析近一周活跃用户趋势"],
            good: &[
                "已汇总各分行上季度交易额，华东区居首，整体环比增长12%。",
                "近一周活跃用户稳步上升，周五达到峰值。",
            ],
            bad: &["SQL执行失败，无法返回结果。", "数据为空，分析结果未知。"],
            fail_ratio: 0.20,
        },
    ]
}

// ───────────────────────── 合成数据生成 ─────────────────────────

/// 造一个 span 的 start + end 两条线格式记录（mirror 真实 SDK：属性挂 start、状态/输出挂 end）。
#[allow(clippy::too_many_arguments)]
fn emit_span(
    out: &mut Vec<WireRecord>,
    trace: u64,
    span: u64,
    parent: Option<u64>,
    ts: i64,
    session: u64,
    agent: Option<&str>,
    tool: Option<&str>,
    model: &str,
    input: &str,
    output: Option<&str>,
    in_tok: u64,
    out_tok: u64,
    status: u8,
    dur: u64,
) {
    let ext = format!("{trace}-{span}");
    // SPAN_START：身份 + 输入文本 + agent/工具/模型 + 输入 token。
    out.push(WireRecord {
        trace_id: trace,
        span_id: span,
        ts,
        seq: 1,
        event_type_tag: 1, // SpanStart
        ext_span_id: ext.clone(),
        parent_span_id: parent,
        status: None,
        duration_ns: None,
        input_tokens: if in_tok > 0 { Some(in_tok) } else { None },
        output_tokens: None,
        session_id: Some(session),
        agent_name: agent.map(str::to_string),
        tool_name: tool.map(str::to_string),
        model: Some(model.to_string()),
        input_text: Some(input.to_string()),
        output_text: None,
        logs: Vec::new(),
    });
    // SPAN_END：输出文本 + 状态 + 耗时 + 输出 token。
    out.push(WireRecord {
        trace_id: trace,
        span_id: span,
        ts: ts + dur as i64,
        seq: 2,
        event_type_tag: 2, // SpanEnd
        ext_span_id: ext,
        parent_span_id: parent,
        status: Some(status),
        duration_ns: Some(dur),
        input_tokens: None,
        output_tokens: if out_tok > 0 { Some(out_tok) } else { None },
        session_id: Some(session),
        agent_name: agent.map(str::to_string),
        tool_name: tool.map(str::to_string),
        model: Some(model.to_string()),
        input_text: None,
        output_text: output.map(str::to_string),
        logs: Vec::new(),
    });
}

/// 一个场景一次生成的统计。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenStats {
    pub traces: usize,
    pub spans: usize,
    /// 实际注入的失败答案数（answer span 用了 bad 文本）。eval 应当精确还原这个数。
    pub injected_failures: usize,
}

/// 给一个场景造 `n_traces` 条 trace 并经 `ingest_wire` 真实灌入。
///
/// 每条 trace = root(agent 编排，无输出，不被打分) + tool(工具调用，无输出) + answer(agent 作答，被打分)。
/// 多 agent 场景按 agent 轮转，并给不同 agent 不同失败权重（agent0 偏好、agent1 偏差），
/// 这样 per-agent 看板能看出「哪个 agent 通过率低」。
pub fn generate_scenario(
    coord: &WriteCoordinator,
    sc: &Scenario,
    n_traces: usize,
    base_trace: u64,
    ts_base: i64,
    rng: &mut Rng,
) -> GenStats {
    let mut recs = Vec::with_capacity(n_traces * 6);
    let mut injected_failures = 0;
    for i in 0..n_traces {
        let trace = base_trace + i as u64;
        let ts = ts_base + (i as i64) * 100; // 同场景内 trace 间隔 100ns，远小于场景间隔
        let session = base_trace + (i as u64 / 5); // 每 5 条 trace 归一个会话

        let ai = i % sc.agents.len();
        let agent = sc.agents[ai];
        // 多 agent：agent0 表现好(0.6×)、agent1 表现差(1.5×)，制造可见的 per-agent 差异。
        let weight = if sc.agents.len() > 1 {
            if ai == 0 {
                0.6
            } else {
                1.5
            }
        } else {
            1.0
        };
        let p = (sc.fail_ratio * weight).min(0.95);
        let is_fail = rng.below(p);
        if is_fail {
            injected_failures += 1;
        }

        let prompt = *rng.pick(sc.prompts);
        let tool = *rng.pick(sc.tools);
        let answer = if is_fail { *rng.pick(sc.bad) } else { *rng.pick(sc.good) };
        let in_tok = rng.range(200, 1500);
        let out_tok = rng.range(50, 600);
        let st = if is_fail { 1 } else { 0 };

        // root：编排 span，无 output_text → scorer 跳过。
        emit_span(&mut recs, trace, 1, None, ts, session, Some(agent), None, sc.model, prompt, None, in_tok, 0, 0, rng.range(1_000_000, 5_000_000));
        // tool：工具调用 span，无 agent、无 output_text → scorer 跳过。
        emit_span(&mut recs, trace, 2, Some(1), ts + 1, session, None, Some(tool), sc.model, prompt, None, 0, 0, st, rng.range(500_000, 3_000_000));
        // answer：作答 span，有 output_text → 被 scorer 打分。
        emit_span(&mut recs, trace, 3, Some(1), ts + 2, session, Some(agent), None, sc.model, prompt, Some(answer), in_tok, out_tok, st, rng.range(800_000, 4_000_000));
    }
    coord.ingest_wire(recs);
    GenStats { traces: n_traces, spans: n_traces * 3, injected_failures }
}

// ───────────────────────── 端到端 harness ─────────────────────────

/// 一个场景跑完一轮 eval 的报告。
#[derive(Debug, Clone)]
pub struct ScenarioReport {
    pub key: &'static str,
    pub traces: usize,
    pub spans: usize,
    pub injected_failures: usize,
    /// 评测看板：`[0]`=整体，其后按 agent 名升序。
    pub summary: Vec<EvalSummary>,
    /// per-agent 成本归因。
    pub cost: Vec<AgentCost>,
}

/// 整个 harness 的报告。
#[derive(Debug, Clone)]
pub struct HarnessReport {
    pub scenarios: Vec<ScenarioReport>,
    /// 回归数据集名（取「风控研判」场景的全部答案 span 冻结而成）。
    pub dataset_name: String,
    pub dataset_size: usize,
    /// 基准 scorer 在数据集上的看板（`[0]`=整体）。
    pub dataset_baseline: Vec<EvalSummary>,
    /// 更严 scorer 在**同一**数据集上的看板 —— 通过率应低于基准（回归检出）。
    pub dataset_stricter: Vec<EvalSummary>,
}

/// 每个场景占一段不重叠的时间带（100M 间隔），eval 用时间窗按场景隔离查询。
fn scenario_window(i: usize) -> (i64, i64) {
    let base = i as i64 * 100_000_000;
    (base, base + 99_999_999)
}

/// 端到端跑：造 4 类场景数据 → 真实摄入 → eval 打分写回 → per-agent 看板 → 收集回归集 → 更严重跑。
///
/// - `n_per`：每个场景造多少条 trace。
/// - `seed`：伪随机种子（同 seed 完全可复现）。
pub fn run_harness(coord: &Arc<WriteCoordinator>, n_per: usize, seed: u64) -> HarnessReport {
    let mut rng = Rng::new(seed);
    let scs = builtin_scenarios();
    let base_scorer = KeywordScorer::new(BAD_WORDS);

    let dataset_name = "风控回归集".to_string();
    let mut scenarios = Vec::new();

    for (i, sc) in scs.iter().enumerate() {
        let base_trace = (i as u64 + 1) * 100_000;
        let (from, to) = scenario_window(i);
        let gen = generate_scenario(coord, sc, n_per, base_trace, from, &mut rng);

        let q = TraceQuery { trace_id: None, time_from: from, time_to: to };
        // 打分写回（内部会先 flush，把 answer span 落段、upgrade 有落点）。
        coord.eval_and_writeback(&base_scorer, &q);

        let snap = coord.pin_snapshot();
        // 阈值 1000：千分制满分才算通过（KeywordScorer 给 0 或 1000）。
        let summary = coord.eval_summary(&snap, &q, 1000);
        let cost = coord.cost_by_agent(&snap, &q);

        // 风控场景：把全部 answer span（无论通过与否）冻成回归数据集。
        if sc.key == "风控研判" {
            coord.collect_into_dataset(&dataset_name, &snap, &q, &|s: &FoldedSpan| s.output_text.is_some());
        }
        drop(snap);

        scenarios.push(ScenarioReport {
            key: sc.key,
            traces: gen.traces,
            spans: gen.spans,
            injected_failures: gen.injected_failures,
            summary,
            cost,
        });
    }

    // 回归：同一数据集，基准 scorer vs 更严 scorer，看通过率怎么掉。
    let dataset_size = coord.dataset(&dataset_name).map(|d| d.examples.len()).unwrap_or(0);
    let dataset_baseline = coord.eval_dataset(&dataset_name, &base_scorer, 1000).unwrap_or_default();

    let mut strict_words: Vec<&str> = BAD_WORDS.to_vec();
    strict_words.extend_from_slice(STRICTER_EXTRA);
    let strict_scorer = KeywordScorer::new(&strict_words);
    let dataset_stricter = coord.eval_dataset(&dataset_name, &strict_scorer, 1000).unwrap_or_default();

    HarnessReport { scenarios, dataset_name, dataset_size, dataset_baseline, dataset_stricter }
}

/// 把 harness 报告打印成人看的看板（example 用）。
pub fn print_report(r: &HarnessReport) {
    println!("\n══════════════ eval 场景模拟报告 ══════════════\n");
    for s in &r.scenarios {
        let overall = &s.summary[0];
        println!("▶ 场景【{}】 trace={} span={} 注入失败={}", s.key, s.traces, s.spans, s.injected_failures);
        println!(
            "    整体：打分 {} 条，通过率 {:.0}%（通过 {}），均分 {}",
            overall.scored_spans,
            overall.pass_rate() * 100.0,
            overall.pass_count,
            overall.avg_score
        );
        for row in s.summary.iter().skip(1) {
            if let Some(name) = &row.agent_name {
                println!(
                    "      └ agent『{}』：通过率 {:.0}%（{}/{}），均分 {}",
                    name,
                    row.pass_rate() * 100.0,
                    row.pass_count,
                    row.scored_spans,
                    row.avg_score
                );
            }
        }
        for c in &s.cost {
            println!("      成本『{}』：span {} · 输入 {} tok · 输出 {} tok", c.agent_name, c.span_count, c.input_tokens, c.output_tokens);
        }
        println!();
    }

    println!("══════════════ 回归数据集复跑 ══════════════");
    let base = r.dataset_baseline.first().map(|s| s.pass_rate()).unwrap_or(0.0);
    let strict = r.dataset_stricter.first().map(|s| s.pass_rate()).unwrap_or(0.0);
    println!("数据集【{}】 样本 {} 条", r.dataset_name, r.dataset_size);
    println!("  基准评判通过率：{:.0}%", base * 100.0);
    println!("  更严评判通过率：{:.0}%   （评判标准收紧 → 通过率下降 = 回归被检出）", strict * 100.0);
    println!();
}

// ───────────────────────── 会话级评测（多轮专属） ─────────────────────────

/// 一轮算不算失败：该轮出错（status≠0）或答复含坏词（含没有答复）。
fn turn_failed(t: &SessionTurn) -> bool {
    if t.error_count > 0 {
        return true;
    }
    match &t.agent_output {
        Some(o) => BAD_WORDS.iter().any(|w| o.contains(w)),
        None => true, // 这一轮没给出答复，也算没解决
    }
}

/// 会话级评测结果 —— 把评测从 per-span 推到 per-session 的多轮专属指标。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEval {
    pub session_id: u64,
    /// 轮数。
    pub turns: usize,
    /// 失败轮数。
    pub failed_turns: usize,
    /// 最终是否解决（最后一轮成功）。
    pub resolved: bool,
    /// 是否绕圈：连续 ≥2 轮失败，或同一问题被重复问 ≥2 次。
    pub looped: bool,
    /// 千分制综合分：未解决=0；绕圈后解决=500；一次到位=1000。
    pub score: u32,
    /// 人看标签。
    pub label: String,
}

/// 对一个会话的对话流打分（规则版，多轮维度）。换 LLM-judge 时只换这个函数体，harness 不变。
pub fn score_session(tl: &SessionTimeline) -> SessionEval {
    let turns = tl.turns.len();
    let failed_turns = tl.turns.iter().filter(|t| turn_failed(t)).count();
    let resolved = tl.turns.last().map(|t| !turn_failed(t)).unwrap_or(false);

    // 绕圈①：连续 ≥2 轮失败（一直在错、没往前走）。
    let mut looped = false;
    let mut streak = 0;
    for t in &tl.turns {
        if turn_failed(t) {
            streak += 1;
            if streak >= 2 {
                looped = true;
            }
        } else {
            streak = 0;
        }
    }
    // 绕圈②：同一个问题被重复问 ≥2 次（用户在原地打转）。
    let mut asked: std::collections::HashMap<&str, u32> = std::collections::HashMap::new();
    for t in &tl.turns {
        if let Some(q) = t.user_input.as_deref() {
            let c = asked.entry(q).or_insert(0);
            *c += 1;
            if *c >= 2 {
                looped = true;
            }
        }
    }

    let (score, label) = if !resolved {
        (0, "未解决")
    } else if looped {
        (500, "绕圈后解决")
    } else {
        (1000, "一次到位")
    };
    SessionEval { session_id: tl.session_id, turns, failed_turns, resolved, looped, score, label: label.to_string() }
}

// ───────────────────────── 连贯多轮会话生成 ─────────────────────────

/// 造一轮（= 一条 trace：root 编排 + tool 工具 + answer 作答），good=false 时埋坏词+置错。
fn emit_turn(out: &mut Vec<WireRecord>, sc: &Scenario, agent: &str, trace: u64, ts: i64, session: u64, prompt: &str, good: bool, rng: &mut Rng) {
    let answer = if good { *rng.pick(sc.good) } else { *rng.pick(sc.bad) };
    let st = if good { 0 } else { 1 };
    let in_tok = rng.range(200, 1500);
    let out_tok = rng.range(50, 600);
    emit_span(out, trace, 1, None, ts, session, Some(agent), None, sc.model, prompt, None, in_tok, 0, 0, rng.range(1_000_000, 5_000_000));
    emit_span(out, trace, 2, Some(1), ts + 1, session, None, Some(*rng.pick(sc.tools)), sc.model, prompt, None, 0, 0, st, rng.range(500_000, 3_000_000));
    emit_span(out, trace, 3, Some(1), ts + 2, session, Some(agent), None, sc.model, prompt, Some(answer), in_tok, out_tok, st, rng.range(800_000, 4_000_000));
}

/// 连贯多轮会话的生成统计（每类会话各多少个，用于和评测分类对账）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConvStats {
    pub sessions: usize,
    pub turns: usize,
    /// 一轮到位（→ 评测应判「一次到位」）。
    pub resolved_fast: usize,
    /// 失败几轮后才成功（→ 评测应判「绕圈后解决」，连续失败触发）。
    pub resolved_after_retry: usize,
    /// 重复问同一问题后成功（→ 评测应判「绕圈后解决」，重复问触发）。
    pub repeat_question: usize,
    /// 始终没解决（→ 评测应判「未解决」）。
    pub unresolved: usize,
}

/// 造 `n_sessions` 个**连贯多轮会话**并真实摄入：一个会话 = 一个用户围绕一个任务的多轮交互，
/// 质量有四种弧线（一次到位 / 重试后成功 / 重复问后成功 / 始终失败），让会话级评测有各类样本。
pub fn generate_conversations(coord: &WriteCoordinator, sc: &Scenario, n_sessions: usize, base_trace: u64, ts_base: i64, base_session: u64, rng: &mut Rng) -> ConvStats {
    let mut recs = Vec::new();
    let mut stats = ConvStats::default();
    let agent = sc.agents[0];
    let mut trace = base_trace;
    let mut ts = ts_base;

    for s in 0..n_sessions {
        let session = base_session + s as u64;
        let kind = rng.unit();
        if kind < 0.40 {
            // 一次到位：1 轮成功。
            let p = *rng.pick(sc.prompts);
            emit_turn(&mut recs, sc, agent, trace, ts, session, p, true, rng);
            trace += 1;
            ts += 1000;
            stats.resolved_fast += 1;
            stats.turns += 1;
        } else if kind < 0.65 {
            // 重试后成功：2 轮失败（连续）→ 1 轮成功。用户每轮换个说法（不同 prompt）。
            for good in [false, false, true] {
                let p = *rng.pick(sc.prompts);
                emit_turn(&mut recs, sc, agent, trace, ts, session, p, good, rng);
                trace += 1;
                ts += 1000;
            }
            stats.resolved_after_retry += 1;
            stats.turns += 3;
        } else if kind < 0.80 {
            // 重复问后成功：同一个问题问 2 轮，都给了像样答复（但用户在重复 = 绕圈信号）。
            let p = *rng.pick(sc.prompts);
            for _ in 0..2 {
                emit_turn(&mut recs, sc, agent, trace, ts, session, p, true, rng);
                trace += 1;
                ts += 1000;
            }
            stats.repeat_question += 1;
            stats.turns += 2;
        } else {
            // 始终没解决：3 轮全失败。
            for _ in 0..3 {
                let p = *rng.pick(sc.prompts);
                emit_turn(&mut recs, sc, agent, trace, ts, session, p, false, rng);
                trace += 1;
                ts += 1000;
            }
            stats.unresolved += 1;
            stats.turns += 3;
        }
        stats.sessions += 1;
    }
    coord.ingest_wire(recs);
    stats
}

/// 会话级 harness 的报告。
#[derive(Debug, Clone)]
pub struct SessionHarnessReport {
    pub gen: ConvStats,
    /// 每个会话一条评测。
    pub evals: Vec<SessionEval>,
    /// 「一次到位」会话数（resolved 且非 looped）。
    pub efficient: usize,
    /// 「绕圈后解决」会话数（resolved 且 looped）。
    pub looped_resolved: usize,
    /// 「未解决」会话数。
    pub unresolved: usize,
    pub avg_turns: f32,
    /// 一个绕圈会话的对话流样本（给视图打印用）。
    pub sample: Option<SessionTimeline>,
}

/// 端到端会话级评测：造连贯多轮会话 → 真实摄入 → 逐会话装对话流 → 会话级打分 → 聚合分类。
/// 用「客服问答」场景（天然多轮）。
pub fn run_session_harness(coord: &Arc<WriteCoordinator>, n_sessions: usize, seed: u64) -> SessionHarnessReport {
    let mut rng = Rng::new(seed);
    let scs = builtin_scenarios();
    let sc = &scs[0]; // 客服问答
    let gen = generate_conversations(coord, sc, n_sessions, 900_000, 0, 50_000, &mut rng);

    let snap = coord.pin_snapshot();
    let sessions = coord.list_sessions(&snap, &TraceQuery::all());
    let mut evals = Vec::with_capacity(sessions.len());
    let mut sample = None;
    for ss in &sessions {
        let tl = coord.load_session_timeline(&snap, ss.session_id);
        let ev = score_session(&tl);
        if sample.is_none() && ev.looped && ev.resolved {
            sample = Some(tl); // 留一个「绕圈后解决」的会话当对话流样本
        }
        evals.push(ev);
    }
    drop(snap);

    let efficient = evals.iter().filter(|e| e.resolved && !e.looped).count();
    let looped_resolved = evals.iter().filter(|e| e.resolved && e.looped).count();
    let unresolved = evals.iter().filter(|e| !e.resolved).count();
    let avg_turns = if evals.is_empty() { 0.0 } else { evals.iter().map(|e| e.turns).sum::<usize>() as f32 / evals.len() as f32 };

    SessionHarnessReport { gen, evals, efficient, looped_resolved, unresolved, avg_turns, sample }
}

/// 打印会话级评测报告（example 用）。
pub fn print_session_report(r: &SessionHarnessReport) {
    println!("══════════════ 会话级（多轮）评测报告 ══════════════\n");
    println!(
        "会话 {} 个 · 共 {} 轮 · 平均 {:.1} 轮/会话",
        r.gen.sessions, r.gen.turns, r.avg_turns
    );
    let total = r.evals.len().max(1);
    println!("  一次到位  ：{:>3} （{:.0}%）", r.efficient, r.efficient as f32 / total as f32 * 100.0);
    println!("  绕圈后解决：{:>3} （{:.0}%）  ← 连续失败或重复问后才成功", r.looped_resolved, r.looped_resolved as f32 / total as f32 * 100.0);
    println!("  未解决    ：{:>3} （{:.0}%）", r.unresolved, r.unresolved as f32 / total as f32 * 100.0);

    if let Some(tl) = &r.sample {
        println!("\n  ── 对话流样本（会话 {}，{} 轮，绕圈后解决）──", tl.session_id, tl.turns.len());
        for t in &tl.turns {
            let q = t.user_input.as_deref().unwrap_or("");
            let a = t.agent_output.as_deref().unwrap_or("");
            let mark = if turn_failed(t) { "✗" } else { "✓" };
            println!("    第{}轮 {} 用户：{}", t.turn_index + 1, mark, q);
            println!("           答复：{}", a);
        }
    }
    println!();
}
