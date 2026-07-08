//! 规模压测入口：用真实 durable engine 写入，再通过 `EngineJsonApi` 查询。
//!
//! 这个版本只压主线已经稳定的单机 API：ingest、search、traces、sessions、trace detail
//! 和基础读模型（traceSearch / aggregate / storageStats / trajectory / loop / task）。

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use yt_engine::{EngineJsonApi, WireRecord, WriteCoordinator};

const DEFAULT_SPANS: usize = 10_000;
const DEFAULT_QUERIES: usize = 200;
const TENANT: u64 = 42;

struct Args {
    spans: usize,
    queries: usize,
    batch: usize,
    data_dir: PathBuf,
    report: Option<PathBuf>,
    cold_queries: bool,
    keep_data: bool,
}

#[derive(Clone)]
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 16
    }

    fn choose<'a>(&mut self, values: &'a [&'a str]) -> &'a str {
        values[self.next() as usize % values.len()]
    }
}

struct TimedStats {
    name: &'static str,
    count: usize,
    total: Duration,
    p50: Duration,
    p95: Duration,
    p99: Duration,
    max: Duration,
    errors: usize,
    response_bytes: usize,
}

impl TimedStats {
    fn qps(&self) -> f64 {
        if self.total.as_secs_f64() == 0.0 {
            return 0.0;
        }
        self.count as f64 / self.total.as_secs_f64()
    }
}

struct Report {
    spans: usize,
    queries: usize,
    cold_queries: bool,
    data_dir: PathBuf,
    data_bytes: u64,
    segment_files: usize,
    wal_bytes: u64,
    ingest_rows: usize,
    ingest_duration: Duration,
    flush_duration: Duration,
    stats: Vec<TimedStats>,
}

fn main() {
    let args = Args::parse();
    let _ = std::fs::remove_dir_all(&args.data_dir);
    std::fs::create_dir_all(&args.data_dir).expect("create data dir");

    let coord = WriteCoordinator::open_durable(&args.data_dir).expect("open durable engine");
    coord.recover();
    let api = EngineJsonApi::new(Arc::clone(&coord));

    let mut rng = Rng(0x9E3779B97F4A7C15);
    eprintln!(
        "scale_bench: ingest spans={} batch={} tenant={}",
        args.spans, args.batch, TENANT
    );
    let ingest_start = Instant::now();
    let mut written = 0usize;
    while written < args.spans {
        let take = args.batch.min(args.spans - written);
        let mut records = Vec::with_capacity(take);
        for offset in 0..take {
            let id = (written + offset + 1) as u64;
            records.push(make_record(id, TENANT, &mut rng));
        }
        coord.ingest_wire_for_tenant(records, Some(TENANT));
        written += take;
    }
    let ingest_duration = ingest_start.elapsed();

    eprintln!("scale_bench: flush memtable");
    let flush_start = Instant::now();
    coord.flush_memtable();
    let flush_duration = flush_start.elapsed();

    let queries = build_queries(args.queries, args.spans);
    let mut stats = Vec::new();
    for query in queries {
        eprintln!("scale_bench: query {} count={}", query.name, query.count);
        stats.push(measure_query(query.name, query.count, |iteration| {
            let cold_body;
            let body = if args.cold_queries {
                cold_body = json_cache_bust(&query.body, iteration);
                &cold_body
            } else {
                &query.body
            };
            api.route_with_tenant(query.method, &query.path, body, Some(TENANT))
        }));
    }

    let (data_bytes, segment_files, wal_bytes) = dir_stats(&args.data_dir);
    let report = Report {
        spans: args.spans,
        queries: args.queries,
        cold_queries: args.cold_queries,
        data_dir: args.data_dir.clone(),
        data_bytes,
        segment_files,
        wal_bytes,
        ingest_rows: written,
        ingest_duration,
        flush_duration,
        stats,
    };

    let markdown = render_report(&report);
    println!("{markdown}");
    if let Some(path) = &args.report {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create report dir");
        }
        std::fs::write(path, &markdown).expect("write scale bench report");
        eprintln!("scale bench report written: {}", path.display());
    }

    if !args.keep_data {
        let _ = std::fs::remove_dir_all(&args.data_dir);
    }
}

