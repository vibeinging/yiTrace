use std::collections::BTreeMap;

use yt_engine::WireRecord;

const PROJECTS: [&str; 4] = ["scale-a", "scale-b", "scale-c", "scale-d"];
const SKILLS: [&str; 8] = [
    "plan",
    "search",
    "review",
    "execute",
    "test",
    "browser",
    "summarize",
    "recover",
];
const MODES: [&str; 3] = ["auto", "interactive", "eval"];
const AGENTS: [&str; 6] = [
    "planner-agent",
    "coding-agent",
    "research-agent",
    "browser-agent",
    "review-agent",
    "eval-agent",
];
const TOOLS: [&str; 8] = [
    "shell",
    "browser",
    "search",
    "read_file",
    "apply_patch",
    "sql",
    "http",
    "model",
];
const MODELS: [&str; 4] = [
    "gpt-5.3-codex-spark",
    "qwen3",
    "claude-sonnet",
    "local-small-model",
];
const TASKS: [&str; 8] = [
    "risk-review",
    "code-fix",
    "release-check",
    "data-analysis",
    "browser-research",
    "customer-support",
    "prompt-eval",
    "incident-recovery",
];

#[derive(Clone, Copy)]
pub struct GeneratorConfig {
    pub spans: usize,
    pub batch_records: usize,
    pub seed: u64,
    pub tenant: u64,
}

#[derive(Clone, Debug, Default)]
pub struct DatasetStats {
    pub spans: usize,
    pub traces: usize,
    pub sessions: usize,
    pub loops: usize,
    pub wire_events: usize,
    pub log_events: usize,
    pub duplicate_events: usize,
    pub incomplete_spans: usize,
    pub scale_a_spans: usize,
    pub scale_a_traces: usize,
    pub risk_review_traces: usize,
    pub seed: u64,
}

#[derive(Clone)]
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 16
    }

    fn index(&mut self, len: usize) -> usize {
        self.next() as usize % len
    }
}

