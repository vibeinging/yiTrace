#[derive(Clone)]
struct RetentionPlanConfig {
    apply: bool,
    cutoff: Option<i64>,
    protect_annotations: bool,
    protect_dataset_associations: bool,
    protect_snapshots: bool,
    protect_eval_links: bool,
    protect_path_memory: bool,
    example_limit: usize,
    compact_after_apply: bool,
    compact_min_deleted_rows: u32,
    compact_min_deleted_percent: u32,
    compact_max_segments: usize,
    reclaim_after_compact: bool,
    audit_source: Option<String>,
    audit_reason: Option<String>,
    query_json: String,
}

struct StorageMetadata {
    annotations: Vec<crate::TraceAnnotation>,
    dataset_associations: Vec<crate::DatasetAssociation>,
}

struct RetentionPlanOutcome {
    candidate_stats: StorageStatsBucket,
    protected_stats: StorageStatsBucket,
    deletable_stats: StorageStatsBucket,
    protected: std::collections::BTreeMap<u64, Vec<String>>,
    deletable_trace_ids: std::collections::HashSet<u64>,
    applied: Option<crate::RetentionDeleteResult>,
    compacted: Option<crate::RetentionCompactResult>,
    audit: Option<crate::RetentionAuditRecord>,
}

fn retention_plan_config_from_json(
    v: &crate::wire::Json,
    route_apply: bool,
) -> Result<RetentionPlanConfig, String> {
    let apply = route_apply || json_bool_alias(v, &["apply", "execute", "delete"]).unwrap_or(false);
    let cutoff = json_field_alias(
        v,
        &[
            "delete_before_ts",
            "deleteBeforeTs",
            "older_than_ts",
            "olderThanTs",
            "time_to",
            "timeTo",
        ],
    )
    .and_then(crate::wire::Json::as_i64);
    if apply && cutoff.is_none() {
        return Err("retention apply requires deleteBeforeTs".to_string());
    }
    Ok(RetentionPlanConfig {
        apply,
        cutoff,
        protect_annotations: retention_protect_bool(v, "annotations", "annotations", true),
        protect_dataset_associations: retention_protect_bool(
            v,
            "datasetAssociations",
            "dataset_associations",
            true,
        ),
        protect_snapshots: retention_protect_bool(v, "snapshots", "snapshots", true),
        protect_eval_links: retention_protect_bool(v, "evalLinks", "eval_links", true),
        protect_path_memory: retention_protect_bool(v, "pathMemory", "path_memory", true),
        example_limit: json_field_alias(v, &["exampleLimit", "examples", "limit"])
            .and_then(crate::wire::Json::as_u64)
            .unwrap_or(20)
            .clamp(0, 100) as usize,
        compact_after_apply:
            json_bool_alias(v, &["compact", "compactAfterApply", "compact_after_apply"])
                .unwrap_or(false),
        compact_min_deleted_rows: json_field_alias(
            v,
            &[
                "compactMinDeletedRows",
                "compact_min_deleted_rows",
                "minDeletedRows",
                "min_deleted_rows",
            ],
        )
        .and_then(crate::wire::Json::as_u64)
        .unwrap_or(1)
        .clamp(1, u32::MAX as u64) as u32,
        compact_min_deleted_percent: json_field_alias(
            v,
            &[
                "compactMinDeletedPercent",
                "compact_min_deleted_percent",
                "minDeletedPercent",
                "min_deleted_percent",
            ],
        )
        .and_then(crate::wire::Json::as_u64)
        .unwrap_or(1)
        .clamp(1, 100) as u32,
        compact_max_segments: json_field_alias(
            v,
            &[
                "compactMaxSegments",
                "compact_max_segments",
                "maxSegments",
                "max_segments",
            ],
        )
        .and_then(crate::wire::Json::as_u64)
        .unwrap_or(64)
        .clamp(0, 1024) as usize,
        reclaim_after_compact: json_bool_alias(
            v,
            &[
                "reclaim",
                "reclaimAfterCompact",
                "reclaim_after_compact",
                "compactReclaim",
                "compact_reclaim",
            ],
        )
        .unwrap_or(true),
        audit_source: json_field_alias(
            v,
            &[
                "source",
                "requestedBy",
                "requested_by",
                "actor",
                "createdBy",
                "created_by",
            ],
        )
        .and_then(crate::wire::Json::as_str)
        .map(ToString::to_string),
        audit_reason: json_field_alias(v, &["reason", "comment", "note"])
            .and_then(crate::wire::Json::as_str)
            .map(ToString::to_string),
        query_json: v.to_compact_json(),
    })
}

