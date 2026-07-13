# yiTrace 百万 Span 压测计划

> 日期：2026-07-10
> 分支：trace-engineering
> 状态：第一版生成器、跨进程 reopen 查询和 read-plan 校验已实现；百万档保留为手动压测。

## 结论

百万 span 压测不需要运行百万次真实 LLM 对话。

正确方法是：

1. 用少量真实 trace 提取数据分布，不保留敏感正文。
2. 按分布生成可复现的合成 trace。
3. 用少量脱敏真实 trace 做放大回放。
4. 将数据库规模、检索正确性和 LLM/embedding 质量分开测试。

数据库压测关心的是数据大小、层级、字段分布、选择性、并发和生命周期，不关心这些文字是否真由 LLM 花钱生成。

## 已实现的压测能力

仓库现在支持可复现的 10k、100k 和 1m 档位：

```bash
./scripts/bench_scale.sh --smoke
./scripts/bench_scale.sh --medium --cold-queries
./scripts/bench_scale.sh --large --cold-queries --keep-data
```

`--large` 默认设置：

- 1,000,000 spans
- 请求每个查询端点 200 次；百万档对扫描型端点实际限制为 5 次，其余端点为 20 次
- release 模式
- durable engine 真写入、真 flush、真查询
- 固定 seed，生成结果可重复

`--cold-queries` 会拆成两个独立进程：第一个进程生成并 flush 数据，第二个进程重新打开同一个目录后执行查询。它能验证引擎 reopen/recover，不把 OS page cache 误称为磁盘冷读。

当前入口：

- `scripts/bench_scale.sh`
- `yitrace-engine/crates/yt-engine/examples/scale_bench.rs`
- `yitrace-engine/crates/yt-engine/examples/scale_bench/generator.rs`
- `yitrace-engine/crates/yt-engine/examples/scale_bench/queries.rs`

当前生成器已经覆盖：

- 多 span trace 和二叉父子关系。
- start、log、end 多事件折叠，以及 1% 重复 event 和 0.1% 未完成 span。
- agent、tool、model、project、skill、mode、task、loop、call_site 等字段分布。
- 常见关键词、稀有关键词、失败路径、中文文本和长尾文本。
- 低/中/高选择性过滤、BM25、aggregate、trajectory、loop、task、session、trace detail 和 diff 查询。
- 查询返回内容校验和 `readPlan`/索引命中校验。

它可以支撑数据库规模和查询路径的百万档基线，但不能代替真实 LLM 质量评测，也不能直接推出生产容量承诺。

## 已验证结果

在本机 release 模式执行 100k、固定 seed、batch=512 的跨进程 reopen 压测，第一版基线为：

- 100,000 folded spans，227,735 wire events，9,862 traces。
- 数据目录约 589 MB，约 5.9 KB/folded span。
- 写入约 4,652 spans/s，写入后 RSS 约 756 MiB。
- reopen/recover 约 3.7 秒，恢复后 RSS 约 1.19 GiB，查询后约 1.53 GiB。
- 所有查询项 `0/0` 错误；rollup、attrs filter index、aggregate rollup、trajectory rollup 的计划证据全部命中。

这些数字是当前机器和当前数据形状的基线，不是跨机器的 SLA。百万档预计会接近数 GB 数据和更高 RSS，应放在手动或定时任务中运行。

## 优化后 100k 结果

同一台机器、同一 seed 和数据形状再次执行 `--medium --cold-queries`，加入候选行读取、文本 BM25 top-k、BM25 top-k 内存保护和 sessions rollup/cache 后：

- reopen/recover 约 3.7 秒，查询后 RSS 约 1.36 GiB，比第一版查询后的约 1.53 GiB 下降约 11%。
- `trace_detail` P50 约 40 ms，第一版约 2.19 s；点查不再把每个 segment 完整折叠后才取目标 span。
- `trace_search_text_tenant_index` P50 约 313 ms，第一版约 0.93 s；文本查询先走 BM25 候选，再做最终校验。
- `sessions_page_index` 热查询 P50 约 9 ms；rollup 负责聚合，重复请求命中内存 cache，写入和恢复会主动失效 cache。
- 所有查询项 `0/0` 错误，`risk_eval_matrix` 10/10、功能 eval 6/6、多进程 eval 4/4、全 workspace `cargo test --offline` 通过。

