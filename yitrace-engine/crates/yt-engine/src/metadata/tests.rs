use super::*;

#[test]
fn metadata_roundtrip() {
    let attrs = BTreeMap::from([("project_id".to_string(), "\"agentic\"".to_string())]);
    let state = MetadataState {
        annotations: vec![TraceAnnotation {
            annotation_id: 7,
            tenant_id: Some(1),
            target: AnnotationTarget::Span,
            trace_id: 10,
            span_id: Some(2),
            external_trace_id: Some("run-a".to_string()),
            external_span_id: Some("span-a".to_string()),
            label: "best_path".to_string(),
            score: Some(920),
            reason: Some("人工确认".to_string()),
            source: Some("human".to_string()),
            created_at_ns: 123,
            updated_at_ns: 124,
            status: AnnotationStatus::Resolved,
            reviewer: Some("four".to_string()),
            attrs: attrs.clone(),
        }],
        dataset_associations: vec![DatasetAssociation {
            association_id: 9,
            tenant_id: Some(1),
            dataset_id: "regression".to_string(),
            item_id: "case-1".to_string(),
            trace_id: 10,
            span_id: Some(2),
            external_trace_id: Some("run-a".to_string()),
            external_span_id: Some("span-a".to_string()),
            snapshot_id: Some("snap-1".to_string()),
            snapshot_hash: Some("fnv1a64:abc".to_string()),
            eval_run_id: Some("eval-1".to_string()),
            split: Some("train".to_string()),
            label: Some("pass".to_string()),
            score: Some(900),
            created_at_ns: 456,
            attrs,
        }],
        next_annotation_id: 8,
        next_dataset_association_id: 10,
        retention_audits: vec![RetentionAuditRecord {
            audit_id: 13,
            tenant_id: Some(1),
            created_at_ns: 789,
            source: Some("nightly".to_string()),
            reason: Some("ttl".to_string()),
            delete_before_ts: Some(100),
            query_json: "{\"deleteBeforeTs\":100}".to_string(),
            protect_annotations: true,
            protect_dataset_associations: true,
            protect_snapshots: true,
            protect_eval_links: true,
            protect_path_memory: true,
            compact_requested: false,
            compact_reclaim: true,
            candidate_trace_count: 3,
            protected_trace_count: 1,
            deletable_trace_count: 2,
            requested_trace_count: 2,
            deleted_trace_count: 1,
            deleted_segment_row_count: 4,
            skipped_live_trace_count: 1,
            compacted_segment_count: 0,
            reclaimed_segment_count: 0,
            dropped_deleted_row_count: 0,
            rewritten_live_row_count: 0,
            deletable_trace_ids: vec![10, 11],
            deleted_trace_ids: vec![10],
            skipped_live_trace_ids: vec![11],
            trace_id_sample_truncated: false,
        }],
        retention_policies: vec![RetentionPolicy {
            policy_id: 17,
            tenant_id: Some(1),
            name: "nightly".to_string(),
            enabled: true,
            created_at_ns: 800,
            updated_at_ns: 801,
            last_run_at_ns: Some(900),
            next_run_at_ns: Some(1000),
            interval_ns: 86_400_000_000_000,
            source: Some("policy".to_string()),
            reason: Some("ttl".to_string()),
            query_json: "{\"olderThanNs\":1000}".to_string(),
        }],
        next_retention_audit_id: 14,
        next_retention_policy_id: 18,
    };

    let back = decode(&encode(&state)).unwrap();
    assert_eq!(back.annotations, state.annotations);
    assert_eq!(back.dataset_associations, state.dataset_associations);
    assert_eq!(back.retention_audits, state.retention_audits);
    assert_eq!(back.retention_policies, state.retention_policies);
    assert_eq!(back.next_annotation_id, 8);
    assert_eq!(back.next_dataset_association_id, 10);
    assert_eq!(back.next_retention_audit_id, 14);
    assert_eq!(back.next_retention_policy_id, 18);
}
