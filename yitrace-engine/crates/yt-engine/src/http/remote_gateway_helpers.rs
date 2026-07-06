fn json_u64(item: &crate::wire::Json, key: &str) -> u64 {
    crate::wire::field(item, key)
        .and_then(crate::wire::Json::as_u64)
        .unwrap_or(0)
}

fn json_i64(item: &crate::wire::Json, key: &str) -> Option<i64> {
    match crate::wire::field(item, key) {
        Some(crate::wire::Json::Num(s)) => s.parse::<i64>().ok(),
        _ => None,
    }
}

fn json_array_items(item: &crate::wire::Json, key: &str) -> Vec<String> {
    crate::wire::field(item, key)
        .map(crate::wire::Json::as_array)
        .unwrap_or_default()
        .iter()
        .map(crate::wire::Json::to_compact_json)
        .collect()
}

fn remote_cost_nanos(item: &crate::wire::Json) -> u64 {
    if let Some(detail) = crate::wire::field(item, "costDetail") {
        return json_u64(detail, "costUsdNanos");
    }
    crate::wire::field(item, "costUsd")
        .and_then(crate::wire::Json::as_f64)
        .map(|cost| (cost * 1_000_000_000.0).max(0.0) as u64)
        .unwrap_or(0)
}

fn ratio_f64(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn opt_i64_json(value: Option<i64>) -> String {
    value
        .map(|v| v.to_string())
        .unwrap_or_else(|| "null".to_string())
}

fn remote_trace_id_from_body(body: &str) -> Option<u64> {
    let value = crate::wire::parse(body).ok()?;
    json_field_alias(
        &value,
        &[
            "trace_id",
            "traceId",
            "sourceTraceId",
            "source_trace_id",
            "candidateTraceId",
            "candidate_trace_id",
        ],
    )
    .and_then(json_id_with_external)
    .map(|(id, _)| id)
}

fn remote_global_metadata_id(shard_idx: usize, response: &str, id_field: &str) -> Option<u64> {
    let value = crate::wire::parse(response).ok()?;
    let local = crate::wire::field(&value, id_field).and_then(json_internal_id)?;
    Some(remote_make_metadata_id(shard_idx, local))
}

fn remote_make_metadata_id(shard_idx: usize, local_id: u64) -> u64 {
    (((shard_idx as u64) + 1) << 56) | (local_id & ((1u64 << 56) - 1))
}

fn remote_split_metadata_id(global_id: u64, shard_count: usize) -> Option<(usize, u64)> {
    let prefix = global_id >> 56;
    if prefix == 0 {
        return None;
    }
    let idx = prefix.saturating_sub(1) as usize;
    if idx >= shard_count {
        return None;
    }
    Some((idx, global_id & ((1u64 << 56) - 1)))
}

fn rewrite_top_level_id(body: &str, id_field: &str, id: Option<u64>) -> String {
    let Some(id) = id else {
        return body.to_string();
    };
    let Ok(mut value) = crate::wire::parse(body) else {
        return body.to_string();
    };
    rewrite_json_id_value(&mut value, id_field, id);
    value.to_compact_json()
}

fn rewrite_json_id_in_place(value: &mut crate::wire::Json, id_field: &str, shard_idx: usize) {
    let Some(local) = json_field_alias(value, &[id_field]).and_then(json_internal_id) else {
        return;
    };
    rewrite_json_id_value(value, id_field, remote_make_metadata_id(shard_idx, local));
}

fn rewrite_json_id_value(value: &mut crate::wire::Json, id_field: &str, id: u64) {
    let crate::wire::Json::Obj(fields) = value else {
        return;
    };
    for (key, val) in fields.iter_mut() {
        if key == id_field {
            *val = crate::wire::Json::Str(id.to_string());
            return;
        }
    }
}

fn rewrite_json_body_id(value: &crate::wire::Json, aliases: &[&str], id: u64) -> String {
    let crate::wire::Json::Obj(fields) = value else {
        return value.to_compact_json();
    };
    let mut out = Vec::new();
    for (key, val) in fields {
        let encoded = if aliases.iter().any(|alias| key == alias) {
            json_string_value(&id.to_string())
        } else {
            val.to_compact_json()
        };
        out.push(format!(r#""{}":{}"#, gateway_json_escape(key), encoded));
    }
    format!("{{{}}}", out.join(","))
}

fn remote_replace_last_path_id(base: &str, query: &str, old_id: &str, new_id: u64) -> String {
    let mut path = base.to_string();
    if let Some(pos) = path.rfind(old_id) {
        path.replace_range(pos..pos + old_id.len(), &new_id.to_string());
    }
    if query.is_empty() {
        path
    } else {
        format!("{path}?{query}")
    }
}

fn remote_items_json_from_body(body: &str) -> Vec<crate::wire::Json> {
    let Ok(value) = crate::wire::parse(body) else {
        return Vec::new();
    };
    match &value {
        crate::wire::Json::Arr(items) => items.clone(),
        crate::wire::Json::Obj(_) => crate::wire::field(&value, "items")
            .map(crate::wire::Json::as_array)
            .unwrap_or_default()
            .to_vec(),
        _ => Vec::new(),
    }
}

fn remote_sort_time(value: &crate::wire::Json) -> u64 {
    gateway_json_u64_alias(value, &["updatedAtNs", "createdAtNs"]).unwrap_or(0)
}

fn remote_any_id(value: &crate::wire::Json) -> u64 {
    gateway_json_u64_alias(
        value,
        &[
            "annotationId",
            "associationId",
            "goldenPathId",
            "auditId",
            "policyId",
        ],
    )
    .unwrap_or(0)
}
