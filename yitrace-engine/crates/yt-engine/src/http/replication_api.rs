use super::*;

impl EngineJsonApi {
    pub(super) fn replication_status_json(&self) -> String {
        replication_status_json(&self.coord().replication_status())
    }

    pub(super) fn replication_wal_json(&self, query: &str) -> (u16, String) {
        let after_lsn = replication_after_lsn(query).unwrap_or(0);
        let batch = self.coord().export_wal_after(after_lsn);
        (200, replication_batch_json(&batch, self.coord()))
    }

    pub(super) fn apply_replication_wal_json(&self, body: &str) -> (u16, String) {
        let batch = match parse_replication_batch(body) {
            Ok(batch) => batch,
            Err(e) => return (400, format!(r#"{{"error":"{}"}}"#, json_escape(&e))),
        };
        match self.coord().apply_wal_replication_batch(&batch) {
            Ok(status) => {
                self.invalidate_read_model_cache();
                (
                    200,
                    format!(
                        r#"{{"applied":true,"fromLsn":{},"toLsn":{},"recordCount":{},"status":{}}}"#,
                        batch.from_lsn,
                        batch.to_lsn,
                        batch.records.len(),
                        replication_status_json(&status)
                    ),
                )
            }
            Err(e) => (
                409,
                format!(
                    r#"{{"error":"{}","code":"replication_apply_failed"}}"#,
                    json_escape(&e)
                ),
            ),
        }
    }

    pub(super) fn replication_pull_once_json(
        &self,
        body: &str,
        tenant: Option<u64>,
    ) -> (u16, String) {
        let request = match parse_replication_pull_request(body) {
            Ok(request) => request,
            Err(error) => return (400, format!(r#"{{"error":"{}"}}"#, json_escape(&error))),
        };
        let after_lsn = request
            .after_lsn
            .unwrap_or_else(|| self.coord().replication_status().committed_tail);
        let path = format!("/v1/replication/wal?afterLsn={after_lsn}");
        let leader = RemoteShardClient::new(request.leader_addr.clone()).with_timeout(
            std::time::Duration::from_millis(request.timeout_ms.unwrap_or(3_000)),
        );
        let (status, batch_json) = match leader.route_json_with_tenant("GET", &path, "", tenant) {
            Ok(result) => result,
            Err(error) => {
                return (
                    503,
                    format!(
                        r#"{{"error":"replication pull failed","stage":"fetch","leaderAddr":"{}","detail":"{}"}}"#,
                        json_escape(&request.leader_addr),
                        json_escape(&error)
                    ),
                )
            }
        };
        if status != 200 {
            return (
                status,
                format!(
                    r#"{{"error":"replication pull failed","stage":"fetch","leaderAddr":"{}","leaderStatus":{},"leaderBody":{}}}"#,
                    json_escape(&request.leader_addr),
                    status,
                    json_string_value(&batch_json)
                ),
            );
        }
        let batch = match parse_replication_batch(&batch_json) {
            Ok(batch) => batch,
            Err(error) => {
                return (
                    502,
                    format!(
                        r#"{{"error":"replication pull failed","stage":"parse","detail":"{}"}}"#,
                        json_escape(&error)
                    ),
                )
            }
        };
        match self.coord().apply_wal_replication_batch(&batch) {
            Ok(follower_status) => {
                self.invalidate_read_model_cache();
                (
                    200,
                    format!(
                        r#"{{"pulled":true,"fromLsn":{},"toLsn":{},"recordCount":{},"leaderAddr":"{}","status":{}}}"#,
                        batch.from_lsn,
                        batch.to_lsn,
                        batch.records.len(),
                        json_escape(&request.leader_addr),
                        replication_status_json(&follower_status)
                    ),
                )
            }
            Err(error) => (
                409,
                format!(
                    r#"{{"error":"{}","code":"replication_pull_apply_failed","fromLsn":{},"toLsn":{}}}"#,
                    json_escape(&error),
                    batch.from_lsn,
                    batch.to_lsn
                ),
            ),
        }
    }
}

struct ReplicationPullRequest {
    leader_addr: String,
    after_lsn: Option<u64>,
    timeout_ms: Option<u64>,
}

fn parse_replication_pull_request(body: &str) -> Result<ReplicationPullRequest, String> {
    let value = crate::wire::parse(body)?;
    let leader_addr = json_field_alias(
        &value,
        &[
            "leaderAddr",
            "leader_addr",
            "leader",
            "sourceAddr",
            "source_addr",
            "addr",
        ],
    )
    .and_then(crate::wire::Json::as_str)
    .map(ToString::to_string)
    .ok_or_else(|| "replication pull requires leaderAddr".to_string())?;
    Ok(ReplicationPullRequest {
        leader_addr,
        after_lsn: json_field_alias(&value, &["afterLsn", "after_lsn", "after"])
            .and_then(crate::wire::Json::as_u64),
        timeout_ms: json_field_alias(&value, &["timeoutMs", "timeout_ms"])
            .and_then(crate::wire::Json::as_u64),
    })
}

fn replication_after_lsn(query: &str) -> Option<u64> {
    for (key, value) in query_pairs(query) {
        if matches!(
            key.as_str(),
            "after_lsn" | "afterLsn" | "after" | "from_lsn" | "fromLsn"
        ) {
            return value.parse().ok();
        }
    }
    None
}

fn replication_status_json(status: &crate::ReplicationStatus) -> String {
    format!(
        r#"{{"committedTail":{},"manifestVersion":{},"memtableWatermark":{},"memtableRows":{},"segmentCount":{}}}"#,
        status.committed_tail,
        status.manifest_version,
        status.memtable_watermark,
        status.memtable_rows,
        status.segment_count
    )
}

fn replication_batch_json(
    batch: &crate::WalReplicationBatch,
    coord: &Arc<WriteCoordinator>,
) -> String {
    let records = batch
        .records
        .iter()
        .map(wal_record_wire_json)
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"{{"fromLsn":{},"toLsn":{},"recordCount":{},"records":[{}],"status":{}}}"#,
        batch.from_lsn,
        batch.to_lsn,
        batch.records.len(),
        records,
        replication_status_json(&coord.replication_status())
    )
}

fn parse_replication_batch(body: &str) -> Result<crate::WalReplicationBatch, String> {
    let v = crate::wire::parse(body)?;
    let from_lsn = json_field_alias(&v, &["fromLsn", "from_lsn"])
        .and_then(crate::wire::Json::as_u64)
        .ok_or_else(|| "replication batch 缺/坏字段 fromLsn".to_string())?;
    let to_lsn = json_field_alias(&v, &["toLsn", "to_lsn"])
        .and_then(crate::wire::Json::as_u64)
        .ok_or_else(|| "replication batch 缺/坏字段 toLsn".to_string())?;
    let records_json = json_field_alias(&v, &["records"])
        .ok_or_else(|| "replication batch 缺字段 records".to_string())?
        .to_compact_json();
    let wires = parse_wire_batch(&records_json)?;
    let records = wires
        .into_iter()
        .map(crate::WireRecord::into_wal_record)
        .collect();
    Ok(crate::WalReplicationBatch {
        from_lsn,
        to_lsn,
        records,
    })
}

fn wal_record_wire_json(record: &yt_wal::WalRecord) -> String {
    let mut fields = Vec::new();
    push_num(&mut fields, "trace_id", record.trace_id);
    push_num(&mut fields, "span_id", record.span_id);
    push_num_i64(&mut fields, "ts", record.ts);
    push_num(&mut fields, "seq", record.identity.seq);
    push_num(
        &mut fields,
        "event_type",
        record.identity.event_type.tag() as u64,
    );
    push_str(&mut fields, "ext_span_id", &record.identity.ext_span_id);
    push_opt_num(&mut fields, "parent_span_id", record.fields.parent_span_id);
    push_opt_u8(&mut fields, "status", record.fields.status);
    push_opt_num(&mut fields, "duration_ns", record.fields.duration_ns);
    push_opt_num(&mut fields, "input_tokens", record.fields.input_tokens);
    push_opt_num(&mut fields, "output_tokens", record.fields.output_tokens);
    push_opt_num(
        &mut fields,
        "cached_input_tokens",
        record.fields.cached_input_tokens,
    );
    push_opt_num(
        &mut fields,
        "reasoning_tokens",
        record.fields.reasoning_tokens,
    );
    push_opt_num(&mut fields, "total_tokens", record.fields.total_tokens);
    push_opt_num(&mut fields, "cost_usd_nanos", record.fields.cost_usd_nanos);
    push_opt_str(
        &mut fields,
        "cost_currency",
        record.fields.cost_currency.as_deref(),
    );
    push_opt_str(&mut fields, "provider", record.fields.provider.as_deref());
    push_opt_num(&mut fields, "session_id", record.fields.session_id);
    push_opt_num(&mut fields, "tenant_id", record.fields.tenant_id);
    push_opt_str(
        &mut fields,
        "external_trace_id",
        record.fields.external_trace_id.as_deref(),
    );
    push_opt_str(
        &mut fields,
        "external_span_id",
        record.fields.external_span_id.as_deref(),
    );
    push_opt_str(
        &mut fields,
        "external_parent_span_id",
        record.fields.external_parent_span_id.as_deref(),
    );
    push_opt_str(
        &mut fields,
        "external_session_id",
        record.fields.external_session_id.as_deref(),
    );
    push_opt_str(
        &mut fields,
        "agent_name",
        record.fields.agent_name.as_deref(),
    );
    push_opt_str(&mut fields, "tool_name", record.fields.tool_name.as_deref());
    push_opt_str(&mut fields, "model", record.fields.model.as_deref());
    push_opt_str(
        &mut fields,
        "input_text",
        record.fields.input_text.as_deref(),
    );
    push_opt_str(
        &mut fields,
        "output_text",
        record.fields.output_text.as_deref(),
    );
    if !record.fields.logs.is_empty() {
        fields.push(format!(
            r#""logs":[{}]"#,
            record
                .fields
                .logs
                .iter()
                .map(|log| format!(r#""{}""#, json_escape(log)))
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    let attrs = wal_record_attrs_json(record);
    if !attrs.is_empty() {
        fields.push(format!(r#""attrs":{{{}}}"#, attrs));
    }
    format!("{{{}}}", fields.join(","))
}

fn wal_record_attrs_json(record: &yt_wal::WalRecord) -> String {
    let mut attrs = record.fields.attrs.clone();
    insert_attr_if_absent(
        &mut attrs,
        "project_id",
        record.fields.project_id.as_deref(),
    );
    insert_attr_if_absent(&mut attrs, "skill", record.fields.skill.as_deref());
    insert_attr_if_absent(&mut attrs, "mode", record.fields.mode.as_deref());
    insert_attr_if_absent(&mut attrs, "call_site", record.fields.call_site.as_deref());
    insert_attr_if_absent(
        &mut attrs,
        "task_fingerprint",
        record.fields.task_fingerprint.as_deref(),
    );
    insert_attr_if_absent(&mut attrs, "loop_id", record.fields.loop_id.as_deref());
    insert_attr_if_absent(
        &mut attrs,
        "harness_version",
        record.fields.harness_version.as_deref(),
    );
    insert_attr_if_absent(
        &mut attrs,
        "schema_fingerprint",
        record.fields.schema_fingerprint.as_deref(),
    );
    insert_attr_if_absent(
        &mut attrs,
        "intent_signature",
        record.fields.intent_signature.as_deref(),
    );
    insert_attr_if_absent(
        &mut attrs,
        "validation_status",
        record.fields.validation_status.as_deref(),
    );
    insert_attr_if_absent(
        &mut attrs,
        "review_status",
        record.fields.review_status.as_deref(),
    );
    insert_attr_if_absent(
        &mut attrs,
        "eval_status",
        record.fields.eval_status.as_deref(),
    );
    insert_attr_if_absent(
        &mut attrs,
        "path_memory_id",
        record.fields.path_memory_id.as_deref(),
    );
    insert_attr_if_absent(
        &mut attrs,
        "stop_reason",
        record.fields.stop_reason.as_deref(),
    );
    insert_attr_if_absent(&mut attrs, "phase", record.fields.phase.as_deref());
    insert_attr_if_absent(&mut attrs, "validator", record.fields.validator.as_deref());
    attrs
        .into_iter()
        .map(|(k, v)| format!(r#""{}":{}"#, json_escape(&k), v))
        .collect::<Vec<_>>()
        .join(",")
}

fn insert_attr_if_absent(attrs: &mut BTreeMap<String, String>, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        attrs
            .entry(key.to_string())
            .or_insert_with(|| value.to_string());
    }
}

fn push_num(fields: &mut Vec<String>, key: &str, value: u64) {
    fields.push(format!(r#""{key}":{value}"#));
}

fn push_num_i64(fields: &mut Vec<String>, key: &str, value: i64) {
    fields.push(format!(r#""{key}":{value}"#));
}

fn push_str(fields: &mut Vec<String>, key: &str, value: &str) {
    fields.push(format!(r#""{key}":"{}""#, json_escape(value)));
}

fn push_opt_num(fields: &mut Vec<String>, key: &str, value: Option<u64>) {
    if let Some(value) = value {
        push_num(fields, key, value);
    }
}

fn push_opt_u8(fields: &mut Vec<String>, key: &str, value: Option<u8>) {
    if let Some(value) = value {
        push_num(fields, key, value as u64);
    }
}

fn push_opt_str(fields: &mut Vec<String>, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        push_str(fields, key, value);
    }
}
