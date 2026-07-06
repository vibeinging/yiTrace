fn product_query_parts(query: &str, default_limit: usize) -> ProductQueryParts {
    let mut cursor = 0usize;
    let mut limit = default_limit.clamp(1, 500);
    let mut filter = String::new();
    let mut attrs = std::collections::BTreeMap::new();
    let pairs = query_pairs(query);
    for (k, v) in &pairs {
        match k.as_str() {
            "cursor" | "offset" => cursor = v.parse().unwrap_or(0),
            "limit" | "k" => limit = v.parse().unwrap_or(default_limit).clamp(1, 500),
            "filter" | "q" | "text" => filter = v.clone(),
            "attrs" => collect_attr_query_json(v, &mut attrs),
            _ => collect_attr_query_pair(k, v, &mut attrs),
        }
    }
    ProductQueryParts {
        cursor,
        limit,
        filter,
        attrs,
        annotation: trace_search_annotation_spec_from_query(&pairs),
        dataset: trace_search_dataset_spec_from_query(&pairs),
    }
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
            "attrs" => collect_attr_query_json(&v, &mut filter.attrs),
            _ => collect_attr_query_pair(&k, &v, &mut filter.attrs),
        }
    }
    (filter, cursor, limit)
}

fn annotations_page_json(
    mut items: Vec<TraceAnnotation>,
    cursor: usize,
    limit: usize,
    shard_count: Option<usize>,
    metadata_index: &str,
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
    let cluster = shard_count
        .map(|count| format!(r#","queryMode":"fanout_merge","shardCount":{}"#, count))
        .unwrap_or_default();
    (
        200,
        format!(
            r#"{{"items":[{}],"count":{},"total":{},"pageCount":{},"nextCursor":{}{},"metadataIndex":"{}"}}"#,
            body,
            total,
            total,
            page.len(),
            next_cursor,
            cluster,
            metadata_index,
        ),
    )
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
    collect_attr_map(&v, &mut attrs);
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
            "attrs" => collect_attr_query_json(&v, &mut filter.attrs),
            _ => collect_attr_query_pair(&k, &v, &mut filter.attrs),
        }
    }
    (filter, cursor, limit)
}

fn dataset_associations_page_json(
    mut items: Vec<DatasetAssociation>,
    cursor: usize,
    limit: usize,
    shard_count: Option<usize>,
    metadata_index: &str,
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
    let cluster = shard_count
        .map(|count| format!(r#","queryMode":"fanout_merge","shardCount":{}"#, count))
        .unwrap_or_default();
    (
        200,
        format!(
            r#"{{"items":[{}],"count":{},"total":{},"pageCount":{},"nextCursor":{}{},"metadataIndex":"{}"}}"#,
            body,
            total,
            total,
            page.len(),
            next_cursor,
            cluster,
            metadata_index,
        ),
    )
}
