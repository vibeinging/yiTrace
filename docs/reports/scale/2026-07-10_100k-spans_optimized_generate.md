# yiTrace Scale Bench Report

- generatedAtUnix: 1783661852
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
- dataDir: /var/folders/j6/3tm8zfn56fn47lfvwstq5y100000gp/T//yitrace-scale.s6Cq9d
- openAndRecoverSeconds: 0.106
- rssAfterOpenKiB: 48388
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
| ingest folded spans | 100000 | 21.770 | 4594 spans/s |
| ingest wire events | 227735 | 21.770 | 10461 events/s |
| flush remaining memtable | 227735 | 0.721 | - |
| RSS after ingest KiB | 924632 | - | - |
