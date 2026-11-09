//! bash 的智能路径（§5.1 / §4.8 / §3.8 / §7.3）。
//!
//! 纯逻辑：intent 分流、理解链 DAG 定义、槽位全空时的降级告知。
//! 不含模型后端（可选层，§19.6）：decide / summarize 槽位当前不可用，
//! summary 与 auto 的大输出路径按 §3.8 退到机械路径，并按 §7.3 显式告知。

use serde_json::{Value, json};

use crate::compat::{CompatConfig, resolve_intent};
use crate::decision::Intent;
use crate::notice::{Notice, SlotFallback};
use crate::understanding::{ChainAction, ChainNode};

/// 估算 token：按 §4.4b 的 4 字节/token 口径。
pub fn estimate_tokens(text: &str) -> usize {
    text.len() / 4
}

/// auto 走 summary 的阈值（§5.1 / §7.1）。
pub const AUTO_SUMMARY_TOKENS: usize = 500;

/// intent 缺省 raw（§4.2 兼容性约束），复用兼容层的同一套解析规则。
pub fn resolve_bash_intent(args: &Value) -> Result<Intent, String> {
    resolve_intent(args, &CompatConfig::default())
}

pub fn intent_tag(intent: Intent) -> &'static str {
    match intent {
        Intent::Raw => "raw",
        Intent::Summary => "summary",
        Intent::Structured => "structured",
        Intent::Visual => "visual",
        Intent::Auto => "auto",
    }
}

/// §5.1 的 5 步主链：召回错误行与尾部（可并发）→ decide 判定 → summarize → self_check。
/// 补读 / 二次摘要属超预算扩展，按 §4.8 不中途加步；链级上限 5 步、并发 4。
pub fn bash_chain() -> Vec<ChainNode> {
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
            action: ChainAction::SelfCheck,
            depends_on: vec![3],
        },
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BashMode {
    /// 截断原文 + 落盘指针（v1 行为）。
    Raw,
    /// 槽位全空，按 §3.8 退到机械路径并告知（§7.3）。
    SummaryDegraded,
    /// 结构化事实 JSON（§5.1）。
    Structured,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BashPlan {
    pub mode: BashMode,
    /// 需同时进 content 文本与 structuredContent 的告知（§4.4b 两个通道）。
    pub notices: Vec<Notice>,
}

fn slot_unavailable_notices() -> Vec<Notice> {
    vec![
        Notice::SlotUnavailable {
            slot: "decide".to_owned(),
            fallback: SlotFallback::Mechanical,
        },
        Notice::SlotUnavailable {
            slot: "summarize".to_owned(),
            fallback: SlotFallback::Mechanical,
        },
    ]
}

/// 协商层：按 intent 与输出体量决定返回形态。纯函数，同输入同输出。
pub fn plan_bash(intent: Intent, tokens: usize) -> Result<BashPlan, String> {
    let plan = match intent {
        Intent::Raw => BashPlan {
            mode: BashMode::Raw,
            notices: vec![],
        },
        Intent::Visual => {
            return Err(
                "bash 不返回图片，intent=visual 不适用；要精确看图请用 read 工具，或改用 intent=raw"
                    .to_owned(),
            );
        }
        Intent::Structured => BashPlan {
            mode: BashMode::Structured,
            notices: vec![],
        },
        Intent::Auto if tokens < AUTO_SUMMARY_TOKENS => BashPlan {
            mode: BashMode::Raw,
            notices: vec![],
        },
        Intent::Summary | Intent::Auto => BashPlan {
            mode: BashMode::SummaryDegraded,
            notices: slot_unavailable_notices(),
        },
    };
    Ok(plan)
}

/// §5.1 structured 路径的事实 JSON。
/// decide 槽位不可用时按 §3.8「不判错、不判类型」给 unknown / 空数组，不编造判定结果。
pub fn structured_facts(exit_code: Option<i32>, spill_path: Option<&str>) -> Value {
    json!({
        "exitCode": exit_code,
        "errorKind": "unknown",
        "severity": "unknown",
        "failedItems": [],
        "spillPath": spill_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notice::render_notices;
    use crate::understanding::{MAX_PARALLEL, MAX_STEPS, schedule, validate_chain};

    #[test]
    fn chain_is_valid_bounded_and_deterministic() {
        let chain = bash_chain();
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
    fn raw_path_has_no_notices() {
        let plan = plan_bash(Intent::Raw, 10_000).unwrap();
        assert_eq!(plan.mode, BashMode::Raw);
        assert!(plan.notices.is_empty());
    }

    #[test]
    fn summary_degrades_with_two_slot_notices() {
        let plan = plan_bash(Intent::Summary, 10).unwrap();
        assert_eq!(plan.mode, BashMode::SummaryDegraded);
        assert_eq!(plan.notices.len(), 2);
        let rendered = render_notices(&plan.notices);
        assert!(rendered.contains("slot=decide"));
        assert!(rendered.contains("slot=summarize"));
        assert!(rendered.contains("fallback=mechanical"));
        assert_eq!(rendered.lines().count(), 2);
    }

    #[test]
    fn auto_threshold_is_500_tokens() {
        assert_eq!(
            plan_bash(Intent::Auto, AUTO_SUMMARY_TOKENS - 1)
                .unwrap()
                .mode,
            BashMode::Raw
        );
        assert_eq!(
            plan_bash(Intent::Auto, AUTO_SUMMARY_TOKENS).unwrap().mode,
            BashMode::SummaryDegraded
        );
    }

    #[test]
    fn structured_mode_has_no_notices() {
        let plan = plan_bash(Intent::Structured, 10).unwrap();
        assert_eq!(plan.mode, BashMode::Structured);
        assert!(plan.notices.is_empty());
    }

    #[test]
    fn visual_is_rejected_actionably() {
        let err = plan_bash(Intent::Visual, 10).unwrap_err();
        assert!(err.contains("read"));
        assert!(err.contains("raw"));
    }

    #[test]
    fn structured_facts_shape_and_no_fabricated_judgment() {
        let facts = structured_facts(Some(2), Some("spill://ab12cd34"));
        assert_eq!(facts["exitCode"], json!(2));
        assert_eq!(facts["errorKind"], json!("unknown"));
        assert_eq!(facts["severity"], json!("unknown"));
        assert_eq!(facts["failedItems"], json!([]));
        assert_eq!(facts["spillPath"], json!("spill://ab12cd34"));
        let none = structured_facts(None, None);
        assert!(none["exitCode"].is_null());
        assert!(none["spillPath"].is_null());
    }

    #[test]
    fn intent_parsing_defaults_to_raw() {
        assert_eq!(resolve_bash_intent(&json!({})).unwrap(), Intent::Raw);
        assert_eq!(
            resolve_bash_intent(&json!({ "intent": "summary" })).unwrap(),
            Intent::Summary
        );
        assert!(resolve_bash_intent(&json!({ "intent": "bogus" })).is_err());
        assert_eq!(intent_tag(Intent::Auto), "auto");
    }

    #[test]
    fn token_estimate_uses_four_bytes() {
        assert_eq!(estimate_tokens(&"x".repeat(2000)), 500);
        assert_eq!(estimate_tokens(""), 0);
    }
}
