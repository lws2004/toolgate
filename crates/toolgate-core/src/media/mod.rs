//! 附件处理流水线。
//!
//! 云端下发的附件（或工具引用到的文件）在进入模型上下文前统一过这条
//! 流水线：类型嗅探 → 图片多级降级压缩 → 内容寻址去重。
//!
//! # 与 pi 的差异
//!
//! pi 的图片上限是 2000px 与 4.5MB base64，输出只有 PNG/JPEG。
//! 本模块把默认值调到 1568px（主流视觉编码器的训练分辨率上限，
//! 再大通常只是白花 token）、优先 WebP（同质量下体积约为 JPEG 的
//! 六到七成），并在 1.2MB base64 处硬熔断。

pub mod image_pipeline;

use std::path::Path;

use crate::ToolError;

/// 常见图片格式的魔数表：前若干字节 → MIME。
const IMAGE_SIGNATURES: &[(&[u8], &str)] = &[
    (b"\x89PNG\r\n\x1a\n", "image/png"),
    (b"\xff\xd8\xff", "image/jpeg"),
    (b"GIF87a", "image/gif"),
    (b"GIF89a", "image/gif"),
    (b"BM", "image/bmp"),
];

/// 嗅探文件是否为图片，返回 MIME。只读前 4KB。
pub async fn detect_image_mime(path: &Path) -> Result<Option<String>, ToolError> {
    let head = match read_head(path, 4096).await {
        Ok(b) => b,
        Err(_) => return Ok(None),
    };
    Ok(sniff_mime(&head))
}

/// 从字节头嗅探 MIME。
pub fn sniff_mime(head: &[u8]) -> Option<String> {
    for (sig, mime) in IMAGE_SIGNATURES {
        if head.starts_with(sig) {
            return Some((*mime).to_owned());
        }
    }
    // WebP：RIFF....WEBP
    if head.len() >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        return Some("image/webp".to_owned());
    }
    None
}

/// 粗判二进制：前 8KB 里出现 NUL 字节即视为二进制。
pub fn looks_binary(bytes: &[u8]) -> bool {
    let probe = &bytes[..bytes.len().min(8192)];
    probe.contains(&0)
}

async fn read_head(path: &Path, n: usize) -> std::io::Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    let mut f = tokio::fs::File::open(path).await?;
    let mut buf = vec![0u8; n];
    let read = f.read(&mut buf).await?;
    buf.truncate(read);
    Ok(buf)
}

/// 把图片交给压缩流水线，产出 image content block。
pub async fn prepare_image(
    path: &Path,
    mime: &str,
) -> Result<crate::tools::ToolResponse, ToolError> {
    let bytes = tokio::fs::read(path).await?;
    image_pipeline::prepare(&bytes, mime, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_png() {
        let head = b"\x89PNG\r\n\x1a\n\x00\x00";
        assert_eq!(sniff_mime(head).as_deref(), Some("image/png"));
    }

    #[test]
    fn sniffs_jpeg() {
        assert_eq!(sniff_mime(b"\xff\xd8\xff\xe0").as_deref(), Some("image/jpeg"));
    }

    #[test]
    fn sniffs_webp() {
        let mut head = b"RIFF".to_vec();
        head.extend_from_slice(&[0, 0, 0, 0]);
        head.extend_from_slice(b"WEBP");
        assert_eq!(sniff_mime(&head).as_deref(), Some("image/webp"));
    }

    #[test]
    fn text_is_not_image() {
        assert_eq!(sniff_mime(b"hello world"), None);
    }

    #[test]
    fn detects_binary() {
        assert!(looks_binary(b"abc\x00def"));
        assert!(!looks_binary(b"plain text"));
    }
}
