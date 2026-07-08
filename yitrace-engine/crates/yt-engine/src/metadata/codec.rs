pub(crate) fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

fn put_u8(b: &mut Vec<u8>, v: u8) {
    b.push(v);
}

fn put_u32(b: &mut Vec<u8>, v: u32) {
    b.extend_from_slice(&v.to_le_bytes());
}

fn put_i64(b: &mut Vec<u8>, v: i64) {
    b.extend_from_slice(&v.to_le_bytes());
}

fn put_u64(b: &mut Vec<u8>, v: u64) {
    b.extend_from_slice(&v.to_le_bytes());
}

fn put_bool(b: &mut Vec<u8>, v: bool) {
    put_u8(b, if v { 1 } else { 0 });
}

fn put_opt_i64(b: &mut Vec<u8>, v: Option<i64>) {
    match v {
        Some(x) => {
            put_u8(b, 1);
            put_i64(b, x);
        }
        None => put_u8(b, 0),
    }
}

fn put_opt_u64(b: &mut Vec<u8>, v: Option<u64>) {
    match v {
        Some(x) => {
            put_u8(b, 1);
            put_u64(b, x);
        }
        None => put_u8(b, 0),
    }
}

fn put_opt_u32(b: &mut Vec<u8>, v: Option<u32>) {
    match v {
        Some(x) => {
            put_u8(b, 1);
            put_u32(b, x);
        }
        None => put_u8(b, 0),
    }
}

fn put_str(b: &mut Vec<u8>, s: &str) {
    put_u64(b, s.len() as u64);
    b.extend_from_slice(s.as_bytes());
}

fn put_opt_str(b: &mut Vec<u8>, s: Option<&str>) {
    match s {
        Some(v) => {
            put_u8(b, 1);
            put_str(b, v);
        }
        None => put_u8(b, 0),
    }
}

fn put_map(b: &mut Vec<u8>, m: &BTreeMap<String, String>) {
    put_u64(b, m.len() as u64);
    for (k, v) in m {
        put_str(b, k);
        put_str(b, v);
    }
}

fn put_u64_vec(b: &mut Vec<u8>, items: &[u64]) {
    put_u64(b, items.len() as u64);
    for item in items {
        put_u64(b, *item);
    }
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Cursor<'_> {
    fn u8(&mut self) -> Option<u8> {
        let v = *self.bytes.get(self.pos)?;
        self.pos += 1;
        Some(v)
    }

    fn u32(&mut self) -> Option<u32> {
        let end = self.pos.checked_add(4)?;
        let bytes = self.bytes.get(self.pos..end)?;
        self.pos = end;
        Some(u32::from_le_bytes(bytes.try_into().ok()?))
    }

    fn u64(&mut self) -> Option<u64> {
        let end = self.pos.checked_add(8)?;
        let bytes = self.bytes.get(self.pos..end)?;
        self.pos = end;
        Some(u64::from_le_bytes(bytes.try_into().ok()?))
    }

    fn i64(&mut self) -> Option<i64> {
        let end = self.pos.checked_add(8)?;
        let bytes = self.bytes.get(self.pos..end)?;
        self.pos = end;
        Some(i64::from_le_bytes(bytes.try_into().ok()?))
    }

    fn bool(&mut self) -> Option<bool> {
        Some(self.u8()? != 0)
    }

    fn opt_i64(&mut self) -> Option<Option<i64>> {
        match self.u8()? {
            0 => Some(None),
            _ => Some(Some(self.i64()?)),
        }
    }

    fn opt_u64(&mut self) -> Option<Option<u64>> {
        match self.u8()? {
            0 => Some(None),
            _ => Some(Some(self.u64()?)),
        }
    }

    fn opt_u32(&mut self) -> Option<Option<u32>> {
        match self.u8()? {
            0 => Some(None),
            _ => Some(Some(self.u32()?)),
        }
    }

    fn str(&mut self) -> Option<String> {
        let n = self.u64()? as usize;
        let end = self.pos.checked_add(n)?;
        let bytes = self.bytes.get(self.pos..end)?;
        self.pos = end;
        String::from_utf8(bytes.to_vec()).ok()
    }

    fn opt_str(&mut self) -> Option<Option<String>> {
        match self.u8()? {
            0 => Some(None),
            _ => Some(Some(self.str()?)),
        }
    }

    fn map(&mut self) -> Option<BTreeMap<String, String>> {
        let n = self.u64()? as usize;
        let mut out = BTreeMap::new();
        for _ in 0..n {
            out.insert(self.str()?, self.str()?);
        }
        Some(out)
    }

    fn u64_vec(&mut self) -> Option<Vec<u64>> {
        let n = self.u64()? as usize;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            out.push(self.u64()?);
        }
        Some(out)
    }
}

