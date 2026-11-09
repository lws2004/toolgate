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
