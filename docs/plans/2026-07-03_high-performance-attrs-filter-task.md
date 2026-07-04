# High-Performance Attrs Filter Task

> 日期：2026-07-03
> 状态：第一阶段已落地
> 背景：P0 已支持通用 attrs 过滤和 `fields` 输出，但 `traces()` / `sessions()` / `traceSearch()` 的纯 attrs 查询仍需要进一步降低扫描成本。

## 目标

把高频 AgenticData 过滤从“折叠后全量扫描”升级为“索引候选集 + 折叠验证”：

- attrs postings sidecar：`(attr_key, attr_value)` 到 `(trace_id, span_id)` 候选集合。
- string array includes postings：支持 `connection_ids: ["a", "b"]` 被 `connection_ids: "a"` 命中。
- trace span keys sidecar：从匹配 span 扩展到整条 trace 的 span keys，保证 trace summary 仍是完整聚合。
- 复用同一套 JSON 语义验证，索引只做候选集，最终结果仍以折叠后的真实 span 为准。

## 第一阶段

- [x] 维护 attrs postings sidecar，随 ingest/recover 重建。
- [x] `traceSearch()` 有 attrs 条件时先用 postings 缩小候选。
- [x] `traces({ attrs })` 先用 postings 找匹配 trace，再只折叠这些 trace 的 span。
- [x] `sessions({ attrs })` 先用 postings 找匹配 session，再过滤 session rows。
- [x] 保持租户隔离仍由折叠读路径验证，不依赖客户端 body。
- [x] `/v1/search` 的 attrs 谓词也用 postings 做预过滤，最终仍由过滤边车验证。
- [x] `fields` 输出只为当前 trace list 可见 trace 做窄投影补全。
- [x] `project_id`、`skill`、`mode`、`call_site`、`task_fingerprint`、`loop_id`、`harness_version`、`validation_status`、`stop_reason`、`phase`、`validator` 从 attrs schema-on-write 提升为折叠后的一等字段，并保留 attrs fallback。
- [x] 第二批业务字段 `schema_fingerprint`、`intent_signature`、`review_status`、`eval_status`、`path_memory_id` 提升为折叠后的一等字段，并保留 attrs fallback。

## 当前实现

- `AttrPostings` 维护 compact JSON exact postings 和 string-array includes postings。
- postings 返回候选 `(trace_id, span_id)`，读路径再用 snapshot/deletion/tenant 折叠结果校验。
- postings 只默认索引稳定高频字段：`project_id`、`skill`、`mode`、`call_site`、`task_fingerprint`、`loop_id`、`harness_version`、`validation_status`、`stop_reason`、`phase`、`validator`、`connection_ids`、`data_source_ids`、`schema_fingerprint`、`intent_signature`、`review_status`、`eval_status`。
- `external_run_id`、`path_memory_id` 等高基数字段仍支持过滤，但默认不进 postings；查询会慢路径折叠校验，或和其他已索引字段组合缩小候选后再校验。
- value 超过 256 bytes、数组字符串项超过 32 个、postings entry 超过 2,000,000、或近似 postings 内存超过 256 MiB 时，该 key 降级为 incomplete，后续查询不会使用该 key 的不完整索引。
- posting list 使用小集合优化：单命中 key/value 用 `One(span_key)`，2-8 个 span 用有序 `Vec<SpanKey>`，超过 8 个才升级为 `HashSet`，避免大量单元素和中小 HashSet。
- postings key/value 使用进程内字符串字典，HashMap key 只保存 `(key_id, value_id)`，避免每个倒排桶重复持有字段名和值字符串；查询未知值只 lookup，不会把字典撑大。
- 持久 segment 现在生成 segment-local attrs sidecar，文件位于持久 data dir 的 `attr_postings/seg-<id>.attrs`。内存只常驻 term → segment ids 的轻量目录；具体 posting list 由 LRU cache 按需加载。
- `attr_postings` 现在是 live overlay，只覆盖 MemTable/WAL tail；flush 后会根据仍被旧快照 pin 住的 MemTable 行重建，不再长期持有历史 segment 的 span-level postings。
- segment sidecar 缺失或损坏时可从 segment 原始记录重建；sidecar 是派生索引，不是事实源。
- `project_id`、`skill`、`mode`、`call_site`、`task_fingerprint`、`loop_id`、`harness_version`、`validation_status`、`stop_reason`、`phase`、`validator` 已进入 `SpanFields` / `FoldedSpan` / WAL+segment 共享编码 v5；`schema_fingerprint`、`intent_signature`、`review_status`、`eval_status`、`path_memory_id` 已进入共享编码 v6。查询和 `fields` 输出优先读一等字段，回退 attrs。这样新数据即使不镜像 attrs，也能被 attrs filter 和 trace list 命中。
- trace list 采用两段式读取：候选 span 找匹配 trace，再扩展为整条 trace 的 span keys 做完整摘要。
- session list 采用两段式读取：候选 span 找匹配 session，再复用现有 session 聚合行过滤。
- 新增 `yt_attr_posting_keys` / `yt_attr_posting_singleton_keys` / `yt_attr_posting_small_vec_keys` / `yt_attr_posting_hashset_keys` / `yt_attr_posting_interned_keys` / `yt_attr_posting_interned_values` / `yt_attr_posting_entries` / `yt_attr_posting_entry_budget` / `yt_attr_posting_estimated_bytes` / `yt_attr_posting_estimated_byte_budget` / `yt_attr_posting_incomplete_keys` 指标观察 live postings 规模、字符串字典规模和降级状态。
- 新增 `yt_attr_sidecar_segments` / `yt_attr_sidecar_exact_terms` / `yt_attr_sidecar_array_terms` / `yt_attr_sidecar_incomplete_keys` / `yt_attr_sidecar_cache_entries` / `yt_attr_sidecar_cache_bytes` / `yt_attr_sidecar_cache_byte_budget` / `yt_attr_sidecar_cache_hits` / `yt_attr_sidecar_cache_misses` / `yt_attr_sidecar_cache_loads` / `yt_attr_sidecar_cache_evictions` 指标观察 segment sidecar 目录和 cache 行为。

