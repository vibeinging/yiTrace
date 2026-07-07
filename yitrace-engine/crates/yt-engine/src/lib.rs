//! yt-engine —— 把各层串成一台引擎，并定义外部件的接口边界。
//!
//! 落地的设计：
//! - **单写者**：所有改动 manifest 的提交（flush / compaction / delete / upgrade）都过同一把
//!   `WriteCoordinator` 锁串行。这样没有写-写竞争，难点只剩「1 写者 vs N 读者」（由 yt-manifest 处理）。
//! - **段五态生命周期**（草案 1 §D1.2）：building → sealed → live → compacting → dead。
//! - **三块外部件的接口边界**：列式段存储（Vortex）、BM25 中文倒排、graph_index 向量。
//!   这三块在决策文档里是「FFI 复用算法 / 重写存储」的对象，这里只立 trait，
//!   真实实现分别接 Vortex、团队 BM25(cppjieba+倒排)、团队 graph_index。
//! - **四源折叠读算子** `MergeOnReadExec` 的骨架：在固定快照上跨 memtable+段+deletion+upgrade
//!   归并，去重键 = 确定性 event_id。真实实现是 DataFusion 的 `ExecutionPlan`。
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use yt_core::chunk::{DeletionVec, UpgradeColChunk};
use yt_core::event::{EventIdentity, EventType};
use yt_core::fold::{fold_events, FoldInput, FoldedSpan, SpanFields};
use yt_core::ids::{SegmentId, WalLsn};
use yt_core::manifest::{Manifest, SegState, SegmentEntry};
use yt_core::rank::rrf_fuse;
use yt_manifest::{Current, Snapshot};
use yt_memtable::{MemRow, MemTable};
use yt_wal::{Wal, WalRecord};

mod wire;
pub use wire::parse_wire_batch;

mod otlp;
pub use otlp::parse_otlp_traces;

mod graph;
pub use graph::GraphAnnIndex;

mod bm25;
pub use bm25::{Bm25TextIndex, CjkBigramTokenizer, Tokenizer};

mod tokenizer_cn;
pub use tokenizer_cn::{ChineseTokenizer, Dict};

mod segstore;
pub use segstore::FileSegmentStore;

mod metadata;
mod persist;
pub use metadata::{
    AnnotationStatus, AnnotationTarget, DatasetAssociation, DatasetAssociationFilter,
    GoldenPathCandidate, GoldenPathFilter, GoldenPathStatus, NewDatasetAssociation,
    NewGoldenPathCandidate, NewRetentionAuditRecord, NewRetentionPolicy, NewTraceAnnotation,
    RetentionAuditFilter, RetentionAuditRecord, RetentionPolicy, RetentionPolicyFilter,
    TraceAnnotation, TraceAnnotationFilter, UpdateTraceAnnotation,
};

mod vecstore;

mod gc_log;

pub mod olog;

mod vecindex_disk;
pub use vecindex_disk::{DiskGraphConfig, DiskGraphIndex, DiskGraphStore, DurableGraphIndex};

mod http;
pub use http::{
    EngineJsonApi, HttpIngestServer, InProcessReplicaSpec, InProcessShardSpec, RemoteGatewayServer,
    RemoteShardClient, RemoteShardGateway, RemoteShardRoute, RemoteShardRouteRole,
    RemoteShardRouteTable, ShardId, StorageMode,
};

/// 编译期嵌入的控制台静态资源（build.rs 生成；console_dist/ 不存在则为空表）。
pub mod assets {
    include!(concat!(env!("OUT_DIR"), "/assets.rs"));
}

pub mod evalkit;

// ───────────────────────── 段生命周期 ─────────────────────────

/// 段五态（草案 1 §D1.2）。building/sealed 不进 manifest；dead 已从 manifest 移除、等回收。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegLifecycle {
    Building,
    Sealed,
    Live,
    Compacting,
    Dead,
}

// ───────────────────────── 外部件接口边界 ─────────────────────────

/// **折叠列投影**：聚合/列表类查询声明它要读哪些**可折叠值列**。
///
/// 身份与分组列（trace_id/span_id/ts/seq/event_type/ext_span_id）**恒读**——折叠去重、组内定序、
/// 分组都要用，不在投影里。投影只挑可折叠值列，主要价值是让**列式段（Vortex）跳过不读的列**，
/// 尤其两个大文本列 `input_text`/`output_text`（多数聚合/成本/会话查询根本不碰原文）。
///
/// 行式/内存源忽略投影（数据本就全在手边、没有列 I/O 可省）；只有列式段从中受益。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Projection(u32);

impl Projection {
    pub const STATUS: u32 = 1 << 0;
    pub const DURATION_NS: u32 = 1 << 1;
    pub const PARENT_SPAN_ID: u32 = 1 << 2;
    pub const INPUT_TOKENS: u32 = 1 << 3;
    pub const OUTPUT_TOKENS: u32 = 1 << 4;
    pub const SESSION_ID: u32 = 1 << 5;
    pub const AGENT_NAME: u32 = 1 << 6;
    pub const TOOL_NAME: u32 = 1 << 7;
    pub const MODEL: u32 = 1 << 8;
    pub const INPUT_TEXT: u32 = 1 << 9;
    pub const OUTPUT_TEXT: u32 = 1 << 10;
    pub const EVAL_SCORE: u32 = 1 << 11;
    pub const EVAL_LABEL: u32 = 1 << 12;
    pub const LOGS: u32 = 1 << 13;
    pub const TENANT_ID: u32 = 1 << 14;
    pub const EXTERNAL_IDS: u32 = 1 << 15;
    pub const ATTRS: u32 = 1 << 16;
    pub const AGENTIC_FIELDS: u32 = 1 << 17;
    pub const USAGE_COST: u32 = 1 << 18;

    const MASK: u32 = (1 << 19) - 1;

    /// 全列（含两个大文本列）。普通读 / trace 详情 / eval 打分 / 数据集采集要原文，用这个。
    pub const ALL: Projection = Projection(Self::MASK);

    /// 选定若干列（位或）。如 `Projection::of(Projection::AGENT_NAME | Projection::INPUT_TOKENS)`。
    pub const fn of(cols: u32) -> Self {
        Projection(cols & Self::MASK)
    }

    /// 该投影是否要某列（传列常量）。
    pub const fn has(self, col: u32) -> bool {
        self.0 & col != 0
    }

    /// 是否要全部列——存储据此走"读全列"快路（与历史行为完全一致），不必逐列裁剪。
    pub const fn is_all(self) -> bool {
        self.0 == Self::MASK
    }

    /// 原始位（列式存储据此判断每列读不读）。
    pub const fn bits(self) -> u32 {
        self.0
    }
}

pub(crate) fn project_span_fields(fields: &SpanFields, proj: Projection) -> SpanFields {
    if proj.is_all() {
        return fields.clone();
    }
    let mut out = SpanFields::default();
    if proj.has(Projection::STATUS) {
        out.status = fields.status;
    }
    if proj.has(Projection::DURATION_NS) {
        out.duration_ns = fields.duration_ns;
    }
    if proj.has(Projection::PARENT_SPAN_ID) {
        out.parent_span_id = fields.parent_span_id;
    }
    if proj.has(Projection::INPUT_TOKENS) {
        out.input_tokens = fields.input_tokens;
    }
    if proj.has(Projection::OUTPUT_TOKENS) {
        out.output_tokens = fields.output_tokens;
    }
    if proj.has(Projection::USAGE_COST) {
        out.cached_input_tokens = fields.cached_input_tokens;
        out.reasoning_tokens = fields.reasoning_tokens;
        out.total_tokens = fields.total_tokens;
        out.cost_usd_nanos = fields.cost_usd_nanos;
        out.cost_currency = fields.cost_currency.clone();
        out.provider = fields.provider.clone();
    }
    if proj.has(Projection::SESSION_ID) {
        out.session_id = fields.session_id;
    }
    if proj.has(Projection::TENANT_ID) {
        out.tenant_id = fields.tenant_id;
    }
    if proj.has(Projection::EXTERNAL_IDS) {
        out.external_trace_id = fields.external_trace_id.clone();
        out.external_span_id = fields.external_span_id.clone();
        out.external_parent_span_id = fields.external_parent_span_id.clone();
        out.external_session_id = fields.external_session_id.clone();
    }
    if proj.has(Projection::AGENTIC_FIELDS) {
        out.project_id = fields.project_id.clone();
        out.skill = fields.skill.clone();
        out.mode = fields.mode.clone();
        out.call_site = fields.call_site.clone();
        out.task_fingerprint = fields.task_fingerprint.clone();
        out.loop_id = fields.loop_id.clone();
        out.harness_version = fields.harness_version.clone();
        out.schema_fingerprint = fields.schema_fingerprint.clone();
        out.intent_signature = fields.intent_signature.clone();
        out.validation_status = fields.validation_status.clone();
        out.review_status = fields.review_status.clone();
        out.eval_status = fields.eval_status.clone();
        out.path_memory_id = fields.path_memory_id.clone();
        out.stop_reason = fields.stop_reason.clone();
        out.phase = fields.phase.clone();
        out.validator = fields.validator.clone();
    }
    if proj.has(Projection::AGENT_NAME) {
        out.agent_name = fields.agent_name.clone();
    }
    if proj.has(Projection::TOOL_NAME) {
        out.tool_name = fields.tool_name.clone();
    }
    if proj.has(Projection::MODEL) {
        out.model = fields.model.clone();
    }
    if proj.has(Projection::INPUT_TEXT) {
        out.input_text = fields.input_text.clone();
    }
    if proj.has(Projection::OUTPUT_TEXT) {
        out.output_text = fields.output_text.clone();
    }
    if proj.has(Projection::EVAL_SCORE) {
        out.eval_score = fields.eval_score;
    }
    if proj.has(Projection::EVAL_LABEL) {
        out.eval_label = fields.eval_label.clone();
    }
    if proj.has(Projection::LOGS) {
        out.logs = fields.logs.clone();
    }
    if proj.has(Projection::ATTRS) {
        out.attrs = fields.attrs.clone();
    }
    out
}

/// 列式不可变段存储。真实实现接 **Vortex**（layouts + zone-map + 统计）；
/// 删除/manifest/版本不归它管（那是本引擎自己的事，见 yt-core::manifest）。
pub trait SegmentStore: Send + Sync {
    /// 把一批已 ack 事件写成段 `seg`（building→sealed）。
    /// seg 由协调器分配（单写者、全局唯一、永不复用），不由存储自选。
    fn flush_to_segment(&self, seg: SegmentId, records: &[WalRecord]);
    /// 扫一个段，返回 (段内行号, 折叠输入)。读路径据行号查 deletion_vec 跳过已删行。
    /// 真实实现是 Vortex 段扫描 + 谓词/zone 剪枝下推；这里是接口边界。
    fn scan_fold_inputs(&self, seg: SegmentId) -> Vec<(u32, FoldInput)>;
    /// 扫一个段的原始记录（compaction 重建新段用）。
    fn scan_records(&self, seg: SegmentId) -> Vec<WalRecord>;
    /// 物理删除一个 dead 段文件（仅在 §D1.4 三条水位放行后调用）。
    fn unlink_segment(&self, seg: SegmentId);

    /// 可选：**投影扫描**，只解码 `proj` 选中的可折叠值列（身份/分组列恒读），返回**带物理行号**的
    /// `FoldInput`。投影只裁列、不丢行，故行号完整、与删除位图共存安全——**任何查询都能用**。
    /// 默认 `None` = 不支持，引擎回退 `scan_fold_inputs` 读全列。列式存储（Vortex）覆盖它，让聚合/列表
    /// 查询跳过不读的大文本列（上列式最大的单点收益）。
    fn scan_fold_inputs_projected(
        &self,
        _seg: SegmentId,
        _proj: Projection,
    ) -> Option<Vec<(u32, FoldInput)>> {
        None
    }

    /// 可选：**按时间范围下推扫描 + 投影**，返回 `ts ∈ [from, to]` 命中行的 `FoldInput`（不带物理行号），
    /// 只解码 `proj` 选中的列。默认 `None` = 不支持下推，引擎回退全扫。列式存储（Vortex）覆盖它，把时间
    /// 过滤推进文件扫描、只解码命中行的命中列。
    /// **注意**：下推丢了物理行号，而删除按物理行号定位，二者不能共存——引擎只在「段无删除」时用它。
    fn scan_fold_inputs_in_time(
        &self,
        _seg: SegmentId,
        _from: i64,
        _to: i64,
        _proj: Projection,
    ) -> Option<Vec<FoldInput>> {
        None
    }
}

/// dead_set 里的一个待回收资源（草案 1 §D1.4）。
/// 目前只建段；deletion / upgrade 块同理共用此水位（留扩展）。
struct DeadResource {
    seg: SegmentId,
    /// 该资源变 dead 的 manifest 版本号。
    v_dead: u64,
}

/// compaction 计划：选了哪些输入段 + 选段瞬间各段的 (deletion_seq, upgrade_seq)。
/// `compaction_finish` 据此判断选段后是否有并发删除/补写打进来（OPEN-3）。
pub struct CompactionPlan {
    inputs: Vec<SegmentId>,
    seqs_at_select: HashMap<u64, (u64, u64)>,
}

/// 段文件的 buffer pin 计数（GC 安全条件 (2)：字节级最后保险）。
/// 真实实现复用 vector_smgr 的 pin/release；这里用计数表骨架。
#[derive(Default)]
struct BufferPins {
    counts: Mutex<HashMap<u64, u32>>,
}
impl BufferPins {
    fn pin(&self, seg: SegmentId) {
        *self.counts.lock().unwrap().entry(seg.get()).or_insert(0) += 1;
    }
    fn unpin(&self, seg: SegmentId) {
        let mut g = self.counts.lock().unwrap();
        if let Some(c) = g.get_mut(&seg.get()) {
            *c = c.saturating_sub(1);
            if *c == 0 {
                g.remove(&seg.get());
            }
        }
    }
    fn is_pinned(&self, seg: SegmentId) -> bool {
        self.counts
            .lock()
            .unwrap()
            .get(&seg.get())
            .map_or(false, |&c| c > 0)
    }
}

/// BM25 中文倒排。真实实现 = 团队自有 BM25（cppjieba 分词 FFI + Rust 重写的倒排 + block-max-WAND）。
/// 这是「FFI 复用评分/分词、重写存储」的落点（决策文档 §2.1）。接口按 span 维度（检索返回的是 trace/span）。
pub trait Bm25Index: Send + Sync {
    /// 把某 span 的文本喂进倒排（ingest/flush 时调用）。真实实现走 jieba 分词 + 段内倒排。
    fn index_text(&self, trace_id: u64, span_id: u64, text: &str);
    /// 中文检索，返回 (trace_id, span_id, 评分)，按分降序、取前 k。
    /// 真实实现作为 DataFusion 自定义扫描节点下推（@~@ + LIMIT）。
    fn search(&self, query: &str, k: usize) -> Vec<(u64, u64, f32)>;
}

/// graph_index 向量 ANN。真实实现 = 团队自有图索引（algorithm/distance/PQ 经 C ABI FFI 复用）。
/// 「带过滤 ANN」目前是半成品（PoC C 要验进图过滤能否把召回拉回来），这里把 filter 作为一等参数。
pub trait GraphIndex: Send + Sync {
    /// 给某 span 建/更新向量（向量由外部 embedder 算，不是每个 span 都有）。
    fn index_embedding(&self, trace_id: u64, span_id: u64, embedding: Vec<f32>);
    /// 带过滤的近邻搜索：`filter` 是下推进图搜索的谓词（service/time/status…）。
    /// 返回 (trace_id, span_id, 距离)，按距离升序、取前 k。真实实现把 filter 接进 search_layer 的导航。
    fn search(
        &self,
        query: &[f32],
        k: usize,
        filter: &dyn Fn(u64, u64) -> bool,
    ) -> Vec<(u64, u64, f32)>;
    /// 落盘点（提交时调）：插入只写不刷的实现（如磁盘索引）在此批量 fsync。内存实现默认空操作。
    /// 我们的场景 **append 极多、删除少** —— 插入走"只写不刷"，靠这里在提交点批量持久，吞吐才扛得住。
    fn flush(&self) {}
}

