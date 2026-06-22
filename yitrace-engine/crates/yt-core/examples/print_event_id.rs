//! 打印几个已知输入的 event_id，作为 SDK 跨语言一致性的基准。
//! `cargo run -p yt-core --example print_event_id`
use yt_core::event::{EventIdentity, EventType};

fn main() {
    let cases = [
        ("demo-span", 7u64, EventType::SpanEnd),
        ("1002-1", 1, EventType::SpanStart),
        ("反洗钱-1", 3, EventType::Attr),
    ];
    for (ext, seq, et) in cases {
        let id = EventIdentity { ext_span_id: ext.to_string(), seq, event_type: et }.event_id();
        println!("{ext}|{seq}|{et:?} -> {}", id.0);
    }
}
