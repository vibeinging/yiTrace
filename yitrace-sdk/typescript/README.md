# @yitrace/trace-sdk (TypeScript)

给 Agent 打点，产出与 yiTrace 引擎一致的 trace 事件（产物②的 TS 半边）。

```
node test/test_sdk.ts     # 4 个测试,含与引擎逐字节一致的 event_id 校验(Node 23+ 原生跑 .ts)
```

## 用法

JS 没有 Python 的 `with`，用回调式作用域：

```ts
import { Tracer, ConsoleExporter } from "./src/index.ts";

const tracer = new Tracer(new ConsoleExporter(), 1);

tracer.trace("反洗钱筛查", (t) => {
  t.span("交易风控", (root) => {
    root.span("调用LLM研判", (child) => {   // 嵌套 → 自动以 root 为父
      child.log("研判结论 需人工复核");
      child.setStatus(0);
    });
  });
});
```

嵌套 `span` 自动建父子（`parent_span_id` 进线格式 + 引擎），trace 还原成树。

## 关键保证：event_id 三方逐字节一致

`event_id = FNV-1a(ext_span_id ++ seq(8字节小端) ++ [event_type_tag])`，与 **Rust 引擎** 和
**Python SDK** 完全一致。u64 用 `BigInt` 才精确（JS number 是 f64 装不下 64 位）。

意义：客户的 Agent 不管用 Python 还是 TS 框架打点，同一条逻辑 span 事件算出的 event_id 都相同 →
进引擎后去重、崩溃重放幂等全对得上。基准值来自引擎 `cargo run -p yt-core --example print_event_id`，
Python 与 TS 的测试都据此断言一致。

## 注意

- 用**可擦除 TS 语法**（不用 `enum`/`namespace`/参数属性），这样 Node 的类型剥离能直接跑，免编译。
- `BigInt` 贯穿 trace/span id、seq、ts、event_id；`toWire()` 把 BigInt 转字符串避免 JSON 精度丢失。

## 发到引擎（跨进程已打通）

```ts
import { Tracer, HttpExporter } from "./src/index.ts";
const tr = new Tracer(new HttpExporter("http://127.0.0.1:7878/v1/ingest"), 2);
tr.trace("盗刷拦截", (t) => {
  t.span("调用LLM研判", (s) => s.setTokens(800, 150));
});
await (tr.exporter as HttpExporter).flush();  // POST 到引擎摄入服务
```

## 还没做

- 后台异步批量发送 / 失败重试；打包发布（tsup/tsc 出 dist）；采样；上下文跨进程传播。
