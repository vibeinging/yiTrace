# yiTrace Scale Bench Report

- generatedAtUnix: 1783667333
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
- dataDir: /var/folders/j6/3tm8zfn56fn47lfvwstq5y100000gp/T//yitrace-scale.JMTT3I
- openAndRecoverSeconds: 0.102
- rssAfterOpenKiB: 48460
- rssAfterQueriesKiB: n/a

## Data Shape

| Item | Value |
|---|---:|
| total bytes | 5920012808 |
| bytes / folded span | 5920.0 |
| WAL bytes | 1989523860 |
| segment bytes | 1989357225 |
| sidecar / index bytes | 1941102513 |
| manifest bytes | 28972 |
| other bytes | 238 |
| segment files | 556 |

## Write Path

| Step | Count | Seconds | Rate |
|---|---:|---:|---:|
| ingest folded spans | 1000000 | 218.490 | 4577 spans/s |
| ingest wire events | 2277482 | 218.490 | 10424 events/s |
| flush remaining memtable | 2277482 | 9.786 | - |
| RSS after ingest KiB | 6986608 | - | - |
