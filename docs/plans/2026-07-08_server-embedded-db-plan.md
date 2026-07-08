# 服务端使用 embedded DB 的补强计划

> 日期：2026-07-08
> 结论：服务端系统可以使用 yiTrace embedded DB。真正不能接受的是多个进程同时打开同一个 data dir 写入。要让 AgenticData-on-fire 这类服务端系统放心用，需要补齐单写者接入、后台写入、启动降级、锁恢复、打包验证和运维指标。

## 先把话说准

之前建议第一版用独立 yiTrace 服务，是保守方案，不是 embedded DB 不能用于服务端。

服务端能用 embedded DB，但必须满足一个原则：

> 一个 data dir 只能由一个进程持有写者。服务里可以有很多线程、很多请求、很多后台任务，但它们都要通过同一个 `YiTraceDB` 实例写入。

所以边界是：

- 可以：单进程 FastAPI / Flask / Node 服务，在启动时打开一个 `YiTraceDB`，所有请求共用它。
- 可以：`uvicorn --workers 1`，后台任务和请求都走同一个进程内写入队列。
- 不可以：`uvicorn --workers N`，每个 worker 都 `YiTraceDB.open("./data")`。
- 不可以：多个容器或多个服务进程共享同一个 data dir，并且都直接 embedded 写入。

## 推荐给 AgenticData-on-fire 的接入形态

目标不是让客户额外运维一个 yiTrace 服务，而是让 AgenticData 主服务内部持有一个 yiTrace 写者。

推荐形态：

1. AgenticData 启动时初始化一个全局 `YiTraceDB`。
2. 业务代码只调用轻量 `trace.log(...)` / `DbExporter`。
3. trace 事件先进内存队列，由后台线程批量写入 embedded DB。
4. 写入失败不影响主请求；失败只记录日志和指标。
5. 关闭服务时 flush 队列并 `db.close()`。

最小代码心智：

```python
from yitrace import BufferedDbExporter, Tracer
from yitrace_db import YiTraceDB

db = YiTraceDB.open("./data/yitrace", tenant_id=1)
tracer = Tracer(exporter=BufferedDbExporter(db), node_id=1)
```

`BufferedDbExporter` 已落地：业务线程不直接同步写 DB，而是把事件交给后台写入器。

## 串行写线程和写日志消费日志

可以做，而且这应该是服务端 embedded 的核心设计。

## Chroma 的做法对照

Chroma 的产品形态可以作为参考，但不要照搬到“多进程共写本地目录”。

Chroma 有三种入口：

- `PersistentClient(path=...)`：Python 进程内 embedded，本地目录持久化。
- `HttpClient(...)`：连接一个独立 Chroma server。
- `chroma run --path /db_path`：启动本地持久化 server，再让多个客户端走 HTTP。

Chroma 文档明确说：

- `PersistentClient` 适合本地开发、嵌入应用、降低部署复杂度和低延迟。
- HTTP client 适合需要扩展或离开本机存储的场景。
- Chroma 是 thread-safe，但不是 process-safe；不要让多个进程写同一个 local path。

Chroma 内部确实有 WAL。它的 WAL 用来保证 durability：请求先写 WAL，再写入索引，所以写入后可以马上查询。它还有 BF/HNSW 两层向量索引缓冲和同步点。

但 Chroma 没有把“多个业务进程写同一个本地目录”包装成官方支持的 embedded 模式。真实 issue 里已经出现过：

- 两个进程访问同一个 persistent DB 时，`get` 能看到更新，但 `query` 看不到更新。
- 多个 Lambda 并发写同一个持久目录时，用户报告过 persistent client 损坏。

所以 Chroma 给我们的启发是：

1. 同进程 embedded 可以做，而且应该支持多线程。
2. 多进程共写同一个本地目录不要放开。
3. 如果要比 Chroma 更适合服务端，我们可以补一层 `ingest spool`：多个 worker 不直接打开 DB，只写本地 spool；唯一消费者打开 embedded DB 并串行消费。

