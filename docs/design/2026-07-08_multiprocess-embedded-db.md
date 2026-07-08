# 多进程 embedded DB 设计

> 日期：2026-07-08

## 结论

yiTrace embedded DB 现在支持同一台机器上的多个进程打开同一个本地 `data dir`。多个进程可以各自 `YiTraceDB.open(path)`，但写入不是并行写文件，而是由引擎内部跨进程串行。

这个方案替代旧的外层 `.yitrace.lock` 独占打开。Python、Rust、Node wrapper 不再先拒绝第二个 embedded handle。

## 做法

### 写入

持久模式下，`WriteCoordinator` 会在 data dir 内创建内部锁：

- `.yitrace.open.lock.d/`：串行化 `open_durable` 的目录初始化和 GC 日志恢复。
- `.yitrace.write.lock.d/`：串行化 WAL append、flush、manifest commit、metadata、retention、vector append、reclaim。

`open_durable` 读取 WAL、manifest、metadata 和做 GC 日志恢复时也会持有跨进程 write 锁，避免新进程启动时读到另一个进程正在提交的文件状态。每个写路径拿到跨进程写锁后，会先刷新磁盘状态。刷新分两档：

1. `wal.log` 先看文件长度。长度没变就不读文件；只有追加时，只解析上次已确认偏移之后的 WAL tail，并更新本进程的 next LSN。
2. 读取 `manifest.dat`。如果 manifest 版本和 watermark 没变，只把新增 WAL 记录增量应用到 memtable、BM25、attrs sidecar、trace rollup、session index 和 vector graph。
3. 如果 manifest 版本或 watermark 变了，才替换当前 manifest，并从持久段 + WAL tail 重建派生内存索引。
4. 读取 `metadata.dat`，避免 annotation、dataset、retention policy 的 id 分配互相覆盖。

然后才写本进程的新事件或元数据。

### 读取和回收

读快照仍然使用 `yt-manifest` 的进程内 pin 协议。持久模式下，快照还会带一个跨进程 reader pin 文件：

- `.yitrace.readers/reader-<pid>-<n>.json`

快照 drop 时 reader pin 自动删除。`reclaim()` 如果看到仍有活跃 reader pin，就不会物理删除 dead segment。这样一个进程读旧快照时，另一个进程不会把它还需要的段文件删掉。

stale lock 和 stale reader pin 会按 pid 清理。锁文件内写 owner JSON，包含 `pid`、`host`、`created_unix_ms`、`data_dir`、`executable`，方便排查。

## 支持边界

支持：

- 同机多个 Python/Node/Rust 进程打开同一个本地 data dir。
- gunicorn/uvicorn/ARQ 这类本机多 worker，只要它们共享的是同一个本地文件系统目录。
- 同进程多个 handle 或多线程并发写。

不支持：

- 多台机器共享同一个 data dir。
- 网络文件系统上的锁语义不可靠场景。
- 把 yiTrace 文件当成公开格式由业务代码直接读写。

多机器、多容器跨主机或高写入量场景仍然推荐独立 yiTrace server 或外部队列。

## 测试覆盖

新增 `yitrace-engine/crates/yt-engine/tests/multiprocess_embedded.rs`：

- 两个 OS 子进程同时打开同一个 data dir，各自写 trace，并各自写 annotation；父进程 reopen 后确认两条 trace 和两条 metadata 都存在。
- 子进程持有快照时，父进程 compaction 后 `reclaim()` 返回 0；子进程退出后再次 `reclaim()` 才删除旧段。
- 4 个 OS 子进程同时写入同一个 data dir，每个进程写 32 条 trace；父进程 reopen 后确认 128 条 trace 全部可查。
- 两个 OS 子进程写入带 embedding 的 trace；父进程 reopen 后确认向量检索能搜到另一个进程写入的数据。

已跑：

- `cd yitrace-engine && cargo test --offline`
- `cd yitrace-engine && cargo test --offline -p yt-wal`
- `cd yitrace-engine && cargo test --offline -p yt-engine --test multiprocess_embedded -- --nocapture`
