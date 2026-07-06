# yiTrace

**A local-first TraceDB for AI agents.**

yiTrace turns raw agent events into searchable evidence. Replay multi-turn runs,
inspect tool calls, search Chinese and English trace text, track token/cost/eval
signals, and export path evidence for Agent Memory. Use it in-process with
`@yitrace/db`, `yitrace-db`, or Rust `yitrace-db`, as a private server, or
behind a shard gateway.

[中文](README.zh-CN.md) · English

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.80%2B-orange?logo=rust)](https://www.rust-lang.org/)
[![status](https://img.shields.io/badge/status-alpha-3fb950)](#project-status)
[![engine](https://img.shields.io/badge/engine-std--only%20zero--dep-4b7fd1)](#how-it-works)
[![OTLP](https://img.shields.io/badge/ingest-OTLP%20%2F%20OpenInference-7c3aed)](#ingest-agent-runs)

![yiTrace console](docs/images/console-overview.png)

yiTrace is built for agent teams that need trace data to be private, queryable,
and useful after the run is over:

- keep agent traces local instead of sending prompts and tool output to a hosted service
- replay conversations, tool calls, retries, errors, and multi-agent handoffs
- search traces with BM25, attrs filters, vector recall, and trajectory similarity
- track token usage, cost, eval results, annotations, and dataset links
- mine repeated task paths and export Golden Path evidence for Agent Memory

> Status: alpha, runnable today. The storage engine, WAL recovery, SDK ingest,
> OTLP ingest, Node/Electron package, read-model indexes, retention, and
> distributed gateway primitives are covered by offline tests. This is not yet a
> managed production cluster: automatic failover, fencing, background replication
> scheduling, TLS/RBAC, and enterprise hardening are still roadmap items.

---

## Start Here

Embed yiTrace in Node.js or Electron:

```bash
npm install @yitrace/db
```

```ts
import { YiTraceDB } from "@yitrace/db";

const db = await YiTraceDB.open({ dataDir: "./data", tenantId: 1 });
const hits = await db.search({ text: "card fraud", k: 10 });
```

Embed yiTrace in Python:

```bash
pip install yitrace-db
```

```python
from yitrace_db import YiTraceDB

db = YiTraceDB.open("./data", tenant_id=1)
hits = db.search(text="card fraud", k=10)
```

Embed yiTrace in Rust:

```toml
[dependencies]
yitrace-db = { path = "yitrace-db-rs" }
```

```rust
use yitrace_db::{OpenOptions, SearchQuery, YiTraceDb};

let db = YiTraceDb::open_with_options(OpenOptions::new("./data").tenant_id(1))?;
let hits = db.search(&SearchQuery::text("card fraud").k(10))?;
```

Or run the local server and bundled console:

```bash
./scripts/demo_all.sh
```

Open `http://127.0.0.1:7878`, search for `盗刷`, and inspect the span input,
output, logs, token usage, cost, and eval evidence.

---

## Run Modes

| Mode | Use it when | What exists today |
|---|---|---|
| SDK + local server | You want private trace capture and console replay | `cargo run -p yt-engine --example server`, HTTP JSON API, embedded console |
| Durable single server | You want one private data directory with restart recovery | `server_durable`, WAL, manifest, immutable segments, disk vector index |
| Node / Electron embedded DB | You want `import { YiTraceDB } from "@yitrace/db"` and no extra process | Node-API package, ESM/CJS, optional native packages, same Rust engine in-process |
| Python embedded DB | You want `from yitrace_db import YiTraceDB` inside a Python agent | PyO3/maturin package, same `EngineJsonApi` in-process boundary |
| Rust embedded DB | You want `use yitrace_db::YiTraceDb` in a Rust agent/backend | Thin Rust crate over `EngineJsonApi`, no N-API/PyO3 layer |
| Sharded gateway | You have multiple shard servers or want the distributed path | route table, write routing, read fanout, partial/strict consistency, follower read targets |
| Replicated shard | You want leader/follower primitives | replication status, WAL export/apply, one-shot follower pull, lag/health diagnostics |

The important distinction: yiTrace is no longer just a one-process tool. The storage
core is still single-writer per shard because that is the correct failure model.
Cluster-level scale comes from routing, fanout, snapshots, and replication around
those shards.

---

## Quick Start

Requires Rust 1.80+.

One-command local demo:

```bash
./scripts/demo_all.sh
```

This builds the console, starts the engine, ingests sample traces, and prints
ready-to-run search commands. Set `YT_DEMO_OPEN=1` to open the console.

Docker:

```bash
docker compose up --build
```

Then open `http://127.0.0.1:7878`.

Manual server:

```bash
cd yitrace-engine
cargo run -p yt-engine --example server
```

The demo server listens on `http://127.0.0.1:7878` and seeds eval data.

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

Chinese search:

```bash
curl -XPOST localhost:7878/v1/search \
  -H 'Content-Type: application/json' \
  -d '{"text":"盗刷","k":10,"filter":{"agent_name":"风控","status":1}}'
```

Durable server:

```bash
cd yitrace-engine
YT_BIND=127.0.0.1:7879 cargo run -p yt-engine --example server_durable -- ./data/yitrace
```

Optional auth:

```bash
YT_TOKEN=secret cargo run -p yt-engine --example server

curl localhost:7878/v1/traces \
  -H 'Authorization: Bearer secret' \
  -H 'X-Tenant-Id: 1'
```

---

## Ingest Agent Runs

You do not need to adopt yiTrace as a database on day one. Run the collector,
add the SDK, and treat it like a private flight recorder for agent runs.

Python:

```python
from yitrace import Tracer, HttpExporter

tracer = Tracer(
    exporter=HttpExporter("http://127.0.0.1:7878/v1/ingest", tenant_id=1),
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

---

## Agent Recipes

These are the common ways agent systems use yiTrace. They are patterns, not
separate products.

### 1. Give an agent memory backed by real runs

Before planning, search previous task traces by task fingerprint, project, and
schema. After the run, store an embedding for the task or trajectory. yiTrace
does not call an embedding model; your agent or memory pipeline provides
`taskEmbedding`.

```ts
const similarTasks = await db.searchVector({
  namespace: "task",
  vector: taskEmbedding,
  k: 5,
  filter: {
    attrs: {
      project_id: "agentic-data",
      schema_fingerprint: "schema-v1",
    },
  },
});

const priorRuns = await db.traceSearch({
  filter: {
    taskFingerprint: "npm-native-packaging",
    validationStatus: "pass",
    attrs: { project_id: "agentic-data" },
  },
  limit: 20,
});

await db.indexVector({
  namespace: "task",
  key: "npm-native-packaging",
  vector: taskEmbedding,
  traceId: "builder-run-42",
  attrs: {
    project_id: "agentic-data",
    schema_fingerprint: "schema-v1",
    embedding_model: "text-embedding-3-large",
  },
});
```

The memory is grounded in trace evidence: what the agent did, which tools it
called, how much it cost, whether validation passed, and what changed later.

### 2. Debug a loop that keeps failing

When a user asks the same question many times, trace data can show which route
kept looping and which attempt finally worked.

```bash
curl -XPOST localhost:7878/v1/trajectory-groups \
  -H 'Content-Type: application/json' \
  -d '{
    "filter": {
      "taskFingerprint": "refund-risk-review",
      "attrs": { "project_id": "agentic-data" }
    },
    "sort": "best",
    "limit": 10
  }'
```

Use this for agent debugging pages: repeated failed routes, common tool
sequences, success rate, duration, token cost, and example traces.

### 3. Turn the best observed run into a Golden Path

yiTrace does not decide what is best. It stores the evidence so the product or
eval layer can decide.

```ts
const candidates = await db.trajectoryGroups({
  filter: {
    taskFingerprint: "npm-native-packaging",
    validationStatus: "pass",
    attrs: { project_id: "agentic-data" },
  },
  sort: "best",
  limit: 5,
});

const sourceTraceId = candidates.items[0]?.examples[0]?.traceId;
if (!sourceTraceId) throw new Error("no golden path candidate");

const golden = await db.createGoldenPath({
  sourceTraceId,
  taskFingerprint: "npm-native-packaging",
  score: 960,
  label: "fast packaging path",
  reason: "best observed validated route",
  source: "human-review",
  projectId: "agentic-data",
});

await db.updateGoldenPathStatus(golden.goldenPathId, {
  status: "confirmed",
  reason: "accepted for regression baseline",
  source: "reviewer",
});
```

Later runs can be compared against that path:

```ts
const adherence = await db.pathAdherence(golden.goldenPathId, "builder-run-43");
console.log(adherence.adherence, adherence.missingSteps, adherence.extraSteps);
```

### 4. Build an eval regression inbox

Use annotations and dataset links to turn failures into regression cases. The
agent team can review spans, label root causes, and rerun the same dataset after
a prompt, tool, or model change.

```ts
await db.annotate({
  traceId: "builder-run-42",
  spanId: "tool-call-7",
  label: "regression",
  source: "human",
  score: 900,
  projectId: "agentic-data",
});

await db.linkDatasetItem({
  datasetId: "release-gate",
  itemId: "case-184",
  traceId: "builder-run-42",
  spanId: "tool-call-7",
});

const regressions = await db.traceSearch({
  filter: {
    annotation: { label: "regression" },
    dataset: { datasetId: "release-gate" },
  },
});
```

### 5. Put a private trace store inside an agent desktop app

Electron apps can keep traces on the user's machine. Open `YiTraceDB` in the
main process, expose narrow IPC calls, and let the renderer show sessions,
span details, log events, and search results.

```ts
// main process
const db = await YiTraceDB.open({ dataDir: app.getPath("userData"), tenantId: 1 });

ipcMain.handle("trace-search", async (_event, query) => {
  return db.traceSearch({
    ...query,
    filter: {
      ...(query.filter ?? {}),
      attrs: { project_id: "desktop-agent" },
    },
  });
});
```

This is useful for coding agents, analyst workbenches, support copilots, and any
agent that handles data users do not want to send to a hosted observability
service.

---

## Query The TraceDB

The console has no private API. It calls the same `/v1/*` JSON endpoints that
your own UI or backend can call. See [HTTP API Reference](docs/API_REFERENCE.md)
for the full contract.

Core endpoints:

| Endpoint | What it gives you |
|---|---|
| `POST /v1/search` | BM25, vector, and hybrid retrieval with tenant/attrs filters |
| `POST /v1/trace-search` | structured span search with pagination, sort, token/cost ranges, annotations, datasets |
| `POST /v1/trace-aggregate` | group-by rollups for inbox stats, task stats, path mining |
| `POST /v1/trajectory-groups` | stable path buckets for Golden Path candidates |
| `POST /v1/trace-trajectories` | per-trace materialized trajectory summaries |
| `POST /v1/storage-stats` | storage and metadata reference estimates before retention |
| `POST /v1/retention-plan` / `POST /v1/retention/apply` | dry-run and apply soft-delete plans |
| `POST /v1/golden-paths` / `POST /v1/golden-path-health` | store path evidence and monitor adherence |
| `POST /v1/vector-index` / `POST /v1/vector-search` | task/span/trajectory vector namespace recall |

Most product APIs return an index label such as `attrs_postings`,
`segment_rollup_tail_overlay`, `metadata_sidecar+verify`, or
`folded_scan`. That is intentional. Callers can see whether the query hit a
materialized path or fell back to a slower proof path.

---

## Embedded Node / Python / Rust / Electron

For Node backends and Electron apps that want local persistence without a
separate process:

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
    task_fingerprint: "npm-native-packaging",
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
const span = await db.span("run-uuid", "span-uuid");
const logEvents = span?.logEvents ?? [];

await db.close();
```

`@yitrace/db` does not parse database files in JavaScript. It embeds the Rust
engine through Node-API and calls the in-process `EngineJsonApi`, so WAL
recovery, manifest snapshots, folding, tenant filtering, BM25, vector search,
metadata, retention, and Golden Path evidence use the same code as the server.

Python agents can use the same embedded model:

```bash
pip install yitrace-db
```

```python
from yitrace_db import YiTraceDB, create_span_event_builder

with YiTraceDB.open("./data", tenant_id=1) as db:
    events = create_span_event_builder({
        "trace_id": "run-uuid",
        "session_id": "session-uuid",
        "attrs": {"project_id": "agentic-data", "skill": "review"},
    })
    events.start_span(span_id="span-uuid", name="risk review", input_text="possible card fraud")
    events.log("possible card fraud", span_id="span-uuid")
    events.end_span(span_id="span-uuid", status=0, duration_ns=12_000_000)
    events.ingest(db)

    hits = db.search(text="fraud", k=10)
```

`yitrace-db` does not parse database files in Python either. It embeds the Rust
engine through PyO3 and calls the same `EngineJsonApi` in-process boundary as
the Node package. Use the existing `yitrace` Python package when you only need
SDK instrumentation to a running yiTrace service; use `yitrace-db` when a Python
app needs an embedded local TraceDB.

Rust agents and backends can use the same boundary without a native language
bridge:

```toml
[dependencies]
yitrace-db = { path = "yitrace-db-rs" }
```

```rust
use yitrace_db::{OpenOptions, SearchQuery, SpanEndOptions, SpanEventBuilder, YiTraceDb};

let db = YiTraceDb::open_with_options(OpenOptions::new("./data").tenant_id(1))?;

let mut events = SpanEventBuilder::new("run-uuid");
events
    .session_id("session-uuid")
    .attr("project_id", "agentic-data")
    .attr("skill", "review")
    .start_span("span-uuid", "risk review")
    .log("span-uuid", "possible card fraud")
    .end_span_with("span-uuid", SpanEndOptions::ok().duration_ns(12_000_000));

db.ingest_builder(&events)?;
let hits = db.search(&SearchQuery::text("fraud").k(10).attr("project_id", "agentic-data"))?;
```

The Rust crate is deliberately thin: common helpers are typed, and
`route_json()` remains available for every `/v1/*` API that has not yet grown a
typed wrapper.

Direct `db.ingest()` accepts numeric IDs and external string IDs such as UUIDs.
String IDs are hashed into stable internal `u64` keys for indexing while the
original values are returned as `external_*` fields. `attrs` round-trips as
JSON and high-frequency keys such as `project_id`, `skill`, `mode`,
`task_fingerprint`, `loop_id`, `validation_status`, and `eval_status` are
promoted into filterable engine fields.

Electron apps should open `YiTraceDB` in the main process and expose narrow IPC
methods to renderers. A data directory still has one writer. The package uses a
small JS root package plus optional native platform packages such as
`@yitrace/db-darwin-arm64` and `@yitrace/db-linux-x64-gnu`; maintainers should
read [yitrace-node/README.md](yitrace-node/README.md) before publishing.

---

## Distributed Path

yiTrace scales by keeping each shard simple and making the gateway explicit.

```text
SDKs / OTLP / @yitrace/db / yitrace-db clients
        |
        v
  yiTrace gateway
        |
        +--> shard A leader ---- WAL ----> shard A follower
        |
        +--> shard B leader ---- WAL ----> shard B follower
        |
        +--> shard C leader ---- WAL ----> shard C follower
```

What exists now:

- route table v1 and v2, including logical shards and replicas
- one writable replica per logical shard, rejected if a route table tries dual writers
- route table hot reload via JSON body or file path
- write routing by tenant/session/trace
- read fanout and global merge for search, traceSearch, aggregate, trajectory, storage, metadata, retention, and vector APIs
- `partial` and `strict` consistency policies
- bounded-stale read targets, with follower lag checks
- remote snapshot tokens and explicit snapshot leases with TTL
- network WAL tail export/apply and one-shot follower pull
- health, heartbeat, retry, timeout, and circuit breaker primitives

Example route table:

```json
{
  "routeTableVersion": 51,
  "shards": [
    {
      "shardId": "logical-a",
      "replicas": [
        { "replicaId": "a-primary", "addr": "127.0.0.1:7901", "role": "leader", "readable": true, "writable": true },
        { "replicaId": "a-follower", "addr": "127.0.0.1:7902", "role": "follower", "readable": true, "writable": false, "maxLagLsn": 10 }
      ]
    }
  ]
}
```

Important boundary: this is a verified distributed data path, not a full
production control plane. Still missing: background route-table watcher,
automatic failover, fencing, scheduled replication worker, snapshot bootstrap,
and remote syncing for sealed segments, sidecars, metadata, and GC logs. The
tests already start real shard and gateway processes, but deployment automation
is still yours.

---

## Why yiTrace

Most tracing systems stop at export. Most databases ask you to model agent
execution yourself. yiTrace is the middle path: an agent-native TraceDB that is
easy to start, but not trapped in a single process forever.

| You need | Use |
|---|---|
| Hosted prompt/run tracing and SaaS team workflow | LangSmith / Langfuse |
| OpenTelemetry routing, metrics, and vendor pipelines | OpenTelemetry Collector |
| General SQL analytics over huge event tables | ClickHouse / DuckDB |
| Private Agent TraceDB with replay, Chinese search, eval evidence, and shardable storage | yiTrace |

What is different:

- **Agent-native records**: sessions, spans, tools, models, logs, tokens, cost,
  eval scores, annotations, datasets, and trajectories are first-class.
- **Retry-safe ingest**: deterministic `event_id = hash(ext_span_id, seq, event_type)`
  is shared by Rust, Python, and TypeScript.
- **Search built in**: Chinese word-level BM25, field-domain search, vector
  namespaces, attrs filters, and hybrid RRF.
- **Storage governance**: retention dry-run, soft delete, compaction, audits,
  and protection for annotations, datasets, snapshots, eval links, and path memory.
- **Distributed without pretending**: shard-level single writer for correctness,
  gateway-level routing/fanout for scale, clear labels when a query is degraded.

---

## How It Works

```text
events
  |
  v
WAL + memtable ---- flush ----> immutable segments
  |                                  |
  |                                  +--> attrs postings / rollups / text domains
  |                                  +--> vector namespace records
  v
read-time fold
  |
  +--> replay / search / aggregate / retention / eval / Golden Path evidence
```

Three mechanisms carry the design:

- **Events, not mutable spans**: a span is written as start, end, logs, usage,
  cost, and late attribute updates. Readers fold events into one complete span.
- **Content-derived identity**: event identity is deterministic, so retransmit
  and crash replay do not double-count tokens or cost.
- **Derived indexes are rebuildable**: rollups, metadata indexes, attrs postings,
  text domains, and vector namespaces are acceleration paths. The truth remains
  WAL + segments + manifest + metadata.

The engine body is std-only Rust. Heavier integrations, such as Vortex columnar
segments, jieba FFI, and external graph indexes, live in separate crates behind
traits.

---

## Project Status

| Area | Status | Notes |
|---|---|---|
| Storage, WAL, snapshots, restart recovery | Done, tested | crash replay, compaction, GC, online backup, restart |
| SDK and OTLP ingest | Done | Python, TypeScript, custom wire JSON, OTLP/OpenInference |
| HTTP API and console | Usable | console uses public `/v1/*` APIs |
| Node / Electron embedded DB | Usable | ESM/CJS, native packages, clean consumer pack verification |
| Python embedded DB | Usable | PyO3 package, `YiTraceDB.open`, builder, ingest/search/session/span tests |
| Chinese tokenizer and BM25 | Done in pure Rust | dictionary DAG, embedded jieba dictionary, user dict support |
| Vector recall | Done in engine | disk-backed HNSW plus vector namespace flat index; high-performance namespace ANN still pending |
| Read-model indexes | First production shape | attrs postings, metadata sidecar, traceAggregate rollup, loop/task sidecar, text domains |
| Eval and Golden Path evidence | Alpha | rule scorer, annotations, datasets, trajectory groups, export, health |
| Distributed gateway path | Alpha but tested | real process evals, route tables, fanout, follower read, leases, WAL replication primitives |
| Production security | Roadmap | TLS, RBAC, encryption, rate limits, persistent audit logs |
| Managed distributed control plane | Roadmap | automatic failover, fencing, scheduled replication, bootstrap, sidecar sync |

Run the engine suite:

```bash
cd yitrace-engine
cargo test --offline
```

Optional crates:

```bash
cd yitrace-segstore-vortex && cargo build
cd yitrace-tokenizer-jieba && cargo test
cd yitrace-vecindex-graph && cargo test
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
    yt-engine                # coordinator, search, eval, HTTP, OTLP, gateway, console assets
yitrace-node/                # @yitrace/db Node/Electron embedded package
yitrace-db-python/           # yitrace-db Python embedded package
yitrace-db-rs/               # yitrace-db Rust embedded package
yitrace-console/             # React console
yitrace-sdk/
  python/                    # Python tracing SDK
  typescript/                # TypeScript tracing SDK
yitrace-segstore-vortex/     # optional Vortex segment store
yitrace-tokenizer-jieba/     # optional jieba FFI tokenizer
yitrace-vecindex-graph/      # optional graph_index FFI vector index
docs/                        # current state, API reference, design notes, research
```

Start with [Current State](docs/CURRENT_STATE.md) if you want the engineering
truth, including what is verified, what is alpha, and what is still roadmap.
Use [HTTP API Reference](docs/API_REFERENCE.md) when integrating another UI or
backend.

## License

MIT
