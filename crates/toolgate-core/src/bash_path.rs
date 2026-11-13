//! bash 的智能路径（§5.1 / §4.8 / §3.8 / §7.3）。
//!
//! 纯逻辑：intent 分流、理解链 DAG 定义、L1 判定的问题集与归并规则、
//! 槽位不可用时的降级告知。本模块不发模型调用（HTTP 发送在 slm/client.rs）。
//!
//! §5.1 的 error_kind / severity 是多值语义，而 decide 槽位契约（§3.2）只有
//! 「每个问题一个概率」的二元形状，systemone 的 choice 类型本轮未映射；
//! 因此把多值语义分解成九个二元问题，再按规则归并（§13.4 方案 A）。

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::compat::{CompatConfig, resolve_intent};
use crate::decision::Intent;
use crate::notice::Notice;
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
    /// 摘要路径：summarize 槽位可用时给摘要；不可用时按 §3.8 退到原文并告知（§7.3）。
    SummaryDegraded,
    /// 结构化事实 JSON（§5.1）。
    Structured,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BashPlan {
    pub mode: BashMode,
    /// 需同时进 content 文本与 structuredContent 的告知（§4.4b 两个通道）。
    pub notices: Vec<Notice>,
    /// §5.1/§9.2：输出 ≥ 500 token 才触发 L1 判定；低于门槛由 exitCode 机械判断。
    /// 由协商层单点产出，运行时不再另算一次门槛（避免双源）。
    pub should_decide: bool,
    /// §5.1：summary / auto 且输出 ≥ 500 token 时尝试 L2 摘要；告知由运行时按
    /// 实际结果追加（与 decide 对称），这里不写死降级告知。
    pub should_summarize: bool,
}

/// 协商层：按 intent 与输出体量决定返回形态。纯函数，同输入同输出。
/// §5.1/§9.2：summary / structured / auto 都只在输出 ≥ 500 token 时触发判定；
/// 低于门槛给机械结果（原文或由 exitCode 支撑的结构化事实），不发降级告知。
pub fn plan_bash(intent: Intent, tokens: usize) -> Result<BashPlan, String> {
    let should_decide = matches!(intent, Intent::Summary | Intent::Structured | Intent::Auto)
        && tokens >= AUTO_SUMMARY_TOKENS;
    let should_summarize =
        matches!(intent, Intent::Summary | Intent::Auto) && tokens >= AUTO_SUMMARY_TOKENS;
    let (mode, notices) = match intent {
        Intent::Raw => (BashMode::Raw, vec![]),
        Intent::Visual => {
            return Err(
                "bash 不返回图片，intent=visual 不适用；要精确看图请用 read 工具，或改用 intent=raw"
                    .to_owned(),
            );
        }
        Intent::Structured => (BashMode::Structured, vec![]),
        Intent::Auto | Intent::Summary if tokens < AUTO_SUMMARY_TOKENS => (BashMode::Raw, vec![]),
        Intent::Summary | Intent::Auto => (BashMode::SummaryDegraded, vec![]),
    };
    Ok(BashPlan {
        mode,
        notices,
        should_decide,
        should_summarize,
    })
}

// ---------- §5.1 L1 判定（decide 槽位） ----------

/// §5.1 的 L1 判定问题集：九个二元问题，顺序即判定顺序。
///
/// §5.1 的 error_kind / severity 是多值语义，而 decide 槽位契约（§3.2）只有
/// 「每个问题一个概率」的二元形状，systemone 的 choice 类型本轮未映射；
/// 因此把多值语义分解成若干二元问题，再按规则归并（§13.4 方案 A：
/// 一次前向读出全部问题的概率）。
pub const L1_QUESTIONS: [&str; 9] = [
    "这段输出中是否包含错误或失败迹象？",
    "这次失败属于编译错误吗？",
    "这次失败属于测试失败吗？",
    "这次失败属于权限问题吗？",
    "这次失败属于网络问题吗？",
    "这次失败属于资源问题吗（例如内存不足、磁盘已满）？",
    "这次失败是否阻断了任务继续（必须人工处理）？",
    "这次失败是否只是噪音（可以忽略）？",
    "判断这些问题是否必须查看完整输出才能确定？",
];

/// L1 的错误类型候选（对应 L1_QUESTION_KEYS 里的 kind 五问）。
const L1_ERROR_KINDS: [&str; 5] = ["compile", "test", "permission", "network", "resource"];

