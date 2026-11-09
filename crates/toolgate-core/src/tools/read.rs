//! read：带行号与锚点的读取器。
//!
//! 治理规则与 pi 一致：超过 2000 行或 50KB 时头部截断，并提示用
//! offset 续读；**不落盘**——文件本身就在磁盘上，指针没有意义。

use std::path::Path;

use serde_json::{Value, json};

use super::{
    Content, ToolContext, ToolResponse, ToolSpec, optional_bool, optional_usize, required_str,
    round2,
};
use crate::ToolError;
use crate::compat::{CompatConfig, resolve_intent};
use crate::decision::Intent;
use crate::hashline::render_anchored;
use crate::locale::{tf, tformat};
use crate::media;
use crate::media::image_pipeline;
use crate::notice::Notice;
use crate::notice::budgeted_notices;
use crate::read_path::{
    FileKind, ImageFacts, ImageOutcome, ImagePlan, TextFacts, TextMode, TextPlan, display_path,
    image_decide_questions, merge_image_answers, merge_text_answers, metadata_facts, metadata_text,
    plan_image, plan_text, text_decide_questions, text_summarize_schema,
};
use crate::slots::{DecideInput, SlotKind, SummarizeInput, VisionInput, VisionTask};
use crate::truncate::{TruncationOptions, format_size, truncate_head};

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: "read",
        description: "能做什么：读取文本文件（带行号与可选行锚点）或图片；超过 2000 行 / 50KB 头部截断并用 offset 续读；图片按压缩流水线返回或转译。 \
                      不能做什么：不解析网页 JS（fetch.provider 默认 none）；不跟随符号链接；文本路径的 intent 只影响协商层标注。 \
                      什么时候用别的工具：按内容查找用 search；局部修改用 edit；整段重写用 write。",
        description_en: "Can: read text files (line numbers / optional anchors) or images; head-truncates past 2000 lines / 50KB, resume with offset; images go through the compression pipeline. Cannot: run page JS (fetch.provider defaults to none), follow symlinks; text-path intent only annotates negotiation. Use instead: search to find content, edit for local changes, write for a full rewrite.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "文件路径，相对路径按工作目录解析"
                },
                "offset": {
                    "type": "integer",
                    "description": "起始行号，1 起始。默认 1",
                    "minimum": 1
                },
                "limit": {
                    "type": "integer",
                    "description": "最多读取的行数。默认读到文件末尾",
                    "minimum": 1
                },
                "anchor": {
                    "type": "boolean",
                    "description": "是否给每行附加 行号#短哈希 锚点，供 edit 的锚点模式使用。\
                                    大文件反复编辑时开启可省 token，其余场景建议关闭。默认 false"
                },
                "intent": {
                    "type": "string",
                    "enum": ["raw", "summary", "structured", "visual", "auto"],
                    "description": "图片协商：visual 给压缩后原图 / structured 给元素列表或 OCR 文本 / auto 按 need_pixels 判定 / raw 与 summary 按 §5.3 分流。文本路径 intent 暂只影响协商层标注"
                }
            },
            "required": ["path"]
        }),
    }
}

