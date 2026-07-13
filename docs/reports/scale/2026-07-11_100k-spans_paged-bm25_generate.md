# yiTrace Scale Bench Report

- generatedAtUnix: 1783734011
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
- dataDir: /var/folders/j6/3tm8zfn56fn47lfvwstq5y100000gp/T//yitrace-scale.5a5aZ8
- openAndRecoverSeconds: 0.001
- openAndRecoverMillis: 1.063
- rssAfterOpenKiB: 3744
- rssAfterQueriesKiB: n/a

## Data Shape

| Item | Value |
|---|---:|
| total bytes | 589874814 |
| bytes / folded span | 5898.7 |
| WAL bytes | 198010448 |
| segment bytes | 197993731 |
| sidecar / index bytes | 193867436 |
| manifest bytes | 2972 |
| other bytes | 227 |
| segment files | 56 |

## Write Path

| Step | Count | Seconds | Rate |
|---|---:|---:|---:|
| ingest folded spans | 100000 | 19.090 | 5238 spans/s |
| ingest wire events | 227735 | 19.090 | 11929 events/s |
| flush remaining memtable | 227735 | 19.383 | - |
| RSS after ingest KiB | 832232 | - | - |