impl Args {
    fn parse() -> Self {
        let mut spans = DEFAULT_SPANS;
        let mut queries = DEFAULT_QUERIES;
        let mut batch = 512usize;
        let mut data_dir: Option<PathBuf> = None;
        let mut report = None;
        let mut cold_queries = false;
        let mut keep_data = false;

        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--spans" => spans = parse_next(&mut args, "--spans"),
                "--queries" => queries = parse_next(&mut args, "--queries"),
                "--batch" => batch = parse_next(&mut args, "--batch"),
                "--data-dir" => data_dir = Some(PathBuf::from(next_value(&mut args, "--data-dir"))),
                "--report" => report = Some(PathBuf::from(next_value(&mut args, "--report"))),
                "--cold-queries" => cold_queries = true,
                "--keep-data" => keep_data = true,
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                other => {
                    eprintln!("unknown arg: {other}");
                    print_help();
                    std::process::exit(2);
                }
            }
        }

        let data_dir = data_dir.unwrap_or_else(|| {
            std::env::temp_dir().join(format!("yt_scale_bench_{}", std::process::id()))
        });

        Self {
            spans,
            queries,
            batch,
            data_dir,
            report,
            cold_queries,
            keep_data,
        }
    }
}

fn next_value(args: &mut impl Iterator<Item = String>, name: &str) -> String {
    args.next().unwrap_or_else(|| {
        eprintln!("{name} requires a value");
        std::process::exit(2);
    })
}

fn parse_next<T>(args: &mut impl Iterator<Item = String>, name: &str) -> T
where
    T: std::str::FromStr,
{
    let raw = next_value(args, name);
    raw.parse().unwrap_or_else(|_| {
        eprintln!("invalid value for {name}: {raw}");
        std::process::exit(2);
    })
}

fn print_help() {
    eprintln!(
        "usage: scale_bench [--spans N] [--queries N] [--batch N] [--data-dir DIR] [--report PATH] [--cold-queries] [--keep-data]"
    );
}

fn json_cache_bust(body: &str, iteration: usize) -> String {
    let trimmed = body.trim_end();
    let Some(prefix) = trimmed.strip_suffix('}') else {
        return body.to_string();
    };
    if prefix.trim_end().ends_with('{') {
        format!("{prefix}\"benchCacheBust\":{iteration}}}")
    } else {
        format!("{prefix},\"benchCacheBust\":{iteration}}}")
    }
}

fn make_record(id: u64, tenant: u64, rng: &mut Rng) -> WireRecord {
    let projects = ["scale-a", "scale-b", "scale-c"];
    let skills = ["review", "plan", "execute", "test"];
    let modes = ["auto", "manual"];
    let validations = ["pass", "fail"];
    let tools = ["planner", "executor", "browser", "shell"];
    let models = ["qwen3", "gpt-4.1", "claude-sonnet"];
    let phrases = [
        "用户登录 风控系统 疑似盗刷",
        "智能体规划 工具超时 会话重试",
        "提示词注入 安全拦截 调用链追踪",
        "反欺诈研判 可疑交易 人工复核",
        "代码生成 单元测试 回归失败",
    ];

    let project = rng.choose(&projects);
    let skill = rng.choose(&skills);
    let mode = rng.choose(&modes);
    let validation = rng.choose(&validations);
    let tool = rng.choose(&tools);
    let model = rng.choose(&models);
    let mut attrs = BTreeMap::new();
    attrs.insert("project_id".to_string(), json_str(project));
    attrs.insert("skill".to_string(), json_str(skill));
    attrs.insert("mode".to_string(), json_str(mode));
    attrs.insert(
        "task_fingerprint".to_string(),
        json_str(if id % 2 == 0 {
            "risk-review"
        } else {
            "packaging"
        }),
    );
    attrs.insert(
        "loop_id".to_string(),
        json_str(&format!("loop-{}", id % 64)),
    );
    attrs.insert("validation_status".to_string(), json_str(validation));

    WireRecord {
        trace_id: id,
        span_id: 1,
        ts: id as i64,
        seq: 1,
        event_type_tag: 2,
        ext_span_id: format!("{id}-1"),
        parent_span_id: None,
        status: Some(if validation == "pass" { 0 } else { 1 }),
        duration_ns: Some(1_000_000 + (rng.next() % 50_000_000)),
        input_tokens: Some(100 + (rng.next() % 500)),
        output_tokens: Some(20 + (rng.next() % 150)),
        session_id: Some(10_000 + id / 3),
        tenant_id: Some(tenant),
        external_trace_id: Some(format!("run-{id}")),
        external_span_id: Some(format!("span-{id}-1")),
        external_parent_span_id: None,
        external_session_id: Some(format!("session-{}", 10_000 + id / 3)),
        agent_name: Some("scale-agent".to_string()),
        tool_name: Some(tool.to_string()),
        model: Some(model.to_string()),
        input_text: Some(format!("{} input {id}", rng.choose(&phrases))),
        output_text: Some(format!("{} output {id}", rng.choose(&phrases))),
        logs: vec![format!("scale log {}", id % 97)],
        attrs,
    }
}

