# Node/Electron 嵌入式 DB API 判断

> 日期：2026-07-02
> 结论：应该做 `@yitrace/db`，形态类似 Chroma / DuckDB / SQLite 的 `open(data_dir)`，但实现上必须通过 yiTrace engine API，不允许用户直接读数据文件。

## 为什么值得做

对 Node 后端和 Electron 应用来说，要求用户先启动 `yitrace-server` 再写 HTTP client，会显得像外部服务，而不是数据库。更好的体验是：

```ts
import { YiTraceDB } from "@yitrace/db";

const db = await YiTraceDB.open("./data");
await db.ingest(events);
const traces = await db.search({ text: "盗刷" });
await db.close();
```

这不是“直接读文件”。`open("./data")` 应该打开的是同一个 Rust engine，只是运行在 Node 进程内。WAL、manifest、snapshot pin、折叠、BM25、向量检索、租户过滤都仍由 engine 执行。当前实现通过进程内 `EngineJsonApi` 复用 HTTP JSON 契约，不启动本地 HTTP server、不绑定端口、不走 TCP socket。

## 推荐产品分层

| 包 / 形态 | 面向用户 | 作用 |
|---|---|---|
| `@yitrace/trace-sdk` | 已有后端服务 / agent runtime | 只负责打点上报，可发到 server 或 embedded db |
| `@yitrace/db` | Node 后端 / Electron main process | 本地嵌入式 DB：`open`、`ingest`、`search`、`sessions`、`trace` |
| `yitrace-server` | 多语言 / 远程 / 团队共享部署 | 独立 HTTP/OTLP 服务、控制台、监控、鉴权 |

Electron 推荐 main process 持有 `YiTraceDB` 实例，renderer 通过 IPC 调用。这样不暴露 token，也不需要本地端口。

## MVP API

```ts
type OpenOptions = {
  dataDir: string;
  tenantId?: string | number;
};

class YiTraceDB {
  static open(dataDir: string | OpenOptions): Promise<YiTraceDB>;
  close(): Promise<void>;

  ingest(events: SpanEvent[]): Promise<{ ingested: number }>;
  ingestOtlp(body: unknown): Promise<{ ingested: number }>;

  search(query: {
    text?: string;
    vector?: number[];
    k?: number;
    filter?: {
      traceId?: string | number;
      agentName?: string;
      status?: number;
      timeFrom?: number;
      timeTo?: number;
      project_id?: unknown;
      skill?: unknown;
      mode?: unknown;
      call_site?: unknown;
      attrs?: {
        project_id?: unknown;
        skill?: unknown;
        mode?: unknown;
        call_site?: unknown;
      };
    };
  }): Promise<SearchHit[]>;

  traces(): Promise<TraceSummary[]>;
  sessions(opts?: {
    cursor?: number;
    limit?: number;
    filter?: string;
    attrs?: { project_id?: unknown; skill?: unknown; mode?: unknown; call_site?: unknown };
  }): Promise<SessionPage>;
  trace(traceId: string | number): Promise<TraceDetail | null>;
  span(traceId: string | number, spanId: string | number): Promise<SpanDetail | null>;
}
```

第一版不必暴露 SQL。yiTrace 的差异化查询不是通用 SQL，而是 agent trace replay、中文/向量混合检索、session/eval/成本归因。

## 实现路线

### 阶段 1：Rust engine API 稳定化

- 已抽出 `EngineJsonApi` 作为非网络层 JSON API 边界，HTTP server 和 `@yitrace/db` 共同复用。✅
- 后续再把 JSON 拼接逻辑逐步下沉成 typed API。
- 明确 `WriteCoordinator::open_durable(data_dir)` 的嵌入式生命周期。
- data-dir 文件锁方案已更新：旧的“同一目录只允许一个写者打开”被 2026-07-08 的引擎级多进程 embedded 取代；现在用内部 open/write 锁和 reader pin 支持同机多进程打开同一个本地 data dir。

