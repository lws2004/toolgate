//! 跨槽位服务能力判定（§3.10 准入 / §18.1 协议能力表）。
//!
//! 槽位形状固定（§3.2）：六个槽位的输入输出契约不随模型变。一个模型要服务
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
use serde::{Deserialize, Serialize};

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
            CapabilityGrade::Available,
            "choice 的 probabilities 是逐选项校准概率，按 §3.10 映射成 §3.2 的逐标签 scores；confidence / choice 字段不参与映射",
        ),
        SlotKind::Summarize => cap(
            CapabilityGrade::Unavailable,
            "只做判定、不生成文本，拿不到 summarize 所需的文本输出通道；从 serves 移除 summarize，或改用 openai / anthropic / responses 来源",
        ),
        SlotKind::Rerank => cap(
            CapabilityGrade::Unavailable,
            "无排序接口，拿不到 rerank 所需的候选集打分；从 serves 移除 rerank，或改用支持排序的来源",
        ),
        SlotKind::Embedding => cap(
            CapabilityGrade::Unavailable,
            "无向量化接口，拿不到 embedding 所需的定长向量；从 serves 移除 embedding，或改用 embedding 来源",
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
        SlotKind::Embedding => cap(
            CapabilityGrade::Unavailable,
            "通用 chat 协议无原生向量端点；从 serves 移除 embedding，或改用 source: embedding",
        ),
    }
}

/// §18.1：原生 rerank 协议只返回排序结果，不生成文本、不出校准概率、不传图。
fn rerank(slot: SlotKind) -> Capability {
    match slot {
        SlotKind::Rerank => cap(
            CapabilityGrade::Available,
            "原生 /v1/rerank 返回候选相关度排序，满足 §3.2 的 rerank 契约",
        ),
        _ => cap(
            CapabilityGrade::Unavailable,
            "rerank 协议只返回排序结果，缺文本 / 概率 / 图像输出通道；从 serves 移除该槽位或改用其它来源",
        ),
    }
}

/// §18.1：原生 embedding 协议只产出向量，不做判定 / 生成 / 排序 / 传图。
fn embedding(slot: SlotKind) -> Capability {
    match slot {
        SlotKind::Embedding => cap(
            CapabilityGrade::Available,
            "原生 /v1/embeddings 返回定长向量，满足 §3.2 的 embedding 契约",
        ),
        _ => cap(
            CapabilityGrade::Unavailable,
            "embedding 协议只返回向量，缺文本 / 概率 / 排序 / 图像输出通道；从 serves 移除该槽位或改用其它来源",
        ),
    }
}

/// §4.4b / §18.9：槽位不可用的**具体原因**。
///
/// 值域与 `SLOT_UNAVAILABLE.reason` 完全一致——一份枚举，notice 的 `reason=`、
/// 返回里的 `slmStatusDetail.reason` 与启动告警读的都是它，不在三处各算一遍。
/// 原因决定下一步：前两种改配置，`no_client` 重编译，`disabled` 等恢复或换模型，
/// `call_failed` 检查服务监听与地址后重试。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotBlocker {
    /// 没有能服务该槽位的模型（`serves` 未声明或解析后为空）。
    Unbound,
    /// 来源×槽位在 §18.1 能力矩阵里判 `Unavailable`（信息不充分或形状不可转换）。
    SourceSlotMismatch,
    /// 当前构建未含该来源的协议客户端（默认构建关闭 `external-protocols`，§19.6）。
    NoClient,
    /// 连续失败被禁用（§3.11），属运行期状态。
    Disabled,
    /// 本次调用失败或超时，槽位本身仍可用（找到了模型但没有拿到结果）。
    CallFailed,
}

