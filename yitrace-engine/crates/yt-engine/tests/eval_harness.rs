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

fn assert_json_contains(body: &str, needle: &str) {
    assert!(body.contains(needle), "missing {needle:?} in {body}");
}

fn metric_value(metrics: &str, name: &str) -> u64 {
    metrics
        .lines()
        .filter(|line| !line.starts_with('#'))
        .find_map(|line| {
            let mut parts = line.split_whitespace();
            let metric = parts.next()?;
            let value = parts.next()?;
            (metric == name).then(|| value.parse::<u64>().ok()).flatten()
        })
        .unwrap_or_else(|| panic!("missing metric {name} in:\n{metrics}"))
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

/// 索引/性能契约：attrs segment sidecar 必须是按需加载的派生索引。
/// 这个测试不做脆弱的耗时断言，而是用指标约束查询路径：recover 不预热 posting list，
/// 首次索引查询产生 sidecar load/miss，重复查询命中 cache；高基数字段仍能慢路径精确返回。
#[test]
fn attrs_sidecar_index_cache_is_lazy_and_trace_summaries_stay_complete() {
    let dir = durable_dir("attrs_index_perf");
    {
        let coord = WriteCoordinator::open_durable(&dir).unwrap();
        coord.recover();
        let api = EngineJsonApi::new(coord.clone());
        let batch1 = r#"[
          {
            "trace_id":701,
            "span_id":1,
            "session_id":7701,
            "ts":10,
            "seq":1,
            "event_type":2,
            "ext_span_id":"701-1",
            "status":0,
            "duration_ns":10,
            "input_tokens":10,
            "output_tokens":5,
            "cost_usd_nanos":1000,
            "attrs":{"project_id":"index-perf","connection_ids":["conn-a","conn-b"],"task_fingerprint":"index-task","path_memory_id":"pm-701"}
          },
          {
            "trace_id":701,
            "span_id":2,
            "session_id":7701,
            "ts":20,
            "seq":1,
            "event_type":2,
            "ext_span_id":"701-2",
            "status":0,
            "duration_ns":20,
            "input_tokens":20,
            "output_tokens":10,
            "cost_usd_nanos":2000,
            "attrs":{"project_id":"index-perf","task_fingerprint":"index-task"}
          },
          {
            "trace_id":702,
            "span_id":1,
            "session_id":7702,
            "ts":30,
            "seq":1,
            "event_type":2,
            "ext_span_id":"702-1",
            "status":0,
            "duration_ns":10,
            "attrs":{"project_id":"other-project","connection_ids":["conn-z"],"task_fingerprint":"index-task"}
          }
        ]"#;
        let (status, body) = api.route_with_tenant("POST", "/v1/ingest", batch1, Some(16));
        assert_eq!(status, 200, "{body}");
        coord.flush_memtable();

        let batch2 = r#"[
          {
            "trace_id":703,
            "span_id":1,
            "session_id":7703,
            "ts":40,
            "seq":1,
            "event_type":2,
            "ext_span_id":"703-1",
            "status":0,
            "duration_ns":10,
            "input_tokens":30,
            "output_tokens":15,
            "cost_usd_nanos":3000,
            "attrs":{"project_id":"index-perf","connection_ids":["conn-a"],"task_fingerprint":"index-task","path_memory_id":"pm-703"}
          }
        ]"#;
        let (status, body) = api.route_with_tenant("POST", "/v1/ingest", batch2, Some(16));
        assert_eq!(status, 200, "{body}");
        coord.flush_memtable();

        let sidecar_dir = dir.join("attr_postings");
        let sidecar_files = std::fs::read_dir(&sidecar_dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().and_then(|s| s.to_str()) == Some("attrs"))
            .count();
        assert!(
            sidecar_files >= 2,
            "two flushes should create segment-local attrs sidecars"
        );
    }

    {
        let coord = WriteCoordinator::open_durable(&dir).unwrap();
        coord.recover();
        let api = EngineJsonApi::new(coord.clone());
        let before = coord.metrics();
        assert!(
            metric_value(&before, "yt_attr_sidecar_segments") >= 2,
            "{before}"
        );
        assert_eq!(
            metric_value(&before, "yt_attr_sidecar_cache_entries"),
            0,
            "recover should rebuild only the sidecar directory, not prewarm posting lists"
        );
        assert_eq!(
            metric_value(&before, "yt_attr_sidecar_cache_loads"),
            0,
            "no sidecar posting list should be loaded before the first query"
        );

        let indexed_query =
            r#"{"filter":{"projectId":"index-perf","connectionIds":"conn-a"},"limit":10}"#;
        let (status, indexed) =
            api.route_with_tenant("POST", "/v1/trace-search", indexed_query, Some(16));
        assert_eq!(status, 200, "{indexed}");
        assert_json_contains(&indexed, r#""total":2"#);
        assert_json_contains(&indexed, r#""traceId":"701""#);
        assert_json_contains(&indexed, r#""traceId":"703""#);
        assert_json_contains(&indexed, r#""index":"attrs_postings+folded_verify""#);

        let after_first = coord.metrics();
        assert!(
            metric_value(&after_first, "yt_attr_sidecar_cache_loads") > 0,
            "{after_first}"
        );
        assert!(
            metric_value(&after_first, "yt_attr_sidecar_cache_misses") > 0,
            "{after_first}"
        );
        assert!(
            metric_value(&after_first, "yt_attr_sidecar_cache_entries") > 0,
            "{after_first}"
        );

        let first_hits = metric_value(&after_first, "yt_attr_sidecar_cache_hits");
        let (status, indexed_again) =
            api.route_with_tenant("POST", "/v1/trace-search", indexed_query, Some(16));
        assert_eq!(status, 200, "{indexed_again}");
        assert_json_contains(&indexed_again, r#""total":2"#);
        let after_second = coord.metrics();
        assert!(
            metric_value(&after_second, "yt_attr_sidecar_cache_hits") > first_hits,
            "repeat indexed query should hit the sidecar cache\nbefore:\n{after_first}\nafter:\n{after_second}"
        );

        let (status, trace_list) =
            api.route_with_tenant("GET", "/v1/traces?connectionIds=conn-a", "", Some(16));
        assert_eq!(status, 200, "{trace_list}");
        assert_json_contains(&trace_list, r#""trace_id":701"#);
        assert_json_contains(&trace_list, r#""span_count":2"#);
        assert_json_contains(&trace_list, r#""total_cost_usd_nanos":3000"#);

        let (status, sessions) =
            api.route_with_tenant("GET", "/v1/sessions?connectionIds=conn-a", "", Some(16));
        assert_eq!(status, 200, "{sessions}");
        assert_json_contains(&sessions, r#""sessionId":"7701""#);
        assert_json_contains(&sessions, r#""sessionId":"7703""#);
        assert_json_contains(&sessions, r#""total":2"#);

        let loads_before_unindexed = metric_value(&coord.metrics(), "yt_attr_sidecar_cache_loads");
        let (status, slow_path) = api.route_with_tenant(
            "POST",
            "/v1/trace-search",
            r#"{"filter":{"pathMemoryId":"pm-703"}}"#,
            Some(16),
        );
        assert_eq!(status, 200, "{slow_path}");
        assert_json_contains(&slow_path, r#""total":1"#);
        assert_json_contains(&slow_path, r#""traceId":"703""#);
        let loads_after_unindexed = metric_value(&coord.metrics(), "yt_attr_sidecar_cache_loads");
        assert_eq!(
            loads_after_unindexed, loads_before_unindexed,
            "path_memory_id is intentionally not a sidecar-postings key; query should not load new sidecar lists"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// 这两天新增的 cost 查询不能只覆盖显式 cost。
/// eval 场景里常见三种混用：上游直接报 cost、按 provider/model 估算、未知模型走默认估算。
#[test]
fn eval_trace_search_filters_explicit_model_and_default_cost_sources() {
    let coord = fresh();
    let api = EngineJsonApi::new(coord.clone());
    let batch = r#"[
      {
        "trace_id":301,
        "span_id":1,
        "ts":10,
        "seq":1,
        "event_type":2,
        "ext_span_id":"301-1",
        "status":0,
        "duration_ns":10,
        "tool_name":"explicit-cost",
        "input_tokens":20,
        "output_tokens":10,
        "total_tokens":30,
        "cost_usd_nanos":5000,
        "output_text":"显式 cost 通过",
        "attrs":{"project_id":"eval-cost","task_fingerprint":"cost-long-tail","validation_status":"pass","path_memory_id":"pm-explicit"}
      },
      {
        "trace_id":302,
        "span_id":1,
        "ts":20,
        "seq":1,
        "event_type":2,
        "ext_span_id":"302-1",
        "status":0,
        "duration_ns":10,
        "tool_name":"model-priced",
        "provider":"openai",
        "model":"gpt-4o-mini",
        "input_tokens":10,
        "cached_input_tokens":10,
        "output_tokens":10,
        "output_text":"模型估算 cost 通过",
        "attrs":{"project_id":"eval-cost","task_fingerprint":"cost-long-tail","validation_status":"pass","path_memory_id":"pm-model"}
      },
      {
        "trace_id":303,
        "span_id":1,
        "ts":30,
        "seq":1,
        "event_type":2,
        "ext_span_id":"303-1",
        "status":1,
        "duration_ns":10,
        "tool_name":"default-priced",
        "provider":"unknown",
        "model":"unknown-model",
        "input_tokens":10,
        "output_tokens":10,
        "output_text":"默认估算失败，无法完成",
        "attrs":{"project_id":"eval-cost","task_fingerprint":"cost-long-tail","validation_status":"fail","path_memory_id":"pm-default"}
      }
    ]"#;
    let (status, body) = api.route_with_tenant("POST", "/v1/ingest", batch, Some(9));
    assert_eq!(status, 200, "{body}");

    let scorer = KeywordScorer::new(&["无法"]);
    let scored = coord.eval_and_writeback(&scorer, &TraceQuery::all());
    assert_eq!(scored.len(), 3, "三条 eval span 都应被打分");
    assert!(scored
        .iter()
        .any(|s| s.trace_id == 303 && s.outcome.score == 0));

    let (status, explicit) = api.route_with_tenant(
        "POST",
        "/v1/trace-search",
        r#"{"filter":{"projectId":"eval-cost","minCostUsdNanos":4000,"maxCostUsdNanos":6000}}"#,
        Some(9),
    );
    assert_eq!(status, 200, "{explicit}");
    assert_json_contains(&explicit, r#""total":1"#);
    assert_json_contains(&explicit, r#""traceId":"301""#);
    assert_json_contains(&explicit, r#""costUsdNanos":5000"#);
    assert_json_contains(&explicit, r#""source":"explicit""#);
    assert_json_contains(&explicit, r#""index":"attrs_postings+folded_verify""#);

    let (status, model_priced) = api.route_with_tenant(
        "POST",
        "/v1/trace-search",
        r#"{"filter":{"projectId":"eval-cost","minCostUsd":0.000008,"maxCostUsd":0.000009,"minTokens":30,"maxTokens":30}}"#,
        Some(9),
    );
    assert_eq!(status, 200, "{model_priced}");
    assert_json_contains(&model_priced, r#""total":1"#);
    assert_json_contains(&model_priced, r#""traceId":"302""#);
    assert_json_contains(&model_priced, r#""costUsdNanos":8250"#);
    assert_json_contains(&model_priced, r#""source":"estimated_model_price""#);

    let (status, default_priced) = api.route_with_tenant(
        "POST",
        "/v1/trace-search",
        r#"{"filter":{"projectId":"eval-cost","minCostUsdNanos":47000,"maxCostUsdNanos":49000}}"#,
        Some(9),
    );
    assert_eq!(status, 200, "{default_priced}");
    assert_json_contains(&default_priced, r#""total":1"#);
    assert_json_contains(&default_priced, r#""traceId":"303""#);
    assert_json_contains(&default_priced, r#""costUsdNanos":48000"#);
    assert_json_contains(&default_priced, r#""source":"estimated_default""#);

    let (status, path_memory) = api.route_with_tenant(
        "POST",
        "/v1/trace-search",
        r#"{"filter":{"pathMemoryId":"pm-default"}}"#,
        Some(9),
    );
    assert_eq!(status, 200, "{path_memory}");
    assert_json_contains(&path_memory, r#""total":1"#);
    assert_json_contains(&path_memory, r#""traceId":"303""#);
}

/// 顶层 evalProfile 是 Golden Path 治理元数据，不应污染 attrs scope。
/// 否则后续 health 会额外带上 attrs.eval_profile 过滤，导致同 scope trace 全部被筛掉。
#[test]
fn golden_path_eval_profile_governance_does_not_pollute_scope_filter() {
    let coord = fresh();
    let api = EngineJsonApi::new(coord);
    let batch = r#"[
      {
        "trace_id":401,
        "span_id":1,
        "ts":10,
        "seq":1,
        "event_type":2,
        "ext_span_id":"401-1",
        "status":0,
        "duration_ns":10,
        "tool_name":"planner",
        "attrs":{"project_id":"eval-governance","task_fingerprint":"governance-scope","skill":"review","mode":"auto","harness_version":"h1","schema_fingerprint":"s1","phase":"plan"}
      },
      {
        "trace_id":402,
        "span_id":1,
        "ts":20,
        "seq":1,
        "event_type":2,
        "ext_span_id":"402-1",
        "status":0,
        "duration_ns":10,
        "tool_name":"planner",
        "attrs":{"project_id":"eval-governance","task_fingerprint":"governance-scope","skill":"review","mode":"auto","harness_version":"h1","schema_fingerprint":"s1","phase":"plan"}
      },
      {
        "trace_id":403,
        "span_id":1,
        "ts":30,
        "seq":1,
        "event_type":2,
        "ext_span_id":"403-1",
        "status":0,
        "duration_ns":10,
        "tool_name":"planner",
        "attrs":{"project_id":"eval-governance","task_fingerprint":"governance-scope","skill":"review","mode":"auto","harness_version":"h1","schema_fingerprint":"s1","phase":"plan"}
      },
      {
        "trace_id":403,
        "span_id":2,
        "ts":31,
        "seq":1,
        "event_type":2,
        "ext_span_id":"403-2",
        "status":0,
        "duration_ns":10,
        "tool_name":"validator",
        "attrs":{"project_id":"eval-governance","task_fingerprint":"governance-scope","skill":"review","mode":"auto","harness_version":"h1","schema_fingerprint":"s1","phase":"verify","validator":"npm test"}
      },
      {
        "trace_id":404,
        "span_id":1,
        "ts":40,
        "seq":1,
        "event_type":2,
        "ext_span_id":"404-1",
        "status":1,
        "duration_ns":10,
        "tool_name":"fallback",
        "attrs":{"project_id":"eval-governance","task_fingerprint":"governance-scope","skill":"review","mode":"auto","harness_version":"h1","schema_fingerprint":"s1","phase":"fallback"}
      }
    ]"#;
    let (status, body) = api.route_with_tenant("POST", "/v1/ingest", batch, Some(10));
    assert_eq!(status, 200, "{body}");

    let create = r#"{
      "sourceTraceId":401,
      "taskFingerprint":"governance-scope",
      "score":980,
      "evalProfile":"release-gate",
      "minSampleCount":4,
      "marginScore":900,
      "projectId":"eval-governance"
    }"#;
    let (status, golden) = api.route_with_tenant("POST", "/v1/golden-paths", create, Some(10));
    assert_eq!(status, 200, "{golden}");
    assert_json_contains(&golden, r#""evalProfile":"release-gate""#);
    assert_json_contains(&golden, r#""project_id":"eval-governance""#);
    assert_json_contains(
        &golden,
        r#""attrs":{"harness_version":"h1","mode":"auto","project_id":"eval-governance","schema_fingerprint":"s1","skill":"review","task_fingerprint":"governance-scope"}"#,
    );

    let (status, listed) = api.route_with_tenant(
        "GET",
        "/v1/golden-paths?taskFingerprint=governance-scope&evalProfile=release-gate",
        "",
        Some(10),
    );
    assert_eq!(status, 200, "{listed}");
    assert_json_contains(&listed, r#""count":1"#);

    let (status, health) = api.route_with_tenant(
        "POST",
        "/v1/golden-path-health",
        r#"{"goldenPathId":1,"filter":{"projectId":"eval-governance"},"limit":10,"examples":10}"#,
        Some(10),
    );
    assert_eq!(status, 200, "{health}");
    assert_json_contains(&health, r#""matchingTraceTotal":3"#);
    assert_json_contains(&health, r#""analyzedTraceTotal":3"#);
    assert_json_contains(&health, r#""followed":1"#);
    assert_json_contains(&health, r#""extended":1"#);
    assert_json_contains(&health, r#""deviated":1"#);
    assert_json_contains(&health, r#""usable":0.666667"#);
    assert_json_contains(&health, r#""stale":true"#);
    assert_json_contains(&health, r#""insufficient_samples""#);
    assert_json_contains(&health, r#""health_below_margin""#);
}

/// Golden Path 只保存 source trace 的引用和轻量 trajectory。
/// retention 可以清掉 source trace payload，但底座仍应能用保存的 trajectory 做 adherence/health。
#[test]
fn golden_path_adherence_survives_source_trace_retention_cleanup() {
    let dir = durable_dir("golden_path_retention");
    {
        let coord = WriteCoordinator::open_durable(&dir).unwrap();
        coord.recover();
        let api = EngineJsonApi::new(coord.clone());
        let batch = r#"[
          {
            "trace_id":501,
            "span_id":1,
            "ts":10,
            "seq":1,
            "event_type":2,
            "ext_span_id":"501-1",
            "status":0,
            "duration_ns":10,
            "tool_name":"planner",
            "attrs":{"project_id":"retained-source","task_fingerprint":"retention-golden","phase":"plan"}
          },
          {
            "trace_id":502,
            "span_id":1,
            "ts":2000,
            "seq":1,
            "event_type":2,
            "ext_span_id":"502-1",
            "status":0,
            "duration_ns":10,
            "tool_name":"planner",
            "attrs":{"project_id":"retained-source","task_fingerprint":"retention-golden","phase":"plan"}
          }
        ]"#;
        let (status, body) = api.route_with_tenant("POST", "/v1/ingest", batch, Some(11));
        assert_eq!(status, 200, "{body}");
        coord.flush_memtable();

        let (status, golden) = api.route_with_tenant(
            "POST",
            "/v1/golden-paths",
            r#"{"sourceTraceId":501,"taskFingerprint":"retention-golden","score":1000,"projectId":"retained-source","status":"confirmed"}"#,
            Some(11),
        );
        assert_eq!(status, 200, "{golden}");
        assert_json_contains(&golden, r#""source_trajectory_step_count":1"#);

        let apply = r#"{
          "filter":{"projectId":"retained-source"},
          "deleteBeforeTs":100,
          "protect":{"goldenPaths":false,"annotations":false,"datasetAssociations":false,"snapshots":false,"evalLinks":false,"pathMemory":false},
          "requestedBy":"eval-retention-test"
        }"#;
        let (status, applied) =
            api.route_with_tenant("POST", "/v1/retention/apply", apply, Some(11));
        assert_eq!(status, 200, "{applied}");
        assert_json_contains(&applied, r#""deletedTraceIds":["501"]"#);

        let (status, search) = api.route_with_tenant(
            "POST",
            "/v1/trace-search",
            r#"{"filter":{"projectId":"retained-source"}}"#,
            Some(11),
        );
        assert_eq!(status, 200, "{search}");
        assert_json_contains(&search, r#""total":1"#);
        assert!(!search.contains(r#""traceId":"501""#), "{search}");
        assert_json_contains(&search, r#""traceId":"502""#);

        let (status, adherence) = api.route_with_tenant(
            "POST",
            "/v1/path-adherence",
            r#"{"goldenPathId":1,"traceId":502}"#,
            Some(11),
        );
        assert_eq!(status, 200, "{adherence}");
        assert_json_contains(&adherence, r#""adherence":"followed""#);
        assert_json_contains(&adherence, r#""sourceAvailable":false"#);
        assert_json_contains(&adherence, r#""sourceRetained":true"#);
        assert_json_contains(&adherence, r#""sameSignature":true"#);
        assert_json_contains(&adherence, r#""storedSignatureMatchesSource":true"#);

        let (status, evidence) = api.route_with_tenant(
            "POST",
            "/v1/golden-path-evidence",
            r#"{"goldenPathId":1,"candidateTraceId":502}"#,
            Some(11),
        );
        assert_eq!(status, 200, "{evidence}");
        assert_json_contains(&evidence, r#""source":{"available":false"#);
        assert_json_contains(&evidence, r#""pathAdherence":{"goldenPath""#);
        assert_json_contains(&evidence, r#""traceDiff":null"#);

        let (status, health) = api.route_with_tenant(
            "POST",
            "/v1/golden-path-health",
            r#"{"goldenPathId":1,"filter":{"projectId":"retained-source"},"limit":10}"#,
            Some(11),
        );
        assert_eq!(status, 200, "{health}");
        assert_json_contains(&health, r#""sourceAvailable":false"#);
        assert_json_contains(&health, r#""sourceRetained":true"#);
        assert_json_contains(&health, r#""matchingTraceTotal":1"#);
        assert_json_contains(&health, r#""followed":1"#);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// span detail 应直接返回 LOG events；eval 写回之后也不能要求业务把日志镜像进 attrs。
#[test]
fn span_detail_log_events_round_trip_after_eval_writeback() {
    let coord = fresh();
    let api = EngineJsonApi::new(coord.clone());
    let batch = r#"[
      {
        "trace_id":601,
        "span_id":1,
        "ts":10,
        "seq":1,
        "event_type":1,
        "ext_span_id":"601-1",
        "input_text":"检查打包失败",
        "attrs":{"project_id":"log-events","task_fingerprint":"log-round-trip"}
      },
      {
        "trace_id":601,
        "span_id":1,
        "ts":11,
        "seq":2,
        "event_type":4,
        "ext_span_id":"601-1",
        "logs":["open repo","read package.json","发现 native binding 缺失"],
        "attrs":{"call_site":"pack.js:42","attempt":2,"nested":{"ok":true}}
      },
      {
        "trace_id":601,
        "span_id":1,
        "ts":12,
        "seq":3,
        "event_type":2,
        "ext_span_id":"601-1",
        "status":1,
        "duration_ns":2000,
        "output_text":"无法加载 darwin-arm64 node binding"
      }
    ]"#;
    let (status, body) = api.route_with_tenant("POST", "/v1/ingest", batch, Some(12));
    assert_eq!(status, 200, "{body}");

    let scorer = KeywordScorer::new(&["无法"]);
    let scored = coord.eval_and_writeback(&scorer, &TraceQuery::all());
    assert_eq!(scored.len(), 1);
    assert_eq!(scored[0].outcome.score, 0);

    let (status, detail) = api.route_with_tenant("GET", "/v1/traces/601/spans/1", "", Some(12));
    assert_eq!(status, 200, "{detail}");
    assert_json_contains(&detail, r#""logEvents":[{"#);
    assert_json_contains(&detail, r#""eventOrdinal":0"#);
    assert_json_contains(&detail, r#""eventType":4"#);
    assert_json_contains(
        &detail,
        r#""messages":["open repo","read package.json","发现 native binding 缺失"]"#,
    );
    assert_json_contains(&detail, r#""call_site":"pack.js:42""#);
    assert_json_contains(&detail, r#""attempt":2"#);
    assert_json_contains(&detail, r#""nested":{"ok":true}"#);
}

/// 后验 annotation / dataset association 是 eval 和 golden path 的证据层。
/// 它们要能反向过滤 trace/session，并在 retention 中保护仍有效的证据引用。
#[test]
fn eval_metadata_lifecycle_reverse_filters_and_retention_guards() {
    let dir = durable_dir("metadata_eval");
    {
        let coord = WriteCoordinator::open_durable(&dir).unwrap();
        coord.recover();
        let api = EngineJsonApi::new(coord.clone());
        let batch = r#"[
          {
            "trace_id":"meta-run-1",
            "span_id":"meta-span-1",
            "session_id":"meta-session",
            "ts":10,
            "seq":1,
            "event_type":2,
            "ext_span_id":"meta-span-1",
            "status":0,
            "duration_ns":10,
            "agent_name":"eval-agent",
            "output_text":"验证通过，可以沉淀为回归样本",
            "attrs":{"project_id":"metadata-eval","skill":"review","task_fingerprint":"metadata-task"}
          },
          {
            "trace_id":"meta-run-2",
            "span_id":"meta-span-2",
            "session_id":"meta-session",
            "ts":20,
            "seq":1,
            "event_type":2,
            "ext_span_id":"meta-span-2",
            "status":1,
            "duration_ns":10,
            "agent_name":"eval-agent",
            "output_text":"无法完成验证",
            "attrs":{"project_id":"metadata-eval","skill":"review","task_fingerprint":"metadata-task"}
          },
          {
            "trace_id":"meta-run-3",
            "span_id":"meta-span-3",
            "session_id":"meta-session",
            "ts":300,
            "seq":1,
            "event_type":2,
            "ext_span_id":"meta-span-3",
            "status":0,
            "duration_ns":10,
            "agent_name":"eval-agent",
            "output_text":"新 trace 不应被旧 TTL 清理",
            "attrs":{"project_id":"metadata-eval","skill":"review","task_fingerprint":"metadata-task"}
          }
        ]"#;
        let (status, body) = api.route_with_tenant("POST", "/v1/ingest", batch, Some(13));
        assert_eq!(status, 200, "{body}");

        let scorer = KeywordScorer::new(&["无法"]);
        let scored = coord.eval_and_writeback(&scorer, &TraceQuery::all());
        assert!(scored.iter().any(|s| s.trace_id != 0 && s.outcome.score == 0));
        coord.flush_memtable();

        let (status, annotation) = api.route_with_tenant(
            "POST",
            "/v1/annotations",
            r#"{"traceId":"meta-run-1","spanId":"meta-span-1","target":"span","label":"best_path","score":950,"source":"human","projectId":"metadata-eval","skill":"review"}"#,
            Some(13),
        );
        assert_eq!(status, 200, "{annotation}");
        assert_json_contains(&annotation, r#""annotationId":"1""#);
        assert_json_contains(&annotation, r#""externalTraceId":"meta-run-1""#);

        let (status, dataset) = api.route_with_tenant(
            "POST",
            "/v1/dataset-associations",
            r#"{"datasetId":"eval-regression","itemId":"case-1","traceId":"meta-run-1","spanId":"meta-span-1","snapshotId":"snap-1","snapshotHash":"fnv1a64:meta","evalRunId":"eval-1","label":"pass","score":940,"projectId":"metadata-eval"}"#,
            Some(13),
        );
        assert_eq!(status, 200, "{dataset}");
        assert_json_contains(&dataset, r#""datasetId":"eval-regression""#);

        let (status, bad_annotation) = api.route_with_tenant(
            "POST",
            "/v1/annotations",
            r#"{"traceId":"meta-run-2","spanId":"meta-span-2","target":"span","label":"bad_answer","score":100,"source":"eval","projectId":"metadata-eval"}"#,
            Some(13),
        );
        assert_eq!(status, 200, "{bad_annotation}");
        assert_json_contains(&bad_annotation, r#""annotationId":"2""#);

        let (status, deleted) = api.route_with_tenant(
            "DELETE",
            "/v1/annotations/2",
            r#"{"reviewer":"four","reason":"superseded by fixed run"}"#,
            Some(13),
        );
        assert_eq!(status, 200, "{deleted}");
        assert_json_contains(&deleted, r#""status":"deleted""#);

        let (status, hidden_annotation) = api.route_with_tenant(
            "GET",
            "/v1/annotations?traceId=meta-run-2&label=bad_answer",
            "",
            Some(13),
        );
        assert_eq!(status, 200, "{hidden_annotation}");
        assert_json_contains(&hidden_annotation, r#""count":0"#);

        let (status, deleted_annotation) = api.route_with_tenant(
            "GET",
            "/v1/annotations?traceId=meta-run-2&label=bad_answer&status=deleted",
            "",
            Some(13),
        );
        assert_eq!(status, 200, "{deleted_annotation}");
        assert_json_contains(&deleted_annotation, r#""count":1"#);
        assert_json_contains(&deleted_annotation, r#""reviewer":"four""#);

        let (status, by_annotation) = api.route_with_tenant(
            "POST",
            "/v1/trace-search",
            r#"{"filter":{"annotation":{"label":"best_path","source":"human","scoreMin":900}}}"#,
            Some(13),
        );
        assert_eq!(status, 200, "{by_annotation}");
        assert_json_contains(&by_annotation, r#""total":1"#);
        assert_json_contains(&by_annotation, r#""externalSpanId":"meta-span-1""#);
        assert_json_contains(&by_annotation, r#""index":"metadata_filter+folded_scan""#);

        let (status, by_deleted_annotation) = api.route_with_tenant(
            "POST",
            "/v1/trace-search",
            r#"{"filter":{"annotation":{"label":"bad_answer"}}}"#,
            Some(13),
        );
        assert_eq!(status, 200, "{by_deleted_annotation}");
        assert_json_contains(&by_deleted_annotation, r#""total":0"#);

        let (status, by_dataset) = api.route_with_tenant(
            "GET",
            "/v1/sessions?datasetId=eval-regression&datasetLabel=pass",
            "",
            Some(13),
        );
        assert_eq!(status, 200, "{by_dataset}");
        assert_json_contains(&by_dataset, r#""externalSessionId":"meta-session""#);

        let (status, traces) = api.route_with_tenant(
            "GET",
            "/v1/traces?annotationLabel=best_path&annotationScoreMin=900",
            "",
            Some(13),
        );
        assert_eq!(status, 200, "{traces}");
        assert_json_contains(&traces, r#""external_trace_id":"meta-run-1""#);

        let retention_query =
            r#"{"filter":{"projectId":"metadata-eval"},"deleteBeforeTs":100}"#;
        let (status, plan) =
            api.route_with_tenant("POST", "/v1/retention-plan", retention_query, Some(13));
        assert_eq!(status, 200, "{plan}");
        assert_json_contains(&plan, r#""candidates":{"traceCount":2"#);
        assert_json_contains(&plan, r#""protected":{"traceCount":1"#);
        assert_json_contains(&plan, r#""deletable":{"traceCount":1"#);
        assert_json_contains(&plan, r#""annotation""#);
        assert_json_contains(&plan, r#""datasetAssociation""#);

        let apply = r#"{"filter":{"projectId":"metadata-eval"},"deleteBeforeTs":100,"compact":true,"requestedBy":"eval-metadata-retention","reason":"ttl cleanup"}"#;
        let (status, applied) =
            api.route_with_tenant("POST", "/v1/retention/apply", apply, Some(13));
        assert_eq!(status, 200, "{applied}");
        assert_json_contains(&applied, r#""deletedTraceCount":1"#);
        assert_json_contains(&applied, r#""source":"eval-metadata-retention""#);

        let (status, after) = api.route_with_tenant(
            "POST",
            "/v1/trace-search",
            r#"{"filter":{"projectId":"metadata-eval"}}"#,
            Some(13),
        );
        assert_eq!(status, 200, "{after}");
        assert_json_contains(&after, r#""total":2"#);
        assert_json_contains(&after, r#""externalTraceId":"meta-run-1""#);
        assert_json_contains(&after, r#""externalTraceId":"meta-run-3""#);
        assert!(!after.contains(r#""externalTraceId":"meta-run-2""#), "{after}");
    }
    {
        let coord = WriteCoordinator::open_durable(&dir).unwrap();
        coord.recover();
        let api = EngineJsonApi::new(coord);
        let (status, audits) = api.route_with_tenant(
            "GET",
            "/v1/retention-audits?source=eval-metadata-retention",
            "",
            Some(13),
        );
        assert_eq!(status, 200, "{audits}");
        assert_json_contains(&audits, r#""total":1"#);
        assert_json_contains(&audits, r#""deletedTraceCount":1"#);

        let (status, dataset) = api.route_with_tenant(
            "GET",
            "/v1/dataset-associations?datasetId=eval-regression&itemId=case-1",
            "",
            Some(13),
        );
        assert_eq!(status, 200, "{dataset}");
        assert_json_contains(&dataset, r#""count":1"#);
        assert_json_contains(&dataset, r#""snapshotHash":"fnv1a64:meta""#);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// loop/task/trajectory 三组 read model 都应该复用同一套高频字段、metadata 和 attrs 过滤语义。
#[test]
fn eval_loop_task_and_trajectory_read_models_share_filters_and_scores() {
    let coord = fresh();
    let api = EngineJsonApi::new(coord.clone());
    let batch = r#"[
      {
        "trace_id":801,
        "span_id":1,
        "ts":10,
        "seq":1,
        "event_type":2,
        "ext_span_id":"801-1",
        "session_id":8801,
        "status":0,
        "duration_ns":10,
        "tool_name":"planner",
        "input_tokens":10,
        "output_tokens":5,
        "total_tokens":15,
        "cost_usd_nanos":1000,
        "output_text":"规划完成",
        "attrs":{"project_id":"loop-eval","skill":"packaging","mode":"auto","task_fingerprint":"native-pack","loop_id":"loop-native","harness_version":"h2","validation_status":"pass","phase":"plan"}
      },
      {
        "trace_id":801,
        "span_id":2,
        "ts":20,
        "seq":1,
        "event_type":2,
        "ext_span_id":"801-2",
        "session_id":8801,
        "status":0,
        "duration_ns":20,
        "tool_name":"tester",
        "input_tokens":20,
        "output_tokens":10,
        "total_tokens":30,
        "cost_usd_nanos":2000,
        "output_text":"npm test passed",
        "attrs":{"project_id":"loop-eval","skill":"packaging","mode":"auto","task_fingerprint":"native-pack","loop_id":"loop-native","harness_version":"h2","validation_status":"pass","phase":"verify","validator":"npm test"}
      },
      {
        "trace_id":802,
        "span_id":1,
        "ts":30,
        "seq":1,
        "event_type":2,
        "ext_span_id":"802-1",
        "session_id":8802,
        "status":0,
        "duration_ns":10,
        "tool_name":"planner",
        "input_tokens":15,
        "output_tokens":5,
        "total_tokens":20,
        "cost_usd_nanos":1500,
        "output_text":"规划完成",
        "attrs":{"project_id":"loop-eval","skill":"packaging","mode":"auto","task_fingerprint":"native-pack","loop_id":"loop-native","harness_version":"h2","validation_status":"fail","phase":"plan"}
      },
      {
        "trace_id":802,
        "span_id":2,
        "ts":40,
        "seq":1,
        "event_type":2,
        "ext_span_id":"802-2",
        "session_id":8802,
        "status":1,
        "duration_ns":30,
        "tool_name":"fallback",
        "input_tokens":30,
        "output_tokens":10,
        "total_tokens":40,
        "cost_usd_nanos":3000,
        "output_text":"无法找到 native binding",
        "attrs":{"project_id":"loop-eval","skill":"packaging","mode":"auto","task_fingerprint":"native-pack","loop_id":"loop-native","harness_version":"h2","validation_status":"fail","stop_reason":"error","phase":"fallback"}
      },
      {
        "trace_id":803,
        "span_id":1,
        "ts":50,
        "seq":1,
        "event_type":2,
        "ext_span_id":"803-1",
        "session_id":8803,
        "status":0,
        "duration_ns":10,
        "tool_name":"planner",
        "attrs":{"project_id":"loop-eval","skill":"review","mode":"manual","task_fingerprint":"other-task","loop_id":"loop-other","validation_status":"pass","phase":"plan"}
      }
    ]"#;
    let (status, body) = api.route_with_tenant("POST", "/v1/ingest", batch, Some(14));
    assert_eq!(status, 200, "{body}");

    let scorer = KeywordScorer::new(&["无法"]);
    let scored = coord.eval_and_writeback(&scorer, &TraceQuery::all());
    assert!(scored.iter().any(|s| s.trace_id == 802 && s.outcome.score == 0));

    let (status, annotation) = api.route_with_tenant(
        "POST",
        "/v1/annotations",
        r#"{"traceId":801,"label":"best_path","score":960,"source":"human","projectId":"loop-eval"}"#,
        Some(14),
    );
    assert_eq!(status, 200, "{annotation}");
    let (status, dataset) = api.route_with_tenant(
        "POST",
        "/v1/dataset-associations",
        r#"{"datasetId":"loop-regression","itemId":"native-pack-pass","traceId":801,"label":"pass","score":940,"projectId":"loop-eval"}"#,
        Some(14),
    );
    assert_eq!(status, 200, "{dataset}");

    let (status, loops) = api.route_with_tenant(
        "GET",
        "/v1/loops?taskFingerprint=native-pack&projectId=loop-eval",
        "",
        Some(14),
    );
    assert_eq!(status, 200, "{loops}");
    assert_json_contains(&loops, r#""total":1"#);
    assert_json_contains(&loops, r#""loopId":"loop-native""#);
    assert_json_contains(&loops, r#""traceCount":2"#);
    assert_json_contains(&loops, r#""errorCount":1"#);
    assert_json_contains(&loops, r#""phases":["fallback","plan","verify"]"#);

    let (status, loop_detail) =
        api.route_with_tenant("GET", "/v1/loops/loop-native", "", Some(14));
    assert_eq!(status, 200, "{loop_detail}");
    assert_json_contains(&loop_detail, r#""summary":{"loopId":"loop-native""#);
    assert_json_contains(&loop_detail, r#""traceId":"801""#);
    assert_json_contains(&loop_detail, r#""traceId":"802""#);

    let (status, task_traces) = api.route_with_tenant(
        "GET",
        "/v1/tasks/native-pack/traces?validationStatus=pass&projectId=loop-eval",
        "",
        Some(14),
    );
    assert_eq!(status, 200, "{task_traces}");
    assert_json_contains(&task_traces, r#""total":1"#);
    assert_json_contains(&task_traces, r#""traceId":"801""#);
    assert!(!task_traces.contains(r#""traceId":"802""#), "{task_traces}");

    let (status, trajectories) = api.route_with_tenant(
        "POST",
        "/v1/trace-trajectories",
        r#"{"filter":{"taskFingerprint":"native-pack","projectId":"loop-eval"},"limit":10}"#,
        Some(14),
    );
    assert_eq!(status, 200, "{trajectories}");
    assert_json_contains(&trajectories, r#""total":2"#);
    assert_json_contains(&trajectories, r#""spanTotal":4"#);
    assert_json_contains(&trajectories, r#""index":"materialized""#);
    assert_json_contains(&trajectories, r#""trajectory":{"signature":"fnv1a64:"#);

    let (status, groups) = api.route_with_tenant(
        "POST",
        "/v1/trajectory-groups",
        r#"{"filter":{"taskFingerprint":"native-pack","projectId":"loop-eval"},"sort":"best","limit":10}"#,
        Some(14),
    );
    assert_eq!(status, 200, "{groups}");
    assert_json_contains(&groups, r#""total":2"#);
    assert_json_contains(&groups, r#""traceTotal":2"#);
    assert_json_contains(&groups, r#""spanTotal":4"#);
    assert_json_contains(&groups, r#""index":"attrs_postings+folded_verify""#);
    assert_json_contains(&groups, r#""trajectoryIndex":"materialized_cache""#);
    assert_json_contains(&groups, r#""annotation":{"count":1,"avg":960"#);
    assert_json_contains(&groups, r#""dataset":{"count":1,"avg":940"#);
}

/// Golden Path 需要覆盖完整候选资产生命周期：确认、challenger、evidence、export 和 health。
#[test]
fn eval_golden_path_full_lifecycle_exports_evidence_and_challenger_health() {
    let coord = fresh();
    let api = EngineJsonApi::new(coord);
    let batch = r#"[
      {
        "trace_id":901,
        "span_id":1,
        "ts":10,
        "seq":1,
        "event_type":2,
        "ext_span_id":"901-1",
        "status":0,
        "duration_ns":10,
        "tool_name":"planner",
        "model":"qwen",
        "provider":"openai",
        "attrs":{"project_id":"golden-eval","skill":"review","mode":"auto","task_fingerprint":"golden-task","harness_version":"h3","schema_fingerprint":"s3","phase":"plan"}
      },
      {
        "trace_id":901,
        "span_id":2,
        "ts":20,
        "seq":1,
        "event_type":2,
        "ext_span_id":"901-2",
        "status":0,
        "duration_ns":20,
        "tool_name":"tester",
        "model":"qwen",
        "provider":"openai",
        "attrs":{"project_id":"golden-eval","skill":"review","mode":"auto","task_fingerprint":"golden-task","harness_version":"h3","schema_fingerprint":"s3","phase":"verify","validator":"npm test"}
      },
      {
        "trace_id":902,
        "span_id":1,
        "ts":30,
        "seq":1,
        "event_type":2,
        "ext_span_id":"902-1",
        "status":0,
        "duration_ns":10,
        "tool_name":"planner",
        "model":"qwen",
        "provider":"openai",
        "attrs":{"project_id":"golden-eval","skill":"review","mode":"auto","task_fingerprint":"golden-task","harness_version":"h3","schema_fingerprint":"s3","phase":"plan"}
      },
      {
        "trace_id":902,
        "span_id":2,
        "ts":40,
        "seq":1,
        "event_type":2,
        "ext_span_id":"902-2",
        "status":0,
        "duration_ns":20,
        "tool_name":"tester",
        "model":"qwen",
        "provider":"openai",
        "attrs":{"project_id":"golden-eval","skill":"review","mode":"auto","task_fingerprint":"golden-task","harness_version":"h3","schema_fingerprint":"s3","phase":"verify","validator":"npm test"}
      },
      {
        "trace_id":903,
        "span_id":1,
        "ts":50,
        "seq":1,
        "event_type":2,
        "ext_span_id":"903-1",
        "status":0,
        "duration_ns":10,
        "tool_name":"planner",
        "model":"qwen",
        "provider":"openai",
        "attrs":{"project_id":"golden-eval","skill":"review","mode":"auto","task_fingerprint":"golden-task","harness_version":"h3","schema_fingerprint":"s3","phase":"plan"}
      },
      {
        "trace_id":903,
        "span_id":2,
        "ts":60,
        "seq":1,
        "event_type":2,
        "ext_span_id":"903-2",
        "status":0,
        "duration_ns":20,
        "tool_name":"tester",
        "model":"qwen",
        "provider":"openai",
        "attrs":{"project_id":"golden-eval","skill":"review","mode":"auto","task_fingerprint":"golden-task","harness_version":"h3","schema_fingerprint":"s3","phase":"verify","validator":"npm test"}
      },
      {
        "trace_id":903,
        "span_id":3,
        "ts":70,
        "seq":1,
        "event_type":2,
        "ext_span_id":"903-3",
        "status":0,
        "duration_ns":15,
        "tool_name":"exporter",
        "model":"qwen",
        "provider":"openai",
        "attrs":{"project_id":"golden-eval","skill":"review","mode":"auto","task_fingerprint":"golden-task","harness_version":"h3","schema_fingerprint":"s3","phase":"export"}
      }
    ]"#;
    let (status, body) = api.route_with_tenant("POST", "/v1/ingest", batch, Some(15));
    assert_eq!(status, 200, "{body}");

    let (status, annotation) = api.route_with_tenant(
        "POST",
        "/v1/annotations",
        r#"{"traceId":901,"label":"golden_source","score":980,"source":"human","projectId":"golden-eval"}"#,
        Some(15),
    );
    assert_eq!(status, 200, "{annotation}");
    let (status, dataset) = api.route_with_tenant(
        "POST",
        "/v1/dataset-associations",
        r#"{"datasetId":"golden-regression","itemId":"golden-case","traceId":901,"label":"pass","score":970,"projectId":"golden-eval"}"#,
        Some(15),
    );
    assert_eq!(status, 200, "{dataset}");

    let create = r#"{
      "sourceTraceId":901,
      "taskFingerprint":"golden-task",
      "score":980,
      "label":"stable path",
      "source":"eval_harness",
      "evalProfile":"release-gate",
      "minSampleCount":2,
      "marginScore":900,
      "comparisonWindowNs":1000,
      "projectId":"golden-eval"
    }"#;
    let (status, golden) = api.route_with_tenant("POST", "/v1/golden-paths", create, Some(15));
    assert_eq!(status, 200, "{golden}");
    assert_json_contains(&golden, r#""goldenPathId":"1""#);
    assert_json_contains(&golden, r#""sourceTrajectory":{"signature":"fnv1a64:"#);
    assert_json_contains(&golden, r#""source_trajectory_step_count":2"#);

    let (status, confirmed) = api.route_with_tenant(
        "POST",
        "/v1/golden-paths/1/status",
        r#"{"status":"confirmed","reason":"accepted by eval","source":"eval_harness"}"#,
        Some(15),
    );
    assert_eq!(status, 200, "{confirmed}");
    assert_json_contains(&confirmed, r#""status":"confirmed""#);

    let challenger = r#"{
      "sourceTraceId":903,
      "taskFingerprint":"golden-task",
      "score":960,
      "label":"extended challenger",
      "challengerOf":1,
      "evalProfile":"release-gate",
      "projectId":"golden-eval"
    }"#;
    let (status, challenger_body) =
        api.route_with_tenant("POST", "/v1/golden-paths", challenger, Some(15));
    assert_eq!(status, 200, "{challenger_body}");
    assert_json_contains(&challenger_body, r#""goldenPathId":"2""#);
    assert_json_contains(&challenger_body, r#""challengerOf":"1""#);

    let (status, challengers) = api.route_with_tenant(
        "GET",
        "/v1/golden-paths?challengerOf=1&evalProfile=release-gate",
        "",
        Some(15),
    );
    assert_eq!(status, 200, "{challengers}");
    assert_json_contains(&challengers, r#""count":1"#);
    assert_json_contains(&challengers, r#""goldenPathId":"2""#);

    let (status, adherence) = api.route_with_tenant(
        "POST",
        "/v1/path-adherence",
        r#"{"goldenPathId":1,"traceId":902}"#,
        Some(15),
    );
    assert_eq!(status, 200, "{adherence}");
    assert_json_contains(&adherence, r#""adherence":"followed""#);
    assert_json_contains(&adherence, r#""sameSignature":true"#);

    let (status, evidence) = api.route_with_tenant(
        "POST",
        "/v1/golden-path-evidence",
        r#"{"goldenPathId":1,"candidateTraceId":903}"#,
        Some(15),
    );
    assert_eq!(status, 200, "{evidence}");
    assert_json_contains(&evidence, r#""source":{"available":true"#);
    assert_json_contains(&evidence, r#""annotationCount":1"#);
    assert_json_contains(&evidence, r#""datasetAssociationCount":1"#);
    assert_json_contains(&evidence, r#""adherence":"extended""#);
    assert_json_contains(&evidence, r#""traceDiff":{"left""#);

    let (status, export_page) = api.route_with_tenant(
        "POST",
        "/v1/golden-path-export",
        r#"{"filter":{"taskFingerprint":"golden-task","projectId":"golden-eval"},"limit":10}"#,
        Some(15),
    );
    assert_eq!(status, 200, "{export_page}");
    assert_json_contains(&export_page, r#""schemaVersion":"yitrace.golden_path_export.v1""#);
    assert_json_contains(&export_page, r#""count":1"#);
    assert_json_contains(&export_page, r#""recordType":"golden_path""#);
    assert_json_contains(&export_page, r#""jsonl":"{\"schemaVersion\":\"yitrace.golden_path_export.v1\""#);

    let (status, health) = api.route_with_tenant(
        "POST",
        "/v1/golden-path-health",
        r#"{"goldenPathId":1,"filter":{"projectId":"golden-eval"},"limit":10,"examples":10}"#,
        Some(15),
    );
    assert_eq!(status, 200, "{health}");
    assert_json_contains(&health, r#""matchingTraceTotal":2"#);
    assert_json_contains(&health, r#""followed":1"#);
    assert_json_contains(&health, r#""extended":1"#);
    assert_json_contains(&health, r#""usable":1.000000"#);
    assert_json_contains(&health, r#""governance":{"evalProfile":"release-gate""#);
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
