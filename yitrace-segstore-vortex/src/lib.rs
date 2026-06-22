//! yt-segstore-vortex —— **列式段存储（Vortex）**，实现引擎的 `SegmentStore` trait。
//!
//! 一个段 = 一个 `.vortex` 文件，SpanFields 的每个字段一**列**（StructLayout：按列存、只读子集列线性、
//! 随机访问任意列常数时间）。`input_text`/`output_text` 是大列，列式让"数 token / 列表 / 聚合"等查询
//! 完全不碰它们——这是上列式最大的单点收益。
//!
//! 当前是**第一版 round-trip**：一批 `WalRecord` 写成列式文件、读回逐字段一致。投影/谓词下推（Vortex 的
//! `.scan().with_filter(...)` / 列裁剪）是下一步——接缝已在，trait 后续加投影参数即可。
//! 决策与计划见 `docs/design/2026-06-22_列式段存储-vortex-选型与落地计划.md`。

use std::path::{Path, PathBuf};

use tokio::runtime::Runtime;

use vortex::VortexSessionDefault;
use vortex::buffer::{ByteBuffer, ByteBufferMut};
use vortex::error::{VortexError, VortexResult};
use vortex::array::arrays::{PrimitiveArray, StructArray, VarBinViewArray};
use vortex::array::arrow::IntoArrowArray;
use vortex::array::stream::ArrayStreamExt;
use vortex::array::{ArrayRef, IntoArray};
use vortex::expr::{and, col, gt_eq, lit, lt_eq, Expression};
use vortex::file::{OpenOptionsSessionExt, WriteOptionsSessionExt};
use vortex::io::session::RuntimeSessionExt;
use vortex::session::VortexSession;

use arrow::array::{
    Array, AsArray, Int64Array, StringViewArray, UInt32Array, UInt64Array, UInt8Array,
};

use yt_core::event::{EventIdentity, EventType};
use yt_core::fold::{FoldInput, SpanFields};
use yt_core::ids::SegmentId;
use yt_engine::SegmentStore;
use yt_wal::WalRecord;

/// logs（Vec<String>）压成单列：用 NUL 连接，空 logs → None。日志文本里出现 NUL 概率极低（v1 简化，
/// 后续换真正的 list<utf8> 列）。
const LOG_SEP: char = '\u{0}';

/// 列式段存储到一个目录，每段一个 `.vortex` 文件。
pub struct VortexSegmentStore {
    dir: PathBuf,
    session: VortexSession,
    rt: Runtime,
}

