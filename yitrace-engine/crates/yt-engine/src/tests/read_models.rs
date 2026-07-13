#[test]
fn list_traces_aggregates_per_trace() {
    let store = Arc::new(CapturingStore::default());
    let wc = WriteCoordinator::new(store);
    let evs = vec![
        ev(1001, 1, 1, Some(0), Some(100), &[]),
        ev(1002, 1, 1, Some(0), Some(200), &[]),
        ev(1002, 2, 1, Some(1), Some(50), &[]), // 报错 span
    ];
    wc.ingest(evs);

    let snap = wc.pin_snapshot();
    let traces = wc.list_traces(&snap, &TraceQuery::all());
    assert_eq!(traces.len(), 2);
    // 按 trace_id 升序
    assert_eq!(traces[0].trace_id, 1001);
    assert_eq!(traces[0].span_count, 1);
    assert_eq!(traces[0].error_count, 0);
    assert_eq!(traces[1].trace_id, 1002);
    assert_eq!(traces[1].span_count, 2);
    assert_eq!(traces[1].total_duration_ns, 250);
    assert_eq!(traces[1].max_duration_ns, 200);
    assert_eq!(traces[1].error_count, 1, "status=1 的 span 计入报错");
}

#[test]
fn list_traces_rolls_up_tokens() {
    let store = Arc::new(CapturingStore::default());
    let wc = WriteCoordinator::new(store);
    let mut s1 = ev(1, 1, 1, Some(0), Some(100), &[]);
    s1.fields.input_tokens = Some(120);
    s1.fields.output_tokens = Some(45);
    let mut s2 = ev(1, 2, 1, Some(0), Some(50), &[]);
    s2.fields.input_tokens = Some(80);
    s2.fields.output_tokens = Some(30);
    wc.ingest(vec![s1, s2]);

    let snap = wc.pin_snapshot();
    let t = wc.list_traces(&snap, &TraceQuery::all());
    assert_eq!(t[0].total_input_tokens, 200, "输入 token 汇总 = 120+80");
    assert_eq!(t[0].total_output_tokens, 75, "输出 token 汇总 = 45+30");
}

#[test]
fn trace_rollup_fetches_only_requested_trace_ids() {
    let wc = WriteCoordinator::new(Arc::new(CapturingStore::default()));
    let mut a = ev(1, 1, 1, Some(0), Some(10), &[]);
    a.fields.tenant_id = Some(7);
    let mut b1 = ev(2, 1, 1, Some(0), Some(20), &[]);
    b1.fields.tenant_id = Some(7);
    let mut b2 = ev(2, 2, 1, Some(1), Some(30), &[]);
    b2.fields.tenant_id = Some(7);
    let mut hidden = ev(3, 1, 1, Some(0), Some(40), &[]);
    hidden.fields.tenant_id = Some(8);
    wc.ingest(vec![a, b1, b2, hidden]);

    let (by_trace, stats) = wc
        .trace_rollup_spans_for_trace_ids(&[2, 999], Some(7))
        .expect("rollup trace fetch should be available");
    assert_eq!(stats.source.as_deref(), Some("trajectory_rollup"));
    assert_eq!(stats.scanned_segments, 0);
    assert_eq!(stats.matched_spans, 2);
    assert_eq!(by_trace.keys().copied().collect::<Vec<_>>(), vec![2]);
    assert_eq!(by_trace[&2].len(), 2);
    assert_eq!(
        by_trace[&2].iter().map(|s| s.span_id).collect::<Vec<_>>(),
        vec![1, 2]
    );

    let (hidden_by_trace, hidden_stats) = wc
        .trace_rollup_spans_for_trace_ids(&[3], Some(7))
        .expect("rollup trace fetch should still be available");
    assert!(hidden_by_trace.is_empty());
    assert_eq!(hidden_stats.matched_spans, 0);
}

