//! yt-tokenizer-jieba —— 团队 **jieba 词级分词** 接入引擎的 [`yt_engine::Tokenizer`] 接缝。
//!
//! 设计同 Vortex 列式段：把外部（C/C++）依赖关在工作区外的独立 crate，引擎骨架保持零依赖、离线可编。
//! 分工 = **FFI 复用团队分词，倒排/BM25 评分仍是引擎里的自有 `Bm25TextIndex`**。换分词器只换这一层：
//!
//! ```ignore
//! use yt_engine::CoordinatorBuilder;
//! use yt_tokenizer_jieba::JiebaTokenizer;
//! let eng = CoordinatorBuilder::new()
//!     .with_tokenizer(Box::new(JiebaTokenizer::open("/opt/vexjieba/dict")?))
//!     .open_durable("/data/trace")?;
//! ```
//!
//! **C ABI 见 `ABI.md`（提案契约，按团队真实符号调整）。** 默认 `mock` feature 用 crate 内 Rust 桩提供
//! 这些符号，离线可编可测（验证 FFI 编组正确，**不是生产分词质量**）；生产用 `--features link` 接真库。

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_void};

use yt_engine::Tokenizer;

// 团队 cppjieba 包装库的 C ABI（契约见 ABI.md）。mock 下由本 crate 的 Rust 桩提供；link 下由真库提供。
extern "C" {
    fn vexjieba_open(dict_dir: *const c_char) -> *mut c_void;
    fn vexjieba_close(handle: *mut c_void);
    fn vexjieba_cut(handle: *mut c_void, text: *const c_char, text_len: usize) -> *mut c_char;
    fn vexjieba_free(s: *mut c_char);
}

/// 词典加载失败（路径含 NUL，或 `vexjieba_open` 返回 NULL）。
#[derive(Debug)]
pub enum JiebaError {
    BadDictPath,
    OpenFailed,
}

impl std::fmt::Display for JiebaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JiebaError::BadDictPath => write!(f, "词典路径非法（含 NUL 字节）"),
            JiebaError::OpenFailed => write!(f, "vexjieba_open 加载词典失败（路径不存在或词典损坏）"),
        }
    }
}
impl std::error::Error for JiebaError {}

/// 团队 jieba 词级分词器。持有 cppjieba 句柄，实现引擎的 `Tokenizer`。
pub struct JiebaTokenizer {
    handle: *mut c_void,
}

// 句柄加载后只读使用，`vexjieba_cut` 在同一句柄上可并发调用（见 ABI.md 线程安全约定）。
// 若团队实现非线程安全，去掉这两行并在外层加锁。
unsafe impl Send for JiebaTokenizer {}
unsafe impl Sync for JiebaTokenizer {}

impl JiebaTokenizer {
    /// 加载词典目录，返回分词器。
    pub fn open(dict_dir: &str) -> Result<Self, JiebaError> {
        let c_dir = CString::new(dict_dir).map_err(|_| JiebaError::BadDictPath)?;
        // SAFETY: c_dir 是有效 NUL 结尾串；vexjieba_open 失败返回 NULL，下面判空。
        let handle = unsafe { vexjieba_open(c_dir.as_ptr()) };
        if handle.is_null() {
            return Err(JiebaError::OpenFailed);
        }
        Ok(Self { handle })
    }
}

impl Drop for JiebaTokenizer {
    fn drop(&mut self) {
        // SAFETY: handle 来自 vexjieba_open 且非空（open 已判），只在此释放一次。
        unsafe { vexjieba_close(self.handle) };
    }
}

impl Tokenizer for JiebaTokenizer {
    fn tokenize(&self, text: &str) -> Vec<String> {
        if text.is_empty() {
            return Vec::new();
        }
        // SAFETY: 传 (ptr, len)，库按 len 读 UTF-8，不依赖 NUL 结尾。返回 NULL 视为分词失败 → 空，不 panic。
        let out = unsafe { vexjieba_cut(self.handle, text.as_ptr() as *const c_char, text.len()) };
        if out.is_null() {
            return Vec::new();
        }
        // SAFETY: out 是库返回的 NUL 结尾串；先拷成 owned，再用 vexjieba_free 归还所有权。
        let toks = unsafe { CStr::from_ptr(out) }
            .to_string_lossy()
            .split('\n')
            .filter(|w| !w.is_empty())
            .map(|w| w.to_string())
            .collect();
        unsafe { vexjieba_free(out) };
        toks
    }
}

