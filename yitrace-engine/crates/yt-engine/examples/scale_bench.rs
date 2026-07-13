//! 规模压测入口：生成接近 Agent 工作负载的 trace，再通过真实 durable engine 查询。
//!
//! 生成和查询可以分进程执行。这样 `--phase query` 会重新打开已有数据目录，能测恢复时间和
//! 引擎冷启动；操作系统页缓存是否冷不做假设，报告会明确写成 process reopen。

#[path = "scale_bench/generator.rs"]
mod generator;
#[path = "scale_bench/queries.rs"]
mod queries;
#[path = "scale_bench/source_oracle.rs"]
mod source_oracle;

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use generator::{generate_dataset, DatasetStats, GeneratorConfig};
use queries::{build_queries, Query};
use source_oracle::{run_source_index_oracle, SourceOracleReport};
use yt_engine::{
    Bm25Index, Bm25TextIndex, ChineseTokenizer, CoordinatorBuilder, EngineJsonApi, WriteCoordinator,
};

const DEFAULT_SPANS: usize = 10_000;
const DEFAULT_QUERIES: usize = 200;
const DEFAULT_SEED: u64 = 0x9E3779B97F4A7C15;
const TENANT: u64 = 42;
const META_FILE: &str = "scale-bench.meta";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Full,
    Generate,
    Query,
    Open,
}

impl Phase {
    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "full" => Some(Self::Full),
            "generate" => Some(Self::Generate),
            "query" => Some(Self::Query),
            "open" => Some(Self::Open),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Generate => "generate",
            Self::Query => "query",
            Self::Open => "open",
        }
    }
}

struct Args {
    phase: Phase,
    spans: usize,
    queries: usize,
    batch: usize,
    seed: u64,
    data_dir: PathBuf,
    report: Option<PathBuf>,
    only_query: Option<String>,
    concurrency: usize,
    cold_queries: bool,
    verify_search: bool,
    verify_source_index: bool,
    keep_data: bool,
}

struct WriteStats {
    ingest_duration: Duration,
    flush_duration: Duration,
    rss_after_ingest_kib: Option<u64>,
}

struct TimedStats {
    name: &'static str,
    selectivity: &'static str,
    count: usize,
    total: Duration,
    first: Duration,
    p50: Duration,
    p95: Duration,
    p99: Duration,
    max: Duration,
    http_errors: usize,
    validation_errors: usize,
    expected_read_source: Option<&'static str>,
    read_source_hits: usize,
    expect_point_lookup: bool,
    point_lookup_hits: usize,
    max_point_lookup_segments: u64,
    max_decoded_segment_rows: u64,
    max_index_bytes_read: u64,
    max_data_bytes_read: u64,
    max_indexes_validated: u64,
    max_indexes_rebuilt: u64,
    response_bytes: usize,
    first_failure: Option<String>,
}

impl TimedStats {
    fn qps(&self) -> f64 {
        if self.total.is_zero() {
            return 0.0;
        }
        self.count as f64 / self.total.as_secs_f64()
    }
}

#[derive(Default)]
struct DirStats {
    total_bytes: u64,
    wal_bytes: u64,
    segment_bytes: u64,
    sidecar_bytes: u64,
    manifest_bytes: u64,
    other_bytes: u64,
    segment_files: usize,
}

struct Report {
    phase: Phase,
    query_mode: &'static str,
    dataset: DatasetStats,
    queries: usize,
    query_concurrency: usize,
    data_dir: PathBuf,
    dir: DirStats,
    open_duration: Duration,
    write: Option<WriteStats>,
    rss_after_open_kib: Option<u64>,
    rss_after_queries_kib: Option<u64>,
    stats: Vec<TimedStats>,
    search_oracle: Option<SearchOracleReport>,
    source_oracle: Option<SourceOracleReport>,
}

struct SearchOracleReport {
    duration: Duration,
    cases: Vec<SearchOracleCaseResult>,
}

struct SearchOracleCaseResult {
    name: &'static str,
    filter: &'static str,
    k: usize,
    optimized_count: usize,
    exact_count: usize,
    recall_at_k: f64,
    exact_rank_and_score: bool,
}

fn main() {
    let args = Args::parse();
    let report = match args.phase {
        Phase::Generate => run_generate_only(&args),
        Phase::Query => run_query_only(&args),
        Phase::Open => run_open_only(&args),
        Phase::Full => run_full(&args),
    };
    let failed = report
        .stats
        .iter()
        .any(|stat| stat.http_errors > 0 || stat.validation_errors > 0)
        || report
            .search_oracle
            .as_ref()
            .is_some_and(|oracle| oracle.cases.iter().any(|case| !case.exact_rank_and_score))
        || report
            .source_oracle
            .as_ref()
            .is_some_and(|oracle| oracle.cases.iter().any(|case| !case.exact_rank_and_score));
    write_report(&args, &report);

    if args.phase == Phase::Full && !args.keep_data {
        let _ = std::fs::remove_dir_all(&args.data_dir);
    }
    if failed {
        eprintln!("scale_bench: query correctness, recall oracle, or read-plan validation failed");
        std::process::exit(1);
    }
}

