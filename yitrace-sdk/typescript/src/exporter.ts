// 事件导出。
import { toWire, type SpanEvent } from "./event.ts";

export interface Exporter {
  export(e: SpanEvent): void;
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
    for (const e of this.buf) this.sink.export(e); // TODO: 改成一次 HTTP/OTLP 批量请求
    this.buf = [];
  }

  close(): void {
    this.flush();
    this.sink.close?.();
  }
}

// 攒批并 POST 到引擎摄入端 /v1/ingest（线格式 JSON 数组）。
export class HttpExporter implements Exporter {
  private url: string;
  private max: number;
  private buf: SpanEvent[] = [];

  constructor(url = "http://127.0.0.1:7878/v1/ingest", max = 256) {
    this.url = url;
    this.max = max;
  }

  export(e: SpanEvent): void {
    this.buf.push(e);
    if (this.buf.length >= this.max) void this.flush();
  }

  async flush(): Promise<void> {
    if (this.buf.length === 0) return;
    const batch = this.buf.map(toWire);
    this.buf = [];
    await fetch(this.url, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(batch),
    });
  }

  async close(): Promise<void> {
    await this.flush();
  }
}
