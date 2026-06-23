//! OTLP / OpenInference 摄入适配器：把业界标准的 OpenTelemetry trace 导出（OTLP/HTTP JSON）
//! 映射成本引擎的 `WireRecord`。这是「生态入口」——任何已用 OpenTelemetry / OpenInference 埋点的
//! agent 应用，不改打点就能把数据灌进来。
//!
//! 认两套语义约定（业界 LLM trace 的两个事实标准）：
//! - **OTel GenAI**（`gen_ai.*`）：`gen_ai.request.model` / `gen_ai.usage.input_tokens` …
//! - **OpenInference**（Arize 系）：`llm.model_name` / `llm.token_count.prompt` / `input.value` …
//!
//! 映射策略：一条 OTLP span（带 start/end 两个时间戳）→ 拆成本引擎的 **SpanStart + SpanEnd 两个事件**
//! （seq=1/2，同一 `ext_span_id`=spanId 十六进制，确定性 event_id 自然成立）。属性挂在 start 事件上，
//! 状态/耗时挂在 end 事件上，读时折叠合并。零依赖：复用 `wire.rs` 的标准库 JSON 解析器。
//!
//! OTLP id 是 128 位 trace / 64 位 span（十六进制串）；本引擎 trace_id/span_id 是 u64，取低 64 位
//! 用于分组/剪枝，真正的去重身份是 `ext_span_id`（原样保留十六进制串）。
#![allow(dead_code)]

use crate::wire::{field, parse, Json};
use crate::WireRecord;
use yt_core::event::EventType;

/// 把一段 OTLP/HTTP JSON（`{"resourceSpans":[...]}`）解析并映射成 WireRecord 批。
pub fn parse_otlp_traces(s: &str) -> Result<Vec<WireRecord>, String> {
    let root = parse(s)?;
    let resource_spans = get2(&root, "resourceSpans", "resource_spans")
        .map(Json::as_array)
        .ok_or("缺 resourceSpans")?;
    let mut out = Vec::new();
    for rs in resource_spans {
        let scope_spans = get2(rs, "scopeSpans", "scope_spans").map(Json::as_array).unwrap_or(&[]);
        for ss in scope_spans {
            let spans = ss.get("spans").map(Json::as_array).unwrap_or(&[]);
            for sp in spans {
                map_span(sp, &mut out)?;
            }
        }
    }
    Ok(out)
}