完整查询结果见 [`2026-07-10_100k-spans_optimized.md`](../reports/scale/2026-07-10_100k-spans_optimized.md)，生成阶段见 [`2026-07-10_100k-spans_optimized_generate.md`](../reports/scale/2026-07-10_100k-spans_optimized_generate.md)。

这一阶段还不能把 100k 的数字直接当成百万级容量承诺。后续章节记录了 1m 实测，以及 BM25/attrs postings 从整块内存结构升级为按需磁盘读取的结果。带 text 的分页仍需补全全库精确计数与稳定 cursor 语义。

## 实测 1m 结果

随后在同一台机器、同一 seed、release、跨进程 reopen 模式下完成了 1,000,000 spans 压测：

- 1,000,000 folded spans、2,277,482 wire events，数据目录约 5.92 GB，共 556 个 segment。
- 写入约 4554 spans/s；写入阶段 RSS 约 7.5 GiB。
- reopen/recover 约 47.5 秒；命中四份持久读模型缓存，`segs_scanned=0`，但加载百万行 rollup、attrs 和全文索引仍需要时间和内存。
- reopen 后 RSS 约 9.16 GiB，查询后约 9.78 GiB。
- 点查 `trace_detail` P50 约 76 ms；文本 + 租户过滤 P50 约 360 ms；sessions 热查询 P50 约 13 ms。
- aggregate、storage stats、trajectory group 等需要遍历百万条小字段的接口，P50 约 1.2 到 1.7 秒；这已经是当前百万档的主要读瓶颈。
- 全部查询 `0/0` 错误，所有已声明的 read-plan 证据命中。

完整报告见 [`2026-07-10_1m-spans_optimized.md`](../reports/scale/2026-07-10_1m-spans_optimized.md) 和 [`2026-07-10_1m-spans_optimized_generate.md`](../reports/scale/2026-07-10_1m-spans_optimized_generate.md)。

### 恢复优化后二次实测

定位到启动慢不只是 sidecar：旧实现打开时全量解析约 2 GB WAL，恢复末尾又遍历 227 万条历史事件确认没有 tail；attrs 解码还会克隆百万行重建 postings，BM25 同时保存 HashMap 和排序 Vec 两份 postings。优化后：

- flush 后写入带 CRC 的 `wal.state` checkpoint，记录已持久化 LSN 和 WAL 文件偏移；重启只读取 checkpoint 后的尾部。checkpoint 缺失、损坏或越界时自动回退 WAL 正文全量校验。
- watermark 已等于 WAL tail 时直接跳过 replay，不再遍历历史 WAL。
- attrs 解码时直接建立预算内 postings，不再克隆全部 row；rollup 解码时同时建立 trace 目录。
- BM25 的持久主索引只保留一份排序 postings，WAL tail 作为增量合并；不再同时常驻两份 doc/tf。
- rollup/attrs 与 BM25/bloom 分成两条受控加载流水线，缩短串行等待，同时限制并发解码数量。

同一台机器、同一 seed 和数据形状的二次结果：

- 100k reopen/recover：约 3.74 s 降到 1.08 s；打开后 RSS 约 1.21 GiB 降到 0.61 GiB。
- 1m reopen/recover：约 47.47 s 降到 8.74 s，缩短约 81.6%，约 5.4 倍。
- 1m 打开后 RSS：约 9.16 GiB 降到 4.70 GiB；查询后约 9.77 GiB 降到 6.00 GiB。
- 1m 点查、文本过滤和 sessions 延迟保持原水平，全部查询仍为 `0/0` 错误，read-plan 证据全部命中。

