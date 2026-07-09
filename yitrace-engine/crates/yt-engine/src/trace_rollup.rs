use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::Path;

use crate::{ReadPlanStats, SearchFilter, TraceQuery};
use yt_core::fold::{FoldedSpan, SpanFields};
use yt_wal::WalRecord;

const CACHE_MAGIC: u32 = 0x5954_524f; // "YTRO"
const CACHE_VERSION: u32 = 1;

#[derive(Default)]
pub(crate) struct TraceAggregateRollupIndex {
    rows: HashMap<(u64, u64), TraceAggregateRollupRow>,
    by_trace: BTreeMap<u64, Vec<u64>>,
    dirty: bool,
}

impl TraceAggregateRollupIndex {
    pub(crate) fn apply_record(&mut self, record: &WalRecord) {
        if self.dirty {
            return;
        }
        let key = (record.trace_id, record.span_id);
        if !self.rows.contains_key(&key) {
            self.by_trace
                .entry(record.trace_id)
                .or_default()
                .push(record.span_id);
        }
        let row = self
            .rows
            .entry(key)
            .or_insert_with(|| TraceAggregateRollupRow::new(record.trace_id, record.span_id));
        row.apply_record(record);
    }

    pub(crate) fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub(crate) fn from_records(
        records: impl IntoIterator<Item = WalRecord>,
        patches: impl IntoIterator<Item = ((u64, u64), SpanFields)>,
    ) -> Self {
        let mut next = Self::default();
        for record in records {
            next.apply_record(&record);
        }
        for ((trace_id, span_id), fields) in patches {
            if let Some(row) = next.rows.get_mut(&(trace_id, span_id)) {
                row.apply_fields(&fields);
            }
        }
        next
    }

    pub(crate) fn rebuild(
        &mut self,
        records: impl IntoIterator<Item = WalRecord>,
        patches: impl IntoIterator<Item = ((u64, u64), SpanFields)>,
    ) {
        *self = Self::from_records(records, patches);
    }

    pub(crate) fn len(&self) -> usize {
        self.rows.len()
    }

    pub(crate) fn save_cache(
        &self,
        path: &Path,
        manifest_version: u64,
        memtable_watermark: u64,
    ) -> std::io::Result<()> {
        let Some(parent) = path.parent() else {
            return Ok(());
        };
        std::fs::create_dir_all(parent)?;
        let tmp = path.with_extension("tmp");
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(&self.encode_cache(manifest_version, memtable_watermark))?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(tmp, path)
    }

    pub(crate) fn load_cache(
        path: &Path,
        manifest_version: u64,
        memtable_watermark: u64,
    ) -> Option<Self> {
        let bytes = std::fs::read(path).ok()?;
        Self::decode_cache(&bytes, manifest_version, memtable_watermark)
    }

    pub(crate) fn query(
        &self,
        query: &TraceQuery,
        filter: &SearchFilter,
    ) -> Option<(Vec<FoldedSpan>, ReadPlanStats)> {
        if self.dirty {
            let stats = ReadPlanStats {
                source: Some("scan".to_string()),
                fallback_reason: Some("rollup_dirty".to_string()),
                ..ReadPlanStats::default()
            };
            return Some((Vec::new(), stats));
        }
        let mut spans = Vec::new();
        for row in self.rows.values() {
            if !row.matches_query(query) || !row.matches_filter(filter) {
                continue;
            }
            spans.push(row.to_folded_span());
        }
        spans.sort_by_key(|span| (span.trace_id, span.span_id));
        let stats = ReadPlanStats {
            source: Some("aggregate_rollup".to_string()),
            used_filter_index: filter.needs_indexed_filter(),
            candidate_span_keys: Some(spans.len()),
            scanned_segments: 0,
            matched_spans: spans.len(),
            ..ReadPlanStats::default()
        };
        Some((spans, stats))
    }