/// 一条 OTLP span → SpanStart + SpanEnd 两个 WireRecord，推进 out。
fn map_span(sp: &Json, out: &mut Vec<WireRecord>) -> Result<(), String> {
    let trace_hex = get2(sp, "traceId", "trace_id").and_then(Json::as_str).ok_or("span 缺 traceId")?;
    let span_hex = get2(sp, "spanId", "span_id").and_then(Json::as_str).ok_or("span 缺 spanId")?;
    let trace_id = hex_low_u64(trace_hex);
    let span_id = hex_low_u64(span_hex);
    let parent_hex = get2(sp, "parentSpanId", "parent_span_id").and_then(Json::as_str).unwrap_or("");
    let parent_span_id = if parent_hex.is_empty() { None } else { Some(hex_low_u64(parent_hex)) };
    let name = sp.get("name").and_then(Json::as_str).unwrap_or("").to_string();
    let ts_start = get2(sp, "startTimeUnixNano", "start_time_unix_nano").and_then(Json::as_i64).unwrap_or(0);
    let ts_end = get2(sp, "endTimeUnixNano", "end_time_unix_nano").and_then(Json::as_i64).unwrap_or(ts_start);
    let duration_ns = (ts_end - ts_start).max(0) as u64;

    // status.code：2=Error → 本引擎 status=1（非0=异常）；1=Ok → 0；0/缺失=Unset → None。
    let status = sp.get("status").and_then(|st| get2(st, "code", "code").and_then(Json::as_u64)).and_then(|c| match c {
        2 => Some(1u8),
        1 => Some(0u8),
        _ => None,
    });

    let attrs = sp.get("attributes").map(Json::as_array).unwrap_or(&[]);
    let model = first_str(attrs, &["gen_ai.request.model", "gen_ai.response.model", "llm.model_name"]);
    let input_tokens = first_u64(attrs, &["gen_ai.usage.input_tokens", "gen_ai.usage.prompt_tokens", "llm.token_count.prompt"]);
    let output_tokens = first_u64(attrs, &["gen_ai.usage.output_tokens", "gen_ai.usage.completion_tokens", "llm.token_count.completion"]);
    let agent_name = first_str(attrs, &["gen_ai.agent.name", "agent.name"]);
    let tool_name = first_str(attrs, &["gen_ai.tool.name", "tool.name"]);
    // 大文本：OTel GenAI 的 gen_ai.prompt/completion 常是 **JSON 消息数组串**（[{role,content}]），
    // 不是人读的纯文本——直接存会把 eval 的输入/输出污染成 JSON。这里拍平成纯文本；OpenInference 的
    // input.value/output.value 是扁平串，flatten_messages 原样返回。再不行就从 span events 里捞
    //（新版 GenAI 约定把内容放在 span 事件里，不在属性上）。
    let mut input_text = first_str(attrs, &["input.value", "gen_ai.prompt"]).map(|s| flatten_messages(&s));
    let mut output_text = first_str(attrs, &["output.value", "gen_ai.completion"]).map(|s| flatten_messages(&s));
    if input_text.is_none() || output_text.is_none() {
        let (ev_in, ev_out) = texts_from_events(sp);
        input_text = input_text.or(ev_in);
        output_text = output_text.or(ev_out);
    }
    // 会话 id：OTLP 里是字符串（session.id / 会话 id）。本引擎要 u64 → 数字直接解析，否则确定性哈希。
    let session_id = first_str(attrs, &["session.id", "gen_ai.conversation.id", "session_id"]).map(|s| str_to_u64(&s));

    // SpanStart：携带所有属性派生字段（model/tokens/agent/tool/session/文本），name 进 logs。
    out.push(WireRecord {
        trace_id,
        span_id,
        ts: ts_start,
        seq: 1,
        event_type_tag: EventType::SpanStart.tag(),
        ext_span_id: span_hex.to_string(),
        parent_span_id,
        status: None,
        duration_ns: None,
        input_tokens,
        output_tokens,
        session_id,
        tenant_id: None, // OTLP 的 tenant 透传作后续（标准格式扩展属性映射）
        agent_name,
        tool_name,
        model,
        input_text,
        output_text,
        logs: if name.is_empty() { Vec::new() } else { vec![name] },
    });
    // SpanEnd：状态 + 耗时。
    out.push(WireRecord {
        trace_id,
        span_id,
        ts: ts_end,
        seq: 2,
        event_type_tag: EventType::SpanEnd.tag(),
        ext_span_id: span_hex.to_string(),
        parent_span_id,
        status,
        duration_ns: Some(duration_ns),
        input_tokens: None,
        output_tokens: None,
        session_id: None,
        tenant_id: None,
        agent_name: None,
        tool_name: None,
        model: None,
        input_text: None,
        output_text: None,
        logs: Vec::new(),
    });
    Ok(())
}

// ───────────────────────── 小工具 ─────────────────────────

/// 取 camelCase 或 snake_case 两种 key 之一（OTLP/JSON 多为 camelCase，部分库用 snake）。
fn get2<'a>(obj: &'a Json, camel: &str, snake: &str) -> Option<&'a Json> {
    field(obj, camel).or_else(|| field(obj, snake))
}

/// 取低 64 位：trace id 是 128 位（32 hex），取末 16 hex；span id 本就 64 位。非法 hex → 0。
fn hex_low_u64(hex: &str) -> u64 {
    let s = hex.trim();
    let start = s.len().saturating_sub(16);
    u64::from_str_radix(&s[start..], 16).unwrap_or(0)
}

/// OTLP 属性值取字符串：`{"stringValue":"..."}`。
fn val_str(v: &Json) -> Option<String> {
    v.get("stringValue").and_then(Json::as_str).map(|s| s.to_string())
}

/// OTLP 属性值取整数：`{"intValue":"123"}`（OTLP/JSON 里 int64 编码成字符串）或 `{"doubleValue":1.2}`。
fn val_u64(v: &Json) -> Option<u64> {
    v.get("intValue")
        .and_then(Json::as_u64)
        .or_else(|| v.get("doubleValue").and_then(Json::as_u64))
}

