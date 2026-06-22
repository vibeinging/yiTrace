# 附录 K：内核修改边界查证（build-on-yiTrace 是否需要改内核）

## K1 yiTrace 打包形态：内核 fork vs 可加载扩展

This nails the red-team's core concern precisely. openGauss DOES have a `TableAmRoutine` struct, BUT it is a **closed/hardcoded enum dispatch, not a pluggable registration API**:

- `GetTableAmRoutine()` has `Assert(type == TAM_HEAP || type == TAM_USTORE)` and returns one of exactly two compile-time globals (`TableAmHeap`, `TableAmUstore`).
- `g_tableam_routines[]` is a fixed `const` array — there is no `RegisterTableAm()` / `CREATE ACCESS METHOD ... TYPE TABLE` / `amhandler` path.
- `pg_am.h` has **no `amtype` column** (the PG12 marker distinguishing index-AM vs table-AM). This confirms the red-team's claim: this kernel predates PG12's pluggable Table AM. Adding a third storage engine (e.g. a custom LSM segment) would require editing this enum + the const array + bootstrap catalogs = a kernel fork.

I now have complete, file-backed answers to all four questions plus the Table AM concern.

---

# yiTrace 打包/集成形态查实结论

## 总判定

**yiTrace 是"一个改过的 openGauss 发行版二进制"（modified openGauss distribution），不是"stock openGauss + 装扩展"。** 向量/全文/中文分词全部**编进内核二进制**（gaussdb），通过 bootstrap 系统目录（pg_am / pg_type / pg_proc 内置）注册，**没有任何 `.control` / `CREATE EXTENSION` / contrib 形式**。客户拿到的就是这个改过的二进制，initdb 后开箱即用、无需装扩展。

**对 yiTrace 的直接含义**：v1 schema 只用到的这些既有能力（diskann/hnsw/bm25/floatvector/vex_jieba）**已经在内核里、零额外内核改动即可用**。同时——回应第三轮红队——这个 PG 9.2.4 基线**确实没有 PG12 的可插拔 Table AM 注册接口**，所以"自研可注册 LSM 表存储引擎"那条路若走就是内核 fork；但 v1 已绕开它，**yiTrace v1 不需要再碰内核**（详见末尾 Table AM 专节）。

---

## ① hnsw / ivf / diskann / bm25 / vex_jieba：编进内核，不是可加载扩展

**证据 A — 在 `src/gausskernel/storage/access/` 内，列入内核构建的 SUBDIRS：**
`/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/gausskernel/storage/access/Makefile`
```
SUBDIRS = annvector cbtree common diskann heap hnsw index ivf nbtree ubtree psort rmgrdesc transam \
          obs hash spgist gist gin hbstore redo table ustore bm25 hybridann rabitq embedding index_backend
```
CMakeLists.txt 同样 `add_subdirectory(diskann/hnsw/ivf/bm25/...)`。每个目录的 Makefile 用 `include $(top_srcdir)/src/gausskernel/common.mk`（backend 对象收集，产出 `objfiles.txt` → 链入服务端二进制），**不是 `MODULE_big`/`.so` 扩展构建**。

**证据 B — 访问方法在 bootstrap 系统目录 `pg_am.h` 里用硬编码 OID 内置注册：**
`/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/include/catalog/pg_am.h`
```
OID 439  ivfflat   ... ivfflatinsert ivfflatbeginscan ... ivfflatbuild ...
OID 506  ivfpq     ... ivfpqinsert ...
OID 4471 diskann   ... diskanninsert ... diskannbuild ...
OID 494  hnsw      ... hnswinsert ... hnswbuild ...
OID 4387 graph_index ... (hnsw 别名)
OID 4429 fulltext  ... bm25insert bm25beginscan ... bm25build ...   (bm25 的 AM 名叫 fulltext)
```
对应的支持函数在内置函数表 `builtin_funcs.ini` 里用 `AddBuiltinFunc(...)`、`INTERNALlanguageId`、`PG_CATALOG_NAMESPACE`、`BOOTSTRAP_SUPERUSERID` 注册——即直接编进内核的 C 函数，不是从 `.so` 动态加载。

**证据 C — 测试/用例里直接 `USING hnsw/diskann/ivfflat`，全程没有 `CREATE EXTENSION`：**
- `src/test/regress/expected/hnsw.out`：`CREATE INDEX ON items_test_hnsw USING hnsw (embedding floatvector_l2_ops) ...`
- `src/test/regress/input/diskann.source`：`CREATE INDEX ... USING diskann (embedding floatvector_l2_ops)`
- 在 `src/test` 下搜索 `CREATE EXTENSION (vector|hnsw|bm25|diskann|floatvector|annvector)` → **0 命中**。

**证据 D — contrib/ 下没有任何向量扩展。** 全仓 55 个 `.control` 文件全是 stock openGauss contrib（postgres_fdw、pg_trgm、hstore、pgcrypto…）。按名/内容搜 `vector|hnsw|diskann|bm25|jieba|vex|floatvector` 的 `.control` → **0 命中**（唯一含 "vector" 字样的是 ndpplugin.control，无关）。

---

## ② floatvector：内核内置类型（非扩展类型）

