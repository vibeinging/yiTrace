//! eval 测试框架的集成测试：用 evalkit 自造多场景数据、真实摄入、跑 eval 闭环，断言不变量。
//! 验证「框架真把数据灌进去了、eval 真还原了注入的失败、回归机制真能检出退步」。

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use yt_engine::evalkit;
use yt_engine::{
    EngineJsonApi, InMemorySegmentStore, KeywordScorer, TraceQuery, WireRecord, WriteCoordinator,
};

fn fresh() -> Arc<WriteCoordinator> {
    WriteCoordinator::new(Arc::new(InMemorySegmentStore::default()))
}

fn durable_dir(name: &str) -> std::path::PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "yt_eval_{name}_{}_{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn json_str(s: &str) -> String {
    format!("{s:?}")
}

fn eval_trace(trace: u64, project: &str, output: &str, status: u8) -> Vec<WireRecord> {
    let span = 3;
    let ext = format!("{trace}-{span}");
    let mut attrs = BTreeMap::new();
    attrs.insert("project_id".to_string(), json_str(project));
    attrs.insert("skill".to_string(), json_str("eval-sidecar"));
    vec![
        WireRecord {
            trace_id: trace,
            span_id: span,
            ts: trace as i64,
            seq: 1,
            event_type_tag: 1,
            ext_span_id: ext.clone(),
            parent_span_id: None,
            status: None,
            duration_ns: None,
            input_tokens: Some(100),
            output_tokens: None,
            cached_input_tokens: None,
            reasoning_tokens: None,
            total_tokens: None,
            cost_usd_nanos: None,
            cost_currency: None,
            provider: None,
            session_id: Some(trace / 10),
            tenant_id: None,
            external_trace_id: None,
            external_span_id: None,
            external_parent_span_id: None,
            external_session_id: None,
            agent_name: Some("eval-agent".to_string()),
            tool_name: None,
            model: Some("qwen-max".to_string()),
            input_text: Some("评估 attrs sidecar 过滤后的 eval 结果".to_string()),
            output_text: None,
            logs: Vec::new(),
            attrs,
        },
        WireRecord {
            trace_id: trace,
            span_id: span,
            ts: trace as i64 + 1,
            seq: 2,
            event_type_tag: 2,
            ext_span_id: ext,
            parent_span_id: None,
            status: Some(status),
            duration_ns: Some(1_000_000),
            input_tokens: None,
            output_tokens: Some(80),
            cached_input_tokens: None,
            reasoning_tokens: None,
            total_tokens: None,
            cost_usd_nanos: None,
            cost_currency: None,
            provider: None,
            session_id: Some(trace / 10),
            tenant_id: None,
            external_trace_id: None,
            external_span_id: None,
            external_parent_span_id: None,
            external_session_id: None,
            agent_name: Some("eval-agent".to_string()),
            tool_name: None,
            model: Some("qwen-max".to_string()),
            input_text: None,
            output_text: Some(output.to_string()),
            logs: Vec::new(),
            attrs: BTreeMap::new(),
        },
    ]
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
        assert_eq!(
            overall.scored_spans, s.traces,
            "场景[{}]：每条 trace 应恰好一个被评 span",
            s.key
        );
        assert_eq!(
            overall.pass_count,
            s.traces - s.injected_failures,
            "场景[{}]：通过数应等于 trace 数减去注入失败数",
            s.key
        );
        // 既有通过也有失败，eval 才有意义（注入比例都在 (0,1) 内）。
        assert!(s.injected_failures > 0, "场景[{}]：应注入了一些失败", s.key);
        assert!(
            s.injected_failures < s.traces,
            "场景[{}]：不应全失败",
            s.key
        );
    }
}

/// 多 agent 场景（风控研判）应能看出 per-agent 通过率差异：
/// 「风控研判」(低失败权重) 通过率应高于「反洗钱核查」(高失败权重)。
#[test]
fn per_agent_pass_rate_differs_in_multi_agent_scenario() {
    let coord = fresh();
    let report = evalkit::run_harness(&coord, 80, 42);
    let risk = report
        .scenarios
        .iter()
        .find(|s| s.key == "风控研判")
        .expect("有风控场景");

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
    assert!(
        strict < base,
        "更严评判通过率应低于基准：基准={base:.2} 更严={strict:.2}"
    );
}

