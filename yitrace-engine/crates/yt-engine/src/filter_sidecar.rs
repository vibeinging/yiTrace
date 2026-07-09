use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::Path;

use crate::{is_filter_attr_key, FilterAttrs, SearchFilter};
use yt_core::fold::SpanFields;
use yt_wal::WalRecord;

type SpanKey = (u64, u64);

const CACHE_MAGIC: u32 = 0x5954_4641; // "YTFA"
const CACHE_VERSION: u32 = 2;
const DEFAULT_POSTING_ENTRY_BUDGET: usize = 2_000_000;
const DEFAULT_POSTING_SET_BUDGET: usize = 200_000;

pub(crate) struct FilterAttrsIndex {
    rows: HashMap<SpanKey, FilterAttrs>,
    postings: HashMap<PostingKey, HashSet<SpanKey>>,
    disabled_postings: HashSet<PostingKey>,
    posting_entries: usize,
    posting_entry_budget: usize,
    posting_set_budget: usize,
}

impl Default for FilterAttrsIndex {
    fn default() -> Self {
        Self::with_posting_budget(DEFAULT_POSTING_ENTRY_BUDGET, DEFAULT_POSTING_SET_BUDGET)
    }
}

#[derive(Clone, Debug, Eq)]
struct PostingKey {
    field: String,
    value: String,
}

impl PostingKey {
    fn new(field: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            value: value.into(),
        }
    }
}

impl PartialEq for PostingKey {
    fn eq(&self, other: &Self) -> bool {
        self.field == other.field && self.value == other.value
    }
}

impl Hash for PostingKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.field.hash(state);
        self.value.hash(state);
    }
}

enum PostingLookup<'a> {
    Resident(&'a HashSet<SpanKey>),
    Disabled,
    Missing,
}