/// 朴素内存 BM25 骨架：按 span 存文本，检索按「查询子串命中数」打分。
/// 真实实现换成团队自有 BM25（jieba 词级分词 + block-max-WAND 评分）。这里只为把检索路径打通可测。
#[derive(Default)]
pub struct InMemoryBm25 {
    docs: Mutex<BTreeMap<(u64, u64), String>>,
}
impl Bm25Index for InMemoryBm25 {
    fn index_text(&self, trace_id: u64, span_id: u64, text: &str) {
        let mut g = self.docs.lock().unwrap();
        let doc = g.entry((trace_id, span_id)).or_default();
        doc.push_str(text);
        doc.push(' ');
    }
    fn search(&self, query: &str, k: usize) -> Vec<(u64, u64, f32)> {
        // 朴素：每个查询词（空白切）在文档里出现就 +1 分。中文用子串命中（真实实现是 jieba 词级）。
        let qtokens: Vec<&str> = query.split_whitespace().collect();
        let g = self.docs.lock().unwrap();
        let mut scored: Vec<(u64, u64, f32)> = g
            .iter()
            .filter_map(|(&(t, s), text)| {
                let score = qtokens.iter().filter(|q| text.contains(**q)).count() as f32;
                (score > 0.0).then_some((t, s, score))
            })
            .collect();
        scored.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap());
        scored.truncate(k);
        scored
    }
}

/// 朴素内存向量索引骨架：暴力 L2 距离。真实实现换团队 graph_index（图式 ANN + 带过滤导航）。
#[derive(Default)]
pub struct InMemoryGraphIndex {
    vecs: Mutex<BTreeMap<(u64, u64), Vec<f32>>>,
}
impl GraphIndex for InMemoryGraphIndex {
    fn index_embedding(&self, trace_id: u64, span_id: u64, embedding: Vec<f32>) {
        self.vecs
            .lock()
            .unwrap()
            .insert((trace_id, span_id), embedding);
    }
    fn search(
        &self,
        query: &[f32],
        k: usize,
        filter: &dyn Fn(u64, u64) -> bool,
    ) -> Vec<(u64, u64, f32)> {
        let g = self.vecs.lock().unwrap();
        let mut scored: Vec<(u64, u64, f32)> = g
            .iter()
            .filter(|(&(t, s), _)| filter(t, s))
            .map(|(&(t, s), v)| (t, s, l2_distance(query, v)))
            .collect();
        scored.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap());
        scored.truncate(k);
        scored
    }
}

fn l2_distance(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y) * (x - y))
        .sum::<f32>()
        .sqrt()
}

