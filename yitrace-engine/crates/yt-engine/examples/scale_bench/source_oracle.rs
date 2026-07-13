use std::cmp::Ordering;
use std::collections::{BTreeMap, BinaryHeap};
use std::time::{Duration, Instant};

use yt_core::event::{EventIdentity, EventType};
use yt_engine::{Bm25TextIndex, ChineseTokenizer, Tokenizer, WireRecord};

use crate::generator::{generate_dataset, DatasetStats, GeneratorConfig};

const K1: f32 = 1.5;
const B: f32 = 0.75;

#[derive(Clone, Copy)]
enum SourceFilter {
    All,
    ScaleA,
}

struct SourceCase {
    name: &'static str,
    query: &'static str,
    k: usize,
    filter: SourceFilter,
}

const CASES: [SourceCase; 6] = [
    SourceCase {
        name: "common-k10",
        query: "任务执行",
        k: 10,
        filter: SourceFilter::All,
    },
    SourceCase {
        name: "common-k50",
        query: "任务执行",
        k: 50,
        filter: SourceFilter::All,
    },
    SourceCase {
        name: "common-project-k10",
        query: "任务执行",
        k: 10,
        filter: SourceFilter::ScaleA,
    },
    SourceCase {
        name: "rare-k10",
        query: "月蚀校验码",
        k: 10,
        filter: SourceFilter::All,
    },
    SourceCase {
        name: "risk-k20",
        query: "支付风控",
        k: 20,
        filter: SourceFilter::All,
    },
    SourceCase {
        name: "multi-term-k20",
        query: "任务执行 支付风控",
        k: 20,
        filter: SourceFilter::All,
    },
];

pub struct SourceOracleReport {
    pub build_duration: Duration,
    pub compare_duration: Duration,
    pub source_events: usize,
    pub unique_source_events: usize,
    pub source_docs: usize,
    pub cases: Vec<SourceOracleCaseResult>,
}

pub struct SourceOracleCaseResult {
    pub name: &'static str,
    pub filter: &'static str,
    pub k: usize,
    pub source_count: usize,
    pub index_count: usize,
    pub recall_at_k: f64,
    pub exact_rank_and_score: bool,
    pub max_score_delta: f32,
}

/// 从确定性 wire 源重新计算固定查询所需的最小 BM25 状态。
///
/// 这里不读 `bm25.dat`，也不调用 `Bm25TextIndex::index_text`。它独立提取 wire 中的
/// input/output/agent/tool/model/logs，并自己累计文档长度和查询词词频，用来发现漏字段、
/// 漏事件和持久索引数据错误。只保留固定查询涉及的词，百万 span 也不复制整套倒排。
pub fn run_source_index_oracle(
    bm25: &Bm25TextIndex,
    expected: &DatasetStats,
    batch_records: usize,
    tenant: u64,
) -> SourceOracleReport {
    let build_started = Instant::now();
    let mut source = SourceBm25::new(CASES.iter().map(|case| case.query));
    let generated = generate_dataset(
        GeneratorConfig {
            spans: expected.spans,
            batch_records,
            seed: expected.seed,
            tenant,
        },
        |records| source.observe_batch(&records),
    );
    source.finish();
    assert_dataset_shape(expected, &generated);
    let build_duration = build_started.elapsed();

    let compare_started = Instant::now();
    let cases = CASES
        .iter()
        .map(|case| {
            eprintln!(
                "scale_bench: source index oracle {} query={:?} k={}",
                case.name, case.query, case.k
            );
            let source_hits = source.search(case.query, case.k, case.filter);
            let index_hits = match case.filter {
                SourceFilter::All => bm25.search_exact_for_eval(case.query, case.k),
                SourceFilter::ScaleA => {
                    let scale_a = |trace_id: u64, _: u64| trace_id > 0 && (trace_id - 1) % 4 == 0;
                    bm25.search_exact_filtered_for_eval(case.query, case.k, &scale_a)
                }
            };
            let source_docs = source_hits
                .iter()
                .map(|&(trace_id, span_id, _)| (trace_id, span_id))
                .collect::<std::collections::HashSet<_>>();
            let index_docs = index_hits
                .iter()
                .map(|&(trace_id, span_id, _)| (trace_id, span_id))
                .collect::<std::collections::HashSet<_>>();
            let recall_at_k = if source_docs.is_empty() {
                if index_docs.is_empty() {
                    1.0
                } else {
                    0.0
                }
            } else {
                source_docs.intersection(&index_docs).count() as f64 / source_docs.len() as f64
            };
            let max_score_delta = source_hits
                .iter()
                .zip(&index_hits)
                .map(|(source, index)| (source.2 - index.2).abs())
                .fold(0.0f32, f32::max);
            SourceOracleCaseResult {
                name: case.name,
                filter: match case.filter {
                    SourceFilter::All => "all",
                    SourceFilter::ScaleA => "project_id=scale-a",
                },
                k: case.k,
                source_count: source_hits.len(),
                index_count: index_hits.len(),
                recall_at_k,
                exact_rank_and_score: source_hits == index_hits,
                max_score_delta,
            }
        })
        .collect();

    SourceOracleReport {
        build_duration,
        compare_duration: compare_started.elapsed(),
        source_events: generated.wire_events,
        unique_source_events: source.unique_events,
        source_docs: source.docs.len(),
        cases,
    }
}

