# yiTrace Engine

> ⚠️ **状态:alpha,不是生产托管集群。** 技术前提用代码 + 会失败的测试钉死,单机、嵌入式、读模型索引和分布式 gateway 原语都有 eval 覆盖。自动 failover、fencing、后台复制调度、TLS/RBAC 仍未完成。**不要未经评估直接上生产。** 详见根目录 [`docs/CURRENT_STATE.md`](../docs/CURRENT_STATE.md)。
>
> 许可证:**MIT**(见根目录 [LICENSE](../LICENSE))。内嵌 jieba 词典为 MIT(见 `data/JIEBA_DICT_NOTICE.md`),Vortex 为 Apache-2.0。

本地优先、可分片演进的 AI Agent TraceDB。自研 Rust 引擎,刻意只用标准库、**零外部依赖**(`cargo test --offline` 离线可过)。它既可以作为单个私有服务运行,也可以嵌入 Node/Electron,还可以通过 gateway + route table 路由到多个 shard。shard 内保持单写者正确性,cluster 层通过 fanout、snapshot lease、follower read 和 WAL replication 原语扩展。

```bash
cargo test --offline                              # 全量引擎测试,含分布式 eval
cargo run -p yt-engine --example demo --offline  # 灌几条银行风控假 trace,跑写入→折叠→中文搜→找相似→混合召回
cargo run -p yt-engine --example server          # 起 HTTP 摄入服务(8 线程池),curl 即可灌/查
YT_BIND=127.0.0.1:7879 cargo run -p yt-engine --example server_durable -- ./data/yitrace  # 持久化服务样板
YT_TOKEN=secret cargo run ... --example server   # 开 Bearer token 鉴权
cargo test -p yt-engine --features gzip          # 含 gzip 请求体解压(可选 feature,默认离线 std-only)
cargo run -p yt-engine --release --example bench_qps  # 真实 QPS 压测(摄入/检索/建图)
```

```bash
#   curl -XPOST localhost:7878/v1/ingest -d '[{"trace_id":7,"span_id":1,"ts":1,"seq":1,"event_type":1,"ext_span_id":"7-1","status":0,"input_tokens":900,"logs":["开始"]}]'
#   curl localhost:7878/v1/traces
#   curl -XPOST localhost:7878/v1/search -d '{"text":"盗刷","k":10,"filter":{"agent_name":"风控","status":1}}'   # 中文搜 + 按 agent/状态过滤
#   curl -XPOST localhost:7878/v1/search -d '{"vector":[0.1,0.2],"k":10}'                # 找相似(纯向量)
#   curl -XPOST localhost:7878/v1/search -d '{"text":"盗刷","vector":[0.1,0.2],"k":10}'   # 混合(关键词+语义 RRF 融合)
```

---

## 功能清单(Feature List)

按"一个 Agent 可观测性产品需要什么"组织。每条都有对应测试钉死(测试名见各小节)。

### 数据摄入

| 功能 | 状态 | 说明 |
|---|---|---|
| 自有打点 SDK(Python/TS) | ✅ | 嵌套 span 自动建父子、set_tokens/agent/tool/model/session/tenant、HTTP 批量导出 |
| OTLP/OpenInference 摄入 | ✅ | 任何已用 OpenTelemetry 埋点的 agent 应用**不改打点**就能灌进来;认两套语义约定(OTel GenAI + OpenInference),GenAI 嵌套消息数组拍平 |
| 跨语言 event_id 一致 | ✅ | 同一条事件 Rust/Python/TS 算出的 id **逐字节一致** → 重复送达只算一次,token/cost 不翻倍 |
| HTTP 摄入服务 | ✅ | 8 线程池、Bearer 鉴权、请求体上限(堵 OOM)、审计留痕、可选 gzip |

### 存储(持久化)

| 功能 | 状态 | 说明 |
|---|---|---|
| 追加写 + 读时折叠 | ✅ | 一条 trace 由多源(span、事件、评测分)读时合成一条完整记录 |
| WAL 落盘 + fsync | ✅ | 崩溃安全帧(长度+CRC+marker,撕裂尾自动截断);重放只认已确认批次 |
| 段落盘(FileSegmentStore) | ✅ | 不可变段,原子写(tmp+fsync+rename),crc 守门 |
| Manifest 持久化 | ✅ | 段集合 + 删除位图 + upgrade 补写块 + 水位 + epoch + id 计数器,原子写 |
| **重启不丢** | ✅ | flush → 丢引擎 → 重开 → 数据从持久段 + WAL 回来、删除也还在、段 id 不复用 |
| 索引重启重建 | ✅ | BM25/属性边车从段派生重建;向量从独立向量段文件重载 |
| **Vortex 列式段** | ✅ | 隔离 crate `yitrace-segstore-vortex`;**谓词下推 + 投影下推**(聚合只读窄列、跳过大文本列) |

### 并发与正确性(技术脊梁)

