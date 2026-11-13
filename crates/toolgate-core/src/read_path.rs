//! read 图片路径（§5.3 / §10.1 / §10.3 / §3.8）。
//!
//! 纯逻辑：前置判断（上游是否收 image block）→ 意图分流 → 降级告知。
//! 不含模型后端（可选层，§19.6）：vision 槽位真实可用性由调用方传入，
//! 不可用时按 §3.8 退到压缩后原图或文件元信息，并按 §7.3 显式告知。

use std::path::Path;

use serde_json::{Value, json};

use crate::decision::Intent;
use crate::notice::{
    DegradeReason, DegradedActual, Notice, OutputShape, SlotFallback, render_notices,
};
use crate::slots::VisionTask;
use crate::truncate::format_size;
use crate::understanding::{ChainAction, ChainNode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageOutcome {
    /// 压缩后原图（v1 压缩流水线）。
    Original,
    /// 文件元信息 + 路径（槽位全空且上游不收图，§10.1:143）。
    Metadata,
    /// vision 槽位可用：强制本地理解，只返回文本。
    TextUnderstanding,
    /// §10.1 中间区间：结构化理解在前、压缩后原图在后。
    Both,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImagePlan {
    pub outcome: ImageOutcome,
    /// TextUnderstanding / Both 时交给 vision 槽位的任务档（§3.2 的 task）。
    pub task: Option<VisionTask>,
    /// §10.1 的 need_pixels 三档；仅 auto 且 decide 判定可用时有值。
    pub need_pixels: Option<NeedPixels>,
    pub notices: Vec<Notice>,
}

/// §5.3 / §10.1 的 image_kind 五分类（unknown 表示判定置信不足）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    UiScreenshot,
    Document,
    Chart,
    Photo,
    CodeScreenshot,
    Unknown,
}

impl ImageKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ImageKind::UiScreenshot => "ui_screenshot",
            ImageKind::Document => "document",
            ImageKind::Chart => "chart",
            ImageKind::Photo => "photo",
            ImageKind::CodeScreenshot => "code_screenshot",
            ImageKind::Unknown => "unknown",
        }
    }
}

/// decide 对图片的判定结果（§5.3 / §10.1）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImageFacts {
    pub kind: ImageKind,
    /// 被选中的 kind 类别概率（unknown 时是五个候选中的最大值）。
    pub kind_confidence: f32,
    pub has_text: bool,
    /// need_pixels 的原始概率 p；三档门控见 need_pixels_gate（§10.1 / §10.3）。
    pub need_pixels: f32,
}

/// §5.3 图片路径 L1 的七问：五类 image_kind 各一问 + has_text + need_pixels。
/// 多值语义拆成二元问题、一次调用问完（同 §5.1 的落地口径）。
pub const IMAGE_KIND_QUESTIONS: [(&str, ImageKind); 5] = [
    ("这张图是 UI 界面截图吗？", ImageKind::UiScreenshot),
    ("这张图是文档或表单的照片吗？", ImageKind::Document),
    ("这张图是图表（折线、柱状、饼图等）吗？", ImageKind::Chart),
    ("这张图是普通照片吗？", ImageKind::Photo),
    ("这张图是代码截图吗？", ImageKind::CodeScreenshot),
];
pub const HAS_TEXT_QUESTION: &str = "图中包含可读文字吗？";
pub const NEED_PIXELS_QUESTION: &str = "必须看到原图像素才能完成用户的请求吗？";

/// 七问的固定顺序：image_kind 五问 → has_text → need_pixels。
pub fn image_decide_questions() -> Vec<String> {
    IMAGE_KIND_QUESTIONS
        .iter()
        .map(|(question, _)| (*question).to_owned())
        .chain([
            HAS_TEXT_QUESTION.to_owned(),
            NEED_PIXELS_QUESTION.to_owned(),
        ])
        .collect()
}

/// 归并 decide 答案（§5.3 / §10.1）：
/// - image_kind 取五个候选里概率最大者，最大者 < 0.5 记 unknown；
/// - has_text 概率 ≥ 0.5；
/// - need_pixels 保留原始概率，由 need_pixels_gate 按三档处理。
pub fn merge_image_answers(
    answers: &std::collections::BTreeMap<String, crate::slots::DecideAnswer>,
) -> Option<ImageFacts> {
    let mut best: Option<(ImageKind, f64)> = None;
    for (question, kind) in IMAGE_KIND_QUESTIONS {
        let p = answers.get(question)?.probability;
        if best.is_none_or(|(_, best_p)| p > best_p) {
            best = Some((kind, p));
        }
    }
    let (kind, max_p) = best?;
    let kind = if max_p < 0.5 {
        ImageKind::Unknown
    } else {
        kind
    };
    let has_text = answers.get(HAS_TEXT_QUESTION)?.probability >= 0.5;
    let need_pixels = answers.get(NEED_PIXELS_QUESTION)?.probability as f32;
    Some(ImageFacts {
        kind,
        kind_confidence: max_p as f32,
        has_text,
        need_pixels,
    })
}

/// image_kind → vision 任务档（§5.3 / §10.1 的分流表）。
pub fn vision_task_for(kind: ImageKind) -> VisionTask {
    match kind {
        ImageKind::UiScreenshot => VisionTask::Ground,
        ImageKind::Document | ImageKind::CodeScreenshot => VisionTask::Ocr,
        ImageKind::Chart | ImageKind::Photo | ImageKind::Unknown => VisionTask::Describe,
    }
}

