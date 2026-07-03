import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { YiTraceDB, createSpanEventBuilder } from "../index.js";

const dir = await mkdtemp(join(tmpdir(), "yitrace-node-"));

try {
  await assert.rejects(
    () => YiTraceDB.open({ dataDir: join(dir, "read-only"), readOnly: true }),
    /readOnly is not supported/,
    "readOnly must fail fast until true read-only engine open exists",
  );

  const db = await YiTraceDB.open({ dataDir: dir, tenantId: 1 });

  await assert.rejects(
    () => YiTraceDB.open({ dataDir: dir, tenantId: 1 }),
    /already open or locked/,
    "same data dir must be single-writer locked",
  );

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
  assert.equal(hits.length, 1);
  assert.equal(hits[0].trace_id, 1);

  const traces = await db.traces();
  assert.equal(traces.length, 1);
  assert.equal(traces[0].trace_id, 1);

  const sessions = await db.sessions();
  assert.equal(sessions.items.length, 1);
  assert.equal(sessions.items[0].sessionId, "700");

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

  const builder = createSpanEventBuilder({
    traceId: "builder-run",
    sessionId: "builder-session",
    projectId: "agentic-data",
    skill: "builder",
    mode: "auto",
    callSite: "builder.ts:1",
    taskFingerprint: "npm-native-packaging",
    loopId: "loop-builder",
    harnessVersion: "h1",
    validationStatus: "pass",
    stopReason: "goal_met",
    phase: "verify",
    validator: "npm test",
    attrs: {
      connection_ids: ["conn-a", "conn-b"],
      path_memory_id: "pm-builder",
    },
  });
  builder.startSpan({
    spanId: "builder-span",
    name: "builder span",
    agentName: "builder-agent",
    toolName: "builder-tool",
    model: "qwen",
    provider: "qwen",
    inputText: "builder 输入",
    inputTokens: 120,
    cachedInputTokens: 10,
    reasoningTokens: 5,
    totalTokens: 170,
    costUsd: 0.00042,
    costCurrency: "USD",
  });
  builder.log({ spanId: "builder-span", message: "builder 处理中" });
  builder.endSpan({ spanId: "builder-span", status: 0, durationNs: 10, outputText: "builder 输出" });
  const builtEvents = builder.events();
  assert.deepEqual(
    builtEvents.map((event) => event.event_type),
    [1, 4, 2],
  );
  assert.deepEqual(
    builtEvents.map((event) => event.seq),
    [1, 2, 3],
  );
  assert.equal(builtEvents[0].attrs.project_id, "agentic-data");
  assert.equal(builtEvents[1].attrs.call_site, "builder.ts:1");
  assert.equal(builtEvents[0].attrs.task_fingerprint, "npm-native-packaging");
  assert.equal(builtEvents[0].attrs.loop_id, "loop-builder");
  assert.equal(builtEvents[0].attrs.validation_status, "pass");
  assert.equal(builtEvents[0].attrs.phase, "verify");
  assert.equal(builtEvents[0].attrs.validator, "npm test");
  await builder.ingest(db);

  const attrHits = await db.search({
    text: "builder",
    filter: { attrs: { project_id: "agentic-data", skill: "builder", mode: "auto", call_site: "builder.ts:1" } },
  });
  assert.equal(attrHits.length, 1);
  assert.equal(attrHits[0].attrs.skill, "builder");

  const modelProviderHits = await db.search({
    text: "builder",
    filter: { attrs: { model: "qwen", provider: "qwen" } },
  });
  assert.equal(modelProviderHits.length, 1);
  assert.equal(modelProviderHits[0].fields.model, "qwen");
  assert.equal(modelProviderHits[0].fields.provider, "qwen");

  const arrayAttrHits = await db.search({ text: "builder", filter: { attrs: { connection_ids: "conn-a" } } });
  assert.equal(arrayAttrHits.length, 1);
  assert.deepEqual(arrayAttrHits[0].attrs.connection_ids, ["conn-a", "conn-b"]);

  const unindexedAttrHits = await db.search({ text: "builder", filter: { attrs: { path_memory_id: "pm-builder" } } });
  assert.equal(unindexedAttrHits.length, 1);
  assert.equal(unindexedAttrHits[0].attrs.path_memory_id, "pm-builder");

  const attrMisses = await db.search({ text: "builder", filter: { project_id: "agentic-data", skill: "review" } });
  assert.equal(attrMisses.length, 0);

  const filteredTraces = await db.traces({ attrs: { connection_ids: "conn-a" } });
  assert.equal(filteredTraces.length, 1);
  assert.equal(filteredTraces[0].external_trace_id, "builder-run");
  assert.deepEqual(filteredTraces[0].fields.connection_ids, ["conn-a", "conn-b"]);

  const tracesByUnindexedAttr = await db.traces({ attrs: { path_memory_id: "pm-builder" } });
  assert.equal(tracesByUnindexedAttr.length, 1);
  assert.equal(tracesByUnindexedAttr[0].external_trace_id, "builder-run");

  const filteredSessions = await db.sessions({ attrs: { project_id: "agentic-data", skill: "builder", connection_ids: "conn-a" } });
  assert.equal(filteredSessions.items.length, 1);
  assert.equal(filteredSessions.items[0].externalSessionId, "builder-session");

  const sessionsByUnindexedAttr = await db.sessions({ attrs: { path_memory_id: "pm-builder" } });
  assert.equal(sessionsByUnindexedAttr.items.length, 1);
  assert.equal(sessionsByUnindexedAttr.items[0].externalSessionId, "builder-session");

  const missedSessions = await db.sessions({ projectId: "agentic-data", skill: "missing" });
  assert.equal(missedSessions.items.length, 0);

  const traceSearch = await db.traceSearch({
    text: "builder",
    sort: "duration",
    order: "desc",
    filter: { toolName: "builder-tool", attrs: { connection_ids: "conn-a", path_memory_id: "pm-builder" } },
  });
  assert.equal(traceSearch.total, 1);
  assert.equal(traceSearch.items[0].externalSpanId, "builder-span");
  assert.equal(traceSearch.items[0].inputText.preview, "builder 输入");
  assert.equal(traceSearch.items[0].provider, "qwen");
  assert.equal(traceSearch.items[0].usage.inputTokens, 120);
  assert.equal(traceSearch.items[0].usage.cachedInputTokens, 10);
  assert.equal(traceSearch.items[0].usage.reasoningTokens, 5);
  assert.equal(traceSearch.items[0].usage.totalTokens, 170);
  assert.equal(traceSearch.items[0].costDetail.costUsdNanos, 420000);

  const traceSearchByUnindexedAttr = await db.traceSearch({
    filter: { attrs: { path_memory_id: "pm-builder" } },
  });
  assert.equal(traceSearchByUnindexedAttr.total, 1);
  assert.equal(traceSearchByUnindexedAttr.items[0].externalSpanId, "builder-span");

  const traceSearchByTask = await db.traceSearch({
    filter: {
      taskFingerprint: "npm-native-packaging",
      validationStatus: "pass",
      phase: "verify",
    },
  });
  assert.equal(traceSearchByTask.total, 1);
  assert.equal(traceSearchByTask.items[0].fields.task_fingerprint, "npm-native-packaging");
  assert.equal(traceSearchByTask.items[0].fields.validation_status, "pass");

  const traceAggregate = await db.traceAggregate({
    groupBy: ["skill", "mode", "toolName", "taskFingerprint", "validationStatus"],
    filter: { attrs: { project_id: "agentic-data", path_memory_id: "pm-builder" } },
  });
  assert.equal(traceAggregate.total, 1);
  assert.equal(traceAggregate.spanTotal, 1);
  assert.equal(traceAggregate.items[0].key.skill, "builder");
  assert.equal(traceAggregate.items[0].key.mode, "auto");
  assert.equal(traceAggregate.items[0].key.toolName, "builder-tool");
  assert.equal(traceAggregate.items[0].key.task_fingerprint, "npm-native-packaging");
  assert.equal(traceAggregate.items[0].key.validation_status, "pass");
  assert.equal(traceAggregate.items[0].spanCount, 1);
  assert.equal(traceAggregate.items[0].traceCount, 1);
  assert.equal(traceAggregate.items[0].usage.totalTokens, 170);
  assert.equal(traceAggregate.items[0].costDetail.costUsdNanos, 420000);

  const loops = await db.loops({ taskFingerprint: "npm-native-packaging" });
  assert.equal(loops.total, 1);
  assert.equal(loops.items[0].loopId, "loop-builder");
  assert.equal(loops.items[0].taskFingerprint, "npm-native-packaging");
  assert.equal(loops.items[0].traceCount, 1);
  assert.equal(loops.items[0].spanCount, 1);
  assert.deepEqual(loops.items[0].phases, ["verify"]);
  assert.equal(loops.items[0].fields.validation_status, "pass");

  const loopDetail = await db.loop("loop-builder");
  assert.equal(loopDetail.summary.loopId, "loop-builder");
  assert.equal(loopDetail.traces.length, 1);
  assert.equal(loopDetail.traces[0].externalTraceId, "builder-run");
  assert.equal(loopDetail.spans.length, 1);
  assert.equal(loopDetail.spans[0].externalSpanId, "builder-span");

  const hiddenLoop = await db.loop("loop-builder", { tenantId: 999 });
  assert.equal(hiddenLoop, null);

  const taskTraces = await db.taskTraces("npm-native-packaging", { validationStatus: "pass" });
  assert.equal(taskTraces.total, 1);
  assert.equal(taskTraces.items[0].externalTraceId, "builder-run");
  assert.equal(taskTraces.items[0].fields.loop_id, "loop-builder");

  const traceDiff = await db.traceDiff("run-uuid", "builder-run");
  assert.equal(traceDiff.left.externalTraceId, "run-uuid");
  assert.equal(traceDiff.right.externalTraceId, "builder-run");
  assert.equal(traceDiff.delta.spanCount, 0);
  assert.equal(traceDiff.trajectory.same, false);
  assert.ok(traceDiff.trajectory.left.signature.startsWith("fnv1a64:"));
  assert.ok(traceDiff.trajectory.right.steps.includes("tool:builder-tool|phase:verify|validator:npm_test"));
  assert.equal(traceDiff.steps.length, 1);
  assert.equal(traceDiff.steps[0].status, "changed");
  assert.ok(traceDiff.steps[0].changes.includes("toolName"));
  assert.equal(traceDiff.steps[0].right.toolName, "builder-tool");

  const traceDiffByObject = await db.traceDiff({ left: "run-uuid", right: "builder-run" });
  assert.equal(traceDiffByObject.right.externalTraceId, "builder-run");

  await assert.rejects(
    () => db.traceDiff("run-uuid", "builder-run", { tenantId: 999 }),
    /status=404/,
    "trace diff must respect tenant isolation",
  );

  const hiddenTraceAggregate = await db.traceAggregate({
    groupBy: ["skill"],
    filter: { attrs: { project_id: "agentic-data" } },
  }, { tenantId: 999 });
  assert.equal(hiddenTraceAggregate.spanTotal, 0);

  const annotation = await db.annotate({
    traceId: "builder-run",
    spanId: "builder-span",
    target: "span",
    label: "best_path",
    score: 920,
    reason: "manual review picked this path",
    source: "human",
    projectId: "agentic-data",
    skill: "builder",
  });
  assert.equal(annotation.externalTraceId, "builder-run");
  assert.equal(annotation.externalSpanId, "builder-span");
  assert.equal(annotation.attrs.project_id, "agentic-data");

  const annotations = await db.annotations({ traceId: "builder-run", label: "best_path", projectId: "agentic-data" });
  assert.equal(annotations.count, 1);
  assert.equal(annotations.items[0].label, "best_path");

  const datasetLink = await db.linkDatasetItem({
    datasetId: "best-path-regression",
    itemId: "case-1",
    traceId: "builder-run",
    spanId: "builder-span",
    snapshotId: "snap-builder",
    snapshotHash: "fnv1a64:builder",
    evalRunId: "eval-1",
    split: "train",
    label: "pass",
    score: 920,
    projectId: "agentic-data",
    skill: "builder",
  });
  assert.equal(datasetLink.datasetId, "best-path-regression");
  assert.equal(datasetLink.externalTraceId, "builder-run");

  const datasetLinks = await db.datasetAssociations({ datasetId: "best-path-regression", itemId: "case-1" });
  assert.equal(datasetLinks.count, 1);
  assert.equal(datasetLinks.items[0].snapshotHash, "fnv1a64:builder");

  const builderRepeat = createSpanEventBuilder({
    traceId: "builder-run-2",
    sessionId: "builder-session-2",
    projectId: "agentic-data",
    skill: "builder",
    mode: "auto",
    callSite: "builder.ts:2",
    taskFingerprint: "npm-native-packaging",
    loopId: "loop-builder",
    harnessVersion: "h1",
    validationStatus: "pass",
    stopReason: "goal_met",
    phase: "verify",
    validator: "npm test",
  });
  builderRepeat.startSpan({
    spanId: "builder-repeat-span",
    name: "builder repeat span",
    agentName: "builder-agent",
    toolName: "builder-tool",
    model: "qwen",
    provider: "qwen",
    inputText: "builder repeat 输入",
    inputTokens: 80,
    outputTokens: 20,
    totalTokens: 100,
    costUsdNanos: 300000,
  });
  builderRepeat.endSpan({ spanId: "builder-repeat-span", status: 0, durationNs: 8, outputText: "builder repeat 输出" });
  await builderRepeat.ingest(db);

  const trajectoryGroups = await db.trajectoryGroups({
    filter: { taskFingerprint: "npm-native-packaging" },
    sort: "best",
  });
  assert.equal(trajectoryGroups.total, 1);
  assert.equal(trajectoryGroups.traceTotal, 2);
  assert.equal(trajectoryGroups.spanTotal, 2);
  assert.equal(trajectoryGroups.items[0].traceCount, 2);
  assert.equal(trajectoryGroups.items[0].successCount, 2);
  assert.equal(trajectoryGroups.items[0].scores.annotation.avg, 920);
  assert.equal(trajectoryGroups.items[0].scores.dataset.avg, 920);
  assert.ok(trajectoryGroups.items[0].signature.startsWith("fnv1a64:"));
  assert.ok(trajectoryGroups.items[0].steps.includes("tool:builder-tool|phase:verify|validator:npm_test"));
  assert.equal(trajectoryGroups.items[0].examples.length, 2);

  const traceTrajectories = await db.traceTrajectories({
    filter: { taskFingerprint: "npm-native-packaging", projectId: "agentic-data" },
    limit: 10,
  });
  assert.equal(traceTrajectories.index, "materialized");
  assert.equal(traceTrajectories.total, 2);
  assert.ok(traceTrajectories.items[0].trajectory.signature.startsWith("fnv1a64:"));
  assert.ok(traceTrajectories.items.some((item) => item.trajectory.steps.includes("tool:builder-tool|phase:verify|validator:npm_test")));

  const hiddenTrajectoryGroups = await db.trajectoryGroups({
    filter: { taskFingerprint: "npm-native-packaging" },
  }, { tenantId: 999 });
  assert.equal(hiddenTrajectoryGroups.traceTotal, 0);

  const goldenPath = await db.createGoldenPath({
    sourceTraceId: "builder-run",
    taskFingerprint: "npm-native-packaging",
    score: 960,
    label: "fast packaging path",
    reason: "best observed route",
    source: "human",
    projectId: "agentic-data",
    skill: "builder",
  });
  assert.equal(goldenPath.status, "candidate");
  assert.equal(goldenPath.externalSourceTraceId, "builder-run");
  assert.match(goldenPath.trajectorySignature, /^fnv1a64:/);
  assert.ok(goldenPath.sourceTrajectory.steps.includes("tool:builder-tool|phase:verify|validator:npm_test"));
  assert.equal(goldenPath.attrs.project_id, "agentic-data");
  assert.equal(goldenPath.attrs.model, "qwen");
  assert.equal(goldenPath.evidenceSummary.source_trajectory_step_count, 1);
  assert.equal(goldenPath.evidenceSummary.source_status, "ok");

  const confirmedGoldenPath = await db.updateGoldenPathStatus(goldenPath.goldenPathId, {
    status: "confirmed",
    reason: "manual accept",
    source: "reviewer",
  });
  assert.equal(confirmedGoldenPath.status, "confirmed");
  assert.equal(confirmedGoldenPath.reason, "manual accept");

  const goldenPaths = await db.goldenPaths({
    taskFingerprint: "npm-native-packaging",
    status: "confirmed",
    projectId: "agentic-data",
  });
  assert.equal(goldenPaths.count, 1);

  const storageStats = await db.storageStats({
    filter: { taskFingerprint: "npm-native-packaging", projectId: "agentic-data" },
    groupBy: ["projectId", "validationStatus"],
  });
  assert.equal(storageStats.total.traceCount, 2);
  assert.equal(storageStats.total.metadata.annotations, 1);
  assert.equal(storageStats.total.metadata.datasetAssociations, 1);
  assert.equal(storageStats.total.metadata.goldenPaths, 1);
  assert.equal(storageStats.groups.length, 1);
  assert.equal(storageStats.groups[0].key.project_id, "agentic-data");
  assert.equal(storageStats.groups[0].key.validation_status, "pass");
  assert.ok(storageStats.total.bytes.estimatedBytes > 0);

  const retentionPlan = await db.retentionPlan({
    filter: { taskFingerprint: "npm-native-packaging", projectId: "agentic-data" },
    protect: { annotations: true, datasetAssociations: true, goldenPaths: true },
  });
  assert.equal(retentionPlan.dryRun, true);
  assert.equal(retentionPlan.candidates.traceCount, 2);
  assert.equal(retentionPlan.protected.traceCount, 1);
  assert.equal(retentionPlan.deletable.traceCount, 1);
  assert.equal(retentionPlan.applyResult, null);
  assert.ok(Object.values(retentionPlan.protectedReasons).some((reasons) => reasons.includes("goldenPath")));
  assert.ok(Object.values(retentionPlan.protectedReasons).some((reasons) => reasons.includes("annotation")));
  assert.ok(Object.values(retentionPlan.protectedReasons).some((reasons) => reasons.includes("datasetAssociation")));

  const adherence = await db.pathAdherence(goldenPath.goldenPathId, "builder-run-2");
  assert.equal(adherence.adherence, "followed");
  assert.equal(adherence.sameSignature, true);
  assert.equal(adherence.sourceAvailable, true);
  assert.equal(adherence.scores.commonStepCount, 1);
  assert.equal(adherence.missingSteps.length, 0);
  assert.equal(adherence.extraSteps.length, 0);
  assert.ok(adherence.traceTrajectory.steps.includes("tool:builder-tool|phase:verify|validator:npm_test"));

  const adherenceByObject = await db.pathAdherence({
    goldenPathId: goldenPath.goldenPathId,
    traceId: "builder-run-2",
  });
  assert.equal(adherenceByObject.adherence, "followed");

  const evidence = await db.goldenPathEvidence({
    goldenPathId: goldenPath.goldenPathId,
    candidateTraceId: "builder-run-2",
  });
  assert.equal(evidence.source.available, true);
  assert.equal(evidence.source.annotationCount, 1);
  assert.equal(evidence.source.datasetAssociationCount, 1);
  assert.equal(evidence.candidate.pathAdherence.adherence, "followed");
  assert.equal(evidence.candidate.traceDiff.trajectory.same, true);

  const sourceOnlyEvidence = await db.goldenPathEvidence(goldenPath.goldenPathId);
  assert.equal(sourceOnlyEvidence.source.available, true);
  assert.equal(sourceOnlyEvidence.candidate, null);

  const exportPage = await db.goldenPathExport({
    filter: {
      taskFingerprint: "npm-native-packaging",
      projectId: "agentic-data",
    },
  });
  assert.equal(exportPage.schemaVersion, "yitrace.golden_path_export.v1");
  assert.equal(exportPage.format, "jsonl");
  assert.equal(exportPage.count, 1);
  assert.equal(exportPage.items[0].recordType, "golden_path");
  assert.equal(exportPage.items[0].goldenPath.goldenPathId, goldenPath.goldenPathId);
  assert.equal(exportPage.items[0].source.annotationCount, 1);
  assert.equal(exportPage.items[0].source.datasetAssociationCount, 1);
  assert.ok(exportPage.jsonl.includes('"schemaVersion":"yitrace.golden_path_export.v1"'));

  const builderDrift = createSpanEventBuilder({
    traceId: "builder-run-3",
    sessionId: "builder-session-3",
    projectId: "agentic-data",
    skill: "builder",
    mode: "auto",
    callSite: "builder.ts:3",
    taskFingerprint: "npm-native-packaging",
    loopId: "loop-builder",
    harnessVersion: "h1",
    validationStatus: "pass",
    stopReason: "goal_met",
    phase: "verify",
    validator: "npm test",
  });
  builderDrift.startSpan({
    spanId: "builder-drift-span",
    name: "builder drift span",
    agentName: "builder-agent",
    toolName: "packager-tool",
    model: "qwen",
    provider: "qwen",
    inputText: "builder drift 输入",
    inputTokens: 90,
    outputTokens: 30,
    totalTokens: 120,
    costUsdNanos: 400000,
  });
  builderDrift.endSpan({ spanId: "builder-drift-span", status: 0, durationNs: 10, outputText: "builder drift 输出" });
  await builderDrift.ingest(db);

  const health = await db.goldenPathHealth({
    goldenPathId: goldenPath.goldenPathId,
    filter: { projectId: "agentic-data" },
    examples: 10,
  });
  assert.equal(health.window.includeSource, false);
  assert.equal(health.window.matchingTraceTotal, 2);
  assert.equal(health.counts.total, 2);
  assert.equal(health.counts.followed, 1);
  assert.equal(health.counts.deviated, 1);
  assert.equal(health.rates.usable, 0.5);
  assert.ok(health.examples.some((item) => item.adherence === "deviated"));

  const healthWithSource = await db.goldenPathHealth(goldenPath.goldenPathId, {
    filter: { projectId: "agentic-data" },
    includeSource: true,
    examples: 10,
  });
  assert.equal(healthWithSource.window.includeSource, true);
  assert.equal(healthWithSource.window.matchingTraceTotal, 3);
  assert.equal(healthWithSource.counts.followed, 2);

  const hiddenGoldenPaths = await db.goldenPaths({
    taskFingerprint: "npm-native-packaging",
  }, { tenantId: 999 });
  assert.equal(hiddenGoldenPaths.count, 0);
  await assert.rejects(
    () => db.pathAdherence(goldenPath.goldenPathId, "builder-run-2", { tenantId: 999 }),
    /status=404/,
  );
  await assert.rejects(
    () => db.goldenPathEvidence(goldenPath.goldenPathId, { tenantId: 999 }),
    /status=404/,
  );
  const hiddenExportPage = await db.goldenPathExport({
    filter: { taskFingerprint: "npm-native-packaging" },
  }, { tenantId: 999 });
  assert.equal(hiddenExportPage.count, 0);
  await assert.rejects(
    () => db.goldenPathHealth({ goldenPathId: goldenPath.goldenPathId }, { tenantId: 999 }),
    /status=404/,
  );

  const traceSearchByAnnotation = await db.traceSearch({
    filter: { annotation: { label: "best_path", source: "human", scoreMin: 900 } },
  });
  assert.equal(traceSearchByAnnotation.total, 1);
  assert.equal(traceSearchByAnnotation.items[0].externalSpanId, "builder-span");

  const traceSearchByDataset = await db.traceSearch({
    filter: { dataset: { datasetId: "best-path-regression", itemId: "case-1", evalRunId: "eval-1", scoreMin: 900 } },
  });
  assert.equal(traceSearchByDataset.total, 1);
  assert.equal(traceSearchByDataset.items[0].externalSpanId, "builder-span");

  const traceSearchByMissingAnnotation = await db.traceSearch({
    filter: { annotationLabel: "missing" },
  });
  assert.equal(traceSearchByMissingAnnotation.total, 0);

  const tracesByAnnotation = await db.traces({
    annotation: { label: "best_path", source: "human", scoreMin: 900 },
  });
  assert.equal(tracesByAnnotation.length, 1);
  assert.equal(tracesByAnnotation[0].external_trace_id, "builder-run");

  const sessionsByDataset = await db.sessions({
    dataset: { datasetId: "best-path-regression", itemId: "case-1", evalRunId: "eval-1" },
  });
  assert.equal(sessionsByDataset.items.length, 1);
  assert.equal(sessionsByDataset.items[0].externalSessionId, "builder-session");

  const tracesByMissingAnnotation = await db.traces({ annotationLabel: "missing" });
  assert.equal(tracesByMissingAnnotation.length, 0);

  const hiddenAnnotations = await db.annotations({ traceId: "builder-run" }, { tenantId: 999 });
  assert.equal(hiddenAnnotations.count, 0);

  const builderTrace = await db.trace("builder-run");
  assert.equal(builderTrace.spans[0].spanOrdinal, 0);
  assert.equal(builderTrace.spans[0].logEvents[0].eventOrdinal, 0);

  const builderSnapshot = await db.traceSnapshot("builder-run");
  assert.match(builderSnapshot.snapshotHash, /^fnv1a64:/);
  assert.equal(builderSnapshot.trace.spans[0].outputText.full, "builder 输出");

  const builderSpans = await db.spans("builder-run", { limit: 1 });
  assert.equal(builderSpans.total, 1);
  assert.equal(builderSpans.items[0].inputText.preview, "builder 输入");
  assert.equal(builderSpans.items[0].inputText.full, null);

  const builderSpanBatch = await db.spansBatch("builder-run", ["builder-span"], { includeFull: true });
  assert.equal(builderSpanBatch.items.length, 1);
  assert.equal(builderSpanBatch.items[0].inputText.full, "builder 输入");

  const hiddenFromSpoofedTenant = await db.traces({ tenantId: 999 });
  assert.equal(hiddenFromSpoofedTenant.length, 0);

  await db.close();

  const reopened = await YiTraceDB.open({ dataDir: dir, tenantId: 1 });
  const recovered = await reopened.traces();
  assert.equal(recovered.length, 5);
  assert.equal(recovered.find((t) => t.external_trace_id === "run-uuid")?.external_trace_id, "run-uuid");
  const recoveredAttrHits = await reopened.search({ text: "builder", filter: { attrs: { connection_ids: "conn-a" } } });
  assert.equal(recoveredAttrHits.length, 1);
  assert.equal(recoveredAttrHits[0].external_span_id, "builder-span");
  const recoveredAnnotations = await reopened.annotations({ traceId: "builder-run", projectId: "agentic-data" });
  assert.equal(recoveredAnnotations.count, 1);
  assert.equal(recoveredAnnotations.items[0].label, "best_path");
  const recoveredDatasetLinks = await reopened.datasetAssociations({ datasetId: "best-path-regression", itemId: "case-1" });
  assert.equal(recoveredDatasetLinks.count, 1);
  assert.equal(recoveredDatasetLinks.items[0].snapshotHash, "fnv1a64:builder");
  const recoveredGoldenPaths = await reopened.goldenPaths({ taskFingerprint: "npm-native-packaging", status: "confirmed" });
  assert.equal(recoveredGoldenPaths.count, 1);
  const recoveredTraceSearchByDataset = await reopened.traceSearch({
    filter: { datasetId: "best-path-regression", datasetLabel: "pass" },
  });
  assert.equal(recoveredTraceSearchByDataset.total, 1);
  const recoveredSessionsByDataset = await reopened.sessions({
    datasetId: "best-path-regression",
    datasetLabel: "pass",
  });
  assert.equal(recoveredSessionsByDataset.items.length, 1);
  await reopened.close();

  const retentionDir = join(dir, "retention-apply");
  const retentionDb = await YiTraceDB.open({ dataDir: retentionDir, tenantId: 1 });
  await retentionDb.ingest([
    {
      trace_id: "retention-old",
      span_id: "retention-old-span",
      ts: 10,
      seq: 1,
      event_type: 2,
      ext_span_id: "retention-old-span",
      input_text: "old trace",
      attrs: { project_id: "retention-node" },
    },
    {
      trace_id: "retention-new",
      span_id: "retention-new-span",
      ts: 200,
      seq: 1,
      event_type: 2,
      ext_span_id: "retention-new-span",
      input_text: "new trace",
      attrs: { project_id: "retention-node" },
    },
  ]);
  await retentionDb.flush();
  const appliedRetention = await retentionDb.applyRetention({
    filter: { projectId: "retention-node" },
    deleteBeforeTs: 100,
    compact: true,
    requestedBy: "node-test",
    reason: "ttl cleanup",
  });
  assert.equal(appliedRetention.applied, true);
  assert.equal(appliedRetention.applyResult.deletedTraceCount, 1);
  assert.equal(appliedRetention.applyResult.deletedSegmentRowCount, 1);
  assert.equal(appliedRetention.compact.requested, true);
  assert.equal(appliedRetention.compactResult.compactedSegmentCount, 1);
  assert.equal(appliedRetention.compactResult.droppedDeletedRowCount, 1);
  assert.equal(appliedRetention.compactResult.rewrittenLiveRowCount, 1);
  assert.equal(appliedRetention.audit.source, "node-test");
  assert.equal(appliedRetention.audit.reason, "ttl cleanup");
  assert.equal(appliedRetention.audit.counts.deletedTraceCount, 1);
  assert.equal(appliedRetention.audit.traceIds.deleted.length, 1);
  const retentionAudits = await retentionDb.retentionAudits({ filter: { source: "node-test" } });
  assert.equal(retentionAudits.total, 1);
  assert.equal(retentionAudits.items[0].auditId, appliedRetention.audit.auditId);
  const retentionAfter = await retentionDb.traceSearch({ filter: { projectId: "retention-node" } });
  assert.equal(retentionAfter.total, 1);
  assert.equal(retentionAfter.items[0].externalTraceId, "retention-new");
  await retentionDb.close();

  const reopenedRetentionDb = await YiTraceDB.open({ dataDir: retentionDir, tenantId: 1 });
  const recoveredRetentionAudits = await reopenedRetentionDb.retentionAudits({ source: "node-test" });
  assert.equal(recoveredRetentionAudits.total, 1);
  assert.equal(recoveredRetentionAudits.items[0].counts.deletedTraceCount, 1);
  await reopenedRetentionDb.close();

  const retentionPolicyDir = join(dir, "retention-policy");
  const retentionPolicyDb = await YiTraceDB.open({ dataDir: retentionPolicyDir, tenantId: 1 });
  await retentionPolicyDb.ingest([
    {
      trace_id: "retention-policy-old",
      span_id: "retention-policy-old-span",
      ts: 10,
      seq: 1,
      event_type: 2,
      ext_span_id: "retention-policy-old-span",
      input_text: "old policy trace",
      attrs: { project_id: "retention-policy-node" },
    },
    {
      trace_id: "retention-policy-new",
      span_id: "retention-policy-new-span",
      ts: 200,
      seq: 1,
      event_type: 2,
      ext_span_id: "retention-policy-new-span",
      input_text: "new policy trace",
      attrs: { project_id: "retention-policy-node" },
    },
  ]);
  await retentionPolicyDb.flush();
  const retentionPolicy = await retentionPolicyDb.createRetentionPolicy({
    name: "node-retention-policy",
    intervalNs: 1000,
    nextRunAtNs: 100,
    source: "node-policy-test",
    reason: "ttl cleanup",
    query: {
      filter: { attrs: { project_id: "retention-policy-node" } },
      olderThanNs: 50,
      compact: true,
    },
  });
  assert.equal(retentionPolicy.name, "node-retention-policy");
  assert.equal(retentionPolicy.enabled, true);
  assert.equal(retentionPolicy.intervalNs, "1000");
  assert.equal(retentionPolicy.query.olderThanNs, 50);

  const retentionPolicies = await retentionPolicyDb.retentionPolicies({ policyName: "node-retention-policy" });
  assert.equal(retentionPolicies.total, 1);
  assert.equal(retentionPolicies.items[0].policyId, retentionPolicy.policyId);

  const retentionRun = await retentionPolicyDb.runRetentionPolicies({ nowNs: 100 });
  assert.equal(retentionRun.ran, 1);
  assert.equal(retentionRun.failed, 0);
  assert.equal(retentionRun.items.length, 1);
  assert.equal(retentionRun.items[0].ok, true);
  assert.equal(retentionRun.items[0].result.applied, true);
  assert.equal(retentionRun.items[0].result.applyResult.deletedTraceCount, 1);
  assert.equal(retentionRun.items[0].result.audit.source, "node-policy-test");
  assert.equal(retentionRun.items[0].policy.lastRunAtNs, "100");
  assert.equal(retentionRun.items[0].policy.nextRunAtNs, "1100");

  const policyRetentionAfter = await retentionPolicyDb.traceSearch({
    filter: { attrs: { project_id: "retention-policy-node" } },
  });
  assert.equal(policyRetentionAfter.total, 1);
  assert.equal(policyRetentionAfter.items[0].externalTraceId, "retention-policy-new");
  const policyRetentionAudits = await retentionPolicyDb.retentionAudits({ filter: { source: "node-policy-test" } });
  assert.equal(policyRetentionAudits.total, 1);
  assert.equal(policyRetentionAudits.items[0].counts.deletedTraceCount, 1);
  await retentionPolicyDb.close();

  const reopenedRetentionPolicyDb = await YiTraceDB.open({ dataDir: retentionPolicyDir, tenantId: 1 });
  const recoveredRetentionPolicies = await reopenedRetentionPolicyDb.retentionPolicies({ name: "node-retention-policy" });
  assert.equal(recoveredRetentionPolicies.total, 1);
  assert.equal(recoveredRetentionPolicies.items[0].lastRunAtNs, "100");
  assert.equal(recoveredRetentionPolicies.items[0].nextRunAtNs, "1100");
  const recoveredPolicyRetentionAudits = await reopenedRetentionPolicyDb.retentionAudits({ source: "node-policy-test" });
  assert.equal(recoveredPolicyRetentionAudits.total, 1);
  assert.equal(recoveredPolicyRetentionAudits.items[0].counts.deletedTraceCount, 1);
  await reopenedRetentionPolicyDb.close();
} finally {
  await rm(dir, { recursive: true, force: true });
}