/// 内存段存储（默认实现 / demo / 测试用）。真实实现换 Vortex 列式不可变段。
/// `unlink` 真删 —— 配合回收水位，过早回收会让读者读到空（被压测当场抓住）。
#[derive(Default)]
pub struct InMemorySegmentStore {
    rows: Mutex<BTreeMap<u64, Vec<WalRecord>>>,
}
impl SegmentStore for InMemorySegmentStore {
    fn flush_to_segment(&self, seg: SegmentId, records: &[WalRecord]) {
        self.rows
            .lock()
            .unwrap()
            .insert(seg.get(), records.to_vec());
    }
    fn scan_fold_inputs(&self, seg: SegmentId) -> Vec<(u32, FoldInput)> {
        self.rows
            .lock()
            .unwrap()
            .get(&seg.get())
            .map(|rs| {
                rs.iter()
                    .enumerate()
                    .map(|(i, r)| (i as u32, r.to_fold_input()))
                    .collect()
            })
            .unwrap_or_default()
    }
    fn scan_fold_inputs_projected(
        &self,
        seg: SegmentId,
        proj: Projection,
    ) -> Option<Vec<(u32, FoldInput)>> {
        Some(
            self.rows
                .lock()
                .unwrap()
                .get(&seg.get())
                .map(|rs| {
                    rs.iter()
                        .enumerate()
                        .map(|(i, r)| {
                            (
                                i as u32,
                                FoldInput {
                                    trace_id: r.trace_id,
                                    span_id: r.span_id,
                                    identity: r.identity.clone(),
                                    fields: project_span_fields(&r.fields, proj),
                                },
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
        )
    }
    fn scan_fold_inputs_in_time(
        &self,
        seg: SegmentId,
        from: i64,
        to: i64,
        proj: Projection,
    ) -> Option<Vec<FoldInput>> {
        Some(
            self.rows
                .lock()
                .unwrap()
                .get(&seg.get())
                .map(|rs| {
                    rs.iter()
                        .filter(|r| r.ts >= from && r.ts <= to)
                        .map(|r| FoldInput {
                            trace_id: r.trace_id,
                            span_id: r.span_id,
                            identity: r.identity.clone(),
                            fields: project_span_fields(&r.fields, proj),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        )
    }
    fn scan_records(&self, seg: SegmentId) -> Vec<WalRecord> {
        self.rows
            .lock()
            .unwrap()
            .get(&seg.get())
            .cloned()
            .unwrap_or_default()
    }
    fn unlink_segment(&self, seg: SegmentId) {
        self.rows.lock().unwrap().remove(&seg.get());
    }
}

/// 一批记录的时间范围（zone-map）。空批返回 (0, 0)。
fn ts_range(records: &[WalRecord]) -> (i64, i64) {
    let mut it = records.iter().map(|r| r.ts);
    match it.next() {
        None => (0, 0),
        Some(first) => it.fold((first, first), |(lo, hi), t| (lo.min(t), hi.max(t))),
    }
}

/// 读一条/一批 trace 的查询条件。时间窗 + 可选 trace_id。
pub struct TraceQuery {
    /// None = 所有 trace。
    pub trace_id: Option<u64>,
    /// 时间窗 [from, to]（闭区间）。
    pub time_from: i64,
    pub time_to: i64,
    /// **租户隔离**：设了它，只读该租户的 span。服务层须按鉴权身份注入（与检索路径一致）。
    pub tenant_id: Option<u64>,
}

impl TraceQuery {
    /// 全开窗、所有 trace（等价于不剪枝）。
    pub fn all() -> Self {
        Self {
            trace_id: None,
            time_from: i64::MIN,
            time_to: i64::MAX,
            tenant_id: None,
        }
    }
    pub fn trace(trace_id: u64, time_from: i64, time_to: i64) -> Self {
        Self {
            trace_id: Some(trace_id),
            time_from,
            time_to,
            tenant_id: None,
        }
    }
    /// 限定租户（链式）。
    pub fn for_tenant(mut self, tenant_id: u64) -> Self {
        self.tenant_id = Some(tenant_id);
        self
    }
}

/// 一个 span 的**可过滤元数据**（带过滤 ANN 的 payload）。摄入时按 last-non-null 累积、ts 取范围。
/// 让向量检索能按真实查询维度（agent / 状态 / 时间）过滤，而不只按 (trace,span) id。
#[derive(Clone, Debug, Default)]
struct FilterAttrs {
    status: Option<u8>,
    agent_name: Option<String>,
    attrs: BTreeMap<String, String>,
    min_ts: i64,
    max_ts: i64,
    /// 租户隔离维度（last-non-null）。
    tenant_id: Option<u64>,
}

type SpanKey = (u64, u64);

#[derive(Clone)]
enum PostingList {
    One(SpanKey),
    Small(Vec<SpanKey>),
    Many(HashSet<SpanKey>),
}

impl PostingList {
    fn from_keys(mut keys: Vec<SpanKey>) -> Option<Self> {
        keys.sort_unstable();
        keys.dedup();
        match keys.len() {
            0 => None,
            1 => Some(PostingList::One(keys[0])),
            n if n <= ATTR_POSTING_SMALL_VEC_MAX => Some(PostingList::Small(keys)),
            _ => Some(PostingList::Many(keys.into_iter().collect())),
        }
    }

    fn contains(&self, key: SpanKey) -> bool {
        match self {
            PostingList::One(one) => *one == key,
            PostingList::Small(keys) => keys.binary_search(&key).is_ok(),
            PostingList::Many(keys) => keys.contains(&key),
        }
    }

    fn len(&self) -> usize {
        match self {
            PostingList::One(_) => 1,
            PostingList::Small(keys) => keys.len(),
            PostingList::Many(keys) => keys.len(),
        }
    }

    fn is_empty(&self) -> bool {
        match self {
            PostingList::One(_) => false,
            PostingList::Small(keys) => keys.is_empty(),
            PostingList::Many(keys) => keys.is_empty(),
        }
    }

    fn is_singleton(&self) -> bool {
        matches!(self, PostingList::One(_))
    }

    fn is_small(&self) -> bool {
        matches!(self, PostingList::Small(_))
    }

    fn is_hashset(&self) -> bool {
        matches!(self, PostingList::Many(_))
    }

    fn insert(&mut self, key: SpanKey) -> bool {
        match self {
            PostingList::One(one) if *one == key => false,
            PostingList::One(one) => {
                let old = *one;
                let mut keys = vec![old, key];
                keys.sort_unstable();
                *self = PostingList::Small(keys);
                true
            }
            PostingList::Small(keys) => match keys.binary_search(&key) {
                Ok(_) => false,
                Err(pos) => {
                    keys.insert(pos, key);
                    if keys.len() > ATTR_POSTING_SMALL_VEC_MAX {
                        *self = PostingList::Many(keys.iter().copied().collect());
                    }
                    true
                }
            },
            PostingList::Many(keys) => keys.insert(key),
        }
    }

    fn remove(&mut self, key: SpanKey) -> bool {
        match self {
            PostingList::One(one) if *one == key => {
                *self = PostingList::Many(HashSet::new());
                true
            }
            PostingList::One(_) => false,
            PostingList::Small(keys) => match keys.binary_search(&key) {
                Ok(pos) => {
                    keys.remove(pos);
                    if keys.len() == 1 {
                        *self = PostingList::One(keys[0]);
                    }
                    true
                }
                Err(_) => false,
            },
            PostingList::Many(keys) => {
                let removed = keys.remove(&key);
                if removed && keys.len() == 1 {
                    if let Some(one) = keys.iter().next().copied() {
                        *self = PostingList::One(one);
                    }
                } else if removed && keys.len() <= ATTR_POSTING_SMALL_VEC_MAX {
                    let mut small: Vec<SpanKey> = keys.iter().copied().collect();
                    small.sort_unstable();
                    *self = PostingList::Small(small);
                }
                removed
            }
        }
    }

    fn extend_into(&self, out: &mut HashSet<SpanKey>) {
        match self {
            PostingList::One(one) => {
                out.insert(*one);
            }
            PostingList::Small(keys) => {
                out.extend(keys.iter().copied());
            }
            PostingList::Many(keys) => {
                out.extend(keys.iter().copied());
            }
        }
    }

    fn to_hashset(&self) -> HashSet<SpanKey> {
        match self {
            PostingList::One(one) => HashSet::from([*one]),
            PostingList::Small(keys) => keys.iter().copied().collect(),
            PostingList::Many(keys) => keys.clone(),
        }
    }

    fn to_sorted_vec(&self) -> Vec<SpanKey> {
        let mut out: Vec<SpanKey> = match self {
            PostingList::One(one) => vec![*one],
            PostingList::Small(keys) => keys.clone(),
            PostingList::Many(keys) => keys.iter().copied().collect(),
        };
        out.sort_unstable();
        out
    }

    fn estimated_bytes(&self) -> usize {
        match self {
            PostingList::One(_) => {
                posting_list_estimated_bytes() + ATTR_POSTING_SINGLE_ENTRY_ESTIMATED_BYTES
            }
            PostingList::Small(keys) => posting_total_estimated_bytes_for_small(keys.len()),
            PostingList::Many(keys) => {
                posting_list_estimated_bytes()
                    + ATTR_POSTING_HASHSET_ESTIMATED_BYTES
                    + keys
                        .len()
                        .saturating_mul(ATTR_POSTING_HASHSET_ENTRY_ESTIMATED_BYTES)
            }
        }
    }
}

type AttrPostingTerm = (u32, u32);

#[derive(Default)]
struct StringInterner {
    ids: HashMap<String, u32>,
    next_id: u32,
}

impl StringInterner {
    fn get(&self, value: &str) -> Option<u32> {
        self.ids.get(value).copied()
    }

    fn len(&self) -> usize {
        self.ids.len()
    }

    fn estimated_insert_bytes(&self, value: &str) -> usize {
        if self.ids.contains_key(value) {
            0
        } else {
            interned_string_estimated_bytes(value)
        }
    }

    fn get_or_intern(&mut self, value: &str) -> Option<(u32, usize)> {
        if let Some(id) = self.get(value) {
            return Some((id, 0));
        }
        let next = self.next_id.checked_add(1)?;
        let id = self.next_id;
        self.next_id = next;
        let bytes = interned_string_estimated_bytes(value);
        self.ids.insert(value.to_string(), id);
        Some((id, bytes))
    }
}

#[derive(Default)]
struct AttrPostings {
    /// compact JSON exact postings: (key_id, value_json_id) -> span keys.
    exact: HashMap<AttrPostingTerm, PostingList>,
    /// string-array includes postings: (key_id, item_json_string_id) -> span keys.
    array_items: HashMap<AttrPostingTerm, PostingList>,
    /// postings 字符串字典。HashMap key 只保存小整数，避免每个桶重复持有 String。
    attr_keys: StringInterner,
    attr_values: StringInterner,
    /// 当前索引条目数（exact + array item posting entries）。用于给派生索引做硬预算。
    indexed_entries: usize,
    /// postings 的近似内存占用。用于保护常驻 sidecar，不等同进程 RSS。
    estimated_bytes: usize,
    /// 因预算/value/fan-out 限制被降级的 key。查询这些 key 必须走慢路径，不能用不完整 postings。
    incomplete_keys: HashSet<String>,
}

impl AttrPostings {
    fn update(&mut self, span_key: (u64, u64), attr_key: &str, old: Option<&str>, new: &str) {
        if old == Some(new) {
            return;
        }
        if let Some(old) = old {
            self.remove_value(span_key, attr_key, old);
        }
        self.add_value(span_key, attr_key, new);
    }

    fn add_value(&mut self, span_key: (u64, u64), attr_key: &str, value: &str) {
        if !self.key_is_complete(attr_key) {
            return;
        }
        if value.len() > ATTR_POSTINGS_MAX_VALUE_BYTES {
            self.mark_key_incomplete(attr_key);
            return;
        }
        let array_items = string_array_items(value);
        if array_items.len() > ATTR_POSTINGS_MAX_ARRAY_ITEMS {
            self.mark_key_incomplete(attr_key);
            return;
        }
        let needed = 1 + array_items.len();
        if self.indexed_entries.saturating_add(needed) > ATTR_POSTINGS_MAX_ENTRIES {
            self.mark_key_incomplete(attr_key);
            return;
        }
        let mut values_to_intern = Vec::with_capacity(1 + array_items.len());
        values_to_intern.push(value);
        for item in &array_items {
            values_to_intern.push(item.as_str());
        }
        let estimated_new_bytes = self.estimate_intern_bytes(attr_key, &values_to_intern)
            + estimate_posting_insert(&self.exact, self.lookup_term(attr_key, value), span_key)
            + array_items
                .iter()
                .map(|item| {
                    estimate_posting_insert(
                        &self.array_items,
                        self.lookup_term(attr_key, item),
                        span_key,
                    )
                })
                .sum::<usize>();
        if self.estimated_bytes.saturating_add(estimated_new_bytes)
            > ATTR_POSTINGS_MAX_ESTIMATED_BYTES
        {
            self.mark_key_incomplete(attr_key);
            return;
        }
        let Some(exact_term) = self.intern_term(attr_key, value) else {
            self.mark_key_incomplete(attr_key);
            return;
        };
        let mut array_terms = Vec::with_capacity(array_items.len());
        for item in &array_items {
            let Some(term) = self.intern_term(attr_key, item) else {
                self.mark_key_incomplete(attr_key);
                return;
            };
            array_terms.push(term);
        }
        insert_posting(
            &mut self.exact,
            &mut self.indexed_entries,
            &mut self.estimated_bytes,
            exact_term,
            span_key,
        );
        for term in array_terms {
            insert_posting(
                &mut self.array_items,
                &mut self.indexed_entries,
                &mut self.estimated_bytes,
                term,
                span_key,
            );
        }
    }

    fn remove_value(&mut self, span_key: (u64, u64), attr_key: &str, value: &str) {
        let exact_term = self.lookup_term(attr_key, value);
        remove_posting(
            &mut self.exact,
            &mut self.indexed_entries,
            &mut self.estimated_bytes,
            exact_term,
            span_key,
        );
        for item in string_array_items(value) {
            let item_term = self.lookup_term(attr_key, &item);
            remove_posting(
                &mut self.array_items,
                &mut self.indexed_entries,
                &mut self.estimated_bytes,
                item_term,
                span_key,
            );
        }
    }

    fn candidates_for_filters(
        &self,
        attrs: &BTreeMap<String, String>,
    ) -> Option<HashSet<(u64, u64)>> {
        let mut out: Option<HashSet<(u64, u64)>> = None;
        let mut used_index = false;
        for (attr_key, expected) in attrs {
            if !self.key_is_complete(attr_key) {
                continue;
            }
            used_index = true;
            let one = self.candidates_for_attr(attr_key, expected);
            out = Some(match out {
                None => one,
                Some(prev) => prev.intersection(&one).copied().collect(),
            });
            if out.as_ref().map_or(false, HashSet::is_empty) {
                break;
            }
        }
        if used_index {
            Some(out.unwrap_or_default())
        } else {
            None
        }
    }

    fn candidates_for_attr(&self, attr_key: &str, expected: &str) -> HashSet<(u64, u64)> {
        let mut out = HashSet::new();
        if let Some(term) = self.lookup_term(attr_key, expected) {
            if let Some(keys) = self.exact.get(&term) {
                keys.extend_into(&mut out);
            }
        }
        let Ok(expected_json) = crate::wire::parse(expected) else {
            return out;
        };
        match expected_json {
            crate::wire::Json::Str(s) => {
                let item = json_string_compact(&s);
                if let Some(term) = self.lookup_term(attr_key, &item) {
                    if let Some(keys) = self.array_items.get(&term) {
                        keys.extend_into(&mut out);
                    }
                }
            }
            crate::wire::Json::Arr(items) => {
                let expected_strings: Vec<String> = items
                    .iter()
                    .filter_map(crate::wire::Json::as_str)
                    .map(json_string_compact)
                    .collect();
                for item in &expected_strings {
                    if let Some(term) = self.lookup_term(attr_key, item) {
                        if let Some(keys) = self.exact.get(&term) {
                            keys.extend_into(&mut out);
                        }
                    }
                }
                if !expected_strings.is_empty() {
                    let mut array_match: Option<HashSet<SpanKey>> = None;
                    for item in expected_strings {
                        let keys = self
                            .lookup_term(attr_key, &item)
                            .and_then(|term| self.array_items.get(&term))
                            .map(PostingList::to_hashset)
                            .unwrap_or_default();
                        array_match = Some(match array_match {
                            None => keys,
                            Some(prev) => prev.intersection(&keys).copied().collect(),
                        });
                    }
                    if let Some(keys) = array_match {
                        out.extend(keys);
                    }
                }
            }
            _ => {}
        }
        out
    }

    fn key_is_complete(&self, attr_key: &str) -> bool {
        is_postings_attr_key(attr_key) && !self.incomplete_keys.contains(attr_key)
    }

    fn mark_key_incomplete(&mut self, attr_key: &str) {
        self.incomplete_keys.insert(attr_key.to_string());
        if let Some(attr_key_id) = self.attr_keys.get(attr_key) {
            remove_attr_key_from_postings(
                &mut self.exact,
                &mut self.indexed_entries,
                &mut self.estimated_bytes,
                attr_key_id,
            );
            remove_attr_key_from_postings(
                &mut self.array_items,
                &mut self.indexed_entries,
                &mut self.estimated_bytes,
                attr_key_id,
            );
        }
    }

    fn lookup_term(&self, attr_key: &str, attr_value: &str) -> Option<AttrPostingTerm> {
        Some((
            self.attr_keys.get(attr_key)?,
            self.attr_values.get(attr_value)?,
        ))
    }

    fn intern_term(&mut self, attr_key: &str, attr_value: &str) -> Option<AttrPostingTerm> {
        let (key_id, key_bytes) = self.attr_keys.get_or_intern(attr_key)?;
        let (value_id, value_bytes) = self.attr_values.get_or_intern(attr_value)?;
        self.estimated_bytes = self
            .estimated_bytes
            .saturating_add(key_bytes.saturating_add(value_bytes));
        Some((key_id, value_id))
    }

    fn estimate_intern_bytes(&self, attr_key: &str, attr_values: &[&str]) -> usize {
        let mut bytes = self.attr_keys.estimated_insert_bytes(attr_key);
        let mut new_values = HashSet::new();
        for attr_value in attr_values {
            if self.attr_values.get(attr_value).is_none() && new_values.insert(*attr_value) {
                bytes = bytes.saturating_add(interned_string_estimated_bytes(attr_value));
            }
        }
        bytes
    }
}

const ATTR_POSTINGS_MAX_VALUE_BYTES: usize = 256;
const ATTR_POSTINGS_MAX_ARRAY_ITEMS: usize = 32;
const ATTR_POSTINGS_MAX_ENTRIES: usize = 2_000_000;
const ATTR_POSTINGS_MAX_ESTIMATED_BYTES: usize = 256 * 1024 * 1024;
const ATTR_POSTING_LIST_ESTIMATED_BYTES: usize = 96;
const ATTR_POSTING_SINGLE_ENTRY_ESTIMATED_BYTES: usize = 16;
const ATTR_POSTING_SMALL_VEC_MAX: usize = 8;
const ATTR_POSTING_SMALL_VEC_ESTIMATED_BYTES: usize = 32;
const ATTR_POSTING_SMALL_VEC_ENTRY_ESTIMATED_BYTES: usize = 16;
const ATTR_POSTING_HASHSET_ESTIMATED_BYTES: usize = 96;
const ATTR_POSTING_HASHSET_ENTRY_ESTIMATED_BYTES: usize = 32;
const ATTR_POSTING_INTERNED_STRING_ESTIMATED_BYTES: usize = 64;

fn is_postings_attr_key(k: &str) -> bool {
    matches!(
        k,
        "project_id"
            | "skill"
            | "mode"
            | "call_site"
            | "task_fingerprint"
            | "loop_id"
            | "harness_version"
            | "validation_status"
            | "stop_reason"
            | "phase"
            | "validator"
            | "connection_ids"
            | "data_source_ids"
            | "schema_fingerprint"
            | "eval_profile"
            | "tool_version"
            | "model"
            | "provider"
            | "intent_signature"
            | "review_status"
            | "eval_status"
    )
}

fn insert_posting(
    index: &mut HashMap<AttrPostingTerm, PostingList>,
    entry_count: &mut usize,
    byte_count: &mut usize,
    term: AttrPostingTerm,
    span_key: SpanKey,
) {
    let estimated_bytes = estimate_posting_insert(index, Some(term), span_key);
    let inserted = match index.get_mut(&term) {
        Some(list) => list.insert(span_key),
        None => {
            index.insert(term, PostingList::One(span_key));
            true
        }
    };
    if inserted {
        *entry_count += 1;
        *byte_count = byte_count.saturating_add(estimated_bytes);
    }
}

fn remove_posting(
    index: &mut HashMap<AttrPostingTerm, PostingList>,
    entry_count: &mut usize,
    byte_count: &mut usize,
    term: Option<AttrPostingTerm>,
    span_key: SpanKey,
) {
    let Some(term) = term else {
        return;
    };
    let remove_empty = if let Some(list) = index.get_mut(&term) {
        let before_bytes = list.estimated_bytes();
        if list.remove(span_key) {
            *entry_count = entry_count.saturating_sub(1);
            let after_bytes = if list.is_empty() {
                0
            } else {
                list.estimated_bytes()
            };
            *byte_count = byte_count.saturating_sub(before_bytes.saturating_sub(after_bytes));
        }
        list.is_empty()
    } else {
        false
    };
    if remove_empty {
        index.remove(&term);
    }
}

fn remove_attr_key_from_postings(
    index: &mut HashMap<AttrPostingTerm, PostingList>,
    entry_count: &mut usize,
    byte_count: &mut usize,
    attr_key_id: u32,
) {
    let keys: Vec<AttrPostingTerm> = index
        .keys()
        .filter(|(k, _)| *k == attr_key_id)
        .copied()
        .collect();
    for key in keys {
        if let Some(values) = index.remove(&key) {
            *entry_count = entry_count.saturating_sub(values.len());
            *byte_count = byte_count.saturating_sub(values.estimated_bytes());
        }
    }
}

fn estimate_posting_insert(
    index: &HashMap<AttrPostingTerm, PostingList>,
    term: Option<AttrPostingTerm>,
    span_key: SpanKey,
) -> usize {
    match term.and_then(|term| index.get(&term)) {
        Some(list) if list.contains(span_key) => 0,
        Some(PostingList::One(_)) => posting_total_estimated_bytes_for_small(2).saturating_sub(
            posting_list_estimated_bytes() + ATTR_POSTING_SINGLE_ENTRY_ESTIMATED_BYTES,
        ),
        Some(PostingList::Small(keys)) if keys.len() < ATTR_POSTING_SMALL_VEC_MAX => {
            posting_total_estimated_bytes_for_small(keys.len() + 1)
                .saturating_sub(posting_total_estimated_bytes_for_small(keys.len()))
        }
        Some(PostingList::Small(keys)) => posting_total_estimated_bytes_for_many(keys.len() + 1)
            .saturating_sub(posting_total_estimated_bytes_for_small(keys.len())),
        Some(PostingList::Many(_)) => ATTR_POSTING_HASHSET_ENTRY_ESTIMATED_BYTES,
        None => posting_list_estimated_bytes() + ATTR_POSTING_SINGLE_ENTRY_ESTIMATED_BYTES,
    }
}

fn posting_list_estimated_bytes() -> usize {
    ATTR_POSTING_LIST_ESTIMATED_BYTES
}

fn posting_total_estimated_bytes_for_small(entries: usize) -> usize {
    posting_list_estimated_bytes()
        + ATTR_POSTING_SMALL_VEC_ESTIMATED_BYTES
        + entries.saturating_mul(ATTR_POSTING_SMALL_VEC_ENTRY_ESTIMATED_BYTES)
}

fn posting_total_estimated_bytes_for_many(entries: usize) -> usize {
    posting_list_estimated_bytes()
        + ATTR_POSTING_HASHSET_ESTIMATED_BYTES
        + entries.saturating_mul(ATTR_POSTING_HASHSET_ENTRY_ESTIMATED_BYTES)
}

fn interned_string_estimated_bytes(value: &str) -> usize {
    ATTR_POSTING_INTERNED_STRING_ESTIMATED_BYTES + value.len()
}

fn string_array_items(value: &str) -> Vec<String> {
    match crate::wire::parse(value) {
        Ok(crate::wire::Json::Arr(items)) => items
            .iter()
            .filter_map(crate::wire::Json::as_str)
            .map(json_string_compact)
            .collect(),
        _ => Vec::new(),
    }
}

fn json_string_compact(s: &str) -> String {
    crate::wire::Json::Str(s.to_string()).to_compact_json()
}

#[derive(Clone, Default)]
struct SegmentAttrSidecar {
    /// compact JSON exact postings for one immutable segment.
    exact: HashMap<(String, String), PostingList>,
    /// string-array includes postings for one immutable segment.
    array_items: HashMap<(String, String), PostingList>,
    /// Segment-level fallback set. Used when a key is incomplete in this segment.
    all_span_keys: HashSet<SpanKey>,
    /// Attr keys whose postings are intentionally incomplete inside this segment.
    incomplete_keys: HashSet<String>,
}

impl SegmentAttrSidecar {
    fn build(records: &[WalRecord]) -> Self {
        let mut out = SegmentAttrSidecar::default();
        for r in records {
            let span_key = (r.trace_id, r.span_id);
            out.all_span_keys.insert(span_key);
            emit_indexable_attrs(&r.fields, |attr_key, value| {
                out.add_value(span_key, attr_key, value);
            });
        }
        out
    }

    fn add_value(&mut self, span_key: SpanKey, attr_key: &str, value: &str) {
        if !is_postings_attr_key(attr_key) || self.incomplete_keys.contains(attr_key) {
            return;
        }
        if value.len() > ATTR_POSTINGS_MAX_VALUE_BYTES {
            self.mark_key_incomplete(attr_key);
            return;
        }
        let array_items = string_array_items(value);
        if array_items.len() > ATTR_POSTINGS_MAX_ARRAY_ITEMS {
            self.mark_key_incomplete(attr_key);
            return;
        }
        insert_segment_attr_posting(&mut self.exact, attr_key, value, span_key);
        for item in array_items {
            insert_segment_attr_posting(&mut self.array_items, attr_key, &item, span_key);
        }
    }

    fn mark_key_incomplete(&mut self, attr_key: &str) {
        self.incomplete_keys.insert(attr_key.to_string());
        self.exact.retain(|(k, _), _| k != attr_key);
        self.array_items.retain(|(k, _), _| k != attr_key);
    }

    fn candidates_for_attr(&self, attr_key: &str, expected: &str) -> HashSet<SpanKey> {
        if self.incomplete_keys.contains(attr_key) {
            return self.all_span_keys.clone();
        }
        let mut out = HashSet::new();
        if let Some(keys) = self
            .exact
            .get(&(attr_key.to_string(), expected.to_string()))
        {
            keys.extend_into(&mut out);
        }
        let Ok(expected_json) = crate::wire::parse(expected) else {
            return out;
        };
        match expected_json {
            crate::wire::Json::Str(s) => {
                let item = json_string_compact(&s);
                if let Some(keys) = self.array_items.get(&(attr_key.to_string(), item)) {
                    keys.extend_into(&mut out);
                }
            }
            crate::wire::Json::Arr(items) => {
                let expected_strings: Vec<String> = items
                    .iter()
                    .filter_map(crate::wire::Json::as_str)
                    .map(json_string_compact)
                    .collect();
                for item in &expected_strings {
                    if let Some(keys) = self.exact.get(&(attr_key.to_string(), item.clone())) {
                        keys.extend_into(&mut out);
                    }
                }
                if !expected_strings.is_empty() {
                    let mut array_match: Option<HashSet<SpanKey>> = None;
                    for item in expected_strings {
                        let keys = self
                            .array_items
                            .get(&(attr_key.to_string(), item))
                            .map(PostingList::to_hashset)
                            .unwrap_or_default();
                        array_match = Some(match array_match {
                            None => keys,
                            Some(prev) => prev.intersection(&keys).copied().collect(),
                        });
                    }
                    if let Some(keys) = array_match {
                        out.extend(keys);
                    }
                }
            }
            _ => {}
        }
        out
    }

    fn terms(&self) -> SegmentAttrTerms {
        SegmentAttrTerms {
            exact: self
                .exact
                .keys()
                .filter(|(k, _)| !self.incomplete_keys.contains(k))
                .cloned()
                .collect(),
            array_items: self
                .array_items
                .keys()
                .filter(|(k, _)| !self.incomplete_keys.contains(k))
                .cloned()
                .collect(),
            incomplete_keys: self.incomplete_keys.iter().cloned().collect(),
        }
    }

    fn estimated_bytes(&self) -> usize {
        let terms = self
            .exact
            .iter()
            .chain(self.array_items.iter())
            .map(|((k, v), list)| {
                ATTR_SIDECAR_TERM_ESTIMATED_BYTES
                    + k.len()
                    + v.len()
                    + list
                        .len()
                        .saturating_mul(ATTR_POSTING_SMALL_VEC_ENTRY_ESTIMATED_BYTES)
            })
            .sum::<usize>();
        terms
            + self
                .all_span_keys
                .len()
                .saturating_mul(ATTR_POSTING_SMALL_VEC_ENTRY_ESTIMATED_BYTES)
            + self
                .incomplete_keys
                .iter()
                .map(|k| ATTR_SIDECAR_TERM_ESTIMATED_BYTES + k.len())
                .sum::<usize>()
    }
}

#[derive(Clone, Default)]
struct SegmentAttrTerms {
    exact: Vec<(String, String)>,
    array_items: Vec<(String, String)>,
    incomplete_keys: Vec<String>,
}

#[derive(Default)]
struct SegmentAttrDirectory {
    exact: HashMap<(String, String), HashSet<u64>>,
    array_items: HashMap<(String, String), HashSet<u64>>,
    incomplete_keys: HashMap<String, HashSet<u64>>,
    terms_by_segment: HashMap<u64, SegmentAttrTerms>,
}

impl SegmentAttrDirectory {
    fn add_segment(&mut self, seg: SegmentId, sidecar: &SegmentAttrSidecar) {
        self.remove_segment(seg);
        let seg_id = seg.get();
        let terms = sidecar.terms();
        for term in &terms.exact {
            self.exact.entry(term.clone()).or_default().insert(seg_id);
        }
        for term in &terms.array_items {
            self.array_items
                .entry(term.clone())
                .or_default()
                .insert(seg_id);
        }
        for key in &terms.incomplete_keys {
            self.incomplete_keys
                .entry(key.clone())
                .or_default()
                .insert(seg_id);
        }
        self.terms_by_segment.insert(seg_id, terms);
    }

    fn remove_segment(&mut self, seg: SegmentId) {
        let seg_id = seg.get();
        let Some(terms) = self.terms_by_segment.remove(&seg_id) else {
            return;
        };
        for term in terms.exact {
            remove_segment_from_directory_map(&mut self.exact, &term, seg_id);
        }
        for term in terms.array_items {
            remove_segment_from_directory_map(&mut self.array_items, &term, seg_id);
        }
        for key in terms.incomplete_keys {
            remove_segment_from_directory_map(&mut self.incomplete_keys, &key, seg_id);
        }
    }

    fn candidate_segments_for_attr(&self, attr_key: &str, expected: &str) -> Option<HashSet<u64>> {
        if !is_postings_attr_key(attr_key) {
            return None;
        }
        let mut out = HashSet::new();
        extend_segment_set(
            &mut out,
            self.exact
                .get(&(attr_key.to_string(), expected.to_string())),
        );
        let Ok(expected_json) = crate::wire::parse(expected) else {
            extend_segment_set(&mut out, self.incomplete_keys.get(attr_key));
            return Some(out);
        };
        match expected_json {
            crate::wire::Json::Str(s) => {
                let item = json_string_compact(&s);
                extend_segment_set(
                    &mut out,
                    self.array_items.get(&(attr_key.to_string(), item)),
                );
            }
            crate::wire::Json::Arr(items) => {
                let expected_strings: Vec<String> = items
                    .iter()
                    .filter_map(crate::wire::Json::as_str)
                    .map(json_string_compact)
                    .collect();
                for item in &expected_strings {
                    extend_segment_set(
                        &mut out,
                        self.exact.get(&(attr_key.to_string(), item.clone())),
                    );
                }
                if !expected_strings.is_empty() {
                    let mut array_segments: Option<HashSet<u64>> = None;
                    for item in expected_strings {
                        let segs = self
                            .array_items
                            .get(&(attr_key.to_string(), item))
                            .cloned()
                            .unwrap_or_default();
                        array_segments = Some(match array_segments {
                            None => segs,
                            Some(prev) => prev.intersection(&segs).copied().collect(),
                        });
                    }
                    if let Some(segs) = array_segments {
                        out.extend(segs);
                    }
                }
            }
            _ => {}
        }
        extend_segment_set(&mut out, self.incomplete_keys.get(attr_key));
        Some(out)
    }

    fn stats(&self) -> (usize, usize, usize, usize) {
        (
            self.terms_by_segment.len(),
            self.exact.len(),
            self.array_items.len(),
            self.incomplete_keys
                .values()
                .map(HashSet::len)
                .sum::<usize>(),
        )
    }
}

struct SegmentAttrSidecarCache {
    cap_bytes: usize,
    cur_bytes: usize,
    map: HashMap<u64, (Arc<SegmentAttrSidecar>, usize, u64)>,
    tick: u64,
    hits: u64,
    misses: u64,
    loads: u64,
    evictions: u64,
}

impl SegmentAttrSidecarCache {
    fn new(cap_bytes: usize) -> Self {
        Self {
            cap_bytes: cap_bytes.max(1),
            cur_bytes: 0,
            map: HashMap::new(),
            tick: 0,
            hits: 0,
            misses: 0,
            loads: 0,
            evictions: 0,
        }
    }

    fn get(&mut self, seg: SegmentId) -> Option<Arc<SegmentAttrSidecar>> {
        self.tick += 1;
        let tick = self.tick;
        match self.map.get_mut(&seg.get()) {
            Some((sidecar, _, seen)) => {
                *seen = tick;
                self.hits += 1;
                Some(sidecar.clone())
            }
            None => {
                self.misses += 1;
                None
            }
        }
    }

    fn insert(
        &mut self,
        seg: SegmentId,
        sidecar: Arc<SegmentAttrSidecar>,
    ) -> Arc<SegmentAttrSidecar> {
        self.loads += 1;
        let bytes = sidecar.estimated_bytes();
        if bytes > self.cap_bytes {
            return sidecar;
        }
        self.tick += 1;
        if let Some((_, old_bytes, _)) = self.map.remove(&seg.get()) {
            self.cur_bytes = self.cur_bytes.saturating_sub(old_bytes);
        }
        self.cur_bytes = self.cur_bytes.saturating_add(bytes);
        self.map
            .insert(seg.get(), (sidecar.clone(), bytes, self.tick));
        self.evict();
        sidecar
    }

    fn remove(&mut self, seg: SegmentId) {
        if let Some((_, bytes, _)) = self.map.remove(&seg.get()) {
            self.cur_bytes = self.cur_bytes.saturating_sub(bytes);
        }
    }

    fn evict(&mut self) {
        let target = (self.cap_bytes * 9 / 10).max(1);
        let mut by_tick: Vec<(u64, u64, usize)> = self
            .map
            .iter()
            .map(|(&seg, (_, bytes, tick))| (*tick, seg, *bytes))
            .collect();
        by_tick.sort_unstable_by_key(|x| x.0);
        for (_, seg, bytes) in by_tick {
            if self.cur_bytes <= target || self.map.len() <= 1 {
                break;
            }
            self.map.remove(&seg);
            self.cur_bytes = self.cur_bytes.saturating_sub(bytes);
            self.evictions += 1;
        }
    }

    fn stats(&self) -> (usize, usize, usize, u64, u64, u64, u64) {
        (
            self.map.len(),
            self.cur_bytes,
            self.cap_bytes,
            self.hits,
            self.misses,
            self.loads,
            self.evictions,
        )
    }
}

const ATTR_SIDECAR_CACHE_MAX_BYTES: usize = 64 * 1024 * 1024;
const ATTR_SIDECAR_TERM_ESTIMATED_BYTES: usize = 64;
const ATTR_SIDECAR_MAGIC: &[u8; 8] = b"YTAS1\0\0\0";

fn insert_segment_attr_posting(
    index: &mut HashMap<(String, String), PostingList>,
    attr_key: &str,
    attr_value: &str,
    span_key: SpanKey,
) {
    let key = (attr_key.to_string(), attr_value.to_string());
    match index.get_mut(&key) {
        Some(list) => {
            list.insert(span_key);
        }
        None => {
            index.insert(key, PostingList::One(span_key));
        }
    }
}

fn remove_segment_from_directory_map<K: Eq + std::hash::Hash + Clone>(
    map: &mut HashMap<K, HashSet<u64>>,
    key: &K,
    seg: u64,
) {
    let remove = if let Some(segs) = map.get_mut(key) {
        segs.remove(&seg);
        segs.is_empty()
    } else {
        false
    };
    if remove {
        map.remove(key);
    }
}

fn extend_segment_set(out: &mut HashSet<u64>, segs: Option<&HashSet<u64>>) {
    if let Some(segs) = segs {
        out.extend(segs.iter().copied());
    }
}

fn attr_sidecar_path(dir: &std::path::Path, seg: SegmentId) -> std::path::PathBuf {
    dir.join(format!("seg-{}.attrs", seg.get()))
}

fn write_segment_attr_sidecar_file(
    dir: &std::path::Path,
    seg: SegmentId,
    sidecar: &SegmentAttrSidecar,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = attr_sidecar_path(dir, seg);
    let tmp = dir.join(format!("seg-{}.attrs.tmp", seg.get()));
    std::fs::write(&tmp, encode_segment_attr_sidecar(sidecar))?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

fn read_segment_attr_sidecar_file(
    dir: &std::path::Path,
    seg: SegmentId,
) -> Option<SegmentAttrSidecar> {
    let bytes = std::fs::read(attr_sidecar_path(dir, seg)).ok()?;
    decode_segment_attr_sidecar(&bytes).ok()
}

fn encode_segment_attr_sidecar(sidecar: &SegmentAttrSidecar) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(ATTR_SIDECAR_MAGIC);
    write_sidecar_map(&mut out, &sidecar.exact);
    write_sidecar_map(&mut out, &sidecar.array_items);
    write_u32(&mut out, sidecar.all_span_keys.len() as u32);
    let mut span_keys: Vec<SpanKey> = sidecar.all_span_keys.iter().copied().collect();
    span_keys.sort_unstable();
    for (trace_id, span_id) in span_keys {
        write_u64(&mut out, trace_id);
        write_u64(&mut out, span_id);
    }
    write_u32(&mut out, sidecar.incomplete_keys.len() as u32);
    let mut incomplete: Vec<&String> = sidecar.incomplete_keys.iter().collect();
    incomplete.sort_unstable();
    for key in incomplete {
        write_string(&mut out, key);
    }
    out
}

fn decode_segment_attr_sidecar(bytes: &[u8]) -> Result<SegmentAttrSidecar, String> {
    let mut pos = 0usize;
    if bytes.len() < ATTR_SIDECAR_MAGIC.len()
        || &bytes[..ATTR_SIDECAR_MAGIC.len()] != ATTR_SIDECAR_MAGIC
    {
        return Err("bad attr sidecar magic".into());
    }
    pos += ATTR_SIDECAR_MAGIC.len();
    let exact = read_sidecar_map(bytes, &mut pos)?;
    let array_items = read_sidecar_map(bytes, &mut pos)?;
    let span_count = read_u32(bytes, &mut pos)? as usize;
    let mut all_span_keys = HashSet::with_capacity(span_count);
    for _ in 0..span_count {
        let trace_id = read_u64(bytes, &mut pos)?;
        let span_id = read_u64(bytes, &mut pos)?;
        all_span_keys.insert((trace_id, span_id));
    }
    let incomplete_count = read_u32(bytes, &mut pos)? as usize;
    let mut incomplete_keys = HashSet::with_capacity(incomplete_count);
    for _ in 0..incomplete_count {
        incomplete_keys.insert(read_string(bytes, &mut pos)?);
    }
    Ok(SegmentAttrSidecar {
        exact,
        array_items,
        all_span_keys,
        incomplete_keys,
    })
}

fn write_sidecar_map(out: &mut Vec<u8>, map: &HashMap<(String, String), PostingList>) {
    let mut entries: Vec<(&(String, String), &PostingList)> = map.iter().collect();
    entries.sort_unstable_by(|a, b| a.0.cmp(b.0));
    write_u32(out, entries.len() as u32);
    for ((key, value), list) in entries {
        write_string(out, key);
        write_string(out, value);
        let span_keys = list.to_sorted_vec();
        write_u32(out, span_keys.len() as u32);
        for (trace_id, span_id) in span_keys {
            write_u64(out, trace_id);
            write_u64(out, span_id);
        }
    }
}

fn read_sidecar_map(
    bytes: &[u8],
    pos: &mut usize,
) -> Result<HashMap<(String, String), PostingList>, String> {
    let count = read_u32(bytes, pos)? as usize;
    let mut out = HashMap::with_capacity(count);
    for _ in 0..count {
        let key = read_string(bytes, pos)?;
        let value = read_string(bytes, pos)?;
        let span_count = read_u32(bytes, pos)? as usize;
        let mut span_keys = Vec::with_capacity(span_count);
        for _ in 0..span_count {
            span_keys.push((read_u64(bytes, pos)?, read_u64(bytes, pos)?));
        }
        if let Some(list) = PostingList::from_keys(span_keys) {
            out.insert((key, value), list);
        }
    }
    Ok(out)
}

fn write_u32(out: &mut Vec<u8>, n: u32) {
    out.extend_from_slice(&n.to_le_bytes());
}

fn write_u64(out: &mut Vec<u8>, n: u64) {
    out.extend_from_slice(&n.to_le_bytes());
}

fn write_string(out: &mut Vec<u8>, s: &str) {
    write_u32(out, s.len() as u32);
    out.extend_from_slice(s.as_bytes());
}

fn read_u32(bytes: &[u8], pos: &mut usize) -> Result<u32, String> {
    let end = pos.saturating_add(4);
    let Some(slice) = bytes.get(*pos..end) else {
        return Err("truncated u32".into());
    };
    *pos = end;
    Ok(u32::from_le_bytes(slice.try_into().unwrap()))
}

fn read_u64(bytes: &[u8], pos: &mut usize) -> Result<u64, String> {
    let end = pos.saturating_add(8);
    let Some(slice) = bytes.get(*pos..end) else {
        return Err("truncated u64".into());
    };
    *pos = end;
    Ok(u64::from_le_bytes(slice.try_into().unwrap()))
}

fn read_string(bytes: &[u8], pos: &mut usize) -> Result<String, String> {
    let len = read_u32(bytes, pos)? as usize;
    let end = pos.saturating_add(len);
    let Some(slice) = bytes.get(*pos..end) else {
        return Err("truncated string".into());
    };
    *pos = end;
    String::from_utf8(slice.to_vec()).map_err(|_| "invalid utf8".to_string())
}

/// 检索过滤条件（产品维度）。下推进图搜索 / 后置过滤关键词候选。全 None = 不过滤。
/// 例："找 agent『风控研判』报错(status≠0)的相似 span" → trace_id=None, agent_name=Some(风控研判), status...
#[derive(Default, Clone)]
pub struct SearchFilter {
    pub trace_id: Option<u64>,
    pub agent_name: Option<String>,
    pub status: Option<u8>,
    pub time_from: Option<i64>,
    pub time_to: Option<i64>,
    /// attrs 精确过滤。value 是 compact JSON；标量走精确匹配，字符串数组支持 includes。
    pub attrs: BTreeMap<String, String>,
    /// **租户隔离**：设了它，只返回该租户的 span。服务层须按鉴权身份对每个查询注入它。
    pub tenant_id: Option<u64>,
}

impl SearchFilter {
    /// 是否带"要查属性边车"的约束（agent/status/时间/租户）。仅 trace_id 约束不算（trace_id 在 key 里直接判）。
    fn needs_attrs(&self) -> bool {
        self.agent_name.is_some()
            || self.status.is_some()
            || self.time_from.is_some()
            || self.time_to.is_some()
            || !self.attrs.is_empty()
            || self.tenant_id.is_some()
    }

    /// 属性是否匹配（不含 trace_id，那个在 key 上单独判）。
    fn attrs_match(&self, a: &FilterAttrs) -> bool {
        // 租户隔离：tenant 不符直接出局（最先判，隔离优先）。
        if let Some(t) = self.tenant_id {
            if a.tenant_id != Some(t) {
                return false;
            }
        }
        if let Some(ag) = &self.agent_name {
            if a.agent_name.as_deref() != Some(ag.as_str()) {
                return false;
            }
        }
        if let Some(st) = self.status {
            if a.status != Some(st) {
                return false;
            }
        }
        // 时间窗：span 的 [min_ts,max_ts] 与 [time_from,time_to] 有重叠才算命中。
        if let Some(from) = self.time_from {
            if a.max_ts < from {
                return false;
            }
        }
        if let Some(to) = self.time_to {
            if a.min_ts > to {
                return false;
            }
        }
        for (k, v) in &self.attrs {
            if !a
                .attrs
                .get(k)
                .map(|actual| attr_json_matches(actual, v))
                .unwrap_or(false)
            {
                return false;
            }
        }
        true
    }
}

fn is_filter_attr_key(k: &str) -> bool {
    !k.is_empty()
}

fn is_agentic_field_key(k: &str) -> bool {
    matches!(
        k,
        "project_id"
            | "session_id"
            | "external_run_id"
            | "skill"
            | "mode"
            | "call_site"
            | "task_fingerprint"
            | "loop_id"
            | "harness_version"
            | "validation_status"
            | "stop_reason"
            | "phase"
            | "validator"
            | "connection_ids"
            | "data_source_ids"
            | "schema_fingerprint"
            | "eval_profile"
            | "tool_version"
            | "model"
            | "provider"
            | "intent_signature"
            | "review_status"
            | "eval_status"
            | "path_memory_id"
    )
}

pub(crate) fn attr_json_matches(actual: &str, expected: &str) -> bool {
    if actual == expected {
        return true;
    }
    if let Ok(expected_json) = crate::wire::parse(expected) {
        match &expected_json {
            crate::wire::Json::Str(expected_s) if actual == expected_s => return true,
            crate::wire::Json::Arr(expected_items) => {
                if expected_items
                    .iter()
                    .any(|item| item.as_str() == Some(actual))
                {
                    return true;
                }
            }
            _ => {}
        }
    }
    let Ok(actual_json) = crate::wire::parse(actual) else {
        return false;
    };
    let Ok(expected_json) = crate::wire::parse(expected) else {
        return false;
    };
    match (&actual_json, &expected_json) {
        (crate::wire::Json::Arr(actual_items), crate::wire::Json::Str(expected_s)) => actual_items
            .iter()
            .any(|item| item.as_str() == Some(expected_s.as_str())),
        (crate::wire::Json::Str(actual_s), crate::wire::Json::Arr(expected_items)) => {
            expected_items
                .iter()
                .any(|item| item.as_str() == Some(actual_s.as_str()))
        }
        (crate::wire::Json::Arr(actual_items), crate::wire::Json::Arr(expected_items)) => {
            let actual_strings: std::collections::HashSet<&str> = actual_items
                .iter()
                .filter_map(crate::wire::Json::as_str)
                .collect();
            let expected_strings: Vec<&str> = expected_items
                .iter()
                .filter_map(crate::wire::Json::as_str)
                .collect();
            !expected_strings.is_empty()
                && expected_strings
                    .iter()
                    .all(|expected_s| actual_strings.contains(expected_s))
        }
        _ => false,
    }
}

fn metadata_attrs_match(
    actual: &BTreeMap<String, String>,
    expected: &BTreeMap<String, String>,
) -> bool {
    expected.iter().all(|(key, want)| {
        actual
            .get(key)
            .map(|got| attr_json_matches(got, want))
            .unwrap_or(false)
    })
}

fn annotation_matches(a: &TraceAnnotation, f: &TraceAnnotationFilter) -> bool {
    if let Some(status) = f.status {
        if a.status != status {
            return false;
        }
    } else if !f.include_deleted && a.status == AnnotationStatus::Deleted {
        return false;
    }
    if let Some(tenant_id) = f.tenant_id {
        if a.tenant_id != Some(tenant_id) {
            return false;
        }
    }
    if let Some(target) = f.target {
        if a.target != target {
            return false;
        }
    }
    if let Some(trace_id) = f.trace_id {
        if a.trace_id != trace_id {
            return false;
        }
    }
    if let Some(span_id) = f.span_id {
        if a.span_id != Some(span_id) {
            return false;
        }
    }
    if let Some(label) = &f.label {
        if a.label != *label {
            return false;
        }
    }
    if let Some(source) = &f.source {
        if a.source.as_deref() != Some(source.as_str()) {
            return false;
        }
    }
    metadata_attrs_match(&a.attrs, &f.attrs)
}

fn dataset_association_matches(a: &DatasetAssociation, f: &DatasetAssociationFilter) -> bool {
    if let Some(tenant_id) = f.tenant_id {
        if a.tenant_id != Some(tenant_id) {
            return false;
        }
    }
    if let Some(dataset_id) = &f.dataset_id {
        if a.dataset_id != *dataset_id {
            return false;
        }
    }
    if let Some(item_id) = &f.item_id {
        if a.item_id != *item_id {
            return false;
        }
    }
    if let Some(trace_id) = f.trace_id {
        if a.trace_id != trace_id {
            return false;
        }
    }
    if let Some(span_id) = f.span_id {
        if a.span_id != Some(span_id) {
            return false;
        }
    }
    if let Some(eval_run_id) = &f.eval_run_id {
        if a.eval_run_id.as_deref() != Some(eval_run_id.as_str()) {
            return false;
        }
    }
    if let Some(split) = &f.split {
        if a.split.as_deref() != Some(split.as_str()) {
            return false;
        }
    }
    if let Some(label) = &f.label {
        if a.label.as_deref() != Some(label.as_str()) {
            return false;
        }
    }
    metadata_attrs_match(&a.attrs, &f.attrs)
}

fn golden_path_matches(g: &GoldenPathCandidate, f: &GoldenPathFilter) -> bool {
    if let Some(tenant_id) = f.tenant_id {
        if g.tenant_id != Some(tenant_id) {
            return false;
        }
    }
    if let Some(golden_path_id) = f.golden_path_id {
        if g.golden_path_id != golden_path_id {
            return false;
        }
    }
    if let Some(task_fingerprint) = &f.task_fingerprint {
        if g.task_fingerprint != *task_fingerprint {
            return false;
        }
    }
    if let Some(signature) = &f.trajectory_signature {
        if g.trajectory_signature != *signature {
            return false;
        }
    }
    if let Some(source_trace_id) = f.source_trace_id {
        if g.source_trace_id != source_trace_id {
            return false;
        }
    }
    if let Some(challenger_of) = f.challenger_of {
        if g.challenger_of != Some(challenger_of) {
            return false;
        }
    }
    if let Some(eval_profile) = &f.eval_profile {
        if g.eval_profile.as_deref() != Some(eval_profile.as_str()) {
            return false;
        }
    }
    if let Some(status) = f.status {
        if g.status != status {
            return false;
        }
    }
    metadata_attrs_match(&g.attrs, &f.attrs)
}

fn retention_audit_matches(a: &RetentionAuditRecord, f: &RetentionAuditFilter) -> bool {
    if let Some(tenant_id) = f.tenant_id {
        if a.tenant_id != Some(tenant_id) {
            return false;
        }
    }
    if let Some(audit_id) = f.audit_id {
        if a.audit_id != audit_id {
            return false;
        }
    }
    if let Some(source) = &f.source {
        if a.source.as_deref() != Some(source.as_str()) {
            return false;
        }
    }
    if let Some(min_created_at_ns) = f.min_created_at_ns {
        if a.created_at_ns < min_created_at_ns {
            return false;
        }
    }
    if let Some(max_created_at_ns) = f.max_created_at_ns {
        if a.created_at_ns > max_created_at_ns {
            return false;
        }
    }
    true
}

fn retention_policy_matches(p: &RetentionPolicy, f: &RetentionPolicyFilter) -> bool {
    if let Some(tenant_id) = f.tenant_id {
        if p.tenant_id != Some(tenant_id) {
            return false;
        }
    }
    if let Some(policy_id) = f.policy_id {
        if p.policy_id != policy_id {
            return false;
        }
    }
    if let Some(name) = &f.name {
        if p.name != *name {
            return false;
        }
    }
    if let Some(enabled) = f.enabled {
        if p.enabled != enabled {
            return false;
        }
    }
    true
}

fn first_class_attr_value<'a>(fields: &'a yt_core::fold::SpanFields, key: &str) -> Option<&'a str> {
    match key {
        "project_id" => fields.project_id.as_deref(),
        "skill" => fields.skill.as_deref(),
        "mode" => fields.mode.as_deref(),
        "call_site" => fields.call_site.as_deref(),
        "task_fingerprint" => fields.task_fingerprint.as_deref(),
        "loop_id" => fields.loop_id.as_deref(),
        "harness_version" => fields.harness_version.as_deref(),
        "schema_fingerprint" => fields.schema_fingerprint.as_deref(),
        "intent_signature" => fields.intent_signature.as_deref(),
        "validation_status" => fields.validation_status.as_deref(),
        "review_status" => fields.review_status.as_deref(),
        "eval_status" => fields.eval_status.as_deref(),
        "path_memory_id" => fields.path_memory_id.as_deref(),
        "stop_reason" => fields.stop_reason.as_deref(),
        "phase" => fields.phase.as_deref(),
        "validator" => fields.validator.as_deref(),
        "model" => fields.model.as_deref(),
        "provider" => fields.provider.as_deref(),
        _ => None,
    }
}

pub(crate) fn first_class_span_attr_value<'a>(s: &'a FoldedSpan, key: &str) -> Option<&'a str> {
    match key {
        "project_id" => s.project_id.as_deref(),
        "skill" => s.skill.as_deref(),
        "mode" => s.mode.as_deref(),
        "call_site" => s.call_site.as_deref(),
        "task_fingerprint" => s.task_fingerprint.as_deref(),
        "loop_id" => s.loop_id.as_deref(),
        "harness_version" => s.harness_version.as_deref(),
        "schema_fingerprint" => s.schema_fingerprint.as_deref(),
        "intent_signature" => s.intent_signature.as_deref(),
        "validation_status" => s.validation_status.as_deref(),
        "review_status" => s.review_status.as_deref(),
        "eval_status" => s.eval_status.as_deref(),
        "path_memory_id" => s.path_memory_id.as_deref(),
        "stop_reason" => s.stop_reason.as_deref(),
        "phase" => s.phase.as_deref(),
        "validator" => s.validator.as_deref(),
        "model" => s.model.as_deref(),
        "provider" => s.provider.as_deref(),
        _ => None,
    }
}

pub(crate) fn first_class_console_attr_value<'a>(s: &'a ConsoleSpan, key: &str) -> Option<&'a str> {
    match key {
        "project_id" => s.project_id.as_deref(),
        "skill" => s.skill.as_deref(),
        "mode" => s.mode.as_deref(),
        "call_site" => s.call_site.as_deref(),
        "task_fingerprint" => s.task_fingerprint.as_deref(),
        "loop_id" => s.loop_id.as_deref(),
        "harness_version" => s.harness_version.as_deref(),
        "schema_fingerprint" => s.schema_fingerprint.as_deref(),
        "intent_signature" => s.intent_signature.as_deref(),
        "validation_status" => s.validation_status.as_deref(),
        "review_status" => s.review_status.as_deref(),
        "eval_status" => s.eval_status.as_deref(),
        "path_memory_id" => s.path_memory_id.as_deref(),
        "stop_reason" => s.stop_reason.as_deref(),
        "phase" => s.phase.as_deref(),
        "validator" => s.validator.as_deref(),
        "model" => s.model.as_deref(),
        "provider" => s.provider.as_deref(),
        _ => None,
    }
}

fn fields_attr_value<'a>(fields: &'a yt_core::fold::SpanFields, key: &str) -> Option<&'a str> {
    first_class_attr_value(fields, key).or_else(|| fields.attrs.get(key).map(String::as_str))
}

pub(crate) fn folded_span_attr_value<'a>(s: &'a FoldedSpan, key: &str) -> Option<&'a str> {
    first_class_span_attr_value(s, key).or_else(|| s.attrs.get(key).map(String::as_str))
}

