# yiTrace 发行版打包架构 —— 把 trace 专用层做成 yiTrace 衍生数据库

> 日期：2026-06-17｜本文是**产品化/交付层**设计，位于 引擎/schema/调度/摄入 之上。
> 回答的问题：**怎么把 trace 专用层做成"yiTrace 衍生发行版"(一个数据库),而不是"openGauss + 一个外挂 app(服务/平台)"。**
> 配套：`2026-06-16_tracevault-schema.md`(数据模型)·`2026-06-17_tracevault-background-scheduling.md`(调度)·`2026-06-17_tracevault-ingestion.md`(摄入)·`2026-06-16_tracevault-platform-gateway-trace-browser.md`(平台/浏览器)·`appendix-K`(内核边界)。

## 0. 定位与参照:TimescaleDB 模式
**TimescaleDB = PostgreSQL 扩展,但对外是"时序数据库"**(hypertable/chunk/压缩/continuous aggregate = 它的专用数据组织,作为 PG 扩展交付,却是一家"数据库"公司)。
→ **yiTrace = yiTrace(openGauss) + trace 专用扩展 + 后台维护进程 + 摄入网关 + 浏览器,作为"Agent trace 数据库"发行版交付。** 同一打法,成熟、走通过。
- 这是"数据库"而非"服务"的依据:**trace 的数据模型/函数/索引是引擎的一部分(随发行版交付),维护进程是产品的一部分(像 SmithDB 的 compaction 服务),用户连上去跑 trace SQL。** 摄入网关和浏览器是这个数据库的"写入前端"和"客户端"。

---

## 1. 五层打包架构

```
┌─────────────────────────────────────────────────────────────────┐
│ L5 发行版打包:initdb 模板 + 一键部署 + 信创适配 + 迁移/版本工具      │
│ ┌─────────────────────────┐  ┌──────────────────────────────┐    │
│ │ L4 摄入网关(OTLP/LangSmith│  │ L4 浏览器(三视图+线程重建)      │    │ ← DB 的写入前端 + 客户端
│ │   -compat → SQL)         │  │   (静态资源)                  │    │
│ └────────────┬────────────┘  └──────────────┬───────────────┘    │
│              │ 普通 SQL / PG 线协议           │ Gateway(读)          │
│ ┌────────────▼───────────────────────────────▼───────────────┐   │
│ │ L3 后台维护进程(折叠/冻结/GC/索引重建) = 数据库的内部维护      │   │ ← 随库起停,像 autovacuum
│ ├──────────────────────────────────────────────────────────────┤  │
│ │ L2 tracevault 扩展:trace 表/分区/索引 + 内置 trace 函数        │   │ ← trace 的数据模型(引擎的一部分)
│ ├──────────────────────────────────────────────────────────────┤  │
│ │ L1 yiTrace/openGauss 内核(不动):向量/BM25/分区/JSONB 已内置      │   │ ← 引擎底座
│ └──────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
```

### L1 · 内核底座(不动)
yiTrace 二进制照用:`floatvector` 类型、hnsw/diskann 索引 AM、`fulltext`(BM25)+`vex_jieba`、RANGE+INTERVAL 分区、JSONB+GIN —— 已编进内核(`appendix-K` 证实)。这是引擎的既有能力,**v1 不改**。

### L2 · `tracevault` 扩展(trace 的数据模型 + 语言)
做成**数据库扩展**(像 PostGIS/TimescaleDB):`CREATE EXTENSION tracevault`,或 initdb 模板自动装。内含:
- **预置表/分区/索引**:`span_events`(ASTORE,RANGE+INTERVAL)、`span_current`(ASTORE,单 PK)、`span_current_cold`(CStore)、`span_vectors`、`payload_store`、`fold_dirty`、`frozen_registry`、`late_event_inbox`、控制表。装上即有,用户不建表。
- **内置 trace 函数/算子**(成为这个库的"语言"):`load_trace_tree(trace_id)`、`subtree(span_id)`、`rebuild_thread(thread_id)`、`semantic_recall(span,k,filter)`、`export_trajectory(...)`、折叠深合并聚合 `tv_jsonb_deep_merge_agg`、`fold_trace(tenant,trace)`。
- **打包形态**(已核实 openGauss 支持 `.control`/`CREATE EXTENSION` —— contrib 下大量 .control):
```
tracevault.control          # 扩展元数据(版本/依赖/可重定位)
tracevault--1.0.sql         # 安装脚本:CREATE TABLE/INDEX/FUNCTION/AGGREGATE…
tracevault--1.0--1.1.sql    # 版本升级脚本(迁移)
```
- → **纯 SQL/plpgsql,不改内核 C。**

