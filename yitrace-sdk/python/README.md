# yitrace SDK（Python）

> 许可证:MIT。要求 Python ≥ 3.8。

给 Agent 打点，产出与 yiTrace 引擎一致的 trace 事件。

```
python3 tests/test_sdk.py     # 含与引擎逐字节一致的 event_id、失败缓冲、HTTP header 校验
```

从仓库根目录改包形态时跑统一回归：

```bash
./scripts/package_mode_eval.sh
```

它会覆盖 `connect(url/path)`、`DbExporter`、Python embedded DB、Node/Rust embedded 包和 TypeScript SDK。

## 三种用法

### 1. 只在本地调试

```python
from yitrace import Tracer, ConsoleExporter

tracer = Tracer(exporter=ConsoleExporter(), node_id=1)

with tracer.trace("反洗钱筛查") as t:
    with t.span("交易风控") as root:
        with root.span("调用LLM研判") as child:   # 嵌套 → 自动以 root 为父
            child.log("研判结论 需人工复核")
            child.set_status(0)
```

嵌套 `span` 自动建父子（`parent_span_id` 进线格式 + 引擎），trace 还原成树。

每个 span 产出三类事件：`SPAN_START`（带 span 名）+ 若干 `LOG` + `SPAN_END`（带状态+耗时）。
`seq` 在 span 内单调递增、由客户端给定，原样进引擎、引擎绝不重补 —— 进引擎后按 `(trace, span)`
折叠成一条完整 span。

### 2. 发到运行中的 yiTrace server

```python
from yitrace import Tracer, HttpExporter

tr = Tracer(exporter=HttpExporter("http://127.0.0.1:7878/v1/ingest"), node_id=1)
with tr.trace("反洗钱筛查") as t:
    with t.span("调用LLM研判") as s:
        s.set_tokens(1200, 340)
tr.close()  # flush → POST 到引擎摄入服务
```

也可以用统一 client 查数据：

```python
from yitrace import connect

client = connect(url="http://127.0.0.1:7878", tenant_id=1)
print(client.search(text="盗刷", k=10))
```

### 3. 直接写本地 embedded DB

先安装 embedded DB 包，再用 `connect(path=...)`：

```bash
python -m pip install "yitrace[db]"
# 或者分开安装:
python -m pip install yitrace yitrace-db
```

```python
from yitrace import DbExporter, Tracer, connect

db = connect(path="./data", tenant_id=1)
tr = Tracer(exporter=DbExporter(db, tenant_id=1), node_id=1)

with tr.trace("反洗钱筛查", tenant_id=1) as t:
    with t.span("调用LLM研判") as s:
        s.log("疑似盗刷")

tr.close()
print(db.search(text="盗刷", k=10))
db.close()
```

`connect(url=...)` 返回 HTTP client；`connect(path=...)` 返回 `yitrace-db` 的 embedded DB handle。

服务端推荐不要让请求线程直接写 DB，而是用后台单写线程：

```python
from yitrace import init_yitrace, shutdown_yitrace

runtime = init_yitrace(path="./data/yitrace", tenant_id=1, node_id=1)
tr = runtime.tracer

# 请求线程只入队；后台线程批量写 embedded DB。
with tr.trace("tuner-run", tenant_id=1) as t:
    with t.span("llm-call") as s:
        s.log("model returned")

shutdown_yitrace()  # 等待队列 flush，并关闭 embedded DB
```

默认 `fail_open=True`。如果 native 包缺失、data dir 被锁、恢复失败，`init_yitrace(...)` 会返回 no-op tracer，主服务继续启动；`runtime.enabled == False`，`runtime.error` 里保留原因。

服务端可以把 `runtime.health()` 接到自己的 `/healthz` 或日志里。它会返回 `enabled`、`mode`、`data_dir`、`queue`、`dropped`、`last_error` 和 `lock`；其中 `lock.active_wait_count`、`lock.wait_count`、`lock.wait_ms` 能看出是不是正在等 embedded DB 锁。

同一台机器上的多 worker 服务端可以让每个 worker 都 `connect(path=...)` 打开同一个本地 data dir。引擎内部会串行化 open/write，并在写前刷新 WAL、manifest 和 metadata：

```python
from yitrace import init_yitrace

# 每个本机 worker 进程都可以这样初始化。
runtime = init_yitrace(path="./data/yitrace", tenant_id=1, node_id=1)
tr = runtime.tracer
```

