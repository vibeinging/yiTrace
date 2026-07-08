use std::collections::{HashMap, HashSet};

use crate::{
    AnnotationStatus, AnnotationTarget, DatasetAssociation, DatasetAssociationFilter,
    RetentionAuditFilter, RetentionAuditRecord, RetentionPolicy, RetentionPolicyFilter,
    TraceAnnotation, TraceAnnotationFilter,
};

#[derive(Default)]
pub(crate) struct MetadataIndex {
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
    audit_all: HashSet<u64>,
    audit_tenant: HashMap<Option<u64>, HashSet<u64>>,
    audit_source: HashMap<String, HashSet<u64>>,
    policy_all: HashSet<u64>,
    policy_tenant: HashMap<Option<u64>, HashSet<u64>>,
    policy_name: HashMap<String, HashSet<u64>>,
    policy_enabled: HashMap<bool, HashSet<u64>>,
}

impl MetadataIndex {
    pub(crate) fn build(
        annotations: &[TraceAnnotation],
        datasets: &[DatasetAssociation],
        audits: &[RetentionAuditRecord],
        policies: &[RetentionPolicy],
    ) -> Self {
        let mut out = Self::default();
        for annotation in annotations {
            out.add_annotation(annotation);
        }
        for dataset in datasets {
            out.add_dataset(dataset);
        }
        for audit in audits {
            out.add_audit(audit);
        }
        for policy in policies {
            out.add_policy(policy);
        }
        out
    }

    pub(crate) fn add_annotation(&mut self, annotation: &TraceAnnotation) {
        let id = annotation.annotation_id;
        self.annotation_all.insert(id);
        if annotation.status != AnnotationStatus::Deleted {
            self.annotation_not_deleted.insert(id);
        }
        insert_posting(&mut self.annotation_tenant, annotation.tenant_id, id);
        insert_posting(&mut self.annotation_target, annotation.target, id);
        insert_posting(&mut self.annotation_trace, annotation.trace_id, id);
        if let Some(span_id) = annotation.span_id {
            insert_posting(&mut self.annotation_span, span_id, id);
        }
        insert_posting(&mut self.annotation_label, annotation.label.clone(), id);
        if let Some(source) = &annotation.source {
            insert_posting(&mut self.annotation_source, source.clone(), id);
        }
        insert_posting(&mut self.annotation_status, annotation.status, id);
        for (key, value) in &annotation.attrs {
            insert_posting(&mut self.annotation_attr_keys, key.clone(), id);
            insert_posting(&mut self.annotation_attrs, (key.clone(), value.clone()), id);
        }
    }

    pub(crate) fn add_dataset(&mut self, dataset: &DatasetAssociation) {
        let id = dataset.association_id;
        self.dataset_all.insert(id);
        insert_posting(&mut self.dataset_tenant, dataset.tenant_id, id);
        insert_posting(&mut self.dataset_dataset, dataset.dataset_id.clone(), id);
        insert_posting(&mut self.dataset_item, dataset.item_id.clone(), id);
        insert_posting(&mut self.dataset_trace, dataset.trace_id, id);
        if let Some(span_id) = dataset.span_id {
            insert_posting(&mut self.dataset_span, span_id, id);
        }
        if let Some(eval_run_id) = &dataset.eval_run_id {
            insert_posting(&mut self.dataset_eval_run, eval_run_id.clone(), id);
        }
        if let Some(split) = &dataset.split {
            insert_posting(&mut self.dataset_split, split.clone(), id);
        }
        if let Some(label) = &dataset.label {
            insert_posting(&mut self.dataset_label, label.clone(), id);
        }
        for (key, value) in &dataset.attrs {
            insert_posting(&mut self.dataset_attr_keys, key.clone(), id);
            insert_posting(&mut self.dataset_attrs, (key.clone(), value.clone()), id);
        }
    }

    pub(crate) fn add_audit(&mut self, audit: &RetentionAuditRecord) {
        let id = audit.audit_id;
        self.audit_all.insert(id);
        insert_posting(&mut self.audit_tenant, audit.tenant_id, id);
        if let Some(source) = &audit.source {
            insert_posting(&mut self.audit_source, source.clone(), id);
        }
    }

    pub(crate) fn add_policy(&mut self, policy: &RetentionPolicy) {
        let id = policy.policy_id;
        self.policy_all.insert(id);
        insert_posting(&mut self.policy_tenant, policy.tenant_id, id);
        insert_posting(&mut self.policy_name, policy.name.clone(), id);
        insert_posting(&mut self.policy_enabled, policy.enabled, id);
    }

