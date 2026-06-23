//! eval 测试框架演示 / 场景模拟器：
//!   cargo run -p yt-engine --example eval_harness --offline
//!
//! 自造 4 类 agent 场景（客服问答 / 风控研判多 agent / 代码助手 / 数据分析）的合成 trace，
//! 经**真实摄入路径**（ingest_wire）灌进引擎，跑完整 eval 闭环：
//!   打分写回 → per-agent 通过率看板 → 失败答案冻成回归数据集 → 更严 scorer 复跑检出回归。
//!
//! 改 n_per / seed 可调数据量与随机分布（同 seed 完全可复现）。

use std::sync::Arc;

use yt_engine::evalkit;
use yt_engine::{InMemorySegmentStore, WriteCoordinator};

fn main() {
    // ① per-span / per-agent 评测：4 类场景各 40 条 trace。
    let coord = WriteCoordinator::new(Arc::new(InMemorySegmentStore::default()));
    let report = evalkit::run_harness(&coord, 40, 20_260_623);
    evalkit::print_report(&report);

    // ② 会话级（多轮）评测：30 个连贯多轮会话（客服问答），按对话流打分。
    let conv = WriteCoordinator::new(Arc::new(InMemorySegmentStore::default()));
    let sreport = evalkit::run_session_harness(&conv, 30, 20_260_623);
    evalkit::print_session_report(&sreport);
}