#[test]
fn parent_span_id_survives_fold_for_tree() {
    // trace 是棵树:root(1) → child(2) → grandchild(3)。父子链要穿过折叠活下来。
    let store = Arc::new(CapturingStore::default());
    let wc = WriteCoordinator::new(store);

    let root = ev(1, 1, 1, Some(0), Some(300), &["root"]); // 无父
    let mut child = ev(1, 2, 1, Some(0), Some(200), &["child"]);
    child.fields.parent_span_id = Some(1);
    let mut grandchild = ev(1, 3, 1, Some(0), Some(100), &["grandchild"]);
    grandchild.fields.parent_span_id = Some(2);
    wc.ingest(vec![root, child, grandchild]);

    let snap = wc.pin_snapshot();
    let spans = wc.read_spans(&snap);
    let find = |id: u64| spans.iter().find(|s| s.span_id == id).unwrap();
    assert_eq!(find(1).parent_span_id, None, "root 无父");
    assert_eq!(find(2).parent_span_id, Some(1), "child 的父是 root");
    assert_eq!(find(3).parent_span_id, Some(2), "grandchild 的父是 child");
}

#[test]
fn agent_graph_collapses_tree_into_caller_callee() {
    // trace 树:规划(1) ├─ 工具 kb_lookup(2)
    //                  └─ 执行(3) ├─ 执行(4,同 agent,自环跳过)
    //                            └─ 工具 calc(5)
    let store = Arc::new(CapturingStore::default());
    let wc = WriteCoordinator::new(store);

    let mut s1 = ev(1, 1, 1, Some(0), Some(300), &[]);
    s1.fields.agent_name = Some("规划".into());
    let mut s2 = ev(1, 2, 1, Some(0), Some(50), &[]);
    s2.fields.tool_name = Some("kb_lookup".into());
    s2.fields.parent_span_id = Some(1);
    let mut s3 = ev(1, 3, 1, Some(0), Some(200), &[]);
    s3.fields.agent_name = Some("执行".into());
    s3.fields.parent_span_id = Some(1);
    s3.fields.input_tokens = Some(80);
    let mut s4 = ev(1, 4, 1, Some(0), Some(100), &[]);
    s4.fields.agent_name = Some("执行".into()); // 同 agent → 自环
    s4.fields.parent_span_id = Some(3);
    s4.fields.input_tokens = Some(20);
    let mut s5 = ev(1, 5, 1, Some(0), Some(30), &[]);
    s5.fields.tool_name = Some("calc".into());
    s5.fields.parent_span_id = Some(3);
    wc.ingest(vec![s1, s2, s3, s4, s5]);

    let snap = wc.pin_snapshot();
    let g = wc.agent_graph(&snap, 1);

    // 节点:4 个角色,按名升序;执行 聚合两条 span + token 80+20。
    let names: Vec<&str> = g.nodes.iter().map(|n| n.actor.as_str()).collect();
    assert_eq!(names, vec!["calc", "kb_lookup", "执行", "规划"]);
    let exec = g.nodes.iter().find(|n| n.actor == "执行").unwrap();
    assert_eq!(
        (exec.kind, exec.span_count, exec.input_tokens),
        (ActorKind::Agent, 2, 100)
    );
    let kb = g.nodes.iter().find(|n| n.actor == "kb_lookup").unwrap();
    assert_eq!(kb.kind, ActorKind::Tool);

    // 边:规划→kb_lookup、规划→执行、执行→calc;执行→执行 自环被剔除。
    let edges: Vec<(&str, &str, usize)> = g
        .edges
        .iter()
        .map(|e| (e.from.as_str(), e.to.as_str(), e.count))
        .collect();
    assert_eq!(
        edges,
        vec![
            ("执行", "calc", 1),
            ("规划", "kb_lookup", 1),
            ("规划", "执行", 1)
        ],
        "跨角色调用/移交,自环已剔除,按 (from,to) 升序"
    );
}

#[test]
fn load_trace_tree_assembles_parent_child() {
    // root(1) ├─ child(2) ─ grandchild(4)
    //         └─ child(3)
    let store = Arc::new(CapturingStore::default());
    let wc = WriteCoordinator::new(store);
    let root = ev(1, 1, 1, Some(0), Some(300), &["root"]);
    let mut c2 = ev(1, 2, 1, Some(0), Some(200), &[]);
    c2.fields.parent_span_id = Some(1);
    let mut c3 = ev(1, 3, 1, Some(0), Some(100), &[]);
    c3.fields.parent_span_id = Some(1);
    let mut gc4 = ev(1, 4, 1, Some(0), Some(50), &[]);
    gc4.fields.parent_span_id = Some(2);
    wc.ingest(vec![root, c2, c3, gc4]);

    let snap = wc.pin_snapshot();
    let tree = wc.load_trace_tree(&snap, 1);
    assert_eq!(tree.roots, vec![1]);
    assert_eq!(tree.nodes[&1].children, vec![2, 3]);
    assert_eq!(tree.nodes[&2].children, vec![4]);
    assert!(tree.nodes[&3].children.is_empty());
    // 瀑布顺序：深度优先,孩子升序
    assert_eq!(tree.dfs_order(), vec![1, 2, 4, 3]);
}

