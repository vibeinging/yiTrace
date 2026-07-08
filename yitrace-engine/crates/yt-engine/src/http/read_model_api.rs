impl EngineJsonApi {
    /// POST /v1/trace-search：跨 trace 的结构化 span 搜索。
    fn trace_search_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        let mut read = match self.trace_search_spans(body, tenant) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        sort_trace_search_spans(&mut read.spans, &read.sort_by);
        let total = read.spans.len();
        let items: Vec<String> = read
            .spans
            .into_iter()
            .skip(read.cursor)
            .take(read.limit)
            .map(|s| trace_search_item_json(&s))
            .collect();
        (
            200,
            format!(
                r#"{{"items":[{}],"total":{},"cursor":{},"limit":{},"scannedSpans":{},"readPlan":{}}}"#,
                items.join(","),
                total,
                read.cursor,
                read.limit,
                read.read_plan.scanned_segments,
                json_read_plan(&read.read_plan),
            ),
        )
    }

    /// POST /v1/trace-aggregate：对 trace-search 的结果做 groupBy。
    fn trace_aggregate_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        let v = match crate::wire::parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let group_by = match group_by_fields(&v) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let parsed = self.parse_trace_search_value(&v, tenant);
        let read = if parsed.spec.text.is_none() {
            match self
                .coord
                .trace_aggregate_rollup_spans(&parsed.query, &parsed.index_filter)
            {
                Some((spans, mut read_plan)) => {
                    let spans = spans
                        .into_iter()
                        .filter(|span| trace_search_matches(span, &parsed.spec))
                        .collect::<Vec<_>>();
                    read_plan.matched_spans = spans.len();
                    TraceSearchRead {
                        spans,
                        read_plan,
                        cursor: parsed.cursor,
                        limit: parsed.limit,
                        sort_by: parsed.sort_by,
                    }
                }
                None => match self.trace_search_spans(body, tenant) {
                    Ok(v) => v,
                    Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
                },
            }
        } else {
            match self.trace_search_spans(body, tenant) {
                Ok(v) => v,
                Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
            }
        };
        let mut buckets: std::collections::BTreeMap<Vec<String>, TraceAggregateBucket> =
            std::collections::BTreeMap::new();
        for span in &read.spans {
            let values: Vec<String> = group_by
                .iter()
                .map(|field| span_group_value_json(span, field))
                .collect();
            let bucket = buckets
                .entry(values.clone())
                .or_insert_with(|| TraceAggregateBucket::new(values));
            bucket.add(span);
        }
        let mut items: Vec<TraceAggregateBucket> = buckets.into_values().collect();
        items.sort_by(|a, b| {
            b.span_count
                .cmp(&a.span_count)
                .then_with(|| a.values.cmp(&b.values))
        });
        let limit = json_field_alias(&v, &["limit"])
            .and_then(crate::wire::Json::as_u64)
            .unwrap_or(50) as usize;
        let item_json: Vec<String> = items
            .into_iter()
            .take(limit.max(1))
            .map(|bucket| bucket.to_json(&group_by))
            .collect();
        (
            200,
            format!(
                r#"{{"items":[{}],"total":{},"groupBy":[{}],"scannedSpans":{},"readPlan":{}}}"#,
                item_json.join(","),
                read.spans.len(),
                group_by
                    .iter()
                    .map(|g| json_string_value(g))
                    .collect::<Vec<_>>()
                    .join(","),
                read.read_plan.scanned_segments,
                json_read_plan(&read.read_plan),
            ),
        )
    }

    /// POST /v1/storage-stats：按同一过滤语义估算 trace/span/event 与 payload 大小。
    fn storage_stats_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        let read = match self.trace_search_spans(body, tenant) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let v = match crate::wire::parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let group_by = group_by_fields_optional(&v);
        let total = StorageStatsBucket::from_spans(&read.spans, &[]);
        let mut groups: std::collections::BTreeMap<Vec<String>, Vec<FoldedSpan>> =
            std::collections::BTreeMap::new();
        if !group_by.is_empty() {
            for span in &read.spans {
                let values: Vec<String> = group_by
                    .iter()
                    .map(|field| span_group_value_json(span, field))
                    .collect();
                groups.entry(values).or_default().push(span.clone());
            }
        }
        let group_json: Vec<String> = groups
            .into_iter()
            .map(|(values, spans)| {
                StorageStatsBucket::from_spans(&spans, &values).to_json(&group_by)
            })
            .collect();
        (
            200,
            format!(
                r#"{{"total":{},"groups":[{}],"groupBy":[{}],"scannedSpans":{},"readPlan":{}}}"#,
                total.to_json(&[]),
                group_json.join(","),
                group_by
                    .iter()
                    .map(|g| json_string_value(g))
                    .collect::<Vec<_>>()
                    .join(","),
                read.read_plan.scanned_segments,
                json_read_plan(&read.read_plan),
            ),
        )
    }

    /// POST /v1/trace-trajectories：按 traceSearch 过滤后，返回每条 trace 的路径摘要。
    fn trace_trajectories_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        let mut read = match self.trace_read_model_spans(body, tenant) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let v = match crate::wire::parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let cursor = json_field_alias(&v, &["cursor", "offset"])
            .and_then(crate::wire::Json::as_u64)
            .unwrap_or(0) as usize;
        let limit = json_field_alias(&v, &["limit"])
            .and_then(crate::wire::Json::as_u64)
            .unwrap_or(50)
            .clamp(1, 500) as usize;
        let trace_ids = unique_trace_ids(&read.spans);
        let total = trace_ids.len();
        let page_trace_ids: Vec<u64> = trace_ids
            .iter()
            .copied()
            .skip(cursor)
            .take(limit)
            .collect();
        let (mut by_trace, fetch_plan) =
            self.read_model_spans_by_trace_ids_with_plan(&page_trace_ids, tenant);
        merge_trace_fetch_plan(&mut read.read_plan, &fetch_plan);
        let items: Vec<String> = trace_ids
            .into_iter()
            .skip(cursor)
            .take(limit)
            .filter_map(|trace_id| {
                by_trace.remove(&trace_id).map(|mut spans| {
                    sort_spans_for_trajectory(&mut spans);
                    trace_trajectory_json(&spans)
                })
            })
            .collect();
        (
            200,
            format!(
                r#"{{"items":[{}],"total":{},"cursor":{},"limit":{},"scannedSpans":{},"readPlan":{}}}"#,
                items.join(","),
                total,
                cursor,
                limit,
                read.read_plan.scanned_segments,
                json_read_plan(&read.read_plan),
            ),
        )
    }

    /// POST /v1/trajectory-groups：把相同路径签名的 trace 分桶，找稳定路径候选证据。
    fn trajectory_groups_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        let mut read = match self.trace_read_model_spans(body, tenant) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let v = match crate::wire::parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let limit = json_field_alias(&v, &["limit"])
            .and_then(crate::wire::Json::as_u64)
            .unwrap_or(50)
            .clamp(1, 500) as usize;
        let sort = json_field_alias(&v, &["sort", "sortBy", "sort_by"])
            .and_then(crate::wire::Json::as_str)
            .unwrap_or("count");
        let mut buckets: std::collections::BTreeMap<String, TrajectoryGroupBucket> =
            std::collections::BTreeMap::new();
        let trace_ids = unique_trace_ids(&read.spans);
        let (mut by_trace, fetch_plan) =
            self.read_model_spans_by_trace_ids_with_plan(&trace_ids, tenant);
        merge_trace_fetch_plan(&mut read.read_plan, &fetch_plan);
        for trace_id in trace_ids {
            let Some(mut spans) = by_trace.remove(&trace_id) else {
                continue;
            };
            sort_spans_for_trajectory(&mut spans);
            let signature = trajectory_signature(&spans);
            buckets
                .entry(signature.clone())
                .or_insert_with(|| {
                    TrajectoryGroupBucket::new(signature, trajectory_steps_json(&spans))
                })
                .add(&spans);
        }
        let mut items: Vec<TrajectoryGroupBucket> = buckets.into_values().collect();
        match sort.to_ascii_lowercase().as_str() {
            "best" | "success" => items.sort_by(|a, b| {
                b.success_count
                    .cmp(&a.success_count)
                    .then_with(|| b.trace_count.cmp(&a.trace_count))
                    .then_with(|| a.signature.cmp(&b.signature))
            }),
            _ => items.sort_by(|a, b| {
                b.trace_count
                    .cmp(&a.trace_count)
                    .then_with(|| b.success_count.cmp(&a.success_count))
                    .then_with(|| a.signature.cmp(&b.signature))
            }),
        }
        let total = items.len();
        let item_json: Vec<String> = items
            .into_iter()
            .take(limit)
            .map(TrajectoryGroupBucket::to_json)
            .collect();
        (
            200,
            format!(
                r#"{{"items":[{}],"total":{},"scannedSpans":{},"readPlan":{}}}"#,
                item_json.join(","),
                total,
                read.read_plan.scanned_segments,
                json_read_plan(&read.read_plan),
            ),
        )
    }

    /// POST /v1/traces/diff：比较两条 trace 的路径和粗粒度指标差异。
    fn trace_diff_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        use crate::wire::Json;
        let v = match crate::wire::parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let Some(left_id) = json_field_alias(
            &v,
            &[
                "leftTraceId",
                "left_trace_id",
                "baseTraceId",
                "base_trace_id",
                "sourceTraceId",
                "source_trace_id",
                "traceA",
            ],
        )
        .and_then(json_id_or_hash) else {
            return (400, r#"{"error":"left trace id required"}"#.to_string());
        };
        let Some(right_id) = json_field_alias(
            &v,
            &[
                "rightTraceId",
                "right_trace_id",
                "candidateTraceId",
                "candidate_trace_id",
                "targetTraceId",
                "target_trace_id",
                "traceB",
            ],
        )
        .and_then(json_id_or_hash) else {
            return (400, r#"{"error":"right trace id required"}"#.to_string());
        };
        let left = self.full_trace_spans(left_id, tenant);
        let right = self.full_trace_spans(right_id, tenant);
        if left.is_empty() || right.is_empty() {
            return (404, r#"{"error":"trace not found"}"#.to_string());
        }
        let include_steps = json_field_alias(&v, &["includeSteps", "include_steps"])
            .and_then(|j| match j {
                Json::Bool(v) => Some(*v),
                _ => None,
            })
            .unwrap_or(true);
        (200, trace_diff_result_json(&left, &right, include_steps))
    }

    // POST /v1/annotations：给 trace/span 记录人工或自动审核结论。
}