/// 生成接近 Agent 工作负载的确定性数据，并分批交给调用方。
///
/// `spans` 是折叠后的 span 数，不是 wire event 数。每个 span 至少有 start，绝大多数有
/// end，部分有 log；另外注入少量重复和未完成事件，用来覆盖真实重试与进程中断。
pub fn generate_dataset<F>(config: GeneratorConfig, mut sink: F) -> DatasetStats
where
    F: FnMut(Vec<WireRecord>),
{
    let mut rng = Rng(config.seed);
    let mut stats = DatasetStats {
        seed: config.seed,
        ..DatasetStats::default()
    };
    let mut pending = Vec::with_capacity(config.batch_records.max(64));
    let mut global_span = 0usize;
    let mut trace_id = 0u64;

    while global_span < config.spans {
        trace_id += 1;
        let trace_spans = sample_trace_size(&mut rng).min(config.spans - global_span);
        let project = PROJECTS[(trace_id as usize - 1) % PROJECTS.len()];
        let task = TASKS[(trace_id as usize - 1) % TASKS.len()];
        let session_id = 10_000 + (trace_id - 1) / 4;
        let loop_id = 20_000 + (trace_id - 1) / 8;

        stats.traces += 1;
        if project == "scale-a" {
            stats.scale_a_traces += 1;
        }
        if task == "risk-review" {
            stats.risk_review_traces += 1;
        }

        for local_span in 1..=trace_spans {
            global_span += 1;
            stats.spans += 1;
            if project == "scale-a" {
                stats.scale_a_spans += 1;
            }

            let span_id = local_span as u64;
            let parent_span_id = if span_id == 1 {
                None
            } else {
                Some(span_id / 2)
            };
            let skill = SKILLS[(global_span + rng.index(SKILLS.len())) % SKILLS.len()];
            let mode = MODES[(trace_id as usize + local_span) % MODES.len()];
            let agent = AGENTS[(trace_id as usize + local_span * 3) % AGENTS.len()];
            let tool = TOOLS[(global_span + rng.index(TOOLS.len())) % TOOLS.len()];
            let model = MODELS[(trace_id as usize + local_span) % MODELS.len()];
            let failed = global_span % 13 == 0 || (task == "incident-recovery" && local_span == 2);
            let incomplete = global_span % 1_000 == 0;
            let has_log = global_span % 5 == 0 || failed;
            let status = if failed { 1 } else { 0 };
            let duration_ns = 400_000 + rng.next() % 1_500_000_000;
            let ts = 1_720_000_000_000_000_000i64
                + trace_id as i64 * 10_000_000
                + local_span as i64 * 100_000;
            let ext_span_id = format!("trace-{trace_id}-span-{span_id}");
            let external_parent_span_id =
                parent_span_id.map(|parent| format!("trace-{trace_id}-span-{parent}"));
            let attrs = make_attrs(
                project,
                skill,
                mode,
                task,
                loop_id,
                trace_id,
                span_id,
                if failed { "fail" } else { "pass" },
            );

            pending.push(WireRecord {
                trace_id,
                span_id,
                ts,
                seq: 1,
                event_type_tag: 1,
                ext_span_id: ext_span_id.clone(),
                parent_span_id,
                status: None,
                duration_ns: None,
                input_tokens: Some(80 + rng.next() % 2_400),
                output_tokens: None,
                cache_read_tokens: None,
                cache_write_tokens: None,
                session_id: Some(session_id),
                tenant_id: Some(config.tenant),
                external_trace_id: Some(format!("run-{trace_id}")),
                external_span_id: Some(ext_span_id.clone()),
                external_parent_span_id: external_parent_span_id.clone(),
                external_session_id: Some(format!("session-{session_id}")),
                span_name: None,
                display_name: None,
                agent_name: Some(agent.to_string()),
                tool_name: Some(tool.to_string()),
                model: Some(model.to_string()),
                input_text: Some(make_text(task, tool, global_span, false, failed, &mut rng)),
                output_text: None,
                logs: Vec::new(),
                attrs: attrs.clone(),
            });
            stats.wire_events += 1;

            let mut end_seq = 2;
            if has_log {
                pending.push(WireRecord {
                    trace_id,
                    span_id,
                    ts: ts + duration_ns as i64 / 2,
                    seq: 2,
                    event_type_tag: 4,
                    ext_span_id: ext_span_id.clone(),
                    parent_span_id,
                    status: None,
                    duration_ns: None,
                    input_tokens: None,
                    output_tokens: None,
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    session_id: Some(session_id),
                    tenant_id: Some(config.tenant),
                    external_trace_id: Some(format!("run-{trace_id}")),
                    external_span_id: Some(ext_span_id.clone()),
                    external_parent_span_id: external_parent_span_id.clone(),
                    external_session_id: Some(format!("session-{session_id}")),
                    span_name: None,
                    display_name: None,
                    agent_name: Some(agent.to_string()),
                    tool_name: Some(tool.to_string()),
                    model: Some(model.to_string()),
                    input_text: None,
                    output_text: None,
                    logs: vec![if failed {
                        format!("tool={tool} failed retryable=true span={global_span}")
                    } else {
                        format!("tool={tool} progress=done span={global_span}")
                    }],
                    attrs: BTreeMap::new(),
                });
                stats.log_events += 1;
                stats.wire_events += 1;
                end_seq = 3;
            }

            if incomplete {
                stats.incomplete_spans += 1;
            } else {
                let mut duplicate_rng = rng.clone();
                let end = make_end_record(
                    trace_id,
                    span_id,
                    ts + duration_ns as i64,
                    end_seq,
                    &ext_span_id,
                    parent_span_id,
                    external_parent_span_id.as_deref(),
                    session_id,
                    config.tenant,
                    agent,
                    tool,
                    model,
                    status,
                    duration_ns,
                    task,
                    global_span,
                    &mut rng,
                );
                pending.push(end);
                stats.wire_events += 1;

                // 精确重复同一 identity，模拟 SDK retry。折叠结果仍只能有一个事件。
                if global_span % 100 == 0 {
                    pending.push(make_end_record(
                        trace_id,
                        span_id,
                        ts + duration_ns as i64,
                        end_seq,
                        &ext_span_id,
                        parent_span_id,
                        external_parent_span_id.as_deref(),
                        session_id,
                        config.tenant,
                        agent,
                        tool,
                        model,
                        status,
                        duration_ns,
                        task,
                        global_span,
                        &mut duplicate_rng,
                    ));
                    stats.duplicate_events += 1;
                    stats.wire_events += 1;
                }
            }

            if pending.len() >= config.batch_records.max(1) {
                sink(std::mem::take(&mut pending));
                pending = Vec::with_capacity(config.batch_records.max(64));
            }
        }
    }

    if !pending.is_empty() {
        sink(pending);
    }
    stats.sessions = stats.traces.div_ceil(4);
    stats.loops = stats.traces.div_ceil(8);
    stats
}

