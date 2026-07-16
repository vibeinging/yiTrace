# Python 服务端接入 yiTrace（v0.1.5）

这篇面向 FastAPI、ARQ、Celery 一类长期运行的 Python 服务。核心规则只有一条：

> 每个进程启动时初始化一次，进程内一直复用，进程退出时关闭一次。不要按请求、任务、Agent run 或查询反复 open/close。

## 1. 安装

建议先锁定已经验证过的版本：

```bash
python -m pip install "yitrace==0.1.5" "yitrace-db==0.1.5"
```

如果进程只通过 HTTP 向独立 yiTrace server 上报，不需要本地查询，可以只安装 `yitrace==0.1.5`。

## 2. 先选运行模式

| 场景 | 推荐模式 | 说明 |
|---|---|---|
| 单机、少量 API/后台 worker | `buffered` | 每个进程打开同一个本地 data dir。业务线程只把事件放进队列，后台线程批量写 DB。 |
| 单机、worker 较多，或不想让业务进程加载 native DB | `spool` | worker 只写本地 spool；一个 `consume-spool` 进程负责写 DB。 |
| 多台机器、跨主机容器、网络文件系统 | `http` | 只运行一个 yiTrace server，业务进程通过 HTTP 上报和查询。不要跨机器共享 embedded data dir。 |

`direct` 会让业务线程同步写 DB，适合短脚本和测试，不建议作为服务端默认方式。

## 3. 为什么 API worker 和 ARQ worker 都要初始化

API worker 和 ARQ worker 是不同的操作系统进程，内存不共享。`init_yitrace()` 创建的是当前进程自己的 runtime、tracer、exporter 队列，以及 buffered 模式下的 embedded DB handle 和后台写线程。

因此，只要某个进程会执行需要 trace 的 Agent、工具、eval 或后台任务，它就要在自己的启动钩子里初始化一次。这里的“一次”是每个进程一次，不是整个部署只初始化一次，也不是每个请求初始化一次。

同一台机器上的多个进程可以打开同一个本地 data dir。yiTrace 会串行化 open/write，并用 reader pin 保护查询使用中的快照。多台机器不能用这种方式共享同一个目录。

## 4. FastAPI 接法

```python
from contextlib import asynccontextmanager

from fastapi import FastAPI
from yitrace import init_yitrace, shutdown_yitrace


@asynccontextmanager
async def lifespan(app: FastAPI):
    runtime = init_yitrace(
        mode="buffered",
        data_dir="./data/yitrace",
        tenant_id=1,
        fail_open=True,
    )
    app.state.yitrace = runtime
    try:
        yield
    finally:
        shutdown_yitrace()


app = FastAPI(lifespan=lifespan)
```

Uvicorn/Gunicorn 启动多个 worker 时，每个 worker 都会执行一次 lifespan。这正是期望行为。

## 5. ARQ 接法

```python
from yitrace import init_yitrace, shutdown_yitrace


async def on_startup(ctx):
    ctx["yitrace"] = init_yitrace(
        mode="buffered",
        data_dir="./data/yitrace",
        tenant_id=1,
        fail_open=True,
    )


async def on_shutdown(ctx):
    shutdown_yitrace()


class WorkerSettings:
    on_startup = on_startup
    on_shutdown = on_shutdown
```

如果 ARQ 进程完全不会执行需要 trace 的代码，可以不初始化。只要后台任务也会跑 Agent、工具或 eval，就应保留初始化。

## 6. 业务代码只拿 tracer 使用

建议先在两个稳定的公共入口打点，不要一开始散到每个业务工具里：

1. Agent `execute`：一条 Agent run。
2. 公共 `_call_tool`：每次工具调用。
3. LLM 公共调用层稳定后再补模型、token 和输入输出。

```python
from yitrace import get_yitrace_runtime


def run_agent():
    runtime = get_yitrace_runtime()
    if runtime is None:
        return run_agent_without_trace()

    with runtime.tracer.trace("agent-run", tenant_id=1) as trace:
        with trace.span("tool-call") as span:
            span.set_agent("planner")
            span.set_tool("search_metadata")
            return call_tool()
```

