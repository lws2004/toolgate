//! 输出形态决策（§4.5）：执行事实告知合并。
//!
//! 本模块实现 §4.5 的第二段纯函数 finalize_notices：把决策阶段的告知与
//! 执行层观测到的事实合并成唯一事实源 notices，按 §4.4b 固定渲染顺序排序。
//! 纯函数、无副作用、同输入同输出（§16.3）。
//!
//! 第一段 decide_output 的输入契约尚不完整（§4.5 的 7 个参数未列
//! fallback_chain，但决策规则与属性测试都依赖它），需先与架构对齐后实现。

use crate::notice::{
    AdapterKind, ConfidenceViews, DegradeReason, DegradedActual, NormalizeKind, NormalizeLevel,
    Notice, OutputShape, SniffActual, TruncUnit, sort_notices,
};

#[derive(Debug, Clone, PartialEq)]
pub struct TruncationObs {
    pub kept: u64,
    pub total: u64,
    pub unit: TruncUnit,
    pub pointer: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RedactionObs {
    pub count: u32,
    pub retention: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConfidenceObs {
    pub p: f32,
    pub views: ConfidenceViews,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NormalizationObs {
    pub level: NormalizeLevel,
    pub kind: NormalizeKind,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DecodeObs {
    pub encoding: String,
    pub replacements: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SniffObs {
    pub ext: String,
    pub actual: SniffActual,
}

/// 压缩熔断（§4.5:398-400）：lossy / cutoff 由 requested/actual/reason 区分。
#[derive(Debug, Clone, PartialEq)]
pub struct CompressionObs {
    pub requested: OutputShape,
    pub actual: DegradedActual,
    pub slot: Option<String>,
    pub reason: DegradeReason,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UncalibratedObs {
    pub slot: String,
    pub adapter: AdapterKind,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ObservedFacts {
    pub truncation: Option<TruncationObs>,
    pub redaction: Option<RedactionObs>,
    pub confidence: Option<ConfidenceObs>,
    pub normalization: Option<NormalizationObs>,
    pub decode: Option<DecodeObs>,
    pub sniff: Option<SniffObs>,
    pub compression: Option<CompressionObs>,
    pub uncalibrated: Option<UncalibratedObs>,
}

/// §4.5:406-408：只产 8 类执行事实 code，不产 ISOLATION_FALLBACK / SLOT_UNAVAILABLE。
#[cfg(test)]
const EXECUTION_CODES: [&str; 8] = [
    "TRUNCATED",
    "REDACTED",
    "LOW_CONFIDENCE",
    "NORMALIZED",
    "LOSSY_DECODE",
    "SNIFFED",
    "DEGRADED",
    "UNCALIBRATED",
];

impl ObservedFacts {
    fn execution_notices(&self) -> Vec<Notice> {
        let mut out = Vec::new();
        if let Some(o) = &self.truncation {
            out.push(Notice::Truncated {
                kept: o.kept,
                total: o.total,
                unit: o.unit,
                pointer: o.pointer.clone(),
            });
        }
        if let Some(o) = &self.redaction {
            out.push(Notice::Redacted {
                count: o.count,
                retention: o.retention,
            });
        }
        if let Some(o) = &self.confidence {
            out.push(Notice::LowConfidence {
                p: o.p,
                views: o.views,
            });
        }
        if let Some(o) = &self.normalization {
            out.push(Notice::Normalized {
                level: o.level,
                kind: o.kind,
            });
        }
        if let Some(o) = &self.decode {
            out.push(Notice::LossyDecode {
                encoding: o.encoding.clone(),
                replacements: o.replacements,
            });
        }
        if let Some(o) = &self.sniff {
            out.push(Notice::Sniffed {
                ext: o.ext.clone(),
                actual: o.actual,
            });
        }
        if let Some(o) = &self.compression {
            out.push(Notice::Degraded {
                requested: o.requested,
                actual: o.actual,
                reason: o.reason,
                slot: o.slot.clone(),
            });
        }
        if let Some(o) = &self.uncalibrated {
            out.push(Notice::Uncalibrated {
                slot: o.slot.clone(),
                adapter: o.adapter,
            });
        }
        out
    }
}

/// §4.5:403 纯函数：合并决策阶段告知与执行事实，按固定渲染顺序排序。
pub fn finalize_notices(decision: Vec<Notice>, observed: &ObservedFacts) -> Vec<Notice> {
    let mut notices = decision;
    notices.extend(observed.execution_notices());
    sort_notices(&mut notices);
    notices
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notice::{
        FallbackAction, IsolationLevel, IsolationReason, SlotFallback, render_notices,
    };

    fn sample_facts() -> ObservedFacts {
        ObservedFacts {
            truncation: Some(TruncationObs {
                kept: 2000,
                total: 8734,
                unit: TruncUnit::Lines,
                pointer: Some("spill://ab12cd34".to_owned()),
            }),
            redaction: Some(RedactionObs {
                count: 3,
                retention: 0.994,
            }),
            confidence: Some(ConfidenceObs {
                p: 0.62,
                views: ConfidenceViews::Local,
            }),
            normalization: Some(NormalizationObs {
                level: NormalizeLevel::L2,
                kind: NormalizeKind::Eol,
            }),
            decode: Some(DecodeObs {
                encoding: "gbk".to_owned(),
                replacements: 3,
            }),
            sniff: Some(SniffObs {
                ext: ".txt".to_owned(),
                actual: SniffActual::Binary,
            }),
            compression: Some(CompressionObs {
                requested: OutputShape::Visual,
                actual: DegradedActual::Compressed,
                slot: Some("vision".to_owned()),
                reason: DegradeReason::LossyCompression,
            }),
            uncalibrated: Some(UncalibratedObs {
                slot: "decide".to_owned(),
                adapter: AdapterKind::Json,
            }),
        }
    }

    /// 8 个 bit 对应 8 个 Option；全量穷举 2^8 组合做属性测试（§15.7）。
    fn facts_by_mask(mask: u16) -> ObservedFacts {
        let f = sample_facts();
        ObservedFacts {
            truncation: if mask & 1 != 0 { f.truncation } else { None },
            redaction: if mask & 2 != 0 { f.redaction } else { None },
            confidence: if mask & 4 != 0 { f.confidence } else { None },
            normalization: if mask & 8 != 0 { f.normalization } else { None },
            decode: if mask & 16 != 0 { f.decode } else { None },
            sniff: if mask & 32 != 0 { f.sniff } else { None },
            compression: if mask & 64 != 0 { f.compression } else { None },
            uncalibrated: if mask & 128 != 0 {
                f.uncalibrated
            } else {
                None
            },
        }
    }

    /// 按 §4.4b 固定渲染顺序排列的 8 个执行事实 code（class 顺序即 rank 顺序）。
    fn expected_execution_order(mask: u16) -> Vec<&'static str> {
        let ordered = [
            (2u16, "REDACTED"),
            (1u16, "TRUNCATED"),
            (64u16, "DEGRADED"),
            (4u16, "LOW_CONFIDENCE"),
            (8u16, "NORMALIZED"),
            (128u16, "UNCALIBRATED"),
            (16u16, "LOSSY_DECODE"),
            (32u16, "SNIFFED"),
        ];
        ordered
            .iter()
            .filter(|(bit, _)| mask & bit != 0)
            .map(|(_, code)| *code)
            .collect()
    }

    #[test]
    fn sample_facts_produce_all_eight_codes() {
        let out = finalize_notices(vec![], &sample_facts());
        let mut codes: Vec<&str> = out.iter().map(Notice::code).collect();
        codes.sort_unstable();
        let mut want = EXECUTION_CODES.to_vec();
        want.sort_unstable();
        assert_eq!(codes, want);
    }

    /// §15.7 执行事实属性测试：穷举 256 组输入。
    #[test]
    fn exhaustive_observed_facts_properties() {
        for mask in 0u16..256 {
            let f = facts_by_mask(mask);
            let out = finalize_notices(vec![], &f);
            // 只产执行事实类 code
            for n in &out {
                assert!(
                    EXECUTION_CODES.contains(&n.code()),
                    "mask {mask} 产出了非执行事实 code {}",
                    n.code()
                );
            }
            // 每个置位 bit 恰好一条告知
            assert_eq!(out.len(), mask.count_ones() as usize, "mask {mask}");
            // 渲染顺序固定
            let codes: Vec<&str> = out.iter().map(Notice::code).collect();
            assert_eq!(
                codes,
                expected_execution_order(mask),
                "mask {mask} 顺序不符"
            );
            // 同输入同输出（§16.3）
            assert_eq!(finalize_notices(vec![], &f), out, "mask {mask} 不确定");
            // content 渲染行数与告知数一致
            assert_eq!(
                render_notices(&out).lines().count(),
                out.len(),
                "mask {mask}"
            );
        }
    }

    #[test]
    fn decision_notices_preserved_and_sorted_first() {
        let decision = vec![
            Notice::SlotUnavailable {
                slot: "vision".to_owned(),
                fallback: SlotFallback::Metadata,
                reason: None,
            },
            Notice::IsolationFallback {
                requested: IsolationLevel::Raw,
                actual: FallbackAction::Structure,
                rule: "**/.env*".to_owned(),
                reason: IsolationReason::RetentionBelowThreshold,
            },
        ];
        let out = finalize_notices(decision, &sample_facts());
        let codes: Vec<&str> = out.iter().map(Notice::code).collect();
        assert_eq!(codes.first(), Some(&"ISOLATION_FALLBACK"));
        assert_eq!(codes.get(3), Some(&"SLOT_UNAVAILABLE"));
        assert!(codes.contains(&"TRUNCATED"));
        assert!(!codes.contains(&"PARTIAL"));
    }

    #[test]
    fn empty_facts_returns_only_decision() {
        let decision = vec![Notice::Redacted {
            count: 1,
            retention: 0.9,
        }];
        assert_eq!(
            finalize_notices(decision.clone(), &ObservedFacts::default()),
            decision
        );
        assert!(finalize_notices(vec![], &ObservedFacts::default()).is_empty());
    }

    #[test]
    fn compression_maps_to_degraded_with_fixed_values() {
        let out = finalize_notices(vec![], &facts_by_mask(64));
        assert_eq!(out.len(), 1);
        match &out[0] {
            Notice::Degraded {
                requested,
                actual,
                reason,
                slot,
            } => {
                assert_eq!(*requested, OutputShape::Visual);
                assert_eq!(*actual, DegradedActual::Compressed);
                assert_eq!(*reason, DegradeReason::LossyCompression);
                assert_eq!(slot.as_deref(), Some("vision"));
            }
            other => panic!("压缩熔断应产出 DEGRADED，实际 {}", other.code()),
        }
    }
}

// ---------- §4.5 第一段：decide_output ----------

use crate::notice::{FallbackAction, IsolationLevel, IsolationReason, SlotFallback, sanitize_rule};
use serde_json::{Value, json};

/// 五档 intent（§4.2）。v2 首版默认 raw，auto 需显式开启。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    Raw,
    Summary,
    Structured,
    Visual,
    Auto,
}

/// 四档隔离强度（§8.1），按「暴露量」递增：None 最严、Raw 最宽。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ExposureLevel {
    None,
    Understand,
    Summary,
    Raw,
}

/// 四级回退动作（§8.6），信息量从多到少。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackStep {
    Redact,
    Structure,
    Summarize,
    Refuse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentRuleKind {
    SecretPattern,
    SensitivePathMention,
    PathUsername,
    HostMetadata,
    HighEntropy,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PathRule {
    pub glob: String,
    pub level: ExposureLevel,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ContentRule {
    pub kind: ContentRuleKind,
    pub level: FallbackStep,
}

/// 上游能力（§18.1b）：每次调用传入，不是启动声明。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpstreamCapability {
    pub vision: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DecisionInput {
    pub intent: Intent,
    pub isolation_default: ExposureLevel,
    pub max_exposure: ExposureLevel,
    pub fallback_chain: Vec<FallbackStep>,
    pub redact_min_retention: f32,
    pub path_rule: Option<PathRule>,
    pub content_rules: Vec<ContentRule>,
    pub upstream_capability: UpstreamCapability,
    pub slot_available: bool,
    /// §4.4b：不可用的具体原因；与 `slot_available` 由同一份判定产出。
    pub slot_blocker: Option<crate::capability::SlotBlocker>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DecisionOutput {
    pub output_shape: OutputShape,
    pub fallback_steps: Vec<FallbackStep>,
    pub decision_notices: Vec<Notice>,
    pub meta: Value,
}

fn level_str(l: ExposureLevel) -> &'static str {
    match l {
        ExposureLevel::None => "none",
        ExposureLevel::Understand => "understand",
        ExposureLevel::Summary => "summary",
        ExposureLevel::Raw => "raw",
    }
}

fn shape_from_level(l: ExposureLevel) -> OutputShape {
    match l {
        ExposureLevel::None => OutputShape::None,
        ExposureLevel::Understand => OutputShape::Structured,
        ExposureLevel::Summary => OutputShape::Summary,
        ExposureLevel::Raw => OutputShape::Raw,
    }
}

fn level_from_shape(s: OutputShape) -> ExposureLevel {
    match s {
        OutputShape::None => ExposureLevel::None,
        OutputShape::Structured => ExposureLevel::Understand,
        OutputShape::Summary => ExposureLevel::Summary,
        OutputShape::Raw | OutputShape::Visual => ExposureLevel::Raw,
    }
}

fn shape_from_intent(intent: Intent, default_level: ExposureLevel) -> OutputShape {
    match intent {
        Intent::Raw => OutputShape::Raw,
        Intent::Summary => OutputShape::Summary,
        Intent::Structured => OutputShape::Structured,
        Intent::Visual => OutputShape::Visual,
        Intent::Auto => shape_from_level(default_level),
    }
}

fn isolation_level(l: ExposureLevel) -> IsolationLevel {
    match l {
        ExposureLevel::None => IsolationLevel::None,
        ExposureLevel::Understand => IsolationLevel::Understand,
        ExposureLevel::Summary => IsolationLevel::Summary,
        ExposureLevel::Raw => IsolationLevel::Raw,
    }
}

fn fallback_action(step: FallbackStep) -> FallbackAction {
    match step {
        FallbackStep::Redact => FallbackAction::Redact,
        FallbackStep::Structure => FallbackAction::Structure,
        FallbackStep::Summarize => FallbackAction::Summarize,
        FallbackStep::Refuse => FallbackAction::Refuse,
    }
}

/// 输出形态主要依赖的槽位（§3.8），用于 SLOT_UNAVAILABLE 的 slot 字段。
fn dependent_slot(shape: OutputShape) -> &'static str {
    match shape {
        OutputShape::Visual => "vision",
        OutputShape::Summary => "summarize",
        OutputShape::Structured | OutputShape::Raw | OutputShape::None => "decide",
    }
}

/// §4.5 第一段纯函数：能力 → 隔离 → 意图，三段顺序固定，前一段是硬约束。
pub fn decide_output(input: &DecisionInput) -> DecisionOutput {
    let mut notices: Vec<Notice> = Vec::new();
    let mut meta: serde_json::Map<String, Value> = serde_json::Map::new();

    let requested = match input.intent {
        Intent::Raw | Intent::Visual => ExposureLevel::Raw,
        Intent::Summary => ExposureLevel::Summary,
        Intent::Structured => ExposureLevel::Understand,
        Intent::Auto => input.isolation_default,
    };
    meta.insert("requestedLevel".to_owned(), json!(level_str(requested)));

    let mut effective_cap = input.max_exposure;
    if let Some(rule) = &input.path_rule
        && rule.level < effective_cap
    {
        effective_cap = rule.level;
    }
    meta.insert("effectiveLevel".to_owned(), json!(level_str(effective_cap)));

    // 1. 能力：上游收不收 image block。
    let mut shape = shape_from_intent(input.intent, input.isolation_default);
    if shape == OutputShape::Visual && !input.upstream_capability.vision {
        notices.push(Notice::Degraded {
            requested: OutputShape::Visual,
            actual: DegradedActual::Raw,
            reason: DegradeReason::UpstreamNoVision,
            slot: Some("vision".to_owned()),
        });
        shape = OutputShape::Raw;
    }

    // 2. 隔离：路径规则与 max_exposure 取 min，超出走回退链。
    let needs_fallback = requested > effective_cap || !input.content_rules.is_empty();
    let isolation_refused = effective_cap == ExposureLevel::None;
    meta.insert("isolationRefused".to_owned(), json!(isolation_refused));
    let mut fallback_steps: Vec<FallbackStep> = Vec::new();
    if isolation_refused {
        shape = OutputShape::None;
    } else if needs_fallback {
        fallback_steps = input.fallback_chain.clone();
        let first = fallback_steps.first().copied();
        let rule = input
            .path_rule
            .as_ref()
            .map(|r| sanitize_rule(&r.glob))
            .unwrap_or_else(|| "max_exposure".to_owned());
        notices.push(Notice::IsolationFallback {
            requested: isolation_level(requested),
            actual: first.map(fallback_action).unwrap_or(FallbackAction::Refuse),
            rule,
            reason: IsolationReason::RetentionBelowThreshold,
        });
    }

    // 3. 意图：不得突破第 2 段的硬顶；槽位不可用退回机械路径。
    shape = shape_from_level(level_from_shape(shape).min(effective_cap));
    if !input.slot_available {
        notices.push(Notice::SlotUnavailable {
            slot: dependent_slot(shape).to_owned(),
            fallback: SlotFallback::Mechanical,
            reason: input.slot_blocker,
        });
    }

    sort_notices(&mut notices);
    DecisionOutput {
        output_shape: shape,
        fallback_steps,
        decision_notices: notices,
        meta: Value::Object(meta),
    }
}

#[cfg(test)]
mod decide_tests {
    use super::*;

    fn base_input() -> DecisionInput {
        DecisionInput {
            intent: Intent::Auto,
            isolation_default: ExposureLevel::Summary,
            max_exposure: ExposureLevel::Raw,
            fallback_chain: vec![
                FallbackStep::Redact,
                FallbackStep::Structure,
                FallbackStep::Summarize,
                FallbackStep::Refuse,
            ],
            redact_min_retention: 0.5,
            path_rule: None,
            content_rules: vec![],
            upstream_capability: UpstreamCapability { vision: true },
            slot_available: true,
            slot_blocker: None,
        }
    }

    /// §4.5:417-422 属性测试：穷举 5×4×4×2×2×2×2 = 1280 组输入。
    #[test]
    fn decision_properties_exhaustive() {
        let intents = [
            Intent::Raw,
            Intent::Summary,
            Intent::Structured,
            Intent::Visual,
            Intent::Auto,
        ];
        let levels = [
            ExposureLevel::None,
            ExposureLevel::Understand,
            ExposureLevel::Summary,
            ExposureLevel::Raw,
        ];
        let allowed = ["ISOLATION_FALLBACK", "DEGRADED", "SLOT_UNAVAILABLE"];
        let mut cases = 0u32;
        for intent in intents {
            for max_exposure in levels {
                for default in levels {
                    for vision in [true, false] {
                        for slot_available in [true, false] {
                            for has_path in [true, false] {
                                for has_content in [true, false] {
                                    let mut input = base_input();
                                    input.intent = intent;
                                    input.max_exposure = max_exposure;
                                    input.isolation_default = default;
                                    input.upstream_capability.vision = vision;
                                    input.slot_available = slot_available;
                                    if has_path {
                                        input.path_rule = Some(PathRule {
                                            glob: "**/*.log".to_owned(),
                                            level: ExposureLevel::Summary,
                                        });
                                    }
                                    if has_content {
                                        input.content_rules = vec![ContentRule {
                                            kind: ContentRuleKind::SecretPattern,
                                            level: FallbackStep::Redact,
                                        }];
                                    }
                                    let out = decide_output(&input);
                                    // ① output_shape 不超出 max_exposure
                                    assert!(
                                        level_from_shape(out.output_shape) <= input.max_exposure,
                                        "越过硬顶：{intent:?}/{max_exposure:?} -> {:?}",
                                        out.output_shape
                                    );
                                    // ② fallback_steps 每步在配置链中且保序
                                    let mut last_idx: Option<usize> = None;
                                    for step in &out.fallback_steps {
                                        let idx = input
                                            .fallback_chain
                                            .iter()
                                            .position(|s| s == step)
                                            .expect("回退步骤必须在配置链里");
                                        if let Some(prev) = last_idx {
                                            assert!(idx > prev, "回退链未保序");
                                        }
                                        last_idx = Some(idx);
                                    }
                                    // ③ decision_notices 只含三类
                                    for n in &out.decision_notices {
                                        assert!(
                                            allowed.contains(&n.code()),
                                            "决策阶段产出了 {} ",
                                            n.code()
                                        );
                                    }
                                    // ④ 同输入同输出
                                    assert_eq!(decide_output(&input), out);
                                    cases += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(cases, 1280);
    }

    #[test]
    fn visual_without_upstream_vision_degrades_to_text() {
        let mut input = base_input();
        input.intent = Intent::Visual;
        input.upstream_capability.vision = false;
        let out = decide_output(&input);
        assert_eq!(out.output_shape, OutputShape::Raw);
        assert_eq!(out.decision_notices.len(), 1);
        assert_eq!(out.decision_notices[0].code(), "DEGRADED");
        assert!(
            out.decision_notices[0]
                .render()
                .contains("upstream_no_vision")
        );
    }

    #[test]
    fn none_level_marks_isolation_refused() {
        let mut input = base_input();
        input.max_exposure = ExposureLevel::None;
        let out = decide_output(&input);
        assert_eq!(out.output_shape, OutputShape::None);
        assert_eq!(out.meta["isolationRefused"], json!(true));
    }

    #[test]
    fn intent_above_cap_emits_isolation_fallback_with_sanitized_rule() {
        let mut input = base_input();
        input.intent = Intent::Raw;
        input.path_rule = Some(PathRule {
            glob: "/Users/lanws/workspace/toolgate/**/.env*".to_owned(),
            level: ExposureLevel::Understand,
        });
        let out = decide_output(&input);
        assert_eq!(out.decision_notices[0].code(), "ISOLATION_FALLBACK");
        let rendered = out.decision_notices[0].render();
        assert!(!rendered.contains("/Users/"), "rule 未脱敏：{rendered}");
        assert!(rendered.contains("rule=<external>"));
        assert_eq!(out.fallback_steps.len(), 4);
    }

    #[test]
    fn slot_unavailable_emits_notice_without_breaking_cap() {
        let mut input = base_input();
        input.intent = Intent::Raw;
        input.max_exposure = ExposureLevel::Understand;
        input.slot_available = false;
        let out = decide_output(&input);
        assert!(level_from_shape(out.output_shape) <= ExposureLevel::Understand);
        assert!(
            out.decision_notices
                .iter()
                .any(|n| n.code() == "SLOT_UNAVAILABLE")
        );
    }
}
