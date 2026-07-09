# 多进程 embedded DB 易用性反思

> 日期：2026-07-09

## 结论

功能已经正常，但“别人用起来是否好用”还不能算满分。现在最容易踩坑的不是底层写锁，而是入口选择、文档口径、错误提示和运维可见性。

当前可以对外说：

- 同一台机器上的多个 Python / Node / Rust 进程可以打开同一个本地 data dir。
- 不支持多台机器、网络文件系统或跨主机容器共享一个 embedded data dir。
- `SpoolDbExporter` 不是多 worker 的必需方案，而是削峰、隔离 native 包、降低请求路径等待的可选方案。

## 目前好用的地方

1. Python 用户可以继续用 `connect(path=...)` 或 `init_yitrace(path=...)`，不用理解 WAL/manifest。
2. 多 worker direct embedded 有真实 Python 多进程测试：4 个 Python 子进程各自打开同一个 data dir，总共写入 48 条 trace，父进程 reopen 后通过 trace_search 和中文搜索校验。
3. `fail_open=True` 对服务端友好，trace 初始化失败不会拖垮主服务。
4. `BufferedDbExporter` 让请求线程只入队，默认路径不会同步等待每条 trace 写盘。

## 仍不好用的地方

1. 文档入口太分散。根 README、Python SDK README、`yitrace-db` README、server embedded plan 都讲了 embedded，但用户不知道该先看哪一篇。
2. 旧口径容易残留。比如“多 worker 必须用 spool/server”的说法已经不准确，会让用户误以为功能仍不支持。
3. 缺一个真正面向服务端的最小样例。现在测试证明可行，但没有 `examples/python_multiworker_embedded.py` 或 FastAPI/gunicorn 示例告诉用户怎么接。
4. CLI 语义容易混淆。`yitrace-db serve --workers 2` 仍应拒绝，因为这是“一个 embedded DB 暴露成一个 HTTP 服务”的进程，不等于业务 app 多 worker direct embedded。

## 建议补强顺序

### P0：接入不迷路

- 给 Python 用户一个“服务端推荐写法”文档页：单进程、同机多 worker、spool、HTTP server 四种模式怎么选。
- 在根 README 保持一句话边界：同机多 worker 可以 direct embedded；跨机器用 server。
- 在 Python SDK README 把 spool 改成可选方案，而不是多 worker 必需方案。

### P1：服务端可观测（已补）

- `YiTraceRuntime.health()` 已返回 `enabled`、`mode`、`data_dir`、`queue`、`sent`、`dropped`、`write_errors`、`last_error` 和 `lock`。
- engine 已暴露锁等待指标：open/write lock acquire 次数、try_acquire 次数、当前等待线程数、等待次数、等待时长、timeout、stale lock/read pin 清理次数。
- `yitrace-db` 已提供 `db.lock_metrics()`；`/v1/metrics` 已包含 `yt_process_lock_*` 指标。
- 锁超时错误已说明 data dir 正被另一个本机进程使用，并提示查看 owner 和 health/lock metrics。

### P2：更像生产组件

- 增加 `examples/python_multiworker_embedded.py`，用 `multiprocessing` 或 FastAPI/gunicorn 形态演示。
- 增加一个长跑测试：多 Python worker 持续写、周期 reopen/search、期间 kill 一个 worker，再验证数据不丢。
- 提供只读 open，让纯查询进程不打开写端。

## 当前判断

如果客户是 AgenticData-on-fire 这类同机 API worker + ARQ worker 部署，现在可以推荐 direct embedded。为了让别人真的敢用，还需要补“最小服务端示例 + 长跑测试 + 文档入口继续收敛”。这些不是存储正确性问题，是接入体验问题。
