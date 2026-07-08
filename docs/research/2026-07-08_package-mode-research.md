# yiTrace 包形态与运行模式调研

> 日期：2026-07-08
> 目标：判断 yiTrace 是否要拆成 `db`、`server`、`client/sdk` 多个用户包，还是用更少的包承载本地嵌入和远程服务两种模式。

## 结论

这篇调研参考的是**本地数据库 / 向量数据库**的包形态，不代表所有可观测平台。结论是：

1. 用户不应该记很多包名。对外入口要收敛到一个清楚的 client API。
2. 本地 embedded 和远程 server 可以共存，但必须把多进程边界说死。
3. embedded 不是“直接读文件”，而是数据库引擎跑在当前进程，用本地目录落盘。
4. 多 worker、多容器、多服务共享同一个 data dir 时，应该走 server 或单 writer 进程。

所以 yiTrace 采用两层包心智：

| 包 | 用户心智 | 责任 |
|---|---|---|
| `yitrace` | 连接 yiTrace | `Tracer`、HTTP client、`connect(url=...)`；如果装了 `yitrace-db`，也能 `connect(path=...)` |
| `yitrace-db` | 嵌入 yiTrace | `YiTraceDB.open()`、本地 ingest/search/trace/span、FastAPI router、`yitrace-db serve` |

暂不发布 `yitrace-client`，避免出现第三个名字。

Rust 侧同样采用两层心智：

| 包 | 用户心智 | 责任 |
|---|---|---|
| `yitrace` | Rust 只打点 | `Tracer`、`HttpExporter`、确定性 event_id，发到运行中的 yiTrace server |
| `yitrace-db` | Rust 嵌入 DB | 打开本地 data dir，进程内 ingest/search/trace/span/read-model helpers |

## 参考项目

### Chroma

Chroma 同时提供三种入口：

- `PersistentClient(path=...)`：本地持久化。
- `chroma run --path ...`：启动 server。
- `HttpClient(host=..., port=...)`：连接远程 server。

官方 Python client 文档还把 HTTP client 描述为支持多 client 连接同一个 server，并说这是推荐的生产配置：
https://docs.trychroma.com/reference/python/client

这能支撑一个判断：**本地嵌入 + server + HTTP client 可以放在同一个产品心智里**。

Chroma 的 issue 也说明 local persistent 模式容易在多进程/多 client 场景踩坑：

- 多进程访问同一个 persistent database 时，曾出现 `get` 看到更新但 `query` 看不到的索引不一致问题。
- 同一进程里创建多个 local client 曾被讨论为 undefined behavior，建议抛异常或让用户共享一个 client。

相关 issue：

- https://github.com/chroma-core/chroma/issues/3792
- https://github.com/chroma-core/chroma/issues/1234

对 yiTrace 的决策不是“Chroma 官方承诺只支持单进程”，而是：**yiTrace 自己把 embedded 边界定成单进程、单 writer，更容易解释，也更安全**。

### Qdrant

Qdrant Python client 支持同一套 client 代码切换本地和远程：

- `QdrantClient(":memory:")`
- `QdrantClient(path="path/to/db")`
- `QdrantClient(url="http://localhost:6333")`

它的说明很清楚：local mode 适合开发、原型和测试；上规模用 server。

来源：

- https://github.com/qdrant/qdrant-client
- https://qdrant.tech/documentation/quickstart/

### LanceDB

LanceDB 用 URI 选择模式：

- `/path/to/database`：本地数据库。
- `db://host:port`：远程。
- `s3://...` / `gs://...`：对象存储。

这对 yiTrace 有一个直接启发：**同一个 API 可以靠 `url` / `path` 切换模式**。

来源：

- https://lancedb.github.io/lancedb/js/functions/connect/
- https://docs.lancedb.com/quickstart

### Milvus Lite

Milvus Lite 是 Python 本地轻量版，同一生态也有 Standalone / Distributed。官方口径是 Lite 适合小规模、本地、边缘、Notebook；大规模用 Standalone / Distributed。

来源：

- https://milvus.io/docs/milvus_lite.md
- https://milvus.io/docs/install-overview.md

### Weaviate Embedded

Weaviate Embedded 是从应用代码启动一个 Weaviate instance，并用本地目录持久化。官方把它标成 experimental。

