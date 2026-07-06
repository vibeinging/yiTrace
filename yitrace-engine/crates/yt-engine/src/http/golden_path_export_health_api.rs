use super::*;

impl EngineJsonApi {
    pub(super) fn golden_path_export_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        let v = match parse_json_body_or_empty(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let filter_source = json_field_alias(&v, &["filter"]).unwrap_or(&v);
        let (mut filter, explicit_status) =
            match golden_path_filter_from_json(filter_source, tenant) {
                Ok(out) => out,
                Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
            };
        if !explicit_status {
            filter.status = Some(GoldenPathStatus::Confirmed);
        }
        let limit = json_field_alias(&v, &["limit", "k"])
            .and_then(crate::wire::Json::as_u64)
            .unwrap_or(100)
            .clamp(1, 500) as usize;

        let mut paths = self.golden_paths_for_filter(&filter);
        paths.sort_by(|a, b| {
            b.updated_at_ns
                .cmp(&a.updated_at_ns)
                .then_with(|| a.golden_path_id.cmp(&b.golden_path_id))
        });
        paths.truncate(limit);

        let records = paths
            .iter()
            .map(|path| {
                let source_spans = self.trace_folded_spans_any_shard(path.source_trace_id, tenant);
                self.golden_path_export_record_json(path, &source_spans, tenant)
            })
            .collect::<Vec<_>>();
        let jsonl = records.join("\n");
        (
            200,
            format!(
                r#"{{"schemaVersion":"yitrace.golden_path_export.v1","format":"jsonl","count":{},"items":[{}],"jsonl":{}}}"#,
                records.len(),
                records.join(","),
                json_string_value(&jsonl),
            ),
        )
    }

    pub(super) fn golden_path_export_record_json(
        &self,
        path: &crate::GoldenPathCandidate,
        source_spans: &[FoldedSpan],
        tenant: Option<u64>,
    ) -> String {
        let evidence = self.trace_evidence_json(path.source_trace_id, source_spans, tenant);
        format!(
            r#"{{"schemaVersion":"yitrace.golden_path_export.v1","recordType":"golden_path","goldenPath":{},"source":{},"exportedAtNs":"{}"}}"#,
            json_golden_path(path),
            evidence,
            unix_now_ns(),
        )
    }

