# yitrace-db

Embedded yiTrace DB for Rust agents.

This crate is the Rust equivalent of `@yitrace/db` and `yitrace-db` for
Python. It opens a yiTrace data directory in-process, calls the Rust engine
through `EngineJsonApi`, and does not start an HTTP server or parse storage
files in application code.

## Install

Local development from this repository:

```toml
[dependencies]
yitrace-db = { path = "../yitrace-db-rs" }
```

The crate is not published yet. A real release should publish this crate as
`yitrace-db` and keep the internal `yt-engine` API behind this wrapper.

## Usage

```rust
use yitrace_db::{OpenOptions, SearchQuery, SpanEndOptions, SpanEventBuilder, YiTraceDb};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = YiTraceDb::open_with_options(OpenOptions::new("./data").tenant_id(1))?;

    let mut events = SpanEventBuilder::new("run-uuid");
    events
        .session_id("session-uuid")
        .attr("project_id", "agentic-data")
        .attr("skill", "review")
        .start_span("span-uuid", "风控研判")
        .log("span-uuid", "疑似盗刷")
        .end_span_with(
            "span-uuid",
            SpanEndOptions::ok()
                .duration_ns(12_000_000)
                .input_tokens(900)
                .output_tokens(120),
        );

    db.ingest_builder(&events)?;

    let _rows = db.search(
        &SearchQuery::text("盗刷")
            .k(10)
            .attr("project_id", "agentic-data"),
    )?;

    Ok(())
}
```

`SpanEventBuilder` hides `seq`, `event_type`, `ext_span_id`, and start/end event
pairs. String trace/span/session IDs are accepted; the engine hashes them into
internal IDs and keeps the original external IDs in query output.

For APIs that do not have a typed helper yet, use the raw in-process JSON
boundary:

```rust
let json = db.route_json("POST", "/v1/trace-aggregate", r#"{"groupBy":["skill"]}"#)?;
```

## Test

```bash
cd yitrace-db-rs
cargo test --offline
```
