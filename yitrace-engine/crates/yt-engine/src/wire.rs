//! 极小 JSON 解析器（只用标准库）+ `parse_wire_batch`：把 SDK `to_wire()` 输出的 JSON 批量
//! 解析成 `WireRecord`。这是网络网关的解析层（HTTP server 收到 body 后调它）。
//!
//! 为什么自己写：保持引擎零外部依赖、离线可编译。真实部署嫌烦可换 serde_json，接口不变。
//!
//! 两个坑都处理了：
//! 1. **大整数超 f64 精度**（trace_id ~8.5e17、event_id ~1.2e19）→ 数字按**原始字符串**存，
//!    按需解析成 u64/i64，绝不过 f64。
//! 2. **Python 发数字、TS 发字符串**（BigInt.toString 避免 JS 精度丢失）→ 整数字段两种都接。
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::iter::Peekable;
use std::str::Chars;

use crate::WireRecord;
use yt_core::event::fnv1a64;

/// JSON 值。数字存原始字面量字符串（避免 f64 精度问题）。
/// `pub(crate)` 是给 OTLP 适配器（`otlp.rs`）复用这套零依赖解析器。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Json {
    Null,
    Bool(bool),
    Num(String),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub(crate) fn get<'a>(&'a self, key: &str) -> Option<&'a Json> {
        match self {
            Json::Obj(kvs) => kvs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    /// 取整数（接受数字字面量 或 数字字符串，兼容 Python/TS 两种 SDK）。Null/缺失 → None。
    pub(crate) fn as_u64(&self) -> Option<u64> {
        match self {
            Json::Num(s) | Json::Str(s) => s.parse::<u64>().ok(),
            _ => None,
        }
    }
    pub(crate) fn as_i64(&self) -> Option<i64> {
        match self {
            Json::Num(s) | Json::Str(s) => s.parse::<i64>().ok(),
            _ => None,
        }
    }
    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }
    /// 取浮点（数字字面量或数字字符串）。向量分量解析用。
    pub(crate) fn as_f32(&self) -> Option<f32> {
        match self {
            Json::Num(s) | Json::Str(s) => s.parse::<f32>().ok(),
            _ => None,
        }
    }
    pub(crate) fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Num(s) | Json::Str(s) => s.parse::<f64>().ok(),
            _ => None,
        }
    }
    fn as_str_array(&self) -> Vec<String> {
        match self {
            Json::Arr(items) => items
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect(),
            _ => Vec::new(),
        }
    }
    /// 数组元素（非数组 → 空切片）。OTLP 适配器遍历 resourceSpans/scopeSpans/spans 用。
    pub(crate) fn as_array(&self) -> &[Json] {
        match self {
            Json::Arr(items) => items,
            _ => &[],
        }
    }
    pub(crate) fn to_compact_json(&self) -> String {
        match self {
            Json::Null => "null".to_string(),
            Json::Bool(v) => v.to_string(),
            Json::Num(s) => s.clone(),
            Json::Str(s) => format!("\"{}\"", json_escape(s)),
            Json::Arr(items) => format!(
                "[{}]",
                items
                    .iter()
                    .map(Json::to_compact_json)
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            Json::Obj(kvs) => format!(
                "{{{}}}",
                kvs.iter()
                    .map(|(k, v)| format!("\"{}\":{}", json_escape(k), v.to_compact_json()))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        }
    }
}

/// 取字段（缺失或 null 都算 None）。
pub(crate) fn field<'a>(obj: &'a Json, key: &str) -> Option<&'a Json> {
    match obj.get(key) {
        Some(Json::Null) | None => None,
        Some(v) => Some(v),
    }
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn parse_id_value(v: &Json) -> Option<(u64, Option<String>)> {
    match v {
        Json::Num(s) => s.parse::<u64>().ok().map(|id| (id, None)),
        Json::Str(s) => match s.parse::<u64>() {
            Ok(id) => Some((id, None)),
            Err(_) => Some((fnv1a64(s.as_bytes()), Some(s.clone()))),
        },
        _ => None,
    }
}

