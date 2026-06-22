-- ============================================================================
-- yiTrace 扩展 v1.0  (产物③:Agent trace 专用存储)
--   装法:CREATE EXTENSION tracevault;
--   底座:yiTrace / openGauss 内核(向量 floatvector/diskann/hnsw、BM25 fulltext/vex_jieba 已内置)
--   语法已按 yiTrace 真实写法校准;不确定处标 [验]——请配 docs/design/poc/verify_opengauss_yitrace.sql 上机验。
--   设计依据:docs/design/2026-06-16_tracevault-schema.md(已修 USTORE→ASTORE / ON CONFLICT→ON DUPLICATE 两 bug)
--             docs/design/2026-06-17_tracevault-background-scheduling.md(脏队列/折叠/冻结)
-- ============================================================================
\echo Use "CREATE EXTENSION tracevault" to load this file. \quit

-- 约定:id 一律 bigint(雪花,提交单调,非 SEQUENCE CACHE —— 见调度层正确性前置)
-- event_type: 1 START 2 UPDATE 3 END 4 TOOL_RESULT 5 ERROR 6 FEEDBACK
-- span_kind : 1 llm 2 chain 3 tool 4 retriever 5 embedding 6 agent 7 prompt
-- status    : 0 running 1 ok 2 error
-- encoding_state(span_current): 0 活/未编码  1 已物化区间  2 stale需重算  3 溢出走邻接

-- ============================================================================
-- 1. 核心表
-- ============================================================================

-- 1.1 span_events —— append-only 事件表(ASTORE 行存,RANGE+INTERVAL 按 ts 自动分区)
CREATE TABLE tracevault.span_events (
    event_id        bigint        NOT NULL,             -- 应用端雪花(全局提交单调)
    tenant_id       bigint        NOT NULL,
    trace_id        bigint        NOT NULL,
    span_id         bigint        NOT NULL,
    seq             int           NOT NULL DEFAULT 0,   -- 单 span 内事件序
    event_type      smallint      NOT NULL,
    ts              timestamptz   NOT NULL,             -- 分区键
    parent_span_id  bigint,                             -- 写侧邻接(可空/晚到)
    root_id         bigint,
    thread_id       bigint,
    span_kind       smallint,
    name            text,
    model           text,
    input_tokens    int,
    output_tokens   int,
    total_cost      numeric(18,6),
    latency_ms      int,
    status          smallint,
    start_time      timestamptz,
    end_time        timestamptz,
    attrs_patch     jsonb,                              -- 增量属性(折叠期深合并);超阈值走 payload CAS
    payload_ref     bytea,                              -- -> payload_store.sha256(大字段晚物化)
    ingest_ts       timestamptz   NOT NULL DEFAULT now(),
    CONSTRAINT pk_span_events PRIMARY KEY (ts, event_id) -- 分区本地唯一须含分区键
)
WITH (ORIENTATION = ROW)                                -- ASTORE:只 INSERT,undo 段无负担
PARTITION BY RANGE (ts)
INTERVAL ('1 day')                                      -- [验] 自动按天建分区(sys_pN)
( PARTITION p_init VALUES LESS THAN ('2026-01-01 00:00:00+08') );

