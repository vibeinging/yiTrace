use super::*;

impl EngineJsonApi {
    pub(super) fn vector_index_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        let input = match vector_embedding_input_from_json(body, tenant) {
            Ok(input) => input,
            Err(error) => return (400, format!(r#"{{"error":"{}"}}"#, json_escape(&error))),
        };
        match self.coord().index_named_embedding(input) {
            Ok(()) => (
                200,
                r#"{"ok":true,"vectorIndex":"vector_namespace_flat"}"#.to_string(),
            ),
            Err(error) => (400, format!(r#"{{"error":"{}"}}"#, json_escape(&error))),
        }
    }

    pub(super) fn vector_search_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        let (query, k, filter) = match vector_search_request_from_json(body, tenant) {
            Ok(request) => request,
            Err(error) => return (400, format!(r#"{{"error":"{}"}}"#, json_escape(&error))),
        };
        let hits = self.coord().search_named_embeddings(&query, k, &filter);
        let items: Vec<String> = hits.iter().map(json_vector_search_hit).collect();
        (
            200,
            format!(
                r#"{{"items":[{}],"total":{},"vectorIndex":"vector_namespace_flat"}}"#,
                items.join(","),
                items.len()
            ),
        )
    }
}

fn vector_embedding_input_from_json(
    body: &str,
    tenant: Option<u64>,
) -> Result<crate::VectorEmbeddingInput, String> {
    use crate::wire::{parse, Json};
    let value = parse(body)?;
    let namespace = json_field_alias(&value, &["namespace", "vectorNamespace"])
        .and_then(Json::as_str)
        .and_then(crate::VectorNamespace::parse)
        .ok_or_else(|| "namespace must be span, task, or trajectory".to_string())?;
    let key = json_field_alias(
        &value,
        &["key", "id", "taskFingerprint", "trajectorySignature"],
    )
    .and_then(Json::as_str)
    .map(ToString::to_string)
    .or_else(|| {
        let trace = json_field_alias(&value, &["trace_id", "traceId"]).and_then(json_id_or_hash)?;
        let span = json_field_alias(&value, &["span_id", "spanId"]).and_then(json_id_or_hash);
        Some(match span {
            Some(span) => format!("{trace}:{span}"),
            None => trace.to_string(),
        })
    })
    .ok_or_else(|| "vector key is required".to_string())?;
    let embedding = json_field_alias(&value, &["embedding", "vector"])
        .map(|v| v.as_array().iter().filter_map(Json::as_f32).collect())
        .unwrap_or_default();
    let mut attrs = BTreeMap::new();
    collect_attr_map(&value, &mut attrs);
    Ok(crate::VectorEmbeddingInput {
        namespace,
        key,
        tenant_id: tenant,
        trace_id: json_field_alias(&value, &["trace_id", "traceId"]).and_then(json_id_or_hash),
        span_id: json_field_alias(&value, &["span_id", "spanId"]).and_then(json_id_or_hash),
        attrs,
        embedding,
    })
}

fn vector_search_request_from_json(
    body: &str,
    tenant: Option<u64>,
) -> Result<(Vec<f32>, usize, crate::VectorSearchFilter), String> {
    use crate::wire::{parse, Json};
    let value = parse(body)?;
    let query: Vec<f32> = json_field_alias(&value, &["embedding", "vector", "queryVector"])
        .map(|v| v.as_array().iter().filter_map(Json::as_f32).collect())
        .unwrap_or_default();
    if query.is_empty() {
        return Err("vector query is required".to_string());
    }
    let k = json_field_alias(&value, &["k", "limit"])
        .and_then(Json::as_u64)
        .unwrap_or(10)
        .clamp(1, 500) as usize;
    let filter_json = json_field_alias(&value, &["filter"]).unwrap_or(&value);
    let mut attrs = BTreeMap::new();
    collect_attr_map(filter_json, &mut attrs);
    let filter = crate::VectorSearchFilter {
        namespace: json_field_alias(filter_json, &["namespace", "vectorNamespace"])
            .and_then(Json::as_str)
            .and_then(crate::VectorNamespace::parse),
        tenant_id: tenant,
        key: json_field_alias(
            filter_json,
            &["key", "id", "taskFingerprint", "trajectorySignature"],
        )
        .and_then(Json::as_str)
        .map(ToString::to_string),
        trace_id: json_field_alias(filter_json, &["trace_id", "traceId"]).and_then(json_id_or_hash),
        span_id: json_field_alias(filter_json, &["span_id", "spanId"]).and_then(json_id_or_hash),
        attrs,
    };
    Ok((query, k, filter))
}

fn json_vector_search_hit(hit: &crate::VectorSearchHit) -> String {
    format!(
        r#"{{"namespace":"{}","key":{},"tenantId":{},"traceId":{},"spanId":{},"distance":{:.6},"score":{:.6},"attrs":{}}}"#,
        hit.namespace.as_str(),
        json_string_value(&hit.key),
        hit.tenant_id
            .map_or("null".to_string(), |id| id.to_string()),
        hit.trace_id.map_or("null".to_string(), |id| id.to_string()),
        hit.span_id.map_or("null".to_string(), |id| id.to_string()),
        hit.distance,
        hit.score,
        json_attrs(&hit.attrs),
    )
}
