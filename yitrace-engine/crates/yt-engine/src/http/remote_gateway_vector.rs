impl RemoteShardGateway {
    fn vector_index_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        let shards = self.shards_snapshot();
        if shards.is_empty() {
            return (503, r#"{"error":"no writable shards"}"#.to_string());
        }
        let key = match gateway_vector_key(body) {
            Ok(key) => key,
            Err(error) => {
                return (
                    400,
                    format!(r#"{{"error":"{}"}}"#, gateway_json_escape(&error)),
                )
            }
        };
        let idx = (yt_core::event::fnv1a64(key.as_bytes()) as usize) % shards.len();
        match shards[idx].route_json_with_tenant("POST", "/v1/vector-index", body, tenant) {
            Ok((status, response)) => {
                if status == 200 {
                    (
                        200,
                        format!(
                            r#"{{"ok":true,"queryMode":"process_gateway_route","shard":{idx},"vectorIndex":"vector_namespace_flat","shardResponse":{response}}}"#
                        ),
                    )
                } else {
                    (status, response)
                }
            }
            Err(error) => (
                503,
                format!(
                    r#"{{"error":"vector index shard unavailable","shard":{idx},"detail":"{}"}}"#,
                    gateway_json_escape(&error)
                ),
            ),
        }
    }

    fn vector_search_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        let policy = remote_consistency_from_body(body);
        let k = gateway_vector_search_limit(body).unwrap_or(10);
        let (read_targets, results) =
            match self.fanout_read_route("POST", "/v1/vector-search", body, tenant, policy.strict) {
                Ok(result) => result,
                Err(resp) => return resp,
            };
        let mut items = Vec::<GatewayVectorItem>::new();
        let mut failed = Vec::new();
        let mut ok_shards = 0usize;
        for (idx, result) in results {
            match result {
                Ok((200, response)) => {
                    ok_shards += 1;
                    items.extend(gateway_vector_items_from_body(&response));
                }
                Ok((status, response)) => failed.push(format!(
                    r#"{{"shard":{idx},"status":{status},"error":"shard query failed","body":"{}"}}"#,
                    gateway_json_escape(&response)
                )),
                Err(error) => failed.push(format!(
                    r#"{{"shard":{idx},"status":0,"error":"shard unreachable","detail":"{}"}}"#,
                    gateway_json_escape(&error)
                )),
            }
        }
        if let Some(resp) = policy.reject_degraded(self.shard_count(), ok_shards, &failed) {
            return resp;
        }
        if ok_shards == 0 {
            return (
                503,
                format!(
                    r#"{{"error":"all shards unavailable","queryMode":"process_gateway_fanout","shardCount":{},"okShards":0,"degraded":true,"failedShards":[{}]{} }}"#,
                    self.shard_count(),
                    failed.join(","),
                    policy.json_fields()
                )
                .replace(" }", "}"),
            );
        }
        items.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.distance.total_cmp(&right.distance))
                .then_with(|| left.namespace.cmp(&right.namespace))
                .then_with(|| left.key.cmp(&right.key))
        });
        let mut seen = std::collections::HashSet::new();
        items.retain(|item| seen.insert((item.namespace.clone(), item.key.clone())));
        items.truncate(k);
        let merged: Vec<String> = items.into_iter().map(|item| item.json).collect();
        let degraded = !failed.is_empty();
        (
            200,
            format!(
                r#"{{"items":[{}],"total":{},"queryMode":"process_gateway_fanout","shardCount":{},"okShards":{ok_shards},"degraded":{degraded},"failedShards":[{}]{},"vectorIndex":"fanout_vector_namespace_flat","readTargets":[{}] }}"#,
                merged.join(","),
                merged.len(),
                self.shard_count(),
                failed.join(","),
                policy.json_fields(),
                remote_read_targets_json(&read_targets)
            )
            .replace(" }", "}"),
        )
    }
}

struct GatewayVectorItem {
    namespace: String,
    key: String,
    score: f32,
    distance: f32,
    json: String,
}

fn gateway_vector_key(body: &str) -> Result<String, String> {
    let value = crate::wire::parse(body)?;
    json_field_alias(&value, &["key", "id", "taskFingerprint", "trajectorySignature"])
        .and_then(crate::wire::Json::as_str)
        .map(ToString::to_string)
        .or_else(|| {
            let trace =
                json_field_alias(&value, &["trace_id", "traceId"]).and_then(json_id_or_hash)?;
            let span = json_field_alias(&value, &["span_id", "spanId"]).and_then(json_id_or_hash);
            Some(match span {
                Some(span) => format!("{trace}:{span}"),
                None => trace.to_string(),
            })
        })
        .ok_or_else(|| "vector key is required".to_string())
}

fn gateway_vector_search_limit(body: &str) -> Option<usize> {
    let value = crate::wire::parse(body).ok()?;
    json_field_alias(&value, &["k", "limit"])
        .and_then(crate::wire::Json::as_u64)
        .map(|value| value.clamp(1, 500) as usize)
}

fn gateway_vector_items_from_body(body: &str) -> Vec<GatewayVectorItem> {
    let Some(items) = json_items_from_body(body, "items") else {
        return Vec::new();
    };
    items
        .into_iter()
        .filter_map(|json| {
            let value = crate::wire::parse(&json).ok()?;
            Some(GatewayVectorItem {
                namespace: json_field_alias(&value, &["namespace"])
                    .and_then(crate::wire::Json::as_str)
                    .unwrap_or("")
                    .to_string(),
                key: json_field_alias(&value, &["key", "id"])
                    .and_then(crate::wire::Json::as_str)
                    .unwrap_or("")
                    .to_string(),
                score: json_field_alias(&value, &["score"])
                    .and_then(crate::wire::Json::as_f32)
                    .unwrap_or(0.0),
                distance: json_field_alias(&value, &["distance"])
                    .and_then(crate::wire::Json::as_f32)
                    .unwrap_or(f32::MAX),
                json,
            })
        })
        .collect()
}
