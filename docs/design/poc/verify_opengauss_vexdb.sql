-- =====================================================================
-- yiTrace Schema PoC 验证脚本  (openGauss-yiTrace)
-- 目的：在重建之前，用最小成本证伪/确认 schema 设计依赖的 9 类关键假设。
-- 跑法：  gsql -d <db> -p <port> -f verify_opengauss_yitrace.sql -L verify.log 2>&1
-- 读法：  每个测试块前有 [EXPECT] 注释，跑完对照输出；失败的项即 schema 需调整处。
-- 说明：  脚本对失败不中断(ON_ERROR_STOP off)，逐项独立、可重复跑(先 DROP)。
--         向量/全文语法已按 openGauss-vector-main 真实回归测试校准。
-- =====================================================================
\set ON_ERROR_STOP off
\timing on
\echo '==================== 环境信息 ===================='
SELECT version();
SHOW dbcompatibility;          -- A/PG 还是 B/Dolphin，影响 §A1 的 WHEN MATCHED ... WHERE
SELECT amname FROM pg_am ORDER BY amname;   -- 确认 hnsw/ivfflat/ivfpq/diskann/bm25/fulltext 等访问方法是否在册
\echo ''

-- =====================================================================
-- 区 A：SQL / 平台特性（折叠、分区、索引、聚合、排序）
-- =====================================================================

\echo '==================== A1. MERGE INTO (折叠核心) ===================='
-- [EXPECT] MERGE INTO ... WHEN MATCHED THEN UPDATE ... WHEN NOT MATCHED THEN INSERT 可执行；
--          重点验证 WHEN MATCHED 是否允许带条件(fold_version 单调守卫)。若不允许，把守卫挪进源 SELECT。
DROP TABLE IF EXISTS t_cur;
DROP TABLE IF EXISTS t_src;
CREATE TABLE t_cur (span_id bigint PRIMARY KEY, status smallint, fold_version bigint, attrs jsonb);
CREATE TABLE t_src (span_id bigint, status smallint, fold_version bigint, attrs jsonb);
INSERT INTO t_cur VALUES (1, 0, 10, '{"a":1}');
INSERT INTO t_src VALUES (1, 1, 20, '{"b":2}'), (2, 0, 5, '{"c":3}');
-- A1a: 基础 MERGE（无条件 UPDATE）
MERGE INTO t_cur c USING t_src s ON (c.span_id = s.span_id)
WHEN MATCHED THEN UPDATE SET status = GREATEST(c.status, s.status), fold_version = s.fold_version, attrs = c.attrs || s.attrs
WHEN NOT MATCHED THEN INSERT (span_id, status, fold_version, attrs) VALUES (s.span_id, s.status, s.fold_version, s.attrs);
SELECT 'A1a 基础MERGE', * FROM t_cur ORDER BY span_id;   -- [EXPECT] span1 status=1 fv=20 attrs={a,b}; span2 inserted
-- A1b: 带条件 UPDATE（fold_version 单调守卫）——可能需 dbcompatibility=B
INSERT INTO t_src VALUES (1, 2, 15, '{"d":4}');  -- 旧版本(15<20)不应覆盖
MERGE INTO t_cur c USING (SELECT span_id, max(fold_version) fv, max(status) st FROM t_src GROUP BY span_id) s
  ON (c.span_id = s.span_id)
WHEN MATCHED AND s.fv > c.fold_version THEN UPDATE SET fold_version = s.fv, status = s.st;
SELECT 'A1b 条件MERGE', * FROM t_cur WHERE span_id=1;     -- [EXPECT] 若支持 WHEN MATCHED AND，fv 仍=20(未被旧版覆盖)
\echo '   -> 若 A1b 报语法错: 改用 INSERT...ON DUPLICATE KEY UPDATE，或把 fold_version 守卫放进源子查询'
\echo ''

\echo '==================== A2. ON DUPLICATE KEY UPDATE + EXCLUDED (CAS去重备选) ===================='
-- [EXPECT] 支持(回归测试里确认)；用于 payload_store 去重 refcount+1
DROP TABLE IF EXISTS t_cas;
CREATE TABLE t_cas (sha bytea PRIMARY KEY, refcount bigint);
INSERT INTO t_cas VALUES (sha256('abc'), 1);
INSERT INTO t_cas VALUES (sha256('abc'), 1) ON DUPLICATE KEY UPDATE refcount = t_cas.refcount + 1;
SELECT 'A2 ON DUP', refcount FROM t_cas;                  -- [EXPECT] refcount=2
\echo ''

