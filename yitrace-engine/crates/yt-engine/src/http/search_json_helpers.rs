fn search_json_request(body: &str, tenant: Option<u64>) -> Result<SearchJsonRequest, String> {
    use crate::wire::{field, parse, Json};
    let v = parse(body)?;
    let mut text = field(&v, "text")
        .and_then(Json::as_str)
        .unwrap_or("")
        .to_string();
    let mut text_domains = search_text_domains_from_json(&v);
    if text.is_empty() {
        if let Some((domain, query)) = search_domain_query_alias(&v) {
            text = query;
            text_domains = vec![domain];
        }
    }
    let k = field(&v, "k").and_then(Json::as_u64).unwrap_or(10) as usize;
    let vector: Vec<f32> = field(&v, "vector")
        .map(|j| j.as_array().iter().filter_map(Json::as_f32).collect())
        .unwrap_or_default();
    let mut filter = crate::SearchFilter::default();
    if let Some(f) = field(&v, "filter") {
        filter.trace_id = field(f, "trace_id").and_then(json_id_or_hash);
        filter.agent_name = field(f, "agent_name")
            .and_then(Json::as_str)
            .map(ToString::to_string);
        filter.status = field(f, "status").and_then(Json::as_u64).map(|x| x as u8);
        filter.time_from = field(f, "time_from").and_then(Json::as_i64);
        filter.time_to = field(f, "time_to").and_then(Json::as_i64);
        collect_attr_filters(f, &mut filter);
    }
    // 租户来自鉴权头（X-Tenant-Id），覆盖请求体，客户端不能越权查别的租户。
    filter.tenant_id = tenant;
    Ok(SearchJsonRequest {
        raw_body: body.to_string(),
        text,
        text_domains,
        vector,
        k,
        filter,
        include_fanout: json_bool_alias(&v, &["includeFanout", "include_fanout"])
            .unwrap_or(false),
    })
}