也就是说，Chroma 的方案是“embedded 或 server 二选一”；yiTrace 可以做成“embedded + 本地单写者队列/spool”，让客户仍然感觉是 embedded，但内部保持单写者。

## 这个做法是否值得做

值得做，但边界要讲清。

它适合 yiTrace 的原因：

1. trace 是旁路观察数据，不是主业务交易数据。短时间延迟写入可以接受。
2. yiTrace event_id 是确定性的，重复消费可以去重，所以 spool 可以用 at-least-once 语义。
3. 客户不想多运维一个独立服务，spool + 单消费者能保留 embedded 体验。
4. 多 worker 服务端是常见部署，直接禁止 embedded 会让接入面变窄。

它不适合的场景：

- 写入结果必须同步决定业务请求成败。
- 要求 exactly-once 且不能靠 event_id 去重。
- 多台机器共享一个 data dir。
- 查询必须立刻看到刚写入的 trace，不能接受 spool 消费延迟。
- 写入量已经大到需要 Kafka/Redis Stream 这类成熟队列。

所以推荐定位是：

> `BufferedDbExporter` 是默认服务端 embedded 方案；`SpoolDbExporter` 是多 worker 本机方案；跨机器或强一致写入仍然推荐独立 yiTrace server 或外部队列。

不能把 spool 做成“另一个数据库 WAL”。它只是写入前的 durable inbox。真正的 WAL、索引、折叠、租户隔离仍然只在唯一的 `YiTraceDB` 写者里发生。

## 2026-07-08 第一版落地

已在 Python SDK 里先落地最小可用版本：

- `BufferedDbExporter`：请求线程只入队，一个后台线程独占 `YiTraceDB`，按批串行 `db.ingest(...)`。
- `SpoolDbExporter`：worker 进程只写本地 spool 文件，不打开 embedded DB。
- `SpoolConsumer`：唯一消费者扫描 `ready/` 文件，写入 DB；DB 写失败时文件回到 `ready/`，下轮可重试。
- spool 文件采用 `tmp/` 写入后原子 rename 到 `ready/`，避免消费者读到半文件。
- 消费者启动时会把遗留 `inflight/` 文件移回 `ready/`，支持崩溃后继续消费。
- SDK 测试覆盖后台写入、失败重试预算、spool 写入、spool 消费和 DB 失败后保留 ready 文件。
- Python embedded DB eval 已接入真实 `YiTraceDB`：`BufferedDbExporter` 写入真实 DB，`SpoolDbExporter -> SpoolConsumer` 再写同一个 DB，最后通过搜索确认两路 trace 都可查。
- `init_yitrace` / `shutdown_yitrace` 已落地：服务端可一行初始化 tracer，默认 `fail_open=True`，初始化失败时返回 no-op tracer，不影响主服务启动。
- clean consumer 验证已覆盖：只安装 `yitrace`、不安装 `yitrace-db` 时，`init_yitrace(path=..., fail_open=True)` 会安全降级。
- `yitrace consume-spool` CLI 已落地：可作为本地唯一消费者进程，把 `SpoolDbExporter` 写出的 ready 文件写入 embedded DB；支持 `--once` 用于脚本和测试。
- Python native 绑定已对 `open/recover/route_json/flush/close` 使用 PyO3 `Python::detach`，耗时 Rust 路径不再持有 GIL。
- Python / Rust / Node embedded DB 的 `.yitrace.lock` 已写入 owner JSON：`pid`、`host`、`created_unix_ms`、`data_dir`、`executable`；第二个写者报错会带上 existing lock owner，便于判断残留锁。

当前仍未做：

- Python wheel clean consumer 验证。

### 同一进程内

同一进程内不需要多个请求线程直接写 DB。做法是：

1. 进程启动时打开一个 `YiTraceDB`。
2. 启动一个后台写线程，由它独占这个 DB handle。
3. 请求线程只把 trace event 放进内存队列。
4. 后台写线程按批消费队列，串行调用 `db.ingest(...)`。

