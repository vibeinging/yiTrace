fn json_trace_diff(
    left_id: u64,
    right_id: u64,
    left: &[FoldedSpan],
    right: &[FoldedSpan],
) -> String {
    let left_summary = trace_summary_buckets_from_spans(left);
    let right_summary = trace_summary_buckets_from_spans(right);
    let left_bucket = left_summary.first();
    let right_bucket = right_summary.first();
    format!(
        r#"{{"left":{},"right":{},"delta":{},"trajectory":{},"routes":{{"left":{},"right":{}}},"steps":{}}}"#,
        trace_diff_side_json(left_id, left_bucket),
        trace_diff_side_json(right_id, right_bucket),
        trace_diff_delta_json(left_bucket, right_bucket),
        trace_diff_trajectory_json(left, right),
        trace_diff_route_json(left),
        trace_diff_route_json(right),
        trace_diff_steps_json(left, right),
    )
}

fn trace_diff_side_json(trace_id: u64, bucket: Option<&TaskTraceSummaryBucket>) -> String {
    if let Some(bucket) = bucket {
        format!(
            r#"{{"traceId":"{}","externalTraceId":{},"spanCount":{},"errorCount":{},"status":"{}","durationNs":{{"sum":{},"max":{}}},"usage":{},"costUsd":{},"costDetail":{},"fields":{}}}"#,
            trace_id,
            json_opt_str(bucket.external_trace_id.as_deref()),
            bucket.span_count,
            bucket.error_count,
            if bucket.error_count > 0 {
                "error"
            } else {
                "ok"
            },
            bucket.duration_sum_ns,
            bucket.duration_max_ns,
            usage_json(
                bucket.input_tokens,
                bucket.output_tokens,
                bucket.cached_input_tokens,
                bucket.reasoning_tokens,
                bucket.total_tokens,
            ),
            cost_usd_num_from_nanos(bucket.cost_usd_nanos),
            cost_detail_json(bucket.cost_usd_nanos, Some("USD"), "mixed"),
            json_attrs(&bucket.fields),
        )
    } else {
        format!(
            r#"{{"traceId":"{}","spanCount":0,"errorCount":0,"status":"missing","durationNs":{{"sum":0,"max":0}},"usage":{},"costUsd":0.000000,"costDetail":{},"fields":{{}}}}"#,
            trace_id,
            usage_json(0, 0, 0, 0, 0),
            cost_detail_json(0, Some("USD"), "mixed"),
        )
    }
}

fn trace_trajectory_side_json(summary: &crate::TraceTrajectorySummary) -> String {
    format!(
        r#"{{"traceId":"{}","externalTraceId":{},"spanCount":{},"errorCount":{},"status":"{}","durationNs":{{"sum":{},"max":{}}},"usage":{},"costUsd":{},"costDetail":{},"fields":{}}}"#,
        summary.trace_id,
        json_opt_str(summary.external_trace_id.as_deref()),
        summary.span_count,
        summary.error_count,
        if summary.error_count > 0 {
            "error"
        } else {
            "ok"
        },
        summary.duration_sum_ns,
        summary.duration_max_ns,
        usage_json(
            summary.input_tokens,
            summary.output_tokens,
            summary.cached_input_tokens,
            summary.reasoning_tokens,
            summary.total_tokens,
        ),
        cost_usd_num_from_nanos(summary.cost_usd_nanos),
        cost_detail_json(summary.cost_usd_nanos, Some("USD"), "mixed"),
        json_attrs(&summary.fields),
    )
}

fn json_trace_trajectory_summary(summary: &crate::TraceTrajectorySummary) -> String {
    format!(
        r#"{{"trace":{},"trajectory":{},"index":"materialized"}}"#,
        trace_trajectory_side_json(summary),
        trajectory_summary_json_with_signature(&summary.steps, &summary.trajectory_signature),
    )
}
