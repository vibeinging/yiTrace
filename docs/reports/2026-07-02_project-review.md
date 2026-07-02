# yiTrace 项目代码审查报告

> 日期：2026-07-02
> 范围：当前 `main` 分支，重点审查 Rust 引擎 HTTP/租户隔离/控制台嵌入、SDK、FFI 接缝与构建验证。
> 方法：读取 `AGENTS.md`、`docs/CURRENT_STATE.md`、旧项目 Review；运行引擎/SDK/控制台/FFI mock 测试；抽查承重路径代码。

## 结论

工程主体比 6 月 22 日旧报告里的状态更成熟：GC 日志、OTLP 文本拍平、BM25 索引文本范围、重启后索引恢复等高风险项已补上，Rust 引擎 fresh target 测试全绿。

但当前仍有两个必须优先修的租户隔离缺陷：写入端允许客户端自选 `tenant_id`，控制台详情端点没有租户过滤。这两项和 API 文档“租户来自 `X-Tenant-Id`、不信任请求体”的承诺相冲突。

## 本轮修复状态

已处理：

- [P1] HTTP 写入端 tenant 绑定：`POST /v1/ingest` 和 `POST /v1/traces` 现在统一用 `X-Tenant-Id` 覆盖 body / OTLP 中的租户字段。
- [P1] 控制台端点租户隔离：`/v1/sessions`、turns、trace、steps、span detail 全部按请求租户过滤；新增两租户同 `session_id` 的回归测试。
- [P2] auth-enabled 控制台与 SDK：控制台静态页允许匿名加载，`/v1/*` 仍要求 Bearer；控制台 HTTP 客户端、Python SDK、TypeScript SDK 都支持 token 和 tenant header。
- [P2] 控制台资源内嵌稳定性：`build.rs` 不再把 `canonicalize` 得到的绝对路径写进 `include_bytes!`，避免 `target/` 缓存引用旧工作区路径。
- [P2] TypeScript `BatchExporter.close()`：现在等待异步 `exportBatch` 和下游 `close`，避免最后一批 trace 在进程退出时丢失。

仍需后续处理：

- FFI 真库线程安全尚未验证；默认 mock 测试能过，但 `--features link` 真库并发压力测试需要在接入真实 C/C++ 库时补齐。
- 若要把“启用 auth 时必须带 `X-Tenant-Id`”作为生产强约束，需要另加 server 配置或策略开关；本轮保持未带 tenant 写入 `tenant_id=null` 的开发兼容行为。

## Findings

### [P1] 写入端没有把记录绑定到 `X-Tenant-Id`，客户端可以伪造租户写入

证据：

- `yitrace-engine/crates/yt-engine/src/http.rs:268` 对 `/v1/ingest` 直接 `parse_wire_batch(body)`，随后 `self.coord.ingest_wire(recs)`。
- `yitrace-engine/crates/yt-engine/src/wire.rs:114` 从请求体读取 `tenant_id`。
- `yitrace-engine/crates/yt-engine/src/http.rs:277` 对 `/v1/traces` 直接 `self.coord.ingest_otlp(body)`，没有传入 header tenant。
- `yitrace-engine/crates/yt-engine/src/otlp.rs:94`、`:116` 把 OTLP 记录的 `tenant_id` 写成 `None`。
- `docs/API_REFERENCE.md:32` 写的是租户从 `X-Tenant-Id` 取，且“不信任请求体”。

影响：

- SDK 线格式摄入时，恶意或错误客户端可以在 body 里写任意 `tenant_id`，把数据写进别的租户命名空间。
- OTLP 摄入没有 `tenant_id`，在带租户查询时会被过滤掉；不带租户查询又会暴露未归属数据。

建议：

- HTTP 层在解析后统一覆盖所有 `WireRecord.tenant_id = tenant`，不要信 body。
- 对没有 tenant header 的生产模式请求明确拒绝，至少在启用 auth 时拒绝。
- `ingest_otlp` 增加带 tenant 的入口，或在 HTTP 层解析 OTLP 后覆盖 tenant 再 ingest。
- 增加端到端测试：body 写 `tenant_id=2`，header `X-Tenant-Id: 1`，最终只能按 tenant 1 查到。

### [P1] 控制台会话和 trace 详情端点绕过租户过滤

证据：