/// L1 九问的稳定 key：与 L1_QUESTIONS 一一对应，顺序固定。
/// 归并只按 key 取值，不靠中文问题文本匹配。
pub const L1_QUESTION_KEYS: [&str; 9] = [
    "has_error",
    "kind_compile",
    "kind_test",
    "kind_permission",
    "kind_network",
    "kind_resource",
    "severity_blocking",
    "severity_noise",
    "needs_full_context",
];

/// §5.1 的 L1 判定结论。
#[derive(Debug, Clone, PartialEq)]
pub struct BashDecision {
    /// 是否判定为有错误。
    pub has_error: bool,
    /// 错误类型：none / compile / test / permission / network / resource / unknown。
    pub error_kind: String,
    /// 严重度：blocking / noise / warning。
    pub severity: String,
    /// 是否必须查看完整输出才能确定。
    pub needs_full_context: bool,
    /// 九问中的最小概率：本次结论的置信度（§7.2 门控输入）。
    pub confidence: f64,
}

/// 归并 L1 判定（纯函数）：九问概率 → 结论。
///
/// 规则（§5.1）：has_error < 0.5 → errorKind=none；否则在五个 kind 候选里取
/// 概率最大者，最大者 < 0.5 → unknown。severity_blocking ≥ 0.5 → blocking；
/// 否则 severity_noise ≥ 0.5 → noise；否则 warning。needs_full_context ≥ 0.5 → true。
///
/// 归并阈值 0.5 与 §7.2 的门控阈值是两回事：门控决定采不采用，归并决定落到哪个字段值。
pub fn merge_decision(probs: &BTreeMap<String, f64>) -> BashDecision {
    let p = |key: &str| probs.get(key).copied().unwrap_or(0.0);
    let has_error_probability = p(L1_QUESTION_KEYS[0]);
    let has_error = has_error_probability >= 0.5;

    // kind 候选取 argmax（五个 kind 问）。
    let mut best_index = 0usize;
    let mut best = p(L1_QUESTION_KEYS[1]);
    for (offset, key) in L1_QUESTION_KEYS[1..5].iter().enumerate() {
        let candidate = p(key);
        if candidate > best {
            best = candidate;
            best_index = offset;
        }
    }
    let (error_kind, kind_probability) = if has_error {
        if best < 0.5 {
            ("unknown".to_owned(), best)
        } else {
            (L1_ERROR_KINDS[best_index].to_owned(), best)
        }
    } else {
        // 判为「无错」时没有选类别，结论只依赖 has_error 本身。
        ("none".to_owned(), has_error_probability)
    };

    let severity = if p(L1_QUESTION_KEYS[6]) >= 0.5 {
        "blocking"
    } else if p(L1_QUESTION_KEYS[7]) >= 0.5 {
        "noise"
    } else {
        "warning"
    };

    BashDecision {
        has_error,
        error_kind,
        severity: severity.to_owned(),
        needs_full_context: p(L1_QUESTION_KEYS[8]) >= 0.5,
        // confidence 是「本次结论所依赖的问题概率的最小值」：has_error 与被选中的类别。
        // **不是全部问题的最小值**：多选问题的非答案概率天然就低（它们本来就该低），
        // 取全部最小值会把任何多问题判定压到低置信档，§7.2 的门控等于失效
        // （2026-11-09 本机实测：bash 与 edit 的 confidence 恒为 0.01~0.11，恒走 both_views）。
        confidence: has_error_probability.min(kind_probability),
    }
}

/// §5.1 structured 路径的判定事实：decide 不可用时各字段留空
/// （§3.8「不判错、不判类型」：unknown / 空数组 / null，不编造判定结果）。
#[derive(Debug, Clone, Default)]
pub struct DecisionFacts {
    /// 错误类型；None → unknown。
    pub error_kind: Option<String>,
    /// 严重度；None → unknown。
    pub severity: Option<String>,
    /// 失败项：本轮恒为空数组（§5.1 的 failedItems 语义未定义，不编造）。
    pub failed_items: Vec<String>,
    /// 是否必须看完整输出才能确定；None → null。
    pub needs_full_context: Option<bool>,
    /// 本次判定的置信度（九问最小概率）；None → null。
    pub confidence: Option<f64>,
}