    pub(crate) fn query_trace_ids(
        &self,
        trace_ids: &[u64],
        tenant: Option<u64>,
    ) -> Option<(BTreeMap<u64, Vec<FoldedSpan>>, ReadPlanStats)> {
        if self.dirty {
            let stats = ReadPlanStats {
                source: Some("scan".to_string()),
                fallback_reason: Some("rollup_dirty".to_string()),
                ..ReadPlanStats::default()
            };
            return Some((BTreeMap::new(), stats));
        }
        let mut out: BTreeMap<u64, Vec<FoldedSpan>> = BTreeMap::new();
        let mut matched = 0usize;
        for trace_id in trace_ids
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
        {
            let Some(span_ids) = self.by_trace.get(&trace_id) else {
                continue;
            };
            for span_id in span_ids {
                let Some(row) = self.rows.get(&(trace_id, *span_id)) else {
                    continue;
                };
                if tenant.is_some() && row.tenant_id != tenant {
                    continue;
                }
                out.entry(trace_id).or_default().push(row.to_folded_span());
                matched += 1;
            }
        }
        for spans in out.values_mut() {
            spans.sort_by_key(|span| span.span_id);
        }
        let stats = ReadPlanStats {
            source: Some("trajectory_rollup".to_string()),
            candidate_span_keys: Some(matched),
            scanned_segments: 0,
            matched_spans: matched,
            ..ReadPlanStats::default()
        };
        Some((out, stats))
    }

    fn encode_cache(&self, manifest_version: u64, memtable_watermark: u64) -> Vec<u8> {
        let mut out = Vec::new();
        put_u32(&mut out, CACHE_MAGIC);
        put_u32(&mut out, CACHE_VERSION);
        put_u64(&mut out, manifest_version);
        put_u64(&mut out, memtable_watermark);
        let mut rows: Vec<&TraceAggregateRollupRow> = self.rows.values().collect();
        rows.sort_by_key(|row| (row.trace_id, row.span_id));
        put_u64(&mut out, rows.len() as u64);
        for row in rows {
            row.encode(&mut out);
        }
        out
    }

    fn decode_cache(bytes: &[u8], manifest_version: u64, memtable_watermark: u64) -> Option<Self> {
        let mut cur = CacheCursor { bytes, pos: 0 };
        if cur.u32()? != CACHE_MAGIC || cur.u32()? != CACHE_VERSION {
            return None;
        }
        if cur.u64()? != manifest_version || cur.u64()? != memtable_watermark {
            return None;
        }
        let count = cur.u64()? as usize;
        let mut rows = HashMap::new();
        for _ in 0..count {
            let row = TraceAggregateRollupRow::decode(&mut cur)?;
            rows.insert((row.trace_id, row.span_id), row);
        }
        if cur.pos != bytes.len() {
            return None;
        }
        let mut by_trace: BTreeMap<u64, Vec<u64>> = BTreeMap::new();
        for &(trace_id, span_id) in rows.keys() {
            by_trace.entry(trace_id).or_default().push(span_id);
        }
        for span_ids in by_trace.values_mut() {
            span_ids.sort_unstable();
        }
        Some(Self {
            rows,
            by_trace,
            dirty: false,
        })
    }
}

#[derive(Clone, Debug)]
struct TraceAggregateRollupRow {
    trace_id: u64,
    span_id: u64,
    parent_span_id: Option<u64>,
    session_id: Option<u64>,
    tenant_id: Option<u64>,
    external_trace_id: Option<String>,
    external_span_id: Option<String>,
    external_parent_span_id: Option<String>,
    external_session_id: Option<String>,
    status: Option<u8>,
    duration_ns: Option<u64>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    agent_name: Option<String>,
    tool_name: Option<String>,
    model: Option<String>,
    attrs: BTreeMap<String, String>,
    min_ts: i64,
    max_ts: i64,
    event_count: usize,
}

impl TraceAggregateRollupRow {
    fn new(trace_id: u64, span_id: u64) -> Self {
        Self {
            trace_id,
            span_id,
            parent_span_id: None,
            session_id: None,
            tenant_id: None,
            external_trace_id: None,
            external_span_id: None,
            external_parent_span_id: None,
            external_session_id: None,
            status: None,
            duration_ns: None,
            input_tokens: None,
            output_tokens: None,
            agent_name: None,
            tool_name: None,
            model: None,
            attrs: BTreeMap::new(),
            min_ts: i64::MAX,
            max_ts: i64::MIN,
            event_count: 0,
        }
    }

    fn apply_record(&mut self, record: &WalRecord) {
        self.min_ts = self.min_ts.min(record.ts);
        self.max_ts = self.max_ts.max(record.ts);
        self.event_count = self.event_count.saturating_add(1);
        self.apply_fields(&record.fields);
    }

