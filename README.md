# yiTrace

**Trace SDK and run replay for AI agents.**

Add the SDK, send agent runs to a local yiTrace collector, and replay every
multi-turn conversation, tool call, token cost, and failure without sending data
to a hosted observability service.

[中文](README.zh-CN.md) · English

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.80%2B-orange?logo=rust)](https://www.rust-lang.org/)
[![status](https://img.shields.io/badge/status-alpha-3fb950)](#project-status)
[![engine](https://img.shields.io/badge/engine-std--only%20zero--dep-4b7fd1)](#how-it-works)
[![OTLP](https://img.shields.io/badge/ingest-OTLP%20%2F%20OpenInference-7c3aed)](#start-with-the-trace-sdk)

Open the bundled console to replay multi-turn sessions, inspect spans, search
Chinese trace text, and drill into model/tool calls.

![yiTrace console](docs/images/console-overview.png)

yiTrace is for teams building agents that want tracing to feel like adding an
SDK, not adopting a database:

- add Python or TypeScript tracing with a few lines of code
- replay multi-turn conversations, tool calls, and multi-agent handoffs
- search Chinese trace text with BM25, then blend it with vector recall
- filter search by tenant, agent, status, trace, and time
- attribute token cost per trace, session, and agent
- collect failed spans into eval datasets and track regressions
- keep the storage engine local and hidden until you need it

> Status: alpha, runnable today. Storage, WAL recovery, OTLP ingest, SDKs,
> Chinese search, vector recall, and evals are covered by offline tests.
> RBAC/TLS/hosted deployment are roadmap items.

---

## Quick Start

Requires Rust 1.80+.

One-command local demo:

```bash
./scripts/demo_all.sh
```

This builds the console, starts the engine, ingests a sample trace, and prints
ready-to-run search commands. Set `YT_DEMO_OPEN=1` to open the console
automatically.

Docker:

```bash
docker compose up --build
```

Then open `http://127.0.0.1:7878`.

Manual path:

```bash
cd yitrace-engine
cargo run -p yt-engine --example server
```

The server listens on `http://127.0.0.1:7878` and seeds demo eval data.

In another terminal:

```bash
curl -XPOST localhost:7878/v1/ingest \
  -H 'Content-Type: application/json' \
  -d '[
    {"trace_id":7,"span_id":1,"ts":1,"seq":1,"event_type":1,"ext_span_id":"7-1","agent_name":"risk","input_text":"possible card fraud","logs":["start"]},
    {"trace_id":7,"span_id":1,"ts":2,"seq":2,"event_type":2,"ext_span_id":"7-1","status":0,"duration_ns":4200000,"output_text":"needs review","logs":["done"]}
  ]'

curl localhost:7878/v1/traces

curl -XPOST localhost:7878/v1/search \
  -H 'Content-Type: application/json' \
  -d '{"text":"fraud","k":10}'
```

For Chinese search:

```bash
curl -XPOST localhost:7878/v1/search \
  -H 'Content-Type: application/json' \
  -d '{"text":"盗刷","k":10,"filter":{"agent_name":"风控","status":1}}'
```

Optional auth:

```bash
YT_TOKEN=secret cargo run -p yt-engine --example server

curl localhost:7878/v1/traces \
  -H 'Authorization: Bearer secret' \
  -H 'X-Tenant-Id: 1'
```

## Start With The Trace SDK

You do not need to adopt yiTrace as a database to begin. Run the local collector
or server, add the SDK, and treat yiTrace like a private flight recorder for
agent runs.

Python:

```python
from yitrace import Tracer, HttpExporter

tracer = Tracer(
    exporter=HttpExporter(
        "http://127.0.0.1:7878/v1/ingest",
        tenant_id=1,
    ),
    node_id=1,
)

with tracer.trace("AML screening", tenant_id=1) as t:
    with t.span("risk agent") as span:
        span.log("possible card fraud")
        span.set_tokens(input_tokens=900, output_tokens=120)

tracer.close()
```

TypeScript:

```ts
import { HttpExporter, Tracer } from "@yitrace/trace-sdk";

const tracer = new Tracer(
  new HttpExporter({
    url: "http://127.0.0.1:7878/v1/ingest",
    tenantId: 1,
  }),
  1,
);

tracer.trace("AML screening", (t) => {
  t.span("risk agent", (span) => {
    span.log("possible card fraud");
    span.setTokens(900, 120);
  });
}, undefined, 1);

await tracer.close();
```

Already have OpenTelemetry or OpenInference spans? POST OTLP/HTTP JSON to
`/v1/traces`. yiTrace maps OTel GenAI `gen_ai.*` and OpenInference `llm.*`
attributes into the same trace store.

## Console

The engine can serve the React console as embedded static assets. From source,
build it once and copy it into the engine crate before starting the server:

```bash
cd yitrace-console
npm install
VITE_API=http npm run build
rm -rf ../yitrace-engine/crates/yt-engine/console_dist
cp -r dist ../yitrace-engine/crates/yt-engine/console_dist

cd ../yitrace-engine
cargo run -p yt-engine --example server
```

Then open `http://127.0.0.1:7878/`.

The console has no private API. It talks to the same `/v1/*` JSON endpoints as
any other UI. See [HTTP API Reference](docs/API_REFERENCE.md).

---

## Advanced: Embedded Node / Electron

For Node backends and Electron apps that want local persistence without running
a separate process, yiTrace can also run in-process. This is the advanced path:
it loads the Rust engine through Node-API and uses the same WAL, manifest,
folding, search, and tenant filtering code as the server.

```bash
npm install @yitrace/db
```

```ts
import { YiTraceDB, createSpanEventBuilder } from "@yitrace/db";

const db = await YiTraceDB.open({ dataDir: "./data", tenantId: 1 });

const events = createSpanEventBuilder({
  traceId: "run-uuid",
  sessionId: "session-uuid",
  attrs: {
    project_id: "agentic-data",
    skill: "review",
    mode: "auto",
    call_site: "worker.ts:10",
  },
});

events.startSpan({ spanId: "span-uuid", name: "risk review", agentName: "risk-agent" });
events.log({ spanId: "span-uuid", message: "possible card fraud" });
events.endSpan({ spanId: "span-uuid", status: 0, durationNs: 12_000_000 });

await events.ingest(db);

const hits = await db.search({
  text: "fraud",
  k: 10,
  filter: { attrs: { project_id: "agentic-data", skill: "review" } },
});
const trace = await db.trace("run-uuid");
const logEvents = trace?.spans?.flatMap((span) => span.logEvents ?? []) ?? [];
await db.close();
```

The package lives in `yitrace-node/` while it is being stabilized. Electron
apps should open `YiTraceDB` in the main process and expose narrow IPC methods
to renderers. Both ESM `import` and CommonJS `require` are supported.
Internally this path calls the engine's in-process `EngineJsonApi`; it does not
start a local HTTP server or use TCP sockets.
Direct ingest accepts numeric IDs or external string IDs such as UUIDs. String
IDs are hashed into stable internal `u64` keys for indexing while the original
values are returned as `external_*` fields. `attrs` is persisted and returned on
search, trace, and span detail responses. Exact attrs filtering is supported for
`project_id`, `skill`, `mode`, and `call_site`. `OpenOptions.readOnly` is not
exposed until the engine has a true read-only open path. Trace and span detail
responses also return `logEvents`, so apps can render span-level process logs
without copying them into `attrs.event_logs`.

`@yitrace/db` is distributed as a small JavaScript entry package plus optional
native platform packages (`@yitrace/db-darwin-arm64`,
`@yitrace/db-linux-x64-gnu`, and so on). This keeps the user-facing install to a
single command while avoiding a Rust toolchain requirement on the user's
machine. Maintainer packaging and publish steps are documented in
[`yitrace-node/README.md`](yitrace-node/README.md).
Before the public npm release, teams can use `npm run pack:verify` in
`yitrace-node/` to produce immutable commit-labeled tarballs and lock those
tarballs in a consumer repository or internal npm registry.

---

## Why yiTrace

Most tracing SDKs stop at export, and most databases feel too heavy for first
contact. yiTrace keeps the first step SDK-shaped, then gives you private replay,
search, cost attribution, and eval data behind it.

| You need | Use |
|---|---|
| Hosted tracing, prompt runs, team workflows | LangSmith / Langfuse |
| OpenTelemetry routing, metrics, and pipeline glue | OpenTelemetry Collector |
| SQL analytics over large general-purpose event tables | ClickHouse / DuckDB |
| SDK-first private agent run replay with Chinese search, vector recall, and evals | yiTrace |

What is different:

- **SDK-first adoption**: start with `@yitrace/trace-sdk` or the Python SDK; use
  the embedded DB only when you need in-process local persistence.
- **Private by default**: one local process, one data directory, no external services.
- **Agent-native records**: multi-turn sessions, span trees, tools, models, tokens, eval scores.
- **Retry-safe ingest**: deterministic `event_id = hash(ext_span_id, seq, event_type)` across Rust, Python, and TypeScript.
- **Search built in**: Chinese BM25, filtered vector recall, and hybrid RRF.
- **Tenant-aware API**: tenant comes from `X-Tenant-Id`, not from untrusted request bodies.

---

## How It Works

```text
SDKs / OTLP
    |
    v
HTTP ingest gateway
    |
    v
WAL + memtable --flush--> immutable segments
    |                         |
    v                         v
BM25 / vector / attr indexes  read-time fold
    |                         |
    +---------- search / replay / cost / eval
```

Three mechanisms carry the design:

- **Events, not mutable spans**: a span is written as `SpanStart`, `SpanEnd`, logs,
  and late attribute updates. Readers fold events into one complete span.
- **Content-derived identity**: event identity is deterministic, so retransmit and
  crash replay do not double-count tokens or cost.
- **Four-source fold**: a snapshot merges memtable, immutable segments, delete
  bitmaps, and late-write blocks by `event_id`.

The engine body is std-only Rust. Heavier integrations, such as Vortex columnar
segments, jieba FFI, and external graph indexes, live in separate crates behind
traits.

---

## Project Status

| Area | Status | Notes |
|---|---|---|
| Storage, WAL, snapshots, restart recovery | Done, tested | `cargo test --offline` covers crash replay, compaction, GC, backup, and restart |
| HTTP API and OTLP/OpenInference ingest | Done | `/v1/ingest`, `/v1/traces`, `/v1/search`, `/v1/sessions`, `/v1/metrics` |
| Python and TypeScript SDKs | Done | deterministic event id parity with the Rust engine |
| Chinese tokenizer and BM25 | Done in pure Rust | dictionary DAG + max-probability DP, embedded jieba dictionary, user dict support |
| Vector recall | Done in engine | disk-backed multi-layer HNSW, filtered search, L2/Cosine/IP |
| Console | Usable | React app can be embedded into the engine binary |
| Eval loop | Alpha | rule scorer today, LLM judge is roadmap |
| Production security | Roadmap | TLS, RBAC, encryption, rate limits, persistent audit logs |
| Query engine | Roadmap | hand-written query paths today, DataFusion integration pending |

Run the verification suite:

```bash
cd yitrace-engine
cargo test --offline
```

Optional crates:

```bash
cd yitrace-segstore-vortex && cargo build      # Vortex columnar segment store
cd yitrace-tokenizer-jieba && cargo test       # jieba FFI wrapper, mock by default
cd yitrace-vecindex-graph && cargo test        # graph_index FFI wrapper, mock by default
```

---

## Repository Layout

```text
yitrace-engine/              # Rust engine workspace, std-only core
  crates/
    yt-core                  # ids, event_id, fold, manifest types
    yt-manifest              # reader pin protocol and reclamation watermark
    yt-wal                   # crash-safe WAL frames
    yt-memtable              # live rows and gated eviction
    yt-engine                # coordinator, search, eval, HTTP, OTLP, console assets
yitrace-console/             # React console
yitrace-sdk/
  python/                    # Python tracing SDK
  typescript/                # TypeScript tracing SDK
yitrace-segstore-vortex/     # optional Vortex segment store
yitrace-tokenizer-jieba/     # optional jieba FFI tokenizer
yitrace-vecindex-graph/      # optional graph_index FFI vector index
docs/                        # design notes, API reference, current-state index
```

Start with [Current State](docs/CURRENT_STATE.md) if you want the engineering
truth, including what is verified, what is alpha, and what is still roadmap.

## License

MIT