/// §5.1 structured 路径的事实 JSON。
pub fn structured_facts(
    exit_code: Option<i32>,
    spill_path: Option<&str>,
    decision: &DecisionFacts,
) -> Value {
    json!({
        "exitCode": exit_code,
        "errorKind": decision.error_kind.clone().unwrap_or_else(|| "unknown".to_owned()),
        "severity": decision.severity.clone().unwrap_or_else(|| "unknown".to_owned()),
        "failedItems": decision.failed_items.clone(),
        "spillPath": spill_path,
        "needsFullContext": decision.needs_full_context,
        "confidence": decision.confidence,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notice::render_notices;
    use crate::understanding::{MAX_PARALLEL, MAX_STEPS, schedule, validate_chain};

    /// 按 L1_QUESTION_KEYS 的顺序把九个概率装成映射。
    fn probs(values: [f64; 9]) -> BTreeMap<String, f64> {
        L1_QUESTION_KEYS
            .iter()
            .zip(values)
            .map(|(key, value)| ((*key).to_owned(), value))
            .collect()
    }

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

    /// 到达门槛才进理解链；decide / summarize 的告知都由运行时按实际结果追加。
    #[test]
    fn summary_path_requests_both_slots_without_plan_notices() {
        let plan = plan_bash(Intent::Summary, AUTO_SUMMARY_TOKENS).unwrap();
        assert_eq!(plan.mode, BashMode::SummaryDegraded);
        assert!(plan.should_decide);
        assert!(plan.should_summarize);
        assert!(plan.notices.is_empty());
        assert!(render_notices(&plan.notices).is_empty());
    }

    /// §5.1/§9.2：summary 低于 500 token 给机械结果（原文），不判定、不告知。
    #[test]
    fn summary_below_threshold_stays_raw() {
        let plan = plan_bash(Intent::Summary, AUTO_SUMMARY_TOKENS - 1).unwrap();
        assert_eq!(plan.mode, BashMode::Raw);
        assert!(plan.notices.is_empty());
        assert!(!plan.should_decide);
        assert!(!plan.should_summarize);
    }

    /// §5.1：三种 intent 都到 500 token 门槛才触发判定；raw 永不判定。
    #[test]
    fn decision_triggered_at_threshold_for_all_intents() {
        for intent in [Intent::Summary, Intent::Structured, Intent::Auto] {
            assert!(
                !plan_bash(intent, AUTO_SUMMARY_TOKENS - 1)
                    .unwrap()
                    .should_decide
            );
            assert!(
                plan_bash(intent, AUTO_SUMMARY_TOKENS)
                    .unwrap()
                    .should_decide
            );
        }
        assert!(!plan_bash(Intent::Raw, 10_000).unwrap().should_decide);
        assert!(
            !plan_bash(Intent::Structured, 10_000)
                .unwrap()
                .should_summarize
        );
    }

    /// §5.1：只有 summary / auto 且到门槛才尝试摘要；raw / structured 不摘要。
    #[test]
    fn summarize_triggered_only_for_summary_and_auto() {
        for intent in [Intent::Summary, Intent::Auto] {
            assert!(
                !plan_bash(intent, AUTO_SUMMARY_TOKENS - 1)
                    .unwrap()
                    .should_summarize
            );
            assert!(
                plan_bash(intent, AUTO_SUMMARY_TOKENS)
                    .unwrap()
                    .should_summarize
            );
        }
        assert!(!plan_bash(Intent::Raw, 10_000).unwrap().should_summarize);
        assert!(
            !plan_bash(Intent::Structured, 10_000)
                .unwrap()
                .should_summarize
        );
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
        assert!(!plan.should_decide);
        assert!(!plan.should_summarize);
    }

    #[test]
    fn visual_is_rejected_actionably() {
        let err = plan_bash(Intent::Visual, 10).unwrap_err();
        assert!(err.contains("read"));
        assert!(err.contains("raw"));
    }

    #[test]
    fn structured_facts_shape_and_no_fabricated_judgment() {
        // 判定不可用：unknown / 空数组 / null，不编造判定结果（§3.8）。
        let facts = structured_facts(Some(2), Some("spill://ab12cd34"), &DecisionFacts::default());
        assert_eq!(facts["exitCode"], json!(2));
        assert_eq!(facts["errorKind"], json!("unknown"));
        assert_eq!(facts["severity"], json!("unknown"));
        assert_eq!(facts["failedItems"], json!([]));
        assert_eq!(facts["spillPath"], json!("spill://ab12cd34"));
        assert!(facts["needsFullContext"].is_null());
        assert!(facts["confidence"].is_null());
        let none = structured_facts(None, None, &DecisionFacts::default());
        assert!(none["exitCode"].is_null());
        assert!(none["spillPath"].is_null());

        // 判定可用：填真实值（§5.1）。
        let decided = DecisionFacts {
            error_kind: Some("compile".to_owned()),
            severity: Some("blocking".to_owned()),
            failed_items: Vec::new(),
            needs_full_context: Some(false),
            confidence: Some(0.91),
        };
        let facts = structured_facts(Some(2), None, &decided);
        assert_eq!(facts["errorKind"], json!("compile"));
        assert_eq!(facts["severity"], json!("blocking"));
        assert_eq!(facts["needsFullContext"], json!(false));
        assert_eq!(facts["confidence"], json!(0.91));
        assert_eq!(facts["failedItems"], json!([]));
    }

    /// §5.1：L1 归并规则（has_error / kind argmax / severity 三档 / needs_full_context）。
    #[test]
    fn merge_decision_follows_spec() {
        // has_error 低 → errorKind=none，severity=warning。
        // confidence = 结论所依赖的问题的最小值：判为「无错」时只取 has_error 本身（0.2），
        // 不是九问的最小值（0.1）——否则任何多问题判定都会被压到低置信档。
        let d = merge_decision(&probs([0.2, 0.9, 0.1, 0.1, 0.1, 0.1, 0.2, 0.1, 0.1]));
        assert!(!d.has_error);
        assert_eq!(d.error_kind, "none");
        assert_eq!(d.severity, "warning");
        assert!(!d.needs_full_context);
        assert!((d.confidence - 0.2).abs() < 1e-9);

        // has_error 高 → kind 取 argmax（test），severity=blocking。
        // confidence = min(has_error 0.9, 被选中的 kind 0.8) = 0.8。
        let d = merge_decision(&probs([0.9, 0.2, 0.8, 0.3, 0.1, 0.1, 0.7, 0.1, 0.2]));
        assert!(d.has_error);
        assert_eq!(d.error_kind, "test");
        assert_eq!(d.severity, "blocking");
        assert!((d.confidence - 0.8).abs() < 1e-9);

        // kind 最大者 < 0.5 → unknown；severity_noise ≥ 0.5 → noise。
        let d = merge_decision(&probs([0.9, 0.2, 0.3, 0.1, 0.1, 0.1, 0.1, 0.9, 0.1]));
        assert_eq!(d.error_kind, "unknown");
        assert_eq!(d.severity, "noise");

        // needs_full_context 阈值 0.5。
        let d = merge_decision(&probs([0.9, 0.9, 0.1, 0.1, 0.1, 0.1, 0.1, 0.1, 0.5]));
        assert!(d.needs_full_context);
        let d = merge_decision(&probs([0.9, 0.9, 0.1, 0.1, 0.1, 0.1, 0.1, 0.1, 0.49]));
        assert!(!d.needs_full_context);
    }

    /// 归并只按稳定 key 取值，不依赖中文问题文本（lead 口径）。
    #[test]
    fn merge_decision_uses_stable_keys() {
        assert_eq!(L1_QUESTION_KEYS.len(), L1_QUESTIONS.len());
        assert_eq!(L1_QUESTION_KEYS[0], "has_error");
        assert_eq!(L1_QUESTION_KEYS[6], "severity_blocking");
        assert_eq!(L1_QUESTION_KEYS[7], "severity_noise");
        assert_eq!(L1_QUESTION_KEYS[8], "needs_full_context");
        // 缺 key 时按 0 处理，不编造结论。
        let d = merge_decision(&BTreeMap::new());
        assert!(!d.has_error);
        assert_eq!(d.error_kind, "none");
        assert_eq!(d.confidence, 0.0);
    }

    /// §5.1：九个问题覆盖 has_error / 五个 kind / severity 两问 / needs_full_context。
    #[test]
    fn l1_question_set_covers_spec() {
        assert_eq!(L1_QUESTIONS.len(), 9);
        assert!(L1_QUESTIONS[0].contains("错误或失败"));
        assert!(L1_QUESTIONS[6].contains("阻断"));
        assert!(L1_QUESTIONS[7].contains("噪音"));
        assert!(L1_QUESTIONS[8].contains("完整输出"));
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