- `yitrace-engine/crates/yt-engine/src/http.rs:281`、`:283` 只给 `GET /v1/traces` 和 `POST /v1/search` 注入 tenant。
- `yitrace-engine/crates/yt-engine/src/http.rs:287` 到 `:288` 的 `/v1/sessions` 与 `route_console` 没有 tenant 参数。
- `yitrace-engine/crates/yt-engine/src/http.rs:393` `sessions_page_json` 调用 `console_sessions(&snap)`，不带 tenant。
- `yitrace-engine/crates/yt-engine/src/http.rs:423` `turns_json` 调用 `load_session_timeline(&snap, sid)`，不带 tenant。
- `yitrace-engine/crates/yt-engine/src/http.rs:454`、`:512`、`:543` trace/detail/steps 都调用 `console_trace_spans(&snap, tid)`，不带 tenant。
- 底层 `console_sessions` 在 `yitrace-engine/crates/yt-engine/src/lib.rs:1777` 用 `TraceQuery::all()` 重建边车，`console_trace_spans` 在 `:1789` 用 `TraceQuery::trace(...)`，tenant 仍为 `None`。
- `docs/API_REFERENCE.md:33` 明确说 `/v1/sessions`、turns、trace、span 详情都应按 `X-Tenant-Id` 过滤。

影响：

- 知道或猜到 `session_id` / `trace_id` 的租户，可以读取其他租户的会话、瀑布、输入输出大文本。
- 当前测试 `http_tenant_header_isolates_traces_and_search` 只覆盖列表和搜索，没覆盖控制台详情端点。

建议：

- `route_console`、`sessions_page_json`、`turns_json`、`trace_json`、`steps_json`、`span_detail_json` 全部接收 tenant。
- 引擎层补 `console_sessions_for_tenant`、`load_session_timeline_for_tenant`、`console_trace_spans_for_tenant`，不要只在 HTTP 层后置过滤 JSON。
- `SessionIndex` 当前按 `session_id` 聚合，缺 tenant 维度；要么改 key 为 `(tenant_id, session_id)`，要么带 tenant 时走安全的 `TraceQuery::all().for_tenant(...)` 重建/过滤。
- 增加覆盖两租户同 session_id、跨租户 trace detail、span detail、steps 的测试。

### [P2] 启用 `YT_TOKEN` 后内嵌控制台和 SDK 缺少认证头路径

证据：

- `yitrace-engine/crates/yt-engine/src/http.rs:137` 到 `:145` 先执行 Bearer 鉴权，再服务静态资源；浏览器直接访问 `/` 没法带 `Authorization`，会 401。
- `yitrace-console/src/api/http.ts:16`、`:23` 的 `fetch` 只带 `accept` / `content-type`，没有 token 或 tenant header 配置。
- TS SDK `HttpExporter` 在 `yitrace-sdk/typescript/src/exporter.ts:110` 到 `:113` 只发 `Content-Type`。
- Python SDK `HttpExporter` 在 `yitrace-sdk/python/yitrace/exporter.py:92` 到 `:94` 只发 `Content-Type`。
- 示例服务 `yitrace-engine/crates/yt-engine/examples/server.rs:24` 到 `:26` 支持 `YT_TOKEN`。

影响：

- 按文档打开 `YT_TOKEN=secret cargo run ... --example server` 后，控制台页面本身就打不开。
- SDK 无法向启用 auth 的服务上报，除非用户自己绕过 SDK 或改 URL 代理。
- 多租户部署也没有统一的 `X-Tenant-Id` 注入点。

建议：

- 静态控制台认证策略要明确：要么静态页匿名可访问、API 需要 token；要么提供登录/本地 token 输入机制。
- 前端 HTTP 客户端支持从配置或本地安全存储读取 `Authorization` 与 `X-Tenant-Id`。
- Python/TS `HttpExporter` 增加 `headers` / `token` / `tenant_id` 选项。
- 增加 auth-enabled smoke test，覆盖静态页、API、SDK 上报。

### [P2] 控制台嵌入产物被 gitignore，且 build.rs 生成绝对路径，发布和本地测试容易漂移

证据：

- `.gitignore:38` 忽略 `yitrace-engine/crates/yt-engine/console_dist/`。
- `yitrace-engine/crates/yt-engine/build.rs:14` 到 `:19` 只在本地存在 `console_dist` 时嵌入，并把 `canonicalize` 得到的绝对路径写入 `include_bytes!`。
- 当前普通 `cargo test --offline` 在原 `target/` 下失败，错误引用旧路径 `/Users/Four/JobProjects/vexdb/vex-x/.../console_dist/...`。
- 使用新的临时 `CARGO_TARGET_DIR` 后同一源码测试通过，说明是构建缓存与绝对路径耦合导致。
- fresh 前端构建输出 `index-C6Pb-miS.js`，本地 `console_dist` 是旧的 `index-CFazBGdC.js`；两者都未入库。

影响：

- fresh clone 编译出的引擎可能没有内嵌控制台，和“单二进制自带控制台”的口径不一致。
- 复用或移动 `target/` 后，推荐命令 `cargo test --offline` 会因旧绝对路径失败。
- 前端源码和实际嵌入产物可以静默不一致。

建议：

