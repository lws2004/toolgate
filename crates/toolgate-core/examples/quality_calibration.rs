//! §10.2b 质量阶梯标定基准：固定尺寸、确定性合成样本。
//!
//! 只测体积与分类统计量，不测主观质量；合成样本不代表真实业务图片。
//! 运行：cargo run --release -p toolgate-core --example quality_calibration

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use image::RgbImage;
use toolgate_core::media::image_pipeline::{
    ContentKind, PipelineConfig, classify_content, content_stats, encode_jpeg, ladder_for,
};

const W: u32 = 1024;
const H: u32 = 768;

fn flat_color() -> RgbImage {
    RgbImage::from_fn(W, H, |x, y| {
        if (x / 128) % 3 == 0 && (y / 96) % 2 == 0 {
            image::Rgb([24, 48, 96])
        } else {
            image::Rgb([240, 240, 240])
        }
    })
}

fn text_like() -> RgbImage {
    RgbImage::from_fn(W, H, |x, y| {
        let line = y % 24;
        if line < 3 && (x / 7) % 5 != 0 {
            image::Rgb([16, 16, 16])
        } else {
            image::Rgb([255, 255, 255])
        }
    })
}

fn gradient() -> RgbImage {
    RgbImage::from_fn(W, H, |x, y| {
        image::Rgb([
            (x * 255 / W) as u8,
            (y * 255 / H) as u8,
            ((x + y) * 255 / (W + H)) as u8,
        ])
    })
}

fn photo_like() -> RgbImage {
    RgbImage::from_fn(W, H, |x, y| {
        let n = (x.wrapping_mul(73856093) ^ y.wrapping_mul(19349663)).wrapping_mul(2654435761);
        image::Rgb([(n >> 16) as u8, (n >> 8) as u8, n as u8])
    })
}

fn main() {
    let cfg = PipelineConfig::default();
    let samples: [(&str, RgbImage); 4] = [
        ("flat-color", flat_color()),
        ("text-like", text_like()),
        ("gradient", gradient()),
        ("photo-like", photo_like()),
    ];
    println!("样本 1024x768，确定性合成；image 0.25 / macOS arm64；体积单位 = base64 字节");
    println!(
        "{:<12} {:<6} {:>6} {:>8} {:>9} {:>9} {:>9} {:>9} {:>9}  阶梯",
        "样本", "类型", "采样", "不同色", "平坦率", "q85", "q70", "q55", "q40",
    );
    for (name, rgb) in &samples {
        let s = content_stats(rgb);
        let kind = classify_content(rgb);
        let kind_str = match kind {
            ContentKind::Flat => "flat",
            ContentKind::Photo => "photo",
        };
        let mut sizes = [0usize; 4];
        for (i, q) in [85u8, 70, 55, 40].iter().enumerate() {
            let encoded = encode_jpeg(rgb, *q).expect("jpeg encode");
            sizes[i] = B64.encode(&encoded).len();
        }
        let ladder = ladder_for(kind, &cfg);
        println!(
            "{:<12} {:<6} {:>6} {:>8} {:>9.3} {:>9} {:>9} {:>9} {:>9}  {:?}",
            name,
            kind_str,
            s.sampled,
            s.distinct,
            s.flat_ratio,
            sizes[0],
            sizes[1],
            sizes[2],
            sizes[3],
            ladder,
        );
    }
    println!();
    println!("口径：合成样本仅用于验证 flat/photo 判据可分离、体积随 q 单调下降；");
    println!("不得当作真实业务图片的收益数字；原图占比仍无数据（§15.4）。");
}