这解决的是“服务端多请求并发写”的问题。它不需要独立 yiTrace 服务，也不需要多个 DB writer。

Python 绑定还要补一个细节：native 写入和恢复期间应释放 GIL。否则后台线程在 Rust 里写 DB 时，仍可能短时间挡住 Python 请求线程。

### 多进程内

多进程不能靠一个普通线程解决，因为每个 worker 是不同进程。这里可以做“写 log，消费 log”，但这个 log 应该是 yiTrace 的 **ingest spool**，不能让多个进程直接写 engine WAL。

推荐形态：

1. 每个 worker 把 trace event 写入本地 `spool/` 目录，格式是长度前缀 JSON frame 或 JSONL batch。
2. 写文件先写临时文件，再 `fsync`，最后原子 rename 成 ready 文件。
3. 唯一消费者进程打开 `YiTraceDB`，扫描 ready 文件，批量 ingest。
4. ingest 成功后把文件移到 done 或删除；失败移到 retry/dead-letter。
5. 因为 yiTrace event_id 是确定性的，重复消费可以靠引擎去重，所以 spool 可以采用 at-least-once 语义。

这样客户看到的是 embedded/local 模式，但内部仍保持单写者，不会破坏 data dir。

这条路比直接要求客户跑独立 yiTrace 服务更容易接受；本质是把“服务端 embedded”做成一个本地单写者架构。

## 已落地和剩余需要补的功能

### P0：让服务端敢接入

P0 基础已经落地，下面保留验收口径，并标出还没完成的部分。

1. **后台写入器**
   - 已落地 `BufferedDbExporter`。
   - 内部使用一个串行写线程独占 `YiTraceDB`。
   - 已支持批量写入、定时 flush、队列上限、丢弃策略。
   - 默认策略：队列满了丢 trace，不阻塞业务请求。
   - 已暴露 `sent_count()`、`dropped_count()`、`write_error_count()`。
   - 已让 Python native open/recover/route_json/flush/close 释放 GIL，避免后台写线程挡住请求线程。
   - 待补：队列长度、最近错误、写入延迟等更完整指标。

2. **服务端生命周期管理**
   - 已提供 `init_yitrace(...)` / `shutdown_yitrace()` helper。
   - 已自动注册 `atexit`。
   - 关闭时会 flush 队列并 `db.close()`。
   - 待补：`SIGTERM`、`SIGINT` handler 和关闭超时日志。

3. **启动降级**
   - 已支持 `fail_open=True`：native 包加载失败、data dir 锁住、恢复失败时，主服务可以继续启动。
   - trace 不可用时退化成 no-op exporter，`runtime.enabled == False`，`runtime.error` 保留原因。
   - 待补：内置日志和修复建议。

4. **锁文件可诊断**
   - 已在 Python / Rust / Node embedded DB 的 `.yitrace.lock` 写入 owner JSON。
   - owner 包含 `pid`、`host`、`created_unix_ms`、`data_dir`、`executable`。
   - 打开失败时返回 existing lock owner，能看出“谁持有锁”。
   - 待补：对确定的残留锁提供安全恢复命令或 API。

5. **AgenticData 接入示例**
   - 待补一个服务端 demo：模拟请求、后台任务、eval/tuner worker 都写同一个 DB handle。
   - 待补文档：明确 `uvicorn --workers 1` 可以用，`--workers N` 必须改成单 writer 或 spool。

验收：

- 单进程 100 个并发请求写 trace，不丢、不阻塞主请求。
- 队列满时主请求仍成功，指标显示丢弃数。
- native 包缺失时主服务能启动，trace 自动降级。
- `SIGTERM` 后队列 flush，重启能查到退出前 trace。

### P1：让服务端跑得稳

1. **服务端 health/readiness**
   - 提供 `db.health()` 或 `/yitrace/healthz`。
   - 返回 open 状态、恢复状态、队列状态、最近错误、磁盘目录。