- build.rs 生成 `include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/console_dist/..."))`，避免把旧工作区绝对路径烙进 `target`。
- 加一个可执行的 embed 脚本或 Make target：前端 build、拷贝、校验 hash，一条命令完成。
- CI/release 至少跑一次 `VITE_API=http npm run build` + 拷贝 + `cargo test --offline`。
- 如果控制台是交付物，考虑让 release artifact 跟踪或打包 `console_dist`，不要依赖开发机 ignored 文件。

### [P2] TS `BatchExporter` 丢弃异步 `exportBatch` Promise，包住 `HttpExporter` 时可能丢最后一批 trace

证据：

- `yitrace-sdk/typescript/src/exporter.ts:7` 允许 `exportBatch` 返回 `void | Promise<void>`。
- `BatchExporter.flush` 在 `:47` 使用 `void this.sink.exportBatch(batch)`，没有返回或等待 Promise。
- `BatchExporter.close` 在 `:51` 到 `:53` 调用 `flush()` 后立即 `sink.close?.()`，自身也不是 async。
- `HttpExporter.exportBatch` 在 `:96` 到 `:98` 返回 Promise。

影响：

- `new BatchExporter(new HttpExporter(...))` 是一个自然组合，但 `close()` 可能在 POST 完成前返回；Node 进程退出时最后一批 trace 会丢。
- 当前 TS 测试只覆盖 `HttpExporter` 自身失败回退，没有覆盖 `BatchExporter` 包异步 sink。

建议：

- 把 `BatchExporter.flush/close` 改成返回 `Promise<void>`，等待异步 sink。
- 或者在类型层把 `BatchExporter` 限定为同步 sink，并在构造时拒绝 async sink。前者更符合现有 `Exporter` 设计。
- 增加一个 fake async sink 测试，断言 `await batch.close()` 后网络 Promise 已完成。

### [P2] FFI crate 对真库线程安全做了 `unsafe impl Send + Sync` 假设，但默认测试只验证 mock

证据：

- `yitrace-tokenizer-jieba/src/lib.rs:52` 到 `:55` 对 `JiebaTokenizer` 裸 C handle 做 `unsafe impl Send + Sync`。
- `yitrace-tokenizer-jieba/ABI.md:28` 到 `:29` 把同句柄多线程并发 `cut` 作为 ABI 约定。
- `yitrace-vecindex-graph/src/lib.rs:62` 到 `:64` 对 `GraphAnnFfi` 做 `unsafe impl Send + Sync`。
- 默认测试是 mock feature：jieba 3 个测试、graph 4 个测试均通过，但没有真库并发压力测试。

影响：

- 一旦团队真库同句柄 search/cut 非线程安全，HTTP 多线程或共享索引会跨线程调用裸 handle，轻则错结果，重则内存破坏。

建议：

- 真库接入前增加 `--no-default-features --features link` 的构建机测试。
- 加并发压力测试：多线程同时 tokenize/search，跑足够次数，并用 sanitizer/ASAN 构建跑一轮。
- 若真库不能证明同句柄并发安全，去掉 `Sync`，在 wrapper 内部加 `Mutex` 或改为每线程 handle 池。

## 验证结果

修复后验证：

- `cd yitrace-engine && cargo test --offline`：通过。结果：`yt-core` 9、`yt-engine` 123 passed / 1 ignored、`eval_harness` 6、`yt-manifest` 4、`yt-memtable` 2、`yt-wal` 4 全绿；2 个 ignored doctests。
- `cd yitrace-engine && cargo test --offline -p yt-engine http::tests::`：15 passed，覆盖租户写入覆盖、控制台端点隔离、auth、socket 往返、检索和 OTLP。
- `cd yitrace-console && npm run build`：通过。
- `cd yitrace-sdk/python && python -m pytest`：9 passed。
- `cd yitrace-sdk/typescript && npm run build`：通过。
- `cd yitrace-sdk/typescript && npm test`：通过，8 条业务断言；期间先用 `npm rebuild esbuild` 修复了本机 `node_modules` 中 esbuild 平台包与当前 Node 架构不匹配的问题。
- 本轮未重跑 `yitrace-tokenizer-jieba` / `yitrace-vecindex-graph` / `yitrace-segstore-vortex`；FFI 真库线程安全仍按上面的后续项处理。

## 建议修复顺序

1. 先修 P1 租户写入绑定和控制台详情端点过滤，并补端到端测试。
2. 再修 auth-enabled 控制台/SDK 头注入，否则真实私有化模式不可用。
3. 修 build.rs 绝对路径和控制台嵌入流程，让 `cargo test --offline` 在当前 checkout 稳定可复现。
4. 补 TS `BatchExporter` async 语义测试。
5. 真库接入前补 FFI 并发和 link-feature CI。
