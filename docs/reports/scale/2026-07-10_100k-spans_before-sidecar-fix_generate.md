# yiTrace Scale Bench Report

- generatedAtUnix: 1783656977
- phase: generate
- queryProcessMode: not-run
- seed: 11400714819323198485
- foldedSpans: 100000
- wireEvents: 227735
- traces: 9862
- sessions: 2466
- loops: 1233
- logEvents: 26935
- duplicateEvents: 900
- incompleteSpans: 100
- requestedQueriesPerEndpoint: 100
- dataDir: /var/folders/j6/3tm8zfn56fn47lfvwstq5y100000gp/T//yitrace-scale.jShTeG
- openAndRecoverSeconds: 0.094
- rssAfterOpenKiB: 1144524
- rssAfterQueriesKiB: n/a

## Data Shape

| Item | Value |
|---|---:|
| total bytes | 589473858 |
| bytes / folded span | 5894.7 |
| WAL bytes | 198010408 |
| segment bytes | 197993731 |
| sidecar / index bytes | 193466520 |
| manifest bytes | 2972 |
| other bytes | 227 |
| segment files | 56 |

## Write Path

| Step | Count | Seconds | Rate |
|---|---:|---:|---:|
| ingest folded spans | 100000 | 164.994 | 606 spans/s |
| ingest wire events | 227735 | 164.994 | 1380 events/s |
| flush remaining memtable | 227735 | 5.106 | - |
| RSS after ingest KiB | 1144504 | - | - |