    fn apply_fields(&mut self, fields: &SpanFields) {
        if fields.session_id.is_some() {
            self.session_id = fields.session_id;
        }
        if fields.tenant_id.is_some() {
            self.tenant_id = fields.tenant_id;
        }
        if fields.external_trace_id.is_some() {
            self.external_trace_id = fields.external_trace_id.clone();
        }
        if fields.external_span_id.is_some() {
            self.external_span_id = fields.external_span_id.clone();
        }
        if fields.external_parent_span_id.is_some() {
            self.external_parent_span_id = fields.external_parent_span_id.clone();
        }
        if fields.external_session_id.is_some() {
            self.external_session_id = fields.external_session_id.clone();
        }
        if fields.status.is_some() {
            self.status = fields.status;
        }
        if fields.parent_span_id.is_some() {
            self.parent_span_id = fields.parent_span_id;
        }
        if fields.duration_ns.is_some() {
            self.duration_ns = fields.duration_ns;
        }
        if fields.input_tokens.is_some() {
            self.input_tokens = fields.input_tokens;
        }
        if fields.output_tokens.is_some() {
            self.output_tokens = fields.output_tokens;
        }
        if fields.agent_name.is_some() {
            self.agent_name = fields.agent_name.clone();
        }
        if fields.tool_name.is_some() {
            self.tool_name = fields.tool_name.clone();
        }
        if fields.model.is_some() {
            self.model = fields.model.clone();
        }
        for (key, value) in &fields.attrs {
            self.attrs.insert(key.clone(), value.clone());
        }
    }

    fn matches_query(&self, query: &TraceQuery) -> bool {
        if query
            .trace_id
            .map_or(false, |trace_id| trace_id != self.trace_id)
        {
            return false;
        }
        if let Some(tenant_id) = query.tenant_id {
            if self.tenant_id != Some(tenant_id) {
                return false;
            }
        }
        self.max_ts >= query.time_from && self.min_ts <= query.time_to
    }

    fn matches_filter(&self, filter: &SearchFilter) -> bool {
        if filter
            .trace_id
            .map_or(false, |trace_id| trace_id != self.trace_id)
        {
            return false;
        }
        if let Some(tenant_id) = filter.tenant_id {
            if self.tenant_id != Some(tenant_id) {
                return false;
            }
        }
        if let Some(external_trace_id) = &filter.external_trace_id {
            if self.external_trace_id.as_deref() != Some(external_trace_id.as_str()) {
                return false;
            }
        }
        if let Some(agent_name) = &filter.agent_name {
            if self.agent_name.as_deref() != Some(agent_name.as_str()) {
                return false;
            }
        }
        if let Some(tool_name) = &filter.tool_name {
            if self.tool_name.as_deref() != Some(tool_name.as_str()) {
                return false;
            }
        }
        if let Some(model) = &filter.model {
            if self.model.as_deref() != Some(model.as_str()) {
                return false;
            }
        }
        if let Some(status) = filter.status {
            if self.status != Some(status) {
                return false;
            }
        }
        if let Some(from) = filter.time_from {
            if self.max_ts < from {
                return false;
            }
        }
        if let Some(to) = filter.time_to {
            if self.min_ts > to {
                return false;
            }
        }
        for (key, expected) in &filter.attrs {
            if self.attrs.get(key) != Some(expected) {
                return false;
            }
        }
        true
    }

    fn to_folded_span(&self) -> FoldedSpan {
        FoldedSpan {
            trace_id: self.trace_id,
            span_id: self.span_id,
            parent_span_id: self.parent_span_id,
            status: self.status,
            duration_ns: self.duration_ns,
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            session_id: self.session_id,
            tenant_id: self.tenant_id,
            external_trace_id: self.external_trace_id.clone(),
            external_span_id: self.external_span_id.clone(),
            external_parent_span_id: self.external_parent_span_id.clone(),
            external_session_id: self.external_session_id.clone(),
            agent_name: self.agent_name.clone(),
            tool_name: self.tool_name.clone(),
            model: self.model.clone(),
            input_text: None,
            output_text: None,
            eval_score: None,
            eval_label: None,
            logs: Vec::new(),
            attrs: self.attrs.clone(),
            event_count: self.event_count,
        }
    }