它和 yiTrace embedded 有区别：Weaviate 更像“应用托管一个本地 server”，yiTrace 当前是“Rust engine 进程内调用”。但它仍然说明一件事：embedded 模式可行，不过生命周期、稳定性和退出清理必须讲清。

来源：

- https://docs.weaviate.io/deploy/installation-guides/embedded

### SQLite / DuckDB

SQLite 和 DuckDB 是更纯的 embedded 心智：同进程、少配置、不需要单独 server。SQLite 官方明确说 serverless 的好处是少安装、少配置、少运维；代价是隔离、锁和多客户端治理能力不如 server。DuckDB 也强调自己是 in-process 数据库。

来源：

- https://www.sqlite.org/serverless.html
- https://duckdb.org/why_duckdb.html

## yiTrace 的最终 API 口径

对外推荐这三种写法。

### 1. 远程 server

```python
from yitrace import connect

client = connect(url="http://localhost:7878", tenant_id=1)
client.search(text="盗刷", k=10)
```

或者：

```python
from yitrace import YiTraceClient

client = YiTraceClient("http://localhost:7878", tenant_id=1)
client.ingest([...])
```

### 2. 本地 embedded

```python
from yitrace import connect

db = connect(path="./data", tenant_id=1)  # requires yitrace-db
db.search(text="盗刷", k=10)
db.close()
```

也可以直接用底层包：

```python
from yitrace_db import YiTraceDB

db = YiTraceDB.open("./data", tenant_id=1)
db.search(text="盗刷", k=10)
db.close()
```

### 3. 本地 embedded + 打点 SDK

```python
from yitrace import DbExporter, Tracer, connect

db = connect(path="./data", tenant_id=1)
tracer = Tracer(exporter=DbExporter(db, tenant_id=1), node_id=1)
```

这让 `Tracer` 既可以用 `HttpExporter` 发到 server，也可以用 `DbExporter` 直接写 embedded DB。

Rust 只打点：

```rust
use yitrace::{HttpExporter, TraceOptions, Tracer};

let exporter = HttpExporter::new("http://127.0.0.1:7878/v1/ingest")?.with_tenant_id(1);
let mut tracer = Tracer::with_exporter(exporter, 1);
tracer.trace_with_result("risk review", TraceOptions::default().tenant_id(1), |trace| {
    trace.span_result("LLM check", |span| {
        span.log("疑似盗刷")?;
        Ok(())
    })
})?;
tracer.close()?;
```

### 4. 本地 embedded + Web 框架

```python
from fastapi import FastAPI
from yitrace_db import YiTraceDB
from yitrace_db.fastapi import create_yitrace_router

db = YiTraceDB.open("./data", tenant_id=1)
app = FastAPI()
app.include_router(create_yitrace_router(db), prefix="/yitrace")
```

CLI：

```bash
yitrace-db serve --data-dir ./data --bind 0.0.0.0:7878
```

## 单写者边界

`yitrace-db` 可以在同一进程内用队列处理并发请求，但它不能解决**多个进程同时写同一个 data dir**。

必须写进 README/API 的边界：

- 单进程 FastAPI、本地 agent、Electron main process：可以 embedded。
- `uvicorn --workers 1`：可以 embedded。
- `uvicorn --workers N`、多个容器、多个服务共享同一个 data dir：不要 embedded 共写，改用 `yitrace-db serve` / 独立 server / 单 writer 进程。
- `.yitrace.lock` 要继续保留，误开第二个 embedded writer 时明确报错。

## 实现决策

短期实现范围：

1. `yitrace` 增加 `YiTraceClient` 和 `connect()`。
2. `yitrace` 增加 `DbExporter`，让现有 `Tracer` 能写 embedded DB。
3. `yitrace-db` 增加可选 FastAPI router。
4. `yitrace-db` 增加 `yitrace-db serve` CLI，并拒绝 `--workers > 1`。
5. `scripts/package_mode_eval.sh` 固化 package-mode eval，覆盖 Python facade、Python embedded DB、Node embedded DB、Rust embedded crate 和 TypeScript SDK。
6. Rust 增加纯 std `yitrace` SDK crate，补齐“只打点上报”的 Rust 包形态。

暂不做：

- 不发布 `yitrace-client`。
- 不让 Python 层直接读 WAL / manifest / segment 文件。
- 不在 embedded 模式里支持多进程共写同一 data dir。
- 不强制 `yitrace-db` 默认依赖 FastAPI/uvicorn；server 能力走 optional extra。