\echo '==================== A3. RANGE+INTERVAL 分区 (单 timestamptz 键) + ASTORE ===================='
-- [EXPECT] 自动按天建分区；INSERT 落到未来时间自动生 sys_pN；行存(ASTORE)默认
DROP TABLE IF EXISTS t_evt;
CREATE TABLE t_evt (event_id bigint, ts timestamptz NOT NULL, span_id bigint, CONSTRAINT pk_evt PRIMARY KEY (ts, event_id))
WITH (ORIENTATION = ROW)
PARTITION BY RANGE (ts) INTERVAL ('1 day')
( PARTITION p_init VALUES LESS THAN ('2026-06-01 00:00:00+08') );
INSERT INTO t_evt VALUES (1, '2026-06-10 09:00+08', 100), (2, '2026-06-11 10:00+08', 101);
SELECT 'A3 分区数', count(*) FROM pg_partition WHERE parentid = 't_evt'::regclass;  -- [EXPECT] >=2 自动分区
\echo ''

\echo '==================== A4. 分区表 LOCAL 二级索引 + LOCAL 部分索引 ===================='
-- [EXPECT] LOCAL 普通索引 OK(随分区滚动)；LOCAL + 部分(WHERE)能否共存 = 关键不确定项
CREATE INDEX ix_evt_span ON t_evt (span_id, ts) LOCAL;
\echo '   A4a LOCAL 普通索引: 上面无报错即 PASS'
-- A4b: LOCAL + 部分索引（活 trace 用）
CREATE INDEX ix_evt_partial ON t_evt (span_id) LOCAL WHERE span_id > 100;
\echo '   A4b LOCAL+部分索引: 上面无报错即 PASS；若报错 -> 退化为普通复合 LOCAL 索引'
\echo ''

\echo '==================== A5. 自定义有序聚合 (jsonb 深合并) + query_dop ===================='
-- [EXPECT] CREATE AGGREGATE 可用；折叠期 query_dop=1 保序
SET query_dop = 1;
CREATE OR REPLACE FUNCTION tv_jsonb_merge2(a jsonb, b jsonb) RETURNS jsonb
LANGUAGE plpgsql IMMUTABLE AS $$
BEGIN RETURN COALESCE(a,'{}'::jsonb) || COALESCE(b,'{}'::jsonb); END $$;  -- 浅合并占位(深合并见 schema 文档)
DROP AGGREGATE IF EXISTS tv_jsonb_merge_agg(jsonb);
CREATE AGGREGATE tv_jsonb_merge_agg(jsonb) ( SFUNC = tv_jsonb_merge2, STYPE = jsonb, INITCOND = '{}' );
SELECT 'A5 有序聚合', tv_jsonb_merge_agg(p ORDER BY o)
FROM (VALUES ('{"x":1}'::jsonb,1),('{"x":2,"y":3}'::jsonb,2)) v(p,o);  -- [EXPECT] {"x":2,"y":3}（后序覆盖）
\echo '   -> 若 ORDER BY 在聚合内不被尊重，折叠改在子查询里先 sort 再聚合'
\echo ''

\echo '==================== A6. JSONB GIN(jsonb_path_ops) + dotted_order C collation ===================='
DROP TABLE IF EXISTS t_attr;
CREATE TABLE t_attr (span_id bigint, attrs jsonb, dotted_order text);
CREATE INDEX ix_attr_gin ON t_attr USING gin (attrs jsonb_path_ops);  -- [EXPECT] 行存 GIN OK
\echo '   A6a jsonb_path_ops GIN: 无报错即 PASS'
-- 子树前缀匹配需要 dotted_order 字典序==时间序 => text_pattern_ops + C collation
CREATE INDEX ix_attr_dotted ON t_attr (dotted_order text_pattern_ops);
INSERT INTO t_attr VALUES (1,'{"k":1}','20260610T090000.000001Z-aaa'),(2,'{"k":2}','20260610T090000.000001Z-aaa.20260610T090100.000002Z-bbb');
SELECT 'A6b 前缀子树', span_id FROM t_attr WHERE dotted_order LIKE '20260610T090000.000001Z-aaa%' ORDER BY dotted_order;
SELECT 'A6c GIN包含', span_id FROM t_attr WHERE attrs @> '{"k":2}';   -- [EXPECT] 命中 span2
\echo ''

