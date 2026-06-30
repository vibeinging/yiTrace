# AGENTS.md

> 本文件给 AI 编程助手（Cursor / Claude Code / ZCode / 其他 agentic 工具）读，也供人类开发者参考。
> 它说清：**这个项目是什么、怎么构建测试、改代码要注意什么、以及如何对接 yiTrace（灌 trace / 查询 / 检索）**。
> AI 助手在动手改代码前应先读本文，避免破坏既定约定。

---

## 1. 项目是什么

**yiTrace** 是一台单机、零外部依赖的 **AI Agent 可观测性数据库引擎**（Rust 自研）。把 Agent（多轮对话、调工具、多 agent 协作）跑出来的 trace 灌进来，提供：

- trace 还原（事件折叠成完整 span）
- 中文 BM25 检索 + 带过滤的向量 ANN 召回 + 混合检索（RRF）
- 成本归因（token / 费用，per-agent）
- 评测闭环（eval：打分、回归数据集、per-agent 看板）
- 多租户隔离（tenant_id 全流程贯穿）

**关键约束（改代码时必须守住）：**

- **引擎主体零外部依赖、只用 Rust 标准库**。`cargo test --offline` 必须离线可过。重依赖（Vortex 等）隔离在独立 crate（`yitrace-segstore-vortex` 等），**不要把外部 crate 拉进 `yt-engine`**。
- **确定性 `event_id`** = `hash(ext_span_id, seq, event_type)`，跨 Python / TypeScript / 引擎逐字节一致。改 event 编码 = 破坏跨语言去重，必须有跨语言对账测试。
- **接缝优先于实现**：分词 / 向量索引 / 段存储都是 trait 接缝（`Tokenizer` / `GraphIndex` / `SegmentStore` / `Bm25Index`）。换实现不动上层。

---

## 2. 仓库结构

```
yitrace-engine/              # 引擎（Rust workspace，std-only 零依赖）— 当前承重代码
│   └── crates/
│       ├── yt-core/            # 核心类型：ids、确定性 event_id、不可变 Manifest、折叠算法
│       ├── yt-manifest/        # 单写者-多读者：快照 pin 协议、回收水位（正确性脊梁）
│       ├── yt-wal/             # 写前日志：fsync、崩溃安全帧、二进制编码
│       ├── yt-memtable/        # 活内存表：双水位 + 受 gate 的 evict
│       └── yt-engine/          # 协调器、四源折叠读、检索、eval、HTTP/OTLP、控制台
│           └── examples/       # demo / server / bench_qps / eval_harness
yitrace-segstore-vortex/     # Vortex 列式段（工作区外，隔离重依赖）
yitrace-tokenizer-jieba/     # cppjieba FFI（可选；引擎默认用纯 Rust ChineseTokenizer）
yitrace-vecindex-graph/      # graph_index FFI（可选；引擎默认用自研磁盘 HNSW）
yitrace-sdk/                 # 打点 SDK
│   ├── python/                  # yitrace 包（pyproject.toml 已配，纯标准库）
│   └── typescript/              # @yitrace/trace-sdk（tsconfig + build 已配）
yitrace-console/             # 控制台前端（React + Vite + TS，构建产物内嵌进引擎单二进制）
docs/                        # 设计文档 / 现状索引 / 分析
```

> **`docs/CURRENT_STATE.md` 是现状的唯一权威入口**，新读者从那里看，别被历史过程文档带偏。

---

## 3. 构建与测试

**引擎（主力）：**

```bash
cd yitrace-engine
cargo test --offline                    # 全测试（含并发压测 + HTTP 往返 + ANN 召回 + 重启不丢）
cargo run -p yt-engine --example demo   # 可运行 demo：灌数据 → 折叠 → 中文搜 → 向量 → 混合召回
cargo run -p yt-engine --example server # 起 HTTP 服务（:7878，自带 eval 种子数据）
cargo run -p yt-engine --example bench_qps --release  # 真实 QPS 压测（务必 --release）
```

- **测试必须 `--offline` 能过**（守零依赖原则）。release 才跑 bench（debug 慢几十倍，数字无意义）。
- 改了 HTTP/控制台后，要重新构建前端并内嵌：`cd yitrace-console && VITE_API=http npm run build && rm -rf ../yitrace-engine/crates/yt-engine/console_dist && cp -r dist ../yitrace-engine/crates/yt-engine/console_dist`。
- 可选：`YT_TOKEN=secret cargo run ... --example server` 开 Bearer 鉴权；`cargo test -p yt-engine --features gzip` 含 gzip 解压。

**外部 crate（隔离的重依赖，按需构建）：**

```bash
cd yitrace-segstore-vortex && cargo build     # Vortex（需 Rust 1.91+）
cd yitrace-tokenizer-jieba && cargo build     # jieba FFI（默认 mock，--features link 接真库）
cd yitrace-vecindex-graph && cargo build      # graph_index FFI（同上）
```

**SDK：**

```bash
# Python
cd yitrace-sdk/python && python -m build      # 出 wheel/sdist（pyproject.toml 已配）
python -m pytest                              # 跑测试
# TypeScript
cd yitrace-sdk/typescript && npm install && npm run build   # tsc 出 dist/
npm test                                      # tsx 跑测试
```

**前端：**

```bash
cd yitrace-console && npm install && npm run dev     # 开发服务 :5180（默认 mock 数据）
VITE_API=http npm run build                          # 构建对接真实引擎的版本
```

---

## 4. 代码约定（AI 改代码前必读）

