#[test]
fn attr_postings_intern_terms_and_keep_query_semantics() {
    let mut postings = AttrPostings::default();
    let project = json_string_compact("agentic-data");
    let next_project = json_string_compact("agentic-data-next");
    let conn_a = json_string_compact("conn-a");
    let missing = json_string_compact("missing");
    let connections = crate::wire::Json::Arr(vec![
        crate::wire::Json::Str("conn-a".to_string()),
        crate::wire::Json::Str("conn-b".to_string()),
    ])
    .to_compact_json();

    postings.add_value((1, 10), "project_id", &project);
    postings.add_value((1, 11), "project_id", &project);
    postings.add_value((1, 12), "connection_ids", &connections);

    assert_eq!(postings.attr_keys.len(), 2, "attr key strings are interned");
    assert_eq!(
        postings.attr_values.len(),
        4,
        "shared values are interned once across exact and array postings"
    );
    assert_eq!(postings.exact.len(), 2);
    assert_eq!(postings.array_items.len(), 2);

    let project_hits = postings.candidates_for_attr("project_id", &project);
    assert!(project_hits.contains(&(1, 10)));
    assert!(project_hits.contains(&(1, 11)));
    let conn_hits = postings.candidates_for_attr("connection_ids", &conn_a);
    assert_eq!(conn_hits, HashSet::from([(1, 12)]));

    let value_count = postings.attr_values.len();
    assert!(postings
        .candidates_for_attr("connection_ids", &missing)
        .is_empty());
    assert_eq!(
        postings.attr_values.len(),
        value_count,
        "querying an unknown value must not grow the interner"
    );

    postings.update((1, 10), "project_id", Some(&project), &next_project);
    let old_hits = postings.candidates_for_attr("project_id", &project);
    assert!(!old_hits.contains(&(1, 10)));
    assert!(old_hits.contains(&(1, 11)));
    assert_eq!(
        postings.candidates_for_attr("project_id", &next_project),
        HashSet::from([(1, 10)])
    );
}

