use super::generator::DatasetStats;

#[derive(Clone)]
pub struct Query {
    pub name: &'static str,
    pub selectivity: &'static str,
    pub count: usize,
    pub method: &'static str,
    pub path: String,
    pub body: String,
    pub expected_fragments: Vec<String>,
    pub expected_read_source: Option<&'static str>,
    pub expect_point_lookup: bool,
}

pub fn build_queries(count: usize, dataset: &DatasetStats) -> Vec<Query> {
    let trace_id = (dataset.traces / 2).max(1);
    let scan_count = match dataset.spans {
        1_000_000.. => count.min(5),
        100_000.. => count.min(20),
        _ => count,
    };
    let count = if dataset.spans >= 1_000_000 {
        count.min(20)
    } else {
        count
    };
    let rare_fragments = if dataset.spans >= 997 {
        vec!["\"trace_id\"".to_string()]
    } else {
        vec!["[".to_string()]
    };
    vec![
        Query {
            name: "search_common_text",
            selectivity: "low",
            count: scan_count,
            method: "POST",
            path: "/v1/search".to_string(),
            body: r#"{"text":"任务执行","k":10}"#.to_string(),
            expected_fragments: vec!["\"trace_id\"".to_string()],
            expected_read_source: None,
            expect_point_lookup: false,
        },
        Query {
            name: "search_common_text_project",
            selectivity: "medium",
            count: scan_count,
            method: "POST",
            path: "/v1/search".to_string(),
            body: r#"{"text":"任务执行","k":10,"filter":{"attrs":{"project_id":"scale-a"}}}"#.to_string(),
            expected_fragments: vec!["\"trace_id\"".to_string()],
            expected_read_source: None,
            expect_point_lookup: false,
        },
        Query {
            name: "trace_search_common_text_point",
            selectivity: "low",
            count: scan_count,
            method: "POST",
            path: "/v1/trace-search".to_string(),
            body: r#"{"text":"任务执行","limit":10}"#.to_string(),
            expected_fragments: vec![
                "\"usedFilterIndex\":true".to_string(),
                "\"fallbackReason\":null".to_string(),
            ],
            expected_read_source: Some("filter_index"),
            expect_point_lookup: true,
        },
        Query {
            name: "search_rare_text",
            selectivity: "high",
            count,
            method: "POST",
            path: "/v1/search".to_string(),
            body: r#"{"text":"月蚀校验码","k":10}"#.to_string(),
            expected_fragments: rare_fragments,
            expected_read_source: None,
            expect_point_lookup: false,
        },
        Query {
            name: "trace_search_low_cardinality",
            selectivity: "low",
            count,
            method: "POST",
            path: "/v1/trace-search".to_string(),
            body: r#"{"filter":{"projectId":"scale-a"},"limit":20}"#.to_string(),
            expected_fragments: vec![
                "\"readPlan\"".to_string(),
                "\"usedFilterIndex\":true".to_string(),
            ],
            expected_read_source: Some("trajectory_rollup"),
            expect_point_lookup: false,
        },
        Query {
            name: "trace_search_high_cardinality",
            selectivity: "high",
            count,
            method: "POST",
            path: "/v1/trace-search".to_string(),
            body: r#"{"filter":{"projectId":"scale-a","taskFingerprint":"risk-review"},"limit":20}"#.to_string(),
            expected_fragments: vec![
                "\"readPlan\"".to_string(),
                "\"usedFilterIndex\":true".to_string(),
            ],
            expected_read_source: Some("trajectory_rollup"),
            expect_point_lookup: false,
        },
        Query {
            name: "trace_search_text_tenant_index",
            selectivity: "medium",
            count: scan_count,
            method: "POST",
            path: "/v1/trace-search".to_string(),
            body: r#"{"text":"支付风控","limit":20}"#.to_string(),
            expected_fragments: vec![
                "\"usedFilterIndex\":true".to_string(),
                "\"fallbackReason\":null".to_string(),
            ],
            expected_read_source: Some("filter_index"),
            expect_point_lookup: true,
        },
        Query {
            name: "trace_aggregate_rollup",
            selectivity: "low",
            count,
            method: "POST",
            path: "/v1/trace-aggregate".to_string(),
            body: r#"{"filter":{"projectId":"scale-a"},"groupBy":["validationStatus","toolName"],"limit":20}"#.to_string(),
            expected_fragments: vec!["\"groupBy\"".to_string()],
            expected_read_source: Some("aggregate_rollup"),
            expect_point_lookup: false,
        },
        Query {
            name: "storage_stats_rollup",
            selectivity: "low",
            count,
            method: "POST",
            path: "/v1/storage-stats".to_string(),
            body: r#"{"filter":{"projectId":"scale-a"},"groupBy":["projectId","validationStatus"]}"#.to_string(),
            expected_fragments: vec!["\"spanCount\"".to_string()],
            expected_read_source: Some("trajectory_rollup"),
            expect_point_lookup: false,
        },
        Query {
            name: "trace_trajectories_rollup",
            selectivity: "high",
            count,
            method: "POST",
            path: "/v1/trace-trajectories".to_string(),
            body: r#"{"filter":{"projectId":"scale-a","taskFingerprint":"risk-review"},"limit":20}"#.to_string(),
            expected_fragments: vec!["\"items\"".to_string()],
            expected_read_source: Some("trajectory_rollup"),
            expect_point_lookup: false,
        },
        Query {
            name: "trajectory_groups_rollup",
            selectivity: "high",
            count,
            method: "POST",
            path: "/v1/trajectory-groups".to_string(),
            body: r#"{"filter":{"projectId":"scale-a","taskFingerprint":"risk-review"},"limit":20}"#.to_string(),
            expected_fragments: vec!["\"items\"".to_string()],
            expected_read_source: Some("trajectory_rollup"),
            expect_point_lookup: false,
        },
        Query {
            name: "loops_page_rollup",
            selectivity: "high",
            count,
            method: "GET",
            path: "/v1/loops?cursor=0&limit=20&project_id=scale-a&taskFingerprint=risk-review".to_string(),
            body: String::new(),
            expected_fragments: vec!["\"items\"".to_string()],
            expected_read_source: Some("trajectory_rollup"),
            expect_point_lookup: false,
        },
        Query {
            name: "task_traces_rollup",
            selectivity: "medium",
            count,
            method: "GET",
            path: "/v1/tasks/risk-review/traces?cursor=0&limit=20&validationStatus=pass".to_string(),
            body: String::new(),
            expected_fragments: vec!["\"items\"".to_string()],
            expected_read_source: Some("trajectory_rollup"),
            expect_point_lookup: false,
        },
        Query {
            name: "sessions_page_index",
            selectivity: "low",
            count: scan_count,
            method: "GET",
            path: "/v1/sessions?cursor=0&limit=50&project_id=scale-a".to_string(),
            body: String::new(),
            expected_fragments: vec!["\"items\"".to_string()],
            expected_read_source: None,
            expect_point_lookup: false,
        },
        Query {
            name: "trace_detail",
            selectivity: "point",
            count: scan_count,
            method: "GET",
            path: format!("/v1/traces/{trace_id}"),
            body: String::new(),
            expected_fragments: vec![format!("\"externalTraceId\":\"run-{trace_id}\"")],
            expected_read_source: None,
            expect_point_lookup: false,
        },
        Query {
            name: "trace_diff",
            selectivity: "point",
            count,
            method: "POST",
            path: "/v1/traces/diff".to_string(),
            body: format!(
                r#"{{"baseTraceId":{trace_id},"candidateTraceId":{},"includeSteps":false}}"#,
                (trace_id + 1).min(dataset.traces.max(1))
            ),
            expected_fragments: vec!["\"sameSignature\"".to_string()],
            expected_read_source: None,
            expect_point_lookup: false,
        },
    ]
}