#[test]
fn parse_wire_batch_then_ingest_reads_back() {
    // 完整数据路:SDK 线格式 JSON → parse → ingest_wire → 折叠 → 读回（就差 HTTP 那层）。
    let json = r#"[
      {"trace_id":7,"span_id":1,"ts":100,"seq":1,"event_type":1,"ext_span_id":"7-1","status":0,"input_tokens":900,"logs":["开始"]},
      {"trace_id":7,"span_id":1,"ts":150,"seq":2,"event_type":2,"ext_span_id":"7-1","duration_ns":50,"output_tokens":150,"logs":["结束"]}
    ]"#;
    let recs = parse_wire_batch(json).unwrap();
    let store = Arc::new(CapturingStore::default());
    let wc = WriteCoordinator::new(store);
    wc.ingest_wire(recs);

    let snap = wc.pin_snapshot();
    let spans = wc.read_spans(&snap);
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].status, Some(0));
    assert_eq!(spans[0].duration_ns, Some(50));
    assert_eq!(spans[0].input_tokens, Some(900));
    assert_eq!(spans[0].output_tokens, Some(150));
    assert_eq!(spans[0].logs, vec!["开始", "结束"]);
}

#[test]
fn engine_durable_wal_survives_restart() {
    // 引擎级持久化:用文件 WAL 写入 → 丢掉整个引擎(模拟进程崩溃)→ 同路径重开 + recover →
    // 数据从盘上 WAL 重放回来。(段/manifest 仍在内存丢了,全靠 WAL 全量重放恢复进 MemTable。)
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "yt_engine_{}_{}.wal",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));

    {
        let wc = WriteCoordinator::open(Arc::new(InMemorySegmentStore::default()), &path).unwrap();
        wc.ingest(vec![
            ev(1, 10, 1, Some(0), Some(100), &["反洗钱"]),
            ev(1, 20, 1, Some(1), Some(50), &["盗刷"]),
        ]);
        // drop wc：内存表/manifest/段全没了,但 WAL 已 fsync 落盘。
    }

    // 重启:全新引擎(空 manifest+空段)+ 同一 WAL 文件
    let wc2 = WriteCoordinator::open(Arc::new(InMemorySegmentStore::default()), &path).unwrap();
    wc2.recover();
    let snap = wc2.pin_snapshot();
    let spans = wc2.read_spans(&snap);
    assert_eq!(spans.len(), 2, "重启后两条 span 从 WAL 重放回来");
    let find = |id: u64| spans.iter().find(|s| s.span_id == id).unwrap();
    assert_eq!(find(10).logs, vec!["反洗钱"]);
    assert_eq!(find(20).status, Some(1));

    let _ = std::fs::remove_file(&path);
}