impl SlotBlocker {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unbound => "unbound",
            Self::SourceSlotMismatch => "source_slot_mismatch",
            Self::NoClient => "no_client",
            Self::Disabled => "disabled",
            Self::CallFailed => "call_failed",
        }
    }

    /// 最少可执行的下一步（§1.4.5 第三层）：每条一句，按顺序尝试；不写寒暄。
    pub fn next_steps(self) -> &'static [&'static str] {
        match self {
            Self::Unbound => &[
                "在 models 的 serves 里声明能服务该槽位的模型",
                "或让它按 §3.8 走机械路径",
            ],
            Self::SourceSlotMismatch => &[
                "从 serves 移除该槽位",
                "或改用覆盖该槽位的来源（§18.1 能力矩阵）",
            ],
            Self::NoClient => &[
                "用 cargo build --release -p toolgate-cli --features external-protocols 重建",
                "或改用不需要协议客户端的本地来源",
            ],
            Self::Disabled => &["检查该模型后端是否恢复", "恢复后按 §3.11 重新声明或更换模型"],
            Self::CallFailed => &["检查该模型服务是否在监听、地址是否可连", "确认服务可用后重试"],
        }
    }
}

/// 来源层原因（不含未绑定与禁用）：能服务却因为构建或能力矩阵不可用。
pub fn source_blocker(source: ModelSource, slot: SlotKind) -> Option<SlotBlocker> {
    if !client_available(source) {
        return Some(SlotBlocker::NoClient);
    }
    if source_slot_capability(source, slot).grade == CapabilityGrade::Unavailable {
        return Some(SlotBlocker::SourceSlotMismatch);
    }
    None
}

/// §19.6 / §3.11：当前构建是否包含该来源所需的协议客户端。
///
/// 外部协议来源（openai / anthropic / responses / systemone / rerank / embedding）走 HTTP 客户端，
/// 属可选层——默认构建关闭 `external-protocols`，此时即使配置绑定了也**不可用**。
/// 已绑定不等于能用：状态不能谎报 `ready`（§7.3）。
pub fn client_available(source: ModelSource) -> bool {
    match source {
        ModelSource::Openai
        | ModelSource::Anthropic
        | ModelSource::Responses
        | ModelSource::SystemOne
        | ModelSource::Rerank
        | ModelSource::Embedding => cfg!(feature = "external-protocols"),
        ModelSource::Auto | ModelSource::Local => true,
    }
}

/// §3.10 准入 + §19.6 构建可用性：一个来源此刻能否真正服务某槽位。
///
/// 两个条件缺一不可：能力矩阵不是 `Unavailable`，且构建含该来源的协议客户端。
/// 各工具的可用性判定统一走这里，避免各自拼装出不一致的结论（§7.3 一份事实源）。
pub fn source_slot_usable(source: ModelSource, slot: SlotKind) -> bool {
    source_blocker(source, slot).is_none()
}

/// 能力矩阵达到指定档位**且**构建可用（如图片判定要求 decide 恰好 `Available`）。
pub fn source_slot_at(source: ModelSource, slot: SlotKind, grade: CapabilityGrade) -> bool {
    source_slot_usable(source, slot) && source_slot_capability(source, slot).grade == grade
}

