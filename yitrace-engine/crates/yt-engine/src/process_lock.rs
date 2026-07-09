//! process_lock.rs —— data dir 内部的跨进程互斥锁。
//!
//! 引擎本体保持 std-only，不能拉 `fs2`/`libc` 这类依赖。这里用 `create_dir` 的原子性实现
//! 进程间互斥：抢到 `<data-dir>/<name>.lock.d/` 的进程持锁，释放时删目录。锁目录里写 owner
//! 文件，便于诊断；Unix 下用 `kill(pid, 0)` 判断明显死亡的 stale lock 并清掉。

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

static NEXT_READER_PIN: AtomicU64 = AtomicU64::new(0);

pub struct ProcessLockManager {
    dir: PathBuf,
    timeout: Duration,
    metrics: Arc<ProcessLockMetrics>,
}

pub struct ProcessLockGuard {
    lock_dir: PathBuf,
}

pub struct ProcessReaderGuard {
    pin_path: PathBuf,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessLockMetricsSnapshot {
    pub acquire_count: u64,
    pub try_acquire_count: u64,
    pub wait_count: u64,
    pub active_wait_count: u64,
    pub wait_ns: u64,
    pub timeout_count: u64,
    pub try_busy_count: u64,
    pub stale_lock_cleared_count: u64,
    pub reader_pin_count: u64,
    pub stale_reader_cleared_count: u64,
}

#[derive(Default)]
struct ProcessLockMetrics {
    acquire_count: AtomicU64,
    try_acquire_count: AtomicU64,
    wait_count: AtomicU64,
    active_wait_count: AtomicU64,
    wait_ns: AtomicU64,
    timeout_count: AtomicU64,
    try_busy_count: AtomicU64,
    stale_lock_cleared_count: AtomicU64,
    reader_pin_count: AtomicU64,
    stale_reader_cleared_count: AtomicU64,
}

impl ProcessLockManager {
    pub fn new(dir: impl AsRef<Path>) -> Self {
        Self {
            dir: dir.as_ref().to_path_buf(),
            timeout: Duration::from_secs(30),
            metrics: Arc::new(ProcessLockMetrics::default()),
        }
    }

    pub fn acquire(&self, name: &str) -> io::Result<ProcessLockGuard> {
        self.metrics.acquire_count.fetch_add(1, Ordering::Relaxed);
        let lock_dir = self.dir.join(format!(".yitrace.{name}.lock.d"));
        let start = Instant::now();
        let mut wait_started: Option<Instant> = None;
        loop {
            match fs::create_dir(&lock_dir) {
                Ok(()) => {
                    if let Some(wait_started) = wait_started {
                        self.finish_wait(wait_started);
                    }
                    write_owner(&lock_dir, &self.dir)?;
                    return Ok(ProcessLockGuard { lock_dir });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    if clear_stale_lock(&lock_dir) {
                        self.metrics
                            .stale_lock_cleared_count
                            .fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    if wait_started.is_none() {
                        self.metrics.wait_count.fetch_add(1, Ordering::Relaxed);
                        self.metrics
                            .active_wait_count
                            .fetch_add(1, Ordering::Relaxed);
                        wait_started = Some(Instant::now());
                    }
                    if start.elapsed() >= self.timeout {
                        if let Some(wait_started) = wait_started {
                            self.finish_wait(wait_started);
                        }
                        self.metrics.timeout_count.fetch_add(1, Ordering::Relaxed);
                        return Err(io::Error::new(
                            io::ErrorKind::WouldBlock,
                            format!(
                                "timed out after {}ms waiting for yiTrace {name} lock at {}; another local process is using this embedded data dir. If this keeps happening, inspect owner.json or YiTraceRuntime.health()['lock']{}",
                                self.timeout.as_millis(),
                                lock_dir.display(),
                                owner_suffix(&lock_dir)
                            ),
                        ));
                    }
                    thread::sleep(Duration::from_millis(20));
                }
                Err(e) => {
                    if let Some(wait_started) = wait_started {
                        self.finish_wait(wait_started);
                    }
                    return Err(e);
                }
            }
        }
    }

    pub fn try_acquire(&self, name: &str) -> io::Result<Option<ProcessLockGuard>> {
        self.metrics
            .try_acquire_count
            .fetch_add(1, Ordering::Relaxed);
        let lock_dir = self.dir.join(format!(".yitrace.{name}.lock.d"));
        loop {
            match fs::create_dir(&lock_dir) {
                Ok(()) => {
                    write_owner(&lock_dir, &self.dir)?;
                    return Ok(Some(ProcessLockGuard { lock_dir }));
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    if clear_stale_lock(&lock_dir) {
                        self.metrics
                            .stale_lock_cleared_count
                            .fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    self.metrics.try_busy_count.fetch_add(1, Ordering::Relaxed);
                    return Ok(None);
                }
                Err(e) => return Err(e),
            }
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
                    self.metrics
                        .reader_pin_count
                        .fetch_add(1, Ordering::Relaxed);
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
                if fs::remove_file(path).is_ok() {
                    self.metrics
                        .stale_reader_cleared_count
                        .fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }

    pub fn metrics_snapshot(&self) -> ProcessLockMetricsSnapshot {
        ProcessLockMetricsSnapshot {
            acquire_count: self.metrics.acquire_count.load(Ordering::Relaxed),
            try_acquire_count: self.metrics.try_acquire_count.load(Ordering::Relaxed),
            wait_count: self.metrics.wait_count.load(Ordering::Relaxed),
            active_wait_count: self.metrics.active_wait_count.load(Ordering::Relaxed),
            wait_ns: self.metrics.wait_ns.load(Ordering::Relaxed),
            timeout_count: self.metrics.timeout_count.load(Ordering::Relaxed),
            try_busy_count: self.metrics.try_busy_count.load(Ordering::Relaxed),
            stale_lock_cleared_count: self
                .metrics
                .stale_lock_cleared_count
                .load(Ordering::Relaxed),
            reader_pin_count: self.metrics.reader_pin_count.load(Ordering::Relaxed),
            stale_reader_cleared_count: self
                .metrics
                .stale_reader_cleared_count
                .load(Ordering::Relaxed),
        }
    }

    fn finish_wait(&self, wait_started: Instant) {
        self.metrics
            .wait_ns
            .fetch_add(duration_ns(wait_started.elapsed()), Ordering::Relaxed);
        self.metrics
            .active_wait_count
            .fetch_sub(1, Ordering::Relaxed);
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

fn duration_ns(duration: Duration) -> u64 {
    duration.as_nanos().min(u128::from(u64::MAX)) as u64
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
