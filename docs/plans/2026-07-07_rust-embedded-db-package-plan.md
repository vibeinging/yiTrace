# Rust Embedded DB Package Plan

## Goal

Provide a Rust-facing embedded package for yiTrace, matching the Node and Python
embedded DB boundary:

- open a data directory in process
- call `EngineJsonApi` directly
- avoid starting a local HTTP server
- avoid exposing `WriteCoordinator` as the user-facing API
- keep the engine crate dependency graph unchanged

## Package Shape

- Directory: `yitrace-db-rs/`
- Crate name: `yitrace-db`
- Public API:
  - `YiTraceDb`
  - `OpenOptions`
  - `SpanEventBuilder`
  - `SearchQuery`
  - `route_json()` fallback for all current and future engine APIs

## Initial Scope

- Single-writer data-dir lock with `.yitrace.lock`
- Durable engine open + recover
- Builder for start/log/end span wire events
- Search helper with text, vector, k, agent/status/attrs filters
- Trace/session/detail helpers
- Integration tests for ingest/search/detail, writer lock, closed/request errors
- Eval-style tests for external string IDs, attrs round-trip, tenant isolation,
  route_json fallback, traceAggregate, and reopen durability

## Non-goals

- No serde dependency in the first version
- No new engine API
- No distributed client transport wrapper yet