\echo '==================== A7. 多租户物理隔离 (tenant 进 PK / LIST 分区) ===================='
-- [EXPECT] 验证能否按 tenant LIST 分区(强隔离) 或 tenant 进 PK 前缀
DROP TABLE IF EXISTS t_tenant;
CREATE TABLE t_tenant (tenant_id int, span_id bigint, v text, CONSTRAINT pk_t PRIMARY KEY (tenant_id, span_id))
PARTITION BY LIST (tenant_id) ( PARTITION p_t1 VALUES (1), PARTITION p_t2 VALUES (2) );
INSERT INTO t_tenant VALUES (1,10,'a'),(2,20,'b');
SELECT 'A7 LIST分区', count(*) FROM t_tenant WHERE tenant_id = 1;  -- [EXPECT] 1；分区裁剪做到物理隔离
\echo '   -> 若不接受 LIST 分区，则靠 (tenant_id, ...) 复合索引 + 强制带 tenant_id 谓词(逻辑隔离)'
\echo ''

\echo '==================== A8. USTORE 表禁建 GIN/BM25 (确认 span_current 必须 ASTORE) ===================='
-- [EXPECT] 内核 indexcmds.cpp:783 硬禁止 USTORE 表建非 ubtree 索引。
--          A8a(USTORE+GIN) 应报错; A8b(ASTORE+GIN) 应成功 -> 证明检索表必须 ASTORE。
DROP TABLE IF EXISTS t_ustore;  DROP TABLE IF EXISTS t_astore;
CREATE TABLE t_ustore (id bigint, attrs jsonb) WITH (ORIENTATION=ROW, STORAGE_TYPE=USTORE);
CREATE INDEX ix_u_gin ON t_ustore USING gin (attrs jsonb_path_ops);
\echo '   A8a USTORE+GIN: 预期报错 "gin index is not supported for ustore"。若反而成功, 说明此版本无此限制(span_current 可保留 USTORE)'
CREATE TABLE t_astore (id bigint, attrs jsonb) WITH (ORIENTATION=ROW);   -- 默认 ASTORE
CREATE INDEX ix_a_gin ON t_astore USING gin (attrs jsonb_path_ops);
\echo '   A8b ASTORE+GIN: 预期成功 -> 确认 span_current 用 ASTORE'
\echo ''

-- =====================================================================
-- 区 B：向量（招牌钩子）—— floatvector / hnsw / diskann / 带过滤
-- =====================================================================
\echo '==================== B1. floatvector 类型 + 距离算子 <-> <=> <#> ===================='
DROP TABLE IF EXISTS t_vec;
CREATE TABLE t_vec (id bigint, embedding floatvector(4));   -- [EXPECT] floatvector(N) 类型存在
INSERT INTO t_vec VALUES (1,'[1,0,0,0]'),(2,'[0,1,0,0]'),(3,'[0.9,0.1,0,0]');
SELECT 'B1 L2 <->',  id, embedding <-> '[1,0,0,0]'::floatvector AS d FROM t_vec ORDER BY d LIMIT 2;
SELECT 'B1 cos <=>', id, embedding <=> '[1,0,0,0]'::floatvector AS d FROM t_vec ORDER BY d LIMIT 2;
SELECT 'B1 ip <#>',  id, embedding <#> '[1,0,0,0]'::floatvector AS d FROM t_vec ORDER BY d LIMIT 2;  -- [验] <#> 是否存在
\echo ''

