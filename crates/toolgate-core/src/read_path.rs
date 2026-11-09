//! read 图片路径（§5.3 / §10.1 / §10.3 / §3.8）。
//!
//! 纯逻辑：前置判断（上游是否收 image block）→ 分流 → 槽位全空时的降级告知。
//! 不含模型后端（可选层，§19.6）；decide / vision 槽位当前不可用，
//! 因此按 §3.8 退到压缩后原图或文件元信息，并按 §7.3 显式告知。

use std::path::Path;

use serde_json::{Value, json};

use crate::decision::Intent;
use crate::notice::{Notice, SlotFallback, render_notices};
use crate::truncate::format_size;
use crate::understanding::{ChainAction, ChainNode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageOutcome {
    /// 压缩后原图（v1 压缩流水线）。
    Original,
    /// 文件元信息 + 路径（槽位全空且上游不收图，§10.1:143）。
    Metadata,
    /// 上游不收图但 vision 槽位可用：强制本地理解，只返回文本。
    TextUnderstanding,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImagePlan {
    pub outcome: ImageOutcome,
    pub notices: Vec<Notice>,
}

fn slot_unavailable(slot: &str, fallback: SlotFallback) -> Notice {
    Notice::SlotUnavailable {
        slot: slot.to_owned(),
        fallback,
    }
}

/// §10.1 前置判断与分流。upstream_vision 来自 _meta.toolgate/upstream.vision
/// （§18.1b，缺省 false）；vision_slot_available 本阶段恒 false，但分支先落地。
pub fn plan_image(intent: Intent, upstream_vision: bool, vision_slot_available: bool) -> ImagePlan {
    if !upstream_vision {
        return if vision_slot_available {
            ImagePlan {
                outcome: ImageOutcome::TextUnderstanding,
                notices: vec![slot_unavailable("decide", SlotFallback::Text)],
            }
        } else {
            ImagePlan {
                outcome: ImageOutcome::Metadata,
                notices: vec![slot_unavailable("vision", SlotFallback::Metadata)],
            }
        };
    }

    // 上游收图：decide / vision 槽位不可用 → 按 §3.8 走压缩后原图。
    // structured / auto / summary 请求理解结果但槽位缺失，附 SLOT_UNAVAILABLE 告知。
    match intent {
        Intent::Raw | Intent::Visual => ImagePlan {
            outcome: ImageOutcome::Original,
            notices: vec![],
        },
        Intent::Structured | Intent::Summary | Intent::Auto => ImagePlan {
            outcome: ImageOutcome::Original,
            notices: vec![slot_unavailable("vision", SlotFallback::Mechanical)],
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
    notices: &[Notice],
    file_name: &str,
    mime: &str,
    size: u64,
    width: Option<u32>,
    height: Option<u32>,
    path: &str,
) -> String {
    let dims = match (width, height) {
        (Some(w), Some(h)) => format!(" · {w}x{h}"),
        _ => String::new(),
    };
    format!(
        "{}\n[图片文件] {file_name} · {mime}{dims} · {}\n路径：{path}\n\n\
         无法提供图片内容：vision 槽位未配置，上游模型也不接受图片输入。\n\
         可选路径：\n\
         - 若你能通过其他方式查看该文件，用上面的路径\n\
         - 配置 vision 槽位后重试\n\
         - 若只需要文件名与尺寸，以上信息已足够",
        render_notices(notices),
        format_size(size as usize),
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
            &notices,
            "screenshot.png",
            "image/png",
            840_000,
            Some(1920),
            Some(1080),
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