// ───────────────────────── 离线 Rust 桩（mock feature） ─────────────────────────
// 提供上面 extern 块声明的 C 符号，使本 crate 在没有团队真库时也能编译/测试，验证 FFI 编组正确。
// 分词策略刻意做成「CJK 按单字、ASCII 按串小写」——与引擎默认的 bigram 不同，便于测试区分是哪套分词在跑。
// 这不是生产分词质量；真分词由 cppjieba 在 `--features link` 下提供。
#[cfg(feature = "mock")]
mod mock {
    use super::*;

    #[no_mangle]
    unsafe extern "C" fn vexjieba_open(_dict_dir: *const c_char) -> *mut c_void {
        // 用一次堆分配充当句柄，让 close 的释放路径也被真实走到。
        Box::into_raw(Box::new(0u8)) as *mut c_void
    }

    #[no_mangle]
    unsafe extern "C" fn vexjieba_close(handle: *mut c_void) {
        if !handle.is_null() {
            drop(Box::from_raw(handle as *mut u8));
        }
    }

    #[no_mangle]
    unsafe extern "C" fn vexjieba_cut(_handle: *mut c_void, text: *const c_char, text_len: usize) -> *mut c_char {
        let bytes = std::slice::from_raw_parts(text as *const u8, text_len);
        let s = String::from_utf8_lossy(bytes);
        let mut words: Vec<String> = Vec::new();
        let mut ascii = String::new();
        let flush = |ascii: &mut String, out: &mut Vec<String>| {
            if !ascii.is_empty() {
                out.push(std::mem::take(ascii).to_lowercase());
            }
        };
        for c in s.chars() {
            if ('\u{4e00}'..='\u{9fff}').contains(&c) {
                flush(&mut ascii, &mut words);
                words.push(c.to_string()); // CJK 单字成词（区别于 bigram）
            } else if c.is_alphanumeric() {
                ascii.push(c);
            } else {
                flush(&mut ascii, &mut words);
            }
        }
        flush(&mut ascii, &mut words);
        // CString::new 失败（含 NUL）几乎不会发生（词内无 NUL）；兜底返回空串。
        CString::new(words.join("\n")).unwrap_or_default().into_raw()
    }

    #[no_mangle]
    unsafe extern "C" fn vexjieba_free(s: *mut c_char) {
        if !s.is_null() {
            drop(CString::from_raw(s));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // mock 桩下验证：open→cut→free→close 整条 FFI 链编组正确，UTF-8 进出无损。
    #[test]
    fn ffi_round_trips_utf8_through_c_boundary() {
        let tok = JiebaTokenizer::open("any/dict/path").expect("mock open 不失败");
        // 中文按单字（mock 策略），ASCII 按串小写。
        assert_eq!(tok.tokenize("风控GPT"), vec!["风", "控", "gpt"]);
        // 标点切分、空段不产空 token。
        assert_eq!(tok.tokenize("盗刷,转账"), vec!["盗", "刷", "转", "账"]);
        // 空输入不过 FFI、直接空。
        assert!(tok.tokenize("").is_empty());
    }

    // 作为 Box<dyn Tokenizer> 用（引擎注入口要的就是这个）。
    #[test]
    fn usable_as_boxed_tokenizer() {
        let tok: Box<dyn Tokenizer> = Box::new(JiebaTokenizer::open("d").unwrap());
        assert_eq!(tok.tokenize("交易"), vec!["交", "易"]);
    }

    // 反复 open/drop 不崩（close 释放路径）。
    #[test]
    fn open_and_drop_repeatedly() {
        for _ in 0..1000 {
            let t = JiebaTokenizer::open("d").unwrap();
            let _ = t.tokenize("风控盗刷转账");
        }
    }
}
