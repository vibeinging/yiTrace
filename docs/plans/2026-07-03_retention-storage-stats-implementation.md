# yiTrace Retention / Storage Stats Implementation

> Date: 2026-07-03

## Scope

This pass adds the first storage governance layer needed by Agent Memory / Golden Path workflows:

- `POST /v1/storage-stats`
- `POST /v1/retention-plan`
- `POST /v1/retention/apply`
- `GET/POST /v1/retention-audits`
- `POST/GET /v1/retention-policies`
- `POST /v1/retention-policies/run-due`
- Node wrappers: `db.storageStats()`, `db.retentionPlan()`, `db.applyRetention()`, `db.retentionAudits()`, `db.createRetentionPolicy()`, `db.retentionPolicies()`, `db.runRetentionPolicies()`

All three APIs reuse `traceSearch` filtering semantics, including tenant isolation and attrs filters.

## Semantics

`storageStats` is read-only. It reports trace/span/event counts and explainable byte estimates:

- input/output/log payload bytes use UTF-8 string length.
- attrs and external id bytes use stored key/value length.
- `estimatedBytes` adds a simple per-event and per-span overhead estimate; it is not a physical disk byte counter.
- metadata counts include annotation, dataset association, Golden Path, snapshot, eval link, and path memory references for matching trace ids.

`retentionPlan` is dry-run. It computes candidate trace ids from the filtered result and optional `deleteBeforeTs`, then removes protected traces.

Default protections:

- annotation references
- dataset association references
- Golden Path source traces when status is `candidate` or `confirmed`
- snapshot references from dataset association or Golden Path `snapshotId` / `snapshotHash`
- eval links from `evalRunId` or eval attrs
- path memory references from metadata attrs `path_memory_id`

`retention/apply` requires `deleteBeforeTs`. It performs segment-row soft deletion only for traces that are already flushed to segments. If a trace still has live MemTable/WAL-tail rows, the whole trace is skipped to avoid partial deletion.

`compact: true` on `retention/apply` now runs deletion-vector compaction after soft delete:

- candidate segments are selected by `compactMinDeletedRows`, `compactMinDeletedPercent`, and `compactMaxSegments`.
- selected segments are rewritten through the existing compaction path, so current deletion/upgrade blocks are reread before commit.
- fully deleted segments do not leave empty live segment files.
- `reclaim=true` tries to physically unlink dead segments through the existing GC log protocol; old readers can still hold reclamation back.

Every real `retention/apply` appends a tenant-scoped audit record to `metadata.dat`:

- source/reason come from `requestedBy` / `source` and `reason` / `comment`.
- it records protect flags, delete cutoff, compact/reclaim settings, candidate/protected/deletable counts, apply result, compact result, and trace id samples.
- trace id samples are capped at 100 per list and set `sampleTruncated=true` when larger batches are clipped, so audit data does not become another unbounded payload store.
- `retentionAudits()` / `/v1/retention-audits` can query by source, audit id, and created time range.

Retention policies are durable metadata records:

- each policy stores `name`, `enabled`, `intervalNs`, `lastRunAtNs`, `nextRunAtNs`, `source`, `reason`, and a retention query template.
- query templates must include `deleteBeforeTs` or a TTL-style field (`olderThanNs`, `ttlNs`, `retentionNs`).
- `runRetentionPolicies()` / `/v1/retention-policies/run-due` selects due policies, converts TTL to the current `deleteBeforeTs`, executes the existing `retention/apply` path, advances `lastRunAtNs/nextRunAtNs` only on success, and writes the normal retention audit.
- no background thread is started by the embedded DB; cron, app timers, queues, or ops jobs should trigger `run-due` explicitly.

## Follow-Ups

- Background daemon / external automation examples for retention policy execution.
- High-performance rollups for storage stats if this becomes a hot dashboard endpoint.
- Reference-count or duplicate-trace compaction remains a separate requirement.