impl VortexSegmentStore {
    pub fn open(dir: impl AsRef<Path>) -> std::io::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        // with_tokio 抓当前 tokio 运行时句柄,必须在运行时上下文里调 → 进 rt.enter() 再配。
        let session = {
            let _enter = rt.enter();
            VortexSession::default().with_tokio()
        };
        Ok(Self { dir, session, rt })
    }

    fn seg_path(&self, seg: SegmentId) -> PathBuf {
        self.dir.join(format!("seg-{}.vortex", seg.get()))
    }

    /// 把一批记录建成列式 StructArray（每字段一列）。
    fn build_struct(records: &[WalRecord]) -> StructArray {
        // 原始列辅助：Option<T> 迭代 → 可空原始列。非空列也用 Some 走同一路，保持代码统一。
        macro_rules! u64col {
            ($f:expr) => {
                PrimitiveArray::from_option_iter(records.iter().map(|r| $f(r))).into_array()
            };
        }
        let trace_id = PrimitiveArray::from_option_iter(records.iter().map(|r| Some(r.trace_id))).into_array();
        let span_id = PrimitiveArray::from_option_iter(records.iter().map(|r| Some(r.span_id))).into_array();
        let ts = PrimitiveArray::from_option_iter(records.iter().map(|r| Some(r.ts))).into_array();
        let seq = PrimitiveArray::from_option_iter(records.iter().map(|r| Some(r.identity.seq))).into_array();
        let event_type = PrimitiveArray::from_option_iter(records.iter().map(|r| Some(r.identity.event_type.tag()))).into_array();
        let ext_span_id = VarBinViewArray::from_iter_str(records.iter().map(|r| r.identity.ext_span_id.clone())).into_array();

        let status = PrimitiveArray::from_option_iter(records.iter().map(|r| r.fields.status)).into_array();
        let duration_ns = u64col!(|r: &WalRecord| r.fields.duration_ns);
        let parent_span_id = u64col!(|r: &WalRecord| r.fields.parent_span_id);
        let input_tokens = u64col!(|r: &WalRecord| r.fields.input_tokens);
        let output_tokens = u64col!(|r: &WalRecord| r.fields.output_tokens);
        let session_id = u64col!(|r: &WalRecord| r.fields.session_id);
        let eval_score = PrimitiveArray::from_option_iter(records.iter().map(|r| r.fields.eval_score)).into_array();

        let strcol = |f: &dyn Fn(&WalRecord) -> Option<String>| {
            VarBinViewArray::from_iter_nullable_str(records.iter().map(f)).into_array()
        };
        let agent_name = strcol(&|r| r.fields.agent_name.clone());
        let tool_name = strcol(&|r| r.fields.tool_name.clone());
        let model = strcol(&|r| r.fields.model.clone());
        let input_text = strcol(&|r| r.fields.input_text.clone());
        let output_text = strcol(&|r| r.fields.output_text.clone());
        let eval_label = strcol(&|r| r.fields.eval_label.clone());
        let logs = strcol(&|r| {
            if r.fields.logs.is_empty() {
                None
            } else {
                Some(r.fields.logs.join(&LOG_SEP.to_string()))
            }
        });

        StructArray::from_fields(&[
            ("trace_id", trace_id),
            ("span_id", span_id),
            ("ts", ts),
            ("seq", seq),
            ("event_type", event_type),
            ("ext_span_id", ext_span_id),
            ("status", status),
            ("duration_ns", duration_ns),
            ("parent_span_id", parent_span_id),
            ("input_tokens", input_tokens),
            ("output_tokens", output_tokens),
            ("session_id", session_id),
            ("eval_score", eval_score),
            ("agent_name", agent_name),
            ("tool_name", tool_name),
            ("model", model),
            ("input_text", input_text),
            ("output_text", output_text),
            ("eval_label", eval_label),
            ("logs", logs),
        ])
        .expect("build struct array")
    }

    /// 从读回的 Arrow StructArray 逐行重建 WalRecord。
    fn rows_from_arrow(st: &arrow::array::StructArray) -> Vec<WalRecord> {
        let n = st.len();
        let u64c = |name: &str| st.column_by_name(name).unwrap().as_any().downcast_ref::<UInt64Array>().unwrap().clone();
        let opt_u64 = |a: &UInt64Array, i: usize| if a.is_null(i) { None } else { Some(a.value(i)) };

        let trace_id = u64c("trace_id");
        let span_id = u64c("span_id");
        let ts = st.column_by_name("ts").unwrap().as_any().downcast_ref::<Int64Array>().unwrap().clone();
        let seq = u64c("seq");
        let event_type = st.column_by_name("event_type").unwrap().as_any().downcast_ref::<UInt8Array>().unwrap().clone();
        let ext_span_id = st.column_by_name("ext_span_id").unwrap().as_string_view().clone();
        let status = st.column_by_name("status").unwrap().as_any().downcast_ref::<UInt8Array>().unwrap().clone();
        let duration_ns = u64c("duration_ns");
        let parent_span_id = u64c("parent_span_id");
        let input_tokens = u64c("input_tokens");
        let output_tokens = u64c("output_tokens");
        let session_id = u64c("session_id");
        let eval_score = st.column_by_name("eval_score").unwrap().as_any().downcast_ref::<UInt32Array>().unwrap().clone();

        let sv = |name: &str| st.column_by_name(name).unwrap().as_string_view().clone();
        let agent_name = sv("agent_name");
        let tool_name = sv("tool_name");
        let model = sv("model");
        let input_text = sv("input_text");
        let output_text = sv("output_text");
        let eval_label = sv("eval_label");
        let logs = sv("logs");

        let opt_str = |a: &StringViewArray, i: usize| if a.is_null(i) { None } else { Some(a.value(i).to_string()) };

        (0..n)
            .map(|i| WalRecord {
                trace_id: trace_id.value(i),
                span_id: span_id.value(i),
                ts: ts.value(i),
                identity: EventIdentity {
                    ext_span_id: ext_span_id.value(i).to_string(),
                    seq: seq.value(i),
                    event_type: EventType::from_tag(event_type.value(i)),
                },
                fields: SpanFields {
                    status: if status.is_null(i) { None } else { Some(status.value(i)) },
                    duration_ns: opt_u64(&duration_ns, i),
                    parent_span_id: opt_u64(&parent_span_id, i),
                    input_tokens: opt_u64(&input_tokens, i),
                    output_tokens: opt_u64(&output_tokens, i),
                    session_id: opt_u64(&session_id, i),
                    eval_score: if eval_score.is_null(i) { None } else { Some(eval_score.value(i)) },
                    agent_name: opt_str(&agent_name, i),
                    tool_name: opt_str(&tool_name, i),
                    model: opt_str(&model, i),
                    input_text: opt_str(&input_text, i),
                    output_text: opt_str(&output_text, i),
                    eval_label: opt_str(&eval_label, i),
                    logs: match opt_str(&logs, i) {
                        None => Vec::new(),
                        Some(s) => s.split(LOG_SEP).map(|x| x.to_string()).collect(),
                    },
                },
            })
            .collect()
    }

    /// 读段（可选谓词下推）。`filter=Some(expr)` 时把过滤**推进 Vortex 文件扫描**（`scan().with_filter`），
    /// 只解码命中的行/块，不在 Rust 后置全读再筛。
    fn read_filtered(&self, seg: SegmentId, filter: Option<Expression>) -> Vec<WalRecord> {
        let path = self.seg_path(seg);
        let bytes = std::fs::read(&path).unwrap_or_default();
        if bytes.is_empty() {
            return Vec::new();
        }
        let arr: VortexResult<ArrayRef> = self.rt.block_on(async {
            let buf = ByteBuffer::copy_from(bytes.as_slice());
            let scan = self.session.open_options().open_buffer(buf)?.scan()?;
            let scan = match filter {
                Some(f) => scan.with_filter(f),
                None => scan,
            };
            scan.into_array_stream()?.read_all().await
        });
        let arr = match arr {
            Ok(a) => a,
            Err(e) => {
                eprintln!("[vortex-segstore] scan seg {} 失败: {e}", seg.get());
                return Vec::new();
            }
        };
        let arrow = arr.into_arrow_preferred().expect("vortex→arrow");
        let st = arrow.as_any().downcast_ref::<arrow::array::StructArray>().expect("struct array");
        Self::rows_from_arrow(st)
    }

    /// **按 ts 范围下推过滤**（谓词进文件扫描）：只返回 `ts ∈ [from, to]` 的行。
    /// 这是列式剪枝的主路 —— 读路径按时间窗只碰相关行/块，大段里查一小段时间不全扫。
    pub fn scan_in_time(&self, seg: SegmentId, from: i64, to: i64) -> Vec<WalRecord> {
        let filter = and(gt_eq(col("ts"), lit(from)), lt_eq(col("ts"), lit(to)));
        self.read_filtered(seg, Some(filter))
    }
}

