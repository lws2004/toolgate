//! read：带行号与锚点的读取器。
//!
//! 治理规则与 pi 一致：超过 2000 行或 50KB 时头部截断，并提示用
//! offset 续读；**不落盘**——文件本身就在磁盘上，指针没有意义。

use std::path::Path;

use serde_json::{Value, json};

use super::{
    Content, ToolContext, ToolResponse, ToolSpec, optional_bool, optional_usize, required_str,
};
use crate::ToolError;
use crate::compat::{CompatConfig, resolve_intent};
use crate::hashline::render_anchored;
use crate::media;
use crate::media::image_pipeline;
use crate::notice::{render_notices, structured_notices};
use crate::read_path::{
    ImageOutcome, ImagePlan, display_path, metadata_facts, metadata_text, plan_image,
};
use crate::truncate::{TruncationOptions, format_size, truncate_head};

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: "read",
        description: "读取文本文件（带行号）或图片。超过 2000 行 / 50KB 时头部截断，\
                      用 offset 参数续读。图片会自动走压缩流水线。",
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
        // vision 槽位当前不可用（可选层未接入，§3.3）。
        let plan = plan_image(intent, ctx.upstream_vision, false);
        return image_response(&path, &mime, &meta, &plan, ctx).await;
    }

    let bytes = tokio::fs::read(&path).await?;

    // 先认编码：UTF-16 文件天然含 NUL 字节，不能按「有 NUL 即二进制」拦下。
    let encoding = crate::encoding::FileEncoding::detect(&bytes);
    let is_wide = encoding.name().starts_with("UTF-16");
    if !is_wide && media::looks_binary(&bytes) {
        return Err(ToolError::failed(format!(
            "{} 是二进制文件（{} 字节），不能按文本读取。若需处理请用 bash 调用专用工具。",
            path.display(),
            format_size(bytes.len())
        )));
    }

    // 按检测出的编码解码。had_errors 为真说明有字节无法在该编码下解释，
    // 此时显示内容里的替换字符不是原文，必须告知调用方。
    let (text, had_decode_errors) = encoding.decode(&bytes);
    let total_lines = text.lines().count();

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
            encoding.name()
        ));
    }
    if truncation.truncated {
        let next = offset + shown.len();
        let reason = match truncation.truncated_by {
            Some(crate::truncate::TruncatedBy::Bytes) => {
                format!(
                    "已截断到 {}（字节上限）",
                    format_size(truncation.output_bytes)
                )
            }
            _ => format!("已截断到 {} 行（行数上限）", shown.len()),
        };
        out.push_str(&format!(
            "\n[{reason}。用 offset={next} 继续读取，文件共 {total_lines} 行。]"
        ));
    }

    Ok(ToolResponse::text(out).with_meta(json!({
        "path": path.display().to_string(),
        "totalLines": total_lines,
        "offset": offset,
        "shownLines": shown.len(),
        "truncated": truncation.truncated,
        "anchored": with_anchor,
        "encoding": encoding.name(),
        "decodeErrors": had_decode_errors,
    })))
}

/// 图片分支的返回组装（§10.1 / §3.8 / §4.4b 两个通道）。
async fn image_response(
    path: &Path,
    mime: &str,
    meta: &std::fs::Metadata,
    plan: &ImagePlan,
    ctx: &ToolContext,
) -> Result<ToolResponse, ToolError> {
    let display = display_path(path, &ctx.cwd);
    let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("image");
    let bytes = tokio::fs::read(path).await?;
    let (width, height) = match image_pipeline::dimensions(&bytes) {
        Some((w, h)) => (Some(w), Some(h)),
        None => (None, None),
    };

    match plan.outcome {
        ImageOutcome::Original => {
            let mut resp = image_pipeline::prepare(&bytes, mime, path)?;
            if !plan.notices.is_empty() {
                resp.content
                    .insert(0, Content::Text(render_notices(&plan.notices)));
                resp.meta["notices"] = structured_notices(&plan.notices);
            }
            Ok(resp)
        }
        // 槽位全空且上游不收图：返回元信息 + 原因 + 可选路径（§10.1:143-162）。
        // TextUnderstanding 需要 vision 槽位的真实输出，本阶段同样退到元信息。
        ImageOutcome::Metadata | ImageOutcome::TextUnderstanding => {
            let facts = metadata_facts(mime, meta.len(), width, height, &display);
            let text = metadata_text(
                &plan.notices,
                file_name,
                mime,
                meta.len(),
                width,
                height,
                &display,
            );
            let mut out_meta = facts;
            if !plan.notices.is_empty() {
                out_meta["notices"] = structured_notices(&plan.notices);
            }
            Ok(ToolResponse::text(text).with_meta(out_meta))
        }
    }
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
}
