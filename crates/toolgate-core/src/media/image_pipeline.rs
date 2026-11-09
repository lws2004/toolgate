//! 图片多级降级压缩。
//!
//! 降级顺序：缩放 → 质量阶梯 → 尺寸再缩 → 熔断。
//! 每降一档都重新测量 base64 体积，达标即停。

use std::collections::HashSet;
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use image::{DynamicImage, GenericImageView, ImageEncoder};
use serde_json::json;

use crate::ToolError;
use crate::tools::{Content, ToolResponse};

/// 压缩流水线的配置。
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// 最长边上限。1568 是主流视觉编码器的训练分辨率上限，
    /// 超过后增加像素通常不再提升识别质量，只是白花 token。
    pub max_edge: u32,
    /// base64 文本的硬熔断阈值。
    pub max_base64_bytes: usize,
    /// photo 类内容的有损质量阶梯，从高到低依次尝试。
    pub quality_ladder: &'static [u8],
    /// flat 类内容的保守阶梯：不降到会糊掉文字边缘的档（§10.2b 标定契约）。
    pub flat_ladder: &'static [u8],
    /// 尺寸兜底轮数：每轮把最长边乘 0.75 重试。
    pub shrink_rounds: u32,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            max_edge: 1568,
            max_base64_bytes: 1_200_000,
            quality_ladder: &[85, 70, 55, 40],
            flat_ladder: &[85, 70],
            shrink_rounds: 3,
        }
    }
}

/// §10.2b 标定契约：由 L0 采样统计量机械判定的内容类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentKind {
    /// 截图 / 图表 / 文字图：大面积平坦色块，降质量易糊文字。
    Flat,
    /// 照片 / 渐变 / 噪声：颜色丰富，可承受更多降质量。
    Photo,
}

/// 分类阈值，来自合成样本实测（§10.2b）：不同颜色占比与相邻像素相等占比。
pub const FLAT_DISTINCT_RATIO: f64 = 0.15;
pub const FLAT_NEIGHBOR_RATIO: f64 = 0.9;

/// 采样统计量，供分类与基准脚本共用。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContentStats {
    pub sampled: usize,
    pub distinct: usize,
    pub flat_ratio: f64,
}

/// 网格采样：最多约 64×64 个点，把颜色量化到 5bit/通道后统计多样性，
/// 并统计水平相邻像素的相等比例。纯函数、零新增依赖。
pub fn content_stats(rgb: &image::RgbImage) -> ContentStats {
    let (w, h) = rgb.dimensions();
    let step_x = (w / 64).max(1);
    let step_y = (h / 64).max(1);
    let mut seen: HashSet<u32> = HashSet::new();
    let mut sampled = 0usize;
    let mut equal_neighbors = 0usize;
    let mut neighbor_pairs = 0usize;
    let mut y = 0u32;
    while y < h {
        let mut x = 0u32;
        while x < w {
            let p = rgb.get_pixel(x, y).0;
            let key = ((p[0] as u32 >> 3) << 10) | ((p[1] as u32 >> 3) << 5) | (p[2] as u32 >> 3);
            seen.insert(key);
            sampled += 1;
            if x + 1 < w {
                neighbor_pairs += 1;
                if p == rgb.get_pixel(x + 1, y).0 {
                    equal_neighbors += 1;
                }
            }
            x += step_x;
        }
        y += step_y;
    }
    ContentStats {
        sampled,
        distinct: seen.len(),
        flat_ratio: if neighbor_pairs == 0 {
            0.0
        } else {
            equal_neighbors as f64 / neighbor_pairs as f64
        },
    }
}

/// 按统计量分类：颜色少或大面积同色即 flat，否则 photo。
pub fn classify_content(rgb: &image::RgbImage) -> ContentKind {
    let s = content_stats(rgb);
    let distinct_ratio = if s.sampled == 0 {
        1.0
    } else {
        s.distinct as f64 / s.sampled as f64
    };
    if distinct_ratio < FLAT_DISTINCT_RATIO || s.flat_ratio > FLAT_NEIGHBOR_RATIO {
        ContentKind::Flat
    } else {
        ContentKind::Photo
    }
}

