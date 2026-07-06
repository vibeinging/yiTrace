# @yitrace/db

Advanced local persistence package for Node.js and Electron apps that want
yiTrace in-process. If you only need to record agent runs, start with
`@yitrace/trace-sdk` and a local yiTrace collector/server instead.

## Install

```bash
npm install @yitrace/db
```

`@yitrace/db` is the JavaScript entry package. Native binaries are published as
optional per-platform packages such as `@yitrace/db-darwin-arm64` and
`@yitrace/db-linux-x64-gnu`, so users only download the binary for their
platform.

Supported targets for the first npm release:

- macOS x64 / arm64
- Linux x64 / arm64 with glibc
- Windows x64 with MSVC

For AgenticData or another internal consumer that needs a stable install source
before the public npm release, build immutable tarballs and lock those exact
files in the consumer repo:

```bash
npm ci
npm run build
npm run release:artifacts
npm run release:prepublish
npm run pack:verify  # runs pack:local first
```

This writes `@yitrace/db` plus any locally available platform packages to
`yitrace-node/dist/*.tgz`. `pack:local` appends a commit label such as
`g1a2b3c4d5e6f` to each tarball name and writes `dist/pack-manifest.json` with
the exact files. Set `YITRACE_PACK_LABEL=<release-id>` when the consuming repo
needs a human-chosen immutable label. Put those tarballs in the consuming repo
or an internal package registry, then depend on exact `file:` tarballs:

```json
{
  "dependencies": {
    "@yitrace/db": "file:vendor/yitrace-db-0.0.1-g1a2b3c4d5e6f.tgz",
    "@yitrace/db-darwin-x64": "file:vendor/yitrace-db-darwin-x64-0.0.1-g1a2b3c4d5e6f.tgz"
  }
}
```

Do not keep overwriting a shared `yitrace-db-0.0.1.tgz`. If the payload changes,
the filename or registry version must change too, so lockfiles and rollbacks can
identify the exact native binary.

For a registry publish, publish platform packages first and the root package
last. Do not publish a root package that contains only the current machine's
native binary.

Current platform policy:

- AgenticData server builds stay on one native architecture at a time. The
  current server baseline is x64: Node, DuckDB, yiTrace native, and sqlite native
  must all be x64.
- Do not mix an arm64 yiTrace native package with x64 DuckDB/sqlite, or the
  inverse. If AgenticData switches the server to arm64, first move DuckDB and
  sqlite to arm64 too, or adopt the same per-platform optional package strategy
  for those dependencies.
- AgenticData local development may still use macOS arm64 artifacts
  (`@yitrace/db` root tarball + `@yitrace/db-darwin-arm64` tarball), but that is
  not the server architecture decision.
- Local tarballs may also include any other native packages already present on
  the build machine, such as macOS x64.
- Public npm / CI release target set remains macOS x64/arm64, Linux x64/arm64
  glibc, and Windows x64 MSVC. Those platform packages must be produced by CI
  or matching build machines before publishing the root package.

## Usage

ESM:

```ts
import { YiTraceDB, createSpanEventBuilder } from "@yitrace/db";

const db = await YiTraceDB.open({ dataDir: "./data", tenantId: 1 });

const builder = createSpanEventBuilder({
  traceId: "run-uuid",
  sessionId: "session-uuid",
  attrs: {
    project_id: "agentic-data",
    skill: "review",
    mode: "auto",
    call_site: "worker.ts:10",
  },
});

builder.startSpan({
  spanId: "span-uuid",
  name: "risk review",
  agentName: "risk-agent",
  toolName: "card-risk",
  model: "gpt-5",
  provider: "openai",
  inputText: "疑似盗刷订单",
  inputTokens: 900,
  cachedInputTokens: 120,
});
builder.log({ spanId: "span-uuid", message: "疑似盗刷" });
builder.endSpan({
  spanId: "span-uuid",
  status: 0,
  durationNs: 12_000_000,
  outputText: "建议人工复核",
  outputTokens: 140,
  reasoningTokens: 30,
  totalTokens: 1190,
  costUsd: 0.0042,
  costCurrency: "USD",
});

await builder.ingest(db);

const hits = await db.search({
  text: "盗刷",
  k: 10,
  filter: {
    attrs: {
      project_id: "agentic-data",
      skill: "review",
      mode: "auto",
      call_site: "worker.ts:10",
    },
  },
});
const inputOnlyHits = await db.search({
  text: "疑似盗刷订单",
  textDomains: ["input_text"],
  filter: { attrs: { project_id: "agentic-data" } },
});
const trace = await db.trace("run-uuid");
const span = await db.span("run-uuid", "span-uuid");
const logMessages = span?.logEvents?.flatMap((event) => event.messages) ?? [];
const traces = await db.traces();

await db.close();
```