pub(crate) fn trajectory_steps_for_spans(spans: &[FoldedSpan]) -> Vec<String> {
    spans.iter().map(trajectory_step_for_span).collect()
}

pub(crate) fn trajectory_signature_value(steps: &[String]) -> u64 {
    let mut bytes = Vec::new();
    for step in steps {
        bytes.extend_from_slice(step.as_bytes());
        bytes.push(0);
    }
    yt_core::event::fnv1a64(&bytes)
}

pub(crate) fn trajectory_signature_label(steps: &[String]) -> String {
    format!("fnv1a64:{:016x}", trajectory_signature_value(steps))
}

pub(crate) fn trajectory_step_for_span(s: &FoldedSpan) -> String {
    let (kind, name) = if let Some(tool) = &s.tool_name {
        ("tool", tool.as_str())
    } else if let Some(agent) = &s.agent_name {
        ("agent", agent.as_str())
    } else if let Some(model) = &s.model {
        ("llm", model.as_str())
    } else {
        ("other", "")
    };
    let fallback;
    let name = if name.is_empty() {
        fallback = format!("span {}", s.span_id);
        fallback.as_str()
    } else {
        name
    };
    let mut out = format!(
        "{}:{}",
        normalize_trajectory_part(kind),
        normalize_trajectory_part(name)
    );
    for key in ["phase", "validator"] {
        if let Some(value) = folded_span_attr_value(s, key) {
            out.push('|');
            out.push_str(key);
            out.push(':');
            out.push_str(&normalize_trajectory_part(&json_compact_label(value)));
        }
    }
    out
}