### 阶段 2：N-API binding

- 新增 `yitrace-node/`。
- 用 N-API 暴露 `open/close/ingest/search/sessions/trace/span`。✅
- npm 包名 `@yitrace/db`。✅
- 发布 macOS arm64/x64、Linux x64/arm64、Windows x64 预编译包。

### 阶段 2.5：npm 分发

`@yitrace/db` 不能按“root 包里塞当前机器 `.node` 文件”的方式发布。那样从 macOS 发布出来的包只有 macOS 二进制，Linux / Windows 用户会安装成功但运行失败。

正确分发形态：

- root 包 `@yitrace/db`：只包含 ESM/CommonJS JS 入口、类型声明、NAPI loader、README。
- 平台包：`@yitrace/db-darwin-arm64`、`@yitrace/db-darwin-x64`、`@yitrace/db-linux-x64-gnu`、`@yitrace/db-linux-arm64-gnu`、`@yitrace/db-win32-x64-msvc`。
- root 包通过 `optionalDependencies` 依赖平台包；npm 会按 OS/CPU/libc 跳过不匹配的包。
- 发布顺序必须是“平台包先发，root 包后发”。

维护命令已写入 `yitrace-node/package.json`：

```bash
npm run npm:dirs
npm run build:release -- --target x86_64-unknown-linux-gnu
npm run release:artifacts
npm run release:prepublish  # 使用 --skip-optional-publish，避免本地脚本直接发包
npm run pack:check
npm run pack:verify         # 生成带 commit/label 后缀的 tarball，并用干净 consumer 验证
```

### 阶段 3：Electron 模板

- 新增 `examples/electron-embedded/`。
- Electron main process 打开 `YiTraceDB`。
- renderer 通过 IPC 调用 search / sessions / trace detail。
- 示例展示“不启动 server，也不占端口”的本地 trace DB 体验。

## 约束

- 不支持直接读 data dir 文件。
- 嵌入式 DB 不启动 HTTP server，不绑定本地端口，不通过 TCP socket 调用自己；只能走进程内 engine API。
- 不允许多个 Node/Electron 进程同时写同一 data dir。
- 多租户仍要通过 API filter/tenant context 进入 engine，不能让用户绕过查询路径。
- 大字段、向量索引、manifest 回收都只由 engine 管理。

## 2026-07-02 集成缺口复核

复核 AgenticData 侧提出的 4 个剩余问题后，结论是：都成立，但优先级和改动半径不同。

| 问题 | 当前状态 | 建议处理 |
|---|---|---|
| direct `db.ingest()` 的 `session_id` / `trace_id` / `span_id` 只吃可解析 `u64` | 成立。`parse_wire_batch` 仍通过 `req_u64` / `opt_u64` 解析这些字段，UUID 字符串不能直接进入 direct wire ingest。 | 短期可在接入方稳定 hash；yiTrace 侧应补 external id 保留字段，避免只剩 hash 后难以回查。 |
| `SpanEvent.attrs` 只是类型声明 | 成立。TypeScript 类型有 `attrs?: Record<string, unknown>`，但 `WireRecord`、`SpanFields`、折叠和持久化层没有 attrs 字段，实际会被忽略。 | 作为 schema 演进实现：wire -> fold -> WAL/segment/manifest -> HTTP/Node 查询输出，先限定 attrs 为可持久化的 JSON 标量/数组/对象子集。 |
| `OpenOptions.readOnly` 未实现 | 成立。JS `open()` 不读取 `readOnly`，native constructor 仍会获取写锁并 `open_durable`。 | 要么实现真正 read-only open，要么先从类型和文档里删掉，避免误导用户。短期更建议删掉声明。 |
| Electron 打包说明不足 | 成立。loader 已有 `NAPI_RS_NATIVE_LIBRARY_PATH` fallback，npm 也采用 optional platform packages，但 README 还没明确 asar unpack、optional native package 保留和 fallback 用法。 | 先补文档；后续加 `examples/electron-embedded/` 验证打包产物。 |

