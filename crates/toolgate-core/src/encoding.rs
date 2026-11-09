//! 文件编码的检测与保真转换。
//!
//! 工具不能假设文件是 UTF-8。真实世界里存在 GBK 的旧代码、Shift-JIS 的
//! 配置文件、带 BOM 的 UTF-8、UTF-16 的 Windows 导出。
//!
//! 策略是**检测 → 解码 → 处理 → 按原编码写回**，而不是「不是 UTF-8 就拒绝」。
//! 唯一必须拒绝的情况是写回时目标编码无法表示新内容（例如往 GBK 文件里
//! 写 emoji），那时明确报错而不是静默替换成问号。

use encoding_rs::{Encoding, UTF_8, UTF_16BE, UTF_16LE};

/// 识别出的文件编码，附带 BOM 信息。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileEncoding {
    encoding: &'static Encoding,
    bom: Option<&'static [u8]>,
}

impl Default for FileEncoding {
    fn default() -> Self {
        Self {
            encoding: UTF_8,
            bom: None,
        }
    }
}

impl FileEncoding {
    /// 从字节内容检测编码。
    ///
    /// 顺序：BOM → 严格 UTF-8 → 统计检测 → 回退。
    pub fn detect(bytes: &[u8]) -> Self {
        // 1. BOM 优先，它是最强的信号。
        if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
            return Self {
                encoding: UTF_8,
                bom: Some(&[0xEF, 0xBB, 0xBF]),
            };
        }
        if bytes.starts_with(&[0xFF, 0xFE]) {
            return Self {
                encoding: UTF_16LE,
                bom: Some(&[0xFF, 0xFE]),
            };
        }
        if bytes.starts_with(&[0xFE, 0xFF]) {
            return Self {
                encoding: UTF_16BE,
                bom: Some(&[0xFE, 0xFF]),
            };
        }

        // 2. 严格 UTF-8 能过就是 UTF-8——最常见也最可靠的判定。
        if std::str::from_utf8(bytes).is_ok() {
            return Self::default();
        }

        // 3. 统计检测。给足样本，短文件容易误判。
        let mut detector = chardetng::EncodingDetector::new();
        detector.feed(bytes, true);
        let guessed = detector.guess(None, true);
        Self {
            encoding: guessed,
            bom: None,
        }
    }

    /// 该编码是否带 BOM。
    pub fn has_bom(&self) -> bool {
        self.bom.is_some()
    }

    /// 编码名，用于回传给调用方。
    pub fn name(&self) -> &'static str {
        self.encoding.name()
    }

    /// 解码成文本。返回 (文本, 是否出现替换字符)。
    ///
    /// 有替换字符意味着字节无法在该编码下解释，此时文本不是原文的忠实表示。
    pub fn decode(&self, bytes: &[u8]) -> (String, bool) {
        let body = match self.bom {
            Some(bom) if bytes.starts_with(bom) => &bytes[bom.len()..],
            _ => bytes,
        };
        let (text, _, had_errors) = self.encoding.decode(body);
        (text.into_owned(), had_errors)
    }

    /// 按原编码编码回去。
    ///
    /// 无法无损表示时报错——不能静默把 emoji 换成问号写进 GBK 文件。
    pub fn encode(&self, text: &str) -> Result<Vec<u8>, String> {
        let (body, _, had_errors) = self.encoding.encode(text);
        if had_errors {
            return Err(format!(
                "内容中有字符无法用 {} 表示。请改用 UTF-8 保存，或删除这些字符。",
                self.name()
            ));
        }
        let mut out = Vec::with_capacity(body.len() + 3);
        if let Some(bom) = self.bom {
            out.extend_from_slice(bom);
        }
        out.extend_from_slice(&body);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_ascii_is_utf8() {
        let enc = FileEncoding::detect(b"hello world");
        assert_eq!(enc.name(), "UTF-8");
        assert!(!enc.has_bom());
    }

    #[test]
    fn utf8_bom_is_detected_and_preserved() {
        let bytes = b"\xEF\xBB\xBFhello";
        let enc = FileEncoding::detect(bytes);
        assert_eq!(enc.name(), "UTF-8");
        assert!(enc.has_bom());

        let (text, had_errors) = enc.decode(bytes);
        assert_eq!(text, "hello", "BOM 不应出现在文本里");
        assert!(!had_errors);

        let back = enc.encode(&text).expect("encode");
        assert_eq!(back, bytes, "写回必须保留 BOM");
    }

    #[test]
    fn gbk_is_detected_and_round_trips() {
        // 中文测试 的 GBK 编码
        let gbk_bytes = [0xD6, 0xD0, 0xCE, 0xC4, 0xB2, 0xE2, 0xCA, 0xD4];
        let enc = FileEncoding::detect(&gbk_bytes);
        assert_ne!(enc.name(), "UTF-8", "不应误判为 UTF-8");

        let (text, had_errors) = enc.decode(&gbk_bytes);
        assert!(!had_errors, "GBK 字节应能无损解码");
        assert_eq!(text, "中文测试");

        let back = enc.encode(&text).expect("encode");
        assert_eq!(back, gbk_bytes, "写回必须与原字节一致");
    }

    #[test]
    fn utf8_chinese_round_trips() {
        let bytes = "中文测试\n".as_bytes();
        let enc = FileEncoding::detect(bytes);
        assert_eq!(enc.name(), "UTF-8");
        let (text, _) = enc.decode(bytes);
        assert_eq!(enc.encode(&text).unwrap(), bytes);
    }

    #[test]
    fn unencodable_content_is_rejected_not_mangled() {
        let gbk_bytes = [0xD6, 0xD0];
        let enc = FileEncoding::detect(&gbk_bytes);
        // 往 GBK 文件里写 emoji 应当报错，而不是静默变成问号。
        let err = enc.encode("中文🚀").unwrap_err();
        assert!(err.contains("无法用"), "错误信息要说明原因：{err}");
    }
}
