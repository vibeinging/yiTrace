# yt-tokenizer-jieba

yiTrace 的 **cppjieba FFI 分词器** —— 通过 C ABI 接团队自有 cppjieba 包装库,实现引擎的 `Tokenizer` trait。

> 状态:**FFI 接缝就绪**,默认离线 mock(验证编组正确)。团队真库到位后 `--features link` 链接真 cppjieba。

## 为什么有这个

引擎默认分词器是**纯 Rust 词级分词**(`yt-engine` 的 `ChineseTokenizer`,jieba 全量词典内嵌)——开箱即用、零依赖。这个 crate 提供**接团队生产级 cppjieba** 的可选路径(如果真库的分词质量/性能更优)。

## 两条分词路(同一 trait,二选一)

| 分词器 | 在哪 | 何时用 |
|---|---|---|
| `ChineseTokenizer`(纯 Rust) | `yt-engine` | 默认,零依赖,jieba 全量词典 |
| `JiebaTokenizer`(FFI,本 crate) | 本 crate | 团队真 cppjieba 到位、想用生产级分词时 |

引擎通过 `CoordinatorBuilder::new().with_tokenizer(...)` 注入,换分词器倒排/评分一行不动。

## 构建

```bash
cargo test                    # 默认离线 mock,3 测试(验证 FFI 编组)
# 接真库:cargo build --features link  (需要团队 cppjieba 库在场)
```

## ABI 契约

见 `src/lib.rs` 顶部注释(`jieba_open` / `jieba_cut` / `jieba_free` / `jieba_close` 的 C 签名)。团队真库的符号名/加载接口若不同,调整 `extern "C"` 块即可。

## 许可证

MIT。cppjieba 本身为 MIT。