| 功能 | 状态 | 说明 |
|---|---|---|
| 快照隔离 | ✅ | EBR pin 协议(先登记再读)、回收水位取最老读者、RAII 自动注销 |
| 三水位 GC 回收 | ✅ | 被合并的旧段在"无读者 pin + 无 buffer pin + 不被引用"三条件满足后才删文件 |
| 四源折叠 | ✅ | 内存表 + 段 + 删除位图 + upgrade 补写;event_id 去重 + 最后非空值优先 + 日志并集 |
| 崩溃重放幂等 | ✅ | 确定性 event_id → 重叠窗口字段不漂移、不算两遍 |
| compaction 并发重读 | ✅ | 两阶段提交(选段→提交前重读合并),并发删除/补写不丢 |
| 多线程压测 | ✅ | 4 读 + 1 写 + 1 回收 + 真删段文件,不崩不死锁、种子 span 始终可见 |

### 检索(产品噱头 / 差异化)

| 功能 | 状态 | 说明 |
|---|---|---|
| **中文 BM25 检索** | ✅ | 真倒排 + BM25(k1/b)评分 + **block-max-WAND** 剪枝;**jieba 全量词典(34.9 万词)默认内嵌**,支持自有词典导入,纯 Rust 词级分词(词典 DAG + 最大概率 DP) |
| **磁盘型多层 HNSW** | ✅ | 落盘版 HNSW(参考 yiTrace graph_index):底层+向量在磁盘、上层稀疏骨架常驻内存、向量按需读页走缓冲池;重启不 rebuild |
| **进图过滤召回** | ✅ | 过滤条件进图导航(ACORN 式),稀疏谓词召回不塌(实测 1% 选择性 post 0.17 → in-graph 1.00) |
| 带属性过滤 | ✅ | 按 agent/status/time/trace 过滤(向量侧走进图、BM25 侧后置) |
| 混合召回 | ✅ | BM25 + 向量用 RRF 融合成一路,双命中排更前 |
| 时间窗 + trace 剪枝 | ✅ | 段级 zone-map 跳无关段;段折叠缓存(检索只取候选行) |
| 段级 key Bloom | ✅ | 折叠定位时跳过"肯定没有"的段(ClickHouse 跳过索引同款) |

### 评测与飞轮

| 功能 | 状态 | 说明 |
|---|---|---|
| eval 闭环 | ✅ | 规则 scorer 打分,**分数走 upgrade 通道写回**(评测分 = trace 后补字段);接 LLM-judge 只换 scorer |
| eval 看板 | ✅ | 通过率 / 均分(整体 + per-agent),回归视图 |
| 评测数据集 | ✅ | 按谓词采集成命名集(收失败样本),对集现跑 scorer 出回归基准 |

### 分析视图

| 功能 | 状态 | 说明 |
|---|---|---|
| trace 列表/摘要 | ✅ | span 数 / 总最大耗时 / 报错数 / **token 汇总** |
| 父子 span 树 | ✅ | load_trace_tree 连成树 + DFS 瀑布顺序 |
| 会话视图 | ✅ | list_sessions 按 session 聚合多轮对话 |
| per-agent 成本 | ✅ | cost_by_agent 按 agent 归因 token |
| agent 执行图(DAG) | ✅ | 父子树收拢成"谁调用了谁",dogfood 自家 SuperAgent 的核心视图 |

### 多租户隔离

| 功能 | 状态 | 说明 |
|---|---|---|
| 逻辑隔离(tenant_id) | ✅ | 共享索引 + 强制过滤;BM25 后置过滤 + 向量进图过滤(低选择性召回不塌) |
| 全栈贯穿 | ✅ | SpanFields / WAL / wire / 属性边车 / 列式段 / SearchFilter |
| HTTP 鉴权头隔离 | ✅ | tenant 从 `X-Tenant-Id` 鉴权头取(**非请求体,客户端不能越权**) |
| SDK 透传 | ✅ | Python `trace(name, tenant_id=)` / TS 同款 |

### 分布式数据路径

| 功能 | 状态 | 说明 |
|---|---|---|
| route table | ✅ | v1 扁平 shard route + v2 logical shard/replicas;每个 logical shard 必须恰好一个 writable replica |
| gateway 写路由 | ✅ | 按 tenant/session/trace 路由到 writable shard;返回 partialSuccess / failedShards / retrySafe |
| 读 fanout merge | ✅ | search、traceSearch、aggregate、trajectory、storage、metadata、retention、vector 已有 remote fanout |
| 一致性策略 | ✅ | 默认 partial;显式 strict/strong 或 `partial:false` 时任一 shard 失败会拒绝 |
| follower read target | ✅ | 根据 health refresh 和 `replicationLagLsn <= maxLagLsn` 选择 readable follower,不合格回 leader |
| remote snapshot lease | ✅ | gateway composite lease + shard-local lease,支持 TTL / renew / release / route table version 校验 |
| WAL 复制原语 | ✅ | `replication/status`、`replication/wal` export/apply、`replication/pull` one-shot follower pull |
| 生产控制面 | 🟡 | 后台 watcher、自动 failover、fencing、后台复制调度、snapshot bootstrap、sidecar/metadata/GC log 同步仍待做 |

### 可调旋钮(部署参数)

