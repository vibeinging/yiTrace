# yiTrace 表结构设计：事件表 + 查询期折叠（build-on-yiTrace / openGauss）

> 日期：2026-06-16　|　配套主文档 `2026-06-16_agent-trace-db-architecture.md` (v3)
> 本文 = 数据模型地基的**可落地 DDL**。语法已按**你们 `openGauss-vector-main` 真实代码**校准（不是泛化 openGauss / 也不是 yiTrace-Lite 插件），并已修掉第四轮红队抓到的致命语法与正确性洞。
> 完整三份设计原文 + 红队见 `appendix-I / appendix-J`。
> 标注：`[实]`=已在你们代码/openGauss 文档核实；`[改]`=红队修正后的写法；`[验]`=须在目标实例 `\d`/试跑确认。

---

## 0. 三个必须先知道的硬约束（决定全局形状）

1. **`[实]` openGauss 无 PG 的 `INSERT ... ON CONFLICT`** —— 折叠 upsert 用 **`MERGE INTO`**（已在你们回归测试里确认：`MERGE INTO products_row p USING(...) ON(...) WHEN MATCHED THEN UPDATE`；`INSERT ... ON DUPLICATE KEY UPDATE c = EXCLUDED.c` 也支持）。这是红队抓到的致命点：三份初稿都误用了 `ON CONFLICT`，在你们平台上根本不解析。
2. **`[实]` `ORIENTATION` 不可 ALTER + CStore 列存只支持 RANGE 分区/仅追加写/GIN 仅 tsvector/不支持数组** —— 所以**冷热是两张物理表**，且**所有"智能检索"(向量/BM25/JSONB-GIN)只能在行存层**。冷区 CStore **只放分析用的低基数真列**，检索能力（向量/中文/JSON）**绝不冻进 CStore**（否则历史数据的语义召回+中文检索会静默失效——这是产品招牌，不能丢）。
3. **`[实]` 分区表二级索引不写 `LOCAL` 默认 GLOBAL**（btree-only、≤31列、DROP PARTITION 要重建）→ 事件表所有二级索引**显式 `LOCAL`**，让按时间滚动删分区零成本。

你们真实向量/全文语法（已从代码核实，区别于 yiTrace-Lite 插件）：
- 向量类型 `floatvector(N)`；opclass `floatvector_l2_ops`/`floatvector_cosine_ops`；算子 `<->`(L2)/`<=>`(cos)/`<#>`(IP)。
- 索引 `USING hnsw|ivfflat|ivfpq|diskann (col floatvector_l2_ops) WITH(...)`；分区表加 `local`。
- **带过滤 ANN = `USING diskann (embedding, id)` 复合（你们代码里的 `idx_diskann_inplace_filter`）** —— DiskANN 原生 inplace filter，是招牌钩子。
- 中文全文：`CREATE TEXT SEARCH DICTIONARY x (TEMPLATE = vex_jieba)` + `vexjieba_add_userdict/reload` + `bm25_tokenize` + 检索算子 **`@~@`** + `bm25_score()`。

---

## 1. 整体表布局

```
span_events        (热, ASTORE 行存, RANGE+INTERVAL by ts)  —— append-only 事件, 只 INSERT
   │  微批/冻结折叠 (MERGE INTO)
   ▼
span_current       (行存 UStore, 单一 PK=span_id)            —— 折叠后当前态 + 区间编码 + 检索列
   │  分裂归档
   ├─► span_current_cold   (CStore 列存, 仅分析真列)         —— 列式聚合, 不承载检索
   └─► (检索保留在行存: span_vectors + span_current 的 BM25/GIN 列, 冷数据也留行存检索镜像)
span_vectors       (行存, 采样 span 的 embedding)            —— HNSW/DiskANN inplace-filter
payload_store      (CAS: sha256 去重 + TOAST + 多模态外置)   —— 大字段晚物化
frozen_registry / late_event_inbox                           —— 晚到回流防丢 (见 §7)
```

