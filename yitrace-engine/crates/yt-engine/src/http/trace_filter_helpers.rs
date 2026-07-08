fn collect_attr_filters(f: &crate::wire::Json, filter: &mut crate::SearchFilter) {
    use crate::wire::{field, Json};
    for key in [
        "project_id",
        "skill",
        "mode",
        "call_site",
        "task_fingerprint",
        "loop_id",
        "harness_version",
        "schema_fingerprint",
        "intent_signature",
        "validation_status",
        "review_status",
        "eval_status",
        "path_memory_id",
        "stop_reason",
        "phase",
        "validator",
    ] {
        if let Some(v) = field(f, key) {
            filter.attrs.insert(key.to_string(), v.to_compact_json());
        }
    }
    for (alias, key) in [
        ("projectId", "project_id"),
        ("callSite", "call_site"),
        ("taskFingerprint", "task_fingerprint"),
        ("loopId", "loop_id"),
        ("harnessVersion", "harness_version"),
        ("schemaFingerprint", "schema_fingerprint"),
        ("intentSignature", "intent_signature"),
        ("validationStatus", "validation_status"),
        ("reviewStatus", "review_status"),
        ("evalStatus", "eval_status"),
        ("pathMemoryId", "path_memory_id"),
        ("stopReason", "stop_reason"),
    ] {
        if let Some(v) = field(f, alias) {
            filter.attrs.insert(key.to_string(), v.to_compact_json());
        }
    }
    if let Some(Json::Obj(kvs)) = field(f, "attrs") {
        for (k, v) in kvs {
            if is_read_model_attr_key(k) {
                filter.attrs.insert(k.clone(), v.to_compact_json());
            }
        }
    }
}

fn collect_attr_query_json(s: &str, attrs: &mut std::collections::BTreeMap<String, String>) {
    use crate::wire::Json;
    let Ok(Json::Obj(kvs)) = crate::wire::parse(s) else {
        return;
    };
    for (k, v) in kvs {
        let key = normalize_group_key(&k);
        if is_read_model_attr_key(&key) {
            attrs.insert(key, v.to_compact_json());
        }
    }
}

fn is_read_model_attr_key(key: &str) -> bool {
    matches!(
        key,
        "project_id"
            | "skill"
            | "mode"
            | "call_site"
            | "task_fingerprint"
            | "loop_id"
            | "harness_version"
            | "schema_fingerprint"
            | "intent_signature"
            | "validation_status"
            | "review_status"
            | "eval_status"
            | "path_memory_id"
            | "stop_reason"
            | "phase"
            | "validator"
    )
}

#[derive(Default)]
struct TraceSearchSpec {
    session_id: Option<u64>,
    span_id: Option<u64>,
    external_trace_id: Option<String>,
    external_span_id: Option<String>,
    external_session_id: Option<String>,
    status: Option<u8>,
    agent_name: Option<String>,
    tool_name: Option<String>,
    model: Option<String>,
    text: Option<String>,
    attrs: std::collections::BTreeMap<String, String>,
}

struct TraceSearchRead {
    spans: Vec<FoldedSpan>,
    read_plan: ReadPlanStats,
    cursor: usize,
    limit: usize,
    sort_by: String,
}

struct TraceSearchParsed {
    query: TraceQuery,
    spec: TraceSearchSpec,
    index_filter: SearchFilter,
    cursor: usize,
    limit: usize,
    sort_by: String,
}

fn json_field_alias<'a>(v: &'a crate::wire::Json, names: &[&str]) -> Option<&'a crate::wire::Json> {
    names.iter().find_map(|name| crate::wire::field(v, name))
}

fn json_raw_field_alias<'a>(
    v: &'a crate::wire::Json,
    names: &[&str],
) -> Option<&'a crate::wire::Json> {
    names.iter().find_map(|name| v.get(name))
}

fn parse_json_body_or_empty(body: &str) -> Result<crate::wire::Json, String> {
    if body.trim().is_empty() {
        Ok(crate::wire::Json::Obj(Vec::new()))
    } else {
        crate::wire::parse(body)
    }
}

fn optional_string_patch(obj: &crate::wire::Json, names: &[&str]) -> Option<Option<String>> {
    json_raw_field_alias(obj, names).and_then(|value| match value {
        crate::wire::Json::Null => Some(None),
        _ => value.as_str().map(|s| Some(s.to_string())),
    })
}

fn optional_score_patch(obj: &crate::wire::Json, names: &[&str]) -> Option<Option<u32>> {
    json_raw_field_alias(obj, names).and_then(|value| match value {
        crate::wire::Json::Null => Some(None),
        _ => value.as_u64().map(|n| Some(n.min(u32::MAX as u64) as u32)),
    })
}

fn json_bool_alias(obj: &crate::wire::Json, names: &[&str]) -> Option<bool> {
    json_field_alias(obj, names).and_then(|value| match value {
        crate::wire::Json::Bool(v) => Some(*v),
        crate::wire::Json::Num(s) | crate::wire::Json::Str(s) => {
            match s.to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "y" | "on" => Some(true),
                "0" | "false" | "no" | "n" | "off" => Some(false),
                _ => None,
            }
        }
        _ => None,
    })
}

fn collect_trace_search_attrs(f: &crate::wire::Json) -> std::collections::BTreeMap<String, String> {
    use crate::wire::{field, Json};
    let mut attrs = std::collections::BTreeMap::new();
    for (alias, key) in [
        ("projectId", "project_id"),
        ("callSite", "call_site"),
        ("taskFingerprint", "task_fingerprint"),
        ("loopId", "loop_id"),
        ("validationStatus", "validation_status"),
        ("reviewStatus", "review_status"),
        ("evalStatus", "eval_status"),
    ] {
        if let Some(v) = field(f, alias) {
            attrs.insert(key.to_string(), v.to_compact_json());
        }
    }
    for key in [
        "project_id",
        "skill",
        "mode",
        "call_site",
        "task_fingerprint",
        "loop_id",
        "harness_version",
        "schema_fingerprint",
        "intent_signature",
        "validation_status",
        "review_status",
        "eval_status",
        "path_memory_id",
        "stop_reason",
        "phase",
        "validator",
    ] {
        if let Some(v) = field(f, key) {
            attrs.insert(key.to_string(), v.to_compact_json());
        }
    }
    if let Some(Json::Obj(kvs)) = field(f, "attrs") {
        for (k, v) in kvs {
            attrs.insert(k.clone(), v.to_compact_json());
        }
    }
    attrs
}
