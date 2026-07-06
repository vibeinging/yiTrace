fn trajectory_duration_json(bucket: &TrajectoryGroupBucket) -> String {
    let mut durations = bucket.durations_ns.clone();
    durations.sort_unstable();
    duration_values_json(&durations, bucket.duration_sum_ns, bucket.duration_max_ns)
}

fn score_stats_json(stats: &ScoreStats) -> String {
    if stats.count == 0 {
        r#"{"count":0,"avg":null,"min":null,"max":null}"#.to_string()
    } else {
        format!(
            r#"{{"count":{},"avg":{},"min":{},"max":{}}}"#,
            stats.count,
            stats.avg(),
            stats.min,
            stats.max,
        )
    }
}

fn trajectory_trace_quality_score(
    success: bool,
    spans: &[FoldedSpan],
    annotation_scores: Option<&[u32]>,
    dataset_scores: Option<&[u32]>,
) -> u32 {
    let mut sum = if success { 1000u64 } else { 0u64 };
    let mut count = 1u64;
    let mut eval = ScoreStats::default();
    for score in spans.iter().filter_map(|s| s.eval_score) {
        eval.add(score);
    }
    if eval.count > 0 {
        sum += eval.avg() as u64;
        count += 1;
    }
    for scores in [annotation_scores, dataset_scores].into_iter().flatten() {
        if !scores.is_empty() {
            let local_sum: u64 = scores.iter().map(|s| *s as u64).sum();
            sum += local_sum / scores.len() as u64;
            count += 1;
        }
    }
    (sum / count) as u32
}

fn trace_annotation_score_map(
    annotations: Vec<crate::TraceAnnotation>,
) -> std::collections::HashMap<u64, Vec<u32>> {
    let mut out = std::collections::HashMap::new();
    for annotation in annotations {
        if let Some(score) = annotation.score {
            out.entry(annotation.trace_id)
                .or_insert_with(Vec::new)
                .push(score);
        }
    }
    out
}

fn trace_dataset_score_map(
    associations: Vec<crate::DatasetAssociation>,
) -> std::collections::HashMap<u64, Vec<u32>> {
    let mut out = std::collections::HashMap::new();
    for assoc in associations {
        if let Some(score) = assoc.score {
            out.entry(assoc.trace_id)
                .or_insert_with(Vec::new)
                .push(score);
        }
    }
    out
}

fn trajectory_default_desc(sort_by: &str) -> bool {
    !matches!(
        sort_by
            .to_ascii_lowercase()
            .replace(['_', '-'], "")
            .as_str(),
        "duration" | "durationns" | "avgduration" | "durationavg" | "cost" | "avgcost"
    )
}

fn sort_trajectory_group_buckets(buckets: &mut [TrajectoryGroupBucket], sort_by: &str, desc: bool) {
    let sort = sort_by.to_ascii_lowercase().replace(['_', '-'], "");
    buckets.sort_by(|a, b| {
        let ord = match sort.as_str() {
            "tracecount" | "traces" | "count" => a.trace_count().cmp(&b.trace_count()),
            "spancount" | "spans" => a.span_count.cmp(&b.span_count),
            "errorcount" | "errors" => a.error_trace_count.cmp(&b.error_trace_count),
            "successrate" | "success" => trajectory_success_cmp(a, b),
            "eval" | "evalscore" | "avgeval" => a.eval_scores.avg().cmp(&b.eval_scores.avg()),
            "annotation" | "annotationscore" | "avgannotation" => {
                a.annotation_scores.avg().cmp(&b.annotation_scores.avg())
            }
            "dataset" | "datasetscore" | "avgdataset" => {
                a.dataset_scores.avg().cmp(&b.dataset_scores.avg())
            }
            "duration" | "durationns" | "avgduration" | "durationavg" => {
                a.avg_duration_ns().cmp(&b.avg_duration_ns())
            }
            "cost" | "avgcost" => a.avg_cost_usd_nanos().cmp(&b.avg_cost_usd_nanos()),
            "tokens" | "totaltokens" => a.total_tokens.cmp(&b.total_tokens),
            _ => trajectory_best_cmp(a, b),
        };
        let ord = if desc { ord.reverse() } else { ord };
        ord.then_with(|| a.signature.cmp(&b.signature))
    });
}

