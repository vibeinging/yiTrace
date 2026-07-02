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

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use yt_core::chunk::{DeletionVec, UpgradeColChunk};
use yt_core::event::{EventIdentity, EventType};
use yt_core::fold::{fold_events, FoldInput, FoldedSpan, SpanFields};
use yt_core::ids::{SegmentId, WalLsn};
use yt_core::rank::rrf_fuse;
use yt_core::manifest::{Manifest, SegState, SegmentEntry};
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

mod persist;
mod vecstore;

mod gc_log;

pub mod olog;

mod vecindex_disk;
pub use vecindex_disk::{DiskGraphConfig, DiskGraphIndex, DiskGraphStore, DurableGraphIndex};

mod http;
pub use http::HttpIngestServer;

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
pub struct Projection(u16);

impl Projection {
    pub const STATUS: u16 = 1 << 0;
    pub const DURATION_NS: u16 = 1 << 1;
    pub const PARENT_SPAN_ID: u16 = 1 << 2;
    pub const INPUT_TOKENS: u16 = 1 << 3;
    pub const OUTPUT_TOKENS: u16 = 1 << 4;
    pub const SESSION_ID: u16 = 1 << 5;
    pub const AGENT_NAME: u16 = 1 << 6;
    pub const TOOL_NAME: u16 = 1 << 7;
    pub const MODEL: u16 = 1 << 8;
    pub const INPUT_TEXT: u16 = 1 << 9;
    pub const OUTPUT_TEXT: u16 = 1 << 10;
    pub const EVAL_SCORE: u16 = 1 << 11;
    pub const EVAL_LABEL: u16 = 1 << 12;
    pub const LOGS: u16 = 1 << 13;
    pub const TENANT_ID: u16 = 1 << 14;

    const MASK: u16 = (1 << 15) - 1;

    /// 全列（含两个大文本列）。普通读 / trace 详情 / eval 打分 / 数据集采集要原文，用这个。
    pub const ALL: Projection = Projection(Self::MASK);

    /// 选定若干列（位或）。如 `Projection::of(Projection::AGENT_NAME | Projection::INPUT_TOKENS)`。
    pub const fn of(cols: u16) -> Self {
        Projection(cols & Self::MASK)
    }

    /// 该投影是否要某列（传列常量）。
    pub const fn has(self, col: u16) -> bool {
        self.0 & col != 0
    }

    /// 是否要全部列——存储据此走"读全列"快路（与历史行为完全一致），不必逐列裁剪。
    pub const fn is_all(self) -> bool {
        self.0 == Self::MASK
    }

    /// 原始位（列式存储据此判断每列读不读）。
    pub const fn bits(self) -> u16 {
        self.0
    }
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
    fn scan_fold_inputs_projected(&self, _seg: SegmentId, _proj: Projection) -> Option<Vec<(u32, FoldInput)>> {
        None
    }

    /// 可选：**按时间范围下推扫描 + 投影**，返回 `ts ∈ [from, to]` 命中行的 `FoldInput`（不带物理行号），
    /// 只解码 `proj` 选中的列。默认 `None` = 不支持下推，引擎回退全扫。列式存储（Vortex）覆盖它，把时间
    /// 过滤推进文件扫描、只解码命中行的命中列。
    /// **注意**：下推丢了物理行号，而删除按物理行号定位，二者不能共存——引擎只在「段无删除」时用它。
    fn scan_fold_inputs_in_time(&self, _seg: SegmentId, _from: i64, _to: i64, _proj: Projection) -> Option<Vec<FoldInput>> {
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
        self.counts.lock().unwrap().get(&seg.get()).map_or(false, |&c| c > 0)
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
    fn search(&self, query: &[f32], k: usize, filter: &dyn Fn(u64, u64) -> bool) -> Vec<(u64, u64, f32)>;
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
        self.vecs.lock().unwrap().insert((trace_id, span_id), embedding);
    }
    fn search(&self, query: &[f32], k: usize, filter: &dyn Fn(u64, u64) -> bool) -> Vec<(u64, u64, f32)> {
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
    a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).sum::<f32>().sqrt()
}

/// 内存段存储（默认实现 / demo / 测试用）。真实实现换 Vortex 列式不可变段。
/// `unlink` 真删 —— 配合回收水位，过早回收会让读者读到空（被压测当场抓住）。
#[derive(Default)]
pub struct InMemorySegmentStore {
    rows: Mutex<BTreeMap<u64, Vec<WalRecord>>>,
}
impl SegmentStore for InMemorySegmentStore {
    fn flush_to_segment(&self, seg: SegmentId, records: &[WalRecord]) {
        self.rows.lock().unwrap().insert(seg.get(), records.to_vec());
    }
    fn scan_fold_inputs(&self, seg: SegmentId) -> Vec<(u32, FoldInput)> {
        self.rows
            .lock()
            .unwrap()
            .get(&seg.get())
            .map(|rs| rs.iter().enumerate().map(|(i, r)| (i as u32, r.to_fold_input())).collect())
            .unwrap_or_default()
    }
    fn scan_records(&self, seg: SegmentId) -> Vec<WalRecord> {
        self.rows.lock().unwrap().get(&seg.get()).cloned().unwrap_or_default()
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
        Self { trace_id: None, time_from: i64::MIN, time_to: i64::MAX, tenant_id: None }
    }
    pub fn trace(trace_id: u64, time_from: i64, time_to: i64) -> Self {
        Self { trace_id: Some(trace_id), time_from, time_to, tenant_id: None }
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
    min_ts: i64,
    max_ts: i64,
    /// 租户隔离维度（last-non-null）。
    tenant_id: Option<u64>,
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
        true
    }
}

/// 一条 trace 的摘要（web 控制台列表视图用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceSummary {
    pub trace_id: u64,
    pub span_count: usize,
    pub total_duration_ns: u64,
    pub max_duration_ns: u64,
    /// 状态非 0 的 span 数（报错）。
    pub error_count: usize,
    /// 全 trace 输入/输出 token 汇总（成本指标）。
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
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
    pub title: String,
    pub turn_count: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub has_error: bool,
    pub first_trace_id: u64,
}

/// 控制台瀑布的一行 span（kind/name/起始时刻为派生值，见 `console_trace_spans`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleSpan {
    pub span_id: u64,
    pub parent_span_id: Option<u64>,
    pub kind: &'static str,
    pub name: String,
    pub start_ns: u64,
    pub duration_ns: u64,
    pub has_error: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub model: Option<String>,
    pub input_text: Option<String>,
    pub output_text: Option<String>,
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
        Self { score: score.min(1000), label: label.into() }
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
        Self { bad_words: bad_words.iter().map(|s| s.to_string()).collect() }
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
fn aggregate_eval(scored: impl Iterator<Item = (Option<String>, u32)>, pass_threshold: u32) -> Vec<EvalSummary> {
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
        avg_score: if scored == 0 { 0 } else { (sum / scored as u64) as u32 },
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
    pub session_id: Option<u64>,
    pub tenant_id: Option<u64>,
    pub agent_name: Option<String>,
    pub tool_name: Option<String>,
    pub model: Option<String>,
    pub input_text: Option<String>,
    pub output_text: Option<String>,
    pub logs: Vec<String>,
}

impl WireRecord {
    fn into_wal_record(self) -> WalRecord {
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
                session_id: self.session_id,
                tenant_id: self.tenant_id,
                agent_name: self.agent_name,
                tool_name: self.tool_name,
                model: self.model,
                input_text: self.input_text,
                output_text: self.output_text,
                eval_score: None,  // 分数由 scorer 事后算、走 upgrade 补写，不从线上摄入
                eval_label: None,
                logs: self.logs,
            },
        }
    }
}

// ───────────────────────── 单写者协调器 ─────────────────────────

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
    /// 向量 ANN（找相似）。真实实现接团队 graph_index。
    graph: Arc<dyn GraphIndex>,
    /// 内存表行数超过此值就自动刷盘（兜住内存上界，OPEN-2）。
    flush_threshold: AtomicUsize,
    /// 段身份分配器（单写者下无并发分配竞争，永不复用）。
    next_segment_id: Mutex<u64>,
    next_chunk_id: Mutex<u64>,
    /// 评测数据集（按名）。元数据,不进 trace 存储;eval 的"燃料"与回归基准。
    datasets: Mutex<BTreeMap<String, Dataset>>,
    /// manifest 持久化路径。Some = 每次 commit 后原子写盘（重启不丢）；None = 纯内存。
    manifest_path: Option<std::path::PathBuf>,
    /// 向量独立落盘路径。Some = `index_embedding` 追加写盘、`recover` 重载（向量不在 trace 数据里,
    /// 段重建不出来,只能单独持久）；None = 纯内存。
    vector_path: Option<std::path::PathBuf>,
    /// 检索过滤的属性边车：(trace,span) → 可过滤元数据（带过滤 ANN 的 payload）。
    /// 派生数据：摄入时建,`recover` 时从持久段重建。
    filter_attrs: Mutex<HashMap<(u64, u64), FilterAttrs>>,
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
        let mut b = KeyBloom { bits: vec![0u64; m_bits / 64], mask: m_bits - 1, k: 7 };
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
        Self { cap_rows: cap_rows.max(1), cur_rows: 0, map: HashMap::new(), tick: 0 }
    }
    fn remove(&mut self, seg: u64) {
        if let Some((sf, _)) = self.map.remove(&seg) {
            self.cur_rows -= sf.rows.len();
        }
    }
    fn evict(&mut self) {
        let target = (self.cap_rows * 9 / 10).max(1);
        let mut by_tick: Vec<(u64, u64, usize)> =
            self.map.iter().map(|(&seg, (sf, t))| (*t, seg, sf.rows.len())).collect();
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
    in_tok: u64,
    out_tok: u64,
    error: bool,
    agent: Option<String>,
    trace: u64,
}

/// 一个会话在边车里的增量聚合。
#[derive(Default, Clone)]
struct SessionAgg {
    traces: std::collections::HashSet<u64>,
    in_tok: u64,
    out_tok: u64,
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
            e.in_tok = (e.in_tok as i64 + new.in_tok as i64 - old.in_tok as i64).max(0) as u64;
            e.out_tok = (e.out_tok as i64 + new.out_tok as i64 - old.out_tok as i64).max(0) as u64;
            e.error_spans = (e.error_spans as i64 + new.error as i64 - old.error as i64).max(0) as usize;
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
        e.error_spans += s.error as usize;
        e.traces.insert(s.trace);
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
                in_tok: s.input_tokens.unwrap_or(0),
                out_tok: s.output_tokens.unwrap_or(0),
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
                title: if a.title.is_empty() { format!("会话 {sid}") } else { a.title.clone() },
                turn_count: a.traces.len(),
                input_tokens: a.in_tok,
                output_tokens: a.out_tok,
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

    /// 内存 WAL（测试/开发）。
    pub fn build(self, segments: Arc<dyn SegmentStore>) -> Arc<WriteCoordinator> {
        WriteCoordinator::build_full(segments, Wal::new(), Manifest::empty(), 1, 1, None, None, self.bm25, self.graph, None)
    }

    /// 文件 WAL。
    pub fn open(self, segments: Arc<dyn SegmentStore>, wal_path: impl AsRef<std::path::Path>) -> std::io::Result<Arc<WriteCoordinator>> {
        Ok(WriteCoordinator::build_full(segments, Wal::open(wal_path)?, Manifest::empty(), 1, 1, None, None, self.bm25, self.graph, None))
    }

    /// 全持久化引擎（与 `WriteCoordinator::open_durable` 同语义，外加注入的索引 / 磁盘向量索引参数）。
    pub fn open_durable(self, dir: impl AsRef<std::path::Path>) -> std::io::Result<Arc<WriteCoordinator>> {
        WriteCoordinator::open_durable_inner(dir, self.bm25, self.graph, self.vec_cfg)
    }
}

impl WriteCoordinator {
    /// 内存 WAL（测试/开发，不落盘）。
    pub fn new(segments: Arc<dyn SegmentStore>) -> Arc<Self> {
        Self::build(segments, Wal::new())
    }

    /// 文件 WAL（真落盘）：重启后用同一路径 `open` + `recover()` 可从盘上重放(WAL 持久化)。
    /// 注意：段/manifest 不持久化,崩溃后靠 WAL 全量重放进 MemTable 恢复。要"flush 后重启不丢"用 `open_durable`。
    pub fn open(segments: Arc<dyn SegmentStore>, wal_path: impl AsRef<std::path::Path>) -> std::io::Result<Arc<Self>> {
        Ok(Self::build_full(segments, Wal::open(wal_path)?, Manifest::empty(), 1, 1, None, None, None, None, None))
    }

    /// **全持久化引擎**：一个目录下放段(`segments/`)+ WAL(`wal.log`)+ manifest(`manifest.dat`)。
    /// 重启用同一目录 `open_durable` + `recover()`：先从 manifest 重建段集合(指向盘上段文件)、再 WAL 重放
    /// 水位之后的尾巴 —— **flush 过的数据(水位之前、WAL 不再重放)从持久段读回,真正重启不丢**。
    pub fn open_durable(dir: impl AsRef<std::path::Path>) -> std::io::Result<Arc<Self>> {
        Self::open_durable_inner(dir, None, None, None)
    }

