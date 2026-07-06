use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use yt_engine::{
    EngineJsonApi, InProcessReplicaSpec, InProcessShardSpec, ShardId, WireRecord, WriteCoordinator,
};

fn durable_dir(name: &str) -> std::path::PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "yt_replica_eval_{name}_{}_{}",
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

fn extract_json_object_field(body: &str, field: &str) -> String {
    let needle = format!("\"{field}\":");
    let value_start = body
        .find(&needle)
        .unwrap_or_else(|| panic!("missing JSON field {field} in {body}"))
        + needle.len();
    let object_start = body[value_start..]
        .find('{')
        .map(|pos| value_start + pos)
        .unwrap_or_else(|| panic!("field {field} is not an object in {body}"));
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, ch) in body[object_start..].char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return body[object_start..=object_start + offset].to_string();
                }
            }
            _ => {}
        }
    }
    panic!("unterminated object field {field} in {body}");
}

fn trace_events(trace_id: u64, project: &str, output: &str) -> Vec<WireRecord> {
    let span_id = 1;
    let ext_span_id = format!("{trace_id}-{span_id}");
    let mut attrs = BTreeMap::new();
    attrs.insert("project_id".to_string(), json_str(project));
    attrs.insert("skill".to_string(), json_str("replica-status"));
    vec![
        WireRecord {
            trace_id,
            span_id,
            ts: trace_id as i64,
            seq: 1,
            event_type_tag: 1,
            ext_span_id: ext_span_id.clone(),
            parent_span_id: None,
            status: None,
            duration_ns: None,
            input_tokens: Some(10),
            output_tokens: None,
            cached_input_tokens: None,
            reasoning_tokens: None,
            total_tokens: None,
            cost_usd_nanos: None,
            cost_currency: None,
            provider: None,
            session_id: Some(trace_id / 10),
            tenant_id: None,
            external_trace_id: None,
            external_span_id: None,
            external_parent_span_id: None,
            external_session_id: None,
            agent_name: Some("replica-eval".to_string()),
            tool_name: None,
            model: Some("qwen-max".to_string()),
            input_text: Some("replica freshness eval".to_string()),
            output_text: None,
            logs: Vec::new(),
            attrs,
        },
        WireRecord {
            trace_id,
            span_id,
            ts: trace_id as i64 + 1,
            seq: 2,
            event_type_tag: 2,
            ext_span_id,
            parent_span_id: None,
            status: Some(0),
            duration_ns: Some(1_000_000),
            input_tokens: None,
            output_tokens: Some(5),
            cached_input_tokens: None,
            reasoning_tokens: None,
            total_tokens: None,
            cost_usd_nanos: None,
            cost_currency: None,
            provider: None,
            session_id: Some(trace_id / 10),
            tenant_id: None,
            external_trace_id: None,
            external_span_id: None,
            external_parent_span_id: None,
            external_session_id: None,
            agent_name: Some("replica-eval".to_string()),
            tool_name: None,
            model: Some("qwen-max".to_string()),
            input_text: None,
            output_text: Some(output.to_string()),
            logs: Vec::new(),
            attrs: BTreeMap::new(),
        },
    ]
}

