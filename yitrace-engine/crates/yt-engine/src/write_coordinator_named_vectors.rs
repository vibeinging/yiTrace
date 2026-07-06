#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum VectorNamespace {
    Span,
    Task,
    Trajectory,
}

impl VectorNamespace {
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().replace(['-', '_'], "").as_str() {
            "span" => Some(Self::Span),
            "task" => Some(Self::Task),
            "trajectory" | "tracepath" | "path" => Some(Self::Trajectory),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Span => "span",
            Self::Task => "task",
            Self::Trajectory => "trajectory",
        }
    }

    fn code(self) -> u8 {
        match self {
            Self::Span => 0,
            Self::Task => 1,
            Self::Trajectory => 2,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Span),
            1 => Some(Self::Task),
            2 => Some(Self::Trajectory),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct VectorEmbeddingInput {
    pub namespace: VectorNamespace,
    pub key: String,
    pub tenant_id: Option<u64>,
    pub trace_id: Option<u64>,
    pub span_id: Option<u64>,
    pub attrs: BTreeMap<String, String>,
    pub embedding: Vec<f32>,
}

#[derive(Clone, Debug, Default)]
pub struct VectorSearchFilter {
    pub namespace: Option<VectorNamespace>,
    pub tenant_id: Option<u64>,
    pub key: Option<String>,
    pub trace_id: Option<u64>,
    pub span_id: Option<u64>,
    pub attrs: BTreeMap<String, String>,
}

#[derive(Clone, Debug)]
pub struct VectorSearchHit {
    pub namespace: VectorNamespace,
    pub key: String,
    pub tenant_id: Option<u64>,
    pub trace_id: Option<u64>,
    pub span_id: Option<u64>,
    pub attrs: BTreeMap<String, String>,
    pub distance: f32,
    pub score: f32,
}

#[derive(Default)]
struct NamedVectorIndex {
    records: BTreeMap<(VectorNamespace, Option<u64>, String), NamedVectorRecord>,
}

#[derive(Clone)]
struct NamedVectorRecord {
    namespace: VectorNamespace,
    key: String,
    tenant_id: Option<u64>,
    trace_id: Option<u64>,
    span_id: Option<u64>,
    attrs: BTreeMap<String, String>,
    embedding: Vec<f32>,
}

impl NamedVectorIndex {
    fn clear(&mut self) {
        self.records.clear();
    }

    fn upsert(&mut self, input: VectorEmbeddingInput) {
        let key = (input.namespace, input.tenant_id, input.key.clone());
        self.records.insert(
            key,
            NamedVectorRecord {
                namespace: input.namespace,
                key: input.key,
                tenant_id: input.tenant_id,
                trace_id: input.trace_id,
                span_id: input.span_id,
                attrs: input.attrs,
                embedding: input.embedding,
            },
        );
    }

    fn search(
        &self,
        query: &[f32],
        k: usize,
        filter: &VectorSearchFilter,
        mut is_live: impl FnMut(&NamedVectorRecord) -> bool,
    ) -> Vec<VectorSearchHit> {
        if query.is_empty() || k == 0 {
            return Vec::new();
        }
        let mut out = Vec::new();
        for record in self.records.values() {
            if !named_vector_matches(record, filter)
                || record.embedding.len() != query.len()
                || !is_live(record)
            {
                continue;
            }
            let distance = named_vector_l2_distance(query, &record.embedding);
            out.push(VectorSearchHit {
                namespace: record.namespace,
                key: record.key.clone(),
                tenant_id: record.tenant_id,
                trace_id: record.trace_id,
                span_id: record.span_id,
                attrs: record.attrs.clone(),
                distance,
                score: 1.0 / (1.0 + distance),
            });
        }
        out.sort_by(|a, b| {
            a.distance
                .total_cmp(&b.distance)
                .then_with(|| a.namespace.cmp(&b.namespace))
                .then_with(|| a.key.cmp(&b.key))
        });
        out.truncate(k);
        out
    }
}

impl WriteCoordinator {
    pub fn index_named_embedding(&self, input: VectorEmbeddingInput) -> Result<(), String> {
        if input.key.is_empty() {
            return Err("vector key is required".to_string());
        }
        if input.embedding.is_empty() {
            return Err("vector embedding is required".to_string());
        }
        if let Some(path) = &self.named_vector_path {
            append_named_vector(path, &input).map_err(|e| e.to_string())?;
        }
        self.named_vectors.lock().unwrap().upsert(input);
        Ok(())
    }

    pub fn search_named_embeddings(
        &self,
        query: &[f32],
        k: usize,
        filter: &VectorSearchFilter,
    ) -> Vec<VectorSearchHit> {
        let snap = self.pin_snapshot();
        let mut live_cache = BTreeMap::<(Option<u64>, u64), bool>::new();
        self.named_vectors
            .lock()
            .unwrap()
            .search(query, k, filter, |record| {
                self.named_vector_record_is_live(&snap, record, &mut live_cache)
            })
    }

    fn load_named_vectors_from_disk(&self) {
        self.named_vectors.lock().unwrap().clear();
        let Some(path) = &self.named_vector_path else {
            return;
        };
        for input in load_named_vectors(path) {
            self.named_vectors.lock().unwrap().upsert(input);
        }
    }

    fn named_vector_record_is_live(
        &self,
        snap: &Snapshot,
        record: &NamedVectorRecord,
        cache: &mut BTreeMap<(Option<u64>, u64), bool>,
    ) -> bool {
        // trajectory 向量是路径资产，source trace 被 retention 清理后仍可用于召回。
        if record.namespace == VectorNamespace::Trajectory {
            return true;
        }
        let Some(trace_id) = record.trace_id else {
            return true;
        };
        let key = (record.tenant_id, trace_id);
        if let Some(live) = cache.get(&key) {
            return *live;
        }
        let mut query = TraceQuery::trace(trace_id, i64::MIN, i64::MAX);
        if let Some(tenant_id) = record.tenant_id {
            query.tenant_id = Some(tenant_id);
        }
        let live = !self.read_spans_query(snap, &query).0.is_empty();
        cache.insert(key, live);
        live
    }
}

fn named_vector_matches(record: &NamedVectorRecord, filter: &VectorSearchFilter) -> bool {
    if let Some(namespace) = filter.namespace {
        if record.namespace != namespace {
            return false;
        }
    }
    if let Some(tenant_id) = filter.tenant_id {
        if record.tenant_id != Some(tenant_id) {
            return false;
        }
    }
    if let Some(key) = &filter.key {
        if record.key != *key {
            return false;
        }
    }
    if let Some(trace_id) = filter.trace_id {
        if record.trace_id != Some(trace_id) {
            return false;
        }
    }
    if let Some(span_id) = filter.span_id {
        if record.span_id != Some(span_id) {
            return false;
        }
    }
    filter.attrs.iter().all(|(key, expected)| {
        record
            .attrs
            .get(key)
            .map(|actual| attr_json_matches(actual, expected))
            .unwrap_or(false)
    })
}

fn named_vector_l2_distance(left: &[f32], right: &[f32]) -> f32 {
    left.iter()
        .zip(right.iter())
        .map(|(a, b)| {
            let d = a - b;
            d * d
        })
        .sum::<f32>()
        .sqrt()
}

fn append_named_vector(
    path: impl AsRef<std::path::Path>,
    input: &VectorEmbeddingInput,
) -> std::io::Result<()> {
    let mut payload = Vec::new();
    payload.push(input.namespace.code());
    write_opt_u64(&mut payload, input.tenant_id);
    write_opt_u64(&mut payload, input.trace_id);
    write_opt_u64(&mut payload, input.span_id);
    write_len_string(&mut payload, &input.key);
    payload.extend_from_slice(&(input.attrs.len() as u32).to_le_bytes());
    for (key, value) in &input.attrs {
        write_len_string(&mut payload, key);
        write_len_string(&mut payload, value);
    }
    payload.extend_from_slice(&(input.embedding.len() as u32).to_le_bytes());
    for value in &input.embedding {
        payload.extend_from_slice(&value.to_le_bytes());
    }

    let mut frame = Vec::with_capacity(payload.len() + 8);
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(&payload);
    let crc = yt_wal::crc32(&frame);
    frame.extend_from_slice(&crc.to_le_bytes());

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    use std::io::Write;
    file.write_all(&frame)?;
    file.sync_data()
}

fn load_named_vectors(path: impl AsRef<std::path::Path>) -> Vec<VectorEmbeddingInput> {
    let bytes = std::fs::read(path).unwrap_or_default();
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos + 8 <= bytes.len() {
        let frame_start = pos;
        let payload_len = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
        let frame_len = 4usize.saturating_add(payload_len).saturating_add(4);
        if frame_start + frame_len > bytes.len() {
            break;
        }
        let crc_at = frame_start + 4 + payload_len;
        let crc = u32::from_le_bytes(bytes[crc_at..crc_at + 4].try_into().unwrap());
        if crc != yt_wal::crc32(&bytes[frame_start..crc_at]) {
            break;
        }
        let mut inner = frame_start + 4;
        let end = crc_at;
        if let Some(input) = read_named_vector_payload(&bytes, &mut inner, end) {
            out.push(input);
        } else {
            break;
        }
        pos = crc_at + 4;
    }
    out
}

fn read_named_vector_payload(
    bytes: &[u8],
    pos: &mut usize,
    end: usize,
) -> Option<VectorEmbeddingInput> {
    if *pos >= end {
        return None;
    }
    let namespace = VectorNamespace::from_code(bytes[*pos])?;
    *pos += 1;
    let tenant_id = read_opt_u64(bytes, pos, end)?;
    let trace_id = read_opt_u64(bytes, pos, end)?;
    let span_id = read_opt_u64(bytes, pos, end)?;
    let key = read_len_string_at(bytes, pos, end)?;
    let attr_len = read_u32_at(bytes, pos, end)? as usize;
    let mut attrs = BTreeMap::new();
    for _ in 0..attr_len {
        let key = read_len_string_at(bytes, pos, end)?;
        let value = read_len_string_at(bytes, pos, end)?;
        attrs.insert(key, value);
    }
    let dim = read_u32_at(bytes, pos, end)? as usize;
    if (*pos).saturating_add(dim.saturating_mul(4)) > end {
        return None;
    }
    let mut embedding = Vec::with_capacity(dim);
    for _ in 0..dim {
        embedding.push(f32::from_le_bytes(bytes[*pos..*pos + 4].try_into().ok()?));
        *pos += 4;
    }
    Some(VectorEmbeddingInput {
        namespace,
        key,
        tenant_id,
        trace_id,
        span_id,
        attrs,
        embedding,
    })
}

fn write_opt_u64(out: &mut Vec<u8>, value: Option<u64>) {
    match value {
        Some(value) => {
            out.push(1);
            out.extend_from_slice(&value.to_le_bytes());
        }
        None => out.push(0),
    }
}

fn read_opt_u64(bytes: &[u8], pos: &mut usize, end: usize) -> Option<Option<u64>> {
    if *pos >= end {
        return None;
    }
    let flag = bytes[*pos];
    *pos += 1;
    if flag == 0 {
        return Some(None);
    }
    if (*pos).saturating_add(8) > end {
        return None;
    }
    let value = u64::from_le_bytes(bytes[*pos..*pos + 8].try_into().ok()?);
    *pos += 8;
    Some(Some(value))
}

fn write_len_string(out: &mut Vec<u8>, value: &str) {
    out.extend_from_slice(&(value.len() as u32).to_le_bytes());
    out.extend_from_slice(value.as_bytes());
}

fn read_len_string_at(bytes: &[u8], pos: &mut usize, end: usize) -> Option<String> {
    let len = read_u32_at(bytes, pos, end)? as usize;
    if (*pos).saturating_add(len) > end {
        return None;
    }
    let out = String::from_utf8(bytes[*pos..*pos + len].to_vec()).ok()?;
    *pos += len;
    Some(out)
}

fn read_u32_at(bytes: &[u8], pos: &mut usize, end: usize) -> Option<u32> {
    if (*pos).saturating_add(4) > end {
        return None;
    }
    let value = u32::from_le_bytes(bytes[*pos..*pos + 4].try_into().ok()?);
    *pos += 4;
    Some(value)
}