-- 1.2 span_current —— 折叠后当前态(ASTORE 行存!USTORE 禁建 GIN/BM25,见 schema 红队)
CREATE TABLE tracevault.span_current (
    span_id         bigint        NOT NULL,
    tenant_id       bigint        NOT NULL,
    trace_id        bigint        NOT NULL,
    root_id         bigint,
    parent_span_id  bigint,
    pre             bigint, post bigint, lvl int,       -- 区间编码(冻结时应用层 DFS 物化)
    dotted_order    text,                               -- LangSmith 兼容(微秒+全id,定宽,C collation)
    thread_id       bigint,
    span_kind       smallint,
    name            text,
    start_time      timestamptz,
    end_time        timestamptz,                        -- NULL = 活/未结束
    status          smallint      NOT NULL DEFAULT 0,
    -- schema-on-write 提列(高频路径→真列,供列式聚合/带过滤 ANN 标量列)
    model           text,
    user_id         bigint,
    session_id      bigint,
    input_tokens    int,
    output_tokens   int,
    total_cost      numeric(18,6),
    latency_ms      int,
    tags            text[],
    attrs           jsonb,                              -- 深合并后全量属性
    input_text      text,                               -- 供 BM25(可截断)
    output_text     text,
    input_ref       bytea,
    output_ref      bytea,
    is_sampled_for_vector boolean NOT NULL DEFAULT false,
    encoding_state  smallint      NOT NULL DEFAULT 0,
    fold_version    bigint,                             -- 内容版本(折叠事件集),守卫用 <>
    frozen_at       timestamptz,
    CONSTRAINT pk_span_current PRIMARY KEY (span_id)    -- 仅此一个唯一约束(折叠冲突检测确定性)
) WITH (ORIENTATION = ROW);                            -- ASTORE,支持 simple_heap_update 原地折叠

-- 1.3 span_current_cold —— 冷区列存(只放低基数分析真列;检索/向量绝不进 CStore)
CREATE TABLE tracevault.span_current_cold (
    span_id bigint, tenant_id bigint, trace_id bigint, span_kind smallint,
    start_time timestamptz, end_time timestamptz, status smallint, model text,
    input_tokens int, output_tokens int, total_cost numeric(18,6), latency_ms int
)
WITH (ORIENTATION = COLUMN, PARTIAL CLUSTER KEY (tenant_id, trace_id))   -- [验] 列存聚簇
PARTITION BY RANGE (start_time)
INTERVAL ('1 month')                                    -- [验] CStore + INTERVAL;不支持则手工建月分区
( PARTITION pc_init VALUES LESS THAN ('2026-01-01') );

-- 1.4 span_vectors —— 采样 span 的 embedding(行存,带标量过滤列供 inplace-filter ANN)
CREATE TABLE tracevault.span_vectors (
    span_id     bigint PRIMARY KEY,
    tenant_id   bigint NOT NULL,
    trace_id    bigint NOT NULL,
    root_id     bigint,
    span_kind   smallint NOT NULL,
    model       text,
    start_time  timestamptz NOT NULL,
    status      smallint,
    is_sampled  boolean NOT NULL DEFAULT true,
    embedding   floatvector(1024) NOT NULL,             -- [验] floatvector(N);维度按 embedding 模型
    embed_source text
);

-- 1.5 payload_store —— 大字段 CAS(sha256 去重 + TOAST + 多模态外置 + 晚物化)
CREATE TABLE tracevault.payload_store (
    tenant_id   bigint NOT NULL,
    sha256      bytea  NOT NULL,
    media_type  text   NOT NULL,
    byte_len    bigint NOT NULL,
    refcount    bigint NOT NULL DEFAULT 0,
    content     text,                                   -- 文本内联(TOAST 自动压缩行外)
    external_uri text,                                  -- 多模态/超大:对象存储 URI
    last_ref_change timestamptz NOT NULL DEFAULT now(),
    created_at  timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT pk_payload PRIMARY KEY (tenant_id, sha256)
);