`/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/include/catalog/pg_type.h`
```
OID 5036  floatvector  ... floatvector_in floatvector_out floatvector_recv floatvector_send floatvector_typmod_in floatvector_typmod_out ...
#define FLOATVECTOROID 5036
```
硬编码 OID 5036 写进 bootstrap 的 `pg_type.h`（扩展类型走 `CREATE TYPE` 拿动态 OID，绝不会进这里）。配套的操作符族/类（`floatvector_l2_ops`/`cosine_ops`/`ip_ops`）同样硬编码进 bootstrap 目录 `pg_opfamily.h`（OID 7816+）、`pg_amproc.h`、`pg_amop.data`、`pg_cast.h`。C 实现在 `src/include/access/annvector/floatvector.h` 等，随内核编译。测试里 `CREATE TABLE ... (embedding floatvector(128))` 不需任何扩展前置步骤。

---

## ③ vex_jieba / bm25_tokenize：内核内置（pg_proc 内置 + 内置 TS 模板），非扩展

**分词函数 = 内置 C 函数**（`builtin_funcs.ini`，`AddBuiltinFunc`，INTERNAL 语言）：
- `bm25_tokenize` OID 4528
- `vexjieba_add_stopwords` 4529、`vexjieba_add_userdict` 4530、`vexjieba_delete_stopwords` 4531/4426、`vexjieba_delete_userdict` 4532/4427、`vexjieba_reload` 4533、`vexjieba_add_synonyms` 8200
- `djieba_init` 3882、`djieba_lexize` 3883（`pg_proc.h_for_llt` 亦有）

**vex_jieba 词典"模板" = 内置 TS 模板**（bootstrap 目录，硬编码 OID）：
`/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/include/catalog/pg_ts_template.h`
```
OID 3884  "vex_jieba"  PGNSP djieba_init djieba_lexize
#define TSTemplateJiebaId 3884
DESCR("vex_jieba dictionary: tokenize chinese-english mixed ... with vex_jieba and then ... snowball")
```
默认 jieba 词典预置在 `pg_ts_dict.h`。分词器 C++ 源（含自带 cppjieba）在 `src/gausskernel/storage/access/bm25/tokenizer/`（`dict_jieba.cpp / tokenizer.cpp / dsnowball.cpp`），其 Makefile `OBJS = dict_jieba.o dsnowball.o token_pool.o tokenizer.o` + `common.mk` → 链入内核二进制，**不产出独立 `.so`**。

---

## ④ 交付形态：改过的 openGauss 发行版二进制

- `build.sh` 调 `package_opengauss.sh` 打包标准 openGauss 发行包（gaussdb 服务端 + om 工具），即 modified openGauss distribution。
- `yitrace-vector` 与 `openGauss-vector-main` 是同一内核代码库（两个版本/快照；`pg_am.h` 内容有差异但架构相同：yitrace-vector 的 access/Makefile 同样把 hnsw/ivf/diskann/bm25 列入 `SUBDIRS`）。两者都把向量/全文能力编进内核。
- 因此客户安装的是这套改过的二进制；initdb 时 bootstrap 目录已含上述类型/AM/函数/TS 模板，**开箱即用，无装扩展步骤**。

---

## Table AM 专节（直接回应第三轮红队）

红队质疑成立。该内核基线**没有 PG12 的可插拔 Table Access Method 注册接口**：

`/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/include/access/tableam.h`
```c
extern const TableAmRoutine * const g_tableam_routines[];   // 固定 const 数组
static inline const TableAmRoutine* GetTableAmRoutine(TableAmType type) {
    Assert(type == TAM_HEAP || type == TAM_USTORE);          // 只有两种，硬编码
    return type == TAM_HEAP ? TableAmHeap : TableAmUstore;
}
```
- openGauss 虽有 `TableAmRoutine` 结构，但分发是**闭合枚举**（`TAM_HEAP` / `TAM_USTORE` 二选一），不是开放注册。无 `RegisterTableAm()`、无 `CREATE ACCESS METHOD ... TYPE TABLE`、无 `amhandler` 表存储路径。
- `pg_am.h` **没有 `amtype` 列**（PG12 用来区分 index-AM / table-AM 的标志位，搜索 `amtype|amhandler` → 0 命中）。这正是"早于 PG12 pluggable Table AM"的实锤。
- 注意区分：上面 hnsw/diskann/bm25 是**索引访问方法（index AM）**，openGauss 9.2.x 本就支持把索引 AM 内置进 `pg_am`（厂商正是这么干的）；但这**不等于**支持**表访问方法（table AM）**的外部注册。
- 推论：要加第三种表存储引擎（如自研 LSM 段），必须改 `tableam.h` 的枚举 + `g_tableam_routines[]` + bootstrap 目录 = **内核 fork（重）**。

**而 yiTrace v1 已绕开它**：span_events 用既有 ASTORE 行存 + RANGE/INTERVAL 分区（标准 DDL）、折叠用 MERGE INTO（应用层/存储过程）、冷区用既有 CStore、语义/全文复用既有内核索引——**全部落在 v1 已确认的"标准 DDL + 扩展层 plpgsql/聚合 + 应用层后台服务"范围内，不触碰 table AM、不需要新内核改动**。

