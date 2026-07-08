fn metadata_attr_aliases() -> &'static [(&'static str, &'static str)] {
    &[
        ("project_id", "project_id"),
        ("projectId", "project_id"),
        ("external_run_id", "external_run_id"),
        ("externalRunId", "external_run_id"),
        ("skill", "skill"),
        ("mode", "mode"),
        ("call_site", "call_site"),
        ("callSite", "call_site"),
        ("task_fingerprint", "task_fingerprint"),
        ("taskFingerprint", "task_fingerprint"),
        ("loop_id", "loop_id"),
        ("loopId", "loop_id"),
        ("harness_version", "harness_version"),
        ("harnessVersion", "harness_version"),
        ("validation_status", "validation_status"),
        ("validationStatus", "validation_status"),
        ("review_status", "review_status"),
        ("reviewStatus", "review_status"),
        ("eval_status", "eval_status"),
        ("evalStatus", "eval_status"),
        ("path_memory_id", "path_memory_id"),
        ("pathMemoryId", "path_memory_id"),
        ("phase", "phase"),
        ("validator", "validator"),
    ]
}

fn collect_metadata_attr_map(
    f: &crate::wire::Json,
    attrs: &mut std::collections::BTreeMap<String, String>,
) {
    use crate::wire::{field, Json};
    for (alias, key) in metadata_attr_aliases() {
        if let Some(v) = field(f, alias) {
            attrs.insert((*key).to_string(), v.to_compact_json());
        }
    }
    if let Some(Json::Obj(kvs)) = field(f, "attrs") {
        for (k, v) in kvs {
            attrs.insert(k.clone(), v.to_compact_json());
        }
    }
}

fn collect_metadata_attr_query_json(
    s: &str,
    attrs: &mut std::collections::BTreeMap<String, String>,
) {
    let Ok(crate::wire::Json::Obj(kvs)) = crate::wire::parse(s) else {
        return;
    };
    for (k, v) in kvs {
        attrs.insert(k, v.to_compact_json());
    }
}

fn collect_metadata_attr_query_pair(
    k: &str,
    v: &str,
    attrs: &mut std::collections::BTreeMap<String, String>,
) {
    if let Some((_, attr_key)) = metadata_attr_aliases()
        .iter()
        .find(|(alias, _)| *alias == k)
    {
        attrs.insert((*attr_key).to_string(), json_string_value(v));
    }
}

fn query_pairs(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter_map(|kv| {
            if kv.is_empty() {
                return None;
            }
            let (k, v) = kv.split_once('=')?;
            Some((url_decode(k), url_decode(v)))
        })
        .collect()
}

fn query_bool(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "y" | "on"
    )
}

fn annotation_filter_from_query(
    query: &str,
    tenant: Option<u64>,
) -> (TraceAnnotationFilter, usize, usize) {
    let mut filter = TraceAnnotationFilter {
        tenant_id: tenant,
        ..Default::default()
    };
    let mut cursor = 0usize;
    let mut limit = 50usize;
    for (k, v) in query_pairs(query) {
        match k.as_str() {
            "cursor" | "offset" => cursor = v.parse::<usize>().unwrap_or(0),
            "limit" => limit = v.parse::<usize>().unwrap_or(50).clamp(1, 500),
            "target" | "target_type" | "targetType" => {
                filter.target = AnnotationTarget::parse(&v);
            }
            "trace_id" | "traceId" => filter.trace_id = parse_id_or_hash(&v),
            "span_id" | "spanId" => filter.span_id = parse_id_or_hash(&v),
            "label" => filter.label = Some(v),
            "source" => filter.source = Some(v),
            "status" => filter.status = AnnotationStatus::parse(&v),
            "includeDeleted" | "include_deleted" => {
                filter.include_deleted = query_bool(&v);
            }
            "attrs" => collect_metadata_attr_query_json(&v, &mut filter.attrs),
            _ => collect_metadata_attr_query_pair(&k, &v, &mut filter.attrs),
        }
    }
    (filter, cursor, limit)
}