/// 按 key 找一条属性的 value 对象。
fn attr<'a>(attrs: &'a [Json], key: &str) -> Option<&'a Json> {
    attrs
        .iter()
        .find(|a| a.get("key").and_then(Json::as_str) == Some(key))
        .and_then(|a| a.get("value"))
}

/// 多个候选 key 取第一个命中的字符串值（GenAI / OpenInference 两套约定的别名表）。
fn first_str(attrs: &[Json], keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| attr(attrs, k).and_then(val_str))
}

/// 多个候选 key 取第一个命中的整数值。
fn first_u64(attrs: &[Json], keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|k| attr(attrs, k).and_then(val_u64))
}

/// 字符串会话 id → u64：纯数字直接解析，否则用 yt-core 的确定性 FNV-1a 64（不再自己抄一份哈希常量）。
fn str_to_u64(s: &str) -> u64 {
    s.parse::<u64>().unwrap_or_else(|_| yt_core::event::fnv1a64(s.as_bytes()))
}

/// 把"可能是 JSON 消息数组"的文本拍平成纯文本。GenAI 的 gen_ai.prompt/completion 常是
/// `[{"role":"user","content":"…"}]`（content 也可能是 `[{"type":"text","text":"…"}]` 多模态分片）。
/// 不是 JSON（OpenInference 扁平串）或解析失败 → 原样返回，绝不丢原文。
fn flatten_messages(s: &str) -> String {
    let t = s.trim_start();
    if !t.starts_with('[') && !t.starts_with('{') {
        return s.to_string(); // 扁平串,原样
    }
    let Ok(j) = parse(s) else { return s.to_string() };
    let arr = j.as_array();
    // 数组 → 逐条消息；单对象 → 当一条消息。
    let msgs: &[Json] = if arr.is_empty() { std::slice::from_ref(&j) } else { arr };
    let mut texts: Vec<String> = Vec::new();
    for m in msgs {
        let Some(c) = m.get("content") else { continue };
        if let Some(flat) = c.as_str() {
            texts.push(flat.to_string());
        } else {
            // content 是多模态分片数组 [{type:text, text:"…"}]
            for part in c.as_array() {
                if let Some(x) = part.get("text").and_then(Json::as_str) {
                    texts.push(x.to_string());
                }
            }
        }
    }
    if texts.is_empty() {
        s.to_string() // 没抽到 content,别丢原文
    } else {
        texts.join("\n")
    }
}