/// §5.3 文本路径的 file_kind 五分类（unknown 表示判定置信不足）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Code,
    Config,
    Log,
    Doc,
    Data,
    Unknown,
}

impl FileKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            FileKind::Code => "code",
            FileKind::Config => "config",
            FileKind::Log => "log",
            FileKind::Doc => "doc",
            FileKind::Data => "data",
            FileKind::Unknown => "unknown",
        }
    }
}

/// §5.3 文本路径的 sensitivity 事实（只作标注，不替代 §8 的隔离规则）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sensitivity {
    None,
    Secret,
    Personal,
}

impl Sensitivity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Sensitivity::None => "none",
            Sensitivity::Secret => "secret",
            Sensitivity::Personal => "personal",
        }
    }
}

/// §5.3 文本路径的 best_view 三档。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BestView {
    Full,
    Structure,
    Fragment,
}

impl BestView {
    pub fn as_str(&self) -> &'static str {
        match self {
            BestView::Full => "full",
            BestView::Structure => "structure",
            BestView::Fragment => "fragment",
        }
    }
}

/// decide 对文本路径的 L1 判定结果（§5.3）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextFacts {
    pub file_kind: FileKind,
    /// 被选中的 file_kind 类别概率（unknown 时是五类最大概率）。
    pub kind_confidence: f32,
    pub sensitivity: Sensitivity,
    /// 仅当本次判定了 best_view 才有值（只有 auto 需要它决定协商形态）；
    /// summary / structured 的输出形态已由 intent 定死，不问 best_view，这里为 None。
    pub best_view: Option<BestView>,
    /// 被选中的 best_view 类别概率（full 兜底时是三档最大概率）；未判定时为 None。
    pub view_confidence: Option<f32>,
}

/// §5.3 文本路径 L1 的判定问题表：file_kind 五类 + sensitivity 两问 + best_view 三档。
pub const TEXT_FILE_KIND_QUESTIONS: [(&str, FileKind); 5] = [
    ("这段文本是源代码吗？", FileKind::Code),
    ("这段文本是配置文件吗？", FileKind::Config),
    ("这段文本是日志吗？", FileKind::Log),
    ("这段文本是文档吗？", FileKind::Doc),
    (
        "这段文本是结构化数据（JSON、CSV、表格等）吗？",
        FileKind::Data,
    ),
];
pub const TEXT_SENSITIVITY_QUESTIONS: [(&str, Sensitivity); 2] = [
    ("这段文本包含密钥、口令或访问令牌吗？", Sensitivity::Secret),
    (
        "这段文本包含个人信息（姓名、证件号、联系方式等）吗？",
        Sensitivity::Personal,
    ),
];
pub const TEXT_BEST_VIEW_QUESTIONS: [(&str, BestView); 3] = [
    ("这段文本最适合完整通读吗？", BestView::Full),
    (
        "这段文本最适合按结构化视图（函数签名、键值树、错误时间线等）阅读吗？",
        BestView::Structure,
    ),
    ("这段文本最适合只取一个片段吗？", BestView::Fragment),
];

/// §5.3 / §6.4 按 intent 分级的问题集：file_kind 五问 + sensitivity 两问恒问；
/// best_view 三问只在 intent=auto 时问——summary / structured 的输出形态已由 intent
/// 定死，问 best_view 纯属白烧判定（decide 成本 ∝ 状态长度 × 问题数，附件 E.3）。
pub fn text_decide_questions_for(intent: Intent) -> Vec<String> {
    if matches!(intent, Intent::Raw | Intent::Visual) {
        return Vec::new();
    }
    let mut questions: Vec<String> = TEXT_FILE_KIND_QUESTIONS
        .iter()
        .map(|(question, _)| (*question).to_owned())
        .chain(
            TEXT_SENSITIVITY_QUESTIONS
                .iter()
                .map(|(question, _)| (*question).to_owned()),
        )
        .collect();
    if matches!(intent, Intent::Auto) {
        questions.extend(
            TEXT_BEST_VIEW_QUESTIONS
                .iter()
                .map(|(question, _)| (*question).to_owned()),
        );
    }
    questions
}

/// 全量十问（auto 口径）：file_kind 五问 → sensitivity 两问 → best_view 三问。
pub fn text_decide_questions() -> Vec<String> {
    text_decide_questions_for(Intent::Auto)
}

/// 归并文本路径 L1 答案（§5.3 的落地口径）：
/// - file_kind 取五类最大概率，< 0.5 记 unknown；
/// - sensitivity 安全优先：密钥 ≥ 0.5 取 secret，否则个人信息 ≥ 0.5 取 personal；
/// - best_view 仅在问过（三问都有答案）时取三档最大概率，< 0.5 兜底 full；未问则为 None。
pub fn merge_text_answers(
    answers: &std::collections::BTreeMap<String, crate::slots::DecideAnswer>,
) -> Option<TextFacts> {
    let mut best: Option<(FileKind, f64)> = None;
    for (question, kind) in TEXT_FILE_KIND_QUESTIONS {
        let p = answers.get(question)?.probability;
        if best.is_none_or(|(_, best_p)| p > best_p) {
            best = Some((kind, p));
        }
    }
    let (mut file_kind, kind_p) = best?;
    if kind_p < 0.5 {
        file_kind = FileKind::Unknown;
    }
    let secret = answers.get(TEXT_SENSITIVITY_QUESTIONS[0].0)?.probability;
    let personal = answers.get(TEXT_SENSITIVITY_QUESTIONS[1].0)?.probability;
    let sensitivity = if secret >= 0.5 {
        Sensitivity::Secret
    } else if personal >= 0.5 {
        Sensitivity::Personal
    } else {
        Sensitivity::None
    };
    let mut best_view: Option<(BestView, f64)> = None;
    let mut asked_view = true;
    for (question, view) in TEXT_BEST_VIEW_QUESTIONS {
        match answers.get(question) {
            Some(answer) => {
                if best_view.is_none_or(|(_, best_p)| answer.probability > best_p) {
                    best_view = Some((view, answer.probability));
                }
            }
            None => asked_view = false,
        }
    }
    let (best_view, view_confidence) = if asked_view {
        let (mut view, p) = best_view?;
        if p < 0.5 {
            view = BestView::Full;
        }
        (Some(view), Some(p as f32))
    } else {
        (None, None)
    };
    Some(TextFacts {
        file_kind,
        kind_confidence: kind_p as f32,
        sensitivity,
        best_view,
        view_confidence,
    })
}