/// Trace Diff 是 golden path / trajectory comparison 的证据层：
/// eval 写回以后，diff 应能同时暴露路径差异、失败验证和 eval 分数差异。
#[test]
fn trace_diff_eval_fixture_exposes_golden_path_evidence() {
    let coord = fresh();
    let api = EngineJsonApi::new(coord.clone());
    let batch = r#"[
      {
        "trace_id":901,
        "span_id":1,
        "ts":100,
        "seq":1,
        "event_type":2,
        "ext_span_id":"901-1",
        "status":0,
        "duration_ns":10,
        "tool_name":"repo-inspect",
        "input_tokens":10,
        "output_tokens":5,
        "total_tokens":15,
        "cost_usd_nanos":1000,
        "output_text":"确认是 macOS arm64 native binding 问题",
        "attrs":{"project_id":"agentic-data","skill":"packaging","mode":"auto","task_fingerprint":"native-binding-pack","loop_id":"loop-pass","validation_status":"pass","phase":"inspect","validator":"npm test"}
      },
      {
        "trace_id":901,
        "span_id":2,
        "ts":120,
        "seq":1,
        "event_type":2,
        "ext_span_id":"901-2",
        "status":0,
        "duration_ns":20,
        "tool_name":"npm-test",
        "input_tokens":10,
        "output_tokens":10,
        "total_tokens":20,
        "cost_usd_nanos":2000,
        "output_text":"npm test passed",
        "attrs":{"project_id":"agentic-data","skill":"packaging","mode":"auto","task_fingerprint":"native-binding-pack","loop_id":"loop-pass","validation_status":"pass","phase":"verify","validator":"npm test"}
      },
      {
        "trace_id":902,
        "span_id":1,
        "ts":100,
        "seq":1,
        "event_type":2,
        "ext_span_id":"902-1",
        "status":0,
        "duration_ns":15,
        "tool_name":"repo-inspect",
        "input_tokens":15,
        "output_tokens":10,
        "total_tokens":25,
        "cost_usd_nanos":1500,
        "output_text":"误判为 Rust host target 问题",
        "attrs":{"project_id":"agentic-data","skill":"packaging","mode":"auto","task_fingerprint":"native-binding-pack","loop_id":"loop-fail","validation_status":"fail","phase":"inspect","validator":"npm test"}
      },
      {
        "trace_id":902,
        "span_id":2,
        "ts":130,
        "seq":1,
        "event_type":2,
        "ext_span_id":"902-2",
        "status":0,
        "duration_ns":30,
        "tool_name":"cargo-build",
        "input_tokens":10,
        "output_tokens":20,
        "total_tokens":30,
        "cost_usd_nanos":3000,
        "output_text":"重新构建 native module",
        "attrs":{"project_id":"agentic-data","skill":"packaging","mode":"auto","task_fingerprint":"native-binding-pack","loop_id":"loop-fail","validation_status":"fail","phase":"build","validator":"npm test"}
      },
      {
        "trace_id":902,
        "span_id":3,
        "ts":160,
        "seq":1,
        "event_type":2,
        "ext_span_id":"902-3",
        "status":1,
        "duration_ns":40,
        "tool_name":"npm-test",
        "input_tokens":20,
        "output_tokens":30,
        "total_tokens":50,
        "cost_usd_nanos":5000,
        "output_text":"cannot find darwin-arm64 node binding",
        "attrs":{"project_id":"agentic-data","skill":"packaging","mode":"auto","task_fingerprint":"native-binding-pack","loop_id":"loop-fail","validation_status":"fail","phase":"verify","validator":"npm test"}
      }
    ]"#;
    let (status, body) = api.route_with_tenant("POST", "/v1/ingest", batch, Some(77));
    assert_eq!(status, 200, "{body}");

    let scorer = KeywordScorer::new(&["cannot find", "误判"]);
    let scored = coord.eval_and_writeback(&scorer, &TraceQuery::all());
    assert!(
        scored
            .iter()
            .any(|s| s.trace_id == 901 && s.outcome.score == 1000),
        "通过路径应得到满分 eval"
    );
    assert!(
        scored
            .iter()
            .any(|s| s.trace_id == 902 && s.outcome.score == 0),
        "失败路径应得到零分 eval"
    );

    let (status, diff) = api.route_with_tenant(
        "POST",
        "/v1/traces/diff",
        r#"{"leftTraceId":901,"rightTraceId":902}"#,
        Some(77),
    );
    assert_eq!(status, 200, "{diff}");
    assert!(diff.contains(r#""delta":{"spanCount":1"#), "{diff}");
    assert!(diff.contains(r#""errorCount":1"#), "{diff}");
    assert!(diff.contains(r#""durationNs":55"#), "{diff}");
    assert!(diff.contains(r#""totalTokens":70"#), "{diff}");
    assert!(diff.contains(r#""costUsdNanos":6500"#), "{diff}");
    assert!(
        diff.contains(r#""trajectory":{"left":{"signature":"fnv1a64:"#),
        "{diff}"
    );
    assert!(diff.contains(r#""same":false"#), "{diff}");
    assert!(
        diff.contains(r#""tool:npm-test|phase:verify|validator:npm_test""#),
        "{diff}"
    );
    assert!(diff.contains(r#""status":"right_only""#), "{diff}");
    assert!(diff.contains(r#""toolName":"cargo-build""#), "{diff}");
    assert!(diff.contains(r#""evalScore":1000"#), "{diff}");
    assert!(diff.contains(r#""evalScore":0"#), "{diff}");
    assert!(diff.contains(r#""evalLabel":"未通过""#), "{diff}");
    assert!(diff.contains(r#""evalScore""#), "{diff}");

    let (status, groups) = api.route_with_tenant(
        "POST",
        "/v1/trajectory-groups",
        r#"{"filter":{"taskFingerprint":"native-binding-pack"},"sort":"best"}"#,
        Some(77),
    );
    assert_eq!(status, 200, "{groups}");
    assert!(groups.contains(r#""total":2"#), "{groups}");
    assert!(groups.contains(r#""traceTotal":2"#), "{groups}");
    assert!(groups.contains(r#""spanTotal":5"#), "{groups}");
    assert!(
        groups.contains(r#""steps":["tool:repo-inspect|phase:inspect|validator:npm_test","tool:npm-test|phase:verify|validator:npm_test"]"#),
        "{groups}"
    );
    assert!(groups.contains(r#""qualityScore":1000"#), "{groups}");
    assert!(
        groups.contains(r#""eval":{"count":2,"avg":1000"#),
        "{groups}"
    );
    assert!(groups.contains(r#""errorTraceCount":1"#), "{groups}");

    let (status, golden) = api.route_with_tenant(
        "POST",
        "/v1/golden-paths",
        r#"{"sourceTraceId":901,"taskFingerprint":"native-binding-pack","score":1000,"label":"eval winner","reason":"eval score 1000","source":"eval_harness","projectId":"agentic-data"}"#,
        Some(77),
    );
    assert_eq!(status, 200, "{golden}");
    assert!(golden.contains(r#""status":"candidate""#), "{golden}");
    assert!(
        golden.contains(r#""trajectorySignature":"fnv1a64:"#),
        "{golden}"
    );

    let (status, confirmed) = api.route_with_tenant(
        "POST",
        "/v1/golden-paths/1/status",
        r#"{"status":"confirmed","reason":"eval accepted","source":"eval_harness"}"#,
        Some(77),
    );
    assert_eq!(status, 200, "{confirmed}");
    assert!(confirmed.contains(r#""status":"confirmed""#), "{confirmed}");

    let (status, paths) = api.route_with_tenant(
        "GET",
        "/v1/golden-paths?taskFingerprint=native-binding-pack&status=confirmed&projectId=agentic-data",
        "",
        Some(77),
    );
    assert_eq!(status, 200, "{paths}");
    assert!(paths.contains(r#""count":1"#), "{paths}");

    let (hidden_status, hidden) = api.route_with_tenant(
        "POST",
        "/v1/traces/diff",
        r#"{"leftTraceId":901,"rightTraceId":902}"#,
        Some(78),
    );
    assert_eq!(hidden_status, 404, "{hidden}");
}

/// 数据是**真灌进引擎**的：摄入后能从 trace 列表读出来。
#[test]
fn ingested_data_is_visible_in_trace_list() {
    let coord = fresh();
    let report = evalkit::run_harness(&coord, 20, 3);
    let total_traces: usize = report.scenarios.iter().map(|s| s.traces).sum();

    let snap = coord.pin_snapshot();
    let traces = coord.list_traces(&snap, &TraceQuery::all());
    assert_eq!(
        traces.len(),
        total_traces,
        "trace 列表条数应等于灌入的 trace 总数"
    );
    // 每条 trace 三个 span（root/tool/answer），且有 token 成本。
    assert!(
        traces.iter().all(|t| t.span_count == 3),
        "每条 trace 应有 3 个 span"
    );
    assert!(
        traces.iter().any(|t| t.total_input_tokens > 0),
        "应有输入 token 成本"
    );
}

/// attrs sidecar 不能只是查询快；它还要能支撑 eval 闭环后的数据读取。
/// 这里走 durable data dir：ingest → eval 写回 → recover → attrs filter → 读回 eval_score。
#[test]
fn durable_attrs_sidecar_filters_eval_results_after_recover() {
    let dir = durable_dir("attrs_sidecar");
    {
        let coord = WriteCoordinator::open_durable(&dir).unwrap();
        let mut records = Vec::new();
        records.extend(eval_trace(
            10,
            "agentic-data",
            "研判结论明确，可以放行。",
            0,
        ));
        records.extend(eval_trace(11, "agentic-data", "抱歉，无法完成研判。", 1));
        records.extend(eval_trace(20, "other-project", "其他项目结果正常。", 0));
        coord.ingest_wire(records);

        let scorer = KeywordScorer::new(evalkit::BAD_WORDS);
        let scored = coord.eval_and_writeback(&scorer, &TraceQuery::all());
        assert_eq!(scored.len(), 3, "三条 answer span 都应被打分");
        assert!(
            dir.join("attr_postings").join("seg-1.attrs").exists(),
            "eval flush 后应生成 attrs segment sidecar"
        );
    }

    {
        let coord = WriteCoordinator::open_durable(&dir).unwrap();
        coord.recover();
        let snap = coord.pin_snapshot();
        let attrs = BTreeMap::from([("project_id".to_string(), json_str("agentic-data"))]);
        let mut spans = coord.read_spans_query_for_attrs(&snap, &TraceQuery::all(), &attrs);
        spans.sort_by_key(|s| s.trace_id);

        assert_eq!(spans.len(), 2, "attrs filter 只应返回目标项目的 span");
        assert_eq!(
            spans.iter().map(|s| s.trace_id).collect::<Vec<_>>(),
            vec![10, 11]
        );
        assert_eq!(spans[0].eval_score, Some(1000), "通过样本分数应保留");
        assert_eq!(spans[1].eval_score, Some(0), "失败样本分数应保留");

        let summary = coord.eval_summary(&snap, &TraceQuery::all(), 1000);
        assert_eq!(summary[0].scored_spans, 3);
        assert_eq!(summary[0].pass_count, 2);
    }

    let _ = std::fs::remove_dir_all(&dir);
}

// ───────────────────────── 会话级（多轮）评测 ─────────────────────────

/// 会话级评测应把多轮对话准确分成「一次到位 / 绕圈后解决 / 未解决」三类，
/// 且分类与生成时注入的会话弧线一一对账（生成什么弧线，就该评成什么类）。
#[test]
fn session_eval_classifies_multi_turn_conversations() {
    let coord = fresh();
    let r = evalkit::run_session_harness(&coord, 60, 99);
    // 三类是一个划分（互斥且周全）。
    assert_eq!(
        r.efficient + r.looped_resolved + r.unresolved,
        r.evals.len()
    );
    // 与生成弧线对账：一次到位=resolved_fast；绕圈后解决=重试+重复问；未解决=始终失败。
    assert_eq!(r.efficient, r.gen.resolved_fast, "一次到位");
    assert_eq!(
        r.looped_resolved,
        r.gen.resolved_after_retry + r.gen.repeat_question,
        "绕圈后解决"
    );
    assert_eq!(r.unresolved, r.gen.unresolved, "未解决");
    // 各类都得有样本，演示才立得住。
    assert!(
        r.efficient > 0 && r.looped_resolved > 0 && r.unresolved > 0,
        "三类应都有样本"
    );
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
    assert!(
        looped >= r.gen.resolved_after_retry,
        "连续失败的重试会话都应被判绕圈"
    );
    // 「绕圈后解决」的会话：既 resolved 又 looped。
    assert!(
        r.evals.iter().any(|e| e.resolved && e.looped),
        "应有绕圈后解决的会话"
    );
    // 未解决的会话最后一轮一定是失败的。
    assert!(r
        .evals
        .iter()
        .filter(|e| !e.resolved)
        .all(|e| e.failed_turns > 0));
}
