# 附录 I：第四轮 schema 设计原文（核心/树编码/索引 三份）+ 红队

## I1 核心：事件表+折叠+冷热分层

Now I have all the confirmations I need:
- `DISTINCT ON` is supported (confirmed in SELECT syntax).
- `jsonb_set` is supported.
- `->`, `->>`, `jsonb_object_field` supported.
- JSONB `||` operator is NOT confirmed in the docs (only string `||` appears) → I'll mark it TODO and provide a PL/pgSQL deep-merge fallback that doesn't depend on it.
- RANGE+INTERVAL partition, CStore RANGE-only, ORIENTATION immutable, BM25 `USING fulltext`, HNSW — all confirmed.

I have everything needed. Here is the complete design.

---

# yiTrace: span_events + span_current 表结构与折叠流水线设计 (openGauss/yiTrace)

## 0. 已核实语法 / 不确定项 (先声明,避免编造)

已通过官方文档核实 (来源见末尾):
- **RANGE + INTERVAL 自动分区** 语法可用,自动分区命名 `sys_p1, sys_p2...`(行存表)。
- **CStore 列存只支持 RANGE 分区,不支持 INTERVAL/HASH/LIST**;GIN 索引仅对 `tsvector` 类型;只追加写。
- **`ORIENTATION` 不可被 ALTER TABLE 修改**(行存↔列存无法原地转换)→ 这是本设计最关键的约束,决定了冷热分层必须是**两张物理表**,而非"同表分区切引擎"。
- `DISTINCT ON (...)`、窗口函数(`first_value/last_value/row_number`)、`jsonb_set`、`->`/`->>`/`jsonb_object_field` 均可用。
- BM25:`CREATE INDEX ... USING fulltext(col) WITH (DICTS=..., ALGORITHMS='BM25', COEFFICIENTS='b=0.75:k=1.2')`,查询算子 `@~@`(打分)/`@-@`(布尔),配 `bm25_score()`。
- HNSW:`USING hnsw (col vector_l2_ops) WITH (m=..., ef_construction=...)`,距离算子 pgvector 风格 `<->`(L2)/`<=>`(cosine)。

**标 TODO 待实测的项:**
- `[TODO-1]` JSONB 的 `||` 拼接运算符在 yiTrace/openGauss 是否原生支持未在文档确认(文档只见字符串 `||`)。本设计的 attrs 深合并**不依赖** `||`,改用 `jsonb_set` 循环 + 自定义 PL/pgSQL 聚合,确保可落地;若实测 `||` 可用,浅合并路径可简化。
- `[TODO-2]` 自定义聚合 `jsonb_deep_merge`(有序合并)能否在列存/向量化执行器下走;若不行,折叠 SQL 退化为行存上跑(本就如此,折叠只读热区行存)。
- `[TODO-3]` BM25/HNSW 索引能否建在**分区表**上(GLOBAL vs LOCAL)。文档示例多为非分区表;`span_current` 设计为**非分区物化表**正是为规避此不确定性。
- `[TODO-4]` 冷区 CStore 表能否建 HNSW 向量索引未确认;故向量统一放独立的 `span_vectors`(行存),不依赖冷区。

---

## 1. span_events — append-only 事件表

### 设计要点
- **近端(热)用 UStore 行存**承接高频写:UStore 原地更新+undo 对高并发 INSERT 友好,且我们这里**只 INSERT 不 UPDATE**,UStore 的回滚段压力极低(无更新链)。
- **RANGE + INTERVAL 按 ts 自动分区**:无需手工建分区,数据落到未来时间自动生 `sys_pN`。
- **为何只 INSERT**:span "早上生/下午死/乱序/晚到",若对一行 span 做 in-place UPDATE(start→end→patch),在行存上会产生**热行更新链 / 死元组膨胀 / 写放大**,且并发写同一 span_id 需行锁;事件化后每个生命周期信号是一条独立不可变行,写入无锁竞争、天然支持乱序与晚到(后到的 end 事件只是 seq/ts 更大的一行),也保留完整审计轨迹。折叠在读侧做。

```sql
-- ============================================================
-- span_events: append-only 事件表 (热区, UStore 行存, ts 自动分区)
-- ============================================================
CREATE TABLE span_events (
    event_id        bigint        NOT NULL,          -- 全局序列, 见下方 SEQUENCE
    tenant_id       int           NOT NULL,
    trace_id        bytea         NOT NULL,          -- 16B uuid 原始字节, 比 text 省空间
    span_id         bytea         NOT NULL,          -- 8B span id
    seq             int           NOT NULL DEFAULT 0,-- 单 span 内事件序 (生产者自增, 兜底乱序)
    event_type      smallint      NOT NULL,          -- 1=start 2=update 3=end 4=tool_result 5=error 6=feedback
    ts              timestamptz   NOT NULL,          -- 事件发生时间 (分区键)

    parent_span_id  bytea,                           -- 写侧邻接, 可空(根)/晚到
    root_id         bytea,                           -- 写侧若已知则带上, 未知留空, 折叠期补
    thread_id       bytea,                           -- 会话/线程线

    -- 热点小列(增量值): 仅该事件携带的字段, 其余 NULL, 折叠期 last-non-null
    span_kind       smallint,                        -- llm/tool/chain/retriever...
    name            text,
    model           text,
    input_tokens    int,
    output_tokens   int,
    total_cost      numeric(18,6),
    latency_ms      int,
    status_delta    smallint,                        -- 该事件声明的状态 (end/error 事件填)

    attrs_patch     jsonb,                           -- 增量属性, 折叠期深合并
    tags            text[],                          -- 该事件追加的标签(折叠期 array union)

    payload_ref     bytea,                           -- 大字段指针(指向 payload_store / TOAST), 晚物化
    is_sampled_for_vector boolean   DEFAULT false,   -- 该 span 是否被采样做 embedding

    ingest_ts       timestamptz   NOT NULL DEFAULT now()  -- 入库时间, 用于晚到检测/折叠水位
)
WITH (ORIENTATION = row, STORAGE_TYPE = USTORE, FILLFACTOR = 100)  -- 行存+UStore; 只插不更新, fillfactor 拉满
PARTITION BY RANGE (ts)
INTERVAL ('1 day') (                                   -- 自动按天建分区, sys_pN
    PARTITION p_init VALUES LESS THAN ('2026-06-01 00:00:00+08')
);

-- 全局自增 event_id (避免多写入端冲突; 也可用 snowflake 由应用生成)
CREATE SEQUENCE span_events_eid_seq CACHE 1000;
ALTER TABLE span_events ALTER COLUMN event_id SET DEFAULT nextval('span_events_eid_seq');
```

```sql
-- ---- span_events 索引 (建在行存热区, 服务折叠 & 活 trace 查询) ----
-- 折叠主路径: 按 (tenant, span) 取该 span 所有事件并排序
CREATE INDEX idx_se_span    ON span_events (tenant_id, span_id, seq, ts);
-- 活 trace / 树查询: 按 trace 拉全部事件
CREATE INDEX idx_se_trace   ON span_events (tenant_id, trace_id, ts);
-- 折叠水位推进: 按入库时间扫增量
CREATE INDEX idx_se_ingest  ON span_events (ingest_ts);
-- 邻接关系重建(找子)
CREATE INDEX idx_se_parent  ON span_events (tenant_id, parent_span_id) WHERE parent_span_id IS NOT NULL;
```

> 关于"近端 UStore / 老分区 CStore":**openGauss 无法对单表的不同分区设置不同 ORIENTATION,且 ORIENTATION 不可 ALTER**(已核实)。所以**不能**"同表老分区转列存"。替代方案 = **冷热双表**:`span_events`(行存,热)+ `span_events_cold`(CStore 列存,冷,见 §4),冻结 = 跨表批量 `INSERT...SELECT` + DROP 老分区。

---

## 2. span_current — 折叠态:**物化表 + 活 trace 视图,混合方案**

### 论证(为什么混合,而非纯视图或纯物化表)

| 方案 | 优点 | 致命缺点 (本场景) |
|---|---|---|
| 纯查询期视图(每次实时折叠) | 永远最新,无刷新逻辑 | 每次树查询/子树查询/聚合都要对该 trace 的**全部事件**做窗口聚合;`<1亿 span/天`下,一个大 trace 几万 span × 多事件,反复折叠太贵;且**向量 ANN 过滤、BM25 检索无法建在视图上**(索引必须落在物理列上) |
| 纯物化表(后台定时全量折叠) | 查询快,可建索引(向量/BM25/树区间编码) | **活 trace 看不到最新**:运行中的 span 还没刷进来;刷新延迟 = 看不到 running 态 |

**结论:混合。**
- **已结束/已冻结的 span → 物化进 `span_current`**(物理表,承载 pre/post/lvl 区间编码、dotted_order、向量采样标记、可建索引)。这是 99% 查询的命中面。
- **活 trace(只有 start 无 end)→ 不进物化表,用视图 `span_current_live` 实时从热区事件折叠**。活 trace 数据量小(只是"正在跑的"那几条 trace),实时折叠成本可控。
- 对外暴露 **`span_current_all` 视图** = `span_current`(冷,物化) `UNION ALL` `span_current_live`(热,实时),业务统一查它。

**中小规模 query-time fold 够快吗?** 对**单个活 trace**(几十~几千 span)够快,这正是 live 视图覆盖的范围。对**全库聚合/历史检索**不够 → 走物化表。**折叠刷新频率**:微批,建议 **每 5~15s 增量折叠**(按 `ingest_ts` 水位推进,只折叠水位之后出现过新事件的 span),活 trace 的实时性由视图兜底,所以物化刷新频率可以放宽,不必追实时。

```sql
-- ============================================================
-- span_current: 折叠后的 span 当前态 (物化表, 行存 UStore, 非分区)
--   只放"已可稳定折叠"的 span(已 end, 或长时间无新事件的活 span 快照)
--   非分区: 规避 [TODO-3] 分区表上建 BM25/向量索引的不确定性, 中小规模单表可承受
-- ============================================================
CREATE TABLE span_current (
    span_id         bytea         PRIMARY KEY,
    tenant_id       int           NOT NULL,
    trace_id        bytea         NOT NULL,
    root_id         bytea,
    parent_span_id  bytea,

    -- 区间编码(冻结/折叠完成时物化, 用于 O(1) 子树查询: 子 ∈ [pre,post])
    pre             bigint,
    post            bigint,
    lvl             int,
    dotted_order    text,                            -- LangSmith 风格有序路径, 便于排序/前缀匹配

    thread_id       bytea,
    span_kind       smallint,
    name            text,
    start_time      timestamptz,
    end_time        timestamptz,                     -- NULL = 活/未结束
    status          smallint      NOT NULL DEFAULT 0,-- 0=running 1=ok 2=error

    input_tokens    int,
    output_tokens   int,
    total_cost      numeric(18,6),
    latency_ms      int,
    model           text,
    tags            text[],

    attrs           jsonb,                           -- 深合并后的全量属性
    input_ref       bytea,                           -- 大字段指针 (晚物化)
    output_ref      bytea,
    is_sampled_for_vector boolean   DEFAULT false,

    fold_version    bigint,                          -- = 折叠时所见最大 event_id, 幂等/增量用
    is_frozen       boolean       NOT NULL DEFAULT false  -- 已迁冷区则 true
)
WITH (ORIENTATION = row, STORAGE_TYPE = USTORE);     -- 物化表会被 UPDATE 折叠 -> UStore 原地更新正合适

-- 树/线程/检索索引
CREATE INDEX idx_sc_trace   ON span_current (tenant_id, trace_id);
CREATE INDEX idx_sc_subtree ON span_current (tenant_id, root_id, pre, post);   -- 子树: WHERE pre BETWEEN ... 
CREATE INDEX idx_sc_parent  ON span_current (tenant_id, parent_span_id);
CREATE INDEX idx_sc_thread  ON span_current (tenant_id, thread_id, start_time);
CREATE INDEX idx_sc_dotted  ON span_current (tenant_id, dotted_order text_pattern_ops); -- 前缀匹配子树
CREATE INDEX idx_sc_attrs   ON span_current USING gin (attrs);                 -- JSONB 过滤
-- 中文全文 (BM25): 对 name 建; 大字段全文走 payload 折叠后另建
CREATE INDEX idx_sc_name_bm25 ON span_current USING fulltext (name)
    WITH (DICTS='cn_tokenizer', ALGORITHMS='BM25', COEFFICIENTS='b=0.75:k=1.2');
```

> `span_current` 用 **UStore 行存**:它本身就是要被折叠流水线反复 `UPSERT/UPDATE` 的"当前态",原地更新 + undo 正是 UStore 的主场,与 §1 的"事件表只插不更"互补。

---

## 3. 折叠 SQL(事件 → 一行当前态)

折叠规则:同 `span_id` 多事件按 `(seq, ts, event_id)` 排序后:
- 小列:**last-non-null**(后写覆盖,但不被 NULL 覆盖)→ 用 `last_value(... IGNORE NULLS)` 或 `DISTINCT ON + COALESCE` 链。
- `attrs_patch`:**有序深合并**(后到的 patch 覆盖同 key,object 递归合并)。
- `end`/`error` 事件:补 `end_time`、`status`。
- `tags`:array union。