Direct event ingest is still supported when you already have wire events:

```ts
await db.ingest([
  {
    trace_id: "run-uuid",
    span_id: "span-uuid",
    ts: 1,
    seq: 1,
    event_type: 2,
    ext_span_id: "span-uuid",
    logs: ["疑似盗刷"],
    attrs: {
      external_run_id: "run-uuid",
      project_id: "agentic-data",
      skill: "review",
      mode: "auto",
      call_site: "worker.ts:10",
    },
  },
]);
```

CommonJS:

```js
const { YiTraceDB, createSpanEventBuilder } = require("@yitrace/db");
```

This package does not read yiTrace files directly. It embeds the Rust engine in
the Node process through Node-API, so WAL recovery, manifest snapshots, folding,
tenant filtering, BM25, and vector search still run through the database engine.
Internally it calls yiTrace's `EngineJsonApi` in-process; it does not start an
HTTP server, bind a port, or send traffic through a TCP socket.

For Electron, open the database from the main process and expose narrow IPC
methods to renderers. Do not let multiple app processes open the same `dataDir`
for writing; yiTrace creates a `.yitrace.lock` file to enforce a single writer.

String IDs are supported for direct `db.ingest()`. yiTrace keeps an internal
stable `u64` hash for indexing and stores the original values in
`external_trace_id`, `external_span_id`, `external_parent_span_id`, and
`external_session_id`. You can query with either numeric IDs or the original
string IDs. `attrs` is persisted through the engine and returned as JSON on
search, trace, and span detail responses.

Trace and span detail responses also return raw `logEvents`, so applications do
not need to mirror log lines into `attrs.event_logs`. Each event keeps its
`ts`, `seq`, `eventType`, stable `eventId`, `messages`, event-level `attrs`,
and stable `eventOrdinal` / `sortKey`:

```ts
const span = await db.span("run-uuid", "tool-call-1");
for (const event of span?.logEvents ?? []) {
  console.log(event.seq, event.messages);
}
```

The event builder hides `seq`, `event_type`, start/end event pairing, and
`ext_span_id`. It emits the same wire format as direct `db.ingest()`, so callers
can inspect `builder.events()` before sending or call `builder.ingest(db)` to
flush the batch.

`SpanEvent` explicitly supports the commonly consumed fields:
`duration_ns`, `tool_name`, `model`, `provider`, `input_text`, and
`output_text`, plus token/cost fields (`input_tokens`, `output_tokens`,
`cached_input_tokens`, `reasoning_tokens`, `total_tokens`, `cost_usd`,
`cost_usd_nanos`, `cost_currency`), status, logs, tenant, external IDs, and
`attrs`. Query responses keep the old `cost` / `inTok` / `outTok` fields for
compatibility and also return structured `usage` and `costDetail`:

```ts
const page = await db.traceSearch({ text: "盗刷" });
const item = page.items[0];
console.log(item.usage.totalTokens, item.costDetail.costUsd);
```

Explicit `costUsd` / `costUsdNanos` always wins. If no explicit cost is
ingested, yiTrace estimates from a small built-in provider/model price table
when `provider` and `model` are known, then falls back to the default token
price. `costDetail.source` is one of `explicit`, `estimated_model_price`,
`estimated_default`, or `mixed` for aggregates. Structured search also supports
cost/token ranges:

```ts
const expensive = await db.traceSearch({
  filter: {
    projectId: "agentic-data",
    minCostUsdNanos: 400_000,
    maxCostUsd: 0.01,
    minTotalTokens: 100,
  },
});
```

Search supports attrs filters for the high-cardinality dimensions used by
AgenticData trace pages:

```ts
await db.search({
  text: "盗刷",
  filter: {
    project_id: "agentic-data",
    skill: "review",
    mode: "auto",
    call_site: "worker.ts:10",
    taskFingerprint: "npm-native-packaging",
    validationStatus: "pass",
  },
});
```

The equivalent nested form is `filter.attrs`. Values are compared after JSON
normalization. Strings, numbers, booleans, `null`, arrays, and objects
round-trip with the same JSON shape in `attrs` on search, trace, and span
detail responses. Scalar values use exact matching; string arrays also support
includes matching, so a stored `connection_ids: ["a", "b"]` matches
`attrs: { connection_ids: "a" }`. Common top-level aliases are supported for
`project_id`, `external_run_id`, `skill`, `mode`, `call_site`,
`task_fingerprint`, `loop_id`, `harness_version`, `validation_status`,
`stop_reason`, `phase`, `validator`, `connection_ids`, `data_source_ids`,
`schema_fingerprint`, `intent_signature`, `review_status`, `eval_status`, and
`path_memory_id`.
Responses keep the original `attrs` and also expose these high-frequency keys
under `fields`, so product code can read stable dimensions without scanning the
full extension object.

`project_id`, `skill`, `mode`, `call_site`, `task_fingerprint`, `loop_id`,
`harness_version`, `schema_fingerprint`, `intent_signature`,
`validation_status`, `review_status`, `eval_status`, `path_memory_id`,
`stop_reason`, `phase`, and `validator` are promoted inside the engine as
schema-on-write fields. Passing them through `attrs`, builder defaults, or the
top-level builder aliases gives the same query behavior; the original `attrs`
object is still returned unchanged for round-trip compatibility. Stable array
keys such as `connection_ids` remain extension attrs that can be filtered through
the sidecar/folded verification path and surfaced under `fields`.

Internally, attrs filters use segment-local postings sidecars to narrow
candidate spans before folded snapshot verification. Durable data dirs contain
derived `attr_postings/seg-*.attrs` files; they are rebuilt from segment data if
missing. The final result still goes through tenant, deletion, and attrs
validation, so stale candidates cannot leak data. Stable high-frequency keys are
indexed by default, while very high-cardinality keys such as `external_run_id`
or `path_memory_id` remain queryable through folded verification without default
postings. Only a light
term-to-segment directory and a bounded LRU cache of hot posting lists stay in
memory; live WAL tail data uses a small in-memory overlay.

Session listing also accepts the same attrs filter shape:

```ts
await db.sessions({
  attrs: { project_id: "agentic-data", skill: "review", mode: "auto" },
});
```

The session filter returns a session when at least one span in that session
matches all supplied attrs, then returns the complete session aggregate.
Trace listing accepts the same filter:

```ts
const traces = await db.traces({ attrs: { connection_ids: "conn-a" } });
console.log(traces[0]?.fields?.project_id);
```

For product trace pages, use `traceSearch()` when you need structured filters,
pagination, and sorting across sessions:

```ts
const page = await db.traceSearch({
  text: "最优路径",
  limit: 20,
  sort: "duration",
  order: "desc",
  filter: {
    toolName: "planner",
    attrs: { project_id: "agentic-data", connection_ids: "conn-a" },
  },
});
console.log(page.index);
```

Use `traceAggregate()` when a product page needs grouped stats before drilling
into individual spans. It uses the same filter shape as `traceSearch()` and
groups the filtered spans. Sealed segments can use the engine's
traceAggregate rollup sidecar; WAL/MemTable tail is overlaid in-process and
unsafe cases fall back to folded scan:

```ts
const stats = await db.traceAggregate({
  groupBy: ["taskFingerprint", "validationStatus", "toolName"],
  sort: "errorRate",
  order: "desc",
  filter: {
    attrs: { project_id: "agentic-data" },
  },
});

console.log(stats.items[0]?.key, stats.items[0]?.errorRate);
console.log(stats.index, stats.aggregationIndex);
console.log(stats.readPlan?.usedSegmentRollup, stats.readPlan?.rollupFallbackReason);
```

Use `trajectoryGroups()` when a product page needs to find repeated successful
paths for the same task. It groups full traces by a stable trajectory signature
and ranks candidate paths with success rate plus eval/annotation/dataset scores:

```ts
const candidates = await db.trajectoryGroups({
  filter: {
    taskFingerprint: "npm-native-packaging",
    attrs: { project_id: "agentic-data" },
  },
  sort: "best",
});

console.log(candidates.items[0]?.signature, candidates.items[0]?.successRate);
console.log(candidates.items[0]?.steps);
console.log(candidates.index, candidates.trajectoryIndex);
```

Use `traceTrajectories()` when a page or export job needs one row per trace
with the materialized path summary. It uses the same filter shape as
`traceSearch()` but returns lightweight trace/trajectory data instead of span
details or large text columns:

```ts
const trajectories = await db.traceTrajectories({
  filter: {
    taskFingerprint: "npm-native-packaging",
    attrs: { project_id: "agentic-data", skill: "builder" },
  },
  limit: 50,
});

console.log(trajectories.items[0]?.trace.fields);
console.log(trajectories.items[0]?.trajectory.signature);
```

Use `indexVector()` / `searchVector()` when an Agent Memory or eval pipeline
already has embeddings for a task, span, or trajectory. yiTrace stores the
vector in a namespace and applies tenant/attrs filters during search; it does
not call an embedding model itself. The current embedded implementation is a
durable flat namespace index (`named_vectors.dat`) that prioritizes recovery
and API semantics; the high-performance HNSW namespace path is a follow-up
engine optimization.

```ts
await db.indexVector({
  namespace: "task",
  key: "npm-native-packaging",
  vector: [0.12, 0.34, 0.56],
  traceId: "builder-run",
  attrs: {
    project_id: "agentic-data",
    schema_fingerprint: "schema-v1",
    embedding_model: "text-embedding-3-large",
  },
});

const similarTasks = await db.searchVector({
  namespace: "task",
  vector: [0.10, 0.30, 0.58],
  k: 10,
  filter: {
    attrs: {
      project_id: "agentic-data",
      schema_fingerprint: "schema-v1",
    },
  },
});

console.log(similarTasks.vectorIndex, similarTasks.items[0]?.key);
```

Use `storageStats()` to inspect what a filtered slice of trace data costs before
building retention or cleanup flows. It uses the same filter shape as
`traceSearch()` and returns explainable estimates for text payload, attrs,
external IDs, event count, and metadata references:

```ts
const storage = await db.storageStats({
  filter: {
    taskFingerprint: "npm-native-packaging",
    attrs: { project_id: "agentic-data" },
  },
  groupBy: ["projectId", "validationStatus"],
});

console.log(storage.total.traceCount, storage.total.bytes.estimatedBytes);
console.log(storage.groups[0]?.key, storage.groups[0]?.metadata);
```

Use `retentionPlan()` for dry-runs and `applyRetention()` only when you want to
execute deletion. By default yiTrace protects traces referenced by annotation,
dataset association, active Golden Path metadata, snapshot references, eval
links, or path memory metadata. Apply only soft-deletes rows already flushed to
segment files; hot MemTable/WAL-tail traces are skipped to avoid partial
deletion:

```ts
const plan = await db.retentionPlan({
  filter: { taskFingerprint: "npm-native-packaging" },
  deleteBeforeTs: 1751540000000000000,
  protect: {
    annotations: true,
    datasetAssociations: true,
    goldenPaths: true,
    snapshots: true,
    evalLinks: true,
    pathMemory: true,
  },
});

console.log(plan.candidates.traceCount, plan.deletable.traceCount);

const result = await db.applyRetention({
  filter: { taskFingerprint: "npm-native-packaging" },
  deleteBeforeTs: 1751540000000000000,
  compact: true,
  requestedBy: "nightly-retention-policy",
  reason: "ttl cleanup",
});

console.log(result.applyResult?.deletedTraceCount);
console.log(result.compactResult?.compactedSegmentCount);
console.log(result.audit?.auditId, result.audit?.counts.deletedTraceCount);

const audits = await db.retentionAudits({
  filter: { source: "nightly-retention-policy" },
  limit: 20,
});

console.log(audits.items[0]?.traceIds.deleted);
```

For repeatable retention, persist a policy and trigger due policies from your
own scheduler. yiTrace does not start a background delete thread in embedded
Node/Electron; `runRetentionPolicies()` explicitly executes the currently due
policies through the same `applyRetention()` path and writes audit records:

