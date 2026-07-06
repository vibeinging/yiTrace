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

#[test]
fn eval_scores_written_back_via_upgrade_and_read_again() {
    // eval 闭环:存 → 规则 scorer 打分 → 分数走 upgrade 写回 → 读回时折叠进 span。
    let store = Arc::new(CapturingStore::default());
    let wc = WriteCoordinator::new(store);

    // 两条 span,各带一段输出文本。span2 的输出含"无法",应判未通过。
    let mut good = ev(1, 10, 1, Some(0), Some(100), &[]);
    good.fields.output_text = Some("已识别为疑似盗刷并拦截".into());
    let mut bad = ev(1, 20, 1, Some(0), Some(120), &[]);
    bad.fields.output_text = Some("抱歉,我无法判断该交易".into());
    wc.ingest(vec![good, bad]);

    // 评测前:还没有分。
    let snap0 = wc.pin_snapshot();
    let before = wc.read_spans(&snap0);
    assert!(before.iter().all(|s| s.eval_score.is_none()), "评测前无分");
    drop(snap0);

    // 跑规则 scorer:输出含"无法/抱歉"判不合格。
    let scorer = KeywordScorer::new(&["无法", "抱歉"]);
    let mut scored = wc.eval_and_writeback(&scorer, &TraceQuery::all());
    scored.sort_by_key(|s| s.span_id);
    assert_eq!(scored.len(), 2, "两条都有 output_text,都被评");
    assert_eq!(scored[0].outcome.score, 1000); // span10 通过
    assert_eq!(scored[1].outcome.score, 0); // span20 未通过
    assert_eq!(scored[1].outcome.label, "未通过");

    // 评测后:分数走 upgrade 写回,读回时折叠进对应 span。
    let snap1 = wc.pin_snapshot();
    let after = wc.read_spans(&snap1);
    let find = |id: u64| after.iter().find(|s| s.span_id == id).unwrap();
    assert_eq!(find(10).eval_score, Some(1000), "span10 满分");
    assert_eq!(find(10).eval_label.as_deref(), Some("通过"));
    assert_eq!(find(20).eval_score, Some(0), "span20 零分");
    assert_eq!(find(20).eval_label.as_deref(), Some("未通过"));
    // 身份/原字段没被评测动:span20 的输出文本还在
    assert_eq!(
        find(20).output_text.as_deref(),
        Some("抱歉,我无法判断该交易")
    );
}

#[test]
fn eval_summary_aggregates_pass_rate_overall_and_per_agent() {
    // eval 看板:打分后按整体 + per-agent 算通过率/均分(回归视图)。
    let store = Arc::new(CapturingStore::default());
    let wc = WriteCoordinator::new(store);

    // 规划 agent:一条好(通过)一条坏(未通过);执行 agent:一条好。
    let mut p_ok = ev(1, 10, 1, Some(0), Some(100), &[]);
    p_ok.fields.agent_name = Some("规划".into());
    p_ok.fields.output_text = Some("结论明确".into());
    let mut p_bad = ev(2, 10, 1, Some(0), Some(100), &[]);
    p_bad.fields.agent_name = Some("规划".into());
    p_bad.fields.output_text = Some("抱歉无法判断".into());
    let mut x_ok = ev(3, 10, 1, Some(0), Some(100), &[]);
    x_ok.fields.agent_name = Some("执行".into());
    x_ok.fields.output_text = Some("已执行".into());
    wc.ingest(vec![p_ok, p_bad, x_ok]);

    let scorer = KeywordScorer::new(&["无法", "抱歉"]);
    wc.eval_and_writeback(&scorer, &TraceQuery::all());

    let snap = wc.pin_snapshot();
    let sum = wc.eval_summary(&snap, &TraceQuery::all(), 1000); // 满分才算通过
                                                                // 第 0 行整体:3 条有分,2 条通过
    assert_eq!(sum[0].agent_name, None);
    assert_eq!(sum[0].scored_spans, 3);
    assert_eq!(sum[0].pass_count, 2);
    assert!((sum[0].pass_rate() - 2.0 / 3.0).abs() < 1e-6);
    // per-agent:规划 1/2 通过,执行 1/1 通过
    let plan = sum
        .iter()
        .find(|r| r.agent_name.as_deref() == Some("规划"))
        .unwrap();
    assert_eq!(
        (plan.scored_spans, plan.pass_count),
        (2, 1),
        "规划 agent 半数通过"
    );
    assert_eq!(plan.avg_score, 500, "规划均分 = (1000+0)/2");
    let exec = sum
        .iter()
        .find(|r| r.agent_name.as_deref() == Some("执行"))
        .unwrap();
    assert_eq!((exec.scored_spans, exec.pass_count), (1, 1));
    assert_eq!(exec.avg_score, 1000);
}