---

## 诚实标注（未亲自验证项）

- 我比对的是源码树中的 bootstrap 目录与构建脚本，**未实际编译/initdb 运行**确认二进制里这些 OID 真被装载；但 bootstrap 目录 + builtin_funcs.ini + Makefile/CMake SUBDIRS 三处一致，证据链充分，结论可靠。
- `openGauss-vector-main` 与 `yitrace-vector` 哪个是当前发版基线，我未从版本/分支元数据确认；两者均为"编进内核"架构，不改变本结论。
- `yitrace-doc` 未逐篇读取（问题已由源码层证据闭合）；若需要官方文档侧佐证可再查，但非必要。

**相关文件证据路径（均绝对路径）：**
- `/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/gausskernel/storage/access/Makefile`（SUBDIRS 含 hnsw/ivf/diskann/bm25）
- `/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/include/catalog/pg_am.h`（AM 内置 OID 439/506/4471/494/4387/4429）
- `/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/include/catalog/pg_type.h`（floatvector OID 5036）
- `/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/include/catalog/pg_ts_template.h`（vex_jieba 模板 OID 3884）
- `/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/common/backend/catalog/builtin_funcs.ini`（bm25_tokenize 4528、vexjieba_* 4529+、djieba_* 3882/3883 内置 C 函数）
- `/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/gausskernel/storage/access/bm25/tokenizer/Makefile`（分词器编进内核）
- `/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/include/access/tableam.h`（闭合 table AM 枚举，无可插拔注册）
- `/Users/Four/JobProjects/yitrace/openGauss-vector-main/build.sh`（package_opengauss.sh 打发行包）
---

## K2 yiTrace 各组件 内核/扩展/SQL/应用层 分类

All confirmed. The findings are complete and consistent:

1. **pg_am is the old PG 9.2 hardcoded catalog** — `CATALOG(pg_am,2601)` with fixed columns (`amname`, `aminsert`, ...) and **no `amtype`/`amhandler` column**. This is the pre-PG-9.6 catalog where index AMs are baked in.
2. **No `CREATE ACCESS METHOD` grammar exists** at all (grammar only has `CREATE OPERATOR FAMILY ... USING access_method`). So you cannot register a new table OR index AM via SQL.
3. **Table AM is a closed 2-entry enum** (`NUM_TABLE_AM=2`, hardcoded ternary, fixed routine array). Adding an LSM table engine = editing enum + array + catalog + reloptions = kernel C fork.
4. **Extension machinery + dynamic `.so` loading exists** (`extension.cpp` control-file parser, `internal_load_library`/`load_external_function` in `dfmgr.cpp`), plus `DefineAggregate`, plpgsql, GIN/jsonb_path_ops, MERGE/ON DUPLICATE KEY, INTERVAL partition, CStore — all present.

Here is my final report.

---

# yiTrace v1 schema 内核改动判定报告

## 结论先行

**v1 schema 中没有任何一项需要新内核 C 改动（0 个 K 级项）。** 全部落在 S（纯 DDL）/ E（扩展层）/ A（应用层）三档。既有的向量/BM25 内核改动是**已完成的存量产品**，v1 完全复用、不新增内核代码。第三轮红队对"自定义 Table AM 不存在该接口"的质疑——**经查实成立**（openGauss 的 Table AM 确实是封闭的 2 值枚举、不可注册），但**v1 已彻底绕开它**，所以这条质疑不构成 v1 的内核改动风险。

## 四级分类表

| # | 组件 | 分级 | 依据（已在内核源码核实 `[实]` / 设计文档 `[设]`） |
|---|------|------|------|
| 1 | span_events（RANGE+INTERVAL 分区 / ASTORE 行存） | **S** | INTERVAL 自动分区机制在内核已实现（`pruning.cpp` 等多处 `PARTTYPE_INTERVAL`）；ASTORE=默认 `TAM_HEAP`。纯 DDL。 |
| 2 | span_current（GIN jsonb_path_ops + 真列 btree） | **S**（GIN opclass 用既有的，算 S/E 边界） | `jsonb_path_ops` opclass 在 `pg_opclass.h` 已内置；建索引是 DDL。**不需要写新 opclass**，所以不碰内核。 |
| 3 | MERGE INTO 折叠（微批 upsert） | **S + A** | `MergeStmt`/`ExecMerge` 在内核已支持（`planner.cpp`/`tablecmds.cpp`）；`ON DUPLICATE KEY UPDATE` 也在 `gram.y`。SQL 是现成的；定时触发是应用层。 |
| 4 | pre/post 树编码（一次性 O(n) DFS + COPY 回写） | **A** | 纯内核外进程：读邻接表→内存 DFS→COPY 回写。不进内核。 |
| 5 | jsonb 深合并函数 + CREATE AGGREGATE | **E** | `DefineAggregate`（`aggregatecmds.cpp`）+ plpgsql（`src/common/pl/plpgsql`）均在内核已支持。**用 SQL/plpgsql 定义聚合 = 扩展层，不改 C 源码。** |
| 6 | 复用 diskann/hnsw 语义召回 | **S（复用既有 K）** | `storage/access/{diskann,hnsw,ivf}` 已编进内核（存量产品）。v1 只写 `CREATE INDEX ... USING diskann (...)` DDL，**零新内核代码**。 |
| 7 | 复用 bm25 + vex_jieba 中文全文 | **S（复用既有 K）** | `storage/access/bm25/` + `@~@` 算子 + `bm25_score()` 已在内核。v1 只写 `USING bm25` DDL + `vexjieba_add_userdict` 调用。零新内核代码。 |
| 8 | CAS payload_store（sha256 去重 + TOAST） | **S + A** | 建表 DDL + 写入用 MERGE INTO（refcount+1/插入）。TOAST 是内核既有能力。 |
| 9 | 冷区 span_current_cold（CStore 列存） | **S** | `storage/cstore/` 列存引擎内核已有；`ORIENTATION=COLUMN` + RANGE 分区是 DDL。 |
| 10 | frozen_registry / late_event_inbox | **S** | 普通行存表，`CREATE TABLE ... (LIKE span_events)`。纯 DDL。 |
| 11 | 冻结 / 重融化 / GC 服务 | **A** | 后台服务：重折叠 + 整分区重写 CStore（因列存无原地更新）+ 清 inbox。纯内核外。 |
| 12 | 活 trace 集合函数 | **E** | 参数化 `RETURNS TABLE` 的 plpgsql/SQL 函数（非裸视图）。扩展层。 |