/// §5.3 文本路径 L2 的结构化 schema：按 file_kind 固定，不生成自由 schema。
pub fn text_summarize_schema(kind: FileKind) -> Value {
    match kind {
        FileKind::Code => json!({
            "type": "object",
            "properties": {
                "signatures": { "type": "array", "items": { "type": "string" } },
                "todos": { "type": "array", "items": { "type": "string" } },
                "imports": { "type": "array", "items": { "type": "string" } }
            },
            "required": ["signatures", "todos", "imports"]
        }),
        FileKind::Config => json!({ "type": "object" }),
        FileKind::Log => json!({
            "type": "object",
            "properties": {
                "errors": { "type": "array", "items": { "type": "string" } },
                "timeline": { "type": "array", "items": { "type": "string" } }
            },
            "required": ["errors", "timeline"]
        }),
        FileKind::Doc | FileKind::Data | FileKind::Unknown => json!({
            "type": "object",
            "properties": {
                "summary": { "type": "string" },
                "keyPoints": { "type": "array", "items": { "type": "string" } }
            },
            "required": ["summary", "keyPoints"]
        }),
    }
}

/// 文本路径的协商模式（§5.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextMode {
    Raw,
    Summary,
    Structured,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextPlan {
    pub mode: TextMode,
    pub notices: Vec<Notice>,
}

/// §5.3 文本路径的 L1/L2 协商：raw 不触发；summary / structured 要 summarize；
/// auto 只在 bestView=structure 且 summarize 可用时结构化。summarize 缺失按 §3.8 退原文。
pub fn plan_text(intent: Intent, facts: Option<&TextFacts>, summarize_available: bool) -> TextPlan {
    let to_summary = |structured: bool| {
        if summarize_available {
            TextPlan {
                mode: if structured {
                    TextMode::Structured
                } else {
                    TextMode::Summary
                },
                notices: vec![],
            }
        } else {
            TextPlan {
                mode: TextMode::Raw,
                notices: vec![slot_unavailable("summarize", SlotFallback::Text)],
            }
        }
    };
    match intent {
        Intent::Raw | Intent::Visual => TextPlan {
            mode: TextMode::Raw,
            notices: vec![],
        },
        Intent::Summary => to_summary(false),
        Intent::Structured => to_summary(true),
        Intent::Auto => match facts {
            Some(f) if f.best_view == Some(BestView::Structure) => to_summary(true),
            _ => TextPlan {
                mode: TextMode::Raw,
                notices: vec![],
            },
        },
    }
}

fn slot_unavailable(slot: &str, fallback: SlotFallback) -> Notice {
    Notice::SlotUnavailable {
        slot: slot.to_owned(),
        fallback,
        reason: None,
    }
}