fn run_open_only(args: &Args) -> Report {
    let dataset = load_dataset_stats(&args.data_dir);
    let open_start = Instant::now();
    let (coord, _) = open_bench_engine(&args.data_dir);
    coord.recover();
    let open_duration = open_start.elapsed();
    let rss_after_open_kib = current_rss_kib();

    Report {
        phase: args.phase,
        query_mode: "open-only",
        dataset,
        queries: 0,
        query_concurrency: args.concurrency,
        dir: dir_stats(&args.data_dir),
        data_dir: args.data_dir.clone(),
        open_duration,
        write: None,
        rss_after_open_kib,
        rss_after_queries_kib: None,
        stats: Vec::new(),
        search_oracle: None,
        source_oracle: None,
    }
}

fn run_generate_only(args: &Args) -> Report {
    prepare_new_data_dir(&args.data_dir);
    let open_start = Instant::now();
    let (coord, _) = open_bench_engine(&args.data_dir);
    coord.recover();
    let open_duration = open_start.elapsed();
    let rss_after_open_kib = current_rss_kib();
    let (dataset, write) = ingest_dataset(&coord, args);
    persist_dataset_stats(&args.data_dir, &dataset);

    Report {
        phase: args.phase,
        query_mode: "not-run",
        dataset,
        queries: args.queries,
        query_concurrency: args.concurrency,
        dir: dir_stats(&args.data_dir),
        data_dir: args.data_dir.clone(),
        open_duration,
        write: Some(write),
        rss_after_open_kib,
        rss_after_queries_kib: None,
        stats: Vec::new(),
        search_oracle: None,
        source_oracle: None,
    }
}

fn run_query_only(args: &Args) -> Report {
    let dataset = load_dataset_stats(&args.data_dir);
    let open_start = Instant::now();
    let (coord, bm25) = open_bench_engine(&args.data_dir);
    coord.recover();
    let open_duration = open_start.elapsed();
    let rss_after_open_kib = current_rss_kib();
    let stats = run_queries(
        &coord,
        args.queries,
        args.concurrency,
        &dataset,
        args.only_query.as_deref(),
    );
    let rss_after_queries_kib = current_rss_kib();
    let search_oracle = args.verify_search.then(|| run_search_oracle(bm25.as_ref()));
    let source_oracle = args
        .verify_source_index
        .then(|| run_source_index_oracle(bm25.as_ref(), &dataset, args.batch, TENANT));

    Report {
        phase: args.phase,
        query_mode: "separate-process-reopen",
        dataset,
        queries: args.queries,
        query_concurrency: args.concurrency,
        dir: dir_stats(&args.data_dir),
        data_dir: args.data_dir.clone(),
        open_duration,
        write: None,
        rss_after_open_kib,
        rss_after_queries_kib,
        stats,
        search_oracle,
        source_oracle,
    }
}

fn run_full(args: &Args) -> Report {
    prepare_new_data_dir(&args.data_dir);
    let open_start = Instant::now();
    let (coord, mut bm25) = open_bench_engine(&args.data_dir);
    coord.recover();
    let mut open_duration = open_start.elapsed();
    let (dataset, write) = ingest_dataset(&coord, args);
    persist_dataset_stats(&args.data_dir, &dataset);

    let (coord, query_mode) = if args.cold_queries {
        drop(coord);
        let reopen_start = Instant::now();
        let (reopened, reopened_bm25) = open_bench_engine(&args.data_dir);
        reopened.recover();
        bm25 = reopened_bm25;
        open_duration = reopen_start.elapsed();
        (reopened, "same-process-reopen")
    } else {
        (coord, "same-process-warm")
    };
    let rss_after_open_kib = current_rss_kib();
    let stats = run_queries(
        &coord,
        args.queries,
        args.concurrency,
        &dataset,
        args.only_query.as_deref(),
    );
    let rss_after_queries_kib = current_rss_kib();
    let search_oracle = args.verify_search.then(|| run_search_oracle(bm25.as_ref()));
    let source_oracle = args
        .verify_source_index
        .then(|| run_source_index_oracle(bm25.as_ref(), &dataset, args.batch, TENANT));

    Report {
        phase: args.phase,
        query_mode,
        dataset,
        queries: args.queries,
        query_concurrency: args.concurrency,
        dir: dir_stats(&args.data_dir),
        data_dir: args.data_dir.clone(),
        open_duration,
        write: Some(write),
        rss_after_open_kib,
        rss_after_queries_kib,
        stats,
        search_oracle,
        source_oracle,
    }
}