```ts
const policy = await db.createRetentionPolicy({
  name: "nightly-retention-policy",
  intervalNs: 86_400_000_000_000,
  source: "nightly-retention-policy",
  reason: "ttl cleanup",
  query: {
    filter: { attrs: { project_id: "agentic-data" } },
    olderThanNs: 30 * 86_400_000_000_000,
    compact: true,
  },
});

const policies = await db.retentionPolicies({
  policyName: "nightly-retention-policy",
  enabled: true,
});

const run = await db.runRetentionPolicies({
  nowNs: (BigInt(Date.now()) * 1_000_000n).toString(),
  limit: 10,
});

console.log(policy.policyId, policies.total, run.ran, run.items[0]?.result?.audit?.auditId);
```

Once a path is accepted by a product workflow, store it as a golden path
candidate/asset. yiTrace stores only the source trace/snapshot reference,
trajectory signature, scope attrs, retained lightweight source trajectory,
evidence summary, status, score, and review metadata. Repeated-run hit tracking
and reference-count compression are intentionally left for a separate future API:

```ts
const golden = await db.createGoldenPath({
  sourceTraceId: "builder-run",
  taskFingerprint: "npm-native-packaging",
  score: 960,
  label: "fast packaging path",
  reason: "best observed route",
  source: "human",
  evalProfile: "release-gate",
  challengerOf: null,
  minSampleCount: 5,
  marginScore: 800,
  comparisonWindowNs: "86400000000000",
  staleReasons: [],
  projectId: "agentic-data",
});

await db.updateGoldenPathStatus(golden.goldenPathId, {
  status: "confirmed",
  reason: "manual accept",
  source: "reviewer",
});

const goldenPaths = await db.goldenPaths({
  taskFingerprint: "npm-native-packaging",
  status: "confirmed",
  projectId: "agentic-data",
});
console.log(goldenPaths.items[0]?.trajectorySignature);
console.log(goldenPaths.items[0]?.sourceTrajectory.steps);
console.log(goldenPaths.items[0]?.evidenceSummary);
console.log(goldenPaths.items[0]?.governance);
```

The Golden Path governance fields are evidence only: `challengerOf`,
`evalProfile`, `minSampleCount`, `marginScore`, and `comparisonWindowNs` help a
product layer run Best/Challenger workflows, but yiTrace does not automatically
promote or deprecate a path. Top-level `evalProfile` is stored as Golden Path
metadata; it is not added to trace scope filters unless you also put it inside
`attrs`.

Use `pathAdherence()` to compare a new run against a golden path without
letting the database decide what is "best". It returns deterministic trajectory
evidence: exact signature match, ordered common steps, missing steps, extra
steps, and coverage.

```ts
const adherence = await db.pathAdherence(golden.goldenPathId, "builder-run-2");
console.log(adherence.adherence, adherence.sameSignature);
console.log(adherence.sourceRetained);
console.log(adherence.commonSteps, adherence.missingSteps, adherence.extraSteps);

const sameAdherence = await db.pathAdherence({
  goldenPathId: golden.goldenPathId,
  traceId: "builder-run-2",
});
```

Use `goldenPathEvidence()` when a review page or export job needs the evidence
behind a golden path. It bundles the source trace summary, trajectory,
annotations, dataset links, and optionally the candidate adherence/diff:

```ts
const evidence = await db.goldenPathEvidence({
  goldenPathId: golden.goldenPathId,
  candidateTraceId: "builder-run-2",
});
console.log(evidence.source.annotationCount, evidence.source.datasetAssociationCount);
console.log(evidence.candidate?.pathAdherence.adherence);

const sourceOnly = await db.goldenPathEvidence(golden.goldenPathId);
```

Use `goldenPathExport()` for a stable JSONL-oriented record shape. By default
it exports only confirmed paths; pass `status` explicitly if a pipeline wants
candidate or deprecated records:

```ts
const exported = await db.goldenPathExport({
  filter: {
    taskFingerprint: "npm-native-packaging",
    projectId: "agentic-data",
  },
});
console.log(exported.schemaVersion, exported.items[0]?.source.trajectory?.signature);
console.log(exported.jsonl);
```