pub async fn run(args: &Value, ctx: &ToolContext) -> Result<ToolResponse, ToolError> {
    let lang = ctx.language;
    let raw_path = required_str(args, "path")?;
    let path = ctx.resolve(raw_path);

    let meta = tokio::fs::metadata(&path).await.map_err(|e| {
        ToolError::failed_with(
            crate::error::ErrorCode::PathNotFound,
            format!("无法访问 {}：{e}", path.display()),
        )
    })?;

    if meta.is_dir() {
        return Err(ToolError::failed(format!(
            "{} 是目录。用 search 的 files 模式列目录内容。",
            path.display()
        )));
    }

    // 图片分支：§10.1 前置判断 → 协商 → 槽位全空时给可行动的元信息。
    if let Some(mime) = media::detect_image_mime(&path).await? {
        let intent =
            resolve_intent(args, &CompatConfig::default()).map_err(ToolError::bad_argument)?;
        // §3.10：槽位可用性按绑定 + 能力矩阵真实判定，不硬编码（§7.3 状态要真实）。
        let vision_available = ctx.slm.as_ref().is_some_and(|client| {
            client.is_bound(SlotKind::Vision)
                && client
                    .source_for_slot(SlotKind::Vision)
                    .map(|source| {
                        crate::capability::source_slot_capability(source, SlotKind::Vision).grade
                            != crate::capability::CapabilityGrade::Unavailable
                    })
                    .unwrap_or(true)
        });
        let decide_available = ctx.slm.as_ref().is_some_and(|client| {
            client.is_bound(SlotKind::Decide)
                && client
                    .source_for_slot(SlotKind::Decide)
                    .map(|source| {
                        // 图片判定需要多模态 decide：systemone 的 images 可用；
                        // 通用 LLM 的 json 适配器不接收 images（§3.10），不能用于这里。
                        crate::capability::source_slot_capability(source, SlotKind::Decide).grade
                            == crate::capability::CapabilityGrade::Available
                    })
                    .unwrap_or(true)
        });
        // §5.3 图片路径 L1：auto / structured / summary 需要 image_kind 与 need_pixels。
        let ruling = if decide_available
            && matches!(intent, Intent::Auto | Intent::Structured | Intent::Summary)
        {
            image_ruling(&path, &mime, &meta, ctx, intent).await
        } else {
            None
        };
        let plan = plan_image(
            intent,
            ctx.upstream_vision,
            vision_available,
            ruling.as_ref().map(|r| &r.facts),
        );
        return image_response(&path, &mime, &meta, &plan, ctx, ruling.as_ref()).await;
    }

    // §22：按 intent 的确定性预判决定要不要预热；缓存可丢弃，
    // 命中只省 IO 与解码，不改变任何返回内容（§22.6）。
    let intent = resolve_intent(args, &CompatConfig::default()).map_err(ToolError::bad_argument)?;
    let warm_key = crate::warm::FileKey::of(meta.modified().ok(), meta.len());
    let warm_enabled = crate::warm::predict("read", intent) == crate::warm::PreheatAction::Full
        && crate::warm::should_warm(meta.len());
    let cached = if warm_enabled {
        ctx.warm.get(&path, warm_key)
    } else {
        None
    };

    let (bytes, encoding_name, text, had_decode_errors) = if let Some(w) = cached {
        (w.bytes, w.encoding_name, w.text, w.had_decode_errors)
    } else {
        let bytes = tokio::fs::read(&path).await?;
        let encoding = crate::encoding::FileEncoding::detect(&bytes);
        let (text, had) = encoding.decode(&bytes);
        if warm_enabled {
            ctx.warm.put(
                path.clone(),
                crate::warm::WarmText {
                    key: warm_key,
                    bytes: bytes.clone(),
                    text: text.clone(),
                    encoding_name: encoding.name(),
                    had_decode_errors: had,
                },
            );
        }
        (bytes, encoding.name(), text, had)
    };

    // 先认编码：UTF-16 文件天然含 NUL 字节，不能按「有 NUL 即二进制」拦下。
    let is_wide = encoding_name.starts_with("UTF-16");
    if !is_wide && media::looks_binary(&bytes) {
        return Err(ToolError::failed(format!(
            "{} 是二进制文件（{} 字节），不能按文本读取。若需处理请用 bash 调用专用工具。",
            path.display(),
            format_size(bytes.len())
        )));
    }

    // had_decode_errors 为真说明有字节无法在该编码下解释，
    // 此时显示内容里的替换字符不是原文，必须告知调用方。
    let total_lines = text.lines().count();

    // §5.3 文本路径：L1 判定 + L2 协商；raw 不触发判定（不白烧一次模型调用）。
    let summarize_available = ctx.slm.as_ref().is_some_and(|client| {
        client.is_bound(SlotKind::Summarize)
            && client
                .source_for_slot(SlotKind::Summarize)
                .map(|source| {
                    crate::capability::source_slot_capability(source, SlotKind::Summarize).grade
                        != crate::capability::CapabilityGrade::Unavailable
                })
                .unwrap_or(true)
    });
    // 文本判定不要求多模态：systemone 的 native 与通用 LLM 的 json 适配器都可用。
    let text_decide_available = ctx.slm.as_ref().is_some_and(|client| {
        client.is_bound(SlotKind::Decide)
            && client
                .source_for_slot(SlotKind::Decide)
                .map(|source| {
                    crate::capability::source_slot_capability(source, SlotKind::Decide).grade
                        != crate::capability::CapabilityGrade::Unavailable
                })
                .unwrap_or(true)
    });
    let text_ruling = if matches!(intent, Intent::Auto | Intent::Summary | Intent::Structured)
        && text_decide_available
        && crate::bash_path::estimate_tokens(&text) >= crate::bash_path::AUTO_SUMMARY_TOKENS
    {
        text_ruling(&text, ctx).await
    } else {
        None
    };
    let mut text_plan = plan_text(
        intent,
        text_ruling.as_ref().map(|ruling| &ruling.facts),
        summarize_available,
    );
    if text_plan.mode != TextMode::Raw {
        if let Some(resp) =
            text_summarize_response(&path, &text, ctx, &text_plan, text_ruling.as_ref()).await
        {
            return Ok(resp);
        }
        // summarize 不可用 / 调用失败：按 §3.8 退编号行原文 + 告知，不编造结构化结果。
        text_plan = TextPlan {
            mode: TextMode::Raw,
            notices: vec![crate::notice::Notice::SlotUnavailable {
                slot: "summarize".to_owned(),
                fallback: crate::notice::SlotFallback::Text,
            }],
        };
    }

    let offset = optional_usize(args, "offset").unwrap_or(1).max(1);
    let limit = optional_usize(args, "limit");
    let with_anchor = optional_bool(args, "anchor").unwrap_or(false);

    // 先按 offset/limit 切出窗口，再对窗口做截断治理。
    let window: Vec<&str> = text
        .lines()
        .skip(offset - 1)
        .take(limit.unwrap_or(usize::MAX))
        .collect();

    if window.is_empty() {
        return Ok(
            ToolResponse::text(format!("起始行 {offset} 超过文件总行数 {total_lines}。"))
                .with_meta(json!({ "totalLines": total_lines, "offset": offset })),
        );
    }

    let window_text = window.join("\n");
    let truncation = truncate_head(&window_text, ctx.limits);

    let shown: Vec<&str> = truncation.content.lines().collect();
    let body = if with_anchor {
        render_anchored(&shown, offset)
    } else {
        render_numbered(&shown, offset)
    };

    let mut out = body;
    if had_decode_errors {
        out.push_str(&format!(
            "\n[警告：此文件按 {} 解码时出现无法解释的字节，\
             显示内容里的问号方块（U+FFFD）是替换字符，不是原文。]",
            encoding_name
        ));
    }
    if truncation.truncated {
        let next = offset + shown.len();
        let reason = match truncation.truncated_by {
            Some(crate::truncate::TruncatedBy::Bytes) => tformat(
                lang,
                "已截断到 {}（字节上限）",
                &[format_size(truncation.output_bytes)],
            ),
            _ => tformat(
                lang,
                "已截断到 {} 行（行数上限）",
                &[shown.len().to_string()],
            ),
        };
        out.push_str(&tf(
            lang,
            "\n[{reason}。用 offset={next} 继续读取，文件共 {total_lines} 行。]",
            &[
                ("reason", reason),
                ("next", next.to_string()),
                ("total_lines", total_lines.to_string()),
            ],
        ));
    }

    let mut out_meta = json!({
        "path": path.display().to_string(),
        "totalLines": total_lines,
        "offset": offset,
        "shownLines": shown.len(),
        "truncated": truncation.truncated,
        "anchored": with_anchor,
        "encoding": encoding_name,
        "decodeErrors": had_decode_errors,
    });
    if let Some(ruling) = &text_ruling {
        out_meta["fileKind"] = json!(ruling.facts.file_kind.as_str());
        out_meta["sensitivity"] = json!(ruling.facts.sensitivity.as_str());
        out_meta["bestView"] = json!(ruling.facts.best_view.as_str());
        out_meta["confidence"] = json!(round2(ruling.confidence));
        if !ruling.calibrated {
            text_plan.notices.push(Notice::Uncalibrated {
                slot: "decide".to_owned(),
                adapter: crate::notice::AdapterKind::Json,
            });
        }
    }
    if !text_plan.notices.is_empty() {
        attach_text_notices(&mut out, &mut out_meta, text_plan.notices.clone());
    }
    Ok(ToolResponse::text(out).with_meta(out_meta))
}