fn open_bench_engine(path: &Path) -> (Arc<WriteCoordinator>, Arc<Bm25TextIndex>) {
    let bm25 = Arc::new(Bm25TextIndex::with_tokenizer(Box::new(
        ChineseTokenizer::full(),
    )));
    let coord = CoordinatorBuilder::new()
        .with_bm25(bm25.clone())
        .open_durable(path)
        .expect("open durable engine");
    (coord, bm25)
}

fn prepare_new_data_dir(path: &Path) {
    if path.exists() {
        std::fs::remove_dir_all(path).expect("remove old benchmark data dir");
    }
    std::fs::create_dir_all(path).expect("create benchmark data dir");
}

fn ingest_dataset(coord: &Arc<WriteCoordinator>, args: &Args) -> (DatasetStats, WriteStats) {
    eprintln!(
        "scale_bench: generate spans={} batch_records={} seed={} tenant={}",
        args.spans, args.batch, args.seed, TENANT
    );
    let ingest_start = Instant::now();
    let stats = generate_dataset(
        GeneratorConfig {
            spans: args.spans,
            batch_records: args.batch,
            seed: args.seed,
            tenant: TENANT,
        },
        |records| {
            coord.ingest_wire_for_tenant(records, Some(TENANT));
        },
    );
    let ingest_duration = ingest_start.elapsed();

    eprintln!(
        "scale_bench: flush remaining memtable spans={} wire_events={}",
        stats.spans, stats.wire_events
    );
    let flush_start = Instant::now();
    coord.flush_memtable();
    let flush_duration = flush_start.elapsed();
    let rss_after_ingest_kib = current_rss_kib();
    (
        stats,
        WriteStats {
            ingest_duration,
            flush_duration,
            rss_after_ingest_kib,
        },
    )
}

fn run_queries(
    coord: &Arc<WriteCoordinator>,
    count: usize,
    concurrency: usize,
    dataset: &DatasetStats,
    only_query: Option<&str>,
) -> Vec<TimedStats> {
    let api = EngineJsonApi::new(Arc::clone(coord));
    let stats = build_queries(count, dataset)
        .into_iter()
        .filter(|query| only_query.is_none_or(|name| query.name == name))
        .map(|mut query| {
            if only_query.is_some() {
                query.count = count.max(1);
            }
            query
        })
        .map(|query| {
            eprintln!(
                "scale_bench: query {} selectivity={} count={} concurrency={}",
                query.name, query.selectivity, query.count, concurrency
            );
            measure_query(&api, query, concurrency)
        })
        .collect::<Vec<_>>();
    if stats.is_empty() {
        eprintln!(
            "unknown --only-query: {}",
            only_query.unwrap_or("<not provided>")
        );
        std::process::exit(2);
    }
    stats
}

#[derive(Clone, Copy)]
enum OracleFilter {
    All,
    ScaleA,
}

struct SearchOracleCase {
    name: &'static str,
    query: &'static str,
    k: usize,
    filter: OracleFilter,
}

fn run_search_oracle(bm25: &Bm25TextIndex) -> SearchOracleReport {
    let cases = [
        SearchOracleCase {
            name: "common-k10",
            query: "任务执行",
            k: 10,
            filter: OracleFilter::All,
        },
        SearchOracleCase {
            name: "common-k50",
            query: "任务执行",
            k: 50,
            filter: OracleFilter::All,
        },
        SearchOracleCase {
            name: "common-project-k10",
            query: "任务执行",
            k: 10,
            filter: OracleFilter::ScaleA,
        },
        SearchOracleCase {
            name: "rare-k10",
            query: "月蚀校验码",
            k: 10,
            filter: OracleFilter::All,
        },
        SearchOracleCase {
            name: "risk-k20",
            query: "支付风控",
            k: 20,
            filter: OracleFilter::All,
        },
        SearchOracleCase {
            name: "multi-term-k20",
            query: "任务执行 支付风控",
            k: 20,
            filter: OracleFilter::All,
        },
    ];
    let started = Instant::now();
    let results = cases
        .iter()
        .map(|case| {
            eprintln!(
                "scale_bench: search oracle {} query={:?} k={}",
                case.name, case.query, case.k
            );
            let (optimized, exact, filter) = match case.filter {
                OracleFilter::All => (
                    bm25.search(case.query, case.k),
                    bm25.search_exact_for_eval(case.query, case.k),
                    "all",
                ),
                OracleFilter::ScaleA => {
                    let scale_a = |trace_id: u64, _: u64| trace_id > 0 && (trace_id - 1) % 4 == 0;
                    (
                        bm25.search_filtered(case.query, case.k, &scale_a),
                        bm25.search_exact_filtered_for_eval(case.query, case.k, &scale_a),
                        "project_id=scale-a",
                    )
                }
            };
            let exact_docs = exact
                .iter()
                .map(|&(trace_id, span_id, _)| (trace_id, span_id))
                .collect::<HashSet<_>>();
            let optimized_docs = optimized
                .iter()
                .map(|&(trace_id, span_id, _)| (trace_id, span_id))
                .collect::<HashSet<_>>();
            let recall_at_k = if exact_docs.is_empty() {
                if optimized_docs.is_empty() {
                    1.0
                } else {
                    0.0
                }
            } else {
                optimized_docs.intersection(&exact_docs).count() as f64 / exact_docs.len() as f64
            };
            SearchOracleCaseResult {
                name: case.name,
                filter,
                k: case.k,
                optimized_count: optimized.len(),
                exact_count: exact.len(),
                recall_at_k,
                exact_rank_and_score: optimized == exact,
            }
        })
        .collect();
    SearchOracleReport {
        duration: started.elapsed(),
        cases: results,
    }
}