impl SegmentStore for VortexSegmentStore {
    fn flush_to_segment(&self, seg: SegmentId, records: &[WalRecord]) {
        if records.is_empty() {
            return;
        }
        let st = Self::build_struct(records);
        let path = self.seg_path(seg);
        // 写到内存 buffer（VortexWrite 接受 BufferMut），再 std::fs 原子落盘。
        let r: VortexResult<ByteBufferMut> = self.rt.block_on(async {
            let mut buf = ByteBufferMut::empty();
            self.session
                .write_options()
                .write(&mut buf, st.into_array().to_array_stream())
                .await?;
            Ok::<ByteBufferMut, VortexError>(buf)
        });
        match r {
            Ok(buf) => {
                let tmp = path.with_extension("tmp");
                if std::fs::write(&tmp, buf.as_slice()).is_ok() {
                    let _ = std::fs::rename(&tmp, &path); // 原子替换
                }
            }
            Err(e) => eprintln!("[vortex-segstore] flush seg {} 失败: {e}", seg.get()),
        }
    }

    fn scan_records(&self, seg: SegmentId) -> Vec<WalRecord> {
        self.read_filtered(seg, None)
    }

    fn scan_fold_inputs(&self, seg: SegmentId) -> Vec<(u32, FoldInput)> {
        self.scan_records(seg)
            .iter()
            .enumerate()
            .map(|(i, r)| (i as u32, r.to_fold_input()))
            .collect()
    }

    fn unlink_segment(&self, seg: SegmentId) {
        let _ = std::fs::remove_file(self.seg_path(seg));
    }