-- ============================================================================
-- 2. 控制 / 队列表(供后台维护:折叠/冻结/重融化/GC,见调度文档)
-- ============================================================================
CREATE TABLE tracevault.fold_dirty (                    -- 折叠脏队列(摄入同事务写入→事务不变量)
    tenant_id bigint NOT NULL, trace_id bigint NOT NULL, span_id bigint NOT NULL,
    enqueued_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, trace_id, span_id)
) WITH (ORIENTATION = ROW);
CREATE TABLE tracevault.frozen_registry (
    tenant_id bigint NOT NULL, trace_id bigint NOT NULL, frozen_at timestamptz NOT NULL DEFAULT now(),
    cold_partition text, PRIMARY KEY (tenant_id, trace_id)
);
CREATE TABLE tracevault.late_event_inbox (LIKE tracevault.span_events INCLUDING DEFAULTS);
CREATE TABLE tracevault.freeze_jobs (
    tenant_id bigint, trace_id bigint, step smallint NOT NULL DEFAULT 0,
    root_end_ts timestamptz NOT NULL, max_event_id_at_enqueue bigint,
    attempts int NOT NULL DEFAULT 0, last_error text, PRIMARY KEY (tenant_id, trace_id)
);
CREATE TABLE tracevault.remelt_jobs (
    tenant_id bigint, trace_id bigint, state smallint NOT NULL DEFAULT 0,
    rebuild_version bigint, attempts int NOT NULL DEFAULT 0, last_error text,
    PRIMARY KEY (tenant_id, trace_id)
);
CREATE TABLE tracevault.trace_retention (
    tenant_id bigint, trace_id bigint, has_error bool DEFAULT false, annotated bool DEFAULT false,
    in_dataset bool DEFAULT false, start_time timestamptz NOT NULL, expire_at timestamptz,
    PRIMARY KEY (tenant_id, trace_id)
);
CREATE TABLE tracevault.payload_ref_decrement (
    dec_id bigserial PRIMARY KEY, batch_id text NOT NULL, tenant_id bigint NOT NULL,
    sha256 bytea NOT NULL, delta int NOT NULL, applied bool NOT NULL DEFAULT false,
    UNIQUE (batch_id, tenant_id, sha256)
);
CREATE TABLE tracevault.tv_ctrl (                       -- 各后台环的水位/租约
    job_name text PRIMARY KEY, watermark jsonb NOT NULL DEFAULT '{}'::jsonb,
    last_ok_at timestamptz, consec_errors int NOT NULL DEFAULT 0
);
CREATE TABLE tracevault.job_dead_letter (
    job_type text, tenant_id bigint, trace_id bigint, reason text, dead_at timestamptz DEFAULT now()
);

-- ============================================================================
-- 3. 索引(全部行存;LOCAL 随分区滚动;检索集中在 span_current/span_vectors)
-- ============================================================================
CREATE INDEX ix_se_span   ON tracevault.span_events (span_id, seq, ts) LOCAL;
CREATE INDEX ix_se_trace  ON tracevault.span_events (tenant_id, trace_id, ts) LOCAL;
CREATE INDEX ix_se_parent ON tracevault.span_events (tenant_id, parent_span_id) LOCAL;
CREATE INDEX ix_se_ingest ON tracevault.span_events (ingest_ts) LOCAL;

CREATE INDEX ix_sc_subtree ON tracevault.span_current (tenant_id, trace_id, pre);
CREATE INDEX ix_sc_root    ON tracevault.span_current (tenant_id, trace_id) WHERE parent_span_id IS NULL;
CREATE INDEX ix_sc_parent  ON tracevault.span_current (parent_span_id);
CREATE INDEX ix_sc_thread  ON tracevault.span_current (tenant_id, thread_id, start_time);
CREATE INDEX ix_sc_running ON tracevault.span_current (tenant_id, trace_id) WHERE end_time IS NULL;
CREATE INDEX ix_sc_dotted  ON tracevault.span_current (tenant_id, dotted_order text_pattern_ops);
CREATE INDEX ix_sc_recent  ON tracevault.span_current (tenant_id, start_time DESC);
CREATE INDEX ix_sc_model   ON tracevault.span_current (tenant_id, model, start_time);
CREATE INDEX ix_sc_cost    ON tracevault.span_current (tenant_id, total_cost DESC);
CREATE INDEX ix_sc_attrs   ON tracevault.span_current USING gin (attrs jsonb_path_ops);

-- 向量:DiskANN 原生 inplace-filter(标量列编进图)。[验] 复合列序/WITH 参数
CREATE INDEX ix_vec_diskann ON tracevault.span_vectors
    USING diskann (embedding, tenant_id, span_kind) WITH (parallel_workers = 8, enable_quantization = true);
