//! task 的智能路径（§5.5 / §4.8 / §3.8 / §7.3）。
//!
//! 纯逻辑：状态判断链 DAG、intent 协商、两档阈值与 L1 判定的二元分解 / 归并。
//! 不发起模型调用（HTTP 发送在 slm/client.rs）；判定不可用时按 §3.8 退到机械
//! 路径并按 §7.3 显式告知。卡住 / 崩溃 / 报错循环这类状态不编造，给 unknown。

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::bash_path::AUTO_SUMMARY_TOKENS;
use crate::compat::{CompatConfig, resolve_intent};
use crate::decision::Intent;
use crate::notice::Notice;
use crate::slots::DecideInput;
use crate::understanding::{ChainAction, ChainNode, decide_request};

/// §5.5 L2：仅当 action=read 且累积输出 > 2000 token 才做增量摘要。
pub const TASK_SUMMARY_TOKENS: usize = 2000;

/// §4.8 / §7.1：输出小于 500 token 不进理解链。复用 bash 的同一口径，
/// 避免两个工具各写一份 500。
pub const AUTO_CHAIN_TOKENS: usize = AUTO_SUMMARY_TOKENS;

/// §5.5：判定 state 取累积输出尾部 8000 字符（§6.4 prefill 预算，与 §5.1 同源）。
pub const TASK_STATE_CHARS: usize = 8000;

/// intent 缺省 raw（§4.2 兼容性约束），复用兼容层的同一套解析规则。
pub fn resolve_task_intent(args: &Value) -> Result<Intent, String> {
    resolve_intent(args, &CompatConfig::default())
}

/// §5.5 的 5 步主链：召回尾部输出与增量（可并发）→ decide(state?/progress?)
/// → summarize(增量摘要) → merge。链级上限 5 步、并发 4（§4.8）。
/// summarize 节点只在输出超过 TASK_SUMMARY_TOKENS 时执行，否则按 §4.8
/// 收束到机械路径（给已完成步骤 + 机械结果），不中途扩展预算。
pub fn task_chain() -> Vec<ChainNode> {
    vec![
        ChainNode {
            id: 0,
            action: ChainAction::Recall,
            depends_on: vec![],
        },
        ChainNode {
            id: 1,
            action: ChainAction::Recall,
            depends_on: vec![],
        },
        ChainNode {
            id: 2,
            action: ChainAction::Decide,
            depends_on: vec![0, 1],
        },
        ChainNode {
            id: 3,
            action: ChainAction::Summarize,
            depends_on: vec![2],
        },
        ChainNode {
            id: 4,
            action: ChainAction::Merge,
            depends_on: vec![3],
        },
    ]
}

// ---------- §5.5 L1 判定（decide 槽位） ----------

/// §5.5 的 state / progress 是多值语义，而 decide 槽位契约（§3.2）只有二元形状；
/// 因此分解成七个二元问题，一次请求问完（§13.4 方案 A）。顺序即判定顺序。
pub const TASK_L1_QUESTIONS: [&str; 7] = [
    "进程当前仍在正常运行（正常等待也算）吗？",
    "进程已经正常完成并退出了吗？",
    "进程输出长时间不再变化、是否卡住了？",
    "这段输出显示进程已经崩溃或异常退出了吗？",
    "累积输出里能看到新的进展吗？",
    "累积输出是否长时间没有变化？",
    "是否在重复同样的报错（报错循环）？",
];

/// 七个问题的稳定 key，与 TASK_L1_QUESTIONS 一一对应；归并只按 key 取值。
pub const TASK_L1_QUESTION_KEYS: [&str; 7] = [
    "state_running",
    "state_completed",
    "state_stuck",
    "state_crashed",
    "progress_made",
    "progress_no_change",
    "progress_error_loop",
];

/// state 的四个候选值（对应前四个 key）。
pub const TASK_STATES: [&str; 4] = ["running", "completed", "stuck", "crashed"];

/// progress 的三个候选值（对应后三个 key）。
pub const TASK_PROGRESSES: [&str; 3] = ["made", "no_change", "error_loop"];

/// §5.5 L1 的七个命名问题，一次 d1 调用问完（§13.4 方案 A）。
pub fn state_questions() -> Vec<String> {
    TASK_L1_QUESTIONS.iter().map(|q| (*q).to_owned()).collect()
}