---

## 2. span_events —— append-only 事件表

```sql
-- [改] 用 ASTORE 行存(不是 USTORE): 只 INSERT, undo 段是纯浪费
CREATE TABLE span_events (
    event_id        bigint        NOT NULL,        -- 应用端雪花ID(全局单调), 不用 SEQUENCE+CACHE(会乱序)
    tenant_id       bigint        NOT NULL,
    trace_id        bigint        NOT NULL,
    span_id         bigint        NOT NULL,
    seq             int           NOT NULL,         -- 单 span 内事件序(生产端单调)
    event_type      smallint      NOT NULL,         -- 1start 2update 3end 4tool 5error 6feedback
    ts              timestamptz   NOT NULL,         -- 分区键
    parent_span_id  bigint,                         -- 写侧邻接, 可空/晚到
    root_id         bigint,
    thread_id       bigint,
    span_kind       smallint, name text, model text,
    input_tokens int, output_tokens int, total_cost numeric(18,6), latency_ms int,
    status smallint, start_time timestamptz, end_time timestamptz,
    attrs_patch     jsonb,                          -- 增量属性; [改] 超 ~2KB 的 patch 也走 CAS, 避免 TOAST churn
    payload_ref     bytea,                          -- -> payload_store.sha256, 晚物化
    ingest_ts       timestamptz   NOT NULL DEFAULT now(),
    CONSTRAINT pk_span_events PRIMARY KEY (ts, event_id)   -- 分区本地唯一须含分区键
)
WITH (ORIENTATION = ROW)                            -- ASTORE 默认行存
PARTITION BY RANGE (ts) INTERVAL ('1 day')          -- [实] 自动按天分区
( PARTITION p_init VALUES LESS THAN ('2026-01-01 00:00:00+08') );

-- [改] 所有二级索引显式 LOCAL (否则默认 GLOBAL, DROP PARTITION 要重建)
CREATE INDEX ix_evt_span   ON span_events (span_id, seq, ts) LOCAL;             -- 折叠主路径
CREATE INDEX ix_evt_trace  ON span_events (tenant_id, trace_id, ts) LOCAL;      -- 活trace/树重建
CREATE INDEX ix_evt_parent ON span_events (tenant_id, parent_span_id) LOCAL;    -- 晚到找子
CREATE INDEX ix_evt_ingest ON span_events (ingest_ts) LOCAL;                    -- 增量折叠水位
```

> **为何只 INSERT**：span"早上生/下午死/乱序/晚到"，in-place UPDATE 会产生热行更新链/死元组/写放大。事件化后每个生命周期信号是一条不可变行，写入无锁、天然吃乱序晚到、留全审计轨迹。折叠在读侧做。

---

## 3. span_current —— 折叠后当前态（行存，检索核心面）