fn sample_trace_size(rng: &mut Rng) -> usize {
    match rng.next() % 100 {
        0..=49 => 1 + rng.index(4),
        50..=84 => 5 + rng.index(11),
        85..=96 => 16 + rng.index(25),
        _ => 41 + rng.index(60),
    }
}

fn make_attrs(
    project: &str,
    skill: &str,
    mode: &str,
    task: &str,
    loop_id: u64,
    trace_id: u64,
    span_id: u64,
    validation: &str,
) -> BTreeMap<String, String> {
    let mut attrs = BTreeMap::new();
    attrs.insert("project_id".to_string(), json_str(project));
    attrs.insert("skill".to_string(), json_str(skill));
    attrs.insert("mode".to_string(), json_str(mode));
    attrs.insert("task_fingerprint".to_string(), json_str(task));
    attrs.insert("loop_id".to_string(), json_str(&format!("loop-{loop_id}")));
    attrs.insert("validation_status".to_string(), json_str(validation));
    attrs.insert(
        "call_site".to_string(),
        json_str(&format!(
            "src/agents/worker_{}.rs:{}",
            trace_id % 257,
            10 + span_id
        )),
    );
    attrs.insert(
        "external_run_id".to_string(),
        json_str(&format!("external-run-{trace_id}")),
    );
    attrs
}

#[allow(clippy::too_many_arguments)]
fn make_end_record(
    trace_id: u64,
    span_id: u64,
    ts: i64,
    seq: u64,
    ext_span_id: &str,
    parent_span_id: Option<u64>,
    external_parent_span_id: Option<&str>,
    session_id: u64,
    tenant: u64,
    agent: &str,
    tool: &str,
    model: &str,
    status: u8,
    duration_ns: u64,
    task: &str,
    global_span: usize,
    rng: &mut Rng,
) -> WireRecord {
    WireRecord {
        trace_id,
        span_id,
        ts,
        seq,
        event_type_tag: 2,
        ext_span_id: ext_span_id.to_string(),
        parent_span_id,
        status: Some(status),
        duration_ns: Some(duration_ns),
        input_tokens: None,
        output_tokens: Some(20 + rng.next() % 900),
        cache_read_tokens: None,
        cache_write_tokens: None,
        session_id: Some(session_id),
        tenant_id: Some(tenant),
        external_trace_id: Some(format!("run-{trace_id}")),
        external_span_id: Some(ext_span_id.to_string()),
        external_parent_span_id: external_parent_span_id.map(str::to_string),
        external_session_id: Some(format!("session-{session_id}")),
        span_name: None,
        display_name: None,
        agent_name: Some(agent.to_string()),
        tool_name: Some(tool.to_string()),
        model: Some(model.to_string()),
        input_text: None,
        output_text: Some(make_text(task, tool, global_span, true, status != 0, rng)),
        logs: Vec::new(),
        attrs: BTreeMap::new(),
    }
}

