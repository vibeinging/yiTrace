//! 可运行 demo：`cargo run -p yt-engine --example demo`
//!
//! 灌几条银行风控域的假 trace，跑一遍：写入 → 折叠读出一条完整 trace → 按「盗刷」中文检索
//! → 向量找相似 → 关键词+语义混合召回。把结果打印出来，让这套引擎从「测试里能跑」变成「跑给人看」。

use std::sync::Arc;

use yt_core::event::{EventIdentity, EventType};
use yt_core::fold::{FoldedSpan, SpanFields};
use yt_engine::{InMemorySegmentStore, TraceQuery, WriteCoordinator};
use yt_wal::WalRecord;

/// 造一个事件。`et` 区分同一 span 的不同上报（start/end/attr），seq 定先后。
fn ev(trace: u64, span: u64, seq: u64, ts: i64, et: EventType, status: Option<u8>, dur: Option<u64>, logs: &[&str]) -> WalRecord {
    WalRecord {
        trace_id: trace,
        span_id: span,
        ts,
        identity: EventIdentity { ext_span_id: format!("{trace}-{span}"), seq, event_type: et },
        fields: SpanFields {
            status,
            duration_ns: dur,
            logs: logs.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        },
    }
}

fn print_spans(title: &str, spans: &[FoldedSpan]) {
    println!("  {title}");
    for s in spans {
        println!(
            "    trace={} span={} status={:?} 耗时={:?}ns 日志={:?} (折叠了{}个事件)",
            s.trace_id, s.span_id, s.status, s.duration_ns, s.logs, s.event_count
        );
    }
}

fn print_hits(title: &str, hits: &[(FoldedSpan, f32)]) {
    println!("  {title}");
    for (s, score) in hits {
        println!("    [分{score:.4}] trace={} span={} 日志={:?}", s.trace_id, s.span_id, s.logs);
    }
}

fn main() {
    let wc = WriteCoordinator::new(Arc::new(InMemorySegmentStore::default()));

    // 三条 trace。每个 span 拆成 start（给 status）和 end（给耗时+日志）两个事件，模拟真实上报。
    let events = vec![
        // trace 1001：反洗钱筛查
        ev(1001, 1, 1, 100, EventType::SpanStart, Some(0), None, &["反洗钱筛查 开始"]),
        ev(1001, 1, 2, 180, EventType::SpanEnd, None, Some(80), &["命中规则 大额可疑 已上报"]),
        ev(1001, 2, 1, 110, EventType::SpanStart, Some(0), None, &["调用 LLM 研判"]),
        ev(1001, 2, 2, 160, EventType::SpanEnd, None, Some(50), &["研判结论 需人工复核"]),
        // trace 1002：疑似盗刷拦截
        ev(1002, 1, 1, 200, EventType::SpanStart, Some(0), None, &["交易风控 开始"]),
        ev(1002, 1, 2, 240, EventType::SpanEnd, None, Some(40), &["疑似盗刷 异地登录+大额", "已拦截"]),
        ev(1002, 2, 1, 210, EventType::SpanStart, Some(1), None, &["短信验证 失败"]),
        ev(1002, 2, 2, 230, EventType::SpanEnd, None, Some(20), &["二次验证未通过 冻结交易"]),
        // trace 1003：转账合规检查
        ev(1003, 1, 1, 300, EventType::SpanStart, Some(0), None, &["转账合规检查 开始"]),
        ev(1003, 1, 2, 350, EventType::SpanEnd, None, Some(50), &["合规通过 转账成功"]),
    ];

    // 给 LLM span 标上 token 用量（agent 可观测性的成本核心）。
    let mut events = events;
    events[3].fields.input_tokens = Some(1200); // 1001 反洗钱研判 LLM
    events[3].fields.output_tokens = Some(340);
    events[5].fields.input_tokens = Some(800); // 1002 盗刷风控 LLM
    events[5].fields.output_tokens = Some(150);

    let lsn = wc.ingest(events.clone());
    wc.commit_flush(&events, lsn); // 全部落段

    // 给几个 span 配二维向量（真实是 embedding；这里手造便于演示「找相似」）。
    wc.index_embedding(1001, 1, vec![0.0, 1.0]); // 反洗钱
    wc.index_embedding(1002, 1, vec![1.0, 0.0]); // 盗刷
    wc.index_embedding(1002, 2, vec![0.9, 0.1]); // 盗刷-验证
    wc.index_embedding(1003, 1, vec![5.0, 5.0]); // 转账（离得远）

    let snap = wc.pin_snapshot();

    println!("== 1) 读一条完整 trace（1002 疑似盗刷），按 trace 剪枝 ==");
    let (spans, scanned) = wc.read_spans_query(&snap, &TraceQuery::trace(1002, i64::MIN, i64::MAX));
    println!("  （扫了 {scanned} 个段）");
    print_spans("折叠出的 span：", &spans);

    println!("\n== 2) 按中文内容搜「盗刷」==");
    print_hits("命中：", &wc.search_text(&snap, "盗刷", 5));

    println!("\n== 3) 向量找相似（查接近 [1,0] 的 span）==");
    print_hits("最相似：", &wc.search_similar(&snap, &[1.0, 0.0], 3));

    println!("\n== 4) 混合召回：「盗刷」关键词 + 向量 [0.0,1.0]（语义偏反洗钱）==");
    println!("  （关键词指向盗刷、向量指向反洗钱，看 RRF 怎么融）");
    print_hits("融合排序：", &wc.search_hybrid(&snap, "盗刷", &[0.0, 1.0], 5));

    println!("\n== 5) trace 列表（控制台主视图）==");
    for t in wc.list_traces(&snap, &TraceQuery::all()) {
        println!(
            "    trace={} span数={} 总耗时={}ns 报错={} token(入/出)={}/{}",
            t.trace_id, t.span_count, t.total_duration_ns, t.error_count, t.total_input_tokens, t.total_output_tokens
        );
    }
}