> 注：第 6、7 项标"复用既有 K"——这里的 K 是**已交付的存量内核改动**，v1 不新增、不修改这些 C 代码，仅在 DDL/应用层调用它们。对"v1 是否引入新内核改动"这个判定问题，它们等价于 S。

## 三个核查点的实证答案

### (a) CREATE AGGREGATE / plpgsql / GIN opclass 是否算"扩展层不碰内核源码"？—— **是，算 E（或 S）**
- `DefineAggregate` 在 `src/gausskernel/optimizer/commands/aggregatecmds.cpp`、plpgsql 在 `src/common/pl/plpgsql` 均为内核**既有能力**。用 `CREATE AGGREGATE` / `CREATE FUNCTION ... LANGUAGE plpgsql` 定义新聚合/函数，是**通过 SQL 注册元数据 + 运行既有 plpgsql 解释器**，不编译/不链接任何新 C 源码 → 扩展层 E，不碰内核源码树。
- GIN `jsonb_path_ops` opclass 已内置于 `pg_opclass.h`，v1 直接 `USING gin(... jsonb_path_ops)` → 纯 DDL（S）。**前提**：v1 不自研新 opclass；若要写新 GIN opclass 的 C support function 才会变成 K——但 v1 没这需求。

### (b) 是否真的不需要自定义 Table AM？—— **不需要；且即使想要也做不到（坐实红队质疑）**
内核实证（`src/include/access/tupdesc.h`）：
```c
const int NUM_TABLE_AM = 2;
typedef enum tableAmType { TAM_INVALID = -1, TAM_HEAP = 0, TAM_USTORE = 1 } TableAmType;
```
```c
// tableam.cpp
const TableAmRoutine * const g_tableam_routines[] = { &g_heapam_methods, &g_ustoream_methods };
// tableam.h
static inline const TableAmRoutine* GetTableAmRoutine(TableAmType type) {
    Assert(type == TAM_HEAP || type == TAM_USTORE);
    return type == TAM_HEAP ? TableAmHeap : TableAmUstore;   // 硬编码三元，仅两种
}
```
- Table AM 是**封闭的 2 值枚举 + 固定 2 元路由数组 + 硬编码 ternary 分派**，AM 由 reloptions 在 `{ASTORE, USTORE}` 间选，无注册机制。
- `pg_am` 仍是 PG 9.2 老目录（`CATALOG(pg_am,2601)`，列为 `amname/aminsert/...`），**没有 `amtype`、没有 `amhandler` 列**——这是 PG 9.6 之前的形态，连"AM 用 handler 函数描述"都没有。
- 语法上**根本没有 `CREATE ACCESS METHOD` 语句**（`gram.y` 仅有 `CREATE OPERATOR FAMILY ... USING access_method`）。
- 结论：**红队质疑成立**——"自研 LSM 段作为可注册自定义 Table AM"在此基线上不存在该接口，强行实现 = 改 enum + 改 `g_tableam_routines` + 改 reloptions + 改 catalog = **重内核 fork**。而 **v1 用"ASTORE append-only 事件表 + MERGE 折叠物化表"完全绕开了 Table AM**，没有任何组件触碰它 → v1 不需要、也没引入自定义 Table AM。

### (c) MERGE INTO / ON DUPLICATE KEY / INTERVAL 分区 / LOCAL 部分索引 是否都是 openGauss 现成 SQL？—— **是**
- **MERGE INTO**：`MergeStmt` / `ExecMerge` 见 `planner.cpp`、`rewriteHandler.cpp`、`tablecmds.cpp`——内核现成。
- **ON DUPLICATE KEY UPDATE**：`gram.y` 中有（`UPSERT`/`DUPLICATE KEY` 解析），EXCLUDED 语义可用。（注意：openGauss **无 PG 的 `INSERT ... ON CONFLICT`**，这正是设计文档 `[实]` 标注、红队抓到三份初稿误用之处——v1 已改用 MERGE INTO，正确。）
- **INTERVAL 分区**：`PARTTYPE_INTERVAL` / `addNewIntervalPartition` 在 `pruning.cpp`、`vacuum.cpp` 等——内核现成，`PARTITION BY RANGE(ts) INTERVAL('1 day')` 可用。
- **LOCAL 分区局部索引**：分区表 `CREATE INDEX ... LOCAL` 是 openGauss 标准能力（向量索引建分区表也走 `local`，设计文档 §0 已注明）——现成 DDL。