pub(crate) fn encode(state: &MetadataState) -> Vec<u8> {
    let mut b = Vec::new();
    put_u32(&mut b, MAGIC);
    put_u32(&mut b, FORMAT_VER);
    put_u64(&mut b, state.next_annotation_id);
    put_u64(&mut b, state.next_dataset_association_id);
    put_u64(&mut b, state.next_retention_audit_id);
    put_u64(&mut b, state.next_retention_policy_id);

    put_u64(&mut b, state.annotations.len() as u64);
    for a in &state.annotations {
        put_u64(&mut b, a.annotation_id);
        put_opt_u64(&mut b, a.tenant_id);
        put_u8(
            &mut b,
            match a.target {
                AnnotationTarget::Trace => 0,
                AnnotationTarget::Span => 1,
            },
        );
        put_u64(&mut b, a.trace_id);
        put_opt_u64(&mut b, a.span_id);
        put_opt_str(&mut b, a.external_trace_id.as_deref());
        put_opt_str(&mut b, a.external_span_id.as_deref());
        put_str(&mut b, &a.label);
        put_opt_u32(&mut b, a.score);
        put_opt_str(&mut b, a.reason.as_deref());
        put_opt_str(&mut b, a.source.as_deref());
        put_u64(&mut b, a.created_at_ns);
        put_u64(&mut b, a.updated_at_ns);
        put_u8(&mut b, a.status.code());
        put_opt_str(&mut b, a.reviewer.as_deref());
        put_map(&mut b, &a.attrs);
    }

    put_u64(&mut b, state.dataset_associations.len() as u64);
    for d in &state.dataset_associations {
        put_u64(&mut b, d.association_id);
        put_opt_u64(&mut b, d.tenant_id);
        put_str(&mut b, &d.dataset_id);
        put_str(&mut b, &d.item_id);
        put_u64(&mut b, d.trace_id);
        put_opt_u64(&mut b, d.span_id);
        put_opt_str(&mut b, d.external_trace_id.as_deref());
        put_opt_str(&mut b, d.external_span_id.as_deref());
        put_opt_str(&mut b, d.snapshot_id.as_deref());
        put_opt_str(&mut b, d.snapshot_hash.as_deref());
        put_opt_str(&mut b, d.eval_run_id.as_deref());
        put_opt_str(&mut b, d.split.as_deref());
        put_opt_str(&mut b, d.label.as_deref());
        put_opt_u32(&mut b, d.score);
        put_u64(&mut b, d.created_at_ns);
        put_map(&mut b, &d.attrs);
    }
    put_u64(&mut b, state.retention_audits.len() as u64);
    for a in &state.retention_audits {
        put_u64(&mut b, a.audit_id);
        put_opt_u64(&mut b, a.tenant_id);
        put_u64(&mut b, a.created_at_ns);
        put_opt_str(&mut b, a.source.as_deref());
        put_opt_str(&mut b, a.reason.as_deref());
        put_opt_i64(&mut b, a.delete_before_ts);
        put_str(&mut b, &a.query_json);
        put_bool(&mut b, a.protect_annotations);
        put_bool(&mut b, a.protect_dataset_associations);
        put_bool(&mut b, a.protect_snapshots);
        put_bool(&mut b, a.protect_eval_links);
        put_bool(&mut b, a.protect_path_memory);
        put_bool(&mut b, a.compact_requested);
        put_bool(&mut b, a.compact_reclaim);
        put_u64(&mut b, a.candidate_trace_count);
        put_u64(&mut b, a.protected_trace_count);
        put_u64(&mut b, a.deletable_trace_count);
        put_u64(&mut b, a.requested_trace_count);
        put_u64(&mut b, a.deleted_trace_count);
        put_u64(&mut b, a.deleted_segment_row_count);
        put_u64(&mut b, a.skipped_live_trace_count);
        put_u64(&mut b, a.compacted_segment_count);
        put_u64(&mut b, a.reclaimed_segment_count);
        put_u64(&mut b, a.dropped_deleted_row_count);
        put_u64(&mut b, a.rewritten_live_row_count);
        put_u64_vec(&mut b, &a.deletable_trace_ids);
        put_u64_vec(&mut b, &a.deleted_trace_ids);
        put_u64_vec(&mut b, &a.skipped_live_trace_ids);
        put_bool(&mut b, a.trace_id_sample_truncated);
    }
    put_u64(&mut b, state.retention_policies.len() as u64);
    for p in &state.retention_policies {
        put_u64(&mut b, p.policy_id);
        put_opt_u64(&mut b, p.tenant_id);
        put_str(&mut b, &p.name);
        put_bool(&mut b, p.enabled);
        put_u64(&mut b, p.created_at_ns);
        put_u64(&mut b, p.updated_at_ns);
        put_opt_u64(&mut b, p.last_run_at_ns);
        put_opt_u64(&mut b, p.next_run_at_ns);
        put_u64(&mut b, p.interval_ns);
        put_opt_str(&mut b, p.source.as_deref());
        put_opt_str(&mut b, p.reason.as_deref());
        put_str(&mut b, &p.query_json);
    }
    b
}