#[test]
fn dataset_collect_failures_then_eval_regression() {
    // eval 燃料闭环:打分 → 把失败样本收集成数据集 → 对数据集回归重跑 scorer。
    let store = Arc::new(CapturingStore::default());
    let wc = WriteCoordinator::new(store);

    let mut ok = ev(1, 10, 1, Some(0), Some(100), &[]);
    ok.fields.agent_name = Some("规划".into());
    ok.fields.output_text = Some("结论明确".into());
    let mut bad1 = ev(2, 10, 1, Some(0), Some(100), &[]);
    bad1.fields.agent_name = Some("规划".into());
    bad1.fields.output_text = Some("抱歉无法判断".into());
    let mut bad2 = ev(3, 10, 1, Some(0), Some(100), &[]);
    bad2.fields.agent_name = Some("执行".into());
    bad2.fields.output_text = Some("无法执行".into());
    wc.ingest(vec![ok, bad1, bad2]);

    let scorer = KeywordScorer::new(&["无法", "抱歉"]);
    wc.eval_and_writeback(&scorer, &TraceQuery::all());

    // 把失败样本(eval_score==0)收集进数据集。
    let snap = wc.pin_snapshot();
    let added = wc.collect_into_dataset("失败集", &snap, &TraceQuery::all(), &|s| {
        s.eval_score == Some(0)
    });
    assert_eq!(added, 2, "两条失败样本入集");
    // 去重:再收集一次不重复加。
    let again = wc.collect_into_dataset("失败集", &snap, &TraceQuery::all(), &|s| {
        s.eval_score == Some(0)
    });
    assert_eq!(again, 0, "已在集里的不重复加");

    let ds = wc.dataset("失败集").unwrap();
    assert_eq!(ds.examples.len(), 2);
    assert_eq!(wc.list_datasets()[0].example_count, 2);

    // 回归:同一 scorer 对数据集重跑 —— 这批本就是失败样本,全不通过。
    let sum = wc.eval_dataset("失败集", &scorer, 1000).unwrap();
    assert_eq!(sum[0].agent_name, None);
    assert_eq!(sum[0].scored_spans, 2);
    assert_eq!(sum[0].pass_count, 0, "失败集对原 scorer 通过率应为 0");

    // 修好的 scorer(不再把这些判失败)→ 回归通过率回升,证明数据集能当基准。
    let lenient = KeywordScorer::new(&["绝不可能出现的词"]);
    let sum2 = wc.eval_dataset("失败集", &lenient, 1000).unwrap();
    assert_eq!(sum2[0].pass_count, 2, "宽松 scorer 下同一数据集全通过");

    assert!(wc.eval_dataset("不存在", &scorer, 1000).is_none());
}

#[test]
fn scorer_skips_spans_without_output_text() {
    // 没有 output_text 的 span 不被评(scorer 返回 None),不写回、不产生噪声分。
    let store = Arc::new(CapturingStore::default());
    let wc = WriteCoordinator::new(store);
    let mut withtext = ev(1, 10, 1, Some(0), Some(100), &[]);
    withtext.fields.output_text = Some("正常结论".into());
    let plain = ev(1, 20, 1, Some(0), Some(50), &[]); // 无 output_text
    wc.ingest(vec![withtext, plain]);

    let scorer = KeywordScorer::new(&["错误"]);
    let scored = wc.eval_and_writeback(&scorer, &TraceQuery::all());
    assert_eq!(scored.len(), 1, "只有带 output_text 的 span 被评");
    assert_eq!(scored[0].span_id, 10);

    let snap = wc.pin_snapshot();
    let after = wc.read_spans(&snap);
    let find = |id: u64| after.iter().find(|s| s.span_id == id).unwrap();
    assert_eq!(find(10).eval_score, Some(1000));
    assert_eq!(find(20).eval_score, None, "无输出文本的 span 不应被打分");
}