fn parse_req_id(obj: &Json, key: &str, i: usize) -> Result<(u64, Option<String>), String> {
    field(obj, key)
        .and_then(parse_id_value)
        .ok_or_else(|| format!("第{i}条缺/坏字段 {key}"))
}

fn parse_opt_id(obj: &Json, key: &str) -> (Option<u64>, Option<String>) {
    field(obj, key)
        .and_then(parse_id_value)
        .map(|(id, ext)| (Some(id), ext))
        .unwrap_or((None, None))
}

fn attrs_map(i: usize, v: Option<&Json>) -> Result<BTreeMap<String, String>, String> {
    match v {
        Some(Json::Obj(kvs)) => Ok(kvs
            .iter()
            .map(|(k, v)| (k.clone(), v.to_compact_json()))
            .collect::<BTreeMap<_, _>>()),
        Some(Json::Null) | None => Ok(BTreeMap::new()),
        Some(_) => Err(format!("第{i}条 attrs 必须是对象")),
    }
}

pub(crate) fn usd_nanos(v: &Json) -> Option<u64> {
    if let Some(n) = v.as_u64() {
        return Some(n.saturating_mul(1_000_000_000));
    }
    let usd = v.as_f64()?;
    if !usd.is_finite() || usd < 0.0 {
        return None;
    }
    let nanos = (usd * 1_000_000_000.0).round();
    if nanos > u64::MAX as f64 {
        None
    } else {
        Some(nanos as u64)
    }
}

/// 把一批 SDK 线格式 JSON（数组）解析成 WireRecord。引擎自算 event_id，故忽略线里的 event_id。
pub fn parse_wire_batch(s: &str) -> Result<Vec<WireRecord>, String> {
    let v = parse(s)?;
    let arr = match v {
        Json::Arr(a) => a,
        _ => return Err("顶层必须是数组".into()),
    };
    let mut out = Vec::with_capacity(arr.len());
    for (i, obj) in arr.iter().enumerate() {
        let req_u64 = |k: &str| {
            field(obj, k)
                .and_then(Json::as_u64)
                .ok_or_else(|| format!("第{i}条缺/坏字段 {k}"))
        };
        let req_i64 = |k: &str| {
            field(obj, k)
                .and_then(Json::as_i64)
                .ok_or_else(|| format!("第{i}条缺/坏字段 {k}"))
        };
        let req_u8 = |k: &str| {
            let value = req_u64(k)?;
            if value <= u8::MAX as u64 {
                Ok(value as u8)
            } else {
                Err(format!("第{i}条字段 {k} 超出 u8 范围"))
            }
        };
        let opt_u64 = |k: &str| field(obj, k).and_then(Json::as_u64);
        let opt_u8 = |k: &str| {
            opt_u64(k)
                .map(|value| {
                    if value <= u8::MAX as u64 {
                        Ok(value as u8)
                    } else {
                        Err(format!("第{i}条字段 {k} 超出 u8 范围"))
                    }
                })
                .transpose()
        };
        let opt_str = |k: &str| field(obj, k).and_then(Json::as_str).map(|s| s.to_string());
        let (trace_id, trace_ext_from_id) = parse_req_id(obj, "trace_id", i)?;
        let (span_id, span_ext_from_id) = parse_req_id(obj, "span_id", i)?;
        let (parent_span_id, parent_ext_from_id) = parse_opt_id(obj, "parent_span_id");
        let (session_id, session_ext_from_id) = parse_opt_id(obj, "session_id");
        let mut attrs = attrs_map(i, field(obj, "attrs"))?;
        for (alias, key) in wire_attr_aliases() {
            if let Some(value) = field(obj, alias) {
                attrs.insert((*key).to_string(), value.to_compact_json());
            }
        }
        out.push(WireRecord {
            trace_id,
            span_id,
            ts: req_i64("ts")?,
            seq: req_u64("seq")?,
            event_type_tag: req_u8("event_type")?,
            ext_span_id: field(obj, "ext_span_id")
                .and_then(Json::as_str)
                .ok_or_else(|| format!("第{i}条缺 ext_span_id"))?
                .to_string(),
            parent_span_id,
            status: opt_u8("status")?,
            duration_ns: opt_u64("duration_ns"),
            input_tokens: opt_u64("input_tokens"),
            output_tokens: opt_u64("output_tokens"),
            cached_input_tokens: json_field_alias(
                obj,
                &[
                    "cached_input_tokens",
                    "cachedInputTokens",
                    "input_tokens_cached",
                ],
            )
            .and_then(Json::as_u64),
            reasoning_tokens: json_field_alias(obj, &["reasoning_tokens", "reasoningTokens"])
                .and_then(Json::as_u64),
            total_tokens: json_field_alias(obj, &["total_tokens", "totalTokens"])
                .and_then(Json::as_u64),
            cost_usd_nanos: json_field_alias(obj, &["cost_usd_nanos", "costUsdNanos"])
                .and_then(Json::as_u64)
                .or_else(|| json_field_alias(obj, &["cost_usd", "costUsd"]).and_then(usd_nanos)),
            cost_currency: json_field_alias(obj, &["cost_currency", "costCurrency"])
                .and_then(Json::as_str)
                .map(|s| s.to_string()),
            provider: json_field_alias(obj, &["provider", "llm_provider", "llmProvider"])
                .and_then(Json::as_str)
                .map(|s| s.to_string()),
            session_id,
            tenant_id: opt_u64("tenant_id"),
            external_trace_id: opt_str("external_trace_id").or(trace_ext_from_id),
            external_span_id: opt_str("external_span_id").or(span_ext_from_id),
            external_parent_span_id: opt_str("external_parent_span_id").or(parent_ext_from_id),
            external_session_id: opt_str("external_session_id").or(session_ext_from_id),
            agent_name: opt_str("agent_name"),
            tool_name: opt_str("tool_name"),
            model: opt_str("model"),
            input_text: opt_str("input_text"),
            output_text: opt_str("output_text"),
            logs: obj.get("logs").map(Json::as_str_array).unwrap_or_default(),
            attrs,
        });
    }
    Ok(out)
}