/// decide 对图片的 L1 判定结果（§5.3 / §7.3）。
struct ImageRuling {
    facts: ImageFacts,
    /// 本次被采纳结论所依赖的问题概率（§7.3）。
    confidence: f64,
    /// systemone 的 noul 是校准概率；外部 JSON 适配器为自报值（§3.10）。
    calibrated: bool,
}

/// 调 decide 槽位做图片 L1 判定（§5.3 七问）。任何失败返回 None，由调用方走保守路径。
async fn image_ruling(
    path: &Path,
    mime: &str,
    meta: &std::fs::Metadata,
    ctx: &ToolContext,
    intent: Intent,
) -> Option<ImageRuling> {
    let client = ctx.slm.as_ref()?;
    let bytes = tokio::fs::read(path).await.ok()?;
    let prepared = image_pipeline::prepare(&bytes, mime, path).ok()?;
    let (mime_type, data_base64) = prepared.content.iter().find_map(|c| match c {
        Content::Image {
            mime_type,
            data_base64,
            ..
        } => Some((mime_type.clone(), data_base64.clone())),
        _ => None,
    })?;
    let dims = image_pipeline::dimensions(&bytes);
    let (width, height) = match dims {
        Some((w, h)) => (Some(w), Some(h)),
        None => (None, None),
    };
    let input = DecideInput {
        state: json!({
            "mime": mime,
            "size": meta.len(),
            "width": width,
            "height": height,
        }),
        images: Some(vec![json!(format!(
            "data:{mime_type};base64,{data_base64}"
        ))]),
        questions: image_decide_questions(),
    };
    let outcome = client.decide(&input).await.ok()?;
    let facts = merge_image_answers(&outcome.output.answers)?;
    // §7.3：confidence = 本次被采纳结论所依赖的问题概率的最小值，不是全部问题的最小值。
    // auto 的结论由 need_pixels 决定；structured / summary 的结论由 image_kind 决定。
    let confidence = match intent {
        Intent::Auto => facts.need_pixels as f64,
        Intent::Structured | Intent::Summary => facts.kind_confidence as f64,
        Intent::Raw | Intent::Visual => return None,
    };
    Some(ImageRuling {
        facts,
        confidence,
        calibrated: outcome.calibrated,
    })
}