    fn encode(&self, out: &mut Vec<u8>) {
        put_u64(out, self.trace_id);
        put_u64(out, self.span_id);
        put_opt_u64(out, self.parent_span_id);
        put_opt_u64(out, self.session_id);
        put_opt_u64(out, self.tenant_id);
        put_opt_string(out, self.external_trace_id.as_deref());
        put_opt_string(out, self.external_span_id.as_deref());
        put_opt_string(out, self.external_parent_span_id.as_deref());
        put_opt_string(out, self.external_session_id.as_deref());
        put_opt_u8(out, self.status);
        put_opt_u64(out, self.duration_ns);
        put_opt_u64(out, self.input_tokens);
        put_opt_u64(out, self.output_tokens);
        put_opt_string(out, self.agent_name.as_deref());
        put_opt_string(out, self.tool_name.as_deref());
        put_opt_string(out, self.model.as_deref());
        put_i64(out, self.min_ts);
        put_i64(out, self.max_ts);
        put_u64(out, self.event_count as u64);
        put_u64(out, self.attrs.len() as u64);
        for (key, value) in &self.attrs {
            put_string(out, key);
            put_string(out, value);
        }
    }

    fn decode(cur: &mut CacheCursor<'_>) -> Option<Self> {
        let trace_id = cur.u64()?;
        let span_id = cur.u64()?;
        let parent_span_id = cur.opt_u64()?;
        let session_id = cur.opt_u64()?;
        let tenant_id = cur.opt_u64()?;
        let external_trace_id = cur.opt_string()?;
        let external_span_id = cur.opt_string()?;
        let external_parent_span_id = cur.opt_string()?;
        let external_session_id = cur.opt_string()?;
        let status = cur.opt_u8()?;
        let duration_ns = cur.opt_u64()?;
        let input_tokens = cur.opt_u64()?;
        let output_tokens = cur.opt_u64()?;
        let agent_name = cur.opt_string()?;
        let tool_name = cur.opt_string()?;
        let model = cur.opt_string()?;
        let min_ts = cur.i64()?;
        let max_ts = cur.i64()?;
        let event_count = cur.u64()? as usize;
        let attr_count = cur.u64()? as usize;
        let mut attrs = BTreeMap::new();
        for _ in 0..attr_count {
            attrs.insert(cur.string()?, cur.string()?);
        }
        Some(Self {
            trace_id,
            span_id,
            parent_span_id,
            session_id,
            tenant_id,
            external_trace_id,
            external_span_id,
            external_parent_span_id,
            external_session_id,
            status,
            duration_ns,
            input_tokens,
            output_tokens,
            agent_name,
            tool_name,
            model,
            attrs,
            min_ts,
            max_ts,
            event_count,
        })
    }
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_i64(out: &mut Vec<u8>, value: i64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_opt_u8(out: &mut Vec<u8>, value: Option<u8>) {
    match value {
        Some(value) => {
            out.push(1);
            out.push(value);
        }
        None => out.push(0),
    }
}

fn put_opt_u64(out: &mut Vec<u8>, value: Option<u64>) {
    match value {
        Some(value) => {
            out.push(1);
            put_u64(out, value);
        }
        None => out.push(0),
    }
}

fn put_string(out: &mut Vec<u8>, value: &str) {
    put_u64(out, value.len() as u64);
    out.extend_from_slice(value.as_bytes());
}

fn put_opt_string(out: &mut Vec<u8>, value: Option<&str>) {
    match value {
        Some(value) => {
            out.push(1);
            put_string(out, value);
        }
        None => out.push(0),
    }
}

struct CacheCursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> CacheCursor<'a> {
    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(len)?;
        let bytes = self.bytes.get(self.pos..end)?;
        self.pos = end;
        Some(bytes)
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    fn i64(&mut self) -> Option<i64> {
        Some(i64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    fn u8(&mut self) -> Option<u8> {
        let value = *self.bytes.get(self.pos)?;
        self.pos += 1;
        Some(value)
    }

    fn opt_u8(&mut self) -> Option<Option<u8>> {
        match self.u8()? {
            0 => Some(None),
            1 => Some(Some(self.u8()?)),
            _ => None,
        }
    }

    fn opt_u64(&mut self) -> Option<Option<u64>> {
        match self.u8()? {
            0 => Some(None),
            1 => Some(Some(self.u64()?)),
            _ => None,
        }
    }

    fn string(&mut self) -> Option<String> {
        let len = self.u64()? as usize;
        String::from_utf8(self.take(len)?.to_vec()).ok()
    }

    fn opt_string(&mut self) -> Option<Option<String>> {
        match self.u8()? {
            0 => Some(None),
            1 => Some(Some(self.string()?)),
            _ => None,
        }
    }
}