#[test]
fn gc_log_crash_after_mark_completes_delete_on_restart() {
    // 生产就绪路线 §1.1：持久化 GC 日志的崩溃安全。
    // 场景：compaction 产生死段 seg1 → reclaim 写了 MARK(意图落盘) → 在 unlink 前 / DONE 前"崩"。
    // 模拟：正常跑完一次 reclaim（MARK+DONE 都写了），然后手动把 gc.log 改回"只有 MARK"，
    //       并把段文件留着（= 模拟"MARK 后、unlink 前崩"）。
    // 预期：open_durable 重启时扫 gc.log，发现"MARK 没 DONE"的 seg1 → 补删段文件 → 不留垃圾。
    let dir = std::env::temp_dir().join(format!("yt_gc_crash_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    // 1) 灌数据 → flush 成 seg1 → 再 flush 成 seg2 → compaction 合出 seg3，seg1 死。
    {
        let wc = WriteCoordinator::open_durable(&dir).unwrap();
        wc.ingest(vec![ev(1, 10, 1, Some(0), Some(100), &["a"])]);
        wc.flush_memtable(); // → seg1
        wc.ingest(vec![ev(1, 10, 2, None, Some(200), &["b"])]);
        wc.flush_memtable(); // → seg2
                             // compaction：把 seg1 + seg2 合成 seg3，seg1/seg2 进 dead_set
        wc.commit_compaction(&[SegmentId::new(1), SegmentId::new(2)]);
        // reclaim：正常走完 MARK + unlink + DONE。seg1/seg2 文件应已删、gc.log 有完整 MARK/DONE。
        let freed = wc.reclaim();
        assert!(freed >= 1, "至少回收到死段");
    }

    // 2) 模拟"MARK 后、unlink 前崩"：重写 gc.log 只留 MARK，并人为把段文件放回来。
    //    （真实崩溃 unlink 没执行，文件还在；这里用 MARK-only 模拟那个状态。）
    let seg_dir = dir.join("segments");
    // 段文件已被 reclaim 删了 → 重新造一个假的 seg1 文件模拟"还在"
    std::fs::write(seg_dir.join("seg-1.dat"), b"fake-leftover-seg1").unwrap();
    // gc.log 改成只有 MARK 1（没有 DONE 1）
    std::fs::write(dir.join("gc.log"), b"MARK 1\n").unwrap();
    assert!(
        seg_dir.join("seg-1.dat").exists(),
        "模拟：段文件还在（unlink 前崩）"
    );

    // 3) 重启：open_durable 应扫 gc.log → 发 seg1 "MARK 没 DONE" → 补删。
    let _wc2 = WriteCoordinator::open_durable(&dir).unwrap();
    assert!(
        !seg_dir.join("seg-1.dat").exists(),
        "重启后补删了残留段文件（崩溃安全）"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn gc_log_normal_reclaim_writes_mark_and_done() {
    // 正常路径：reclaim 在持久模式下应写 MARK 和 DONE 两条（不只删文件）。
    let dir = std::env::temp_dir().join(format!("yt_gc_normal_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    {
        let wc = WriteCoordinator::open_durable(&dir).unwrap();
        wc.ingest(vec![ev(1, 10, 1, Some(0), Some(100), &["a"])]);
        wc.flush_memtable(); // seg1
        wc.ingest(vec![ev(1, 10, 2, None, Some(200), &["b"])]);
        wc.flush_memtable(); // seg2
        wc.commit_compaction(&[SegmentId::new(1), SegmentId::new(2)]);
        wc.reclaim();
    }

    let log = std::fs::read_to_string(dir.join("gc.log")).unwrap();
    assert!(log.contains("MARK"), "reclaim 写了 MARK");
    assert!(log.contains("DONE"), "reclaim 写了 DONE");

    let _ = std::fs::remove_dir_all(&dir);
}

/// §1.4 生产就绪路线：模糊测试。
///
/// 随机生成「插入 / flush / compaction / 崩溃重放」序列,每个操作后用一个简明 oracle
/// 计算预期折叠态,断言引擎 read_spans 与之一致。
/// 钉死:折叠语义(去重 + last-non-null)、compaction 不丢、崩溃重放幂等——
/// 这些的正确性边界在随机组合下不塌。
///
/// **范围说明**:delete/upgrade 的字段语义各有专项测试钉死(read_spans_respects_deletion_vector、
/// read_spans_applies_upgrade_and_respects_snapshot、crash_replay_with_pending_upgrade_is_deterministic),
/// 不纳入本 fuzz——因为它们涉及"删除让该次事件的字段贡献消失"的精确 oracle,写对会绕进折叠内部,
/// 反而偏离 fuzz 的目的(随机组合下发现未知 bug,而非用复杂 oracle 误报)。
#[test]
fn fuzz_fold_semantics_across_random_op_sequences() {
    // 确定性 LCG（可复现、不依赖系统 rand）。
    let rng = |s: &mut u64| {
        *s = s
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (*s >> 33) as usize
    };

    // 跑多个种子，每个种子一个独立序列。
    for seed_orig in [
        0xA11C, 0xB22D, 0xC33E, 0xD44F, 0xE550, 0xF661, 0x1234, 0x5678,
    ] {
        let mut seed = seed_orig;
        let store = Arc::new(InMemorySegmentStore::default());
        let wc = WriteCoordinator::new(store.clone());

        // oracle：(trace,span) → 预期 base 字段（last-non-null）+ 是否存活（未删）。
        use std::collections::BTreeMap;
        #[derive(Default, Clone)]
        struct OracleSpan {
            fields: SpanFields,
            alive: bool,
            next_seq: u64,
        }
        let mut oracle: BTreeMap<(u64, u64), OracleSpan> = BTreeMap::new();
        // 活跃段清单：seg_id → [(row, (trace,span))]（flush 后记录，用于定向 delete/upgrade/compaction）。
        let mut live_segs: Vec<(u64, Vec<(u32, (u64, u64))>)> = Vec::new();

        let steps = 80 + rng(&mut seed) % 40; // 80-119 步
        for _ in 0..steps {
            let op = rng(&mut seed) % 4; // ingest / flush / compaction / crash
            match op {
                0 => {
                    // 插入：随机 (trace,span)，随机状态/耗时/token/logs。
                    let t = 1 + (rng(&mut seed) % 4) as u64;
                    let sp = 1 + (rng(&mut seed) % 4) as u64;
                    let seq = {
                        let o = oracle.entry((t, sp)).or_default();
                        o.next_seq += 1;
                        o.next_seq
                    };
                    let status = if rng(&mut seed) % 3 == 0 {
                        Some(rng(&mut seed) as u8)
                    } else {
                        None
                    };
                    let dur = if rng(&mut seed) % 2 == 0 {
                        Some(100 * (1 + rng(&mut seed) as u64 % 5))
                    } else {
                        None
                    };
                    let logs_idx = rng(&mut seed) % 3;
                    let logs_str: &[&str] = match logs_idx {
                        0 => &["a"],
                        1 => &["b", "c"],
                        _ => &["盗刷"],
                    };
                    let r = ev(t, sp, seq, status, dur, logs_str);
                    wc.ingest(vec![r.clone()]);
                    // oracle：last-non-null 累积
                    let o = oracle.get_mut(&(t, sp)).unwrap();
                    o.alive = true;
                    o.fields.merge_from(&r.fields);
                }
                1 => {
                    // flush（可能产生新段）
                    let snapshot_before = wc.current.manifest().segments.len();
                    wc.flush_memtable();
                    // 若产生了新段，记录它含的 (trace,span)。用 scan_records 读出来。
                    if wc.current.manifest().segments.len() > snapshot_before {
                        let new_seg = *wc.current.manifest().segments.keys().last().unwrap();
                        let recs = wc.segments.scan_records(SegmentId(new_seg));
                        let rows: Vec<(u32, (u64, u64))> = recs
                            .iter()
                            .enumerate()
                            .map(|(i, r)| (i as u32, (r.trace_id, r.span_id)))
                            .collect();
                        live_segs.push((new_seg, rows));
                    }
                }
                2 => {
                    // compaction：合并前两个活跃段（若 ≥2）
                    if live_segs.len() >= 2 {
                        let inputs: Vec<SegmentId> = live_segs
                            .iter()
                            .take(2)
                            .map(|(s, _)| SegmentId(*s))
                            .collect();
                        wc.commit_compaction(&inputs);
                        // compaction 不改折叠结果（只重组段），oracle 不变。移掉被合并的旧段。
                        let removed: Vec<u64> = inputs.iter().map(|s| s.get()).collect();
                        live_segs.retain(|(s, _)| !removed.contains(s));
                    }
                }
                _ => {
                    // 崩溃重放：丢内存表 + recover。确定性 event_id 保证折叠结果不变。
                    wc.simulate_crash_lose_memtable();
                    wc.recover();
                    // oracle 不变（崩溃重放幂等）
                }
            }
        }

        // 序列结束：对比引擎 read_spans 与 oracle。
        let snap = wc.pin_snapshot();
        let actual = wc.read_spans(&snap);
        let actual_map: BTreeMap<(u64, u64), &FoldedSpan> = actual
            .iter()
            .map(|s| ((s.trace_id, s.span_id), s))
            .collect();

        // oracle 里每个 span,引擎必须有且 status/duration 一致（last-non-null 语义）。
        for (key, o) in &oracle {
            let a = actual_map.get(key).unwrap_or_else(|| {
                panic!("种子 {seed_orig:#x}: oracle 说 {key:?} 存在,但引擎没读到");
            });
            assert_eq!(
                a.status, o.fields.status,
                "种子 {seed_orig:#x}: {key:?} status 不一致(last-non-null?)"
            );
            assert_eq!(
                a.duration_ns, o.fields.duration_ns,
                "种子 {seed_orig:#x}: {key:?} duration 不一致"
            );
        }
        // 引擎读出的 span 数 == oracle 的（无多无少）。
        assert_eq!(
            actual_map.len(),
            oracle.len(),
            "种子 {seed_orig:#x}: span 数不一致(引擎 {} vs oracle {})",
            actual_map.len(),
            oracle.len()
        );
    }
}

#[test]
fn backup_snapshot_restores_consistent_data() {
    // §3.3：在线快照备份 → 从备份恢复 → 数据一致。
    let dir = std::env::temp_dir().join(format!("yt_backup_{}", std::process::id()));
    let backup_dir =
        std::env::temp_dir().join(format!("yt_backup_copy_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&backup_dir);

    // 1) 灌数据 + flush（落盘）+ 检索索引建起来
    {
        let wc = WriteCoordinator::open_durable(&dir).unwrap();
        wc.ingest(vec![ev(1, 10, 1, Some(0), Some(100), &["盗刷 拦截"])]);
        wc.flush_memtable(); // → seg1 落盘
        wc.index_embedding(1, 10, vec![0.1, 0.2, 0.3]);

        // 2) 备份
        wc.backup_snapshot(&backup_dir).unwrap();
    }

    // 3) 从备份恢复 → 数据一致
    let restored = WriteCoordinator::open_durable(&backup_dir).unwrap();
    restored.recover();
    let snap = restored.pin_snapshot();
    let spans = restored.read_spans(&snap);
    assert_eq!(spans.len(), 1, "备份恢复后应有一条 span");
    assert_eq!(spans[0].trace_id, 1);
    assert_eq!(spans[0].span_id, 10);
    assert_eq!(spans[0].status, Some(0));
    assert_eq!(spans[0].duration_ns, Some(100));

    // 4) 检索索引也恢复了（BM25 能搜到）
    let empty_filter = SearchFilter::default();
    let hits = restored.search_text_attr(&snap, "盗刷", 10, &empty_filter);
    assert!(!hits.is_empty(), "备份恢复后中文检索应命中");

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&backup_dir);
}

#[test]
fn format_version_check_and_migrate() {
    // §3.4：版本检查 + 迁移。
    let dir = std::env::temp_dir().join(format!("yt_migrate_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    // 新目录：无 manifest → check_format 返回 (0, FORMAT_VER)。
    let (disk, engine) = WriteCoordinator::check_format(&dir);
    assert_eq!(disk, 0, "新目录应报告版本 0");
    assert_eq!(engine, WriteCoordinator::format_version());

    // 灌数据落盘 → manifest 写了 FORMAT_VER。
    {
        let wc = WriteCoordinator::open_durable(&dir).unwrap();
        wc.ingest(vec![ev(1, 1, 1, None, None, &["x"])]);
        wc.flush_memtable();
    }
    let (disk, engine) = WriteCoordinator::check_format(&dir);
    assert_eq!(disk, engine, "落盘后磁盘版本 == 引擎版本");
    assert_eq!(disk, 1, "当前 FORMAT_VER=1");

    // migrate：版本相等 → Ok（无需迁移）。
    assert!(WriteCoordinator::migrate(&dir).is_ok());

    let _ = std::fs::remove_dir_all(&dir);
}
