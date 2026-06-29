# yiTrace Engine

> ⚠️ **状态:验证级骨架,不是生产就绪。** 技术前提用代码 + 会失败的测试钉死(129 测试),但分词/向量索引/安全合规未到生产级。**不要未经评估直接上生产。** 详见文末["距离开源还剩什么"](#距离开源还剩什么)。
>
> 许可证:**MIT**(见根目录 [LICENSE](../LICENSE))。内嵌 jieba 词典为 MIT(见 `data/JIEBA_DICT_NOTICE.md`),Vortex 为 Apache-2.0。

单机、私有部署的 AI Agent 可观测性数据库。自研 Rust 引擎,刻意只用标准库、**零外部依赖**(`cargo test --offline` 离线可过)。面向中国金融/政企气隙机房,对标 Langfuse / LangSmith,但**数据不出内网、中文检索/语义向量是一等公民、自有索引 IP**。

```bash
cargo test --offline                              # 129 测试(引擎 9+105+6+3+2+4)
cargo run -p yt-engine --example demo --offline  # 灌几条银行风控假 trace,跑写入→折叠→中文搜→找相似→混合召回
cargo run -p yt-engine --example server          # 起 HTTP 摄入服务(8 线程池),curl 即可灌/查
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

## 距离开源还剩什么

### 🔴 必须补(开源前)

| 项 | 说明 |
|---|---|
| **LICENSE** | 没有许可证文件 = 法律上不能复用。选 Apache-2.0 / MIT / AGPL 之一(见下"许可证决策") |
| **环境门槛文档** | Rust MSRV、Python ≥3.10(`int\|None` 语法)、Node ≥18 + 平台匹配的 esbuild;现在环境一变 SDK 测试就挂 |
| **外部 crate 的 vendoring / 镜像** | Vortex crate 联网拉依赖;开源用户/气隙环境要能离线构建(vendoring + build 说明) |
| **README 的诚实定位** | "验证级骨架"必须醒目(否则误导使用者当生产级上);现状已写,但需顶部 badge/状态块更显眼 |
| **统一构建脚本** | 多 crate 工作区 + 工作区外 crate + SDK,需要一篇"怎么从零构建 + 跑全部测试"的 CONTRIBUTING |

### 🟡 强烈建议(开源后第一周会被问)

| 项 | 说明 |
|---|---|
| **CHANGELOG + 版本号** | 现在是 git main 一条线,没有 release tag / CHANGELOG |
| **CI(GitHub Actions)** | 多 Rust 版本矩阵 + clippy + fmt + SDK 多版本 + Vortex crate;现在零 CI |
| **示例完整化** | `demo`/`server` 有,但缺"端到端:SDK 打点 → server → 查询 → eval"一条龙示例 |
| **架构图** | 五 crate + 外部 crate + 数据流图,文字 README 说不清;需要一张图 |
| **测试矩阵明示** | "129 测试验什么"现在堆在一段里,需要表格化(功能清单已部分解决) |
| **竞品对比页** | 会反复被问"跟 Langfuse/ClickHouse 比怎样";产品说明里有但 README 没指 |

### 🟢 功能性缺口(不影响开源,但 README 要标"未实现")

| 项 | 说明 |
|---|---|
| DataFusion 查询执行 | 现手写查询路径;换 DataFusion 是架构整洁度,不是功能/性能缺口 |
| 全局跨段定位索引 | 亿级数据对段数真正次线性(阶段 2,见跨段扩展性分析) |
| BM25 倒排段内化 | 真 TB(阶段 3,Quickwit/Lucene 模型) |
| 向量量化(PQ/SQ) | 几十亿向量省内存;SQ 先行(4× 几乎无损) |
| 安全合规 | TLS / RBAC 物理隔离 / 落盘加密 / 持久防篡改审计 / 限流 / PII 脱敏 |
| LLM-judge eval | 现在 KeywordScorer;接 LLM-judge 需出站 HTTP(气隙走本地小模型) |

### 许可证决策(必须先定)

| 许可证 | 适合 |
|---|---|
| **Apache-2.0** | 最宽松、专利授权全、生态最广(Langfuse/Vortex/DuckDB 都是) |
| MIT | 更短更简,但无专利授权条款 |
| AGPL-3.0 | 防竞品白嫖(网络服务也得开源),但会劝退部分企业用户 |

> 注意:内嵌的 jieba 词典是 **MIT**(声明在 `data/JIEBA_DICT_NOTICE.md`);选 Apache-2.0 兼容它。Vortex 是 Apache-2.0。

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
