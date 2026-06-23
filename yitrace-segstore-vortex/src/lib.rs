//! yt-segstore-vortex —— **列式段存储（Vortex）**，实现引擎的 `SegmentStore` trait。
//!
//! 一个段 = 一个 `.vortex` 文件，SpanFields 的每个字段一**列**（StructLayout：按列存、只读子集列线性、
//! 随机访问任意列常数时间）。`input_text`/`output_text` 是大列，列式让"数 token / 列表 / 聚合"等查询
//! 完全不碰它们——这是上列式最大的单点收益。
//!
//! 已落地：写读 round-trip + **谓词下推**（`scan().with_filter(...)` 按时间窗剪行）+ **投影下推**
//! （`scan().with_projection(select(...))` 只解码命中列，聚合查询跳过大文本列）。写入用 Vortex 默认
//! BtrBlocks 压缩策略（字符串列走 FSST/dict），大文本列在盘上是压缩态。
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
use vortex::expr::{and, col, gt_eq, lit, lt_eq, root, select, Expression};
use vortex::file::{OpenOptionsSessionExt, WriteOptionsSessionExt};
use vortex::io::session::RuntimeSessionExt;
use vortex::session::VortexSession;

use arrow::array::{
    Array, AsArray, Int64Array, StringViewArray, UInt32Array, UInt64Array, UInt8Array,
};

use yt_core::event::{EventIdentity, EventType};
use yt_core::fold::{FoldInput, SpanFields};
use yt_core::ids::SegmentId;
use yt_engine::{Projection, SegmentStore};
use yt_wal::WalRecord;

/// logs（Vec<String>）压成单列：转义后用记录分隔符 `\u{1e}` 连接。**对任意内容可逆**——金融系统日志
/// 可能含二进制错误码/协议帧/NUL/换行，所以分隔符与转义符在内容里出现时都被转义（NUL 不再是特殊字符）。
/// 按 `char` 处理，多字节 UTF-8（中文）安全。空 logs → None（不占列）。
/// （比真正的 list<utf8> 列省事，且对当前一段一文件的布局够用；要列内按元素下推再升级 list。）
const LOG_SEP: char = '\u{1e}';
const LOG_ESC: char = '\\';

/// 把一条 span 的 logs 编码成单列字符串；空 → None。
fn encode_logs(logs: &[String]) -> Option<String> {
    if logs.is_empty() {
        return None;
    }
    let mut s = String::new();
    for (i, l) in logs.iter().enumerate() {
        if i > 0 {
            s.push(LOG_SEP);
        }
        for c in l.chars() {
            if c == LOG_ESC || c == LOG_SEP {
                s.push(LOG_ESC); // 内容里的分隔符/转义符 → 转义,解码时还原
            }
            s.push(c);
        }
    }
    Some(s)
}

