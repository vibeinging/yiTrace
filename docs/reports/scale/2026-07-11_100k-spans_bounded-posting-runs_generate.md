# yiTrace Scale Bench Report

- generatedAtUnix: 1783735658
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
- requestedQueriesPerEndpoint: 1
- dataDir: /var/folders/j6/3tm8zfn56fn47lfvwstq5y100000gp/T//yitrace-scale.ame0B7
- openAndRecoverSeconds: 0.001
- openAndRecoverMillis: 1.228
- rssAfterOpenKiB: 3812
- rssAfterQueriesKiB: n/a

## Data Shape

| Item | Value |
|---|---:|
| total bytes | 615475910 |
| bytes / folded span | 6154.8 |
| WAL bytes | 198010448 |
| segment bytes | 197993731 |
| sidecar / index bytes | 219468532 |
| manifest bytes | 2972 |
| other bytes | 227 |
| segment files | 56 |

## Write Path

| Step | Count | Seconds | Rate |
|---|---:|---:|---:|
| ingest folded spans | 100000 | 19.668 | 5084 spans/s |
| ingest wire events | 227735 | 19.668 | 11579 events/s |
| flush remaining memtable | 227735 | 37.834 | - |
| RSS after ingest KiB | 844884 | - | - |