fn retention_protect_bool(v: &crate::wire::Json, camel: &str, snake: &str, default: bool) -> bool {
    let top_camel = format!("protect{}", capitalize_ascii(camel));
    let top_snake = format!("protect_{}", snake);
    if let Some(value) = json_bool_alias(v, &[top_camel.as_str(), top_snake.as_str(), camel, snake])
    {
        return value;
    }
    if let Some(protect) = crate::wire::field(v, "protect") {
        if let Some(value) = json_bool_alias(protect, &[camel, snake]) {
            return value;
        }
    }
    default
}

fn capitalize_ascii(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

fn protected_trace_reasons(
    candidate_trace_ids: &std::collections::HashSet<u64>,
    metadata: &StorageMetadata,
    config: &RetentionPlanConfig,
) -> std::collections::BTreeMap<u64, Vec<String>> {
    let mut reasons = std::collections::BTreeMap::<u64, Vec<String>>::new();
    let mut add = |trace_id: u64, reason: &str| {
        if candidate_trace_ids.contains(&trace_id) {
            let entry = reasons.entry(trace_id).or_default();
            if !entry.iter().any(|item| item == reason) {
                entry.push(reason.to_string());
            }
        }
    };
    if config.protect_annotations {
        for annotation in &metadata.annotations {
            add(annotation.trace_id, "annotation");
        }
    }
    if config.protect_dataset_associations {
        for association in &metadata.dataset_associations {
            add(association.trace_id, "datasetAssociation");
        }
    }
    if config.protect_snapshots {
        for association in &metadata.dataset_associations {
            if dataset_association_has_snapshot(association) {
                add(association.trace_id, "snapshot");
            }
        }
    }
    if config.protect_eval_links {
        for association in &metadata.dataset_associations {
            if dataset_association_is_eval_link(association) {
                add(association.trace_id, "evalLink");
            }
        }
        for annotation in &metadata.annotations {
            if metadata_attrs_have_eval_link(&annotation.attrs) {
                add(annotation.trace_id, "evalLink");
            }
        }
    }
    if config.protect_path_memory {
        for association in &metadata.dataset_associations {
            if metadata_attrs_have_path_memory(&association.attrs) {
                add(association.trace_id, "pathMemory");
            }
        }
        for annotation in &metadata.annotations {
            if metadata_attrs_have_path_memory(&annotation.attrs) {
                add(annotation.trace_id, "pathMemory");
            }
        }
    }
    reasons
}

fn dataset_association_has_snapshot(a: &crate::DatasetAssociation) -> bool {
    a.snapshot_id.as_deref().is_some_and(|v| !v.is_empty())
        || a.snapshot_hash.as_deref().is_some_and(|v| !v.is_empty())
}

fn dataset_association_is_eval_link(a: &crate::DatasetAssociation) -> bool {
    a.eval_run_id.as_deref().is_some_and(|v| !v.is_empty())
        || metadata_attrs_have_eval_link(&a.attrs)
}

fn metadata_attrs_have_eval_link(attrs: &std::collections::BTreeMap<String, String>) -> bool {
    attrs.contains_key("eval_run_id")
        || attrs.contains_key("evalRunId")
        || attrs.contains_key("eval_profile")
        || attrs.contains_key("evalProfile")
        || attrs.contains_key("eval_status")
        || attrs.contains_key("evalStatus")
}

fn metadata_attrs_have_path_memory(attrs: &std::collections::BTreeMap<String, String>) -> bool {
    attrs.contains_key("path_memory_id") || attrs.contains_key("pathMemoryId")
}

fn storage_bucket_for_trace_ids(
    spans: &[FoldedSpan],
    trace_ids: &std::collections::HashSet<u64>,
) -> StorageStatsBucket {
    let filtered = spans
        .iter()
        .filter(|span| trace_ids.contains(&span.trace_id))
        .cloned()
        .collect::<Vec<_>>();
    StorageStatsBucket::from_spans(&filtered, &[])
}

fn sample_u64_set(items: &std::collections::HashSet<u64>, limit: usize) -> Vec<u64> {
    let mut ids = items.iter().copied().collect::<Vec<_>>();
    ids.sort_unstable();
    ids.truncate(limit);
    ids
}

fn sample_u64_slice(items: &[u64], limit: usize) -> Vec<u64> {
    let mut ids = items.to_vec();
    ids.sort_unstable();
    ids.truncate(limit);
    ids
}

fn json_retention_bool(v: bool) -> &'static str {
    if v { "true" } else { "false" }
}

