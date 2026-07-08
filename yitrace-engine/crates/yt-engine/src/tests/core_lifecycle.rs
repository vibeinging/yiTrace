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

#[test]
fn trace_aggregate_rollup_rebuilds_after_upgrade() {
    let wc = WriteCoordinator::new(Arc::new(CapturingStore::default()));
    let e = ev(1, 10, 1, Some(0), Some(100), &["base"]);
    wc.ingest(vec![e.clone()]);
    wc.commit_flush(&[e], WalLsn::new(1));

    wc.commit_upgrade(
        SegmentId::new(1),
        1,
        10,
        SpanFields {
            status: Some(1),
            duration_ns: Some(999),
            input_tokens: Some(7),
            output_tokens: Some(3),
            attrs: std::collections::BTreeMap::from([("validation_status".into(), "fail".into())]),
            ..Default::default()
        },
    );

    let (spans, stats) = wc
        .trace_aggregate_rollup_spans(&TraceQuery::all(), &SearchFilter::default())
        .expect("rollup should be available after synchronous rebuild");
    assert_eq!(stats.source.as_deref(), Some("aggregate_rollup"));
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].status, Some(1));
    assert_eq!(spans[0].duration_ns, Some(999));
    assert_eq!(spans[0].input_tokens, Some(7));
    assert_eq!(spans[0].output_tokens, Some(3));
    assert_eq!(
        spans[0].attrs.get("validation_status").map(String::as_str),
        Some("fail")
    );
    assert_eq!(
        spans[0].event_count, 1,
        "upgrade 是补字段,不能被 rollup 算成新事件"
    );
}

#[test]
fn upgrade_patches_all_fields_not_just_a_subset() {
    // 防回归:upgrade 归并统一走 merge_from,任意可补字段都不被丢
    //（曾经 upgrade 路径只覆盖 status/duration/eval/text 子集,补 model/token 会被悄悄丢）。
    let store = Arc::new(CapturingStore::default());
    let wc = WriteCoordinator::new(store.clone());
    let e = ev(1, 10, 1, Some(0), None, &[]);
    wc.ingest(vec![e.clone()]);
    wc.commit_flush(&[e], WalLsn::new(1));

    // 补写 model + output_tokens —— 这俩不在旧子集里,正是会被丢的字段。
    wc.commit_upgrade(
        SegmentId::new(1),
        1,
        10,
        SpanFields {
            model: Some("qwen3".into()),
            output_tokens: Some(42),
            ..Default::default()
        },
    );

    let snap = wc.pin_snapshot();
    let s = &wc.read_spans(&snap)[0];
    assert_eq!(
        s.model.as_deref(),
        Some("qwen3"),
        "upgrade 补的 model 必须读得到"
    );
    assert_eq!(
        s.output_tokens,
        Some(42),
        "upgrade 补的 output_tokens 必须读得到"
    );
}

#[test]
fn time_window_prunes_segments_and_trace_filter_narrows() {
    // 三个段，时间范围分别在 [0,10] / [100,110] / [200,210]。
    // 查 [100,110] 的窗口应只扫中间那个段，不碰另外两个（活 trace 读扇出收敛）。
    let store = Arc::new(CapturingStore::default());
    let wc = WriteCoordinator::new(store.clone());

    for (lo, trace) in [(0i64, 7u64), (100, 8), (200, 9)] {
        let e = ev_at(trace, 1, (lo as u64) + 1, lo + 5, Some(0), None, &[]); // ts 落在该段窗口内
        let lsn = wc.ingest(vec![e.clone()]);
        wc.commit_flush(&[e], lsn);
    }
    // 三个段：seg1[5,5]、seg2[105,105]、seg3[205,205]（单事件，min=max=ts）
    let snap = wc.pin_snapshot();

    // 全开窗：扫 3 个段
    let (_all, scanned_all) = wc.read_spans_query(&snap, &TraceQuery::all());
    assert_eq!(scanned_all, 3);

    // 时间窗 [100,110]：只扫中间那个段
    let (spans, scanned) = wc.read_spans_query(&snap, &TraceQuery::trace(8, 100, 110));
    assert_eq!(scanned, 1, "时间窗外的两个段被整段剪掉，没扫");
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].trace_id, 8);

    // 时间窗命中中间段、但 trace_id 不匹配 → 段扫了 1 个，但结果为空（行级过滤）
    let (spans2, scanned2) = wc.read_spans_query(&snap, &TraceQuery::trace(999, 100, 110));
    assert_eq!(scanned2, 1);
    assert!(spans2.is_empty());
}