fn assert_dataset_shape(expected: &DatasetStats, generated: &DatasetStats) {
    assert_eq!(generated.spans, expected.spans, "source oracle span drift");
    assert_eq!(
        generated.traces, expected.traces,
        "source oracle trace drift"
    );
    assert_eq!(
        generated.wire_events, expected.wire_events,
        "source oracle event drift"
    );
    assert_eq!(
        generated.duplicate_events, expected.duplicate_events,
        "source oracle duplicate drift"
    );
    assert_eq!(generated.seed, expected.seed, "source oracle seed drift");
}

struct SourceBm25 {
    tokenizer: ChineseTokenizer,
    token_ids: BTreeMap<String, usize>,
    docs: Vec<(u64, u64)>,
    doc_lens: Vec<u32>,
    term_tfs: Vec<Vec<u32>>,
    term_doc_freqs: Vec<u32>,
    total_len: u64,
    current_doc: Option<(u64, u64)>,
    current_len: u32,
    current_tfs: Vec<u32>,
    current_event_ids: Vec<u64>,
    unique_events: usize,
}

impl SourceBm25 {
    fn new<'a>(queries: impl Iterator<Item = &'a str>) -> Self {
        let tokenizer = ChineseTokenizer::full();
        let mut tokens = queries
            .flat_map(|query| tokenizer.tokenize(query))
            .collect::<Vec<_>>();
        tokens.sort_unstable();
        tokens.dedup();
        let token_ids = tokens
            .into_iter()
            .enumerate()
            .map(|(index, token)| (token, index))
            .collect::<BTreeMap<_, _>>();
        let term_count = token_ids.len();
        Self {
            tokenizer,
            token_ids,
            docs: Vec::new(),
            doc_lens: Vec::new(),
            term_tfs: (0..term_count).map(|_| Vec::new()).collect(),
            term_doc_freqs: vec![0; term_count],
            total_len: 0,
            current_doc: None,
            current_len: 0,
            current_tfs: vec![0; term_count],
            current_event_ids: Vec::with_capacity(4),
            unique_events: 0,
        }
    }

    fn observe_batch(&mut self, records: &[WireRecord]) {
        for record in records {
            self.observe(record);
        }
    }

    fn observe(&mut self, record: &WireRecord) {
        let doc = (record.trace_id, record.span_id);
        match self.current_doc {
            Some(current) if current != doc => {
                assert!(
                    current < doc,
                    "source generator must keep documents ordered"
                );
                self.finish_current();
                self.current_doc = Some(doc);
            }
            None => self.current_doc = Some(doc),
            _ => {}
        }

        let event_id = EventIdentity {
            ext_span_id: record.ext_span_id.clone(),
            seq: record.seq,
            event_type: EventType::from_tag(record.event_type_tag),
        }
        .event_id()
        .0;
        if self.current_event_ids.contains(&event_id) {
            return;
        }
        self.current_event_ids.push(event_id);
        self.unique_events += 1;

        let mut parts = Vec::new();
        if let Some(text) = record.input_text.as_deref() {
            parts.push(text);
        }
        if let Some(text) = record.output_text.as_deref() {
            parts.push(text);
        }
        for field in [&record.agent_name, &record.tool_name, &record.model] {
            if let Some(text) = field.as_deref() {
                parts.push(text);
            }
        }
        for log in &record.logs {
            parts.push(log);
        }
        if parts.is_empty() {
            return;
        }
        let tokens = self.tokenizer.tokenize(&parts.join(" "));
        self.current_len = self
            .current_len
            .checked_add(tokens.len() as u32)
            .expect("source oracle document length overflow");
        for token in tokens {
            if let Some(&term) = self.token_ids.get(&token) {
                self.current_tfs[term] = self.current_tfs[term]
                    .checked_add(1)
                    .expect("source oracle term frequency overflow");
            }
        }
    }

    fn finish(&mut self) {
        self.finish_current();
    }

    fn finish_current(&mut self) {
        let Some(doc) = self.current_doc.take() else {
            return;
        };
        if self.current_len == 0 {
            self.current_tfs.fill(0);
            self.current_event_ids.clear();
            return;
        }
        self.docs.push(doc);
        self.doc_lens.push(self.current_len);
        self.total_len += u64::from(self.current_len);
        for (term, tf) in self.current_tfs.iter_mut().enumerate() {
            self.term_tfs[term].push(*tf);
            if *tf > 0 {
                self.term_doc_freqs[term] += 1;
            }
            *tf = 0;
        }
        self.current_len = 0;
        self.current_event_ids.clear();
    }

    fn search(&self, query: &str, k: usize, filter: SourceFilter) -> Vec<(u64, u64, f32)> {
        if self.docs.is_empty() || k == 0 {
            return Vec::new();
        }
        let mut query_tokens = self.tokenizer.tokenize(query);
        query_tokens.sort_unstable();
        query_tokens.dedup();
        let terms = query_tokens
            .iter()
            .filter_map(|token| self.token_ids.get(token).copied())
            .collect::<Vec<_>>();
        if terms.is_empty() {
            return Vec::new();
        }
        let n = self.docs.len() as f32;
        let avgdl = self.total_len as f32 / n;
        let mut heap = BinaryHeap::new();
        for (doc_index, &doc) in self.docs.iter().enumerate() {
            if matches!(filter, SourceFilter::ScaleA) && (doc.0 == 0 || (doc.0 - 1) % 4 != 0) {
                continue;
            }
            let mut score = 0.0f32;
            for &term in &terms {
                let tf = self.term_tfs[term][doc_index];
                if tf == 0 {
                    continue;
                }
                let df = self.term_doc_freqs[term] as f32;
                let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
                score += idf * bm25_norm(tf as f32, self.doc_lens[doc_index] as f32, avgdl);
            }
            if score > 0.0 {
                push_topk(&mut heap, k, doc, score);
            }
        }
        let mut hits = heap
            .into_iter()
            .map(|hit| (hit.doc.0, hit.doc.1, hit.score.0))
            .collect::<Vec<_>>();
        hits.sort_by(|left, right| {
            right
                .2
                .total_cmp(&left.2)
                .then((left.0, left.1).cmp(&(right.0, right.1)))
        });
        hits
    }
}