fn json_str(value: &str) -> String {
    format!("{value:?}")
}

struct Query {
    name: &'static str,
    count: usize,
    method: &'static str,
    path: String,
    body: String,
}

fn build_queries(count: usize, spans: usize) -> Vec<Query> {
    let trace_id = (spans / 2).max(1);
    vec![
        Query {
            name: "search_text_attrs",
            count,
            method: "POST",
            path: "/v1/search".to_string(),
            body: r#"{"text":"疑似盗刷","k":10,"filter":{"attrs":{"project_id":"scale-a","skill":"review"}}}"#.to_string(),
        },
        Query {
            name: "search_text_status",
            count,
            method: "POST",
            path: "/v1/search".to_string(),
            body: r#"{"text":"回归失败","k":10,"filter":{"status":1}}"#.to_string(),
        },
        Query {
            name: "traces_list",
            count,
            method: "GET",
            path: "/v1/traces".to_string(),
            body: String::new(),
        },
        Query {
            name: "trace_search_attrs",
            count,
            method: "POST",
            path: "/v1/trace-search".to_string(),
            body: r#"{"filter":{"projectId":"scale-a","taskFingerprint":"risk-review"},"limit":20}"#.to_string(),
        },
        Query {
            name: "trace_aggregate_attrs",
            count,
            method: "POST",
            path: "/v1/trace-aggregate".to_string(),
            body: r#"{"filter":{"projectId":"scale-a"},"groupBy":["validationStatus","toolName"],"limit":20}"#.to_string(),
        },
        Query {
            name: "storage_stats_attrs",
            count,
            method: "POST",
            path: "/v1/storage-stats".to_string(),
            body: r#"{"filter":{"projectId":"scale-a"},"groupBy":["projectId","validationStatus"]}"#.to_string(),
        },
        Query {
            name: "trace_trajectories_attrs",
            count,
            method: "POST",
            path: "/v1/trace-trajectories".to_string(),
            body: r#"{"filter":{"projectId":"scale-a","taskFingerprint":"risk-review"},"limit":20}"#.to_string(),
        },
        Query {
            name: "trajectory_groups_attrs",
            count,
            method: "POST",
            path: "/v1/trajectory-groups".to_string(),
            body: r#"{"filter":{"projectId":"scale-a","taskFingerprint":"risk-review"},"limit":20}"#.to_string(),
        },
        Query {
            name: "trace_diff",
            count,
            method: "POST",
            path: "/v1/traces/diff".to_string(),
            body: format!(
                r#"{{"baseTraceId":{},"candidateTraceId":{},"includeSteps":false}}"#,
                trace_id,
                (trace_id + 1).min(spans.max(1))
            ),
        },
        Query {
            name: "loops_page",
            count,
            method: "GET",
            path: "/v1/loops?cursor=0&limit=20&project_id=scale-a&taskFingerprint=risk-review".to_string(),
            body: String::new(),
        },
        Query {
            name: "task_traces",
            count,
            method: "GET",
            path: "/v1/tasks/risk-review/traces?cursor=0&limit=20&validationStatus=pass".to_string(),
            body: String::new(),
        },
        Query {
            name: "sessions_page",
            count,
            method: "GET",
            path: "/v1/sessions?cursor=0&limit=50&project_id=scale-a".to_string(),
            body: String::new(),
        },
        Query {
            name: "trace_detail",
            count,
            method: "GET",
            path: format!("/v1/traces/{trace_id}"),
            body: String::new(),
        },
    ]
}

