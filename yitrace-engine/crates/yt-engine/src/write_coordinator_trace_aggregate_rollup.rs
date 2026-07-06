#[derive(Clone, Debug, Default)]
pub(crate) struct TraceAggregateRollupFilters {
    pub status: Option<u8>,
    pub kind: Option<String>,
    pub agent_name: Option<String>,
    pub tool_name: Option<String>,
    pub model: Option<String>,
    pub attrs: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct TraceAggregateRollupStats {
    pub used_segment_rollup: bool,
    pub segment_rollup_segments: usize,
    pub segment_rollup_rows: usize,
    pub tail_folded_span_count: usize,
}

impl TraceAggregateRollupStats {
    pub(crate) fn add_shard(&mut self, other: &TraceAggregateRollupStats) {
        self.used_segment_rollup |= other.used_segment_rollup;
        self.segment_rollup_segments += other.segment_rollup_segments;
        self.segment_rollup_rows += other.segment_rollup_rows;
        self.tail_folded_span_count += other.tail_folded_span_count;
    }
}

#[derive(Clone, Debug)]
pub(crate) struct TraceAggregateRollupRead {
    pub rows: Vec<TraceAggregateRollupRow>,
    pub stats: TraceAggregateRollupStats,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct TraceAggregateRollupRow {
    pub trace_id: u64,
    pub span_id: u64,
    pub session_id: Option<u64>,
    pub tenant_id: Option<u64>,
    pub external_trace_id: Option<String>,
    pub external_span_id: Option<String>,
    pub status: Option<u8>,
    pub duration_ns: Option<u64>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    pub reasoning_tokens: u64,
    pub total_tokens: u64,
    pub cost_usd_nanos: u64,
    pub provider: Option<String>,
    pub agent_name: Option<String>,
    pub tool_name: Option<String>,
    pub model: Option<String>,
    pub attrs: BTreeMap<String, String>,
}

impl TraceAggregateRollupRow {
    fn from_folded_span(s: FoldedSpan) -> Self {
        let mut attrs = s.attrs.clone();
        for key in first_class_agentic_attr_keys() {
            if let Some(value) = first_class_span_attr_value(&s, key) {
                attrs.insert(
                    (*key).to_string(),
                    first_class_agentic_attr_json(key, value),
                );
            }
        }
        let input_tokens = s.input_tokens.unwrap_or(0);
        let output_tokens = s.output_tokens.unwrap_or(0);
        let cached_input_tokens = s.cached_input_tokens.unwrap_or(0);
        let reasoning_tokens = s.reasoning_tokens.unwrap_or(0);
        let total_tokens = usage_total_tokens(
            input_tokens,
            output_tokens,
            cached_input_tokens,
            reasoning_tokens,
            s.total_tokens,
        );
        let cost_usd_nanos = usage_cost_usd_nanos_for_model(
            input_tokens,
            output_tokens,
            cached_input_tokens,
            reasoning_tokens,
            s.cost_usd_nanos,
            s.provider.as_deref(),
            s.model.as_deref(),
        );
        Self {
            trace_id: s.trace_id,
            span_id: s.span_id,
            session_id: s.session_id,
            tenant_id: s.tenant_id,
            external_trace_id: s.external_trace_id,
            external_span_id: s.external_span_id,
            status: s.status,
            duration_ns: s.duration_ns,
            input_tokens,
            output_tokens,
            cached_input_tokens,
            reasoning_tokens,
            total_tokens,
            cost_usd_nanos,
            provider: s.provider,
            agent_name: s.agent_name,
            tool_name: s.tool_name,
            model: s.model,
            attrs,
        }
    }

    pub(crate) fn attr_value(&self, key: &str) -> Option<&str> {
        self.attrs.get(key).map(String::as_str)
    }

    pub(crate) fn kind(&self) -> &'static str {
        if self.agent_name.is_some() {
            "agent"
        } else if self.tool_name.is_some() {
            "tool"
        } else if self.model.is_some() {
            "llm"
        } else {
            "other"
        }
    }