### 标准扩展机制（CREATE EXTENSION / 动态库）—— **支持**
- `extension.cpp` 有完整 control-file 解析（`module_pathname`、control file 校验）；
- `dfmgr.cpp` 有 `internal_load_library` / `load_external_function`，即动态 `.so` 加载 + `MODULE_PATHNAME` 机制齐备。
- 即 openGauss 支持标准 `CREATE EXTENSION` + C 扩展动态库加载。**但 v1 不依赖这条**——v1 的扩展层只用 plpgsql/CREATE AGGREGATE（无需编译动态库），更轻。

## 一句话裁决

v1 schema **零新内核 C 改动**：检索能力全部复用已交付的存量内核（diskann/hnsw/bm25），其余靠 openGauss 现成的 MERGE INTO / INTERVAL 分区 / LOCAL 索引 / CStore（S）+ plpgsql 函数与 CREATE AGGREGATE（E）+ 折叠/树编码/冻结 GC 后台服务（A）。红队对 Table AM 不可注册的质疑经源码坐实为真，但 v1 的事件表+折叠物化架构已正确绕开它，因此该风险对 v1 不成立。

**相关文件路径**
- 内核 Table AM 封闭枚举：`/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/include/access/tupdesc.h`（`NUM_TABLE_AM=2`，第 31/38-43 行）
- 路由分派硬编码：`/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/include/access/tableam.h`（第 518-527 行）、`/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/gausskernel/storage/access/table/tableam.cpp`（第 1260-1266 行）
- pg_am 老目录（无 amtype/amhandler）：`/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/include/catalog/pg_am.h`
- 无 CREATE ACCESS METHOD 语法：`/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/common/backend/parser/gram.y`
- 扩展/动态库机制：`/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/gausskernel/optimizer/commands/extension.cpp`、`/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/common/backend/utils/fmgr/dfmgr.cpp`
- CREATE AGGREGATE：`/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/gausskernel/optimizer/commands/aggregatecmds.cpp`
- 既有向量/BM25 内核（存量）：`/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/gausskernel/storage/access/{hnsw,ivf,diskann,bm25}`
- yiTrace v1 设计文档：`/Users/Four/JobProjects/yitrace/vex-x/docs/design/2026-06-16_tracevault-schema.md`

**诚实标注**：以上均为对本地内核 fork 源码的静态核实，未在运行实例上 `\d`/试跑。设计文档 §12 自列的 `[验]` 清单（MERGE 的 `WHEN MATCHED ... WHERE` 子句、BM25 的精确 WITH 参数、DiskANN inplace-filter 列序、ASTORE+INTERVAL 组合、dbcompatibility 模式）属于"语义/参数细节需上线前在目标实例验证"，**但它们都不改变内核改动的判定**——这些都是现成 SQL 能力的用法确认，不涉及任何新 C 代码。
---

## K3 内核 vs 扩展 策略 + 信创认证影响 + 团队配置

I now have all the facts verified. Let me compose the final answer.

---

# 判定结论：yiTrace v1 不需要修改 openGauss 内核 C 代码

**一句话裁决**：v1 schema 的每一条原语都落在 yiTrace 现有内核**已编进的能力面**上（标准 DDL + 扩展层 plpgsql/AGGREGATE + 应用层服务），**不触碰、也不需要任何新内核 C 改动**。第三轮红队对 "Table AM 不存在 → 被迫 fork 内核" 的质疑是**对的，但它打的是一个 v1 已经弃用的设计（A强化版自研 LSM segment），不是 v1 schema**。下面给代码级查实、两条路取舍、信创认证影响、团队含义与触发信号。

---

## 一、代码级查实：v1 是否真的零内核改动

### 1.1 Table AM 质疑——查实结论：**红队的事实判断成立，但与 v1 无关**

我直接读了内核 fork 源码（`/Users/Four/JobProjects/yitrace/openGauss-vector-main`），结论是 openGauss 的 Table AM **确实是封闭的、不可第三方注册的**：

- 基线确认：`configure.in` 里 `PACKAGE_VERSION='9.2.4'`，Postgres-XC 血统，**早于 PG 12 的 pluggable Table AM**——红队这一点没错。
- openGauss 确有一层 `TableAmRoutine` 回调表（`src/include/access/tableam.h`、`src/gausskernel/storage/access/table/tableam.cpp`），但它是**为 Astore/Ustore 双引擎硬编码的闭集**，不是 PG12 那种开放注册接口。决定性证据：