fn wire_attr_aliases() -> &'static [(&'static str, &'static str)] {
    &[
        ("project_id", "project_id"),
        ("projectId", "project_id"),
        ("skill", "skill"),
        ("mode", "mode"),
        ("call_site", "call_site"),
        ("callSite", "call_site"),
        ("task_fingerprint", "task_fingerprint"),
        ("taskFingerprint", "task_fingerprint"),
        ("loop_id", "loop_id"),
        ("loopId", "loop_id"),
        ("harness_version", "harness_version"),
        ("harnessVersion", "harness_version"),
        ("schema_fingerprint", "schema_fingerprint"),
        ("schemaFingerprint", "schema_fingerprint"),
        ("intent_signature", "intent_signature"),
        ("intentSignature", "intent_signature"),
        ("validation_status", "validation_status"),
        ("validationStatus", "validation_status"),
        ("review_status", "review_status"),
        ("reviewStatus", "review_status"),
        ("eval_status", "eval_status"),
        ("evalStatus", "eval_status"),
        ("path_memory_id", "path_memory_id"),
        ("pathMemoryId", "path_memory_id"),
        ("stop_reason", "stop_reason"),
        ("stopReason", "stop_reason"),
        ("phase", "phase"),
        ("validator", "validator"),
    ]
}

pub(crate) fn json_field_alias<'a>(obj: &'a Json, aliases: &[&str]) -> Option<&'a Json> {
    aliases.iter().find_map(|key| field(obj, key))
}

// ───────────────────────── 解析器 ─────────────────────────

pub(crate) fn parse(s: &str) -> Result<Json, String> {
    let mut it = s.chars().peekable();
    let v = parse_value(&mut it)?;
    skip_ws(&mut it);
    if it.peek().is_some() {
        return Err("尾部有多余内容".into());
    }
    Ok(v)
}