impl FilterAttrsIndex {
    pub(crate) fn with_posting_budget(
        posting_entry_budget: usize,
        posting_set_budget: usize,
    ) -> Self {
        Self {
            rows: HashMap::new(),
            postings: HashMap::new(),
            disabled_postings: HashSet::new(),
            posting_entries: 0,
            posting_entry_budget,
            posting_set_budget,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.rows.len()
    }

    pub(crate) fn posting_count(&self) -> usize {
        self.posting_entries
    }

    pub(crate) fn disabled_posting_count(&self) -> usize {
        self.disabled_postings.len()
    }

    pub(crate) fn apply_record(&mut self, record: &WalRecord) {
        let key = (record.trace_id, record.span_id);
        if let Some(old) = self.rows.get(&key).cloned() {
            self.remove_postings(key, &old);
        }
        let mut row = self.rows.remove(&key).unwrap_or_else(|| FilterAttrs {
            min_ts: record.ts,
            max_ts: record.ts,
            ..Default::default()
        });
        row.apply_record(record);
        self.add_postings(key, &row);
        self.rows.insert(key, row);
    }

    pub(crate) fn from_records(
        records: impl IntoIterator<Item = WalRecord>,
        patches: impl IntoIterator<Item = (SpanKey, SpanFields)>,
    ) -> Self {
        let mut next = Self::default();
        for record in records {
            next.apply_record(&record);
        }
        for (key, fields) in patches {
            next.apply_fields(key, &fields);
        }
        next
    }

    pub(crate) fn rebuild(
        &mut self,
        records: impl IntoIterator<Item = WalRecord>,
        patches: impl IntoIterator<Item = (SpanKey, SpanFields)>,
    ) {
        *self = Self::from_records(records, patches);
    }

    pub(crate) fn candidate_span_keys(&self, filter: &SearchFilter) -> HashSet<SpanKey> {
        let mut sets: Vec<&HashSet<SpanKey>> = Vec::new();
        if let Some(trace_id) = filter.trace_id {
            match self.posting_lookup(&PostingKey::new("trace_id", trace_id.to_string())) {
                PostingLookup::Resident(set) => sets.push(set),
                PostingLookup::Disabled => {}
                PostingLookup::Missing => return HashSet::new(),
            }
        }
        if let Some(external_trace_id) = &filter.external_trace_id {
            match self.posting_lookup(&PostingKey::new(
                "external_trace_id",
                external_trace_id.as_str(),
            )) {
                PostingLookup::Resident(set) => sets.push(set),
                PostingLookup::Disabled => {}
                PostingLookup::Missing => return HashSet::new(),
            }
        }
        if let Some(tenant_id) = filter.tenant_id {
            match self.posting_lookup(&PostingKey::new("tenant_id", tenant_id.to_string())) {
                PostingLookup::Resident(set) => sets.push(set),
                PostingLookup::Disabled => {}
                PostingLookup::Missing => return HashSet::new(),
            }
        }
        if let Some(status) = filter.status {
            match self.posting_lookup(&PostingKey::new("status", status.to_string())) {
                PostingLookup::Resident(set) => sets.push(set),
                PostingLookup::Disabled => {}
                PostingLookup::Missing => return HashSet::new(),
            }
        }
        for (field, value) in [
            ("agent_name", filter.agent_name.as_deref()),
            ("tool_name", filter.tool_name.as_deref()),
            ("model", filter.model.as_deref()),
        ] {
            if let Some(value) = value {
                match self.posting_lookup(&PostingKey::new(field, value)) {
                    PostingLookup::Resident(set) => sets.push(set),
                    PostingLookup::Disabled => {}
                    PostingLookup::Missing => return HashSet::new(),
                }
            }
        }
        for (key, value) in &filter.attrs {
            match self.posting_lookup(&PostingKey::new(attr_field(key), value)) {
                PostingLookup::Resident(set) => sets.push(set),
                PostingLookup::Disabled => {}
                PostingLookup::Missing => return HashSet::new(),
            }
        }

        let mut out = if let Some((first, rest)) = smallest_first(&sets) {
            let mut out = first.clone();
            for set in rest {
                out.retain(|key| set.contains(key));
            }
            out
        } else {
            self.rows.keys().copied().collect()
        };
        out.retain(|key| {
            if let Some(trace_id) = filter.trace_id {
                if key.0 != trace_id {
                    return false;
                }
            }
            self.rows
                .get(key)
                .map(|attrs| filter.attrs_match(attrs))
                .unwrap_or(false)
        });
        out
    }

    pub(crate) fn matches_key(&self, trace_id: u64, span_id: u64, filter: &SearchFilter) -> bool {
        self.rows
            .get(&(trace_id, span_id))
            .map(|attrs| filter.attrs_match(attrs))
            .unwrap_or(false)
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

    fn apply_fields(&mut self, key: SpanKey, fields: &SpanFields) {
        let Some(mut row) = self.rows.remove(&key) else {
            return;
        };
        self.remove_postings(key, &row);
        row.apply_fields(fields);
        self.add_postings(key, &row);
        self.rows.insert(key, row);
    }

    fn add_postings(&mut self, key: SpanKey, row: &FilterAttrs) {
        self.add_posting(PostingKey::new("trace_id", key.0.to_string()), key);
        if let Some(external_trace_id) = &row.external_trace_id {
            self.add_posting(
                PostingKey::new("external_trace_id", external_trace_id.as_str()),
                key,
            );
        }
        if let Some(tenant_id) = row.tenant_id {
            self.add_posting(PostingKey::new("tenant_id", tenant_id.to_string()), key);
        }
        if let Some(status) = row.status {
            self.add_posting(PostingKey::new("status", status.to_string()), key);
        }
        for (field, value) in [
            ("agent_name", row.agent_name.as_deref()),
            ("tool_name", row.tool_name.as_deref()),
            ("model", row.model.as_deref()),
        ] {
            if let Some(value) = value {
                self.add_posting(PostingKey::new(field, value), key);
            }
        }
        for (attr_key, value) in &row.attrs {
            self.add_posting(PostingKey::new(attr_field(attr_key), value), key);
        }
    }

    fn remove_postings(&mut self, key: SpanKey, row: &FilterAttrs) {
        self.remove_posting(&PostingKey::new("trace_id", key.0.to_string()), key);
        if let Some(external_trace_id) = &row.external_trace_id {
            self.remove_posting(
                &PostingKey::new("external_trace_id", external_trace_id.as_str()),
                key,
            );
        }
        if let Some(tenant_id) = row.tenant_id {
            self.remove_posting(&PostingKey::new("tenant_id", tenant_id.to_string()), key);
        }
        if let Some(status) = row.status {
            self.remove_posting(&PostingKey::new("status", status.to_string()), key);
        }
        for (field, value) in [
            ("agent_name", row.agent_name.as_deref()),
            ("tool_name", row.tool_name.as_deref()),
            ("model", row.model.as_deref()),
        ] {
            if let Some(value) = value {
                self.remove_posting(&PostingKey::new(field, value), key);
            }
        }
        for (attr_key, value) in &row.attrs {
            self.remove_posting(&PostingKey::new(attr_field(attr_key), value), key);
        }
    }

    fn add_posting(&mut self, posting: PostingKey, key: SpanKey) {
        if self.disabled_postings.contains(&posting) {
            return;
        }
        let already_present = self
            .postings
            .get(&posting)
            .map_or(false, |keys| keys.contains(&key));
        if already_present {
            return;
        }
        let would_exceed_set = self
            .postings
            .get(&posting)
            .map_or(false, |keys| keys.len() >= self.posting_set_budget);
        if would_exceed_set || self.posting_entries >= self.posting_entry_budget {
            self.disable_posting(posting);
            return;
        }
        self.postings.entry(posting).or_default().insert(key);
        self.posting_entries = self.posting_entries.saturating_add(1);
    }

    fn remove_posting(&mut self, posting: &PostingKey, key: SpanKey) {
        if self.disabled_postings.contains(posting) {
            return;
        }
        let remove_posting = if let Some(keys) = self.postings.get_mut(posting) {
            let removed = keys.remove(&key);
            if removed {
                self.posting_entries = self.posting_entries.saturating_sub(1);
            }
            keys.is_empty()
        } else {
            false
        };
        if remove_posting {
            self.postings.remove(posting);
        }
    }

    fn disable_posting(&mut self, posting: PostingKey) {
        if let Some(keys) = self.postings.remove(&posting) {
            self.posting_entries = self.posting_entries.saturating_sub(keys.len());
        }
        self.disabled_postings.insert(posting);
    }

    fn posting_lookup(&self, posting: &PostingKey) -> PostingLookup<'_> {
        if let Some(keys) = self.postings.get(posting) {
            PostingLookup::Resident(keys)
        } else if self.disabled_postings.contains(posting) {
            PostingLookup::Disabled
        } else {
            PostingLookup::Missing
        }
    }

    fn encode_cache(&self, manifest_version: u64, memtable_watermark: u64) -> Vec<u8> {
        let mut out = Vec::new();
        put_u32(&mut out, CACHE_MAGIC);
        put_u32(&mut out, CACHE_VERSION);
        put_u64(&mut out, manifest_version);
        put_u64(&mut out, memtable_watermark);
        let mut rows: Vec<_> = self.rows.iter().collect();
        rows.sort_by_key(|(&(trace_id, span_id), _)| (trace_id, span_id));
        put_u64(&mut out, rows.len() as u64);
        for (&(trace_id, span_id), row) in rows {
            put_u64(&mut out, trace_id);
            put_u64(&mut out, span_id);
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
            let trace_id = cur.u64()?;
            let span_id = cur.u64()?;
            let row = FilterAttrs::decode(&mut cur)?;
            rows.insert((trace_id, span_id), row);
        }
        if cur.pos != bytes.len() {
            return None;
        }
        let mut out = Self {
            rows,
            ..Default::default()
        };
        out.rebuild_postings();
        Some(out)
    }

    fn rebuild_postings(&mut self) {
        self.postings.clear();
        self.disabled_postings.clear();
        self.posting_entries = 0;
        let rows: Vec<_> = self
            .rows
            .iter()
            .map(|(&key, row)| (key, row.clone()))
            .collect();
        for (key, row) in rows {
            self.add_postings(key, &row);
        }
    }
}

impl FilterAttrs {
    fn apply_record(&mut self, record: &WalRecord) {
        self.min_ts = self.min_ts.min(record.ts);
        self.max_ts = self.max_ts.max(record.ts);
        self.apply_fields(&record.fields);
    }