其中前两项是核心数据模型问题，不能只在 Node binding 层补一层映射就结束。推荐顺序：

1. 先去掉或实现 `readOnly`，并补 Electron packaging 文档；这是低风险 DX 修复。
2. 再做 external id 设计：内部 `u64` 仍可作为索引 key，但保留 `external_trace_id`、`external_span_id`、`external_session_id` 用于展示、回查和跨系统关联。
3. 最后做 attrs 持久化：需要同步升级 wire 编解码、折叠、WAL/segment/manifest 序列化和查询输出，并补旧数据兼容测试。

### 修复结果

已在 yiTrace 侧修复：

- direct `db.ingest()` 支持 UUID / 任意字符串形式的 `trace_id`、`span_id`、`parent_span_id`、`session_id`。内部仍使用稳定 hash 后的 `u64` 做索引 key，原始值保存在 `external_trace_id`、`external_span_id`、`external_parent_span_id`、`external_session_id`。
- `attrs` 已进入 `WireRecord`、`SpanFields`、折叠、WAL/segment/manifest 共用编码和 HTTP/Node 查询输出。实现采用 top-level JSON object merge：同 key 后到覆盖，value 保留为已校验 JSON 字面量。
- WAL/segment batch 和独立 `SpanFields` 编码已加 v2 魔数和长度边界，新写入支持新字段，旧格式仍走 legacy decode。
- `OpenOptions.readOnly` 已从 TypeScript 类型中移除，JS 运行时遇到 `readOnly` 会直接报错，避免用户误以为当前是无写锁/无写入的真正只读模式。
- Electron 打包说明已补到 `yitrace-node/README.md`：main process 打开 DB、asar unpack `.node`、保留 optional native packages、`NAPI_RS_NATIVE_LIBRARY_PATH` fallback。

## 2026-07-02 AgenticData 最小需求处理

本轮只针对 AgenticData 当前提出的 P0/P1 做收敛：

| 需求 | 处理结果 |
|---|---|
| P0：稳定安装来源 | 已补 `npm run pack:local`，可生成 root `@yitrace/db` 和本机已有平台 optional package 的不可变 tarball。文件名追加 commit/label，例如 `yitrace-db-0.0.1-g1a2b3c4d5e6f.tgz`，并写入 `dist/pack-manifest.json`。AgenticData 可把 tarball 放进仓库 `vendor/` 后用 `file:` 锁版本，或上传内部 npm 源。正式公开发布仍要求平台包先发布、root 包后发布。 |
| P0：补齐 `SpanEvent` 类型声明 | 已在 `index.d.ts` 显式声明 `duration_ns`、`tool_name`、`model`、`input_text`、`output_text`，并保留 camelCase builder 输入如 `durationNs`、`toolName`、`inputText`。 |
| P1：轻量 event builder/helper | 已新增 `SpanEventBuilder` / `createSpanEventBuilder`，隐藏 `seq`、`event_type`、start/end 双事件和 `ext_span_id`。AgenticData 只需要调用 `startSpan` / `log` / `endSpan` / `ingest`。 |
| P1：attrs 过滤 | 已在 engine sidecar 中加入 `project_id`、`skill`、`mode`、`call_site` 精确过滤；HTTP 和 Node search 均支持 top-level filter 与 `filter.attrs` 两种写法。 |
| P1：attrs round-trip 契约 | 已写入 `docs/API_REFERENCE.md` 和 `yitrace-node/README.md`：`attrs` value 支持 object / array / string / number / bool / null，返回时恢复同样 JSON 形态；同 key 后到覆盖。 |

当前边界：