不要在请求或任务末尾调用 `runtime.tracer.close()`。它会关闭这个进程共用的 exporter。关闭只放在进程退出钩子里，通过 `shutdown_yitrace()` 统一完成。

没有显式要求时不要给所有 worker 都传 `node_id=1`。同机多进程可以省略 `node_id`，SDK 会按当前进程生成节点号。如果要显式配置，必须保证并行运行的进程使用不同的 `node_id`。

## 7. 查询也要复用已打开的对象

buffered embedded 模式下，查询直接复用 `runtime.db`：

```python
from yitrace import get_yitrace_runtime


def find_run(run_id: str):
    runtime = get_yitrace_runtime()
    if runtime is None or runtime.db is None:
        return None

    result = runtime.db.trace_search({
        "filter": {"externalTraceId": run_id},
        "limit": 1,
    })
    return result
```

`externalTraceId` 在 v0.1.5 会走过滤索引，适合按业务 `runId` 查存在性或下钻详情。判断查询是否走了预期路径时看 `readPlan`：

- `usedFilterIndex`：是否使用过滤索引。
- `candidateSpanKeys`：索引给出的候选数量；负查通常是 `0`。
- `scannedSegments`、`decodedSegmentRows`：实际触碰的 segment 和解码行数。
- `indexBytesRead`、`dataBytesRead`：查询请求读取的逻辑字节。
- `indexesValidated`、`indexesRebuilt`：是否发生首次校验或索引补建。

`scannedSpans` 是兼容旧客户端的字段，新接入不要只看它。

buffered 写入是异步的。如果同一条业务路径刚写完就必须立刻查到，先等待队列写完：

```python
runtime.exporter.flush(timeout=2.0)
result = runtime.db.trace_search({
    "filter": {"externalTraceId": run_id},
    "limit": 1,
})
```

不要为了实现“写后立刻查”而重新 open DB。

HTTP 模式在进程启动时创建并保存一个 `connect(url=...)` client，后续查询一直复用。spool 模式的 worker 本身没有 DB handle；需要查询时应访问持有 DB 的服务，或改用 HTTP 模式。

## 8. spool 模式

业务 worker：

```python
runtime = init_yitrace(
    mode="spool",
    spool_dir="./data/yitrace-spool",
    tenant_id=1,
    fail_open=True,
)
```

另起一个长期运行的消费者进程：

```bash
yitrace consume-spool \
  --data-dir ./data/yitrace \
  --spool-dir ./data/yitrace-spool \
  --tenant-id 1
```

spool 是 at-least-once 语义。消费者写 DB 成功前，文件会保留；进程重启后可以继续消费。重复事件由确定性 `event_id` 去重。

## 9. 健康检查和排查

`runtime.health()` 可以接入应用自己的健康检查或日志：

```python
health = runtime.health()
```

重点看：

- `enabled`、`last_error`：初始化是否成功。`fail_open=True` 时，失败会退化成 no-op，不阻塞主服务启动。
- `queue.queued`、`dropped`、`write_errors`：后台写队列是否积压或丢数据。
- `lock.active_wait_count`、`lock.wait_count`、`lock.wait_ms`：embedded 多进程是否在等待 DB 锁。

从 v0.1.5 开始，数据库写入或错误回调抛出的异常不会让 buffered writer 线程退出。writer 会记录错误、按退避间隔重试，并继续消费后面的数据；重试用尽的批次会计入 `dropped`。

如果锁等待长期偏高，再从 buffered 切到 spool；不要一开始就把单机多进程全部改成独立 server。

## 10. 接入检查表

- API worker 启动一次、退出关闭一次。
- ARQ worker 启动一次、退出关闭一次。
- 请求和任务只复用 tracer，不 open/close DB。
- 多 worker 不共用同一个显式 `node_id`。
- buffered 模式按需监控队列和锁等待。
- 写后立刻查询先 flush，不重新 open。
- 按业务 runId 查询使用 `externalTraceId`，并查看 `readPlan`。
- 多机器改用 HTTP，不共享 embedded data dir。

完整查询字段和响应契约见 [`docs/API_REFERENCE.md`](../API_REFERENCE.md)。Python SDK 的全部 exporter 参数见 [`yitrace-sdk/python/README.md`](../../yitrace-sdk/python/README.md)。
