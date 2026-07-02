# yiTrace 后续逻辑补齐路线

> 日期：2026-07-02
> 目标：把 yiTrace 从“技术上有亮点的 alpha 内核”推进到“GitHub 用户能快速试用、团队能认真评估、PoC 能可信落地”的状态。

## 判断

下一阶段不要优先继续堆复杂内核功能。当前最缺的是三类逻辑：

1. **开箱成功逻辑**：用户 3 分钟内能启动、摄入、搜索、看控制台。
2. **生产信任逻辑**：租户、安全、持久化、备份、监控、配置错误都能被明确处理。
3. **Agent 工作流逻辑**：trace 不只是存下来，还要能从失败中生成 eval、对比版本、定位 agent/tool/model 的责任。

## 本轮已落地

- 新增 `scripts/demo_all.sh`：构建控制台、内嵌到引擎、启动 server、等待 `/v1/healthz`、灌入样例 trace，并打印可复制的查询命令。
- 新增 `Dockerfile`、`docker-compose.yml`、`.dockerignore`：支持 `docker compose up --build` 试用。
- `server` example 支持 `YT_BIND`，容器内可用 `0.0.0.0:7878` 对外监听。
- HTTP API 新增 `/v1/healthz` 和 `/v1/readyz`，供脚本和容器健康检查使用。
- 根 README / 中文 README 已把 Quick Start 调整为优先展示一键 demo 和 Docker。
- Python `HttpExporter` 失败时退回缓冲、支持最大缓冲上限、`on_error` 和 sent/dropped/buffered 统计。
- TypeScript `Tracer.close()` 改为可 `await`，`HttpExporter` 暴露 sent/dropped/buffered 统计，README 示例不再要求访问内部 exporter。
- OTLP 解析新增 `yitrace.session_id` / `yitrace.tenant_id`，HTTP 路径继续由 `X-Tenant-Id` 覆盖租户字段，并用测试固定 header 优先级。
- 新增 `yitrace-node/` MVP：`@yitrace/db` 通过 Node-API 嵌入 Rust engine，支持 `open/close/ingest/search/traces/sessions/trace/span` 和 data-dir 单写者锁。

## P0：一键可运行闭环

### 1. 增加 `demo_all` 一条龙

目标：一个命令跑完 server、灌样例、打开控制台、展示搜索。

建议实现：

- `scripts/demo_all.sh`
- 自动检查 Rust / Node / npm。
- 构建 console，拷贝到 `console_dist/`。
- 启动 `cargo run -p yt-engine --example server`。
- 用 Python 或 curl 灌入 2-3 条真实结构的 agent trace。
- 打印可直接点击的 URL 和 3 条 curl 搜索命令。

验收：

- fresh clone 后执行 `./scripts/demo_all.sh` 可以看到控制台和搜索结果。
- README Quick Start 只保留这个命令和手动路径。

### 2. 增加 Docker Compose

目标：非 Rust 用户也能试。

建议实现：

- `Dockerfile`
- `docker-compose.yml`
- 默认暴露 `7878`。
- 构建时自动 build console 并内嵌。
- volume 挂载 `/data/yitrace`。

验收：

```bash
docker compose up
open http://127.0.0.1:7878
```

## P1：摄入与集成逻辑

### 3. SDK 上报体验补齐

目标：SDK 从“能发”变成“可靠发”。

Python：

- `HttpExporter` 增加失败重试。✅
- 增加最大缓冲上限。✅
- 增加 `on_error` 回调。✅
- 支持 `close()` flush 后返回明确错误或统计。已支持统计；失败保持缓冲并通过 `on_error` 报告。✅

TypeScript：

- `Tracer.close()` 改成 `Promise<void>`，等待 exporter close。✅
- `HttpExporter` 暴露 dropped / buffered / sent 统计。✅
- 增加 Node 进程退出前 flush 示例。✅

验收：

- server 先断开再恢复，SDK 不静默丢 trace。
- 最后一批 trace 在 `close()` 后一定发出或明确报错。

### 4. OTLP tenant/session 映射

目标：已用 OpenTelemetry 的应用不用改代码也能带租户、会话。

建议映射：

- `yitrace.tenant_id`。✅ direct ingest 识别；HTTP 摄入仍由 `X-Tenant-Id` 覆盖。
- `yitrace.session_id`。✅
- `session.id`。✅
- `user.id` 仅作为普通属性，不当 tenant。✅ 未纳入 tenant 映射。

原则：

- 安全边界仍以 `X-Tenant-Id` 为准。✅
- body attribute 只作为 direct ingest / 本地开发路径的普通映射；HTTP 无租户头时写入 `tenant_id=null`，不信任 body tenant。✅
- 开 auth 时，没有 `X-Tenant-Id` 可配置为拒绝。未做，留到配置系统。

验收：

- OTLP JSON 带 session attribute 后，控制台 sessions 能按会话聚合。✅
- header tenant 覆盖 body tenant 的测试继续成立。✅

### 4.5 Node/Electron 嵌入式 DB API

目标：让 Node 后端和 Electron 应用像使用 Chroma / DuckDB / SQLite 一样使用 yiTrace，不需要显式启动 server。