/// §18.1 的 source × slot 能力矩阵（唯一可执行落点）。
pub fn source_slot_capability(source: ModelSource, slot: SlotKind) -> Capability {
    match source {
        ModelSource::SystemOne => systemone(slot),
        ModelSource::Rerank => rerank(slot),
        ModelSource::Embedding => embedding(slot),
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

    const SOURCES: [ModelSource; 8] = [
        ModelSource::Auto,
        ModelSource::Local,
        ModelSource::Openai,
        ModelSource::Anthropic,
        ModelSource::Responses,
        ModelSource::SystemOne,
        ModelSource::Rerank,
        ModelSource::Embedding,
    ];

    /// §19.6：外部协议来源的可用性随构建 feature 变化；local / auto 恒可用。
    #[test]
    fn client_availability_follows_build_feature() {
        let expected_external = cfg!(feature = "external-protocols");
        for source in [
            ModelSource::Openai,
            ModelSource::Anthropic,
            ModelSource::Responses,
            ModelSource::SystemOne,
            ModelSource::Rerank,
            ModelSource::Embedding,
        ] {
            assert_eq!(client_available(source), expected_external, "{source:?}");
            let matrix_usable = source_slot_capability(source, SlotKind::Decide).grade
                != CapabilityGrade::Unavailable;
            assert_eq!(
                source_slot_usable(source, SlotKind::Decide),
                matrix_usable && expected_external,
                "{source:?}"
            );
        }
        assert!(client_available(ModelSource::Local));
        assert!(client_available(ModelSource::Auto));
    }

    /// §7.3：systemone × decide 在能力矩阵里是 Available，但默认构建没有客户端
    /// → 已绑定也不能标 ready；图片判定要求「恰好 Available + 构建可用」同理。
    #[test]
    fn bound_but_unbuilt_source_is_not_usable() {
        assert_eq!(
            source_slot_capability(ModelSource::SystemOne, SlotKind::Decide).grade,
            CapabilityGrade::Available,
            "能力矩阵本身仍是 Available"
        );
        let built = cfg!(feature = "external-protocols");
        assert_eq!(
            source_slot_usable(ModelSource::SystemOne, SlotKind::Decide),
            built
        );
        assert_eq!(
            source_slot_at(
                ModelSource::SystemOne,
                SlotKind::Decide,
                CapabilityGrade::Available
            ),
            built
        );
        // 能力矩阵判不可用的组合，构建可用也仍然不可用。
        assert!(!source_slot_usable(ModelSource::SystemOne, SlotKind::Summarize));
    }

    /// §4.4b / §18.9：不可用的原因四值域——前两种改配置、`no_client` 重编译、`disabled` 等恢复。
    #[test]
    fn source_blocker_reports_why_not_usable() {
        let built = cfg!(feature = "external-protocols");
        // 构建缺客户端优先于能力矩阵：默认构建里外部来源一律 no_client。
        let external = if built {
            Some(SlotBlocker::SourceSlotMismatch)
        } else {
            Some(SlotBlocker::NoClient)
        };
        assert_eq!(
            source_blocker(ModelSource::Openai, SlotKind::Embedding),
            external
        );
        // 本地来源不受构建影响：能力矩阵不预判 → 没有原因（可用）。
        assert_eq!(source_blocker(ModelSource::Local, SlotKind::Decide), None);
        // 原生来源覆盖不了的槽位 → 能力矩阵原因。
        assert_eq!(
            source_blocker(ModelSource::SystemOne, SlotKind::Summarize),
            external
        );
        assert_eq!(SlotBlocker::Unbound.as_str(), "unbound");
        assert_eq!(
            SlotBlocker::NoClient.next_steps()[0],
            "用 cargo build --release -p toolgate-cli --features external-protocols 重建"
        );
    }

    /// §18.1 表 8 来源 × 6 槽位全覆盖，且每条都带理由。
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
        assert_eq!(grade(SlotKind::Classify), G::Available);
        assert_eq!(grade(SlotKind::Decide), G::Available);
        assert_eq!(grade(SlotKind::Vision), G::Available);
        assert_eq!(grade(SlotKind::Embedding), G::Unavailable);
    }

    /// §18.1 rerank 列：只有 rerank 槽位可用，其余五个槽位不可用。
    #[test]
    fn rerank_row_matches_spec_table() {
        use CapabilityGrade as G;
        let grade = |slot| source_slot_capability(ModelSource::Rerank, slot).grade;
        assert_eq!(grade(SlotKind::Rerank), G::Available);
        for slot in [
            SlotKind::Classify,
            SlotKind::Decide,
            SlotKind::Summarize,
            SlotKind::Vision,
            SlotKind::Embedding,
        ] {
            assert_eq!(grade(slot), G::Unavailable, "{slot:?}");
        }
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
            assert_eq!(grade(SlotKind::Embedding), G::Unavailable, "{source:?}");
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
