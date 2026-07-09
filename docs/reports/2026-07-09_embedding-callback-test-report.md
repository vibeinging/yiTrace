# embedding 回调与持久索引测试报告

日期：2026-07-09

## 结论

本轮 embedding 回调、持久 BM25、external id 快查相关改动已完成完整本地验证。

结果：通过。

## 覆盖范围

### 引擎

命令：

```bash
cargo test --offline --manifest-path yitrace-engine/Cargo.toml
cargo fmt --all --manifest-path yitrace-engine/Cargo.toml --check
git diff --check
```

结果：

- `yt-engine` lib tests：138 passed，1 ignored。
- `eval_harness`：6 passed。
- `multiprocess_embedded`：4 passed。
- `risk_eval_matrix`：9 passed。
- `yt_core` / `yt_manifest` / `yt_memtable` / `yt_wal` tests 和 doctests 均通过。
- format check 通过。
- diff whitespace check 通过。

重点覆盖：

- `bm25.dat` / `segment_bloom.dat` 命中后 recover `segs_scanned=0`。
- fast recover 后第一次全文检索不补扫历史 segment。
- 磁盘向量索引重启后不用 rebuild，向量搜索仍可用。
- 向量候选 join 前按 `(trace, span)` 去重，避免重复建向量导致重复结果。

### Node / Electron 嵌入式包

命令：

```bash
cd yitrace-node
npm run build
npm test
npm_config_cache=$PWD/.npm-cache npm run pack:verify
```

结果：

- native ESM/CJS build 通过。
- ESM + CJS tests 通过。
- clean consumer pack verify 通过。

说明：

- 第一次 `npm run pack:verify` 使用全局 `~/.npm/_cacache` 时遇到本机 npm cache 权限/重名错误，不是代码失败。
- 改用项目内临时 `npm_config_cache` 后通过。
- 临时 `.npm-cache/` 已清理；`dist/` 是脚本生成的已忽略打包产物。

新增覆盖：

- `open({ embedder })` 接收 embedding 回调。
- `ingest(events, { indexEmbeddings: true })` 会批量调用 `embedDocuments`。
- 默认 `search({ text })` 不调用 embedder，仍走 BM25。
- `search({ text, mode: "semantic" })` 调用 `embedQuery` 并走向量搜索。
- `search({ text, mode: "hybrid" })` 调用 `embedQuery` 并走 BM25 + 向量融合。
- `indexEmbeddings([{ traceId, spanId, text }])` 可通过 embedder 写入 span 向量。
- `indexEmbedding({ traceId, spanId, vector })` 可直接写入已有向量。
- 维度不匹配会报错。
- CJS 入口同样覆盖 `embedder`、`indexEmbedding` 和 semantic search。

### 跨包 package-mode eval

命令：

```bash
./scripts/package_mode_eval.sh
```

结果：全部通过。

覆盖：

- Python `yitrace` SDK tests：23 passed。
- Python SDK clean consumer 验证通过。
- TypeScript `@yitrace/trace-sdk` tests：8 passed。
- Rust `yitrace` SDK tests：6 passed。
- Python `yitrace-db` embedded tests：7 passed。
- Rust `yitrace-db` embedded tests：3 + 2 passed。
- Node `@yitrace/db` build + tests 通过。

## 文档更新

- `yitrace-node/README.md`：新增 embedding 回调用法、semantic/hybrid 搜索、写入向量、使用注意事项。
- `docs/API_REFERENCE.md`：新增 `Node embedding 回调` 小节，说明 engine 不调用模型，普通 text 搜索不触发模型。
- `docs/CURRENT_STATE.md`：补充 embedding 接入边界。
- `docs/design/2026-07-09_embedding-callback.md`：保存设计说明。

## 风险和注意事项

- 同一个 data dir 不应混用不同 embedding 模型。
- 维度不一致会被 wrapper 和磁盘图索引拒绝；同维度不同模型只能由调用方约束。
- `ingest(..., { indexEmbeddings: true })` 会等待模型调用，默认关闭，避免影响摄入延迟和成本。
- 重复给同一 span 建向量不会让查询结果重复展示，但会增加图索引节点，建议调用方对最终 span 文本建一次向量。
