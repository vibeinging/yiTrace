// 事件导出。
import { toWire, type SpanEvent } from "./event";

export interface Exporter {
  export(e: SpanEvent): void;
  // 一次收一批。能真正批量的传输（HttpExporter）实现成单次请求；没实现就由调用方逐条转。
  exportBatch?(events: SpanEvent[]): void | Promise<void>;
  close?(): void;
}

// 打印成 JSON 行（调试用）。
export class ConsoleExporter implements Exporter {
  export(e: SpanEvent): void {
    console.log(JSON.stringify(toWire(e)));
  }
}

// 收集到内存（测试用）。
export class CollectingExporter implements Exporter {
  events: SpanEvent[] = [];
  export(e: SpanEvent): void {
    this.events.push(e);
  }
}

// 攒批再发（真实部署：批量 POST 到引擎摄入端,这里留 flush 钩子）。
export class BatchExporter implements Exporter {
  private sink: Exporter;
  private max: number;
  private buf: SpanEvent[] = [];

  constructor(sink: Exporter, max = 256) {
    this.sink = sink;
    this.max = max;
  }

  export(e: SpanEvent): void {
    this.buf.push(e);
    if (this.buf.length >= this.max) this.flush();
  }

  flush(): void {
    if (this.buf.length === 0) return;
    const batch = this.buf;
    this.buf = [];
    // 整批一次交下游（sink 能批就批，否则逐条）。
    if (this.sink.exportBatch) void this.sink.exportBatch(batch);
    else for (const e of batch) this.sink.export(e);
  }

  close(): void {
    this.flush();
    this.sink.close?.();
  }
}

// 攒批并 POST 到引擎摄入端 /v1/ingest（线格式 JSON 数组）。
// 失败处理：POST 是全有或全无,失败时把整批退回缓冲队首、下次 flush 重试,并回调 onError 上报——
// 不再静默吞掉（原来 `void this.flush()` + 无 catch 会让网络错误丢 trace 还无人知）。缓冲到 maxBuffered
// 上限就丢最老的一批并上报,避免端点长期不可用时内存无界增长。
//
// 交付语义 = **至少一次（at-least-once）**：服务端已收到、但响应在网络中丢了的那种"模棱两可失败",
// 重试会让同一批再发一次。**幂等的权威保证在引擎摄入端**——它按确定性 event_id 去重（SDK↔引擎逐字节
// 一致),重复送达只算一次,不会让 token/成本翻倍。所以这里**不在 SDK 侧再做一套去重窗口**：那既挡不住
// "已达但响应丢"的真实重发场景（SDK 根本不知道服务端收没收到）,又要额外维护有界 id 窗口的换出策略,
// 是重复劳动。若将来要"皮带加背带"的客户端抑制,可在此挂一个有界 recently-acked 集——但引擎侧才是底线。
// 注意：达到 maxBuffered 丢的是**最老批**,onError 的 dropped 计数是唯一信号,接生产前应把它接到监控。
export class HttpExporter implements Exporter {
  private url: string;
  private max: number;
  private maxBuffered: number;
  private onError: (err: unknown, dropped: number) => void;
  private buf: SpanEvent[] = [];

  // 兼容老用法：传字符串当 url；或传选项对象配 max/maxBuffered/onError。
  constructor(
    opts: string | {
      url?: string;
      max?: number;
      maxBuffered?: number;
      onError?: (err: unknown, dropped: number) => void;
    } = {},
  ) {
    const o = typeof opts === "string" ? { url: opts } : opts;
    this.url = o.url ?? "http://127.0.0.1:7878/v1/ingest";
    this.max = o.max ?? 256;
    this.maxBuffered = o.maxBuffered ?? this.max * 16;
    this.onError = o.onError ?? ((err) => console.error("[yitrace] 上报失败:", err));
  }

  export(e: SpanEvent): void {
    this.buf.push(e);
    if (this.buf.length >= this.max) void this.flush();
  }

  exportBatch(events: SpanEvent[]): Promise<void> {
    return this.post(events);
  }

  flush(): Promise<void> {
    if (this.buf.length === 0) return Promise.resolve();
    const batch = this.buf;
    this.buf = [];
    return this.post(batch);
  }

  private async post(events: SpanEvent[]): Promise<void> {
    if (events.length === 0) return;
    try {
      const res = await fetch(this.url, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(events.map(toWire)),
      });
      if (!res.ok) throw new Error(`HTTP ${res.status} ${res.statusText}`);
    } catch (err) {
      // 退回队首重试；超上限丢最老的并上报丢了多少。
      this.buf = events.concat(this.buf);
      let dropped = 0;
      if (this.buf.length > this.maxBuffered) {
        dropped = this.buf.length - this.maxBuffered;
        this.buf = this.buf.slice(dropped);
      }
      this.onError(err, dropped);
    }
  }

  async close(): Promise<void> {
    await this.flush();
  }
}
