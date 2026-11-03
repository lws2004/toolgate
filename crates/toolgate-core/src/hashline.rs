//! 行锚点：给 read 返回的每一行附加「行号#短哈希」，让 edit 可以
//! 用 7 个字符定位一行，而不必回传整段原文。
//!
//! # 这不是 pi 的特性
//!
//! pi 的 edit 用 oldText/newText 精确匹配（并做引号、破折号、空白的
//! 模糊归一化），并没有行哈希锚点。本模块是 toolgate 自己的设计，
//! 针对一个 pi 没优化的场景：**大文件里改少量行**。
//!
//! # 取舍
//!
//! 省下的是 edit 请求里的原文体积，付出的是 read 返回的体积：
//! 每行多约 7 字节（行号 + 井号 + 4 位哈希 + 分隔符）。因此它默认
//! 关闭，只在满足下列条件时值得开启：
//!
//! - 单文件超过约 500 行，且预计要反复改其中少量行；
//! - 会话里同一文件被读多次，oldText 的回传成本累计超过锚点开销。
//!
//! 反过来，小文件、一次性编辑、或整段重写，用 oldText 更省。

use std::fmt;

/// 锚点哈希长度（十六进制字符数）。4 位即 65536 种取值，
/// 配合行号定位，误命中概率可忽略。
pub const ANCHOR_HASH_LEN: usize = 4;

/// 一个行锚点，形如 12#f8a2。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Anchor {
    /// 1 起始的行号。
    pub line: usize,
    /// 该行内容的短哈希。
    pub hash: String,
}

/// 锚点解析失败的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnchorError {
    /// 缺少井号分隔符。
    MissingSeparator,
    /// 行号不是正整数。
    BadLineNumber(String),
    /// 哈希长度或字符集不对。
    BadHash(String),
}

impl fmt::Display for AnchorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSeparator => write!(f, "锚点格式应为 行号#哈希，例如 12#f8a2"),
            Self::BadLineNumber(s) => write!(f, "行号必须是正整数，收到：{s}"),
            Self::BadHash(s) => write!(
                f,
                "哈希应为 {ANCHOR_HASH_LEN} 位十六进制字符，收到：{s}"
            ),
        }
    }
}

impl std::error::Error for AnchorError {}

/// 计算单行内容的锚点哈希。
///
/// 归一化规则与 pi 的模糊匹配保持一致：去掉行尾空白，把智能引号、
/// Unicode 破折号与特殊空格折成 ASCII。这样即便编辑器重排了空白，
/// 锚点依然有效。
pub fn line_anchor(content: &str) -> String {
    let normalized = normalize_for_match(content.trim_end());
    let digest = crate::spill::sha256_short(normalized.as_bytes());
    digest[..ANCHOR_HASH_LEN].to_owned()
}

/// 把一段文本渲染成带锚点的形式，行号从 start_line 开始。
///
/// 输出每行形如 12#f8a2 加一个竖线分隔符，再接原始行内容。
pub fn render_anchored(lines: &[&str], start_line: usize) -> String {
    let mut out = String::with_capacity(lines.len() * 32);
    for (idx, line) in lines.iter().enumerate() {
        let no = start_line + idx;
        out.push_str(&format!("{no}#{}|{line}\n", line_anchor(line)));
    }
    out
}

/// 校验某行内容是否与锚点哈希一致。
pub fn verify_anchor(content: &str, anchor: &Anchor) -> bool {
    line_anchor(content) == anchor.hash
}

/// 解析 12#f8a2 形式的锚点。
pub fn parse_anchor(raw: &str) -> Result<Anchor, AnchorError> {
    let trimmed = raw.trim();
    let (line_part, hash_part) = trimmed
        .split_once('#')
        .ok_or(AnchorError::MissingSeparator)?;
    let line: usize = line_part
        .trim()
        .parse()
        .map_err(|_| AnchorError::BadLineNumber(line_part.to_owned()))?;
    if line == 0 {
        return Err(AnchorError::BadLineNumber(line_part.to_owned()));
    }
    let hash = hash_part.trim().to_ascii_lowercase();
    if hash.len() != ANCHOR_HASH_LEN || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(AnchorError::BadHash(hash));
    }
    Ok(Anchor { line, hash })
}

/// 把文本归一化成可比较形式：智能引号、破折号、特殊空格折成 ASCII。
fn normalize_for_match(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}' => '-',
            '\u{00A0}' | '\u{2007}' | '\u{202F}' => ' ',
            other => other,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_anchor() {
        let a = parse_anchor("12#f8a2").expect("valid");
        assert_eq!(a.line, 12);
        assert_eq!(a.hash, "f8a2");
    }

    #[test]
    fn rejects_missing_separator() {
        assert_eq!(parse_anchor("12f8a2"), Err(AnchorError::MissingSeparator));
    }

    #[test]
    fn rejects_zero_line() {
        assert!(matches!(
            parse_anchor("0#f8a2"),
            Err(AnchorError::BadLineNumber(_))
        ));
    }

    #[test]
    fn rejects_bad_hash() {
        assert!(matches!(
            parse_anchor("3#zzzz"),
            Err(AnchorError::BadHash(_))
        ));
        assert!(matches!(parse_anchor("3#abc"), Err(AnchorError::BadHash(_))));
    }

    #[test]
    fn anchor_is_stable_across_trailing_whitespace() {
        let a = line_anchor("    let x = 1;");
        let b = line_anchor("    let x = 1;   ");
        assert_eq!(a, b, "行尾空白不应影响锚点");
    }

    #[test]
    fn anchor_changes_when_content_changes() {
        let a = line_anchor("let x = 1;");
        let b = line_anchor("let x = 2;");
        assert_ne!(a, b);
    }

    #[test]
    fn anchor_survives_smart_quotes() {
        let a = line_anchor("say \"hi\"");
        let b = line_anchor("say \u{201C}hi\u{201D}");
        assert_eq!(a, b);
    }

    #[test]
    fn render_includes_line_numbers() {
        let text = render_anchored(&["a", "b"], 5);
        assert!(text.starts_with("5#"));
        assert!(text.contains("6#"));
    }

    #[test]
    fn verify_matches_rendered_anchor() {
        let line = "fn main() {}";
        let hash = line_anchor(line);
        let anchor = Anchor {
            line: 1,
            hash: hash.clone(),
        };
        assert!(verify_anchor(line, &anchor));
        assert!(!verify_anchor("fn other() {}", &anchor));
    }
}