### 3a. 深合并函数(不依赖未确认的 jsonb `||`,用 `jsonb_set` 递归)

```sql
-- 两个 jsonb 的递归深合并: b 覆盖 a, object 递归, 其余整体覆盖
-- [TODO-1] 若实测 yiTrace 支持 jsonb 的 || , 顶层浅合并可直接用 a||b 简化
CREATE OR REPLACE FUNCTION jsonb_deep_merge(a jsonb, b jsonb)
RETURNS jsonb LANGUAGE plpgsql IMMUTABLE AS $$
DECLARE
    k    text;
    v    jsonb;
    res  jsonb := COALESCE(a, '{}'::jsonb);
BEGIN
    IF b IS NULL THEN RETURN res; END IF;
    IF jsonb_typeof(res) <> 'object' OR jsonb_typeof(b) <> 'object' THEN
        RETURN b;                                   -- 非 object: b 整体覆盖
    END IF;
    FOR k, v IN SELECT key, value FROM jsonb_each(b) LOOP
        IF res ? k AND jsonb_typeof(res->k)='object' AND jsonb_typeof(v)='object' THEN
            res := jsonb_set(res, ARRAY[k], jsonb_deep_merge(res->k, v), true);
        ELSE
            res := jsonb_set(res, ARRAY[k], v, true);
        END IF;
    END LOOP;
    RETURN res;
END $$;

-- 有序聚合: 按输入顺序把多个 patch 依次深合并 (折叠 attrs 的核心)
-- [TODO-2] 实测该自定义聚合在向量化执行器下的行为; 折叠只跑在行存热区, 不涉列存
CREATE AGGREGATE jsonb_merge_agg(jsonb) (
    SFUNC  = jsonb_deep_merge,
    STYPE  = jsonb,
    INITCOND = '{}'
);
```

### 3b. 单个 span_id 的折叠(可读版)

```sql
-- 把 :tid / :sid 一个 span 的所有事件折叠成一行
WITH ev AS (
    SELECT *
    FROM span_events
    WHERE tenant_id = :tid AND span_id = :sid
    ORDER BY seq, ts, event_id            -- 折叠定序: seq 优先, 兜底 ts, 再兜底入库序
)
SELECT
    :sid                                                       AS span_id,
    :tid                                                       AS tenant_id,
    max(trace_id)        FILTER (WHERE trace_id IS NOT NULL)   AS trace_id,
    -- last-non-null: 用 DISTINCT 取按定序的最后一个非空
    (array_agg(parent_span_id) FILTER (WHERE parent_span_id IS NOT NULL))[count(parent_span_id)] AS parent_span_id,
    (array_agg(name)       FILTER (WHERE name  IS NOT NULL))[count(name)]      AS name,
    (array_agg(model)      FILTER (WHERE model IS NOT NULL))[count(model)]     AS model,
    (array_agg(span_kind)  FILTER (WHERE span_kind IS NOT NULL))[count(span_kind)] AS span_kind,
    (array_agg(thread_id)  FILTER (WHERE thread_id IS NOT NULL))[count(thread_id)] AS thread_id,
    -- start: 第一条 start 事件的 ts
    min(ts) FILTER (WHERE event_type = 1)                      AS start_time,
    -- end: end/error 事件的 ts (取最后一个)
    max(ts) FILTER (WHERE event_type IN (3,5))                 AS end_time,
    -- status 推断: 有 error -> 2; 有 end -> 1; 否则 0(running)
    CASE
        WHEN bool_or(event_type = 5) THEN 2
        WHEN bool_or(event_type = 3) THEN 1
        ELSE 0
    END                                                        AS status,
    -- 计量: tokens/cost 取最后声明值(若生产者发的是累计值); 若是增量值改成 sum(...)
    (array_agg(input_tokens)  FILTER (WHERE input_tokens  IS NOT NULL))[count(input_tokens)]  AS input_tokens,
    (array_agg(output_tokens) FILTER (WHERE output_tokens IS NOT NULL))[count(output_tokens)] AS output_tokens,
    (array_agg(total_cost)    FILTER (WHERE total_cost    IS NOT NULL))[count(total_cost)]    AS total_cost,
    (array_agg(latency_ms)    FILTER (WHERE latency_ms    IS NOT NULL))[count(latency_ms)]    AS latency_ms,
    -- attrs 有序深合并
    jsonb_merge_agg(attrs_patch) FILTER (WHERE attrs_patch IS NOT NULL)        AS attrs,
    -- tags union
    (SELECT array_agg(DISTINCT t) FROM unnest(array_agg(tags)) AS x(arr), unnest(x.arr) AS t) AS tags,
    -- 大字段指针: 取最后一个非空(晚物化)
    (array_agg(payload_ref) FILTER (WHERE payload_ref IS NOT NULL AND event_type IN (1)))[1]  AS input_ref,
    (array_agg(payload_ref) FILTER (WHERE payload_ref IS NOT NULL AND event_type IN (3,4)))[1] AS output_ref,
    bool_or(is_sampled_for_vector)                             AS is_sampled_for_vector,
    max(event_id)                                              AS fold_version
FROM ev;
```

> 说明:`(array_agg(x) FILTER (...))[count(x)]` 是"按定序取最后一个非空"的可移植写法(`array_agg` 在 `ORDER BY` 的 CTE 下保序)。若 yiTrace 支持 `last_value(x) IGNORE NULLS OVER (...)` 则更直观,但 IGNORE NULLS 在 openGauss 未普遍核实 → 此写法不依赖它。

### 3c. 增量批量折叠(流水线实际用:只折叠近期有新事件的 span)

```sql
-- 每 5~15s 跑一次: 找出上次水位后有新事件的 (tenant, span), 折叠后 UPSERT 进 span_current
-- 用 MERGE (openGauss 支持) 做幂等 upsert; 用 fold_version 防回退
INSERT INTO span_current AS sc (
    span_id, tenant_id, trace_id, parent_span_id, thread_id, span_kind, name, model,
    start_time, end_time, status, input_tokens, output_tokens, total_cost, latency_ms,
    attrs, tags, input_ref, output_ref, is_sampled_for_vector, fold_version
)
SELECT g.* FROM (
    -- 上面 3b 的折叠逻辑, 包成 set-based: GROUP BY span_id, 仅限脏 span
    SELECT
        e.span_id, e.tenant_id,
        max(e.trace_id) FILTER (WHERE e.trace_id IS NOT NULL),
        (array_agg(e.parent_span_id ORDER BY e.seq,e.ts,e.event_id) FILTER (WHERE e.parent_span_id IS NOT NULL))[count(e.parent_span_id)],
        (array_agg(e.thread_id ORDER BY e.seq,e.ts,e.event_id) FILTER (WHERE e.thread_id IS NOT NULL))[count(e.thread_id)],
        (array_agg(e.span_kind ORDER BY e.seq,e.ts,e.event_id) FILTER (WHERE e.span_kind IS NOT NULL))[count(e.span_kind)],
        (array_agg(e.name ORDER BY e.seq,e.ts,e.event_id) FILTER (WHERE e.name IS NOT NULL))[count(e.name)],
        (array_agg(e.model ORDER BY e.seq,e.ts,e.event_id) FILTER (WHERE e.model IS NOT NULL))[count(e.model)],
        min(e.ts) FILTER (WHERE e.event_type=1),
        max(e.ts) FILTER (WHERE e.event_type IN (3,5)),
        CASE WHEN bool_or(e.event_type=5) THEN 2 WHEN bool_or(e.event_type=3) THEN 1 ELSE 0 END,
        (array_agg(e.input_tokens ORDER BY e.seq,e.ts,e.event_id) FILTER (WHERE e.input_tokens IS NOT NULL))[count(e.input_tokens)],
        (array_agg(e.output_tokens ORDER BY e.seq,e.ts,e.event_id) FILTER (WHERE e.output_tokens IS NOT NULL))[count(e.output_tokens)],
        (array_agg(e.total_cost ORDER BY e.seq,e.ts,e.event_id) FILTER (WHERE e.total_cost IS NOT NULL))[count(e.total_cost)],
        (array_agg(e.latency_ms ORDER BY e.seq,e.ts,e.event_id) FILTER (WHERE e.latency_ms IS NOT NULL))[count(e.latency_ms)],
        jsonb_merge_agg(e.attrs_patch ORDER BY e.seq,e.ts,e.event_id) FILTER (WHERE e.attrs_patch IS NOT NULL),
        (SELECT array_agg(DISTINCT t) FROM unnest(array_agg(e.tags)) x(a), unnest(x.a) t),
        (array_agg(e.payload_ref ORDER BY e.seq,e.ts,e.event_id) FILTER (WHERE e.payload_ref IS NOT NULL AND e.event_type=1))[1],
        (array_agg(e.payload_ref ORDER BY e.seq,e.ts,e.event_id) FILTER (WHERE e.payload_ref IS NOT NULL AND e.event_type IN (3,4)))[1],
        bool_or(e.is_sampled_for_vector),
        max(e.event_id)
    FROM span_events e
    WHERE e.tenant_id = :tid
      AND (e.tenant_id, e.span_id) IN (             -- 仅水位后变脏的 span
          SELECT DISTINCT tenant_id, span_id FROM span_events
          WHERE ingest_ts > :last_watermark AND tenant_id = :tid
      )
    GROUP BY e.span_id, e.tenant_id
) g(span_id,tenant_id,trace_id,parent_span_id,thread_id,span_kind,name,model,
    start_time,end_time,status,input_tokens,output_tokens,total_cost,latency_ms,
    attrs,tags,input_ref,output_ref,is_sampled_for_vector,fold_version)
ON CONFLICT (span_id) DO UPDATE SET
    trace_id = EXCLUDED.trace_id,
    parent_span_id = COALESCE(EXCLUDED.parent_span_id, sc.parent_span_id),
    thread_id = COALESCE(EXCLUDED.thread_id, sc.thread_id),
    span_kind = COALESCE(EXCLUDED.span_kind, sc.span_kind),
    name = COALESCE(EXCLUDED.name, sc.name),
    model = COALESCE(EXCLUDED.model, sc.model),
    start_time = COALESCE(EXCLUDED.start_time, sc.start_time),
    end_time = COALESCE(EXCLUDED.end_time, sc.end_time),
    status = GREATEST(EXCLUDED.status, sc.status),       -- error(2)>ok(1)>running(0) 单调
    input_tokens = COALESCE(EXCLUDED.input_tokens, sc.input_tokens),
    output_tokens = COALESCE(EXCLUDED.output_tokens, sc.output_tokens),
    total_cost = COALESCE(EXCLUDED.total_cost, sc.total_cost),
    latency_ms = COALESCE(EXCLUDED.latency_ms, sc.latency_ms),
    attrs = jsonb_deep_merge(sc.attrs, EXCLUDED.attrs),  -- 与已折叠态再合并(防漏批)
    tags = EXCLUDED.tags,
    input_ref = COALESCE(EXCLUDED.input_ref, sc.input_ref),
    output_ref = COALESCE(EXCLUDED.output_ref, sc.output_ref),
    is_sampled_for_vector = sc.is_sampled_for_vector OR EXCLUDED.is_sampled_for_vector,
    fold_version = EXCLUDED.fold_version
WHERE EXCLUDED.fold_version > sc.fold_version;          -- 幂等: 旧批不覆盖新态
```

> `pre/post/lvl/dotted_order` **不在每次微批里算**(树未稳定),在 §4 trace 冻结时一次性物化(见下)。`[TODO]` openGauss `ON CONFLICT DO UPDATE` 与 `MERGE` 均可,二选一按版本实测。

---

## 4. 冷热分层与冻结(行存热区 → CStore 冷区)

由于 **ORIENTATION 不可 ALTER、CStore 不支持 INTERVAL 分区**,冻结 = **跨表批量 INSERT 到独立 CStore 表** + 删热区老分区。

### 4a. 冷区表(CStore,RANGE 分区,聚合查询)

