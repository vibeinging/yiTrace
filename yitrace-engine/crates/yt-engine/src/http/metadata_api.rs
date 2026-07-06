use super::*;

impl EngineJsonApi {
    pub(super) fn create_annotation_json(&self, body: &str, tenant: Option<u64>) -> (u16, String) {
        let v = match crate::wire::parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let Some((trace_id, external_trace_id)) =
            json_field_alias(&v, &["trace_id", "traceId"]).and_then(json_id_with_external)
        else {
            return (400, r#"{"error":"missing trace_id"}"#.to_string());
        };
        let span = json_field_alias(&v, &["span_id", "spanId"]).and_then(json_id_with_external);
        let label = json_field_alias(&v, &["label", "name"])
            .and_then(crate::wire::Json::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if label.is_empty() {
            return (400, r#"{"error":"missing label"}"#.to_string());
        }
        let status = if let Some(status_value) = json_raw_field_alias(&v, &["status"]) {
            let Some(status) = status_value.as_str().and_then(AnnotationStatus::parse) else {
                return (400, r#"{"error":"invalid status"}"#.to_string());
            };
            Some(status)
        } else {
            None
        };
        let target = json_field_alias(&v, &["target", "target_type", "targetType"])
            .and_then(crate::wire::Json::as_str)
            .and_then(AnnotationTarget::parse);
        let mut attrs = std::collections::BTreeMap::new();
        collect_attr_map(&v, &mut attrs);
        let input = NewTraceAnnotation {
            target,
            trace_id,
            span_id: span.as_ref().map(|(id, _)| *id),
            external_trace_id: json_field_alias(&v, &["external_trace_id", "externalTraceId"])
                .and_then(crate::wire::Json::as_str)
                .map(ToString::to_string)
                .or(external_trace_id),
            external_span_id: json_field_alias(&v, &["external_span_id", "externalSpanId"])
                .and_then(crate::wire::Json::as_str)
                .map(ToString::to_string)
                .or_else(|| span.and_then(|(_, ext)| ext)),
            label,
            score: json_field_alias(&v, &["score", "eval_score", "evalScore"])
                .and_then(crate::wire::Json::as_u64)
                .map(|n| n.min(u32::MAX as u64) as u32),
            reason: json_field_alias(&v, &["reason", "comment", "note"])
                .and_then(crate::wire::Json::as_str)
                .map(ToString::to_string),
            source: json_field_alias(&v, &["source", "created_by", "createdBy"])
                .and_then(crate::wire::Json::as_str)
                .map(ToString::to_string),
            status,
            reviewer: json_field_alias(&v, &["reviewer", "reviewed_by", "reviewedBy"])
                .and_then(crate::wire::Json::as_str)
                .map(ToString::to_string),
            attrs,
        };
        let annotation = if self.is_in_process_cluster() {
            let idx = self.metadata_owner_index_for_trace(tenant, trace_id);
            self.shards()[idx].coord.add_annotation_with_id_base(
                input,
                tenant,
                cluster_metadata_id_base(idx),
            )
        } else {
            self.coord().add_annotation(input, tenant)
        };
        (200, json_annotation(&annotation))
    }

    /// GET /v1/annotations?trace_id=...&label=...&attrs={...}
    pub(super) fn annotations_json(&self, query: &str, tenant: Option<u64>) -> (u16, String) {
        let (filter, cursor, limit) = annotation_filter_from_query(query, tenant);
        let items = self.coord().annotations(&filter);
        annotations_page_json(items, cursor, limit, None, "metadata_sidecar+verify")
    }

    pub(super) fn cluster_annotations_json(
        &self,
        query: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let (filter, cursor, limit) = annotation_filter_from_query(query, tenant);
        let mut items = Vec::new();
        for shard in self.shards().iter() {
            items.extend(shard.coord.annotations(&filter));
        }
        annotations_page_json(
            items,
            cursor,
            limit,
            Some(self.shards().len()),
            "fanout_metadata_sidecar+verify",
        )
    }

    /// PATCH /v1/annotations/:id：更新 annotation 的 review 状态或业务字段。
    pub(super) fn update_annotation_json(
        &self,
        id: &str,
        body: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let Ok(annotation_id) = id.parse::<u64>() else {
            return (400, r#"{"error":"invalid annotation_id"}"#.to_string());
        };
        let update = match update_annotation_from_body(body) {
            Ok(update) => update,
            Err((status, body)) => return (status, body),
        };
        match self
            .coord()
            .update_annotation(annotation_id, tenant, update)
        {
            Some(annotation) => (200, json_annotation(&annotation)),
            None => (404, r#"{"error":"annotation not found"}"#.to_string()),
        }
    }

    pub(super) fn cluster_update_annotation_json(
        &self,
        id: &str,
        body: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let Ok(annotation_id) = id.parse::<u64>() else {
            return (400, r#"{"error":"invalid annotation_id"}"#.to_string());
        };
        let update = match update_annotation_from_body(body) {
            Ok(update) => update,
            Err((status, body)) => return (status, body),
        };
        for shard in self.shards().iter() {
            if let Some(annotation) =
                shard
                    .coord
                    .update_annotation(annotation_id, tenant, update.clone())
            {
                return (200, json_annotation(&annotation));
            }
        }
        (404, r#"{"error":"annotation not found"}"#.to_string())
    }

    /// DELETE /v1/annotations/:id：软删除 annotation，默认查询不再返回。
    pub(super) fn delete_annotation_json(
        &self,
        id: &str,
        body: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let Ok(annotation_id) = id.parse::<u64>() else {
            return (400, r#"{"error":"invalid annotation_id"}"#.to_string());
        };
        let (reviewer, reason) = match delete_annotation_from_body(body) {
            Ok(parts) => parts,
            Err((status, body)) => return (status, body),
        };
        match self
            .coord()
            .delete_annotation(annotation_id, tenant, reviewer, reason)
        {
            Some(annotation) => (200, json_annotation(&annotation)),
            None => (404, r#"{"error":"annotation not found"}"#.to_string()),
        }
    }

    pub(super) fn cluster_delete_annotation_json(
        &self,
        id: &str,
        body: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let Ok(annotation_id) = id.parse::<u64>() else {
            return (400, r#"{"error":"invalid annotation_id"}"#.to_string());
        };
        let (reviewer, reason) = match delete_annotation_from_body(body) {
            Ok(parts) => parts,
            Err((status, body)) => return (status, body),
        };
        for shard in self.shards().iter() {
            if let Some(annotation) = shard.coord.delete_annotation(
                annotation_id,
                tenant,
                reviewer.clone(),
                reason.clone(),
            ) {
                return (200, json_annotation(&annotation));
            }
        }
        (404, r#"{"error":"annotation not found"}"#.to_string())
    }

    /// POST /v1/dataset-associations：把 trace/span 绑定到外部 dataset item。
    pub(super) fn create_dataset_association_json(
        &self,
        body: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let v = match crate::wire::parse(body) {
            Ok(v) => v,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, e.replace('"', "'"))),
        };
        let dataset_id = json_field_alias(&v, &["dataset_id", "datasetId", "dataset"])
            .and_then(crate::wire::Json::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if dataset_id.is_empty() {
            return (400, r#"{"error":"missing dataset_id"}"#.to_string());
        }
        let item_id = json_field_alias(
            &v,
            &["item_id", "itemId", "dataset_item_id", "datasetItemId"],
        )
        .and_then(crate::wire::Json::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
        if item_id.is_empty() {
            return (400, r#"{"error":"missing item_id"}"#.to_string());
        }
        let Some((trace_id, external_trace_id)) =
            json_field_alias(&v, &["trace_id", "traceId"]).and_then(json_id_with_external)
        else {
            return (400, r#"{"error":"missing trace_id"}"#.to_string());
        };
        let span = json_field_alias(&v, &["span_id", "spanId"]).and_then(json_id_with_external);
        let mut attrs = std::collections::BTreeMap::new();
        collect_attr_map(&v, &mut attrs);
        let input = NewDatasetAssociation {
            dataset_id,
            item_id,
            trace_id,
            span_id: span.as_ref().map(|(id, _)| *id),
            external_trace_id: json_field_alias(&v, &["external_trace_id", "externalTraceId"])
                .and_then(crate::wire::Json::as_str)
                .map(ToString::to_string)
                .or(external_trace_id),
            external_span_id: json_field_alias(&v, &["external_span_id", "externalSpanId"])
                .and_then(crate::wire::Json::as_str)
                .map(ToString::to_string)
                .or_else(|| span.and_then(|(_, ext)| ext)),
            snapshot_id: json_field_alias(&v, &["snapshot_id", "snapshotId"])
                .and_then(crate::wire::Json::as_str)
                .map(ToString::to_string),
            snapshot_hash: json_field_alias(&v, &["snapshot_hash", "snapshotHash"])
                .and_then(crate::wire::Json::as_str)
                .map(ToString::to_string),
            eval_run_id: json_field_alias(&v, &["eval_run_id", "evalRunId"])
                .and_then(crate::wire::Json::as_str)
                .map(ToString::to_string),
            split: json_field_alias(&v, &["split"])
                .and_then(crate::wire::Json::as_str)
                .map(ToString::to_string),
            label: json_field_alias(&v, &["label"])
                .and_then(crate::wire::Json::as_str)
                .map(ToString::to_string),
            score: json_field_alias(&v, &["score", "eval_score", "evalScore"])
                .and_then(crate::wire::Json::as_u64)
                .map(|n| n.min(u32::MAX as u64) as u32),
            attrs,
        };
        let assoc = if self.is_in_process_cluster() {
            let idx = self.metadata_owner_index_for_trace(tenant, trace_id);
            self.shards()[idx]
                .coord
                .add_dataset_association_with_id_base(input, tenant, cluster_metadata_id_base(idx))
        } else {
            self.coord().add_dataset_association(input, tenant)
        };
        (200, json_dataset_association(&assoc))
    }

    /// GET /v1/dataset-associations?dataset_id=...&item_id=...&trace_id=...
    pub(super) fn dataset_associations_json(
        &self,
        query: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let (filter, cursor, limit) = dataset_filter_from_query(query, tenant);
        let items = self.coord().dataset_associations(&filter);
        dataset_associations_page_json(items, cursor, limit, None, "metadata_sidecar+verify")
    }

    pub(super) fn cluster_dataset_associations_json(
        &self,
        query: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let (filter, cursor, limit) = dataset_filter_from_query(query, tenant);
        let mut items = Vec::new();
        for shard in self.shards().iter() {
            items.extend(shard.coord.dataset_associations(&filter));
        }
        dataset_associations_page_json(
            items,
            cursor,
            limit,
            Some(self.shards().len()),
            "fanout_metadata_sidecar+verify",
        )
    }
}