    pub(crate) fn annotation_candidates(&self, filter: &TraceAnnotationFilter) -> HashSet<u64> {
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
            intersect_candidates(&mut out, self.annotation_tenant.get(&Some(tenant_id)));
        }
        if let Some(target) = filter.target {
            intersect_candidates(&mut out, self.annotation_target.get(&target));
        }
        if let Some(trace_id) = filter.trace_id {
            intersect_candidates(&mut out, self.annotation_trace.get(&trace_id));
        }
        if let Some(span_id) = filter.span_id {
            intersect_candidates(&mut out, self.annotation_span.get(&span_id));
        }
        if let Some(label) = &filter.label {
            intersect_candidates(&mut out, self.annotation_label.get(label));
        }
        if let Some(source) = &filter.source {
            intersect_candidates(&mut out, self.annotation_source.get(source));
        }
        for (key, value) in &filter.attrs {
            if let Some(ids) = self.annotation_attrs.get(&(key.clone(), value.clone())) {
                intersect_candidates(&mut out, Some(ids));
            } else {
                intersect_candidates(&mut out, self.annotation_attr_keys.get(key));
            }
        }
        out
    }

    pub(crate) fn dataset_candidates(&self, filter: &DatasetAssociationFilter) -> HashSet<u64> {
        let mut out = self.dataset_all.clone();
        if let Some(tenant_id) = filter.tenant_id {
            intersect_candidates(&mut out, self.dataset_tenant.get(&Some(tenant_id)));
        }
        if let Some(dataset_id) = &filter.dataset_id {
            intersect_candidates(&mut out, self.dataset_dataset.get(dataset_id));
        }
        if let Some(item_id) = &filter.item_id {
            intersect_candidates(&mut out, self.dataset_item.get(item_id));
        }
        if let Some(trace_id) = filter.trace_id {
            intersect_candidates(&mut out, self.dataset_trace.get(&trace_id));
        }
        if let Some(span_id) = filter.span_id {
            intersect_candidates(&mut out, self.dataset_span.get(&span_id));
        }
        if let Some(eval_run_id) = &filter.eval_run_id {
            intersect_candidates(&mut out, self.dataset_eval_run.get(eval_run_id));
        }
        if let Some(split) = &filter.split {
            intersect_candidates(&mut out, self.dataset_split.get(split));
        }
        if let Some(label) = &filter.label {
            intersect_candidates(&mut out, self.dataset_label.get(label));
        }
        for (key, value) in &filter.attrs {
            if let Some(ids) = self.dataset_attrs.get(&(key.clone(), value.clone())) {
                intersect_candidates(&mut out, Some(ids));
            } else {
                intersect_candidates(&mut out, self.dataset_attr_keys.get(key));
            }
        }
        out
    }

    pub(crate) fn audit_candidates(&self, filter: &RetentionAuditFilter) -> HashSet<u64> {
        let mut out = if let Some(audit_id) = filter.audit_id {
            single_candidate(audit_id)
        } else {
            self.audit_all.clone()
        };
        if let Some(tenant_id) = filter.tenant_id {
            intersect_candidates(&mut out, self.audit_tenant.get(&Some(tenant_id)));
        }
        if let Some(source) = &filter.source {
            intersect_candidates(&mut out, self.audit_source.get(source));
        }
        out
    }

    pub(crate) fn policy_candidates(&self, filter: &RetentionPolicyFilter) -> HashSet<u64> {
        let mut out = if let Some(policy_id) = filter.policy_id {
            single_candidate(policy_id)
        } else {
            self.policy_all.clone()
        };
        if let Some(tenant_id) = filter.tenant_id {
            intersect_candidates(&mut out, self.policy_tenant.get(&Some(tenant_id)));
        }
        if let Some(name) = &filter.name {
            intersect_candidates(&mut out, self.policy_name.get(name));
        }
        if let Some(enabled) = filter.enabled {
            intersect_candidates(&mut out, self.policy_enabled.get(&enabled));
        }
        out
    }
}

fn insert_posting<K: Eq + std::hash::Hash>(map: &mut HashMap<K, HashSet<u64>>, key: K, id: u64) {
    map.entry(key).or_default().insert(id);
}

fn intersect_candidates(out: &mut HashSet<u64>, next: Option<&HashSet<u64>>) {
    match next {
        Some(ids) => out.retain(|id| ids.contains(id)),
        None => out.clear(),
    }
}

fn single_candidate(id: u64) -> HashSet<u64> {
    let mut out = HashSet::new();
    out.insert(id);
    out
}