二次报告见 [`2026-07-10_1m-spans_recovery-optimized.md`](../reports/scale/2026-07-10_1m-spans_recovery-optimized.md)、[`2026-07-10_1m-spans_recovery-optimized_generate.md`](../reports/scale/2026-07-10_1m-spans_recovery-optimized_generate.md) 和 [`2026-07-10_100k-spans_recovery-optimized.md`](../reports/scale/2026-07-10_100k-spans_recovery-optimized.md)。

### 毫秒级 clean reopen 实测

8.74 秒仍然来自启动时全量反序列化四份 sidecar，和 PostgreSQL 的固定预算、按页读取思路不一致。第三轮把“数据库可用”和“读模型已预热”拆开：

- clean reopen 只读取 manifest、`wal.state` 和 WAL tail，不解码 `trace_rollup.dat`、`filter_attrs.dat`、`bm25.dat`、`segment_bloom.dat`。
- rollup、attrs、BM25/bloom 三组状态独立；第一次相关查询只加载自己需要的组，并发首次查询由同一状态机串行化。
- 第一次写入前统一补齐历史读模型，再叠加新事件；跨进程 WAL tail 也走同一规则，避免新数据被后加载的缓存覆盖。
- 中文全量词典改为第一次真实分词时初始化，`open_durable()` 不再为未发生的全文查询解析 34.9 万词。
- 进程锁 owner 和 reader pin 是活进程协调文件，不是数据库数据，取消无意义的 fsync；WAL、manifest、segment 的 fsync 不变。

同一 seed、同一数据形状的结果：

- 100k、约 566 MB：5 次 `open_durable` 为 0.54–0.62 ms，`open + recover` 约 1 ms，启动 RSS 约 3.8 MB。
- 1m、5.92 GB、556 segments、227.7 万 WAL 事件：5 次 `open_durable` 为 0.63–0.94 ms，`open + recover` 约 1–2 ms，启动 RSS 约 4.0 MB。
- 1m 查询 smoke 覆盖全部 15 类端点，全部 `0/0` 错误，read-plan 证据通过。
- 代价没有隐藏：第一次全文+tenant 过滤查询需要加载 BM25 和 attrs，实测约 16.7 秒；加载后查询语义和原有路径一致。

压测新增 `--phase open`，只测独立进程 open/recover，不运行查询；`eval_all.sh --scale` 默认以 100 ms 作为 CI 防回退门槛，可用 `YT_SCALE_OPEN_MAX_MS` 调整。

第三轮报告见 [`2026-07-10_100k-spans_millisecond-open.md`](../reports/scale/2026-07-10_100k-spans_millisecond-open.md)、[`2026-07-10_1m-spans_millisecond-open.md`](../reports/scale/2026-07-10_1m-spans_millisecond-open.md)、[`2026-07-10_1m-spans_millisecond-open_generate.md`](../reports/scale/2026-07-10_1m-spans_millisecond-open_generate.md) 和 [`2026-07-10_1m-spans_lazy-query-smoke.md`](../reports/scale/2026-07-10_1m-spans_lazy-query-smoke.md)。

### 磁盘按需读取后的 1m 结果

第四轮把 attrs 和 BM25 改成目录先读、postings 按命中字段或词读取，并分别使用默认 64 MB 的缓存。attrs flush 用固定 32 MB run 外排和多路归并，磁盘态保留完整 postings，不会因为内存预算丢失超宽值。

- 1,000,000 spans、2,277,482 events，数据目录约 6.17 GB。
- `open + recover` 约 2.086 ms，启动 RSS 约 4 MB。
- 首个高频全文查询约 999 ms；完整查询矩阵后 RSS 约 2.66 GB，低于整块加载版本。
- attrs 磁盘态生成 13,999,000 条完整 postings；百万级 tenant/project 精确过滤命中索引，没有回退全 segment 扫描。
- 所有功能 eval、风险矩阵和真实多进程测试通过。

完整报告见 [`2026-07-11_paged-read-model-results.md`](../reports/scale/2026-07-11_paged-read-model-results.md)。

