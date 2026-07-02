import { execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const { version } = JSON.parse(readFileSync(join(root, "package.json"), "utf8"));
const platformPackage =
  {
    "darwin:arm64": "darwin-arm64",
    "darwin:x64": "darwin-x64",
    "linux:arm64": "linux-arm64-gnu",
    "linux:x64": "linux-x64-gnu",
    "win32:x64": "win32-x64-msvc",
  }[`${process.platform}:${process.arch}`] ?? null;

if (!platformPackage) {
  throw new Error(`Unsupported verify platform: ${process.platform}/${process.arch}`);
}

const rootTarball = join(root, "dist", `yitrace-db-${version}.tgz`);
const platformTarball = join(root, "dist", `yitrace-db-${platformPackage}-${version}.tgz`);

for (const file of [rootTarball, platformTarball]) {
  if (!existsSync(file)) {
    throw new Error(`Missing local package artifact: ${file}`);
  }
}

const consumer = mkdtempSync(join(tmpdir(), "yitrace-pack-consumer-"));

try {
  writeFileSync(join(consumer, "package.json"), JSON.stringify({ type: "module", private: true }, null, 2));
  execFileSync("npm", ["install", "--cache", "/tmp/yitrace-npm-cache", rootTarball, platformTarball], {
    cwd: consumer,
    stdio: "inherit",
  });

  writeFileSync(
    join(consumer, "verify-esm.mjs"),
    `
import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { createRequire } from "node:module";
import { join, dirname } from "node:path";
import { YiTraceDB, createSpanEventBuilder } from "@yitrace/db";

const require = createRequire(import.meta.url);
const platformPkgJson = require.resolve("@yitrace/db-${platformPackage}/package.json");
const platformDir = dirname(platformPkgJson);
assert.ok(existsSync(join(platformDir, "yitrace-db.${platformPackage}.node")));
assert.equal(typeof YiTraceDB.open, "function");

const dir = await mkdtemp(join(tmpdir(), "yitrace-pack-esm-"));
const db = await YiTraceDB.open({ dataDir: dir, tenantId: 1 });
try {
  const builder = createSpanEventBuilder({
    traceId: "esm-run",
    sessionId: "esm-session",
    attrs: { project_id: "agentic-data", skill: "pack", mode: "esm", call_site: "verify-esm.mjs" },
  });
  builder.startSpan({ spanId: "esm-span", agentName: "consumer", inputText: "esm 盗刷验证" });
  builder.endSpan({ spanId: "esm-span", status: 0, durationNs: 5, outputText: "ok" });
  await builder.ingest(db);

  const hits = await db.search({ text: "盗刷", filter: { attrs: { project_id: "agentic-data", skill: "pack" } } });
  assert.equal(hits.length, 1);

  const sessions = await db.sessions({ attrs: { project_id: "agentic-data", skill: "pack", mode: "esm" } });
  assert.equal(sessions.items.length, 1);
  assert.equal(sessions.items[0].externalSessionId, "esm-session");
} finally {
  await db.close();
  await rm(dir, { recursive: true, force: true });
}
`,
  );

  writeFileSync(
    join(consumer, "verify-cjs.cjs"),
    `
const assert = require("node:assert/strict");
const { mkdtemp, rm } = require("node:fs/promises");
const { tmpdir } = require("node:os");
const { join } = require("node:path");
const { YiTraceDB, createSpanEventBuilder } = require("@yitrace/db");

(async () => {
  assert.equal(typeof YiTraceDB.open, "function");
  const dir = await mkdtemp(join(tmpdir(), "yitrace-pack-cjs-"));
  const db = await YiTraceDB.open({ dataDir: dir, tenantId: 1 });
  try {
    const builder = createSpanEventBuilder({
      traceId: "cjs-run",
      sessionId: "cjs-session",
      attrs: { project_id: "agentic-data", skill: "pack-cjs", mode: "cjs" },
    });
    builder.startSpan({ spanId: "cjs-span", inputText: "cjs 盗刷验证" });
    builder.endSpan({ spanId: "cjs-span", status: 0, durationNs: 7, outputText: "ok" });
    await builder.ingest(db);
    const hits = await db.search({ text: "盗刷", filter: { attrs: { project_id: "agentic-data", skill: "pack-cjs" } } });
    assert.equal(hits.length, 1);
  } finally {
    await db.close();
    await rm(dir, { recursive: true, force: true });
  }
})().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
`,
  );

  execFileSync("node", ["verify-esm.mjs"], { cwd: consumer, stdio: "inherit" });
  execFileSync("node", ["verify-cjs.cjs"], { cwd: consumer, stdio: "inherit" });
  console.log(`Verified @yitrace/db local tarballs in clean consumer: ${consumer}`);
} finally {
  rmSync(consumer, { recursive: true, force: true });
}
