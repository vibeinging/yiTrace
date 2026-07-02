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
    attrs: { project_id: "agentic-data", skill: "builder", mode: "auto", call_site: "builder.ts:1" },
  });
  builder.startSpan({
    spanId: "builder-span",
    name: "builder span",
    agentName: "builder-agent",
    toolName: "builder-tool",
    model: "qwen",
    inputText: "builder 输入",
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

  const missedSessions = await db.sessions({ projectId: "agentic-data", skill: "missing" });
  assert.equal(missedSessions.items.length, 0);

  const hiddenFromSpoofedTenant = await db.traces({ tenantId: 999 });
  assert.equal(hiddenFromSpoofedTenant.length, 0);

  await db.close();

  const reopened = await YiTraceDB.open({ dataDir: dir, tenantId: 1 });
  const recovered = await reopened.traces();
  assert.equal(recovered.length, 3);
  assert.equal(recovered.find((t) => t.external_trace_id === "run-uuid")?.external_trace_id, "run-uuid");
  await reopened.close();
} finally {
  await rm(dir, { recursive: true, force: true });
}
