# @yitrace/db

Embedded yiTrace database for Node.js and Electron.

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
  inputText: "疑似盗刷订单",
});
builder.log({ spanId: "span-uuid", message: "疑似盗刷" });
builder.endSpan({
  spanId: "span-uuid",
  status: 0,
  durationNs: 12_000_000,
  outputText: "建议人工复核",
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
`ts`, `seq`, `eventType`, stable `eventId`, `messages`, and event-level
`attrs`:

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
`duration_ns`, `tool_name`, `model`, `input_text`, and `output_text`, plus token
counts, status, logs, tenant, external IDs, and `attrs`.

Search supports exact attrs filters for the high-cardinality dimensions used by
AgenticData trace pages:

```ts
await db.search({
  text: "盗刷",
  filter: {
    project_id: "agentic-data",
    skill: "review",
    mode: "auto",
    call_site: "worker.ts:10",
  },
});
```

The equivalent nested form is `filter.attrs.{project_id,skill,mode,call_site}`.
Values are exact matches after JSON normalization. Strings remain strings,
numbers remain JSON numbers, booleans remain booleans, `null` remains null, and
arrays/objects round-trip as JSON arrays/objects in `attrs` on search, trace,
and span detail responses. Filtering is currently guaranteed only for
`project_id`, `skill`, `mode`, and `call_site`; other attrs are stored and
returned but not indexed as filter sidecars.

Session listing also accepts the same attrs filter shape:

```ts
await db.sessions({
  attrs: { project_id: "agentic-data", skill: "review", mode: "auto" },
});
```

The session filter returns a session when at least one span in that session
matches all supplied attrs, then returns the complete session aggregate.

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