pub(crate) fn decode(bytes: &[u8]) -> Option<MetadataState> {
    let mut c = Cursor { bytes, pos: 0 };
    if c.u32()? != MAGIC {
        return None;
    }
    let ver = c.u32()?;
    if ver == 0 || ver > FORMAT_VER {
        return None;
    }
    let mut state = MetadataState {
        next_annotation_id: c.u64()?,
        next_dataset_association_id: c.u64()?,
        ..Default::default()
    };
    if ver >= 2 {
        state.next_retention_audit_id = c.u64()?;
        state.next_retention_policy_id = c.u64()?;
    }

    let ann_n = c.u64()? as usize;
    for _ in 0..ann_n {
        let annotation_id = c.u64()?;
        let tenant_id = c.opt_u64()?;
        let target = match c.u8()? {
            0 => AnnotationTarget::Trace,
            1 => AnnotationTarget::Span,
            _ => return None,
        };
        state.annotations.push(TraceAnnotation {
            annotation_id,
            tenant_id,
            target,
            trace_id: c.u64()?,
            span_id: c.opt_u64()?,
            external_trace_id: c.opt_str()?,
            external_span_id: c.opt_str()?,
            label: c.str()?,
            score: c.opt_u32()?,
            reason: c.opt_str()?,
            source: c.opt_str()?,
            created_at_ns: c.u64()?,
            updated_at_ns: c.u64()?,
            status: AnnotationStatus::from_code(c.u8()?)?,
            reviewer: c.opt_str()?,
            attrs: c.map()?,
        });
    }

    let assoc_n = c.u64()? as usize;
    for _ in 0..assoc_n {
        state.dataset_associations.push(DatasetAssociation {
            association_id: c.u64()?,
            tenant_id: c.opt_u64()?,
            dataset_id: c.str()?,
            item_id: c.str()?,
            trace_id: c.u64()?,
            span_id: c.opt_u64()?,
            external_trace_id: c.opt_str()?,
            external_span_id: c.opt_str()?,
            snapshot_id: c.opt_str()?,
            snapshot_hash: c.opt_str()?,
            eval_run_id: c.opt_str()?,
            split: c.opt_str()?,
            label: c.opt_str()?,
            score: c.opt_u32()?,
            created_at_ns: c.u64()?,
            attrs: c.map()?,
        });
    }
    if ver >= 2 {
        let audit_n = c.u64()? as usize;
        for _ in 0..audit_n {
            state.retention_audits.push(RetentionAuditRecord {
                audit_id: c.u64()?,
                tenant_id: c.opt_u64()?,
                created_at_ns: c.u64()?,
                source: c.opt_str()?,
                reason: c.opt_str()?,
                delete_before_ts: c.opt_i64()?,
                query_json: c.str()?,
                protect_annotations: c.bool()?,
                protect_dataset_associations: c.bool()?,
                protect_snapshots: c.bool()?,
                protect_eval_links: c.bool()?,
                protect_path_memory: c.bool()?,
                compact_requested: c.bool()?,
                compact_reclaim: c.bool()?,
                candidate_trace_count: c.u64()?,
                protected_trace_count: c.u64()?,
                deletable_trace_count: c.u64()?,
                requested_trace_count: c.u64()?,
                deleted_trace_count: c.u64()?,
                deleted_segment_row_count: c.u64()?,
                skipped_live_trace_count: c.u64()?,
                compacted_segment_count: c.u64()?,
                reclaimed_segment_count: c.u64()?,
                dropped_deleted_row_count: c.u64()?,
                rewritten_live_row_count: c.u64()?,
                deletable_trace_ids: c.u64_vec()?,
                deleted_trace_ids: c.u64_vec()?,
                skipped_live_trace_ids: c.u64_vec()?,
                trace_id_sample_truncated: c.bool()?,
            });
        }
        let policy_n = c.u64()? as usize;
        for _ in 0..policy_n {
            state.retention_policies.push(RetentionPolicy {
                policy_id: c.u64()?,
                tenant_id: c.opt_u64()?,
                name: c.str()?,
                enabled: c.bool()?,
                created_at_ns: c.u64()?,
                updated_at_ns: c.u64()?,
                last_run_at_ns: c.opt_u64()?,
                next_run_at_ns: c.opt_u64()?,
                interval_ns: c.u64()?,
                source: c.opt_str()?,
                reason: c.opt_str()?,
                query_json: c.str()?,
            });
        }
    }

    state.next_annotation_id = state.next_annotation_id.max(
        state
            .annotations
            .iter()
            .map(|a| a.annotation_id)
            .max()
            .unwrap_or(0)
            + 1,
    );
    state.next_dataset_association_id = state.next_dataset_association_id.max(
        state
            .dataset_associations
            .iter()
            .map(|a| a.association_id)
            .max()
            .unwrap_or(0)
            + 1,
    );
    state.next_retention_audit_id = state.next_retention_audit_id.max(
        state
            .retention_audits
            .iter()
            .map(|a| a.audit_id)
            .max()
            .unwrap_or(0)
            + 1,
    );
    state.next_retention_policy_id = state.next_retention_policy_id.max(
        state
            .retention_policies
            .iter()
            .map(|p| p.policy_id)
            .max()
            .unwrap_or(0)
            + 1,
    );
    Some(state)
}

pub(crate) fn save(path: impl AsRef<Path>, state: &MetadataState) -> std::io::Result<()> {
    let payload = encode(state);
    let mut bytes = Vec::with_capacity(payload.len() + 4);
    bytes.extend_from_slice(&yt_wal::crc32(&payload).to_le_bytes());
    bytes.extend_from_slice(&payload);

    let path = path.as_ref();
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

pub(crate) fn load(path: impl AsRef<Path>) -> Option<MetadataState> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() < 4 {
        return None;
    }
    let crc = u32::from_le_bytes(bytes[0..4].try_into().ok()?);
    let payload = &bytes[4..];
    if crc != yt_wal::crc32(payload) {
        return None;
    }
    decode(payload)
}