#[test]
fn flush_then_restart_survives_via_durable_segments_and_manifest() {
    // #2 收尾:flush 推进水位后(WAL 不再重放那段数据)重启,数据从**持久段 + 持久 manifest**读回。
    // 这正是 WAL-only 持久化补不上的洞:flush 过的数据只活在段里,段/manifest 不落盘就丢。
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "yt_durable_{}_{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);

    {
        let wc = WriteCoordinator::open_durable(&dir).unwrap();
        wc.ingest(vec![
            ev(1, 10, 1, Some(0), Some(100), &["反洗钱"]),
            ev(1, 20, 1, Some(1), Some(50), &["盗刷"]),
        ]);
        wc.flush_memtable(); // 封段(写盘)+ 推进水位 + 落 manifest;内存表被回收
        assert_eq!(wc.memtable_len(), 0, "flush 后内存表清空(数据只在持久段里)");
        wc.commit_delete(SegmentId::new(1), 1); // 删 span20(行1),验证删除也持久
                                                // drop wc：内存全没。盘上有 段文件 + manifest + WAL。
    }

    // 重启:同一目录。recover 重放 WAL 水位之后(此处为空,数据都在段里)。
    let wc2 = WriteCoordinator::open_durable(&dir).unwrap();
    wc2.recover();
    let snap = wc2.pin_snapshot();
    let spans = wc2.read_spans(&snap);
    assert_eq!(
        spans.len(),
        1,
        "flush 过的数据从持久段读回;被删的 span20 不出现(删除持久)"
    );
    assert_eq!(spans[0].span_id, 10);
    assert_eq!(spans[0].logs, vec!["反洗钱"]);
    assert_eq!(spans[0].status, Some(0));

    // 新写入接着用,段 id 不复用(从持久 manifest 恢复了计数器)。
    wc2.ingest(vec![ev(2, 30, 1, Some(0), Some(10), &["转账"])]);
    wc2.flush_memtable();
    let snap2 = wc2.pin_snapshot();
    assert_eq!(
        wc2.read_spans(&snap2).len(),
        2,
        "老段(span10)+新段(span30)都在"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn search_indexes_rebuilt_after_restart() {
    // 检索索引(BM25/向量/属性边车)重启后从持久段 + 向量文件重建 —— 不再是"重启后搜啥都空"。
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "yt_idx_{}_{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);

    {
        let wc = WriteCoordinator::open_durable(&dir).unwrap();
        let mut e1 = ev(1, 10, 1, Some(1), Some(100), &["疑似盗刷 已拦截"]);
        e1.fields.agent_name = Some("风控".into());
        let mut e2 = ev(2, 20, 1, Some(0), Some(50), &["正常转账"]);
        e2.fields.agent_name = Some("规划".into());
        wc.ingest(vec![e1, e2]);
        wc.index_embedding(1, 10, vec![0.0, 0.0]); // 写盘到 vectors.dat
        wc.index_embedding(2, 20, vec![5.0, 5.0]);
        wc.flush_memtable(); // 数据进段;内存里的 BM25/边车随 drop 没,但已可从段重建
    }

    // 重启:索引内存态全空,recover 从段重建 BM25/边车、从向量文件重载向量。
    let wc2 = WriteCoordinator::open_durable(&dir).unwrap();
    wc2.recover();
    let snap = wc2.pin_snapshot();

    // 按内容搜:BM25 从段重建,"盗刷" 命中 span10。
    let hits = wc2.search_text(&snap, "盗刷", 10);
    assert!(
        hits.iter().any(|(s, _)| s.span_id == 10),
        "重启后按内容搜还能命中"
    );

    // 找相似:向量从文件重载,查 [0.1,0.1] 最近的是 span10[0,0]。
    let sim = wc2.search_similar(&snap, &[0.1, 0.1], 10);
    assert!(!sim.is_empty(), "重启后找相似不为空(向量已重载)");
    assert_eq!((sim[0].0.trace_id, sim[0].0.span_id), (1, 10));

    // 带过滤:属性边车重建,按 agent 过滤还生效。
    let f = SearchFilter {
        agent_name: Some("风控".into()),
        ..Default::default()
    };
    let filtered = wc2.search_similar_attr(&snap, &[0.1, 0.1], 10, &f);
    assert!(
        filtered.iter().all(|(s, _)| s.span_id == 10),
        "重启后按 agent 过滤还生效"
    );
    assert!(!filtered.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn durable_recover_defers_segment_scan_when_read_model_caches_hit() {
    // P0: clean recover 只恢复控制面，四份大索引保持 deferred；第一次相关查询按组加载缓存，
    // 不能扫描历史 segment，也不能提前把未使用的索引装进内存。
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "yt_fast_recover_{}_{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);

    {
        let wc = WriteCoordinator::open_durable(&dir).unwrap();
        let mut e = ev(7, 70, 1, Some(0), Some(100), &["疑似盗刷"]);
        e.fields.external_trace_id = Some("run-fast".into());
        wc.ingest(vec![e]);
        wc.flush_memtable();
    }

    let wc2 = WriteCoordinator::open_durable(&dir).unwrap();
    wc2.recover();
    assert!(
        *wc2.segment_scan_indexes_stale.lock().unwrap(),
        "recover 返回时 BM25/bloom 应保持 deferred"
    );
    assert_eq!(
        wc2.seg_key_bloom.lock().unwrap().len(),
        0,
        "recover 不应提前加载 bloom"
    );
    let state = *wc2.read_model_load_state.lock().unwrap();
    assert!(!state.rollup_ready && !state.filter_attrs_ready);

    let filter = SearchFilter {
        external_trace_id: Some("run-fast".into()),
        ..Default::default()
    };
    let (spans, stats) = wc2
        .trace_aggregate_rollup_spans(&TraceQuery::all(), &filter)
        .expect("rollup cache should answer external trace id lookup");
    assert_eq!(stats.scanned_segments, 0);
    assert_eq!(stats.matched_spans, 1);
    assert_eq!(spans[0].external_trace_id.as_deref(), Some("run-fast"));
    assert_eq!(
        wc2.seg_key_bloom.lock().unwrap().len(),
        0,
        "纯 rollup 查询不应顺带加载 bloom"
    );
    let state = *wc2.read_model_load_state.lock().unwrap();
    assert!(state.rollup_ready);
    assert!(!state.filter_attrs_ready);

    let snap = wc2.pin_snapshot();
    let hits = wc2.search_text(&snap, "盗刷", 10);
    assert_eq!(hits.len(), 1);
    assert_eq!((hits[0].0.trace_id, hits[0].0.span_id), (7, 70));
    assert!(
        !*wc2.segment_scan_indexes_stale.lock().unwrap(),
        "第一次全文检索后 BM25/bloom 应进入 ready"
    );
    assert_eq!(wc2.seg_key_bloom.lock().unwrap().len(), 1);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn concurrent_first_queries_load_deferred_read_models_safely() {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Barrier};

    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "yt_lazy_concurrent_{}_{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    {
        let wc = WriteCoordinator::open_durable(&dir).unwrap();
        wc.recover();
        let mut event = ev(9, 90, 1, Some(0), Some(20), &["并发惰性加载盗刷"]);
        event.fields.external_trace_id = Some("lazy-run".into());
        event.fields.agent_name = Some("lazy-agent".into());
        wc.ingest(vec![event]);
        wc.flush_memtable();
    }

    let wc = WriteCoordinator::open_durable(&dir).unwrap();
    wc.recover();
    let barrier = Arc::new(Barrier::new(12));
    let mut workers = Vec::new();
    for worker in 0..12 {
        let wc = Arc::clone(&wc);
        let barrier = Arc::clone(&barrier);
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            let snap = wc.pin_snapshot();
            match worker % 3 {
                0 => {
                    let filter = SearchFilter {
                        external_trace_id: Some("lazy-run".into()),
                        ..Default::default()
                    };
                    let (spans, _) = wc
                        .trace_aggregate_rollup_spans(&TraceQuery::all(), &filter)
                        .expect("rollup should load once and answer");
                    assert_eq!(spans.len(), 1);
                }
                1 => {
                    let hits = wc.search_text(&snap, "盗刷", 10);
                    assert_eq!(hits.len(), 1);
                }
                _ => {
                    let filter = SearchFilter {
                        agent_name: Some("lazy-agent".into()),
                        ..Default::default()
                    };
                    let hits = wc.search_text_attr(&snap, "盗刷", 10, &filter);
                    assert_eq!(hits.len(), 1);
                }
            }
        }));
    }
    for worker in workers {
        worker.join().unwrap();
    }

    let state = *wc.read_model_load_state.lock().unwrap();
    assert!(state.rollup_ready && state.filter_attrs_ready);
    assert!(!*wc.segment_scan_indexes_stale.lock().unwrap());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn durable_uses_disk_vector_index_and_survives_restart_without_rebuild() {
    // 阶段 3：持久引擎默认用**磁盘图索引**——向量+图都落盘到 dir/vecindex，不用 vecstore，
    // 重启从盘恢复、不全量 rebuild。append 多删除少场景：插入只写、提交点批量刷。
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "yt_diskvec_{}_{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);

    {
        let wc = WriteCoordinator::open_durable(&dir).unwrap();
        let e1 = ev(1, 10, 1, Some(0), Some(100), &["a"]);
        let e2 = ev(2, 20, 1, Some(0), Some(200), &["b"]);
        let e3 = ev(3, 30, 1, Some(0), Some(300), &["c"]);
        wc.ingest(vec![e1, e2, e3]);
        wc.index_embedding(1, 10, vec![0.0, 0.0, 0.0]);
        wc.index_embedding(2, 20, vec![1.0, 0.0, 0.0]);
        wc.index_embedding(3, 30, vec![9.0, 9.0, 9.0]);
        wc.flush_memtable(); // 走提交 → graph.flush() 把向量索引刷盘
    }

    // 默认走磁盘图索引：vecindex 目录在、旧 vecstore 文件不在。
    assert!(
        dir.join("vecindex").join("meta").exists(),
        "磁盘图索引已落盘"
    );
    assert!(!dir.join("vectors.dat").exists(), "不再用 vecstore");

    // 重启：不 rebuild（recover 不重放向量，磁盘图索引自带持久），找相似照常。
    let wc2 = WriteCoordinator::open_durable(&dir).unwrap();
    wc2.recover();
    let snap = wc2.pin_snapshot();
    let sim = wc2.search_similar(&snap, &[0.9, 0.0, 0.0], 2);
    assert_eq!(
        (sim[0].0.trace_id, sim[0].0.span_id),
        (2, 20),
        "重启后磁盘图搜索：最近的排第一"
    );
    assert_eq!(sim[0].0.duration_ns, Some(200), "折叠出完整 span");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ingest_otlp_end_to_end_folds_genai_span() {
    // 生态入口端到端:OTLP/HTTP JSON(GenAI 约定)→ 适配器 → ingest → 折叠 → 读回。
    let otlp = r#"{"resourceSpans":[{"scopeSpans":[{"spans":[{
        "traceId":"5b8efff798038103d269b633813fc60c",
        "spanId":"eee19b7ec3c1b174",
        "name":"chat qwen3",
        "startTimeUnixNano":"1700000000000000000",
        "endTimeUnixNano":"1700000000500000000",
        "status":{"code":2},
        "attributes":[
          {"key":"gen_ai.request.model","value":{"stringValue":"qwen3"}},
          {"key":"gen_ai.usage.input_tokens","value":{"intValue":"1200"}},
          {"key":"gen_ai.usage.output_tokens","value":{"intValue":"340"}},
          {"key":"gen_ai.agent.name","value":{"stringValue":"风控研判"}}
        ]
    }]}]}]}"#;
    let store = Arc::new(CapturingStore::default());
    let wc = WriteCoordinator::new(store);
    wc.ingest_otlp(otlp).unwrap();

    let snap = wc.pin_snapshot();
    let spans = wc.read_spans(&snap);
    assert_eq!(spans.len(), 1, "start+end 折叠成一条完整 span");
    let s = &spans[0];
    // 属性(来自 start)与状态/耗时(来自 end)都折叠进同一条
    assert_eq!(s.model.as_deref(), Some("qwen3"));
    assert_eq!(s.input_tokens, Some(1200));
    assert_eq!(s.output_tokens, Some(340));
    assert_eq!(s.agent_name.as_deref(), Some("风控研判"));
    assert_eq!(s.status, Some(1), "OTLP Error → status=1");
    assert_eq!(s.duration_ns, Some(500_000_000));
    assert_eq!(s.event_count, 2);

    // 复用既有聚合:OTLP 灌进来的数据照样能按 agent 归因成本。
    let ac = wc.cost_by_agent(&snap, &TraceQuery::all());
    assert_eq!(ac.len(), 1);
    assert_eq!(ac[0].agent_name, "风控研判");
    assert_eq!(ac[0].input_tokens, 1200);
}

