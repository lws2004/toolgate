//! 跨槽位服务能力判定（§3.10 准入 / §18.1 协议能力表）。
//!
//! 槽位形状固定（§3.2）：五个槽位的输入输出契约不随模型变。一个模型要服务
//! 非原生槽位，需两条同时成立（§3.10「信息充分 ∧ 形状可转换」）：
//!   ① 输入信息充分：目标槽位要求的信息都在该模型能拿到的输入里；
//!   ② 输出形状可转换：模型输出能稳定转成目标槽位契约形状（§3.10 适配器表）。
//!
//! 本模块把 §18.1 的 source × slot 能力矩阵固化为唯一的可执行落点：§18.1 的
//! 文档表是权威描述，代码判定必须与它逐格一致，不得各自演化出不同结论。
//! 判定为「不可用 / 受限」时按 §7.3 给可行动理由。
//!
//! §3.11：`serves` 是声明不是承诺，这里只做运行时的前置准入，不拒绝配置声明；
//! 最终以首次使用的形状校验为准。配置前三层的 fail loud 仍按 §18.9。

use crate::config::ModelSource;
use crate::slots::SlotKind;

/// 能力分档（§18.1 能力表：可用 / 受限 / 不可用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityGrade {
    Available,
    Limited,
    Unavailable,
    /// 来源不在 §18.1 协议能力表内（auto / local）：不预判，按 §3.11 交给运行时形状校验。
    Unspecified,
}

impl CapabilityGrade {
    pub fn as_str(&self) -> &'static str {
        match self {
            CapabilityGrade::Available => "available",
            CapabilityGrade::Limited => "limited",
            CapabilityGrade::Unavailable => "unavailable",
            CapabilityGrade::Unspecified => "unspecified",
        }
    }
}

/// 一条 source × slot 判定。受限 / 不可用的 reason 必带可行动下一步（§2 红线 7 / §1.4.5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capability {
    pub grade: CapabilityGrade,
    pub reason: &'static str,
}

fn cap(grade: CapabilityGrade, reason: &'static str) -> Capability {
    Capability { grade, reason }
}

/// §18.1：systemone 只做判定、不生成文本，也没有排序接口。
fn systemone(slot: SlotKind) -> Capability {
    match slot {
        SlotKind::Decide => cap(
            CapabilityGrade::Available,
            "noul = P(yes)，直接满足 §3.2 的 decide 契约与 §7.2 门控",
        ),
        SlotKind::Vision => cap(
            CapabilityGrade::Available,
            "images 需后端模型带 vision tower（§18.1）",
        ),
        SlotKind::Classify => cap(
            CapabilityGrade::Limited,
            "systemone 的 choice 有逐选项概率，但 §3.10 适配器本轮只映射 noul；走 choice 的 classify 映射未落，先把该槽位按 §3.8 降级或改用 json 适配器来源",
        ),
        SlotKind::Summarize => cap(
            CapabilityGrade::Unavailable,
            "只做判定、不生成文本，拿不到 summarize 所需的文本输出通道；从 serves 移除 summarize，或改用 openai / anthropic / responses 来源",
        ),
        SlotKind::Rerank => cap(
            CapabilityGrade::Unavailable,
            "无排序接口，拿不到 rerank 所需的候选集打分；从 serves 移除 rerank，或改用支持排序的来源",
        ),
    }
}

/// §18.1：通用 LLM 协议形态（openai / anthropic / responses）。
fn llm_protocol(slot: SlotKind) -> Capability {
    match slot {
        SlotKind::Classify => cap(
            CapabilityGrade::Available,
            "JSON schema / structured outputs 可产出逐标签分数（§18.1）",
        ),
        SlotKind::Summarize => cap(
            CapabilityGrade::Available,
            "可生成文本，按 §3.10 的 text 适配器归一到槽位形状",
        ),
        SlotKind::Vision => cap(
            CapabilityGrade::Available,
            "可传图并产出文字，按 §3.10 的 text 适配器归一到槽位形状",
        ),
        SlotKind::Decide => cap(
            CapabilityGrade::Limited,
            "chat 协议给的是自报概率、非校准（§3.10 / §7.2 走最保守档）；jev 类判定模型不按 chat 工作，对它们实为不可用（§3.4），改用 source: systemone",
        ),
        SlotKind::Rerank => cap(
            CapabilityGrade::Limited,
            "协议无原生排序接口，需 ordered 适配器产出排序列表且分数非校准；改用专用 reranker 来源",
        ),
    }
}