    fn apply_fields(&mut self, fields: &SpanFields) {
        if fields.external_trace_id.is_some() {
            self.external_trace_id = fields.external_trace_id.clone();
        }
        if fields.status.is_some() {
            self.status = fields.status;
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
        if fields.tenant_id.is_some() {
            self.tenant_id = fields.tenant_id;
        }
        for (key, value) in &fields.attrs {
            if is_filter_attr_key(key) {
                self.attrs.insert(key.clone(), value.clone());
            }
        }
    }

    fn encode(&self, out: &mut Vec<u8>) {
        put_opt_string(out, self.external_trace_id.as_deref());
        put_opt_u8(out, self.status);
        put_opt_string(out, self.agent_name.as_deref());
        put_opt_string(out, self.tool_name.as_deref());
        put_opt_string(out, self.model.as_deref());
        put_i64(out, self.min_ts);
        put_i64(out, self.max_ts);
        put_opt_u64(out, self.tenant_id);
        put_u64(out, self.attrs.len() as u64);
        for (key, value) in &self.attrs {
            put_string(out, key);
            put_string(out, value);
        }
    }

    fn decode(cur: &mut CacheCursor<'_>) -> Option<Self> {
        let external_trace_id = cur.opt_string()?;
        let status = cur.opt_u8()?;
        let agent_name = cur.opt_string()?;
        let tool_name = cur.opt_string()?;
        let model = cur.opt_string()?;
        let min_ts = cur.i64()?;
        let max_ts = cur.i64()?;
        let tenant_id = cur.opt_u64()?;
        let attr_count = cur.u64()? as usize;
        let mut attrs = BTreeMap::new();
        for _ in 0..attr_count {
            attrs.insert(cur.string()?, cur.string()?);
        }
        Some(Self {
            external_trace_id,
            status,
            agent_name,
            tool_name,
            model,
            attrs,
            min_ts,
            max_ts,
            tenant_id,
        })
    }
}

fn smallest_first<'a>(
    sets: &[&'a HashSet<SpanKey>],
) -> Option<(&'a HashSet<SpanKey>, Vec<&'a HashSet<SpanKey>>)> {
    let (idx, first) = sets.iter().enumerate().min_by_key(|(_, set)| set.len())?;
    let rest = sets
        .iter()
        .enumerate()
        .filter_map(|(i, set)| (i != idx).then_some(*set))
        .collect();
    Some((*first, rest))
}

fn attr_field(key: &str) -> String {
    format!("attr:{key}")
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
    out.push(value.is_some() as u8);
    if let Some(value) = value {
        out.push(value);
    }
}

fn put_opt_u64(out: &mut Vec<u8>, value: Option<u64>) {
    out.push(value.is_some() as u8);
    if let Some(value) = value {
        put_u64(out, value);
    }
}

fn put_string(out: &mut Vec<u8>, value: &str) {
    put_u64(out, value.len() as u64);
    out.extend_from_slice(value.as_bytes());
}

fn put_opt_string(out: &mut Vec<u8>, value: Option<&str>) {
    out.push(value.is_some() as u8);
    if let Some(value) = value {
        put_string(out, value);
    }
}

struct CacheCursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl CacheCursor<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let end = self.pos.checked_add(n)?;
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

