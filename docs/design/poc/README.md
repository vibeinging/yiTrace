# yiTrace Schema PoC 验证清单

> 配套脚本 `verify_opengauss_yitrace.sql`。在一台 **openGauss-yiTrace 实例**上跑一遍，对照每项 `[EXPECT]` 记录结果，失败项按"失败回退"调整 schema（主文档 `../2026-06-16_tracevault-schema.md`）。
> 这是红队三轮反复强调的"重建之前先花小成本证伪关键假设"——跑通它，整个 schema 的不确定性基本清零。

## 跑法
```bash
gsql -d <db> -p <port> -f verify_opengauss_yitrace.sql -L verify.log 2>&1
# 看 verify.log，逐块对照 [EXPECT]
```

## 验证矩阵（跑完回填"实测"列）

| # | 测试块 | 验证的 schema 决策 | 期望 | 失败回退 | 实测 |
|---|--------|------------------|------|---------|------|
| A1 | MERGE INTO + WHEN MATCHED AND | **折叠核心**（红队致命点：无 ON CONFLICT） | MERGE 可执行；带条件 UPDATE 支持 | 守卫挪进源 SELECT，或用 `ON DUPLICATE KEY UPDATE` | ☐ |
| A2 | ON DUPLICATE KEY UPDATE + EXCLUDED | payload CAS 去重 refcount+1 | refcount=2 | 应用层 advisory lock + 存在性判断 | ☐ |
| A3 | RANGE+INTERVAL 分区 + ASTORE | 事件表自动按天分区滚动 | 自动生 ≥2 分区 | 改手工预建分区 + 定时建分区作业 | ☐ |
| A4 | LOCAL 二级索引 / LOCAL+部分索引 | DROP PARTITION 零成本回收 | LOCAL 普通索引 PASS；LOCAL+WHERE 待定 | 部分索引退化为普通复合 LOCAL 索引 | ☐ |
| A5 | 自定义有序聚合 + query_dop=1 | 折叠的 attrs 深合并保序 | 后序覆盖正确 | 折叠在子查询先 sort 再聚合 | ☐ |
| A6 | jsonb_path_ops GIN / dotted_order C collation | JSON 过滤 + 子树前缀匹配 | GIN+@> 命中；LIKE 前缀命中 | dotted_order 定宽 + C collation 重建 | ☐ |
| A7 | tenant LIST 分区 / tenant 进 PK | **多租户物理隔离**（红队指出当前仅逻辑） | LIST 分区裁剪 OK | 退逻辑隔离：强制带 tenant_id 谓词 + RLS | ☐ |
| B1 | floatvector + `<->`/`<=>`/`<#>` | 向量类型与距离算子 | L2/cos 通过；`<#>` 待定 | `<#>` 不存在则用 `inner_product()` 函数 | ☐ |
| B2 | USING hnsw / diskann | 向量索引建法 | 均无报错 | — | ☐ |
| **B3** | **DiskANN 带过滤 ANN（inplace filter）** | **招牌钩子：带过滤语义召回** | 复合索引建成；过滤召回不 under-fill；EXPLAIN 走索引+过滤下推 | 复合列序换 `(embedding, id)` 或 `USING hybridann`；高选择度加暴力精排兜底 | ☐ |
| C1 | vex_jieba 词典 + 自定义词 + bm25_tokenize | 中文分词 | "工具调用""思维链"整词切出 | 调词典/词频 | ☐ |
| **C2** | **BM25 索引 + `@~@` + bm25_score()** | **中文全文检索** | span1 居首 | 建索引语法 `USING bm25` 失败则试 `USING fulltext` | ☐ |
| D | 端到端：乱序+晚到事件 → MERGE 折叠 → 树重建 | **整条写路径 + 折叠正确性** | 折叠态正确(晚到 end 合并)；邻接树两层 | 按 A1/A5 结论调折叠 SQL | ☐ |

## 三个最关键、必须先过的项（其余可后补）
1. **A1 MERGE INTO**：整个折叠流水线的地基。回归测试已确认 MERGE 存在，本测试确认 `WHEN MATCHED AND <条件>` 是否支持（决定 fold_version 守卫怎么写）。
2. **B3 DiskANN 带过滤 ANN**：招牌差异化钩子。确认复合 inplace-filter 索引建法 + 过滤下推 + 不 under-fill。
3. **C2 BM25 建索引语法**：中文检索。查询算子 `@~@` 已确认，唯一未定是 `CREATE INDEX ... USING ?` 的精确写法（bm25 还是 fulltext）。

## 跑完之后
把"实测"列回填给我，我据此：① 修正 schema 文档里所有 `[验]` 项为 `[实]` 或调整写法；② 对失败项落地回退方案；③ 进入下一步（折叠/冻结/重融化的后台调度逻辑，或平台层模块）。