fn make_text(
    task: &str,
    tool: &str,
    span: usize,
    output: bool,
    failed: bool,
    rng: &mut Rng,
) -> String {
    let (prompt, success, failure) = match task {
        "risk-review" => (
            "检查交易链路，判断是否存在疑似盗刷并给出人工复核依据",
            "支付风控检查完成，风险证据已按交易和设备归类",
            "支付风控工具超时，本轮没有拿到完整交易证据",
        ),
        "code-fix" => (
            "定位回归失败原因，读取相关代码并运行最小测试集",
            "代码修复完成，相关单元测试和边界用例通过",
            "回归测试失败，调用栈指向状态清理顺序错误",
        ),
        "release-check" => (
            "检查发布产物、平台架构、版本号和干净消费者安装",
            "发布检查完成，ESM CJS native binding 均可加载",
            "安装验证失败，目标平台缺少 native binding",
        ),
        "data-analysis" => (
            "查询业务数据并解释指标变化，不泄露原始敏感字段",
            "数据分析完成，异常来自样本结构变化而不是流量下降",
            "SQL 执行失败，数据源连接在查询中途断开",
        ),
        "browser-research" => (
            "检索官方资料和真实开源实现，记录可复核的来源",
            "调研完成，结论已由官方文档和仓库代码交叉验证",
            "网页访问失败，当前证据不足以支持结论",
        ),
        "customer-support" => (
            "根据用户问题查找相似历史处理路径并生成答复",
            "已找到相似问题，答复引用了成功处理步骤",
            "没有找到可信历史案例，需要转人工处理",
        ),
        "prompt-eval" => (
            "运行固定评测集，对比候选提示词的准确率和成本",
            "评测完成，候选版本准确率提高且 token 成本下降",
            "评测失败，输出格式不符合评分器契约",
        ),
        _ => (
            "恢复中断任务，确认上一步副作用后再继续执行",
            "任务恢复完成，重复操作已通过事件标识去重",
            "恢复失败，快照版本和写前日志水位不一致",
        ),
    };

    let base = if output {
        if failed {
            failure
        } else {
            success
        }
    } else {
        prompt
    };
    let mut text =
        format!("{base}。tool={tool} span={span}。任务执行需要保留输入、输出和状态证据。 ");

    // 形成短文本为主、长文本为长尾的尺寸分布，不依赖外部模型或网络。
    let repeats = match rng.next() % 1_000 {
        0..=849 => 1 + rng.index(3),
        850..=989 => 8 + rng.index(12),
        _ => 40 + rng.index(40),
    };
    for _ in 0..repeats {
        text.push_str("上下文包含工具参数、执行结果、校验状态和下一步决策。 ");
    }
    if span % 997 == 0 {
        text.push_str("月蚀校验码用于测试低频关键词检索。 ");
    }
    text
}

fn json_str(value: &str) -> String {
    format!("{value:?}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    #[test]
    fn generator_hits_exact_span_target_and_event_mix() {
        let mut rows = Vec::new();
        let stats = generate_dataset(
            GeneratorConfig {
                spans: 2_500,
                batch_records: 37,
                seed: 7,
                tenant: 42,
            },
            |batch| rows.extend(batch),
        );

        let span_keys = rows
            .iter()
            .map(|row| (row.trace_id, row.span_id))
            .collect::<BTreeSet<_>>();
        assert_eq!(stats.spans, 2_500);
        assert_eq!(span_keys.len(), 2_500);
        assert_eq!(stats.incomplete_spans, 2);
        assert_eq!(stats.duplicate_events, 23);
        assert_eq!(stats.wire_events, rows.len());
        assert!(stats.traces < stats.spans);
        assert!(rows.iter().any(|row| row.event_type_tag == 1));
        assert!(rows.iter().any(|row| row.event_type_tag == 2));
        assert!(rows.iter().any(|row| row.event_type_tag == 4));
    }

    #[test]
    fn generator_is_deterministic_for_same_seed() {
        fn fingerprint(seed: u64) -> Vec<(u64, u64, u64, u8, Option<String>)> {
            let mut out = Vec::new();
            generate_dataset(
                GeneratorConfig {
                    spans: 80,
                    batch_records: 17,
                    seed,
                    tenant: 42,
                },
                |batch| {
                    out.extend(batch.into_iter().map(|row| {
                        (
                            row.trace_id,
                            row.span_id,
                            row.seq,
                            row.event_type_tag,
                            row.input_text,
                        )
                    }))
                },
            );
            out
        }

        assert_eq!(fingerprint(99), fingerprint(99));
        assert_ne!(fingerprint(99), fingerprint(100));
    }

    #[test]
    fn duplicate_events_keep_the_same_identity() {
        let mut identities: BTreeMap<(String, u64, u8), usize> = BTreeMap::new();
        generate_dataset(
            GeneratorConfig {
                spans: 300,
                batch_records: 100,
                seed: 3,
                tenant: 42,
            },
            |batch| {
                for row in batch {
                    *identities
                        .entry((row.ext_span_id, row.seq, row.event_type_tag))
                        .or_default() += 1;
                }
            },
        );
        assert_eq!(identities.values().filter(|count| **count == 2).count(), 3);
        assert!(identities.values().all(|count| *count <= 2));
    }
}