-- 备:纯语义无强过滤
-- CREATE INDEX ix_vec_hnsw ON tracevault.span_vectors USING hnsw (embedding floatvector_cosine_ops) WITH (m=32, ef_construction=200);

-- ============================================================================
-- 4. 中文分词词典(vex_jieba)+ BM25 全文索引
-- ============================================================================
-- [验] vex_jieba 模板 + 词典创建
CREATE TEXT SEARCH DICTIONARY tracevault.cn_dict (TEMPLATE = vex_jieba);
-- 领域词(工具名/模型名/术语);安装后也可再 vexjieba_add_userdict 增量
-- SELECT vexjieba_add_userdict('tracevault.cn_dict', ARRAY['工具调用','思维链','yiTrace,10000']);
-- SELECT vexjieba_reload('tracevault.cn_dict');

-- BM25 索引(AM 名 fulltext,算子 @~@,排序 bm25_score())。[验] WITH 参数精确写法
CREATE INDEX ix_sc_input_bm25  ON tracevault.span_current
    USING fulltext (input_text)  WITH (DICTS = 'tracevault.cn_dict', ALGORITHMS = 'BM25', COEFFICIENTS = 'b=0.75,k=1.2');
CREATE INDEX ix_sc_output_bm25 ON tracevault.span_current
    USING fulltext (output_text) WITH (DICTS = 'tracevault.cn_dict', ALGORITHMS = 'BM25', COEFFICIENTS = 'b=0.75,k=1.2');

-- ============================================================================
-- 5. 折叠辅助:jsonb 深合并函数 + 有序聚合
-- ============================================================================
CREATE OR REPLACE FUNCTION tracevault.jsonb_deep_merge_2(a jsonb, b jsonb)
RETURNS jsonb LANGUAGE plpgsql IMMUTABLE AS $$
DECLARE k text; v jsonb; r jsonb := COALESCE(a, '{}'::jsonb);
BEGIN
  IF b IS NULL THEN RETURN a; END IF;
  IF jsonb_typeof(a) IS DISTINCT FROM 'object' OR jsonb_typeof(b) IS DISTINCT FROM 'object' THEN
     RETURN b;                                          -- 非对象直接右覆盖
  END IF;
  FOR k, v IN SELECT * FROM jsonb_each(b) LOOP
     IF r ? k AND jsonb_typeof(r->k)='object' AND jsonb_typeof(v)='object' THEN
        r := jsonb_set(r, ARRAY[k], tracevault.jsonb_deep_merge_2(r->k, v));
     ELSE
        r := jsonb_set(r, ARRAY[k], v, true);
     END IF;
  END LOOP;
  RETURN r;
END $$;

-- 有序聚合:fold 时 tracevault.jsonb_deep_merge_agg(attrs_patch ORDER BY seq,ts,event_id)
CREATE AGGREGATE tracevault.jsonb_deep_merge_agg(jsonb) (
    SFUNC = tracevault.jsonb_deep_merge_2, STYPE = jsonb, INITCOND = '{}'
);