## 内存风险与 buffer manager 判断

当前内存 postings 是第一阶段加速，不是最终大数据形态。最危险的情况是高基数字段或数组字段被全部建索引：例如每个 span 都带唯一 `external_run_id`、`path_memory_id`、长 `call_site` 或多个 `connection_ids`，`HashMap<(key,value), HashSet<span_key>>` 会产生大量单值 posting list，HashSet 和 String 的结构开销会明显大于原始数据。

不建议马上写通用 buffer manager。postings 是派生索引，不是源数据页缓存；直接上通用 buffer manager 会把复杂度提前引入到还没稳定的索引形态里。更稳的路线是：

- 短期：给 in-memory postings 加 budget 和 index policy。默认只索引高频查询字段；超预算、超长 value、未知高基数字段走 slow path，不允许因为索引不完整产生漏召回。
- 中期：把 postings 改成 segment-local sidecar。flush 时为每个 segment 生成 attrs postings 文件，内存只保留轻量目录，查询时按 key/value 拉取对应 posting list，并用 LRU cache 控制常驻内存。这是“专用 index page cache”，不是通用 buffer manager。当前已完成第一版：sidecar 文件、目录、LRU cache、recover 重建和缺失回退都已落地。
- 长期：高频字段物理入列，配合 segment-level zone-map/bloom/postings，减少 JSON parse 和 postings fan-out。

查询正确性原则：索引可以缺失或返回超集，但不能返回不完整子集。某个 key 若因预算被降级为 unindexed，包含该 key 的查询必须退回折叠校验或只用其他完整索引字段缩小候选。

## 后续阶段

- [x] in-memory postings 增加 key allowlist、value 长度限制、entry budget、unindexed fallback。
- [x] in-memory postings 增加近似 bytes budget，而不只是 entry budget。
- [x] postings 内部结构压缩第一步：singleton posting 优化，避免大量小 HashSet。
- [x] postings 内部结构压缩第二步：sorted span-key vec 替代中小 HashSet。
- [x] postings 内部结构压缩第三步：key/value interning。
- [x] segment-local attrs postings sidecar + LRU index cache，降低常驻内存。
- [x] 第一批高频字段（project/skill/mode/call_site/task/loop/validation）物理入列。
- [x] 第二批业务字段按稳定度继续物理入列，减少 JSON parse 和内存 sidecar 压力。
- [ ] segment-level attrs zone-map / bloom skip，降低 cold scan。
- [ ] trace/session 级预聚合索引，避免 tenant session list 二次聚合。
- [ ] postings 持久化，避免大库重启后从 WAL/segment 全量重建成本过高。

## 正确性约束

- 索引可以返回超集，不能漏召回。
- 删除/compaction 后的陈旧 postings 不能泄漏数据，最终必须经过 snapshot/deletion/tenant 折叠读验证。
- external string id、attrs JSON round-trip、array includes 语义不能变。
