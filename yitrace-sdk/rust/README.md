# yitrace SDK for Rust

Pure Rust instrumentation SDK for yiTrace agents.

Use this crate when a Rust app only needs to emit trace events to a running
yiTrace server. Use `yitrace-db` when the app needs an embedded local TraceDB.

## Install

Local development from this repository. The crate is not published yet, so use a
path dependency until the release is cut:

```toml
[dependencies]
yitrace = { path = "../yitrace-sdk/rust" }
```

The crate is std-only and has no runtime dependencies. `HttpExporter` is the
small built-in transport for local/dev `http://` endpoints. For TLS or stricter
production transport, implement the small `Exporter` trait and send `SpanEvent`
batches with your HTTP stack.

## Usage

```rust
use yitrace::{HttpExporter, SpanOptions, TraceOptions, Tracer};

fn main() -> yitrace::Result<()> {
    let exporter = HttpExporter::new("http://127.0.0.1:7878/v1/ingest")?
        .with_tenant_id(1);
    let mut tracer = Tracer::with_exporter(exporter, 1);

    tracer.trace_with_result(
        "risk review",
        TraceOptions::default()
            .session_id(9000)
            .tenant_id(1)
            .agent_name("risk-agent"),
        |trace| {
            trace.span_with_result(
                "llm.check",
                SpanOptions::default().display_name("LLM 风险检查"),
                |span| {
                    span.set_model("gpt-5");
                    span.set_io(Some("疑似盗刷订单".to_string()), None);
                    span.log("疑似盗刷")?;
                    span.set_tokens(Some(900), Some(120));
                    span.set_io(None, Some("建议人工复核".to_string()));
                    Ok(())
                },
            )
        },
    )?;

    tracer.close()
}
```

普通用法只需要 `span_result(name, ...)`。只有技术名不适合直接展示时才使用
`SpanOptions::display_name(...)`；`TraceOptions::agent_name(...)` 作为 trace 上下文，
会由其中的子 span 自动继承。展示名不参与检索、过滤或节点合并。

## What It Guarantees

- `event_id = hash(ext_span_id, seq, event_type)` matches the Rust engine,
  Python SDK, and TypeScript SDK byte-for-byte.
- `Tracer` emits `SpanStart`, `Log`, and `SpanEnd` events with monotonic per-span
  `seq`.
- Nested spans automatically set `parent_span_id` and inherit `session_id` /
  `tenant_id`.
- `trace_result` / `span_result` let normal Rust errors bubble up. When a span
  closure returns an error or panics, the SDK still emits `SpanEnd` with
  `status=1`.
- `HttpExporter` uses at-least-once delivery. If a POST fails, the batch is put
  back into the retry buffer. The engine de-duplicates by deterministic
  `event_id`.

## Test

```bash
cargo test --offline
```

From the repository root, run the package-mode eval when changing package
contracts:

```bash
./scripts/package_mode_eval.sh
```