/// §18.1 的 source × slot 能力矩阵（唯一可执行落点）。
pub fn source_slot_capability(source: ModelSource, slot: SlotKind) -> Capability {
    match source {
        ModelSource::SystemOne => systemone(slot),
        ModelSource::Openai | ModelSource::Anthropic | ModelSource::Responses => llm_protocol(slot),
        ModelSource::Auto | ModelSource::Local => cap(
            CapabilityGrade::Unspecified,
            "不在 §18.1 协议能力表内：auto 先按 §18.1 判定顺序推断，local 按 §3.10 走 native；两者都按 §3.11 以运行时形状校验为准",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCES: [ModelSource; 6] = [
        ModelSource::Auto,
        ModelSource::Local,
        ModelSource::Openai,
        ModelSource::Anthropic,
        ModelSource::Responses,
        ModelSource::SystemOne,
    ];

    /// §18.1 表 6 来源 × 5 槽位全覆盖，且每条都带理由。
    #[test]
    fn matrix_covers_every_source_and_slot() {
        for source in SOURCES {
            for slot in SlotKind::ALL {
                let c = source_slot_capability(source, slot);
                assert!(!c.reason.is_empty(), "{source:?}/{slot:?} 缺理由");
            }
        }
    }

    /// §18.1 systemone 列：只做判定、不生成文本、无排序接口。
    #[test]
    fn systemone_row_matches_spec_table() {
        use CapabilityGrade as G;
        let grade = |slot| source_slot_capability(ModelSource::SystemOne, slot).grade;
        assert_eq!(grade(SlotKind::Summarize), G::Unavailable);
        assert_eq!(grade(SlotKind::Rerank), G::Unavailable);
        assert_eq!(grade(SlotKind::Classify), G::Limited);
        assert_eq!(grade(SlotKind::Decide), G::Available);
        assert_eq!(grade(SlotKind::Vision), G::Available);
    }

    /// §18.1 三个通用 LLM 列：decide / rerank 受限，其余可用。
    #[test]
    fn llm_rows_match_spec_table() {
        use CapabilityGrade as G;
        for source in [
            ModelSource::Openai,
            ModelSource::Anthropic,
            ModelSource::Responses,
        ] {
            let grade = |slot| source_slot_capability(source, slot).grade;
            assert_eq!(grade(SlotKind::Decide), G::Limited, "{source:?}");
            assert_eq!(grade(SlotKind::Rerank), G::Limited, "{source:?}");
            for slot in [
                SlotKind::Classify,
                SlotKind::Summarize,
                SlotKind::Vision,
            ] {
                assert_eq!(grade(slot), G::Available, "{source:?}/{slot:?}");
            }
        }
    }

    /// auto / local 不在协议能力表内，不预判（§3.11）。
    #[test]
    fn auto_and_local_are_unspecified() {
        for source in [ModelSource::Auto, ModelSource::Local] {
            for slot in SlotKind::ALL {
                assert_eq!(
                    source_slot_capability(source, slot).grade,
                    CapabilityGrade::Unspecified
                );
            }
        }
    }

    /// 错误路径断言：每个受限 / 不可用的理由都带可行动下一步，不是空泛说明。
    #[test]
    fn limited_and_unavailable_reasons_are_actionable() {
        let actions = ["移除", "改用", "降级", "适配器"];
        for source in SOURCES {
            for slot in SlotKind::ALL {
                let c = source_slot_capability(source, slot);
                if matches!(
                    c.grade,
                    CapabilityGrade::Limited | CapabilityGrade::Unavailable
                ) {
                    assert!(
                        actions.iter().any(|a| c.reason.contains(a)),
                        "{source:?}/{slot:?} 理由缺可执行下一步：{}",
                        c.reason
                    );
                }
            }
        }
    }
}
