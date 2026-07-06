use super::*;

impl EngineJsonApi {
    pub(super) fn create_golden_path_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        let v = match crate::wire::parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let Some((trace_id, external_trace_id)) = json_field_alias(
            &v,
            &["trace_id", "traceId", "sourceTraceId", "source_trace_id"],
        )
        .and_then(json_id_with_external) else {
            return (400, r#"{"error":"missing sourceTraceId"}"#.to_string());
        };
        let owner_idx = self
            .is_in_process_cluster()
            .then(|| self.metadata_owner_index_for_trace(tenant, trace_id));
        let coord = owner_idx
            .map(|idx| self.shards()[idx].coord.as_ref())
            .unwrap_or(self.coord().as_ref());
        let snap = coord.pin_snapshot();
        let spans = self.trace_folded_spans_for_coord(coord, &snap, trace_id, tenant);
        if spans.is_empty() {
            return (404, r#"{"error":"source trace not found"}"#.to_string());
        }
        let source_steps = trajectory_steps(&spans);
        let task_fingerprint = json_field_alias(
            &v,
            &["task_fingerprint", "taskFingerprint", "task", "taskId"],
        )
        .and_then(crate::wire::Json::as_str)
        .map(ToString::to_string)
        .or_else(|| {
            spans
                .iter()
                .find_map(|s| crate::folded_span_attr_value(s, "task_fingerprint"))
                .map(json_compact_label)
        });
        let Some(task_fingerprint) = task_fingerprint.filter(|s| !s.trim().is_empty()) else {
            return (400, r#"{"error":"missing taskFingerprint"}"#.to_string());
        };
        let trajectory_signature = json_field_alias(
            &v,
            &[
                "trajectory_signature",
                "trajectorySignature",
                "signature",
                "pathSignature",
            ],
        )
        .and_then(crate::wire::Json::as_str)
        .map(ToString::to_string)
        .unwrap_or_else(|| trajectory_signature_string(&source_steps));
        let status = json_field_alias(&v, &["status"])
            .and_then(crate::wire::Json::as_str)
            .and_then(GoldenPathStatus::parse);
        let mut attrs = std::collections::BTreeMap::new();
        collect_attr_map(&v, &mut attrs);
        remove_top_level_golden_path_governance_attrs(&v, &mut attrs);
        collect_golden_path_scope_attrs(&spans, &mut attrs);
        let evidence = golden_path_evidence_summary_from_json(&v, &spans);
        let input = NewGoldenPathCandidate {
            task_fingerprint,
            trajectory_signature,
            source_trace_id: trace_id,
            external_source_trace_id: json_field_alias(
                &v,
                &["external_source_trace_id", "externalSourceTraceId"],
            )
            .and_then(crate::wire::Json::as_str)
            .map(ToString::to_string)
            .or(external_trace_id),
            snapshot_id: json_field_alias(&v, &["snapshot_id", "snapshotId"])
                .and_then(crate::wire::Json::as_str)
                .map(ToString::to_string),
            snapshot_hash: json_field_alias(&v, &["snapshot_hash", "snapshotHash"])
                .and_then(crate::wire::Json::as_str)
                .map(ToString::to_string),
            status,
            score: json_field_alias(&v, &["score", "qualityScore"])
                .and_then(crate::wire::Json::as_u64)
                .map(score_u64),
            label: json_field_alias(&v, &["label", "name"])
                .and_then(crate::wire::Json::as_str)
                .map(ToString::to_string),
            reason: json_field_alias(&v, &["reason", "comment", "note"])
                .and_then(crate::wire::Json::as_str)
                .map(ToString::to_string),
            source: json_field_alias(&v, &["source", "created_by", "createdBy"])
                .and_then(crate::wire::Json::as_str)
                .map(ToString::to_string),
            attrs,
            source_trajectory_steps: source_steps,
            evidence,
            challenger_of: json_field_alias(
                &v,
                &[
                    "challenger_of",
                    "challengerOf",
                    "baselineGoldenPathId",
                    "baseline_golden_path_id",
                ],
            )
            .and_then(json_internal_id),
            eval_profile: json_field_alias(&v, &["eval_profile", "evalProfile"])
                .and_then(crate::wire::Json::as_str)
                .map(ToString::to_string),
            min_sample_count: json_field_alias(
                &v,
                &[
                    "min_sample_count",
                    "minSampleCount",
                    "min_samples",
                    "minSamples",
                ],
            )
            .and_then(crate::wire::Json::as_u64),
            margin_score: json_field_alias(&v, &["margin_score", "marginScore", "margin"])
                .and_then(crate::wire::Json::as_u64)
                .map(score_u64),
            comparison_window_ns: json_field_alias(
                &v,
                &[
                    "comparison_window_ns",
                    "comparisonWindowNs",
                    "window_ns",
                    "windowNs",
                ],
            )
            .and_then(crate::wire::Json::as_u64),
            promoted_from: json_field_alias(&v, &["promoted_from", "promotedFrom"])
                .and_then(json_internal_id),
            deprecation_reason: json_field_alias(&v, &["deprecation_reason", "deprecationReason"])
                .and_then(crate::wire::Json::as_str)
                .map(ToString::to_string),
            stale_reasons: json_string_list_alias(&v, &["stale_reasons", "staleReasons"]),
        };
        let candidate = if let Some(idx) = owner_idx {
            self.shards()[idx].coord.add_golden_path_with_id_base(
                input,
                tenant,
                cluster_metadata_id_base(idx),
            )
        } else {
            self.coord().add_golden_path(input, tenant)
        };
        (200, json_golden_path(&candidate))
    }

    /// GET /v1/golden-paths?taskFingerprint=...&status=confirmed
    pub(super) fn golden_paths_json(&self, query: &str, tenant: Option<u64>) -> (u16, String) {
        let mut filter = GoldenPathFilter {
            tenant_id: tenant,
            ..Default::default()
        };
        for (k, v) in query_pairs(query) {
            match k.as_str() {
                "golden_path_id" | "goldenPathId" | "id" => {
                    filter.golden_path_id = parse_id_or_hash(&v)
                }
                "task_fingerprint" | "taskFingerprint" | "task" => {
                    filter.task_fingerprint = Some(v)
                }
                "trajectory_signature" | "trajectorySignature" | "signature" => {
                    filter.trajectory_signature = Some(v)
                }
                "trace_id" | "traceId" | "sourceTraceId" | "source_trace_id" => {
                    filter.source_trace_id = parse_id_or_hash(&v)
                }
                "challenger_of"
                | "challengerOf"
                | "baselineGoldenPathId"
                | "baseline_golden_path_id" => filter.challenger_of = parse_id_or_hash(&v),
                "eval_profile" | "evalProfile" => filter.eval_profile = Some(v),
                "status" => filter.status = GoldenPathStatus::parse(&v),
                "attrs" => collect_attr_query_json(&v, &mut filter.attrs),
                "model" | "provider" => {
                    filter.attrs.insert(k, json_string_value(&v));
                }
                _ => collect_attr_query_pair(&k, &v, &mut filter.attrs),
            }
        }
        let mut items = self.golden_paths_for_filter(&filter);
        items.sort_by(|a, b| {
            b.updated_at_ns
                .cmp(&a.updated_at_ns)
                .then_with(|| a.golden_path_id.cmp(&b.golden_path_id))
        });
        let body = items
            .iter()
            .map(json_golden_path)
            .collect::<Vec<_>>()
            .join(",");
        if self.is_in_process_cluster() {
            (
                200,
                format!(
                    r#"{{"items":[{}],"count":{},"queryMode":"fanout_merge","shardCount":{}}}"#,
                    body,
                    items.len(),
                    self.shards().len()
                ),
            )
        } else {
            (
                200,
                format!(r#"{{"items":[{}],"count":{}}}"#, body, items.len()),
            )
        }
    }

    pub(super) fn golden_paths_for_filter(
        &self,
        filter: &GoldenPathFilter,
    ) -> Vec<GoldenPathCandidate> {
        if self.is_in_process_cluster() {
            let mut items = Vec::new();
            for shard in self.shards().iter() {
                items.extend(shard.coord.golden_paths(filter));
            }
            items
        } else {
            self.coord().golden_paths(filter)
        }
    }

    pub(super) fn golden_path_by_id(
        &self,
        golden_path_id: u64,
        tenant: Option<u64>,
    ) -> Option<GoldenPathCandidate> {
        let filter = GoldenPathFilter {
            tenant_id: tenant,
            golden_path_id: Some(golden_path_id),
            ..Default::default()
        };
        self.golden_paths_for_filter(&filter).into_iter().next()
    }

    /// POST /v1/golden-paths/:id/status：确认、拒绝或废弃候选路径。
    pub(super) fn update_golden_path_status_json(
        &self,
        id: &str,
        body: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let Some(golden_path_id) = parse_id_or_hash(id) else {
            return (400, r#"{"error":"bad golden path id"}"#.to_string());
        };
        let v = match crate::wire::parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let Some(status) = json_field_alias(&v, &["status"])
            .and_then(crate::wire::Json::as_str)
            .and_then(GoldenPathStatus::parse)
        else {
            return (400, r#"{"error":"missing status"}"#.to_string());
        };
        let score = json_field_alias(&v, &["score", "qualityScore"])
            .and_then(crate::wire::Json::as_u64)
            .map(score_u64);
        let reason = json_field_alias(&v, &["reason", "comment", "note"])
            .and_then(crate::wire::Json::as_str)
            .map(ToString::to_string);
        let source = json_field_alias(&v, &["source", "updated_by", "updatedBy"])
            .and_then(crate::wire::Json::as_str)
            .map(ToString::to_string);
        let updated = if self.is_in_process_cluster() {
            let mut out = None;
            for shard in self.shards().iter() {
                if let Some(path) = shard.coord.update_golden_path_status(
                    golden_path_id,
                    tenant,
                    status,
                    score,
                    reason.clone(),
                    source.clone(),
                ) {
                    out = Some(path);
                    break;
                }
            }
            out
        } else {
            self.coord().update_golden_path_status(
                golden_path_id,
                tenant,
                status,
                score,
                reason,
                source,
            )
        };
        match updated {
            Some(path) => (200, json_golden_path(&path)),
            None => (404, r#"{"error":"golden path not found"}"#.to_string()),
        }
    }

    /// POST /v1/path-adherence：比较一条 trace 是否遵循某个 golden path。
    ///
    /// 这是底座读模型：只返回 trajectory signature、共同步骤、缺失步骤和额外步骤，不替业务判断
    /// “这是不是当前最佳路径”。
    pub(super) fn path_adherence_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        let v = match crate::wire::parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let Some(golden_path_id) = json_field_alias(&v, &["golden_path_id", "goldenPathId", "id"])
            .and_then(json_internal_id)
        else {
            return (400, r#"{"error":"missing goldenPathId"}"#.to_string());
        };
        let Some((trace_id, _)) = json_field_alias(
            &v,
            &["trace_id", "traceId", "candidateTraceId", "candidate"],
        )
        .and_then(json_id_with_external) else {
            return (400, r#"{"error":"missing traceId"}"#.to_string());
        };
        self.path_adherence_result_json(golden_path_id, trace_id, tenant)
    }

    /// POST /v1/golden-paths/:id/adherence：路径参数传 goldenPathId，body 只需 traceId。
    pub(super) fn path_adherence_for_golden_path_json(
        &self,
        id: &str,
        body: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let Some(golden_path_id) = parse_id_or_hash(id) else {
            return (400, r#"{"error":"bad golden path id"}"#.to_string());
        };
        let v = match crate::wire::parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let Some((trace_id, _)) = json_field_alias(
            &v,
            &["trace_id", "traceId", "candidateTraceId", "candidate"],
        )
        .and_then(json_id_with_external) else {
            return (400, r#"{"error":"missing traceId"}"#.to_string());
        };
        self.path_adherence_result_json(golden_path_id, trace_id, tenant)
    }

    pub(super) fn path_adherence_result_json(
        &self,
        golden_path_id: u64,
        trace_id: u64,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let Some(golden_path) = self.golden_path_by_id(golden_path_id, tenant) else {
            return (404, r#"{"error":"golden path not found"}"#.to_string());
        };

        let trace_spans = self.trace_folded_spans_any_shard(trace_id, tenant);
        if trace_spans.is_empty() {
            return (404, r#"{"error":"trace not found"}"#.to_string());
        }
        let source_spans = self.trace_folded_spans_any_shard(golden_path.source_trace_id, tenant);
        (
            200,
            json_path_adherence(&golden_path, trace_id, &trace_spans, &source_spans),
        )
    }

    /// POST /v1/golden-path-evidence：导出 Golden Path 的底层证据包。
    ///
    /// 默认只返回 source trace 的摘要/trajectory/annotation/dataset 证据。传 `candidateTraceId`
    /// 时，额外返回 pathAdherence 和 traceDiff，供上层做评审、回归集或 Agent Memory 导出。
    pub(super) fn golden_path_evidence_json(
        &self,
        body: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let v = match crate::wire::parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let Some(golden_path_id) = json_field_alias(&v, &["golden_path_id", "goldenPathId", "id"])
            .and_then(json_internal_id)
        else {
            return (400, r#"{"error":"missing goldenPathId"}"#.to_string());
        };
        let candidate_trace_id = json_field_alias(
            &v,
            &[
                "candidate_trace_id",
                "candidateTraceId",
                "trace_id",
                "traceId",
                "candidate",
            ],
        )
        .and_then(json_id_with_external)
        .map(|(id, _)| id);
        self.golden_path_evidence_result_json(golden_path_id, candidate_trace_id, tenant)
    }

    /// POST /v1/golden-paths/:id/evidence：路径参数传 goldenPathId，body 可选 candidateTraceId。
    pub(super) fn golden_path_evidence_for_id_json(
        &self,
        id: &str,
        body: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let Some(golden_path_id) = parse_id_or_hash(id) else {
            return (400, r#"{"error":"bad golden path id"}"#.to_string());
        };
        let v = match parse_json_body_or_empty(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let candidate_trace_id = json_field_alias(
            &v,
            &[
                "candidate_trace_id",
                "candidateTraceId",
                "trace_id",
                "traceId",
                "candidate",
            ],
        )
        .and_then(json_id_with_external)
        .map(|(id, _)| id);
        self.golden_path_evidence_result_json(golden_path_id, candidate_trace_id, tenant)
    }

    pub(super) fn golden_path_evidence_result_json(
        &self,
        golden_path_id: u64,
        candidate_trace_id: Option<u64>,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let Some(golden_path) = self.golden_path_by_id(golden_path_id, tenant) else {
            return (404, r#"{"error":"golden path not found"}"#.to_string());
        };

        let source_spans = self.trace_folded_spans_any_shard(golden_path.source_trace_id, tenant);
        let source = self.trace_evidence_json(golden_path.source_trace_id, &source_spans, tenant);
        let candidate = match candidate_trace_id {
            Some(trace_id) => {
                let trace_spans = self.trace_folded_spans_any_shard(trace_id, tenant);
                if trace_spans.is_empty() {
                    return (404, r#"{"error":"candidate trace not found"}"#.to_string());
                }
                let evidence = self.trace_evidence_json(trace_id, &trace_spans, tenant);
                let adherence =
                    json_path_adherence(&golden_path, trace_id, &trace_spans, &source_spans);
                let diff = if source_spans.is_empty() {
                    "null".to_string()
                } else {
                    json_trace_diff(
                        golden_path.source_trace_id,
                        trace_id,
                        &source_spans,
                        &trace_spans,
                    )
                };
                format!(
                    r#"{{"evidence":{},"pathAdherence":{},"traceDiff":{}}}"#,
                    evidence, adherence, diff
                )
            }
            None => "null".to_string(),
        };
        (
            200,
            format!(
                r#"{{"goldenPath":{},"source":{},"candidate":{}}}"#,
                json_golden_path(&golden_path),
                source,
                candidate,
            ),
        )
    }

    pub(super) fn trace_evidence_json(
        &self,
        trace_id: u64,
        spans: &[FoldedSpan],
        tenant: Option<u64>,
    ) -> String {
        let summary = trace_summary_buckets_from_spans(spans);
        let trajectory = if spans.is_empty() {
            "null".to_string()
        } else {
            let steps = trajectory_steps(spans);
            trajectory_summary_json_with_signature(&steps, &trajectory_signature_string(&steps))
        };
        let annotation_filter = TraceAnnotationFilter {
            tenant_id: tenant,
            trace_id: Some(trace_id),
            ..Default::default()
        };
        let dataset_filter = DatasetAssociationFilter {
            tenant_id: tenant,
            trace_id: Some(trace_id),
            ..Default::default()
        };
        let annotations = if self.is_in_process_cluster() {
            let mut out = Vec::new();
            for shard in self.shards().iter() {
                out.extend(shard.coord.annotations(&annotation_filter));
            }
            out
        } else {
            self.coord().annotations(&annotation_filter)
        };
        let datasets = if self.is_in_process_cluster() {
            let mut out = Vec::new();
            for shard in self.shards().iter() {
                out.extend(shard.coord.dataset_associations(&dataset_filter));
            }
            out
        } else {
            self.coord().dataset_associations(&dataset_filter)
        };
        let annotations_json = annotations
            .iter()
            .map(json_annotation)
            .collect::<Vec<_>>()
            .join(",");
        let datasets_json = datasets
            .iter()
            .map(json_dataset_association)
            .collect::<Vec<_>>()
            .join(",");
        format!(
            r#"{{"available":{},"trace":{},"trajectory":{},"annotations":[{}],"annotationCount":{},"datasetAssociations":[{}],"datasetAssociationCount":{}}}"#,
            json_bool(!spans.is_empty()),
            trace_diff_side_json(trace_id, summary.first()),
            trajectory,
            annotations_json,
            annotations.len(),
            datasets_json,
            datasets.len(),
        )
    }
}
