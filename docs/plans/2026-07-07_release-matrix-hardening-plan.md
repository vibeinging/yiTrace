# yiTrace 多平台发版硬化计划

> 日期：2026-07-07
> 状态：计划
> 目标：让别人能稳定安装，而不是只在本机能跑。

## 结论

下一阶段发版目标不是马上公开发布，而是先把“可锁定、可复现、可验证”的安装产物做扎实。

当前最容易失败的是 native 包：

- Node `@yitrace/db` 的 N-API `.node`
- Python `yitrace-db` 的 PyO3 wheel
- Electron asar / optional dependency 裁剪
- macOS arm64/x64 与 Linux/Windows 的架构差异

## 发版对象

| 包 | 用户场景 | 产物 |
|---|---|---|
| `@yitrace/trace-sdk` | Node/TS 应用只打点 | JS 包，无 native |
| `@yitrace/db` | Node/Electron 进程内打开 DB | root JS 包 + platform optional native 包 |
| `yitrace` | Python 应用只打点 | 纯 Python wheel/sdist |
| `yitrace-db` | Python 进程内打开 DB | PyO3 wheel |
| `yitrace-db` Rust crate | Rust 应用进程内打开 DB | crate |
| `yt-engine` binary/examples | 服务端/gateway 使用 | cargo build/release artifact |

## Node matrix

### 平台包

| 平台 | 包名 | 状态 |
|---|---|---|
| macOS arm64 | `@yitrace/db-darwin-arm64` | P0 |
| macOS x64 | `@yitrace/db-darwin-x64` | P0 |
| Linux x64 glibc | `@yitrace/db-linux-x64-gnu` | P0 |
| Linux arm64 glibc | `@yitrace/db-linux-arm64-gnu` | P1 |
| Windows x64 MSVC | `@yitrace/db-win32-x64-msvc` | P1 |

### 必跑验证

```bash
cd yitrace-node
npm ci
npm run build
npm test
npm run pack:verify
```

clean consumer 必须验证：

- ESM import。
- CJS require。
- native load。
- builder ingest。
- search。
- trace detail。
- close/reopen。
- optional package 被正确解析。

### Electron 验证

必须单独验证：

- `.node` asar unpack。
- optional native packages 没被打包器裁掉。
- `NAPI_RS_NATIVE_LIBRARY_PATH` fallback。
- main process 持有 DB，renderer 通过 IPC。

## Python matrix

| 平台 | 产物 | 优先级 |
|---|---|---|
| macOS arm64 | wheel | P0 |
| macOS x64 | wheel | P1 |
| Linux x64 | manylinux wheel | P0 |
| Linux arm64 | manylinux wheel | P1 |
| Windows x64 | wheel | P1 |

必跑验证：

```bash
cd yitrace-db-python
python -m pip install -e .
python -m pytest
python -m maturin build --release --interpreter "$(command -v python)"
```

clean venv 验证：

- `from yitrace_db import YiTraceDB`
- open durable dir。
- builder ingest。
- search。
- session/span detail。
- close/reopen。
- second writer lock。

## Rust crate matrix

当前 `yitrace-db-rs` 仍 `publish = false`。正式发布前要决定：

- 是否独立发布 `yitrace-db`。
- 是否同时发布 `yt-engine`。
- crate API 是否足够稳定。
- docs.rs 能否编译。

必跑验证：

```bash
cd yitrace-db-rs
cargo test --offline
cargo doc --no-deps
```

## CI 分层

### PR 默认

```bash
./scripts/eval_all.sh
```

覆盖：

- risk matrix
- distributed chaos
- eval harness
- gateway example check
- Rust DB crate

### 包级检查

```bash
./scripts/eval_all.sh --packages
```

覆盖：

- Python SDK
- TypeScript SDK
- console test/build
- Python DB
- Node DB build/test

### 发版候选

```bash
./scripts/eval_all.sh --packages --pack --crash --crash-rounds 20
```

覆盖：

- clean consumer pack verify
- kill -9 recovery
- package tests

### 多平台 release

需要 GitHub Actions matrix 或内部 CI matrix：

- macOS arm64
- macOS x64
- Ubuntu x64
- Ubuntu arm64
- Windows x64

每个平台产出 artifact，并做本平台 clean consumer install。

## 版本策略

规则：

- 不复用同一个 `0.0.1.tgz` 文件名。
- 本地 tarball 文件名必须带 version + commit + dirty 标记。
- root 包和 optional platform 包版本必须一致。
- release note 必须写清支持平台。
- 不支持的平台要 fail fast，并给出清楚错误。

## 验收

P0 验收：

- macOS arm64 root + optional package clean consumer 通过。
- Linux x64 root + optional package clean consumer 通过。
- Python macOS arm64 wheel clean venv 通过。
- Python Linux x64 wheel clean venv 通过。
- `./scripts/eval_all.sh --packages --pack` 本机通过。

P1 验收：

- Windows x64 Node/Python 通过。
- Electron asar demo 通过。
- Rust crate docs 通过。
- release artifacts 带 SHA256。

## 当前不做

- 不立即 npm publish。
- 不立即 PyPI publish。
- 不承诺所有平台 GA。
- 不把未验证平台写成已支持。