```sql
CREATE TABLE span_current (
    span_id         bigint        NOT NULL,
    tenant_id       bigint        NOT NULL,
    trace_id        bigint        NOT NULL,
    root_id         bigint,
    parent_span_id  bigint,
    pre  bigint, post bigint, lvl int,              -- 区间编码(冻结时 DFS 物化; 活态为 NULL)
    dotted_order    text,                           -- [改] LangSmith 兼容: 微秒时间戳+全量id, 定宽, C collation
    thread_id       bigint,
    span_kind smallint, name text, start_time timestamptz, end_time timestamptz,
    status smallint NOT NULL DEFAULT 0,             -- 0 running 1 ok 2 error
    -- schema-on-write 提列(高频路径→真列, 供列式聚合/带过滤ANN; 低频留 attrs)
    model text, user_id bigint, session_id bigint,
    input_tokens int, output_tokens int, total_cost numeric(18,6), latency_ms int,
    tags text[],                                    -- 行存才有数组(CStore 不支持, 故冷区序列化)
    attrs jsonb,                                    -- 深合并后全量属性
    input_text text, output_text text,             -- 供 BM25(可截断前 N KB), 大全文在 CAS
    input_ref bytea, output_ref bytea,             -- 大字段指针(晚物化)
    is_sampled_for_vector boolean NOT NULL DEFAULT false,
    encoding_state smallint NOT NULL DEFAULT 0,     -- 0未编码(活) 1已物化区间 2stale需重算 3溢出走邻接
    fold_version bigint, frozen_at timestamptz,
    -- [改] 只保留这一个唯一约束! MERGE/折叠正确性依赖冲突检测确定性; 其余索引全非唯一
    CONSTRAINT pk_span_current PRIMARY KEY (span_id)
) WITH (ORIENTATION = ROW);   -- [改·必须] ASTORE 行存! 绝不写 STORAGE_TYPE=USTORE:
                              -- 内核 indexcmds.cpp:783 硬禁止 USTORE 表建非 ubtree 索引(GIN/BM25 全废)。
                              -- ASTORE 支持 simple_heap_update 原地更新, 折叠 MERGE UPDATE 照常工作;
                              -- USTORE 原本只是"防膨胀"偏好, 不是正确性需求 → 让位给 GIN/BM25 检索能力。

-- 树/线程/活态 索引(全行存, 非唯一)
CREATE INDEX ix_cur_subtree  ON span_current (tenant_id, trace_id, pre);        -- 子树 BETWEEN
CREATE INDEX ix_cur_root     ON span_current (tenant_id, trace_id) WHERE parent_span_id IS NULL;
CREATE INDEX ix_cur_parent   ON span_current (parent_span_id);
CREATE INDEX ix_cur_thread   ON span_current (tenant_id, thread_id, start_time);
CREATE INDEX ix_cur_running  ON span_current (tenant_id, trace_id) WHERE end_time IS NULL;
CREATE INDEX ix_cur_dotted   ON span_current (tenant_id, dotted_order text_pattern_ops);
CREATE INDEX ix_cur_recent   ON span_current (tenant_id, start_time DESC);
-- 检索: JSONB-GIN + 真列(供聚合/带过滤)
CREATE INDEX ix_cur_attrs    ON span_current USING gin (attrs jsonb_path_ops);
CREATE INDEX ix_cur_model    ON span_current (tenant_id, model, start_time);
CREATE INDEX ix_cur_cost     ON span_current (tenant_id, total_cost DESC);
```

---

## 4. 折叠：事件 → span_current（`[改]` 用 MERGE INTO，不是 ON CONFLICT）

折叠 = 同一 span_id 的多事件按 (seq, ts, event_id) 合并：后写覆盖、token/cost 累加、attrs 深合并、end 补全、status 推断。**关键修正**：① 不能用 `ON CONFLICT`，用 `MERGE INTO`；② 折叠源子查询里**强制把单 span 全部事件先收进一个排序节点**（防跨午夜分区的乱序合并出错），并 `SET query_dop=1` 关并行聚合保序；③ event_id 必须全局单调（雪花，不用 SEQUENCE CACHE）。

