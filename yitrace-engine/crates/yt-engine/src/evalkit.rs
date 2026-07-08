//! evalkit —— eval 测试框架 / 场景模拟器。
//!
//! 目的：把 eval 闭环从「单元测试里手搓几条 span」升级成「**自造多种 agent 场景的合成 trace、
//! 经真实摄入路径灌进引擎、跑完整评测闭环**」，既当端到端验证、也当可跑的演示。
//!
//! 它真做了什么（不是 mock）：
//! 1. **自产测试数据**：4 类内置 agent 场景（客服问答 / 风控研判多 agent / 代码助手 / 数据分析），
//!    每条 trace 拆成 root(编排) + tool(工具调用) + answer(模型作答) 三个 span，带中文 input/output、
//!    token、agent/工具/模型标注、状态/耗时。失败答案里埋「坏词」，给 scorer 留信号。
//! 2. **走真实摄入**：所有数据经 `WriteCoordinator::ingest_wire`（SDK 线格式同一入口）灌进去，
//!    不是直接塞内存表 —— 确定性 event_id、折叠、落盘全都真实经过。
//! 3. **跑完整 eval 闭环**：`eval_and_writeback` 打分走 upgrade 写回 → `eval_summary` 出 per-agent
//!    通过率看板 → `collect_into_dataset` 把答案 span 冻成回归数据集 → `eval_dataset` 用更严 scorer
//!    重跑，演示「评判标准变严 → 通过率下降」的回归检出。
//!
//! 确定性：用 std-only 的 xorshift 伪随机（同 seed 完全可复现），不碰 `rand` / 时钟，契合零依赖骨架。

use std::sync::Arc;

use yt_core::fold::FoldedSpan;

use crate::{
    AgentCost, EvalSummary, KeywordScorer, SessionTimeline, SessionTurn, TraceQuery, WireRecord,
    WriteCoordinator,
};

include!("evalkit/trace_harness.rs");
include!("evalkit/session_harness.rs");