    pub(crate) fn name(&self) -> String {
        self.agent_name
            .as_ref()
            .or(self.tool_name.as_ref())
            .or(self.model.as_ref())
            .cloned()
            .unwrap_or_else(|| format!("span {}", self.span_id))
    }
}

#[derive(Clone, Debug, Default)]
struct TraceAggregateSegmentRollup {
    rows: Vec<TraceAggregateRollupRow>,
}

impl TraceAggregateSegmentRollup {
    fn build(records: &[WalRecord]) -> Self {
        let inputs: Vec<FoldInput> = records.iter().map(WalRecord::to_fold_input).collect();
        let rows = fold_events(inputs)
            .into_iter()
            .map(TraceAggregateRollupRow::from_folded_span)
            .collect();
        Self { rows }
    }
}

impl WriteCoordinator {
    fn install_trace_aggregate_segment_rollup(
        &self,
        seg: SegmentId,
        records: &[WalRecord],
        cache: bool,
    ) {
        let rollup = Arc::new(TraceAggregateSegmentRollup::build(records));
        if let Some(dir) = &self.trace_aggregate_rollup_dir {
            let _ = write_trace_aggregate_rollup_file(dir, seg, &rollup);
        }
        if cache {
            self.trace_aggregate_rollups
                .lock()
                .unwrap()
                .insert(seg.get(), rollup);
        }
    }

    fn trace_aggregate_segment_rollup(&self, seg: SegmentId) -> Arc<TraceAggregateSegmentRollup> {
        if let Some(rollup) = self
            .trace_aggregate_rollups
            .lock()
            .unwrap()
            .get(&seg.get())
            .cloned()
        {
            return rollup;
        }
        let loaded = self
            .trace_aggregate_rollup_dir
            .as_ref()
            .and_then(|dir| read_trace_aggregate_rollup_file(dir, seg))
            .unwrap_or_else(|| {
                let records = self.segments.scan_records(seg);
                let rollup = TraceAggregateSegmentRollup::build(&records);
                if let Some(dir) = &self.trace_aggregate_rollup_dir {
                    let _ = write_trace_aggregate_rollup_file(dir, seg, &rollup);
                }
                rollup
            });
        let rollup = Arc::new(loaded);
        self.trace_aggregate_rollups
            .lock()
            .unwrap()
            .insert(seg.get(), rollup.clone());
        rollup
    }

    pub(crate) fn trace_aggregate_rollup_read(
        &self,
        snap: &Snapshot,
        q: &TraceQuery,
        filters: &TraceAggregateRollupFilters,
    ) -> Result<TraceAggregateRollupRead, &'static str> {
        if q.trace_id.is_some() {
            return Err("trace_id_filter");
        }
        if q.time_from != i64::MIN || q.time_to != i64::MAX {
            return Err("time_window_filter");
        }

        let mut seen = HashSet::new();
        let mut rows = Vec::new();
        let mut stats = TraceAggregateRollupStats::default();
        for entry in snap.manifest.segments.values() {
            if entry.deletion_seq != 0 || entry.deletion_vec.count() != 0 {
                return Err("segment_deletion_vector");
            }
            if entry.upgrade_seq != 0 || entry.upgrade_ref.is_some() {
                return Err("segment_upgrade_patch");
            }
            let rollup = self.trace_aggregate_segment_rollup(entry.segment_id);
            stats.segment_rollup_segments += 1;
            stats.segment_rollup_rows += rollup.rows.len();
            for row in &rollup.rows {
                if !seen.insert((row.trace_id, row.span_id)) {
                    return Err("span_crosses_multiple_segments");
                }
                if trace_aggregate_rollup_row_matches(row, q, filters) {
                    rows.push(row.clone());
                }
            }
        }

        let tail_inputs: Vec<FoldInput> = self
            .memtable
            .lock()
            .unwrap()
            .read_range(snap.retained_watermark, snap.live_lsn)
            .map(MemRow::to_fold_input)
            .collect();
        let tail_spans = fold_events(tail_inputs);
        stats.tail_folded_span_count = tail_spans.len();
        for span in tail_spans {
            let row = TraceAggregateRollupRow::from_folded_span(span);
            if seen.contains(&(row.trace_id, row.span_id)) {
                return Err("tail_overlaps_segment_span");
            }
            if trace_aggregate_rollup_row_matches(&row, q, filters) {
                rows.push(row);
            }
        }
        stats.used_segment_rollup = stats.segment_rollup_segments > 0;
        Ok(TraceAggregateRollupRead { rows, stats })
    }
}

