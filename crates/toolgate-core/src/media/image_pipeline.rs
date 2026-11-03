//! 图片多级降级压缩。
//!
//! 降级顺序：缩放 → 质量阶梯 → 尺寸再缩 → 熔断。
//! 每降一档都重新测量 base64 体积，达标即停。

use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use image::{DynamicImage, GenericImageView, ImageEncoder};
use serde_json::json;

use crate::tools::{Content, ToolResponse};
use crate::ToolError;

/// 压缩流水线的配置。
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// 最长边上限。1568 是主流视觉编码器的训练分辨率上限，
    /// 超过后增加像素通常不再提升识别质量，只是白花 token。
    pub max_edge: u32,
    /// base64 文本的硬熔断阈值。
    pub max_base64_bytes: usize,
    /// 有损编码的质量阶梯，从高到低依次尝试。
    pub quality_ladder: &'static [u8],
    /// 尺寸兜底轮数：每轮把最长边乘 0.75 重试。
    pub shrink_rounds: u32,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            max_edge: 1568,
            max_base64_bytes: 1_200_000,
            quality_ladder: &[85, 70, 55, 40],
            shrink_rounds: 3,
        }
    }
}

/// 一次压缩的结果。
#[derive(Debug)]
pub struct Prepared {
    pub mime_type: String,
    pub base64: String,
    pub sha256: String,
    /// 压缩后像素尺寸。
    pub width: u32,
    /// 压缩后像素尺寸。
    pub height: u32,
    /// 原始像素尺寸，用于给模型换算坐标。
    pub original_width: u32,
    /// 原始像素尺寸。
    pub original_height: u32,
    /// 实际使用的质量档（None 表示无损 PNG）。
    pub quality: Option<u8>,
    /// 是否发生过缩放。
    pub resized: bool,
}

/// 处理一张图片，产出 MCP image content block。
pub fn prepare(bytes: &[u8], mime: &str, path: &Path) -> Result<ToolResponse, ToolError> {
    prepare_with(bytes, mime, path, &PipelineConfig::default())
}

/// 带配置的处理入口，供测试与上层调整策略。
pub fn prepare_with(
    bytes: &[u8],
    mime: &str,
    path: &Path,
    cfg: &PipelineConfig,
) -> Result<ToolResponse, ToolError> {
    let img = image::load_from_memory(bytes)
        .map_err(|e| ToolError::Failed(format!("图片解码失败（{mime}）：{e}")))?;

    let (ow, oh) = img.dimensions();
    let digest = crate::spill::sha256_short(bytes);

    // 已达标且体积合规：原样透传，不重编码（避免二次损失）。
    let already_small = ow <= cfg.max_edge
        && oh <= cfg.max_edge
        && B64.encode(bytes).len() <= cfg.max_base64_bytes
        && matches!(mime, "image/png" | "image/jpeg" | "image/webp");

    if already_small {
        return Ok(ok_response(Prepared {
            mime_type: mime.to_owned(),
            base64: B64.encode(bytes),
            sha256: digest,
            width: ow,
            height: oh,
            original_width: ow,
            original_height: oh,
            quality: None,
            resized: false,
        }, path));
    }

    let has_alpha = img.color().has_alpha();
    let mut edge = cfg.max_edge;

    for round in 0..=cfg.shrink_rounds {
        let scaled = scale_to_edge(&img, edge);
        let (w, h) = scaled.dimensions();

        // 有透明通道：先试 PNG（无损，保透明）。
        if has_alpha {
            if let Some(encoded) = encode_png(&scaled) {
                if B64.encode(&encoded).len() <= cfg.max_base64_bytes {
                    return Ok(ok_response(Prepared {
                        mime_type: "image/png".to_owned(),
                        base64: B64.encode(&encoded),
                        sha256: crate::spill::sha256_short(&encoded),
                        width: w,
                        height: h,
                        original_width: ow,
                        original_height: oh,
                        quality: None,
                        resized: round > 0 || w != ow || h != oh,
                    }, path));
                }
            }
        }

        // 有损 JPEG：按质量阶梯逐档下降。
        let rgb = scaled.to_rgb8();
        for &q in cfg.quality_ladder {
            if let Some(encoded) = encode_jpeg(&rgb, q) {
                if B64.encode(&encoded).len() <= cfg.max_base64_bytes {
                    return Ok(ok_response(Prepared {
                        mime_type: "image/jpeg".to_owned(),
                        base64: B64.encode(&encoded),
                        sha256: crate::spill::sha256_short(&encoded),
                        width: w,
                        height: h,
                        original_width: ow,
                        original_height: oh,
                        quality: Some(q),
                        resized: round > 0 || w != ow || h != oh,
                    }, path));
                }
            }
        }

        // 本轮全部超限：缩小尺寸再来一轮。
        let next = (edge as f64 * 0.75).round() as u32;
        if next < 64 {
            break;
        }
        edge = next;
    }

    // 熔断：不塞进上下文，返回文本说明 + 原图路径，让调用方决定。
    Ok(ToolResponse {
        content: vec![Content::Text(format!(
            "图片 {} 压缩到 {}px 后仍超过 {} 字节的 base64 上限，已省略原图。\
             原文件仍在 {}，如需分析请先本地降采样或裁切。",
            path.display(),
            cfg.max_edge,
            cfg.max_base64_bytes,
            path.display()
        ))],
        is_error: false,
        meta: json!({
            "omitted": true,
            "reason": "exceeds_base64_limit",
            "path": path.display().to_string(),
            "originalWidth": ow,
            "originalHeight": oh,
        }),
    })
}