```sql
-- 冷区: 列存, 仅追加, RANGE 按月分区(CStore 不支持 INTERVAL, 故预建/脚本补建分区)
-- 列数 < 1000 OK; 不放数组(CStore 不支持数组) -> tags 转 jsonb 或单独表
CREATE TABLE span_current_cold (
    span_id         bytea         NOT NULL,
    tenant_id       int           NOT NULL,
    trace_id        bytea         NOT NULL,
    root_id         bytea,
    parent_span_id  bytea,
    pre             bigint,
    post            bigint,
    lvl             int,
    dotted_order    text,
    thread_id       bytea,
    span_kind       smallint,
    name            text,
    start_time      timestamptz,
    end_time        timestamptz,
    status          smallint      NOT NULL,
    input_tokens    int,
    output_tokens   int,
    total_cost      numeric(18,6),
    latency_ms      int,
    model           text,
    tags_json       jsonb,                            -- CStore 不支持数组 -> 存 jsonb
    attrs           jsonb,
    input_ref       bytea,
    output_ref      bytea,
    is_sampled_for_vector boolean
)
WITH (ORIENTATION = column, COMPRESSION = high)        -- 列存 + 高压缩, 聚合/扫描友好
PARTITION BY RANGE (start_time) (
    PARTITION pc_2026_05 VALUES LESS THAN ('2026-06-01'),
    PARTITION pc_2026_06 VALUES LESS THAN ('2026-07-01')
    -- 后续分区由冻结脚本 ALTER TABLE ... ADD PARTITION 预建
);
-- 列存仅支持 psort/btree/tsvector-GIN; 聚合查询主要靠列存本身扫描 + PCK
-- [可选] PARTIAL CLUSTER KEY 提升按 trace 聚簇扫描
-- ALTER TABLE span_current_cold ADD PARTIAL CLUSTER KEY (tenant_id, trace_id);
```

### 4b. 冻结时机与流程

**冻结时机(trace 级,而非 span 级):**
- trace 的 **root span 已 end** 且 **整树 N 天(如 7 天)无新事件**(`max(ingest_ts) < now()-7d`)→ trace 已死,可冻结。
- 活 trace(root 无 end,或近 7 天仍有事件)**绝不冻结**,留在热区供实时折叠。

**流程(每日离线批):**
```sql
-- step1: 选出可冻结的 trace
CREATE TEMP TABLE froze_traces AS
SELECT tenant_id, trace_id
FROM span_current sc
WHERE status <> 0                                  -- 非 running
GROUP BY tenant_id, trace_id
HAVING bool_and(end_time IS NOT NULL)              -- 整树都已结束
   AND max(end_time) < now() - interval '7 days'
   AND NOT EXISTS (                                -- 热区近 7 天无新事件
       SELECT 1 FROM span_events e
       WHERE e.tenant_id = sc.tenant_id AND e.trace_id = sc.trace_id
         AND e.ingest_ts > now() - interval '7 days');

-- step2: 物化区间编码(pre/post/lvl/dotted_order) —— 树已稳定, 此刻算一次
--   用 CONNECT BY (openGauss 支持) 或递归 CTE 做 DFS 编号; 这里给递归 CTE 版
WITH RECURSIVE dfs AS (
    SELECT span_id, parent_span_id, trace_id, tenant_id, 0 AS lvl,
           lpad(row_number() OVER (PARTITION BY trace_id ORDER BY start_time)::text, 8, '0') AS dord
    FROM span_current
    WHERE parent_span_id IS NULL
      AND (tenant_id,trace_id) IN (SELECT tenant_id,trace_id FROM froze_traces)
    UNION ALL
    SELECT c.span_id, c.parent_span_id, c.trace_id, c.tenant_id, p.lvl+1,
           p.dord||'.'||lpad(row_number() OVER (PARTITION BY c.parent_span_id ORDER BY c.start_time)::text,8,'0')
    FROM span_current c JOIN dfs p
      ON c.parent_span_id = p.span_id AND c.tenant_id = p.tenant_id
)
UPDATE span_current sc SET lvl = d.lvl, dotted_order = d.dord
FROM dfs d WHERE sc.span_id = d.span_id;
-- pre/post (nested set) 可由 dotted_order 排序后 row_number 双序生成, 略

-- step3: 批量 INSERT 进列存冷区 (这就是"冻结=批量追加写到 CStore")
INSERT INTO span_current_cold (span_id, tenant_id, trace_id, root_id, parent_span_id,
    pre, post, lvl, dotted_order, thread_id, span_kind, name, start_time, end_time, status,
    input_tokens, output_tokens, total_cost, latency_ms, model, tags_json, attrs,
    input_ref, output_ref, is_sampled_for_vector)
SELECT span_id, tenant_id, trace_id, root_id, parent_span_id,
    pre, post, lvl, dotted_order, thread_id, span_kind, name, start_time, end_time, status,
    input_tokens, output_tokens, total_cost, latency_ms, model,
    to_jsonb(tags),                                -- 数组 -> jsonb (CStore 无数组)
    attrs, input_ref, output_ref, is_sampled_for_vector
FROM span_current
WHERE (tenant_id,trace_id) IN (SELECT tenant_id,trace_id FROM froze_traces);

-- step4: 标记 + 清理热区
UPDATE span_current SET is_frozen = true
WHERE (tenant_id,trace_id) IN (SELECT tenant_id,trace_id FROM froze_traces);
-- 从热物化表删冻结行(可延迟一两天再删, 双写期对账)
DELETE FROM span_current
WHERE is_frozen AND (tenant_id,trace_id) IN (SELECT tenant_id,trace_id FROM froze_traces);

-- step5: 丢弃热区 span_events 老分区(整分区 DROP, 不是 DELETE, 零写放大)
ALTER TABLE span_events DROP PARTITION FOR ('2026-05-01 00:00:00+08');
```

> **CStore 仅追加写如何配合**:冻结天然是"一次性批量 INSERT 已不可变的死 trace",完全契合列存"只追加"。冻结后冷区数据不再变,无 UPDATE。热区 `span_events` 老分区用 `DROP PARTITION` 整块回收(非逐行删)。`[TODO]` 冷区向量/全文检索:CStore 仅支持 tsvector-GIN,**不支持 BM25/HNSW**(已核实/`[TODO-4]`)→ 语义召回与中文 BM25 统一只对热区 `span_current` + `span_vectors` 提供;冷区只做**聚合/扫描类**分析(按 trace/时间/model 统计),检索类查询若要覆盖冷区,需另建检索镜像或对冷区做 tsvector-GIN 降级方案。

---

## 5. 活 trace 查询:实时从热区折叠 running 态

活 trace = 该 span 有 `start` 事件、**无** `end/error` 事件。视图实时折叠:

```sql
-- ============================================================
-- span_current_live: 活 trace 实时折叠视图 (只覆盖未结束的 span)
-- 数据量小(只有正在跑的 trace), query-time fold 足够快
-- ============================================================
CREATE OR REPLACE VIEW span_current_live AS
SELECT
    e.tenant_id, e.span_id,
    max(e.trace_id) FILTER (WHERE e.trace_id IS NOT NULL)                       AS trace_id,
    (array_agg(e.parent_span_id ORDER BY e.seq,e.ts,e.event_id)
         FILTER (WHERE e.parent_span_id IS NOT NULL))[count(e.parent_span_id)]  AS parent_span_id,
    (array_agg(e.thread_id ORDER BY e.seq,e.ts,e.event_id)
         FILTER (WHERE e.thread_id IS NOT NULL))[count(e.thread_id)]            AS thread_id,
    (array_agg(e.name ORDER BY e.seq,e.ts,e.event_id)
         FILTER (WHERE e.name IS NOT NULL))[count(e.name)]                      AS name,
    (array_agg(e.model ORDER BY e.seq,e.ts,e.event_id)
         FILTER (WHERE e.model IS NOT NULL))[count(e.model)]                    AS model,
    min(e.ts) FILTER (WHERE e.event_type = 1)                                   AS start_time,
    NULL::timestamptz                                                           AS end_time,   -- 定义上未结束
    CASE WHEN bool_or(e.event_type=5) THEN 2 ELSE 0 END                         AS status,     -- 进行中报 error 也可见
    jsonb_merge_agg(e.attrs_patch ORDER BY e.seq,e.ts,e.event_id)
         FILTER (WHERE e.attrs_patch IS NOT NULL)                               AS attrs,
    bool_or(e.is_sampled_for_vector)                                            AS is_sampled_for_vector
FROM span_events e
GROUP BY e.tenant_id, e.span_id
HAVING bool_or(e.event_type = 1)            -- 有 start
   AND NOT bool_or(e.event_type = 3);       -- 无 end -> running

-- 对外统一视图: 冷物化态 + 热实时态
CREATE OR REPLACE VIEW span_current_all AS
SELECT span_id,tenant_id,trace_id,parent_span_id,thread_id,name,model,
       start_time,end_time,status,attrs,is_sampled_for_vector FROM span_current
WHERE end_time IS NOT NULL          -- 已结束的从物化表取
UNION ALL
SELECT span_id,tenant_id,trace_id,parent_span_id,thread_id,name,model,
       start_time,end_time,status,attrs,is_sampled_for_vector FROM span_current_live;
```

**活 trace 子树查询**(树未冻结、无 pre/post 编码时):用 `CONNECT BY` 或递归 CTE 走邻接 `parent_span_id` 实时重建:
```sql
-- 实时找活 trace 的某子树 (邻接遍历, 走 idx_se_parent)
WITH RECURSIVE sub AS (
    SELECT * FROM span_current_all WHERE span_id = :root_span AND tenant_id=:tid
    UNION ALL
    SELECT c.* FROM span_current_all c JOIN sub p
      ON c.parent_span_id = p.span_id AND c.tenant_id = :tid
)
SELECT * FROM sub ORDER BY start_time;
```

> 性能取舍:活 trace 树用**邻接递归**(冻结后才升级到 pre/post 区间编码 O(1) 子树),因为活 trace 树小、随时变,维护区间编码不划算;历史 trace 走 §2 的 `idx_sc_subtree`。

---

## 6. 设计理由小结与不确定项汇总

**核心权衡:**
1. 事件表只插不更 → 规避行存更新链膨胀,天然支持乱序/晚到;折叠成本转移到读侧并被物化表摊销。
2. span_current 混合(物化冷态 + 实时热态视图)→ 既满足"活 trace 必须实时",又让 99% 历史查询命中可建索引(向量/BM25/区间编码)的物理表。
3. 冷热双物理表(行存→列存)而非同表转引擎 → **被 openGauss 硬约束逼出**(ORIENTATION 不可 ALTER、CStore 无 INTERVAL 分区),冻结=批量追加写,契合 CStore 只追加。

**TODO 待实测(诚实标注,未编造语法):**
- `[TODO-1]` JSONB `||` 拼接算子是否原生可用(已用 `jsonb_set`+自定义聚合规避,不阻塞)。
- `[TODO-2]` 自定义有序聚合 `jsonb_merge_agg` 行为 / 是否需 `WITHIN GROUP` 写法。
- `[TODO-3]` BM25(`USING fulltext`)/HNSW 索引能否建在**分区表**(GLOBAL/LOCAL);本设计 `span_current` 用非分区表规避。
- `[TODO-4]` CStore 冷区能否建 HNSW/BM25(已知 GIN 仅 tsvector)→ 现设计冷区不承担检索,只做聚合;若需冷区检索须另设镜像。
- `[TODO-5]` `ON CONFLICT DO UPDATE` vs `MERGE` 在目标 openGauss 版本的可用性二选一。
- `[TODO-6]` `array_agg(... ORDER BY ...)` 的保序性与 `last_value IGNORE NULLS` 替代项;本设计用前者(更可移植)。
- `[TODO-7]` UStore `STORAGE_TYPE=USTORE` 与 INTERVAL 分区组合的语法确认(文档示例多为 Astore)。

---