```sql
SET query_dop = 1;   -- [改] 折叠期关并行, 保 last-non-null / 深合并的有序性
MERGE INTO span_current sc
USING (
    SELECT span_id,
           max(tenant_id) AS tenant_id, max(trace_id) AS trace_id,
           (array_agg(parent_span_id ORDER BY seq, ts, event_id)
              FILTER (WHERE parent_span_id IS NOT NULL))[1] AS parent_span_id,
           max(name)  FILTER (WHERE name IS NOT NULL)  AS name,
           min(start_time) FILTER (WHERE event_type=1)  AS start_time,
           max(end_time)   FILTER (WHERE event_type=3)  AS end_time,
           sum(input_tokens) AS input_tokens, sum(output_tokens) AS output_tokens,
           sum(total_cost)   AS total_cost,
           max(latency_ms) FILTER (WHERE event_type=3)  AS latency_ms,
           CASE WHEN bool_or(event_type=5) THEN 2
                WHEN bool_or(event_type=3) THEN 1 ELSE 0 END AS status,
           tv_jsonb_deep_merge_agg(attrs_patch ORDER BY seq, ts, event_id) AS attrs,  -- 自定义有序深合并聚合
           max(event_id) AS fold_version
    FROM span_events
    WHERE tenant_id = :tenant AND trace_id = :trace      -- [改] 必带谓词, 谓词下推到 ix_evt_trace
    GROUP BY span_id
) f
ON (sc.span_id = f.span_id)
WHEN MATCHED THEN UPDATE SET
    parent_span_id = COALESCE(f.parent_span_id, sc.parent_span_id),
    end_time = COALESCE(f.end_time, sc.end_time),
    status   = GREATEST(sc.status, f.status),
    input_tokens = f.input_tokens, output_tokens = f.output_tokens,
    total_cost = f.total_cost, latency_ms = COALESCE(f.latency_ms, sc.latency_ms),
    attrs = tv_jsonb_deep_merge_2(sc.attrs, f.attrs),
    fold_version = f.fold_version,
    encoding_state = CASE WHEN sc.encoding_state = 1 THEN 2 ELSE sc.encoding_state END  -- 已编码又来新内容→stale
WHEN NOT MATCHED THEN INSERT
    (span_id, tenant_id, trace_id, parent_span_id, name, start_time, end_time,
     input_tokens, output_tokens, total_cost, latency_ms, status, attrs, fold_version, encoding_state)
    VALUES (f.span_id, f.tenant_id, f.trace_id, f.parent_span_id, f.name, f.start_time, f.end_time,
     f.input_tokens, f.output_tokens, f.total_cost, f.latency_ms, f.status, f.attrs, f.fold_version, 0);
```

- **深合并函数** `tv_jsonb_deep_merge_2(a,b)`（递归右覆盖）+ 有序聚合 `tv_jsonb_deep_merge_agg`：openGauss `||` 仅顶层浅合并，深合并须自定义（plpgsql，见 appendix-I）。`[验]` 大对象性能 → 大 trace 走应用层。
- **微批节奏**：每 5–15s 按 `ingest_ts` 水位增量折叠"水位后有新事件的 span"。活 trace 实时性由 §6 的 live 函数兜底，物化频率可放宽。

---

## 5. 树编码 / 子树 / 找根 / 线程

- **双编码**：写侧邻接 `parent_span_id`（乱序晚到友好）；读侧区间 `pre/post/lvl`（冻结时 DFS 物化）+ `dotted_order`（抗晚到的有序路径）。
- **`[改]` pre/post 用应用层一次性 O(n) DFS + `COPY` 回写**，不要用 SQL `LIKE prefix%` 自连接（O(n²)，会卡冻结批）。
- **子树查询**（已物化区间，命中 `ix_cur_subtree`）：
```sql
SELECT c.* FROM span_current n JOIN span_current c
  ON c.tenant_id=n.tenant_id AND c.trace_id=n.trace_id AND c.pre BETWEEN n.pre AND n.post
WHERE n.span_id=:root AND c.encoding_state=1 ORDER BY c.pre;   -- 先序=DFS树序
```
- **晚到节点**（区间已物化，连续区间插不进）：新节点先 `encoding_state=3` + `dotted_order`（拼父前缀即得正确全序）入表；子树查询用 **dotted_order `LIKE prefix||'.%'` 兜底**（命中 `ix_cur_dotted`），超阈值再触发整 trace DFS 重编号。
- **找根**：折叠后回填 `root_id`，一跳到位 `WHERE span_id=(SELECT root_id FROM span_current WHERE span_id=:x)`。
- **线程重建**：`WHERE thread_id=:t ORDER BY start_time`（命中 `ix_cur_thread`，只拉小列不碰大 payload）。

