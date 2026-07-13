# yiTrace Scale Bench Report

- generatedAtUnix: 1783736339
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
- requestedQueriesPerEndpoint: 1
- dataDir: /var/folders/j6/3tm8zfn56fn47lfvwstq5y100000gp/T//yitrace-scale.fFFXBI
- openAndRecoverSeconds: 0.001
- openAndRecoverMillis: 1.202
- rssAfterOpenKiB: 3792
- rssAfterQueriesKiB: n/a

## Data Shape

| Item | Value |
|---|---:|
| total bytes | 6170979941 |
| bytes / folded span | 6171.0 |
| WAL bytes | 1989523860 |
| segment bytes | 1989357225 |
| sidecar / index bytes | 2192069646 |
| manifest bytes | 28972 |
| other bytes | 238 |
| segment files | 556 |

## Write Path

| Step | Count | Seconds | Rate |
|---|---:|---:|---:|
| ingest folded spans | 1000000 | 199.348 | 5016 spans/s |
| ingest wire events | 2277482 | 199.348 | 11425 events/s |
| flush remaining memtable | 2277482 | 312.728 | - |
| RSS after ingest KiB | 7742020 | - | - |