fn trajectory_best_cmp(a: &TrajectoryGroupBucket, b: &TrajectoryGroupBucket) -> std::cmp::Ordering {
    a.quality_score()
        .cmp(&b.quality_score())
        .then_with(|| trajectory_success_cmp(a, b))
        .then_with(|| a.eval_scores.avg().cmp(&b.eval_scores.avg()))
        .then_with(|| a.annotation_scores.avg().cmp(&b.annotation_scores.avg()))
        .then_with(|| a.dataset_scores.avg().cmp(&b.dataset_scores.avg()))
        .then_with(|| a.trace_count().cmp(&b.trace_count()))
        // 低耗时/低成本是更好的 tie-breaker：这里反向比较，让 desc 排序时更小的值靠前。
        .then_with(|| b.avg_duration_ns().cmp(&a.avg_duration_ns()))
        .then_with(|| b.avg_cost_usd_nanos().cmp(&a.avg_cost_usd_nanos()))
}

fn trajectory_success_cmp(
    a: &TrajectoryGroupBucket,
    b: &TrajectoryGroupBucket,
) -> std::cmp::Ordering {
    let left = a.success_count() as u128 * b.trace_count().max(1) as u128;
    let right = b.success_count() as u128 * a.trace_count().max(1) as u128;
    left.cmp(&right)
}

fn json_trajectory_group_bucket(bucket: &TrajectoryGroupBucket) -> String {
    let steps = bucket
        .steps
        .iter()
        .map(|s| json_string_value(s))
        .collect::<Vec<_>>()
        .join(",");
    let examples = bucket
        .examples
        .iter()
        .map(json_trajectory_trace_example)
        .collect::<Vec<_>>()
        .join(",");
    let trace_count = bucket.trace_count();
    let success_rate = if trace_count == 0 {
        0.0
    } else {
        bucket.success_count() as f64 / trace_count as f64
    };
    let error_rate = if trace_count == 0 {
        0.0
    } else {
        bucket.error_trace_count as f64 / trace_count as f64
    };
    format!(
        r#"{{"signature":"fnv1a64:{:016x}","stepCount":{},"steps":[{}],"traceCount":{},"spanCount":{},"successCount":{},"errorTraceCount":{},"errorSpanCount":{},"successRate":{:.6},"errorRate":{:.6},"qualityScore":{},"durationNs":{},"usage":{},"costUsd":{},"costDetail":{},"scores":{{"eval":{},"annotation":{},"dataset":{}}},"examples":[{}]}}"#,
        bucket.signature,
        bucket.steps.len(),
        steps,
        trace_count,
        bucket.span_count,
        bucket.success_count(),
        bucket.error_trace_count,
        bucket.error_span_count,
        success_rate,
        error_rate,
        bucket.quality_score(),
        trajectory_duration_json(bucket),
        usage_json(
            bucket.input_tokens,
            bucket.output_tokens,
            bucket.cached_input_tokens,
            bucket.reasoning_tokens,
            bucket.total_tokens,
        ),
        cost_usd_num_from_nanos(bucket.cost_usd_nanos),
        cost_detail_json(bucket.cost_usd_nanos, Some("USD"), "mixed"),
        score_stats_json(&bucket.eval_scores),
        score_stats_json(&bucket.annotation_scores),
        score_stats_json(&bucket.dataset_scores),
        examples,
    )
}

fn json_trajectory_trace_example(example: &TrajectoryTraceExample) -> String {
    format!(
        r#"{{"traceId":"{}","externalTraceId":{},"status":"{}","durationNs":{{"sum":{},"max":{}}},"usage":{},"costUsd":{},"costDetail":{},"qualityScore":{},"fields":{}}}"#,
        example.trace_id,
        json_opt_str(example.external_trace_id.as_deref()),
        json_escape(&example.status),
        example.duration_sum_ns,
        example.duration_max_ns,
        usage_json(
            example.input_tokens,
            example.output_tokens,
            example.cached_input_tokens,
            example.reasoning_tokens,
            example.total_tokens,
        ),
        cost_usd_num_from_nanos(example.cost_usd_nanos),
        cost_detail_json(example.cost_usd_nanos, Some("USD"), "mixed"),
        example.score,
        json_attrs(&example.fields),
    )
}

fn percentile_json(sorted: &[u64], percentile: usize) -> String {
    if sorted.is_empty() {
        return "null".to_string();
    }
    let idx = ((sorted.len() * percentile + 99) / 100)
        .saturating_sub(1)
        .min(sorted.len() - 1);
    sorted[idx].to_string()
}

fn trace_aggregate_example_json(example: &TraceAggregateExample) -> String {
    format!(
        r#"{{"traceId":"{}","spanId":"{}","externalTraceId":{},"externalSpanId":{},"name":"{}"}}"#,
        example.trace_id,
        example.span_id,
        json_opt_str(example.external_trace_id.as_deref()),
        json_opt_str(example.external_span_id.as_deref()),
        json_escape(&example.name),
    )
}