-- ============================================================================
-- 6. 折叠:事件 → span_current(供后台折叠环 / 活 trace 实时调用)
--    [验] openGauss INSERT ... ON DUPLICATE KEY UPDATE 在函数内的语义 + EXCLUDED 引用
-- ============================================================================
CREATE OR REPLACE FUNCTION tracevault.fold_trace(p_tenant bigint, p_trace bigint)
RETURNS int LANGUAGE plpgsql AS $$
DECLARE n int;
BEGIN
  SET LOCAL query_dop = 1;                              -- 保序:last-non-null / 深合并
  INSERT INTO tracevault.span_current AS sc
    (span_id, tenant_id, trace_id, parent_span_id, name, span_kind, model, thread_id,
     start_time, end_time, input_tokens, output_tokens, total_cost, latency_ms, status,
     attrs, root_id, fold_version, encoding_state)
  SELECT e.span_id, p_tenant, p_trace,
         (array_agg(e.parent_span_id ORDER BY e.seq, e.ts, e.event_id)
            FILTER (WHERE e.parent_span_id IS NOT NULL))[1],
         max(e.name)  FILTER (WHERE e.name IS NOT NULL),
         max(e.span_kind) FILTER (WHERE e.span_kind IS NOT NULL),
         max(e.model) FILTER (WHERE e.model IS NOT NULL),
         max(e.thread_id) FILTER (WHERE e.thread_id IS NOT NULL),
         min(e.start_time) FILTER (WHERE e.event_type = 1),
         max(e.end_time)   FILTER (WHERE e.event_type = 3),
         sum(e.input_tokens), sum(e.output_tokens), sum(e.total_cost),
         max(e.latency_ms) FILTER (WHERE e.event_type = 3),
         CASE WHEN bool_or(e.event_type=5) THEN 2 WHEN bool_or(e.event_type=3) THEN 1 ELSE 0 END,
         tracevault.jsonb_deep_merge_agg(e.attrs_patch ORDER BY e.seq, e.ts, e.event_id),
         coalesce((array_agg(e.root_id ORDER BY e.seq) FILTER (WHERE e.root_id IS NOT NULL))[1], p_trace),
         count(DISTINCT (e.span_id, e.seq)),            -- fold_version = 内容版本
         0
  FROM tracevault.span_events e
  WHERE e.tenant_id = p_tenant AND e.trace_id = p_trace
  GROUP BY e.span_id
  ON DUPLICATE KEY UPDATE                               -- [验] race-safe upsert(非 MERGE)
     parent_span_id = COALESCE(EXCLUDED.parent_span_id, sc.parent_span_id),
     end_time       = COALESCE(EXCLUDED.end_time, sc.end_time),
     status         = GREATEST(sc.status, EXCLUDED.status),
     input_tokens   = EXCLUDED.input_tokens,
     output_tokens  = EXCLUDED.output_tokens,
     total_cost     = EXCLUDED.total_cost,
     latency_ms     = COALESCE(EXCLUDED.latency_ms, sc.latency_ms),
     attrs          = tracevault.jsonb_deep_merge_2(sc.attrs, EXCLUDED.attrs),
     fold_version   = EXCLUDED.fold_version,
     encoding_state = CASE WHEN sc.encoding_state = 1 THEN 2 ELSE sc.encoding_state END;
  GET DIAGNOSTICS n = ROW_COUNT;
  DELETE FROM tracevault.fold_dirty WHERE tenant_id = p_tenant AND trace_id = p_trace;
  RETURN n;
END $$;

-- ============================================================================
-- 7. 对外 trace 函数(这个库的"语言")
-- ============================================================================

-- 7.1 整棵 trace 树(按区间编码先序;只回小列,大字段走 *_ref 晚物化)
CREATE OR REPLACE FUNCTION tracevault.load_trace_tree(p_tenant bigint, p_trace bigint)
RETURNS SETOF tracevault.span_current LANGUAGE sql STABLE AS $$
  SELECT * FROM tracevault.span_current
  WHERE tenant_id = p_tenant AND trace_id = p_trace
  ORDER BY pre NULLS LAST, start_time;
$$;

-- 7.2 子树(区间编码范围扫;晚到节点 encoding_state<>1 走邻接 CTE 兜底,此处给快路径)
CREATE OR REPLACE FUNCTION tracevault.subtree(p_tenant bigint, p_span bigint)
RETURNS SETOF tracevault.span_current LANGUAGE sql STABLE AS $$
  SELECT c.* FROM tracevault.span_current n
  JOIN tracevault.span_current c
    ON c.tenant_id = n.tenant_id AND c.trace_id = n.trace_id
   AND c.pre BETWEEN n.pre AND n.post AND c.encoding_state = 1
  WHERE n.span_id = p_span ORDER BY c.pre;
$$;