    fn opt_u8(&mut self) -> Option<Option<u8>> {
        match *self.take(1)?.first()? {
            0 => Some(None),
            1 => Some(Some(*self.take(1)?.first()?)),
            _ => None,
        }
    }

    fn opt_u64(&mut self) -> Option<Option<u64>> {
        match *self.take(1)?.first()? {
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
        match *self.take(1)?.first()? {
            0 => Some(None),
            1 => Some(Some(self.string()?)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yt_core::event::{EventIdentity, EventType};

    fn record(trace_id: u64, span_id: u64, project_id: &str, skill: &str) -> WalRecord {
        WalRecord {
            trace_id,
            span_id,
            ts: span_id as i64,
            identity: EventIdentity {
                ext_span_id: format!("{trace_id}-{span_id}"),
                seq: span_id,
                event_type: EventType::Attr,
            },
            fields: SpanFields {
                attrs: BTreeMap::from([
                    ("project_id".to_string(), format!("\"{project_id}\"")),
                    ("skill".to_string(), format!("\"{skill}\"")),
                ]),
                ..Default::default()
            },
        }
    }

    fn attr_filter(pairs: &[(&str, &str)]) -> SearchFilter {
        SearchFilter {
            attrs: pairs
                .iter()
                .map(|(key, value)| ((*key).to_string(), format!("\"{value}\"")))
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn wide_posting_is_disabled_but_results_stay_correct() {
        let mut index = FilterAttrsIndex::with_posting_budget(64, 2);
        index.apply_record(&record(1, 1, "wide", "a"));
        index.apply_record(&record(2, 1, "wide", "b"));
        index.apply_record(&record(3, 1, "wide", "c"));

        assert_eq!(index.len(), 3);
        assert_eq!(index.disabled_posting_count(), 1);

        let keys = index.candidate_span_keys(&attr_filter(&[("project_id", "wide")]));
        assert_eq!(keys, HashSet::from([(1, 1), (2, 1), (3, 1)]));

        let keys =
            index.candidate_span_keys(&attr_filter(&[("project_id", "wide"), ("skill", "b")]));
        assert_eq!(keys, HashSet::from([(2, 1)]));
    }

    #[test]
    fn total_posting_budget_falls_back_to_row_sidecar() {
        let mut index = FilterAttrsIndex::with_posting_budget(1, 64);
        index.apply_record(&record(10, 7, "budgeted", "review"));

        assert_eq!(index.len(), 1);
        assert_eq!(index.posting_count(), 1);
        assert!(index.disabled_posting_count() >= 1);

        let keys = index.candidate_span_keys(&attr_filter(&[("project_id", "budgeted")]));
        assert_eq!(keys, HashSet::from([(10, 7)]));
    }

    #[test]
    fn external_trace_id_posting_supports_fast_positive_and_negative_lookup() {
        let mut index = FilterAttrsIndex::with_posting_budget(64, 64);
        let mut first = record(11, 1, "p", "a");
        first.fields.external_trace_id = Some("run-a".to_string());
        let mut second = record(12, 1, "p", "b");
        second.fields.external_trace_id = Some("run-b".to_string());
        index.apply_record(&first);
        index.apply_record(&second);

        let hit = index.candidate_span_keys(&SearchFilter {
            external_trace_id: Some("run-a".to_string()),
            ..Default::default()
        });
        assert_eq!(hit, HashSet::from([(11, 1)]));

        let miss = index.candidate_span_keys(&SearchFilter {
            external_trace_id: Some("run-missing".to_string()),
            ..Default::default()
        });
        assert!(miss.is_empty(), "不存在的外部 trace id 应直接空候选");
    }

    #[test]
    fn disabled_trace_id_posting_still_filters_by_trace_id() {
        let mut index = FilterAttrsIndex::with_posting_budget(0, 64);
        index.apply_record(&record(11, 1, "p", "a"));
        index.apply_record(&record(12, 1, "p", "b"));

        let keys = index.candidate_span_keys(&SearchFilter {
            trace_id: Some(11),
            ..Default::default()
        });
        assert_eq!(keys, HashSet::from([(11, 1)]));
    }
}
