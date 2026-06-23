# 团队 jieba C ABI 契约（提案 — 按真实符号调整）

本 crate 按下面这套 C ABI 调用团队 cppjieba 包装库。**这是提案契约**：若团队库现有符号名/签名不同，
改 `src/lib.rs` 的 `extern "C"` 块与 `JiebaTokenizer` 调用即可，trait 接缝（`yt_engine::Tokenizer`）不变。

```c
// 加载词典，返回不透明句柄；失败返回 NULL。dict_dir 为 NUL 结尾 UTF-8 路径。
// 句柄在多线程下只读使用（cppjieba 的 Cut 加载后线程安全），本 crate 据此实现 Send+Sync。
void* vexjieba_open(const char* dict_dir);

// 释放句柄。
void  vexjieba_close(void* handle);

// 对 text[0..text_len]（UTF-8，不要求 NUL 结尾）做词级切分，
// 返回以 '\n' 连接各词的 NUL 结尾 C 字符串；空输入返回空串（非 NULL）；失败返回 NULL。
// 返回的指针由调用方用 vexjieba_free 释放。
char* vexjieba_cut(void* handle, const char* text, size_t text_len);

// 释放 vexjieba_cut 返回的字符串。
void  vexjieba_free(char* s);
```

## 约定要点

- **编码**：进出都是 UTF-8。`vexjieba_cut` 的输入按 (ptr, len) 传，不依赖 NUL 结尾。
- **分隔符**：输出用 `\n` 连接词。词内部不含 `\n`（cppjieba 不会产出含换行的词）。
- **所有权**：`vexjieba_cut` 的返回串归调用方，必须 `vexjieba_free`；句柄归调用方，必须 `vexjieba_close`。
- **线程安全**：`vexjieba_cut` 在同一句柄上可被多线程并发调用（只读）。本 crate 的 `JiebaTokenizer`
  据此 `unsafe impl Send + Sync`；若团队实现非线程安全，需去掉并在外层加锁。
- **失败语义**：`open` 失败（词典缺失）返回 NULL → `JiebaTokenizer::open` 返回 Err；
  `cut` 返回 NULL → 该次分词降级为空（不 panic）。

## 构建

- 离线/CI（默认）：`cargo test`（mock feature，Rust 桩提供上述符号，验证编组逻辑）。
- 生产：`cargo build --no-default-features --features link`，并设 `VEXJIEBA_LIB_DIR` 指向库目录。