fn json_retention_opt_i64(v: Option<i64>) -> String {
    v.map(|x| x.to_string())
        .unwrap_or_else(|| "null".to_string())
}

fn json_retention_id_array(ids: &[u64]) -> String {
    let values = ids.iter().map(|id| format!(r#""{}""#, id)).collect::<Vec<_>>();
    format!("[{}]", values.join(","))
}

fn json_retention_protected_reasons(
    protected: &std::collections::BTreeMap<u64, Vec<String>>,
) -> String {
    protected
        .iter()
        .map(|(trace_id, reasons)| {
            let values = reasons
                .iter()
                .map(|reason| json_string_value(reason))
                .collect::<Vec<_>>()
                .join(",");
            format!(r#""{}":[{}]"#, trace_id, values)
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn json_retention_delete_result(result: &crate::RetentionDeleteResult) -> String {
    format!(
        r#"{{"requestedTraceCount":{},"deletedTraceCount":{},"deletedSegmentRowCount":{},"skippedLiveTraceCount":{},"deletedTraceIds":{},"skippedLiveTraceIds":{}}}"#,
        result.requested_trace_count,
        result.deleted_trace_count,
        result.deleted_segment_row_count,
        result.skipped_live_trace_count,
        json_retention_id_array(&result.deleted_trace_ids),
        json_retention_id_array(&result.skipped_live_trace_ids),
    )
}

fn json_retention_compact_result(result: &crate::RetentionCompactResult) -> String {
    format!(
        r#"{{"beforeLiveSegmentCount":{},"afterLiveSegmentCount":{},"beforeDeadSegmentCount":{},"afterDeadSegmentCount":{},"selectedSegmentCount":{},"compactedSegmentCount":{},"reclaimedSegmentCount":{},"droppedDeletedRowCount":{},"rewrittenLiveRowCount":{},"selectedSegmentIds":{}}}"#,
        result.before_live_segment_count,
        result.after_live_segment_count,
        result.before_dead_segment_count,
        result.after_dead_segment_count,
        result.selected_segment_count,
        result.compacted_segment_count,
        result.reclaimed_segment_count,
        result.dropped_deleted_row_count,
        result.rewritten_live_row_count,
        json_retention_id_array(&result.selected_segment_ids),
    )
}

fn json_retention_audit(a: &crate::RetentionAuditRecord) -> String {
    format!(
        r#"{{"auditId":"{}","tenantId":{},"createdAtNs":"{}","source":{},"reason":{},"deleteBeforeTs":{},"query":{},"protect":{{"annotations":{},"datasetAssociations":{},"snapshots":{},"evalLinks":{},"pathMemory":{}}},"compact":{{"requested":{},"reclaim":{},"compactedSegmentCount":{},"reclaimedSegmentCount":{},"droppedDeletedRowCount":{},"rewrittenLiveRowCount":{}}},"counts":{{"candidateTraceCount":{},"protectedTraceCount":{},"deletableTraceCount":{},"requestedTraceCount":{},"deletedTraceCount":{},"deletedSegmentRowCount":{},"skippedLiveTraceCount":{}}},"traceIds":{{"deletable":{},"deleted":{},"skippedLive":{},"sampleTruncated":{}}}}}"#,
        a.audit_id,
        json_opt_u64_string(a.tenant_id),
        a.created_at_ns,
        json_opt_str(a.source.as_deref()),
        json_opt_str(a.reason.as_deref()),
        json_retention_opt_i64(a.delete_before_ts),
        if a.query_json.trim().is_empty() {
            "{}".to_string()
        } else {
            a.query_json.clone()
        },
        json_retention_bool(a.protect_annotations),
        json_retention_bool(a.protect_dataset_associations),
        json_retention_bool(a.protect_snapshots),
        json_retention_bool(a.protect_eval_links),
        json_retention_bool(a.protect_path_memory),
        json_retention_bool(a.compact_requested),
        json_retention_bool(a.compact_reclaim),
        a.compacted_segment_count,
        a.reclaimed_segment_count,
        a.dropped_deleted_row_count,
        a.rewritten_live_row_count,
        a.candidate_trace_count,
        a.protected_trace_count,
        a.deletable_trace_count,
        a.requested_trace_count,
        a.deleted_trace_count,
        a.deleted_segment_row_count,
        a.skipped_live_trace_count,
        json_retention_id_array(&a.deletable_trace_ids),
        json_retention_id_array(&a.deleted_trace_ids),
        json_retention_id_array(&a.skipped_live_trace_ids),
        json_retention_bool(a.trace_id_sample_truncated),
    )
}

fn json_retention_policy(p: &crate::RetentionPolicy) -> String {
    format!(
        r#"{{"policyId":"{}","tenantId":{},"name":"{}","enabled":{},"createdAtNs":"{}","updatedAtNs":"{}","lastRunAtNs":{},"nextRunAtNs":{},"intervalNs":"{}","source":{},"reason":{},"query":{}}}"#,
        p.policy_id,
        json_opt_u64_string(p.tenant_id),
        json_escape(&p.name),
        json_retention_bool(p.enabled),
        p.created_at_ns,
        p.updated_at_ns,
        json_opt_u64_string(p.last_run_at_ns),
        json_opt_u64_string(p.next_run_at_ns),
        p.interval_ns,
        json_opt_str(p.source.as_deref()),
        json_opt_str(p.reason.as_deref()),
        if p.query_json.trim().is_empty() {
            "{}".to_string()
        } else {
            p.query_json.clone()
        },
    )
}

fn json_retention_plan(config: &RetentionPlanConfig, outcome: &RetentionPlanOutcome) -> String {
    format!(
        r#"{{"dryRun":{},"applied":{},"deleteBeforeTs":{},"protect":{{"annotations":{},"datasetAssociations":{},"snapshots":{},"evalLinks":{},"pathMemory":{}}},"compact":{{"requested":{},"minDeletedRows":{},"minDeletedPercent":{},"maxSegments":{},"reclaim":{}}},"candidates":{},"protected":{},"deletable":{},"protectedReasons":{{{}}},"deletableTraceIds":{},"applyResult":{},"compactResult":{},"audit":{}}}"#,
        json_retention_bool(!config.apply),
        json_retention_bool(config.apply),
        json_retention_opt_i64(config.cutoff),
        json_retention_bool(config.protect_annotations),
        json_retention_bool(config.protect_dataset_associations),
        json_retention_bool(config.protect_snapshots),
        json_retention_bool(config.protect_eval_links),
        json_retention_bool(config.protect_path_memory),
        json_retention_bool(config.compact_after_apply),
        config.compact_min_deleted_rows,
        config.compact_min_deleted_percent,
        config.compact_max_segments,
        json_retention_bool(config.reclaim_after_compact),
        outcome.candidate_stats.clone().to_json(&[]),
        outcome.protected_stats.clone().to_json(&[]),
        outcome.deletable_stats.clone().to_json(&[]),
        json_retention_protected_reasons(&outcome.protected),
        json_retention_id_array(&sample_u64_set(
            &outcome.deletable_trace_ids,
            config.example_limit,
        )),
        outcome
            .applied
            .as_ref()
            .map(json_retention_delete_result)
            .unwrap_or_else(|| "null".to_string()),
        outcome
            .compacted
            .as_ref()
            .map(json_retention_compact_result)
            .unwrap_or_else(|| "null".to_string()),
        outcome
            .audit
            .as_ref()
            .map(json_retention_audit)
            .unwrap_or_else(|| "null".to_string()),
    )
}

fn retention_policy_query_has_cutoff(v: &crate::wire::Json) -> bool {
    json_field_alias(
        v,
        &[
            "deleteBeforeTs",
            "delete_before_ts",
            "olderThanTs",
            "older_than_ts",
            "timeTo",
            "time_to",
            "olderThanNs",
            "older_than_ns",
            "ttlNs",
            "ttl_ns",
            "retentionNs",
            "retention_ns",
        ],
    )
    .is_some()
}

fn retention_policy_effective_query(
    policy: &crate::RetentionPolicy,
    now_ns: u64,
) -> Result<String, String> {
    let mut json = crate::wire::parse(&policy.query_json)?;
    let crate::wire::Json::Obj(ref mut kvs) = json else {
        return Err("policy query must be an object".to_string());
    };
    if !json_obj_has_alias(
        kvs,
        &[
            "deleteBeforeTs",
            "delete_before_ts",
            "olderThanTs",
            "older_than_ts",
            "timeTo",
            "time_to",
        ],
    ) {
        let ttl = json_obj_field_alias(
            kvs,
            &[
                "olderThanNs",
                "older_than_ns",
                "ttlNs",
                "ttl_ns",
                "retentionNs",
                "retention_ns",
            ],
        )
        .and_then(crate::wire::Json::as_u64)
        .ok_or_else(|| "policy query requires deleteBeforeTs or olderThanNs".to_string())?;
        let cutoff = now_ns.saturating_sub(ttl).min(i64::MAX as u64) as i64;
        json_obj_set(
            kvs,
            "deleteBeforeTs",
            crate::wire::Json::Num(cutoff.to_string()),
        );
    }
    json_obj_set(kvs, "apply", crate::wire::Json::Bool(true));
    if !json_obj_has_alias(
        kvs,
        &[
            "source",
            "requestedBy",
            "requested_by",
            "actor",
            "createdBy",
            "created_by",
        ],
    ) {
        if let Some(source) = &policy.source {
            json_obj_set(kvs, "requestedBy", crate::wire::Json::Str(source.clone()));
        }
    }
    if !json_obj_has_alias(kvs, &["reason", "comment", "note"]) {
        if let Some(reason) = &policy.reason {
            json_obj_set(kvs, "reason", crate::wire::Json::Str(reason.clone()));
        }
    }
    Ok(json.to_compact_json())
}

fn json_obj_field_alias<'a>(
    kvs: &'a [(String, crate::wire::Json)],
    names: &[&str],
) -> Option<&'a crate::wire::Json> {
    names
        .iter()
        .find_map(|name| kvs.iter().find(|(key, _)| key == name).map(|(_, value)| value))
}

fn json_obj_has_alias(kvs: &[(String, crate::wire::Json)], names: &[&str]) -> bool {
    json_obj_field_alias(kvs, names).is_some()
}

fn json_obj_set(kvs: &mut Vec<(String, crate::wire::Json)>, key: &str, value: crate::wire::Json) {
    if let Some((_, existing)) = kvs.iter_mut().find(|(name, _)| name == key) {
        *existing = value;
    } else {
        kvs.push((key.to_string(), value));
    }
}

fn unix_now_ns_u64_for_http() -> u64 {
    crate::metadata::now_ns()
}