/// 判定 state 的尾部窗口（§6.4 有界候选）。
pub fn decision_state_tail(body: &str) -> String {
    body.chars()
        .rev()
        .take(TASK_STATE_CHARS)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

/// 把机械观测 + 尾部窗口映射成 d1 的 state + questions 输入形状（§13.4 方案 A）。
pub fn state_decide_request(exit_code: Option<i32>, elapsed_secs: u64, tail: &str) -> DecideInput {
    decide_request(
        json!({
            "exitCode": exit_code,
            "elapsedSecs": elapsed_secs,
            "tail": tail,
        }),
        state_questions(),
        None,
    )
}

/// §5.5 的 L1 判定结论。
#[derive(Debug, Clone, PartialEq)]
pub struct TaskDecision {
    /// 输出语义层面的状态：running / completed / stuck / crashed / unknown。
    pub state: String,
    /// 进展：made / no_change / error_loop / unknown。
    pub progress: String,
    /// 本次结论的置信度（§7.2 门控输入）。
    pub confidence: f64,
}

/// 一组候选里取概率最大者；最大者 < 0.5 → unknown（归并阈值，非 §7.2 门控阈值）。
fn argmax(probs: &BTreeMap<String, f64>, keys: &[&str], values: &[&str]) -> (String, f64) {
    let p = |key: &str| probs.get(key).copied().unwrap_or(0.0);
    let mut best_index = 0usize;
    let mut best = p(keys[0]);
    for (i, key) in keys.iter().enumerate().skip(1) {
        let candidate = p(key);
        if candidate > best {
            best = candidate;
            best_index = i;
        }
    }
    if best < 0.5 {
        ("unknown".to_owned(), best)
    } else {
        (values[best_index].to_owned(), best)
    }
}

/// 归并 L1 判定（纯函数）：七个二元概率 → state / progress / confidence。
/// 规则见 §5.5「L1 判定的落地口径」。
pub fn merge_task_decision(probs: &BTreeMap<String, f64>) -> TaskDecision {
    let (state, state_probability) = argmax(probs, &TASK_L1_QUESTION_KEYS[..4], &TASK_STATES);
    let (progress, progress_probability) =
        argmax(probs, &TASK_L1_QUESTION_KEYS[4..], &TASK_PROGRESSES);
    TaskDecision {
        state,
        progress,
        confidence: state_probability.min(progress_probability),
    }
}

/// §5.5 structured 路径的判定事实；判定不可用时留空（§3.8 不编造）。
#[derive(Debug, Clone, Default)]
pub struct TaskDecisionFacts {
    pub state: Option<String>,
    pub progress: Option<String>,
    pub confidence: Option<f64>,
}

/// 是否进理解链（§4.8 / §7.1）。
pub fn needs_chain(tokens: usize) -> bool {
    tokens >= AUTO_CHAIN_TOKENS
}

/// 是否执行 summarize 节点（§5.5 L2）。
pub fn needs_summary(tokens: usize) -> bool {
    tokens > TASK_SUMMARY_TOKENS
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskMode {
    /// 截断原文 + 落盘指针（v1 行为）。
    Raw,
    /// 状态判定路径：输出 500–2000 token，不做摘要（§5.5）。
    StateDegraded,
    /// 状态判定 + 摘要路径：输出 > 2000 token 或显式 summary；摘要不可用时按 §3.8 降级告知。
    SummaryDegraded,
    /// 结构化事实 JSON（§4.7）。
    Structured,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TaskPlan {
    pub mode: TaskMode,
    /// 需同时进 content 文本与 structuredContent 的告知（§4.4b 两个通道）。
    pub notices: Vec<Notice>,
    /// §5.1/§9.2：输出 ≥ 500 token 才触发 L1 判定；由协商层单点产出。
    pub should_decide: bool,
    /// §5.5：仅摘要路径尝试 L2 增量摘要；告知由运行时按实际结果追加。
    pub should_summarize: bool,
}

/// 协商层：按 intent 与输出体量决定返回形态。纯函数，同输入同输出。
/// decide / summarize 的告知都由 task.rs 运行时按实际结果追加（与 bash 对称），
/// 所以这里不写死降级告知。
pub fn plan_task(intent: Intent, tokens: usize) -> Result<TaskPlan, String> {
    let should_decide = matches!(intent, Intent::Summary | Intent::Structured | Intent::Auto)
        && tokens >= AUTO_CHAIN_TOKENS;
    let (mode, notices) = match intent {
        Intent::Raw => (TaskMode::Raw, vec![]),
        Intent::Visual => {
            return Err(
                "task 不返回图片，intent=visual 不适用；要看图请用 read 工具，或改用 intent=raw"
                    .to_owned(),
            );
        }
        Intent::Structured => (TaskMode::Structured, vec![]),
        // §5.1/§9.2：summary 与 auto 都只在输出 ≥ 500 token 时进理解链；
        // 低于门槛给机械结果（原文），不发 decide / summarize 降级告知。
        Intent::Summary | Intent::Auto if !needs_chain(tokens) => (TaskMode::Raw, vec![]),
        Intent::Auto if !needs_summary(tokens) => (TaskMode::StateDegraded, vec![]),
        Intent::Summary | Intent::Auto => (TaskMode::SummaryDegraded, vec![]),
    };
    // §5.5：L2 增量摘要只在摘要路径尝试；auto 的 > 2000 token 条件由 needs_summary
    // 分支体现，显式 summary 是调用方明确要加工，不受该自动门槛限制。
    let should_summarize = mode == TaskMode::SummaryDegraded;
    Ok(TaskPlan {
        mode,
        notices,
        should_decide,
        should_summarize,
    })
}

/// §5.5 structured 路径的事实 JSON。
/// 判定不可用时按 §3.8「不判错、不判类型」给 unknown / null，只给机械可观测的
/// 进程事实（§4.6 诚实）。
pub fn structured_facts(
    exit_code: Option<i32>,
    elapsed_secs: u64,
    output_bytes: usize,
    truncated: bool,
    spill_path: Option<&str>,
    decision: &TaskDecisionFacts,
) -> Value {
    let process_state = if exit_code.is_some() {
        "exited"
    } else {
        "running"
    };
    json!({
        "processState": process_state,
        "exitCode": exit_code,
        "elapsedSecs": elapsed_secs,
        "outputBytes": output_bytes,
        "truncated": truncated,
        "state": decision.state.clone().unwrap_or_else(|| "unknown".to_owned()),
        "progress": decision.progress.clone().unwrap_or_else(|| "unknown".to_owned()),
        "confidence": decision.confidence,
        "summary": Value::Null,
        "spillPath": spill_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bash_path::estimate_tokens;
    use crate::notice::render_notices;
    use crate::understanding::{MAX_PARALLEL, MAX_STEPS, schedule, validate_chain};

    fn probs(values: [f64; 7]) -> BTreeMap<String, f64> {
        TASK_L1_QUESTION_KEYS
            .iter()
            .zip(values)
            .map(|(key, value)| ((*key).to_owned(), value))
            .collect()
    }

    #[test]
    fn chain_is_valid_bounded_and_deterministic() {
        let chain = task_chain();
        assert_eq!(chain.len(), MAX_STEPS);
        validate_chain(&chain, MAX_PARALLEL).unwrap();
        let a = schedule(&chain, MAX_PARALLEL);
        let b = schedule(&chain, MAX_PARALLEL);
        assert_eq!(a, b);
        assert_eq!(a[0], vec![0, 1]);
        assert_eq!(a[1], vec![2]);
        assert_eq!(a[2], vec![3]);
        assert_eq!(a[3], vec![4]);
        assert!(a.iter().all(|layer| layer.len() <= MAX_PARALLEL));
    }

    #[test]
    fn state_questions_map_to_one_d1_request() {
        let req = state_decide_request(
            None,
            12,
            "line1
line2",
        );
        assert_eq!(req.questions, state_questions());
        assert_eq!(req.questions.len(), 7);
        assert_eq!(req.state["elapsedSecs"], json!(12));
        assert_eq!(
            req.state["tail"],
            json!(
                "line1
line2"
            )
        );
        assert!(req.images.is_none());
    }

    /// §5.5：七个问题覆盖 state 四值 + progress 三值，key 与问题一一对应。
    #[test]
    fn task_l1_question_set_covers_spec() {
        assert_eq!(TASK_L1_QUESTIONS.len(), 7);
        assert_eq!(TASK_L1_QUESTION_KEYS.len(), 7);
        assert_eq!(TASK_L1_QUESTION_KEYS[0], "state_running");
        assert_eq!(TASK_L1_QUESTION_KEYS[4], "progress_made");
        assert_eq!(TASK_STATES.len(), 4);
        assert_eq!(TASK_PROGRESSES.len(), 3);
    }

    /// §5.5 归并规则：argmax / 0.5 归并阈值 / confidence 取两组被选中概率的最小值。
    #[test]
    fn merge_task_decision_follows_spec() {
        let d = merge_task_decision(&probs([0.9, 0.1, 0.1, 0.1, 0.1, 0.1, 0.8]));
        assert_eq!(d.state, "running");
        assert_eq!(d.progress, "error_loop");
        assert!((d.confidence - 0.8).abs() < 1e-9);

        // 两组最大者都 < 0.5 → 双 unknown，confidence 取较小的最大概率。
        let d = merge_task_decision(&probs([0.4, 0.1, 0.1, 0.1, 0.3, 0.1, 0.1]));
        assert_eq!(d.state, "unknown");
        assert_eq!(d.progress, "unknown");
        assert!((d.confidence - 0.3).abs() < 1e-9);

        // 缺 key 按 0 处理，不编造结论。
        let d = merge_task_decision(&BTreeMap::new());
        assert_eq!(d.state, "unknown");
        assert_eq!(d.progress, "unknown");
        assert_eq!(d.confidence, 0.0);
    }

    /// §6.4：state 是有界尾部窗口，不把整份输出喂给模型。
    #[test]
    fn decision_state_tail_is_bounded() {
        let body = "头部".to_owned() + &"x".repeat(TASK_STATE_CHARS + 100) + "尾部标记";
        let tail = decision_state_tail(&body);
        assert_eq!(tail.chars().count(), TASK_STATE_CHARS);
        assert!(tail.ends_with("尾部标记"));
        assert!(!tail.contains("头部"));
    }

    #[test]
    fn raw_path_has_no_notices() {
        let plan = plan_task(Intent::Raw, 10_000).unwrap();
        assert_eq!(plan.mode, TaskMode::Raw);
        assert!(plan.notices.is_empty());
        assert!(!plan.should_decide);
    }

    /// 到达门槛才进理解链；decide / summarize 的告知都由运行时按实际结果追加。
    #[test]
    fn summary_path_requests_both_slots_without_plan_notices() {
        let plan = plan_task(Intent::Summary, AUTO_CHAIN_TOKENS).unwrap();
        assert_eq!(plan.mode, TaskMode::SummaryDegraded);
        assert!(plan.should_decide);
        assert!(plan.should_summarize);
        assert!(plan.notices.is_empty());
        assert!(render_notices(&plan.notices).is_empty());
    }

    /// §5.1/§9.2：summary 低于 500 token 给机械结果，不进理解链、不发降级告知。
    #[test]
    fn summary_below_threshold_stays_raw() {
        let plan = plan_task(Intent::Summary, AUTO_CHAIN_TOKENS - 1).unwrap();
        assert_eq!(plan.mode, TaskMode::Raw);
        assert!(plan.notices.is_empty());
        assert!(!plan.should_decide);
        assert!(!plan.should_summarize);
    }

    /// §5.5：L2 只在摘要路径尝试；auto 的 > 2000 token 条件生效，显式 summary 不受该门槛。
    #[test]
    fn summarize_only_on_summary_path() {
        assert!(
            !plan_task(Intent::Auto, AUTO_CHAIN_TOKENS)
                .unwrap()
                .should_summarize
        );
        assert!(
            !plan_task(Intent::Auto, TASK_SUMMARY_TOKENS)
                .unwrap()
                .should_summarize
        );
        assert!(
            plan_task(Intent::Auto, TASK_SUMMARY_TOKENS + 1)
                .unwrap()
                .should_summarize
        );
        assert!(
            plan_task(Intent::Summary, AUTO_CHAIN_TOKENS)
                .unwrap()
                .should_summarize
        );
        assert!(!plan_task(Intent::Raw, 10_000).unwrap().should_summarize);
        assert!(
            !plan_task(Intent::Structured, 10_000)
                .unwrap()
                .should_summarize
        );
    }

    #[test]
    fn auto_thresholds_split_into_three_paths() {
        assert_eq!(
            plan_task(Intent::Auto, AUTO_CHAIN_TOKENS - 1).unwrap().mode,
            TaskMode::Raw
        );
        assert_eq!(
            plan_task(Intent::Auto, AUTO_CHAIN_TOKENS).unwrap().mode,
            TaskMode::StateDegraded
        );
        assert_eq!(
            plan_task(Intent::Auto, TASK_SUMMARY_TOKENS).unwrap().mode,
            TaskMode::StateDegraded
        );
        assert_eq!(
            plan_task(Intent::Auto, TASK_SUMMARY_TOKENS + 1)
                .unwrap()
                .mode,
            TaskMode::SummaryDegraded
        );
    }

    /// decide 的告知由运行时追加，纯计划里不写死。
    #[test]
    fn mid_output_has_no_plan_notices() {
        let plan = plan_task(Intent::Auto, 1000).unwrap();
        assert_eq!(plan.mode, TaskMode::StateDegraded);
        assert!(plan.notices.is_empty());
        assert!(plan.should_decide);
        assert!(!plan.should_summarize);
    }

    /// §5.1/§9.2：三种 intent 都到 500 token 门槛才触发判定；raw 永不判定。
    #[test]
    fn decision_triggered_at_threshold_for_all_intents() {
        for intent in [Intent::Summary, Intent::Structured, Intent::Auto] {
            assert!(
                !plan_task(intent, AUTO_CHAIN_TOKENS - 1)
                    .unwrap()
                    .should_decide
            );
            assert!(plan_task(intent, AUTO_CHAIN_TOKENS).unwrap().should_decide);
        }
        assert!(!plan_task(Intent::Raw, 10_000).unwrap().should_decide);
    }

    #[test]
    fn structured_mode_has_no_notices() {
        let plan = plan_task(Intent::Structured, 10).unwrap();
        assert_eq!(plan.mode, TaskMode::Structured);
        assert!(plan.notices.is_empty());
        assert!(!plan.should_decide);
        assert!(!plan.should_summarize);
    }

    #[test]
    fn visual_is_rejected_actionably() {
        let err = plan_task(Intent::Visual, 10).unwrap_err();
        assert!(err.contains("read"));
        assert!(err.contains("raw"));
    }

    /// 判定不可用：unknown / null，不编造状态（§3.8 / §4.6）。
    #[test]
    fn structured_facts_never_fabricate_a_judgement() {
        let empty = TaskDecisionFacts::default();
        let facts = structured_facts(None, 3, 9000, true, Some("spill://ab12cd34"), &empty);
        assert_eq!(facts["processState"], json!("running"));
        assert_eq!(facts["state"], json!("unknown"));
        assert_eq!(facts["progress"], json!("unknown"));
        assert!(facts["confidence"].is_null());
        assert!(facts["summary"].is_null());
        assert!(facts["exitCode"].is_null());
        assert_eq!(facts["spillPath"], json!("spill://ab12cd34"));
        let exited = structured_facts(Some(2), 9, 10, false, None, &empty);
        assert_eq!(exited["processState"], json!("exited"));
        assert_eq!(exited["exitCode"], json!(2));
        assert!(exited["spillPath"].is_null());
    }

    /// 判定可用：填真实值，不写占位。
    #[test]
    fn structured_facts_carry_real_decision() {
        let decided = TaskDecisionFacts {
            state: Some("stuck".to_owned()),
            progress: Some("error_loop".to_owned()),
            confidence: Some(0.77),
        };
        let facts = structured_facts(None, 12, 9000, false, None, &decided);
        assert_eq!(facts["state"], json!("stuck"));
        assert_eq!(facts["progress"], json!("error_loop"));
        assert_eq!(facts["confidence"], json!(0.77));
    }

    #[test]
    fn intent_parsing_defaults_to_raw() {
        assert_eq!(resolve_task_intent(&json!({})).unwrap(), Intent::Raw);
        assert_eq!(
            resolve_task_intent(&json!({ "intent": "auto" })).unwrap(),
            Intent::Auto
        );
        assert!(resolve_task_intent(&json!({ "intent": "bogus" })).is_err());
    }

    #[test]
    fn token_estimate_uses_four_bytes() {
        assert_eq!(
            estimate_tokens(&"x".repeat(TASK_SUMMARY_TOKENS * 4)),
            TASK_SUMMARY_TOKENS
        );
    }
}