\echo '==================== B2. HNSW / DiskANN 索引 (真实语法) ===================='
CREATE INDEX ix_vec_hnsw ON t_vec USING hnsw (embedding floatvector_l2_ops) WITH (m=16, ef_construction=64);
\echo '   B2a HNSW: 无报错即 PASS'
DROP INDEX IF EXISTS ix_vec_hnsw;
CREATE INDEX ix_vec_diskann ON t_vec USING diskann (embedding floatvector_l2_ops) WITH (parallel_workers=4, enable_quantization=true);
\echo '   B2b DiskANN: 无报错即 PASS（注：HybridANN/DiskANN 可能不支持空表建索引，已先灌数据）'
SET enable_seqscan TO off;
SELECT 'B2c diskann查询', id FROM t_vec ORDER BY embedding <-> '[1,0,0,0]'::floatvector LIMIT 2;
\echo ''

\echo '==================== B3. DiskANN 带过滤 ANN (inplace filter) —— 招牌钩子核心 ===================='
-- [EXPECT] 你们代码有 idx_diskann_inplace_filter ON items USING diskann (embedding, id)。
--          验证：复合索引建法 + 带标量过滤的 ORDER BY <-> 能否走索引且不 under-fill。
DROP TABLE IF EXISTS t_vecf;
CREATE TABLE t_vecf (id bigint, tenant_id int, span_kind smallint, embedding floatvector(4));
INSERT INTO t_vecf SELECT g, (g%3)+1, (g%2), ('['||(g%7)||',1,0,0]')::floatvector FROM generate_series(1,2000) g;
-- B3a: 复合 inplace-filter 索引（标量列编进图）
CREATE INDEX ix_vecf_filter ON t_vecf USING diskann (embedding, tenant_id, span_kind) WITH (parallel_workers=4);
\echo '   B3a 复合 diskann(embedding, scalar...) 建索引: 无报错即 PASS；若报错试 (embedding, id) 或 USING hybridann'
-- B3b: 带过滤召回（先标量裁剪域，再 ANN）
SET enable_seqscan TO off;
SELECT 'B3b 带过滤召回', id, tenant_id FROM t_vecf
WHERE tenant_id = 1 AND span_kind = 0
ORDER BY embedding <-> '[1,1,0,0]'::floatvector LIMIT 10;   -- [EXPECT] 返回 ~10 条且都满足过滤(不 under-fill)
EXPLAIN (COSTS OFF) SELECT id FROM t_vecf WHERE tenant_id=1 AND span_kind=0
ORDER BY embedding <-> '[1,1,0,0]'::floatvector LIMIT 10;   -- [EXPECT] 走 diskann 索引 + 过滤下推
\echo '   -> 看 EXPLAIN 是否 Index Scan(diskann) 且过滤被吸收；高选择度时若返回 < LIMIT, 即 under-fill, 需暴力兜底'
\echo ''

-- =====================================================================
-- 区 C：中文全文 —— vex_jieba 词典 + BM25 + @~@
-- =====================================================================
\echo '==================== C1. vex_jieba 中文词典 + 自定义词 + 分词 ===================='
DROP TEXT SEARCH DICTIONARY IF EXISTS cn_dict;
CREATE TEXT SEARCH DICTIONARY cn_dict (TEMPLATE = vex_jieba);   -- [EXPECT] vex_jieba 模板存在
SELECT vexjieba_add_userdict('cn_dict', ARRAY['工具调用','思维链','yiTrace,10000']);
SELECT vexjieba_reload('cn_dict');
SELECT 'C1 分词', bm25_tokenize('Agent 在工具调用时触发了思维链', 'cn_dict');  -- [EXPECT] 含"工具调用""思维链"整词
\echo ''

\echo '==================== C2. BM25 索引 + @~@ 检索 + bm25_score() ===================='
DROP TABLE IF EXISTS t_ft;
CREATE TABLE t_ft (span_id bigint, input_text text);
INSERT INTO t_ft VALUES (1,'用户请求工具调用但失败需要重试'),(2,'模型生成了向量召回结果'),(3,'思维链推理过程正常');
-- [实] BM25 的访问方法名是 fulltext(pg_am OID 4429); 算子 @~@ + bm25_score()
CREATE INDEX ix_ft_bm25 ON t_ft USING fulltext (input_text)
    WITH (DICTS='cn_dict', ALGORITHMS='BM25', COEFFICIENTS='b=0.75,k=1.2');