```c
// src/include/access/tupdesc.h
typedef enum tableAmType {
    TAM_INVALID = -1, TAM_HEAP = 0, TAM_USTORE = 1,   // 只有两种，写死
} TableAmType;

// src/include/access/tableam.h —— 静态 if/else 派发，没有 pg_am handler 查找
static inline const TableAmRoutine* GetTableAmRoutine(TableAmType type) {
    Assert(type == TAM_HEAP || type == TAM_USTORE);
    return type == TAM_HEAP ? TableAmHeap : TableAmUstore;
}
```

- `g_tableam_routines[]` 只注册了 `g_heapam_methods` 与 `g_ustoream_methods` 两个编译期常量（`tableam.cpp:1260-1266`）。
- 语法层无 `CREATE ACCESS METHOD ... TYPE TABLE`：`gram.y` 里搜不到 `AMTYPE_TABLE`，现有的 `setAccessMethod` 走的是**索引/约束**的 access method（USING btree/hnsw/diskann/bm25），不是表存储 AM。

**含义**：任何"把自研 LSM/列存 segment 作为 openGauss 可识别的自定义表引擎插进去"的方案（即附录 G 的"A强化版"、附录 H 第④不确定项），都**必然是内核 fork 级重度改造**——红队判得对。但——

### 1.2 v1 schema **根本不走 Table AM 这条路**

读 v1 schema（`docs/design/2026-06-16_tracevault-schema.md`）与第四轮 schema 红队（`appendix-J`），v1 的每个原语都落在 openGauss **已有的内置能力**上，我逐项在内核源码里验了：

| v1 原语 | 依赖的内核能力 | 是否已在内核 / 查实 |
|---|---|---|
| `span_events` ASTORE 行存 + RANGE+INTERVAL 分区 | 内置行存 + 区间分区 | ✅ `gram.y` INTERVAL 分区语法在册（81 处匹配） |
| `span_current` 折叠态 + USTORE | 内置 Ustore（`TAM_USTORE`，**现成的那两个引擎之一**） | ✅ relcache 里 storage_type 路径在册 |
| 折叠用 `MERGE INTO` | 内置 MERGE（openGauss 无 ON CONFLICT，这正是 J 红队修正点） | ✅ `gram.y` MergeStmt 在册 |
| GIN(jsonb_path_ops) / btree 真列 / LOCAL 二级索引 | 内置索引 AM | ✅ |
| 自定义 jsonb 深合并 `tv_jsonb_deep_merge_*` + 有序 `CREATE AGGREGATE` | plpgsql + DefineAggregate（**扩展层，非内核 C**） | ✅ `gram.y` AGGREGATE 在册 |
| pre/post 树编码 | **应用层 O(n) DFS + COPY 回写**（不在库内） | ✅ 纯应用层 |
| 语义召回 `USING diskann/hnsw` | **既有向量索引 AM（已编进内核，既有产品）** | ✅ `storage/access/{diskann,hnsw,ivf}` |
| 中文全文 `USING bm25` + `@~@` + vex_jieba | **既有 BM25/分词（已编进内核，既有产品）** | ✅ `storage/access/bm25` |
| 冷区 CStore 列存 / payload CAS / frozen_registry / late_event_inbox | 内置列存 + 标准表 | ✅ `gram.y` ORIENTATION=COLUMN 在册 |
| 冻结/重融化/GC | **应用层后台服务** | ✅ 纯应用层 |

**关键区分**：向量索引和 BM25 是**索引访问方法（Index AM）**，openGauss 的 Index AM 是**开放的**（这正是 yiTrace 当初能把 hnsw/diskann/bm25 编进去的机制），而且**这部分内核工作已经完成、是既有产品**。v1 只是**复用**它们，不新增任何 Index AM，更不碰封闭的 Table AM。

> **结论**：v1 schema **完全不需要 Table AM，不需要任何新内核 C 改动**。它跑在"已发布的 yiTrace 内核二进制 + 纯 SQL DDL + plpgsql/聚合扩展 + 应用层服务"之上。红队的 Table AM 警告是对"自研 LSM 段"路线（v1 已主动绕开）的有效警告，不构成 v1 的内核改动需求。

---

## 二、两条路的取舍：v1 不动内核 vs 动内核

### A. 不动内核（纯 SQL+扩展+应用层，基于既有 yiTrace 内核）—— **v1 推荐**

| 维度 | 评估 |
|---|---|
| 工程量 | 轻：DDL + 几个 plpgsql/聚合 + 折叠/冻结/编码三个后台服务 |
| 风险面 | 低且可证伪：J 红队已把致命点（MERGE INTO 替 ON CONFLICT、LOCAL 索引、跨午夜分区折叠保序、冷区检索镜像、晚到回流）全部前移到 SQL/应用层，**全部可在不重编内核的前提下修复** |
| 性能上限 | 中高。瓶颈是 merge-on-read 折叠走 SQL（MERGE + 自定义聚合 + query_dop=1 保序），大 trace 退化到应用层 DFS。**对 <1亿 span/天单机中小规模够用**（附录 G/H 已论证） |
| 复用 | 最大化：优化器、向量化执行、HA 主备、WAL/恢复、向量索引、BM25 全部白嫖 |
| 认证 | **不改内核二进制 → 认证可大幅继承**（见三） |

### B. 动内核（kernel 级 merge-on-read 算子 / 轻量 LSM 段）