#[test]
fn session_and_per_agent_cost_aggregation() {
    let store = Arc::new(CapturingStore::default());
    let wc = WriteCoordinator::new(store);
    // session 100: trace1(规划) + trace2(执行);  session 200: trace3(规划)
    let mut e1 = ev(1, 1, 1, Some(0), Some(100), &[]);
    e1.fields.session_id = Some(100);
    e1.fields.agent_name = Some("规划".into());
    e1.fields.input_tokens = Some(120);
    e1.fields.output_tokens = Some(40);
    let mut e2 = ev(2, 1, 1, Some(0), Some(50), &[]);
    e2.fields.session_id = Some(100);
    e2.fields.agent_name = Some("执行".into());
    e2.fields.input_tokens = Some(80);
    e2.fields.output_tokens = Some(30);
    let mut e3 = ev(3, 1, 1, Some(0), Some(70), &[]);
    e3.fields.session_id = Some(200);
    e3.fields.agent_name = Some("规划".into());
    e3.fields.input_tokens = Some(60);
    e3.fields.output_tokens = Some(20);
    wc.ingest(vec![e1, e2, e3]);

    let snap = wc.pin_snapshot();

    // 会话:session 100 含 2 条 trace、token 200/70;session 200 含 1 条
    let ss = wc.list_sessions(&snap, &TraceQuery::all());
    assert_eq!(ss.len(), 2);
    assert_eq!(ss[0].session_id, 100);
    assert_eq!(ss[0].trace_count, 2);
    assert_eq!(ss[0].total_input_tokens, 200);
    assert_eq!(ss[1].session_id, 200);
    assert_eq!(ss[1].trace_count, 1);

    // per-agent 成本:规划 = trace1+trace3 token,执行 = trace2
    let ac = wc.cost_by_agent(&snap, &TraceQuery::all());
    let find = |name: &str| ac.iter().find(|a| a.agent_name == name).unwrap();
    assert_eq!(find("规划").input_tokens, 180, "120+60");
    assert_eq!(find("规划").span_count, 2);
    assert_eq!(find("执行").input_tokens, 80);
}

