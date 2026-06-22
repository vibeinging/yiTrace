# tracevault 扩展（产物③）

yiTrace 三产物中的**数据库本体**：把 Agent trace 的专用存储（表/分区/索引/中文词典/trace 函数/折叠逻辑）做成一个 **yiTrace(openGauss) 扩展**，像 pgvector/TimescaleDB 之于 PostgreSQL。

> 另两个产物（不在本目录）：① 带前端的应用二进制（Web 控制台 + 读后端 + OTLP/LangSmith 接收器，Rust+TS）；② 打点 SDK（Python/TS）。后台维护（折叠/冻结/GC）由产物① 的守护进程驱动，调用本扩展的 `fold_trace` 等函数。

## 安装
```bash
# 方式一：PGXS（[验] openGauss 的 pg_config/PGXS 是否兼容）
make && make install

# 方式二：手工拷贝（PGXS 不可用时）
make install-manual PGSHARE=$(pg_config --sharedir)
#   或直接 cp tracevault.control tracevault--1.0.sql 到 <sharedir>/extension/

# 登库启用
psql -d <db> -c "CREATE EXTENSION tracevault;"
```

## 装完有什么
- **表**：`span_events`(append-only事件,RANGE+INTERVAL分区) · `span_current`(折叠态,ASTORE) · `span_current_cold`(列存) · `span_vectors`(floatvector) · `payload_store`(CAS) + 控制/队列表(fold_dirty/frozen_registry/freeze_jobs/…)
- **索引**：树/线程/活态 btree · JSONB GIN · **DiskANN 带过滤 ANN** · **BM25(fulltext)+vex_jieba 中文**
- **函数（这个库的"语言"）**：`load_trace_tree` · `subtree` · `rebuild_thread` · `semantic_recall` · `export_trajectory` · `fold_trace` · `jsonb_deep_merge_agg`

## 用法
```sql
SELECT * FROM tracevault.load_trace_tree(1, 1234567890);              -- 整棵决策树
SELECT * FROM tracevault.rebuild_thread(1, 7701);                     -- 线程重建(多轮会话)
SELECT * FROM tracevault.subtree(1, 9876543210);                     -- 子树(区间范围扫)
SELECT span_id,dist FROM tracevault.semantic_recall(1, '[...]'::floatvector, 20, 7, 1); -- 带过滤语义召回
-- 摄入侧(产物①/②)写入,同事务:
--   INSERT INTO tracevault.span_events(...) VALUES (...);
--   INSERT INTO tracevault.fold_dirty(tenant_id,trace_id,span_id) VALUES (...);
```

## ⚠️ 上机必验 [验]（配 `../docs/design/poc/verify_opengauss_yitrace.sql` 逐条验证后再定稿）
1. `RANGE ... INTERVAL('1 day')` 自动分区；`PRIMARY KEY(ts,event_id)` 分区本地唯一。
2. **CStore + INTERVAL('1 month')** 是否支持自动建月分区（不支持则改手工建分区）。
3. **`INSERT ... ON DUPLICATE KEY UPDATE` + `EXCLUDED` 在 plpgsql 函数内**的语义（fold_trace 折叠核心）；是否需 dbcompatibility=B。
4. **BM25**：`USING fulltext(col) WITH(DICTS#ALGORITHMS#COEFFICIENTS)` 精确写法；`@~@` + `bm25_score()`。
5. **DiskANN inplace-filter**：`USING diskann(embedding, tenant_id, span_kind)` 复合列序/WITH 参数；带过滤召回是否吃下标量过滤、不 under-fill。
6. **距离算子** `<=>`(余弦) / `floatvector` 维度。
7. **`vex_jieba`** 词典模板 + `vexjieba_add_userdict/reload`。
8. **`CREATE AGGREGATE`** 自定义有序聚合在折叠期(query_dop=1)的保序与性能；大 trace 改应用层 DFS。
9. **`CREATE EXTENSION`** 对自定义扩展(非 contrib)的支持。

## 边界
- 这是**产物③**：数据库本体（trace 能力）。**不含**摄入网关/Web/SDK（在产物①②）。
- 后台维护（折叠环/冻结/重融化/GC）的**调度逻辑**见 `../docs/design/2026-06-17_tracevault-background-scheduling.md`，由产物① 守护进程驱动（调用 `fold_trace` 等）；若 openGauss 暴露 bgworker API，可改为库内 worker（[验]，则本扩展带一个 `.so`）。
- 设计全集：`../docs/design/2026-06-16_tracevault-schema.md`（数据模型，已修 USTORE/ON CONFLICT 两 bug）。