fn measure_query<F>(name: &'static str, count: usize, mut f: F) -> TimedStats
where
    F: FnMut(usize) -> (u16, String),
{
    let mut latencies = Vec::with_capacity(count);
    let mut errors = 0usize;
    let mut response_bytes = 0usize;
    let total_start = Instant::now();
    for i in 0..count {
        let start = Instant::now();
        let (status, body) = f(i);
        let elapsed = start.elapsed();
        if status != 200 {
            errors += 1;
        }
        response_bytes += body.len();
        latencies.push(elapsed);
    }
    let total = total_start.elapsed();
    latencies.sort_unstable();
    TimedStats {
        name,
        count,
        total,
        p50: percentile(&latencies, 50),
        p95: percentile(&latencies, 95),
        p99: percentile(&latencies, 99),
        max: *latencies.last().unwrap_or(&Duration::ZERO),
        errors,
        response_bytes,
    }
}

fn percentile(values: &[Duration], pct: usize) -> Duration {
    if values.is_empty() {
        return Duration::ZERO;
    }
    let idx = ((values.len() * pct).saturating_add(99) / 100)
        .saturating_sub(1)
        .min(values.len() - 1);
    values[idx]
}

fn dir_stats(path: &PathBuf) -> (u64, usize, u64) {
    fn walk(path: &std::path::Path, bytes: &mut u64, segments: &mut usize, wal: &mut u64) {
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                walk(&path, bytes, segments, wal);
            } else {
                *bytes += meta.len();
                let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
                if name.ends_with(".seg") || name.starts_with("seg-") {
                    *segments += 1;
                }
                if name.contains("wal") {
                    *wal += meta.len();
                }
            }
        }
    }

    let mut bytes = 0;
    let mut segments = 0;
    let mut wal = 0;
    walk(path, &mut bytes, &mut segments, &mut wal);
    (bytes, segments, wal)
}

fn render_report(report: &Report) -> String {
    let mut out = String::new();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let ingest_qps = report.ingest_rows as f64 / report.ingest_duration.as_secs_f64().max(0.001);
    let _ = writeln!(out, "# yiTrace Scale Bench Report");
    let _ = writeln!(out);
    let _ = writeln!(out, "- generatedAtUnix: {now}");
    let _ = writeln!(out, "- spans: {}", report.spans);
    let _ = writeln!(out, "- queriesPerEndpoint: {}", report.queries);
    let _ = writeln!(
        out,
        "- queryCacheMode: {}",
        if report.cold_queries { "cold" } else { "warm" }
    );
    let _ = writeln!(out, "- dataDir: {}", report.data_dir.display());
    let _ = writeln!(out, "- dataBytes: {}", report.data_bytes);
    let _ = writeln!(out, "- walBytes: {}", report.wal_bytes);
    let _ = writeln!(out, "- segmentLikeFiles: {}", report.segment_files);
    let _ = writeln!(out);
    let _ = writeln!(out, "## Write Path");
    let _ = writeln!(out);
    let _ = writeln!(out, "| Step | Count | Seconds | Rate |");
    let _ = writeln!(out, "|---|---:|---:|---:|");
    let _ = writeln!(
        out,
        "| ingest_wire | {} | {:.3} | {:.0} spans/s |",
        report.ingest_rows,
        report.ingest_duration.as_secs_f64(),
        ingest_qps
    );
    let _ = writeln!(
        out,
        "| flush_memtable | {} | {:.3} | - |",
        report.ingest_rows,
        report.flush_duration.as_secs_f64()
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "## Read Path");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "| Query | Count | QPS | P50 ms | P95 ms | P99 ms | Max ms | Errors | Avg bytes |"
    );
    let _ = writeln!(out, "|---|---:|---:|---:|---:|---:|---:|---:|---:|");
    for stat in &report.stats {
        let avg_bytes = if stat.count == 0 {
            0
        } else {
            stat.response_bytes / stat.count
        };
        let _ = writeln!(
            out,
            "| {} | {} | {:.0} | {:.3} | {:.3} | {:.3} | {:.3} | {} | {} |",
            stat.name,
            stat.count,
            stat.qps(),
            ms(stat.p50),
            ms(stat.p95),
            ms(stat.p99),
            ms(stat.max),
            stat.errors,
            avg_bytes
        );
    }
    out
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}