/// 从 span 的 `events[]` 里捞输入/输出文本（新版 GenAI 约定把内容放事件里，不在 span 属性上）。
/// 事件名含 completion/assistant/choice → 输出；含 prompt/user/system/message → 输入。
/// 内容从事件属性的多种 key 兜底取，再 `flatten_messages` 拍平。
fn texts_from_events(sp: &Json) -> (Option<String>, Option<String>) {
    let events = sp.get("events").map(Json::as_array).unwrap_or(&[]);
    let mut inp: Vec<String> = Vec::new();
    let mut out: Vec<String> = Vec::new();
    for ev in events {
        let attrs = ev.get("attributes").map(Json::as_array).unwrap_or(&[]);
        let Some(body) =
            first_str(attrs, &["gen_ai.prompt", "gen_ai.completion", "gen_ai.event.content", "content", "message"])
        else {
            continue;
        };
        let text = flatten_messages(&body);
        let n = ev.get("name").and_then(Json::as_str).unwrap_or("").to_ascii_lowercase();
        if n.contains("completion") || n.contains("assistant") || n.contains("choice") {
            out.push(text);
        } else if n.contains("prompt") || n.contains("user") || n.contains("system") || n.contains("message") {
            inp.push(text);
        }
    }
    let join = |v: Vec<String>| if v.is_empty() { None } else { Some(v.join("\n")) };
    (join(inp), join(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    // 一条 OpenTelemetry GenAI 风格的 OTLP/HTTP JSON 导出（含中文、大整数 token、错误状态）。
    const GENAI: &str = r#"{
      "resourceSpans": [{
        "resource": {"attributes": [{"key":"service.name","value":{"stringValue":"agent-svc"}}]},
        "scopeSpans": [{
          "spans": [{
            "traceId": "5b8efff798038103d269b633813fc60c",
            "spanId": "eee19b7ec3c1b174",
            "parentSpanId": "",
            "name": "chat qwen3",
            "startTimeUnixNano": "1700000000000000000",
            "endTimeUnixNano": "1700000000500000000",
            "status": {"code": 2, "message": "boom"},
            "attributes": [
              {"key":"gen_ai.request.model","value":{"stringValue":"qwen3"}},
              {"key":"gen_ai.usage.input_tokens","value":{"intValue":"1200"}},
              {"key":"gen_ai.usage.output_tokens","value":{"intValue":"340"}},
              {"key":"gen_ai.agent.name","value":{"stringValue":"风控研判"}},
              {"key":"session.id","value":{"stringValue":"会话-7"}}
            ]
          }]
        }]
      }]
    }"#;

    #[test]
    fn maps_genai_otlp_span_to_two_events() {
        let recs = parse_otlp_traces(GENAI).unwrap();
        assert_eq!(recs.len(), 2, "一条 OTLP span → SpanStart + SpanEnd 两个事件");

        let start = &recs[0];
        let end = &recs[1];
        assert_eq!(start.event_type_tag, EventType::SpanStart.tag());
        assert_eq!(end.event_type_tag, EventType::SpanEnd.tag());
        // 两事件共享 ext_span_id（确定性 event_id 的身份），seq 区分
        assert_eq!(start.ext_span_id, "eee19b7ec3c1b174");
        assert_eq!(start.ext_span_id, end.ext_span_id);
        assert_eq!((start.seq, end.seq), (1, 2));
        // span_id 低 64 位
        assert_eq!(start.span_id, 0xeee1_9b7e_c3c1_b174);
        assert_eq!(start.trace_id, hex_low_u64("5b8efff798038103d269b633813fc60c"));
        assert_eq!(start.parent_span_id, None, "空 parentSpanId → 根");
        // GenAI 属性映射
        assert_eq!(start.model.as_deref(), Some("qwen3"));
        assert_eq!(start.input_tokens, Some(1200));
        assert_eq!(start.output_tokens, Some(340));
        assert_eq!(start.agent_name.as_deref(), Some("风控研判"));
        assert!(start.session_id.is_some(), "字符串会话 id 哈希成 u64");
        assert_eq!(start.logs, vec!["chat qwen3"], "span 名进 logs");
        // 状态/耗时在 end 上
        assert_eq!(end.status, Some(1), "OTLP Error(2) → 本引擎 status=1");
        assert_eq!(end.duration_ns, Some(500_000_000), "end-start 纳秒");
    }

    #[test]
    fn maps_openinference_aliases() {
        // OpenInference（Arize 系）用另一套 key：llm.model_name / llm.token_count.* / input.value。
        let oi = r#"{"resourceSpans":[{"scopeSpans":[{"spans":[{
            "traceId":"00000000000000000000000000000abc",
            "spanId":"0000000000000abc",
            "name":"llm",
            "startTimeUnixNano":"100","endTimeUnixNano":"160",
            "status":{"code":1},
            "attributes":[
              {"key":"llm.model_name","value":{"stringValue":"gpt-4"}},
              {"key":"llm.token_count.prompt","value":{"intValue":"900"}},
              {"key":"llm.token_count.completion","value":{"intValue":"150"}},
              {"key":"input.value","value":{"stringValue":"请研判"}},
              {"key":"output.value","value":{"stringValue":"疑似盗刷"}}
            ]
        }]}]}]}"#;
        let recs = parse_otlp_traces(oi).unwrap();
        let start = &recs[0];
        assert_eq!(start.model.as_deref(), Some("gpt-4"));
        assert_eq!(start.input_tokens, Some(900));
        assert_eq!(start.output_tokens, Some(150));
        assert_eq!(start.input_text.as_deref(), Some("请研判"), "OpenInference input.value → input_text");
        assert_eq!(start.output_text.as_deref(), Some("疑似盗刷"));
        assert_eq!(recs[1].status, Some(0), "OTLP Ok(1) → status=0");
        assert_eq!(recs[1].duration_ns, Some(60));
    }

    #[test]
    fn parent_child_hex_ids_survive() {
        let j = r#"{"resourceSpans":[{"scopeSpans":[{"spans":[{
            "traceId":"abc","spanId":"00000000000000aa","parentSpanId":"00000000000000bb",
            "name":"child","startTimeUnixNano":"1","endTimeUnixNano":"2","attributes":[]
        }]}]}]}"#;
        let recs = parse_otlp_traces(j).unwrap();
        assert_eq!(recs[0].span_id, 0xaa);
        assert_eq!(recs[0].parent_span_id, Some(0xbb), "父 span hex → u64");
    }

    #[test]
    fn rejects_non_otlp() {
        assert!(parse_otlp_traces("not json").is_err());
        assert!(parse_otlp_traces(r#"{"foo":1}"#).is_err(), "缺 resourceSpans 应报错");
    }

    #[test]
    fn genai_prompt_completion_json_arrays_are_flattened() {
        // GenAI 的 gen_ai.prompt/completion 是 JSON 消息数组串 → 拍平成纯文本（不是存原始 JSON）。
        let j = r#"{"resourceSpans":[{"scopeSpans":[{"spans":[{
            "traceId":"abc","spanId":"00000000000000aa",
            "name":"chat","startTimeUnixNano":"1","endTimeUnixNano":"2",
            "attributes":[
              {"key":"gen_ai.prompt","value":{"stringValue":"[{\"role\":\"system\",\"content\":\"你是风控助手\"},{\"role\":\"user\",\"content\":\"这笔交易可疑吗\"}]"}},
              {"key":"gen_ai.completion","value":{"stringValue":"[{\"role\":\"assistant\",\"content\":\"疑似盗刷,建议拦截\"}]"}}
            ]
        }]}]}]}"#;
        let recs = parse_otlp_traces(j).unwrap();
        let start = &recs[0];
        assert_eq!(
            start.input_text.as_deref(),
            Some("你是风控助手\n这笔交易可疑吗"),
            "消息数组的 content 被抽出拍平,不是存 JSON 串"
        );
        assert_eq!(start.output_text.as_deref(), Some("疑似盗刷,建议拦截"));
    }

    #[test]
    fn genai_multimodal_content_parts_are_flattened() {
        // content 是多模态分片数组 [{type:text,text:"…"}] → 取出 text 拼接。
        let j = r#"{"resourceSpans":[{"scopeSpans":[{"spans":[{
            "traceId":"abc","spanId":"00000000000000aa",
            "name":"chat","startTimeUnixNano":"1","endTimeUnixNano":"2",
            "attributes":[
              {"key":"gen_ai.prompt","value":{"stringValue":"[{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"看这张图\"},{\"type\":\"image\",\"url\":\"x\"}]}]"}}
            ]
        }]}]}]}"#;
        let recs = parse_otlp_traces(j).unwrap();
        assert_eq!(recs[0].input_text.as_deref(), Some("看这张图"), "多模态分片只取 text 部分");
    }

    #[test]
    fn genai_content_from_span_events() {
        // 新版约定:内容在 span events[] 里,不在属性上 → 从事件捞 + 按事件名分输入/输出。
        let j = r#"{"resourceSpans":[{"scopeSpans":[{"spans":[{
            "traceId":"abc","spanId":"00000000000000aa",
            "name":"chat","startTimeUnixNano":"1","endTimeUnixNano":"2",
            "attributes":[],
            "events":[
              {"name":"gen_ai.user.message","attributes":[{"key":"content","value":{"stringValue":"这笔交易可疑吗"}}]},
              {"name":"gen_ai.choice","attributes":[{"key":"content","value":{"stringValue":"疑似盗刷"}}]}
            ]
        }]}]}]}"#;
        let recs = parse_otlp_traces(j).unwrap();
        assert_eq!(recs[0].input_text.as_deref(), Some("这笔交易可疑吗"), "user.message 事件 → 输入");
        assert_eq!(recs[0].output_text.as_deref(), Some("疑似盗刷"), "choice 事件 → 输出");
    }

    #[test]
    fn flatten_messages_leaves_flat_text_untouched() {
        // OpenInference 的扁平串不受影响(不是 JSON → 原样)。
        assert_eq!(flatten_messages("请研判这笔交易"), "请研判这笔交易");
        assert_eq!(flatten_messages("  not json [oops"), "  not json [oops");
    }

    #[test]
    fn session_string_hash_is_deterministic() {
        assert_eq!(str_to_u64("会话-7"), str_to_u64("会话-7"));
        assert_ne!(str_to_u64("会话-7"), str_to_u64("会话-8"));
        assert_eq!(str_to_u64("12345"), 12345, "纯数字直接解析");
    }
}