fn bm25_norm(tf: f32, dl: f32, avgdl: f32) -> f32 {
    tf * (K1 + 1.0) / (tf + K1 * (1.0 - B + B * dl / avgdl))
}

#[derive(Clone, Copy, PartialEq)]
struct OrdF32(f32);

impl Eq for OrdF32 {}

impl PartialOrd for OrdF32 {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrdF32 {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0)
    }
}

#[derive(Clone, Copy, PartialEq)]
struct RankedScore {
    doc: (u64, u64),
    score: OrdF32,
}

impl Eq for RankedScore {}

impl PartialOrd for RankedScore {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RankedScore {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .score
            .cmp(&self.score)
            .then_with(|| self.doc.cmp(&other.doc))
    }
}

fn push_topk(heap: &mut BinaryHeap<RankedScore>, k: usize, doc: (u64, u64), score: f32) {
    let item = RankedScore {
        doc,
        score: OrdF32(score),
    };
    if heap.len() < k {
        heap.push(item);
    } else if heap.peek().is_some_and(|worst| item < *worst) {
        heap.pop();
        heap.push(item);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yt_engine::Bm25Index;

    #[test]
    fn compact_source_scorer_matches_direct_bm25() {
        let bm25 = Bm25TextIndex::with_tokenizer(Box::new(ChineseTokenizer::full()));
        let mut source = SourceBm25::new(["任务执行", "月蚀校验码"].into_iter());
        let records = vec![
            wire(1, 10, Some("任务执行 月蚀校验码"), None),
            wire(1, 10, None, Some("任务执行完成")),
            wire(2, 20, Some("任务执行"), None),
        ];
        for record in &records {
            let mut parts = Vec::new();
            if let Some(text) = record.input_text.as_deref() {
                parts.push(text);
            }
            if let Some(text) = record.output_text.as_deref() {
                parts.push(text);
            }
            bm25.index_text(record.trace_id, record.span_id, &parts.join(" "));
        }
        source.observe_batch(&records);
        source.finish();
        assert_eq!(
            source.search("任务执行", 10, SourceFilter::All),
            bm25.search_exact_for_eval("任务执行", 10)
        );
        assert_eq!(
            source.search("月蚀校验码", 10, SourceFilter::All),
            bm25.search_exact_for_eval("月蚀校验码", 10)
        );
    }

    fn wire(
        trace_id: u64,
        span_id: u64,
        input_text: Option<&str>,
        output_text: Option<&str>,
    ) -> WireRecord {
        let is_end = output_text.is_some();
        WireRecord {
            trace_id,
            span_id,
            ts: 1,
            seq: if is_end { 2 } else { 1 },
            event_type_tag: if is_end { 2 } else { 1 },
            ext_span_id: format!("{trace_id}-{span_id}"),
            parent_span_id: None,
            status: None,
            duration_ns: None,
            input_tokens: None,
            output_tokens: None,
            session_id: None,
            tenant_id: Some(42),
            external_trace_id: None,
            external_span_id: None,
            external_parent_span_id: None,
            external_session_id: None,
            agent_name: None,
            tool_name: None,
            model: None,
            input_text: input_text.map(str::to_owned),
            output_text: output_text.map(str::to_owned),
            logs: Vec::new(),
            attrs: BTreeMap::new(),
        }
    }
}