fn ok_response(p: Prepared, path: &Path) -> ToolResponse {
    let note = if p.resized {
        format!(
            "[图片已从 {}x{} 压缩为 {}x{}。模型看到的是压缩后版本，\
             若需按原图坐标定位，请把坐标乘以 {:.2}。]",
            p.original_width,
            p.original_height,
            p.width,
            p.height,
            p.original_width as f64 / p.width as f64
        )
    } else {
        String::new()
    };

    let mut content = Vec::new();
    if !note.is_empty() {
        content.push(Content::Text(note));
    }
    content.push(Content::Image {
        mime_type: p.mime_type.clone(),
        data_base64: p.base64.clone(),
        sha256: p.sha256.clone(),
    });

    ToolResponse {
        content,
        is_error: false,
        meta: json!({
            "path": path.display().to_string(),
            "mimeType": p.mime_type,
            "imageHash": p.sha256,
            "width": p.width,
            "height": p.height,
            "originalWidth": p.original_width,
            "originalHeight": p.original_height,
            "quality": p.quality,
            "resized": p.resized,
            "base64Bytes": p.base64.len(),
        }),
    }
}

/// 等比缩放到最长边不超过 edge；本来就没超则原样返回。
fn scale_to_edge(img: &DynamicImage, edge: u32) -> DynamicImage {
    let (w, h) = img.dimensions();
    if w <= edge && h <= edge {
        return img.clone();
    }
    let scale = edge as f64 / w.max(h) as f64;
    let nw = ((w as f64 * scale).round() as u32).max(1);
    let nh = ((h as f64 * scale).round() as u32).max(1);
    img.resize_exact(nw, nh, image::imageops::FilterType::Lanczos3)
}

fn encode_jpeg(rgb: &image::RgbImage, quality: u8) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality);
    enc.write_image(
        rgb.as_raw(),
        rgb.width(),
        rgb.height(),
        image::ExtendedColorType::Rgb8,
    )
    .ok()?;
    Some(buf)
}

fn encode_png(img: &DynamicImage) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .ok()?;
    Some(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_fn(w, h, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
        });
        let mut buf = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .expect("encode png");
        buf
    }

    #[test]
    fn small_image_passes_through() {
        let png = make_png(64, 64);
        let resp = prepare(&png, "image/png", Path::new("/tmp/x.png")).expect("prepare");
        assert!(resp.content.iter().any(|c| matches!(c, Content::Image { .. })));
        let meta = &resp.meta;
        assert_eq!(meta["resized"], json!(false));
    }

    #[test]
    fn oversized_image_is_scaled_down() {
        let png = make_png(3000, 2000);
        let resp = prepare(&png, "image/png", Path::new("/tmp/big.png")).expect("prepare");
        let meta = &resp.meta;
        let w = meta["width"].as_u64().expect("width") as u32;
        let h = meta["height"].as_u64().expect("height") as u32;
        assert!(w <= 1568 && h <= 1568, "压缩后应不超过最长边上限，实际 {w}x{h}");
        assert_eq!(meta["originalWidth"], json!(3000));
    }

    #[test]
    fn tiny_limit_triggers_omission() {
        let png = make_png(400, 400);
        let cfg = PipelineConfig {
            max_base64_bytes: 100, // 不可能达到
            ..Default::default()
        };
        let resp = prepare_with(&png, "image/png", Path::new("/tmp/x.png"), &cfg).expect("prepare");
        assert_eq!(resp.meta["omitted"], json!(true));
        assert!(matches!(resp.content[0], Content::Text(_)));
    }
}
