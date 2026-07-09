# embedding 回调接入设计

日期：2026-07-09

## 结论

yiTrace 引擎只负责存向量、查向量和维护磁盘图索引，不直接调用外部 embedding 模型。

Node / Electron 嵌入式包负责接收业务方传入的 embedding 回调：

- `embedQuery(text)`：查询文本转 query vector。
- `embedDocuments(texts)`：span 文本批量转 document vectors。

这样做的原因：

- embedding 模型可能是 OpenAI、本地模型、公司内部服务或离线批处理，不应该绑死在 Rust engine 里。
- 模型调用有网络、鉴权、限流、成本和失败重试，应该由业务进程控制。
- Rust engine 继续保持零外部模型依赖，只接受已经算好的 `vector`。

## 使用场景

### 1. 纯关键词搜索

```ts
await db.search({ text: "盗刷", k: 10 });
```

默认只走 BM25，不调用 embedder。这个路径最便宜、最稳定，适合普通筛选和本地工作台快速查询。

### 2. 语义搜索

```ts
await db.search({ text: "类似的风控失败", mode: "semantic", k: 10 });
```

Node 包调用 `embedQuery(text)` 得到 query vector，然后走向量检索。

### 3. 混合搜索

```ts
await db.search({ text: "盗刷", mode: "hybrid", k: 10 });
```

Node 包调用 `embedQuery(text)`，再把 `text + vector` 一起传给 engine，engine 做 BM25 和向量 RRF 融合。

### 4. 调用方已经有向量

```ts
await db.search({ vector, k: 10 });
await db.indexEmbedding({ traceId, spanId, vector });
```

这种情况下不会调用 embedder。

### 5. 写入后建 span 向量

```ts
await db.indexEmbeddings([
  { traceId: "run-1", spanId: "span-1", text: "疑似盗刷 建议人工复核" },
]);
```

Node 包批量调用 `embedDocuments(texts)`，再把结果写进 engine 的磁盘图索引。

也可以显式让 `ingest` 后建向量：

```ts
await db.ingest(events, { indexEmbeddings: true });
```

这个选项默认关闭，因为 embedding 调用可能慢、会失败、会产生费用，不应该默认阻塞 trace 摄入。

## 约束

- 同一个 data dir 不应混用不同 embedding 模型。
- 维度必须一致；Node wrapper 会在当前进程校验维度，磁盘图索引也会拒绝错误维度。
- 同维度但不同模型无法自动识别，调用方要把模型版本当成业务约束管理。
- 尽量对最终 span 文本建一次向量。重复给同一 `(traceId, spanId)` 建向量会追加图节点；engine 查询结果会按 span 去重，但索引会变大。
- 多 worker 场景下，建议统一使用同一个 embedder 配置；否则同一个 data dir 可能混入不同模型的向量。

## 当前落地

- `@yitrace/db` native 新增 `indexEmbedding(traceId, spanId, embedding)`。
- JS 包新增 `open({ embedder })`、`search({ mode: "semantic" | "hybrid" })`、`indexEmbedding()`、`indexEmbeddings()`。
- JS 包支持 `ingest(events, { indexEmbeddings: true })`，但默认不启用。
- Rust engine 不新增模型依赖，继续只处理 vector。