/// 分布式 L2 底座：同一 shard 可以注册 follower，cluster status 必须暴露
/// follower 是否能被读路由使用，而不是只告诉调用方“有几个 shard”。
#[test]
fn cluster_shard_status_reports_follower_freshness_budget() {
    let leader_dir = durable_dir("leader");
    let follower_dir = durable_dir("follower");
    {
        let leader = WriteCoordinator::open_durable(&leader_dir).unwrap();
        leader.recover();
        let follower = WriteCoordinator::open_durable(&follower_dir).unwrap();
        follower.recover();

        leader.ingest_wire_for_tenant(
            trace_events(71_001, "replica-cluster", "first event on follower"),
            Some(710),
        );
        let first = leader.export_wal_after(0);
        follower.apply_wal_replication_batch(&first).unwrap();

        leader.ingest_wire_for_tenant(
            trace_events(71_002, "replica-cluster", "leader ahead of follower"),
            Some(710),
        );

        let strict =
            EngineJsonApi::new_in_process_cluster_with_replicas(vec![InProcessShardSpec::new(
                ShardId::new("tenant-710-shard-0"),
                leader.clone(),
            )
            .with_replica(InProcessReplicaSpec::new(
                "tenant-710-shard-0-follower-0",
                follower.clone(),
                0,
            ))])
            .unwrap();
        let (status, stale) = strict.route("GET", "/v1/cluster/shards", "");
        assert_eq!(status, 200, "{stale}");
        assert_json_contains(&stale, r#""replicaCount":1"#);
        assert_json_contains(&stale, r#""replicaId":"tenant-710-shard-0-follower-0""#);
        assert_json_contains(&stale, r#""role":"follower""#);
        assert_json_contains(&stale, r#""readable":false"#);
        assert_json_contains(&stale, r#""syncState":"stale""#);
        assert_json_contains(&stale, r#""replicationLagLsn":2"#);
        assert_json_contains(&stale, r#""reason":"lag_exceeds_budget""#);
        assert_json_contains(&stale, r#""maxLagLsn":0"#);

        let relaxed =
            EngineJsonApi::new_in_process_cluster_with_replicas(vec![InProcessShardSpec::new(
                ShardId::new("tenant-710-shard-0"),
                leader.clone(),
            )
            .with_replica(InProcessReplicaSpec::new(
                "tenant-710-shard-0-follower-0",
                follower.clone(),
                2,
            ))])
            .unwrap();
        let (status, bounded_stale) = relaxed.route("GET", "/v1/cluster/shards", "");
        assert_eq!(status, 200, "{bounded_stale}");
        assert_json_contains(&bounded_stale, r#""readable":true"#);
        assert_json_contains(&bounded_stale, r#""syncState":"catching_up""#);
        assert_json_contains(&bounded_stale, r#""reason":"within_lag_budget""#);
        assert_json_contains(&bounded_stale, r#""maxLagLsn":2"#);

        let delta = leader.export_wal_after(follower.replication_status().committed_tail);
        follower.apply_wal_replication_batch(&delta).unwrap();
        let (status, caught_up) = strict.route("GET", "/v1/cluster/shards", "");
        assert_eq!(status, 200, "{caught_up}");
        assert_json_contains(&caught_up, r#""readable":true"#);
        assert_json_contains(&caught_up, r#""syncState":"ready""#);
        assert_json_contains(&caught_up, r#""replicationLagLsn":0"#);
        assert_json_contains(&caught_up, r#""reason":"caught_up""#);
    }

    let _ = std::fs::remove_dir_all(&leader_dir);
    let _ = std::fs::remove_dir_all(&follower_dir);
}

/// `/v1/search` 是召回型接口，可以选择 bounded-stale follower；严格读则必须回落 leader。
/// 这个 eval 用“leader-only 新 trace”证明读路由真的生效，而不是只把 follower 状态展示出来。
#[test]
fn cluster_search_uses_readable_follower_and_falls_back_when_stale() {
    let leader_dir = durable_dir("search_route_leader");
    let follower_dir = durable_dir("search_route_follower");
    {
        let leader = WriteCoordinator::open_durable(&leader_dir).unwrap();
        leader.recover();
        let follower = WriteCoordinator::open_durable(&follower_dir).unwrap();
        follower.recover();

        leader.ingest_wire_for_tenant(
            trace_events(71_101, "replica-search-route", "副本已经可见"),
            Some(711),
        );
        let first = leader.export_wal_after(0);
        follower.apply_wal_replication_batch(&first).unwrap();
        leader.ingest_wire_for_tenant(
            trace_events(71_102, "replica-search-route", "仅主节点可见"),
            Some(711),
        );

        let relaxed =
            EngineJsonApi::new_in_process_cluster_with_replicas(vec![InProcessShardSpec::new(
                ShardId::new("tenant-711-shard-0"),
                leader.clone(),
            )
            .with_replica(InProcessReplicaSpec::new(
                "tenant-711-shard-0-follower-0",
                follower.clone(),
                2,
            ))])
            .unwrap();
        let search = r#"{"text":"仅主节点","k":10,"includeFanout":true,"filter":{"projectId":"replica-search-route"}}"#;
        let (status, stale) = relaxed.route_with_tenant("POST", "/v1/search", search, Some(711));
        assert_eq!(status, 200, "{stale}");
        assert_json_contains(&stale, r#""items":[]"#);
        assert_json_contains(&stale, r#""total":0"#);
        assert_json_contains(&stale, r#""queryMode":"fanout_merge""#);
        assert_json_contains(&stale, r#""okShards":1"#);
        assert_json_contains(&stale, r#""degraded":false"#);

        let strict =
            EngineJsonApi::new_in_process_cluster_with_replicas(vec![InProcessShardSpec::new(
                ShardId::new("tenant-711-shard-0"),
                leader.clone(),
            )
            .with_replica(InProcessReplicaSpec::new(
                "tenant-711-shard-0-follower-0",
                follower.clone(),
                0,
            ))])
            .unwrap();
        let (status, leader_read) =
            strict.route_with_tenant("POST", "/v1/search", search, Some(711));
        assert_eq!(status, 200, "{leader_read}");
        assert_json_contains(&leader_read, r#""queryMode":"fanout_merge""#);
        assert_json_contains(&leader_read, r#""okShards":1"#);
        assert_json_contains(&leader_read, r#""trace_id":71102"#);

        let delta = leader.export_wal_after(follower.replication_status().committed_tail);
        follower.apply_wal_replication_batch(&delta).unwrap();
        let (status, caught_up) =
            relaxed.route_with_tenant("POST", "/v1/search", search, Some(711));
        assert_eq!(status, 200, "{caught_up}");
        assert_json_contains(&caught_up, r#""trace_id":71102"#);
    }

    let _ = std::fs::remove_dir_all(&leader_dir);
    let _ = std::fs::remove_dir_all(&follower_dir);
}

/// 带 snapshot lease 的 fanout 查询不能只记 manifest version，还要记住当时读的是哪个副本。
/// 否则第二页可能把 follower token 套到 leader 上，或者 follower 追平后旧分页看到新数据。
#[test]
fn trace_search_snapshot_lease_pins_follower_read_target() {
    let leader_dir = durable_dir("snapshot_route_leader");
    let follower_dir = durable_dir("snapshot_route_follower");
    {
        let leader = WriteCoordinator::open_durable(&leader_dir).unwrap();
        leader.recover();
        let follower = WriteCoordinator::open_durable(&follower_dir).unwrap();
        follower.recover();

        leader.ingest_wire_for_tenant(
            trace_events(71_201, "replica-snapshot-route", "副本第一页可见"),
            Some(712),
        );
        let first = leader.export_wal_after(0);
        follower.apply_wal_replication_batch(&first).unwrap();
        leader.ingest_wire_for_tenant(
            trace_events(71_202, "replica-snapshot-route", "仅主节点快照可见"),
            Some(712),
        );

        let relaxed =
            EngineJsonApi::new_in_process_cluster_with_replicas(vec![InProcessShardSpec::new(
                ShardId::new("tenant-712-shard-0"),
                leader.clone(),
            )
            .with_replica(InProcessReplicaSpec::new(
                "tenant-712-shard-0-follower-0",
                follower.clone(),
                2,
            ))])
            .unwrap();
        let body = r#"{"filter":{"projectId":"replica-snapshot-route","outputContains":"仅主节点快照可见"},"limit":10}"#;
        let (status, stale_page) =
            relaxed.route_with_tenant("POST", "/v1/trace-search", body, Some(712));
        assert_eq!(status, 200, "{stale_page}");
        assert_json_contains(&stale_page, r#""total":0"#);
        assert_json_contains(
            &stale_page,
            r#""readTarget":"tenant-712-shard-0-follower-0""#,
        );
        let snapshot = extract_json_object_field(&stale_page, "snapshot");

        let aggregate_body = r#"{"filter":{"projectId":"replica-snapshot-route","outputContains":"仅主节点快照可见"},"groupBy":["model"],"limit":10}"#;
        let (status, stale_aggregate) =
            relaxed.route_with_tenant("POST", "/v1/trace-aggregate", aggregate_body, Some(712));
        assert_eq!(status, 200, "{stale_aggregate}");
        assert_json_contains(&stale_aggregate, r#""spanTotal":0"#);
        assert_json_contains(
            &stale_aggregate,
            r#""readTarget":"tenant-712-shard-0-follower-0""#,
        );

        let delta = leader.export_wal_after(follower.replication_status().committed_tail);
        follower.apply_wal_replication_batch(&delta).unwrap();

        let replay_body = format!(
            r#"{{"filter":{{"projectId":"replica-snapshot-route","outputContains":"仅主节点快照可见"}},"limit":10,"snapshot":{snapshot}}}"#
        );
        let (status, stable_page) =
            relaxed.route_with_tenant("POST", "/v1/trace-search", &replay_body, Some(712));
        assert_eq!(status, 200, "{stable_page}");
        assert_json_contains(&stable_page, r#""total":0"#);

        let (status, fresh_page) =
            relaxed.route_with_tenant("POST", "/v1/trace-search", body, Some(712));
        assert_eq!(status, 200, "{fresh_page}");
        assert_json_contains(&fresh_page, r#""total":1"#);
        assert_json_contains(&fresh_page, r#""traceId":"71202""#);

        let (status, fresh_aggregate) =
            relaxed.route_with_tenant("POST", "/v1/trace-aggregate", aggregate_body, Some(712));
        assert_eq!(status, 200, "{fresh_aggregate}");
        assert_json_contains(&fresh_aggregate, r#""spanTotal":1"#);
    }

    let _ = std::fs::remove_dir_all(&leader_dir);
    let _ = std::fs::remove_dir_all(&follower_dir);
}