/// decide 对文本路径的 L1 判定结果（§5.3）。
struct TextRuling {
    facts: TextFacts,
    /// min(被选中的 file_kind 概率, 被选中的 best_view 概率)；sensitivity 只标注不参与。
    confidence: f64,
    /// systemone 的 noul 是校准概率；外部 JSON 适配器为自报值（§3.10）。
    calibrated: bool,
}

/// 调 decide 槽位做文本路径 L1 十问（§5.3）。任何失败返回 None，调用方走原文。
async fn text_ruling(text: &str, ctx: &ToolContext) -> Option<TextRuling> {
    let client = ctx.slm.as_ref()?;
    let bounded: String = text.chars().take(8000).collect();
    let input = DecideInput {
        state: json!(bounded),
        images: None,
        questions: text_decide_questions(),
    };
    let outcome = client.decide(&input).await.ok()?;
    let facts = merge_text_answers(&outcome.output.answers)?;
    Some(TextRuling {
        facts,
        confidence: facts.kind_confidence.min(facts.view_confidence) as f64,
        calibrated: outcome.calibrated,
    })
}

/// §5.3 文本路径 L2：summarize 结构化 / 摘要；失败返回 None 由调用方降级原文。
async fn text_summarize_response(
    path: &Path,
    text: &str,
    ctx: &ToolContext,
    plan: &TextPlan,
    ruling: Option<&TextRuling>,
) -> Option<ToolResponse> {
    let client = ctx.slm.as_ref()?;
    let bounded: String = text.chars().take(8000).collect();
    let kind = ruling.map(|r| r.facts.file_kind).unwrap_or(FileKind::Unknown);
    let schema = (plan.mode == TextMode::Structured).then(|| text_summarize_schema(kind));
    let input = SummarizeInput {
        text: bounded,
        schema,
        max_tokens: Some(600),
    };
    let outcome = client.summarize(&input).await.ok()?;
    let content = match plan.mode {
        TextMode::Structured => outcome
            .output
            .data
            .as_ref()
            .map(|data| serde_json::to_string_pretty(data).unwrap_or_default())?,
        TextMode::Summary => outcome.output.text.clone()?,
        TextMode::Raw => return None,
    };
    let mut notices = plan.notices.clone();
    let mut meta = json!({
        "path": path.display().to_string(),
        "totalLines": text.lines().count(),
        "structured": plan.mode == TextMode::Structured,
    });
    if let Some(ruling) = ruling {
        meta["fileKind"] = json!(ruling.facts.file_kind.as_str());
        meta["sensitivity"] = json!(ruling.facts.sensitivity.as_str());
        meta["bestView"] = json!(ruling.facts.best_view.as_str());
        meta["confidence"] = json!(round2(ruling.confidence));
        if !ruling.calibrated {
            notices.push(Notice::Uncalibrated {
                slot: "decide".to_owned(),
                adapter: crate::notice::AdapterKind::Json,
            });
        }
    }
    let mut body = content;
    attach_text_notices(&mut body, &mut meta, notices);
    Some(ToolResponse::text(body).with_meta(meta))
}

/// 文本路径的告知合并（content 追加 + structuredContent 同源）。
fn attach_text_notices(body: &mut String, meta: &mut Value, notices: Vec<Notice>) {
    if notices.is_empty() {
        return;
    }
    let channels = budgeted_notices(notices);
    body.push('\n');
    body.push_str(&channels.text);
    meta["notices"] = channels.structured;
    meta["noticeOverflow"] = json!(channels.overflow);
}

/// 图片分支的返回组装（§10.1 / §3.8 / §4.4b 两个通道）。
async fn image_response(
    path: &Path,
    mime: &str,
    meta: &std::fs::Metadata,
    plan: &ImagePlan,
    ctx: &ToolContext,
    ruling: Option<&ImageRuling>,
) -> Result<ToolResponse, ToolError> {
    // 非校准概率必须标注（§3.10 / §4.4b）：decide 走外部 JSON 适配器时追加 UNCALIBRATED。
    let mut effective = plan.clone();
    if let Some(ruling) = ruling
        && !ruling.calibrated
    {
        effective.notices.push(Notice::Uncalibrated {
            slot: "decide".to_owned(),
            adapter: crate::notice::AdapterKind::Json,
        });
    }
    let plan = &effective;

    let bytes = tokio::fs::read(path).await?;
    let dims = image_pipeline::dimensions(&bytes);
    let (width, height) = match dims {
        Some((w, h)) => (Some(w), Some(h)),
        None => (None, None),
    };

    let mut resp = match plan.outcome {
        ImageOutcome::Original => {
            let mut resp = image_pipeline::prepare(&bytes, mime, path)?;
            attach_notices(&mut resp, plan.notices.clone(), true);
            resp
        }
        ImageOutcome::Metadata => {
            metadata_response(path, mime, meta, width, height, plan.notices.clone(), ctx)
        }
        ImageOutcome::TextUnderstanding => {
            text_understanding_response(path, mime, meta, dims, plan, ctx, &bytes).await?
        }
        // §10.1 中间区间：结构化在前、原图在后。
        ImageOutcome::Both => {
            let mut resp =
                text_understanding_response(path, mime, meta, dims, plan, ctx, &bytes).await?;
            // vision 失败时 text_understanding_response 已退回原图，不要重复追加。
            if !resp
                .content
                .iter()
                .any(|c| matches!(c, Content::Image { .. }))
            {
                let original = image_pipeline::prepare(&bytes, mime, path)?;
                resp.content.extend(original.content);
            }
            resp
        }
    };

    if let Some(ruling) = ruling {
        resp.meta["imageKind"] = json!(ruling.facts.kind.as_str());
        resp.meta["hasText"] = json!(ruling.facts.has_text);
        if let Some(need_pixels) = plan.need_pixels {
            resp.meta["needPixels"] = json!(need_pixels.as_str());
        }
        resp.meta["confidence"] = json!(round2(ruling.confidence));
    }
    Ok(resp)
}

