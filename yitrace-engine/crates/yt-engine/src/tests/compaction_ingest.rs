#[test]
fn compaction_reconciles_concurrent_delete_and_upgrade_open3() {
    // OPEN-3：选段后、提交前并发打到输入段的删除/补写,提交时必须重读合并,否则丢删除/丢补写。
    let store = Arc::new(CapturingStore::default());
    let wc = WriteCoordinator::new(store.clone());
    let a = ev(1, 10, 1, Some(0), Some(100), &[]); // 行0 = span10
    let b = ev(1, 20, 1, Some(0), Some(200), &[]); // 行1 = span20
    let rows = vec![a.clone(), b.clone()];
    wc.ingest(rows.clone());
    wc.commit_flush(&rows, WalLsn::new(2)); // seg1：行0=span10、行1=span20

    // 选段（记录 seg1 的 seq = 0,0）
    let plan = wc.compaction_begin(&[SegmentId::new(1)]);

    // 选段之后、提交之前：并发删除 span20（行1），并发给 span10 补 duration=999
    wc.commit_delete(SegmentId::new(1), 1);
    wc.commit_upgrade(
        SegmentId::new(1),
        1,
        10,
        SpanFields {
            status: None,
            duration_ns: Some(999),
            ..Default::default()
        },
    );

    // 提交：重读合并,删除和补写都不能丢
    let reconciled = wc.compaction_finish(&plan);
    assert!(reconciled, "选段后 seq 变了 → 触发重读合并");

    let snap = wc.pin_snapshot();
    let spans = wc.read_spans(&snap);
    assert_eq!(spans.len(), 1, "span20 的删除没丢 → 只剩 span10");
    assert_eq!(spans[0].span_id, 10);
    assert_eq!(
        spans[0].duration_ns,
        Some(999),
        "span10 的晚到补写没丢 → 来自 upgrade"
    );
}

#[test]
fn concurrent_readers_writer_reclaimer_stay_consistent() {
    // 真·多线程：4 读 + 1 写 + 1 回收 同时跑。验证不崩、不死锁、不变量守住
    //（这套并发设计的全部意义就在这里——前面单线程测试覆盖不到真正的竞争）。
    use std::thread;

    let store = Arc::new(CapturingStore::default());
    let wc = WriteCoordinator::new(store);

    // 种子 span(1,10),全程从不删除 → 任何读者任何时刻都应看得到
    let seed = vec![ev(1, 10, 1, Some(0), Some(100), &["seed"])];
    wc.ingest(seed.clone());
    wc.commit_flush(&seed, WalLsn::new(1));

    let mut handles = Vec::new();

    for _ in 0..4 {
        let wc = wc.clone();
        handles.push(thread::spawn(move || {
            for _ in 0..400 {
                let snap = wc.pin_snapshot();
                let spans = wc.read_spans(&snap);
                // 不变量：种子 span 在任何快照里都可见(它从未被删,被合并也会带进新段)
                assert!(
                    spans.iter().any(|s| s.trace_id == 1 && s.span_id == 10),
                    "并发下种子 span 必须始终可见"
                );
            }
        }));
    }

    {
        let wc = wc.clone();
        handles.push(thread::spawn(move || {
            for i in 2..150u64 {
                let e = ev(2, i, i, Some(0), Some(i), &["w"]);
                let lsn = wc.ingest(vec![e.clone()]);
                if i % 5 == 0 {
                    wc.commit_flush(&[e], lsn);
                }
                if i % 30 == 0 {
                    // 偶尔合并已有段（种子段 + 其它），验证合并与并发读/回收共存
                    wc.commit_compaction(&[SegmentId::new(1)]);
                }
            }
        }));
    }

    {
        let wc = wc.clone();
        handles.push(thread::spawn(move || {
            for _ in 0..400 {
                wc.reclaim();
            }
        }));
    }

    for h in handles {
        h.join()
            .expect("线程不应 panic（无 use-after-free / 无断言失败）");
    }

    // 跑完仍能正常读,种子 span 还在
    let snap = wc.pin_snapshot();
    let spans = wc.read_spans(&snap);
    assert!(
        spans.iter().any(|s| s.trace_id == 1 && s.span_id == 10),
        "压测后种子 span 仍在"
    );
}