/// 按内容类型取标定后的阶梯；两条阶梯都来自配置，压缩循环不硬编码档位。
pub fn ladder_for(kind: ContentKind, cfg: &PipelineConfig) -> &'static [u8] {
    match kind {
        ContentKind::Flat => cfg.flat_ladder,
        ContentKind::Photo => cfg.quality_ladder,
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
        .map_err(|e| ToolError::failed(format!("图片解码失败（{mime}）：{e}")))?;

    let (ow, oh) = img.dimensions();
    let digest = crate::spill::sha256_short(bytes);

    // 已达标且体积合规：原样透传，不重编码（避免二次损失）。
    let already_small = ow <= cfg.max_edge
        && oh <= cfg.max_edge
        && B64.encode(bytes).len() <= cfg.max_base64_bytes
        && matches!(mime, "image/png" | "image/jpeg" | "image/webp");

    if already_small {
        return Ok(ok_response(
            Prepared {
                mime_type: mime.to_owned(),
                base64: B64.encode(bytes),
                sha256: digest,
                width: ow,
                height: oh,
                original_width: ow,
                original_height: oh,
                quality: None,
                resized: false,
            },
            path,
        ));
    }

    let has_alpha = img.color().has_alpha();
    let mut edge = cfg.max_edge;

    for round in 0..=cfg.shrink_rounds {
        let scaled = scale_to_edge(&img, edge);
        let (w, h) = scaled.dimensions();

        // 有透明通道：先试 PNG（无损，保透明）。
        if has_alpha
            && let Some(encoded) = encode_png(&scaled)
            && B64.encode(&encoded).len() <= cfg.max_base64_bytes
        {
            return Ok(ok_response(
                Prepared {
                    mime_type: "image/png".to_owned(),
                    base64: B64.encode(&encoded),
                    sha256: crate::spill::sha256_short(&encoded),
                    width: w,
                    height: h,
                    original_width: ow,
                    original_height: oh,
                    quality: None,
                    resized: round > 0 || w != ow || h != oh,
                },
                path,
            ));
        }

        // 有损 JPEG：按内容类型选标定后的阶梯，逐档下降（§10.2b）。
        let rgb = scaled.to_rgb8();
        let ladder = ladder_for(classify_content(&rgb), cfg);
        for &q in ladder {
            if let Some(encoded) = encode_jpeg(&rgb, q)
                && B64.encode(&encoded).len() <= cfg.max_base64_bytes
            {
                return Ok(ok_response(
                    Prepared {
                        mime_type: "image/jpeg".to_owned(),
                        base64: B64.encode(&encoded),
                        sha256: crate::spill::sha256_short(&encoded),
                        width: w,
                        height: h,
                        original_width: ow,
                        original_height: oh,
                        quality: Some(q),
                        resized: round > 0 || w != ow || h != oh,
                    },
                    path,
                ));
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

/// 只读取像素尺寸，供「返回元信息」的降级分支使用（§10.1）。
pub fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let img = image::load_from_memory(bytes).ok()?;
    Some(img.dimensions())
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

pub fn encode_jpeg(rgb: &image::RgbImage, quality: u8) -> Option<Vec<u8>> {
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
        assert!(
            resp.content
                .iter()
                .any(|c| matches!(c, Content::Image { .. }))
        );
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
        assert!(
            w <= 1568 && h <= 1568,
            "压缩后应不超过最长边上限，实际 {w}x{h}"
        );
        assert_eq!(meta["originalWidth"], json!(3000));
    }

    fn flat_sample() -> image::RgbImage {
        image::RgbImage::from_fn(256, 256, |x, y| {
            if (x / 32) % 2 == 0 && y % 16 < 3 {
                image::Rgb([0, 0, 0])
            } else {
                image::Rgb([255, 255, 255])
            }
        })
    }

    fn photo_sample() -> image::RgbImage {
        image::RgbImage::from_fn(256, 256, |x, y| {
            let n = (x.wrapping_mul(73856093) ^ y.wrapping_mul(19349663)).wrapping_mul(2654435761);
            image::Rgb([(n >> 16) as u8, (n >> 8) as u8, n as u8])
        })
    }

    #[test]
    fn content_kinds_are_separable() {
        assert_eq!(classify_content(&flat_sample()), ContentKind::Flat);
        assert_eq!(classify_content(&photo_sample()), ContentKind::Photo);
    }

    #[test]
    fn flat_ladder_is_conservative_and_configurable() {
        let cfg = PipelineConfig::default();
        assert_eq!(ladder_for(ContentKind::Flat, &cfg), &[85u8, 70][..]);
        assert_eq!(
            ladder_for(ContentKind::Photo, &cfg),
            &[85u8, 70, 55, 40][..]
        );
        let custom = PipelineConfig {
            flat_ladder: &[90],
            ..Default::default()
        };
        assert_eq!(ladder_for(ContentKind::Flat, &custom), &[90u8][..]);
    }

    #[test]
    fn quality_ladder_bytes_decrease_monotonically() {
        let rgb = photo_sample();
        let sizes: Vec<usize> = [85u8, 70, 55, 40]
            .iter()
            .map(|&q| encode_jpeg(&rgb, q).expect("encode").len())
            .collect();
        assert!(
            sizes.windows(2).all(|w| w[0] >= w[1]),
            "体积未随质量下降单调不增: {sizes:?}"
        );
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
