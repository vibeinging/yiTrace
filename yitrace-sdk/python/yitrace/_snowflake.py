"""提交单调的雪花 ID 生成器。

id = 41 位毫秒时间戳 | 10 位节点 | 12 位序列  → 单调、可排序、跨进程不撞。
调度层正确性硬前置:event_id 必须真单调(不能用 SEQUENCE CACHE),这里满足。
"""
import os
import threading
import time

_EPOCH_MS = 1_577_836_800_000  # 2020-01-01Z,缩短数值


class Snowflake:
    def __init__(self, node_id: int | None = None):
        if node_id is None:
            # 默认用 PID 低 10 位;多机部署应显式配不同 node_id
            node_id = os.getpid() & 0x3FF
        self.node = node_id & 0x3FF
        self._lock = threading.Lock()
        self._last_ms = -1
        self._seq = 0

    def next(self) -> int:
        with self._lock:
            ms = int(time.time() * 1000)
            if ms == self._last_ms:
                self._seq = (self._seq + 1) & 0xFFF
                if self._seq == 0:  # 同毫秒序列耗尽,自旋到下一毫秒
                    while ms <= self._last_ms:
                        ms = int(time.time() * 1000)
            else:
                self._seq = 0
            self._last_ms = ms
            return ((ms - _EPOCH_MS) << 22) | (self.node << 12) | self._seq
