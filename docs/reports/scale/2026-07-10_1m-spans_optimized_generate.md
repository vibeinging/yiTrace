# yiTrace Scale Bench Report

- generatedAtUnix: 1783662684
- phase: generate
- queryProcessMode: not-run
- seed: 11400714819323198485
- foldedSpans: 1000000
- wireEvents: 2277482
- traces: 98205
- sessions: 24552
- loops: 12276
- logEvents: 269482
- duplicateEvents: 9000
- incompleteSpans: 1000
- requestedQueriesPerEndpoint: 200
- dataDir: /var/folders/j6/3tm8zfn56fn47lfvwstq5y100000gp/T//yitrace-scale.5ludau
- openAndRecoverSeconds: 0.102
- rssAfterOpenKiB: 48356
- rssAfterQueriesKiB: n/a

## Data Shape

| Item | Value |
|---|---:|
| total bytes | 5920012768 |
| bytes / folded span | 5920.0 |
| WAL bytes | 1989523820 |
| segment bytes | 1989357225 |
| sidecar / index bytes | 1941102513 |
| manifest bytes | 28972 |
| other bytes | 238 |
| segment files | 556 |

## Write Path

| Step | Count | Seconds | Rate |
|---|---:|---:|---:|
| ingest folded spans | 1000000 | 219.594 | 4554 spans/s |
| ingest wire events | 2277482 | 219.594 | 10371 events/s |
| flush remaining memtable | 2277482 | 11.226 | - |
| RSS after ingest KiB | 7870104 | - | - |
