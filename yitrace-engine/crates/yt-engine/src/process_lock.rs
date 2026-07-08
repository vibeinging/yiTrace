//! process_lock.rs —— data dir 内部的跨进程互斥锁。
//!
//! 引擎本体保持 std-only，不能拉 `fs2`/`libc` 这类依赖。这里用 `create_dir` 的原子性实现
//! 进程间互斥：抢到 `<data-dir>/<name>.lock.d/` 的进程持锁，释放时删目录。锁目录里写 owner
//! 文件，便于诊断；Unix 下用 `kill(pid, 0)` 判断明显死亡的 stale lock 并清掉。

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

static NEXT_READER_PIN: AtomicU64 = AtomicU64::new(0);

pub struct ProcessLockManager {
    dir: PathBuf,
    timeout: Duration,
}

pub struct ProcessLockGuard {
    lock_dir: PathBuf,
}

pub struct ProcessReaderGuard {
    pin_path: PathBuf,
}

impl ProcessLockManager {
    pub fn new(dir: impl AsRef<Path>) -> Self {
        Self {
            dir: dir.as_ref().to_path_buf(),
            timeout: Duration::from_secs(30),
        }
    }

    pub fn acquire(&self, name: &str) -> io::Result<ProcessLockGuard> {
        let lock_dir = self.dir.join(format!(".yitrace.{name}.lock.d"));
        let start = Instant::now();
        loop {
            match fs::create_dir(&lock_dir) {
                Ok(()) => {
                    write_owner(&lock_dir, &self.dir)?;
                    return Ok(ProcessLockGuard { lock_dir });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    if clear_stale_lock(&lock_dir) {
                        continue;
                    }
                    if start.elapsed() >= self.timeout {
                        return Err(io::Error::new(
                            io::ErrorKind::WouldBlock,
                            format!(
                                "timed out waiting for yiTrace {name} lock at {}{}",
                                lock_dir.display(),
                                owner_suffix(&lock_dir)
                            ),
                        ));
                    }
                    thread::sleep(Duration::from_millis(20));
                }
                Err(e) => return Err(e),
            }
        }
    }

    pub fn try_acquire(&self, name: &str) -> io::Result<Option<ProcessLockGuard>> {
        let lock_dir = self.dir.join(format!(".yitrace.{name}.lock.d"));
        match fs::create_dir(&lock_dir) {
            Ok(()) => {
                write_owner(&lock_dir, &self.dir)?;
                Ok(Some(ProcessLockGuard { lock_dir }))
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                if clear_stale_lock(&lock_dir) {
                    return self.try_acquire(name);
                }
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    pub fn pin_reader(&self) -> io::Result<ProcessReaderGuard> {
        let readers_dir = self.dir.join(".yitrace.readers");
        fs::create_dir_all(&readers_dir)?;
        self.clear_stale_readers();
        for _ in 0..1024 {
            let n = NEXT_READER_PIN.fetch_add(1, Ordering::Relaxed);
            let pin_path = readers_dir.join(format!("reader-{}-{n}.json", std::process::id()));
            match fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&pin_path)
            {
                Ok(mut f) => {
                    f.write_all(owner_json(&self.dir).as_bytes())?;
                    f.sync_all()?;
                    return Ok(ProcessReaderGuard { pin_path });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate yiTrace reader pin file",
        ))
    }

    pub fn has_active_readers(&self) -> bool {
        self.clear_stale_readers();
        let readers_dir = self.dir.join(".yitrace.readers");
        match fs::read_dir(&readers_dir) {
            Ok(mut entries) => entries.any(|entry| {
                entry
                    .ok()
                    .and_then(|e| e.file_type().ok().map(|ty| (e, ty)))
                    .map(|(_, ty)| ty.is_file())
                    .unwrap_or(false)
            }),
            Err(_) => false,
        }
    }

    fn clear_stale_readers(&self) {
        let readers_dir = self.dir.join(".yitrace.readers");
        let Ok(entries) = fs::read_dir(&readers_dir) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(ty) = entry.file_type() else {
                continue;
            };
            if !ty.is_file() {
                continue;
            }
            let path = entry.path();
            let Ok(owner) = fs::read_to_string(&path) else {
                continue;
            };
            let Some(pid) = parse_json_u32(&owner, "pid") else {
                continue;
            };
            if !process_is_alive(pid) {
                let _ = fs::remove_file(path);
            }
        }
    }
}

impl Drop for ProcessLockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.lock_dir);
    }
}

impl Drop for ProcessReaderGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.pin_path);
    }
}

fn write_owner(lock_dir: &Path, data_dir: &Path) -> io::Result<()> {
    let owner = owner_json(data_dir);
    let mut f = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(lock_dir.join("owner.json"))?;
    f.write_all(owner.as_bytes())?;
    f.sync_all()
}

fn owner_json(data_dir: &Path) -> String {
    let created_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let executable = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    format!(
        "{{\"pid\":{},\"host\":\"{}\",\"created_unix_ms\":{},\"data_dir\":\"{}\",\"executable\":\"{}\"}}\n",
        std::process::id(),
        json_escape(&host_name()),
        created_unix_ms,
        json_escape(&data_dir.display().to_string()),
        json_escape(&executable),
    )
}

fn owner_suffix(lock_dir: &Path) -> String {
    match fs::read_to_string(lock_dir.join("owner.json")) {
        Ok(text) if !text.trim().is_empty() => format!("; owner: {}", text.trim()),
        Ok(_) => "; owner: <empty>".to_string(),
        Err(e) => format!("; owner unreadable: {e}"),
    }
}

fn clear_stale_lock(lock_dir: &Path) -> bool {
    let owner = match fs::read_to_string(lock_dir.join("owner.json")) {
        Ok(text) => text,
        Err(_) => return false,
    };
    let Some(pid) = parse_json_u32(&owner, "pid") else {
        return false;
    };
    if process_is_alive(pid) {
        return false;
    }
    fs::remove_dir_all(lock_dir).is_ok()
}

fn parse_json_u32(text: &str, key: &str) -> Option<u32> {
    let needle = format!("\"{key}\":");
    let pos = text.find(&needle)? + needle.len();
    let tail = &text[pos..];
    let digits: String = tail
        .chars()
        .skip_while(|c| c.is_ascii_whitespace())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    unsafe { kill(pid as i32, 0) == 0 }
}

#[cfg(not(unix))]
fn process_is_alive(_pid: u32) -> bool {
    true
}

fn host_name() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "unknown".to_string())
}

fn json_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}