    /// open_durable 的内部实现，多收可选索引覆盖 + 磁盘向量索引参数（[`CoordinatorBuilder`] 用它注入）。
    fn open_durable_inner(
        dir: impl AsRef<std::path::Path>,
        bm25: Option<Arc<dyn Bm25Index>>,
        graph: Option<Arc<dyn GraphIndex>>,
        vec_cfg: Option<DiskGraphConfig>,
    ) -> std::io::Result<Arc<Self>> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir)?;
        let segments = Arc::new(FileSegmentStore::open(dir.join("segments"))?);
        let wal = Wal::open(dir.join("wal.log"))?;
        let manifest_path = dir.join("manifest.dat");
        let gc_log_path = dir.join("gc.log");
        // 有持久 manifest 就从它恢复段集合与 id 计数器；否则从空开始。
        let (manifest, next_seg, next_chunk) = match persist::load(&manifest_path) {
            Some(s) => (s.manifest, s.next_segment_id, s.next_chunk_id),
            None => (Manifest::empty(), 1, 1),
        };
        // 默认向量索引 = **磁盘图索引**（向量+图都落盘、重启不 rebuild、append 友好），不用 vecstore。
        // 注入了自定义 graph（可能内存型）则保留 vecstore 重建路径（向后兼容）。
        let (graph, vector_path): (Option<Arc<dyn GraphIndex>>, Option<std::path::PathBuf>) = match graph {
            Some(g) => (Some(g), Some(dir.join("vectors.dat"))),
            None => {
                let disk = DurableGraphIndex::open(dir.join("vecindex"), vec_cfg.unwrap_or_default());
                (Some(Arc::new(disk) as Arc<dyn GraphIndex>), None)
            }
        };
        let coord = Self::build_full(segments, wal, manifest, next_seg, next_chunk, Some(manifest_path), vector_path, bm25, graph, Some(dir.to_path_buf()));
        // 打开 GC 日志，先补删上次崩溃残留的"MARK 没 DONE"段（崩溃安全），再装上。
        let entries = gc_log::GcLog::scan(&gc_log_path).unwrap_or_default();
        for seg in gc_log::pending_deletions(&entries) {
            // 段文件可能已删了一半（崩溃在 unlink 中）；补删幂等（不存在就跳过）。
            coord.segments.unlink_segment(SegmentId(seg));
            // 这些段上次崩溃前 manifest 已不引用（reclaim 前提），不用动 manifest。
            // 段 id 不复用、dead_set 是内存态重启后清空，所以不用动 dead_set。
        }
        // 重置 gc.log：已补删的不再记；之后 reclaim 重新记新意图。truncate 即可。
        let _ = std::fs::write(&gc_log_path, b"");
        // GC 日志和 WAL/manifest 同等重要（崩溃安全的承重组件）——打开失败必须 fail-fast，
        // 不能静默降级成"无 GC 日志、reclaim 直接删"（那样崩溃恢复失效且无人知晓）。
        let log = gc_log::GcLog::open(&gc_log_path)?;
        *coord.gc_log.lock().unwrap() = Some(log);
        Ok(coord)
    }

    fn build(segments: Arc<dyn SegmentStore>, wal: Wal) -> Arc<Self> {
        Self::build_full(segments, wal, Manifest::empty(), 1, 1, None, None, None, None, None)
    }

    #[allow(clippy::too_many_arguments)]
    fn build_full(
        segments: Arc<dyn SegmentStore>,
        wal: Wal,
        manifest: Manifest,
        next_segment_id: u64,
        next_chunk_id: u64,
        manifest_path: Option<std::path::PathBuf>,
        vector_path: Option<std::path::PathBuf>,
        bm25: Option<Arc<dyn Bm25Index>>,
        graph: Option<Arc<dyn GraphIndex>>,
        dir: Option<std::path::PathBuf>,
    ) -> Arc<Self> {
        Arc::new(Self {
            write_lock: Mutex::new(()),
            current: Current::new(manifest),
            wal: Mutex::new(wal),
            memtable: Mutex::new(MemTable::new()),
            segments,
            dead_set: Mutex::new(Vec::new()),
            buffer_pins: BufferPins::default(),
            // 默认 BM25 用纯 Rust 中文词级分词（jieba 全量词典，开箱即生产级）/ 图式 ANN；
            // 可被 builder 注入覆盖（团队 jieba FFI、bigram、或叠了自有词典的 ChineseTokenizer）。
            bm25: bm25.unwrap_or_else(|| Arc::new(Bm25TextIndex::with_tokenizer(Box::new(ChineseTokenizer::full())))),
            graph: graph.unwrap_or_else(|| Arc::new(GraphAnnIndex::default())),
            flush_threshold: AtomicUsize::new(4096),
            next_segment_id: Mutex::new(next_segment_id),
            next_chunk_id: Mutex::new(next_chunk_id),
            datasets: Mutex::new(BTreeMap::new()),
            manifest_path,
            vector_path,
            filter_attrs: Mutex::new(HashMap::new()),
            session_idx: Mutex::new(SessionIndex::default()),
            seg_fold_cache: Mutex::new(SegFoldCache::new(2_000_000)), // 缓存上限 ~200 万行
            seg_key_bloom: Mutex::new(HashMap::new()),
            gc_log: Mutex::new(None), // open_durable 设成 Some；非持久模式保持 None
            dir,

        })
    }

    /// commit 后若开了持久化,原子写 manifest（含 id 计数器）。崩溃在写 manifest 前 = 退回上个 manifest
    /// （那次 commit 的段文件成孤儿,无害,等回收或忽略）；写后 = 新状态生效。两边都不脏读。
    fn persist_manifest(&self) {
        let Some(path) = &self.manifest_path else { return };
        let state = persist::PersistedState {
            manifest: (*self.current.manifest()).clone(),
            next_segment_id: *self.next_segment_id.lock().unwrap(),
            next_chunk_id: *self.next_chunk_id.lock().unwrap(),
        };
        let _ = persist::save(path, &state);
        // 提交点：向量索引批量刷盘（append 期间只写不刷，靠这里持久；删除少、append 多场景的吞吐取舍）。
        self.graph.flush();
    }

    /// 提交新 manifest 版本并（若开了持久化）落盘。所有 commit 走这里,保证段集合改动都持久。
    fn commit_and_persist(&self, draft: Manifest) {
        self.current.commit(draft);
        self.persist_manifest();
    }

    /// 读者入口：pin 一个一致快照（委托给 yt-manifest）。
    pub fn pin_snapshot(&self) -> Snapshot {
        self.current.pin_snapshot()
    }

    /// 把一条记录喂进**派生检索索引**：BM25 中文倒排 + 过滤属性边车。
    /// ingest、WAL 重放、从段重建索引三处共用 —— 派生索引的喂法只此一份。
    fn index_record(&self, r: &WalRecord) {
        // 中文倒排：把该 span 的**可检索文本**喂进 BM25。检索的主对象是 LLM 的输入/输出原文
        // （input_text/output_text），logs（含 span name）作补充。三者拼起来索引——真实 SDK 灌进来的
        // input/output 文本会被索引，而不是只索引 logs（否则真实数据上"中文检索"会突然失效）。
        let mut parts: Vec<&str> = Vec::new();
        if let Some(t) = r.fields.input_text.as_deref() {
            parts.push(t);
        }
        if let Some(t) = r.fields.output_text.as_deref() {
            parts.push(t);
        }
        // agent/tool/model 名也索引——用户会按"搜某个 agent/tool 的 trace"（如"搜风控 agent 的报错"）。
        for field in [&r.fields.agent_name, &r.fields.tool_name, &r.fields.model] {
            if let Some(t) = field.as_deref() {
                parts.push(t);
            }
        }
        for l in &r.fields.logs {
            parts.push(l);
        }
        if !parts.is_empty() {
            self.bm25.index_text(r.trace_id, r.span_id, &parts.join(" "));
        }
        // 过滤属性边车：last-non-null 累积 status/agent，ts 取范围（带过滤 ANN 的 payload）。
        let mut fa = self.filter_attrs.lock().unwrap();
        let a = fa.entry((r.trace_id, r.span_id)).or_insert(FilterAttrs { min_ts: r.ts, max_ts: r.ts, ..Default::default() });
        if r.fields.status.is_some() {
            a.status = r.fields.status;
        }
        if r.fields.agent_name.is_some() {
            a.agent_name = r.fields.agent_name.clone();
        }
        if r.fields.tenant_id.is_some() {
            a.tenant_id = r.fields.tenant_id;
        }
        a.min_ts = a.min_ts.min(r.ts);
        a.max_ts = a.max_ts.max(r.ts);
        drop(fa);

        // 会话边车：用 last-non-null 算出该 span 的新聚合，差量更新会话级（增量、O(1)/事件）。
        let key = (r.trace_id, r.span_id);
        let mut idx = self.session_idx.lock().unwrap();
        let mut new = idx.span.get(&key).cloned().unwrap_or_default();
        new.trace = r.trace_id;
        if let Some(s) = r.fields.session_id {
            new.session = Some(s);
        }
        if let Some(t) = r.fields.input_tokens {
            new.in_tok = t;
        }
        if let Some(t) = r.fields.output_tokens {
            new.out_tok = t;
        }
        if let Some(st) = r.fields.status {
            new.error = st != 0;
        }
        if new.agent.is_none() {
            if let Some(a) = &r.fields.agent_name {
                new.agent = Some(a.clone());
            }
        }
        idx.apply_span(key, new);
    }

    /// 写入：先进 WAL（ack 后才算持久），同步进活 MemTable，再推进已提交尾。
    /// 折叠在读时做，所以写路径不需要「脏队列」（决策文档已去掉 fold_dirty）。
    /// 整个过 write_lock 串行（单写者）。
    pub fn ingest(&self, records: Vec<WalRecord>) -> WalLsn {
        let _w = self.write_lock.lock().unwrap();
        let mut wal = self.wal.lock().unwrap();
        // 这批的起始 LSN（在 append 之前确定），逐条分配 commit_lsn。
        let first = wal.committed_tail().get() + 1;
        {
            let mut mt = self.memtable.lock().unwrap();
            for (i, r) in records.iter().enumerate() {
                self.index_record(r); // 喂检索索引（BM25 + 属性边车）
                mt.append(MemRow {
                    commit_lsn: first + i as u64,
                    trace_id: r.trace_id,
                    span_id: r.span_id,
                    ts: r.ts,
                    identity: r.identity.clone(),
                    fields: r.fields.clone(),
                });
            }
        }
        let last = wal.append_committed(records);
        drop(wal);
        // ack 之后才推进 committed_tail（读者据此取 live_lsn 上界）。
        self.current.advance_committed_tail(last);

        // 内存表超阈值就自动刷盘，兜住内存上界（OPEN-2）。仍在 write_lock 下。
        if self.memtable.lock().unwrap().len() >= self.flush_threshold.load(Ordering::Relaxed) {
            self.flush_memtable_locked();
        }
        // 会话边车已在 index_record 里逐事件增量维护，这里无需额外动作。
        let n = first;
        let cnt = last.get() - first + 1;
        let tail = last.get();
        olog::log(olog::Level::Debug, "ingest", &[("lsn", &n), ("count", &cnt), ("tail", &tail)]);
        last
    }

    /// 摄入 SDK 线格式记录：转成内部 WalRecord（引擎自算 event_id）后走正常 `ingest`。
    /// 这是「打点 → 引擎存」的数据契约入口；上面再套一层 HTTP/OTLP 网关即闭环（网关是纯管道）。
    pub fn ingest_wire(&self, records: Vec<WireRecord>) -> WalLsn {
        let recs: Vec<WalRecord> = records.into_iter().map(WireRecord::into_wal_record).collect();
        self.ingest(recs)
    }

    /// HTTP 网关专用摄入：租户来自鉴权上下文（如 `X-Tenant-Id`），覆盖 wire body 里的 tenant_id。
    /// 这是多租户安全边界；SDK/客户端可以重复发送 body，但不能自选或伪造租户。
    pub fn ingest_wire_for_tenant(&self, mut records: Vec<WireRecord>, tenant: Option<u64>) -> WalLsn {
        for r in &mut records {
            r.tenant_id = tenant;
        }
        self.ingest_wire(records)
    }

    /// 摄入 OTLP/OpenInference 标准 trace（OTLP/HTTP JSON）：经适配器映射成 WireRecord 后走正常摄入。
    /// 这是「生态入口」——已用 OpenTelemetry / OpenInference 埋点的 agent 应用不改打点即可灌进来。
    /// 解析失败返回 Err（调用方/HTTP 网关据此回 400）。
    pub fn ingest_otlp(&self, body: &str) -> Result<WalLsn, String> {
        let wires = parse_otlp_traces(body)?;
        Ok(self.ingest_wire(wires))
    }

    /// HTTP 网关专用 OTLP 摄入：OTLP attributes 里带的 tenant 也不作为安全边界，统一由请求上下文覆盖。
    pub fn ingest_otlp_for_tenant(&self, body: &str, tenant: Option<u64>) -> Result<WalLsn, String> {
        let wires = parse_otlp_traces(body)?;
        Ok(self.ingest_wire_for_tenant(wires, tenant))
    }

    /// 设置内存表自动刷盘阈值（行数）。
    pub fn set_flush_threshold(&self, n: usize) {
        self.flush_threshold.store(n.max(1), Ordering::Relaxed);
    }

    /// 当前内存表行数（可观测 / 测试用）。
    pub fn memtable_len(&self) -> usize {
        self.memtable.lock().unwrap().len()
    }

    /// 主动把内存表当前内容封成一个段（周期刷盘 / 关机前）。
    pub fn flush_memtable(&self) {
        let _w = self.write_lock.lock().unwrap();
        let before = self.memtable.lock().unwrap().len();
        let v_before = self.current.version();
        self.flush_memtable_locked();
        let seg = v_before;
        olog::log(olog::Level::Info, "flush", &[
            ("seg", &seg),
            ("rows", &before),
            ("version", &self.current.version()),
        ]);
    }

    /// 把内存表内容封段（调用方须已持 write_lock）。watermark 推进到内存表最新 LSN。
    fn flush_memtable_locked(&self) {
        let (records, max_lsn) = {
            let mt = self.memtable.lock().unwrap();
            if mt.is_empty() {
                return;
            }
            let records: Vec<WalRecord> = mt
                .iter()
                .map(|r| WalRecord {
                    trace_id: r.trace_id,
                    span_id: r.span_id,
                    ts: r.ts,
                    identity: r.identity.clone(),
                    fields: r.fields.clone(),
                })
                .collect();
            (records, mt.newest_lsn().unwrap())
        };
        let seg = self.alloc_segment_id();
        self.segments.flush_to_segment(seg, &records);
        // 段级 key bloom：从这批记录的 (trace,span) 建，供检索折叠定位跳过无关段。
        let bloom = KeyBloom::build(records.iter().map(|r| (r.trace_id, r.span_id)), records.len());
        self.seg_key_bloom.lock().unwrap().insert(seg.get(), Arc::new(bloom));
        let (min_ts, max_ts) = ts_range(&records);
        let mut draft = self.current.cow_next();
        draft.memtable_watermark = WalLsn::new(max_lsn);
        draft.segments.insert(
            seg.get(),
            SegmentEntry {
                segment_id: seg,
                level: 0,
                state: SegState::Live,
                min_ts,
                max_ts,
                deletion_vec: Arc::new(DeletionVec::empty()),
                deletion_seq: 0,
                upgrade_ref: None,
                upgrade_seq: 0,
            },
        );
        self.commit_and_persist(draft);
        let gate = WalLsn::new(self.current.min_retained_watermark());
        self.memtable.lock().unwrap().evict_up_to(gate);
    }

    /// 读 MemTable 源：某快照可见的半开区间 `(retained_watermark, live_lsn]`（测试/折叠用）。
    pub fn read_memtable_lsns(&self, snap: &Snapshot) -> Vec<u64> {
        self.memtable
            .lock()
            .unwrap()
            .read_range(snap.retained_watermark, snap.live_lsn)
            .map(|r| r.commit_lsn)
            .collect()
    }

    /// 读路径：在固定快照上跨四源折叠出可见的所有 span（草案 2 §D2.2 端到端，全开窗）。
    pub fn read_spans(&self, snap: &Snapshot) -> Vec<FoldedSpan> {
        self.read_spans_query(snap, &TraceQuery::all()).0
    }

    /// 取某段的折叠缓存（不可变段，首次解码全列 + 建 (trace,span)→行号 索引，之后命中直接用）。
    fn seg_fold(&self, seg: SegmentId) -> Arc<SegFold> {
        {
            let mut c = self.seg_fold_cache.lock().unwrap();
            c.tick += 1;
            let t = c.tick;
            if let Some(e) = c.map.get_mut(&seg.get()) {
                e.1 = t;
                return e.0.clone();
            }
        }
        // 未命中：在锁外解码整段一次（之后所有查询命中缓存）。
        let raw = self.segments.scan_fold_inputs(seg);
        let mut rows = Vec::with_capacity(raw.len());
        let mut by_key: HashMap<(u64, u64), Vec<u32>> = HashMap::new();
        for (row, fi) in raw {
            by_key.entry((fi.trace_id, fi.span_id)).or_default().push(row);
            rows.push(fi);
        }
        let n = rows.len();
        let sf = Arc::new(SegFold { rows, by_key });
        let mut c = self.seg_fold_cache.lock().unwrap();
        c.tick += 1;
        let tk = c.tick;
        if let Some((old, _)) = c.map.insert(seg.get(), (sf.clone(), tk)) {
            c.cur_rows -= old.rows.len();
        }
        c.cur_rows += n;
        if c.cur_rows > c.cap_rows {
            c.evict();
        }
        sf
    }

    /// 带剪枝的读路径。按时间窗（段 zone-map）+ trace_id 剪枝，减少触及的段数（活 trace 读扇出上界）。
    /// 返回 (折叠出的 span, 实际扫描的段数)。所有判定只用快照里钉死的版本。
    pub fn read_spans_query(&self, snap: &Snapshot, q: &TraceQuery) -> (Vec<FoldedSpan>, usize) {
        // 普通读 / trace 详情要原文,读全列。
        self.fold_query(snap, q, None, Projection::ALL)
    }

    /// 折叠核心。`keys=Some(集合)` 时**只折叠命中这些 (trace,span) 的行**（检索用：先由索引拿到命中 key,
    /// 只折叠它们,不折叠全库）；`None` = 折叠全部（普通读）。`proj` 声明要读哪些可折叠值列——列式段据此
    /// 跳过不读的列（尤其大文本列），行式/内存源忽略它（无列 I/O 可省）。段扫描仍是全段（行级行指针待真实
    /// 索引），但折叠/克隆只发生在候选行上。
    fn fold_query(
        &self,
        snap: &Snapshot,
        q: &TraceQuery,
        keys: Option<&std::collections::HashSet<(u64, u64)>>,
        proj: Projection,
    ) -> (Vec<FoldedSpan>, usize) {
        // 租户隔离时，强制把 tenant_id 列纳入投影（否则列式段窄投影读不到 tenant，过滤会误删全部）。
        let proj = if q.tenant_id.is_some() { Projection::of(proj.bits() | Projection::TENANT_ID) } else { proj };
        let mut inputs: Vec<FoldInput> = Vec::new();
        let mut scanned = 0usize;
        let in_keys = |t: u64, s: u64| keys.map_or(true, |ks| ks.contains(&(t, s)));

        // 段源：先用段 zone-map(min_ts/max_ts) 做时间窗剪枝 —— 不重叠的段整段跳过、不扫。
        let mut upgrades: std::collections::BTreeMap<(u64, u64), SpanFields> = std::collections::BTreeMap::new();
        for entry in snap.manifest.segments.values() {
            if entry.max_ts < q.time_from || entry.min_ts > q.time_to {
                continue; // 时间窗外，整段剪掉
            }
            scanned += 1;
            match keys {
                // ★ 检索快路：已知候选 key → 段折叠缓存 + 段内 key→行号 索引，**只取候选行**、不扫全段。
                //   首次解码该段后缓存，之后所有查询命中缓存（这是把检索 QPS 从"每查全段扫"解放出来的关键）。
                Some(ks) => {
                    // 段级 bloom：这个段肯定没有任何候选 key → 整段跳过折叠定位（upgrade 仍在下面照常处理）。
                    let bloom_skip = self
                        .seg_key_bloom
                        .lock()
                        .unwrap()
                        .get(&entry.segment_id.get())
                        .map_or(false, |b| !ks.iter().any(|&k| b.maybe_contains(k)));
                    let sf = if bloom_skip { None } else { Some(self.seg_fold(entry.segment_id)) };
                    for &(t, s) in ks {
                        let Some(sf) = &sf else { break };
                        if q.trace_id.map_or(false, |tid| t != tid) {
                            continue;
                        }
                        let Some(rowlist) = sf.by_key.get(&(t, s)) else { continue };
                        for &row in rowlist {
                            if entry.deletion_vec.is_deleted(row) {
                                continue; // 删除位图按行号照查
                            }
                            // 时间窗已由段 zone-map 整段剪枝（FoldInput 不带行级 ts，与投影路一致）。
                            inputs.push(sf.rows[row as usize].clone()); // 只克隆候选行（极少）
                        }
                    }
                }
                // 普通读/聚合：三条扫描路（投影 `proj` 贯穿——列式段据此只解码命中列）：
                //   ① 段无删除 + 有真实时间窗 → 时间下推 + 投影（丢行号，段无删除用不到）。
                //   ② 否则纯投影下推：只裁列、不丢行 → 行号完整，删除位图照行号生效。
                //   ③ 都不支持 → 回退 `scan_fold_inputs` 读全列。
                None => {
                    let time_pushed = if entry.deletion_seq == 0 && (q.time_from != i64::MIN || q.time_to != i64::MAX) {
                        self.segments.scan_fold_inputs_in_time(entry.segment_id, q.time_from, q.time_to, proj)
                    } else {
                        None
                    };
                    match time_pushed {
                        Some(folds) => {
                            for fi in folds {
                                if q.trace_id.map_or(false, |tid| fi.trace_id != tid) {
                                    continue;
                                }
                                inputs.push(fi);
                            }
                        }
                        None => {
                            let rows = self
                                .segments
                                .scan_fold_inputs_projected(entry.segment_id, proj)
                                .unwrap_or_else(|| self.segments.scan_fold_inputs(entry.segment_id));
                            for (row, fi) in rows {
                                if entry.deletion_vec.is_deleted(row) {
                                    continue;
                                }
                                if let Some(tid) = q.trace_id {
                                    if fi.trace_id != tid {
                                        continue;
                                    }
                                }
                                inputs.push(fi);
                            }
                        }
                    }
                }
            }
            if let Some(up) = &entry.upgrade_ref {
                for (&(t, s), patch) in up.iter() {
                    if q.trace_id.map_or(false, |tid| t != tid) {
                        continue;
                    }
                    if !in_keys(t, s) {
                        continue;
                    }
                    // 同一 span 跨段的多份 upgrade 也按 last-non-null + logs 并集合一起。
                    upgrades.entry((t, s)).or_default().merge_from(patch);
                }
            }
        }

        // MemTable 源：半开区间 (retained_watermark, live_lsn]，再按时间窗 + trace_id 行级过滤。
        {
            let mt = self.memtable.lock().unwrap();
            for r in mt.read_range(snap.retained_watermark, snap.live_lsn) {
                if r.ts < q.time_from || r.ts > q.time_to {
                    continue;
                }
                if let Some(tid) = q.trace_id {
                    if r.trace_id != tid {
                        continue;
                    }
                }
                if !in_keys(r.trace_id, r.span_id) {
                    continue;
                }
                inputs.push(r.to_fold_input());
            }
        }

        // 四源 k 路归并折叠：event_id 去重、last-non-null-wins、logs union。
        let mut spans = fold_events(inputs);

        // upgrade 校正：晚到属性补写盖到对应 span 上（只覆盖非身份属性，非空才覆盖）。
        for sp in &mut spans {
            if let Some(patch) = upgrades.get(&(sp.trace_id, sp.span_id)) {
                sp.apply_patch(patch);
            }
        }
        // 租户隔离：只留本租户的 span（列表/读路径与检索路径一致地强制过滤）。
        if let Some(t) = q.tenant_id {
            spans.retain(|sp| sp.tenant_id == Some(t));
        }
        (spans, scanned)
    }

    /// 列出 trace 摘要（web 控制台列表视图）。按 trace_id 把折叠出的 span 聚合：span 数、总/最大耗时、报错数。
    /// 输出按 trace_id 升序，确定可复算。
    pub fn list_traces(&self, snap: &Snapshot, q: &TraceQuery) -> Vec<TraceSummary> {
        // 只读 status/耗时/token —— 不碰大文本列。
        let proj = Projection::of(
            Projection::STATUS | Projection::DURATION_NS | Projection::INPUT_TOKENS | Projection::OUTPUT_TOKENS,
        );
        let (spans, _) = self.fold_query(snap, q, None, proj);
        let mut by_trace: BTreeMap<u64, TraceSummary> = BTreeMap::new();
        for s in spans {
            let e = by_trace.entry(s.trace_id).or_insert(TraceSummary {
                trace_id: s.trace_id,
                span_count: 0,
                total_duration_ns: 0,
                max_duration_ns: 0,
                error_count: 0,
                total_input_tokens: 0,
                total_output_tokens: 0,
            });
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
        }
        by_trace.into_values().collect()
    }

    /// 列出会话摘要（多轮会话视图）：按 session_id 聚合,数 trace 数/span 数/token 汇总。升序。
    pub fn list_sessions(&self, snap: &Snapshot, q: &TraceQuery) -> Vec<SessionSummary> {
        // 按 session 聚合 token —— 只读 session_id + token,跳过文本。
        let proj = Projection::of(Projection::SESSION_ID | Projection::INPUT_TOKENS | Projection::OUTPUT_TOKENS);
        let (spans, _) = self.fold_query(snap, q, None, proj);
        // session_id -> (distinct traces, span_count, in_tok, out_tok)
        let mut acc: BTreeMap<u64, (std::collections::HashSet<u64>, usize, u64, u64)> = BTreeMap::new();
        for s in spans {
            if let Some(sid) = s.session_id {
                let e = acc.entry(sid).or_default();
                e.0.insert(s.trace_id);
                e.1 += 1;
                e.2 += s.input_tokens.unwrap_or(0);
                e.3 += s.output_tokens.unwrap_or(0);
            }
        }
        acc.into_iter()
            .map(|(session_id, (traces, span_count, i, o))| SessionSummary {
                session_id,
                trace_count: traces.len(),
                span_count,
                total_input_tokens: i,
                total_output_tokens: o,
            })
            .collect()
    }

    /// 装一个会话的**多轮对话流**：把同一 `session_id` 的多条 trace（每条=一轮）按 trace_id 升序
    /// 拼成「用户问 → agent 答」的时间线。这是多轮会话视图的渲染源，也是会话级评测的输入。
    ///
    /// 取原文要读全列。当前没有 session→trace 倒排，按 session_id **扫全量过滤**（O(全库)）——
    /// 会话视图是低频操作可接受；真要高频再加 session 边车索引。
    pub fn load_session_timeline(&self, snap: &Snapshot, session_id: u64) -> SessionTimeline {
        self.load_session_timeline_query(snap, session_id, &TraceQuery::all())
    }

    /// 带查询约束的会话时间线。控制台网关用它把 tenant 过滤压到折叠读取层。
    pub fn load_session_timeline_query(&self, snap: &Snapshot, session_id: u64, q: &TraceQuery) -> SessionTimeline {
        let (spans, _) = self.read_spans_query(snap, q);
        // 按 trace 分组本会话的 span（BTreeMap → trace_id 升序 = 轮次序）。
        let mut by_trace: BTreeMap<u64, Vec<FoldedSpan>> = BTreeMap::new();
        for s in spans {
            if s.session_id == Some(session_id) {
                by_trace.entry(s.trace_id).or_default().push(s);
            }
        }
        let mut turns = Vec::with_capacity(by_trace.len());
        let mut total_in = 0u64;
        let mut total_out = 0u64;
        for (turn_index, (trace_id, mut sps)) in by_trace.into_iter().enumerate() {
            sps.sort_by_key(|s| s.span_id);
            // 输入取最早（span_id 最小）带 input_text 的；答复取最末带 output_text 的。
            let user_input = sps.iter().find(|s| s.input_text.is_some()).and_then(|s| s.input_text.clone());
            let answer = sps.iter().rev().find(|s| s.output_text.is_some());
            let agent_output = answer.and_then(|s| s.output_text.clone());
            let eval_score = answer.and_then(|s| s.eval_score);
            let mut agents: Vec<String> = sps.iter().filter_map(|s| s.agent_name.clone()).collect();
            agents.sort();
            agents.dedup();
            let input_tokens: u64 = sps.iter().map(|s| s.input_tokens.unwrap_or(0)).sum();
            let output_tokens: u64 = sps.iter().map(|s| s.output_tokens.unwrap_or(0)).sum();
            let error_count = sps.iter().filter(|s| s.status.unwrap_or(0) != 0).count();
            total_in += input_tokens;
            total_out += output_tokens;
            turns.push(SessionTurn {
                trace_id,
                turn_index,
                user_input,
                agent_output,
                agents,
                span_count: sps.len(),
                input_tokens,
                output_tokens,
                error_count,
                eval_score,
            });
        }
        SessionTimeline { session_id, turns, total_input_tokens: total_in, total_output_tokens: total_out }
    }

    /// 控制台用：会话行列表（标题/轮数/状态/token/首 trace），按 session_id 降序。
    /// 走**增量边车索引**：摄入时已逐事件 O(1) 维护，这里直接产出（带排序缓存）→ 写多读少也不全扫。
    /// 仅当 delete/upgrade 标脏时，才在此做一次全量重建（这两类不走 index_record）。
    pub fn console_sessions(&self, snap: &Snapshot) -> Vec<ConsoleSession> {
        // 先看是否标脏（不持锁去扫，避免 session_idx→memtable 的锁序反转死锁）。
        let dirty = self.session_idx.lock().unwrap().dirty;
        if dirty {
            let (spans, _) = self.read_spans_query(snap, &TraceQuery::all()); // 不持 session_idx 锁
            let mut idx = self.session_idx.lock().unwrap();
            if idx.dirty {
                idx.rebuild(&spans);
            }
        }
        self.session_idx.lock().unwrap().rows()
    }

    /// 控制台用：按请求租户隔离的会话行列表。
    /// 无租户时走增量边车；有租户时基于已过滤 span 临时聚合，避免全局 session_idx 泄露别的租户。
    pub fn console_sessions_for_tenant(&self, snap: &Snapshot, tenant: Option<u64>) -> Vec<ConsoleSession> {
        let Some(t) = tenant else { return self.console_sessions(snap) };
        let (spans, _) = self.read_spans_query(snap, &TraceQuery::all().for_tenant(t));
        let mut idx = SessionIndex::default();
        idx.rebuild(&spans);
        idx.rows()
    }

    /// 控制台用：一条 trace 的折叠 span（瀑布）。引擎不存 span 的 kind/name/起始时刻，这里**派生**：
    /// kind = agent>tool>model>other；name = 同源；起始时刻按 span_id 升序累加 duration 顺排（逻辑瀑布）。
    pub fn console_trace_spans(&self, snap: &Snapshot, trace_id: u64) -> Vec<ConsoleSpan> {
        self.console_trace_spans_for_tenant(snap, trace_id, None)
    }

    /// 控制台用：按请求租户隔离的一条 trace 折叠 span。
    pub fn console_trace_spans_for_tenant(
        &self,
        snap: &Snapshot,
        trace_id: u64,
        tenant: Option<u64>,
    ) -> Vec<ConsoleSpan> {
        let mut q = TraceQuery::trace(trace_id, i64::MIN, i64::MAX);
        q.tenant_id = tenant;
        let (mut spans, _) = self.read_spans_query(snap, &q);
        spans.sort_by_key(|s| s.span_id);
        let mut start = 0u64;
        spans
            .into_iter()
            .map(|s| {
                let (kind, name) = if let Some(a) = &s.agent_name {
                    ("agent", a.clone())
                } else if let Some(t) = &s.tool_name {
                    ("tool", t.clone())
                } else if let Some(m) = &s.model {
                    ("llm", m.clone())
                } else {
                    ("other", format!("span {}", s.span_id))
                };
                let dur = s.duration_ns.unwrap_or(0);
                let cs = ConsoleSpan {
                    span_id: s.span_id,
                    parent_span_id: s.parent_span_id,
                    kind,
                    name,
                    start_ns: start,
                    duration_ns: dur,
                    has_error: s.status.unwrap_or(0) != 0,
                    input_tokens: s.input_tokens.unwrap_or(0),
                    output_tokens: s.output_tokens.unwrap_or(0),
                    model: s.model.clone(),
                    input_text: s.input_text.clone(),
                    output_text: s.output_text.clone(),
                };
                start += dur;
                cs
            })
            .collect()
    }

    /// 按 agent 的成本归因（per-agent 成本下钻）：按 agent_name 聚合 token。按 agent 名升序。
    pub fn cost_by_agent(&self, snap: &Snapshot, q: &TraceQuery) -> Vec<AgentCost> {
        // 按 agent 归因 token —— 只读 agent_name + token,跳过文本（成本下钻是典型的"只数不读原文"）。
        let proj = Projection::of(Projection::AGENT_NAME | Projection::INPUT_TOKENS | Projection::OUTPUT_TOKENS);
        let (spans, _) = self.fold_query(snap, q, None, proj);
        let mut acc: BTreeMap<String, (usize, u64, u64)> = BTreeMap::new();
        for s in spans {
            if let Some(a) = &s.agent_name {
                let e = acc.entry(a.clone()).or_default();
                e.0 += 1;
                e.1 += s.input_tokens.unwrap_or(0);
                e.2 += s.output_tokens.unwrap_or(0);
            }
        }
        acc.into_iter()
            .map(|(agent_name, (span_count, input_tokens, output_tokens))| AgentCost {
                agent_name,
                span_count,
                input_tokens,
                output_tokens,
            })
            .collect()
    }

    /// eval 闭环：用 `scorer` 给命中 `q` 的每条 span 打分，分数**走 upgrade（晚到补写）通道写回**。
    /// 返回打了分的 span。读回时分数被折叠进对应 span 的 `eval_score`/`eval_label`。
    ///
    /// 把产品从"看 trace"推到"评 trace"。这里的妙处：评测分本质就是一种"trace 事后才有的字段"，
    /// 与晚到属性补写同构 —— 直接复用 upgrade 王牌，不需要给评测另起一套存储。
    /// 先 flush 内存表（让被评 span 都进段、upgrade 有落点），再按 (trace,span)→段 映射把分写回所在段。
    /// scorer 现在是不依赖 LLM 的规则版；换成 LLM-judge / 本地小模型裁判时，这条闭环骨架不变。
    pub fn eval_and_writeback(&self, scorer: &dyn Scorer, q: &TraceQuery) -> Vec<ScoredSpan> {
        // 1) 先封段：被评 span 都落进段，output_text 也随段持久化，upgrade 才有段可落。
        self.flush_memtable();

        // 2) 读出待评 span（此刻 output_text 来自段）。
        let snap = self.pin_snapshot();
        let (spans, _) = self.read_spans_query(&snap, q);

        // 3) 建 (trace,span) → 所在段 映射：分数写回该段（多段命中取最小段号，稳定）。
        // 与读路径同口径做 zone-map 时间窗 + trace_id 剪枝：只扫 q 命中的段,不扫全库
        //（否则按单条 trace 评测也要扫遍所有段）。
        let mut span_seg: HashMap<(u64, u64), SegmentId> = HashMap::new();
        for entry in snap.manifest.segments.values() {
            if entry.max_ts < q.time_from || entry.min_ts > q.time_to {
                continue; // 时间窗外，整段跳过
            }
            for (_row, fi) in self.segments.scan_fold_inputs(entry.segment_id) {
                if q.trace_id.map_or(false, |tid| fi.trace_id != tid) {
                    continue; // trace_id 不匹配（行级）
                }
                span_seg.entry((fi.trace_id, fi.span_id)).or_insert(entry.segment_id);
            }
        }
        drop(snap);

        // 4) 逐条打分并写回（scorer 返回 None 的 span 跳过、不写）。
        let mut out = Vec::new();
        for sp in spans {
            let Some(outcome) = scorer.score(&sp) else { continue };
            if let Some(&seg) = span_seg.get(&(sp.trace_id, sp.span_id)) {
                self.commit_upgrade(
                    seg,
                    sp.trace_id,
                    sp.span_id,
                    SpanFields {
                        eval_score: Some(outcome.score),
                        eval_label: Some(outcome.label.clone()),
                        ..Default::default()
                    },
                );
                out.push(ScoredSpan { trace_id: sp.trace_id, span_id: sp.span_id, outcome });
            }
        }
        out
    }

    /// 评测看板：把已打分的 span 聚合成 通过率/均分 —— 整体一行 +（有 agent 名的）每 agent 一行。
    /// `pass_threshold` 千分制，分数 ≥ 它算通过。这是 eval 的产品出口:回归视图("哪个 agent 退步了")。
    /// 输出第 0 行恒为整体(agent_name=None),其后按 agent 名升序。
    pub fn eval_summary(&self, snap: &Snapshot, q: &TraceQuery, pass_threshold: u32) -> Vec<EvalSummary> {
        // 看板只看分数 + agent 名 —— 不读被评的原文（原文在打分时已用过、写回成了分数）。
        let proj = Projection::of(Projection::EVAL_SCORE | Projection::EVAL_LABEL | Projection::AGENT_NAME);
        let (spans, _) = self.fold_query(snap, q, None, proj);
        // 只取已打分的 span（无 eval_score 的不计），喂进共用聚合口径。
        let scored = spans.into_iter().filter_map(|s| s.eval_score.map(|sc| (s.agent_name, sc)));
        aggregate_eval(scored, pass_threshold)
    }

    /// 建一个空数据集（已存在则不动）。返回是否新建。
    pub fn create_dataset(&self, name: &str) -> bool {
        let mut ds = self.datasets.lock().unwrap();
        if ds.contains_key(name) {
            return false;
        }
        ds.insert(name.to_string(), Dataset { name: name.to_string(), examples: Vec::new() });
        true
    }

    /// 把命中 `q` 且通过 `pred` 的 span 采集进数据集（不存在则自动建）。返回新增样本数。
    /// 典型用法:`pred = |s| s.eval_score == Some(0)` 把失败样本收集成回归集;
    /// 或配合 `search_similar` 先捞"相似失败 trace"再传它们的 span 进来(中文/语义召回的差异化用法)。
    /// 按 (trace_id, span_id) 去重:已在集里的不重复加。存的是 span 快照,底层 trace 后续被合并/回收也不影响。
    pub fn collect_into_dataset(
        &self,
        name: &str,
        snap: &Snapshot,
        q: &TraceQuery,
        pred: &dyn Fn(&FoldedSpan) -> bool,
    ) -> usize {
        let (spans, _) = self.read_spans_query(snap, q);
        let mut ds = self.datasets.lock().unwrap();
        let entry = ds.entry(name.to_string()).or_insert_with(|| Dataset { name: name.to_string(), examples: Vec::new() });
        let mut existing: std::collections::HashSet<(u64, u64)> =
            entry.examples.iter().map(|e| (e.span.trace_id, e.span.span_id)).collect();
        let mut added = 0;
        for s in spans {
            if !pred(&s) {
                continue;
            }
            if existing.insert((s.trace_id, s.span_id)) {
                entry.examples.push(DatasetExample { span: s, expected: None });
                added += 1;
            }
        }
        added
    }

    /// 取一个数据集的副本（检视/导出用）。
    pub fn dataset(&self, name: &str) -> Option<Dataset> {
        self.datasets.lock().unwrap().get(name).cloned()
    }

    /// 列出所有数据集摘要,按名升序。
    pub fn list_datasets(&self) -> Vec<DatasetSummary> {
        self.datasets
            .lock()
            .unwrap()
            .values()
            .map(|d| DatasetSummary { name: d.name.clone(), example_count: d.examples.len() })
            .collect()
    }

    /// 对一个数据集**现跑 scorer**,聚合成通过率/均分看板(整体 + per-agent)——回归基准:
    /// 同一数据集 + 同一 scorer 反复跑,通过率掉了就是 agent/prompt 退步了。返回 None=无此数据集。
    /// 注意:这里直接对数据集里**冻结的 span 快照**评分,不走 upgrade 写回(那是线上 trace 的事)。
    pub fn eval_dataset(&self, name: &str, scorer: &dyn Scorer, pass_threshold: u32) -> Option<Vec<EvalSummary>> {
        let ds = self.datasets.lock().unwrap().get(name).cloned()?;
        let scored = ds
            .examples
            .iter()
            .filter_map(|ex| scorer.score(&ex.span).map(|o| (ex.span.agent_name.clone(), o.score)));
        Some(aggregate_eval(scored, pass_threshold))
    }

    /// 装一条 trace 的父子树（树+瀑布视图用）：读出该 trace 的 span，按 parent_span_id 连成树。
    /// 父不在本 trace 内的 span 当根（容错：丢了 root 事件也能渲染）。
    pub fn load_trace_tree(&self, snap: &Snapshot, trace_id: u64) -> TraceTree {
        let (spans, _) = self.read_spans_query(snap, &TraceQuery::trace(trace_id, i64::MIN, i64::MAX));
        let mut nodes: BTreeMap<u64, TraceNode> = BTreeMap::new();
        for s in spans {
            nodes.insert(s.span_id, TraceNode { span: s, children: Vec::new() });
        }
        let mut roots = Vec::new();
        let ids: Vec<u64> = nodes.keys().copied().collect();
        for id in ids {
            let parent = nodes[&id].span.parent_span_id;
            match parent {
                Some(p) if nodes.contains_key(&p) => nodes.get_mut(&p).unwrap().children.push(id),
                _ => roots.push(id),
            }
        }
        for n in nodes.values_mut() {
            n.children.sort_unstable(); // 确定序
        }
        roots.sort_unstable();
        TraceTree { trace_id, roots, nodes }
    }

    /// 一条 trace 的 **agent 执行图（DAG）**：把 span 父子树按 agent/工具维度收拢成"谁调用了谁"。
    /// 角色判定:有 agent_name → Agent;否则有 tool_name → Tool;都没有 → `span:<id>`(Other)。
    /// 边 = 父 span 的角色 → 子 span 的角色(同角色自环剔除,只留跨角色调用/移交),按出现次数聚合。
    /// 节点带聚合统计(span 数、token)。节点/边都确定排序,可复算。
    pub fn agent_graph(&self, snap: &Snapshot, trace_id: u64) -> AgentGraph {
        // 执行图按 agent/工具/父子连边 + 聚合 token —— 只读这些维度,不读原文。
        let proj = Projection::of(
            Projection::AGENT_NAME
                | Projection::TOOL_NAME
                | Projection::PARENT_SPAN_ID
                | Projection::INPUT_TOKENS
                | Projection::OUTPUT_TOKENS,
        );
        let (spans, _) = self.fold_query(snap, &TraceQuery::trace(trace_id, i64::MIN, i64::MAX), None, proj);

        // 角色判定（返回 (名字, 类型)）。
        let actor_of = |s: &FoldedSpan| -> (String, ActorKind) {
            if let Some(a) = &s.agent_name {
                (a.clone(), ActorKind::Agent)
            } else if let Some(t) = &s.tool_name {
                (t.clone(), ActorKind::Tool)
            } else {
                (format!("span:{}", s.span_id), ActorKind::Other)
            }
        };

        // span_id → 角色名，供连边时查父角色。
        let mut span_actor: HashMap<u64, String> = HashMap::new();
        // 节点聚合：actor → (kind, span_count, in_tok, out_tok)。
        let mut nodes: BTreeMap<String, (ActorKind, usize, u64, u64)> = BTreeMap::new();
        for s in &spans {
            let (name, kind) = actor_of(s);
            span_actor.insert(s.span_id, name.clone());
            let e = nodes.entry(name).or_insert((kind, 0, 0, 0));
            e.1 += 1;
            e.2 += s.input_tokens.unwrap_or(0);
            e.3 += s.output_tokens.unwrap_or(0);
        }

        // 边聚合：父角色 → 子角色（跳过父不在本 trace 内 / 同角色自环）。
        let mut edges: BTreeMap<(String, String), usize> = BTreeMap::new();
        for s in &spans {
            let Some(parent_id) = s.parent_span_id else { continue };
            let Some(from) = span_actor.get(&parent_id) else { continue };
            let to = &span_actor[&s.span_id];
            if from == to {
                continue; // 同角色多步,不算一次调用/移交
            }
            *edges.entry((from.clone(), to.clone())).or_insert(0) += 1;
        }

        AgentGraph {
            trace_id,
            nodes: nodes
                .into_iter()
                .map(|(actor, (kind, span_count, input_tokens, output_tokens))| AgentGraphNode {
                    actor,
                    kind,
                    span_count,
                    input_tokens,
                    output_tokens,
                })
                .collect(),
            edges: edges
                .into_iter()
                .map(|((from, to), count)| AgentGraphEdge { from, to, count })
                .collect(),
        }
    }

    /// 给某 span 加向量（向量由外部 embedder 算，不是每个 span 都建）。
    /// 开了持久化(`open_durable`)则**先追加写盘再进内存图** —— 向量段里推不出来,必须单独落盘,
    /// 否则重启后"找相似"全空。
    pub fn index_embedding(&self, trace_id: u64, span_id: u64, embedding: Vec<f32>) {
        if let Some(p) = &self.vector_path {
            let _ = vecstore::append(p, trace_id, span_id, &embedding);
        }
        self.graph.index_embedding(trace_id, span_id, embedding);
    }

    /// 中文检索：BM25 找到候选 span，再折叠成完整 span 返回（带分，按相关性序）。
    /// 这是产品噱头之一「按内容搜 trace」。真实实现把检索下推、只折叠命中行。
    pub fn search_text(&self, snap: &Snapshot, query: &str, k: usize) -> Vec<(FoldedSpan, f32)> {
        self.search_text_filtered(snap, query, k, &|_, _| true)
    }

    /// 带过滤的中文检索：谓词限定 (trace_id, span_id)（如只搜某些 trace）。BM25 无图可下推，过滤后置 +
    /// 过取候选兜住截断。
    pub fn search_text_filtered(&self, snap: &Snapshot, query: &str, k: usize, filter: &dyn Fn(u64, u64) -> bool) -> Vec<(FoldedSpan, f32)> {
        let mut cands = self.bm25.search(query, k.max(50));
        cands.retain(|&(t, s, _)| filter(t, s));
        cands.truncate(k);
        self.join_folded(snap, cands)
    }

    /// 找相似：graph_index 向量近邻找到候选 span，再折叠返回（带距离，按相似度序）。
    pub fn search_similar(&self, snap: &Snapshot, query: &[f32], k: usize) -> Vec<(FoldedSpan, f32)> {
        self.search_similar_filtered(snap, query, k, &|_, _| true)
    }

    /// **带过滤找相似**：谓词**下推进图搜索**（`graph.search` 走进图过滤）—— 这正是验证过的 in-graph 过滤
    /// 在引擎层真正用起来（选择性谓词下召回不塌，见 `graph.rs` 的实测）。`filter` 按 (trace_id, span_id) 判。
    /// 快照可见性仍由 `join_folded` 自然裁（不在快照里的 span 折叠不出来）。
    pub fn search_similar_filtered(&self, snap: &Snapshot, query: &[f32], k: usize, filter: &dyn Fn(u64, u64) -> bool) -> Vec<(FoldedSpan, f32)> {
        let cands = self.graph.search(query, k, filter);
        self.join_folded(snap, cands)
    }

    /// 混合检索：BM25 关键词命中 + 向量语义相似，用 RRF 融合成一路排序，再折叠返回。
    /// 同时被关键词和语义命中的 span 排更前 —— 「关键词 + 语义混合召回」，单走一路给不出这个排序。
    pub fn search_hybrid(&self, snap: &Snapshot, text: &str, query_vec: &[f32], k: usize) -> Vec<(FoldedSpan, f32)> {
        self.search_hybrid_filtered(snap, text, query_vec, k, &|_, _| true)
    }

    /// 带过滤的混合检索：向量侧谓词**下推进图搜索**（in-graph 过滤），关键词侧过滤后置（BM25 无图），
    /// 再 RRF 融合。两路都只在满足谓词的 span 上召回。
    pub fn search_hybrid_filtered(&self, snap: &Snapshot, text: &str, query_vec: &[f32], k: usize, filter: &dyn Fn(u64, u64) -> bool) -> Vec<(FoldedSpan, f32)> {
        let pool = k.max(10);
        let mut bm = self.bm25.search(text, pool);
        bm.retain(|&(t, s, _)| filter(t, s)); // 关键词侧：后置过滤
        let vec = self.graph.search(query_vec, pool, filter); // 向量侧：下推进图过滤
        let r1: Vec<(u64, u64)> = bm.iter().map(|&(t, s, _)| (t, s)).collect();
        let r2: Vec<(u64, u64)> = vec.iter().map(|&(t, s, _)| (t, s)).collect();
        let fused = rrf_fuse(&[r1, r2], 60.0);
        let cands: Vec<(u64, u64, f32)> = fused.into_iter().take(k).map(|((t, s), sc)| (t, s, sc)).collect();
        self.join_folded(snap, cands)
    }

    /// 用 (trace,span) 谓词回调跑一段逻辑，谓词由 `SearchFilter` + 属性边车构造（在锁内有效）。
    /// 把"按产品维度（agent/状态/时间）过滤"翻译成 `graph.search` 认的 `Fn(u64,u64)->bool`。
    fn with_filter_pred<R>(&self, f: &SearchFilter, body: impl FnOnce(&dyn Fn(u64, u64) -> bool) -> R) -> R {
        let attrs = self.filter_attrs.lock().unwrap();
        let need_attrs = f.needs_attrs();
        let pred = move |t: u64, s: u64| -> bool {
            if let Some(tid) = f.trace_id {
                if t != tid {
                    return false;
                }
            }
            if !need_attrs {
                return true; // 仅 trace_id 约束（或无约束），不必查边车
            }
            match attrs.get(&(t, s)) {
                Some(a) => f.attrs_match(a),
                None => false, // 有属性约束但无元数据 → 不命中
            }
        };
        body(&pred)
    }

    /// **按产品维度过滤的找相似**：`SearchFilter`（agent/状态/时间/trace）翻成谓词，下推进图搜索。
    /// 这才是"带过滤 ANN"在真实查询里的样子 —— "找 agent X 报错的相似 span"。
    pub fn search_similar_attr(&self, snap: &Snapshot, query: &[f32], k: usize, filter: &SearchFilter) -> Vec<(FoldedSpan, f32)> {
        let cands = self.with_filter_pred(filter, |pred| self.graph.search(query, k, pred));
        self.join_folded(snap, cands)
    }

    /// 按产品维度过滤的**中文检索**：BM25 命中后按 `SearchFilter`（agent/状态/时间/trace）后置过滤。
    /// "搜『盗刷』里 agent=风控、报错的那些 span" —— HTTP 检索端点用这个。
    pub fn search_text_attr(&self, snap: &Snapshot, query: &str, k: usize, filter: &SearchFilter) -> Vec<(FoldedSpan, f32)> {
        let cands = self.with_filter_pred(filter, |pred| {
            let mut c = self.bm25.search(query, k.max(50));
            c.retain(|&(t, s, _)| pred(t, s));
            c.truncate(k);
            c
        });
        self.join_folded(snap, cands)
    }

    /// 按产品维度过滤的混合检索（向量侧下推进图、关键词侧后置过滤）。
    pub fn search_hybrid_attr(&self, snap: &Snapshot, text: &str, query_vec: &[f32], k: usize, filter: &SearchFilter) -> Vec<(FoldedSpan, f32)> {
        let pool = k.max(10);
        let (bm, vec) = self.with_filter_pred(filter, |pred| {
            let mut bm = self.bm25.search(text, pool);
            bm.retain(|&(t, s, _)| pred(t, s));
            let vec = self.graph.search(query_vec, pool, pred);
            (bm, vec)
        });
        let r1: Vec<(u64, u64)> = bm.iter().map(|&(t, s, _)| (t, s)).collect();
        let r2: Vec<(u64, u64)> = vec.iter().map(|&(t, s, _)| (t, s)).collect();
        let fused = rrf_fuse(&[r1, r2], 60.0);
        let cands: Vec<(u64, u64, f32)> = fused.into_iter().take(k).map(|((t, s), sc)| (t, s, sc)).collect();
        self.join_folded(snap, cands)
    }

    /// 把检索候选 (trace, span, 分) join 上「在快照里折叠出的完整 span」，保持检索的排序。
    /// **只折叠命中行**：把候选 key 集喂给 `fold_query`，不折叠全库（大数据下检索不再为几条命中折叠整库）。
    fn join_folded(&self, snap: &Snapshot, cands: Vec<(u64, u64, f32)>) -> Vec<(FoldedSpan, f32)> {
        let keys: std::collections::HashSet<(u64, u64)> = cands.iter().map(|&(t, s, _)| (t, s)).collect();
        // 检索结果要展示原文（命中片段），读全列。
        let (hits, _) = self.fold_query(snap, &TraceQuery::all(), Some(&keys), Projection::ALL);
        let map: HashMap<(u64, u64), FoldedSpan> =
            hits.into_iter().map(|s| ((s.trace_id, s.span_id), s)).collect();
        cands
            .into_iter()
            .filter_map(|(t, s, score)| map.get(&(t, s)).cloned().map(|sp| (sp, score)))
            .collect()
    }

    /// 崩溃恢复：从 WAL 重放重建 MemTable（§M.6）+ **重建派生检索索引**。
    /// 重放点 = 当前 manifest 的 memtable_watermark（已吸收进段的最大 LSN）。
    /// 重放只取 watermark 之后的记录；即便段与重放有重叠（崩溃窗口里段已落、水位未推进），
    /// 读时的确定性 event_id 去重也保证不重复折叠 —— 这正是「seq 原样持久化、不重补」的意义。
    ///
    /// 检索索引(BM25/属性边车/向量)是内存态,重启全空,这里一并重建,否则重启后"按内容搜/找相似"返回空:
    /// - BM25 + 属性边车是**派生数据**：扫持久段(水位之前)+ 重放的 WAL 尾(水位之后)各喂一次,合起来覆盖全部、不重不漏。
    /// - 向量**段里推不出来**：从独立向量文件重载,喂回图索引(后写覆盖先写)。
    pub fn recover(&self) {
        olog::log(olog::Level::Info, "recover_start", &[("version", &self.current.version())]);
        // 1) 派生索引：扫所有持久段(水位之前的数据)喂回 BM25 + 属性边车；顺带重建段级 key bloom。
        let m = self.current.manifest();
        let seg_count = m.segments.len();
        for entry in m.segments.values() {
            let recs = self.segments.scan_records(entry.segment_id);
            let bloom = KeyBloom::build(recs.iter().map(|r| (r.trace_id, r.span_id)), recs.len());
            self.seg_key_bloom.lock().unwrap().insert(entry.segment_id.get(), Arc::new(bloom));
            for r in &recs {
                self.index_record(r);
            }
        }
        drop(m);
        // 2) 向量：从独立向量文件重载,喂回图索引。
        let mut vec_count = 0u64;
        if let Some(p) = &self.vector_path {
            for ((t, s), v) in vecstore::load(p) {
                self.graph.index_embedding(t, s, v);
                vec_count += 1;
            }
        }
        // 3) WAL 重放：水位之后的尾巴进 MemTable,并喂派生索引(与段不重叠,因 manifest 水位与段同事务持久)。
        let wal = self.wal.lock().unwrap();
        let mut mt = self.memtable.lock().unwrap();
        let mut wal_count = 0u64;
        for (lsn, r) in wal.replay_after(WalLsn::new(self.current.memtable_watermark())) {
            self.index_record(&r);
            mt.append(MemRow {
                commit_lsn: lsn,
                trace_id: r.trace_id,
                span_id: r.span_id,
                ts: r.ts,
                identity: r.identity.clone(), // seq 来自 WAL 原值，绝不重补
                fields: r.fields.clone(),
            });
            wal_count += 1;
        }
        // 已提交尾从 WAL 恢复（重启后 committed_tail 不是持久态，由 WAL 重新确定）。
        let tail = wal.committed_tail();
        self.current.advance_committed_tail(tail);
        olog::log(olog::Level::Info, "recover_done", &[
            ("segs_scanned", &seg_count),
            ("vectors_reloaded", &vec_count),
            ("wal_replayed", &wal_count),
            ("committed_tail", &tail.get()),
        ]);
    }

    /// 测试/演示：模拟崩溃，丢弃易失的 MemTable。WAL 与 manifest 是持久的，保留不动。
    pub fn simulate_crash_lose_memtable(&self) {
        *self.memtable.lock().unwrap() = MemTable::new();
    }

    fn alloc_segment_id(&self) -> SegmentId {
        let mut g = self.next_segment_id.lock().unwrap();
        let id = *g;
        *g += 1;
        SegmentId::new(id)
    }

    fn alloc_chunk_id(&self) -> yt_core::ids::ChunkId {
        let mut g = self.next_chunk_id.lock().unwrap();
        let id = *g;
        *g += 1;
        yt_core::ids::ChunkId::new(id)
    }

    /// flush 提交（sealed → live）：把一批已 ack 事件封段，新段 Live 进新版本，watermark 推进。
    /// 段加入 + watermark 推进必须在**同一次** commit 里原子生效（堵「既不在 memtable 又不在段」空窗）。
    pub fn commit_flush(&self, records: &[WalRecord], up_to_lsn: WalLsn) {
        let _w = self.write_lock.lock().unwrap();
        let seg = self.alloc_segment_id();
        self.segments.flush_to_segment(seg, records); // building→sealed（写完 fsync）
        let bloom = KeyBloom::build(records.iter().map(|r| (r.trace_id, r.span_id)), records.len());
        self.seg_key_bloom.lock().unwrap().insert(seg.get(), Arc::new(bloom));
        let (min_ts, max_ts) = ts_range(records);
        let mut draft = self.current.cow_next();
        draft.memtable_watermark = up_to_lsn; // 与下面加段同事务
        draft.segments.insert(
            seg.get(),
            SegmentEntry {
                segment_id: seg,
                level: 0,
                state: SegState::Live,
                min_ts, // zone-map：读路径据此做时间窗剪枝
                max_ts,
                deletion_vec: Arc::new(DeletionVec::empty()),
                deletion_seq: 0,
                upgrade_ref: None,
                upgrade_seq: 0,
            },
        );
        self.commit_and_persist(draft); // 原子换指针：sealed→live + watermark 同时生效;并落盘 manifest

        // 提交后按 gate 回收 MemTable 被吸收前缀。gate 必须取「所有活跃读者下界的最小值」，
        // 绝不能直接用 up_to_lsn —— 否则就是 flush-evict 漏行 bug。仍有旧读者时此值更小、不删其行。
        let gate = WalLsn::new(self.current.min_retained_watermark());
        self.memtable.lock().unwrap().evict_up_to(gate);
    }

    /// 删除提交：给某段换一个新的 deletion 块（deletion_seq+1），绝不原地改旧块。
    pub fn commit_delete(&self, seg: SegmentId, row: u32) {
        let _w = self.write_lock.lock().unwrap();
        let chunk_id = self.alloc_chunk_id();
        let mut draft = self.current.cow_next();
        if let Some(entry) = draft.segments.get_mut(&seg.get()) {
            let new_dv = entry.deletion_vec.with_deleted(row, chunk_id);
            entry.deletion_vec = Arc::new(new_dv);
            entry.deletion_seq += 1;
        }
        self.commit_and_persist(draft);
        self.session_idx.lock().unwrap().dirty = true; // 删除改了段，边车下次读重建
    }

    /// 属性补写（upgrade）提交：给某段 (trace_id, span_id) 补写**非身份属性**，与 delete 完全对称——
    /// 写时复制出新 upgrade 块（upgrade_seq+1），绝不原地改旧块（旧版本读者读旧块）。
    /// 身份字段冻结（M.7），由上层 schema 保证不进 `fields`。
    pub fn commit_upgrade(&self, seg: SegmentId, trace_id: u64, span_id: u64, fields: yt_core::fold::SpanFields) {
        let _w = self.write_lock.lock().unwrap();
        let chunk_id = self.alloc_chunk_id();
        let mut draft = self.current.cow_next();
        if let Some(entry) = draft.segments.get_mut(&seg.get()) {
            let base = entry.upgrade_ref.as_deref().cloned().unwrap_or_else(UpgradeColChunk::empty);
            let new_chunk = base.with_patch(trace_id, span_id, fields, chunk_id);
            entry.upgrade_ref = Some(Arc::new(new_chunk));
            entry.upgrade_seq += 1;
        }
        self.commit_and_persist(draft);
        self.session_idx.lock().unwrap().dirty = true; // 补写改了段，边车下次读重建
    }

    /// compaction 第 1 步：选段，记录选段瞬间各输入段的 (deletion_seq, upgrade_seq)。
    /// 返回的 plan 交给调用方在**锁外**做昂贵的段重建，再用 `compaction_finish` 提交。
    pub fn compaction_begin(&self, inputs: &[SegmentId]) -> CompactionPlan {
        let _w = self.write_lock.lock().unwrap();
        let m = self.current.manifest();
        let seqs_at_select = inputs
            .iter()
            .filter_map(|s| m.segments.get(&s.get()).map(|e| (s.get(), (e.deletion_seq, e.upgrade_seq))))
            .collect();
        CompactionPlan { inputs: inputs.to_vec(), seqs_at_select }
    }

    /// compaction 第 3 步：提交（草案 1 §D1.3 / OPEN-3）。
    /// 在 write_lock 下**重读输入段当前状态**重建新段 —— 这样选段后、提交前并发打到输入段的
    /// 删除/补写**不会丢**：当前 deletion_vec 把后到的删除也滤掉，当前 upgrade 块也并进新段。
    /// 返回是否发生了重读合并（输入段 seq 变了），便于观测/测试。
    pub fn compaction_finish(&self, plan: &CompactionPlan) -> bool {
        let _w = self.write_lock.lock().unwrap();
        let m = self.current.manifest();

        let mut reconciled = false;
        let mut merged: Vec<WalRecord> = Vec::new();
        let mut merged_upgrade = UpgradeColChunk::empty();
        let up_chunk_id = self.alloc_chunk_id();

        for &seg in &plan.inputs {
            let Some(entry) = m.segments.get(&seg.get()) else { continue };
            // 选段以来 seq 涨了 = 期间有并发删除/补写打到这个输入段 → 触发重读合并
            if plan.seqs_at_select.get(&seg.get()) != Some(&(entry.deletion_seq, entry.upgrade_seq)) {
                reconciled = true;
            }
            // 用「当前」deletion_vec 过滤（含选段后新增的删除）→ 删除不丢
            for (row, rec) in self.segments.scan_records(seg).into_iter().enumerate() {
                if !entry.deletion_vec.is_deleted(row as u32) {
                    merged.push(rec);
                }
            }
            // 把「当前」upgrade 块并进新段（按 (trace,span) 键，行号变了也不影响）→ 补写不丢
            if let Some(up) = &entry.upgrade_ref {
                for (&(t, s), fields) in up.iter() {
                    merged_upgrade = merged_upgrade.with_patch(t, s, fields.clone(), up_chunk_id);
                }
            }
        }

        let new_seg = self.alloc_segment_id();
        self.segments.flush_to_segment(new_seg, &merged);
        let bloom = KeyBloom::build(merged.iter().map(|r| (r.trace_id, r.span_id)), merged.len());
        self.seg_key_bloom.lock().unwrap().insert(new_seg.get(), Arc::new(bloom));
        let (min_ts, max_ts) = ts_range(&merged);
        let has_upgrade = merged_upgrade.iter().next().is_some();

        let mut draft = self.current.cow_next();
        let v_dead = draft.version.get();
        for s in &plan.inputs {
            draft.segments.remove(&s.get());
        }
        draft.segments.insert(
            new_seg.get(),
            SegmentEntry {
                segment_id: new_seg,
                level: 1,
                state: SegState::Live,
                min_ts,
                max_ts,
                deletion_vec: Arc::new(DeletionVec::empty()), // 删除已物化进 merged，新段从干净开始
                deletion_seq: 0,
                upgrade_ref: has_upgrade.then(|| Arc::new(merged_upgrade)),
                upgrade_seq: 0,
            },
        );
        self.commit_and_persist(draft);

        let mut dead = self.dead_set.lock().unwrap();
        for s in &plan.inputs {
            dead.push(DeadResource { seg: *s, v_dead });
        }
        reconciled
    }

    /// 便捷：无并发窗口的一次性 compaction（begin + finish 连续）。
    pub fn commit_compaction(&self, inputs: &[SegmentId]) {
        let n_in = inputs.len();
        let plan = self.compaction_begin(inputs);
        self.compaction_finish(&plan);
        olog::log(olog::Level::Info, "compaction", &[
            ("inputs", &n_in),
            ("version", &self.current.version()),
        ]);
    }

    /// 取 / 放一个段文件的 buffer pin（读路径扫段字节时持有，用完释放）。
    pub fn pin_buffer(&self, seg: SegmentId) {
        self.buffer_pins.pin(seg);
    }
    pub fn unpin_buffer(&self, seg: SegmentId) {
        self.buffer_pins.unpin(seg);
    }

    /// 段回收线程的一轮（草案 1 §D1.4）。对 dead_set 里每个资源，三条同真才物理删除：
    ///   (1) v_dead ≤ safe_version   (没有读者还 pin 在它 dead 之前的版本)
    ///   (2) ∧ 无未释放的 buffer pin  (字节级最后保险)
    ///   (3) ∧ 不被当前 manifest 引用 (防崩溃竞态)
    /// 返回这一轮回收了多少个段。真实实现是后台线程 + IO 限速。
    ///
    /// **崩溃安全**：持久模式（gc.log 存在）下，每个可删段走 MARK→fsync→unlink→DONE→fsync。
    /// 崩溃在 unlink 前（只写了 MARK）：重启据 gc.log 补删（文件还在 → 删）；崩溃在 unlink 后 DONE 前
    /// （文件已没）：重启据 gc.log 判定文件可能已删，幂等补删（不存在跳过）。两边都不留"删一半 +
    /// manifest 没更新"的不一致。
    ///
    /// **非持久模式**（gc.log 不存在）：reclaim 走旧的"直接删"路径——仅靠"段 id 永不复用 + compaction
    /// 只产新段"这两个不变量兜底，没有崩溃恢复。这是纯内存 / 测试场景可接受的退化。
    pub fn reclaim(&self) -> usize {
        let safe = self.current.safe_version();
        let mut freed = 0;
        let mut dead = self.dead_set.lock().unwrap();
        let mut gc = self.gc_log.lock().unwrap();
        dead.retain(|r| {
            let ok = r.v_dead <= safe
                && !self.buffer_pins.is_pinned(r.seg)
                && !self.current.contains_segment(r.seg);
            if !ok {
                return true; // 留着，下一轮再看
            }
            // 崩溃安全路径：MARK → fsync → unlink → DONE → fsync。
            if let Some(log) = gc.as_mut() {
                // MARK 写失败 = 意图没落盘，不能进 unlink（否则崩溃后无法恢复）。保守不删，留下轮。
                if log.mark(r.seg.get()).is_err() {
                    return true;
                }
            }
            self.segments.unlink_segment(r.seg);
            self.seg_fold_cache.lock().unwrap().remove(r.seg.get()); // 段没了，缓存失效
            self.seg_key_bloom.lock().unwrap().remove(&r.seg.get()); // bloom 同失效
            if let Some(log) = gc.as_mut() {
                // DONE 写失败：文件已删但完成标记没落盘。重启时会当成"MARK 没 DONE"补删——
                // 文件不存在了，unlink 幂等（store 实现容忍），正确。所以这里不回滚、继续。
                let _ = log.done(r.seg.get());
            }
            freed += 1;
            false // 出 dead_set
        });
        if freed > 0 {
            olog::log(olog::Level::Info, "reclaim", &[("freed", &freed), ("remaining_dead", &dead.len())]);
        }
        freed
    }

    /// 待回收 dead 资源数（可观测 / 测试用）。
    pub fn dead_count(&self) -> usize {
        self.dead_set.lock().unwrap().len()
    }

    /// **在线快照备份**（§3.3 数据安全底线）。
    ///
    /// 走 pin 协议拿一致快照（持有的版本不会被 GC），把所有持久文件拷到目标目录，
    /// 得到一个可独立 `open_durable` 恢复的一致快照。备份期间读写不阻塞（snapshot 隔离）。
    ///
    /// 拷的文件：`segments/`（目录）+ `wal.log` + `manifest.dat` + `vecindex/`（或 `vectors.dat`）+ `gc.log`。
    /// 段文件是不可变的、manifest 是当前版本快照——拷的是那一刻的一致态。
    /// WAL 可能比 manifest 新（有未 flush 的事务），recover 时重放水位之后的尾巴,幂等（确定性 event_id）。
    pub fn backup_snapshot(&self, dest: impl AsRef<std::path::Path>) -> std::io::Result<()> {
        let dest = dest.as_ref();
        let src = self.dir.as_ref().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::Unsupported, "backup 需要 open_durable 的数据目录")
        })?;
        // pin 住当前版本——拷贝期间 reclaim 不会删这个版本引用的段文件。
        let _snap = self.current.pin_snapshot();
        let version = self.current.version();
        olog::log(olog::Level::Info, "backup_start", &[("dest", &dest.to_string_lossy().to_string()), ("version", &version)]);

        std::fs::create_dir_all(dest)?;
        // 拷贝数据文件/目录。segments/ 和 vecindex/ 是目录,其余是文件。
        for name in ["segments", "vecindex"] {
            let s = src.join(name);
            if s.exists() {
                copy_dir_recursive(&s, &dest.join(name))?;
            }
        }
        for name in ["wal.log", "manifest.dat", "vectors.dat", "gc.log"] {
            let s = src.join(name);
            if s.exists() {
                std::fs::copy(&s, dest.join(name))?;
            }
        }
        olog::log(olog::Level::Info, "backup_done", &[("dest", &dest.to_string_lossy().to_string()), ("version", &version)]);
        Ok(())
    }

    /// 生产可观测（§3.1）：聚合所有关键运行态，供 /metrics 端点输出。
    /// 返回的字符串是 Prometheus 文本格式（每行一个 metric + 注释），零依赖、好排查。
    /// 返回 owned String，调用者直接写进 HTTP body。
    pub fn metrics(&self) -> String {
        let mut out = String::with_capacity(2048);
        let version = self.current.version();
        let segments = self.current.manifest().segments.len();
        let memtable_rows = self.memtable_len();
        let dead = self.dead_count();
        let active_readers = self.current.active_reader_count();
        let committed_tail = self.current.committed_tail();
        let flush_threshold = self.flush_threshold.load(Ordering::Relaxed);
        let filter_attrs = self.filter_attrs.lock().unwrap().len();
        let fold_cache_entries = self.seg_fold_cache.lock().unwrap().map.len();
        let bloom_count = self.seg_key_bloom.lock().unwrap().len();
        let datasets = self.datasets.lock().unwrap().len();

        // 确定性 manifest 版本（每次 commit +1）。
        out.push_str("# HELP yt_manifest_version Manifest 版本号（每次 commit +1）。\n");
        out.push_str("# TYPE yt_manifest_version gauge\n");
        out.push_str(&format!("yt_manifest_version {version}\n\n"));

        out.push_str("# HELP yt_format_version 数据格式版本（persist::FORMAT_VER，升级迁移用）。\n");
        out.push_str("# TYPE yt_format_version gauge\n");
        out.push_str(&format!("yt_format_version {}\n\n", persist::FORMAT_VER));

        out.push_str("# HELP yt_segments_live 活跃段数（含 sealed/live/compacting）。\n");
        out.push_str("# TYPE yt_segments_live gauge\n");
        out.push_str(&format!("yt_segments_live {segments}\n\n"));

        out.push_str("# HELP yt_memtable_rows 活内存表行数。\n");
        out.push_str("# TYPE yt_memtable_rows gauge\n");
        out.push_str(&format!("yt_memtable_rows {memtable_rows}\n\n"));

        out.push_str("# HELP yt_segments_dead 待回收 dead 段数（compaction 摘下、等水位满足删）。\n");
        out.push_str("# TYPE yt_segments_dead gauge\n");
        out.push_str(&format!("yt_segments_dead {dead}\n\n"));

        out.push_str("# HELP yt_readers_active 活跃快照读者数（pin 了某版本的）。\n");
        out.push_str("# TYPE yt_readers_active gauge\n");
        out.push_str(&format!("yt_readers_active {active_readers}\n\n"));

        out.push_str("# HELP yt_wal_committed_tail 已确认的最大 WAL LSN。\n");
        out.push_str("# TYPE yt_wal_committed_tail counter\n");
        out.push_str(&format!("yt_wal_committed_tail {committed_tail}\n\n"));

        out.push_str("# HELP yt_flush_threshold 内存表自动刷盘阈值（行数）。\n");
        out.push_str("# TYPE yt_flush_threshold gauge\n");
        out.push_str(&format!("yt_flush_threshold {flush_threshold}\n\n"));

        out.push_str("# HELP yt_filter_attrs 检索过滤属性边车条目数。\n");
        out.push_str("# TYPE yt_filter_attrs gauge\n");
        out.push_str(&format!("yt_filter_attrs {filter_attrs}\n\n"));

        out.push_str("# HELP yt_fold_cache_entries 段折叠缓存条目数（解码后的段）。\n");
        out.push_str("# TYPE yt_fold_cache_entries gauge\n");
        out.push_str(&format!("yt_fold_cache_entries {fold_cache_entries}\n\n"));

        out.push_str("# HELP yt_seg_bloom_count 段级 key Bloom 条目数。\n");
        out.push_str("# TYPE yt_seg_bloom_count gauge\n");
        out.push_str(&format!("yt_seg_bloom_count {bloom_count}\n\n"));

        out.push_str("# HELP yt_datasets 评测数据集数。\n");
        out.push_str("# TYPE yt_datasets gauge\n");
        out.push_str(&format!("yt_datasets {datasets}\n"));

        out
    }

    /// 当前引擎支持的数据格式版本（persist::FORMAT_VER）。
    pub fn format_version() -> u32 {
        persist::FORMAT_VER
    }

    /// 检查数据目录的 manifest 版本：返回 (磁盘上的版本, 引擎支持的版本)。
    /// 两者相等 = 兼容；磁盘 < 引擎 = 需迁移；磁盘 > 引擎 = 需新引擎。
    /// 无 manifest = 新目录（返回 (0, FORMAT_VER)）。
    pub fn check_format(dir: impl AsRef<std::path::Path>) -> (u32, u32) {
        let manifest_path = dir.as_ref().join("manifest.dat");
        match std::fs::read(&manifest_path) {
            Ok(bytes) => {
                // 文件布局：[crc32 u32][MAGIC u32][FORMAT_VER u32]...
                // 跳过 4 字节 crc 前缀读 magic + version。
                if bytes.len() < 12 {
                    return (0, persist::FORMAT_VER);
                }
                let magic = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
                if magic != 0x5654_4D46 {
                    return (0, persist::FORMAT_VER); // 损坏或非本格式
                }
                let disk_ver = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
                (disk_ver, persist::FORMAT_VER)
            }
            Err(_) => (0, persist::FORMAT_VER), // 无文件 = 新目录
        }
    }

    /// **迁移骨架**（§3.4）：把数据目录从 `from_ver` 升级到当前引擎版本。
    ///
    /// 当前 FORMAT_VER=1，无历史老版本数据，所以 from_ver 只可能是 1（无操作）或损坏（报错）。
    /// 真实迁移工具的逻辑（版本 1→2、2→3…）会在引入格式变更时逐版本实现，沿这个签名扩展。
    pub fn migrate(dir: impl AsRef<std::path::Path>) -> std::io::Result<()> {
        let (disk, current) = Self::check_format(&dir);
        match disk.cmp(&current) {
            std::cmp::Ordering::Equal => {
                olog::log(olog::Level::Info, "migrate", &[("status", &"already current"), ("ver", &disk)]);
                Ok(())
            }
            std::cmp::Ordering::Less => {
                olog::log(olog::Level::Error, "migrate", &[
                    ("status", &"old version not yet supported"),
                    ("disk", &disk),
                    ("engine", &current),
                ]);
                Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    format!("从格式版本 {} 迁移到 {} 尚未实现（当前引擎无历史老版本数据）", disk, current),
                ))
            }
            std::cmp::Ordering::Greater => {
                olog::log(olog::Level::Error, "migrate", &[
                    ("status", &"data newer than engine"),
                    ("disk", &disk),
                    ("engine", &current),
                ]);
                Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    format!("数据格式版本 {} 比引擎支持的 {} 新，需升级引擎", disk, current),
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use yt_core::fold::SpanFields;

    /// 测试用的空段存储。
    struct NoopStore;
    impl SegmentStore for NoopStore {
        fn flush_to_segment(&self, _seg: SegmentId, _records: &[WalRecord]) {}
        fn scan_fold_inputs(&self, _seg: SegmentId) -> Vec<(u32, FoldInput)> {
            Vec::new()
        }
        fn scan_records(&self, _seg: SegmentId) -> Vec<WalRecord> {
            Vec::new()
        }
        fn unlink_segment(&self, _seg: SegmentId) {}
    }

    /// 记录被 unlink 的段 id，供回收测试断言。
    #[derive(Default)]
    struct RecordingStore {
        unlinked: Mutex<Vec<u64>>,
    }
    impl RecordingStore {
        fn unlinked(&self) -> Vec<u64> {
            self.unlinked.lock().unwrap().clone()
        }
    }
    impl SegmentStore for RecordingStore {
        fn flush_to_segment(&self, _seg: SegmentId, _records: &[WalRecord]) {}
        fn scan_fold_inputs(&self, _seg: SegmentId) -> Vec<(u32, FoldInput)> {
            Vec::new()
        }
        fn scan_records(&self, _seg: SegmentId) -> Vec<WalRecord> {
            Vec::new()
        }
        fn unlink_segment(&self, seg: SegmentId) {
            self.unlinked.lock().unwrap().push(seg.get());
        }
    }

    /// 支持下推的 mock 段存储：时间下推 / 投影下推都真做，并把「最近一次收到的投影」与「时间下推次数」
    /// 记下来，供测试断言引擎确实走了下推、且传下来的投影是窄的（聚合不带文本列）。
    #[derive(Default)]
    struct PushdownStore {
        rows: Mutex<std::collections::HashMap<u64, Vec<WalRecord>>>,
        pushdowns: std::sync::atomic::AtomicUsize,
        /// 最近一次任意下推（时间/投影）收到的投影位，供断言"聚合查询不要文本列"。
        last_proj: std::sync::atomic::AtomicU16,
    }
    impl PushdownStore {
        fn last_proj(&self) -> Projection {
            Projection::of(self.last_proj.load(std::sync::atomic::Ordering::Relaxed))
        }
    }
    impl SegmentStore for PushdownStore {
        fn flush_to_segment(&self, seg: SegmentId, records: &[WalRecord]) {
            self.rows.lock().unwrap().insert(seg.get(), records.to_vec());
        }
        fn scan_fold_inputs(&self, seg: SegmentId) -> Vec<(u32, FoldInput)> {
            self.rows
                .lock()
                .unwrap()
                .get(&seg.get())
                .map(|rs| rs.iter().enumerate().map(|(i, r)| (i as u32, r.to_fold_input())).collect())
                .unwrap_or_default()
        }
        fn scan_records(&self, seg: SegmentId) -> Vec<WalRecord> {
            self.rows.lock().unwrap().get(&seg.get()).cloned().unwrap_or_default()
        }
        fn unlink_segment(&self, seg: SegmentId) {
            self.rows.lock().unwrap().remove(&seg.get());
        }
        fn scan_fold_inputs_projected(&self, seg: SegmentId, proj: Projection) -> Option<Vec<(u32, FoldInput)>> {
            self.last_proj.store(proj.bits(), std::sync::atomic::Ordering::Relaxed);
            Some(self.scan_fold_inputs(seg))
        }
        fn scan_fold_inputs_in_time(&self, seg: SegmentId, from: i64, to: i64, proj: Projection) -> Option<Vec<FoldInput>> {
            self.pushdowns.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.last_proj.store(proj.bits(), std::sync::atomic::Ordering::Relaxed);
            let g = self.rows.lock().unwrap();
            Some(
                g.get(&seg.get())
                    .map(|rs| rs.iter().filter(|r| r.ts >= from && r.ts <= to).map(|r| r.to_fold_input()).collect())
                    .unwrap_or_default(),
            )
        }
    }

    /// 端到端测试用的段存储 = 公开的内存段存储（flush 存、scan 返回、unlink 真删）。
    use super::InMemorySegmentStore as CapturingStore;

    fn rec(span: &str, seq: u64) -> WalRecord {
        WalRecord {
            trace_id: 1,
            span_id: seq,
            ts: seq as i64,
            identity: EventIdentity { ext_span_id: span.into(), seq, event_type: EventType::SpanEnd },
            fields: SpanFields::default(),
        }
    }

    /// 带可折叠字段的事件构造器（ts 默认 = seq）。
    fn ev(trace: u64, span: u64, seq: u64, status: Option<u8>, dur: Option<u64>, logs: &[&str]) -> WalRecord {
        ev_at(trace, span, seq, seq as i64, status, dur, logs)
    }

    /// 指定时间戳的事件构造器。
    fn ev_at(trace: u64, span: u64, seq: u64, ts: i64, status: Option<u8>, dur: Option<u64>, logs: &[&str]) -> WalRecord {
        WalRecord {
            trace_id: trace,
            span_id: span,
            ts,
            identity: EventIdentity { ext_span_id: format!("{trace}-{span}"), seq, event_type: EventType::Attr },
            fields: SpanFields {
                status,
                duration_ns: dur,
                logs: logs.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            },
        }
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
        assert_eq!(spans.len(), 1, "段里的 start + 内存里的 end 折叠成一条 span");
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

        let rows = vec![ev(1, 10, 1, Some(0), None, &[]), ev(1, 20, 1, Some(1), None, &[])];
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
        assert_eq!(wc.read_spans(&before).len(), 2, "老读者快照不受后来的删除影响");
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
        assert_eq!(before[0].event_count, 2, "段+内存已重叠，event_id 去重 → 仍是 2");
        drop(snap0);

        // 崩溃：丢内存表
        wc.simulate_crash_lose_memtable();
        // 恢复：从 WAL 重放 watermark(0) 之后的记录 → e1,e2 回到内存表
        wc.recover();

        // 恢复后再读：段(e1,e2) 与重放回内存的(e1,e2) 重叠，但确定性 event_id 去重 → 逐字段一致
        let snap1 = wc.pin_snapshot();
        let after = wc.read_spans(&snap1);
        assert_eq!(after, before, "崩溃恢复前后折叠结果逐字段一致（重放幂等）");
        assert_eq!(after[0].event_count, 2, "没有因为重放把事件算两遍 → token/cost 不翻倍");
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
        assert_eq!(after, before, "崩溃重放前后逐字段一致 —— 补写字段没因重叠去重而丢");
        assert_eq!(after[0].event_count, 2, "base 事件没被算两遍");
        assert_eq!(after[0].eval_score, Some(900), "补写的 eval_score 重放后仍在");
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
            SpanFields { status: None, duration_ns: Some(999), logs: vec!["late".into()], ..Default::default() },
        );

        // 升级后新读者：duration 来自补写，status 仍是原值，日志并集
        let after = wc.pin_snapshot();
        let s = wc.read_spans(&after);
        assert_eq!(s[0].status, Some(0), "status 没被补写动（补写 status=None）");
        assert_eq!(s[0].duration_ns, Some(999), "duration 来自晚到补写");
        assert_eq!(s[0].logs, vec!["start", "late"]);

        // 快照隔离：升级前 pin 的读者仍看到未升级的值
        assert_eq!(wc.read_spans(&before)[0].duration_ns, None, "老读者不受后来补写影响");
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
            SpanFields { model: Some("qwen3".into()), output_tokens: Some(42), ..Default::default() },
        );

        let snap = wc.pin_snapshot();
        let s = &wc.read_spans(&snap)[0];
        assert_eq!(s.model.as_deref(), Some("qwen3"), "upgrade 补的 model 必须读得到");
        assert_eq!(s.output_tokens, Some(42), "upgrade 补的 output_tokens 必须读得到");
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
        assert_eq!(store.pushdowns.load(Ordering::Relaxed), n0, "全开窗不触发下推");

        // 时间窗 [150,250] → 触发下推,行级过滤只剩 span20(ts=200)。
        let (hit, _) = wc.read_spans_query(&snap, &TraceQuery { trace_id: None, time_from: 150, time_to: 250, tenant_id: None });
        assert!(store.pushdowns.load(Ordering::Relaxed) > n0, "有时间窗 → 走下推");
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
        assert!(p.has(Projection::AGENT_NAME) && p.has(Projection::INPUT_TOKENS), "聚合要的列在投影里");
        assert!(
            !p.has(Projection::INPUT_TEXT) && !p.has(Projection::OUTPUT_TEXT),
            "聚合不读原文 → 投影不含大文本列(列式段据此跳过解码)"
        );

        // trace 详情:读全列,原文必须读得到。
        let detail = &wc.read_spans(&snap)[0];
        assert_eq!(detail.output_text.as_deref(), Some("一大段研判正文……"), "详情读全列、原文在");
        assert!(store.last_proj().is_all(), "详情下推的是全列投影");
        let _ = store.pushdowns.load(Ordering::Relaxed); // 触达字段,消除未读告警
    }

    #[test]
    fn search_text_and_vector_find_and_fold_spans() {
        // 产品噱头：按中文内容搜 trace、按向量找相似 trace。
        let store = Arc::new(CapturingStore::default());
        let wc = WriteCoordinator::new(store.clone());

        let e1 = ev(1, 10, 1, Some(0), Some(100), &["用户登录 风控通过"]);
        let e2 = ev(2, 20, 1, Some(0), Some(200), &["疑似盗刷 已拦截"]);
        let e3 = ev(3, 30, 1, Some(0), Some(300), &["转账成功"]);
        let all = vec![e1.clone(), e2.clone(), e3.clone()];
        wc.ingest(all.clone());
        wc.commit_flush(&all, WalLsn::new(3));

        // 给三个 span 各加一个二维向量
        wc.index_embedding(1, 10, vec![0.0, 0.0]);
        wc.index_embedding(2, 20, vec![1.0, 0.0]);
        wc.index_embedding(3, 30, vec![5.0, 5.0]);

        let snap = wc.pin_snapshot();

        // 中文检索「盗刷」：只命中 span(2,20)，且折叠出完整 span（带 duration）
        let hits = wc.search_text(&snap, "盗刷", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!((hits[0].0.trace_id, hits[0].0.span_id), (2, 20));
        assert_eq!(hits[0].0.duration_ns, Some(200), "返回的是折叠出的完整 span，不只是命中行");

        // 向量找相似：查 [0.9,0.0] 最近的是 span(2,20) 的 [1,0]，其次 span(1,10) 的 [0,0]
        let sim = wc.search_similar(&snap, &[0.9, 0.0], 2);
        assert_eq!(sim.len(), 2);
        assert_eq!((sim[0].0.trace_id, sim[0].0.span_id), (2, 20));
        assert_eq!((sim[1].0.trace_id, sim[1].0.span_id), (1, 10));
    }

    #[test]
    fn builder_injects_custom_tokenizer_end_to_end() {
        // 注入口验证：用 CoordinatorBuilder 换分词器后起引擎，自定义分词一路贯穿到 search_text。
        // 这条就是「团队 jieba 到位后只换分词层」在引擎层的契约。
        struct WordTokenizer; // 按空白切，整段中文当一个词（不拆 bigram）
        impl Tokenizer for WordTokenizer {
            fn tokenize(&self, text: &str) -> Vec<String> {
                text.split_whitespace().map(|w| w.to_lowercase()).collect()
            }
        }

        let store = Arc::new(CapturingStore::default());
        let wc = CoordinatorBuilder::new()
            .with_tokenizer(Box::new(WordTokenizer))
            .build(store);

        // (1,10) 文本里 "风控" 是独立词；(2,20) 没有空格分隔的 "风控" 词。
        let e1 = ev(1, 10, 1, Some(0), Some(100), &["盗刷 风控 已拦截"]);
        let e2 = ev(2, 20, 1, Some(0), Some(200), &["盗刷风控合并成一个词"]);
        let all = vec![e1.clone(), e2.clone()];
        wc.ingest(all.clone());
        wc.commit_flush(&all, WalLsn::new(2));
        let snap = wc.pin_snapshot();

        // 注入的分词器决定切分：查 "风控" 只命中 (1,10)（默认 bigram 会把两条都命中）。
        let hits = wc.search_text(&snap, "风控", 10);
        assert_eq!(hits.len(), 1, "注入的分词器一路生效到检索");
        assert_eq!((hits[0].0.trace_id, hits[0].0.span_id), (1, 10));
    }

    #[test]
    fn segment_key_bloom_skips_unrelated_segments_keeps_results() {
        // 段级 bloom：候选 key 只在段 A，段 B 的 bloom 拒绝它 → B 被跳过，结果仍正确。
        let store = Arc::new(CapturingStore::default());
        let wc = WriteCoordinator::new(store);
        // 段 A：trace 1 含"盗刷"
        let a = ev(1, 10, 1, Some(0), Some(100), &["疑似盗刷 已拦截"]);
        wc.ingest(vec![a.clone()]);
        wc.flush_memtable(); // → 段 A，建 bloom（含 (1,10)）
        // 段 B：trace 2 不含"盗刷"，且 key 不同
        let b = ev(2, 20, 1, Some(0), Some(200), &["正常转账"]);
        wc.ingest(vec![b.clone()]);
        wc.flush_memtable(); // → 段 B，建 bloom（含 (2,20)，不含 (1,10)）

        // 两段都有 bloom
        assert_eq!(wc.seg_key_bloom.lock().unwrap().len(), 2);
        let snap = wc.pin_snapshot();
        // 查"盗刷"：候选 (1,10) 只在段 A；段 B 的 bloom 拒绝它 → 只回 trace 1，结果正确。
        let hits = wc.search_text(&snap, "盗刷", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!((hits[0].0.trace_id, hits[0].0.span_id), (1, 10));
        assert_eq!(hits[0].0.duration_ns, Some(100), "折叠出完整 span（跨段定位正确）");
        // 直接验证 bloom 语义：段 B 的 bloom 对 (1,10) 说"肯定没有"。
        let blooms = wc.seg_key_bloom.lock().unwrap();
        let seg_ids: Vec<u64> = snap.manifest.segments.keys().copied().collect();
        let any_rejects_a = seg_ids.iter().any(|sid| blooms.get(sid).map_or(false, |bl| !bl.maybe_contains((1, 10))));
        assert!(any_rejects_a, "应有段的 bloom 对 (1,10) 判定肯定没有");
    }

    #[test]
    fn tenant_filter_isolates_list_and_read_paths() {
        // 列表/读路径的租户隔离：read_spans_query / list_traces 带 tenant → 只见本租户。
        let store = Arc::new(CapturingStore::default());
        let wc = WriteCoordinator::new(store);
        let mut a = ev(1, 10, 1, Some(0), Some(100), &["t1"]);
        a.fields.tenant_id = Some(1);
        let mut b = ev(2, 20, 1, Some(0), Some(200), &["t2"]);
        b.fields.tenant_id = Some(2);
        let all = vec![a, b];
        wc.ingest(all.clone());
        wc.commit_flush(&all, WalLsn::new(2));
        let snap = wc.pin_snapshot();

        // 不带 tenant：两条都见。
        assert_eq!(wc.read_spans_query(&snap, &TraceQuery::all()).0.len(), 2);
        // 带 tenant 1：只见 trace 1。
        let (s1, _) = wc.read_spans_query(&snap, &TraceQuery::all().for_tenant(1));
        assert_eq!(s1.len(), 1);
        assert_eq!(s1[0].trace_id, 1);
        // 列表也隔离。
        let l1 = wc.list_traces(&snap, &TraceQuery::all().for_tenant(1));
        assert!(l1.iter().all(|t| t.trace_id == 1) && !l1.is_empty(), "列表只见租户1");
        let l2 = wc.list_traces(&snap, &TraceQuery::all().for_tenant(2));
        assert!(l2.iter().all(|t| t.trace_id == 2) && !l2.is_empty(), "列表只见租户2");
    }

    #[test]
    fn tenant_filter_isolates_search_across_tenants() {
        // 逻辑隔离：共享一套索引，查询强制带 tenant 过滤 → 只见本租户的 span（BM25 文本 + 向量找相似都隔离）。
        let store = Arc::new(CapturingStore::default());
        let wc = WriteCoordinator::new(store);

        // 两个租户各一条"盗刷"相关 span，文本相同、向量相近 —— 不隔离的话会互相召回。
        let mut a = ev(1, 10, 1, Some(0), Some(100), &["疑似盗刷 已拦截"]);
        a.fields.tenant_id = Some(1);
        let mut b = ev(2, 20, 1, Some(0), Some(200), &["疑似盗刷 已拦截"]);
        b.fields.tenant_id = Some(2);
        let all = vec![a.clone(), b.clone()];
        wc.ingest(all.clone());
        wc.commit_flush(&all, WalLsn::new(2));
        wc.index_embedding(1, 10, vec![0.0, 0.0]);
        wc.index_embedding(2, 20, vec![0.01, 0.0]); // 和租户1的几乎重合
        let snap = wc.pin_snapshot();

        let t1 = SearchFilter { tenant_id: Some(1), ..Default::default() };
        let t2 = SearchFilter { tenant_id: Some(2), ..Default::default() };

        // BM25 文本检索：查"盗刷"，scope 租户1 → 只回 (1,10)，不漏租户2。
        let txt1 = wc.search_text_attr(&snap, "盗刷", 10, &t1);
        assert!(txt1.iter().all(|(s, _)| s.trace_id == 1), "租户1 文本检索不漏租户2");
        assert!(txt1.iter().any(|(s, _)| s.span_id == 10));
        let txt2 = wc.search_text_attr(&snap, "盗刷", 10, &t2);
        assert!(txt2.iter().all(|(s, _)| s.trace_id == 2), "租户2 文本检索不漏租户1");

        // 向量找相似：scope 租户1 → 即便租户2的向量更近也不返回（进图过滤隔离）。
        let sim1 = wc.search_similar_attr(&snap, &[0.0, 0.0], 10, &t1);
        assert!(!sim1.is_empty());
        assert!(sim1.iter().all(|(s, _)| s.trace_id == 1), "租户1 找相似不漏租户2（向量更近也挡）");
        let sim2 = wc.search_similar_attr(&snap, &[0.0, 0.0], 10, &t2);
        assert!(sim2.iter().all(|(s, _)| s.trace_id == 2), "租户2 找相似不漏租户1");
    }

    #[test]
    fn builder_injects_custom_graph_index_end_to_end() {
        // 注入口验证：用 CoordinatorBuilder 换 GraphIndex 后，search_similar 走的是注入的实现，不是默认 ANN。
        // 这条是「团队 graph_index 到位后只换向量索引层」在引擎层的契约（与 jieba 那条对称）。
        struct StubGraph; // 无视查询向量，永远只返回 (7,99) —— 默认 L2 ANN 不会这么选
        impl GraphIndex for StubGraph {
            fn index_embedding(&self, _t: u64, _s: u64, _e: Vec<f32>) {}
            fn search(&self, _q: &[f32], _k: usize, _f: &dyn Fn(u64, u64) -> bool) -> Vec<(u64, u64, f32)> {
                vec![(7, 99, 0.0)]
            }
        }

        let store = Arc::new(CapturingStore::default());
        let wc = CoordinatorBuilder::new().with_graph(Arc::new(StubGraph)).build(store);

        // 两个 span 都摄入（才能被折叠出来）；查询向量明显更靠近 (1,10)。
        let e1 = ev(1, 10, 1, Some(0), Some(100), &["a"]);
        let e2 = ev(7, 99, 1, Some(0), Some(700), &["b"]);
        let all = vec![e1.clone(), e2.clone()];
        wc.ingest(all.clone());
        wc.commit_flush(&all, WalLsn::new(2));
        let snap = wc.pin_snapshot();

        let sim = wc.search_similar(&snap, &[0.0, 0.0], 5);
        assert_eq!(sim.len(), 1, "注入的图索引决定返回什么");
        assert_eq!((sim[0].0.trace_id, sim[0].0.span_id), (7, 99), "走的是 StubGraph，不是默认 L2");
        assert_eq!(sim[0].0.duration_ns, Some(700), "返回折叠出的完整 span");
    }

    #[test]
    fn hybrid_fusion_beats_single_signal() {
        let store = Arc::new(CapturingStore::default());
        let wc = WriteCoordinator::new(store.clone());
        let e1 = ev(1, 10, 1, Some(0), Some(100), &["用户登录 风控通过"]);
        let e2 = ev(2, 20, 1, Some(0), Some(200), &["疑似盗刷 已拦截"]);
        let e3 = ev(3, 30, 1, Some(0), Some(300), &["转账成功"]);
        let all = vec![e1.clone(), e2.clone(), e3.clone()];
        wc.ingest(all.clone());
        wc.commit_flush(&all, WalLsn::new(3));
        wc.index_embedding(1, 10, vec![0.0, 0.0]);
        wc.index_embedding(2, 20, vec![1.0, 0.0]);
        wc.index_embedding(3, 30, vec![5.0, 5.0]);
        let snap = wc.pin_snapshot();

        // 向量查 [0.1,0.0]：单走向量,最近的是 span(1,10)
        assert_eq!((wc.search_similar(&snap, &[0.1, 0.0], 3)[0].0.trace_id), 1);

        // 混合「盗刷」+ 同一个向量：span(2,20) 被关键词和语义双命中 → 融合后反超到第一,
        // 这是单走向量给不出的排序。
        let hy = wc.search_hybrid(&snap, "盗刷", &[0.1, 0.0], 3);
        assert_eq!((hy[0].0.trace_id, hy[0].0.span_id), (2, 20), "双命中的 span 经 RRF 融合居首");
        assert_eq!(hy[1].0.trace_id, 1, "向量单命中的次之");
    }

    #[test]
    fn search_folds_only_hit_rows_across_sources() {
        // 只折叠命中行:命中 span 的 start 在段、end 在内存,检索仍跨源折叠正确;无关 span 不进结果。
        let store = Arc::new(CapturingStore::default());
        let wc = WriteCoordinator::new(store);
        let start = ev(2, 20, 1, Some(0), None, &["疑似盗刷"]); // 段
        wc.ingest(vec![start.clone()]);
        wc.commit_flush(&[start], WalLsn::new(1));
        wc.ingest(vec![ev(2, 20, 2, None, Some(500), &["已拦截"])]); // 内存
        // 噪声 span(别的 trace),不该被折进检索结果。
        wc.ingest(vec![ev(1, 10, 1, Some(0), Some(9), &["登录"]), ev(3, 30, 1, Some(0), Some(9), &["转账"])]);

        let snap = wc.pin_snapshot();
        let hits = wc.search_text(&snap, "盗刷", 10);
        assert_eq!(hits.len(), 1, "只命中 span(2,20),噪声不进结果");
        let s = &hits[0].0;
        assert_eq!((s.trace_id, s.span_id), (2, 20));
        assert_eq!(s.status, Some(0), "来自段的 start");
        assert_eq!(s.duration_ns, Some(500), "来自内存的 end");
        assert_eq!(s.logs, vec!["疑似盗刷", "已拦截"], "命中行跨源折叠正确");
        assert_eq!(s.event_count, 2);
    }

    #[test]
    fn filtered_similar_search_pushes_predicate_into_graph() {
        // 进图过滤接到引擎层:谓词下推进 graph.search,即便被排除 trace 里有更近的点,也不返回。
        let store = Arc::new(CapturingStore::default());
        let wc = WriteCoordinator::new(store);
        let rows = vec![
            ev(1, 10, 1, Some(0), Some(100), &["a"]),
            ev(1, 11, 1, Some(0), Some(100), &["b"]),
            ev(2, 20, 1, Some(0), Some(100), &["c"]),
        ];
        wc.ingest(rows);
        wc.index_embedding(1, 10, vec![0.0, 1.0]);
        wc.index_embedding(1, 11, vec![0.0, 2.0]);
        wc.index_embedding(2, 20, vec![0.0, 0.0]); // 离 query[0,0] 最近,但属 trace2

        let snap = wc.pin_snapshot();
        // 不过滤:最近的是 trace2 的 span20
        let all = wc.search_similar(&snap, &[0.0, 0.0], 3);
        assert_eq!((all[0].0.trace_id, all[0].0.span_id), (2, 20));

        // 只搜 trace1:谓词下推进图,trace2 的最近点被排除,仍能召回 trace1 里最近的 span10。
        let only1 = wc.search_similar_filtered(&snap, &[0.0, 0.0], 3, &|t, _| t == 1);
        assert!(only1.iter().all(|(s, _)| s.trace_id == 1), "过滤后只剩 trace1");
        assert!(!only1.iter().any(|(s, _)| s.span_id == 20), "trace2 的最近点被进图过滤排除");
        assert_eq!((only1[0].0.trace_id, only1[0].0.span_id), (1, 10), "trace1 里离 query 最近的是 span10");
    }

    #[test]
    fn attr_filtered_search_filters_by_agent_status_and_time() {
        // 带过滤 ANN 在真实查询维度上:按 agent / 状态 / 时间过滤,不只 (trace,span) id。
        let store = Arc::new(CapturingStore::default());
        let wc = WriteCoordinator::new(store);
        let mut e1 = ev_at(1, 10, 1, 100, Some(0), Some(100), &["a"]); // 风控, 正常, 最近
        e1.fields.agent_name = Some("风控".into());
        let mut e2 = ev_at(1, 11, 1, 200, Some(1), Some(100), &["b"]); // 规划, 报错
        e2.fields.agent_name = Some("规划".into());
        let mut e3 = ev_at(2, 20, 1, 300, Some(1), Some(100), &["c"]); // 风控, 报错, 较远
        e3.fields.agent_name = Some("风控".into());
        wc.ingest(vec![e1, e2, e3]);
        wc.index_embedding(1, 10, vec![0.0, 0.0]); // 离 query[0,0] 最近
        wc.index_embedding(1, 11, vec![0.0, 1.0]);
        wc.index_embedding(2, 20, vec![0.0, 2.0]);

        let snap = wc.pin_snapshot();
        // 找 agent=风控 且 报错(status=1) 的相似:最近的 span10 是风控但正常 → 排除;命中 span20。
        let f = SearchFilter { agent_name: Some("风控".into()), status: Some(1), ..Default::default() };
        let hits = wc.search_similar_attr(&snap, &[0.0, 0.0], 5, &f);
        assert!(!hits.is_empty(), "应召回风控+报错的 span");
        assert!(hits.iter().all(|(s, _)| s.agent_name.as_deref() == Some("风控") && s.status == Some(1)));
        assert!(hits.iter().any(|(s, _)| s.span_id == 20), "命中风控+报错的 span20");
        assert!(!hits.iter().any(|(s, _)| s.span_id == 10), "最近但 status=0 被排除");
        assert!(!hits.iter().any(|(s, _)| s.span_id == 11), "agent 不符被排除");

        // 时间窗:只要 ts ≤ 150 → 只 span10(ts=100)。
        let ft = SearchFilter { time_to: Some(150), ..Default::default() };
        let timed = wc.search_similar_attr(&snap, &[0.0, 0.0], 5, &ft);
        assert!(!timed.is_empty() && timed.iter().all(|(s, _)| s.span_id == 10), "只剩时间窗内的 span10");
    }

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
        wc.commit_upgrade(SegmentId::new(1), 1, 10, SpanFields { status: None, duration_ns: Some(999), ..Default::default() });

        // 提交：重读合并,删除和补写都不能丢
        let reconciled = wc.compaction_finish(&plan);
        assert!(reconciled, "选段后 seq 变了 → 触发重读合并");

        let snap = wc.pin_snapshot();
        let spans = wc.read_spans(&snap);
        assert_eq!(spans.len(), 1, "span20 的删除没丢 → 只剩 span10");
        assert_eq!(spans[0].span_id, 10);
        assert_eq!(spans[0].duration_ns, Some(999), "span10 的晚到补写没丢 → 来自 upgrade");
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
            h.join().expect("线程不应 panic（无 use-after-free / 无断言失败）");
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
        assert!(wc.memtable_len() < 20, "内存表应被自动刷盘限制,而不是涨到 20");

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
                agent_name: None,
                tool_name: None,
                model: None,
                input_text: None,
                output_text: None,
                logs: vec!["交易风控 开始".into()],
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
                agent_name: None,
                tool_name: None,
                model: None,
                input_text: None,
                output_text: None,
                logs: vec!["疑似盗刷 已拦截".into()],
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
        assert_eq!(spans[0].input_tokens, Some(900), "token 从线格式透传 + 折叠");
        assert_eq!(spans[0].output_tokens, Some(150));
    }

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
        assert_eq!((exec.kind, exec.span_count, exec.input_tokens), (ActorKind::Agent, 2, 100));
        let kb = g.nodes.iter().find(|n| n.actor == "kb_lookup").unwrap();
        assert_eq!(kb.kind, ActorKind::Tool);

        // 边:规划→kb_lookup、规划→执行、执行→calc;执行→执行 自环被剔除。
        let edges: Vec<(&str, &str, usize)> =
            g.edges.iter().map(|e| (e.from.as_str(), e.to.as_str(), e.count)).collect();
        assert_eq!(
            edges,
            vec![("执行", "calc", 1), ("规划", "kb_lookup", 1), ("规划", "执行", 1)],
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
        let path = std::env::temp_dir()
            .join(format!("yt_engine_{}_{}.wal", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));

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
        let dir = std::env::temp_dir()
            .join(format!("yt_durable_{}_{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
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
        assert_eq!(spans.len(), 1, "flush 过的数据从持久段读回;被删的 span20 不出现(删除持久)");
        assert_eq!(spans[0].span_id, 10);
        assert_eq!(spans[0].logs, vec!["反洗钱"]);
        assert_eq!(spans[0].status, Some(0));

        // 新写入接着用,段 id 不复用(从持久 manifest 恢复了计数器)。
        wc2.ingest(vec![ev(2, 30, 1, Some(0), Some(10), &["转账"])]);
        wc2.flush_memtable();
        let snap2 = wc2.pin_snapshot();
        assert_eq!(wc2.read_spans(&snap2).len(), 2, "老段(span10)+新段(span30)都在");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn search_indexes_rebuilt_after_restart() {
        // 检索索引(BM25/向量/属性边车)重启后从持久段 + 向量文件重建 —— 不再是"重启后搜啥都空"。
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir()
            .join(format!("yt_idx_{}_{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
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
        assert!(hits.iter().any(|(s, _)| s.span_id == 10), "重启后按内容搜还能命中");

        // 找相似:向量从文件重载,查 [0.1,0.1] 最近的是 span10[0,0]。
        let sim = wc2.search_similar(&snap, &[0.1, 0.1], 10);
        assert!(!sim.is_empty(), "重启后找相似不为空(向量已重载)");
        assert_eq!((sim[0].0.trace_id, sim[0].0.span_id), (1, 10));

        // 带过滤:属性边车重建,按 agent 过滤还生效。
        let f = SearchFilter { agent_name: Some("风控".into()), ..Default::default() };
        let filtered = wc2.search_similar_attr(&snap, &[0.1, 0.1], 10, &f);
        assert!(filtered.iter().all(|(s, _)| s.span_id == 10), "重启后按 agent 过滤还生效");
        assert!(!filtered.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn durable_uses_disk_vector_index_and_survives_restart_without_rebuild() {
        // 阶段 3：持久引擎默认用**磁盘图索引**——向量+图都落盘到 dir/vecindex，不用 vecstore，
        // 重启从盘恢复、不全量 rebuild。append 多删除少场景：插入只写、提交点批量刷。
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir()
            .join(format!("yt_diskvec_{}_{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
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
        assert!(dir.join("vecindex").join("meta").exists(), "磁盘图索引已落盘");
        assert!(!dir.join("vectors.dat").exists(), "不再用 vecstore");

        // 重启：不 rebuild（recover 不重放向量，磁盘图索引自带持久），找相似照常。
        let wc2 = WriteCoordinator::open_durable(&dir).unwrap();
        wc2.recover();
        let snap = wc2.pin_snapshot();
        let sim = wc2.search_similar(&snap, &[0.9, 0.0, 0.0], 2);
        assert_eq!((sim[0].0.trace_id, sim[0].0.span_id), (2, 20), "重启后磁盘图搜索：最近的排第一");
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
        assert_eq!(tl.turns[0].user_input.as_deref(), Some("如何修改预留手机号"));
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
        assert_eq!(r[0].input_tokens, 800, "500(span1) + 300(span2)，end 不重复加 in");
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
        assert_eq!(find(20).output_text.as_deref(), Some("抱歉,我无法判断该交易"));
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
        let plan = sum.iter().find(|r| r.agent_name.as_deref() == Some("规划")).unwrap();
        assert_eq!((plan.scored_spans, plan.pass_count), (2, 1), "规划 agent 半数通过");
        assert_eq!(plan.avg_score, 500, "规划均分 = (1000+0)/2");
        let exec = sum.iter().find(|r| r.agent_name.as_deref() == Some("执行")).unwrap();
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
        let added = wc.collect_into_dataset("失败集", &snap, &TraceQuery::all(), &|s| s.eval_score == Some(0));
        assert_eq!(added, 2, "两条失败样本入集");
        // 去重:再收集一次不重复加。
        let again = wc.collect_into_dataset("失败集", &snap, &TraceQuery::all(), &|s| s.eval_score == Some(0));
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
        assert!(seg_dir.join("seg-1.dat").exists(), "模拟：段文件还在（unlink 前崩）");

        // 3) 重启：open_durable 应扫 gc.log → 发 seg1 "MARK 没 DONE" → 补删。
        let _wc2 = WriteCoordinator::open_durable(&dir).unwrap();
        assert!(!seg_dir.join("seg-1.dat").exists(), "重启后补删了残留段文件（崩溃安全）");

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
        let mut rng = |s: &mut u64| {
            *s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (*s >> 33) as usize
        };

        // 跑多个种子，每个种子一个独立序列。
        for seed_orig in [0xA11C, 0xB22D, 0xC33E, 0xD44F, 0xE550, 0xF661, 0x1234, 0x5678] {
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
                        let status = if rng(&mut seed) % 3 == 0 { Some(rng(&mut seed) as u8) } else { None };
                        let dur = if rng(&mut seed) % 2 == 0 { Some(100 * (1 + rng(&mut seed) as u64 % 5)) } else { None };
                        let logs_idx = rng(&mut seed) % 3;
                        let logs_str: &[&str] = match logs_idx { 0 => &["a"], 1 => &["b", "c"], _ => &["盗刷"] };
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
                            let inputs: Vec<SegmentId> = live_segs.iter().take(2).map(|(s, _)| SegmentId(*s)).collect();
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
            let actual_map: BTreeMap<(u64, u64), &FoldedSpan> =
                actual.iter().map(|s| ((s.trace_id, s.span_id), s)).collect();

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
}