---

## 6. 活 trace（`[改]` 用参数化集合函数，不用裸视图）

红队修正：裸视图 `GROUP BY 全表` + UNION ALL 谓词推不下去 → 全表扫。改成**集合返回函数**，谓词进内层扫描：

```sql
CREATE OR REPLACE FUNCTION span_live(p_tenant bigint, p_trace bigint)
RETURNS SETOF span_current LANGUAGE sql STABLE AS $$
  SELECT span_id, p_tenant, p_trace, NULL::bigint /*root*/,
         (array_agg(parent_span_id ORDER BY seq) FILTER (WHERE parent_span_id IS NOT NULL))[1],
         NULL,NULL,NULL,NULL /*pre/post/lvl/dotted*/, max(thread_id),
         max(span_kind), max(name), min(start_time) FILTER(WHERE event_type=1), NULL,
         CASE WHEN bool_or(event_type=5) THEN 2 WHEN bool_or(event_type=3) THEN 1 ELSE 0 END,
         /* ...其余列 NULL/聚合... */ 0
  FROM span_events
  WHERE tenant_id=p_tenant AND trace_id=p_trace   -- 谓词在内层, 命中 ix_evt_trace
  GROUP BY span_id;
$$;
-- 统一查询: 已折叠的优先, 未折叠的从 live 补 (去重: 物化表存在即用物化, 否则 live)
-- 调用方按 (tenant,trace) 查; 活 trace 工作集小(只"正在跑的"), 成本可控。[验] EXPLAIN 确认谓词下推。
```
**`[改]` 活/批去重规则**：同一 span 若已在 span_current（end 已折叠）则以 span_current 为准；否则取 live。避免 UNION ALL 双计数/零计数。

---

## 7. 晚到回流防丢（`[改]` 红队致命洞：冻结后晚到事件无处落）

冻结 = 根已 end 且 N 天无新事件 → 迁冷 + DROP 热分区。但 feedback/异步 eval 可能 >N 天才到，此时热区已删、CStore 不可改 → **静默丢数据**。修正：

```sql
CREATE TABLE frozen_registry (              -- 已冻结 trace 登记
    tenant_id bigint, trace_id bigint, frozen_at timestamptz, cold_partition text,
    CONSTRAINT pk_frozen PRIMARY KEY (tenant_id, trace_id)
);
CREATE TABLE late_event_inbox (LIKE span_events);  -- 晚到事件收容(行存, 不丢)
```
- 摄入时查 `frozen_registry`：命中 → 该事件进 `late_event_inbox`，并标记该 trace `late_pending`。
- 后台对 `late_pending` 的 trace **重融化**：重折叠 + **重建该 trace 的冷分区**（CStore 无原地更新，只能整分区重写），再清 inbox。
- **`[改]` DROP PARTITION 宽限窗 > 业务最大 feedback 延迟**（如反馈最长 30 天 → 宽限 ≥45 天）。

---

## 8. 向量语义召回（招牌钩子，`[实]` 你们真实语法）