#[test]
fn session_timeline_orders_turns_and_pairs_input_output() {
    let store = Arc::new(CapturingStore::default());
    let wc = WriteCoordinator::new(store);
    // 会话 700 两轮：trace 20 故意先灌（乱序），timeline 应按 trace_id 升序 → trace 10 在前。
    // 每轮两个 span：span1 带输入、span2 带输出，验证「输入取最早、答复取最末」的配对。
    let mut t2s1 = ev(20, 1, 1, Some(0), Some(10), &[]);
    t2s1.fields.session_id = Some(700);
    t2s1.fields.agent_name = Some("客服助手".into());
    t2s1.fields.input_text = Some("还是不行".into());
    t2s1.fields.input_tokens = Some(40);
    let mut t2s2 = ev(20, 2, 1, Some(1), Some(10), &[]);
    t2s2.fields.session_id = Some(700);
    t2s2.fields.output_text = Some("请联系人工客服".into());
    t2s2.fields.output_tokens = Some(15);
    let mut t1s1 = ev(10, 1, 1, Some(0), Some(10), &[]);
    t1s1.fields.session_id = Some(700);
    t1s1.fields.agent_name = Some("客服助手".into());
    t1s1.fields.input_text = Some("如何修改预留手机号".into());
    t1s1.fields.input_tokens = Some(60);
    let mut t1s2 = ev(10, 2, 1, Some(0), Some(10), &[]);
    t1s2.fields.session_id = Some(700);
    t1s2.fields.output_text = Some("到安全中心修改".into());
    t1s2.fields.output_tokens = Some(20);
    wc.ingest(vec![t2s1, t2s2, t1s1, t1s2]);

    let snap = wc.pin_snapshot();
    let tl = wc.load_session_timeline(&snap, 700);
    assert_eq!(tl.turns.len(), 2, "两轮");
    // 按 trace_id 升序定序：trace 10 是第 0 轮、trace 20 是第 1 轮（即便乱序灌入）。
    assert_eq!(tl.turns[0].turn_index, 0);
    assert_eq!(tl.turns[0].trace_id, 10);
    assert_eq!(
        tl.turns[0].user_input.as_deref(),
        Some("如何修改预留手机号")
    );
    assert_eq!(tl.turns[0].agent_output.as_deref(), Some("到安全中心修改"));
    assert_eq!(tl.turns[0].error_count, 0);
    assert_eq!(tl.turns[1].trace_id, 20);
    assert_eq!(tl.turns[1].user_input.as_deref(), Some("还是不行"));
    assert_eq!(tl.turns[1].agent_output.as_deref(), Some("请联系人工客服"));
    assert_eq!(tl.turns[1].error_count, 1, "第二轮 span2 status=1");
    // token 全会话汇总。
    assert_eq!(tl.total_input_tokens, 100, "60+40");
    assert_eq!(tl.total_output_tokens, 35, "20+15");
}

