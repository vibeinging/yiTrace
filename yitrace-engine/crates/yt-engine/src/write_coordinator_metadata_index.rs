#[derive(Default)]
struct MetadataIndex {
    annotation_all: HashSet<u64>,
    annotation_not_deleted: HashSet<u64>,
    annotation_tenant: HashMap<Option<u64>, HashSet<u64>>,
    annotation_target: HashMap<AnnotationTarget, HashSet<u64>>,
    annotation_trace: HashMap<u64, HashSet<u64>>,
    annotation_span: HashMap<u64, HashSet<u64>>,
    annotation_label: HashMap<String, HashSet<u64>>,
    annotation_source: HashMap<String, HashSet<u64>>,
    annotation_status: HashMap<AnnotationStatus, HashSet<u64>>,
    annotation_attrs: HashMap<(String, String), HashSet<u64>>,
    annotation_attr_keys: HashMap<String, HashSet<u64>>,
    dataset_all: HashSet<u64>,
    dataset_tenant: HashMap<Option<u64>, HashSet<u64>>,
    dataset_dataset: HashMap<String, HashSet<u64>>,
    dataset_item: HashMap<String, HashSet<u64>>,
    dataset_trace: HashMap<u64, HashSet<u64>>,
    dataset_span: HashMap<u64, HashSet<u64>>,
    dataset_eval_run: HashMap<String, HashSet<u64>>,
    dataset_split: HashMap<String, HashSet<u64>>,
    dataset_label: HashMap<String, HashSet<u64>>,
    dataset_attrs: HashMap<(String, String), HashSet<u64>>,
    dataset_attr_keys: HashMap<String, HashSet<u64>>,
}

impl MetadataIndex {
    fn build(annotations: &[TraceAnnotation], datasets: &[DatasetAssociation]) -> Self {
        let mut out = Self::default();
        for a in annotations {
            out.add_annotation(a);
        }
        for d in datasets {
            out.add_dataset(d);
        }
        out
    }

    fn add_annotation(&mut self, a: &TraceAnnotation) {
        let id = a.annotation_id;
        self.annotation_all.insert(id);
        if a.status != AnnotationStatus::Deleted {
            self.annotation_not_deleted.insert(id);
        }
        insert_metadata_posting(&mut self.annotation_tenant, a.tenant_id, id);
        insert_metadata_posting(&mut self.annotation_target, a.target, id);
        insert_metadata_posting(&mut self.annotation_trace, a.trace_id, id);
        if let Some(span_id) = a.span_id {
            insert_metadata_posting(&mut self.annotation_span, span_id, id);
        }
        insert_metadata_posting(&mut self.annotation_label, a.label.clone(), id);
        if let Some(source) = &a.source {
            insert_metadata_posting(&mut self.annotation_source, source.clone(), id);
        }
        insert_metadata_posting(&mut self.annotation_status, a.status, id);
        for (key, value) in &a.attrs {
            insert_metadata_posting(&mut self.annotation_attr_keys, key.clone(), id);
            insert_metadata_posting(&mut self.annotation_attrs, (key.clone(), value.clone()), id);
        }
    }

    fn add_dataset(&mut self, d: &DatasetAssociation) {
        let id = d.association_id;
        self.dataset_all.insert(id);
        insert_metadata_posting(&mut self.dataset_tenant, d.tenant_id, id);
        insert_metadata_posting(&mut self.dataset_dataset, d.dataset_id.clone(), id);
        insert_metadata_posting(&mut self.dataset_item, d.item_id.clone(), id);
        insert_metadata_posting(&mut self.dataset_trace, d.trace_id, id);
        if let Some(span_id) = d.span_id {
            insert_metadata_posting(&mut self.dataset_span, span_id, id);
        }
        if let Some(eval_run_id) = &d.eval_run_id {
            insert_metadata_posting(&mut self.dataset_eval_run, eval_run_id.clone(), id);
        }
        if let Some(split) = &d.split {
            insert_metadata_posting(&mut self.dataset_split, split.clone(), id);
        }
        if let Some(label) = &d.label {
            insert_metadata_posting(&mut self.dataset_label, label.clone(), id);
        }
        for (key, value) in &d.attrs {
            insert_metadata_posting(&mut self.dataset_attr_keys, key.clone(), id);
            insert_metadata_posting(&mut self.dataset_attrs, (key.clone(), value.clone()), id);
        }
    }