/// §10.1 前置判断与分流。upstream_vision 来自 _meta.toolgate/upstream.vision
/// （§18.1b，缺省 false）；vision_slot_available / facts 由调用方按绑定、能力矩阵与
/// decide 判定真实给出（§3.10），不得硬编码。
pub fn plan_image(
    intent: Intent,
    upstream_vision: bool,
    vision_slot_available: bool,
    facts: Option<&ImageFacts>,
) -> ImagePlan {
    let task = facts.map(|f| vision_task_for(f.kind));
    let need_pixels = facts.map(|f| need_pixels_gate(f.need_pixels));

    // §10.1:114-116：上游不收图 → 有 vision 槽位就强制本地理解，只返回文本。
    if !upstream_vision {
        return if vision_slot_available {
            let mut notices = Vec::new();
            if facts.is_none() {
                notices.push(slot_unavailable("decide", SlotFallback::Text));
            }
            // §10.1:167-169：判定「必须看原图」也只能退文本理解，用 DEGRADED 说明。
            if facts.is_some() && need_pixels == Some(NeedPixels::OriginalOnly) {
                notices.push(Notice::Degraded {
                    requested: OutputShape::Visual,
                    actual: DegradedActual::None,
                    reason: DegradeReason::UpstreamNoVision,
                    slot: Some("vision".to_owned()),
                });
            }
            ImagePlan {
                outcome: ImageOutcome::TextUnderstanding,
                task: Some(task.unwrap_or(VisionTask::Describe)),
                need_pixels: None,
                notices,
            }
        } else {
            ImagePlan {
                outcome: ImageOutcome::Metadata,
                task: None,
                need_pixels: None,
                notices: vec![slot_unavailable("vision", SlotFallback::Metadata)],
            }
        };
    }

    // 上游收图。
    match intent {
        // §5.3 协商表：raw / visual 要原图。
        Intent::Raw | Intent::Visual => ImagePlan {
            outcome: ImageOutcome::Original,
            task: None,
            need_pixels: None,
            notices: vec![],
        },
        // §5.3 协商表：structured / summary 要理解结果 → vision 可用时本地理解；
        // 不可用时按 §3.8 退压缩后原图并告知。decide 不可用时任务档退化 describe。
        Intent::Structured | Intent::Summary => {
            if vision_slot_available {
                ImagePlan {
                    outcome: ImageOutcome::TextUnderstanding,
                    task: Some(task.unwrap_or(VisionTask::Describe)),
                    need_pixels: None,
                    notices: if facts.is_none() {
                        vec![slot_unavailable("decide", SlotFallback::Text)]
                    } else {
                        vec![]
                    },
                }
            } else {
                ImagePlan {
                    outcome: ImageOutcome::Original,
                    task: None,
                    need_pixels: None,
                    notices: vec![slot_unavailable("vision", SlotFallback::Mechanical)],
                }
            }
        }
        // §10.1 的 need_pixels 三档：p ≥ 0.8 只给原图、p ≤ 0.2 只给结构化、
        // 中间区间两种都给（结构化在前，原图在后）。判定缺位时保守给原图。
        Intent::Auto => match (vision_slot_available, facts, need_pixels) {
            (true, Some(_), Some(NeedPixels::OriginalOnly)) => ImagePlan {
                outcome: ImageOutcome::Original,
                task: None,
                need_pixels,
                notices: vec![],
            },
            (true, Some(_), Some(NeedPixels::StructuredOnly)) => ImagePlan {
                outcome: ImageOutcome::TextUnderstanding,
                task: Some(task.unwrap_or(VisionTask::Describe)),
                need_pixels,
                notices: vec![],
            },
            (true, Some(_), Some(NeedPixels::Both)) => ImagePlan {
                outcome: ImageOutcome::Both,
                task: Some(task.unwrap_or(VisionTask::Describe)),
                need_pixels,
                notices: vec![],
            },
            // vision 可用但 need_pixels 判定缺位：保守给原图（信息最全），告知 decide 缺口。
            (true, _, _) => ImagePlan {
                outcome: ImageOutcome::Original,
                task: None,
                need_pixels: None,
                notices: vec![slot_unavailable("decide", SlotFallback::Mechanical)],
            },
            // vision 不可用：结构化拿不到，即使 need_pixels 判「要结构化」也只能给原图。
            (false, _, _) => ImagePlan {
                outcome: ImageOutcome::Original,
                task: None,
                need_pixels: None,
                notices: vec![slot_unavailable("vision", SlotFallback::Mechanical)],
            },
        },
    }
}

/// §10.1 / §10.3 的 need_pixels 三档。不要另设第二套阈值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NeedPixels {
    OriginalOnly,
    StructuredOnly,
    Both,
}

pub fn need_pixels_gate(p: f32) -> NeedPixels {
    if p >= 0.8 {
        NeedPixels::OriginalOnly
    } else if p <= 0.2 {
        NeedPixels::StructuredOnly
    } else {
        NeedPixels::Both
    }
}

impl NeedPixels {
    /// 机器可读形态：短枚举，供 meta 直接消费（比原始概率更贴近决策结果）。
    pub fn as_str(&self) -> &'static str {
        match self {
            NeedPixels::OriginalOnly => "original_only",
            NeedPixels::StructuredOnly => "structured_only",
            NeedPixels::Both => "both",
        }
    }
}

/// §5.3 图片路径的理解链 DAG：decide 判定后，OCR 与 grounding 两个 vision
/// 节点无数据依赖可并发提交（同一模型实例内仍按 §4.8 排队），最后 merge。
pub fn image_chain() -> Vec<ChainNode> {
    vec![
        ChainNode {
            id: 0,
            action: ChainAction::Recall,
            depends_on: vec![],
        },
        ChainNode {
            id: 1,
            action: ChainAction::Decide,
            depends_on: vec![0],
        },
        ChainNode {
            id: 2,
            action: ChainAction::Vision,
            depends_on: vec![1],
        },
        ChainNode {
            id: 3,
            action: ChainAction::Vision,
            depends_on: vec![1],
        },
        ChainNode {
            id: 4,
            action: ChainAction::Merge,
            depends_on: vec![2, 3],
        },
    ]
}

/// §8.11：模型可见路径默认工作区相对，工作区外不暴露绝对路径。
pub fn display_path(path: &Path, cwd: &Path) -> String {
    match path.strip_prefix(cwd) {
        Ok(rel) if !rel.as_os_str().is_empty() => format!("<workspace>/{}", rel.display()),
        _ => "<external>".to_owned(),
    }
}

/// §10.1:155 的 structuredContent 字段：kind / mime / size / path。
pub fn metadata_facts(
    mime: &str,
    size: u64,
    width: Option<u32>,
    height: Option<u32>,
    path: &str,
) -> Value {
    json!({
        "kind": "image",
        "mime": mime,
        "size": size,
        "width": width,
        "height": height,
        "path": path,
    })
}