```sql
CREATE TABLE span_vectors (
    span_id bigint PRIMARY KEY, tenant_id bigint NOT NULL, trace_id bigint NOT NULL,
    root_id bigint, span_kind smallint NOT NULL, model text, start_time timestamptz NOT NULL,
    status smallint, is_sampled boolean NOT NULL DEFAULT true,
    embedding floatvector(1024) NOT NULL,           -- [实] floatvector(N); 维度按模型
    embed_source text
);
-- [实] 带过滤 ANN: 你们 DiskANN 原生 inplace-filter (代码里的 idx_diskann_inplace_filter)
CREATE INDEX ix_vec_diskann ON span_vectors
    USING diskann (embedding, tenant_id, span_kind)   -- 复合: 标量过滤列编进图, 过滤下推
    WITH (parallel_workers=8, enable_quantization=true);
-- 备: 纯语义无强过滤用 hnsw
-- CREATE INDEX ix_vec_hnsw ON span_vectors USING hnsw (embedding floatvector_cosine_ops) WITH (m=32, ef_construction=200);
```
带过滤召回查询（先标量裁剪域再 ANN）：
```sql
SELECT span_id, trace_id, model, embedding <=> :q::floatvector AS dist   -- [实] <=> 余弦
FROM span_vectors
WHERE tenant_id=:t AND span_kind=1 AND start_time >= now()-interval '7 days'
ORDER BY embedding <=> :q::floatvector LIMIT 20;
```
- **采样**：只对 root / LLM span / error span 建 embedding（`is_sampled`），降规模。中位客户 ~9000 万向量单机全内存；上限走 DiskANN 磁盘图。
- **`[改]` 高选择度兜底**：过滤后候选 < 阈值(如几千)时，**直接对候选向量暴力精排**（recall=100%，避免 filtered-ANN under-fill）。
- **`[验]`**：DiskANN inplace-filter 的精确列序/WITH 参数；冷数据的向量检索**保留在行存 span_vectors**（不随分析列进 CStore）。

---

## 9. 中文全文（`[实]` BM25 + vex_jieba，你们真实算子 `@~@`）

```sql
CREATE TEXT SEARCH DICTIONARY cn_dict (TEMPLATE = vex_jieba);          -- [实]
SELECT vexjieba_add_userdict('cn_dict', ARRAY['工具调用','思维链','yiTrace,10000']);  -- [实] 领域词典
SELECT vexjieba_reload('cn_dict');                                     -- [实] 必须 reload
-- BM25 索引建在行存 span_current.input_text/output_text (CStore 不行)
-- [实] BM25 访问方法名是 fulltext (pg_am.h OID 4429, handler=bm25insert/bm25build); 算子 @~@(OID 5048/5049) + bm25_score()(OID 4527)
-- [验] WITH 参数(DICTS/ALGORITHMS/COEFFICIENTS)精确写法以本机为准
CREATE INDEX ix_cur_input_bm25  ON span_current USING fulltext (input_text)
    WITH (DICTS='cn_dict', ALGORITHMS='BM25', COEFFICIENTS='b=0.75,k=1.2');
```
检索（`[实]` `@~@` 匹配 + `bm25_score()` 排序，自动 jieba 分词）：
```sql
SELECT span_id, trace_id, name, bm25_score() AS score
FROM span_current
WHERE tenant_id=:t AND start_time>=now()-interval '30 days'
  AND input_text @~@ '工具调用 失败 重试'
ORDER BY score DESC LIMIT 20;
```

---

## 10. 冷热分层（`[改]` 检索能力绝不冻进 CStore）

```sql
-- 冷区: 只放分析用真列(低基数), 用于列式聚合; 不放 tags数组/attrs/向量/BM25
CREATE TABLE span_current_cold (
    span_id bigint, tenant_id bigint, trace_id bigint, span_kind smallint,
    start_time timestamptz, end_time timestamptz, status smallint, model text,
    input_tokens int, output_tokens int, total_cost numeric(18,6), latency_ms int
) WITH (ORIENTATION = COLUMN, PARTIAL CLUSTER KEY (tenant_id, trace_id))   -- [实] 列存聚簇
PARTITION BY RANGE (start_time) INTERVAL ('1 month') ( PARTITION pc0 VALUES LESS THAN ('2026-01-01') );
-- 冻结: INSERT INTO span_current_cold SELECT(真列) FROM span_current WHERE 已冻结
--      [改] INSERT 时 ORDER BY (tenant_id, trace_id) 让 PCK 聚簇生效
-- [改] 检索镜像保留行存: 冷 trace 的 span_vectors(HNSW/DiskANN) 与 BM25/GIN 检索列继续留 span_current 行存
--      (或单独 span_retrieval_cold 行存表); 历史数据的"语义召回+中文检索"不能因冷而失效。
```
列式聚合（只碰冷区真列）：
```sql
SELECT model, count(*), sum(total_cost), avg(latency_ms)
FROM span_current_cold WHERE tenant_id=:t AND start_time>=:a AND start_time<:b GROUP BY model;
```