#[test]
fn console_sessions_cache_serves_then_invalidates_on_write() {
    let wc = WriteCoordinator::new(Arc::new(CapturingStore::default()));
    let mut e1 = ev(1, 1, 1, Some(0), Some(10), &[]);
    e1.fields.session_id = Some(100);
    e1.fields.agent_name = Some("风控研判".into());
    e1.fields.input_tokens = Some(500);
    wc.ingest(vec![e1]);

    let snap = wc.pin_snapshot();
    let a = wc.console_sessions(&snap);
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].title, "风控研判");
    // 同代次再读 → 命中缓存、结果一致。
    let b = wc.console_sessions(&snap);
    assert_eq!(a, b, "缓存命中返回同一结果");

    // 新写入 → 代次变、缓存失效，能看到第二个会话。
    let mut e2 = ev(2, 1, 1, Some(0), Some(10), &[]);
    e2.fields.session_id = Some(200);
    e2.fields.agent_name = Some("反洗钱核查".into());
    wc.ingest(vec![e2]);
    let snap2 = wc.pin_snapshot();
    let c = wc.console_sessions(&snap2);
    assert_eq!(c.len(), 2, "写入后缓存失效、能看到新会话");
}

#[test]
fn console_sessions_attrs_rollup_keeps_full_session_and_invalidates() {
    let wc = WriteCoordinator::new(Arc::new(CapturingStore::default()));
    let mut hit = ev(1, 1, 1, Some(0), Some(10), &[]);
    hit.fields.tenant_id = Some(7);
    hit.fields.session_id = Some(700);
    hit.fields.input_tokens = Some(10);
    hit.fields.attrs.insert("project_id".into(), "alpha".into());

    let mut sibling = ev(2, 1, 1, Some(0), Some(20), &[]);
    sibling.fields.tenant_id = Some(7);
    sibling.fields.session_id = Some(700);
    sibling.fields.input_tokens = Some(20);
    sibling
        .fields
        .attrs
        .insert("project_id".into(), "other".into());
    wc.ingest(vec![hit, sibling]);

    let mut attrs = std::collections::BTreeMap::new();
    attrs.insert("project_id".to_string(), "alpha".to_string());
    let snap = wc.pin_snapshot();
    let first = wc.console_sessions_for_tenant_and_attrs(&snap, Some(7), &attrs);
    assert_eq!(first.len(), 1, "命中一条 span 也应返回整个 session");
    assert_eq!(first[0].session_id, 700);
    assert_eq!(first[0].turn_count, 2, "session 聚合要包含未命中 attrs 的另一轮");
    assert_eq!(first[0].input_tokens, 30);

    let cached = wc.console_sessions_for_tenant_and_attrs(&snap, Some(7), &attrs);
    assert_eq!(cached, first, "重复查询应命中 rollup session cache");

    let mut new_session = ev(3, 1, 1, Some(0), Some(30), &[]);
    new_session.fields.tenant_id = Some(7);
    new_session.fields.session_id = Some(701);
    new_session
        .fields
        .attrs
        .insert("project_id".into(), "alpha".into());
    wc.ingest(vec![new_session]);
    let snap2 = wc.pin_snapshot();
    let after_write = wc.console_sessions_for_tenant_and_attrs(&snap2, Some(7), &attrs);
    assert_eq!(after_write.len(), 2, "新写入应让 attrs session cache 失效");
}