#[test]
fn segment_attr_sidecar_serves_attrs_after_flush_and_recover() {
    let dir =
        std::env::temp_dir().join(format!("yt_attr_sidecar_{}_{}", std::process::id(), 1));
    let _ = std::fs::remove_dir_all(&dir);
    let project = json_string_compact("agentic-data");
    let conn_a = json_string_compact("conn-a");
    let connections = crate::wire::Json::Arr(vec![
        crate::wire::Json::Str("conn-a".to_string()),
        crate::wire::Json::Str("conn-b".to_string()),
    ])
    .to_compact_json();

    {
        let wc = WriteCoordinator::open_durable(&dir).unwrap();
        let mut r = ev(70, 1, 1, Some(0), Some(100), &["builder 盗刷"]);
        r.fields.attrs.insert("project_id".into(), project.clone());
        r.fields
            .attrs
            .insert("connection_ids".into(), connections.clone());
        wc.ingest(vec![r]);
        wc.flush_memtable();

        assert_eq!(
            wc.attr_postings.lock().unwrap().indexed_entries,
            0,
            "flush 后 live postings 不应继续持有历史 segment postings"
        );
        assert!(
            dir.join("attr_postings").join("seg-1.attrs").exists(),
            "flush 后应写出 segment-local attrs sidecar"
        );
        let snap = wc.pin_snapshot();
        let attrs = BTreeMap::from([("project_id".to_string(), project.clone())]);
        let hits = wc.read_spans_query_for_attrs(&snap, &TraceQuery::all(), &attrs);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].trace_id, 70);
    }

    {
        let wc = WriteCoordinator::open_durable(&dir).unwrap();
        wc.recover();
        assert_eq!(
            wc.attr_postings.lock().unwrap().indexed_entries,
            0,
            "recover 扫历史 segment 时不应重建全局 span-level postings"
        );
        assert_eq!(
            wc.seg_attr_cache.lock().unwrap().map.len(),
            0,
            "recover 只重建轻量目录，不预热全部 sidecar posting list"
        );

        let snap = wc.pin_snapshot();
        let attrs = BTreeMap::from([("connection_ids".to_string(), conn_a.clone())]);
        let hits = wc.read_spans_query_for_attrs(&snap, &TraceQuery::all(), &attrs);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].span_id, 1);
        assert!(
            wc.seg_attr_cache.lock().unwrap().map.len() > 0,
            "查询时才按需加载 sidecar posting list"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn first_class_agentic_fields_filter_without_attrs_after_recover() {
    let dir = std::env::temp_dir().join(format!(
        "yt_first_class_agentic_{}_{}",
        std::process::id(),
        1
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let project = json_string_compact("agentic-data");
    let skill = json_string_compact("review");
    let mode = json_string_compact("auto");
    let call_site = json_string_compact("worker.ts:10");
    let task_fingerprint = json_string_compact("npm-native-packaging");
    let loop_id = json_string_compact("loop-1");
    let harness_version = json_string_compact("h1");
    let schema_fingerprint = json_string_compact("schema-v1");
    let intent_signature = json_string_compact("refund-review");
    let validation_status = json_string_compact("pass");
    let review_status = json_string_compact("approved");
    let eval_status = json_string_compact("pass");
    let path_memory_id = json_string_compact("pm-1");
    let stop_reason = json_string_compact("goal_met");
    let phase = json_string_compact("verify");
    let validator = json_string_compact("npm test");

    {
        let wc = WriteCoordinator::open_durable(&dir).unwrap();
        let mut r = ev(71, 1, 1, Some(0), Some(100), &["first-class fields"]);
        r.fields.project_id = Some(project.clone());
        r.fields.skill = Some(skill.clone());
        r.fields.mode = Some(mode.clone());
        r.fields.call_site = Some(call_site.clone());
        r.fields.task_fingerprint = Some(task_fingerprint.clone());
        r.fields.loop_id = Some(loop_id.clone());
        r.fields.harness_version = Some(harness_version.clone());
        r.fields.schema_fingerprint = Some(schema_fingerprint.clone());
        r.fields.intent_signature = Some(intent_signature.clone());
        r.fields.validation_status = Some(validation_status.clone());
        r.fields.review_status = Some(review_status.clone());
        r.fields.eval_status = Some(eval_status.clone());
        r.fields.path_memory_id = Some(path_memory_id.clone());
        r.fields.stop_reason = Some(stop_reason.clone());
        r.fields.phase = Some(phase.clone());
        r.fields.validator = Some(validator.clone());
        assert!(r.fields.attrs.is_empty(), "测试必须不走 attrs 镜像路径");
        wc.ingest(vec![r]);
        wc.flush_memtable();
        assert!(
            dir.join("attr_postings").join("seg-1.attrs").exists(),
            "一等字段也应写入 segment-local attrs sidecar"
        );
    }

    {
        let wc = WriteCoordinator::open_durable(&dir).unwrap();
        wc.recover();
        let snap = wc.pin_snapshot();
        let attrs = BTreeMap::from([
            ("project_id".to_string(), project.clone()),
            ("skill".to_string(), skill.clone()),
            ("task_fingerprint".to_string(), task_fingerprint.clone()),
            ("schema_fingerprint".to_string(), schema_fingerprint.clone()),
            ("validation_status".to_string(), validation_status.clone()),
            ("path_memory_id".to_string(), path_memory_id.clone()),
        ]);
        let hits = wc.read_spans_query_for_attrs(&snap, &TraceQuery::all(), &attrs);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].trace_id, 71);
        assert_eq!(hits[0].project_id.as_deref(), Some(project.as_str()));
        assert_eq!(hits[0].skill.as_deref(), Some(skill.as_str()));
        assert_eq!(
            hits[0].task_fingerprint.as_deref(),
            Some(task_fingerprint.as_str())
        );
        assert_eq!(
            hits[0].validation_status.as_deref(),
            Some(validation_status.as_str())
        );
        assert_eq!(
            hits[0].schema_fingerprint.as_deref(),
            Some(schema_fingerprint.as_str())
        );
        assert_eq!(
            hits[0].path_memory_id.as_deref(),
            Some(path_memory_id.as_str())
        );
        assert!(hits[0].attrs.is_empty(), "过滤不能依赖 attrs 镜像");

        let trace_ids = HashSet::from([71]);
        let fields = wc.trace_attr_fields_for_tenant_and_traces(&snap, None, &trace_ids);
        let row = fields.get(&71).expect("trace fields");
        assert_eq!(
            row.get("project_id").map(String::as_str),
            Some(project.as_str())
        );
        assert_eq!(row.get("skill").map(String::as_str), Some(skill.as_str()));
        assert_eq!(row.get("mode").map(String::as_str), Some(mode.as_str()));
        assert_eq!(
            row.get("call_site").map(String::as_str),
            Some(call_site.as_str())
        );
        assert_eq!(
            row.get("task_fingerprint").map(String::as_str),
            Some(task_fingerprint.as_str())
        );
        assert_eq!(
            row.get("loop_id").map(String::as_str),
            Some(loop_id.as_str())
        );
        assert_eq!(
            row.get("harness_version").map(String::as_str),
            Some(harness_version.as_str())
        );
        assert_eq!(
            row.get("schema_fingerprint").map(String::as_str),
            Some(schema_fingerprint.as_str())
        );
        assert_eq!(
            row.get("intent_signature").map(String::as_str),
            Some(intent_signature.as_str())
        );
        assert_eq!(
            row.get("validation_status").map(String::as_str),
            Some(validation_status.as_str())
        );
        assert_eq!(
            row.get("review_status").map(String::as_str),
            Some(review_status.as_str())
        );
        assert_eq!(
            row.get("eval_status").map(String::as_str),
            Some(eval_status.as_str())
        );
        assert_eq!(
            row.get("path_memory_id").map(String::as_str),
            Some(path_memory_id.as_str())
        );
        assert_eq!(
            row.get("stop_reason").map(String::as_str),
            Some(stop_reason.as_str())
        );
        assert_eq!(row.get("phase").map(String::as_str), Some(phase.as_str()));
        assert_eq!(
            row.get("validator").map(String::as_str),
            Some(validator.as_str())
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn log_events_round_trip_from_memtable_and_segment() {
    let wc = WriteCoordinator::new(Arc::new(CapturingStore::default()));
    let batch = r#"[
      {"trace_id":42,"span_id":7,"ts":100,"seq":1,"event_type":1,"ext_span_id":"42-7","agent_name":"builder"},
      {"trace_id":42,"span_id":7,"ts":120,"seq":2,"event_type":4,"ext_span_id":"42-7","logs":["npm test failed"],"attrs":{"call_site":"package"}},
      {"trace_id":42,"span_id":7,"ts":150,"seq":3,"event_type":2,"ext_span_id":"42-7","status":0,"duration_ns":50}
    ]"#;
    wc.ingest_wire(parse_wire_batch(batch).unwrap());

    let assert_events = |wc: &WriteCoordinator| {
        let snap = wc.pin_snapshot();
        let (spans, _) = wc.read_spans_query(&snap, &TraceQuery::trace(42, i64::MIN, i64::MAX));
        let keys: std::collections::HashSet<(u64, u64)> =
            spans.iter().map(|s| (s.trace_id, s.span_id)).collect();
        let events = wc.log_events_for_trace_keys(&snap, 42, &keys);
        let span_events = events.get(&7).expect("span log events");
        assert_eq!(span_events.len(), 1);
        assert_eq!(span_events[0].seq, 2);
        assert_eq!(span_events[0].event_type, 4);
        assert_eq!(span_events[0].messages, vec!["npm test failed"]);
        assert_eq!(
            span_events[0].attrs.get("call_site").map(String::as_str),
            Some("\"package\"")
        );
    };

    assert_events(&wc);
    wc.flush_memtable();
    assert_events(&wc);
}

#[test]
fn flush_evict_does_not_drop_old_reader_rows() {
    // 引擎级复现并修掉「flush-evict 漏行」（红队棱镜 B）。
    let wc = WriteCoordinator::new(Arc::new(NoopStore));
    wc.ingest(vec![rec("a", 1), rec("b", 2), rec("c", 3)]); // commit_lsn 1,2,3

    // 旧读者 pin（此时 watermark=0）→ 下界=0、上界=committed_tail
    let old = wc.pin_snapshot();
    assert_eq!(wc.read_memtable_lsns(&old), vec![1, 2, 3]);

    // flush 把前缀吸收、watermark 推到 1；但旧读者下界仍=0 → evict gate=0 → 一行都不删
    wc.commit_flush(&[], WalLsn::new(1));
    assert_eq!(
        wc.read_memtable_lsns(&old),
        vec![1, 2, 3],
        "旧读者必须仍看到行 1，不能因 flush evict 漏读"
    );

    // 新读者在 flush 之后 pin → 下界=1
    let newr = wc.pin_snapshot();

    // 旧读者还在时再 flush，gate 仍=min(0,1)=0，行 1 保住
    wc.commit_flush(&[], WalLsn::new(1));
    assert_eq!(wc.read_memtable_lsns(&old), vec![1, 2, 3]);

    // 旧读者走后再 flush，gate 升到 1 → 行 1 被回收；新读者读 (1, tail] 不重不漏
    drop(old);
    wc.commit_flush(&[], WalLsn::new(1));
    assert_eq!(wc.read_memtable_lsns(&newr), vec![2, 3]);
}

#[test]
fn flush_then_delete_keeps_old_snapshot_consistent() {
    let wc = WriteCoordinator::new(Arc::new(NoopStore));
    // 写一批并 flush 成段
    let recs: Vec<WalRecord> = Vec::new();
    let lsn = wc.ingest(recs);
    wc.commit_flush(&[], lsn);
    let v_after_flush = wc.current.version();
    assert_eq!(v_after_flush, 1);

    // 读者 pin 在 v1
    let snap = wc.pin_snapshot();
    assert_eq!(snap.snapshot_id, 1);

    // 并发删除推进到 v2，但旧读者仍 pin v1 → 回收水位被钉在 1
    wc.commit_delete(SegmentId::new(1), 0); // flush 出来的段由协调器分配 = 1
    assert_eq!(wc.current.version(), 2);
    assert_eq!(wc.current.safe_version(), 1);
    // v2 的 dead 资源不可回收，v1 可回收
    assert!(!wc.current.can_reclaim(2, true, true));

    drop(snap);
    assert_eq!(wc.current.safe_version(), 2);
}

#[test]
fn reclaimer_frees_dead_segments_only_when_safe() {
    let store = Arc::new(RecordingStore::default());
    let wc = WriteCoordinator::new(store.clone());
    wc.ingest(vec![rec("a", 1)]);
    wc.commit_flush(&[rec("a", 1)], WalLsn::new(1)); // seg 1, v1
    wc.ingest(vec![rec("b", 2)]);
    wc.commit_flush(&[rec("b", 2)], WalLsn::new(2)); // seg 2, v2

    // 读者在 compaction 之前 pin 在 v2
    let reader = wc.pin_snapshot();
    assert_eq!(reader.snapshot_id, 2);

    // 合并 seg 1+2 → 新段 seg 3，旧段进 dead_set（v_dead=3）
    wc.commit_compaction(&[SegmentId::new(1), SegmentId::new(2)]);
    assert_eq!(wc.dead_count(), 2);

    // 读者仍 pin v2 → safe_version=2 < v_dead=3 → 一个都不能回收
    assert_eq!(wc.reclaim(), 0);
    assert!(store.unlinked().is_empty());
    assert_eq!(wc.dead_count(), 2);

    // 读者释放 → safe_version=3 → seg 1、2 可回收
    drop(reader);
    assert_eq!(wc.reclaim(), 2);
    let mut u = store.unlinked();
    u.sort();
    assert_eq!(u, vec![1, 2]);
    assert_eq!(wc.dead_count(), 0);

    // 幂等：再回收一次什么都不删
    assert_eq!(wc.reclaim(), 0);
}

#[test]
fn buffer_pin_blocks_reclaim() {
    let store = Arc::new(RecordingStore::default());
    let wc = WriteCoordinator::new(store.clone());
    wc.ingest(vec![rec("a", 1)]);
    wc.commit_flush(&[rec("a", 1)], WalLsn::new(1)); // seg 1
    wc.ingest(vec![rec("b", 2)]);
    wc.commit_flush(&[rec("b", 2)], WalLsn::new(2)); // seg 2
    wc.commit_compaction(&[SegmentId::new(1), SegmentId::new(2)]); // dead {1,2}, 无读者 → safe=3

    // seg 1 上有一个未释放的 buffer pin → 即使水位允许也不能删
    wc.pin_buffer(SegmentId::new(1));
    assert_eq!(wc.reclaim(), 1); // 只回收 seg 2
    assert_eq!(store.unlinked(), vec![2]);
    assert_eq!(wc.dead_count(), 1);

    // 释放 buffer pin → seg 1 可回收
    wc.unpin_buffer(SegmentId::new(1));
    assert_eq!(wc.reclaim(), 1);
    let mut u = store.unlinked();
    u.sort();
    assert_eq!(u, vec![1, 2]);
}

#[test]
fn read_spans_folds_segment_and_memtable_end_to_end() {
    // 端到端：一条 span 的 start 进了段、end 还在内存表；读出来折叠成一条完整 span。
    let store = Arc::new(CapturingStore::default());
    let wc = WriteCoordinator::new(store.clone());

    // span(1,10) 的 start 事件：给 status；flush 进 seg 1
    let start = ev(1, 10, 1, Some(0), None, &["开始"]);
    wc.ingest(vec![start.clone()]);
    wc.commit_flush(&[start], WalLsn::new(1)); // seg 1 = 该事件

    // span(1,10) 的 end 事件：给 duration + 日志；仍在内存表（未 flush）
    wc.ingest(vec![ev(1, 10, 2, None, Some(500), &["结束"])]);

    let snap = wc.pin_snapshot();
    let spans = wc.read_spans(&snap);
    assert_eq!(
        spans.len(),
        1,
        "段里的 start + 内存里的 end 折叠成一条 span"
    );
    let s = &spans[0];
    assert_eq!((s.trace_id, s.span_id), (1, 10));
    assert_eq!(s.status, Some(0), "来自段里的 start");
    assert_eq!(s.duration_ns, Some(500), "来自内存里的 end");
    assert_eq!(s.logs, vec!["开始", "结束"], "两源日志并集");
    assert_eq!(s.event_count, 2);
}

#[test]
fn read_spans_respects_deletion_vector() {
    // 段里两个 span，删掉其中一行；读出来只剩没被删的那个。
    let store = Arc::new(CapturingStore::default());
    let wc = WriteCoordinator::new(store.clone());

    let rows = vec![
        ev(1, 10, 1, Some(0), None, &[]),
        ev(1, 20, 1, Some(1), None, &[]),
    ];
    wc.ingest(rows.clone());
    wc.commit_flush(&rows, WalLsn::new(2)); // seg 1，行 0 = span10，行 1 = span20

    // 读者 A 在删除前 pin → 应看到两个 span
    let before = wc.pin_snapshot();
    assert_eq!(wc.read_spans(&before).len(), 2);

    // 删掉段 1 的行 1（span20）
    wc.commit_delete(SegmentId::new(1), 1);

    // 删除后新读者只看到 span10；老读者 A（pin 在删除前版本）仍看到两个（快照隔离）
    let after = wc.pin_snapshot();
    let after_spans = wc.read_spans(&after);
    assert_eq!(after_spans.len(), 1);
    assert_eq!(after_spans[0].span_id, 10);
    assert_eq!(
        wc.read_spans(&before).len(),
        2,
        "老读者快照不受后来的删除影响"
    );
}

#[test]
fn retention_compaction_drops_fully_deleted_segment_without_empty_live_segment() {
    let store = Arc::new(CapturingStore::default());
    let wc = WriteCoordinator::new(store.clone());
    let row = ev(9, 10, 1, Some(0), Some(10), &[]);
    let rows = vec![row.clone()];
    wc.ingest(rows.clone());
    wc.commit_flush(&rows, WalLsn::new(1)); // seg 1
    wc.commit_delete(SegmentId::new(1), 0);

    let compacted = wc.compact_deleted_segments(10, 1, 1, true);
    assert_eq!(compacted.selected_segment_count, 1);
    assert_eq!(compacted.compacted_segment_count, 1);
    assert_eq!(compacted.dropped_deleted_row_count, 1);
    assert_eq!(compacted.rewritten_live_row_count, 0);
    assert_eq!(compacted.reclaimed_segment_count, 1);
    assert_eq!(compacted.after_live_segment_count, 0);
    assert_eq!(compacted.after_dead_segment_count, 0);

    let snap = wc.pin_snapshot();
    assert!(snap.manifest.segments.is_empty(), "不应留下空 live segment");
    assert!(wc.read_spans(&snap).is_empty());
}

#[test]
fn crash_recovery_replay_is_idempotent_no_double_fold() {
    // 红队棱镜 D：崩溃重放不能把已折叠的事件再算一遍。
    let store = Arc::new(CapturingStore::default());
    let wc = WriteCoordinator::new(store.clone());

    let e1 = ev(1, 10, 1, Some(0), None, &["start"]);
    let e2 = ev(1, 10, 2, None, Some(500), &["end"]);
    wc.ingest(vec![e1.clone(), e2.clone()]); // 内存 lsn 1,2

    // 把这俩 flush 进段，但 watermark 故意只推到 0
    //（模拟「段已落盘、水位还没推进」的崩溃窗口 → 段与 WAL 重放会重叠）
    wc.commit_flush(&[e1.clone(), e2.clone()], WalLsn::new(0)); // seg1 含 e1,e2；watermark=0

    let snap0 = wc.pin_snapshot();
    let before = wc.read_spans(&snap0);
    assert_eq!(before.len(), 1);
    assert_eq!(
        before[0].event_count, 2,
        "段+内存已重叠，event_id 去重 → 仍是 2"
    );
    drop(snap0);

    // 崩溃：丢内存表
    wc.simulate_crash_lose_memtable();
    // 恢复：从 WAL 重放 watermark(0) 之后的记录 → e1,e2 回到内存表
    wc.recover();

    // 恢复后再读：段(e1,e2) 与重放回内存的(e1,e2) 重叠，但确定性 event_id 去重 → 逐字段一致
    let snap1 = wc.pin_snapshot();
    let after = wc.read_spans(&snap1);
    assert_eq!(after, before, "崩溃恢复前后折叠结果逐字段一致（重放幂等）");
    assert_eq!(
        after[0].event_count, 2,
        "没有因为重放把事件算两遍 → token/cost 不翻倍"
    );
}

#[test]
fn crash_replay_with_pending_upgrade_is_deterministic() {
    // M2：段已 flush + upgrade 已补写 + 崩溃重放重叠窗口 —— 折叠结果（含补写字段）必须确定不变。
    // 重点：去重保留的是段里的 base 版本（不带 upgrade），upgrade 是折叠后另叠的；崩溃重放把 base
    // 重新灌回内存表后，两份 base 同 event_id 去重，upgrade 仍按 (trace,span) 叠上 → 字段取值不漂移。
    let store = Arc::new(CapturingStore::default());
    let wc = WriteCoordinator::new(store.clone());

    let e1 = ev(1, 10, 1, Some(0), None, &["start"]);
    let e2 = ev(1, 10, 2, None, Some(500), &["end"]);
    wc.ingest(vec![e1.clone(), e2.clone()]);
    // flush 进段但 watermark 只到 0 → 段与 WAL 重放重叠（崩溃窗口）。
    wc.commit_flush(&[e1.clone(), e2.clone()], WalLsn::new(0));

    // 补写：eval_score + model + output_text（base 里没有的字段，正是会被"丢一份"误伤的对象）。
    wc.commit_upgrade(
        SegmentId::new(1),
        1,
        10,
        SpanFields {
            eval_score: Some(900),
            model: Some("qwen3".into()),
            output_text: Some("研判结论".into()),
            ..Default::default()
        },
    );

    let before = wc.read_spans(&wc.pin_snapshot());
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].eval_score, Some(900));
    assert_eq!(before[0].model.as_deref(), Some("qwen3"));
    assert_eq!(before[0].output_text.as_deref(), Some("研判结论"));

    // 崩溃丢内存表 → 重放 watermark(0) 之后的 base 事件回内存表（upgrade 在 manifest，不随内存表丢）。
    wc.simulate_crash_lose_memtable();
    wc.recover();

    let after = wc.read_spans(&wc.pin_snapshot());
    assert_eq!(
        after, before,
        "崩溃重放前后逐字段一致 —— 补写字段没因重叠去重而丢"
    );
    assert_eq!(after[0].event_count, 2, "base 事件没被算两遍");
    assert_eq!(
        after[0].eval_score,
        Some(900),
        "补写的 eval_score 重放后仍在"
    );
}

#[test]
fn read_spans_applies_upgrade_and_respects_snapshot() {
    // 第四个源：晚到属性补写（upgrade）盖到老 span 上，且尊重快照隔离。
    let store = Arc::new(CapturingStore::default());
    let wc = WriteCoordinator::new(store.clone());

    // span(1,10) 进段：status=0，无 duration
    let e = ev(1, 10, 1, Some(0), None, &["start"]);
    wc.ingest(vec![e.clone()]);
    wc.commit_flush(&[e], WalLsn::new(1)); // seg1

    // 升级前读者：duration 还是空
    let before = wc.pin_snapshot();
    assert_eq!(wc.read_spans(&before)[0].duration_ns, None);

    // 晚到补写：给 span(1,10) 补 duration=999 + 一条日志（只补非身份属性）
    wc.commit_upgrade(
        SegmentId::new(1),
        1,
        10,
        SpanFields {
            status: None,
            duration_ns: Some(999),
            logs: vec!["late".into()],
            ..Default::default()
        },
    );

    // 升级后新读者：duration 来自补写，status 仍是原值，日志并集
    let after = wc.pin_snapshot();
    let s = wc.read_spans(&after);
    assert_eq!(
        s[0].status,
        Some(0),
        "status 没被补写动（补写 status=None）"
    );
    assert_eq!(s[0].duration_ns, Some(999), "duration 来自晚到补写");
    assert_eq!(s[0].logs, vec!["start", "late"]);

    // 快照隔离：升级前 pin 的读者仍看到未升级的值
    assert_eq!(
        wc.read_spans(&before)[0].duration_ns,
        None,
        "老读者不受后来补写影响"
    );
}