不要让多台机器、网络文件系统或跨主机容器共享同一个 embedded data dir；这些场景用 yiTrace server 或外部队列。

如果希望 worker 完全不加载 native DB、需要落盘削峰，或者不想让请求路径等待 DB 锁，可以改用 spool：

```python
from yitrace import SpoolConsumer, SpoolDbExporter, Tracer, connect

# worker 进程：只写 spool 文件，不打开 YiTraceDB。
tr = Tracer(exporter=SpoolDbExporter("./data/yitrace-spool", tenant_id=1), node_id=1)
with tr.trace("worker-task", tenant_id=1) as t:
    with t.span("tool-call") as s:
        s.log("ok")
tr.close()

# 单独的消费者进程：打开 embedded DB 消费 spool。
db = connect(path="./data/yitrace", tenant_id=1)
consumer = SpoolConsumer(db, "./data/yitrace-spool")
consumer.consume_once()
db.close()
```

也可以直接跑 CLI：

```bash
yitrace consume-spool --data-dir ./data/yitrace --spool-dir ./data/yitrace-spool
```

脚本或测试里可用 `--once` 消费一轮后退出。

`SpoolDbExporter` 是 at-least-once 语义：消费者写 DB 成功前文件会留在 `ready/` 或 `inflight/`，重启后可继续消费。重复消费由引擎按确定性 `event_id` 去重。

## 关键保证：event_id 跨语言逐字节一致

`event_id = FNV-1a(ext_span_id ++ seq(8字节小端) ++ [event_type_tag])`，与引擎 `yt-core::event`
**完全一致**（同样的哈希、常数、字段顺序、UTF-8 编码，中文也对得上）。

意义：同一条 span 事件无论重传几次、在 SDK 还是引擎算，event_id 都相同 → 引擎的去重、崩溃重放幂等
全都对得上（同一 span 重传/崩溃恢复不会被算两遍，token/费用不翻倍）。

基准值来自引擎：`cargo run -p yt-core --example print_event_id`；`tests/test_sdk.py` 据此断言一致。

## 模块

| 文件 | 作用 |
|---|---|
| `client.py` | `YiTraceClient` / `connect()`，用同一入口连接远程 server 或本地 embedded DB |
| `event.py` | `EventType` / `event_id`（与引擎一致的 FNV）/ `SpanEvent`（对应引擎 WalRecord） |
| `tracer.py` | `Tracer` / `Trace` / `Span` 打点 API（上下文管理器） |
| `exporter.py` | `ConsoleExporter`（调试）/ `CollectingExporter`（测试）/ `DbExporter`（同步写 embedded DB）/ `BufferedDbExporter`（后台单写线程）/ `SpoolDbExporter` + `SpoolConsumer`（多 worker 本地 spool）/ `NoopExporter`（fail-open）/ `BatchExporter`（攒批）/ `HttpExporter`（批量 POST + 失败缓冲） |
| `service.py` | `init_yitrace` / `shutdown_yitrace` / `YiTraceRuntime` 服务端生命周期 helper |
| `cli.py` | `yitrace consume-spool` 本地 spool 消费者命令 |
| `_snowflake.py` | 单调雪花 ID（trace/span id） |

引擎侧 `cargo run -p yt-engine --example server` 起 HTTP 摄入服务即可接收；`curl localhost:7878/v1/traces` 查回。

## 可靠上报语义

- `HttpExporter` 失败时把整批退回缓冲队首，下次 `flush/close` 重试；`on_error(err, dropped)` 会暴露错误和超上限丢弃数。
- `buffered_count()` / `sent_count()` / `dropped_count()` 可接监控。
- 语义是 at-least-once：网络“已送达但响应丢失”会重发，同一事件由引擎按确定性 `event_id` 去重，token/成本不会翻倍。
- `BufferedDbExporter` 用一个后台线程独占 `YiTraceDB`，请求线程只入队；`sent_count()` / `dropped_count()` / `write_error_count()` 可接监控。
- `SpoolDbExporter` 把事件写入本地 `spool/ready` 文件；`SpoolConsumer` 是唯一 DB 写者，失败时文件留在 ready 供下次重试。
- 当前 `flush/close` 会等待队列或本地 spool 写完；进程退出前调用 `tr.close()`。

## 还没做

- 采样；上下文跨进程传播(traceparent)；spool 消费者健康检查和指标。