/// §10.1:143-162 的元信息降级页；统一走一次预算合并（§4.4b 一个事实源）。
fn metadata_response(
    path: &Path,
    mime: &str,
    meta: &std::fs::Metadata,
    width: Option<u32>,
    height: Option<u32>,
    notices: Vec<Notice>,
    ctx: &ToolContext,
) -> ToolResponse {
    let display = display_path(path, &ctx.cwd);
    let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("image");
    let channels = budgeted_notices(notices);
    let facts = metadata_facts(mime, meta.len(), width, height, &display);
    let text = metadata_text(
        ctx.language,
        &channels.notices,
        file_name,
        mime,
        meta.len(),
        width.zip(height),
        &display,
    );
    let mut out_meta = facts;
    if !channels.notices.is_empty() {
        out_meta["notices"] = channels.structured;
        out_meta["noticeOverflow"] = json!(channels.overflow);
    }
    ToolResponse::text(text).with_meta(out_meta)
}

/// §10.1:114-116 / §5.3：vision 槽位做本地文本理解。
/// 图片先走压缩流水线（§10.2d），再把压缩后的 image part 交给 vision。
/// 调用失败按上游是否收图降级：收图退压缩后原图，不收图退元信息（§3.8）。
async fn text_understanding_response(
    path: &Path,
    mime: &str,
    meta: &std::fs::Metadata,
    dims: Option<(u32, u32)>,
    plan: &ImagePlan,
    ctx: &ToolContext,
    bytes: &[u8],
) -> Result<ToolResponse, ToolError> {
    let display = display_path(path, &ctx.cwd);
    let (width, height) = match dims {
        Some((w, h)) => (Some(w), Some(h)),
        None => (None, None),
    };
    let prepared = image_pipeline::prepare(bytes, mime, path)?;
    let image_part = prepared.content.iter().find_map(|c| match c {
        Content::Image {
            mime_type,
            data_base64,
            ..
        } => Some((mime_type.clone(), data_base64.clone())),
        _ => None,
    });
    let Some((mime_type, data_base64)) = image_part else {
        // 压缩熔断：返回压缩器的省略说明，并附上原有告知。
        let mut resp = prepared;
        attach_notices(&mut resp, plan.notices.clone(), true);
        return Ok(resp);
    };

    let task = plan.task.unwrap_or(VisionTask::Describe);
    let input = VisionInput {
        image: json!({
            "type": "image_url",
            "image_url": { "url": format!("data:{mime_type};base64,{data_base64}") }
        }),
        task,
        query: None,
    };
    let called = match ctx.slm.as_ref() {
        Some(client) => client.vision(&input).await,
        None => Err(crate::slm::SlotCallError::NotBound {
            slot: SlotKind::Vision,
        }),
    };

    match called {
        Ok(outcome) => {
            let text = match (&outcome.output.text, &outcome.output.boxes) {
                (Some(text), _) => text.clone(),
                (None, Some(boxes)) => serde_json::to_string(boxes).unwrap_or_default(),
                (None, None) => String::new(),
            };
            let channels = budgeted_notices(plan.notices.clone());
            let mut body = String::new();
            if !channels.notices.is_empty() {
                body.push_str(&channels.text);
                body.push('\n');
            }
            body.push_str(&text);
            let mut out_meta = metadata_facts(mime, meta.len(), width, height, &display);
            out_meta["vision"] = json!({
                "task": json!(task),
                "compressedWidth": prepared.meta["width"],
                "compressedHeight": prepared.meta["height"],
                "originalWidth": prepared.meta["originalWidth"],
                "originalHeight": prepared.meta["originalHeight"],
            });
            if !channels.notices.is_empty() {
                out_meta["notices"] = channels.structured;
                out_meta["noticeOverflow"] = json!(channels.overflow);
            }
            Ok(ToolResponse::text(body).with_meta(out_meta))
        }
        Err(_) => {
            let mut notices = plan.notices.clone();
            notices.push(Notice::SlotUnavailable {
                slot: "vision".to_owned(),
                fallback: if ctx.upstream_vision {
                    crate::notice::SlotFallback::Mechanical
                } else {
                    crate::notice::SlotFallback::Metadata
                },
            });
            if ctx.upstream_vision {
                let mut resp = prepared;
                attach_notices(&mut resp, notices, true);
                Ok(resp)
            } else {
                Ok(metadata_response(
                    path, mime, meta, width, height, notices, ctx,
                ))
            }
        }
    }
}