struct QueryObservation {
    index: usize,
    elapsed: Duration,
    status: u16,
    body: String,
}

fn execute_queries(
    api: &EngineJsonApi,
    query: &Query,
    count: usize,
    concurrency: usize,
) -> (Duration, Vec<QueryObservation>) {
    let total_start = Instant::now();
    if concurrency <= 1 || count <= 1 {
        let mut observations = Vec::with_capacity(count);
        for index in 0..count {
            let start = Instant::now();
            let (status, body) =
                api.route_with_tenant(query.method, &query.path, &query.body, Some(TENANT));
            observations.push(QueryObservation {
                index,
                elapsed: start.elapsed(),
                status,
                body,
            });
        }
        return (total_start.elapsed(), observations);
    }

    let next = AtomicUsize::new(0);
    let observations = Mutex::new(Vec::with_capacity(count));
    std::thread::scope(|scope| {
        for _ in 0..concurrency.min(count) {
            scope.spawn(|| loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                if index >= count {
                    break;
                }
                let start = Instant::now();
                let (status, body) =
                    api.route_with_tenant(query.method, &query.path, &query.body, Some(TENANT));
                observations.lock().unwrap().push(QueryObservation {
                    index,
                    elapsed: start.elapsed(),
                    status,
                    body,
                });
            });
        }
    });
    let total = total_start.elapsed();
    let mut observations = observations.into_inner().unwrap();
    observations.sort_unstable_by_key(|observation| observation.index);
    (total, observations)
}