fn normalize_trajectory_part(value: &str) -> String {
    value
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_whitespace() || matches!(c, '|' | ':' | '\0') {
                '_'
            } else {
                c
            }
        })
        .collect()
}

fn json_compact_label(value: &str) -> String {
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        value[1..value.len() - 1].to_string()
    } else {
        value.to_string()
    }
}

fn emit_indexable_attrs(fields: &yt_core::fold::SpanFields, mut emit: impl FnMut(&str, &str)) {
    for key in first_class_agentic_attr_keys() {
        if let Some(value) = first_class_attr_value(fields, key) {
            let value_json = first_class_agentic_attr_json(key, value);
            emit(key, &value_json);
        }
    }
    for (key, value) in &fields.attrs {
        if first_class_attr_value(fields, key).is_none() {
            emit(key, value);
        }
    }
}

fn first_class_agentic_attr_keys() -> &'static [&'static str] {
    &[
        "project_id",
        "skill",
        "mode",
        "call_site",
        "task_fingerprint",
        "loop_id",
        "harness_version",
        "schema_fingerprint",
        "intent_signature",
        "validation_status",
        "review_status",
        "eval_status",
        "path_memory_id",
        "stop_reason",
        "phase",
        "validator",
        "model",
        "provider",
    ]
}

fn first_class_agentic_attr_json(key: &str, value: &str) -> String {
    match key {
        // `model` / `provider` are native string columns. The other agentic
        // dimensions are promoted from attrs and already store compact JSON.
        "model" | "provider" => json_string_compact(value),
        _ => value.to_string(),
    }
}

fn folded_span_attrs_match(s: &FoldedSpan, attrs: &BTreeMap<String, String>) -> bool {
    attrs.iter().all(|(k, v)| {
        folded_span_attr_value(s, k)
            .map(|actual| attr_json_matches(actual, v))
            .unwrap_or(false)
    })
}

fn summarize_trace_spans(spans: Vec<FoldedSpan>) -> Vec<TraceSummary> {
    let mut by_trace: BTreeMap<u64, TraceSummary> = BTreeMap::new();
    for s in spans {
        let e = by_trace.entry(s.trace_id).or_insert(TraceSummary {
            trace_id: s.trace_id,
            external_trace_id: s.external_trace_id.clone(),
            span_count: 0,
            total_duration_ns: 0,
            max_duration_ns: 0,
            error_count: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cached_input_tokens: 0,
            total_reasoning_tokens: 0,
            total_tokens: 0,
            total_cost_usd_nanos: 0,
        });
        if e.external_trace_id.is_none() {
            e.external_trace_id = s.external_trace_id.clone();
        }
        e.span_count += 1;
        if let Some(d) = s.duration_ns {
            e.total_duration_ns += d;
            e.max_duration_ns = e.max_duration_ns.max(d);
        }
        if matches!(s.status, Some(st) if st != 0) {
            e.error_count += 1;
        }
        e.total_input_tokens += s.input_tokens.unwrap_or(0);
        e.total_output_tokens += s.output_tokens.unwrap_or(0);
        e.total_cached_input_tokens += s.cached_input_tokens.unwrap_or(0);
        e.total_reasoning_tokens += s.reasoning_tokens.unwrap_or(0);
        e.total_tokens += usage_total_tokens(
            s.input_tokens.unwrap_or(0),
            s.output_tokens.unwrap_or(0),
            s.cached_input_tokens.unwrap_or(0),
            s.reasoning_tokens.unwrap_or(0),
            s.total_tokens,
        );
        e.total_cost_usd_nanos += usage_cost_usd_nanos_for_model(
            s.input_tokens.unwrap_or(0),
            s.output_tokens.unwrap_or(0),
            s.cached_input_tokens.unwrap_or(0),
            s.reasoning_tokens.unwrap_or(0),
            s.cost_usd_nanos,
            s.provider.as_deref(),
            s.model.as_deref(),
        );
    }
    by_trace.into_values().collect()
}

fn trace_trajectory_summary_from_spans(
    trace_id: u64,
    tenant_id: Option<u64>,
    spans: &[FoldedSpan],
) -> Option<TraceTrajectorySummary> {
    let first = spans.first()?;
    let steps = trajectory_steps_for_spans(spans);
    let mut out = TraceTrajectorySummary {
        tenant_id,
        trace_id,
        external_trace_id: first.external_trace_id.clone(),
        trajectory_signature: trajectory_signature_label(&steps),
        steps,
        span_count: 0,
        error_count: 0,
        duration_sum_ns: 0,
        duration_max_ns: 0,
        input_tokens: 0,
        output_tokens: 0,
        cached_input_tokens: 0,
        reasoning_tokens: 0,
        total_tokens: 0,
        cost_usd_nanos: 0,
        fields: BTreeMap::new(),
    };
    for s in spans {
        if out.external_trace_id.is_none() {
            out.external_trace_id = s.external_trace_id.clone();
        }
        out.span_count += 1;
        if s.status.unwrap_or(0) != 0 {
            out.error_count += 1;
        }
        if let Some(duration) = s.duration_ns {
            out.duration_sum_ns += duration as u128;
            out.duration_max_ns = out.duration_max_ns.max(duration);
        }
        out.input_tokens += s.input_tokens.unwrap_or(0);
        out.output_tokens += s.output_tokens.unwrap_or(0);
        out.cached_input_tokens += s.cached_input_tokens.unwrap_or(0);
        out.reasoning_tokens += s.reasoning_tokens.unwrap_or(0);
        out.total_tokens += usage_total_tokens(
            s.input_tokens.unwrap_or(0),
            s.output_tokens.unwrap_or(0),
            s.cached_input_tokens.unwrap_or(0),
            s.reasoning_tokens.unwrap_or(0),
            s.total_tokens,
        );
        out.cost_usd_nanos += usage_cost_usd_nanos_for_model(
            s.input_tokens.unwrap_or(0),
            s.output_tokens.unwrap_or(0),
            s.cached_input_tokens.unwrap_or(0),
            s.reasoning_tokens.unwrap_or(0),
            s.cost_usd_nanos,
            s.provider.as_deref(),
            s.model.as_deref(),
        );
        for key in first_class_agentic_attr_keys() {
            if let Some(value) = first_class_span_attr_value(s, key) {
                out.fields
                    .entry((*key).to_string())
                    .or_insert_with(|| first_class_agentic_attr_json(key, value));
            } else if let Some(value) = s.attrs.get(*key) {
                out.fields
                    .entry((*key).to_string())
                    .or_insert_with(|| value.to_string());
            }
        }
        for key in ["eval_profile", "tool_version"] {
            if let Some(value) = first_class_span_attr_value(s, key) {
                out.fields
                    .entry(key.to_string())
                    .or_insert_with(|| first_class_agentic_attr_json(key, value));
            } else if let Some(value) = s.attrs.get(key) {
                out.fields
                    .entry(key.to_string())
                    .or_insert_with(|| value.to_string());
            }
        }
    }
    Some(out)
}