\echo '   C2a USING fulltext 建索引: 无报错即 PASS；WITH 键名若报错按本机文档调整(DICTS/ALGORITHMS/COEFFICIENTS)'
SELECT 'C2b 中文检索', span_id, input_text, bm25_score() AS score
FROM t_ft WHERE input_text @~@ '工具调用 失败 重试' ORDER BY score DESC NULLS LAST LIMIT 5;  -- [EXPECT] span1 居首
\echo ''

-- =====================================================================
-- 区 D：端到端最小折叠（事件乱序+晚到 → MERGE 折叠 → 子树查询）
-- =====================================================================
\echo '==================== D. 端到端：乱序/晚到事件 → 折叠 → 树查询 ===================='
DROP TABLE IF EXISTS d_events;
DROP TABLE IF EXISTS d_current;
CREATE TABLE d_events (event_id bigint, span_id bigint, trace_id bigint, seq int, event_type smallint,
                       ts timestamptz, parent_span_id bigint, name text, status smallint, attrs_patch jsonb);
CREATE TABLE d_current (span_id bigint PRIMARY KEY, trace_id bigint, parent_span_id bigint,
                        name text, status smallint, attrs jsonb, fold_version bigint);
-- 乱序灌入：子 span(102) 的事件先到，父(101)的 start 后到；span102 的 end 最后到(晚到)
INSERT INTO d_events VALUES
 (5, 102, 1, 1, 1, '2026-06-10 09:00:01+08', 101, 'tool', 0, '{"tool":"search"}'),  -- 102 start(父先于父start到)
 (3, 101, 1, 1, 1, '2026-06-10 09:00:00+08', NULL,'root', 0, '{"k":1}'),             -- 101 start(root)
 (9, 102, 1, 2, 3, '2026-06-10 09:05:00+08', 101, NULL,  1, '{"result":"ok"}'),      -- 102 end(晚到)
 (7, 101, 1, 2, 3, '2026-06-10 09:06:00+08', NULL, NULL, 1, '{"k":2}');              -- 101 end
SET query_dop = 1;
-- 折叠（MERGE INTO；attrs 用浅合并占位）
MERGE INTO d_current c USING (
  SELECT span_id, max(trace_id) trace_id,
         (array_agg(parent_span_id ORDER BY seq) FILTER (WHERE parent_span_id IS NOT NULL))[1] parent_span_id,
         max(name) FILTER (WHERE name IS NOT NULL) name,
         CASE WHEN bool_or(event_type=5) THEN 2 WHEN bool_or(event_type=3) THEN 1 ELSE 0 END status,
         tv_jsonb_merge_agg(attrs_patch ORDER BY seq) attrs,
         max(event_id) fv
  FROM d_events WHERE trace_id = 1 GROUP BY span_id
) s ON (c.span_id = s.span_id)
WHEN MATCHED THEN UPDATE SET status=GREATEST(c.status,s.status), attrs=c.attrs||s.attrs, fold_version=s.fv,
                             parent_span_id=COALESCE(s.parent_span_id,c.parent_span_id), name=COALESCE(s.name,c.name)
WHEN NOT MATCHED THEN INSERT (span_id,trace_id,parent_span_id,name,status,attrs,fold_version)
                      VALUES (s.span_id,s.trace_id,s.parent_span_id,s.name,s.status,s.attrs,s.fv);
SELECT 'D 折叠结果', span_id, parent_span_id, name, status, attrs, fold_version FROM d_current ORDER BY span_id;
-- [EXPECT] span101: parent=NULL,status=1,attrs={k:2}; span102: parent=101,status=1,attrs={tool,result}
-- 邻接树查询（区间编码留给应用层 DFS，这里验证邻接可重建）
WITH RECURSIVE tree AS (
  SELECT span_id, parent_span_id, name, 0 lvl FROM d_current WHERE parent_span_id IS NULL AND trace_id=1
  UNION ALL SELECT c.span_id, c.parent_span_id, c.name, t.lvl+1 FROM d_current c JOIN tree t ON c.parent_span_id=t.span_id
)
SELECT 'D 树重建', repeat('  ',lvl)||name AS tree_node, span_id FROM tree;  -- [EXPECT] root -> tool 两层
\echo ''
\echo '==================== 验证完成。对照每块 [EXPECT]，记录失败项回填 schema §12。 ===================='