fn measure_query(api: &EngineJsonApi, query: Query, concurrency: usize) -> TimedStats {
    let count = query.count.max(1);
    let mut latencies = Vec::with_capacity(count);
    let mut http_errors = 0usize;
    let mut validation_errors = 0usize;
    let mut read_source_hits = 0usize;
    let mut point_lookup_hits = 0usize;
    let mut max_point_lookup_segments = 0u64;
    let mut max_decoded_segment_rows = 0u64;
    let mut max_index_bytes_read = 0u64;
    let mut max_data_bytes_read = 0u64;
    let mut max_indexes_validated = 0u64;
    let mut max_indexes_rebuilt = 0u64;
    let mut response_bytes = 0usize;
    let mut first_failure = None;
    let expected_source_fragment = query
        .expected_read_source
        .map(|source| format!(r#""source":"{source}""#));
    let (total, observations) = execute_queries(api, &query, count, concurrency);

    for observation in observations {
        let status = observation.status;
        let body = observation.body;
        let elapsed = observation.elapsed;
        let missing = query
            .expected_fragments
            .iter()
            .find(|fragment| !body.contains(fragment.as_str()));
        let source_ok = expected_source_fragment
            .as_ref()
            .is_none_or(|fragment| body.contains(fragment));
        let point_lookup_segments = json_u64_field(&body, "pointLookupSegments").unwrap_or(0);
        let decoded_segment_rows = json_u64_field(&body, "decodedSegmentRows").unwrap_or(0);
        let index_bytes_read = json_u64_field(&body, "indexBytesRead").unwrap_or(0);
        let data_bytes_read = json_u64_field(&body, "dataBytesRead").unwrap_or(0);
        let indexes_validated = json_u64_field(&body, "indexesValidated").unwrap_or(0);
        let indexes_rebuilt = json_u64_field(&body, "indexesRebuilt").unwrap_or(0);
        let point_lookup_ok = !query.expect_point_lookup || point_lookup_segments > 0;

        if status != 200 {
            http_errors += 1;
            first_failure
                .get_or_insert_with(|| format!("status={status} body={}", truncate(&body, 240)));
        }
        if missing.is_some() || !source_ok || !point_lookup_ok {
            validation_errors += 1;
            first_failure.get_or_insert_with(|| {
                format!(
                    "missing={:?} expectedSource={:?} expectPointLookup={} body={}",
                    missing,
                    query.expected_read_source,
                    query.expect_point_lookup,
                    truncate(&body, 240)
                )
            });
        }
        if source_ok && query.expected_read_source.is_some() {
            read_source_hits += 1;
        }
        if query.expect_point_lookup && point_lookup_ok {
            point_lookup_hits += 1;
        }
        max_point_lookup_segments = max_point_lookup_segments.max(point_lookup_segments);
        max_decoded_segment_rows = max_decoded_segment_rows.max(decoded_segment_rows);
        max_index_bytes_read = max_index_bytes_read.max(index_bytes_read);
        max_data_bytes_read = max_data_bytes_read.max(data_bytes_read);
        max_indexes_validated = max_indexes_validated.max(indexes_validated);
        max_indexes_rebuilt = max_indexes_rebuilt.max(indexes_rebuilt);
        response_bytes += body.len();
        latencies.push(elapsed);
    }

    let first = latencies.first().copied().unwrap_or_default();
    latencies.sort_unstable();
    TimedStats {
        name: query.name,
        selectivity: query.selectivity,
        count,
        total,
        first,
        p50: percentile(&latencies, 50),
        p95: percentile(&latencies, 95),
        p99: percentile(&latencies, 99),
        max: latencies.last().copied().unwrap_or_default(),
        http_errors,
        validation_errors,
        expected_read_source: query.expected_read_source,
        read_source_hits,
        expect_point_lookup: query.expect_point_lookup,
        point_lookup_hits,
        max_point_lookup_segments,
        max_decoded_segment_rows,
        max_index_bytes_read,
        max_data_bytes_read,
        max_indexes_validated,
        max_indexes_rebuilt,
        response_bytes,
        first_failure,
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

fn truncate(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{head}...")
    } else {
        head
    }
}

fn json_u64_field(body: &str, key: &str) -> Option<u64> {
    let needle = format!(r#""{key}":"#);
    let rest = body.split_once(&needle)?.1;
    let digits = rest
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    (!digits.is_empty()).then(|| digits.parse().ok()).flatten()
}

fn persist_dataset_stats(path: &Path, stats: &DatasetStats) {
    let body = format!(
        concat!(
            "format=2\n",
            "spans={}\ntraces={}\nsessions={}\nloops={}\nwire_events={}\n",
            "log_events={}\nduplicate_events={}\nincomplete_spans={}\n",
            "scale_a_spans={}\nscale_a_traces={}\nrisk_review_traces={}\nseed={}\n"
        ),
        stats.spans,
        stats.traces,
        stats.sessions,
        stats.loops,
        stats.wire_events,
        stats.log_events,
        stats.duplicate_events,
        stats.incomplete_spans,
        stats.scale_a_spans,
        stats.scale_a_traces,
        stats.risk_review_traces,
        stats.seed,
    );
    std::fs::write(path.join(META_FILE), body).expect("write benchmark metadata");
}

fn load_dataset_stats(path: &Path) -> DatasetStats {
    let meta_path = path.join(META_FILE);
    let body = std::fs::read_to_string(&meta_path).unwrap_or_else(|error| {
        panic!(
            "read benchmark metadata {} failed: {error}; run --phase generate first",
            meta_path.display()
        )
    });
    let values = body
        .lines()
        .filter_map(|line| line.split_once('='))
        .collect::<BTreeMap<_, _>>();
    let get = |key: &str| -> usize {
        values
            .get(key)
            .unwrap_or_else(|| panic!("benchmark metadata missing {key}"))
            .parse()
            .unwrap_or_else(|_| panic!("benchmark metadata has invalid {key}"))
    };
    DatasetStats {
        spans: get("spans"),
        traces: get("traces"),
        sessions: get("sessions"),
        loops: get("loops"),
        wire_events: get("wire_events"),
        log_events: get("log_events"),
        duplicate_events: get("duplicate_events"),
        incomplete_spans: get("incomplete_spans"),
        scale_a_spans: get("scale_a_spans"),
        scale_a_traces: get("scale_a_traces"),
        risk_review_traces: get("risk_review_traces"),
        seed: values
            .get("seed")
            .expect("benchmark metadata missing seed")
            .parse()
            .expect("benchmark metadata has invalid seed"),
    }
}

fn dir_stats(path: &Path) -> DirStats {
    fn walk(path: &Path, stats: &mut DirStats) {
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                walk(&path, stats);
                continue;
            }
            let bytes = meta.len();
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("");
            stats.total_bytes += bytes;
            if name.contains("wal") {
                stats.wal_bytes += bytes;
            } else if name.ends_with(".seg") || name.starts_with("seg-") {
                stats.segment_bytes += bytes;
                stats.segment_files += 1;
            } else if name.contains("manifest") {
                stats.manifest_bytes += bytes;
            } else if name.ends_with(".dat") || name.contains("index") || name.contains("rollup") {
                stats.sidecar_bytes += bytes;
            } else {
                stats.other_bytes += bytes;
            }
        }
    }

    let mut stats = DirStats::default();
    walk(path, &mut stats);
    stats
}

fn current_rss_kib() -> Option<u64> {
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()?.trim().parse().ok()
}

fn write_report(args: &Args, report: &Report) {
    let markdown = render_report(report);
    println!("{markdown}");
    if let Some(path) = &args.report {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create report dir");
        }
        std::fs::write(path, &markdown).expect("write scale benchmark report");
        eprintln!("scale bench report written: {}", path.display());
    }
}

fn render_report(report: &Report) -> String {
    let mut out = String::new();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let bytes_per_span = report.dir.total_bytes as f64 / report.dataset.spans.max(1) as f64;

    let _ = writeln!(out, "# yiTrace Scale Bench Report");
    let _ = writeln!(out);
    let _ = writeln!(out, "- generatedAtUnix: {now}");
    let _ = writeln!(out, "- phase: {}", report.phase.label());
    let _ = writeln!(out, "- queryProcessMode: {}", report.query_mode);
    let _ = writeln!(out, "- pageCacheMode: uncontrolled");
    let _ = writeln!(out, "- seed: {}", report.dataset.seed);
    let _ = writeln!(out, "- foldedSpans: {}", report.dataset.spans);
    let _ = writeln!(out, "- wireEvents: {}", report.dataset.wire_events);
    let _ = writeln!(out, "- traces: {}", report.dataset.traces);
    let _ = writeln!(out, "- sessions: {}", report.dataset.sessions);
    let _ = writeln!(out, "- loops: {}", report.dataset.loops);
    let _ = writeln!(out, "- logEvents: {}", report.dataset.log_events);
    let _ = writeln!(
        out,
        "- duplicateEvents: {}",
        report.dataset.duplicate_events
    );
    let _ = writeln!(
        out,
        "- incompleteSpans: {}",
        report.dataset.incomplete_spans
    );
    let _ = writeln!(out, "- requestedQueriesPerEndpoint: {}", report.queries);
    let _ = writeln!(out, "- queryConcurrency: {}", report.query_concurrency);
    let _ = writeln!(
        out,
        "- searchOracle: {}",
        report.search_oracle.as_ref().map_or_else(
            || "not-run".to_string(),
            |oracle| {
                let failures = oracle
                    .cases
                    .iter()
                    .filter(|case| !case.exact_rank_and_score)
                    .count();
                format!("{} cases, {} failures", oracle.cases.len(), failures)
            }
        )
    );
    let _ = writeln!(
        out,
        "- sourceIndexOracle: {}",
        report.source_oracle.as_ref().map_or_else(
            || "not-run".to_string(),
            |oracle| {
                let failures = oracle
                    .cases
                    .iter()
                    .filter(|case| !case.exact_rank_and_score)
                    .count();
                format!("{} cases, {} failures", oracle.cases.len(), failures)
            }
        )
    );
    let _ = writeln!(out, "- dataDir: {}", report.data_dir.display());
    let _ = writeln!(
        out,
        "- openAndRecoverSeconds: {:.3}",
        report.open_duration.as_secs_f64()
    );
    let _ = writeln!(
        out,
        "- openAndRecoverMillis: {:.3}",
        report.open_duration.as_secs_f64() * 1_000.0
    );
    let _ = writeln!(
        out,
        "- rssAfterOpenKiB: {}",
        option_u64(report.rss_after_open_kib)
    );
    let _ = writeln!(
        out,
        "- rssAfterQueriesKiB: {}",
        option_u64(report.rss_after_queries_kib)
    );
    let _ = writeln!(out);

    let _ = writeln!(out, "## Data Shape");
    let _ = writeln!(out);
    let _ = writeln!(out, "| Item | Value |");
    let _ = writeln!(out, "|---|---:|");
    let _ = writeln!(out, "| total bytes | {} |", report.dir.total_bytes);
    let _ = writeln!(out, "| bytes / folded span | {:.1} |", bytes_per_span);
    let _ = writeln!(out, "| WAL bytes | {} |", report.dir.wal_bytes);
    let _ = writeln!(out, "| segment bytes | {} |", report.dir.segment_bytes);
    let _ = writeln!(
        out,
        "| sidecar / index bytes | {} |",
        report.dir.sidecar_bytes
    );
    let _ = writeln!(out, "| manifest bytes | {} |", report.dir.manifest_bytes);
    let _ = writeln!(out, "| other bytes | {} |", report.dir.other_bytes);
    let _ = writeln!(out, "| segment files | {} |", report.dir.segment_files);
    let _ = writeln!(out);

    if let Some(write) = &report.write {
        let span_rate =
            report.dataset.spans as f64 / write.ingest_duration.as_secs_f64().max(0.001);
        let event_rate =
            report.dataset.wire_events as f64 / write.ingest_duration.as_secs_f64().max(0.001);
        let _ = writeln!(out, "## Write Path");
        let _ = writeln!(out);
        let _ = writeln!(out, "| Step | Count | Seconds | Rate |");
        let _ = writeln!(out, "|---|---:|---:|---:|");
        let _ = writeln!(
            out,
            "| ingest folded spans | {} | {:.3} | {:.0} spans/s |",
            report.dataset.spans,
            write.ingest_duration.as_secs_f64(),
            span_rate
        );
        let _ = writeln!(
            out,
            "| ingest wire events | {} | {:.3} | {:.0} events/s |",
            report.dataset.wire_events,
            write.ingest_duration.as_secs_f64(),
            event_rate
        );
        let _ = writeln!(
            out,
            "| flush remaining memtable | {} | {:.3} | - |",
            report.dataset.wire_events,
            write.flush_duration.as_secs_f64()
        );
        let _ = writeln!(
            out,
            "| RSS after ingest KiB | {} | - | - |",
            option_u64(write.rss_after_ingest_kib)
        );
        let _ = writeln!(out);
    }

    if !report.stats.is_empty() {
        let _ = writeln!(out, "## Read Path");
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "| Query | Selectivity | Count | QPS | First ms | P50 ms | P95 ms | P99 ms | Max ms | Errors | Plan evidence | Avg bytes |"
        );
        let _ = writeln!(
            out,
            "|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---|---:|"
        );
        for stat in &report.stats {
            let avg_bytes = stat.response_bytes / stat.count.max(1);
            let mut plan = stat.expected_read_source.map_or_else(
                || "n/a".to_string(),
                |source| format!("{source} {}/{}", stat.read_source_hits, stat.count),
            );
            if stat.expect_point_lookup {
                plan.push_str(&format!(
                    "; point {}/{} segments<={} rows<={} indexBytes<={} dataBytes<={} validated<={} rebuilt<={}",
                    stat.point_lookup_hits,
                    stat.count,
                    stat.max_point_lookup_segments,
                    stat.max_decoded_segment_rows,
                    stat.max_index_bytes_read,
                    stat.max_data_bytes_read,
                    stat.max_indexes_validated,
                    stat.max_indexes_rebuilt
                ));
            }
            let _ = writeln!(
                out,
                "| {} | {} | {} | {:.0} | {:.3} | {:.3} | {:.3} | {:.3} | {:.3} | {}/{} | {} | {} |",
                stat.name,
                stat.selectivity,
                stat.count,
                stat.qps(),
                ms(stat.first),
                ms(stat.p50),
                ms(stat.p95),
                ms(stat.p99),
                ms(stat.max),
                stat.http_errors,
                stat.validation_errors,
                plan,
                avg_bytes
            );
        }
        let failures = report
            .stats
            .iter()
            .filter_map(|stat| {
                stat.first_failure
                    .as_ref()
                    .map(|failure| (stat.name, failure))
            })
            .collect::<Vec<_>>();
        if !failures.is_empty() {
            let _ = writeln!(out);
            let _ = writeln!(out, "## Validation Failures");
            let _ = writeln!(out);
            for (name, failure) in failures {
                let _ = writeln!(out, "- `{name}`: {failure}");
            }
        }
    }
    if let Some(oracle) = &report.search_oracle {
        let _ = writeln!(out);
        let _ = writeln!(out, "## Search Correctness Oracle");
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "完整评分扫描查询词的全部 posting，不使用 WAND、block 跳过、结果缓存或 singleflight。"
        );
        let _ = writeln!(out);
        let _ = writeln!(out, "- totalSeconds: {:.3}", oracle.duration.as_secs_f64());
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "| Case | Filter | k | Optimized | Exact | Recall@k | Rank and score exact |"
        );
        let _ = writeln!(out, "|---|---|---:|---:|---:|---:|---|");
        for case in &oracle.cases {
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} | {:.3} | {} |",
                case.name,
                case.filter,
                case.k,
                case.optimized_count,
                case.exact_count,
                case.recall_at_k,
                case.exact_rank_and_score
            );
        }
    }
    if let Some(oracle) = &report.source_oracle {
        let _ = writeln!(out);
        let _ = writeln!(out, "## Source-to-Index Correctness Oracle");
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "从同一 seed 重新生成 wire event，独立提取可检索字段、分词并计算 BM25；不读取持久倒排。"
        );
        let _ = writeln!(out);
        let _ = writeln!(out, "- sourceEvents: {}", oracle.source_events);
        let _ = writeln!(out, "- uniqueSourceEvents: {}", oracle.unique_source_events);
        let _ = writeln!(out, "- sourceDocs: {}", oracle.source_docs);
        let _ = writeln!(
            out,
            "- buildSeconds: {:.3}",
            oracle.build_duration.as_secs_f64()
        );
        let _ = writeln!(
            out,
            "- compareSeconds: {:.3}",
            oracle.compare_duration.as_secs_f64()
        );
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "| Case | Filter | k | Source | Persisted index | Recall@k | Rank and score exact | Max score delta |"
        );
        let _ = writeln!(out, "|---|---|---:|---:|---:|---:|---|---:|");
        for case in &oracle.cases {
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} | {:.3} | {} | {:.8} |",
                case.name,
                case.filter,
                case.k,
                case.source_count,
                case.index_count,
                case.recall_at_k,
                case.exact_rank_and_score,
                case.max_score_delta
            );
        }
    }
    out
}