fn skip_ws(it: &mut Peekable<Chars>) {
    while matches!(it.peek(), Some(' ' | '\t' | '\n' | '\r')) {
        it.next();
    }
}

fn parse_value(it: &mut Peekable<Chars>) -> Result<Json, String> {
    skip_ws(it);
    match it.peek().copied() {
        Some('{') => parse_obj(it),
        Some('[') => parse_arr(it),
        Some('"') => Ok(Json::Str(parse_string(it)?)),
        Some('t') | Some('f') => parse_bool(it),
        Some('n') => parse_null(it),
        Some(c) if c == '-' || c.is_ascii_digit() => parse_number(it),
        Some(c) => Err(format!("意外字符 {c:?}")),
        None => Err("空输入".into()),
    }
}

fn expect(it: &mut Peekable<Chars>, c: char) -> Result<(), String> {
    skip_ws(it);
    match it.next() {
        Some(x) if x == c => Ok(()),
        other => Err(format!("期望 {c:?}，得到 {other:?}")),
    }
}

fn parse_obj(it: &mut Peekable<Chars>) -> Result<Json, String> {
    expect(it, '{')?;
    let mut kvs = Vec::new();
    skip_ws(it);
    if it.peek() == Some(&'}') {
        it.next();
        return Ok(Json::Obj(kvs));
    }
    loop {
        skip_ws(it);
        let key = parse_string(it)?;
        expect(it, ':')?;
        let val = parse_value(it)?;
        kvs.push((key, val));
        skip_ws(it);
        match it.next() {
            Some(',') => continue,
            Some('}') => break,
            other => return Err(format!("对象里期望 , 或 }}，得到 {other:?}")),
        }
    }
    Ok(Json::Obj(kvs))
}

fn parse_arr(it: &mut Peekable<Chars>) -> Result<Json, String> {
    expect(it, '[')?;
    let mut items = Vec::new();
    skip_ws(it);
    if it.peek() == Some(&']') {
        it.next();
        return Ok(Json::Arr(items));
    }
    loop {
        items.push(parse_value(it)?);
        skip_ws(it);
        match it.next() {
            Some(',') => continue,
            Some(']') => break,
            other => return Err(format!("数组里期望 , 或 ]，得到 {other:?}")),
        }
    }
    Ok(Json::Arr(items))
}

fn parse_string(it: &mut Peekable<Chars>) -> Result<String, String> {
    expect(it, '"')?;
    let mut out = String::new();
    loop {
        match it.next() {
            None => return Err("字符串未闭合".into()),
            Some('"') => break,
            Some('\\') => match it.next() {
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('/') => out.push('/'),
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('b') => out.push('\u{08}'),
                Some('f') => out.push('\u{0C}'),
                Some('u') => {
                    let mut code = 0u32;
                    for _ in 0..4 {
                        let h = it.next().ok_or("\\u 后不足 4 位")?;
                        code = code * 16 + h.to_digit(16).ok_or("\\u 后非十六进制")?;
                    }
                    out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                }
                other => return Err(format!("非法转义 \\{other:?}")),
            },
            Some(c) => out.push(c), // 含多字节 UTF-8（中文）
        }
    }
    Ok(out)
}

fn parse_number(it: &mut Peekable<Chars>) -> Result<Json, String> {
    let mut s = String::new();
    while let Some(&c) = it.peek() {
        if c == '-' || c == '+' || c == '.' || c == 'e' || c == 'E' || c.is_ascii_digit() {
            s.push(c);
            it.next();
        } else {
            break;
        }
    }
    if s.is_empty() {
        return Err("空数字".into());
    }
    Ok(Json::Num(s))
}

fn parse_bool(it: &mut Peekable<Chars>) -> Result<Json, String> {
    let want = if it.peek() == Some(&'t') {
        "true"
    } else {
        "false"
    };
    for c in want.chars() {
        if it.next() != Some(c) {
            return Err("非法 bool".into());
        }
    }
    Ok(Json::Bool(want == "true"))
}