fn search_text_domains_from_json(v: &crate::wire::Json) -> Vec<crate::TextDomain> {
    use crate::wire::{field, Json};
    let mut out = Vec::new();
    for name in ["textDomains", "text_domains", "domains", "fields"] {
        let Some(value) = field(v, name) else {
            continue;
        };
        match value {
            Json::Arr(items) => {
                for item in items {
                    if let Some(domain) = item.as_str().and_then(crate::TextDomain::parse) {
                        if !out.contains(&domain) {
                            out.push(domain);
                        }
                    }
                }
            }
            Json::Str(s) => {
                if let Some(domain) = crate::TextDomain::parse(s) {
                    if !out.contains(&domain) {
                        out.push(domain);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn search_domain_query_alias(v: &crate::wire::Json) -> Option<(crate::TextDomain, String)> {
    use crate::wire::Json;
    for (names, domain) in [
        (
            &["inputTextContains", "input_text", "inputText", "inputContains"][..],
            crate::TextDomain::Input,
        ),
        (
            &["outputContains", "output_text", "outputText"][..],
            crate::TextDomain::Output,
        ),
        (
            &["logContains", "log_text", "logText"][..],
            crate::TextDomain::Logs,
        ),
        (
            &["toolNameContains", "tool_name", "toolName"][..],
            crate::TextDomain::Tool,
        ),
        (&["modelContains", "model"][..], crate::TextDomain::Model),
        (
            &["agentNameContains", "agent_name", "agentName"][..],
            crate::TextDomain::Agent,
        ),
    ] {
        if let Some(query) = json_field_alias(v, names)
            .and_then(Json::as_str)
            .map(ToString::to_string)
        {
            return Some((domain, query));
        }
    }
    None
}

fn json_search_hits(hits: &[(FoldedSpan, f32)], request: &SearchJsonRequest) -> String {
    let items = json_search_hit_items(hits, request);
    format!("[{}]", items.join(","))
}

fn json_search_hits_with_fanout(
    hits: &[(FoldedSpan, f32)],
    request: &SearchJsonRequest,
    report: &FanoutReport,
) -> String {
    let items = json_search_hit_items(hits, request);
    format!(
        r#"{{"items":[{}],"total":{},"queryMode":"fanout_merge","searchIndex":"{}","textDomains":{}{} }}"#,
        items.join(","),
        items.len(),
        search_index_label(request),
        crate::text_domain_names_json(&request.text_domains),
        report.json_fields()
    )
}

fn json_search_hit_items(hits: &[(FoldedSpan, f32)], request: &SearchJsonRequest) -> Vec<String> {
    let items: Vec<String> = hits
        .iter()
        .map(|(s, score)| {
            let logs: Vec<String> = s.logs.iter().map(|l| json_string_value(l)).collect();
            format!(
                r#"{{"trace_id":{},"span_id":{},"external_trace_id":{},"external_span_id":{},"score":{:.4},"searchIndex":"{}","textDomains":{},"status":{},"duration_ns":{},"agent_name":{},"logs":[{}],"fields":{},"attrs":{}}}"#,
                s.trace_id,
                s.span_id,
                json_opt_str(s.external_trace_id.as_deref()),
                json_opt_str(s.external_span_id.as_deref()),
                score,
                search_index_label(request),
                crate::text_domain_names_json(&request.text_domains),
                s.status.map_or("null".to_string(), |x| x.to_string()),
                s.duration_ns.map_or("null".to_string(), |x| x.to_string()),
                s.agent_name
                    .as_ref()
                    .map_or("null".to_string(), |a| json_string_value(a)),
                logs.join(","),
                json_folded_agent_fields(s),
                json_attrs(&s.attrs),
            )
        })
        .collect();
    items
}

fn search_index_label(request: &SearchJsonRequest) -> &'static str {
    match (
        !request.text.is_empty(),
        !request.vector.is_empty(),
        request.text_domains.is_empty(),
    ) {
        (true, true, true) => "hybrid_bm25_vector",
        (true, true, false) => "hybrid_text_domain_vector",
        (true, false, true) => "bm25_all_text",
        (true, false, false) => "text_domain_bm25",
        (false, true, _) => "vector_graph",
        _ => "bm25_all_text",
    }
}

fn fuse_search_hit_rows(
    text_hits: Vec<(FoldedSpan, f32)>,
    vector_hits: Vec<(FoldedSpan, f32)>,
    k: usize,
) -> Vec<(FoldedSpan, f32)> {
    let mut spans: HashMap<(u64, u64), FoldedSpan> = HashMap::new();
    let mut ranked: Vec<Vec<(u64, u64)>> = Vec::new();
    for hits in [&text_hits, &vector_hits] {
        ranked.push(
            hits.iter()
                .map(|(span, _)| {
                    spans
                        .entry((span.trace_id, span.span_id))
                        .or_insert_with(|| span.clone());
                    (span.trace_id, span.span_id)
                })
                .collect(),
        );
    }
    yt_core::rank::rrf_fuse(&ranked, 60.0)
        .into_iter()
        .take(k)
        .filter_map(|((trace_id, span_id), score)| {
            spans
                .remove(&(trace_id, span_id))
                .map(|span| (span, score))
        })
        .collect()
}

fn json_string_value(s: &str) -> String {
    format!("\"{}\"", json_escape(s))
}

fn json_opt_preview(value: Option<&str>) -> String {
    value
        .map(trunc)
        .map(|s| json_string_value(&s))
        .unwrap_or_else(|| "null".to_string())
}

fn collect_attr_filters(f: &crate::wire::Json, filter: &mut crate::SearchFilter) {
    collect_attr_map(f, &mut filter.attrs);
}

fn collect_golden_path_scope_attrs(
    spans: &[FoldedSpan],
    attrs: &mut std::collections::BTreeMap<String, String>,
) {
    for key in golden_path_scope_keys() {
        if attrs.contains_key(*key) {
            continue;
        }
        if let Some(value) = spans.iter().find_map(|s| golden_path_scope_value(s, key)) {
            attrs.insert((*key).to_string(), json_string_value(&value));
        }
    }
}

fn remove_top_level_golden_path_governance_attrs(
    f: &crate::wire::Json,
    attrs: &mut std::collections::BTreeMap<String, String>,
) {
    if json_raw_field_alias(f, &["eval_profile", "evalProfile"]).is_some() {
        attrs.remove("eval_profile");
    }
}

fn golden_path_scope_value(s: &FoldedSpan, key: &str) -> Option<String> {
    match key {
        "model" => s.model.clone(),
        "provider" => s.provider.clone(),
        _ => crate::folded_span_attr_value(s, key).map(json_compact_label),
    }
}

fn golden_path_scope_keys() -> &'static [&'static str] {
    &[
        "project_id",
        "task_fingerprint",
        "skill",
        "mode",
        "harness_version",
        "schema_fingerprint",
        "eval_profile",
        "model",
        "provider",
        "tool_version",
    ]
}