- **Rust**：edition 2021，`#![allow(dead_code)]` 在骨架 crate 里是刻意的（接缝实现待替换）。模块用中文 doc-comment 解释"为什么这么设计"，改代码要延续这个习惯（写清意图，不只是 what）。
- **零依赖**：要在 `yt-engine` 引入外部 crate，先想清楚能不能放进独立的外部 crate。引擎本体只 std。
- **测试是承重的**：每个不变量都有"真会失败的测试"（崩溃重放、召回对标、确定性 event_id 跨语言对账）。改逻辑前先看相关测试，改完跑全量。
- **命名**：crate `yt-*`，Rust 标识符 `yt_`，Prometheus 指标 `yt_*`，环境变量 `YT_*`，Python 包 `yitrace`，TS 包 `@yitrace/trace-sdk`。顶层目录 `yitrace-*`。**不要引入旧前缀。**
- **提交信息**：纯净的中文/英文描述，首行简短，body 说清 what + why。不带 AI 工具名。

---

## 5. 对接 yiTrace（灌 trace / 查询 / 检索）

### 5.1 起服务

```bash
cd yitrace-engine && cargo run -p yt-engine --example server
# → http://127.0.0.1:7878  （自带 eval 种子数据，开箱可看）
```

### 5.2 用 SDK 打点（推荐）

**Python**

```python
from yitrace import Tracer, HttpExporter

# 指向 yiTrace 服务；event_id 与引擎逐字节一致，重传/崩溃重放自动去重
tracer = Tracer(exporter=HttpExporter(url="http://localhost:7878/v1/ingest"), node_id=1)

with tracer.trace("反洗钱筛查", tenant_id=1) as t:
    with t.span("交易风控") as root:
        with root.span("LLM 研判") as child:
            child.log("研判结论 需人工复核")
            child.set_tokens(input_tokens=900, output_tokens=120)
            child.set_status(0)   # 0=ok, 非0=error
```

嵌套 `span` 自动建父子；每个 span 产出 `SPAN_START` + `LOG` + `SPAN_END`，引擎按 `(trace, span)` 折叠成一条完整 span。多轮会话用同一 `session_id` 串起。

**TypeScript**（同款语义，BigInt 处理大整数精度）：

```typescript
import { Tracer, HttpExporter } from "@yitrace/trace-sdk";
const tracer = new Tracer({ exporter: new HttpExporter("http://localhost:7878/v1/ingest"), nodeId: 1 });
```

### 5.3 用 HTTP 直接对接（OTLP 生态入口，零改动接入）

已埋点 OTLP/OpenInference 的应用**不改一行**即可灌入——`POST /v1/traces` 是标准 OTLP/HTTP 端点（OTel GenAI `gen_ai.*`、Arize `llm.*`）。

> **完整端点契约**（方法/路径/请求体/响应字段/curl 示例/鉴权/租户）见 [`docs/API_REFERENCE.md`](docs/API_REFERENCE.md)。写自己的前端或对接，照那份文档即可。⚠️ 注意：原始 API（`/v1/traces`、`/v1/search`）是 snake_case，控制台 API（`/v1/sessions`、`/v1/traces/:id` 等）是 camelCase，别混用。

| 方法 | 端点 | 用途 |
|---|---|---|
| POST | `/v1/ingest` | 灌入 SDK 线格式 JSON 批（自定义高效格式） |
| POST | `/v1/traces` | **OTLP/HTTP 标准端点**（生态入口，已埋点应用直接接） |
| GET  | `/v1/traces` | trace 列表 |
| POST | `/v1/search` | 中文检索 + 向量召回 + 混合，可带 `filter`（agent/状态/tenant/时间） |
| GET  | `/v1/sessions` | 会话列表（游标分页） |
| GET  | `/v1/sessions/:id/turns` | 一个会话的各轮 |
| GET  | `/v1/traces/:id` | 一条 trace 的折叠 span（瀑布） |
| GET  | `/v1/traces/:id/spans/:spanId` | 单 span 大字段（晚物化） |

**检索示例：**

```bash
# 中文 BM25 + 按 agent/状态过滤
curl -XPOST localhost:7878/v1/search \
  -d '{"text":"盗刷","k":10,"filter":{"agent_name":"风控","status":1}}'

# 纯向量找相似
curl -XPOST localhost:7878/v1/search -d '{"vector":[0.1,0.2,...],"k":10}'

# 关键词 + 语义混合（RRF 融合）
curl -XPOST localhost:7878/v1/search -d '{"text":"盗刷","vector":[0.1,0.2,...],"k":10}'
```

**多租户**：tenant 从 `X-Tenant-Id` 请求头取（非 body，客户端不能越权），`/v1/search` 与 `GET /v1/traces` 都按 tenant 隔离。

### 5.4 控制台

服务起来后浏览器开 `http://127.0.0.1:7878/`——前端已内嵌进引擎单二进制。左栏会话列表、中栏多轮时间线 + 瀑布、右栏 Span 详情。

---

## 6. 给 AI 助手的工作守则

1. **改代码前先跑 `cargo test --offline`** 确认基线绿，改完再跑一遍。
2. **不要在 `yt-engine` 加外部依赖**；要加重依赖，放进独立外部 crate。
3. **改 event 编码 / 折叠逻辑 / 检索算子**，必须更新或新增对应测试（这些是承重不变量）。
4. **改了前端**，记得重新 build 并拷到 `console_dist/`（否则引擎内嵌的是旧版）。
5. **不确定就先读 `docs/CURRENT_STATE.md`**，它是现状权威，别被 docs/ 下的历史过程文档误导。
6. **提交信息不带 AI 工具名**，写清 what + why。