-- 7.3 线程重建(thread_id → 跨 trace 多轮会话时间线;只拉小列)
CREATE OR REPLACE FUNCTION tracevault.rebuild_thread(p_tenant bigint, p_thread bigint)
RETURNS TABLE(trace_id bigint, run_start timestamptz, run_end timestamptz,
              span_count bigint, title text, status smallint) LANGUAGE sql STABLE AS $$
  SELECT trace_id, min(start_time), max(end_time), count(*),
         (array_agg(name ORDER BY start_time) FILTER (WHERE parent_span_id IS NULL))[1],
         max(status)
  FROM tracevault.span_current
  WHERE tenant_id = p_tenant AND thread_id = p_thread
  GROUP BY trace_id ORDER BY 2;
$$;

-- 7.4 语义召回(招牌:带过滤 ANN)。文本→向量在 SDK/网关侧 embed 后传 query 向量进来。
--     [验] 距离算子 <=>(余弦);DiskANN inplace-filter 是否吃下 tenant/span_kind/start_time 过滤
CREATE OR REPLACE FUNCTION tracevault.semantic_recall(
    p_tenant bigint, p_query floatvector, p_k int DEFAULT 20,
    p_days int DEFAULT 90, p_span_kind smallint DEFAULT NULL)
RETURNS TABLE(span_id bigint, trace_id bigint, dist float4) LANGUAGE sql STABLE AS $$
  SELECT span_id, trace_id, (embedding <=> p_query)::float4 AS dist
  FROM tracevault.span_vectors
  WHERE tenant_id = p_tenant
    AND (p_span_kind IS NULL OR span_kind = p_span_kind)
    AND start_time >= now() - (p_days || ' days')::interval
  ORDER BY embedding <=> p_query
  LIMIT p_k;
$$;
-- 高选择度兜底(候选 < 阈值时暴力精排,recall=100%)由调用层/网关按 EXPLAIN 选择,见调度红队。

-- 7.5 轨迹导出(飞轮出口:trace → 训练/评测样本)。骨架,按 format 提取 messages / DPO 对。
CREATE OR REPLACE FUNCTION tracevault.export_trajectory(
    p_tenant bigint, p_trace bigint, p_format text DEFAULT 'messages')
RETURNS jsonb LANGUAGE plpgsql STABLE AS $$
DECLARE result jsonb;
BEGIN
  -- v1 骨架:按时序取 llm span 的 input/output 拼 messages;大字段需 JOIN payload_store 还原(此处略)
  SELECT jsonb_agg(jsonb_build_object(
            'role', CASE WHEN span_kind=1 THEN 'assistant' ELSE 'tool' END,
            'name', name, 'input', input_text, 'output', output_text,
            'tokens', jsonb_build_object('in', input_tokens, 'out', output_tokens)) ORDER BY pre)
    INTO result
  FROM tracevault.span_current
  WHERE tenant_id = p_tenant AND trace_id = p_trace AND span_kind IN (1,3);
  -- TODO: p_format='dpo' 时构造 (prompt, chosen, rejected);多模态引用还原;见飞轮设计
  RETURN COALESCE(result, '[]'::jsonb);
END $$;

-- 7.6 采样判定(折叠/采样器据此置 is_sampled_for_vector:root/llm/error)
CREATE OR REPLACE FUNCTION tracevault.should_sample(p_kind smallint, p_parent bigint, p_status smallint)
RETURNS boolean LANGUAGE sql IMMUTABLE AS $$
  SELECT p_status = 2 OR p_parent IS NULL OR p_kind = 1;   -- error / root / llm
$$;

-- ============================================================================
-- 8. 说明
--   · 本扩展 = 产物③(数据库本体的 trace 能力)。后台维护(折叠环/冻结/GC)由产物①二进制里的
--     维护守护进程驱动(调用 fold_trace 等);若 openGauss 暴露 bgworker API,可改为库内 worker([验])。
--   · 摄入(产物① 接收器 / 产物② SDK)在事务里:INSERT span_events + INSERT fold_dirty。
--   · 所有 [验] 项请用 docs/design/poc/verify_opengauss_yitrace.sql 在目标 yiTrace 实例上逐条验证后再定稿。
-- ============================================================================