fn trace_aggregate_rollup_row_matches(
    row: &TraceAggregateRollupRow,
    q: &TraceQuery,
    filters: &TraceAggregateRollupFilters,
) -> bool {
    if let Some(tenant_id) = q.tenant_id {
        if row.tenant_id != Some(tenant_id) {
            return false;
        }
    }
    if let Some(status) = filters.status {
        if row.status != Some(status) {
            return false;
        }
    }
    if let Some(kind) = &filters.kind {
        if row.kind() != kind {
            return false;
        }
    }
    if let Some(agent_name) = &filters.agent_name {
        if row.agent_name.as_deref() != Some(agent_name.as_str()) {
            return false;
        }
    }
    if let Some(tool_name) = &filters.tool_name {
        if row.tool_name.as_deref() != Some(tool_name.as_str()) {
            return false;
        }
    }
    if let Some(model) = &filters.model {
        if row.model.as_deref() != Some(model.as_str()) {
            return false;
        }
    }
    filters.attrs.iter().all(|(key, expected)| {
        row.attr_value(key)
            .map(|actual| attr_json_matches(actual, expected))
            .unwrap_or(false)
    })
}

const TRACE_AGGREGATE_ROLLUP_MAGIC: &[u8; 8] = b"YTAR1\0\0\0";

fn trace_aggregate_rollup_path(dir: &std::path::Path, seg: SegmentId) -> std::path::PathBuf {
    dir.join(format!("seg-{}.agg", seg.get()))
}

fn write_trace_aggregate_rollup_file(
    dir: &std::path::Path,
    seg: SegmentId,
    rollup: &TraceAggregateSegmentRollup,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = trace_aggregate_rollup_path(dir, seg);
    let tmp = dir.join(format!("seg-{}.agg.tmp", seg.get()));
    std::fs::write(&tmp, encode_trace_aggregate_rollup(rollup))?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

fn read_trace_aggregate_rollup_file(
    dir: &std::path::Path,
    seg: SegmentId,
) -> Option<TraceAggregateSegmentRollup> {
    let bytes = std::fs::read(trace_aggregate_rollup_path(dir, seg)).ok()?;
    decode_trace_aggregate_rollup(&bytes).ok()
}

fn encode_trace_aggregate_rollup(rollup: &TraceAggregateSegmentRollup) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(TRACE_AGGREGATE_ROLLUP_MAGIC);
    write_u32(&mut out, rollup.rows.len() as u32);
    for row in &rollup.rows {
        write_trace_aggregate_rollup_row(&mut out, row);
    }
    out
}

fn decode_trace_aggregate_rollup(bytes: &[u8]) -> Result<TraceAggregateSegmentRollup, String> {
    let mut pos = 0usize;
    if bytes.len() < TRACE_AGGREGATE_ROLLUP_MAGIC.len()
        || &bytes[..TRACE_AGGREGATE_ROLLUP_MAGIC.len()] != TRACE_AGGREGATE_ROLLUP_MAGIC
    {
        return Err("bad trace aggregate rollup magic".to_string());
    }
    pos += TRACE_AGGREGATE_ROLLUP_MAGIC.len();
    let count = read_u32(bytes, &mut pos)? as usize;
    let mut rows = Vec::with_capacity(count);
    for _ in 0..count {
        rows.push(read_trace_aggregate_rollup_row(bytes, &mut pos)?);
    }
    Ok(TraceAggregateSegmentRollup { rows })
}

fn write_trace_aggregate_rollup_row(out: &mut Vec<u8>, row: &TraceAggregateRollupRow) {
    write_u64(out, row.trace_id);
    write_u64(out, row.span_id);
    write_trace_aggregate_optional_u64(out, row.session_id);
    write_trace_aggregate_optional_u64(out, row.tenant_id);
    write_trace_aggregate_optional_string(out, row.external_trace_id.as_deref());
    write_trace_aggregate_optional_string(out, row.external_span_id.as_deref());
    write_trace_aggregate_optional_u8(out, row.status);
    write_trace_aggregate_optional_u64(out, row.duration_ns);
    write_u64(out, row.input_tokens);
    write_u64(out, row.output_tokens);
    write_u64(out, row.cached_input_tokens);
    write_u64(out, row.reasoning_tokens);
    write_u64(out, row.total_tokens);
    write_u64(out, row.cost_usd_nanos);
    write_trace_aggregate_optional_string(out, row.provider.as_deref());
    write_trace_aggregate_optional_string(out, row.agent_name.as_deref());
    write_trace_aggregate_optional_string(out, row.tool_name.as_deref());
    write_trace_aggregate_optional_string(out, row.model.as_deref());
    write_u32(out, row.attrs.len() as u32);
    for (key, value) in &row.attrs {
        write_string(out, key);
        write_string(out, value);
    }
}

