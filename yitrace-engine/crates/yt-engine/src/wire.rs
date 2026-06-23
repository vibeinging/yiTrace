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

use std::iter::Peekable;
use std::str::Chars;

use crate::WireRecord;

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
    fn as_str_array(&self) -> Vec<String> {
        match self {
            Json::Arr(items) => items.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect(),
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
}

/// 取字段（缺失或 null 都算 None）。
pub(crate) fn field<'a>(obj: &'a Json, key: &str) -> Option<&'a Json> {
    match obj.get(key) {
        Some(Json::Null) | None => None,
        Some(v) => Some(v),
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
        let req_u64 = |k: &str| field(obj, k).and_then(Json::as_u64).ok_or_else(|| format!("第{i}条缺/坏字段 {k}"));
        let req_i64 = |k: &str| field(obj, k).and_then(Json::as_i64).ok_or_else(|| format!("第{i}条缺/坏字段 {k}"));
        let opt_u64 = |k: &str| field(obj, k).and_then(Json::as_u64);
        let opt_str = |k: &str| field(obj, k).and_then(Json::as_str).map(|s| s.to_string());
        out.push(WireRecord {
            trace_id: req_u64("trace_id")?,
            span_id: req_u64("span_id")?,
            ts: req_i64("ts")?,
            seq: req_u64("seq")?,
            event_type_tag: req_u64("event_type")? as u8,
            ext_span_id: field(obj, "ext_span_id")
                .and_then(Json::as_str)
                .ok_or_else(|| format!("第{i}条缺 ext_span_id"))?
                .to_string(),
            parent_span_id: opt_u64("parent_span_id"),
            status: opt_u64("status").map(|v| v as u8),
            duration_ns: opt_u64("duration_ns"),
            input_tokens: opt_u64("input_tokens"),
            output_tokens: opt_u64("output_tokens"),
            session_id: opt_u64("session_id"),
            tenant_id: opt_u64("tenant_id"),
            agent_name: opt_str("agent_name"),
            tool_name: opt_str("tool_name"),
            model: opt_str("model"),
            input_text: opt_str("input_text"),
            output_text: opt_str("output_text"),
            logs: obj.get("logs").map(Json::as_str_array).unwrap_or_default(),
        });
    }
    Ok(out)
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
    let want = if it.peek() == Some(&'t') { "true" } else { "false" };
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
    fn rejects_malformed() {
        assert!(parse_wire_batch("not json").is_err());
        assert!(parse_wire_batch(r#"{"not":"array"}"#).is_err());
        assert!(parse_wire_batch(r#"[{"span_id":1}]"#).is_err()); // 缺 trace_id
    }
}
