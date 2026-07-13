# BM25 与 attrs 磁盘分页结果

## 结论

10 万 span 的独立进程重开保持在毫秒级。BM25 和 attrs 不再在第一次查询时完整载入倒排；它们只读目录和当前查询命中的 postings，并分别受 64 MB 内存预算约束。

同一份确定性数据、release 构建下，改造前后结果如下：

| 指标 | 完成 BM25 分页后 | 再完成 attrs 分页后 |
|---|---:|---:|
| open + recover | 1.159 ms | 1.171 ms |
| attrs 首次加载 | 约 690 ms | 约 14 ms |
| 首个全文 + tenant 查询 | 918.336 ms | 233.689 ms |
| 全文查询 P50 | 132.252 ms | 135.219 ms |
| 全查询矩阵后 RSS | 874052 KiB | 514196 KiB |
| 每 span 磁盘占用 | 5898.7 B | 6154.8 B |

attrs 分页把首次加载和常驻内存降了下来，但没有改变全文查询稳定态耗时。高频查询 `任务执行` 会同时命中多个高频词，当前仍需读取这些词的完整 postings。下一步性能工作应是把 block 上界持久化，并实现磁盘 block-max-WAND。

## 百万级结果

第二轮使用固定 32 MB posting run 做外排，真实生成 100 万 span / 2277482 条 wire event，并在独立进程重开后执行完整查询矩阵：

| 指标 | 旧的完整 sidecar 首查 | 分页 + 有界外排 |
|---|---:|---:|
| open + recover | 约 1-2 ms | 2.086 ms |
| 首个高频全文查询 | 16659 ms | 999.098 ms |
| 查询矩阵后 RSS | 5890544 KiB | 2789880 KiB |
| 数据目录 | 约 5.92 GB | 6.17 GB |

新实现没有扫描 556 个历史 segment。BM25 目录和文档长度加载约 241 ms，attrs 行目录和 posting 目录加载约 109 ms；磁盘 attrs 保留了 13999000 条完整 postings，没有继承内存索引的宽字段禁用状态。

写路径仍需继续优化：100 万 span ingest 为 199.348 秒，显式 flush 为 312.728 秒，其中约 113 秒用于 segment 和各类 sidecar 落盘。外排期间 RSS 从约 6.99 GB 上升后稳定在约 7.38 GB，没有再随 posting 数量持续增长，但生成器和当前内存读模型本身仍占用较多内存。

最后把 BM25 文档长度查询从“每条 posting 二分查找”改为按已排序 doc 做线性合并。10 万 span 复测中，高频全文首查从约 231 ms 降到 213 ms，带 project 过滤从约 253 ms 降到 185 ms；结果排序继续通过 WAND 与暴力打分逐位对账。百万级报告采集于这项 CPU 优化之前，因此其中 999 ms 是保守值。

## 验证范围

- BM25 缓存：目录加载、按词读取、64 MB LRU、损坏/版本不匹配拒绝、WAL tail 合并、WAND 与暴力打分逐位一致。
- attrs 缓存：精确字段求交集、时间范围最终校验、禁用宽 postings 的正确回退、第一次写入完整物化、只推进 manifest 时原子更新缓存头。
- 引擎回归：147 个单元测试（146 通过、1 个 SIMD benchmark 按设计忽略）、6 个 eval、4 个真实多进程测试、10 个 risk eval 全部通过。
- 查询报告：[`2026-07-11_100k-spans_paged-bm25-attrs.md`](2026-07-11_100k-spans_paged-bm25-attrs.md)
- 建库报告：[`2026-07-11_100k-spans_paged-bm25-attrs_generate.md`](2026-07-11_100k-spans_paged-bm25-attrs_generate.md)
- 百万级查询报告：[`2026-07-11_1m-spans_bounded-paged-indexes.md`](2026-07-11_1m-spans_bounded-paged-indexes.md)
- 百万级建库报告：[`2026-07-11_1m-spans_bounded-paged-indexes_generate.md`](2026-07-11_1m-spans_bounded-paged-indexes_generate.md)
- 线性 doc-length 复测：[`2026-07-11_100k-spans_linear-doclen.md`](2026-07-11_100k-spans_linear-doclen.md)

## 兼容策略

`bm25.dat` v1 和 `filter_attrs.dat` v2 不会被新读取器误读。命中旧版本、缺失、截断或 manifest/watermark 不匹配时，引擎从当前快照重建派生缓存；WAL、segment 和 manifest 的主数据格式没有变化。
