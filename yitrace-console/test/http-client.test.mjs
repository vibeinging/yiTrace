import assert from "node:assert/strict";
import test from "node:test";

import { createHttpApi } from "../.test-dist/src/api/http-client.js";

function jsonResponse(body, init = {}) {
  return new Response(JSON.stringify(body), {
    status: init.status ?? 200,
    statusText: init.statusText ?? "OK",
    headers: { "content-type": "application/json" },
  });
}

test("listSessions sends query, auth and tenant headers", async () => {
  const calls = [];
  const api = createHttpApi({
    base: "/api",
    apiToken: "secret",
    tenantId: () => "42",
    fetchImpl: async (url, init) => {
      calls.push({ url, init });
      return jsonResponse({ items: [], nextCursor: null, total: 0 });
    },
  });

  await api.listSessions({ cursor: "10", limit: 25, filter: "盗刷" });

  assert.equal(calls.length, 1);
  assert.equal(calls[0].url, "/api/sessions?cursor=10&limit=25&filter=%E7%9B%97%E5%88%B7");
  assert.deepEqual(calls[0].init.headers, {
    accept: "application/json",
    authorization: "Bearer secret",
    "x-tenant-id": "42",
  });
});

test("path params are encoded before fetch", async () => {
  let seenUrl = "";
  const api = createHttpApi({
    fetchImpl: async (url) => {
      seenUrl = String(url);
      return jsonResponse({ id: "span/1" });
    },
  });

  await api.getSpanDetail("trace/a b", "span/1");

  assert.equal(seenUrl, "/v1/traces/trace%2Fa%20b/spans/span%2F1");
});

test("searchSpans maps engine snake_case hits into console status model", async () => {
  let postedBody = null;
  const api = createHttpApi({
    fetchImpl: async (_url, init) => {
      postedBody = JSON.parse(String(init.body));
      return jsonResponse([
        { trace_id: 1, span_id: 11, score: 0.9, status: null, agent_name: "风控", logs: ["命中一"] },
        { trace_id: 2, span_id: 22, score: 0.5, status: 0, agent_name: null, logs: [] },
        { trace_id: 3, span_id: 33, score: 0.1, status: 7, agent_name: "工具", logs: ["失败"] },
      ]);
    },
  });

  const hits = await api.searchSpans("盗刷", 3);

  assert.deepEqual(postedBody, { text: "盗刷", k: 3 });
  assert.deepEqual(
    hits.map((h) => ({ traceId: h.traceId, spanId: h.spanId, status: h.status, agentName: h.agentName, snippet: h.snippet })),
    [
      { traceId: "1", spanId: "11", status: "ok", agentName: "风控", snippet: "命中一" },
      { traceId: "2", spanId: "22", status: "ok", agentName: undefined, snippet: undefined },
      { traceId: "3", spanId: "33", status: "error", agentName: "工具", snippet: "失败" },
    ],
  );
});

test("non-2xx responses reject with status text", async () => {
  const api = createHttpApi({
    fetchImpl: async () => jsonResponse({}, { status: 503, statusText: "Service Unavailable" }),
  });

  await assert.rejects(() => api.getTrace("trace-1"), /503 Service Unavailable/);
});