pub const DEFAULT_INPUT_TOKEN_COST_USD_NANOS: u64 = 800;
pub const DEFAULT_OUTPUT_TOKEN_COST_USD_NANOS: u64 = 4_000;
pub const DEFAULT_CACHED_INPUT_TOKEN_COST_USD_NANOS: u64 = 0;

pub fn usage_total_tokens(
    input_tokens: u64,
    output_tokens: u64,
    cached_input_tokens: u64,
    reasoning_tokens: u64,
    explicit_total_tokens: Option<u64>,
) -> u64 {
    explicit_total_tokens
        .unwrap_or_else(|| input_tokens + output_tokens + cached_input_tokens + reasoning_tokens)
}

pub fn usage_cost_usd_nanos(
    input_tokens: u64,
    output_tokens: u64,
    _cached_input_tokens: u64,
    reasoning_tokens: u64,
    explicit_cost_usd_nanos: Option<u64>,
) -> u64 {
    estimate_usage_cost_usd_nanos(
        input_tokens,
        output_tokens,
        0,
        reasoning_tokens,
        explicit_cost_usd_nanos,
        DEFAULT_INPUT_TOKEN_COST_USD_NANOS,
        DEFAULT_OUTPUT_TOKEN_COST_USD_NANOS,
        DEFAULT_CACHED_INPUT_TOKEN_COST_USD_NANOS,
    )
}

pub fn usage_cost_usd_nanos_for_model(
    input_tokens: u64,
    output_tokens: u64,
    cached_input_tokens: u64,
    reasoning_tokens: u64,
    explicit_cost_usd_nanos: Option<u64>,
    provider: Option<&str>,
    model: Option<&str>,
) -> u64 {
    if let Some(explicit) = explicit_cost_usd_nanos {
        return explicit;
    }
    let (input_rate, output_rate, cached_rate) = model_token_prices(provider, model).unwrap_or((
        DEFAULT_INPUT_TOKEN_COST_USD_NANOS,
        DEFAULT_OUTPUT_TOKEN_COST_USD_NANOS,
        DEFAULT_CACHED_INPUT_TOKEN_COST_USD_NANOS,
    ));
    estimate_usage_cost_usd_nanos(
        input_tokens,
        output_tokens,
        cached_input_tokens,
        reasoning_tokens,
        None,
        input_rate,
        output_rate,
        cached_rate,
    )
}

pub fn usage_cost_source(
    explicit_cost_usd_nanos: Option<u64>,
    provider: Option<&str>,
    model: Option<&str>,
) -> &'static str {
    if explicit_cost_usd_nanos.is_some() {
        "explicit"
    } else if model_token_prices(provider, model).is_some() {
        "estimated_model_price"
    } else {
        "estimated_default"
    }
}

fn estimate_usage_cost_usd_nanos(
    input_tokens: u64,
    output_tokens: u64,
    cached_input_tokens: u64,
    reasoning_tokens: u64,
    explicit_cost_usd_nanos: Option<u64>,
    input_rate: u64,
    output_rate: u64,
    cached_input_rate: u64,
) -> u64 {
    explicit_cost_usd_nanos.unwrap_or_else(|| {
        input_tokens.saturating_mul(input_rate)
            + cached_input_tokens.saturating_mul(cached_input_rate)
            + output_tokens
                .saturating_add(reasoning_tokens)
                .saturating_mul(output_rate)
    })
}

fn model_token_prices(provider: Option<&str>, model: Option<&str>) -> Option<(u64, u64, u64)> {
    let provider = provider.unwrap_or("").to_ascii_lowercase();
    let model = model.unwrap_or("").to_ascii_lowercase();
    if model.is_empty() {
        return None;
    }
    let is_provider = |name: &str| provider.is_empty() || provider == name;
    if is_provider("openai") {
        if model.contains("gpt-4o-mini") {
            return Some((150, 600, 75));
        }
        if model.contains("gpt-4o") {
            return Some((2_500, 10_000, 1_250));
        }
        if model.contains("o3-mini") {
            return Some((1_100, 4_400, 550));
        }
    }
    if is_provider("anthropic") {
        if model.contains("claude-3-5-sonnet") || model.contains("claude-3.5-sonnet") {
            return Some((3_000, 15_000, 300));
        }
        if model.contains("claude-3-haiku") {
            return Some((250, 1_250, 25));
        }
    }
    None
}

/// 一条 trace 的摘要（web 控制台列表视图用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceSummary {
    pub trace_id: u64,
    pub external_trace_id: Option<String>,
    pub span_count: usize,
    pub total_duration_ns: u64,
    pub max_duration_ns: u64,
    /// 状态非 0 的 span 数（报错）。
    pub error_count: usize,
    /// 全 trace 输入/输出 token 汇总（成本指标）。
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cached_input_tokens: u64,
    pub total_reasoning_tokens: u64,
    pub total_tokens: u64,
    pub total_cost_usd_nanos: u64,
}

/// 每条 trace 的轻量 trajectory 物化摘要。
///
/// 这是派生读模型：不进入 WAL/segment 主格式，不保存输入输出原文；写入同 trace 新事件后失效。
/// Golden Path health、路径导出和后续产品页可复用它，避免每次都重新折叠整条 trace。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceTrajectorySummary {
    pub tenant_id: Option<u64>,
    pub trace_id: u64,
    pub external_trace_id: Option<String>,
    pub trajectory_signature: String,
    pub steps: Vec<String>,
    pub span_count: usize,
    pub error_count: usize,
    pub duration_sum_ns: u128,
    pub duration_max_ns: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    pub reasoning_tokens: u64,
    pub total_tokens: u64,
    pub cost_usd_nanos: u64,
    pub fields: BTreeMap<String, String>,
}

/// retention apply 的物理删除结果。
///
/// 当前只删除已经 flush 进 segment 的行；仍在 MemTable/WAL tail 的热 trace 会跳过，避免半删。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RetentionDeleteResult {
    pub requested_trace_count: usize,
    pub deleted_trace_count: usize,
    pub deleted_segment_row_count: usize,
    pub skipped_live_trace_count: usize,
    pub deleted_trace_ids: Vec<u64>,
    pub skipped_live_trace_ids: Vec<u64>,
}

/// retention 后可选 compaction 的结果。
///
/// 这层只负责把 deletion vector 物化进新段，并触发已有 GC 安全回收；是否真正释放磁盘取决于
/// 当前是否仍有读者 pin 住旧版本或 buffer pin。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RetentionCompactResult {
    pub before_live_segment_count: usize,
    pub after_live_segment_count: usize,
    pub before_dead_segment_count: usize,
    pub after_dead_segment_count: usize,
    pub selected_segment_count: usize,
    pub compacted_segment_count: usize,
    pub reclaimed_segment_count: usize,
    pub dropped_deleted_row_count: usize,
    pub rewritten_live_row_count: usize,
    pub selected_segment_ids: Vec<u64>,
}

/// trace 树的一个节点 = 折叠出的 span + 它的孩子 span_id。
#[derive(Debug, Clone)]
pub struct TraceNode {
    pub span: FoldedSpan,
    pub children: Vec<u64>,
}

/// 一条 trace 的父子树（树+瀑布视图直接渲染）。
#[derive(Debug, Clone)]
pub struct TraceTree {
    pub trace_id: u64,
    /// 无父（或父不在本 trace 内）的 span_id，升序。
    pub roots: Vec<u64>,
    pub nodes: BTreeMap<u64, TraceNode>,
}

impl TraceTree {
    /// 深度优先顺序的 span_id（瀑布视图按此从上到下排）。孩子按 span_id 升序。
    pub fn dfs_order(&self) -> Vec<u64> {
        let mut out = Vec::new();
        let mut stack: Vec<u64> = self.roots.iter().rev().copied().collect();
        while let Some(id) = stack.pop() {
            out.push(id);
            if let Some(n) = self.nodes.get(&id) {
                for &c in n.children.iter().rev() {
                    stack.push(c);
                }
            }
        }
        out
    }
}

/// agent 执行图里一个节点的角色类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorKind {
    /// 有 agent_name 的 span。
    Agent,
    /// 无 agent_name 但有 tool_name 的 span。
    Tool,
    /// 两者都无（用 span:<id> 占位）。
    Other,
}

/// agent 执行图的一个节点 = 一个"角色"（agent / 工具），带聚合统计。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentGraphNode {
    pub actor: String,
    pub kind: ActorKind,
    pub span_count: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    pub reasoning_tokens: u64,
    pub total_tokens: u64,
    pub cost_usd_nanos: u64,
}

/// agent 执行图的一条边 = 父 span 的角色"调用/移交给"子 span 的角色（聚合次数）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentGraphEdge {
    pub from: String,
    pub to: String,
    pub count: usize,
}

/// 一条 trace 的 agent 执行图（DAG）：谁调用了谁。
/// 把"span 父子树"按 agent/工具维度收拢成"角色调用图"——dogfood 自家 SuperAgent 最想看的视图。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentGraph {
    pub trace_id: u64,
    /// 按 actor 名升序。
    pub nodes: Vec<AgentGraphNode>,
    /// 按 (from, to) 升序。已剔除同角色自环（只留跨角色的调用/移交）。
    pub edges: Vec<AgentGraphEdge>,
}

/// 多轮对话里的**一轮** = 会话内的一条 trace，抽成「用户问 → agent 答」的对子 + 该轮统计。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionTurn {
    pub trace_id: u64,
    /// 轮次序号（0 起）。按 trace_id 升序定序 —— trace id 单调下发，是对话时间序的可靠代理
    /// （折叠后的 span 不保留 ts，故不按 ts 排）。
    pub turn_index: usize,
    /// 该轮输入：span_id 最小的、带 input_text 的 span（通常是编排根 span 上的提示词）。
    pub user_input: Option<String>,
    /// 该轮最终答复：span_id 最大的、带 output_text 的 span（最末一步的作答）。
    pub agent_output: Option<String>,
    /// 该轮参与的 agent（去重升序）。
    pub agents: Vec<String>,
    pub span_count: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    pub reasoning_tokens: u64,
    pub total_tokens: u64,
    pub cost_usd_nanos: u64,
    /// 该轮 status≠0 的 span 数（这一轮有没有出错）。
    pub error_count: usize,
    /// 该轮答复 span 的评测分（若已 eval 写回）。
    pub eval_score: Option<u32>,
}

/// 一个会话的**多轮对话流**（多轮会话视图直接渲染）：把会话内多条 trace 按时间序拼成对话。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionTimeline {
    pub session_id: u64,
    /// 按 turn_index 升序。
    pub turns: Vec<SessionTurn>,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
}

/// 控制台会话行（一次扫描聚合）。比 `SessionSummary` 多了标题/状态/首 trace，给前端列表直接用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleSession {
    pub session_id: u64,
    pub external_session_id: Option<String>,
    pub title: String,
    pub turn_count: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    pub reasoning_tokens: u64,
    pub total_tokens: u64,
    pub cost_usd_nanos: u64,
    pub has_error: bool,
    pub first_trace_id: u64,
}

/// 控制台瀑布的一行 span（kind/name/起始时刻为派生值，见 `console_trace_spans`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleSpan {
    pub span_id: u64,
    pub parent_span_id: Option<u64>,
    pub external_trace_id: Option<String>,
    pub external_span_id: Option<String>,
    pub external_parent_span_id: Option<String>,
    pub external_session_id: Option<String>,
    pub project_id: Option<String>,
    pub skill: Option<String>,
    pub mode: Option<String>,
    pub call_site: Option<String>,
    pub task_fingerprint: Option<String>,
    pub loop_id: Option<String>,
    pub harness_version: Option<String>,
    pub schema_fingerprint: Option<String>,
    pub intent_signature: Option<String>,
    pub validation_status: Option<String>,
    pub review_status: Option<String>,
    pub eval_status: Option<String>,
    pub path_memory_id: Option<String>,
    pub stop_reason: Option<String>,
    pub phase: Option<String>,
    pub validator: Option<String>,
    pub kind: &'static str,
    pub name: String,
    pub start_ns: u64,
    pub duration_ns: u64,
    pub has_error: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    pub reasoning_tokens: u64,
    pub total_tokens: u64,
    pub cost_usd_nanos: u64,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub cost_currency: Option<String>,
    pub input_text: Option<String>,
    pub output_text: Option<String>,
    pub attrs: BTreeMap<String, String>,
}

/// span 内的原始日志事件视图。它不是折叠后的 `logs` 字符串并集，而是保留事件顺序与 attrs 的明细，
/// 供 trace/span 详情页还原执行现场，避免业务侧把日志镜像进 `attrs.event_logs`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpanLogEvent {
    pub ts: i64,
    pub seq: u64,
    pub event_type: u8,
    pub event_id: u64,
    pub messages: Vec<String>,
    pub attrs: BTreeMap<String, String>,
}

/// 一个会话的摘要（多轮对话/agent 会话视图）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    pub session_id: u64,
    pub trace_count: usize,
    pub span_count: usize,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
}

/// 按 agent 的成本归因（per-agent 成本下钻）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCost {
    pub agent_name: String,
    pub span_count: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    pub reasoning_tokens: u64,
    pub total_tokens: u64,
    pub cost_usd_nanos: u64,
}

// ───────────────────────── 评测（eval 闭环） ─────────────────────────

/// 一个 scorer 对一条 span 的产出：千分制分数 + 标签。
///
/// 这是 eval 闭环的"评"那一步的结果。分数用千分制整数（保住可比/可持久化且不引入 f32 的 Eq 麻烦），
/// 展示层除以 10 得百分。label 给人看（"通过"/"未通过"/scorer 名）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalOutcome {
    pub score: u32,
    pub label: String,
}

impl EvalOutcome {
    pub fn new(score: u32, label: impl Into<String>) -> Self {
        Self {
            score: score.min(1000),
            label: label.into(),
        }
    }
}

/// scorer：看一条折叠出的 span，给个分。
///
/// 先做**不依赖 LLM 的规则 scorer**（关键词/正则/非空/无错），把"存→评→写回→读分"主链跑通；
/// LLM-judge 只是换一个 impl（异步调模型、本地小模型当裁判），闭环骨架不变。
/// 返回 None = 这条 span 不适用此 scorer（跳过，不写回）。
pub trait Scorer: Send + Sync {
    fn score(&self, span: &FoldedSpan) -> Option<EvalOutcome>;
}

/// 关键词规则 scorer：output_text 命中任一"坏词"判未通过(0)，否则通过(1000)。
/// 反洗钱/风控场景的探路用法：答案里出现"无法/抱歉/未知"等即判不合格。
pub struct KeywordScorer {
    bad_words: Vec<String>,
}

impl KeywordScorer {
    pub fn new(bad_words: &[&str]) -> Self {
        Self {
            bad_words: bad_words.iter().map(|s| s.to_string()).collect(),
        }
    }
}

impl Scorer for KeywordScorer {
    fn score(&self, span: &FoldedSpan) -> Option<EvalOutcome> {
        let text = span.output_text.as_deref()?; // 没有输出文本 → 不评
        let hit = self.bad_words.iter().any(|w| text.contains(w));
        Some(if hit {
            EvalOutcome::new(0, "未通过")
        } else {
            EvalOutcome::new(1000, "通过")
        })
    }
}

/// 一条 span 的评测记录（eval_and_writeback 的返回，便于观测/断言）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScoredSpan {
    pub trace_id: u64,
    pub span_id: u64,
    pub outcome: EvalOutcome,
}

/// 评测汇总的一行（整体一行 + 每个 agent 一行）。通过率/均分用于"哪个 agent 退步了"的回归视图。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalSummary {
    /// None = 整体；Some(name) = 该 agent。
    pub agent_name: Option<String>,
    /// 有分的 span 数（无 eval_score 的不计）。
    pub scored_spans: usize,
    /// 分数 ≥ 阈值的 span 数（通过）。
    pub pass_count: usize,
    /// 千分制平均分（scored_spans=0 时为 0）。
    pub avg_score: u32,
}

impl EvalSummary {
    /// 通过率（0.0..=1.0）。无打分 span 时为 0。
    pub fn pass_rate(&self) -> f32 {
        if self.scored_spans == 0 {
            0.0
        } else {
            self.pass_count as f32 / self.scored_spans as f32
        }
    }
}