| 参数 | 说明 |
|---|---|
| `ef_construction` | HNSW 建图候选列表(大→建图慢但召回高) |
| `ef_search` | 查询候选列表(大→召回高但查询慢) |
| `vector_cache_bytes` | 向量缓冲池预算(如 1GiB;超预算的热向量常驻、冷的回磁盘) |
| `flush_threshold` | 内存表行数上限(超则自动刷盘) |

---

## 性能(本机 release,2 万 span / 128 维 / 各 2000 查询)

| 指标 | 数字 |
|---|---|
| 摄入吞吐 | ~40,000 span/s(含 WAL 落盘 + 全量词典分词) |
| 向量建图 | ~1,500 点/s(单线程;HNSW 建图天生重) |
| BM25 检索 | ~1,500 QPS(热缓存) |
| 向量检索 | ~1,000 QPS(热缓存) |
| JSON 解析+灌入 | ~480,000 事件/s(单线程) |

`bench_qps` 实例可复现。扩展性优化(段级 Bloom + block-max-WAND)在 5 万规模下 BM25 +66%、向量 +40%。

---

## 五个 crate

| crate | 干什么 |
|---|---|
| `yt-core` | 核心类型:三类不可变标识、**确定性 event_id**、不可变 Manifest(写时复制)、deletion/upgrade 对称块、**四源折叠算法**(纯函数)、RRF 融合 |
| `yt-manifest` | **正确性脊梁**:读者 pin 协议(先登记再读)、回收水位、RAII 自动注销 |
| `yt-wal` | 写前日志:文件落盘 + fsync;崩溃安全帧;自研二进制编码(表查 CRC32) |
| `yt-memtable` | 活内存表:上下界双水位 + 受 gate 的 evict(修"flush 后漏读一截") + 自动刷盘 |
| `yt-engine` | 单写者协调器、段五态生命周期、磁盘型 HNSW、BM25 倒排、四源折叠、HTTP 服务、OTLP 适配、eval、多租户、投影/谓词下推 |

## 相关 crate(工作区外,隔离重依赖)

| crate | 干什么 |
|---|---|
| `yitrace-segstore-vortex` | Vortex 列式段存储(实现 SegmentStore);谓词下推 + 投影下推 |
| `yitrace-tokenizer-jieba` | cppjieba FFI 接入(crate,团队真库到位时 `--features link`);**引擎默认用纯 Rust 词级分词** |
| `yitrace-vecindex-graph` | 团队 graph_index FFI 接入;**引擎默认用自研磁盘型 HNSW** |
| `yitrace-sdk` | Python / TypeScript 打点 SDK |

---

## 当前边界

### 🔴 进入生产前必须补

| 项 | 说明 |
|---|---|
| **安全边界** | TLS、RBAC、落盘加密、限流、PII 脱敏、持久审计仍未落地 |
| **生产控制面** | route table 后台 watcher、自动 failover、old leader fencing、后台复制调度仍未落地 |
| **复制恢复** | WAL tail 复制已有;leader compaction/retention 后的 snapshot bootstrap、sealed segment/sidecar/metadata/GC log 远程同步仍待做 |
| **发布矩阵** | `@yitrace/db` root 包 + per-platform optional native packages 还需要正式 CI matrix 和 npm 发布 |
| **外部真库对标** | jieba / graph_index FFI 接缝已就位,真库链接和生产召回对标仍要在构建机完成 |

### 🟡 上量后优先补

| 项 | 说明 |
|---|---|
| 高性能向量 namespace | `named_vectors.dat` + flat index 已可用;task/trajectory namespace 后续要接 HNSW/GraphIndex 和 recall/perf 回归 |
| 100k/1M 性能 bench | 现有 bench 覆盖单机核心路径;读模型索引、gateway fanout 和 vector namespace 还需要大规模门禁 |
| 预聚合 counter | `traceAggregate` 已有 segment rollup row;更激进的 `(group_schema, group_key) -> counters` 还没做 |
| DataFusion 查询执行 | 现手写查询路径;DataFusion 仍是后续工程化方向 |
| LLM-judge eval | 现在是规则 scorer;接 LLM judge 需要本地模型或受控出站 HTTP |

---

## 设计文档(在仓库根 `docs/`)

- `docs/CURRENT_STATE.md` — **唯一权威现状索引**(先读这篇)
- `docs/2026-06-22_yitrace-产品说明.md` — 产品定位 / 三条护城河 / 竞品对比
- `docs/design/2026-06-22_列式段存储-vortex-选型与落地计划.md` — Vortex 选型 + 落地
- `docs/analysis/2026-06-24_检索跨段扩展性分析.md` — 检索扩展性三轴 + 开源方案对标
- `docs/design/2026-06-23_BM25-生产化与检索索引接缝.md` — BM25 生产化决策
- `docs/design/2026-06-17_yitrace-segment-snapshot-hardened.md` — 并发设计加固稿(段生命周期 + 快照)
- 其余设计/分析/调研见 `docs/` 各子目录

> `docs/design/appendix-*` 是历史溯源产物(红队多轮 + 修订),**非当前态**;看当前态读 `CURRENT_STATE.md`。