#[test]
fn segment_time_pushdown_used_and_row_filters() {
    // 引擎读路径在「有时间窗 + 段无删除」时走谓词下推,且下推做了段内行级时间过滤
    //（std-only 全扫路径只有段级 zone-map、做不到行级）。
    use std::sync::atomic::Ordering;
    let store = Arc::new(PushdownStore::default());
    let wc = WriteCoordinator::new(store.clone());
    let rows = vec![
        ev_at(1, 10, 1, 100, Some(0), Some(1), &[]),
        ev_at(1, 20, 2, 200, Some(0), Some(1), &[]),
        ev_at(1, 30, 3, 300, Some(0), Some(1), &[]),
    ];
    wc.ingest(rows.clone());
    wc.commit_flush(&rows, WalLsn::new(3)); // 进段(seg 无删除),内存表回收
    let snap = wc.pin_snapshot();

    // 全开窗:无时间窗 → 不触发下推,3 行全在。
    let n0 = store.pushdowns.load(Ordering::Relaxed);
    let (all, _) = wc.read_spans_query(&snap, &TraceQuery::all());
    assert_eq!(all.len(), 3);
    assert_eq!(
        store.pushdowns.load(Ordering::Relaxed),
        n0,
        "全开窗不触发下推"
    );

    // 时间窗 [150,250] → 触发下推,行级过滤只剩 span20(ts=200)。
    let (hit, _) = wc.read_spans_query(
        &snap,
        &TraceQuery {
            trace_id: None,
            time_from: 150,
            time_to: 250,
            tenant_id: None,
        },
    );
    assert!(
        store.pushdowns.load(Ordering::Relaxed) > n0,
        "有时间窗 → 走下推"
    );
    assert_eq!(hit.len(), 1, "下推做了段内行级时间过滤");
    assert_eq!(hit[0].span_id, 20);
}

#[test]
fn aggregation_pushes_narrow_projection_detail_reads_all() {
    // 投影下推:聚合类查询(cost_by_agent)把「不含大文本列」的窄投影下推给段存储;trace 详情读全列。
    use std::sync::atomic::Ordering;
    let store = Arc::new(PushdownStore::default());
    let wc = WriteCoordinator::new(store.clone());

    // 一条带 agent + token + 原文 的 span,flush 进段(无删除)。
    let mut r = ev_at(1, 10, 1, 100, Some(0), Some(5), &[]);
    r.fields.agent_name = Some("风控".into());
    r.fields.input_tokens = Some(100);
    r.fields.output_tokens = Some(20);
    r.fields.output_text = Some("一大段研判正文……".into());
    wc.ingest(vec![r.clone()]);
    wc.commit_flush(&[r], WalLsn::new(1));
    let snap = wc.pin_snapshot();

    // 成本下钻:走投影下推,投影应只含 agent + token,**不含两个文本列**。
    let cost = wc.cost_by_agent(&snap, &TraceQuery::all());
    assert_eq!(cost.len(), 1);
    assert_eq!(cost[0].input_tokens, 100);
    let p = store.last_proj();
    assert!(
        p.has(Projection::AGENT_NAME) && p.has(Projection::INPUT_TOKENS),
        "聚合要的列在投影里"
    );
    assert!(
        !p.has(Projection::INPUT_TEXT) && !p.has(Projection::OUTPUT_TEXT),
        "聚合不读原文 → 投影不含大文本列(列式段据此跳过解码)"
    );

    // trace 详情:读全列,原文必须读得到。
    let detail = &wc.read_spans(&snap)[0];
    assert_eq!(
        detail.output_text.as_deref(),
        Some("一大段研判正文……"),
        "详情读全列、原文在"
    );
    assert!(store.last_proj().is_all(), "详情下推的是全列投影");
    let _ = store.pushdowns.load(Ordering::Relaxed); // 触达字段,消除未读告警
}