fn parse_null(it: &mut Peekable<Chars>) -> Result<Json, String> {
    for c in "null".chars() {
        if it.next() != Some(c) {
            return Err("非法 null".into());
        }
    }
    Ok(Json::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Python SDK 真实输出（含大整数、转义引号、中文、null、数组）。
    const SAMPLE: &str = r#"[{"trace_id": 855355598420578304, "span_id": 855355598420578305, "ts": 1781769466402119000, "seq": 1, "event_type": 1, "ext_span_id": "855355598420578304-855355598420578305", "parent_span_id": null, "event_id": 5031140639032392837, "status": null, "duration_ns": null, "input_tokens": null, "output_tokens": null, "logs": ["LLM研判"]}, {"trace_id": 855355598420578304, "span_id": 855355598420578305, "ts": 1781769466402124000, "seq": 2, "event_type": 4, "ext_span_id": "855355598420578304-855355598420578305", "parent_span_id": null, "event_id": 12855482683663564275, "status": null, "duration_ns": null, "input_tokens": null, "output_tokens": null, "logs": ["结论: \"需复核\""]}, {"trace_id": 855355598420578304, "span_id": 855355598420578305, "ts": 1781769466402128000, "seq": 3, "event_type": 2, "ext_span_id": "855355598420578304-855355598420578305", "parent_span_id": null, "event_id": 2233092749213094418, "status": null, "duration_ns": 9000, "input_tokens": 1200, "output_tokens": 340, "logs": []}]"#;

    #[test]
    fn parses_real_python_wire_sample() {
        let recs = parse_wire_batch(SAMPLE).unwrap();
        assert_eq!(recs.len(), 3);
        // 大整数不丢精度
        assert_eq!(recs[0].trace_id, 855355598420578304);
        assert_eq!(recs[0].span_id, 855355598420578305);
        assert_eq!(recs[0].ts, 1781769466402119000);
        assert_eq!(recs[0].event_type_tag, 1);
        assert_eq!(recs[0].logs, vec!["LLM研判"]); // 中文
                                                   // 转义引号 + 中文
        assert_eq!(recs[1].logs, vec!["结论: \"需复核\""]);
        // null → None；token 整数
        assert_eq!(recs[0].parent_span_id, None);
        assert_eq!(recs[2].duration_ns, Some(9000));
        assert_eq!(recs[2].input_tokens, Some(1200));
        assert_eq!(recs[2].output_tokens, Some(340));
    }

    #[test]
    fn accepts_ts_style_string_encoded_ints() {
        // TS 的 to_wire 把 BigInt 转成字符串("855...")避免精度丢失 —— 解析器要接住。
        let ts_json = r#"[{"trace_id":"855355598420578304","span_id":"5","ts":"100","seq":"1","event_type":2,"ext_span_id":"x","parent_span_id":"5","status":null,"duration_ns":"9000","input_tokens":"1200","output_tokens":null,"logs":[]}]"#;
        let recs = parse_wire_batch(ts_json).unwrap();
        assert_eq!(recs[0].trace_id, 855355598420578304);
        assert_eq!(recs[0].parent_span_id, Some(5));
        assert_eq!(recs[0].duration_ns, Some(9000));
        assert_eq!(recs[0].input_tokens, Some(1200));
    }

    #[test]
    fn parses_usage_cost_fields() {
        let body = r#"[{
          "trace_id":"run-1",
          "span_id":"span-1",
          "ts":"100",
          "seq":"1",
          "event_type":2,
          "ext_span_id":"span-1",
          "input_tokens":1200,
          "output_tokens":340,
          "cachedInputTokens":80,
          "reasoningTokens":40,
          "totalTokens":1660,
          "costUsd":0.001234,
          "costCurrency":"USD",
          "llmProvider":"openai",
          "logs":[]
        }]"#;
        let recs = parse_wire_batch(body).unwrap();
        let rec = &recs[0];
        assert_eq!(rec.cached_input_tokens, Some(80));
        assert_eq!(rec.reasoning_tokens, Some(40));
        assert_eq!(rec.total_tokens, Some(1660));
        assert_eq!(rec.cost_usd_nanos, Some(1_234_000));
        assert_eq!(rec.cost_currency.as_deref(), Some("USD"));
        assert_eq!(rec.provider.as_deref(), Some("openai"));
    }

    #[test]
    fn hashes_external_ids_and_preserves_attrs() {
        let body = r#"[{"trace_id":"run-uuid","span_id":"span-uuid","parent_span_id":"parent-uuid","session_id":"session-uuid","ts":100,"seq":1,"event_type":1,"ext_span_id":"span-uuid","taskFingerprint":"npm-native-packaging","loopId":"loop-1","harnessVersion":"h1","schemaFingerprint":"schema-v1","intentSignature":"refund-review","validationStatus":"pass","reviewStatus":"approved","evalStatus":"pass","pathMemoryId":"pm-1","stopReason":"goal_met","phase":"verify","validator":"npm test","attrs":{"external_run_id":"run-uuid","project_id":7,"nested":{"ok":true},"skill":"review"}}]"#;
        let recs = parse_wire_batch(body).unwrap();
        let r = &recs[0];
        assert_eq!(r.trace_id, fnv1a64(b"run-uuid"));
        assert_eq!(r.span_id, fnv1a64(b"span-uuid"));
        assert_eq!(r.parent_span_id, Some(fnv1a64(b"parent-uuid")));
        assert_eq!(r.session_id, Some(fnv1a64(b"session-uuid")));
        assert_eq!(r.external_trace_id.as_deref(), Some("run-uuid"));
        assert_eq!(r.external_span_id.as_deref(), Some("span-uuid"));
        assert_eq!(r.external_parent_span_id.as_deref(), Some("parent-uuid"));
        assert_eq!(r.external_session_id.as_deref(), Some("session-uuid"));
        assert_eq!(
            r.attrs.get("external_run_id").map(String::as_str),
            Some("\"run-uuid\"")
        );
        assert_eq!(r.attrs.get("project_id").map(String::as_str), Some("7"));
        assert_eq!(
            r.attrs.get("task_fingerprint").map(String::as_str),
            Some("\"npm-native-packaging\"")
        );
        assert_eq!(
            r.attrs.get("loop_id").map(String::as_str),
            Some("\"loop-1\"")
        );
        assert_eq!(
            r.attrs.get("harness_version").map(String::as_str),
            Some("\"h1\"")
        );
        assert_eq!(
            r.attrs.get("schema_fingerprint").map(String::as_str),
            Some("\"schema-v1\"")
        );
        assert_eq!(
            r.attrs.get("intent_signature").map(String::as_str),
            Some("\"refund-review\"")
        );
        assert_eq!(
            r.attrs.get("validation_status").map(String::as_str),
            Some("\"pass\"")
        );
        assert_eq!(
            r.attrs.get("review_status").map(String::as_str),
            Some("\"approved\"")
        );
        assert_eq!(
            r.attrs.get("eval_status").map(String::as_str),
            Some("\"pass\"")
        );
        assert_eq!(
            r.attrs.get("path_memory_id").map(String::as_str),
            Some("\"pm-1\"")
        );
        assert_eq!(
            r.attrs.get("stop_reason").map(String::as_str),
            Some("\"goal_met\"")
        );
        assert_eq!(r.attrs.get("phase").map(String::as_str), Some("\"verify\""));
        assert_eq!(
            r.attrs.get("validator").map(String::as_str),
            Some("\"npm test\"")
        );
        assert_eq!(
            r.attrs.get("nested").map(String::as_str),
            Some("{\"ok\":true}")
        );
    }

    #[test]
    fn rejects_malformed() {
        assert!(parse_wire_batch("not json").is_err());
        assert!(parse_wire_batch(r#"{"not":"array"}"#).is_err());
        assert!(parse_wire_batch(r#"[{"span_id":1}]"#).is_err()); // 缺 trace_id
    }
}
