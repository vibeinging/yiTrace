fn metadata_tenant_allowed(record_tenant: Option<u64>, wanted: Option<u64>) -> bool {
    wanted.map_or(true, |tenant| record_tenant == Some(tenant))
}

fn metadata_json_scalar_label(raw: &str) -> &str {
    let raw = raw.trim();
    if raw.len() >= 2 && raw.starts_with('"') && raw.ends_with('"') {
        &raw[1..raw.len() - 1]
    } else {
        raw
    }
}

fn metadata_value_matches(actual: &str, expected: &str) -> bool {
    actual == expected || metadata_json_scalar_label(actual) == metadata_json_scalar_label(expected)
}

fn metadata_attrs_match(
    actual: &BTreeMap<String, String>,
    expected: &BTreeMap<String, String>,
) -> bool {
    expected.iter().all(|(key, value)| {
        actual
            .get(key)
            .map(|actual| metadata_value_matches(actual, value))
            .unwrap_or(false)
    })
}

fn annotation_matches(a: &TraceAnnotation, f: &TraceAnnotationFilter) -> bool {
    if !metadata_tenant_allowed(a.tenant_id, f.tenant_id) {
        return false;
    }
    if !f.include_deleted && a.status == AnnotationStatus::Deleted {
        return false;
    }
    if let Some(target) = f.target {
        if a.target != target {
            return false;
        }
    }
    if let Some(trace_id) = f.trace_id {
        if a.trace_id != trace_id {
            return false;
        }
    }
    if let Some(span_id) = f.span_id {
        if a.span_id != Some(span_id) {
            return false;
        }
    }
    if let Some(label) = &f.label {
        if a.label != *label {
            return false;
        }
    }
    if let Some(source) = &f.source {
        if a.source.as_deref() != Some(source.as_str()) {
            return false;
        }
    }
    if let Some(status) = f.status {
        if a.status != status {
            return false;
        }
    }
    metadata_attrs_match(&a.attrs, &f.attrs)
}

fn dataset_association_matches(d: &DatasetAssociation, f: &DatasetAssociationFilter) -> bool {
    if !metadata_tenant_allowed(d.tenant_id, f.tenant_id) {
        return false;
    }
    if let Some(dataset_id) = &f.dataset_id {
        if d.dataset_id != *dataset_id {
            return false;
        }
    }
    if let Some(item_id) = &f.item_id {
        if d.item_id != *item_id {
            return false;
        }
    }
    if let Some(trace_id) = f.trace_id {
        if d.trace_id != trace_id {
            return false;
        }
    }
    if let Some(span_id) = f.span_id {
        if d.span_id != Some(span_id) {
            return false;
        }
    }
    if let Some(eval_run_id) = &f.eval_run_id {
        if d.eval_run_id.as_deref() != Some(eval_run_id.as_str()) {
            return false;
        }
    }
    if let Some(split) = &f.split {
        if d.split.as_deref() != Some(split.as_str()) {
            return false;
        }
    }
    if let Some(label) = &f.label {
        if d.label.as_deref() != Some(label.as_str()) {
            return false;
        }
    }
    metadata_attrs_match(&d.attrs, &f.attrs)
}

fn retention_audit_matches(a: &RetentionAuditRecord, f: &RetentionAuditFilter) -> bool {
    if !metadata_tenant_allowed(a.tenant_id, f.tenant_id) {
        return false;
    }
    if let Some(audit_id) = f.audit_id {
        if a.audit_id != audit_id {
            return false;
        }
    }
    if let Some(source) = &f.source {
        if a.source.as_deref() != Some(source.as_str()) {
            return false;
        }
    }
    if let Some(min_created_at_ns) = f.min_created_at_ns {
        if a.created_at_ns < min_created_at_ns {
            return false;
        }
    }
    if let Some(max_created_at_ns) = f.max_created_at_ns {
        if a.created_at_ns > max_created_at_ns {
            return false;
        }
    }
    true
}

fn retention_policy_matches(p: &RetentionPolicy, f: &RetentionPolicyFilter) -> bool {
    if !metadata_tenant_allowed(p.tenant_id, f.tenant_id) {
        return false;
    }
    if let Some(policy_id) = f.policy_id {
        if p.policy_id != policy_id {
            return false;
        }
    }
    if let Some(name) = &f.name {
        if p.name != *name {
            return false;
        }
    }
    if let Some(enabled) = f.enabled {
        if p.enabled != enabled {
            return false;
        }
    }
    true
}