| 维度 | 评估 |
|---|---|
| 收益 | 折叠/merge-on-read 从 SQL 下沉为内核算子可降折叠延迟、省掉 query_dop=1 串行保序的吞吐损失；轻量 LSM 段能解决高频碎片化写（cstore delta 表碎片化、ASTORE 死元组）的理论上限 |
| 代价（重）| **三重**：① 封闭 Table AM → 想让段被优化器/向量化/HA 覆盖，**必须 fork 改内核**（1.1 已证）；② 改内核二进制 → **触发内核级重新测评**（见三）；③ 工程从"扩展"膨胀为"准自研数据库"，并行拖累认证长杆（附录 H 的 [HIGH] 双重自研负担） |
| 风险 | 高。把可证伪的关键假设（段能否被优化器/向量化/DataVec/HA 覆盖）推迟到内核工程做完才暴露——附录 H 标为最可能崩的技术地基 |

**取舍判据**：B 的性能收益**只有在 v1（A路线）PoC 实测打不到目标 SLA 时**才值得用"重新过内核测评 + 内核团队深度投入"去换。在那之前，B 是"为一个尚未被证明存在的瓶颈预付最重的成本"。

---

## 三、信创/安可认证影响（关键，决定团队与时间线）

这是不动内核最被低估的杠杆。基于联网查实的认证粒度口径：

### 3.1 核心机制：测评/认证按"变更范围"做回归，不是每次全量重测