/// 把一串 (可选 agent 名, 千分制分数) 聚合成评测看板：第 0 行恒为整体，其后按 agent 名升序。
/// `eval_summary`（线上已打分的 span）和 `eval_dataset`（对数据集现跑 scorer）共用这套口径。
fn aggregate_eval(
    scored: impl Iterator<Item = (Option<String>, u32)>,
    pass_threshold: u32,
) -> Vec<EvalSummary> {
    let mut overall = (0usize, 0usize, 0u64);
    let mut by_agent: BTreeMap<String, (usize, usize, u64)> = BTreeMap::new();
    for (agent, score) in scored {
        let pass = (score >= pass_threshold) as usize;
        overall.0 += 1;
        overall.1 += pass;
        overall.2 += score as u64;
        if let Some(a) = agent {
            let e = by_agent.entry(a).or_default();
            e.0 += 1;
            e.1 += pass;
            e.2 += score as u64;
        }
    }
    let mk = |agent_name: Option<String>, (scored, pass, sum): (usize, usize, u64)| EvalSummary {
        agent_name,
        scored_spans: scored,
        pass_count: pass,
        avg_score: if scored == 0 {
            0
        } else {
            (sum / scored as u64) as u32
        },
    };
    let mut out = vec![mk(None, overall)]; // 第 0 行恒为整体
    for (agent, acc) in by_agent {
        out.push(mk(Some(agent), acc));
    }
    out
}

// ───────────────────────── 评测数据集（Datasets） ─────────────────────────

/// 数据集的一条样本 = 采集时的 span 快照（含 input/output 文本、agent 名）+ 可选参考答案（人工标注）。
/// 存 span 快照而非引用:数据集是"冻结的回归基准",底层 trace 被合并/回收也不影响它。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatasetExample {
    pub span: FoldedSpan,
    /// 参考答案/期望输出（人工标注，可选）。给"对照参考答案打分"的 scorer 用。
    pub expected: Option<String>,
}

/// 一个命名评测数据集。eval 的燃料:把生产里的（失败/低分）trace 收集成固定集，反复回归重跑。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Dataset {
    pub name: String,
    pub examples: Vec<DatasetExample>,
}

/// 数据集摘要（列表视图）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatasetSummary {
    pub name: String,
    pub example_count: usize,
}

/// SDK 打点的线格式（对齐 Python / TS 的 `to_wire()` 字段）。
///
/// 摄入端据 `(ext_span_id, seq, event_type_tag)` **自己重算 event_id** —— 契约是这三个身份字段，
/// 不信任 SDK 传来的 event_id（SDK 算的与引擎一致是为了客户端去重/调试，引擎以自己算的为准）。
pub struct WireRecord {
    pub trace_id: u64,
    pub span_id: u64,
    pub ts: i64,
    pub seq: u64,
    pub event_type_tag: u8,
    pub ext_span_id: String,
    pub parent_span_id: Option<u64>,
    pub status: Option<u8>,
    pub duration_ns: Option<u64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cost_usd_nanos: Option<u64>,
    pub cost_currency: Option<String>,
    pub provider: Option<String>,
    pub session_id: Option<u64>,
    pub tenant_id: Option<u64>,
    pub external_trace_id: Option<String>,
    pub external_span_id: Option<String>,
    pub external_parent_span_id: Option<String>,
    pub external_session_id: Option<String>,
    pub agent_name: Option<String>,
    pub tool_name: Option<String>,
    pub model: Option<String>,
    pub input_text: Option<String>,
    pub output_text: Option<String>,
    pub logs: Vec<String>,
    pub attrs: BTreeMap<String, String>,
}

impl WireRecord {
    fn into_wal_record(self) -> WalRecord {
        let project_id = self.attrs.get("project_id").cloned();
        let skill = self.attrs.get("skill").cloned();
        let mode = self.attrs.get("mode").cloned();
        let call_site = self.attrs.get("call_site").cloned();
        let task_fingerprint = self.attrs.get("task_fingerprint").cloned();
        let loop_id = self.attrs.get("loop_id").cloned();
        let harness_version = self.attrs.get("harness_version").cloned();
        let schema_fingerprint = self.attrs.get("schema_fingerprint").cloned();
        let intent_signature = self.attrs.get("intent_signature").cloned();
        let validation_status = self.attrs.get("validation_status").cloned();
        let review_status = self.attrs.get("review_status").cloned();
        let eval_status = self.attrs.get("eval_status").cloned();
        let path_memory_id = self.attrs.get("path_memory_id").cloned();
        let stop_reason = self.attrs.get("stop_reason").cloned();
        let phase = self.attrs.get("phase").cloned();
        let validator = self.attrs.get("validator").cloned();
        WalRecord {
            trace_id: self.trace_id,
            span_id: self.span_id,
            ts: self.ts,
            identity: EventIdentity {
                ext_span_id: self.ext_span_id,
                seq: self.seq,
                event_type: EventType::from_tag(self.event_type_tag),
            },
            fields: SpanFields {
                status: self.status,
                duration_ns: self.duration_ns,
                parent_span_id: self.parent_span_id,
                input_tokens: self.input_tokens,
                output_tokens: self.output_tokens,
                cached_input_tokens: self.cached_input_tokens,
                reasoning_tokens: self.reasoning_tokens,
                total_tokens: self.total_tokens,
                cost_usd_nanos: self.cost_usd_nanos,
                cost_currency: self.cost_currency,
                provider: self.provider,
                session_id: self.session_id,
                tenant_id: self.tenant_id,
                external_trace_id: self.external_trace_id,
                external_span_id: self.external_span_id,
                external_parent_span_id: self.external_parent_span_id,
                external_session_id: self.external_session_id,
                project_id,
                skill,
                mode,
                call_site,
                task_fingerprint,
                loop_id,
                harness_version,
                schema_fingerprint,
                intent_signature,
                validation_status,
                review_status,
                eval_status,
                path_memory_id,
                stop_reason,
                phase,
                validator,
                agent_name: self.agent_name,
                tool_name: self.tool_name,
                model: self.model,
                input_text: self.input_text,
                output_text: self.output_text,
                eval_score: None, // 分数由 scorer 事后算、走 upgrade 补写，不从线上摄入
                eval_label: None,
                logs: self.logs,
                attrs: self.attrs,
            },
        }
    }
}

// ───────────────────────── 单写者协调器 ─────────────────────────

/// trace rollup 预聚合 profile 的物化预算。
///
/// 默认 `full()` 保持现有行为：storageStats 和 traceAggregate 的内置高频 profile 全部物化，
/// 所以 `WriteCoordinator::new/open/open_durable` 的单机默认路径不变。只有通过
/// [`CoordinatorBuilder`] 显式设置 limit 时，才会减少预聚合 profile；缺失的 profile 会回退到
/// segment rollup 或 folded scan，不会返回截断结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraceRollupProfileConfig {
    storage_profile_limit: Option<usize>,
    aggregate_profile_limit: Option<usize>,
    max_buckets_per_profile: Option<usize>,
}

impl Default for TraceRollupProfileConfig {
    fn default() -> Self {
        Self::full()
    }
}

impl TraceRollupProfileConfig {
    pub fn full() -> Self {
        Self {
            storage_profile_limit: None,
            aggregate_profile_limit: None,
            max_buckets_per_profile: None,
        }
    }

    pub fn with_storage_profile_limit(mut self, limit: usize) -> Self {
        self.storage_profile_limit = Some(limit);
        self
    }

    pub fn with_aggregate_profile_limit(mut self, limit: usize) -> Self {
        self.aggregate_profile_limit = Some(limit);
        self
    }

    pub fn with_max_buckets_per_profile(mut self, limit: usize) -> Self {
        self.max_buckets_per_profile = Some(limit);
        self
    }

    pub(crate) fn storage_profile_limit(&self) -> Option<usize> {
        self.storage_profile_limit
    }

    pub(crate) fn aggregate_profile_limit(&self) -> Option<usize> {
        self.aggregate_profile_limit
    }

    pub(crate) fn max_buckets_per_profile(&self) -> Option<usize> {
        self.max_buckets_per_profile
    }
}

/// 所有 manifest 提交的串行入口。持有 WAL + current 指针 + 段存储。
pub struct WriteCoordinator {
    /// 单写者锁：flush/compaction/delete/upgrade 全过这把锁。
    write_lock: Mutex<()>,
    current: Arc<Current>,
    wal: Mutex<Wal>,
    /// 活内存表（带双水位）。读路径的四源之一。
    memtable: Mutex<MemTable>,
    segments: Arc<dyn SegmentStore>,
    /// 等回收的 dead 资源（compaction 摘下的旧段）。
    dead_set: Mutex<Vec<DeadResource>>,
    /// 段文件 buffer pin 计数（GC 条件 (2)）。
    buffer_pins: BufferPins,
    /// BM25 中文倒排（检索）。真实实现接团队自有 BM25。
    bm25: Arc<dyn Bm25Index>,
    /// 分域全文倒排：input/output/log/tool/model/agent 单独索引，用于 Trace Inbox 精准检索。
    text_domains: Mutex<TextDomainIndexes>,
    /// 向量 ANN（找相似）。真实实现接团队 graph_index。
    graph: Arc<dyn GraphIndex>,
    /// 内存表行数超过此值就自动刷盘（兜住内存上界，OPEN-2）。
    flush_threshold: AtomicUsize,
    /// 段身份分配器（单写者下无并发分配竞争，永不复用）。
    next_segment_id: Mutex<u64>,
    next_chunk_id: Mutex<u64>,
    /// 评测数据集（按名）。元数据,不进 trace 存储;eval 的"燃料"与回归基准。
    datasets: Mutex<BTreeMap<String, Dataset>>,
    /// 业务 annotation：给 trace/span 记录人工或自动判断。独立持久化，不污染 trace 主格式。
    annotations: Mutex<Vec<TraceAnnotation>>,
    /// 外部 dataset item 与 trace/span 的关联。用于把线上路径沉淀成回归样本或最佳路径样本。
    dataset_associations: Mutex<Vec<DatasetAssociation>>,
    /// Golden path 候选与确认状态：只保存源 trace/snapshot 引用和评审状态，不复制 trace 主数据。
    golden_paths: Mutex<Vec<GoldenPathCandidate>>,
    /// Retention 执行审计：记录清理策略、保护原因和实际删除/压缩结果，便于误删排查。
    retention_audits: Mutex<Vec<RetentionAuditRecord>>,
    /// Retention policy：保存可重复执行的清理策略，调度器只负责触发，不复制删除逻辑。
    retention_policies: Mutex<Vec<RetentionPolicy>>,
    /// 业务元数据 revision：annotation/dataset/Golden Path/retention policy 变化时递增。
    /// 高频 read model cache 用它和 WAL/manifest revision 组成失效条件。
    metadata_epoch: AtomicU64,
    /// metadata 查询索引：annotation/dataset 按 tenant/status/trace/span/label/source/attrs 等维度取候选集。
    metadata_index: Mutex<MetadataIndex>,
    next_annotation_id: Mutex<u64>,
    next_dataset_association_id: Mutex<u64>,
    next_golden_path_id: Mutex<u64>,
    next_retention_audit_id: Mutex<u64>,
    next_retention_policy_id: Mutex<u64>,
    /// manifest 持久化路径。Some = 每次 commit 后原子写盘（重启不丢）；None = 纯内存。
    manifest_path: Option<std::path::PathBuf>,
    /// 业务元数据持久化路径（annotation + dataset association）。None = 纯内存。
    metadata_path: Option<std::path::PathBuf>,
    /// 向量独立落盘路径。Some = `index_embedding` 追加写盘、`recover` 重载（向量不在 trace 数据里,
    /// 段重建不出来,只能单独持久）；None = 纯内存。
    vector_path: Option<std::path::PathBuf>,
    /// task/span/trajectory 命名空间向量持久化路径。外部 embedder 写入，engine 只负责存储和召回。
    named_vector_path: Option<std::path::PathBuf>,
    /// 命名空间向量索引：第一版 flat scan，确保 namespace/tenant/attrs 语义正确，可后续替换为 ANN。
    named_vectors: Mutex<NamedVectorIndex>,
    /// 检索过滤的属性边车：(trace,span) → 可过滤元数据（带过滤 ANN 的 payload）。
    /// 派生数据：摄入时建,`recover` 时从持久段重建。
    filter_attrs: Mutex<HashMap<(u64, u64), FilterAttrs>>,
    /// live attrs postings 候选集：(attr_key, attr_value_json) → (trace,span)。
    /// 只覆盖 MemTable/WAL tail；持久段走 segment-local sidecar，最终结果仍经 snapshot 折叠读验证。
    attr_postings: Mutex<AttrPostings>,
    /// segment-local attrs sidecar 的轻量目录：term → segment ids，不保存 span posting list。
    seg_attr_directory: Mutex<SegmentAttrDirectory>,
    /// segment-local attrs sidecar 的 LRU cache：按需加载具体 posting list，受 bytes budget 约束。
    seg_attr_cache: Mutex<SegmentAttrSidecarCache>,
    /// 持久模式下的 attrs sidecar 目录。缺失/损坏时可由 segment 文件重建。
    attr_sidecar_dir: Option<std::path::PathBuf>,
    /// traceAggregate segment rollup：每个不可变段折叠成轻量 span 统计行，缺失时可由段文件重建。
    trace_aggregate_rollups: Mutex<HashMap<u64, Arc<TraceAggregateSegmentRollup>>>,
    /// 持久模式下的 traceAggregate rollup 目录。它是派生读模型，不进入 WAL/manifest 主格式。
    trace_aggregate_rollup_dir: Option<std::path::PathBuf>,
    /// rollup 预聚合物化预算。默认 full，不改变单机版现有行为。
    trace_rollup_profile_config: TraceRollupProfileConfig,
    /// trace → span keys。attrs 命中一个 span 后，用它扩展回整条 trace 的完整聚合。
    trace_span_keys: Mutex<HashMap<u64, HashSet<(u64, u64)>>>,
    /// trace trajectory 物化读模型缓存。写入同 trace 后失效，读路径按需重建。
    trace_trajectory_idx: Mutex<HashMap<(Option<u64>, u64), TraceTrajectorySummary>>,
    /// 控制台会话边车索引：摄入时**增量差量**维护（O(1)/事件），delete/upgrade 标脏、下次读重建。
    session_idx: Mutex<SessionIndex>,
    /// **段折叠缓存**：不可变段首次解码后缓存（行 + (trace,span)→行号 索引），检索路径只取候选行、
    /// 不再每查重读+重解码整段。段 unlink（compaction/GC）时失效。LRU、按总行数封顶。
    seg_fold_cache: Mutex<SegFoldCache>,
    /// **段级 key Bloom**（对齐 ClickHouse bloom_filter 跳过索引）：seg_id → 该段 (trace,span) 的 bloom。
    /// 检索折叠定位时，bloom 判"这个段肯定没有任何候选 key" → 整段跳过，不碰折叠缓存。派生数据：flush
    /// 时建、recover 时随重建索引一起重建、unlink 时移除。每段几 KB，常驻内存可控。
    seg_key_bloom: Mutex<HashMap<u64, Arc<KeyBloom>>>,
    /// **GC 日志**（崩溃安全）：Some = reclaim 走"MARK→fsync→unlink→DONE→fsync"，崩溃在中途重启补删；
    /// None = 纯内存态（非持久模式，reclaim 直接删，旧路径）。
    gc_log: Mutex<Option<gc_log::GcLog>>,
    /// 数据目录路径（持久模式 = Some）。`backup_snapshot` 用它知道拷哪些文件。
    dir: Option<std::path::PathBuf>,
}

/// 递归拷贝目录（备份用，零依赖）。
fn copy_dir_recursive(src: &std::path::Path, dest: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        let ft = entry.file_type()?;
        if ft.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

fn unix_now_ns_u64() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}

/// 段级 key Bloom 过滤器（双哈希 + 位组，std-only，无依赖）。`maybe_contains` 假阳允许、假阴不允许：
/// 返回 false = **肯定没有**（可放心跳段），返回 true = 可能有（要进一步查）。约 10 bit/key、7 个哈希。
struct KeyBloom {
    bits: Vec<u64>,
    mask: usize, // m_bits-1（m_bits 取 2 的幂，用 & 代 %）
    k: u32,
}