    fn annotation_candidates(&self, filter: &TraceAnnotationFilter) -> HashSet<u64> {
        let mut out = if let Some(status) = filter.status {
            self.annotation_status
                .get(&status)
                .cloned()
                .unwrap_or_default()
        } else if filter.include_deleted {
            self.annotation_all.clone()
        } else {
            self.annotation_not_deleted.clone()
        };
        if let Some(tenant_id) = filter.tenant_id {
            intersect_metadata_candidates(&mut out, self.annotation_tenant.get(&Some(tenant_id)));
        }
        if let Some(target) = filter.target {
            intersect_metadata_candidates(&mut out, self.annotation_target.get(&target));
        }
        if let Some(trace_id) = filter.trace_id {
            intersect_metadata_candidates(&mut out, self.annotation_trace.get(&trace_id));
        }
        if let Some(span_id) = filter.span_id {
            intersect_metadata_candidates(&mut out, self.annotation_span.get(&span_id));
        }
        if let Some(label) = &filter.label {
            intersect_metadata_candidates(&mut out, self.annotation_label.get(label));
        }
        if let Some(source) = &filter.source {
            intersect_metadata_candidates(&mut out, self.annotation_source.get(source));
        }
        for (key, value) in &filter.attrs {
            if let Some(exact_ids) = self.annotation_attrs.get(&(key.clone(), value.clone())) {
                intersect_metadata_candidates(&mut out, Some(exact_ids));
            } else {
                intersect_metadata_candidates(&mut out, self.annotation_attr_keys.get(key));
            }
        }
        out
    }

    fn dataset_candidates(&self, filter: &DatasetAssociationFilter) -> HashSet<u64> {
        let mut out = self.dataset_all.clone();
        if let Some(tenant_id) = filter.tenant_id {
            intersect_metadata_candidates(&mut out, self.dataset_tenant.get(&Some(tenant_id)));
        }
        if let Some(dataset_id) = &filter.dataset_id {
            intersect_metadata_candidates(&mut out, self.dataset_dataset.get(dataset_id));
        }
        if let Some(item_id) = &filter.item_id {
            intersect_metadata_candidates(&mut out, self.dataset_item.get(item_id));
        }
        if let Some(trace_id) = filter.trace_id {
            intersect_metadata_candidates(&mut out, self.dataset_trace.get(&trace_id));
        }
        if let Some(span_id) = filter.span_id {
            intersect_metadata_candidates(&mut out, self.dataset_span.get(&span_id));
        }
        if let Some(eval_run_id) = &filter.eval_run_id {
            intersect_metadata_candidates(&mut out, self.dataset_eval_run.get(eval_run_id));
        }
        if let Some(split) = &filter.split {
            intersect_metadata_candidates(&mut out, self.dataset_split.get(split));
        }
        if let Some(label) = &filter.label {
            intersect_metadata_candidates(&mut out, self.dataset_label.get(label));
        }
        for (key, value) in &filter.attrs {
            if let Some(exact_ids) = self.dataset_attrs.get(&(key.clone(), value.clone())) {
                intersect_metadata_candidates(&mut out, Some(exact_ids));
            } else {
                intersect_metadata_candidates(&mut out, self.dataset_attr_keys.get(key));
            }
        }
        out
    }
}

fn insert_metadata_posting<K: Eq + std::hash::Hash>(
    map: &mut HashMap<K, HashSet<u64>>,
    key: K,
    id: u64,
) {
    map.entry(key).or_default().insert(id);
}

fn intersect_metadata_candidates(out: &mut HashSet<u64>, next: Option<&HashSet<u64>>) {
    match next {
        Some(ids) => out.retain(|id| ids.contains(id)),
        None => out.clear(),
    }
}