2. **恢复过程不阻塞主启动**
   - 支持 `open_async` 或后台恢复。
   - 恢复期间 exporter 先缓存少量事件，恢复完成后写入。
   - 恢复超时进入 no-op 或降级模式。

3. **打包和安装验证**
   - Python wheel 做多平台构建和 clean consumer 验证。
   - 和 Node 的 `pack:verify` 一样，验证“从包安装后真的能 open/ingest/search”。
   - 对 AgenticData 的部署架构固定一个 wheel 来源，避免现场编译。

4. **多 worker 防误用**
   - 检测常见环境变量：`WEB_CONCURRENCY`、`UVICORN_WORKERS`、`GUNICORN_WORKER_ID`。
   - 如果发现多 worker 且配置为 direct embedded，启动时报清楚错误。
   - 文档给出替代方案：单 writer 进程、IPC、ingest spool，或者本地 `yitrace-db serve`。

5. **指标接入**
   - 暴露 Prometheus 风格指标。
   - 至少包括：写入队列、写入延迟、drop 数、flush 次数、DB open 状态、锁错误数。

6. **本地 ingest spool**
   - 已为多 worker 场景提供 `SpoolDbExporter`。
   - worker 只写 spool 文件，不打开 `YiTraceDB`。
   - 已提供 `SpoolConsumer` 单消费者，负责读取 spool、写 embedded DB、处理 retry/dead-letter。
   - 已使用原子 rename 和确定性 event_id 去重，保证崩溃后可重放。
   - 待补：消费者 health、指标和更完整的 dead-letter 运维命令。

验收：

- clean consumer 安装 wheel 后跑完整 embedded smoke。
- 多 worker 配置下不会静默开多个写者。
- 业务服务能用 healthz 判断 trace 是否可用。
- worker 写 spool 后 kill 消费者，重启消费者仍能补写，重复消费不产生重复 trace。

### P2：让服务端上规模

1. **真正只读 open**
   - 支持多个只读进程打开同一个快照目录。
   - 读者不创建写 WAL，不拿写锁。
   - 适合“一个写者，多个只读查询进程”。

2. **单 writer IPC 模式**
   - 提供轻量本地 IPC writer，不要求用户理解 yiTrace server。
   - 多 worker 都把事件发给本机 writer。
   - 这本质还是单写者，只是包装成更适合服务端的部署方式。

3. **热备份和导出**
   - 服务运行时可安全备份 data dir。
   - 支持导出给独立 yiTrace 控制台查看。

4. **数据保留策略自动执行**
   - 当前 retention 有 plan/apply 能力。
   - 服务端需要定时任务 helper，按配置清理旧 trace。

验收：

- 一个写者 + 多个只读查询进程不互相影响。
- 多 worker 通过 IPC writer 写入，同一 data dir 没有锁冲突。
- 长跑压测覆盖重启、kill、恢复、备份、retention。

## 对客户的新说法

可以这样回答：

> 服务端当然可以用 embedded DB。限制不是“服务端不能用”，而是“同一个 data dir 只能有一个写者”。如果 AgenticData-on-fire 用单进程持有一个 `YiTraceDB`，所有请求和后台任务都通过这个实例写入，就可以用。我们需要补的是服务端接入层：后台写入队列、启动降级、关闭 flush、锁文件诊断、wheel 打包验证和 health/metrics。补完后，embedded DB 就能成为推荐方案，而不是只适合本地工具。

## 当前判断

如果目标是“让客户必须用 embedded DB”，最合理路线不是直接要求他们改部署，而是我们补一个服务端友好的接入包。

最小可交付范围：

1. Python `BufferedDbExporter`。已落地。
2. `init_yitrace` / `shutdown_yitrace` 服务端 helper。已落地。
3. `fail_open` 降级。已落地。
4. 锁文件 owner 信息和第二写者诊断。已落地。
5. AgenticData-on-fire 接入示例和测试。待补。

这 5 项补完后，可以把推荐口径从“第一版先用独立服务更稳”改成“单进程服务端默认用 embedded，多进程/多容器再切单 writer 服务”。
