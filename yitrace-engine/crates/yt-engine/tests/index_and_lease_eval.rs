use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use yt_engine::{EngineJsonApi, WriteCoordinator};

fn durable_dir(name: &str) -> std::path::PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "yt_index_lease_eval_{name}_{}_{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn assert_contains(body: &str, needle: &str) {
    assert!(body.contains(needle), "missing {needle:?} in {body}");
}

#[test]
fn vector_namespace_search_hides_retention_deleted_trace_after_reopen() {
    let dir = durable_dir("vector_retention");
    {
        let coord = WriteCoordinator::open_durable(&dir).unwrap();
        coord.recover();
        let api = EngineJsonApi::new(Arc::clone(&coord));
        let batch = r#"[
          {"trace_id":88001,"span_id":1,"session_id":8800,"ts":10,"seq":1,"event_type":2,"ext_span_id":"88001-1","status":0,"duration_ns":10,"output_text":"old vector source","attrs":{"project_id":"vector-retention","task_fingerprint":"vector-retention-task"}},
          {"trace_id":88002,"span_id":1,"session_id":8800,"ts":200,"seq":1,"event_type":2,"ext_span_id":"88002-1","status":0,"duration_ns":10,"output_text":"new vector source","attrs":{"project_id":"vector-retention","task_fingerprint":"vector-retention-task"}}
        ]"#;
        assert_eq!(
            api.route_with_tenant("POST", "/v1/ingest", batch, Some(880))
                .0,
            200
        );
        coord.flush_memtable();

        let old_vector = r#"{"namespace":"task","key":"old-path","traceId":88001,"embedding":[0.0,0.0],"attrs":{"project_id":"vector-retention"}}"#;
        let new_vector = r#"{"namespace":"task","key":"new-path","traceId":88002,"embedding":[0.2,0.2],"attrs":{"project_id":"vector-retention"}}"#;
        assert_eq!(
            api.route_with_tenant("POST", "/v1/vector-index", old_vector, Some(880))
                .0,
            200
        );
        assert_eq!(
            api.route_with_tenant("POST", "/v1/vector-index", new_vector, Some(880))
                .0,
            200
        );

        let query = r#"{"vector":[0.0,0.0],"k":2,"filter":{"namespace":"task","attrs":{"project_id":"vector-retention"}}}"#;
        let (status, before) = api.route_with_tenant("POST", "/v1/vector-search", query, Some(880));
        assert_eq!(status, 200, "{before}");
        assert_contains(&before, r#""key":"old-path""#);
        assert_contains(&before, r#""key":"new-path""#);

        let retention = r#"{"filter":{"projectId":"vector-retention"},"deleteBeforeTs":100,"requestedBy":"vector-retention-eval","reason":"ttl cleanup"}"#;
        let (status, applied) =
            api.route_with_tenant("POST", "/v1/retention/apply", retention, Some(880));
        assert_eq!(status, 200, "{applied}");
        assert_contains(&applied, r#""deletedTraceCount":1"#);

        let (status, after) = api.route_with_tenant("POST", "/v1/vector-search", query, Some(880));
        assert_eq!(status, 200, "{after}");
        assert!(
            !after.contains(r#""key":"old-path""#),
            "retention-deleted source trace must not be returned: {after}"
        );
        assert_contains(&after, r#""key":"new-path""#);
    }
    {
        let reopened = WriteCoordinator::open_durable(&dir).unwrap();
        reopened.recover();
        let api = EngineJsonApi::new(reopened);
        let query = r#"{"vector":[0.0,0.0],"k":2,"filter":{"namespace":"task","attrs":{"project_id":"vector-retention"}}}"#;
        let (status, after_reopen) =
            api.route_with_tenant("POST", "/v1/vector-search", query, Some(880));
        assert_eq!(status, 200, "{after_reopen}");
        assert!(
            !after_reopen.contains(r#""key":"old-path""#),
            "recovered named vector index must still obey retention visibility: {after_reopen}"
        );
        assert_contains(&after_reopen, r#""key":"new-path""#);
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn explicit_snapshot_lease_expires_and_renew_returns_snapshot_expired() {
    let coord = WriteCoordinator::new(Arc::new(yt_engine::InMemorySegmentStore::default()));
    let api = EngineJsonApi::new(coord);
    let (status, lease) = api.route("POST", "/v1/snapshots/lease", r#"{"ttlNs":1}"#);
    assert_eq!(status, 200, "{lease}");
    assert_contains(&lease, r#""leaseState":"active""#);
    assert_contains(&lease, r#""expiresAtNs":"#);

    std::thread::sleep(std::time::Duration::from_millis(2));
    let (status, renewed) = api.route("POST", "/v1/snapshots/renew", r#"{"leaseId":"lease-1"}"#);
    assert_eq!(status, 409, "{renewed}");
    assert_contains(&renewed, r#""code":"snapshot_expired""#);
}
