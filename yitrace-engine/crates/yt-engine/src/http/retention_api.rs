use super::*;

impl EngineJsonApi {
    pub(super) fn retention_plan_json(
        &self,
        body: &str,
        tenant: Option<u64>,
        route_apply: bool,
    ) -> (u16, String) {
        use crate::wire::parse;
        let v = match parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let config = match retention_plan_config_from_json(&v, route_apply) {
            Ok(config) => config,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        if self.is_in_process_cluster() {
            return self.cluster_retention_plan_json(&v, &config, tenant);
        }

        let outcome = match self.retention_plan_for_coord(self.coord(), &v, &config, tenant, 0) {
            Ok(outcome) => outcome,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };

        (
            200,
            json_retention_plan(
                &config,
                &outcome.candidate_stats,
                &outcome.protected_stats,
                &outcome.deletable_stats,
                &outcome.protected,
                &outcome.deletable_trace_ids,
                outcome.applied.as_ref(),
                outcome.compacted.as_ref(),
                outcome.audit.as_ref(),
            ),
        )
    }

    pub(super) fn cluster_retention_plan_json(
        &self,
        v: &crate::wire::Json,
        config: &RetentionPlanConfig,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let mut shards = Vec::new();
        for (idx, shard) in self.shards().iter().enumerate() {
            let id_base = cluster_metadata_id_base(idx);
            let outcome =
                match self.retention_plan_for_coord(&shard.coord, v, config, tenant, id_base) {
                    Ok(outcome) => outcome,
                    Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
                };
            shards.push(ShardRetentionPlanOutcome {
                shard_id: shard.id.clone(),
                outcome,
            });
        }
        let aggregate = merge_retention_plan_outcomes(&shards);
        (
            200,
            json_retention_plan_with_cluster(config, &aggregate, &shards, self.shards().len()),
        )
    }

    pub(super) fn retention_plan_for_coord(
        &self,
        coord: &WriteCoordinator,
        v: &crate::wire::Json,
        config: &RetentionPlanConfig,
        tenant: Option<u64>,
        audit_id_base: u64,
    ) -> Result<RetentionPlanOutcome, String> {
        let (snap, spans) = match self.filtered_spans_for_storage_for_coord(coord, v, tenant) {
            Ok(v) => v,
            Err(e) => return Err(e),
        };
        let all_trace_ids: std::collections::HashSet<u64> =
            spans.iter().map(|s| s.trace_id).collect();
        let bounds = coord.trace_time_bounds(&snap, &all_trace_ids);
        let metadata = self.storage_metadata_for_coord(coord, tenant);

        let mut candidate_trace_ids = std::collections::HashSet::new();
        for trace_id in &all_trace_ids {
            let Some((_, max_ts)) = bounds.get(trace_id) else {
                continue;
            };
            if config.cutoff.map(|cut| *max_ts <= cut).unwrap_or(true) {
                candidate_trace_ids.insert(*trace_id);
            }
        }

        let protected = protected_trace_reasons(
            &candidate_trace_ids,
            &metadata,
            config.protect_golden_paths,
            config.protect_annotations,
            config.protect_dataset_associations,
            config.protect_snapshots,
            config.protect_eval_links,
            config.protect_path_memory,
        );
        let protected_trace_ids: std::collections::HashSet<u64> =
            protected.keys().copied().collect();
        let deletable_trace_ids: std::collections::HashSet<u64> = candidate_trace_ids
            .difference(&protected_trace_ids)
            .copied()
            .collect();

        let mut candidate_stats =
            storage_bucket_for_trace_ids(&spans, &bounds, &candidate_trace_ids);
        let mut protected_stats =
            storage_bucket_for_trace_ids(&spans, &bounds, &protected_trace_ids);
        let mut deletable_stats =
            storage_bucket_for_trace_ids(&spans, &bounds, &deletable_trace_ids);
        storage_bucket_apply_metadata_counts(&mut candidate_stats, &metadata);
        storage_bucket_apply_metadata_counts(&mut protected_stats, &metadata);
        storage_bucket_apply_metadata_counts(&mut deletable_stats, &metadata);
        let applied = if config.apply {
            Some(coord.delete_segment_rows_for_traces(&snap, &deletable_trace_ids))
        } else {
            None
        };
        let compacted = if config.apply && config.compact_after_apply {
            drop(snap);
            Some(coord.compact_deleted_segments(
                config.compact_max_segments,
                config.compact_min_deleted_rows,
                config.compact_min_deleted_percent,
                config.reclaim_after_compact,
            ))
        } else {
            None
        };
        let audit = if config.apply {
            let applied_ref = applied.as_ref();
            let compacted_ref = compacted.as_ref();
            let sample_limit = 100;
            let deletable_sample = sample_u64_set(&deletable_trace_ids, sample_limit);
            let deleted_sample = applied_ref
                .map(|result| sample_u64_slice(&result.deleted_trace_ids, sample_limit))
                .unwrap_or_default();
            let skipped_sample = applied_ref
                .map(|result| sample_u64_slice(&result.skipped_live_trace_ids, sample_limit))
                .unwrap_or_default();
            let sample_truncated = deletable_trace_ids.len() > deletable_sample.len()
                || applied_ref
                    .map(|result| result.deleted_trace_ids.len() > deleted_sample.len())
                    .unwrap_or(false)
                || applied_ref
                    .map(|result| result.skipped_live_trace_ids.len() > skipped_sample.len())
                    .unwrap_or(false);
            Some(
                coord.add_retention_audit_with_id_base(
                    NewRetentionAuditRecord {
                        source: config.audit_source.clone(),
                        reason: config.audit_reason.clone(),
                        delete_before_ts: config.cutoff,
                        query_json: config.query_json.clone(),
                        protect_golden_paths: config.protect_golden_paths,
                        protect_annotations: config.protect_annotations,
                        protect_dataset_associations: config.protect_dataset_associations,
                        protect_snapshots: config.protect_snapshots,
                        protect_eval_links: config.protect_eval_links,
                        protect_path_memory: config.protect_path_memory,
                        compact_requested: config.compact_after_apply,
                        compact_reclaim: config.reclaim_after_compact,
                        candidate_trace_count: candidate_stats.trace_ids.len() as u64,
                        protected_trace_count: protected_stats.trace_ids.len() as u64,
                        deletable_trace_count: deletable_stats.trace_ids.len() as u64,
                        requested_trace_count: applied_ref
                            .map(|result| result.requested_trace_count as u64)
                            .unwrap_or(0),
                        deleted_trace_count: applied_ref
                            .map(|result| result.deleted_trace_count as u64)
                            .unwrap_or(0),
                        deleted_segment_row_count: applied_ref
                            .map(|result| result.deleted_segment_row_count as u64)
                            .unwrap_or(0),
                        skipped_live_trace_count: applied_ref
                            .map(|result| result.skipped_live_trace_count as u64)
                            .unwrap_or(0),
                        compacted_segment_count: compacted_ref
                            .map(|result| result.compacted_segment_count as u64)
                            .unwrap_or(0),
                        reclaimed_segment_count: compacted_ref
                            .map(|result| result.reclaimed_segment_count as u64)
                            .unwrap_or(0),
                        dropped_deleted_row_count: compacted_ref
                            .map(|result| result.dropped_deleted_row_count as u64)
                            .unwrap_or(0),
                        rewritten_live_row_count: compacted_ref
                            .map(|result| result.rewritten_live_row_count as u64)
                            .unwrap_or(0),
                        deletable_trace_ids: deletable_sample,
                        deleted_trace_ids: deleted_sample,
                        skipped_live_trace_ids: skipped_sample,
                        trace_id_sample_truncated: sample_truncated,
                    },
                    tenant,
                    audit_id_base,
                ),
            )
        } else {
            None
        };

        Ok(RetentionPlanOutcome {
            candidate_stats,
            protected_stats,
            deletable_stats,
            protected,
            deletable_trace_ids,
            applied,
            compacted,
            audit,
        })
    }

    /// GET /v1/retention-audits：查询 retention/apply 审计记录。
    pub(super) fn retention_audits_query_json(
        &self,
        query: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let mut filter = RetentionAuditFilter {
            tenant_id: tenant,
            ..Default::default()
        };
        let mut cursor = 0usize;
        let mut limit = 50usize;
        for (k, v) in query_pairs(query) {
            match k.as_str() {
                "audit_id" | "auditId" | "id" => filter.audit_id = parse_id_or_hash(&v),
                "source" | "requestedBy" | "requested_by" | "actor" => filter.source = Some(v),
                "created_after_ns" | "createdAfterNs" | "minCreatedAtNs" => {
                    filter.min_created_at_ns = v.parse::<u64>().ok()
                }
                "created_before_ns" | "createdBeforeNs" | "maxCreatedAtNs" => {
                    filter.max_created_at_ns = v.parse::<u64>().ok()
                }
                "cursor" | "offset" => cursor = v.parse::<usize>().unwrap_or(0),
                "limit" => limit = v.parse::<usize>().unwrap_or(50).clamp(1, 500),
                _ => {}
            }
        }
        (200, self.retention_audits_page_json(filter, cursor, limit))
    }

    /// POST /v1/retention-audits：JSON 查询审计记录，兼容 `{filter:{...},limit,cursor}`。
    pub(super) fn retention_audits_body_json(
        &self,
        body: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let v = match parse_json_body_or_empty(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let f = crate::wire::field(&v, "filter").unwrap_or(&v);
        let mut filter = RetentionAuditFilter {
            tenant_id: tenant,
            ..Default::default()
        };
        filter.audit_id =
            json_field_alias(f, &["audit_id", "auditId", "id"]).and_then(json_internal_id);
        filter.source = json_field_alias(
            f,
            &[
                "source",
                "requestedBy",
                "requested_by",
                "actor",
                "createdBy",
            ],
        )
        .and_then(crate::wire::Json::as_str)
        .map(ToString::to_string);
        filter.min_created_at_ns =
            json_field_alias(f, &["created_after_ns", "createdAfterNs", "minCreatedAtNs"])
                .and_then(crate::wire::Json::as_u64);
        filter.max_created_at_ns = json_field_alias(
            f,
            &["created_before_ns", "createdBeforeNs", "maxCreatedAtNs"],
        )
        .and_then(crate::wire::Json::as_u64);
        let cursor = json_field_alias(&v, &["cursor", "offset"])
            .and_then(crate::wire::Json::as_u64)
            .unwrap_or(0) as usize;
        let limit = json_field_alias(&v, &["limit"])
            .and_then(crate::wire::Json::as_u64)
            .unwrap_or(50)
            .clamp(1, 500) as usize;
        (200, self.retention_audits_page_json(filter, cursor, limit))
    }

    pub(super) fn retention_audits_page_json(
        &self,
        filter: RetentionAuditFilter,
        cursor: usize,
        limit: usize,
    ) -> String {
        let cluster = self.is_in_process_cluster();
        let mut items = if cluster {
            self.shards()
                .iter()
                .flat_map(|shard| shard.coord.retention_audits(&filter))
                .collect::<Vec<_>>()
        } else {
            self.coord().retention_audits(&filter)
        };
        items.sort_by(|a, b| {
            b.created_at_ns
                .cmp(&a.created_at_ns)
                .then_with(|| b.audit_id.cmp(&a.audit_id))
        });
        let total = items.len();
        let end = cursor.saturating_add(limit).min(total);
        let page = if cursor < total {
            &items[cursor..end]
        } else {
            &[][..]
        };
        let body = page
            .iter()
            .map(json_retention_audit)
            .collect::<Vec<_>>()
            .join(",");
        let next = if end < total {
            end.to_string()
        } else {
            "null".to_string()
        };
        if cluster {
            format!(
                r#"{{"items":[{}],"nextCursor":{},"total":{},"queryMode":"fanout_merge","shardCount":{}}}"#,
                body,
                next,
                total,
                self.shards().len()
            )
        } else {
            format!(
                r#"{{"items":[{}],"nextCursor":{},"total":{}}}"#,
                body, next, total
            )
        }
    }

    /// POST /v1/retention-policies：保存一条可重复执行的 retention policy。
    pub(super) fn create_retention_policy_json(
        &self,
        body: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let v = match crate::wire::parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let name = json_field_alias(&v, &["name", "policyName", "policy_name"])
            .and_then(crate::wire::Json::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if name.is_empty() {
            return (400, r#"{"error":"missing name"}"#.to_string());
        }
        let interval_ns =
            json_field_alias(&v, &["intervalNs", "interval_ns", "everyNs", "every_ns"])
                .and_then(crate::wire::Json::as_u64)
                .unwrap_or(0);
        if interval_ns == 0 {
            return (400, r#"{"error":"missing intervalNs"}"#.to_string());
        }
        let Some(query) = json_field_alias(
            &v,
            &["query", "retention", "retentionQuery", "retention_query"],
        ) else {
            return (400, r#"{"error":"missing query"}"#.to_string());
        };
        if !matches!(query, crate::wire::Json::Obj(_)) {
            return (400, r#"{"error":"query must be an object"}"#.to_string());
        }
        if !retention_policy_query_has_cutoff(query) {
            return (
                400,
                r#"{"error":"query requires deleteBeforeTs or olderThanNs"}"#.to_string(),
            );
        }
        let now = unix_now_ns_u64_for_http();
        let policy = self.coord().add_retention_policy(
            NewRetentionPolicy {
                name,
                enabled: json_bool_alias(&v, &["enabled"]).unwrap_or(true),
                next_run_at_ns: json_field_alias(&v, &["nextRunAtNs", "next_run_at_ns"])
                    .and_then(crate::wire::Json::as_u64)
                    .or(Some(now)),
                interval_ns,
                source: json_field_alias(
                    &v,
                    &[
                        "source",
                        "requestedBy",
                        "requested_by",
                        "actor",
                        "createdBy",
                    ],
                )
                .and_then(crate::wire::Json::as_str)
                .map(ToString::to_string),
                reason: json_field_alias(&v, &["reason", "comment", "note"])
                    .and_then(crate::wire::Json::as_str)
                    .map(ToString::to_string),
                query_json: query.to_compact_json(),
            },
            tenant,
        );
        (200, json_retention_policy(&policy))
    }

    /// GET /v1/retention-policies：查询已保存的 retention policies。
    pub(super) fn retention_policies_query_json(
        &self,
        query: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let mut filter = RetentionPolicyFilter {
            tenant_id: tenant,
            ..Default::default()
        };
        let mut cursor = 0usize;
        let mut limit = 50usize;
        for (k, v) in query_pairs(query) {
            match k.as_str() {
                "policy_id" | "policyId" | "id" => filter.policy_id = parse_id_or_hash(&v),
                "name" | "policyName" | "policy_name" => filter.name = Some(v),
                "enabled" => filter.enabled = parse_query_bool(&v),
                "cursor" | "offset" => cursor = v.parse::<usize>().unwrap_or(0),
                "limit" => limit = v.parse::<usize>().unwrap_or(50).clamp(1, 500),
                _ => {}
            }
        }
        (
            200,
            self.retention_policies_page_json(filter, cursor, limit),
        )
    }

    pub(super) fn retention_policies_page_json(
        &self,
        filter: RetentionPolicyFilter,
        cursor: usize,
        limit: usize,
    ) -> String {
        let mut items = self.coord().retention_policies(&filter);
        items.sort_by(|a, b| a.policy_id.cmp(&b.policy_id));
        let total = items.len();
        let end = (cursor + limit).min(total);
        let page = if cursor < total {
            &items[cursor..end]
        } else {
            &[][..]
        };
        let body = page
            .iter()
            .map(json_retention_policy)
            .collect::<Vec<_>>()
            .join(",");
        let next = if end < total {
            end.to_string()
        } else {
            "null".to_string()
        };
        format!(
            r#"{{"items":[{}],"nextCursor":{},"total":{}}}"#,
            body, next, total
        )
    }

    /// POST /v1/retention-policies/run-due：执行当前到期的 policies。
    pub(super) fn run_due_retention_policies_json(
        &self,
        body: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let v = match parse_json_body_or_empty(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let now = json_field_alias(&v, &["nowNs", "now_ns"])
            .and_then(crate::wire::Json::as_u64)
            .unwrap_or_else(unix_now_ns_u64_for_http);
        let limit = json_field_alias(&v, &["limit", "maxPolicies", "max_policies"])
            .and_then(crate::wire::Json::as_u64)
            .unwrap_or(10)
            .clamp(1, 100) as usize;
        let include_disabled =
            json_bool_alias(&v, &["includeDisabled", "include_disabled"]).unwrap_or(false);
        let mut filter = RetentionPolicyFilter {
            tenant_id: tenant,
            ..Default::default()
        };
        filter.policy_id =
            json_field_alias(&v, &["policyId", "policy_id", "id"]).and_then(json_internal_id);
        filter.name = json_field_alias(&v, &["name", "policyName", "policy_name"])
            .and_then(crate::wire::Json::as_str)
            .map(ToString::to_string);
        let policies = self.coord().retention_policies(&filter);
        let mut due = Vec::new();
        let mut skipped = 0usize;
        for policy in policies {
            if !include_disabled && !policy.enabled {
                skipped += 1;
                continue;
            }
            if policy.next_run_at_ns.map(|n| n <= now).unwrap_or(false) {
                due.push(policy);
            } else {
                skipped += 1;
            }
        }
        due.sort_by(|a, b| {
            a.next_run_at_ns
                .cmp(&b.next_run_at_ns)
                .then_with(|| a.policy_id.cmp(&b.policy_id))
        });
        skipped += due.len().saturating_sub(limit);

        let mut ran = 0usize;
        let mut failed = 0usize;
        let mut items = Vec::new();
        for policy in due.into_iter().take(limit) {
            match retention_policy_effective_query(&policy, now) {
                Ok(query) => {
                    let (status, result) = self.retention_plan_json(&query, tenant, true);
                    if status == 200 {
                        ran += 1;
                        let policy = self
                            .coord()
                            .mark_retention_policy_ran(policy.policy_id, tenant, now)
                            .unwrap_or(policy);
                        items.push(format!(
                            r#"{{"policy":{},"ok":true,"statusCode":{},"result":{}}}"#,
                            json_retention_policy(&policy),
                            status,
                            result
                        ));
                    } else {
                        failed += 1;
                        items.push(format!(
                            r#"{{"policy":{},"ok":false,"statusCode":{},"error":{}}}"#,
                            json_retention_policy(&policy),
                            status,
                            result
                        ));
                    }
                }
                Err(error) => {
                    failed += 1;
                    items.push(format!(
                        r#"{{"policy":{},"ok":false,"statusCode":400,"error":{{"error":"{}"}}}}"#,
                        json_retention_policy(&policy),
                        json_escape(&error)
                    ));
                }
            }
        }
        (
            200,
            format!(
                r#"{{"nowNs":"{}","ran":{},"failed":{},"skipped":{},"items":[{}]}}"#,
                now,
                ran,
                failed,
                skipped,
                items.join(",")
            ),
        )
    }

    pub(super) fn filtered_spans_for_storage(
        &self,
        v: &crate::wire::Json,
        tenant: Option<u64>,
    ) -> Result<(yt_manifest::Snapshot, Vec<FoldedSpan>), String> {
        self.filtered_spans_for_storage_for_coord(self.coord(), v, tenant)
    }

    pub(super) fn filtered_spans_for_storage_projected(
        &self,
        v: &crate::wire::Json,
        tenant: Option<u64>,
        proj: crate::Projection,
    ) -> Result<(yt_manifest::Snapshot, Vec<FoldedSpan>), String> {
        self.filtered_spans_for_storage_for_coord_projected(self.coord(), v, tenant, proj)
    }

    pub(super) fn filtered_spans_for_storage_for_coord(
        &self,
        coord: &WriteCoordinator,
        v: &crate::wire::Json,
        tenant: Option<u64>,
    ) -> Result<(yt_manifest::Snapshot, Vec<FoldedSpan>), String> {
        self.filtered_spans_for_storage_for_coord_projected(
            coord,
            v,
            tenant,
            crate::Projection::ALL,
        )
    }

    pub(super) fn filtered_spans_for_storage_for_coord_projected(
        &self,
        coord: &WriteCoordinator,
        v: &crate::wire::Json,
        tenant: Option<u64>,
        proj: crate::Projection,
    ) -> Result<(yt_manifest::Snapshot, Vec<FoldedSpan>), String> {
        let snap = coord.pin_snapshot();
        let spans = self.filtered_spans_for_storage_for_coord_snapshot_projected(
            coord, &snap, v, tenant, proj,
        )?;
        Ok((snap, spans))
    }

    pub(super) fn filtered_spans_for_storage_for_coord_snapshot(
        &self,
        coord: &WriteCoordinator,
        snap: &yt_manifest::Snapshot,
        v: &crate::wire::Json,
        tenant: Option<u64>,
    ) -> Result<Vec<FoldedSpan>, String> {
        self.filtered_spans_for_storage_for_coord_snapshot_projected(
            coord,
            snap,
            v,
            tenant,
            crate::Projection::ALL,
        )
    }

    pub(super) fn filtered_spans_for_storage_for_coord_snapshot_projected(
        &self,
        coord: &WriteCoordinator,
        snap: &yt_manifest::Snapshot,
        v: &crate::wire::Json,
        tenant: Option<u64>,
        proj: crate::Projection,
    ) -> Result<Vec<FoldedSpan>, String> {
        let request = trace_search_request_from_json(v, tenant);
        let metadata_matches = self.trace_search_metadata_matches_for_coord(
            coord,
            &request.annotation,
            &request.dataset,
            tenant,
        );
        let mut spans = if request.spec.attrs.is_empty() {
            coord
                .read_spans_query_projected(snap, &request.query, proj)
                .0
        } else {
            coord
                .read_spans_query_for_attrs_projected_with_stats(
                    snap,
                    &request.query,
                    &request.spec.attrs,
                    proj,
                )
                .0
        };
        spans.retain(|s| trace_search_match(s, &request.spec, &metadata_matches));
        Ok(spans)
    }

    pub(super) fn storage_metadata_for_tenant(&self, tenant: Option<u64>) -> StorageMetadata {
        self.storage_metadata_for_coord(self.coord(), tenant)
    }

    pub(super) fn storage_metadata_for_coord(
        &self,
        coord: &WriteCoordinator,
        tenant: Option<u64>,
    ) -> StorageMetadata {
        StorageMetadata {
            annotations: coord.annotations(&TraceAnnotationFilter {
                tenant_id: tenant,
                ..TraceAnnotationFilter::default()
            }),
            dataset_associations: coord.dataset_associations(&DatasetAssociationFilter {
                tenant_id: tenant,
                ..DatasetAssociationFilter::default()
            }),
            golden_paths: coord.golden_paths(&GoldenPathFilter {
                tenant_id: tenant,
                ..GoldenPathFilter::default()
            }),
        }
    }
}