### L3 · 后台维护进程(折叠/冻结/GC = 数据库的内部维护)
调度逻辑(微批折叠、冻结、重融化、GC、索引重建,见调度文档)是**数据库的内部维护**,不是外挂 cron。**两种交付,v1 推荐 (b):**

- **(a) 库内 background worker**:openGauss bgworker 注册,随库起停。**⚠️ [验]**:openGauss 基线 PG 9.2.4 早于 PG 9.3 的 BackgroundWorker API,**是否暴露可注册 bgworker 的公开接口待核实**(openGauss 是线程池模型,有自己的后台线程框架)。需一个 `shared_preload_libraries` 的小 `.so`(扩展级,非内核 fork)。
- **(b) 发行版捆绑的维护守护进程(sidecar,v1 默认)**:一个随发行版一起装的守护进程,**用普通 SQL 连进库**跑折叠/冻结/GC 循环。**不依赖任何内核/扩展 API,一定能用**,而且这正是 **SmithDB 的做法**(它的 ingestion/compaction 是独立服务进程)。它由发行版的部署单元统一拉起/监控,生命周期与库绑定 → 仍是"这个数据库的维护进程",不是用户的脚本。
- → **v1 用 (b),零新 C 代码**;(a) 作为后续把维护"沉进库内"的优化([验] API 后)。

### L4 · 摄入网关 + 浏览器(数据库的前端)
- **摄入网关**(OTLP/gRPC+HTTP、OpenInference、LangSmith-compat REST → 归一 → `INSERT span_events + fold_dirty`):随发行版交付,是这个库的**写入网关**(像 SmithDB 的 ingestion service)。应用层进程(Rust/Go/Python 皆可)。
- **浏览器**(三视图 + 线程重建):静态资源,经 Platform Gateway(读)访问引擎,是这个库的**客户端**(像 LangSmith UI)。
- → 应用层,不碰内核。

### L5 · 发行版打包(交付为"一个数据库")
- **initdb 模板**:新实例初始化时自动 `CREATE EXTENSION tracevault` + 建初始分区 + 拉起维护守护进程 + 配 GUC(分区策略/采样率/保留)。→ 装完连进去**就是个 trace 数据库**。
- **一键部署**:单静态二进制 / Docker Compose(库 + 维护进程 + 网关 + 浏览器),本地盘零外部依赖,气隙可装。
- **信创适配**:鲲鹏/飞腾/海光 + 麒麟/统信,**继承 yiTrace 的信创认证**(同内核二进制,trace 层是扩展+应用,增量测评在应用软件层,见 v3 战略文档)。
- **迁移/版本工具**:`tracevault--X--Y.sql` 扩展升级脚本 + 数据格式 N-2 回读 + golden corpus CI(见 schema 文档)。

---

## 2. 内核 / 扩展 / 应用 边界(一张表说清"动不动内核")

| 层 | 内容 | 动内核? | 形态 |
|---|---|---|---|
| L1 | 向量/BM25/分区/JSONB | 否(已在) | 既有 yiTrace 二进制 |
| L2 | trace 表/索引/函数 | **否** | SQL/plpgsql 扩展(`.control`+SQL) |
| L3 | 折叠/冻结/GC 维护 | **否(v1 sidecar)** | 捆绑守护进程(SQL 客户端);(a) 库内 bgworker 需扩展级 `.so`[验] |
| L4 | 摄入网关 / 浏览器 | 否 | 应用层进程 / 静态资源 |
| L5 | 打包/部署/信创/迁移 | 否 | 发行版工程 |
| v2 条件性 | merge-on-read 算子 | **可能是**(仅当折叠吞吐 benchmark 不达标) | 内核算子(Rust 嵌入,Doris 套路) |

