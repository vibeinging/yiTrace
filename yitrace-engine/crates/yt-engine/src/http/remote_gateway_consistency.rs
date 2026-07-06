#[derive(Clone, Copy)]
struct RemoteConsistencyPolicy {
    strict: bool,
}

impl RemoteConsistencyPolicy {
    fn partial() -> Self {
        Self { strict: false }
    }

    fn strict() -> Self {
        Self { strict: true }
    }

    fn json_fields(&self) -> String {
        if self.strict {
            r#","consistencyUsed":"strict","partial":false"#.to_string()
        } else {
            r#","consistencyUsed":"partial","partial":true"#.to_string()
        }
    }

    fn reject_degraded(
        &self,
        shard_count: usize,
        ok_shards: usize,
        failed: &[String],
    ) -> Option<(u16, String)> {
        if !self.strict || failed.is_empty() {
            return None;
        }
        let status = if ok_shards == 0 { 503 } else { 502 };
        Some((
            status,
            format!(
                r#"{{"error":"strict consistency requires all shards","queryMode":"process_gateway_fanout","shardCount":{},"okShards":{},"degraded":true,"failedShards":[{}]{} }}"#,
                shard_count,
                ok_shards,
                failed.join(","),
                self.json_fields()
            )
            .replace(" }", "}"),
        ))
    }
}

fn remote_consistency_from_body(body: &str) -> RemoteConsistencyPolicy {
    crate::wire::parse(body)
        .ok()
        .map(|value| remote_consistency_from_json(&value))
        .unwrap_or_else(RemoteConsistencyPolicy::partial)
}

fn remote_consistency_from_path_body(path: &str, body: &str) -> RemoteConsistencyPolicy {
    let body_policy = remote_consistency_from_body(body);
    if body_policy.strict {
        return body_policy;
    }
    let Some((_, query)) = path.split_once('?') else {
        return body_policy;
    };
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if key.eq_ignore_ascii_case("consistency") && is_strict_consistency(value) {
            return RemoteConsistencyPolicy::strict();
        }
        if key.eq_ignore_ascii_case("partial")
            && (value.eq_ignore_ascii_case("false") || value == "0")
        {
            return RemoteConsistencyPolicy::strict();
        }
    }
    body_policy
}

fn remote_consistency_from_json(value: &crate::wire::Json) -> RemoteConsistencyPolicy {
    if json_field_alias(value, &["consistency", "consistencyPolicy"])
        .and_then(crate::wire::Json::as_str)
        .map(is_strict_consistency)
        .unwrap_or(false)
    {
        return RemoteConsistencyPolicy::strict();
    }
    if let Some(crate::wire::Json::Bool(false)) = json_field_alias(value, &["partial"]) {
        return RemoteConsistencyPolicy::strict();
    }
    RemoteConsistencyPolicy::partial()
}

fn is_strict_consistency(value: &str) -> bool {
    value.eq_ignore_ascii_case("strict") || value.eq_ignore_ascii_case("strong")
}