fn dataset_filter_from_query(
    query: &str,
    tenant: Option<u64>,
) -> (DatasetAssociationFilter, usize, usize) {
    let mut filter = DatasetAssociationFilter {
        tenant_id: tenant,
        ..Default::default()
    };
    let mut cursor = 0usize;
    let mut limit = 50usize;
    for (k, v) in query_pairs(query) {
        match k.as_str() {
            "cursor" | "offset" => cursor = v.parse::<usize>().unwrap_or(0),
            "limit" => limit = v.parse::<usize>().unwrap_or(50).clamp(1, 500),
            "dataset_id" | "datasetId" | "dataset" => filter.dataset_id = Some(v),
            "item_id" | "itemId" | "dataset_item_id" | "datasetItemId" => filter.item_id = Some(v),
            "trace_id" | "traceId" => filter.trace_id = parse_id_or_hash(&v),
            "span_id" | "spanId" => filter.span_id = parse_id_or_hash(&v),
            "eval_run_id" | "evalRunId" => filter.eval_run_id = Some(v),
            "split" => filter.split = Some(v),
            "label" => filter.label = Some(v),
            "attrs" => collect_metadata_attr_query_json(&v, &mut filter.attrs),
            _ => collect_metadata_attr_query_pair(&k, &v, &mut filter.attrs),
        }
    }
    (filter, cursor, limit)
}

