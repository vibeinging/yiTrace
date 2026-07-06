import assert from "node:assert/strict";
import test from "node:test";

import { mockApi } from "../.test-dist/src/api/mock.js";

test("mock sessions are deterministic and cursor paged", async () => {
  const first = await mockApi.listSessions({ limit: 3 });
  const again = await mockApi.listSessions({ limit: 3 });
  const second = await mockApi.listSessions({ cursor: first.nextCursor, limit: 3 });

  assert.equal(first.items.length, 3);
  assert.equal(first.nextCursor, "3");
  assert.deepEqual(first, again);
  assert.equal(second.items.length, 3);
  assert.notEqual(second.items[0].sessionId, first.items[0].sessionId);
});

test("mock filter only returns matching sessions", async () => {
  const page = await mockApi.listSessions({ limit: 5, filter: "盗刷" });

  assert.ok(page.items.length > 0);
  assert.ok(page.items.every((item) => item.title.includes("盗刷") || item.sessionId.includes("盗刷")));
});

test("mock trace keeps summary and span tree consistent", async () => {
  const sessions = await mockApi.listSessions({ limit: 1 });
  const trace = await mockApi.getTrace(sessions.items[0].firstTraceId);

  assert.equal(trace.spans.length, trace.summary.spanCount);
  assert.equal(trace.spans[0].parentId, null);
  assert.equal(trace.spans[0].depth, 0);
  assert.ok(trace.spans.slice(1).every((span) => span.parentId !== null && span.depth > 0));
});
