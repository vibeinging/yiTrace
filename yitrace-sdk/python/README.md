# yitrace SDK（Python）

给 Agent 打点，产出与 yiTrace 引擎一致的 trace 事件（产物②）。

```
python3 tests/test_sdk.py     # 4 个测试,含与引擎逐字节一致的 event_id 校验
```

## 用法

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

## 关键保证：event_id 跨语言逐字节一致

`event_id = FNV-1a(ext_span_id ++ seq(8字节小端) ++ [event_type_tag])`，与引擎 `yt-core::event`
**完全一致**（同样的哈希、常数、字段顺序、UTF-8 编码，中文也对得上）。

意义：同一条 span 事件无论重传几次、在 SDK 还是引擎算，event_id 都相同 → 引擎的去重、崩溃重放幂等
全都对得上（同一 span 重传/崩溃恢复不会被算两遍，token/费用不翻倍）。

基准值来自引擎：`cargo run -p yt-core --example print_event_id`；`tests/test_sdk.py` 据此断言一致。

## 模块

| 文件 | 作用 |
|---|---|
| `event.py` | `EventType` / `event_id`（与引擎一致的 FNV）/ `SpanEvent`（对应引擎 WalRecord） |
| `tracer.py` | `Tracer` / `Trace` / `Span` 打点 API（上下文管理器） |
| `exporter.py` | `ConsoleExporter`（调试）/ `CollectingExporter`（测试）/ `BatchExporter`（攒批,留 HTTP 钩子） |
| `_snowflake.py` | 单调雪花 ID（trace/span id） |

## 发到引擎（跨进程已打通）

```python
from yitrace import Tracer, HttpExporter
tr = Tracer(exporter=HttpExporter("http://127.0.0.1:7878/v1/ingest"), node_id=1)
with tr.trace("反洗钱筛查") as t:
    with t.span("调用LLM研判") as s:
        s.set_tokens(1200, 340)
tr.close()  # flush → POST 到引擎摄入服务
```

引擎侧 `cargo run -p yt-engine --example server` 起 HTTP 摄入服务即可接收；`curl localhost:7878/v1/traces` 查回。

## 还没做

- 异步/批量后台发送(现在 `flush` 同步阻塞)、失败重试/落盘缓冲;采样;上下文跨进程传播(traceparent)。