    /// 覆盖默认（None）：把时间过滤**真下推**进 Vortex 文件扫描，返回命中行的 FoldInput。
    /// 引擎只在「段无删除」时调它（见 trait 文档），故这里不管删除位图。
    fn scan_fold_inputs_in_time(&self, seg: SegmentId, from: i64, to: i64) -> Option<Vec<FoldInput>> {
        Some(self.scan_in_time(seg, from, to).iter().map(|r| r.to_fold_input()).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use yt_core::event::EventType;

    fn temp_dir() -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!("yt_vortex_{}_{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    fn rec(trace: u64, span: u64, seq: u64) -> WalRecord {
        WalRecord {
            trace_id: trace,
            span_id: span,
            ts: seq as i64 * 100,
            identity: EventIdentity { ext_span_id: format!("{trace}-{span}"), seq, event_type: EventType::SpanEnd },
            fields: SpanFields::default(),
        }
    }

    #[test]
    fn columnar_segment_round_trips_all_fields() {
        let dir = temp_dir();
        let store = VortexSegmentStore::open(&dir).unwrap();
        let seg = SegmentId::new(1);

        let mut a = rec(1, 10, 1);
        a.fields.status = Some(0);
        a.fields.input_tokens = Some(1200);
        a.fields.agent_name = Some("风控".into());
        a.fields.output_text = Some("疑似盗刷".into());
        a.fields.logs = vec!["开始".into(), "研判".into()];
        let mut b = rec(2, 20, 1);
        b.fields.status = Some(1);
        b.fields.eval_score = Some(800);
        b.fields.eval_label = Some("未通过".into());
        // b 的 token/agent 留空,验证可空列

        store.flush_to_segment(seg, &[a.clone(), b.clone()]);

        let back = store.scan_records(seg);
        assert_eq!(back.len(), 2);
        // 逐字段一致
        assert_eq!(back[0].trace_id, 1);
        assert_eq!(back[0].identity.ext_span_id, "1-10");
        assert_eq!(back[0].fields.status, Some(0));
        assert_eq!(back[0].fields.input_tokens, Some(1200));
        assert_eq!(back[0].fields.agent_name.as_deref(), Some("风控"));
        assert_eq!(back[0].fields.output_text.as_deref(), Some("疑似盗刷"));
        assert_eq!(back[0].fields.logs, vec!["开始", "研判"]);
        // 可空列:b 的 token/agent 是 None,logs 空
        assert_eq!(back[1].trace_id, 2);
        assert_eq!(back[1].fields.status, Some(1));
        assert_eq!(back[1].fields.input_tokens, None);
        assert_eq!(back[1].fields.agent_name, None);
        assert!(back[1].fields.logs.is_empty());
        assert_eq!(back[1].fields.eval_score, Some(800));
        assert_eq!(back[1].fields.eval_label.as_deref(), Some("未通过"));

        // fold input 行号映射
        let folds = store.scan_fold_inputs(seg);
        assert_eq!(folds.len(), 2);
        assert_eq!(folds[1].0, 1);
        assert_eq!(folds[1].1.trace_id, 2);

        // unlink
        store.unlink_segment(seg);
        assert!(store.scan_records(seg).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn predicate_pushdown_filters_by_time_range() {
        // 谓词下推:按 ts 范围过滤,只读命中行(过滤进 Vortex 扫描,不在 Rust 后置)。
        let dir = temp_dir();
        let store = VortexSegmentStore::open(&dir).unwrap();
        let seg = SegmentId::new(2);
        // 5 行,ts = 100,200,300,400,500(rec 里 ts = seq*100)
        let rows: Vec<WalRecord> = (1..=5).map(|i| rec(1, i, i)).collect();
        store.flush_to_segment(seg, &rows);

        // 全读 5 行
        assert_eq!(store.scan_records(seg).len(), 5);

        // ts ∈ [200,400] → 只剩 3 行(ts=200,300,400)
        let hit = store.scan_in_time(seg, 200, 400);
        let ts: Vec<i64> = hit.iter().map(|r| r.ts).collect();
        assert_eq!(ts, vec![200, 300, 400], "下推过滤只返回时间窗内的行");

        // 窗口外 → 空
        assert!(store.scan_in_time(seg, 1000, 2000).is_empty());
        // 单点窗口
        let one = store.scan_in_time(seg, 300, 300);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].ts, 300);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn engine_uses_vortex_pushdown_end_to_end() {
        // 端到端:用 VortexSegmentStore 起引擎,灌数据 flush 进列式段,带时间窗读 → 引擎走真 Vortex 下推。
        use std::sync::Arc;
        use yt_engine::{TraceQuery, WriteCoordinator};
        use yt_core::ids::WalLsn;

        let dir = temp_dir();
        let store = Arc::new(VortexSegmentStore::open(&dir).unwrap());
        let wc = WriteCoordinator::new(store);

        let rows: Vec<WalRecord> = (1..=3).map(|i| rec(1, i, i)).collect(); // ts = 100,200,300
        wc.ingest(rows.clone());
        wc.commit_flush(&rows, WalLsn::new(3)); // 写进 .vortex 段、内存表回收

        let snap = wc.pin_snapshot();
        // 全开窗:3 条都在(从列式段读回)
        assert_eq!(wc.read_spans_query(&snap, &TraceQuery::all()).0.len(), 3);
        // 时间窗 [150,250]:引擎走 Vortex 下推,只回 ts=200 的 span2
        let (hit, _) = wc.read_spans_query(&snap, &TraceQuery { trace_id: None, time_from: 150, time_to: 250 });
        assert_eq!(hit.len(), 1, "Vortex 下推穿过引擎读路径,行级时间过滤");
        assert_eq!(hit[0].span_id, 2);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