#[test]
fn console_sidecar_token_delta_no_double_count() {
    // 增量边车：token 分布在 start(in) / end(out) 两个事件，差量累加不能重复计数（要与折叠一致）。
    let wc = WriteCoordinator::new(Arc::new(CapturingStore::default()));
    let mut start = ev(1, 1, 1, Some(0), None, &[]);
    start.fields.session_id = Some(100);
    start.fields.agent_name = Some("风控研判".into());
    start.fields.input_tokens = Some(500);
    let mut end = ev(1, 1, 2, Some(0), Some(10), &[]);
    end.fields.session_id = Some(100);
    end.fields.output_tokens = Some(120);
    // 再来一条同会话的 trace（第 2 轮）。
    let mut t2 = ev(2, 1, 1, Some(0), Some(10), &[]);
    t2.fields.session_id = Some(100);
    t2.fields.input_tokens = Some(300);
    wc.ingest(vec![start, end, t2]);

    let snap = wc.pin_snapshot();
    let r = wc.console_sessions(&snap);
    assert_eq!(r.len(), 1);
    assert_eq!(
        r[0].input_tokens, 800,
        "500(span1) + 300(span2)，end 不重复加 in"
    );
    assert_eq!(r[0].output_tokens, 120, "只 end 的 out");
    assert_eq!(r[0].turn_count, 2, "两条 trace = 两轮");
    assert_eq!(r[0].title, "风控研判");

    // 增量结果应与全量重建一致。
    let mut idx = wc.session_idx.lock().unwrap();
    let (spans, _) = (wc.read_spans_query(&snap, &TraceQuery::all()).0, 0);
    idx.rebuild(&spans);
    let rebuilt = idx.rows();
    drop(idx);
    assert_eq!(rebuilt, r, "增量维护与全量重建结果一致");
}
