# 贡献指南 / CONTRIBUTING

感谢关注 yiTrace。这篇文档说清**怎么从零构建、跑测试、改代码**,以及各组件的**环境门槛**。

## 项目结构

```
vex-x/
├── yitrace-engine/              # 自研 Rust 引擎(工作区,零外部依赖)
│   ├── crates/
│   │   ├── yt-core/                # 核心类型:event_id / Manifest / 折叠算法
│   │   ├── yt-manifest/            # 并发正确性:pin 协议 / 回收水位
│   │   ├── yt-wal/                 # 写前日志:fsync / 崩溃安全帧
│   │   ├── yt-memtable/            # 活内存表:双水位 / 自动刷盘
│   │   └── yt-engine/              # 主引擎:协调器 / HTTP / 检索 / eval / 多租户
│   └── examples/                    # demo / server / bench_qps / eval_harness
├── yitrace-segstore-vortex/     # Vortex 列式段(工作区外,隔离重依赖)
├── yitrace-tokenizer-jieba/     # cppjieba FFI(可选;引擎默认用纯 Rust 分词)
├── yitrace-vecindex-graph/      # graph_index FFI(可选;引擎默认用自研 HNSW)
├── yitrace-sdk/                 # Python / TypeScript 打点 SDK
│   ├── python/
│   └── typescript/
├── yitrace-console/             # (占位)Web 控制台
├── tracevault-extension/            # 历史方案(openGauss 扩展),非当前态,保留参考
└── docs/                            # 设计 / 分析 / 调研文档
```

## 环境门槛(必读)

### Rust 引擎

- **Rust ≥ 1.82**(edition 2021;用了 `OnceLock` 等稳定 API)
- 引擎工作区 `yitrace-engine/` **零外部依赖**,`cargo test --offline` 离线可过

### Vortex 列式段 crate(`yitrace-segstore-vortex/`)

- **Rust ≥ 1.91**(Vortex 0.75 要求)
- **首次构建需联网**(拉 vortex + arrow + zstd 依赖树,约 200 crate);之后离线可编
- 气隙/离线构建:vendoring 后 `cargo build --offline`

### Python SDK(`yitrace-sdk/python/`)

- **Python ≥ 3.10**(代码用了 `int | None` union 语法)
- 纯标准库,**零第三方依赖**(不依赖 pytest;`python3 tests/test_sdk.py` 直接跑)

### TypeScript SDK(`yitrace-sdk/typescript/`)

- **Node ≥ 18**(用了原生 test runner `node --test`)
- 用可擦除 TS 语法(无 enum/namespace),`npx tsx --test test/test_sdk.ts` 直接跑,**无需编译**
- 注意:esbuild(tsx 依赖)是平台特定的 native binary;换平台要重装 `npm install`

## 从零构建 + 跑全部测试

```bash
# 1. Rust 引擎(零依赖,离线可跑)
cd yitrace-engine
cargo test --offline                    # 129 测试(9+105+6+3+2+4)

# 2. Vortex 列式段 crate(首次联网)
cd ../yitrace-segstore-vortex
cargo test                              # 7 测试(首次编译几分钟)

# 3. 可选 FFI crate(默认离线 mock,真库要 --features link)
cd ../yitrace-tokenizer-jieba && cargo test     # 3 测试(mock)
cd ../yitrace-vecindex-graph && cargo test      # 4 测试(mock)

# 4. Python SDK
cd ../yitrace-sdk/python
python3 tests/test_sdk.py               # 8 测试(需 Python ≥ 3.10)

# 5. TypeScript SDK
cd ../typescript
npm install                             # 首次
npx tsx --test test/test_sdk.ts         # 8 测试(需 Node ≥ 18)
```

## 跑示例

```bash
cd yitrace-engine

# 灌几条银行风控假 trace,跑写入→折叠→中文搜→找相似→混合召回
cargo run -p yt-engine --example demo --offline

# 起 HTTP 摄入服务,curl 可灌/查
cargo run -p yt-engine --example server
# 另一个终端:
#   curl -XPOST localhost:7878/v1/ingest -d '[{"trace_id":1,"span_id":1,"ts":1,"seq":1,"event_type":1,"ext_span_id":"1-1","status":0,"logs":["盗刷 已拦截"]}]'
#   curl localhost:7878/v1/traces
#   curl -XPOST localhost:7878/v1/search -d '{"text":"盗刷","k":10}'

# 真实 QPS 压测(release 模式才有意义)
cargo run -p yt-engine --release --example bench_qps
```

## 怎么贡献

### 提交前检查清单

- [ ] `cargo test --offline`(引擎工作区)全绿
- [ ] `cargo clippy --offline -- -D warnings`(如果你改了引擎)
- [ ] `cargo fmt --all -- --check`(格式)
- [ ] 改了 SDK 的话,跑对应 SDK 测试
- [ ] 改了 Vortex crate 的话,跑 `yitrace-segstore-vortex` 测试
- [ ] 新功能加了**会失败的测试**(不是凑覆盖率)

### 代码风格

- **引擎保持零外部依赖**(`yitrace-engine/` 工作区)—— 不引 crate,需要新能力优先看 std 有没有
- **重依赖隔离在工作区外**(像 Vortex 那样建独立 crate,引擎通过 trait 注入)
- **诚实标注边界**——占位/验证级/未实现的地方用注释或 `todo!()` 标明,不假装已完成
- **commit message 纯净、中文、无 AI 模型署名**

### 测试文化(项目核心价值)

这个项目的测试**不是凑覆盖率,是钉技术前提**。每个测试应该:
- 验证一个**会失败的不变量**(故意构造会破坏它的场景)
- 名字说明"它在防什么"(如 `pinned_reader_holds_back_safe_version`)
- 红队/边界场景优先(并发竞态、崩溃、乱序、晚到)

新增功能必须配测试;修 bug 必须加**会暴露该 bug**的回归测试。

## 文档

- `docs/CURRENT_STATE.md` — **唯一权威现状索引**(改了代码请同步更新这篇)
- `docs/design/` — 设计文档(当前态 + 历史溯源)
- `docs/analysis/` — 分析(性能/扩展性/竞品)
- `docs/research/` — 调研

改了功能,请同步改对应文档。`docs/design/appendix-*` 是历史过程产物,非当前态,**别改也别参考为现状**。

## 问题 / 讨论

- Bug 报告:附上最小复现(最好是个会失败的测试)
- 功能建议:先看 `docs/CURRENT_STATE.md` 的"已知工程债"和"距离开源还剩什么",确认不是已标的事项