### 对 1m 的判断

当前版本可以承载百万级 trace 数据，clean reopen 已不再随数据量线性变慢，attrs 和 BM25 也不再整块加载。剩余主要瓶颈是 rollup 仍按整份读取，以及高频词查询仍需读取该词的完整 postings。

下一阶段应优先做两件事：

1. 给 rollup 做按维度、租户和时间分区的物化聚合，避免每次从百万行重新 group。
2. 给 BM25 落盘块上界并按块跳读，减少高频多词查询读取的数据量。

## 三层测试数据

### A. 完全合成数据

用于日常性能回归，固定 seed，每次结果可复现。

不调用 LLM，不含真实用户数据。

### B. 真实分布驱动的合成数据

从 500 到 5,000 条内部真实 trace 中只提取统计值：

- 每条 trace 的 span 数
- 树深度和父子分支数
- span kind 比例
- input/output/log 字节数分位数
- attrs 数量和 key 基数
- tool/model 分布
- error、retry、timeout 比例
- session/loop/task 重复次数
- start/log/end 事件数
- embedding 覆盖率和维度

输出一个不含正文的 `trace-profile.json`，生成器按这个 profile 造数据。

### C. 脱敏真实 trace 放大

准备 100 到 1,000 条脱敏真实 trace：

- 删除 secret、路径、账号和业务正文。
- 保留 span 层级、字段长度和事件顺序。
- 替换 trace/span/session ID、时间、tenant、project 和 task。
- 重放 1,000 到 10,000 次。

它用于发现纯随机生成器覆盖不到的结构组合。

## 百万 Span 默认数据形状

第一版可以使用下面的固定 profile，后续再由真实统计替换：

| 维度 | 建议规模 |
|---|---:|
| Folded spans | 1,000,000 |
| Traces | 约 80,000 |
| Sessions | 约 20,000 |
| Loops | 约 10,000 |
| 平均 spans/trace | 约 12.5 |
| Span 树深度 | 1 到 6 |
| Wire events | 约 250 万到 350 万 |
| Error spans | 5% 到 10% |
| 重复 event | 1% |
| 未结束 span | 0.1% |
| 有 embedding 的 span | 单独测试 10% 和 100% 两档 |

文本大小不能全是短句：

- P50：约 512 B
- P95：约 8 KB
- P99：约 64 KB
- 少量 256 KB 到 1 MB 超长结果

关键词需要同时包含：

- 高频词：命中 20% 到 50% span。
- 中频词：命中约 1%。
- 稀有词：命中 1 到 10 条。
- 中文、英文、代码、JSON、错误栈和 Unicode。

attrs 需要同时包含：

- 低基数：status、mode、model。
- 中基数：project、skill、tool。
- 高基数：call_site、task_fingerprint、external_run_id。
- 超宽值：某个值命中 50% 数据，用来验证 postings 预算降级。

## 不要混在一起的测试

### 数据库规模测试

目标是 100 万 folded spans 和数百万 wire events。

可以使用合成文本和确定性向量。

### 向量召回测试

不需要调用 embedding API。生成带 cluster 标签的确定性向量，并保留精确近邻作为 oracle。

建议先测：

- 10 万向量 × 128 维
- 10 万向量 × 768 维
- 100 万向量 × 128 维作为重档

分别记录建图时间、磁盘、RSS、Recall@10 和过滤后召回。

### Agent/Eval 质量测试

这部分才需要真实 LLM，但只需要几十到几百个代表性 case，不需要百万次。

它验证：

- Context Pack 是否保留关键 span。
- Eval draft 是否可用。
- Best Path Candidate 是否提高固定 eval 集通过率。

## 查询矩阵

百万数据不能只测一个关键词。

每类查询都要覆盖不同选择性：

