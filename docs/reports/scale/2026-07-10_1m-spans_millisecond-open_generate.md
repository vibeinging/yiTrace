# yiTrace Scale Bench Report

- generatedAtUnix: 1783690537
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
- requestedQueriesPerEndpoint: 0
- dataDir: /tmp/yitrace-scale-1m-lazy-data
- openAndRecoverSeconds: 0.001
- rssAfterOpenKiB: 3764
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
| ingest folded spans | 1000000 | 197.211 | 5071 spans/s |
| ingest wire events | 2277482 | 197.211 | 11548 events/s |
| flush remaining memtable | 2277482 | 9.005 | - |
| RSS after ingest KiB | 7116668 | - | - |