- 信创软件产品认证的变更原则是**针对变更内容做有限回归**——已兼容部分不重测，仅新增/变更功能回归（[信创软件产品认证常见问题](https://www.sohu.com/a/989800106_100232921)、[软硬件更换影响的测试范围](https://blog.csdn.net/Super_TianLei/article/details/123008461)）。
- 适配/兼容性互认证是**分层的**：底层硬件 → 基础软件 → 应用软件全链路；拿到 CPU+OS+数据库底座的互认证后，新增应用软件**只需补做应用软件层的适配测试**（[信创适配全链路](https://blog.csdn.net/CQC_rjlhsys/article/details/148229449)、[兼容性互认证书区别](https://blog.csdn.net/iotintop2/article/details/144531178)）。

### 3.2 推论——**不改内核二进制 = 认证可大幅继承/复用**

> **若 yiTrace 主产品内核已过安可/信创测评，yiTrace 只加表/扩展/应用、不改内核二进制：**
> - **数据库底座栈（CPU+OS+yiTrace 内核）的互认证可直接复用**——同一二进制、同一适配清单，无需重跑底座栈测评。
> - 增量测评面收缩到**应用软件层**（yiTrace 平台/服务/扩展函数）+ 该应用对底座的适配验证。这是**轻量得多**的增量，而非内核级重测。
> - 复用维度：代码溯源、软件供应链安全(SCA)、开源风险评估在**内核部分可沿用主产品结论**，只对新增应用/扩展代码做增量。

### 3.3 反向——**改内核二进制 = 触发内核级重新测评（更重）**

- 安可测评的判定核心是"代码数据/研发环境/知识产权可控"，针对的是**送测的那个二进制**。一旦改内核 C（新存储引擎/算子），**送测客体变了，数据库底座栈要重新过内核级测评**——而安可测评结果**按批次公告、一年仅约 2 次**（附录 G/H 已查实），错过一窗 = 丢半年。这把"轻量增量"打回"重量长杆"。

### 3.4 一个必须诚实标注的前提（不能编造）

附录 G 自己标注的不确定项③仍然成立：**yiTrace 作为"基于 yiTrace/openGauss 内核的新产品"，大概率仍需独立走它自己那一份信创适配认证，不能 100% 自动套用主内核的在册资格**（参照 Vastbase/MogDB 基于 openGauss 仍各自做了产品认证——[海量数据](https://www.modb.pro/db/1849642597217820672)）。

> 但"独立走一份**应用软件层**适配认证"与"重过**内核级**测评"是两个数量级的事。**不动内核**把 yiTrace 的认证粒度锁死在前者（轻、可大幅继承底座结论）；**动内核**把它推到后者（重、可能回炉、卡批次窗口）。**这一条本身就足以把天平压向 v1 不动内核。**

精确报价/具体客户招标条款仍需销售/合规拿首单客户的采购需求书 + 向 CESI/测评机构书面确认（沿用附录 H 的待核实项），此处不编造数字。

---

## 四、团队配置含义

| | 不动内核（v1 推荐） | 动内核 |
|---|---|---|
| 主力 | **SQL/存储过程工程师**（折叠 MERGE、深合并聚合、索引）+ **应用服务团队**（折叠/冻结/重融化/GC/树编码 DFS 后台服务、摄入）+ **平台前端团队** | 上述全部 **+ 内核 C 团队深度投入**（改 Table AM/算子/WAL/恢复/向量化覆盖） |
| 内核团队角色 | **仅少量索引调优**（diskann inplace-filter 参数、bm25 WITH、向量化执行验证）——既有内核团队按需点状支持，非主线 | 主线、长期占用，且要同时背认证回炉风险 |
| 认证团队 | 增量：应用层适配认证，复用底座结论 | 重量：内核级重测 + 排批次窗口 |
| 关键路径 | 应用 + SQL 工程并行，认证长杆是日历瓶颈非产能瓶颈 | 内核工程进度 = 认证进度 = 首单进度，串成一根更长的杆 |

**含义**：v1 不动内核让项目变成"**SQL + 应用 + 前端**主导，内核团队解放出来继续打磨既有向量/BM25 索引"；这恰好把团队最稀缺、最贵的内核 C 产能从 yiTrace 上释放掉。

---

## 五、推荐与触发信号

### 推荐：**v1 走 A 路线——不动内核，纯 SQL DDL + 扩展(plpgsql/AGGREGATE) + 应用层服务，全部基于已发布的 yiTrace 内核二进制。**

四条理由收口：
1. **技术上零内核改动可达**——1.1/1.2 已逐项在源码验明，v1 不依赖封闭的 Table AM，只复用已编进内核的向量/BM25 索引与内置 MERGE/分区/列存。
2. **红队的 Table AM 雷被结构性绕开**——v1 用"事件表 + 读侧折叠"替代"自定义表引擎"，把红队点名的最可能崩的技术地基从关键路径上拿掉了。
3. **认证可大幅继承**——不改内核二进制 → 底座栈互认证复用、增量收缩到应用层，避开内核级重测与一年 2 次的批次窗口长杆。
4. **释放内核团队**——把最贵的 C 产能留给既有索引产品，yiTrace 由 SQL/应用/前端承载。

### 触发"动内核"的信号（呼应附录 H"自研 LSM 推迟到 PoC 不达标才考虑"）

只有以下信号**同时**出现，才重启 B 路线评估：

1. **PoC 实测打不到 SLA 且定位到根因在引擎层**：在目标负载（<1亿 span/天、单机 NVMe）下，merge-on-read 折叠延迟 / 高频碎片化写吞吐 / 跨午夜分区折叠保序的 query_dop=1 串行损失，经 EXPLAIN + benchmark 证明**是 SQL/应用层无法优化掉的内核级瓶颈**（而非索引参数、批次节奏、采样策略可解）。
2. **应用层所有低成本手段已穷尽**：微批节奏调整、活 trace 集合函数下推、应用层 DFS、CAS 去重、检索行存镜像等(J 红队的缓解项)都上了仍不达标。
3. **认证窗口允许回炉**：首单时间线能吸收"内核改动 → 重过内核级测评 → 排下一个批次公告窗口"的额外日历开销，或已有不需要信创的兜底客户群消化这段时间。
4. **拿到真实采购需求书**确认性能指标是硬准入而非叙事加分。

**在这四个信号集齐之前，自研 LSM 段 / kernel merge-on-read 算子应保持"推迟"状态**——正如附录 H 的正确顺序：先用零成本事实核实和 v1 PoC 暴露可证伪假设，不要用"不缺时间"麻醉自己提前付最重的内核成本。

---

## 附：关键文件路径（均为绝对路径）

- 内核基线 PG9.2.4 证据：`/Users/Four/JobProjects/yitrace/openGauss-vector-main/configure.in`
- Table AM 封闭性证据：`/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/include/access/tupdesc.h`（`TAM_HEAP/TAM_USTORE` 闭集）、`/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/include/access/tableam.h`（`GetTableAmRoutine` 静态派发）、`/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/gausskernel/storage/access/table/tableam.cpp`（`g_tableam_routines[]` 仅两项）
- 既有向量/BM25 Index AM（已编进内核、既有产品）：`/Users/Four/JobProjects/yitrace/openGauss-vector-main/src/gausskernel/storage/access/{hnsw,ivf,diskann,bm25}`
- v1 schema：`/Users/Four/JobProjects/yitrace/vex-x/docs/design/2026-06-16_tracevault-schema.md`
- 第三轮红队（Table AM 质疑出处）：`/Users/Four/JobProjects/yitrace/vex-x/docs/design/appendix-H_redteam-round3.md`
- 第四轮 schema 红队（确认 v1 全走标准 DDL/扩展）：`/Users/Four/JobProjects/yitrace/vex-x/docs/design/appendix-J_schema-redteam.md`
- 信创/安可认证路线图（含 G③ "新产品须独立适配认证" 不确定项）：`/Users/Four/JobProjects/yitrace/vex-x/docs/design/appendix-G_xinchuang-redesign.md`

## Sources（认证粒度查实）

- [信创软件产品认证测试常见问题：内容、周期和有效性](https://www.sohu.com/a/989800106_100232921)
- [信创项目硬件及软件更换影响的测试范围](https://blog.csdn.net/Super_TianLei/article/details/123008461)
- [信创适配证书、兼容性互认证书、信创产品认证证书有什么区别](https://blog.csdn.net/iotintop2/article/details/144531178)
- [软件企业必看：信创适配认证全流程解析](https://blog.csdn.net/CQC_rjlhsys/article/details/148229449)
- [信创入围厂商之海量数据（基于 openGauss 衍生产品仍各自认证）](https://www.modb.pro/db/1849642597217820672)
- [国产数据库 2025 国测解读及信创选型策略](https://blog.csdn.net/solihawk/article/details/150703108)