See `examples/golden-path-export-consumer.mjs` for a minimal consumer that turns
`yitrace.golden_path_export.v1` records into Agent Memory candidates and
regression dataset items.

Use `goldenPathHealth()` to watch whether later traces still follow a confirmed
path. It defaults to the golden path scope and excludes the source trace, so the
numbers describe follow-up runs rather than the original example:

```ts
const health = await db.goldenPathHealth({
  goldenPathId: golden.goldenPathId,
  filter: { projectId: "agentic-data" },
  examples: 5,
});
console.log(health.counts, health.rates.usable);
console.log(health.sourceRetained);
console.log(health.governance.stale, health.governance.staleReasons);

const healthWithSource = await db.goldenPathHealth(golden.goldenPathId, {
  includeSource: true,
});
```

Use `traceDiff()` when a product page needs to compare two runs of the same
task before picking a golden path. It returns deterministic evidence only:
route changes, per-step changes, and duration/token/cost/status deltas.

```ts
const diff = await db.traceDiff("run-old", "run-new");
console.log(diff.delta.costUsdNanos, diff.steps[0]?.changes);
console.log(diff.steps[0]?.right?.evalScore, diff.steps[0]?.right?.evalLabel);
console.log(diff.trajectory.same, diff.trajectory.right.signature);

const sameDiff = await db.traceDiff({
  leftTraceId: "run-old",
  rightTraceId: "run-new",
});
```

For agent loop and task pages, use the lightweight read models instead of
grouping spans in product code:

```ts
const loops = await db.loops({
  taskFingerprint: "npm-native-packaging",
  validationStatus: "pass",
});
console.log(loops.items[0]?.loopId, loops.items[0]?.traceCount);

const loop = await db.loop("loop-builder");
console.log(loop?.summary.status, loop?.traces.length, loop?.spans.length);

const taskTraces = await db.taskTraces("npm-native-packaging", {
  validationStatus: "pass",
});
console.log(taskTraces.items[0]?.fields?.loop_id);
```

Trace detail exposes stable `spanOrdinal` and `siblingOrdinal`. For eval drafts
or regression samples, export a snapshot:

```ts
const snapshot = await db.traceSnapshot("run-uuid");
console.log(snapshot?.snapshotHash, snapshot?.trace.spans.length);
```

For product review flows, add lightweight annotations without changing the trace
itself:

```ts
await db.annotate({
  traceId: "run-uuid",
  spanId: "span-uuid",
  target: "span",
  label: "best_path",
  score: 920,
  reason: "manual review picked this path",
  source: "human",
  projectId: "agentic-data",
  skill: "review",
});

const annotations = await db.annotations({
  traceId: "run-uuid",
  label: "best_path",
  projectId: "agentic-data",
  limit: 50,
});
console.log(annotations.count, annotations.nextCursor, annotations.items[0]?.reason);

const reviewed = await db.updateAnnotation(annotations.items[0].annotationId, {
  status: "resolved",
  reviewer: "four",
  reason: "accepted by manual review",
  attrs: { review_round: 1 },
});

await db.deleteAnnotation(reviewed.annotationId, {
  reviewer: "four",
  reason: "superseded by a newer judgment",
});
```

To connect trace evidence to an external eval or training dataset, store a
dataset association. yiTrace stores the source link and snapshot identity; the
dataset item body can stay in your own eval system:

```ts
await db.linkDatasetItem({
  datasetId: "best-path-regression",
  itemId: "case-1",
  traceId: "run-uuid",
  spanId: "span-uuid",
  snapshotId: snapshot?.snapshotId,
  snapshotHash: snapshot?.snapshotHash,
  evalRunId: "eval-2026-07-03",
  split: "train",
  label: "pass",
  score: 920,
  projectId: "agentic-data",
});

const links = await db.datasetAssociations({
  datasetId: "best-path-regression",
  itemId: "case-1",
  limit: 50,
});
```

`db.annotations()` and `db.datasetAssociations()` return stable pages ordered by
`createdAtNs` descending, then id descending. Use `nextCursor` as the next
request's `cursor`; `count` and `total` are the full matched total, while
`pageCount` is the current page size.
Annotation status is `active` by default and can move to `resolved`, `rejected`,
or `deleted`. Deleted annotations are soft-deleted: they stay in `metadata.dat`
for audit, but default list/search filters ignore them unless you pass
`status: "deleted"` or `includeDeleted: true`.

