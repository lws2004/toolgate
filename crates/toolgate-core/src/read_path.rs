//! read 图片路径（§5.3 / §10.1 / §10.3 / §3.8）。
//!
//! 纯逻辑：前置判断（上游是否收 image block）→ 意图分流 → 降级告知。
//! 不含模型后端（可选层，§19.6）：vision 槽位真实可用性由调用方传入，
//! 不可用时按 §3.8 退到压缩后原图或文件元信息，并按 §7.3 显式告知。

use std::path::Path;

use serde_json::{Value, json};

use crate::decision::Intent;
use crate::notice::{Notice, SlotFallback, render_notices};
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
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImagePlan {
    pub outcome: ImageOutcome,
    /// TextUnderstanding 时交给 vision 槽位的任务档（§3.2 的 task）。
    /// 按 image_kind 分流需要 decide 判定；decide 未接入时退化为 describe。
    pub task: Option<VisionTask>,
    pub notices: Vec<Notice>,
}

fn slot_unavailable(slot: &str, fallback: SlotFallback) -> Notice {
    Notice::SlotUnavailable {
        slot: slot.to_owned(),
        fallback,
    }
}

/// §10.1 前置判断与分流。upstream_vision 来自 _meta.toolgate/upstream.vision
/// （§18.1b，缺省 false）；vision_slot_available 由调用方按绑定 + 能力矩阵真实判定
/// （§3.10），不得硬编码。
pub fn plan_image(intent: Intent, upstream_vision: bool, vision_slot_available: bool) -> ImagePlan {
    // §10.1:114-116：上游不收图 → 有 vision 槽位就强制本地理解，只返回文本。
    // decide 未接入时拿不到 image_kind / need_pixels，按 kind 分流退化为 describe，
    // 用 SLOT_UNAVAILABLE(slot=decide, fallback=text) 如实告知。
    if !upstream_vision {
        return if vision_slot_available {
            ImagePlan {
                outcome: ImageOutcome::TextUnderstanding,
                task: Some(VisionTask::Describe),
                notices: vec![slot_unavailable("decide", SlotFallback::Text)],
            }
        } else {
            ImagePlan {
                outcome: ImageOutcome::Metadata,
                task: None,
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
            notices: vec![],
        },
        // §5.3 协商表：structured / summary 要理解结果 → vision 可用时本地理解；
        // 不可用时按 §3.8 退压缩后原图并告知。按 kind 分流仍缺 decide，
        // 因此任务档先退化 describe，并如实告知 decide 不可用。
        Intent::Structured | Intent::Summary => {
            if vision_slot_available {
                ImagePlan {
                    outcome: ImageOutcome::TextUnderstanding,
                    task: Some(VisionTask::Describe),
                    notices: vec![slot_unavailable("decide", SlotFallback::Text)],
                }
            } else {
                ImagePlan {
                    outcome: ImageOutcome::Original,
                    task: None,
                    notices: vec![slot_unavailable("vision", SlotFallback::Mechanical)],
                }
            }
        }
        // §10.1 的 need_pixels 三档依赖 decide 判定。decide 未接入时无法判断
        // 「要不要原图」，保守给原图（信息最全），并按实际情况告知缺的是哪个槽位。
        Intent::Auto => ImagePlan {
            outcome: ImageOutcome::Original,
            task: None,
            notices: vec![if vision_slot_available {
                slot_unavailable("decide", SlotFallback::Mechanical)
            } else {
                slot_unavailable("vision", SlotFallback::Mechanical)
            }],
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
        let plan = plan_image(Intent::Auto, false, false);
        assert_eq!(plan.outcome, ImageOutcome::Metadata);
        assert_eq!(plan.notices.len(), 1);
        assert_eq!(plan.notices[0].code(), "SLOT_UNAVAILABLE");
        assert!(render_notices(&plan.notices).contains("fallback=metadata"));
    }

    #[test]
    fn upstream_no_vision_with_slot_returns_text_understanding() {
        let plan = plan_image(Intent::Raw, false, true);
        assert_eq!(plan.outcome, ImageOutcome::TextUnderstanding);
        assert_eq!(plan.task, Some(VisionTask::Describe));
        assert!(render_notices(&plan.notices).contains("slot=decide"));
        assert!(render_notices(&plan.notices).contains("fallback=text"));
    }

    #[test]
    fn upstream_vision_raw_and_visual_return_original_without_notice() {
        for intent in [Intent::Raw, Intent::Visual] {
            let plan = plan_image(intent, true, false);
            assert_eq!(plan.outcome, ImageOutcome::Original);
            assert!(plan.notices.is_empty());
        }
    }

    #[test]
    fn upstream_vision_structured_degrades_with_notice() {
        let plan = plan_image(Intent::Structured, true, false);
        assert_eq!(plan.outcome, ImageOutcome::Original);
        assert_eq!(plan.notices.len(), 1);
        assert!(render_notices(&plan.notices).contains("slot=vision"));
    }

    /// §5.3 协商表：structured / summary 要理解结果，vision 可用时走本地理解；
    /// 按 kind 分流缺 decide，任务档退化为 describe 并如实告知。
    #[test]
    fn upstream_vision_structured_with_slot_returns_text_understanding() {
        for intent in [Intent::Structured, Intent::Summary] {
            let plan = plan_image(intent, true, true);
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
        let plan = plan_image(Intent::Auto, true, true);
        assert_eq!(plan.outcome, ImageOutcome::Original);
        let rendered = render_notices(&plan.notices);
        assert!(rendered.contains("slot=decide"), "{rendered}");
        assert!(rendered.contains("fallback=mechanical"), "{rendered}");
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
