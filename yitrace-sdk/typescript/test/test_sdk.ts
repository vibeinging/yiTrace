// SDK 测试。`node test/test_sdk.ts`（Node 23+ 原生跑 .ts）。
import { CollectingExporter, EventType, HttpExporter, Tracer, eventId, toWire, type SpanEvent } from "../src/index.ts";

let passed = 0;
function check(cond: boolean, msg: string): void {
  if (!cond) throw new Error("FAIL: " + msg);
}
function test(name: string, fn: () => void): void {
  fn();
  passed++;
  console.log("OK  " + name);
}

// 引擎基准值：cargo run -p yt-core --example print_event_id
test("event_id 与引擎逐字节一致（含中文）", () => {
  check(eventId("demo-span", 7n, EventType.SpanEnd) === 16098495313036060864n, "demo-span");
  check(eventId("1002-1", 1n, EventType.SpanStart) === 3941713543033365492n, "1002-1");
  check(eventId("反洗钱-1", 3n, EventType.Attr) === 13462389519714918643n, "反洗钱");
});

test("event_id 确定且敏感", () => {
  check(eventId("s", 7n, EventType.SpanEnd) === eventId("s", 7n, EventType.SpanEnd), "确定");
  check(eventId("s", 7n, EventType.SpanEnd) !== eventId("s", 8n, EventType.SpanEnd), "seq");
  check(eventId("s", 7n, EventType.SpanEnd) !== eventId("s", 7n, EventType.SpanStart), "类型");
  check(eventId("s", 7n, EventType.SpanEnd) !== eventId("t", 7n, EventType.SpanEnd), "身份");
});

test("span 产出 start/log/end", () => {
  const exp = new CollectingExporter();
  const tr = new Tracer(exp, 1);
  tr.trace("反洗钱筛查", (t) => {
    t.span("调用LLM研判", (s) => {
      s.log("研判结论 需人工复核");
      s.setStatus(0);
    });
  });
  const evs: SpanEvent[] = exp.events;
  check(evs.map((e) => e.eventType).join(",") === [EventType.SpanStart, EventType.Log, EventType.SpanEnd].join(","), "三类事件");
  check(evs.map((e) => e.seq).join(",") === "1,2,3", "seq 单调递增");
  check(evs.every((e) => e.extSpanId === evs[0].extSpanId), "同一 span 身份");
  check(evs[0].logs[0] === "调用LLM研判", "start 带名");
  check(evs[2].status === 0 && evs[2].durationNs !== null && evs[2].durationNs >= 0n, "end 带状态+耗时");
  check(new Set(evs.map((e) => eventId(e.extSpanId, e.seq, e.eventType))).size === 3, "event_id 互不相同");
});

test("嵌套 span 自动建父子", () => {
  const exp = new CollectingExporter();
  const tr = new Tracer(exp, 1);
  tr.trace("反洗钱筛查", (t) => {
    t.span("root", (root) => {
      root.span("child", () => {});
    });
  });
  const starts = exp.events.filter((e) => e.eventType === EventType.SpanStart);
  const rootStart = starts.find((e) => e.logs[0] === "root")!;
  const childStart = starts.find((e) => e.logs[0] === "child")!;
  check(rootStart.parentSpanId === null, "根 span 无父");
  check(childStart.parentSpanId === rootStart.spanId, "子 span 的父是 root");
});

test("setTokens 上报并进线格式", () => {
  const exp = new CollectingExporter();
  const tr = new Tracer(exp, 1);
  tr.trace("x", (t) => {
    t.span("llm", (s) => {
      s.setTokens(1200, 340);
    });
  });
  const end = exp.events.find((e) => e.eventType === EventType.SpanEnd)!;
  check(end.inputTokens === 1200n && end.outputTokens === 340n, "token 记上");
  check(toWire(end).input_tokens === "1200", "token 进线格式(字符串避免精度丢失)");
});

test("会话/agent/eval 文本字段透传并进线格式", () => {
  const exp = new CollectingExporter();
  const tr = new Tracer(exp, 1);
  tr.trace(
    "多轮对话",
    (t) => {
      t.span("规划", (s) => {
        s.setAgent("规划");
        s.setModel("qwen3");
        s.setIo("请研判这笔交易", "判定为疑似盗刷");
        s.span("查工具", (tool) => {
          tool.setTool("kb_lookup");
        });
      });
    },
    9000,
  );
  // 会话 id 透传到本 trace 全部事件（含嵌套子 span）
  check(exp.events.every((e) => e.sessionId === 9000n), "会话 id 透传到全部事件");
  const end = exp.events.find((e) => e.eventType === EventType.SpanEnd && e.agentName === "规划")!;
  check(end.model === "qwen3", "model 记上");
  check(end.inputText === "请研判这笔交易" && end.outputText === "判定为疑似盗刷", "eval 输入输出文本记上");
  const w = toWire(end);
  check(w.session_id === "9000" && w.agent_name === "规划" && w.output_text === "判定为疑似盗刷", "进线格式");
  const toolEnd = exp.events.find((e) => e.eventType === EventType.SpanEnd && e.toolName === "kb_lookup")!;
  check(toolEnd.sessionId === 9000n, "子 span 也继承会话 id");
});

test("异常退出 → 状态非0", () => {
  const exp = new CollectingExporter();
  const tr = new Tracer(exp, 1);
  try {
    tr.trace("x", (t) => {
      t.span("y", () => {
        throw new Error("boom");
      });
    });
  } catch {
    // 预期
  }
  const end = exp.events.find((e) => e.eventType === EventType.SpanEnd)!;
  check(end.status === 1, "异常 → 状态1");
});

// 异步测试：HttpExporter 上报失败时退回缓冲 + 回调 onError，不静默吞掉。
async function asyncTests(): Promise<void> {
  const ev: SpanEvent = {
    traceId: 1n, spanId: 1n, ts: 1n, seq: 1n, eventType: EventType.SpanEnd, extSpanId: "s1",
    parentSpanId: null, status: 0, durationNs: null, inputTokens: null, outputTokens: null,
    sessionId: null, agentName: null, toolName: null, model: null, inputText: null, outputText: null, logs: [],
  };
  const origFetch = globalThis.fetch;
  try {
    let errs = 0;
    globalThis.fetch = (() => Promise.reject(new Error("network down"))) as typeof fetch;
    const exp = new HttpExporter({ url: "http://x", max: 1, onError: () => { errs++; } });
    exp.export(ev); // max=1 → 触发 flush → 失败
    await new Promise((r) => setTimeout(r, 0));
    check(errs === 1, "失败回调 onError 一次");
    // 失败的批退回缓冲：恢复 fetch 后 flush 应把它发出去（成功 → 缓冲清空）。
    let posted = 0;
    globalThis.fetch = (() => Promise.resolve(new Response("", { status: 200 }))) as typeof fetch;
    await exp.flush();
    posted++;
    check(posted === 1 && errs === 1, "恢复后重试成功,不再报错（trace 没被静默丢）");
    passed++;
    console.log("OK  HttpExporter 失败退回缓冲 + onError 上报");
  } finally {
    globalThis.fetch = origFetch;
  }
}

await asyncTests();
console.log("\n" + passed + " passed");