    /// POST /v1/golden-path-health：统计一批同 scope trace 对某条 Golden Path 的遵循情况。
    ///
    /// 这是底座证据模型：默认用 Golden Path 的 taskFingerprint + attrs 收窄窗口，并排除 source trace。
    /// 它只输出 followed/extended/partial/deviated 分布和覆盖率，不维护“当前最佳路径”。
    pub(super) fn golden_path_health_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        let v = match parse_json_body_or_empty(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let Some(golden_path_id) = json_field_alias(&v, &["golden_path_id", "goldenPathId", "id"])
            .and_then(json_internal_id)
        else {
            return (400, r#"{"error":"missing goldenPathId"}"#.to_string());
        };
        self.golden_path_health_result_json(golden_path_id, &v, tenant)
    }

    /// POST /v1/golden-paths/:id/health：路径参数传 goldenPathId，body 可传 filter/limit。
    pub(super) fn golden_path_health_for_id_json(
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
        self.golden_path_health_result_json(golden_path_id, &v, tenant)
    }

    pub(super) fn golden_path_health_result_json(
        &self,
        golden_path_id: u64,
        v: &crate::wire::Json,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let Some(golden_path) = self.golden_path_by_id(golden_path_id, tenant) else {
            return (404, r#"{"error":"golden path not found"}"#.to_string());
        };

        let limit = json_field_alias(v, &["limit", "k"])
            .and_then(crate::wire::Json::as_u64)
            .unwrap_or(100)
            .clamp(1, 500) as usize;
        let example_limit = json_field_alias(v, &["example_limit", "exampleLimit", "examples"])
            .and_then(crate::wire::Json::as_u64)
            .unwrap_or(5)
            .clamp(0, 50) as usize;
        let include_source =
            json_bool_alias(v, &["include_source", "includeSource"]).unwrap_or(false);

        let mut request = trace_search_request_from_json(v, tenant);
        if !golden_path.task_fingerprint.is_empty() {
            request
                .spec
                .attrs
                .entry("task_fingerprint".to_string())
                .or_insert_with(|| json_string_value(&golden_path.task_fingerprint));
        }
        for (key, value) in &golden_path.attrs {
            request
                .spec
                .attrs
                .entry(key.clone())
                .or_insert_with(|| value.clone());
        }

        let matching_spans = if self.is_in_process_cluster() {
            let mut spans = Vec::new();
            for shard in self.shards().iter() {
                spans.extend(self.trace_search_spans_for_coord(&shard.coord, &request, tenant));
            }
            spans
        } else {
            let metadata_matches =
                self.trace_search_metadata_matches(&request.annotation, &request.dataset, tenant);
            let snap = self.coord().pin_snapshot();
            let mut spans = if request.spec.attrs.is_empty() {
                self.coord().read_spans_query(&snap, &request.query).0
            } else {
                self.coord()
                    .read_spans_query_for_attrs(&snap, &request.query, &request.spec.attrs)
            };
            spans.retain(|s| trace_search_match(s, &request.spec, &metadata_matches));
            spans
        };
        let span_total = matching_spans.len();
        let mut trace_ids: Vec<u64> = matching_spans
            .iter()
            .map(|s| s.trace_id)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        if !include_source {
            trace_ids.retain(|trace_id| *trace_id != golden_path.source_trace_id);
        }
        trace_ids.sort_by(|a, b| b.cmp(a));
        let matching_trace_total = trace_ids.len();
        trace_ids.truncate(limit);

        let source_spans = self.trace_folded_spans_any_shard(golden_path.source_trace_id, tenant);
        let source_available = !source_spans.is_empty();
        let source_retained = !golden_path.source_trajectory_steps.is_empty();
        let source_steps = if source_available {
            trajectory_steps(&source_spans)
        } else if source_retained {
            golden_path.source_trajectory_steps.clone()
        } else {
            Vec::new()
        };
        let source_signature =
            (!source_steps.is_empty()).then(|| trajectory_signature_string(&source_steps));
        let stored_signature_matches_source = source_signature
            .as_ref()
            .map(|signature| signature == &golden_path.trajectory_signature);

        let mut analyzed_trace_total = 0usize;
        let mut followed = 0usize;
        let mut extended = 0usize;
        let mut partial = 0usize;
        let mut deviated = 0usize;
        let mut unknown = 0usize;
        let mut common_step_count = 0usize;
        let mut golden_step_count = 0usize;
        let mut trace_step_count = 0usize;
        let mut examples = Vec::new();

        for trace_id in trace_ids {
            let Some(trajectory) = self.materialized_trace_trajectory_any_shard(trace_id, tenant)
            else {
                continue;
            };
            let facts = path_adherence_facts_from_steps(
                &golden_path,
                trajectory.steps.clone(),
                &source_spans,
            );
            match facts.adherence() {
                "followed" => followed += 1,
                "extended" => extended += 1,
                "partial" => partial += 1,
                "deviated" => deviated += 1,
                _ => unknown += 1,
            }
            analyzed_trace_total += 1;
            common_step_count += facts.common_steps.len();
            golden_step_count += facts.source_steps.len();
            trace_step_count += facts.trace_steps.len();
            if examples.len() < example_limit {
                examples.push(path_adherence_health_example_json(&trajectory, &facts));
            }
        }

        let stale_reasons = golden_path_stale_reasons(
            &golden_path,
            stored_signature_matches_source,
            analyzed_trace_total,
            followed + extended,
        );
        let examples_json = examples.join(",");
        (
            200,
            format!(
                r#"{{"goldenPath":{},"sourceAvailable":{},"sourceRetained":{},"storedSignatureMatchesSource":{},"goldenTrajectory":{},"sourceTrajectory":{},"window":{{"limit":{},"includeSource":{},"spanTotal":{},"matchingTraceTotal":{},"analyzedTraceTotal":{}}},"counts":{{"total":{},"followed":{},"extended":{},"partial":{},"deviated":{},"unknown":{}}},"rates":{{"followed":{},"usable":{},"deviated":{},"unknown":{}}},"coverage":{{"commonStepCount":{},"goldenStepCount":{},"traceStepCount":{},"goldenCoverage":{},"traceCoverage":{}}},"governance":{{"evalProfile":{},"challengerOf":{},"minSampleCount":{},"marginScore":{},"comparisonWindowNs":{},"stale":{},"staleReasons":{}}},"examples":[{}]}}"#,
                json_golden_path(&golden_path),
                json_bool(source_available),
                json_bool(source_retained),
                json_opt_bool(stored_signature_matches_source),
                trajectory_summary_json_with_signature(
                    &source_steps,
                    &golden_path.trajectory_signature
                ),
                source_signature
                    .as_ref()
                    .map(|signature| trajectory_summary_json_with_signature(
                        &source_steps,
                        signature
                    ))
                    .unwrap_or_else(|| "null".to_string()),
                limit,
                json_bool(include_source),
                span_total,
                matching_trace_total,
                analyzed_trace_total,
                analyzed_trace_total,
                followed,
                extended,
                partial,
                deviated,
                unknown,
                ratio_json(followed, analyzed_trace_total),
                ratio_json(followed + extended, analyzed_trace_total),
                ratio_json(deviated, analyzed_trace_total),
                ratio_json(unknown, analyzed_trace_total),
                common_step_count,
                golden_step_count,
                trace_step_count,
                ratio_json(common_step_count, golden_step_count),
                ratio_json(common_step_count, trace_step_count),
                json_opt_str(golden_path.eval_profile.as_deref()),
                json_opt_u64_string(golden_path.challenger_of),
                json_opt_u64_string(golden_path.min_sample_count),
                golden_path
                    .margin_score
                    .map_or("null".to_string(), |score| score.to_string()),
                json_opt_u64_string(golden_path.comparison_window_ns),
                json_bool(!stale_reasons.is_empty()),
                json_string_array(&stale_reasons),
                examples_json,
            ),
        )
    }
}