推荐形态：

```ts
import { YiTraceDB } from "@yitrace/db";

const db = await YiTraceDB.open("./data");
const traces = await db.search({ text: "盗刷" });
await db.close();
```

原则：

- 不是直接读 data dir 文件，而是通过 N-API 把 yiTrace Rust engine 嵌入 Node 进程。
- `@yitrace/db` 暴露 `open/close/ingest/search/sessions/trace/span`。✅
- npm 分发采用 root JS 包 + optional native platform packages，避免用户本机安装 Rust toolchain。✅
- Electron main process 持有 DB，renderer 通过 IPC 调用。文档建议已补，示例待做。
- 同一 data dir 必须有文件锁，先保证单写者。✅

参考分析：`docs/analysis/2026-07-02_embedded-node-db-api.md`。

## P1：生产信任逻辑

### 5. 配置系统

目标：不要靠散落的环境变量。

建议：

- 新增 `yt-engine/examples/server` 支持 `--config yitrace.toml`。
- 配置项覆盖：
  - bind address
  - data dir
  - auth token
  - require tenant header
  - max body size
  - flush threshold
  - vector cache bytes
  - log level

验收：

```bash
cargo run -p yt-engine --example server -- --config examples/yitrace.toml
```

### 6. 安全最小闭环

目标：PoC 前不会被安全评审一票否决。

先做最小集合：

- `require_tenant_header = true`
- API token 从文件或 env 读取，启动日志不打印 token。
- `/v1/healthz` 和 `/v1/readyz`。
- 审计日志从 stderr 升级为 JSONL 文件。
- 请求限流的简单 token bucket。

暂不做：

- 完整 RBAC
- 多用户登录
- TLS 终止，可建议放反向代理

验收：

- 未带 tenant 的写入在生产配置下返回 400/401。
- 审计日志能查到 method/path/status/tenant/body_len。

## P1：Agent 工作流逻辑

### 7. Eval 数据闭环产品化

目标：用户不仅看 trace，还能把失败变成回归集。

建议端点：

- `POST /v1/datasets`
- `POST /v1/datasets/:name/items/from-search`
- `GET /v1/datasets/:name/items`
- `POST /v1/evals/run`
- `GET /v1/evals/runs/:id`

控制台逻辑：

- 搜索结果一键加入 dataset。
- trace detail 里标注 pass/fail。
- per-agent 通过率和成本同屏显示。

验收：

- “搜 盗刷 → 加入失败集 → 跑 eval → 看到 per-agent 回归结果”形成 UI 闭环。

### 8. Agent DAG 和责任归因

目标：从 trace 树升级到“哪个 agent/tool/model 导致失败或成本异常”。

建议：

- 控制台新增 Agent Graph 视图。
- 节点显示 span count、error count、input/output tokens、cost。
- 边显示调用次数和失败率。
- 点击节点过滤 trace 列表。

验收：

- 一个多 agent 样例能显示规划 agent、执行 agent、tool 调用和失败边。

## P2：数据库内核增强

### 9. 查询索引和分页稳定性

目标：数据量上来后 UI 不靠全扫。

建议：

- session index key 改为 `(tenant_id, session_id)`，避免带租户时临时全量重建。
- trace list 增加 cursor，不只 offset。
- span detail 走 late materialization，避免列表读大字段。

验收：

- 10 万 span 下 `/v1/sessions` 和 `/v1/traces/:id` 延迟稳定。

### 10. Segment 级倒排和向量索引路线

目标：把“内存索引够用”升级为“段级索引可扩展”。

阶段：

1. 当前全局内存 BM25 保留。
2. 每个 segment 生成局部倒排。
3. 查询时段级 WAND + top-k merge。
4. 压缩 postings 和 term dictionary。

验收：

- 重启无需全量 rebuild BM25。
- 100 万 span 下 BM25 查询保持可接受延迟。

## P2：开源可信度

### 11. CI 与 Release

必须补：

- GitHub Actions:
  - `cargo test --offline`
  - Python pytest
  - TypeScript build + test
  - console build
- release workflow:
  - macOS / Linux binaries
  - Docker image
  - checksums

验收：

- README badge 变成真实 CI。
- 用户可以下载 release binary 直接跑。

### 12. Examples 和 Docs

建议新增：

- `examples/python-agent/`
- `examples/typescript-agent/`
- `examples/otlp-openinference/`
- `docs/DEPLOYMENT.md`
- `docs/SECURITY.md`
- `docs/COMPARISON.md`

验收：

- 用户能按语言和场景找到最短路径。
- 竞品比较不用在 README 里写太长。

## 推荐执行顺序

1. `scripts/demo_all.sh` + Docker Compose。
2. SDK 可靠 flush / retry / close。
3. config + require tenant + healthz/readyz + audit JSONL。
4. eval dataset API + 控制台失败集闭环。
5. CI + release binary + Docker image。
6. session index tenant 化和 cursor pagination。

这样补，yiTrace 的叙事会从“我写了一个很酷的引擎”变成“这是一个可以立刻试、可以认真 PoC、并且有自研内核差异化的 agent trace database”。
