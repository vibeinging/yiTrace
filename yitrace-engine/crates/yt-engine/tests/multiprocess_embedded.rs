use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use yt_core::event::{EventIdentity, EventType};
use yt_core::fold::SpanFields;
use yt_core::ids::SegmentId;
use yt_wal::WalRecord;

use yt_engine::{NewTraceAnnotation, TraceAnnotationFilter, WriteCoordinator};

const TEST_NAME: &str = "multiple_processes_open_same_data_dir_and_write";
const READER_TEST_NAME: &str = "reclaim_waits_for_cross_process_reader_pin";
const STRESS_TEST_NAME: &str = "many_processes_incrementally_refresh_wal_tail";
const VECTOR_TEST_NAME: &str = "multiple_processes_write_vectors_and_reopen_searches_them";

#[test]
fn multiple_processes_open_same_data_dir_and_write() {
    if let Some(dir) = std::env::var_os("YT_MULTIPROCESS_EMBEDDED_HELPER_DIR") {
        let trace = std::env::var("YT_MULTIPROCESS_EMBEDDED_HELPER_TRACE").unwrap();
        helper_write(Path::new(&dir), &trace);
        return;
    }

    let dir = fresh_dir("multiprocess_embedded");
    let writers = vec![
        ("proc-a", spawn_writer(&dir, "proc-a")),
        ("proc-b", spawn_writer(&dir, "proc-b")),
    ];
    for (trace, writer) in writers {
        wait_writer(trace, writer);
    }

    let coord = WriteCoordinator::open_durable(&dir).unwrap();
    coord.recover();
    let snap = coord.pin_snapshot();
    let traces = coord
        .read_spans(&snap)
        .into_iter()
        .filter_map(|span| span.external_trace_id)
        .collect::<BTreeSet<_>>();

    assert_eq!(
        traces,
        BTreeSet::from(["proc-a".to_string(), "proc-b".to_string()]),
        "两个独立进程写入同一个 data dir 后，父进程应能读到两条 trace"
    );
    let labels = coord
        .annotations(&TraceAnnotationFilter {
            tenant_id: Some(1),
            ..TraceAnnotationFilter::default()
        })
        .into_iter()
        .map(|annotation| annotation.label)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        labels,
        BTreeSet::from([
            "annotation-proc-a".to_string(),
            "annotation-proc-b".to_string()
        ]),
        "metadata 写入也必须跨进程刷新，否则 annotation id 或 metadata.dat 会互相覆盖"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn multiple_processes_write_vectors_and_reopen_searches_them() {
    if let Some(dir) = std::env::var_os("YT_MULTIPROCESS_EMBEDDED_VECTOR_DIR") {
        let trace = std::env::var("YT_MULTIPROCESS_EMBEDDED_VECTOR_TRACE").unwrap();
        let x = std::env::var("YT_MULTIPROCESS_EMBEDDED_VECTOR_X")
            .unwrap()
            .parse::<f32>()
            .unwrap();
        helper_write_vector(Path::new(&dir), &trace, x);
        return;
    }

    let dir = fresh_dir("multiprocess_vector");
    let writers = vec![
        ("vec-a", spawn_vector_writer(&dir, "vec-a", 0.0)),
        ("vec-b", spawn_vector_writer(&dir, "vec-b", 10.0)),
    ];
    for (trace, writer) in writers {
        wait_writer(trace, writer);
    }

    let coord = WriteCoordinator::open_durable(&dir).unwrap();
    coord.recover();
    let snap = coord.pin_snapshot();
    let hits = coord.search_similar(&snap, &[9.9, 0.0, 0.0], 2);
    assert!(
        hits.iter()
            .any(|(span, _)| span.external_trace_id.as_deref() == Some("vec-b")),
        "父进程重开后应能搜到另一个进程写入并持久化的向量"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn many_processes_incrementally_refresh_wal_tail() {
    if let Some(dir) = std::env::var_os("YT_MULTIPROCESS_EMBEDDED_STRESS_DIR") {
        let worker = std::env::var("YT_MULTIPROCESS_EMBEDDED_STRESS_WORKER").unwrap();
        let count = std::env::var("YT_MULTIPROCESS_EMBEDDED_STRESS_COUNT")
            .unwrap()
            .parse::<usize>()
            .unwrap();
        helper_write_many(Path::new(&dir), &worker, count);
        return;
    }

    let dir = fresh_dir("multiprocess_stress");
    let workers = 4usize;
    let count = 32usize;
    let mut children = Vec::new();
    for worker in 0..workers {
        let label = format!("worker-{worker}");
        children.push((label.clone(), spawn_stress_writer(&dir, &label, count)));
    }
    for (label, child) in children {
        wait_writer(&label, child);
    }

    let coord = WriteCoordinator::open_durable(&dir).unwrap();
    coord.recover();
    let snap = coord.pin_snapshot();
    let traces = coord
        .read_spans(&snap)
        .into_iter()
        .filter_map(|span| span.external_trace_id)
        .collect::<BTreeSet<_>>();

    let mut expected = BTreeSet::new();
    for worker in 0..workers {
        for i in 0..count {
            expected.insert(format!("stress-worker-{worker}-{i}"));
        }
    }
    assert_eq!(
        traces, expected,
        "多个进程长期追加 WAL 后，增量刷新路径必须不丢、不重、不覆盖"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn reclaim_waits_for_cross_process_reader_pin() {
    if let Some(dir) = std::env::var_os("YT_MULTIPROCESS_EMBEDDED_READER_DIR") {
        let ready =
            PathBuf::from(std::env::var_os("YT_MULTIPROCESS_EMBEDDED_READER_READY").unwrap());
        helper_hold_reader(Path::new(&dir), &ready);
        return;
    }

    let dir = fresh_dir("multiprocess_reader_pin");
    let coord = WriteCoordinator::open_durable(&dir).unwrap();
    coord.ingest(vec![record("reader-a")]);
    coord.flush_memtable();
    coord.ingest(vec![record("reader-b")]);
    coord.flush_memtable();

    let ready = dir.join("reader-ready");
    let reader = spawn_reader(&dir, &ready);
    wait_for_file(&ready);

    coord.commit_compaction(&[SegmentId::new(1), SegmentId::new(2)]);
    assert_eq!(coord.dead_count(), 2);
    assert_eq!(
        coord.reclaim(),
        0,
        "另一个进程还 pin 着快照时，不能物理删除旧段"
    );

    wait_writer("reader", reader);
    assert_eq!(coord.reclaim(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

fn spawn_writer(dir: &Path, trace: &str) -> Child {
    Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg(TEST_NAME)
        .arg("--nocapture")
        .env("YT_MULTIPROCESS_EMBEDDED_HELPER_DIR", dir)
        .env("YT_MULTIPROCESS_EMBEDDED_HELPER_TRACE", trace)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn spawn_reader(dir: &Path, ready: &Path) -> Child {
    Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg(READER_TEST_NAME)
        .arg("--nocapture")
        .env("YT_MULTIPROCESS_EMBEDDED_READER_DIR", dir)
        .env("YT_MULTIPROCESS_EMBEDDED_READER_READY", ready)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn spawn_stress_writer(dir: &Path, worker: &str, count: usize) -> Child {
    Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg(STRESS_TEST_NAME)
        .arg("--nocapture")
        .env("YT_MULTIPROCESS_EMBEDDED_STRESS_DIR", dir)
        .env("YT_MULTIPROCESS_EMBEDDED_STRESS_WORKER", worker)
        .env("YT_MULTIPROCESS_EMBEDDED_STRESS_COUNT", count.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn spawn_vector_writer(dir: &Path, trace: &str, x: f32) -> Child {
    Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg(VECTOR_TEST_NAME)
        .arg("--nocapture")
        .env("YT_MULTIPROCESS_EMBEDDED_VECTOR_DIR", dir)
        .env("YT_MULTIPROCESS_EMBEDDED_VECTOR_TRACE", trace)
        .env("YT_MULTIPROCESS_EMBEDDED_VECTOR_X", x.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn wait_writer(trace: &str, writer: Child) {
    let output = writer.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "child writer {trace} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn wait_for_file(path: &Path) {
    let start = Instant::now();
    while !path.exists() {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "timed out waiting for {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn helper_write(dir: &Path, trace: &str) {
    let coord = WriteCoordinator::open_durable(dir).unwrap();
    coord.recover();
    coord.ingest(vec![record(trace)]);
    coord.flush_memtable();
    coord.add_annotation(
        NewTraceAnnotation {
            trace_id: stable_id(trace),
            label: format!("annotation-{trace}"),
            ..NewTraceAnnotation::default()
        },
        Some(1),
    );
    let snap = coord.pin_snapshot();
    assert!(
        coord
            .read_spans(&snap)
            .iter()
            .any(|span| span.external_trace_id.as_deref() == Some(trace)),
        "helper should read its own committed trace"
    );
}

fn helper_hold_reader(dir: &Path, ready: &Path) {
    let coord = WriteCoordinator::open_durable(dir).unwrap();
    coord.recover();
    let snap = coord.pin_snapshot();
    std::fs::write(ready, b"ready").unwrap();
    thread::sleep(Duration::from_millis(800));
    assert!(
        coord.read_spans(&snap).len() >= 2,
        "reader should keep its pinned snapshot readable"
    );
}

fn helper_write_many(dir: &Path, worker: &str, count: usize) {
    let coord = WriteCoordinator::open_durable(dir).unwrap();
    coord.recover();
    for i in 0..count {
        let trace = format!("stress-{worker}-{i}");
        coord.ingest(vec![record(&trace)]);
        if i % 4 == 0 {
            thread::sleep(Duration::from_millis(1));
        }
    }
    coord.flush_memtable();
}

fn helper_write_vector(dir: &Path, trace: &str, x: f32) {
    let coord = WriteCoordinator::open_durable(dir).unwrap();
    coord.recover();
    let row = record(trace);
    let trace_id = row.trace_id;
    let span_id = row.span_id;
    coord.ingest(vec![row]);
    coord.index_embedding(trace_id, span_id, vec![x, 0.0, 0.0]);
    coord.flush_memtable();
}

fn record(trace: &str) -> WalRecord {
    let seed = stable_id(trace);
    WalRecord {
        trace_id: seed,
        span_id: seed ^ 0x5eed,
        ts: 1,
        identity: EventIdentity {
            ext_span_id: format!("{trace}-span"),
            seq: 1,
            event_type: EventType::SpanEnd,
        },
        fields: SpanFields {
            status: Some(0),
            duration_ns: Some(1),
            external_trace_id: Some(trace.to_string()),
            external_span_id: Some(format!("{trace}-span")),
            logs: vec![format!("multiprocess embedded {trace}")],
            ..SpanFields::default()
        },
    }
}

fn fresh_dir(name: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "yt_{name}_{}_{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn stable_id(text: &str) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in text.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    h
}