/// 解码单列字符串回 logs（与 `encode_logs` 互逆）。
fn decode_logs(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut esc = false;
    for c in s.chars() {
        if esc {
            cur.push(c);
            esc = false;
        } else if c == LOG_ESC {
            esc = true;
        } else if c == LOG_SEP {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    out.push(cur);
    out
}

/// 投影 → 要 `select` 的列名。身份/分组列（trace_id/span_id/ts/seq/event_type/ext_span_id）**恒选**
/// （折叠去重/定序/分组要用）；可折叠值列按 `proj` 的位选。`proj.is_all()` → `None` = 不裁列、读全表
/// （与历史行为字节一致）。**投影下推的省点全在这**：聚合不选 input_text/output_text，Vortex 连解码都不做。
fn projected_field_names(proj: Projection) -> Option<Vec<&'static str>> {
    if proj.is_all() {
        return None;
    }
    let mut cols = vec!["trace_id", "span_id", "ts", "seq", "event_type", "ext_span_id"];
    for (bit, name) in [
        (Projection::STATUS, "status"),
        (Projection::DURATION_NS, "duration_ns"),
        (Projection::PARENT_SPAN_ID, "parent_span_id"),
        (Projection::INPUT_TOKENS, "input_tokens"),
        (Projection::OUTPUT_TOKENS, "output_tokens"),
        (Projection::SESSION_ID, "session_id"),
        (Projection::TENANT_ID, "tenant_id"),
        (Projection::AGENT_NAME, "agent_name"),
        (Projection::TOOL_NAME, "tool_name"),
        (Projection::MODEL, "model"),
        (Projection::INPUT_TEXT, "input_text"),
        (Projection::OUTPUT_TEXT, "output_text"),
        (Projection::EVAL_SCORE, "eval_score"),
        (Projection::EVAL_LABEL, "eval_label"),
        (Projection::LOGS, "logs"),
    ] {
        if proj.has(bit) {
            cols.push(name);
        }
    }
    Some(cols)
}

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
        let tenant_id = u64col!(|r: &WalRecord| r.fields.tenant_id);
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
        let logs = strcol(&|r| encode_logs(&r.fields.logs));

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
            ("tenant_id", tenant_id),
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

    /// 从读回的 Arrow StructArray 逐行重建 WalRecord。**投影感知**：身份/分组列恒在；可折叠值列可能因
    /// 投影被裁掉（`column_by_name` 返回 None）→ 该字段整列当 None，不 panic。这样同一段读回路径既服务
    /// 全列读、也服务投影读。
    fn rows_from_arrow(st: &arrow::array::StructArray) -> Vec<WalRecord> {
        let n = st.len();
        // 身份/分组列：任何投影都选了它们，恒在 → 直接取。
        let u64req = |name: &str| st.column_by_name(name).unwrap().as_any().downcast_ref::<UInt64Array>().unwrap().clone();
        let trace_id = u64req("trace_id");
        let span_id = u64req("span_id");
        let ts = st.column_by_name("ts").unwrap().as_any().downcast_ref::<Int64Array>().unwrap().clone();
        let seq = u64req("seq");
        let event_type = st.column_by_name("event_type").unwrap().as_any().downcast_ref::<UInt8Array>().unwrap().clone();
        let ext_span_id = st.column_by_name("ext_span_id").unwrap().as_string_view().clone();

        // 可折叠值列：可能被投影裁掉 → Option<列>，缺列即全行 None。
        let optu64 = |name: &str| st.column_by_name(name).map(|c| c.as_any().downcast_ref::<UInt64Array>().unwrap().clone());
        let duration_ns = optu64("duration_ns");
        let parent_span_id = optu64("parent_span_id");
        let input_tokens = optu64("input_tokens");
        let output_tokens = optu64("output_tokens");
        let session_id = optu64("session_id");
        let tenant_id = optu64("tenant_id");
        let status = st.column_by_name("status").map(|c| c.as_any().downcast_ref::<UInt8Array>().unwrap().clone());
        let eval_score = st.column_by_name("eval_score").map(|c| c.as_any().downcast_ref::<UInt32Array>().unwrap().clone());
        let optsv = |name: &str| st.column_by_name(name).map(|c| c.as_string_view().clone());
        let agent_name = optsv("agent_name");
        let tool_name = optsv("tool_name");
        let model = optsv("model");
        let input_text = optsv("input_text");
        let output_text = optsv("output_text");
        let eval_label = optsv("eval_label");
        let logs = optsv("logs");

        // 缺列 → None；在列但该行为 null → None；否则取值。
        let gu64 = |a: &Option<UInt64Array>, i: usize| a.as_ref().filter(|x| !x.is_null(i)).map(|x| x.value(i));
        let gu8 = |a: &Option<UInt8Array>, i: usize| a.as_ref().filter(|x| !x.is_null(i)).map(|x| x.value(i));
        let gu32 = |a: &Option<UInt32Array>, i: usize| a.as_ref().filter(|x| !x.is_null(i)).map(|x| x.value(i));
        let gstr = |a: &Option<StringViewArray>, i: usize| a.as_ref().filter(|x| !x.is_null(i)).map(|x| x.value(i).to_string());

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
                    status: gu8(&status, i),
                    duration_ns: gu64(&duration_ns, i),
                    parent_span_id: gu64(&parent_span_id, i),
                    input_tokens: gu64(&input_tokens, i),
                    output_tokens: gu64(&output_tokens, i),
                    session_id: gu64(&session_id, i),
                    tenant_id: gu64(&tenant_id, i),
                    eval_score: gu32(&eval_score, i),
                    agent_name: gstr(&agent_name, i),
                    tool_name: gstr(&tool_name, i),
                    model: gstr(&model, i),
                    input_text: gstr(&input_text, i),
                    output_text: gstr(&output_text, i),
                    eval_label: gstr(&eval_label, i),
                    logs: match gstr(&logs, i) {
                        None => Vec::new(),
                        Some(s) => decode_logs(&s),
                    },
                },
            })
            .collect()
    }

    /// 读段（可选谓词下推 + 投影下推）。`filter=Some(expr)` 把过滤**推进 Vortex 文件扫描**
    /// （`scan().with_filter`），只解码命中行/块；`proj` 非全列时再 `with_projection(select(...))` 把列也裁掉，
    /// 不读的列（尤其大文本列）连解码都不做。都不在 Rust 后置全读再筛。
    fn read_filtered(&self, seg: SegmentId, filter: Option<Expression>, proj: Projection) -> Vec<WalRecord> {
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
            let scan = match projected_field_names(proj) {
                Some(cols) => scan.with_projection(select(cols, root())),
                None => scan, // 全列读,不裁
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

    /// **按 ts 范围下推过滤**（谓词进文件扫描）+ 可选投影：只返回 `ts ∈ [from, to]` 的行、只解码 `proj` 的列。
    /// 这是列式剪枝的主路 —— 读路径按时间窗只碰相关行/块、按投影只碰相关列，大段里查一小段时间不全扫、不全解。
    pub fn scan_in_time(&self, seg: SegmentId, from: i64, to: i64, proj: Projection) -> Vec<WalRecord> {
        let filter = and(gt_eq(col("ts"), lit(from)), lt_eq(col("ts"), lit(to)));
        self.read_filtered(seg, Some(filter), proj)
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
        // compaction 重建新段要全字段 → 读全列。
        self.read_filtered(seg, None, Projection::ALL)
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

    /// 覆盖默认（None）：**投影下推**——只解码 `proj` 的列，不丢行 → 带物理行号返回（删除位图照常生效）。
    /// 行号 = 段内顺序；投影只裁列、行顺序不变，所以 enumerate 出来的行号与全列读一致。
    fn scan_fold_inputs_projected(&self, seg: SegmentId, proj: Projection) -> Option<Vec<(u32, FoldInput)>> {
        Some(
            self.read_filtered(seg, None, proj)
                .iter()
                .enumerate()
                .map(|(i, r)| (i as u32, r.to_fold_input()))
                .collect(),
        )
    }

    /// 覆盖默认（None）：把时间过滤 + 投影**真下推**进 Vortex 文件扫描，返回命中行的 FoldInput。
    /// 引擎只在「段无删除」时调它（见 trait 文档），故这里不管删除位图。
    fn scan_fold_inputs_in_time(&self, seg: SegmentId, from: i64, to: i64, proj: Projection) -> Option<Vec<FoldInput>> {
        Some(self.scan_in_time(seg, from, to, proj).iter().map(|r| r.to_fold_input()).collect())
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
        let hit = store.scan_in_time(seg, 200, 400, Projection::ALL);
        let ts: Vec<i64> = hit.iter().map(|r| r.ts).collect();
        assert_eq!(ts, vec![200, 300, 400], "下推过滤只返回时间窗内的行");

        // 窗口外 → 空
        assert!(store.scan_in_time(seg, 1000, 2000, Projection::ALL).is_empty());
        // 单点窗口
        let one = store.scan_in_time(seg, 300, 300, Projection::ALL);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].ts, 300);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn logs_encoding_survives_separator_nul_and_cjk() {
        // logs 编码对任意内容可逆：含分隔符/转义符本身、NUL、二进制、换行、中文都 round-trip。
        let cases: Vec<Vec<String>> = vec![
            vec![],
            vec!["".into()],
            vec!["开始".into(), "研判".into()],
            vec!["含分隔符\u{1e}和转义符\\的日志".into()],
            vec!["二进制\u{0}错误码\u{0}帧".into()], // NUL —— 老的 NUL 连接会在这切坏
            vec!["多行\n日志\r\n带制表\t符".into(), "第二条".into()],
            vec!["协议帧\u{1e}\\\u{0}\u{1f}混合".into()],
        ];
        for logs in cases {
            let round = match encode_logs(&logs) {
                None => Vec::new(),
                Some(s) => decode_logs(&s),
            };
            assert_eq!(round, logs, "logs 编解码可逆: {logs:?}");
        }
    }

    #[test]
    fn logs_round_trip_through_segment_with_nul() {
        // 端到端：带 NUL 的 logs 写进列式段、读回一致（不是只测内存编解码）。
        let dir = temp_dir();
        let store = VortexSegmentStore::open(&dir).unwrap();
        let seg = SegmentId::new(11);
        let mut a = rec(1, 10, 1);
        a.fields.logs = vec!["帧\u{0}头".into(), "正常日志".into()];
        store.flush_to_segment(seg, &[a]);
        let back = store.scan_records(seg);
        assert_eq!(back[0].fields.logs, vec!["帧\u{0}头", "正常日志"], "含 NUL 的 logs 过段不丢不错切");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn segment_compresses_repetitive_text() {
        // Vortex 默认写策略是 BtrBlocks（字符串走 FSST/dict）—— 高度重复的大文本列应被压到远小于原文。
        // 这条同时是"压缩确实开着"的回归守卫（若未来误关压缩,文件会暴涨,这里会失败）。
        // ⚠️ 阈值 1/5 是按**当前默认压缩策略（BtrBlocks + FSST）对高度重复文本**定的经验值,不是协议保证。
        //    若升级 Vortex 后默认策略变了（或我们改用 with_strategy 自定义压缩器）,这个硬阈值可能误伤,
        //    需同步重测一组真实样本再调——它守的是"压缩没被关掉",不是某个固定压缩比。
        let dir = temp_dir();
        let store = VortexSegmentStore::open(&dir).unwrap();
        let seg = SegmentId::new(12);
        let big = "疑似盗刷,建议拦截并人工复核。".repeat(50); // 单行约 1.5KB
        let raw_per_row = big.len();
        let rows: Vec<WalRecord> = (1..=200)
            .map(|i| {
                let mut r = rec(1, i, i);
                r.fields.output_text = Some(big.clone());
                r
            })
            .collect();
        store.flush_to_segment(seg, &rows);
        let file = store.seg_path(seg);
        let on_disk = std::fs::metadata(&file).unwrap().len() as usize;
        let raw_total = raw_per_row * rows.len(); // 仅这一列的原始字节量
        assert!(
            on_disk < raw_total / 5,
            "高度重复文本应被压到原文的 1/5 以下：盘上 {on_disk} vs 原文 {raw_total}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn projection_reads_only_selected_columns() {
        // 投影下推:只 select 命中列,被裁掉的列(尤其大文本)读回即 None,身份/选中列照常。
        let dir = temp_dir();
        let store = VortexSegmentStore::open(&dir).unwrap();
        let seg = SegmentId::new(7);

        let mut a = rec(1, 10, 1);
        a.fields.agent_name = Some("风控".into());
        a.fields.input_tokens = Some(100);
        a.fields.output_tokens = Some(20);
        a.fields.input_text = Some("很长的提示词……".into());
        a.fields.output_text = Some("很长的回答正文……".into());
        a.fields.logs = vec!["开始".into()];
        store.flush_to_segment(seg, &[a]);

        // 窄投影:只要 agent + token(成本下钻的列)。
        let proj = Projection::of(Projection::AGENT_NAME | Projection::INPUT_TOKENS | Projection::OUTPUT_TOKENS);
        let folds = store.scan_fold_inputs_projected(seg, proj).unwrap();
        assert_eq!(folds.len(), 1);
        assert_eq!(folds[0].0, 0, "投影不丢行 → 物理行号完整");
        let f = &folds[0].1;
        // 身份恒在
        assert_eq!(f.trace_id, 1);
        assert_eq!(f.identity.ext_span_id, "1-10");
        // 选中列读得到
        assert_eq!(f.fields.agent_name.as_deref(), Some("风控"));
        assert_eq!(f.fields.input_tokens, Some(100));
        assert_eq!(f.fields.output_tokens, Some(20));
        // 未选列(被裁掉)读回 None —— 大文本列连解码都没做
        assert_eq!(f.fields.input_text, None, "投影外的大文本列不读 → None");
        assert_eq!(f.fields.output_text, None, "投影外的大文本列不读 → None");
        assert!(f.fields.logs.is_empty(), "投影外的 logs 列不读 → 空");

        // 对照:全列读回原文都在。
        let all = store.scan_records(seg);
        assert_eq!(all[0].fields.output_text.as_deref(), Some("很长的回答正文……"));

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
        let (hit, _) = wc.read_spans_query(&snap, &TraceQuery { trace_id: None, time_from: 150, time_to: 250, tenant_id: None });
        assert_eq!(hit.len(), 1, "Vortex 下推穿过引擎读路径,行级时间过滤");
        assert_eq!(hit[0].span_id, 2);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
