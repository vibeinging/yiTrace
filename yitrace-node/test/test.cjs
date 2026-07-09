const assert = require("node:assert/strict");
const { mkdtemp, rm } = require("node:fs/promises");
const { tmpdir } = require("node:os");
const { join } = require("node:path");
const { YiTraceDB, createSpanEventBuilder } = require("../index.cjs");

(async () => {
  const dir = await mkdtemp(join(tmpdir(), "yitrace-node-cjs-"));
  const db = await YiTraceDB.open({
    dataDir: dir,
    tenantId: 1,
    dimensions: 2,
    embedder: async (text) => (String(text).includes("commonjs") ? [0, 0] : [9, 0]),
  });

  try {
    const builder = createSpanEventBuilder({ traceId: "cjs-builder" });
    builder.startSpan({ spanId: "cjs-span", inputText: "commonjs builder 盗刷" });
    builder.endSpan({ spanId: "cjs-span", status: 0, durationNs: 1, outputText: "ok" });
    assert.equal(builder.events().length, 2);

    await db.ingest([
      {
        trace_id: 11,
        span_id: 22,
        ts: 1,
        seq: 1,
        event_type: 2,
        ext_span_id: "cjs-11-22",
        logs: ["commonjs 盗刷"],
      },
    ]);

    const hits = await db.search({ text: "盗刷", k: 3 });
    assert.equal(hits.length, 1);
    assert.equal(String(hits[0].trace_id), "11");

    await db.indexEmbedding({ traceId: 11, spanId: 22, text: "commonjs vector" });
    const similar = await db.search({ text: "commonjs query", mode: "semantic", k: 3 });
    assert.equal(String(similar[0].trace_id), "11");
  } finally {
    await db.close();
    await rm(dir, { recursive: true, force: true });
  }
})().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
