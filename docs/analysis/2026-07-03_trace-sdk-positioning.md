# trace-sdk 优先的对外定位

> 日期：2026-07-03
> 结论：对外第一印象应是 `trace-sdk` 和 Agent 运行回放，不是 TraceDB / database。

## 背景

`TraceDB` 或 `trace database` 虽然准确描述了底层能力，但会让第一次接入显得很重。用户容易联想到部署、运维、迁移、备份、锁和数据目录治理；而 Agent 开发者最先需要的是“加几行代码，把一次运行记录下来，出问题时能回放”。

因此 README 和 SDK 文档的入口口径需要从“数据库”改成“trace-sdk + run replay”：

- 先接入 Python / TypeScript SDK。
- 发送到本地 yiTrace collector/server。
- 打开 console 回放多轮会话、工具调用、token 成本和失败现场。
- 需要 Node/Electron 进程内持久化时，再使用 `@yitrace/db`。
- 底层 engine/db 能力保留在架构和高级用法里，不作为第一屏主卖点。

## 推荐分层

| 层 | 对外叫法 | 作用 |
|---|---|---|
| 默认接入 | `@yitrace/trace-sdk` / Python `yitrace` | 记录 Agent 运行并上报 |
| 本地服务 | yiTrace collector/server | 接收 SDK/OTLP，提供 API 和 console |
| 用户体验 | run replay / trace console | 回放、搜索、下钻、成本和 eval |
| 高级嵌入 | `@yitrace/db` | Node/Electron 进程内本地持久化 |
| 底层能力 | engine / trace store | WAL、fold、BM25、vector、attrs、eval 数据 |

## README 文案准则

- 第一屏避免把 yiTrace 定义成 database。
- 可以说“local collector”“run replay”“flight recorder for agent runs”。
- `@yitrace/db` 放在高级路径，不抢 `trace-sdk` 的主入口。
- “数据库/engine”用于解释为什么它能私有化、可检索、可持久化，而不是让用户先承担一个数据库心智。

当前 README 已按这个口径调整：英文首句为 “Trace SDK and run replay for AI agents.”，中文首句为“AI Agent 的 trace-sdk 和运行回放工具。”