- `attrs` 会完整持久化并返回，但只有 `project_id`、`skill`、`mode`、`call_site` 四个 key 承诺过滤。
- attrs filter 是精确匹配，不做 contains / prefix / numeric range。
- `pack:local` 只会打包当前构建机已有的 native `.node` 平台包。多平台 tarball 仍需要 CI 或对应平台机器先产出 artifacts。
- 本地交付包不允许长期覆盖复用 `0.0.1.tgz`；payload 变化时必须换文件名、commit label 或 registry version。

## 2026-07-02 安装产物与 consumer 验收

新增 `npm run pack:verify`，它会：

1. 运行 `pack:local` 生成带 commit/label 后缀的 root tarball + 当前平台 optional package tarball，并写 `dist/pack-manifest.json`。
2. 创建干净临时 consumer 项目。
3. 用 npm 安装 tarball，而不是从源码目录加载。
4. 验证 ESM `import`、CommonJS `require`、当前平台 optional package 内的 native `.node` 文件可解析。
5. 实际打开 `YiTraceDB`，用 builder ingest，跑 search attrs filter 和 sessions attrs filter。

平台策略已明确：

- AgenticData server 当前默认 x64：Node、DuckDB、yiTrace native、sqlite native 必须全部保持 x64。
- 不允许混用 x64/arm64 native 依赖。若 AgenticData server 切 arm64，必须先把 DuckDB 和 sqlite 也切成 arm64，或给它们建立和 `@yitrace/db` 一样的 per-platform optional package 策略。
- AgenticData 本地开发可以继续锁 macOS arm64：`yitrace-db-0.0.1-g<commit>.tgz` + `yitrace-db-darwin-arm64-0.0.1-g<commit>.tgz`；这不代表 server 架构已切 arm64。
- `pack:local` 会同时打包构建机上已有的其他平台 native 包；当前机器也可产出 macOS x64 tarball。
- 正式 npm/CI 发版仍保留 optional platform package 矩阵：macOS x64/arm64、Linux x64/arm64 glibc、Windows x64 MSVC；必须先发布平台包，再发布 root 包。

sessions attrs filter 已补：`GET /v1/sessions?attrs=...`、`GET /v1/sessions?project_id=...&skill=...` 和 Node `db.sessions({ attrs })` 都可用。语义是“会话内至少一个 span 命中所有 attrs 条件，则返回完整 session 聚合行”。

仍未做的是“真正 read-only open”：它需要 engine 提供不创建/不锁写入路径、不打开 append WAL、不触发 flush/manifest 写入的只读 coordinator。这不应由 Node 层伪装。

## 2026-07-02 logEvents 详情返回

新增 trace/span 详情里的原始日志事件返回，避免接入方把日志镜像进 `attrs.event_logs`：

- `GET /v1/traces/:id` 的每个 span item 新增 `logEvents`。
- `GET /v1/traces/:id/spans/:spanId` 新增 `logEvents`。
- `logEvents[]` 字段包含 `eventId`、`ts`、`seq`、`eventType`、`messages`、`attrs`。
- 读取时按可见 span key 过滤，继承 trace/span 详情已有的 tenant 隔离和删除过滤。
- 事件来源覆盖热 MemTable 和 flush 后 segment，按 `ts, seq, eventId` 排序，并按 deterministic `eventId` 去重。

边界：当前只返回携带 `logs` 的原始事件；复杂事件搜索、按日志全文过滤和单独 `/events` 分页接口后续再做。

## 对外说法

当前可以说：

- yiTrace 已支持独立 DB server + HTTP/OTLP/SDK 接入。
- Node/Electron 嵌入式 DB 已有 MVP 包 `yitrace-node/`，接口为 `YiTraceDB.open("./data")`。
- 这不是直接读文件，而是把 yiTrace Rust engine 嵌入 Node 进程。
- 对外 npm 包名是 `@yitrace/db`；正式发布时使用 optional platform packages，不要求用户本机安装 Rust toolchain。

后续 `@yitrace/db` 稳定后，README 的 Node Quick Start 应优先展示 embedded API；server 形态作为“远程/多语言/共享部署”路径。
