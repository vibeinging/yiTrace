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
    SessionEval {
        session_id: tl.session_id,
        turns,
        failed_turns,
        resolved,
        looped,
        score,
        label: label.to_string(),
    }
}

// ───────────────────────── 连贯多轮会话生成 ─────────────────────────

/// 造一轮（= 一条 trace：root 编排 + tool 工具 + answer 作答），good=false 时埋坏词+置错。
fn emit_turn(
    out: &mut Vec<WireRecord>,
    sc: &Scenario,
    agent: &str,
    trace: u64,
    ts: i64,
    session: u64,
    prompt: &str,
    good: bool,
    rng: &mut Rng,
) {
    let answer = if good {
        *rng.pick(sc.good)
    } else {
        *rng.pick(sc.bad)
    };
    let st = if good { 0 } else { 1 };
    let in_tok = rng.range(200, 1500);
    let out_tok = rng.range(50, 600);
    emit_span(
        out,
        trace,
        1,
        None,
        ts,
        session,
        Some(agent),
        None,
        sc.model,
        prompt,
        None,
        in_tok,
        0,
        0,
        rng.range(1_000_000, 5_000_000),
    );
    emit_span(
        out,
        trace,
        2,
        Some(1),
        ts + 1,
        session,
        None,
        Some(*rng.pick(sc.tools)),
        sc.model,
        prompt,
        None,
        0,
        0,
        st,
        rng.range(500_000, 3_000_000),
    );
    emit_span(
        out,
        trace,
        3,
        Some(1),
        ts + 2,
        session,
        Some(agent),
        None,
        sc.model,
        prompt,
        Some(answer),
        in_tok,
        out_tok,
        st,
        rng.range(800_000, 4_000_000),
    );
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
pub fn generate_conversations(
    coord: &WriteCoordinator,
    sc: &Scenario,
    n_sessions: usize,
    base_trace: u64,
    ts_base: i64,
    base_session: u64,
    rng: &mut Rng,
) -> ConvStats {
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
pub fn run_session_harness(
    coord: &Arc<WriteCoordinator>,
    n_sessions: usize,
    seed: u64,
) -> SessionHarnessReport {
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
    let avg_turns = if evals.is_empty() {
        0.0
    } else {
        evals.iter().map(|e| e.turns).sum::<usize>() as f32 / evals.len() as f32
    };

    SessionHarnessReport {
        gen,
        evals,
        efficient,
        looped_resolved,
        unresolved,
        avg_turns,
        sample,
    }
}

/// 打印会话级评测报告（example 用）。
pub fn print_session_report(r: &SessionHarnessReport) {
    println!("══════════════ 会话级（多轮）评测报告 ══════════════\n");
    println!(
        "会话 {} 个 · 共 {} 轮 · 平均 {:.1} 轮/会话",
        r.gen.sessions, r.gen.turns, r.avg_turns
    );
    let total = r.evals.len().max(1);
    println!(
        "  一次到位  ：{:>3} （{:.0}%）",
        r.efficient,
        r.efficient as f32 / total as f32 * 100.0
    );
    println!(
        "  绕圈后解决：{:>3} （{:.0}%）  ← 连续失败或重复问后才成功",
        r.looped_resolved,
        r.looped_resolved as f32 / total as f32 * 100.0
    );
    println!(
        "  未解决    ：{:>3} （{:.0}%）",
        r.unresolved,
        r.unresolved as f32 / total as f32 * 100.0
    );

    if let Some(tl) = &r.sample {
        println!(
            "\n  ── 对话流样本（会话 {}，{} 轮，绕圈后解决）──",
            tl.session_id,
            tl.turns.len()
        );
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
