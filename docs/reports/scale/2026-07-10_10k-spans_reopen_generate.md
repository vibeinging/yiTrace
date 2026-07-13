# yiTrace Scale Bench Report

- generatedAtUnix: 1783656512
- phase: generate
- queryProcessMode: not-run
- seed: 11400714819323198485
- foldedSpans: 10000
- wireEvents: 22774
- traces: 1021
- sessions: 256
- loops: 128
- logEvents: 2694
- duplicateEvents: 90
- incompleteSpans: 10
- requestedQueriesPerEndpoint: 200
- dataDir: /var/folders/j6/3tm8zfn56fn47lfvwstq5y100000gp/T//yitrace-scale.pksOc2
- openAndRecoverSeconds: 0.094
- rssAfterOpenKiB: 287568
- rssAfterQueriesKiB: n/a

## Data Shape

| Item | Value |
|---|---:|
| total bytes | 58370443 |
| bytes / folded span | 5837.0 |
| WAL bytes | 19555448 |
| segment bytes | 19553771 |
| sidecar / index bytes | 19260635 |
| manifest bytes | 372 |
| other bytes | 217 |
| segment files | 6 |

## Write Path

| Step | Count | Seconds | Rate |
|---|---:|---:|---:|
| ingest folded spans | 10000 | 3.419 | 2925 spans/s |
| ingest wire events | 22774 | 3.419 | 6662 events/s |
| flush remaining memtable | 22774 | 0.521 | - |
| RSS after ingest KiB | 287552 | - | - |