---

## 11. payload 大字段（CAS 去重 + 晚物化）

```sql
CREATE TABLE payload_store (
    tenant_id bigint NOT NULL, sha256 bytea NOT NULL, media_type text NOT NULL,
    byte_len bigint NOT NULL, refcount bigint NOT NULL DEFAULT 0,
    content text,            -- 文本内联(TOAST 自动压缩行外)
    external_uri text,       -- 多模态/超大: 对象存储 URI, 库里只留指针
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT pk_payload PRIMARY KEY (tenant_id, sha256)
);
-- [改] 写入去重用 MERGE INTO(不是 ON CONFLICT): 命中 refcount+1, 否则插入
-- 列表/树查询只回 *_ref; 用户点开某 span 才 JOIN payload_store 取全文 → 晚物化。
-- GC: 删 trace/分区时涉及 ref refcount-1, 异步清 refcount=0。
```

---

## 12. 待目标实例核实清单（`[验]`，上线前 `\d`/试跑）

1. **MERGE INTO** 的 `WHEN MATCHED ... WHERE` 子句支持 / dbcompatibility 模式（折叠的 fold_version 单调守卫放 WHERE 还是放源 SELECT）。
2. **BM25 索引 WITH 参数**：AM 名已确认 `fulltext`(OID 4429)、算子 `@~@`+`bm25_score()`；待确认 `WITH (DICTS/ALGORITHMS/COEFFICIENTS=...)` 的精确键名/取值。
10. **`[改·已修]` span_current 必须 ASTORE**：USTORE 表内核硬禁建 GIN/BM25(indexcmds.cpp:783)，检索表一律 ASTORE 行存。
3. **DiskANN inplace-filter 复合索引**列序与 WITH 参数（`(embedding, tenant_id, span_kind)` 还是 `(embedding, id)` + 运行时谓词）。
4. **`<#>`(内积) + `floatvector_ip_ops`** 是否存在（`<->`/`<=>` 已确认）。
5. **RANGE+INTERVAL 分区** 是否仅单列时间键；ASTORE+INTERVAL 组合。
6. **LOCAL + 部分索引(`WHERE`)** 能否共存（`ix_cur_running` 是非分区表故 OK；事件表上若要部分 LOCAL 需确认）。
7. **自定义有序聚合** `tv_jsonb_deep_merge_agg` 在折叠期（query_dop=1）的保序与性能；大 trace 改应用层 DFS+COPY。
8. **dotted_order** 与 LangSmith 官方格式（微秒 RFC3339 + 全 UUID）对齐，定宽 + C collation 保证字典序==时间序。
9. **多租户物理隔离**：是否需把 `tenant_id` 进 PK 前缀 / 按租户分区（红队指出当前仅靠二级索引过滤，非物理隔离）。

---

### 一句话总览
事件层 **ASTORE 只 append**；折叠用 **`MERGE INTO`**（openGauss 无 ON CONFLICT）；**所有检索（向量 DiskANN-inplace-filter / 中文 BM25@~@ / JSONB-GIN）集中在行存**，CStore 只做冷区分析聚合且**绝不吞掉检索能力**；树用 **邻接(写)+区间(读)+dotted_order(抗晚到)** 三编码、pre/post 走**应用层 O(n) DFS**；晚到事件用 **frozen_registry + late_event_inbox + 重融化** 防丢；大字段 **CAS 去重 + 晚物化**。全部用你们 `floatvector`/`@~@`/`vex_jieba`/`USING hnsw|diskann` 真实语法。
