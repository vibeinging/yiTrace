# yiTrace

一台**单机、单目录、零外部依赖**的 AI Agent 可观测性数据库引擎。用 Rust 自研，把 Agent（多轮对话、调工具、多 agent 协作）跑出来的 trace 灌进来，提供 trace 还原、中文检索、带过滤的语义召回、成本归因和评测闭环。

> 状态：**能编译、能跑、测试全绿的验证级骨架**（引擎 94 · 列式段 7 · Python/TS SDK 各 8）。核心数据通路与检索能力已用代码验证，部分内核（生产级分词 / ANN / 查询执行）仍是可替换的占位实现，详见 [当前状态](#当前状态)。

---

## 特性

- **自研存储引擎**：append-only 事件 + 读时折叠（merge-on-read）、不可变段、写时复制 manifest、快照隔离。
- **确定性 event_id**：事件 id = 内容哈希，跨 Python / TypeScript / 引擎逐字节一致。重传、崩溃重放都只算一次，token/成本不重复计数。
- **中文检索**：自研倒排 + BM25，中文走无词典的 CJK bigram 分词，非连续多概念词也能按相关性召回排序。
- **带过滤语义召回**：向量 ANN 把过滤条件下推进图搜索（in-graph filtering），稀疏过滤下召回不塌。
- **混合检索**：关键词 + 语义两路用 RRF 融合。
- **列式段存储**：可选接入 [Vortex](https://github.com/spiraldb/vortex)，谓词下推 + 投影下推已跑通（聚合/成本查询跳过大文本列）。
- **崩溃安全**：WAL（fsync）、段文件、manifest、向量索引全部落盘，进程崩了重启数据和索引自动重建。
- **生态入口**：原生接 OTLP / OpenInference（OTel GenAI `gen_ai.*`、Arize `llm.*`），已埋点的应用不改一行即可灌入。
- **零依赖骨架**：引擎主体只用 Rust 标准库，`cargo test --offline` 离线即过；大依赖（Vortex 等）隔离在独立 crate。

---

## 快速开始

需要 Rust 1.80+。

```bash
cd yitrace-engine

# 跑全部测试（含并发压测 + socket HTTP 往返 + 带过滤 ANN 召回实测 + 重启不丢）
cargo test --offline

# 跑可运行 demo：灌几条假 trace → 折叠读出完整 trace → 中文搜「盗刷」→ 向量找相似 → 混合召回
cargo run -p yt-engine --example demo --offline

# 起 HTTP 摄入/查询服务（8 线程池）
cargo run -p yt-engine --example server
```

服务起来后：

```bash
# 摄入（SDK 线格式 JSON 批）
curl -XPOST localhost:7878/v1/ingest \
  -d '[{"trace_id":7,"span_id":1,"ts":1,"seq":1,"event_type":1,"ext_span_id":"7-1","status":0,"input_tokens":900,"logs":["开始"]}]'

# trace 列表
curl localhost:7878/v1/traces

# 中文检索 + 按 agent / 状态过滤
curl -XPOST localhost:7878/v1/search \
  -d '{"text":"盗刷","k":10,"filter":{"agent_name":"风控","status":1}}'

# 纯向量找相似 / 关键词+语义混合（RRF）
curl -XPOST localhost:7878/v1/search -d '{"vector":[0.1,0.2],"k":10}'
curl -XPOST localhost:7878/v1/search -d '{"text":"盗刷","vector":[0.1,0.2],"k":10}'
```

可选：`YT_TOKEN=secret cargo run ... --example server` 开 Bearer token 鉴权；`cargo test -p yt-engine --features gzip` 含请求体 gzip 解压。

列式段存储（Vortex）在独立 crate，依赖较重、单独构建：

```bash
cd yitrace-segstore-vortex && cargo build
```

---

## 架构

```
SDK(Py/TS) ─┐
OTLP/HTTP  ─┼─► 摄入网关 ─► 写前日志(WAL,fsync) ─► 内存表 ──flush──► 不可变段(行式/Vortex列式)
            │                                        │                    │
            │                        确定性 event_id 去重          时间分层 compaction
            ▼                                        ▼
      检索索引(中文BM25 / 图式向量ANN / 属性边车)   读时四源折叠(内存+段+删除+晚到补写)
            │                                        │
            └──────────────► 检索 / 列表 / 树 / eval / 成本 ◄──────┘
                        全程快照隔离 + 水位安全回收
```

三个核心机制：

- **事件而非 span**：一个 span 拆成 `SpanStart`/`SpanEnd`/属性补写等多个不可变事件，读时按身份折叠成一条完整 span。写入永远 append-only，无原地更新。
- **确定性 event_id** = `hash(ext_span_id, seq, event_type)`：去重键由内容决定，重传/重放天然幂等。
- **四源折叠读**：同一快照上跨「内存表 + 段 + 删除位图 + 晚到补写」归并去重，去重键就是 event_id。

更完整的内部设计见 [`docs/design/2026-06-22_yitrace-技术文档.md`](docs/design/2026-06-22_yitrace-技术文档.md)。

---

## 工程结构

```
vex-x/
├── yitrace-engine/          # 引擎（Rust workspace，std-only 零依赖）
│   └── crates/
│       ├── yt-core             # 核心类型：标识、确定性 event_id、不可变 Manifest、折叠算法
│       ├── yt-manifest         # 单写者-多读者：快照 pin 协议、回收水位（正确性脊梁）
│       ├── yt-wal              # 写前日志：fsync 落盘、崩溃安全帧、零依赖二进制编码
│       ├── yt-memtable         # 活内存表：上下界双水位 + 受 gate 的 evict
│       └── yt-engine           # 协调器、四源折叠读、检索、eval、HTTP/OTLP
├── yitrace-segstore-vortex/ # 列式段存储（Vortex），实现引擎的 SegmentStore trait
└── yitrace-sdk/             # 打点 SDK
    ├── python/                  # Python：嵌套 span、token 计数、确定性 event_id
    └── typescript/              # TypeScript：同款语义，BigInt 处理大整数精度
```

---

## SDK 用法

**Python**

```python
from yitrace import Tracer, ConsoleExporter

tracer = Tracer(exporter=ConsoleExporter(), node_id=1)

with tracer.trace("反洗钱筛查") as t:
    with t.span("交易风控") as root:
        with root.span("调用LLM研判") as child:   # 嵌套自动建父子
            child.log("研判结论 需人工复核")
            child.set_status(0)
```

嵌套 `span` 自动建父子关系，trace 在引擎里还原成树。每个 span 产出 `SPAN_START` + 若干 `LOG` + `SPAN_END`，进引擎后按 `(trace, span)` 折叠成一条完整 span。Python 与 TypeScript SDK 对同一身份算出**逐字节一致**的 event_id（用例里有交叉校验）。详见各自 README：[Python](yitrace-sdk/python/README.md) · [TypeScript](yitrace-sdk/typescript/README.md)。

---

## 当前状态

**已用测试验证（真会失败的不变量，不是摆设）**：

- 存储正确性：确定性 event_id 去重、四源折叠、快照隔离、崩溃重放幂等、compaction 重读合并、重启不丢。
- 检索：中文 BM25 多概念召回完胜朴素子串；带过滤 ANN 的 in-graph 召回 ≫ post-filter（表驱动多选择性实测）。
- 端到端：SDK / OTLP → HTTP → 折叠 → 检索 / eval / 成本，全程实测。
- 列式段：Vortex 段写入 + 谓词下推 + 投影下推在引擎读路径跑通。

**仍是验证级或占位实现**：

- 中文分词是 CJK bigram（够用、是正路），未上 jieba 词级；图式 ANN 是单层 NSW，无量化/SIMD。生产级要换团队自有索引的 FFI。
- eval 是规则 scorer，LLM-judge 待接。
- manifest 用 `RwLock<Arc<>>` 骨架，生产实现换 arc-swap + crossbeam-epoch 无锁化；查询执行待接 DataFusion。

接口边界（`SegmentStore` / `Bm25Index` / `GraphIndex` trait）已立好，替换实现不动上层。

---

## 构建要求

- Rust 1.80+（引擎，edition 2021，零外部依赖）
- Rust 1.91+（`yitrace-segstore-vortex`，依赖 Vortex 0.75 + Arrow 58 + Tokio）
- Python 3.8+ / Node 18+（SDK）

## License

MIT
