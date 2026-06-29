//! 结构化日志（§3.2 可观测运维）。
//!
//! 零依赖、key=value 格式（类 OpenTelemetry attribute）、可配置 sink。
//! 故障定位：一次故障能从日志重建"那段时间发生了什么"。
//!
//! 格式（每行一条）：
//!   `ts=2026-06-29T12:34:56.789Z level=INFO event=flush seg=3 rows=1500 version=2`
//!
//! 用法：
//!   use crate::olog::{log, Level};
//!   log(Level::Info, "event", &[("seg", &seg_id), ("rows", &n)]);
//!
//! 关键路径发日志：ingest / flush / compaction / reclaim / recover / 崩溃 / search(慢查询)。

use std::io::Write;
use std::sync::OnceLock;
use std::sync::Mutex;

/// 日志级别。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Level::Debug => "DEBUG",
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
        }
    }
}

/// 日志输出 sink。默认 stderr；可设文件（生产用）。
enum Sink {
    Stderr,
    File(Mutex<std::fs::File>),
}

static SINK: OnceLock<Sink> = OnceLock::new();
static MIN_LEVEL: OnceLock<Level> = OnceLock::new();

/// 设置日志输出到文件（生产用）。默认是 stderr。
/// 重复调用只有第一次生效（OnceLock）。
pub fn set_file(path: &std::path::Path) {
    if SINK.get().is_some() {
        return;
    }
    if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = SINK.set(Sink::File(Mutex::new(f)));
    }
}

/// 设置最低日志级别（低于的不输出）。默认 Info。
pub fn set_level(level: Level) {
    let _ = MIN_LEVEL.set(level);
}

fn min_level() -> Level {
    *MIN_LEVEL.get().unwrap_or(&Level::Info)
}

/// 一个可日志化的属性值（key=value 里的 value）。
/// 用 trait 而非泛型，避免每处都写 Display bound。
pub trait LogVal {
    fn write_kv(&self, key: &str, out: &mut String);
}

// 常用类型的实现。
impl LogVal for u64 {
    fn write_kv(&self, key: &str, out: &mut String) {
        out.push_str(key);
        out.push('=');
        out.push_str(&self.to_string());
    }
}
impl LogVal for usize {
    fn write_kv(&self, key: &str, out: &mut String) {
        out.push_str(key);
        out.push('=');
        out.push_str(&self.to_string());
    }
}
impl LogVal for u32 {
    fn write_kv(&self, key: &str, out: &mut String) {
        out.push_str(key);
        out.push('=');
        out.push_str(&self.to_string());
    }
}
impl LogVal for u8 {
    fn write_kv(&self, key: &str, out: &mut String) {
        out.push_str(key);
        out.push('=');
        out.push_str(&self.to_string());
    }
}
impl LogVal for i64 {
    fn write_kv(&self, key: &str, out: &mut String) {
        out.push_str(key);
        out.push('=');
        out.push_str(&self.to_string());
    }
}
impl LogVal for &str {
    fn write_kv(&self, key: &str, out: &mut String) {
        out.push_str(key);
        out.push('=');
        // 值含空格/特殊字符加引号（简单处理：总是加引号，安全）。
        out.push('"');
        // 转义内部的 " 和 \
        for ch in self.chars() {
            match ch {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                _ => out.push(ch),
            }
        }
        out.push('"');
    }
}
impl LogVal for String {
    fn write_kv(&self, key: &str, out: &mut String) {
        self.as_str().write_kv(key, out)
    }
}
impl LogVal for bool {
    fn write_kv(&self, key: &str, out: &mut String) {
        out.push_str(key);
        out.push('=');
        out.push_str(if *self { "true" } else { "false" });
    }
}
// 引擎 newtype（yt_core::ids）—— 按内部 u64 日志化。
impl LogVal for yt_core::ids::WalLsn {
    fn write_kv(&self, key: &str, out: &mut String) {
        self.get().write_kv(key, out);
    }
}
impl LogVal for yt_core::ids::SegmentId {
    fn write_kv(&self, key: &str, out: &mut String) {
        self.get().write_kv(key, out);
    }
}

/// 发一条结构化日志。
///
/// ```ignore
/// olog::log(olog::Level::Info, "flush", &[("seg", &seg_id), ("rows", &n)]);
/// // → ts=... level=INFO event=flush seg=3 rows=1500
/// ```
pub fn log(level: Level, event: &str, attrs: &[(&str, &dyn LogVal)]) {
    if (level as u8) < (min_level() as u8) {
        return;
    }
    let mut line = String::with_capacity(128);
    // 时间戳（RFC3339，毫秒精度——够用，零依赖用 SystemTime 算）。
    line.push_str("ts=");
    line.push_str(&now_rfc3339_millis());
    line.push_str(" level=");
    line.push_str(level.as_str());
    line.push_str(" event=\"");
    line.push_str(event);
    line.push('"');
    for (k, v) in attrs {
        line.push(' ');
        v.write_kv(k, &mut line);
    }
    line.push('\n');

    match SINK.get() {
        Some(Sink::File(m)) => {
            if let Ok(mut f) = m.lock() {
                let _ = f.write_all(line.as_bytes());
            }
        }
        Some(Sink::Stderr) | None => {
            let _ = std::io::stderr().write_all(line.as_bytes());
        }
    }
}

/// 当前时间 RFC3339（毫秒精度）。零依赖手算。
fn now_rfc3339_millis() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let millis = now.subsec_millis();

    // 把 epoch 秒拆成日期（civil-from-days 算法，零依赖）。
    let days = (secs / 86400) as i64;
    let time_of_day = secs % 86400;
    let hour = time_of_day / 3600;
    let min = (time_of_day % 3600) / 60;
    let sec = time_of_day % 60;

    // civil_from_days（Howard Hinnant 算法）。
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u64;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u64;
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02}T{hour:02}:{min:02}:{sec:02}.{millis:03}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_kv_correctly() {
        let mut s = String::new();
        let v: u64 = 42;
        LogVal::write_kv(&v, "seg", &mut s);
        assert_eq!(s, "seg=42");

        let mut s = String::new();
        LogVal::write_kv(&"hello world", "msg", &mut s);
        assert_eq!(s, "msg=\"hello world\"");
    }

    #[test]
    fn escapes_special_chars_in_strings() {
        let mut s = String::new();
        LogVal::write_kv(&"a\"b\\c\nd", "msg", &mut s);
        assert_eq!(s, "msg=\"a\\\"b\\\\c\\nd\"");
    }

    #[test]
    fn rfc3339_is_parseable_format() {
        let ts = now_rfc3339_millis();
        // 格式 YYYY-MM-DDTHH:MM:SS.mmmZ
        assert_eq!(ts.len(), 24);
        assert_eq!(ts.as_bytes()[4], b'-');
        assert_eq!(ts.as_bytes()[7], b'-');
        assert_eq!(ts.as_bytes()[10], b'T');
        assert_eq!(ts.as_bytes()[13], b':');
        assert_eq!(ts.as_bytes()[16], b':');
        assert_eq!(ts.as_bytes()[19], b'.');
        assert_eq!(ts.as_bytes()[23], b'Z');
    }
}
