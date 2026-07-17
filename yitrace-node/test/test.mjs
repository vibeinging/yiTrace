import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { YiTraceDB, createSpanEventBuilder } from "../index.js";

function vectorForText(text) {
  const lower = String(text).toLowerCase();
  if (lower.includes("manual")) return [1, 0];
  if (lower.includes("target") || lower.includes("needle")) return [0, 0];
  return [9, 0];
}

const dir = await mkdtemp(join(tmpdir(), "yitrace-node-"));

try {
  await assert.rejects(
    () => YiTraceDB.open({ dataDir: join(dir, "read-only"), readOnly: true }),
    /readOnly is not supported/,
    "readOnly must fail fast until true read-only engine open exists",
  );

  const db = await YiTraceDB.open({ dataDir: dir, tenantId: 1 });
  const secondDb = await YiTraceDB.open({ dataDir: dir, tenantId: 1 });
  await secondDb.ingest([
    {
      trace_id: "multi-node-second",
      span_id: "span-second",
      ts: 90,
      seq: 1,
      event_type: 2,
      ext_span_id: "span-second",
      logs: ["second db 盗刷"],
    },
  ]);
  await secondDb.flush();
  await secondDb.close();

  await db.ingest([
    {
      trace_id: 1,
      span_id: 1,
      ts: 100,
      seq: 1,
      event_type: 1,
      ext_span_id: "1-1",
      tenant_id: 999,
      session_id: 700,
      agent_name: "风控",
      input_text: "检查交易",
      logs: ["开始"],
    },
    {
      trace_id: 1,
      span_id: 1,
      ts: 150,
      seq: 2,
      event_type: 2,
      ext_span_id: "1-1",
      tenant_id: 999,
      session_id: 700,
      status: 1,
      duration_ns: 50,
      output_text: "疑似盗刷",
      logs: ["疑似盗刷"],
    },
  ]);

  const hits = await db.search({ text: "盗刷", k: 10 });
  assert.ok(hits.some((hit) => hit.trace_id === 1));
  assert.ok(hits.some((hit) => hit.external_trace_id === "multi-node-second"));

  let queryEmbeddingCalls = 0;
  let documentEmbeddingCalls = 0;
  const embeddingDb = await YiTraceDB.open({
    dataDir: join(dir, "embedding"),
    tenantId: 1,
    embedder: {
      dimensions: 2,
      embedQuery: async (text) => {
        queryEmbeddingCalls += 1;
        return vectorForText(text);
      },
      embedDocuments: async (texts) => {
        documentEmbeddingCalls += 1;
        return texts.map(vectorForText);
      },
    },
  });
  await embeddingDb.ingest(
    [
      {
        trace_id: "embed-target",
        span_id: "embed-span",
        ts: 160,
        seq: 1,
        event_type: 2,
        ext_span_id: "embed-span",
        input_text: "semantic target span",
      },
      {
        trace_id: "embed-far",
        span_id: "embed-far-span",
        ts: 161,
        seq: 1,
        event_type: 2,
        ext_span_id: "embed-far-span",
        input_text: "unrelated span",
      },
    ],
    { indexEmbeddings: true },
  );
  assert.equal(documentEmbeddingCalls, 1, "ingest({ indexEmbeddings: true }) should batch document embeddings");
  queryEmbeddingCalls = 0;
  await embeddingDb.search({ text: "needle", k: 3 });
  assert.equal(queryEmbeddingCalls, 0, "plain text search must stay BM25-only and avoid embedding cost");
  const semanticHits = await embeddingDb.search({ text: "needle", mode: "semantic", k: 3 });
  assert.equal(queryEmbeddingCalls, 1);
  assert.equal(semanticHits[0].external_trace_id, "embed-target");
  const hybridHits = await embeddingDb.search({ text: "target", mode: "hybrid", k: 3 });
  assert.equal(hybridHits[0].external_trace_id, "embed-target");

  await embeddingDb.ingest([
    {
      trace_id: "manual-embed",
      span_id: "manual-span",
      ts: 162,
      seq: 1,
      event_type: 2,
      ext_span_id: "manual-span",
      input_text: "manual vector span",
    },
  ]);
  const indexed = await embeddingDb.indexEmbeddings([{ traceId: "manual-embed", spanId: "manual-span", text: "manual vector span" }]);
  assert.equal(indexed.indexed, 1);
  const manualHits = await embeddingDb.search({ text: "manual query", mode: "semantic", k: 3 });
  assert.equal(manualHits[0].external_trace_id, "manual-embed");
  await assert.rejects(
    () => embeddingDb.indexEmbedding({ traceId: "bad-dim", spanId: "bad-dim-span", vector: [1, 2, 3] }),
    /dimension 3 does not match expected 2/,
  );
  await embeddingDb.close();

  const traces = await db.traces();
  assert.ok(traces.some((trace) => trace.trace_id === 1));

  const sessions = await db.sessions();
  assert.ok(sessions.items.some((session) => session.sessionId === "700"));

  const trace = await db.trace(1);
  assert.equal(trace.summary.traceId, "1");
  assert.equal(trace.spans.length, 1);

  const span = await db.span(1, 1);
  assert.equal(span.output, "疑似盗刷");

  await db.ingest([
    {
      trace_id: "run-uuid",
      span_id: "span-uuid",
      session_id: "session-uuid",
      ts: 200,
      seq: 1,
      event_type: 2,
      ext_span_id: "span-uuid",
      status: 0,
      duration_ns: 25,
      input_text: "外部 run 疑似盗刷",
      attrs: {
        external_run_id: "run-uuid",
        project_id: "agentic-data",
        skill: "review",
        mode: "auto",
        call_site: "worker.ts:10",
      },
    },
    {
      trace_id: "123456",
      span_id: "numeric-business-span",
      ts: 220,
      seq: 1,
      event_type: 2,
      ext_span_id: "numeric-business-span",
      status: 0,
      input_text: "数字字符串业务主键",
      attrs: {
        project_id: "agentic-data",
        skill: "review",
      },
    },
  ]);

  const uuidHits = await db.search({ text: "外部", k: 10, filter: { traceId: "run-uuid" } });
  assert.equal(uuidHits.length, 1);
  assert.equal(uuidHits[0].external_trace_id, "run-uuid");
  assert.equal(uuidHits[0].external_span_id, "span-uuid");
  assert.equal(uuidHits[0].attrs.project_id, "agentic-data");

  const uuidTrace = await db.trace("run-uuid");
  assert.equal(uuidTrace.summary.externalTraceId, "run-uuid");
  assert.equal(uuidTrace.spans[0].externalSpanId, "span-uuid");
  assert.equal(uuidTrace.spans[0].attrs.call_site, "worker.ts:10");

  const uuidSpan = await db.span("run-uuid", "span-uuid");
  assert.equal(uuidSpan.externalSpanId, "span-uuid");
  assert.equal(uuidSpan.attrs.skill, "review");

  const numericBusinessTrace = await db.traceSearch({
    filter: { traceId: "123456" },
    limit: 1,
  });
  assert.equal(numericBusinessTrace.total, 1);
  assert.equal(numericBusinessTrace.items[0].externalTraceId, "123456");
  assert.equal(numericBusinessTrace.readPlan.usedFilterIndex, true);
  assert.equal(numericBusinessTrace.readPlan.candidateSpanKeys, 1);

  const numericBusinessTraceFromNumber = await db.traceSearch({
    filter: { traceId: 123456 },
    limit: 1,
  });
  assert.equal(numericBusinessTraceFromNumber.total, 1);
  assert.equal(numericBusinessTraceFromNumber.items[0].externalTraceId, "123456");
  assert.equal(numericBusinessTraceFromNumber.readPlan.usedFilterIndex, true);
  assert.equal(numericBusinessTraceFromNumber.readPlan.candidateSpanKeys, 1);

  const builder = createSpanEventBuilder({
    traceId: "builder-run",
    sessionId: "builder-session",
    agentName: "builder-agent",
    attrs: { project_id: "agentic-data", skill: "builder", mode: "auto", call_site: "builder.ts:1" },
  });
  builder.startSpan({
    spanId: "builder-span",
    name: "builder span",
    displayName: "  Builder 展示名  ",
    toolName: "builder-tool",
    model: "qwen",
    inputText: "builder 输入",
  });
  builder.log({ spanId: "builder-span", message: "builder 处理中" });
  builder.endSpan({
    spanId: "builder-span",
    status: 0,
    durationNs: 10,
    outputText: "builder 输出",
    cacheReadTokens: 0,
    cacheWriteTokens: 7,
  });
  const builtEvents = builder.events();
  assert.deepEqual(
    builtEvents.map((event) => event.event_type),
    [1, 4, 2],
  );
  assert.deepEqual(
    builtEvents.map((event) => event.seq),
    [1, 2, 3],
  );
  assert.equal(builtEvents[0].external_trace_id, "builder-run");
  assert.equal(builtEvents[0].external_span_id, "builder-span");
  assert.equal(builtEvents[0].external_session_id, "builder-session");
  assert.equal(builtEvents[0].span_name, "builder span");
  assert.equal(builtEvents[0].display_name, "Builder 展示名");
  assert.equal(builtEvents[0].agent_name, "builder-agent");
  assert.equal(builtEvents[0].logs, undefined, "name 不再混进 logs");
  assert.equal(builtEvents[1].span_name, undefined, "名字只在 start 上报");
  assert.equal(builtEvents[2].cache_read_tokens, 0);
  assert.equal(builtEvents[2].cache_write_tokens, 7);
  await builder.ingest(db);

  const attrHits = await db.search({
    text: "builder",
    filter: { attrs: { project_id: "agentic-data", skill: "builder", mode: "auto", call_site: "builder.ts:1" } },
  });
  assert.equal(attrHits.length, 1);
  assert.equal(attrHits[0].attrs.skill, "builder");

  const attrMisses = await db.search({ text: "builder", filter: { project_id: "agentic-data", skill: "review" } });
  assert.equal(attrMisses.length, 0);

  const filteredSessions = await db.sessions({ attrs: { project_id: "agentic-data", skill: "builder", mode: "auto" } });
  assert.equal(filteredSessions.items.length, 1);
  assert.equal(filteredSessions.items[0].externalSessionId, "builder-session");

  const traceSearch = await db.traceSearch({
    text: "builder",
    filter: { attrs: { project_id: "agentic-data", skill: "builder" } },
    limit: 10,
  });
  assert.equal(traceSearch.total, 1);
  assert.equal(traceSearch.items[0].externalTraceId, "builder-run");
  assert.equal(traceSearch.items[0].externalSpanId, "builder-span");
  assert.equal(traceSearch.items[0].spanName, "builder span");
  assert.equal(traceSearch.items[0].displayName, "Builder 展示名");
  assert.equal(traceSearch.readPlan.source, "filter_index");
  assert.equal(traceSearch.readPlan.usedFilterIndex, true);
  assert.equal(traceSearch.readPlan.candidateSpanKeys, 1);

  const externalRunExists = await db.traceSearch({
    filter: { externalTraceId: "builder-run" },
    limit: 1,
  });
  assert.equal(externalRunExists.total, 1);
  assert.equal(externalRunExists.items[0].externalTraceId, "builder-run");
  assert.equal(externalRunExists.items[0].spanName, "builder span");
  assert.equal(externalRunExists.items[0].displayName, "Builder 展示名");
  assert.equal(externalRunExists.readPlan.usedFilterIndex, true);
  assert.equal(externalRunExists.readPlan.candidateSpanKeys, 1);

  const builderTrace = await db.trace("builder-run");
  assert.equal(builderTrace.summary.name, "Builder 展示名");
  assert.equal(builderTrace.spans[0].spanName, "builder span");
  assert.equal(builderTrace.spans[0].displayName, "Builder 展示名");
  assert.equal(builderTrace.spans[0].actorId, "tool:builder-tool");

  const externalRunMiss = await db.traceSearch({
    filter: { externalTraceId: "builder-run-missing" },
    limit: 1,
  });
  assert.equal(externalRunMiss.total, 0);
  assert.equal(externalRunMiss.readPlan.usedFilterIndex, true);
  assert.equal(externalRunMiss.readPlan.candidateSpanKeys, 0);

  const aggregate = await db.traceAggregate({
    filter: { projectId: "agentic-data" },
    groupBy: ["skill"],
  });
  assert.ok(aggregate.items.some((item) => item.key.skill === "builder" && item.spanCount === 1));
  assert.equal(aggregate.readPlan.usedFilterIndex, true);

  const storage = await db.storageStats({
    filter: { projectId: "agentic-data" },
    groupBy: ["skill"],
  });
  assert.ok(storage.total.traceCount >= 2);
  assert.ok(storage.total.spanCount >= 2);
  assert.ok(storage.total.estimatedBytes > 0);
  assert.equal(storage.readPlan.usedFilterIndex, true);

  await db.ingest([
    {
      trace_id: "node-task-a",
      span_id: "plan",
      ts: 300,
      seq: 1,
      event_type: 2,
      ext_span_id: "node-task-a-plan",
      agent_name: "planner",
      status: 0,
      duration_ns: 100,
      attrs: { project_id: "agentic-data", skill: "builder", task_fingerprint: "node-task", loop_id: "node-loop", validation_status: "pass" },
    },
    {
      trace_id: "node-task-a",
      span_id: "tool",
      parent_span_id: "plan",
      ts: 310,
      seq: 1,
      event_type: 2,
      ext_span_id: "node-task-a-tool",
      tool_name: "sql.check",
      status: 0,
      duration_ns: 50,
      attrs: { project_id: "agentic-data", skill: "builder", task_fingerprint: "node-task", loop_id: "node-loop", validation_status: "pass" },
    },
    {
      trace_id: "node-task-b",
      span_id: "plan",
      ts: 320,
      seq: 1,
      event_type: 2,
      ext_span_id: "node-task-b-plan",
      agent_name: "planner",
      status: 0,
      duration_ns: 80,
      attrs: { project_id: "agentic-data", skill: "builder", task_fingerprint: "node-task", loop_id: "node-loop", validation_status: "fail" },
    },
    {
      trace_id: "node-task-b",
      span_id: "manual",
      parent_span_id: "plan",
      ts: 330,
      seq: 1,
      event_type: 2,
      ext_span_id: "node-task-b-manual",
      tool_name: "manual.review",
      status: 1,
      duration_ns: 300,
      attrs: { project_id: "agentic-data", skill: "builder", task_fingerprint: "node-task", loop_id: "node-loop", validation_status: "fail" },
    },
  ]);

  const trajectories = await db.traceTrajectories({ filter: { projectId: "agentic-data", taskFingerprint: "node-task" }, limit: 10 });
  assert.equal(trajectories.total, 2);
  assert.ok(trajectories.items.some((item) => item.summary.externalTraceId === "node-task-a"));

  const groups = await db.trajectoryGroups({ filter: { projectId: "agentic-data", taskFingerprint: "node-task" }, limit: 10 });
  assert.equal(groups.total, 2);
  assert.ok(groups.items.some((item) => item.successCount === 1));

  const diff = await db.traceDiff("node-task-a", "node-task-b");
  assert.equal(diff.sameSignature, false);
  assert.equal(diff.commonPrefix, 1);

  const loops = await db.loops({ projectId: "agentic-data", taskFingerprint: "node-task" });
  assert.equal(loops.total, 1);
  assert.equal(loops.items[0].loopId, "node-loop");

  const loop = await db.loop("node-loop");
  assert.equal(loop.summary.traceCount, 2);

  const taskPass = await db.taskTraces("node-task", { validationStatus: "pass" });
  assert.equal(taskPass.total, 1);
  assert.equal(taskPass.items[0].summary.externalTraceId, "node-task-a");

  const annotation = await db.annotate({
    traceId: "builder-run",
    spanId: "builder-span",
    label: "best_path",
    score: 950,
    reason: "human confirmed",
    source: "test",
    projectId: "agentic-data",
    attrs: { skill: "builder" },
  });
  assert.equal(annotation.annotationId, "1");
  assert.equal(annotation.externalTraceId, "builder-run");
  assert.equal(annotation.attrs.project_id, "agentic-data");

  const annotations = await db.annotations({ projectId: "agentic-data", skill: "builder", label: "best_path" });
  assert.equal(annotations.total, 1);
  assert.equal(annotations.items[0].label, "best_path");

  const updatedAnnotation = await db.updateAnnotation(annotation.annotationId, {
    status: "resolved",
    reviewer: "qa",
    attrs: { mode: "eval" },
  });
  assert.equal(updatedAnnotation.status, "resolved");
  assert.equal(updatedAnnotation.attrs.project_id, "agentic-data");
  assert.equal(updatedAnnotation.attrs.mode, "eval");

  const deletedAnnotation = await db.deleteAnnotation(annotation.annotationId, { reviewer: "qa", reason: "stale" });
  assert.equal(deletedAnnotation.status, "deleted");

  const activeOnly = await db.annotations({ projectId: "agentic-data", label: "best_path" });
  assert.equal(activeOnly.total, 0);
  const includeDeleted = await db.annotations({ projectId: "agentic-data", label: "best_path", includeDeleted: true });
  assert.equal(includeDeleted.total, 1);
  assert.equal(includeDeleted.items[0].reason, "stale");

  const datasetLink = await db.linkDatasetItem({
    datasetId: "node-regression",
    itemId: "case-1",
    traceId: "builder-run",
    spanId: "builder-span",
    split: "eval",
    label: "pass",
    score: 900,
    projectId: "agentic-data",
    attrs: { skill: "builder" },
  });
  assert.equal(datasetLink.associationId, "1");
  assert.equal(datasetLink.externalSpanId, "builder-span");

  const datasetLinks = await db.datasetAssociations({ datasetId: "node-regression", projectId: "agentic-data" });
  assert.equal(datasetLinks.total, 1);
  assert.equal(datasetLinks.items[0].itemId, "case-1");

  const missedSessions = await db.sessions({ projectId: "agentic-data", skill: "missing" });
  assert.equal(missedSessions.items.length, 0);

  const hiddenFromSpoofedTenant = await db.traces({ tenantId: 999 });
  assert.equal(hiddenFromSpoofedTenant.length, 0);
  const hiddenAnnotations = await db.annotations({ projectId: "agentic-data", includeDeleted: true, tenantId: 999 });
  assert.equal(hiddenAnnotations.total, 0);

  await db.ingest([
    {
      trace_id: "node-retention-keep",
      span_id: "span",
      ts: 400,
      seq: 1,
      event_type: 2,
      ext_span_id: "node-retention-keep-span",
      status: 0,
      duration_ns: 10,
      output_text: "retention keep",
      attrs: { project_id: "node-retention", skill: "cleanup" },
    },
    {
      trace_id: "node-retention-delete",
      span_id: "span",
      ts: 410,
      seq: 1,
      event_type: 2,
      ext_span_id: "node-retention-delete-span",
      status: 0,
      duration_ns: 10,
      output_text: "retention delete",
      attrs: { project_id: "node-retention", skill: "cleanup" },
    },
  ]);
  await db.flush();
  await db.ingest([
    {
      trace_id: "node-retention-hot",
      span_id: "span",
      ts: 420,
      seq: 1,
      event_type: 2,
      ext_span_id: "node-retention-hot-span",
      status: 0,
      duration_ns: 10,
      output_text: "retention hot",
      attrs: { project_id: "node-retention", skill: "cleanup" },
    },
  ]);
  await db.annotate({ traceId: "node-retention-keep", label: "keep", source: "retention-test", projectId: "node-retention" });

  const retentionQuery = {
    filter: { projectId: "node-retention" },
    deleteBeforeTs: 1_000,
    protect: { annotations: true, datasetAssociations: true, snapshots: true, evalLinks: true, pathMemory: true },
    requestedBy: "node-test",
    reason: "ttl",
  };
  const retentionPlan = await db.retentionPlan(retentionQuery);
  assert.equal(retentionPlan.dryRun, true);
  assert.equal(retentionPlan.candidates.traceCount, 3);
  assert.ok(Object.values(retentionPlan.protectedReasons).some((reasons) => reasons.includes("annotation")));
  assert.equal(retentionPlan.deletableTraceIds.length, 2);

  const retentionApply = await db.applyRetention(retentionQuery);
  assert.equal(retentionApply.applied, true);
  assert.equal(retentionApply.applyResult.deletedTraceIds.length, 1);
  assert.equal(retentionApply.applyResult.skippedLiveTraceIds.length, 1);
  const retentionTraces = await db.traceSearch({ filter: { projectId: "node-retention" }, limit: 10 });
  assert.equal(retentionTraces.total, 2);
  assert.ok(retentionTraces.items.some((item) => item.externalTraceId === "node-retention-keep"));
  assert.ok(retentionTraces.items.some((item) => item.externalTraceId === "node-retention-hot"));
  assert.ok(!retentionTraces.items.some((item) => item.externalTraceId === "node-retention-delete"));

  const retentionAudits = await db.retentionAudits({ source: "node-test" });
  assert.equal(retentionAudits.total, 1);
  assert.equal(retentionAudits.items[0].counts.deletedTraceCount, 1);

  const retentionPolicy = await db.createRetentionPolicy({
    name: "node-retention-policy",
    intervalNs: 1000,
    nextRunAtNs: 1,
    query: {
      filter: { projectId: "node-retention" },
      deleteBeforeTs: 1_000,
      protect: { annotations: true },
      requestedBy: "node-policy",
    },
    source: "node-policy",
    reason: "ttl",
  });
  assert.equal(retentionPolicy.policyId, "1");
  const runDue = await db.runRetentionPolicies({ nowNs: 2, limit: 1 });
  assert.equal(runDue.ran, 1);
  assert.equal(runDue.items[0].ok, true);

  await db.close();

  const reopened = await YiTraceDB.open({ dataDir: dir, tenantId: 1 });
  const recovered = await reopened.traces();
  assert.ok(recovered.length >= 8);
  assert.equal(recovered.find((t) => t.external_trace_id === "run-uuid")?.external_trace_id, "run-uuid");
  assert.equal(
    recovered.find((t) => t.external_trace_id === "multi-node-second")?.external_trace_id,
    "multi-node-second",
  );
  const recoveredAnnotations = await reopened.annotations({ projectId: "agentic-data", includeDeleted: true });
  assert.equal(recoveredAnnotations.total, 1);
  assert.equal(recoveredAnnotations.items[0].status, "deleted");
  const recoveredLinks = await reopened.datasetAssociations({ datasetId: "node-regression" });
  assert.equal(recoveredLinks.total, 1);
  assert.equal(recoveredLinks.items[0].itemId, "case-1");
  const recoveredAudits = await reopened.retentionAudits();
  assert.equal(recoveredAudits.total, 2);
  const recoveredPolicies = await reopened.retentionPolicies({ name: "node-retention-policy" });
  assert.equal(recoveredPolicies.total, 1);
  assert.equal(recoveredPolicies.items[0].lastRunAtNs, "2");
  await reopened.close();
} finally {
  await rm(dir, { recursive: true, force: true });
}