impl KeyBloom {
    fn build<I: IntoIterator<Item = (u64, u64)>>(keys: I, n_hint: usize) -> Self {
        let m_bits = (n_hint.max(1) * 10).next_power_of_two().max(64);
        let mut b = KeyBloom {
            bits: vec![0u64; m_bits / 64],
            mask: m_bits - 1,
            k: 7,
        };
        for key in keys {
            b.insert(key);
        }
        b
    }
    fn pair(key: (u64, u64)) -> (u64, u64) {
        let h1 = splitmix64m(key.0 ^ key.1.rotate_left(32));
        let h2 = splitmix64m(key.0.wrapping_add(0x9E37_79B9_7F4A_7C15) ^ key.1) | 1; // 奇数，保证步长与 m 互质
        (h1, h2)
    }
    fn insert(&mut self, key: (u64, u64)) {
        let (h1, h2) = Self::pair(key);
        for i in 0..self.k as u64 {
            let p = (h1.wrapping_add(i.wrapping_mul(h2)) as usize) & self.mask;
            self.bits[p >> 6] |= 1u64 << (p & 63);
        }
    }
    fn maybe_contains(&self, key: (u64, u64)) -> bool {
        let (h1, h2) = Self::pair(key);
        (0..self.k as u64).all(|i| {
            let p = (h1.wrapping_add(i.wrapping_mul(h2)) as usize) & self.mask;
            self.bits[p >> 6] & (1u64 << (p & 63)) != 0
        })
    }
}

fn splitmix64m(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E3779B97F4A7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D049BB133111EB);
    x ^ (x >> 31)
}

/// 一个段解码折叠后的缓存：全部行 + (trace,span)→行号 索引（行号=段内顺序，删除位图照行号生效）。
struct SegFold {
    rows: Vec<FoldInput>,
    by_key: HashMap<(u64, u64), Vec<u32>>,
}

/// 段折叠缓存（LRU，按总缓存行数封顶；段不可变，命中即用、unlink 时移除）。
struct SegFoldCache {
    cap_rows: usize,
    cur_rows: usize,
    map: HashMap<u64, (Arc<SegFold>, u64)>,
    tick: u64,
}

impl SegFoldCache {
    fn new(cap_rows: usize) -> Self {
        Self {
            cap_rows: cap_rows.max(1),
            cur_rows: 0,
            map: HashMap::new(),
            tick: 0,
        }
    }
    fn remove(&mut self, seg: u64) {
        if let Some((sf, _)) = self.map.remove(&seg) {
            self.cur_rows -= sf.rows.len();
        }
    }
    fn evict(&mut self) {
        let target = (self.cap_rows * 9 / 10).max(1);
        let mut by_tick: Vec<(u64, u64, usize)> = self
            .map
            .iter()
            .map(|(&seg, (sf, t))| (*t, seg, sf.rows.len()))
            .collect();
        by_tick.sort_unstable_by_key(|x| x.0);
        for (_, seg, n) in by_tick {
            if self.cur_rows <= target || self.map.len() <= 1 {
                break;
            }
            self.map.remove(&seg);
            self.cur_rows -= n;
        }
    }
}

/// 一个 span 在边车里的当前聚合（last-non-null 口径，与折叠一致）。用于算会话级差量。
#[derive(Default, Clone)]
struct SpanAgg {
    session: Option<u64>,
    external_session: Option<String>,
    in_tok: u64,
    out_tok: u64,
    cached_tok: u64,
    reasoning_tok: u64,
    total_tok: u64,
    explicit_total_tok: Option<u64>,
    cost_usd_nanos: u64,
    explicit_cost_usd_nanos: Option<u64>,
    error: bool,
    agent: Option<String>,
    trace: u64,
}

/// 一个会话在边车里的增量聚合。
#[derive(Default, Clone)]
struct SessionAgg {
    traces: std::collections::HashSet<u64>,
    external_session: Option<String>,
    in_tok: u64,
    out_tok: u64,
    cached_tok: u64,
    reasoning_tok: u64,
    total_tok: u64,
    cost_usd_nanos: u64,
    error_spans: usize,
    title: String,
    first_trace: u64,
    first_trace_set: bool,
}

/// 控制台会话边车：span 级聚合 + 会话级增量聚合 + 排序结果缓存。
#[derive(Default)]
struct SessionIndex {
    span: HashMap<(u64, u64), SpanAgg>,
    sess: BTreeMap<u64, SessionAgg>,
    /// delete/upgrade 改了段（不走 index_record）→ 标脏，下次读全量重建。
    dirty: bool,
    /// 任何改动 +1；排序结果缓存据此判失效。
    ver: u64,
    cache: Option<(u64, Vec<ConsoleSession>)>,
}

impl SessionIndex {
    /// 把一个 span 的"当前聚合 → 新聚合"差量应用到会话级（增量、O(1)）。
    fn apply_span(&mut self, key: (u64, u64), new: SpanAgg) {
        let old = self.span.get(&key).cloned().unwrap_or_default();
        if old.session != new.session {
            if let Some(os) = old.session {
                self.sub(os, &old);
            }
            if let Some(ns) = new.session {
                self.add(ns, &new);
            }
        } else if let Some(s) = new.session {
            // 同会话：只动 token / error 差量。
            let e = self.sess.entry(s).or_default();
            if e.external_session.is_none() {
                e.external_session = new.external_session.clone();
            }
            e.in_tok = (e.in_tok as i64 + new.in_tok as i64 - old.in_tok as i64).max(0) as u64;
            e.out_tok = (e.out_tok as i64 + new.out_tok as i64 - old.out_tok as i64).max(0) as u64;
            e.cached_tok =
                (e.cached_tok as i64 + new.cached_tok as i64 - old.cached_tok as i64).max(0) as u64;
            e.reasoning_tok = (e.reasoning_tok as i64 + new.reasoning_tok as i64
                - old.reasoning_tok as i64)
                .max(0) as u64;
            e.total_tok =
                (e.total_tok as i64 + new.total_tok as i64 - old.total_tok as i64).max(0) as u64;
            e.cost_usd_nanos = (e.cost_usd_nanos as i128 + new.cost_usd_nanos as i128
                - old.cost_usd_nanos as i128)
                .max(0) as u64;
            e.error_spans =
                (e.error_spans as i64 + new.error as i64 - old.error as i64).max(0) as usize;
            if e.title.is_empty() {
                if let Some(a) = &new.agent {
                    e.title = a.clone();
                }
            }
        }
        self.span.insert(key, new);
        self.ver += 1;
        self.cache = None;
    }

    fn add(&mut self, sid: u64, s: &SpanAgg) {
        let e = self.sess.entry(sid).or_default();
        e.in_tok += s.in_tok;
        e.out_tok += s.out_tok;
        e.cached_tok += s.cached_tok;
        e.reasoning_tok += s.reasoning_tok;
        e.total_tok += s.total_tok;
        e.cost_usd_nanos += s.cost_usd_nanos;
        e.error_spans += s.error as usize;
        e.traces.insert(s.trace);
        if e.external_session.is_none() {
            e.external_session = s.external_session.clone();
        }
        if !e.first_trace_set || s.trace < e.first_trace {
            e.first_trace = s.trace;
            e.first_trace_set = true;
        }
        if e.title.is_empty() {
            if let Some(a) = &s.agent {
                e.title = a.clone();
            }
        }
    }

    fn sub(&mut self, sid: u64, s: &SpanAgg) {
        if let Some(e) = self.sess.get_mut(&sid) {
            e.in_tok = e.in_tok.saturating_sub(s.in_tok);
            e.out_tok = e.out_tok.saturating_sub(s.out_tok);
            e.cached_tok = e.cached_tok.saturating_sub(s.cached_tok);
            e.reasoning_tok = e.reasoning_tok.saturating_sub(s.reasoning_tok);
            e.total_tok = e.total_tok.saturating_sub(s.total_tok);
            e.cost_usd_nanos = e.cost_usd_nanos.saturating_sub(s.cost_usd_nanos);
            e.error_spans = e.error_spans.saturating_sub(s.error as usize);
            // traces / first_trace 不在此精确回收（会话切换极罕见）；delete/upgrade 走标脏重建纠正。
        }
    }

    /// 从折叠 span 全量重建（delete/upgrade 标脏后、或首次）。
    fn rebuild(&mut self, spans: &[FoldedSpan]) {
        self.span.clear();
        self.sess.clear();
        for s in spans {
            let sa = SpanAgg {
                session: s.session_id,
                external_session: s.external_session_id.clone(),
                in_tok: s.input_tokens.unwrap_or(0),
                out_tok: s.output_tokens.unwrap_or(0),
                cached_tok: s.cached_input_tokens.unwrap_or(0),
                reasoning_tok: s.reasoning_tokens.unwrap_or(0),
                total_tok: usage_total_tokens(
                    s.input_tokens.unwrap_or(0),
                    s.output_tokens.unwrap_or(0),
                    s.cached_input_tokens.unwrap_or(0),
                    s.reasoning_tokens.unwrap_or(0),
                    s.total_tokens,
                ),
                explicit_total_tok: s.total_tokens,
                cost_usd_nanos: usage_cost_usd_nanos_for_model(
                    s.input_tokens.unwrap_or(0),
                    s.output_tokens.unwrap_or(0),
                    s.cached_input_tokens.unwrap_or(0),
                    s.reasoning_tokens.unwrap_or(0),
                    s.cost_usd_nanos,
                    s.provider.as_deref(),
                    s.model.as_deref(),
                ),
                explicit_cost_usd_nanos: s.cost_usd_nanos,
                error: s.status.unwrap_or(0) != 0,
                agent: s.agent_name.clone(),
                trace: s.trace_id,
            };
            if let Some(sid) = sa.session {
                self.add(sid, &sa);
            }
            self.span.insert((s.trace_id, s.span_id), sa);
        }
        self.dirty = false;
        self.ver += 1;
        self.cache = None;
    }

    /// 产出按 session_id 降序的会话行（带缓存，ver 没变直接复用）。
    fn rows(&mut self) -> Vec<ConsoleSession> {
        if let Some((v, c)) = &self.cache {
            if *v == self.ver {
                return c.clone();
            }
        }
        let mut out: Vec<ConsoleSession> = self
            .sess
            .iter()
            .map(|(sid, a)| ConsoleSession {
                session_id: *sid,
                external_session_id: a.external_session.clone(),
                title: if a.title.is_empty() {
                    format!("会话 {sid}")
                } else {
                    a.title.clone()
                },
                turn_count: a.traces.len(),
                input_tokens: a.in_tok,
                output_tokens: a.out_tok,
                cached_input_tokens: a.cached_tok,
                reasoning_tokens: a.reasoning_tok,
                total_tokens: a.total_tok,
                cost_usd_nanos: a.cost_usd_nanos,
                has_error: a.error_spans > 0,
                first_trace_id: a.first_trace,
            })
            .collect();
        out.sort_by(|a, b| b.session_id.cmp(&a.session_id));
        self.cache = Some((self.ver, out.clone()));
        out
    }
}

/// 引擎构造器：注入自定义检索索引（团队 jieba 分词的 BM25、自有 graph_index）后再起引擎。
/// 不传 = 用默认（bigram BM25 / 内置图式 ANN），所以现有 `WriteCoordinator::new/open/open_durable`
/// 行为不变。外部隔离 crate（如 jieba FFI）走这里把实现接进来，骨架本身仍零依赖。
///
/// ```ignore
/// // 团队 jieba 库就位后：
/// let eng = CoordinatorBuilder::new()
///     .with_tokenizer(Box::new(JiebaTokenizer::open("dict/")?)) // 只换分词层
///     .open_durable("/data/trace")?;
/// ```
#[derive(Default)]
pub struct CoordinatorBuilder {
    bm25: Option<Arc<dyn Bm25Index>>,
    graph: Option<Arc<dyn GraphIndex>>,
    /// 持久模式磁盘向量索引的参数（缓冲预算 / m / ef）。None = 默认。仅在没注入自定义 graph 时生效。
    vec_cfg: Option<DiskGraphConfig>,
    trace_rollup_profile_config: TraceRollupProfileConfig,
}

impl CoordinatorBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// 整体替换 BM25 实现（最一般）。
    pub fn with_bm25(mut self, bm25: Arc<dyn Bm25Index>) -> Self {
        self.bm25 = Some(bm25);
        self
    }

    /// 便捷：只换 BM25 的分词器（团队 jieba 词级分词），倒排与评分仍用自有 `Bm25TextIndex`。
    pub fn with_tokenizer(self, tokenizer: Box<dyn Tokenizer>) -> Self {
        self.with_bm25(Arc::new(Bm25TextIndex::with_tokenizer(tokenizer)))
    }

    /// 替换向量 ANN 实现（接团队 graph_index 时用）。
    pub fn with_graph(mut self, graph: Arc<dyn GraphIndex>) -> Self {
        self.graph = Some(graph);
        self
    }

    /// 设持久磁盘向量索引的**缓冲预算（字节）**，如 `1 << 30` = 1GiB。仅没注入自定义 graph 时生效。
    pub fn with_vector_cache_bytes(mut self, bytes: usize) -> Self {
        self.vec_cfg = Some(self.vec_cfg.unwrap_or_default().with_cache_bytes(bytes));
        self
    }

    /// 设**建图候选列表宽度 `ef_construction`**（对齐 graph_index）：越大召回越好、建图越慢；
    /// 想要更快建图就调小（如 32），是建图速度/召回的主旋钮。默认 64。仅没注入自定义 graph 时生效。
    pub fn with_ef_construction(mut self, ef: usize) -> Self {
        self.vec_cfg = Some(self.vec_cfg.unwrap_or_default().with_ef_construction(ef));
        self
    }

    /// 设**查询候选列表宽度 `ef_search`**（对齐 `hnsw_ef_search`）：越大召回越高、查询越慢。默认 100。
    pub fn with_ef_search(mut self, ef: usize) -> Self {
        self.vec_cfg = Some(self.vec_cfg.unwrap_or_default().with_ef_search(ef));
        self
    }

    /// 设持久磁盘向量索引的完整参数（缓冲预算 / m / ef_construction / ef_search）。仅没注入自定义 graph 时生效。
    pub fn with_disk_graph_config(mut self, cfg: DiskGraphConfig) -> Self {
        self.vec_cfg = Some(cfg);
        self
    }

    /// 控制 trace rollup 预聚合 profile 的物化预算。默认 full；显式收紧时，缺失 profile 会回退慢路径。
    pub fn with_trace_rollup_profile_config(mut self, cfg: TraceRollupProfileConfig) -> Self {
        self.trace_rollup_profile_config = cfg;
        self
    }

    /// 便捷设置 storageStats / traceAggregate 各自最多物化多少个 profile family。
    pub fn with_trace_rollup_profile_limits(
        mut self,
        storage_limit: usize,
        aggregate_limit: usize,
    ) -> Self {
        self.trace_rollup_profile_config = self
            .trace_rollup_profile_config
            .with_storage_profile_limit(storage_limit)
            .with_aggregate_profile_limit(aggregate_limit);
        self
    }

    /// 限制单个 profile 的最大 bucket 数。超限时整个 profile 不物化，避免返回截断聚合。
    pub fn with_trace_rollup_profile_bucket_limit(mut self, limit: usize) -> Self {
        self.trace_rollup_profile_config = self
            .trace_rollup_profile_config
            .with_max_buckets_per_profile(limit);
        self
    }

    /// 内存 WAL（测试/开发）。
    pub fn build(self, segments: Arc<dyn SegmentStore>) -> Arc<WriteCoordinator> {
        WriteCoordinator::build_full(
            segments,
            Wal::new(),
            Manifest::empty(),
            1,
            1,
            None,
            None,
            self.bm25,
            self.graph,
            self.trace_rollup_profile_config,
            None,
        )
    }

    /// 文件 WAL。
    pub fn open(
        self,
        segments: Arc<dyn SegmentStore>,
        wal_path: impl AsRef<std::path::Path>,
    ) -> std::io::Result<Arc<WriteCoordinator>> {
        Ok(WriteCoordinator::build_full(
            segments,
            Wal::open(wal_path)?,
            Manifest::empty(),
            1,
            1,
            None,
            None,
            self.bm25,
            self.graph,
            self.trace_rollup_profile_config,
            None,
        ))
    }

    /// 全持久化引擎（与 `WriteCoordinator::open_durable` 同语义，外加注入的索引 / 磁盘向量索引参数）。
    pub fn open_durable(
        self,
        dir: impl AsRef<std::path::Path>,
    ) -> std::io::Result<Arc<WriteCoordinator>> {
        WriteCoordinator::open_durable_inner(
            dir,
            self.bm25,
            self.graph,
            self.vec_cfg,
            self.trace_rollup_profile_config,
        )
    }
}

include!("write_coordinator_open_ingest.rs");
include!("write_coordinator_text_domains.rs");
include!("write_coordinator_named_vectors.rs");
include!("write_coordinator_trace_aggregate_rollup.rs");
include!("write_coordinator_metadata_index.rs");
include!("write_coordinator_read_query.rs");
include!("write_coordinator_trace_views.rs");
include!("write_coordinator_metadata_eval.rs");
include!("write_coordinator_tree_graph.rs");
include!("write_coordinator_search.rs");
include!("write_coordinator_recovery_commit.rs");
include!("write_coordinator_metrics_migrate.rs");

#[cfg(test)]
#[path = "lib/tests.rs"]
mod tests;
