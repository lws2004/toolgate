//! task 的智能路径（§5.5 / §4.8 / §3.8 / §7.3）。
//!
//! 纯逻辑：状态判断链 DAG、intent 协商与两档阈值。
//! 不含模型后端（可选层，§19.6）：decide / summarize 槽位当前不可用，
//! 按 §3.8 退到机械路径并按 §7.3 显式告知。卡住 / 崩溃 / 报错循环
//! 这类需要 d1 的判定不编造，结构化事实里给 unknown（§4.6 诚实）。

use serde_json::{Value, json};

use crate::bash_path::AUTO_SUMMARY_TOKENS;
use crate::compat::{CompatConfig, resolve_intent};
use crate::decision::Intent;
use crate::notice::{Notice, SlotFallback};
use crate::slots::DecideInput;
use crate::understanding::{ChainAction, ChainNode, decide_request};

/// §5.5 L2：仅当 action=read 且累积输出 > 2000 token 才做增量摘要。
pub const TASK_SUMMARY_TOKENS: usize = 2000;

/// §4.8 / §7.1：输出小于 500 token 不进理解链。复用 bash 的同一口径，
/// 避免两个工具各写一份 500。
pub const AUTO_CHAIN_TOKENS: usize = AUTO_SUMMARY_TOKENS;

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

/// §5.5 L1 的两个命名问题，一次 d1 调用问完（§13.4 方案 A）。
/// 只定义形状，不发起模型调用（模型后端属可选层）。
pub fn state_questions() -> Vec<String> {
    vec!["state?".to_owned(), "progress?".to_owned()]
}

/// 把机械观测映射成 d1 的 state + questions 输入形状（§13.4 方案 A）。
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
    /// 进链但只缺 decide：输出 500–2000 token，不做摘要（§5.5）。
    StateDegraded,
    /// 缺 decide + summarize：输出 > 2000 token 或显式 summary。
    SummaryDegraded,
    /// 结构化事实 JSON（§4.7）。
    Structured,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TaskPlan {
    pub mode: TaskMode,
    /// 需同时进 content 文本与 structuredContent 的告知（§4.4b 两个通道）。
    pub notices: Vec<Notice>,
}

fn slot_unavailable(slot: &str) -> Notice {
    Notice::SlotUnavailable {
        slot: slot.to_owned(),
        fallback: SlotFallback::Mechanical,
    }
}

/// 协商层：按 intent 与输出体量决定返回形态。纯函数，同输入同输出。
/// 槽位全空时按 §3.8 退机械路径并按 §7.3 显式告知；真实 d1 判定随可选层接入。
pub fn plan_task(intent: Intent, tokens: usize) -> Result<TaskPlan, String> {
    let plan = match intent {
        Intent::Raw => TaskPlan {
            mode: TaskMode::Raw,
            notices: vec![],
        },
        Intent::Visual => {
            return Err(
                "task 不返回图片，intent=visual 不适用；要看图请用 read 工具，或改用 intent=raw"
                    .to_owned(),
            );
        }
        Intent::Structured => TaskPlan {
            mode: TaskMode::Structured,
            notices: vec![],
        },
        // §5.1/§9.2：summary 与 auto 都只在输出 ≥ 500 token 时进理解链；
        // 低于门槛给机械结果（原文），不发 decide / summarize 降级告知。
        Intent::Summary | Intent::Auto if !needs_chain(tokens) => TaskPlan {
            mode: TaskMode::Raw,
            notices: vec![],
        },
        Intent::Auto if !needs_summary(tokens) => TaskPlan {
            mode: TaskMode::StateDegraded,
            notices: vec![slot_unavailable("decide")],
        },
        Intent::Summary | Intent::Auto => TaskPlan {
            mode: TaskMode::SummaryDegraded,
            notices: vec![slot_unavailable("decide"), slot_unavailable("summarize")],
        },
    };
    Ok(plan)
}

/// §5.5 structured 路径的事实 JSON。
/// decide / summarize 槽位不可用时按 §3.8「不判错、不判类型」给 unknown / null，
/// 只给机械可观测的进程事实（§4.6 诚实）。
pub fn structured_facts(
    exit_code: Option<i32>,
    elapsed_secs: u64,
    output_bytes: usize,
    truncated: bool,
    spill_path: Option<&str>,
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
        "state": "unknown",
        "progress": "unknown",
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
        let req = state_decide_request(None, 12, "line1\nline2");
        assert_eq!(req.questions, state_questions());
        assert_eq!(req.questions.len(), 2);
        assert_eq!(req.state["elapsedSecs"], json!(12));
        assert_eq!(req.state["tail"], json!("line1\nline2"));
        assert!(req.images.is_none());
    }

    #[test]
    fn raw_path_has_no_notices() {
        let plan = plan_task(Intent::Raw, 10_000).unwrap();
        assert_eq!(plan.mode, TaskMode::Raw);
        assert!(plan.notices.is_empty());
    }

    #[test]
    fn summary_degrades_with_two_slot_notices() {
        // 到达门槛才进理解链；低门槛由 summary_below_threshold_stays_raw 覆盖。
        let plan = plan_task(Intent::Summary, AUTO_CHAIN_TOKENS).unwrap();
        assert_eq!(plan.mode, TaskMode::SummaryDegraded);
        assert_eq!(plan.notices.len(), 2);
        let rendered = render_notices(&plan.notices);
        assert!(rendered.contains("slot=decide"));
        assert!(rendered.contains("slot=summarize"));
        assert!(rendered.contains("fallback=mechanical"));
        assert_eq!(rendered.lines().count(), 2);
    }

    /// §5.1/§9.2：summary 低于 500 token 给机械结果，不进理解链、不发降级告知。
    #[test]
    fn summary_below_threshold_stays_raw() {
        let plan = plan_task(Intent::Summary, AUTO_CHAIN_TOKENS - 1).unwrap();
        assert_eq!(plan.mode, TaskMode::Raw);
        assert!(plan.notices.is_empty());
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

    #[test]
    fn mid_output_only_reports_decide_slot() {
        let plan = plan_task(Intent::Auto, 1000).unwrap();
        assert_eq!(plan.mode, TaskMode::StateDegraded);
        assert_eq!(plan.notices.len(), 1);
        let rendered = render_notices(&plan.notices);
        assert!(rendered.contains("slot=decide"));
        assert!(!rendered.contains("slot=summarize"));
    }

    #[test]
    fn structured_mode_has_no_notices() {
        let plan = plan_task(Intent::Structured, 10).unwrap();
        assert_eq!(plan.mode, TaskMode::Structured);
        assert!(plan.notices.is_empty());
    }

    #[test]
    fn visual_is_rejected_actionably() {
        let err = plan_task(Intent::Visual, 10).unwrap_err();
        assert!(err.contains("read"));
        assert!(err.contains("raw"));
    }

    #[test]
    fn structured_facts_never_fabricate_a_judgement() {
        let facts = structured_facts(None, 3, 9000, true, Some("spill://ab12cd34"));
        assert_eq!(facts["processState"], json!("running"));
        assert_eq!(facts["state"], json!("unknown"));
        assert_eq!(facts["progress"], json!("unknown"));
        assert!(facts["summary"].is_null());
        assert!(facts["exitCode"].is_null());
        assert_eq!(facts["spillPath"], json!("spill://ab12cd34"));
        let exited = structured_facts(Some(2), 9, 10, false, None);
        assert_eq!(exited["processState"], json!("exited"));
        assert_eq!(exited["exitCode"], json!(2));
        assert!(exited["spillPath"].is_null());
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