Those metadata records can also drive product search pages:

```ts
const reviewed = await db.traceSearch({
  filter: {
    annotation: { label: "best_path", source: "human", scoreMin: 900 },
  },
});

const regressionCases = await db.traceSearch({
  filter: {
    dataset: { datasetId: "best-path-regression", evalRunId: "eval-2026-07-03" },
  },
});

const reviewedTraces = await db.traces({
  annotation: { label: "best_path", scoreMin: 900 },
});

const regressionSessions = await db.sessions({
  dataset: { datasetId: "best-path-regression", label: "pass" },
});
```

Trace-level annotations or dataset links match every span in the trace; span-level
records only match that span for `traceSearch()`. Trace and session list filters
return a trace/session when any visible span in it has a matching span-level
record.

Annotations and dataset associations are tenant-scoped, persisted in the same
data directory as `metadata.dat`, and included in `backup_snapshot()`. They are
queried through the same in-process `EngineJsonApi`; there is still no local
HTTP server involved.

Use batch span detail when a page needs many large fields at once:

```ts
const spans = await db.spans("run-uuid", { limit: 50 });
const details = await db.spansBatch("run-uuid", ["tool-call-1"], { includeFull: true });
```

Large text fields in `traceSearch()`, `traceSnapshot()`, `spans()`, and
`spansBatch()` are returned as `{ preview, full, contentHash, byteLength,
truncated, blobRef }`. List-style calls keep `full: null` by default; snapshot
and `includeFull: true` detail calls include the full text.

`OpenOptions.readOnly` is intentionally not exposed yet. Passing `readOnly`
throws at runtime so applications do not accidentally assume a true read-only
open while the engine still uses the writable durable path.

## Electron Packaging

Package the native `.node` binary outside Electron's `asar` archive. For
electron-builder, keep the platform package and unpack native files:

```json
{
  "build": {
    "asarUnpack": [
      "**/*.node",
      "node_modules/@yitrace/db*/**/*"
    ]
  }
}
```

Do not tree-shake or prune `@yitrace/db-*` optional platform packages from the
main-process bundle. The JavaScript loader resolves the matching optional
package at runtime. If your bundler copies native files to a custom location,
set `NAPI_RS_NATIVE_LIBRARY_PATH` to the unpacked `.node` file before importing
`@yitrace/db`:

```js
process.env.NAPI_RS_NATIVE_LIBRARY_PATH = require("path").join(
  process.resourcesPath,
  "native",
  "yitrace-db.darwin-arm64.node",
);
```

Open `YiTraceDB` only from Electron's main process and expose specific IPC
methods such as `search`, `trace`, and `span` to renderers.

## Local Development

```bash
npm install
npm run build
npm test
```

`npm run build` produces a local `yitrace-db.<platform>.node` file for the
current Node platform. That file is intentionally ignored by git. Set
`NAPI_TARGET=aarch64-apple-darwin` or another Rust target triple when you need
to override local target detection.

## Publishing

Do not publish a root package that only contains the local machine's `.node`
file. Public npm releases must use the optional platform-package layout.

Release flow:

```bash
npm ci
npm run npm:dirs

# Run once per target in CI. Example:
npm run build:release -- --target x86_64-unknown-linux-gnu

# After all .node artifacts have been collected under ./artifacts:
npm run release:artifacts
npm run release:prepublish  # metadata only; this script skips automatic optional package publish
npm run pack:check
npm run pack:verify         # write commit-labeled tarballs and verify them in a clean consumer
```

Then publish each platform package first, followed by the root package:

```bash
npm publish npm/darwin-x64 --access public
npm publish npm/darwin-arm64 --access public
npm publish npm/linux-x64-gnu --access public
npm publish npm/linux-arm64-gnu --access public
npm publish npm/win32-x64-msvc --access public
npm publish --access public
```

The root `@yitrace/db` package declares those platform packages as
`optionalDependencies`. npm skips incompatible OS/CPU packages during install,
and `native.js` loads the matching binary at runtime.
