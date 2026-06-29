# yt-segstore-vortex

yiTrace 的 **Vortex 列式段存储** —— 实现引擎的 `SegmentStore` trait,把不可变段存成 Vortex 列式格式。

> 状态:**功能可用**(谓词下推 + 投影下推),隔离在 `yitrace-engine` 工作区之外,目的是把 Vortex/Arrow 重依赖隔离在一个 crate,引擎骨架保持零外部依赖。

## 为什么隔离

引擎工作区(`yitrace-engine/`)刻意只用标准库、`cargo test --offline` 离线可过。Vortex 拖进 arrow+zstd 一大坨依赖,所以单独建 crate,引擎通过 `SegmentStore` trait 注入。

## 能力

- 列式段写读往返(StructArray 列布局)
- **谓词下推**:`scan().with_filter(...)`(时间范围过滤推进文件扫描层)
- **投影下推**:`scan().with_projection(select(...))`(聚合只读窄列、跳过大文本列)
- 默认 BtrBlocks + FSST 压缩(实测重复文本压到 <1/5)

## 构建

```bash
cargo test                    # 首次需联网(拉 vortex + arrow + zstd,约 200 crate)
# 离线构建:vendoring 后 cargo build --offline
```

要求 **Rust ≥ 1.91**(Vortex 0.75 要求)。

## 许可证

MIT。Vortex 本身为 Apache-2.0。