fn read_trace_aggregate_rollup_row(
    bytes: &[u8],
    pos: &mut usize,
) -> Result<TraceAggregateRollupRow, String> {
    let trace_id = read_u64(bytes, pos)?;
    let span_id = read_u64(bytes, pos)?;
    let session_id = read_trace_aggregate_optional_u64(bytes, pos)?;
    let tenant_id = read_trace_aggregate_optional_u64(bytes, pos)?;
    let external_trace_id = read_trace_aggregate_optional_string(bytes, pos)?;
    let external_span_id = read_trace_aggregate_optional_string(bytes, pos)?;
    let status = read_trace_aggregate_optional_u8(bytes, pos)?;
    let duration_ns = read_trace_aggregate_optional_u64(bytes, pos)?;
    let input_tokens = read_u64(bytes, pos)?;
    let output_tokens = read_u64(bytes, pos)?;
    let cached_input_tokens = read_u64(bytes, pos)?;
    let reasoning_tokens = read_u64(bytes, pos)?;
    let total_tokens = read_u64(bytes, pos)?;
    let cost_usd_nanos = read_u64(bytes, pos)?;
    let provider = read_trace_aggregate_optional_string(bytes, pos)?;
    let agent_name = read_trace_aggregate_optional_string(bytes, pos)?;
    let tool_name = read_trace_aggregate_optional_string(bytes, pos)?;
    let model = read_trace_aggregate_optional_string(bytes, pos)?;
    let attr_count = read_u32(bytes, pos)? as usize;
    let mut attrs = BTreeMap::new();
    for _ in 0..attr_count {
        attrs.insert(read_string(bytes, pos)?, read_string(bytes, pos)?);
    }
    Ok(TraceAggregateRollupRow {
        trace_id,
        span_id,
        session_id,
        tenant_id,
        external_trace_id,
        external_span_id,
        status,
        duration_ns,
        input_tokens,
        output_tokens,
        cached_input_tokens,
        reasoning_tokens,
        total_tokens,
        cost_usd_nanos,
        provider,
        agent_name,
        tool_name,
        model,
        attrs,
    })
}

fn write_trace_aggregate_optional_u8(out: &mut Vec<u8>, value: Option<u8>) {
    match value {
        Some(v) => {
            out.push(1);
            out.push(v);
        }
        None => out.push(0),
    }
}

fn read_trace_aggregate_optional_u8(bytes: &[u8], pos: &mut usize) -> Result<Option<u8>, String> {
    let tag = *bytes.get(*pos).ok_or_else(|| "truncated option tag".to_string())?;
    *pos += 1;
    if tag == 0 {
        Ok(None)
    } else {
        let value = *bytes.get(*pos).ok_or_else(|| "truncated u8".to_string())?;
        *pos += 1;
        Ok(Some(value))
    }
}

fn write_trace_aggregate_optional_u64(out: &mut Vec<u8>, value: Option<u64>) {
    match value {
        Some(v) => {
            out.push(1);
            write_u64(out, v);
        }
        None => out.push(0),
    }
}

fn read_trace_aggregate_optional_u64(bytes: &[u8], pos: &mut usize) -> Result<Option<u64>, String> {
    let tag = *bytes.get(*pos).ok_or_else(|| "truncated option tag".to_string())?;
    *pos += 1;
    if tag == 0 {
        Ok(None)
    } else {
        Ok(Some(read_u64(bytes, pos)?))
    }
}

fn write_trace_aggregate_optional_string(out: &mut Vec<u8>, value: Option<&str>) {
    match value {
        Some(v) => {
            out.push(1);
            write_string(out, v);
        }
        None => out.push(0),
    }
}

fn read_trace_aggregate_optional_string(
    bytes: &[u8],
    pos: &mut usize,
) -> Result<Option<String>, String> {
    let tag = *bytes.get(*pos).ok_or_else(|| "truncated option tag".to_string())?;
    *pos += 1;
    if tag == 0 {
        Ok(None)
    } else {
        Ok(Some(read_string(bytes, pos)?))
    }
}
