//! read：带行号与锚点的读取器。
//!
//! 治理规则与 pi 一致：超过 2000 行或 50KB 时头部截断，并提示用
//! offset 续读；**不落盘**——文件本身就在磁盘上，指针没有意义。

use serde_json::{Value, json};

use super::{ToolContext, ToolResponse, ToolSpec, optional_bool, optional_usize, required_str};
use crate::ToolError;
use crate::hashline::render_anchored;
use crate::media;
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

    // 图片分支：交给附件流水线处理，产出 image content block。
    if let Some(mime) = media::detect_image_mime(&path).await? {
        return media::prepare_image(&path, &mime).await;
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
}
