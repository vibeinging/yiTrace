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
    let backup_dir = std::env::temp_dir().join(format!("yt_backup_copy_{}", std::process::id()));
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