#[test]
fn memtable_auto_flushes_to_bound_memory() {
    // 内存表超阈值自动刷盘:写很多条,内存表被限制住,但数据一条不丢(OPEN-2)。
    let store = Arc::new(CapturingStore::default());
    let wc = WriteCoordinator::new(store);
    wc.set_flush_threshold(5);

    for i in 1..=20u64 {
        wc.ingest(vec![ev_at(1, i, i, i as i64, Some(0), Some(i), &[])]);
    }
    // 自动刷盘把内存表压在阈值附近,远小于 20
    assert!(
        wc.memtable_len() < 20,
        "内存表应被自动刷盘限制,而不是涨到 20"
    );

    // 数据一条不丢:20 个 span 都能读出来
    let snap = wc.pin_snapshot();
    let spans = wc.read_spans(&snap);
    assert_eq!(spans.len(), 20, "自动刷盘后 20 条数据全在");
}

#[test]
fn ingest_wire_maps_sdk_format_end_to_end() {
    // 1) 引擎从线格式身份字段算的 event_id 与 SDK/跨语言基准逐字节一致
    let id = EventIdentity {
        ext_span_id: "1002-1".into(),
        seq: 1,
        event_type: EventType::from_tag(1), // SpanStart
    }
    .event_id();
    assert_eq!(id.0, 3941713543033365492, "引擎算的 event_id == SDK 基准");

    // 2) 端到端：灌 SDK 线格式的 start+end 两条 → 折叠出一条完整 span
    let store = Arc::new(CapturingStore::default());
    let wc = WriteCoordinator::new(store);
    let wires = vec![
        WireRecord {
            trace_id: 1002,
            span_id: 1,
            ts: 100,
            seq: 1,
            event_type_tag: EventType::SpanStart.tag(),
            ext_span_id: "1002-1".into(),
            parent_span_id: None,
            status: Some(0),
            duration_ns: None,
            input_tokens: Some(900),
            output_tokens: None,
            session_id: None,
            tenant_id: None,
            external_trace_id: None,
            external_span_id: None,
            external_parent_span_id: None,
            external_session_id: None,
            agent_name: None,
            tool_name: None,
            model: None,
            input_text: None,
            output_text: None,
            logs: vec!["交易风控 开始".into()],
            attrs: Default::default(),
        },
        WireRecord {
            trace_id: 1002,
            span_id: 1,
            ts: 150,
            seq: 2,
            event_type_tag: EventType::SpanEnd.tag(),
            ext_span_id: "1002-1".into(),
            parent_span_id: None,
            status: None,
            duration_ns: Some(50),
            input_tokens: None,
            output_tokens: Some(150),
            session_id: None,
            tenant_id: None,
            external_trace_id: None,
            external_span_id: None,
            external_parent_span_id: None,
            external_session_id: None,
            agent_name: None,
            tool_name: None,
            model: None,
            input_text: None,
            output_text: None,
            logs: vec!["疑似盗刷 已拦截".into()],
            attrs: Default::default(),
        },
    ];
    wc.ingest_wire(wires);

    let snap = wc.pin_snapshot();
    let spans = wc.read_spans(&snap);
    assert_eq!(spans.len(), 1);
    assert_eq!((spans[0].trace_id, spans[0].span_id), (1002, 1));
    assert_eq!(spans[0].status, Some(0), "来自 start");
    assert_eq!(spans[0].duration_ns, Some(50), "来自 end");
    assert_eq!(spans[0].logs, vec!["交易风控 开始", "疑似盗刷 已拦截"]);
    assert_eq!(spans[0].event_count, 2);
    assert_eq!(
        spans[0].input_tokens,
        Some(900),
        "token 从线格式透传 + 折叠"
    );
    assert_eq!(spans[0].output_tokens, Some(150));
}