/// 把告知合并进一次返回；prepend_text 为 true 时把告知文本插到 content 首位。
fn attach_notices(resp: &mut ToolResponse, notices: Vec<Notice>, prepend_text: bool) {
    let channels = budgeted_notices(notices);
    if channels.notices.is_empty() {
        return;
    }
    if prepend_text {
        resp.content.insert(0, Content::Text(channels.text));
    }
    resp.meta["notices"] = channels.structured;
    resp.meta["noticeOverflow"] = json!(channels.overflow);
}

/// 渲染成 行号│内容 的形式，行号右对齐到 6 位。
fn render_numbered(lines: &[&str], start: usize) -> String {
    let mut out = String::with_capacity(lines.len() * 40);
    for (idx, line) in lines.iter().enumerate() {
        out.push_str(&format!("{:>6}\u{2502}{line}\n", start + idx));
    }
    out
}

/// 供测试与上层复用的截断选项构造。
pub fn limits(max_lines: usize, max_bytes: usize) -> TruncationOptions {
    TruncationOptions::new(max_lines, max_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn warm_dir(suffix: &str) -> (ToolContext, std::path::PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("tg-read-warm-{}-{suffix}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("a.txt"), "alpha\nbeta\ngamma\n").expect("write");
        let ctx = ToolContext::new(&dir).expect("ctx");
        (ctx, dir)
    }

    /// §22.6：预热只省 IO/解码，命中缓存与丢弃缓存后返回必须逐字一致。
    #[tokio::test]
    async fn warm_cache_does_not_change_return() {
        let (ctx, dir) = warm_dir("hit");
        let args = json!({ "path": "a.txt", "intent": "raw" });
        let first = run(&args, &ctx).await.expect("first read");
        assert_eq!(ctx.warm.len(), 1, "read + raw 应预热");
        let second = run(&args, &ctx).await.expect("second read");
        assert_eq!(
            format!("{:?}", first.content),
            format!("{:?}", second.content)
        );
        ctx.warm.clear();
        let third = run(&args, &ctx).await.expect("read after cache drop");
        assert_eq!(
            format!("{:?}", first.content),
            format!("{:?}", third.content)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// §22.4：read + summary 不预热。
    #[tokio::test]
    async fn summary_intent_does_not_warm() {
        let (ctx, dir) = warm_dir("summary");
        let _ = run(&json!({ "path": "a.txt", "intent": "summary" }), &ctx)
            .await
            .expect("summary read");
        assert!(ctx.warm.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
    use crate::tools::Content;
    use std::io::Write;

    fn ctx_with_file(name: &str, content: &str) -> (ToolContext, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("tg-read-{}-{}", std::process::id(), name));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let file = dir.join("f.txt");
        let mut f = std::fs::File::create(&file).expect("create");
        f.write_all(content.as_bytes()).expect("write");
        (ToolContext::new(&dir).expect("ctx"), file)
    }

    #[tokio::test]
    async fn reads_with_line_numbers() {
        let (ctx, file) = ctx_with_file("a", "alpha\nbeta\n");
        let resp = run(&json!({ "path": file.to_str().unwrap() }), &ctx)
            .await
            .expect("read ok");
        let Content::Text(body) = &resp.content[0] else {
            panic!("应返回文本")
        };
        assert!(body.contains("alpha"));
        assert!(body.contains('\u{2502}'));
        let _ = std::fs::remove_dir_all(ctx.cwd);
    }

    #[tokio::test]
    async fn honours_offset_and_limit() {
        let (ctx, file) = ctx_with_file("b", "1\n2\n3\n4\n5\n");
        let resp = run(
            &json!({ "path": file.to_str().unwrap(), "offset": 2, "limit": 2 }),
            &ctx,
        )
        .await
        .expect("read ok");
        let Content::Text(body) = &resp.content[0] else {
            panic!()
        };
        assert!(body.contains('2'));
        assert!(body.contains('3'));
        assert!(!body.contains('5'));
        let _ = std::fs::remove_dir_all(ctx.cwd);
    }

    #[tokio::test]
    async fn missing_file_is_error() {
        let (ctx, _f) = ctx_with_file("c", "x");
        let err = run(&json!({ "path": "/nonexistent/nope.txt" }), &ctx).await;
        assert!(err.is_err());
        let _ = std::fs::remove_dir_all(ctx.cwd);
    }

    fn ctx_with_png(name: &str) -> (ToolContext, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("tg-read-img-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let file = dir.join("shot.png");
        let img = image::RgbImage::from_fn(32, 16, |x, y| image::Rgb([x as u8, y as u8, 0]));
        let mut buf = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .expect("png");
        std::fs::write(&file, &buf).expect("write");
        (ToolContext::new(&dir).expect("ctx"), file)
    }

    #[tokio::test]
    async fn image_without_upstream_vision_returns_actionable_metadata() {
        let (ctx, file) = ctx_with_png("meta");
        let resp = run(&json!({ "path": file.to_str().unwrap() }), &ctx)
            .await
            .expect("read ok");
        let Content::Text(body) = &resp.content[0] else {
            panic!("应为文本")
        };
        assert!(body.contains("[SLOT_UNAVAILABLE]"));
        assert!(body.contains("[图片文件] shot.png"));
        assert!(body.contains("配置 vision 槽位后重试"));
        assert!(!body.contains("/Users/"));
        assert!(resp.content.iter().all(|c| matches!(c, Content::Text(_))));
        assert_eq!(resp.meta["kind"], json!("image"));
        assert_eq!(resp.meta["mime"], json!("image/png"));
        assert!(
            resp.meta["path"]
                .as_str()
                .unwrap()
                .starts_with("<workspace>")
        );
        assert_eq!(resp.meta["width"], json!(32));
        let _ = std::fs::remove_dir_all(ctx.cwd);
    }

    #[tokio::test]
    async fn image_with_upstream_vision_returns_original_block() {
        let (mut ctx, file) = ctx_with_png("original");
        ctx.upstream_vision = true;
        let resp = run(
            &json!({ "path": file.to_str().unwrap(), "intent": "visual" }),
            &ctx,
        )
        .await
        .expect("read ok");
        assert!(
            resp.content
                .iter()
                .any(|c| matches!(c, Content::Image { .. }))
        );
        assert!(resp.meta.get("notices").is_none());
        let _ = std::fs::remove_dir_all(ctx.cwd);
    }

    #[tokio::test]
    async fn image_structured_with_upstream_vision_degrades_with_notice() {
        let (mut ctx, file) = ctx_with_png("structured");
        ctx.upstream_vision = true;
        let resp = run(
            &json!({ "path": file.to_str().unwrap(), "intent": "structured" }),
            &ctx,
        )
        .await
        .expect("read ok");
        assert!(
            resp.content
                .iter()
                .any(|c| matches!(c, Content::Image { .. }))
        );
        let Content::Text(notice) = &resp.content[0] else {
            panic!("首块应为告知文本")
        };
        assert!(notice.contains("[SLOT_UNAVAILABLE]"));
        assert_eq!(resp.meta["notices"].as_array().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(ctx.cwd);
    }

    /// 给上下文注入一个 vision 槽位客户端（默认构建没有 external-protocols，
    /// 真实调用会返回 ClientUnavailable；这里只验证编排与降级）。
    fn set_vision_client(ctx: &mut ToolContext, source: &str) {
        let cfg = crate::config::parse_config(
            &format!(
                "models:\n  llm:\n    source: {source}\n    base_url: http://127.0.0.1:8090/v1\n    model: demo\n    path: /tmp/model\n    serves: [vision]\n"
            ),
            "test.yaml",
        )
        .expect("配置应当解析成功");
        ctx.slm = Some(std::sync::Arc::new(crate::slm::SlotClient::new(cfg)));
    }

    /// 同时绑定 decide（systemone）与 vision（openai）的上下文；
    /// 默认构建下两次真实调用都会降级，用于验证编排与保守路径。
    fn set_decide_and_vision_slots(ctx: &mut ToolContext) {
        let cfg = crate::config::parse_config(
            "models:\n  llm:\n    source: openai\n    base_url: http://127.0.0.1:8090/v1\n    model: demo\n    path: /tmp/model\n    serves: [vision]\n  jev:\n    source: systemone\n    base_url: http://127.0.0.1:8090/v1\n    model: demo\n    serves: [decide]\n",
            "test.yaml",
        )
        .expect("配置应当解析成功");
        ctx.slm = Some(std::sync::Arc::new(crate::slm::SlotClient::new(cfg)));
    }

    fn text_blocks(resp: &ToolResponse) -> Vec<&str> {
        resp.content
            .iter()
            .filter_map(|c| match c {
                Content::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect()
    }

    /// §10.1:114-116：上游不收图 + vision 已绑定 → 尝试本地理解；
    /// 调用失败按 §3.8 退元信息，并如实告知 vision 与 decide 两个缺口。
    #[tokio::test]
    async fn image_without_upstream_vision_and_vision_slot_falls_back_to_metadata() {
        let (mut ctx, file) = ctx_with_png("meta-vision");
        set_vision_client(&mut ctx, "openai");
        let resp = run(&json!({ "path": file.to_str().unwrap() }), &ctx)
            .await
            .expect("read ok");
        assert!(
            resp.content.iter().all(|c| matches!(c, Content::Text(_))),
            "上游不收图时不应返回 image block"
        );
        let body = text_blocks(&resp).join("\n");
        assert!(body.contains("slot=vision"), "{body}");
        assert!(body.contains("fallback=metadata"), "{body}");
        assert!(body.contains("slot=decide"), "{body}");
        let _ = std::fs::remove_dir_all(ctx.cwd);
    }

    /// §5.3 协商表：structured 请求 + 上游收图 + vision 已绑定 → 本地理解；
    /// 调用失败退压缩后原图（不是元信息），并告知 vision 降级。
    #[tokio::test]
    async fn image_structured_with_upstream_vision_and_vision_slot_falls_back_to_original() {
        let (mut ctx, file) = ctx_with_png("structured-fallback");
        ctx.upstream_vision = true;
        set_vision_client(&mut ctx, "openai");
        let resp = run(
            &json!({ "path": file.to_str().unwrap(), "intent": "structured" }),
            &ctx,
        )
        .await
        .expect("read ok");
        assert!(
            resp.content
                .iter()
                .any(|c| matches!(c, Content::Image { .. })),
            "上游收图时 vision 失败应退回原图"
        );
        let body = text_blocks(&resp).join("\n");
        assert!(body.contains("slot=vision"), "{body}");
        assert!(body.contains("fallback=mechanical"), "{body}");
        let _ = std::fs::remove_dir_all(ctx.cwd);
    }

    /// §10.1：auto 的 need_pixels 依赖 decide；decide 未接入时保守给原图，
    /// 告知缺的是 decide，而不是谎报 vision 不可用。
    #[tokio::test]
    async fn image_auto_with_upstream_vision_and_vision_slot_keeps_original_with_decide_notice() {
        let (mut ctx, file) = ctx_with_png("auto-decide");
        ctx.upstream_vision = true;
        set_vision_client(&mut ctx, "openai");
        let resp = run(
            &json!({ "path": file.to_str().unwrap(), "intent": "auto" }),
            &ctx,
        )
        .await
        .expect("read ok");
        assert!(
            resp.content
                .iter()
                .any(|c| matches!(c, Content::Image { .. })),
            "auto 在 need_pixels 未判定时应给原图"
        );
        let body = text_blocks(&resp).join("\n");
        assert!(body.contains("slot=decide"), "{body}");
        assert!(body.contains("fallback=mechanical"), "{body}");
        let _ = std::fs::remove_dir_all(ctx.cwd);
    }

    /// 同时绑定 decide 与 vision：默认构建下两次外部调用都不可用，
    /// auto 必须优雅降级为原图，且不伪造 imageKind / confidence。
    #[tokio::test]
    async fn image_auto_with_decide_and_vision_slots_degrades_to_original() {
        let (mut ctx, file) = ctx_with_png("auto-both-slots");
        ctx.upstream_vision = true;
        set_decide_and_vision_slots(&mut ctx);
        let resp = run(
            &json!({ "path": file.to_str().unwrap(), "intent": "auto" }),
            &ctx,
        )
        .await
        .expect("read ok");
        assert!(
            resp.content
                .iter()
                .any(|c| matches!(c, Content::Image { .. })),
            "判定不可用时 auto 应给原图"
        );
        assert!(
            resp.meta.get("imageKind").is_none(),
            "判定失败不应伪造 imageKind"
        );
        let body = text_blocks(&resp).join("\n");
        assert!(body.contains("slot=decide"), "{body}");
        let _ = std::fs::remove_dir_all(ctx.cwd);
    }

    /// §10.1 中间区间（Both）：结构化在前、原图在后；vision 失败时退回原图，
    /// 但决定判定产出的 meta 仍要如实写入。
    #[tokio::test]
    async fn image_both_plan_returns_original_with_ruling_meta() {
        let (mut ctx, file) = ctx_with_png("both-plan");
        ctx.upstream_vision = true;
        set_vision_client(&mut ctx, "openai");
        let facts = ImageFacts {
            kind: crate::read_path::ImageKind::Chart,
            kind_confidence: 0.8,
            has_text: true,
            need_pixels: 0.7084,
        };
        let plan = plan_image(Intent::Auto, true, true, Some(&facts));
        assert_eq!(plan.outcome, ImageOutcome::Both);
        let ruling = ImageRuling {
            facts,
            confidence: 0.7769,
            calibrated: true,
        };
        let meta = std::fs::metadata(&file).expect("meta");
        let resp = image_response(&file, "image/png", &meta, &plan, &ctx, Some(&ruling))
            .await
            .expect("image response");
        assert!(
            resp.content
                .iter()
                .any(|c| matches!(c, Content::Image { .. })),
            "vision 失败时 Both 应退回原图"
        );
        assert_eq!(resp.meta["imageKind"], json!("chart"));
        // needPixels 输出决策档位，confidence 只保留两位小数（§4.7 输出密度）。
        assert_eq!(resp.meta["needPixels"], json!("both"));
        assert_eq!(resp.meta["confidence"], json!(0.78));
        let _ = std::fs::remove_dir_all(ctx.cwd);
    }
}