fn option_u64(value: Option<u64>) -> String {
    value.map_or_else(|| "n/a".to_string(), |number| number.to_string())
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

impl Args {
    fn parse() -> Self {
        let mut phase = Phase::Full;
        let mut spans = DEFAULT_SPANS;
        let mut queries = DEFAULT_QUERIES;
        let mut batch = 512usize;
        let mut seed = DEFAULT_SEED;
        let mut data_dir = None;
        let mut report = None;
        let mut only_query = None;
        let mut concurrency = 1usize;
        let mut cold_queries = false;
        let mut verify_search = false;
        let mut verify_source_index = false;
        let mut keep_data = false;

        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--phase" => {
                    let raw = next_value(&mut args, "--phase");
                    phase = Phase::parse(&raw).unwrap_or_else(|| {
                        eprintln!("invalid --phase: {raw}");
                        std::process::exit(2);
                    });
                }
                "--spans" => spans = parse_next(&mut args, "--spans"),
                "--queries" => queries = parse_next(&mut args, "--queries"),
                "--batch" => batch = parse_next(&mut args, "--batch"),
                "--seed" => seed = parse_next(&mut args, "--seed"),
                "--data-dir" => data_dir = Some(PathBuf::from(next_value(&mut args, "--data-dir"))),
                "--report" => report = Some(PathBuf::from(next_value(&mut args, "--report"))),
                "--only-query" => only_query = Some(next_value(&mut args, "--only-query")),
                "--concurrency" => concurrency = parse_next(&mut args, "--concurrency"),
                "--cold-queries" => cold_queries = true,
                "--verify-search" => verify_search = true,
                "--verify-source-index" => verify_source_index = true,
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
        if matches!(phase, Phase::Query | Phase::Open) && !data_dir.exists() {
            eprintln!(
                "query/open phase requires existing --data-dir: {}",
                data_dir.display()
            );
            std::process::exit(2);
        }
        if concurrency == 0 {
            eprintln!("--concurrency must be at least 1");
            std::process::exit(2);
        }

        Self {
            phase,
            spans,
            queries,
            batch,
            seed,
            data_dir,
            report,
            only_query,
            concurrency,
            cold_queries,
            verify_search,
            verify_source_index,
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
    eprintln!(concat!(
        "usage: scale_bench [--phase full|generate|query|open] [--spans N] [--queries N] ",
        "[--batch N] [--seed N] [--data-dir DIR] [--report PATH] ",
        "[--only-query NAME] [--concurrency N] [--cold-queries] [--verify-search] ",
        "[--verify-source-index] [--keep-data]\n",
        "\n",
        "--phase generate  writes a reusable durable dataset and metadata\n",
        "--phase query     opens an existing dataset and only runs the query matrix\n",
        "--phase open      opens and recovers an existing dataset without running queries\n",
        "--only-query      runs one named query from the query matrix\n",
        "--concurrency     runs up to N query calls at the same time\n",
        "--cold-queries    reopens the engine before queries in full phase\n",
        "--verify-search   compares optimized BM25 with complete posting scoring\n",
        "--verify-source-index regenerates wire source and compares independent BM25 scoring\n"
    ));
}