> **结论:v1 整套发行版 = 既有 yiTrace 二进制 + SQL 扩展 + 捆绑守护进程 + 网关 + 浏览器 + 打包工程,零新内核 C。** "数据库"的身份来自:数据模型/函数/索引是引擎的一部分(扩展)、维护是产品的一部分(守护进程)、作为数据库交付(连上去跑 trace SQL)。

---

## 3. 用户视角(装完之后)
```bash
# 安装发行版(一键)
docker compose up   # 或 ./tracevault-install.sh   → 库 + 维护进程 + 网关 + 浏览器

# 1) 它就是个数据库:连进去跑 trace SQL
psql -h host -d tracevault
  > SELECT * FROM load_trace_tree('4f9a-7e21');
  > SELECT span_id FROM semantic_recall('保本话术', 20, '{"tenant":1,"days":7}');
  > SELECT model, sum(total_cost) FROM span_current_cold GROUP BY model;

# 2) OTLP / LangSmith-compat 指过来 → 自动摄入
export OTEL_EXPORTER_OTLP_ENDPOINT=http://host:4317   # 现有 agent 零改码

# 3) 浏览器看 trace(三视图 + 线程重建)
open http://host:8080
```
对用户:**yiTrace 就是一个专门存 trace 的数据库**,不是"openGauss + 一个 app"。

---

## 4. 落地步骤(给排期)
1. **抽扩展**:把已设计的 schema DDL + trace 函数 整理成 `tracevault` 扩展(`tracevault.control` + `tracevault--1.0.sql`)。验证 `CREATE EXTENSION tracevault` 在 yiTrace 实例跑通(配合 schema PoC)。
2. **维护守护进程(sidecar)**:实现折叠/冻结/GC/索引重建循环(SQL 客户端 + advisory 锁 + 脏队列消费,见调度文档)。打成发行版的一个进程单元。
3. **initdb 模板**:新实例自动装扩展 + 建初始分区 + 拉起守护进程 + 配 GUC。
4. **摄入网关 + 浏览器**:网关(OTLP/LangSmith-compat → SQL)+ 浏览器(已有原型 → 接 Gateway)打进同一发行包。
5. **打包/部署/信创/迁移**:单包/Compose + 国产 CPU/OS 适配 + 扩展升级脚本 + golden corpus 回读 CI。
6. **(后续/可选)** 若 openGauss 暴露 bgworker API → 把维护从 sidecar 沉进库内;若折叠吞吐 PoC 不达标 → 评估 merge-on-read 内核算子(v2)。

## 5. 待核实 [验]
1. **openGauss `CREATE EXTENSION` + `.control`** 对自定义扩展(非 contrib)的支持度与限制(contrib 已用此机制,自定义扩展应同理,需实测安装)。
2. **openGauss 是否暴露可注册的 background worker / 后台线程 API**(决定 L3 能否"沉进库内";v1 用 sidecar 不依赖它)。
3. **initdb 模板/钩子**:openGauss 初始化时自动装扩展 + 起伴随进程的标准做法(或用部署脚本兜底)。
4. 扩展里 plpgsql 函数 + 自定义聚合在目标版本的行为(配合 schema PoC)。

---
> 一句话:**像 TimescaleDB 之于 PostgreSQL —— `tracevault` 扩展(数据模型+函数)+ 捆绑维护守护进程(折叠/冻结/GC)+ 摄入网关 + 浏览器,用 initdb 模板和一键部署打成 yiTrace 衍生发行版,作为"Agent trace 数据库"交付。** trace 专用层是引擎的一部分,不是外挂 app;v1 零新内核 C;继承 yiTrace 信创认证。
