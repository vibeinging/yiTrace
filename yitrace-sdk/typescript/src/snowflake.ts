// 单调雪花 ID（BigInt 精确 64 位）：41 位毫秒 | 10 位节点 | 12 位序列。
const EPOCH_MS = 1_577_836_800_000n;

export class Snowflake {
  node: bigint;
  private lastMs = -1n;
  private seq = 0n;

  constructor(nodeId?: number) {
    const n = nodeId ?? process.pid & 0x3ff;
    this.node = BigInt(n & 0x3ff);
  }

  next(): bigint {
    let ms = BigInt(Date.now());
    if (ms === this.lastMs) {
      this.seq = (this.seq + 1n) & 0xfffn;
      if (this.seq === 0n) {
        while (ms <= this.lastMs) ms = BigInt(Date.now()); // 同毫秒序列耗尽,自旋到下一毫秒
      }
    } else {
      this.seq = 0n;
    }
    this.lastMs = ms;
    return ((ms - EPOCH_MS) << 22n) | (this.node << 12n) | this.seq;
  }
}
