import { YiTraceDB } from "../index.js";

function memoryCandidateFromExport(record) {
  const goldenPath = record.goldenPath ?? {};
  const source = record.source ?? {};
  const trajectory = source.trajectory ?? goldenPath.sourceTrajectory ?? null;

  return {
    kind: "agent_path_memory_candidate",
    source: "yitrace.golden_path_export.v1",
    id: goldenPath.goldenPathId,
    scope: {
      taskFingerprint: goldenPath.taskFingerprint,
      projectId: goldenPath.attrs?.project_id,
      skill: goldenPath.attrs?.skill,
      mode: goldenPath.attrs?.mode,
      model: goldenPath.attrs?.model,
      provider: goldenPath.attrs?.provider,
      toolVersion: goldenPath.attrs?.tool_version,
    },
    confidenceEvidence: {
      score: goldenPath.score,
      label: goldenPath.label,
      status: goldenPath.status,
      reason: goldenPath.reason,
      evidenceSummary: goldenPath.evidenceSummary ?? {},
      sourceAvailable: source.available,
      sourceRetained: Boolean(trajectory?.steps?.length),
    },
    trajectory,
    sourceTrace: {
      traceId: goldenPath.sourceTraceId,
      externalTraceId: goldenPath.externalSourceTraceId,
      snapshotId: goldenPath.snapshotId,
      snapshotHash: goldenPath.snapshotHash,
    },
  };
}

function regressionItemFromExport(record) {
  const candidate = memoryCandidateFromExport(record);
  return {
    datasetId: "golden-path-regression",
    itemId: `golden-path:${candidate.id}`,
    taskFingerprint: candidate.scope.taskFingerprint,
    expectedTrajectorySignature: candidate.trajectory?.signature,
    expectedSteps: candidate.trajectory?.steps ?? [],
    evidence: candidate.confidenceEvidence,
    sourceTrace: candidate.sourceTrace,
  };
}

async function main() {
  const dataDir = process.env.YITRACE_DATA_DIR ?? "./data";
  const tenantId = process.env.YITRACE_TENANT_ID
    ? Number(process.env.YITRACE_TENANT_ID)
    : undefined;

  const db = await YiTraceDB.open({ dataDir, tenantId });
  try {
    const page = await db.goldenPathExport({
      filter: {
        status: "confirmed",
        projectId: process.env.YITRACE_PROJECT_ID,
        taskFingerprint: process.env.YITRACE_TASK_FINGERPRINT,
      },
      limit: 100,
    });

    const memoryCandidates = page.items.map(memoryCandidateFromExport);
    const regressionItems = page.items.map(regressionItemFromExport);

    console.log(
      JSON.stringify(
        {
          schemaVersion: page.schemaVersion,
          exportedCount: page.count,
          memoryCandidates,
          regressionItems,
        },
        null,
        2,
      ),
    );
  } finally {
    await db.close();
  }
}

await main();