| 查询 | 选择性 |
|---|---|
| trace/span ID 点查 | 1 条 |
| 稀有 attrs | 0.001% |
| 常规 attrs | 1% |
| 低选择性 attrs | 10% 到 50% |
| BM25 稀有词 | 1 到 10 条 |
| BM25 高频词 | 20% 以上 |
| BM25 + attrs | 高/低选择性组合 |
| task/loop/trajectory | 1、10、1000 次尝试 |
| trace diff | 小 trace、P95 大 trace |
| aggregate | project/status/tool/model 多维组合 |
| span detail | 短字段、P99 大字段 |

每次查询不仅记录延迟，还要断言：

- 返回数量和已知 oracle 一致。
- 索引结果与强制扫描结果一致。
- `readPlan` 表明命中了预期 postings/rollup/BM25/bloom。
- 索引被预算禁用时，fallback 结果不丢。

## 冷热测试

需要分清三种状态：

1. **热查询**：同进程、引擎缓存和 OS page cache 都热。
2. **引擎冷启动**：写入进程退出，新进程重新 open/recover 后第一次查询。
3. **接近磁盘冷读**：使用新数据目录副本或清理系统缓存后的首次打开。

当前 `--cold-queries` 覆盖第二种状态，即真正的进程退出后 reopen；它仍不清理 OS page cache，因此不能代表第三种状态。

生成和查询应拆成两个命令：

```bash
cargo run --manifest-path yitrace-engine/Cargo.toml --release -p yt-engine --example scale_bench -- \
  --phase generate --spans 1000000 --data-dir /tmp/yitrace-scale-1m --keep-data
cargo run --manifest-path yitrace-engine/Cargo.toml --release -p yt-engine --example scale_bench -- \
  --phase query --spans 1000000 --data-dir /tmp/yitrace-scale-1m
```

## 写入与生命周期测试

百万档还需要覆盖：

- batch=1、100、512、5000。
- 单写者 + 1/8/32 个读者。
- 同机 2/4/8 个写进程争用 data-dir 锁。
- 1% event 重放，验证 event_id 幂等。
- flush 期间查询。
- 多轮 compaction。
- 删除 1%、10% trace。
- retention dry-run/apply。
- kill -9 后 reopen。
- 缺失或损坏 rollup/BM25/bloom 后重建。
- snapshot backup 后从副本恢复。

## 必须记录的指标

- ingest spans/s 和 wire events/s
- flush/compaction 时间
- open/recover 时间
- 第一次查询和热查询 P50/P95/P99
- RSS 峰值
- 数据盘总量和 bytes/span
- WAL、segment、rollup、BM25、bloom、vector 各自占用
- 索引构建/重建时间
- 过滤候选数、扫描 segment 数和 `readPlan`
- 错误数、结果缺失数、重复数

第一轮先建立真实基线，不急着写漂亮阈值。硬门槛只有：

- 零数据错误。
- 索引与扫描结果一致。
- 内存不随查询次数持续增长。
- 进程重启和索引重建后结果不变。

第二轮再根据同一台机器的基线设置性能回归门槛。

## 运行分层

- 每次提交：10k smoke。
- 主分支或每天：100k medium。
- 手动/每周：1m large warm + cold + reopen。
- 发版前：1m 生命周期长跑 + 真实分布回放。

百万档不应放进普通单元测试，也不应每次 GitHub Action 都跑。

## 尚未覆盖

1. 从脱敏真实 trace 提取 `trace-profile.json`，按真实分位数生成数据。
2. 增加 forced-scan 与索引结果的集合级 oracle 对账，而不只检查响应片段。
3. 独立的向量规模档：10 万/100 万向量、Recall@10、过滤后召回和磁盘/RSS。
4. 百万档生命周期长跑：compaction、retention、kill -9、sidecar 重建和 snapshot 恢复。
5. 低内存模式下继续验证 attrs/BM25 postings 缓存淘汰，并为 rollup 增加分页读取，避免折叠缓存随规模线性挤压 RSS。

这些属于下一阶段的性能与容量工作，不需要为了生成百万测试数据而调用 LLM。LLM 或小模型只放在几十到几百个代表性 Agent/Eval case 中，用来验证 Context Pack、Eval draft 和 Best Path 的质量。
