# yitrace-console

yiTrace 的控制台前端。**轻但真**的 SPA：组件化 + 虚拟滚动 + 游标分页 + 数据缓存，扛得住上千会话 / 上千 span 的真实量级。构建产物是几个静态文件（~216KB JS / 8KB CSS，gzip 68KB），可用 `rust-embed` 塞进引擎单二进制——运行期仍单机零依赖、气隙可部署。

## 技术栈

| 件 | 作用 |
|---|---|
| React + Vite + TypeScript | 组件化、构建、类型 |
| @tanstack/react-query | 数据拉取 / 缓存 / 游标无限分页 / 懒加载 |
| @tanstack/react-virtual | 会话列表、瀑布 span 的虚拟滚动（千行只渲染可视区） |

## 扛量的三件事（都已落地）

1. **虚拟滚动**：会话列表与瀑布都只渲染可视区那几十行（`useVirtualizer`），4000 会话 / 千 span 不卡。
2. **游标分页**：会话列表 `useInfiniteQuery` 按游标分页，滚到底自动拉下一页，不全量拉。
3. **大字段晚物化**：span 的输入/输出大文本，选中某个 span 才单独拉（`getSpanDetail`），不进列表/瀑布查询。

## 信息架构

左栏按 **session 分组**：单轮会话一条普通项；多轮会话是「组头（标题 + 🧵N轮 + 合计）+ 折叠在下面的每一轮」，展开才拉该会话的轮次。中栏选中 trace 出瀑布，右栏选中 span 出详情。

## 数据层

`src/api/` 下契约与实现分离：

- `types.ts` —— `TraceApi` 接口 + 数据模型
- `mock.ts` —— 确定性生成 4000 会话 + 大 trace，前端独立可跑、演示量级
- `http.ts` —— 对接引擎 HTTP 网关（**端点已在引擎实现**）
- `index.ts` —— 开关：默认 mock，`VITE_API=http` 走真实引擎

引擎已实现的控制台端点（`yt-engine/src/http.rs`）：

```
GET /v1/sessions?cursor=&limit=&filter=     游标分页的会话列表
GET /v1/sessions/:id/turns                  会话的轮次
GET /v1/traces/:id                          一条 trace 的折叠 span（瀑布）+ 摘要
GET /v1/traces/:id/spans/:spanId            span 大字段（晚物化）
```

> 引擎数据模型的现实约束（前端据此降级）：折叠后的 span **不保留 kind / name / 起始时刻**，
> 故 kind/name 由 agent/tool/model 派生、瀑布起始时刻按 span_id 顺序累加 duration（逻辑瀑布）。
> 会话列表当前每页全量扫一遍聚合（O(spans)），上真量级要加 session 边车索引。

## 开发

```bash
bun install
bun run dev          # 开发服务 :5180（默认 mock 数据）
bun run build        # 类型检查 + 构建到 dist/
```

## 接进引擎单二进制（已打通）

```bash
# 1. 构建（对接真实引擎）
VITE_API=http bun run build
# 2. 拷进引擎，编译期内嵌
cp -r dist ../yitrace-engine/crates/yt-engine/console_dist
# 3. 跑引擎控制台服务（启动自带种子数据）
cd ../yitrace-engine && cargo run -p yt-engine --example server
#    → http://127.0.0.1:7878/  引擎二进制直接服务控制台 + 数据
```

引擎 `build.rs` 在编译期把 `console_dist/` 用 `include_bytes!` 内嵌（零外部依赖），HTTP 网关
`GET /`（非 `/v1/*`）从内嵌资源服务。`console_dist/` 不存在时生成空表、引擎照常编译。运行期零新增依赖、气隙可部署。
