# @yitrace/trace-sdk

TypeScript trace SDK for yiTrace. Use this when you want to record agent runs
without embedding a database in your app.

The SDK records traces, nested spans, logs, token usage, and status, then sends
them to a local yiTrace collector/server. The heavier storage engine stays
behind the HTTP endpoint.

> License: MIT. Requires Node >= 18.

## Install

```bash
npm install @yitrace/trace-sdk
```

From this repo:

```
npm run build
npm test     # 含与引擎逐字节一致的 event_id、失败缓冲、close flush 校验
```

## Usage

JS 没有 Python 的 `with`，用回调式作用域记录 trace/span：

```ts
import { HttpExporter, Tracer } from "@yitrace/trace-sdk";

const tracer = new Tracer(
  new HttpExporter({
    url: "http://127.0.0.1:7878/v1/ingest",
    tenantId: 1,
  }),
  1,
);

tracer.trace("反洗钱筛查", (t) => {
  t.span("交易风控", (root) => {
    root.span("调用LLM研判", (child) => {   // 嵌套 → 自动以 root 为父
      child.log("研判结论 需人工复核");
      child.setStatus(0);
    });
  });
});

await tracer.close();
```

嵌套 `span` 自动建父子（`parent_span_id` 进线格式 + 引擎），trace 还原成树。

For local debugging without a server, use `ConsoleExporter`.

## Key guarantee: deterministic event_id

`event_id = FNV-1a(ext_span_id ++ seq(8字节小端) ++ [event_type_tag])`，与 **Rust 引擎** 和
**Python SDK** 完全一致。u64 用 `BigInt` 才精确（JS number 是 f64 装不下 64 位）。

意义：客户的 Agent 不管用 Python 还是 TS 框架打点，同一条逻辑 span 事件算出的 event_id 都相同 →
进引擎后去重、崩溃重放幂等全对得上。基准值来自引擎 `cargo run -p yt-core --example print_event_id`，
Python 与 TS 的测试都据此断言一致。这让 `trace-sdk` 可以安全采用 at-least-once 上报：
网络失败后重试不会让同一事件在 yiTrace 里重复计费或重复计数。

## 注意

- 用**可擦除 TS 语法**（不用 `enum`/`namespace`/参数属性），这样 Node 的类型剥离能直接跑，免编译。
- `BigInt` 贯穿 trace/span id、seq、ts、event_id；`toWire()` 把 BigInt 转字符串避免 JSON 精度丢失。

## Export to yiTrace

```ts
import { HttpExporter, Tracer } from "@yitrace/trace-sdk";
const tr = new Tracer(new HttpExporter("http://127.0.0.1:7878/v1/ingest"), 2);
tr.trace("盗刷拦截", (t) => {
  t.span("调用LLM研判", (s) => s.setTokens(800, 150));
});
await tr.close();  // flush -> POST to yiTrace
```

## 可靠上报语义

- `HttpExporter` 失败时把整批退回缓冲队首，下次 `flush/close` 重试；`onError(err, dropped)` 会暴露错误和超上限丢弃数。
- `bufferedCount()` / `sentCount()` / `droppedCount()` 可接监控。
- 语义是 at-least-once：网络“已送达但响应丢失”会重发，同一事件由引擎按确定性 `event_id` 去重，token/成本不会翻倍。
- `Tracer.close()` 返回 `Promise<void>`，会等待底层 exporter 的异步 close/flush 完成；Node 进程退出前应 `await tr.close()`。

## 还没做

- 采样；上下文跨进程传播；可选落盘缓冲；更完整的发布自动化。