## 来源
- [Create Table Partition (RANGE+INTERVAL) — openGauss 5.0](https://docs.opengauss.org/en/docs/5.0.0/docs/SQLReference/create-table-partition.html)
- [Column Store 限制(仅 RANGE 分区 / GIN 仅 tsvector / 只追加)— openGauss](https://docs.opengauss.org/en/docs/5.1.0/docs/BriefTutorial/column-store.html)
- [Enhanced Features / 列存索引支持 — openGauss 3.1.1](https://docs.opengauss.org/en/docs/3.1.1/docs/Description/enhanced-features.html)
- [ALTER TABLE / ORIENTATION 不可改 — openGauss](https://docs.opengauss.org/en/docs/3.1.1/docs/BriefTutorial/column-store.html)
- [JSON/JSONB Functions and Operators (jsonb_set, ->, ->>) — openGauss](https://docs.opengauss.org/en/docs/5.0.0/docs/SQLReference/json-jsonb-functions-and-operators.html)
- [SELECT 语法 (DISTINCT ON / WINDOW / CONNECT BY) — opengauss-mirror/docs](https://github.com/opengauss-mirror/docs/blob/master/docs/zh/sql_reference/select.md)
- [BM25 使用指导 — openGauss DataVec](https://docs.opengauss.org/zh/docs/latest-lite/docs/DataVec/BM25%E4%BD%BF%E7%94%A8%E6%8C%87%E5%AF%BC.html)
- [yiTrace Fulltext Indexes (USING fulltext / DICTS / ALGORITHMS / @~@)](https://yitrace.com/docs/user-guide/fulltext-indexes)
---

## I2 树编码+线程重建+晚到处理

I now have verified syntax. Note: yiTrace-Lite (PG plugin) uses `yitrace_graph` / `floatvector`, but the prompt explicitly specifies the openGauss yiTrace build with named indexes HNSW/IVF/IVFPQ/DiskANN and a "专用向量存储管理器" — these are different products. I'll write against the openGauss yiTrace names the prompt gives, mark the index-method keyword as TODO-to-confirm, and provide the verified operators. Here is the complete design.

---

# yiTrace 树编码与查询设计：openGauss DDL + SQL

## 0. 语法核实清单（先声明可信度，避免编造）

| 语法点 | 状态 | 依据 |
|---|---|---|
| `PARTITION BY RANGE(ts) INTERVAL('1 day')` + 初始 `VALUES LESS THAN` | 已核实存在（openGauss 5.x 原生 interval 分区） | docs.opengauss.org create-table-partition |
| `WITH RECURSIVE` 递归 CTE | 已核实（PG 血统，openGauss 支持） | openGauss 论坛/PG 同源 |
| BM25 索引 `USING fulltext(col) WITH (DICTS=..., ALGORITHMS='BM25', COEFFICIENTS='b=..:k=..')`；查询算子 `@~@`；排序函数 `bm25_score()` | 已核实 | yitrace.com/docs/user-guide/fulltext-indexes |
| `CREATE TEXT SEARCH DICTIONARY x (TEMPLATE = vex_jieba)`、`vexjieba_add_userdict()` | 已核实 | 同上 |
| 向量距离算子 `<->`(L2)、`<=>`(cosine)、`<~>`(负内积/MIPS) | 已核实（yiTrace 系） | yiTrace-Lite README |
| **openGauss-yiTrace 向量索引方法关键字**（`hnsw`/`diskann`/`ivf`/`ivfpq` 还是统一 `yitrace_graph`/`GRAPH_INDEX`），算子类名（`vector_cosine_ops` vs `floatvector_cosine_ops`），向量列类型名（`vector(N)` vs `floatvector(N)`） | **TODO 待核实**：yiTrace-Lite（PG 插件）用 `yitrace_graph`/`floatvector`；prompt 说的是 openGauss 信创版"4 种向量索引 + 专用存储管理器"，关键字可能不同。下文用占位 `USING hnsw (... vector_cosine_ops)`，**部署前以本机 `\dA`/`pg_am` 实测为准**。 |
| **CStore(列存)分区上能否建 GIN / 向量索引** | **部分核实**：文档明确"列存表支持 GIN，但不支持 partial/unique index"。**列存上能否建向量索引未核实** → 设计上**不依赖**，所有 GIN/向量/BM25 索引一律建在**行存(UStore)的 `span_current` / `span_vectors`** 上，CStore 仅作老分区冷归档（顺序扫描+列裁剪）。这条是架构安全底线。 |
| interval 分区键是否仅限单列 timestamp/date | **TODO 待核实**，但行业惯例与文档示例均为单列 timestamp，本设计按此用 `ts timestamptz` 单列分区键。 |

---

## 1. 双编码模型（写侧邻接 + 读侧区间）

**写侧**：`span_events.parent_span_id` 邻接表，乱序/晚到友好，只 INSERT。
**读侧**：`span_current` 上物化 `pre / post / lvl`（嵌套集 / DFS 区间编码）+ `dotted_order`（LangSmith 兼容，材化化路径串）。
两者关系：邻接表是"事实来源 + 兜底"；区间编码是"冻结快照的高速读路径"；`dotted_order` 是"无需 JOIN 的有序前缀路径"，三者互补（见 §6）。

### 1.1 span_events（append-only 事件表）

```sql
-- 近端高频写：行存 UStore（原地更新+undo，但本表只 INSERT，选 USTORE 为了 undo 段对高并发 INSERT 的 MVCC 友好；
-- 若该 openGauss 版本 interval 分区不支持 USTORE，则改 ORIENTATION=ROW 默认 ASTORE，见末尾注）
CREATE TABLE span_events (
    event_id        bigint        NOT NULL,          -- 全局自增/雪花，单调即可
    tenant_id       bigint        NOT NULL,
    trace_id        bigint        NOT NULL,          -- 用 bigint(雪花) 而非 uuid，索引更小、范围扫更快
    span_id         bigint        NOT NULL,
    seq             int           NOT NULL,          -- 单 span 内事件序（生产端单调；并列时用 ts 兜底）
    event_type      smallint      NOT NULL,          -- 1=start 2=update 3=end 4=tool_result 5=error 6=feedback
    ts              timestamptz   NOT NULL,          -- 分区键
    parent_span_id  bigint,                          -- 写侧邻接，可空/晚到
    root_id         bigint,                          -- 生产端能给则给，给不了折叠时回填
    thread_id       bigint,
    -- 热点小列：增量值（end 事件携带最终值；start 携带初值；update 携带 delta 或快照，由生产端约定）
    span_kind       smallint,
    name            text,
    model           text,
    input_tokens    int,
    output_tokens   int,
    total_cost      numeric(18,6),
    latency_ms      int,
    status          smallint,                        -- 0=running 1=ok 2=error（end/error 事件写）
    start_time      timestamptz,                     -- start 事件写
    end_time        timestamptz,                     -- end 事件写
    attrs_patch     jsonb,                           -- 增量属性，折叠时深合并
    payload_ref     bigint,                          -- 大字段指针 -> payload_store.payload_id（CAS）
    -- 主键含分区键(openGauss 分区表本地唯一约束须含分区键)
    CONSTRAINT pk_span_events PRIMARY KEY (ts, event_id)
)
WITH (ORIENTATION = ROW, STORAGE_TYPE = USTORE)   -- TODO 待核实: interval 分区 + USTORE 组合;不行则去掉 STORAGE_TYPE
PARTITION BY RANGE (ts)
INTERVAL ('1 day')                                 -- 自动按天建分区
(
    PARTITION p_init VALUES LESS THAN ('2026-01-01 00:00:00+08')
);
```

写侧索引（建在行存事件表，CStore 冷分区不建——见 §0）：

```sql
-- 折叠驱动：按 span 取该 span 的全部事件，按序合并
CREATE INDEX idx_se_span ON span_events (tenant_id, span_id, seq, ts) LOCAL;
-- 活 trace / 树重建驱动：按 trace 拉全量事件
CREATE INDEX idx_se_trace ON span_events (tenant_id, trace_id, ts) LOCAL;
-- 晚到子节点找父：按 parent 反查（晚到 end/child 处理用）
CREATE INDEX idx_se_parent ON span_events (tenant_id, parent_span_id) LOCAL WHERE parent_span_id IS NOT NULL;
```

> `LOCAL` = 分区本地索引，随 interval 分区自动扩展，老分区可独立 DROP/归档。

### 1.2 span_current（折叠后当前态）—— 论证：用**物化表**，不用视图

**结论：物化表（写时折叠 / 增量 upsert），不用纯视图。** 理由：
- 区间编码 `pre/post/lvl` **本质需要一次全树 DFS**，纯视图无法在线 DFS 出连续区间（递归 CTE 每查一次重算，子树 `BETWEEN` 优化就没了）。
- 活 trace 占比小但查询频繁；历史 trace 一旦 end 即"冻结"——天然适合"冻结时物化一次"。
- 折叠是 N 事件→1 行的收敛，物化后读放大从 O(事件数) 降到 O(1)。

但保留一个**视图 `span_live`** 兜底活 trace（见 §1.3），覆盖"还没冻结、物化表里没有或不全"的 span。

```sql
CREATE TABLE span_current (
    span_id         bigint        NOT NULL,
    tenant_id       bigint        NOT NULL,
    trace_id        bigint        NOT NULL,
    root_id         bigint        NOT NULL,
    parent_span_id  bigint,
    -- 区间编码（冻结时 DFS 物化；活 trace 阶段为 NULL）
    pre             bigint,                          -- 进入序（前序）
    post            bigint,                          -- 离开序（后序）；子树 = [pre,post]
    lvl             int,                             -- 深度
    dotted_order    text,                            -- LangSmith 兼容：时间戳+span_id 拼成的有序路径
    thread_id       bigint,
    span_kind       smallint,
    name            text,
    start_time      timestamptz,
    end_time        timestamptz,                     -- NULL = 活/未结束
    status          smallint      NOT NULL DEFAULT 0,-- 0 running 1 ok 2 error
    input_tokens    int,
    output_tokens   int,
    total_cost      numeric(18,6),
    latency_ms      int,
    model           text,
    tags            text[],                          -- 行存可用数组(CStore 不支持数组,正好我们不放 CStore)
    attrs           jsonb,                           -- 折叠后的全量属性(深合并结果)
    input_ref       bigint,
    output_ref      bigint,
    is_sampled_for_vector boolean NOT NULL DEFAULT false,
    -- 晚到降级标记（见 §5）
    encoding_state  smallint      NOT NULL DEFAULT 0,-- 0=未编码(活) 1=已物化区间 2=stale需重算 3=溢出走邻接兜底
    frozen_at       timestamptz,
    CONSTRAINT pk_span_current PRIMARY KEY (span_id)
)
WITH (ORIENTATION = ROW, STORAGE_TYPE = USTORE);     -- 行存:此表是被原地更新(冻结/回填)的,正是 UStore 适用场景
```

索引（全部行存，覆盖 §2~§4 查询）：

```sql
-- ② 子树查询：trace_id + 区间。把 pre 放复合键末位以支持 BETWEEN 范围扫
CREATE INDEX idx_sc_subtree   ON span_current (tenant_id, trace_id, pre);
-- ③ 找根 / 整树加载（按 trace 等值 + 树序）
CREATE INDEX idx_sc_trace     ON span_current (tenant_id, trace_id, pre, lvl);
-- 找根快捷：root 自身 parent 为空
CREATE INDEX idx_sc_root      ON span_current (tenant_id, trace_id) WHERE parent_span_id IS NULL;
-- ④ 线程重建：thread → 跨 trace 序列
CREATE INDEX idx_sc_thread    ON span_current (tenant_id, thread_id, start_time);
-- 活 trace 快速捞：未结束
CREATE INDEX idx_sc_running   ON span_current (tenant_id, trace_id) WHERE end_time IS NULL;
-- dotted_order 前缀查询（子树/有序加载的免 JOIN 路径，text_pattern_ops 支持 LIKE 'prefix%')
CREATE INDEX idx_sc_dotted    ON span_current (tenant_id, dotted_order text_pattern_ops);
-- 晚到重算扫描
CREATE INDEX idx_sc_stale     ON span_current (encoding_state) WHERE encoding_state IN (2,3);
-- attrs 过滤(语义召回的结构化预过滤)
CREATE INDEX idx_sc_attrs_gin ON span_current USING gin (attrs);
```

### 1.3 折叠逻辑（事件 → span_current）

折叠 SQL 核心：单 span 内按 `seq, ts` 取最后非空值（后写覆盖），`attrs_patch` 用 `||` 浅合并/`jsonb_set` 深合并，`end` 事件补 `end_time/status`。openGauss 支持 `DISTINCT ON` 与窗口函数。

```sql
-- 折叠一个 trace 的所有 span 当前态（活 trace 实时折叠 / 冻结前的物化）
-- 用聚合做"后写覆盖 + delta 累加 + patch 合并"
WITH folded AS (
  SELECT
    span_id,
    max(tenant_id)                                   AS tenant_id,
    max(trace_id)                                    AS trace_id,
    -- 后写覆盖类小列：按 (seq,ts) 取最新非空。用相关子查询或 max(...) FILTER
    (array_agg(parent_span_id ORDER BY seq, ts) FILTER (WHERE parent_span_id IS NOT NULL))[1]
                                                     AS parent_span_id,  -- 取最早确定的父(晚到友好)
    max(name)        FILTER (WHERE name IS NOT NULL) AS name,
    max(span_kind)   FILTER (WHERE span_kind IS NOT NULL) AS span_kind,
    max(model)       FILTER (WHERE model IS NOT NULL)     AS model,
    min(start_time)  FILTER (WHERE event_type = 1)   AS start_time,
    max(end_time)    FILTER (WHERE event_type = 3)   AS end_time,
    -- delta 累加 (token/cost/latency 用增量值时 sum；用快照时 max)
    sum(input_tokens)                                AS input_tokens,
    sum(output_tokens)                               AS output_tokens,
    sum(total_cost)                                  AS total_cost,
    max(latency_ms)  FILTER (WHERE event_type = 3)   AS latency_ms,
    -- 状态：有 error 事件→2，有 end→1，否则 0(running)
    CASE WHEN bool_or(event_type = 5) THEN 2
         WHEN bool_or(event_type = 3) THEN 1
         ELSE 0 END                                  AS status,
    -- attrs 深合并：按序聚合 jsonb（openGauss 用 jsonb_object_agg 不够，用自定义有序合并；
    -- 简化：浅合并用 || 折叠，深合并见下方函数 TODO）
    (SELECT jsonb_agg(attrs_patch ORDER BY seq, ts)
       FROM span_events e2 WHERE e2.span_id = e.span_id AND e2.attrs_patch IS NOT NULL)
                                                     AS attrs_patches,
    max(payload_ref) FILTER (WHERE event_type IN (1,2)) AS input_ref,
    max(payload_ref) FILTER (WHERE event_type IN (3,4)) AS output_ref
  FROM span_events e
  WHERE tenant_id = $1 AND trace_id = $2
  GROUP BY span_id
)
INSERT INTO span_current AS sc (span_id, tenant_id, trace_id, parent_span_id, name, span_kind, model,
       start_time, end_time, input_tokens, output_tokens, total_cost, latency_ms, status,
       attrs, input_ref, output_ref, root_id, encoding_state)
SELECT span_id, tenant_id, trace_id, parent_span_id, name, span_kind, model,
       start_time, end_time, input_tokens, output_tokens, total_cost, latency_ms, status,
       tv_jsonb_deep_merge(attrs_patches),          -- 自定义深合并函数, 见下
       input_ref, output_ref,
       coalesce(parent_span_id, span_id),           -- root_id 占位, 折叠后由 §3 回填真根
       0
FROM folded
ON CONFLICT (span_id) DO UPDATE SET
       parent_span_id = COALESCE(EXCLUDED.parent_span_id, sc.parent_span_id),
       end_time       = COALESCE(EXCLUDED.end_time, sc.end_time),
       status         = GREATEST(sc.status, EXCLUDED.status),
       input_tokens   = EXCLUDED.input_tokens,
       output_tokens  = EXCLUDED.output_tokens,
       total_cost     = EXCLUDED.total_cost,
       latency_ms     = COALESCE(EXCLUDED.latency_ms, sc.latency_ms),
       attrs          = tv_jsonb_deep_merge_2(sc.attrs, EXCLUDED.attrs),
       -- 若已物化区间(state=1)又来了新内容 → 标 stale
       encoding_state = CASE WHEN sc.encoding_state = 1 THEN 2 ELSE sc.encoding_state END;
```

深合并 jsonb 辅助函数（openGauss `||` 仅做顶层浅合并，递归合并须自定义；标 TODO 性能）：

```sql
-- 递归深合并两个 jsonb；右覆盖左，object 递归，其余直接覆盖
CREATE OR REPLACE FUNCTION tv_jsonb_deep_merge_2(a jsonb, b jsonb)
RETURNS jsonb LANGUAGE plpgsql IMMUTABLE AS $$
DECLARE k text; v jsonb; r jsonb := COALESCE(a, '{}'::jsonb);
BEGIN
  IF b IS NULL THEN RETURN a; END IF;
  IF jsonb_typeof(a) IS DISTINCT FROM 'object'
     OR jsonb_typeof(b) IS DISTINCT FROM 'object' THEN
     RETURN b;                                      -- 非对象直接右覆盖
  END IF;
  FOR k, v IN SELECT * FROM jsonb_each(b) LOOP
     IF r ? k AND jsonb_typeof(r->k)='object' AND jsonb_typeof(v)='object' THEN
        r := jsonb_set(r, ARRAY[k], tv_jsonb_deep_merge_2(r->k, v));
     ELSE
        r := jsonb_set(r, ARRAY[k], v, true);
     END IF;
  END LOOP;
  RETURN r;
END $$;
-- tv_jsonb_deep_merge(jsonb[]) 对数组按序 fold 调用上面两元函数(略)
```

**活 trace 视图兜底**（事件区直接折叠出 running 态，覆盖尚未物化进 span_current 的 span）：

```sql
CREATE VIEW span_live AS
SELECT span_id, tenant_id, trace_id,
       (array_agg(parent_span_id ORDER BY seq) FILTER (WHERE parent_span_id IS NOT NULL))[1] AS parent_span_id,
       min(start_time) FILTER (WHERE event_type=1) AS start_time,
       CASE WHEN bool_or(event_type=3) THEN 1 WHEN bool_or(event_type=5) THEN 2 ELSE 0 END AS status,
       max(name) FILTER (WHERE name IS NOT NULL)   AS name
FROM span_events
GROUP BY span_id, tenant_id, trace_id;
-- 用法：活 trace 实时面板查 span_live(走 idx_se_trace)；历史/冻结查 span_current。
```

---

## 2. 子树查询（区间编码主路径）

给定子树根 `:root_span_id`，先取其 `pre/post`，再范围扫。**单次自查 + 一次范围扫**：

```sql
-- 子树查询(含根)：trace_id 等值锁定分区集 + pre BETWEEN
SELECT c.*
FROM span_current c
JOIN span_current r
  ON r.span_id = :root_span_id AND r.tenant_id = :tenant_id
WHERE c.tenant_id = :tenant_id
  AND c.trace_id  = r.trace_id
  AND c.pre BETWEEN r.pre AND r.post          -- 子树 = 区间包含
  AND c.encoding_state = 1                     -- 仅已物化区间的部分
ORDER BY c.pre;                                -- 前序 = DFS 树序
```
走 `idx_sc_subtree (tenant_id, trace_id, pre)`：`trace_id` 等值定位，`pre BETWEEN` 做索引范围扫，`ORDER BY pre` 免排序。
**深度限制子树**（只要根下 N 层）：加 `AND c.lvl <= r.lvl + :n`。
**只要直接子节点**：`AND c.lvl = r.lvl + 1`（仍走区间扫，比邻接表 N 次查父快）。

> 若该 trace 有晚到 span（`encoding_state IN (2,3)`），区间扫会漏掉它们 → 用 §5 的 UNION 兜底补齐。

---

## 3. 找根 / 整树加载

```sql
-- 找根：parent 为空即根(一个 trace 可能多根=多个入口 run)
SELECT * FROM span_current
WHERE tenant_id = :tenant_id AND trace_id = :trace_id AND parent_span_id IS NULL;
-- 走 idx_sc_root

-- 整树有序加载(DFS 序)：直接按 pre 排
SELECT span_id, parent_span_id, lvl, pre, post, name, span_kind, status,
       start_time, end_time, latency_ms, total_cost
FROM span_current
WHERE tenant_id = :tenant_id AND trace_id = :trace_id
ORDER BY pre;                                  -- 走 idx_sc_trace, 免排序
-- 注意:只 SELECT 小列, 不取 attrs / *_ref(大字段晚物化, 见 §4 payload)
```

**root_id 回填**（折叠后一次性把整 trace 的 root_id 修正为真根，避免每行递归求根）：

```sql
WITH roots AS (
  SELECT trace_id, span_id AS root_span
  FROM span_current
  WHERE tenant_id = :tenant_id AND trace_id = :trace_id AND parent_span_id IS NULL
)
UPDATE span_current c SET root_id = r.root_span
FROM roots r
WHERE c.tenant_id = :tenant_id AND c.trace_id = r.trace_id;
-- 多根场景:root_id 取"该 span 沿邻接链上溯到的根",在 §5 DFS 物化时顺带写入更准。
```

---

## 4. 线程重建（thread_id → 跨 trace run 序列）

线程是跨多个 trace 的、按时间排列的 run 片段（如同一会话 session 的多轮）。**只拉小列，大 payload 走 ref 延迟取**。

可选 **thread 汇总表**（论证：中小规模可不建，靠 `idx_sc_thread` 索引扫即可；但若线程极长、面板高频翻页，建汇总表把"每个 thread 的 run 边界 + 计数"预聚合更稳）：

```sql
-- 可选 thread 汇总表(增量维护:每个 trace 冻结时 upsert 一行)
CREATE TABLE thread_summary (
    thread_id      bigint     NOT NULL,
    tenant_id      bigint     NOT NULL,
    trace_id       bigint     NOT NULL,       -- 该 thread 包含的一个 run/trace
    first_ts       timestamptz NOT NULL,
    last_ts        timestamptz,
    span_count     int,
    root_span_id   bigint,
    title          text,                      -- 该 run 的入口 span name, 列表展示用
    status         smallint,
    CONSTRAINT pk_thread_summary PRIMARY KEY (thread_id, first_ts, trace_id)
) WITH (ORIENTATION = ROW, STORAGE_TYPE = USTORE);
CREATE INDEX idx_ts_thread ON thread_summary (tenant_id, thread_id, first_ts);
```

线程重建 SQL（只拉小列）：

```sql
-- 方案A:有汇总表 — 列出 thread 的 run 时间线(分页友好)
SELECT trace_id, root_span_id, title, status, first_ts, last_ts, span_count
FROM thread_summary
WHERE tenant_id = :tenant_id AND thread_id = :thread_id
ORDER BY first_ts;                            -- 走 idx_ts_thread

-- 方案B:无汇总表 — 直接从 span_current 重建(取每个 trace 的根作时间线节点)
SELECT trace_id,
       min(start_time)                         AS run_start,
       max(end_time)                           AS run_end,
       count(*)                                AS span_count,
       (array_agg(name      ORDER BY start_time) FILTER (WHERE parent_span_id IS NULL))[1] AS title,
       max(status)                             AS status
FROM span_current
WHERE tenant_id = :tenant_id AND thread_id = :thread_id
GROUP BY trace_id
ORDER BY run_start;                            -- 走 idx_sc_thread, 不触碰 attrs/payload
```

---

## 5. 晚到 span 处理（核心难点：区间编码已物化，连续区间插不进）

**问题**：`pre/post` 是冻结时一次 DFS 分配的连续整数，没有空隙。冻结后到达的 `end`（改 status）尚可（只改小列），但晚到的**新子节点**无法获得落在 `[parent.pre, parent.post]` 内的空闲整数 → 区间扫会漏掉它。

**分级降级策略**（按代价从小到大）：

**L0 — 晚到 end / 同 span 更新（不改树形）**：直接 upsert `span_current`（§1.3 的 `ON CONFLICT`），`encoding_state` 维持 1。区间编码不受影响，零成本。

**L1 — 晚到新节点，标记 stale + 兜底可见**：
新子节点先以 `encoding_state=3`（溢出/未编码）插入 `span_current`，`pre/post=NULL`。
- 它**自身**仍能被 `trace_id` 等值查询命中（§3 整树加载不依赖 pre 也能取到，只是排序退化）。
- §2 的区间子树查询用 **UNION 兜底**把这些未编码节点补回：

```sql
-- 子树查询(健壮版):区间扫 + 邻接表递归 CTE 兜底未编码节点
WITH RECURSIVE root_box AS (
  SELECT pre, post, trace_id FROM span_current
  WHERE span_id = :root_span_id AND tenant_id = :tenant_id
),
-- A. 已物化区间部分(快)
encoded AS (
  SELECT c.* FROM span_current c, root_box b
  WHERE c.tenant_id = :tenant_id AND c.trace_id = b.trace_id
    AND c.encoding_state = 1 AND c.pre BETWEEN b.pre AND b.post
),
-- B. 邻接表递归补未编码/stale 节点(慢但量小,只在有晚到时触发)
adj(span_id) AS (
  SELECT :root_span_id
  UNION ALL
  SELECT c.span_id
  FROM span_current c JOIN adj a ON c.parent_span_id = a.span_id
  WHERE c.tenant_id = :tenant_id
),
overflow AS (
  SELECT c.* FROM span_current c JOIN adj a ON c.span_id = a.span_id
  WHERE c.encoding_state IN (2,3)               -- 只补未编码/stale, 已编码的走 encoded
)
SELECT * FROM encoded
UNION
SELECT * FROM overflow
ORDER BY coalesce(pre, 9223372036854775807), start_time;
```
> 递归 CTE 走 `idx_se_parent`/`idx_sc_*`，单 trace 内规模小（中小客户单 trace 通常 ≤ 数千 span），可接受。

**L2 — 局部重算（攒批 / 阈值触发）**：当某 trace 的 `encoding_state IN (2,3)` 节点数超阈值，或离冻结超过 T（如 1h），对该 trace **整树重跑一次 DFS 物化**（§5.1），把所有节点重新编号、`encoding_state` 归 1。因为是"整 trace 重编号"而非"插空隙"，避免了连续区间的局部插入问题。中小规模整 trace 重编成本低。

### 5.1 pre/post DFS 物化算法（冻结 / 重算时调用）

**思路**：在内存/SQL 里对邻接表做一次 DFS，前序计数→`pre`，回溯计数→`post`，深度→`lvl`，并拼 `dotted_order`。openGauss 递归 CTE 无法天然产出"后序 post 编号"，故推荐 **plpgsql 显式 DFS** 写回；递归 CTE 仅适合算 `lvl/dotted_order`（前缀路径），不适合算 `post`。

```sql
-- 对一个 trace 做 DFS, 物化 pre/post/lvl/dotted_order
CREATE OR REPLACE PROCEDURE tv_materialize_intervals(p_tenant bigint, p_trace bigint)
LANGUAGE plpgsql AS $$
DECLARE
  cnt bigint := 0;
  rec record;
BEGIN
  -- 用栈式 DFS:借助递归 CTE 先求出每个节点的(lvl, dotted_order, 排序键), 前序 pre 即按 DFS 序行号
  CREATE TEMP TABLE _dfs ON COMMIT DROP AS
  WITH RECURSIVE walk AS (
    SELECT span_id, parent_span_id, 0 AS lvl,
           -- dotted_order: 时间戳(20位,补零)+'Z'+span_id, 路径用 '.' 连(LangSmith 风格)
           lpad(extract(epoch FROM start_time)::bigint::text, 20, '0') || to_hex(span_id) AS dotted_order,
           ARRAY[start_time, span_id::timestamptz]   -- 排序键路径(简化)
             AS sortpath,
           ARRAY[span_id] AS path
    FROM span_current
    WHERE tenant_id = p_tenant AND trace_id = p_trace AND parent_span_id IS NULL
    UNION ALL
    SELECT c.span_id, c.parent_span_id, w.lvl+1,
           w.dotted_order || '.' ||
             lpad(extract(epoch FROM c.start_time)::bigint::text, 20, '0') || to_hex(c.span_id),
           w.sortpath || ARRAY[c.start_time],
           w.path || c.span_id
    FROM span_current c JOIN walk w ON c.parent_span_id = w.span_id
    WHERE c.tenant_id = p_tenant
  )
  SELECT span_id, lvl, dotted_order, path
  FROM walk
  ORDER BY dotted_order;                          -- DFS 前序顺序

  -- 前序行号 = pre。post 用区间法:子节点数已知 => post = pre + 子树大小*2 ... 这里用简化双计数:
  -- 物化 pre = 行号*2; 用窗口算每个节点子树内最大行号 => post。
  -- 实务:子树大小 = 在 dotted_order 前缀关系下统计。这里给出 pre 物化与 post 推导:
  WITH ordered AS (
     SELECT span_id, lvl, dotted_order,
            row_number() OVER (ORDER BY dotted_order) AS rn
     FROM _dfs
  ),
  -- post: 某节点 post = 所有 dotted_order 以其为前缀的后代里最大 rn (含自身)
  bounds AS (
     SELECT o.span_id, o.lvl, o.dotted_order, o.rn AS pre_rn,
            max(d.rn) AS post_rn
     FROM ordered o
     JOIN ordered d
       ON d.dotted_order = o.dotted_order
       OR d.dotted_order LIKE o.dotted_order || '.%'    -- 后代前缀匹配
     GROUP BY o.span_id, o.lvl, o.dotted_order, o.rn
  )
  UPDATE span_current c SET
     pre  = b.pre_rn,
     post = b.post_rn,
     lvl  = b.lvl,
     dotted_order = b.dotted_order,
     encoding_state = 1,
     frozen_at = now()
  FROM bounds b
  WHERE c.span_id = b.span_id AND c.tenant_id = p_tenant;
END $$;
```
> 标 **TODO 性能**：上面 `bounds` 用 `LIKE prefix%` 自连接求 post 在大树上是 O(n²)；中小 trace（≤数千节点）可接受。大 trace 应改为应用层（Python/Java）一次内存 DFS 直接产出 `(span_id, pre, post, lvl)` 后 `COPY` 回写——这是生产推荐路径。`extract(epoch)::bigint` 物化 dotted_order 的精度/纳秒处理待按 LangSmith 规范对齐（见 §6）。

---

## 6. dotted_order 作一等列：与 pre/post 互补

`dotted_order`（LangSmith 兼容）= 从根到该节点每一跳的 `<start_time 编码><span_id>` 用 `.` 拼成的有序路径串。三者分工：

| 能力 | pre/post (区间) | dotted_order (路径串) | parent_span_id (邻接) |
|---|---|---|---|
| 子树"全部后代" | `pre BETWEEN`，索引范围扫，最快 | `LIKE 'prefix.%'`，免 JOIN，但前缀索引扫 | 递归 CTE，最慢 |
| 兄弟有序 / 排序展示 | 需配合 lvl | **天然字典序 = 时间序 = DFS 序**，直接 `ORDER BY dotted_order` | 无 |
| 晚到节点 | 插不进连续区间（要重算） | **天然可插**：新节点拼好父前缀即落到正确字典序位置，无需重编号 | 天然可插 |
| 找父/找根 | 需 JOIN | 字符串切分（取最后/第一段） | 直接 |
| 跨重算稳定性 | 重算后 pre/post 全变 | 只要 start_time 不变就稳定 | 稳定 |

**互补用法**：
- **读热路径**用 `pre/post`（整型 BETWEEN 最快）。
- **晚到 / 增量插入**用 `dotted_order`：新子节点直接 `parent.dotted_order || '.' || own_segment`，立即获得正确全序，**无需触发 §5.2 整树重算**即可正确排序展示——这正是把 dotted_order 提为一等列的最大价值（用它扛晚到的"有序性"，用 pre/post 扛"子树范围扫的速度"，stale 标记决定何时再批量对齐两者）。
- 子树查询的 dotted_order 版本（pre/post stale 时的等价兜底）：

```sql
SELECT c.* FROM span_current c
JOIN span_current r ON r.span_id = :root_span_id AND r.tenant_id = :tenant_id
WHERE c.tenant_id = :tenant_id AND c.trace_id = r.trace_id
  AND (c.dotted_order = r.dotted_order OR c.dotted_order LIKE r.dotted_order || '.%')
ORDER BY c.dotted_order;                       -- 走 idx_sc_dotted(text_pattern_ops)
```

---

## 7. 与其他 agent 表的接缝（向量 / payload，本任务范围外只给接口）

仅声明本设计依赖的接口列，DDL 细节由对应 agent 出：

```sql
-- span_vectors：行存，关联 span_current.span_id；过滤列冗余 trace_id/tenant_id 供带过滤 ANN 预过滤
-- 向量索引建在行存(CStore 不保证支持向量索引——§0)：
--   CREATE INDEX idx_sv_hnsw ON span_vectors USING hnsw (embedding vector_cosine_ops)
--        WITH (m=16, ef_construction=64);   -- TODO: 索引方法关键字/算子类名以本机 pg_am 实测为准
-- 带过滤 ANN: WHERE tenant_id=? AND trace_id=? ORDER BY embedding <=> :q LIMIT k;
-- payload_store：内容寻址 CAS(payload_id = hash)，TOAST 承载大 text；span_current.input_ref/output_ref 指向它。
--   大字段晚物化:树/线程查询只取 *_ref,详情展开时再 JOIN payload_store。
```

---

## 8. 冷热分层与 CStore 归档（呼应骨架第 1 条）

- `span_events` 近端 interval 分区 = 行存 UStore，承接高频 INSERT + 折叠扫描。
- 老分区（如 > 14 天、对应 trace 已全部冻结）→ **镜像/转存到列存归档表**做压缩与列裁剪分析扫：

```sql
-- 列存归档表(仅 NULL/NOT NULL/DEFAULT/PCK 约束;无数组;列数<<1000;不建 GIN/向量索引)
CREATE TABLE span_events_cold (LIKE span_events INCLUDING DEFAULTS)
WITH (ORIENTATION = COLUMN)
PARTITION BY RANGE (ts) INTERVAL ('1 month')
( PARTITION pc_init VALUES LESS THAN ('2026-01-01') );
-- 归档:INSERT INTO span_events_cold SELECT ... FROM 老分区; 然后 ALTER TABLE span_events DROP PARTITION ...
-- 列存放数组(tags)不行 → 归档时把 tags 序列化为 text;故 tags 数组只活在行存 span_current。
```
> CStore 仅追加写、不支持数组、约束受限 —— 因此**所有需要数组/GIN/向量/BM25/原地更新的对象都留在行存**，CStore 只做"老事件的压缩冷扫"。这是 §0 架构底线的落地。

---

## 9. 未决 / 待本机实测项（TODO 汇总）

1. **向量索引 DDL 关键字**：openGauss-yiTrace 信创版的 `USING <method>`（`hnsw`/`diskann`/`ivf`/`ivfpq` 还是统一 `yitrace_graph`/`GRAPH_INDEX`）、算子类名（`vector_cosine_ops` vs `floatvector_cosine_ops`）、向量列类型（`vector(N)` vs `floatvector(N)`）。距离算子 `<-> / <=> / <~>` 已核实。→ `\dA+`、`SELECT amname FROM pg_am;` 实测。
2. **interval 分区 + USTORE 组合**是否被支持；不支持则 `span_events` 退回默认 ASTORE 行存（功能不受影响，本表只 INSERT）。
3. **interval 分区键**是否仅限单列 timestamp/date（本设计已按单列 `ts timestamptz` 写）。
4. **CStore 分区能否建向量/GIN 索引**：本设计已规避（一律行存），无需依赖，但归档若想在冷区做 BM25/向量需另案。
5. **`tv_jsonb_deep_merge` 与 DFS post 计算的性能**：大 trace 改走应用层内存 DFS + COPY 回写；jsonb 深合并大对象时考虑落地为预合并快照。
6. **dotted_order 时间编码精度**：与 LangSmith 官方 `RFC3339 微秒 + UUID` 格式对齐（本设计用 epoch+span_id 占位），保证跨系统可比与字典序==时间序。

---

相关产出文件路径（本设计为纯文本返回，未落盘；若需保存请置于 `/Users/Four/JobProjects/yitrace/vex-x/docs/design/2026-06-16_tracevault-tree-encoding.md`）。

Sources:
- [openGauss Create Table Partition](https://docs.opengauss.org/en/docs/5.0.0/docs/SQLReference/create-table-partition.html)
- [openGauss Column Store](https://docs.opengauss.org/en/docs/3.0.0/docs/BriefTutorial/column-store.html)
- [yiTrace Full-Text Indexes](https://yitrace.com/docs/user-guide/fulltext-indexes)
- [yiTrace-Lite GitHub](https://github.com/yiTrace-THU/yiTrace-Lite)
- [openGauss Create Index](https://docs.opengauss.org/en/docs/5.0.0/docs/SQLReference/create-index.html)
---

## I3 索引+大字段+向量+中文全文

I now have all the authoritative syntax I need. Confirmed key facts:

- **CStore GIN**: tsvector-only (must wrap `to_tsvector`) → so JSONB GIN and BM25 must live on the row-store layer.
- **CStore btree**: no expression/partial indexes; PSORT is default.
- **GLOBAL index**: btree only, ≤31 cols, no column-store, no partial/expression → so on partitioned tables, secondary indexes are LOCAL.
- **yiTrace vector**: `floatvector(dim)`, ops classes `floatvector_cosine_ops`/`floatvector_l2_ops`, operators `<=>` (cosine) / `<->` (L2), GUC `hnsw_ef_search`.
- **yiTrace HybridANN**: `USING hybridann(v floatvector_cosine_ops, scalar1, scalar2, ...)` — embeds ≤31 scalar filter columns into the vector index (this is the real filtered-ANN mechanism). No index on empty table, no IS NULL filter pushdown.
- **yiTrace BM25**: `USING fulltext(col) WITH (DICTS, ALGORITHMS=BM25, COEFFICIENTS='b=0.75,k=1.2')`, operators `@~@` (score) / `@-@` (contain), `bm25_score()`, LOCAL/GLOBAL on partitioned tables. Jieba via `TEMPLATE = vex_jieba`, `vexjieba_add_userdict` + `vexjieba_reload`.

Now I'll produce the complete DDL design. This is my final deliverable.

---

# yiTrace — 索引 / 大字段 / 向量 / 中文全文 的 openGauss DDL 设计

约定:`[已查实]` = 在 openGauss/yiTrace 官方文档或文档站确认的语法;`[TODO待核实]` = 推断/需在目标实例上 `\d` 或试跑确认。下文 DDL 假设共享骨架的 `span_events`(RANGE+INTERVAL on `ts`)、`span_current`(折叠物化表)、`payload_store`、`span_vectors` 已建。

---

## 0. 全局存储分层决策(决定索引建在哪一层)

`[已查实]` 三条硬约束决定了整个索引矩阵的形状:

1. **列存(CStore) GIN 只支持 tsvector**:"列存表对 GIN 索引支持仅限于 tsvector 类型,创建列存 GIN 索引入参需要为 to_tsvector 函数(的返回值)"。→ **JSONB 的 `jsonb_path_ops` GIN 无法建在列存上**。
2. **列存 btree 不支持表达式索引、部分索引**(PSORT 是列存默认,不支持唯一索引)。
3. **GLOBAL 索引仅支持 btree、≤31 列、不支持列存、不支持表达式/部分索引**;分区表上不写 LOCAL/GLOBAL 时**默认 GLOBAL**。

结论(本设计据此排布):

| 数据/索引 | 放在哪 | 理由 |
|---|---|---|
| `span_events` 热区(近 N 天) | **UStore 行存**,RANGE+INTERVAL on `ts` | 高频 append + 偶发晚到事件;行存可建任意 btree LOCAL 索引 |
| `span_events` 冷区(老分区) | 镜像/转 **CStore 列存** | 列式压缩 + 扫描;但**只能 btree/psort + tsvector-GIN** |
| `span_current` 折叠物化表 | **UStore 行存** | 既要点查(span_id 主键)又要 JSONB-GIN + 向量 + BM25,只有行存全支持 |
| JSONB `attrs` 的 GIN | **只在行存 `span_current`** | 列存 GIN 仅 tsvector |
| 向量 / HybridANN | **行存 `span_vectors`** | 向量索引基于 ASTORE/行存 |
| BM25 全文 | **行存**(`payload_store` 或 `span_current` 提列) | BM25 索引建在文本列上,行存 |

> 对分区表的二级索引,本设计**一律显式写 `LOCAL`**:GLOBAL 不支持列存、不支持后续 `DROP/EXCHANGE PARTITION` 的高效维护,且 yiTrace 按 `ts` 滚动删分区,LOCAL 索引随分区一起 drop 最省事。

---

## ① 索引矩阵

### 1.1 热区行存 `span_events`(LOCAL 索引)

`[已查实]` 分区表 LOCAL btree 语法 `CREATE INDEX ... ON t(col) LOCAL;`

```sql
-- 子树查询/找根:trace 内按 (trace_id, span_id) 定位 + 折叠
CREATE INDEX ix_evt_trace      ON span_events (tenant_id, trace_id, span_id, seq) LOCAL;
-- 单 span 事件折叠:同一 span 的所有事件按 seq/ts 取回
CREATE INDEX ix_evt_span       ON span_events (span_id, seq) LOCAL;
-- 线程重建:thread_id 时间线
CREATE INDEX ix_evt_thread     ON span_events (thread_id, ts) LOCAL;
-- 写侧邻接(晚到的 parent):按 parent 找子
CREATE INDEX ix_evt_parent     ON span_events (parent_span_id) LOCAL;
-- 活 trace 扫描:只有 start 没 end 的 → 见 §折叠,用 event_type 部分索引(行存支持部分索引)
CREATE INDEX ix_evt_open       ON span_events (tenant_id, trace_id)
    WHERE event_type = 'start' LOCAL;   -- [TODO待核实] LOCAL 部分索引组合,行存支持部分索引[已查实],
                                        --   但 LOCAL+WHERE 同时出现需在实例上确认顺序(WHERE 在 LOCAL 前)
```

> `[TODO待核实]`:LOCAL 与 partial(`WHERE`)子句的语法书写顺序。openGauss 行存支持部分索引`[已查实]`,但部分索引能否同时是 LOCAL 分区索引需在目标版本试跑;若不支持,退化为普通 `(tenant_id, trace_id, event_type)` LOCAL 复合索引。

### 1.2 冷区列存 `span_events`(老分区转 CStore 后)

`[已查实]` 列存只支持 btree/psort(+ tsvector-GIN),btree 不支持表达式/部分索引。

```sql
-- 列存:PARTIAL CLUSTER KEY 让按 trace 聚簇,大幅提升子树范围扫描局部性
-- (建表时声明) ... PARTIAL CLUSTER KEY (tenant_id, trace_id)
-- 列存二级索引(等值/范围回查):
CREATE INDEX ix_cevt_trace ON span_events_cold (tenant_id, trace_id, span_id) LOCAL;  -- btree on CStore [已查实]
CREATE INDEX ix_cevt_thread ON span_events_cold (thread_id, ts) LOCAL;
-- 注意:冷区不要建 partial/expression 索引(列存不支持)[已查实]
```

### 1.3 折叠态 `span_current`(行存,核心查询面)

```sql
-- 主键点查
ALTER TABLE span_current ADD CONSTRAINT pk_span_current PRIMARY KEY (span_id);

-- 区间编码子树查询(冻结后物化的 pre/post/lvl):trace 内 pre 升序 = 先序遍历
-- 子树 = 同 root_id 且 pre 在 [node.pre, node.post] 之间
CREATE INDEX ix_cur_trace_pre  ON span_current (trace_id, pre);            -- 骨架要求①
CREATE INDEX ix_cur_subtree    ON span_current (root_id, pre, post);       -- 子树范围 + 找根
CREATE INDEX ix_cur_parent     ON span_current (parent_span_id);           -- 直接子节点
CREATE INDEX ix_cur_thread_ts  ON span_current (thread_id, start_time);    -- 线程重建/时间线  骨架要求①
CREATE INDEX ix_cur_tenant_st  ON span_current (tenant_id, start_time DESC);-- 租户最近 trace  骨架要求①
-- 活 trace(运行中):end_time 为空 = 未结束,部分索引(行存支持)
CREATE INDEX ix_cur_running    ON span_current (tenant_id, trace_id, start_time)
    WHERE end_time IS NULL;                                                -- [已查实] 行存部分索引
-- 错误 span 快速过滤(可观测高频)
CREATE INDEX ix_cur_status_err ON span_current (tenant_id, start_time)
    WHERE status = 'error';
```

### 1.4 JSONB `attrs` 的 GIN(只在行存 `span_current`)

`[已查实]` `jsonb_path_ops` 操作符类用于 `@>` 包含查询,比默认 `jsonb_ops` 更小更快(但不支持键存在 `?` 类查询)。`[已查实]` 行存 GIN 无限制。

```sql
-- 推荐:jsonb_path_ops(只支持 @>/@@/@? 路径包含,索引体积小)
CREATE INDEX ix_cur_attrs_gin ON span_current
    USING gin (attrs jsonb_path_ops);

-- 若需要键存在性查询(attrs ? 'key'),改用默认 jsonb_ops:
-- CREATE INDEX ix_cur_attrs_gin ON span_current USING gin (attrs);
```

> `[已查实]` 这条 GIN **不能搬到列存冷区**(列存 GIN 仅 tsvector)。冷区若要查 attrs,只能靠列存扫描 + `attrs @> '{...}'` 顺序过滤,或在冷区把高频路径预先提为真列(见 §⑤)。

---

## ② 大字段 payload

### 决策:**内容寻址 CAS 表(sha256 去重 + 引用计数)**,而非裸 TOAST 列

论证:

| 维度 | 裸 TOAST 列 | CAS 表(本设计选用) |
|---|---|---|
| 去重 | 无;同一 prompt 模板被 N 个 span 重复存 N 份 | sha256 去重,Agent 场景 system prompt / few-shot / 重复工具结果命中率极高 |
| 写放大 | 大;`span_events` append 每条都拖大字段进 TOAST | 事件表只存 32 字节 `payload_ref`,大字段单独写一次 |
| 折叠 | 折叠要搬运大字段 | 折叠只搬 ref,**晚物化天然成立** |
| 多模态 | bytea 塞库,膨胀 | ref token + 外置对象存储 URI,库里只留指针 |
| 成本 | 删 trace 时大字段随行删 | 引用计数=0 才回收,GC 可异步 |

> TOAST 仍然在用:CAS 表的 `content` 列本身是大文本/bytea,由 openGauss **TOAST 透明压缩+行外存储**`[已查实 PG/openGauss 通用机制]`。即"CAS 逻辑去重 + TOAST 物理大对象存储"两层叠加。

```sql
CREATE TABLE payload_store (
    sha256        bytea       NOT NULL,            -- 内容哈希,主键
    tenant_id     bigint      NOT NULL,
    media_type    text        NOT NULL,            -- 'text/plain','application/json','image/png','audio/wav'...
    encoding      text        DEFAULT 'utf8',
    byte_len      bigint      NOT NULL,
    refcount      bigint      NOT NULL DEFAULT 0,  -- 引用计数
    -- 文本/JSON 小于阈值内联;大对象走 TOAST;多模态走外置
    content       text,                            -- 文本类内联(TOAST 自动行外+压缩)
    content_bin   bytea,                           -- 二进制内联(可选)
    external_uri  text,                            -- 多模态/超大:对象存储 URI(s3://, obs://...)
    created_at    timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT pk_payload PRIMARY KEY (tenant_id, sha256)
);
-- [TODO待核实] PRIMARY KEY 列序:openGauss 行存 PK 即 btree,(tenant_id, sha256) 隔离租户去重
```

`span_events.payload_ref` / `span_current.input_ref` / `output_ref` 存 `sha256`(bytea)或 `'cas:'||hex(sha256)` 的 ref token。

**写入(去重 + 引用计数)**:

```sql
-- 写大字段:命中则只 +1 refcount,未命中才插入
INSERT INTO payload_store (sha256, tenant_id, media_type, byte_len, content, refcount)
VALUES (sha256($1::bytea), $2, $3, octet_length($1), $1, 1)
ON CONFLICT (tenant_id, sha256)
DO UPDATE SET refcount = payload_store.refcount + 1
RETURNING sha256;
-- [TODO待核实] openGauss 是否支持 INSERT ... ON CONFLICT DO UPDATE(UPSERT)。
--   openGauss 支持 INSERT ON DUPLICATE KEY UPDATE / ON CONFLICT(版本相关),
--   若不支持则用 advisory lock + SELECT 存在性判断 的应用层 UPSERT。
```

**多模态**:图/音频不进库,`media_type='image/png'`、`external_uri='obs://bucket/key'`、`content`/`content_bin` 留空。查询只取 `external_uri` 给前端按需拉取。

**晚物化查询(只拉 ref,点开才取)**:

```sql
-- 列表/树查询:绝不 JOIN payload_store,只回 ref
SELECT span_id, name, status, latency_ms, input_ref, output_ref
FROM   span_current
WHERE  trace_id = $1 ORDER BY pre;

-- 用户点开某 span 才物化:
SELECT media_type, encoding, content, content_bin, external_uri
FROM   payload_store
WHERE  tenant_id = $1 AND sha256 = $2;
```

**GC(引用计数归零回收)**:删 trace/分区时对涉及的 ref 批量 `refcount-1`,异步清理 `refcount=0` 行。

---

## ③ 向量:`span_vectors` + yiTrace 向量索引 + 带过滤 ANN

`[已查实]` yiTrace 向量类型为 **`floatvector(dim)`**(1~16384 维);距离函数 `l2_distance`/`cosine_distance`/`inner_product`;算子 `<=>`(余弦)、`<->`(L2);操作符类 `floatvector_cosine_ops`/`floatvector_l2_ops`。`[TODO待核实]` `<#>`(内积)算子符号与 `floatvector_ip_ops` 是否存在(文档只确认了 cosine/l2 算子)。

### 3.1 表 DDL(行存,带采样标记)

```sql
CREATE TABLE span_vectors (
    span_id        bigint        NOT NULL,
    tenant_id      bigint        NOT NULL,
    trace_id       bigint        NOT NULL,
    root_id        bigint        NOT NULL,
    span_kind      smallint      NOT NULL,        -- 提为标量过滤列(见 HybridANN)
    model          text,
    start_time     timestamptz   NOT NULL,
    is_sampled     boolean       NOT NULL DEFAULT true,
    embedding      floatvector(1024) NOT NULL,    -- [已查实] floatvector(dim);维度按嵌入模型定
    embed_source   text,                          -- 'input' | 'output' | 'name+input'
    CONSTRAINT pk_span_vectors PRIMARY KEY (span_id)
);
```

### 3.2 采样策略(只对 root / 关键 LLM span)

折叠写出 `span_current` 时,对满足条件的 span 置 `is_sampled_for_vector=true`,并异步算 embedding 写入 `span_vectors`:

- `span_kind IN ('llm','chain_root')` 或 `parent_span_id IS NULL`(root);
- 或 `status='error'`(便于"找相似失败");
- tool/retrieval 的中间 span 默认不采样,降索引规模。

```sql
UPDATE span_current SET is_sampled_for_vector = true
WHERE span_id = $1 AND (span_kind IN (1,2) OR parent_span_id IS NULL OR status = 'error');
```

### 3.3 向量索引 DDL

**普通 HNSW(纯语义召回,无强过滤)** `[已查实]` 语法形态:

```sql
CREATE INDEX ix_vec_hnsw ON span_vectors
    USING hnsw (embedding floatvector_cosine_ops)
    WITH (m = 16, ef_construction = 64, parallel_workers = 5);
```

**DiskANN(规模大、单盘、磁盘驻留)** `[TODO待核实]` DiskANN 具体 WITH 参数文档未给全,以下为推断骨架:

```sql
CREATE INDEX ix_vec_diskann ON span_vectors
    USING diskann (embedding floatvector_cosine_ops)
    WITH (/* [TODO待核实] DiskANN 参数,如 max_degree / l_value / 量化选项 */);
```

**HybridANN(本设计主推 —— 带标量过滤的 ANN)** `[已查实]`:把 `tenant_id/start_time/span_kind` 等过滤列**直接编进向量索引**,过滤下推到图遍历,避免"先 ANN 后过滤召回不足":

```sql
-- [已查实] USING hybridann(向量列 操作符类, 标量列1, 标量列2, ...);最多 31 个标量列
CREATE INDEX ix_vec_hybrid ON span_vectors
    USING hybridann (embedding floatvector_cosine_ops, tenant_id, span_kind, start_time)
    WITH (m = 16, ef_construction = 64, parallel_workers = 5);
```

> `[已查实]` HybridANN 限制:① 单索引≤31 标量列;② **不支持在空表上创建**(必须先灌数据再建索引,或建索引前确保非空);③ 标量字段的 `IS NULL/IS NOT NULL` 与 `ORDER BY 标量` 不走索引。

### 3.4 带过滤 ANN 查询(先标量裁剪域,再 ANN)

`[已查实]` 查询模板:`WHERE <标量条件> ORDER BY v <=> '[...]'::floatvector LIMIT k`,过滤随 HybridANN 下推。

```sql
-- 设检索深度(候选邻居数),越大召回越高越慢
SET hnsw_ef_search = 100;                       -- [已查实] GUC
SET hybrid_query_ivf_probes_factor = 3;         -- [已查实] 小选择率查询扩大搜索范围

-- 语义召回:租户 + 最近 7 天 + 只看 LLM span,再按余弦近邻
SELECT span_id, trace_id, model, start_time,
       embedding <=> $1::floatvector AS cos_dist          -- [已查实] <=> 余弦
FROM   span_vectors
WHERE  tenant_id = $2
  AND  span_kind = 1                                       -- llm
  AND  start_time >= now() - interval '7 days'             -- 标量裁剪,HybridANN 下推
ORDER  BY embedding <=> $1::floatvector
LIMIT  20;
```

> 若用普通 HNSW(非 hybrid),则 `WHERE` 过滤是 ANN 之后的后置过滤,需放大 `LIMIT`/`ef_search` 防召回不足;**所以本场景优先 HybridANN**。
> JSON 标量过滤要参与 ANN 裁剪时:把该 JSON 路径**提为真列**(见 §⑤)再放进 `hybridann(...)` 标量列,因为 HybridANN 标量列必须是表的真列,不能是 `attrs->>'x'`。

---

## ④ 中文全文:BM25 + Jieba(yiTrace)

`[已查实]` yiTrace BM25 通过 `USING fulltext(col)` 建索引,算子 `@~@`(BM25 评分)/`@-@`(关键词包含),排序函数 `bm25_score()`;参数 `DICTS`/`ALGORITHMS`(默认 BM25)/`COEFFICIENTS`(b=0.75,k=1.2)/`parallel_workers`;Jieba 经 `TEMPLATE = vex_jieba` 词典 + `vexjieba_add_userdict`/`vexjieba_reload`;**支持分区表(V3.0.0.1+ 默认 GLOBAL,V3.0.0 仅 LOCAL)**。

### 4.1 文本落地策略

BM25 索引要建在**文本列**上。但 input/output 大字段在 CAS 表里晚物化。两种摆法:

- **(推荐)** 在 `span_current` 上加可空提列 `input_text`/`output_text`,**只对采样/可检索的 span** 冗余一份(或截断前 N KB)文本,BM25 建在这两列;大全文仍在 CAS。
- 或直接在 `payload_store.content`(`media_type LIKE 'text/%'`)上建 BM25。

本设计用前者(检索面集中在 `span_current`,与向量/标量过滤同表,便于组合查询)。

### 4.2 Jieba 词典 + 自定义词

```sql
-- 建中文分词词典(模板 vex_jieba) [已查实]
CREATE TEXT SEARCH DICTIONARY cn_dict (
    TEMPLATE  = vex_jieba,
    stopwords = empty,
    userdict  = empty
);

-- 挂载业务自定义词(Agent/工具名/产品术语,可带词频) [已查实]
SELECT vexjieba_add_userdict('cn_dict',
    ARRAY['工具调用', '向量召回', '思维链', 'yiTrace,10000', 'openGauss,10000']);
SELECT vexjieba_reload('cn_dict');   -- [已查实] 必须 reload 才生效
```

### 4.3 BM25 索引 DDL

`[已查实]` `USING fulltext(col) WITH (DICTS=..., ALGORITHMS=BM25, COEFFICIENTS='b=0.75,k=1.2')`;分区表可 LOCAL/GLOBAL。

```sql
-- span_current 是普通行存表(非分区)→ 直接建
CREATE INDEX ix_cur_input_bm25 ON span_current
    USING fulltext (input_text)
    WITH (DICTS = 'cn_dict', ALGORITHMS = 'BM25', COEFFICIENTS = 'b=0.75,k=1.2', parallel_workers = 8);

CREATE INDEX ix_cur_output_bm25 ON span_current
    USING fulltext (output_text)
    WITH (DICTS = 'cn_dict', ALGORITHMS = 'BM25', COEFFICIENTS = 'b=0.75,k=1.2', parallel_workers = 8);

-- 若改为对 payload_store.content(文本类)建,且 payload_store 是分区表时: [已查实] LOCAL/GLOBAL
-- CREATE INDEX ix_payload_bm25 ON payload_store USING fulltext (content)
--     [LOCAL | GLOBAL] WITH (DICTS='cn_dict', ALGORITHMS='BM25');
```

> `[TODO待核实]` 多列联合分词时 `DICTS`/`COEFFICIENTS` 用 `#` 分隔多列的写法(文档提到多列用 `#` 分隔),单列场景无需。

### 4.4 检索 SQL

`[已查实]` `@~@` 评分匹配 + `bm25_score()` 排序;`@-@` 关键词包含;查询参数 `@<PARAM:BOOST=..>@`。

```sql
-- BM25 评分检索 + 排序 + 标量过滤组合
SELECT span_id, trace_id, name, start_time,
       bm25_score() AS score                         -- [已查实]
FROM   span_current
WHERE  tenant_id = $1
  AND  start_time >= now() - interval '30 days'
  AND  input_text @~@ '工具调用 失败 重试'              -- [已查实] @~@ BM25 评分查询(自动 Jieba 分词)
ORDER  BY score DESC
LIMIT  20;

-- 带 BOOST 的字段加权 [已查实]
SELECT span_id, bm25_score() AS score
FROM   span_current
WHERE  output_text @~@ '向量召回 @<PARAM:BOOST=1.5>@'
ORDER  BY score DESC LIMIT 20;

-- 仅判定是否包含关键词(不算分,更快) [已查实]
SELECT span_id FROM span_current WHERE input_text @-@ 'openGauss';
```

---

## ⑤ JSON 提列(schema-on-write):高频路径 → 真列 + 索引

原则:**列式聚合/排序/HybridANN 标量过滤只对真列承诺**;低频、长尾、调试性属性留 JSONB `attrs`。

### 5.1 提升为真列(在 `span_current`,折叠时写入)

```sql
ALTER TABLE span_current
    ADD COLUMN model          text,           -- attrs->>'llm.model'
    ADD COLUMN user_id        bigint,         -- attrs->>'user.id'
    ADD COLUMN session_id     bigint,         -- attrs->>'session.id'
    ADD COLUMN input_tokens   integer,
    ADD COLUMN output_tokens  integer,
    ADD COLUMN total_cost     numeric(12,6),
    ADD COLUMN latency_ms     integer,
    ADD COLUMN span_kind      smallint,
    ADD COLUMN status         smallint,        -- 0 running / 1 ok / 2 error
    ADD COLUMN input_text     text,            -- 供 BM25(可截断)
    ADD COLUMN output_text    text;
```

折叠时由 `attrs_patch` 深合并出 `attrs`,并把约定路径**写入真列**(schema-on-write),例:

```sql
-- 折叠 UPSERT 时(伪代码 SQL):从合并后的 attrs 抽路径填真列
UPDATE span_current SET
    model         = attrs->>'llm.model',
    user_id       = (attrs->>'user.id')::bigint,
    input_tokens  = (attrs->'usage'->>'input_tokens')::int,
    total_cost    = (attrs->'usage'->>'cost')::numeric
WHERE span_id = $1;
-- [已查实] jsonb_set 用于增量 patch 合并:attrs = jsonb_set(attrs, '{path}', $v, true)
--          深合并也可用 attrs = attrs || patch(浅合并)或 jsonb 递归合并函数
```

### 5.2 真列索引(行存 btree;聚合/过滤/排序)

```sql
CREATE INDEX ix_cur_model       ON span_current (tenant_id, model, start_time);
CREATE INDEX ix_cur_user        ON span_current (tenant_id, user_id, start_time);
CREATE INDEX ix_cur_session     ON span_current (session_id, start_time);
CREATE INDEX ix_cur_cost        ON span_current (tenant_id, total_cost DESC);   -- 高成本 trace 排行
CREATE INDEX ix_cur_latency     ON span_current (tenant_id, latency_ms DESC);
```

### 5.3 列式聚合(冷区列存 `span_current_cold` 镜像,只对真列承诺)

```sql
-- 例:按模型统计 token/成本(列存扫描,只碰真列,不碰 JSONB)
SELECT model,
       count(*)            AS spans,
       sum(input_tokens)   AS in_tok,
       sum(output_tokens)  AS out_tok,
       sum(total_cost)     AS cost,
       avg(latency_ms)     AS avg_lat
FROM   span_current_cold
WHERE  tenant_id = $1 AND start_time >= $2 AND start_time < $3
GROUP  BY model;
```

### 5.4 低频路径仍走 JSONB GIN(只行存)

```sql
-- 长尾属性按需查(行存 GIN, jsonb_path_ops)
SELECT span_id FROM span_current
WHERE tenant_id = $1
  AND attrs @> '{"experiment":{"variant":"B"}}';     -- 命中 ix_cur_attrs_gin
```

---

## 附:活 trace / 子树 / 找根 的关键查询(配合上面索引)

```sql
-- 活 trace(运行中,只有 start 没 end):从热区事件直接折叠当前态
-- 命中 ix_cur_running(行存部分索引)
SELECT * FROM span_current
WHERE tenant_id = $1 AND trace_id = $2 AND end_time IS NULL;

-- 子树查询(冻结后用区间编码,命中 ix_cur_subtree)
SELECT c.* FROM span_current n
JOIN span_current c
  ON c.root_id = n.root_id AND c.pre >= n.pre AND c.post <= n.post
WHERE n.span_id = $1
ORDER BY c.pre;            -- 先序 = 树形展开顺序

-- 找根:沿 parent 上溯成本高 → 直接用物化的 root_id 一跳到位
SELECT * FROM span_current WHERE span_id = (SELECT root_id FROM span_current WHERE span_id = $1);

-- 线程重建(命中 ix_cur_thread_ts)
SELECT * FROM span_current WHERE thread_id = $1 ORDER BY start_time;
```

---

## 待核实清单(TODO,需在目标 openGauss/yiTrace 实例 `\d` 或试跑确认)

1. **LOCAL + 部分索引(`WHERE`)** 能否在分区表 `span_events` 上共存(§1.1 `ix_evt_open`)。行存部分索引`[已查实]`,但与 LOCAL 组合的书写/支持待核实;不支持则退化为复合 btree。
2. **`INSERT ... ON CONFLICT DO UPDATE`(UPSERT)** 在目标版本的支持与语法(§② CAS 写入);不支持则用 advisory lock + 存在性判断。
3. **DiskANN 的 `WITH` 参数全集**(§3.3);文档只给出 HNSW/HybridANN 的 `m/ef_construction/parallel_workers`,DiskANN 具体参数未公开。
4. **内积算子 `<#>` 与 `floatvector_ip_ops`** 是否存在(文档只确认 `<=>` cosine / `<->` L2;有 `inner_product` 函数但算子符号未确认)。
5. **BM25 多列联合分词** 时 `DICTS`/`COEFFICIENTS` 用 `#` 分隔的精确写法(§4.3)。
6. **冷区 `span_events` 转 CStore 后**,原 UStore 上的 JSONB-GIN / 向量 / BM25 索引**必须重建在行存检索层**(`span_current` / `span_vectors`),不能随列存迁移;需确认转列存流程(`ALTER TABLE ... SET (orientation=column)` 是否支持原表转换,还是需 CTAS 镜像)。
7. **HybridANN 不支持空表建索引**`[已查实]`:上线流程要保证灌数后建索引,或对新分区/新租户的建索引时机做编排。

---

### 一句话总览

事件层只 append、靠 LOCAL btree 折叠;**所有"智能检索"(JSONB-GIN / 向量-HybridANN / BM25)集中在行存 `span_current`+`span_vectors`**,因为列存对它们只给 tsvector-GIN 一种;大字段走 **sha256 CAS 去重 + TOAST 物理存储 + 多模态外置 + 晚物化**;过滤 ANN 用 **yiTrace HybridANN 把 `tenant_id/span_kind/start_time` 编进向量索引**做下推;中文全文用 **`USING fulltext` + `vex_jieba` 词典 + `@~@`/`bm25_score()`**;高频 JSON 路径 schema-on-write 提为真列供列式聚合,低频留 JSONB。

**关键来源**:[openGauss Create Index](https://docs.opengauss.org/zh/docs/3.1.1/docs/Developerguide/CREATE-INDEX.html)、[openGauss Column Store](https://docs.opengauss.org/en/docs/5.1.0/docs/BriefTutorial/column-store.html)、[openGauss RANGE/INTERVAL 分区](https://docs.opengauss.org/en/docs/6.0.0-lite/docs/SQLReference/create-table-partition.html)、[yiTrace 全文索引](https://yitrace.com/docs/user-guide/fulltext-indexes)、[yiTrace HybridANN](https://yitrace.com/docs/user-guide/hybridann)、[yiTrace 向量类型/函数](https://yitrace.com/docs/user-guide/vector-types)、[openGauss DataVec 向量特性](https://blog.csdn.net/GaussDB/article/details/147562430)。