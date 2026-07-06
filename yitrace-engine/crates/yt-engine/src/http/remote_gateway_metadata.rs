impl RemoteShardGateway {
    fn remote_owner_index_for_trace(&self, tenant: Option<u64>, trace_id: u64) -> usize {
        self.router
            .shard_index_for_record(tenant, None, trace_id, self.shard_count())
    }

    fn remote_metadata_id_route(
        &self,
        method: &str,
        base: &str,
        query: &str,
        id: &str,
        body: &str,
        tenant: Option<u64>,
        id_field: &str,
    ) -> (u16, String) {
        let shards = self.shards_snapshot();
        let Some(global_id) = parse_id_or_hash(id) else {
            return (400, r#"{"error":"bad metadata id"}"#.to_string());
        };
        let Some((idx, local_id)) = remote_split_metadata_id(global_id, shards.len()) else {
            return self.remote_metadata_id_fanout(method, base, query, id, body, tenant, id_field);
        };
        let path = remote_replace_last_path_id(base, query, id, local_id);
        match shards[idx].route_json_with_tenant(method, &path, body, tenant) {
            Ok((200, response)) => (
                200,
                rewrite_top_level_id(&response, id_field, Some(global_id)),
            ),
            Ok((status, response)) => (status, response),
            Err(error) => (
                503,
                format!(r#"{{"error":"metadata owner shard unavailable","detail":"{}"}}"#, gateway_json_escape(&error)),
            ),
        }
    }

    fn remote_metadata_id_fanout(
        &self,
        method: &str,
        base: &str,
        query: &str,
        id: &str,
        body: &str,
        tenant: Option<u64>,
        id_field: &str,
    ) -> (u16, String) {
        let shards = self.shards_snapshot();
        for (idx, shard) in shards.iter().enumerate() {
            let path = remote_replace_last_path_id(base, query, id, parse_id_or_hash(id).unwrap_or(0));
            match shard.route_json_with_tenant(method, &path, body, tenant) {
                Ok((200, response)) => {
                    let global = remote_global_metadata_id(idx, &response, id_field);
                    return (200, rewrite_top_level_id(&response, id_field, global));
                }
                Ok((404, _)) => {}
                Ok((status, response)) => return (status, response),
                Err(_) => {}
            }
        }
        (404, r#"{"error":"metadata not found"}"#.to_string())
    }

    fn remote_golden_path_id_route(
        &self,
        method: &str,
        base: &str,
        query: &str,
        id: &str,
        body: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let shards = self.shards_snapshot();
        let Some(global_id) = parse_id_or_hash(id) else {
            return (400, r#"{"error":"bad golden path id"}"#.to_string());
        };
        let Some((idx, local_id)) = remote_split_metadata_id(global_id, shards.len()) else {
            return (400, r#"{"error":"golden path id is not gateway-scoped"}"#.to_string());
        };
        let path = remote_replace_last_path_id(base, query, id, local_id);
        match shards[idx].route_json_with_tenant(method, &path, body, tenant) {
            Ok(result) => result,
            Err(error) => (
                503,
                format!(r#"{{"error":"golden path owner shard unavailable","detail":"{}"}}"#, gateway_json_escape(&error)),
            ),
        }
    }

    fn remote_golden_path_owner_from_body(&self, body: &str) -> Option<(usize, String)> {
        let value = crate::wire::parse(body).ok()?;
        let id = json_field_alias(&value, &["golden_path_id", "goldenPathId", "id"])
            .and_then(json_internal_id)?;
        let (idx, local_id) = remote_split_metadata_id(id, self.shard_count())?;
        let local_body =
            rewrite_json_body_id(&value, &["golden_path_id", "goldenPathId", "id"], local_id);
        Some((idx, local_body))
    }

    fn remote_create_trace_metadata_json(
        &self,
        path: &str,
        body: &str,
        tenant: Option<u64>,
        id_field: &str,
    ) -> (u16, String) {
        let Some(trace_id) = remote_trace_id_from_body(body) else {
            return (400, r#"{"error":"missing traceId"}"#.to_string());
        };
        let idx = self.remote_owner_index_for_trace(tenant, trace_id);
        let shards = self.shards_snapshot();
        match shards[idx].route_json_with_tenant("POST", path, body, tenant) {
            Ok((200, response)) => (
                200,
                rewrite_top_level_id(
                    &response,
                    id_field,
                    remote_global_metadata_id(idx, &response, id_field),
                ),
            ),
            Ok((status, response)) => (status, response),
            Err(error) => (
                503,
                format!(r#"{{"error":"metadata owner shard unavailable","detail":"{}"}}"#, gateway_json_escape(&error)),
            ),
        }
    }

    fn remote_metadata_items_json(
        &self,
        method: &str,
        path: &str,
        body: &str,
        tenant: Option<u64>,
        id_field: &str,
    ) -> (u16, String) {
        let policy = remote_consistency_from_path_body(path, body);
        let mut items = Vec::<crate::wire::Json>::new();
        let mut failed = Vec::new();
        let mut ok_shards = 0usize;
        let (read_targets, results) =
            match self.fanout_read_route(method, path, body, tenant, policy.strict) {
                Ok(result) => result,
                Err(resp) => return resp,
            };
        for (idx, result) in results {
            match result {
                Ok((200, response)) => {
                    ok_shards += 1;
                    let mut shard_items = remote_items_json_from_body(&response);
                    if !id_field.is_empty() {
                        for item in &mut shard_items {
                            rewrite_json_id_in_place(item, id_field, idx);
                        }
                    }
                    items.extend(shard_items);
                }
                Ok((status, response)) => failed.push(remote_failed_shard(idx, status, &response)),
                Err(error) => failed.push(remote_unreachable_shard(idx, &error)),
            }
        }
        if let Some(resp) = policy.reject_degraded(self.shard_count(), ok_shards, &failed) {
            return resp;
        }
        if ok_shards == 0 {
            return remote_all_shards_failed(self.shard_count(), failed);
        }
        items.sort_by(|a, b| {
            remote_sort_time(b)
                .cmp(&remote_sort_time(a))
                .then_with(|| remote_any_id(b).cmp(&remote_any_id(a)))
        });
        let total = items.len();
        let body = items
            .iter()
            .map(crate::wire::Json::to_compact_json)
            .collect::<Vec<_>>()
            .join(",");
        remote_items_response(
            body,
            total,
            self.shard_count(),
            ok_shards,
            failed,
            policy,
            &read_targets,
            r#","metadataIndex":"remote_fanout_metadata_merge""#,
        )
    }

    fn remote_retention_policy_create_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        // Retention policies are control-plane metadata. Until a separate control plane exists,
        // persist a copy on every shard so any retention run-due fanout can evaluate locally.
        self.remote_retention_fanout_json("POST", "/v1/retention-policies", body, tenant)
    }

    fn remote_golden_path_export_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        let policy = remote_consistency_from_body(body);
        let mut records = Vec::new();
        let mut failed = Vec::new();
        let mut ok_shards = 0usize;
        let (read_targets, results) = match self.fanout_read_route(
            "POST",
            "/v1/golden-path-export",
            body,
            tenant,
            policy.strict,
        ) {
            Ok(result) => result,
            Err(resp) => return resp,
        };
        for (idx, result) in results {
            match result {
                Ok((200, response)) => {
                    ok_shards += 1;
                    records.extend(remote_items_json_from_body(&response));
                }
                Ok((status, response)) => failed.push(remote_failed_shard(idx, status, &response)),
                Err(error) => failed.push(remote_unreachable_shard(idx, &error)),
            }
        }
        if let Some(resp) = policy.reject_degraded(self.shard_count(), ok_shards, &failed) {
            return resp;
        }
        if ok_shards == 0 {
            return remote_all_shards_failed(self.shard_count(), failed);
        }
        let items = records
            .iter()
            .map(crate::wire::Json::to_compact_json)
            .collect::<Vec<_>>();
        let jsonl = items.join("\n");
        (
            200,
            format!(
                r#"{{"schemaVersion":"yitrace.golden_path_export.v1","format":"jsonl","count":{},"items":[{}],"jsonl":{},"queryMode":"process_gateway_fanout","shardCount":{},"okShards":{},"degraded":{},"failedShards":[{}]{},"readTargets":[{}] }}"#,
                items.len(),
                items.join(","),
                json_string_value(&jsonl),
                self.shard_count(),
                ok_shards,
                !failed.is_empty(),
                failed.join(","),
                policy.json_fields(),
                remote_read_targets_json(&read_targets)
            )
            .replace(" }", "}"),
        )
    }

    fn remote_golden_path_owner_route_json(
        &self,
        path: &str,
        body: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let Some((idx, local_body)) = self.remote_golden_path_owner_from_body(body) else {
            return (400, r#"{"error":"missing goldenPathId"}"#.to_string());
        };
        let shards = self.shards_snapshot();
        match shards[idx].route_json_with_tenant("POST", path, &local_body, tenant) {
            Ok((status, response)) => (status, response),
            Err(error) => (
                503,
                format!(r#"{{"error":"golden path owner shard unavailable","detail":"{}"}}"#, gateway_json_escape(&error)),
            ),
        }
    }

    fn remote_dynamic_route_json(
        &self,
        method: &str,
        path: &str,
        body: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let (base, query) = path.split_once('?').unwrap_or((path, ""));
        let segs: Vec<&str> = base
            .trim_start_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();
        match (method, segs.as_slice()) {
            ("PATCH", ["v1", "annotations", id])
            | ("POST", ["v1", "annotations", id, "status"])
            | ("DELETE", ["v1", "annotations", id]) => {
                self.remote_metadata_id_route(method, base, query, id, body, tenant, "annotationId")
            }
            ("POST", ["v1", "golden-paths", id, "status"]) => self.remote_metadata_id_route(
                method,
                base,
                query,
                id,
                body,
                tenant,
                "goldenPathId",
            ),
            ("POST", ["v1", "golden-paths", id, "adherence"])
            | ("POST", ["v1", "golden-paths", id, "evidence"])
            | ("POST", ["v1", "golden-paths", id, "health"]) => {
                self.remote_golden_path_id_route(method, base, query, id, body, tenant)
            }
            ("DELETE", ["v1", "snapshots", id]) => self.remote_snapshot_release_json(id, tenant),
            _ => (400, r#"{"error":"unsupported gateway route"}"#.to_string()),
        }
    }
}
