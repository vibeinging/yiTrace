fn json_truthy(v: &crate::wire::Json) -> bool {
    match v {
        crate::wire::Json::Bool(b) => *b,
        crate::wire::Json::Num(n) | crate::wire::Json::Str(n) => {
            matches!(n.as_str(), "1" | "true" | "yes")
        }
        _ => false,
    }
}

fn parse_json_body_or_empty(body: &str) -> Result<crate::wire::Json, String> {
    if body.trim().is_empty() {
        Ok(crate::wire::Json::Obj(Vec::new()))
    } else {
        crate::wire::parse(body)
    }
}

fn unix_now_ns() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// 极小 URL 解码（只处理 %XX 与 +）：会话过滤词可能是中文 → 解 percent-encoding。
fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let h = |c: u8| (c as char).to_digit(16);
                if let (Some(hi), Some(lo)) = (h(b[i + 1]), h(b[i + 2])) {
                    out.push((hi * 16 + lo) as u8);
                    i += 3;
                    continue;
                }
                out.push(b[i]);
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn cost_usd_num_from_nanos(nanos: u64) -> String {
    format!("{:.6}", nanos as f64 / 1_000_000_000.0)
}

fn cost_detail_json(nanos: u64, currency: Option<&str>, source: &str) -> String {
    format!(
        r#"{{"costUsd":{},"costUsdNanos":{},"currency":"{}","source":"{}"}}"#,
        cost_usd_num_from_nanos(nanos),
        nanos,
        json_escape(currency.unwrap_or("USD")),
        source,
    )
}

fn folded_cost_usd_nanos(s: &FoldedSpan) -> u64 {
    crate::usage_cost_usd_nanos_for_model(
        s.input_tokens.unwrap_or(0),
        s.output_tokens.unwrap_or(0),
        s.cached_input_tokens.unwrap_or(0),
        s.reasoning_tokens.unwrap_or(0),
        s.cost_usd_nanos,
        s.provider.as_deref(),
        s.model.as_deref(),
    )
}

fn folded_cost_source(s: &FoldedSpan) -> &'static str {
    crate::usage_cost_source(s.cost_usd_nanos, s.provider.as_deref(), s.model.as_deref())
}

fn folded_total_tokens(s: &FoldedSpan) -> u64 {
    crate::usage_total_tokens(
        s.input_tokens.unwrap_or(0),
        s.output_tokens.unwrap_or(0),
        s.cached_input_tokens.unwrap_or(0),
        s.reasoning_tokens.unwrap_or(0),
        s.total_tokens,
    )
}

fn folded_usage_json(s: &FoldedSpan) -> String {
    format!(
        r#"{{"inputTokens":{},"outputTokens":{},"cachedInputTokens":{},"reasoningTokens":{},"totalTokens":{}}}"#,
        s.input_tokens.unwrap_or(0),
        s.output_tokens.unwrap_or(0),
        s.cached_input_tokens.unwrap_or(0),
        s.reasoning_tokens.unwrap_or(0),
        folded_total_tokens(s),
    )
}

fn console_usage_json(s: &crate::ConsoleSpan) -> String {
    format!(
        r#"{{"inputTokens":{},"outputTokens":{},"cachedInputTokens":{},"reasoningTokens":{},"totalTokens":{}}}"#,
        s.input_tokens, s.output_tokens, s.cached_input_tokens, s.reasoning_tokens, s.total_tokens,
    )
}

fn usage_json(
    input_tokens: u64,
    output_tokens: u64,
    cached_input_tokens: u64,
    reasoning_tokens: u64,
    total_tokens: u64,
) -> String {
    format!(
        r#"{{"inputTokens":{},"outputTokens":{},"cachedInputTokens":{},"reasoningTokens":{},"totalTokens":{}}}"#,
        input_tokens, output_tokens, cached_input_tokens, reasoning_tokens, total_tokens
    )
}

/// 兼容旧字段：输入 8e-7、输出 4e-6 每 token。新代码优先使用 `costUsd`/`costDetail`。
fn cost_num(in_tok: u64, out_tok: u64) -> String {
    format!("{:.3}", in_tok as f64 * 8e-7 + out_tok as f64 * 4e-6)
}

fn parse_id_or_hash(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        None
    } else {
        Some(
            s.parse::<u64>()
                .unwrap_or_else(|_| yt_core::event::fnv1a64(s.as_bytes())),
        )
    }
}

fn json_id_or_hash(v: &crate::wire::Json) -> Option<u64> {
    v.as_u64()
        .or_else(|| v.as_str().map(|s| yt_core::event::fnv1a64(s.as_bytes())))
}

fn json_id_with_external(v: &crate::wire::Json) -> Option<(u64, Option<String>)> {
    match v {
        crate::wire::Json::Num(s) => s.parse::<u64>().ok().map(|id| (id, None)),
        crate::wire::Json::Str(s) => match s.parse::<u64>() {
            Ok(id) => Some((id, None)),
            Err(_) => Some((yt_core::event::fnv1a64(s.as_bytes()), Some(s.clone()))),
        },
        _ => None,
    }
}

fn json_internal_id(v: &crate::wire::Json) -> Option<u64> {
    v.as_u64().or_else(|| v.as_str().and_then(parse_id_or_hash))
}