/// §10.1:148-152 的可行动元信息文本：告知在前，事实居中，可选路径收尾。
pub fn metadata_text(
    lang: crate::locale::Language,
    notices: &[Notice],
    file_name: &str,
    mime: &str,
    size: u64,
    dims: Option<(u32, u32)>,
    path: &str,
) -> String {
    let dims = match dims {
        Some((w, h)) => format!(" · {w}x{h}"),
        None => String::new(),
    };
    crate::locale::tf(
        lang,
        "{notices}\n[图片文件] {file_name} · {mime}{dims} · {size}\n路径：{path}\n\n无法提供图片内容：vision 槽位未配置，上游模型也不接受图片输入。\n可选路径：\n- 若你能通过其他方式查看该文件，用上面的路径\n- 配置 vision 槽位后重试\n- 若只需要文件名与尺寸，以上信息已足够",
        &[
            ("notices", render_notices(notices)),
            ("file_name", file_name.to_owned()),
            ("mime", mime.to_owned()),
            ("dims", dims),
            ("size", format_size(size as usize)),
            ("path", path.to_owned()),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::understanding::{MAX_PARALLEL, MAX_STEPS, schedule, validate_chain};

    #[test]
    fn need_pixels_three_bands() {
        assert_eq!(need_pixels_gate(1.0), NeedPixels::OriginalOnly);
        assert_eq!(need_pixels_gate(0.8), NeedPixels::OriginalOnly);
        assert_eq!(need_pixels_gate(0.79), NeedPixels::Both);
        assert_eq!(need_pixels_gate(0.21), NeedPixels::Both);
        assert_eq!(need_pixels_gate(0.2), NeedPixels::StructuredOnly);
        assert_eq!(need_pixels_gate(0.0), NeedPixels::StructuredOnly);
    }

    #[test]
    fn upstream_no_vision_without_slot_returns_metadata() {
        let plan = plan_image(Intent::Auto, false, false, None);
        assert_eq!(plan.outcome, ImageOutcome::Metadata);
        assert_eq!(plan.notices.len(), 1);
        assert_eq!(plan.notices[0].code(), "SLOT_UNAVAILABLE");
        assert!(render_notices(&plan.notices).contains("fallback=metadata"));
    }

    #[test]
    fn upstream_no_vision_with_slot_returns_text_understanding() {
        let plan = plan_image(Intent::Raw, false, true, None);
        assert_eq!(plan.outcome, ImageOutcome::TextUnderstanding);
        assert_eq!(plan.task, Some(VisionTask::Describe));
        assert!(render_notices(&plan.notices).contains("slot=decide"));
        assert!(render_notices(&plan.notices).contains("fallback=text"));
    }

    #[test]
    fn upstream_vision_raw_and_visual_return_original_without_notice() {
        for intent in [Intent::Raw, Intent::Visual] {
            let plan = plan_image(intent, true, false, None);
            assert_eq!(plan.outcome, ImageOutcome::Original);
            assert!(plan.notices.is_empty());
        }
    }

    #[test]
    fn upstream_vision_structured_degrades_with_notice() {
        let plan = plan_image(Intent::Structured, true, false, None);
        assert_eq!(plan.outcome, ImageOutcome::Original);
        assert_eq!(plan.notices.len(), 1);
        assert!(render_notices(&plan.notices).contains("slot=vision"));
    }

    /// §5.3 协商表：structured / summary 要理解结果，vision 可用时走本地理解；
    /// 按 kind 分流缺 decide，任务档退化为 describe 并如实告知。
    #[test]
    fn upstream_vision_structured_with_slot_returns_text_understanding() {
        for intent in [Intent::Structured, Intent::Summary] {
            let plan = plan_image(intent, true, true, None);
            assert_eq!(plan.outcome, ImageOutcome::TextUnderstanding);
            assert_eq!(plan.task, Some(VisionTask::Describe));
            let rendered = render_notices(&plan.notices);
            assert!(rendered.contains("slot=decide"), "{rendered}");
            assert!(rendered.contains("fallback=text"), "{rendered}");
        }
    }

    /// §10.1：auto 的 need_pixels 判定依赖 decide；decide 不可用时保守给原图，
    /// 并告知缺的是 decide（不是谎报 vision 不可用）。
    #[test]
    fn upstream_vision_auto_with_slot_reports_decide_unavailable() {
        let plan = plan_image(Intent::Auto, true, true, None);
        assert_eq!(plan.outcome, ImageOutcome::Original);
        let rendered = render_notices(&plan.notices);
        assert!(rendered.contains("slot=decide"), "{rendered}");
        assert!(rendered.contains("fallback=mechanical"), "{rendered}");
    }

    fn answers_with(
        kind_scores: &[(&str, f64)],
        has_text: f64,
        need_pixels: f64,
    ) -> std::collections::BTreeMap<String, crate::slots::DecideAnswer> {
        let mut answers = std::collections::BTreeMap::new();
        for (question, p) in kind_scores {
            answers.insert(
                (*question).to_owned(),
                crate::slots::DecideAnswer {
                    probability: *p,
                    span: None,
                },
            );
        }
        answers.insert(
            HAS_TEXT_QUESTION.to_owned(),
            crate::slots::DecideAnswer {
                probability: has_text,
                span: None,
            },
        );
        answers.insert(
            NEED_PIXELS_QUESTION.to_owned(),
            crate::slots::DecideAnswer {
                probability: need_pixels,
                span: None,
            },
        );
        answers
    }

    /// §5.3 图片路径固定七问：五类 + has_text + need_pixels。
    #[test]
    fn image_decide_questions_cover_seven_fixed_questions() {
        let questions = image_decide_questions();
        assert_eq!(questions.len(), 7);
        for (index, (question, _)) in IMAGE_KIND_QUESTIONS.iter().enumerate() {
            assert_eq!(questions[index], *question);
        }
        assert_eq!(questions[5], HAS_TEXT_QUESTION);
        assert_eq!(questions[6], NEED_PIXELS_QUESTION);
    }

    /// §5.3 归并：image_kind 取最大概率，最大者 < 0.5 记 unknown。
    #[test]
    fn image_answers_merge_by_max_kind_and_half_threshold() {
        let answers = answers_with(
            &[
                ("这张图是 UI 界面截图吗？", 0.91),
                ("这张图是文档或表单的照片吗？", 0.12),
                ("这张图是图表（折线、柱状、饼图等）吗？", 0.05),
                ("这张图是普通照片吗？", 0.02),
                ("这张图是代码截图吗？", 0.31),
            ],
            0.88,
            0.93,
        );
        let facts = merge_image_answers(&answers).expect("答案覆盖全部问题");
        assert_eq!(facts.kind, ImageKind::UiScreenshot);
        assert!((facts.kind_confidence - 0.91).abs() < 1e-6);
        assert!(facts.has_text);
        assert_eq!(
            need_pixels_gate(facts.need_pixels),
            NeedPixels::OriginalOnly
        );
    }

    #[test]
    fn image_answers_below_half_are_unknown() {
        let answers = answers_with(
            &[
                ("这张图是 UI 界面截图吗？", 0.21),
                ("这张图是文档或表单的照片吗？", 0.32),
                ("这张图是图表（折线、柱状、饼图等）吗？", 0.11),
                ("这张图是普通照片吗？", 0.44),
                ("这张图是代码截图吗？", 0.09),
            ],
            0.4,
            0.5,
        );
        let facts = merge_image_answers(&answers).expect("答案覆盖全部问题");
        assert_eq!(facts.kind, ImageKind::Unknown);
        assert!((facts.kind_confidence - 0.44).abs() < 1e-6);
        assert!(!facts.has_text);
        assert_eq!(need_pixels_gate(facts.need_pixels), NeedPixels::Both);
    }

    /// §5.3 / §10.1 的 kind → vision 任务档。
    #[test]
    fn vision_task_mapping_follows_kind() {
        assert_eq!(vision_task_for(ImageKind::UiScreenshot), VisionTask::Ground);
        assert_eq!(vision_task_for(ImageKind::Document), VisionTask::Ocr);
        assert_eq!(vision_task_for(ImageKind::CodeScreenshot), VisionTask::Ocr);
        assert_eq!(vision_task_for(ImageKind::Chart), VisionTask::Describe);
        assert_eq!(vision_task_for(ImageKind::Photo), VisionTask::Describe);
        assert_eq!(vision_task_for(ImageKind::Unknown), VisionTask::Describe);
    }

    /// §10.1 三档：p≥0.8 原图、p≤0.2 结构化、中间区间两种都给。
    #[test]
    fn auto_need_pixels_three_bands_pick_outcomes() {
        let ui = ImageFacts {
            kind: ImageKind::UiScreenshot,
            kind_confidence: 0.9,
            has_text: true,
            need_pixels: 0.9,
        };
        let plan = plan_image(Intent::Auto, true, true, Some(&ui));
        assert_eq!(plan.outcome, ImageOutcome::Original);
        assert_eq!(plan.need_pixels, Some(NeedPixels::OriginalOnly));

        let doc = ImageFacts {
            kind: ImageKind::Document,
            kind_confidence: 0.85,
            has_text: true,
            need_pixels: 0.1,
        };
        let plan = plan_image(Intent::Auto, true, true, Some(&doc));
        assert_eq!(plan.outcome, ImageOutcome::TextUnderstanding);
        assert_eq!(plan.task, Some(VisionTask::Ocr));
        assert_eq!(plan.need_pixels, Some(NeedPixels::StructuredOnly));

        let chart = ImageFacts {
            kind: ImageKind::Chart,
            kind_confidence: 0.8,
            has_text: true,
            need_pixels: 0.5,
        };
        let plan = plan_image(Intent::Auto, true, true, Some(&chart));
        assert_eq!(plan.outcome, ImageOutcome::Both);
        assert_eq!(plan.task, Some(VisionTask::Describe));
        assert_eq!(plan.need_pixels, Some(NeedPixels::Both));
    }

    /// §10.1:167-169：上游不收图且判定「必须看原图」→ 文本理解 + DEGRADED。
    #[test]
    fn upstream_no_vision_and_original_only_degrades_with_degraded_notice() {
        let facts = ImageFacts {
            kind: ImageKind::Document,
            kind_confidence: 0.85,
            has_text: true,
            need_pixels: 0.95,
        };
        let plan = plan_image(Intent::Auto, false, true, Some(&facts));
        assert_eq!(plan.outcome, ImageOutcome::TextUnderstanding);
        assert_eq!(plan.task, Some(VisionTask::Ocr));
        let rendered = render_notices(&plan.notices);
        assert!(rendered.contains("DEGRADED"), "{rendered}");
        assert!(rendered.contains("reason=upstream_no_vision"), "{rendered}");
    }

    /// kind 判定可用时，structured 按 kind 选任务档且无需 decide 缺口告知。
    #[test]
    fn structured_with_facts_uses_kind_task_without_decide_notice() {
        let facts = ImageFacts {
            kind: ImageKind::CodeScreenshot,
            kind_confidence: 0.88,
            has_text: true,
            need_pixels: 0.1,
        };
        let plan = plan_image(Intent::Structured, true, true, Some(&facts));
        assert_eq!(plan.outcome, ImageOutcome::TextUnderstanding);
        assert_eq!(plan.task, Some(VisionTask::Ocr));
        assert!(plan.notices.is_empty());
    }

    fn text_answers(
        kind_scores: &[(FileKind, f64)],
        secret: f64,
        personal: f64,
        view_scores: &[(BestView, f64)],
    ) -> std::collections::BTreeMap<String, crate::slots::DecideAnswer> {
        let mut answers = std::collections::BTreeMap::new();
        for (kind, p) in kind_scores {
            let question = TEXT_FILE_KIND_QUESTIONS
                .iter()
                .find(|(_, candidate)| candidate == kind)
                .expect("kind 在问题表中")
                .0;
            answers.insert(
                question.to_owned(),
                crate::slots::DecideAnswer {
                    probability: *p,
                    span: None,
                },
            );
        }
        answers.insert(
            TEXT_SENSITIVITY_QUESTIONS[0].0.to_owned(),
            crate::slots::DecideAnswer {
                probability: secret,
                span: None,
            },
        );
        answers.insert(
            TEXT_SENSITIVITY_QUESTIONS[1].0.to_owned(),
            crate::slots::DecideAnswer {
                probability: personal,
                span: None,
            },
        );
        for (view, p) in view_scores {
            let question = TEXT_BEST_VIEW_QUESTIONS
                .iter()
                .find(|(_, candidate)| candidate == view)
                .expect("view 在问题表中")
                .0;
            answers.insert(
                question.to_owned(),
                crate::slots::DecideAnswer {
                    probability: *p,
                    span: None,
                },
            );
        }
        answers
    }

    /// §5.3 文本路径固定十问：file_kind 5 + sensitivity 2 + best_view 3。
    #[test]
    fn text_decide_questions_cover_ten_fixed_questions() {
        let questions = text_decide_questions();
        assert_eq!(questions.len(), 10);
        assert_eq!(questions[0], TEXT_FILE_KIND_QUESTIONS[0].0);
        assert_eq!(questions[4], TEXT_FILE_KIND_QUESTIONS[4].0);
        assert_eq!(questions[5], TEXT_SENSITIVITY_QUESTIONS[0].0);
        assert_eq!(questions[7], TEXT_BEST_VIEW_QUESTIONS[0].0);
        assert_eq!(questions[9], TEXT_BEST_VIEW_QUESTIONS[2].0);
    }

    /// §5.3 / §6.4 分级：summary / structured 不判定 best_view（少问 3 题）。
    #[test]
    fn text_decide_questions_summary_and_structured_skip_best_view() {
        for intent in [Intent::Summary, Intent::Structured] {
            let questions = text_decide_questions_for(intent);
            assert_eq!(questions.len(), 7);
            assert_eq!(questions[0], TEXT_FILE_KIND_QUESTIONS[0].0);
            assert_eq!(questions[6], TEXT_SENSITIVITY_QUESTIONS[1].0);
            assert!(!questions.iter().any(|q| q == TEXT_BEST_VIEW_QUESTIONS[0].0));
        }
        assert_eq!(text_decide_questions_for(Intent::Auto).len(), 10);
        assert!(text_decide_questions_for(Intent::Raw).is_empty());
    }

    /// §5.3 未问 best_view 时归并为 None，不假装 full（§4.6「不假装成功」）。
    #[test]
    fn text_answers_merge_without_best_view_keeps_none() {
        let answers = text_answers(
            &[
                (FileKind::Log, 0.9),
                (FileKind::Code, 0.05),
                (FileKind::Config, 0.02),
                (FileKind::Doc, 0.02),
                (FileKind::Data, 0.01),
            ],
            0.1,
            0.1,
            &[],
        );
        let facts = merge_text_answers(&answers).expect("7 问答案足够归并");
        assert_eq!(facts.file_kind, FileKind::Log);
        assert_eq!(facts.best_view, None);
        assert_eq!(facts.view_confidence, None);
    }

    /// §5.3 归并：file_kind 取最大、sensitivity 安全优先、best_view 取最大。
    #[test]
    fn text_answers_merge_kind_sensitivity_and_view() {
        let answers = text_answers(
            &[
                (FileKind::Code, 0.9),
                (FileKind::Config, 0.05),
                (FileKind::Log, 0.02),
                (FileKind::Doc, 0.01),
                (FileKind::Data, 0.02),
            ],
            0.2,
            0.8,
            &[
                (BestView::Full, 0.05),
                (BestView::Structure, 0.85),
                (BestView::Fragment, 0.10),
            ],
        );
        let facts = merge_text_answers(&answers).expect("答案覆盖全部问题");
        assert_eq!(facts.file_kind, FileKind::Code);
        assert!((facts.kind_confidence - 0.9).abs() < 1e-6);
        assert_eq!(facts.sensitivity, Sensitivity::Personal);
        assert_eq!(facts.best_view, Some(BestView::Structure));
        assert!((facts.view_confidence.unwrap() - 0.85).abs() < 1e-6);
    }

    #[test]
    fn text_sensitivity_secret_wins_when_both_high() {
        let answers = text_answers(
            &[
                (FileKind::Doc, 0.6),
                (FileKind::Code, 0.1),
                (FileKind::Config, 0.1),
                (FileKind::Log, 0.1),
                (FileKind::Data, 0.1),
            ],
            0.7,
            0.9,
            &[
                (BestView::Full, 0.8),
                (BestView::Structure, 0.1),
                (BestView::Fragment, 0.1),
            ],
        );
        let facts = merge_text_answers(&answers).unwrap();
        assert_eq!(facts.sensitivity, Sensitivity::Secret);
    }

    /// 三档 best_view 最大者 < 0.5 → full 兜底；unknown 同理。
    #[test]
    fn text_best_view_below_half_defaults_full() {
        let answers = text_answers(
            &[
                (FileKind::Code, 0.2),
                (FileKind::Config, 0.1),
                (FileKind::Log, 0.1),
                (FileKind::Doc, 0.1),
                (FileKind::Data, 0.1),
            ],
            0.1,
            0.1,
            &[
                (BestView::Full, 0.3),
                (BestView::Structure, 0.4),
                (BestView::Fragment, 0.2),
            ],
        );
        let facts = merge_text_answers(&answers).unwrap();
        assert_eq!(facts.file_kind, FileKind::Unknown);
        assert_eq!(facts.best_view, Some(BestView::Full));
        assert!((facts.view_confidence.unwrap() - 0.4).abs() < 1e-6);
    }

    /// §5.3 文本路径 L2 schema 按 file_kind 固定。
    #[test]
    fn text_summarize_schema_per_kind() {
        let code = text_summarize_schema(FileKind::Code);
        assert!(code["properties"]["signatures"].is_object());
        assert!(code["properties"]["todos"].is_object());
        assert!(
            code["required"]
                .as_array()
                .unwrap()
                .contains(&json!("imports"))
        );
        let log = text_summarize_schema(FileKind::Log);
        assert!(log["properties"]["errors"].is_object());
        assert!(log["properties"]["timeline"].is_object());
        let doc = text_summarize_schema(FileKind::Doc);
        assert!(doc["properties"]["summary"].is_object());
        assert!(doc["properties"]["keyPoints"].is_object());
    }

    /// §5.3 文本路径的协商矩阵：raw 不触发；summary/structured 要 summarize；
    /// auto 只在 bestView=structure 且 summarize 可用时结构化。
    #[test]
    fn plan_text_modes_matrix() {
        let structure = TextFacts {
            file_kind: FileKind::Code,
            kind_confidence: 0.9,
            sensitivity: Sensitivity::None,
            best_view: Some(BestView::Structure),
            view_confidence: Some(0.9),
        };
        let full = TextFacts {
            best_view: Some(BestView::Full),
            ..structure
        };
        assert_eq!(
            plan_text(Intent::Raw, Some(&structure), true).mode,
            TextMode::Raw
        );
        assert_eq!(
            plan_text(Intent::Summary, None, true).mode,
            TextMode::Summary
        );
        assert_eq!(
            plan_text(Intent::Structured, None, true).mode,
            TextMode::Structured
        );
        let fallback = plan_text(Intent::Summary, None, false);
        assert_eq!(fallback.mode, TextMode::Raw);
        assert!(render_notices(&fallback.notices).contains("slot=summarize"));
        assert_eq!(
            plan_text(Intent::Auto, Some(&structure), true).mode,
            TextMode::Structured
        );
        assert_eq!(
            plan_text(Intent::Auto, Some(&full), true).mode,
            TextMode::Raw
        );
        assert_eq!(plan_text(Intent::Auto, None, true).mode, TextMode::Raw);
    }

    #[test]
    fn image_chain_is_bounded_and_has_parallel_vision_layer() {
        let chain = image_chain();
        assert_eq!(chain.len(), MAX_STEPS);
        validate_chain(&chain, MAX_PARALLEL).unwrap();
        let layers = schedule(&chain, MAX_PARALLEL);
        assert_eq!(layers, schedule(&chain, MAX_PARALLEL));
        assert_eq!(layers[2], vec![2, 3]);
        assert!(layers.iter().all(|l| l.len() <= MAX_PARALLEL));
    }

    #[test]
    fn metadata_facts_shape() {
        let facts = metadata_facts(
            "image/png",
            840_000,
            Some(1920),
            Some(1080),
            "<workspace>/a.png",
        );
        assert_eq!(facts["kind"], json!("image"));
        assert_eq!(facts["mime"], json!("image/png"));
        assert_eq!(facts["size"], json!(840_000));
        assert_eq!(facts["width"], json!(1920));
        assert_eq!(facts["path"], json!("<workspace>/a.png"));
    }

    #[test]
    fn display_path_is_workspace_relative() {
        let cwd = Path::new("/Users/me/project");
        assert_eq!(
            display_path(Path::new("/Users/me/project/src/a.png"), cwd),
            "<workspace>/src/a.png"
        );
        assert_eq!(display_path(Path::new("/tmp/x.png"), cwd), "<external>");
    }

    #[test]
    fn metadata_text_is_actionable() {
        let notices = vec![slot_unavailable("vision", SlotFallback::Metadata)];
        let text = metadata_text(
            crate::locale::Language::Zh,
            &notices,
            "screenshot.png",
            "image/png",
            840_000,
            Some((1920, 1080)),
            "<workspace>/screenshot.png",
        );
        assert!(text.starts_with("[SLOT_UNAVAILABLE]"));
        assert!(text.contains("[图片文件] screenshot.png"));
        assert!(text.contains("1920x1080"));
        assert!(text.contains("<workspace>/screenshot.png"));
        assert!(text.contains("配置 vision 槽位后重试"));
        assert!(!text.contains("/Users/"));
    }
}