fn update_annotation_from_body(body: &str) -> Result<UpdateTraceAnnotation, (u16, String)> {
    let v = parse_json_body_or_empty(body)
        .map_err(|e| (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))))?;
    let mut update = UpdateTraceAnnotation {
        merge_attrs: !json_bool_alias(&v, &["replaceAttrs", "replace_attrs"]).unwrap_or(false),
        ..Default::default()
    };
    if let Some(label) = json_field_alias(&v, &["label", "name"])
        .and_then(crate::wire::Json::as_str)
        .map(|s| s.trim().to_string())
    {
        if label.is_empty() {
            return Err((400, r#"{"error":"empty label"}"#.to_string()));
        }
        update.label = Some(label);
    }
    if let Some(score) = optional_score_patch(&v, &["score", "eval_score", "evalScore"]) {
        update.score = Some(score);
    }
    if let Some(reason) = optional_string_patch(&v, &["reason", "comment", "note"]) {
        update.reason = Some(reason);
    }
    if let Some(source) = optional_string_patch(
        &v,
        &[
            "source",
            "updated_by",
            "updatedBy",
            "created_by",
            "createdBy",
        ],
    ) {
        update.source = Some(source);
    }
    if let Some(status_value) = json_raw_field_alias(&v, &["status"]) {
        let Some(status) = status_value.as_str().and_then(AnnotationStatus::parse) else {
            return Err((400, r#"{"error":"invalid status"}"#.to_string()));
        };
        update.status = Some(status);
    }
    if let Some(reviewer) = optional_string_patch(&v, &["reviewer", "reviewed_by", "reviewedBy"]) {
        update.reviewer = Some(reviewer);
    }
    let mut attrs = std::collections::BTreeMap::new();
    collect_metadata_attr_map(&v, &mut attrs);
    if !attrs.is_empty()
        || json_raw_field_alias(&v, &["attrs"]).is_some()
        || json_bool_alias(&v, &["replaceAttrs", "replace_attrs"]).unwrap_or(false)
    {
        update.attrs = Some(attrs);
    }
    Ok(update)
}

fn delete_annotation_from_body(
    body: &str,
) -> Result<(Option<String>, Option<String>), (u16, String)> {
    let v = parse_json_body_or_empty(body)
        .map_err(|e| (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))))?;
    let reviewer = json_field_alias(&v, &["reviewer", "reviewed_by", "reviewedBy", "source"])
        .and_then(crate::wire::Json::as_str)
        .map(ToString::to_string);
    let reason = json_field_alias(&v, &["reason", "comment", "note"])
        .and_then(crate::wire::Json::as_str)
        .map(ToString::to_string);
    Ok((reviewer, reason))
}

fn annotations_page_json(
    mut items: Vec<TraceAnnotation>,
    cursor: usize,
    limit: usize,
) -> (u16, String) {
    items.sort_by(|a, b| {
        b.created_at_ns
            .cmp(&a.created_at_ns)
            .then_with(|| b.annotation_id.cmp(&a.annotation_id))
    });
    let total = items.len();
    let end = cursor.saturating_add(limit).min(total);
    let page = if cursor < total {
        &items[cursor..end]
    } else {
        &[]
    };
    let next_cursor = if end < total {
        end.to_string()
    } else {
        "null".to_string()
    };
    let body = page
        .iter()
        .map(json_annotation)
        .collect::<Vec<_>>()
        .join(",");
    (
        200,
        format!(
            r#"{{"items":[{}],"count":{},"total":{},"pageCount":{},"nextCursor":{},"metadataIndex":"sidecar"}}"#,
            body,
            total,
            total,
            page.len(),
            next_cursor,
        ),
    )
}

fn dataset_associations_page_json(
    mut items: Vec<DatasetAssociation>,
    cursor: usize,
    limit: usize,
) -> (u16, String) {
    items.sort_by(|a, b| {
        b.created_at_ns
            .cmp(&a.created_at_ns)
            .then_with(|| b.association_id.cmp(&a.association_id))
    });
    let total = items.len();
    let end = cursor.saturating_add(limit).min(total);
    let page = if cursor < total {
        &items[cursor..end]
    } else {
        &[]
    };
    let next_cursor = if end < total {
        end.to_string()
    } else {
        "null".to_string()
    };
    let body = page
        .iter()
        .map(json_dataset_association)
        .collect::<Vec<_>>()
        .join(",");
    (
        200,
        format!(
            r#"{{"items":[{}],"count":{},"total":{},"pageCount":{},"nextCursor":{},"metadataIndex":"sidecar"}}"#,
            body,
            total,
            total,
            page.len(),
            next_cursor,
        ),
    )
}

fn json_annotation(a: &TraceAnnotation) -> String {
    format!(
        r#"{{"annotationId":"{}","tenantId":{},"target":"{}","traceId":"{}","spanId":{},"externalTraceId":{},"externalSpanId":{},"label":"{}","score":{},"reason":{},"source":{},"status":"{}","reviewer":{},"createdAtNs":"{}","updatedAtNs":"{}","attrs":{}}}"#,
        a.annotation_id,
        json_opt_u64_string(a.tenant_id),
        a.target.as_str(),
        a.trace_id,
        json_opt_u64_string(a.span_id),
        json_opt_str(a.external_trace_id.as_deref()),
        json_opt_str(a.external_span_id.as_deref()),
        json_escape(&a.label),
        a.score.map_or("null".to_string(), |s| s.to_string()),
        json_opt_str(a.reason.as_deref()),
        json_opt_str(a.source.as_deref()),
        a.status.as_str(),
        json_opt_str(a.reviewer.as_deref()),
        a.created_at_ns,
        a.updated_at_ns,
        json_attrs(&a.attrs),
    )
}

fn json_dataset_association(a: &DatasetAssociation) -> String {
    format!(
        r#"{{"associationId":"{}","tenantId":{},"datasetId":"{}","itemId":"{}","traceId":"{}","spanId":{},"externalTraceId":{},"externalSpanId":{},"snapshotId":{},"snapshotHash":{},"evalRunId":{},"split":{},"label":{},"score":{},"createdAtNs":"{}","attrs":{}}}"#,
        a.association_id,
        json_opt_u64_string(a.tenant_id),
        json_escape(&a.dataset_id),
        json_escape(&a.item_id),
        a.trace_id,
        json_opt_u64_string(a.span_id),
        json_opt_str(a.external_trace_id.as_deref()),
        json_opt_str(a.external_span_id.as_deref()),
        json_opt_str(a.snapshot_id.as_deref()),
        json_opt_str(a.snapshot_hash.as_deref()),
        json_opt_str(a.eval_run_id.as_deref()),
        json_opt_str(a.split.as_deref()),
        json_opt_str(a.label.as_deref()),
        a.score.map_or("null".to_string(), |s| s.to_string()),
        a.created_at_ns,
        json_attrs(&a.attrs),
    )
}
