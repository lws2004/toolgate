//! 工具输出的双阈值截断。
//!
//! 两条独立阈值先到先触发：行数与字节数。截断永不返回半行内容
//! （尾截断的首行可能被切，此时 last_line_partial 为真）。
//!
//! 头部截断用于文件读取（读者想看开头），尾部截断用于命令输出
//! （读者想看报错与最终结果）。

/// 单次工具返回允许的最大行数。
pub const DEFAULT_MAX_LINES: usize = 2_000;
/// 单次工具返回允许的最大字节数。
pub const DEFAULT_MAX_BYTES: usize = 50 * 1024;
/// 检索结果单行允许的最大长度，超出则从行中截断。
pub const MAX_MATCH_LINE_LENGTH: usize = 500;

/// 触发截断的阈值种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TruncatedBy {
    Lines,
    Bytes,
}

/// 截断选项，缺省取 DEFAULT_MAX_LINES 与 DEFAULT_MAX_BYTES。
#[derive(Debug, Clone, Copy)]
pub struct TruncationOptions {
    pub max_lines: usize,
    pub max_bytes: usize,
}

impl Default for TruncationOptions {
    fn default() -> Self {
        Self {
            max_lines: DEFAULT_MAX_LINES,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

impl TruncationOptions {
    pub fn new(max_lines: usize, max_bytes: usize) -> Self {
        Self {
            max_lines,
            max_bytes,
        }
    }
}

/// 一次截断的完整结果，保留原始规模以便调用方判断是否需要落盘。
#[derive(Debug, Clone)]
pub struct TruncationResult {
    pub content: String,
    pub truncated: bool,
    pub truncated_by: Option<TruncatedBy>,
    pub total_lines: usize,
    pub total_bytes: usize,
    pub output_lines: usize,
    pub output_bytes: usize,
    pub last_line_partial: bool,
    pub first_line_exceeds_limit: bool,
    pub max_lines: usize,
    pub max_bytes: usize,
}

impl TruncationResult {
    fn untruncated(content: &str, options: TruncationOptions) -> Self {
        let lines = count_lines(content);
        Self {
            content: content.to_owned(),
            truncated: false,
            truncated_by: None,
            total_lines: lines,
            total_bytes: content.len(),
            output_lines: lines,
            output_bytes: content.len(),
            last_line_partial: false,
            first_line_exceeds_limit: false,
            max_lines: options.max_lines,
            max_bytes: options.max_bytes,
        }
    }
}

/// 统计行数：末尾不带换行的最后一段也算一行。
fn count_lines(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    let newlines = text.bytes().filter(|b| *b == b'\n').count();
    if text.ends_with('\n') {
        newlines
    } else {
        newlines + 1
    }
}

/// 保留开头的截断，用于文件读取。
pub fn truncate_head(content: &str, options: TruncationOptions) -> TruncationResult {
    let total_bytes = content.len();
    let total_lines = count_lines(content);
    if total_bytes <= options.max_bytes && total_lines <= options.max_lines {
        return TruncationResult::untruncated(content, options);
    }

    let mut out = String::new();
    let mut lines = 0usize;
    let mut bytes = 0usize;
    let mut truncated_by = TruncatedBy::Lines;

    for line in content.split_inclusive('\n') {
        if lines >= options.max_lines {
            truncated_by = TruncatedBy::Lines;
            break;
        }
        if bytes + line.len() > options.max_bytes {
            // 首行就超字节上限：返回空内容，由调用方提示改用分段读取。
            if lines == 0 {
                return TruncationResult {
                    content: String::new(),
                    truncated: true,
                    truncated_by: Some(TruncatedBy::Bytes),
                    total_lines,
                    total_bytes,
                    output_lines: 0,
                    output_bytes: 0,
                    last_line_partial: false,
                    first_line_exceeds_limit: true,
                    max_lines: options.max_lines,
                    max_bytes: options.max_bytes,
                };
            }
            truncated_by = TruncatedBy::Bytes;
            break;
        }
        out.push_str(line);
        lines += 1;
        bytes += line.len();
    }

    TruncationResult {
        content: out,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines,
        total_bytes,
        output_lines: lines,
        output_bytes: bytes,
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines: options.max_lines,
        max_bytes: options.max_bytes,
    }
}

/// 保留尾部的截断，用于命令输出（错误栈与最终结果在尾部）。
pub fn truncate_tail(content: &str, options: TruncationOptions) -> TruncationResult {
    let total_bytes = content.len();
    let total_lines = count_lines(content);
    if total_bytes <= options.max_bytes && total_lines <= options.max_lines {
        return TruncationResult::untruncated(content, options);
    }

    let all_lines: Vec<&str> = content.split_inclusive('\n').collect();
    let mut kept: Vec<&str> = Vec::new();
    let mut bytes = 0usize;
    let mut truncated_by = TruncatedBy::Lines;

    for line in all_lines.iter().rev() {
        if kept.len() >= options.max_lines {
            truncated_by = TruncatedBy::Lines;
            break;
        }
        if bytes + line.len() > options.max_bytes {
            if kept.is_empty() {
                // 末行本身超限：按字节切尾，标记为半行。
                let start = line.len().saturating_sub(options.max_bytes);
                let tail = &line[start..];
                return TruncationResult {
                    content: tail.to_owned(),
                    truncated: true,
                    truncated_by: Some(TruncatedBy::Bytes),
                    total_lines,
                    total_bytes,
                    output_lines: 1,
                    output_bytes: tail.len(),
                    last_line_partial: true,
                    first_line_exceeds_limit: false,
                    max_lines: options.max_lines,
                    max_bytes: options.max_bytes,
                };
            }
            truncated_by = TruncatedBy::Bytes;
            break;
        }
        kept.push(line);
        bytes += line.len();
    }

    kept.reverse();
    let out: String = kept.concat();
    TruncationResult {
        content: out,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines,
        total_bytes,
        output_lines: kept.len(),
        output_bytes: bytes,
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines: options.max_lines,
        max_bytes: options.max_bytes,
    }
}

/// 单行超长时按字节上限截断该行（§9.1），保证不切在 UTF-8 字符中间。
///
/// 与 `truncate_head` 的分工：那个按「行」保留前缀、首行超限时返回空内容；
/// 这里按「字节」切一行，用于 offset 续读无能为力的压缩 JSON / minified 代码。
pub fn truncate_line_head(line: &str, max_bytes: usize) -> &str {
    if line.len() <= max_bytes {
        return line;
    }
    let mut end = max_bytes;
    while end > 0 && !line.is_char_boundary(end) {
        end -= 1;
    }
    &line[..end]
}

/// 把字节数格式化为人类可读形式，用于截断提示。
pub fn format_size(bytes: usize) -> String {
    const KB: usize = 1024;
    const MB: usize = 1024 * 1024;
    if bytes >= MB {
        format!("{:.1}MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1}KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes}B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_content_is_not_truncated() {
        let r = truncate_head("a\nb\nc", TruncationOptions::default());
        assert!(!r.truncated);
        assert_eq!(r.total_lines, 3);
        assert_eq!(r.content, "a\nb\nc");
    }

    #[test]
    fn head_truncation_keeps_first_lines() {
        let text = "l1\nl2\nl3\nl4\nl5\n";
        let r = truncate_head(text, TruncationOptions::new(2, 1024));
        assert!(r.truncated);
        assert_eq!(r.truncated_by, Some(TruncatedBy::Lines));
        assert_eq!(r.content, "l1\nl2\n");
        assert_eq!(r.total_lines, 5);
    }

    #[test]
    fn tail_truncation_keeps_last_lines() {
        let text = "l1\nl2\nl3\nl4\nl5\n";
        let r = truncate_tail(text, TruncationOptions::new(2, 1024));
        assert!(r.truncated);
        assert_eq!(r.content, "l4\nl5\n");
    }

    #[test]
    fn byte_limit_wins_over_line_limit() {
        let text = "0123456789\n";
        let r = truncate_head(text, TruncationOptions::new(100, 5));
        assert!(r.truncated);
        assert_eq!(r.truncated_by, Some(TruncatedBy::Bytes));
        assert!(r.first_line_exceeds_limit);
        assert_eq!(r.content, "");
    }

    /// §9.1：单行超长按字节截断，且不切在 UTF-8 字符中间。
    #[test]
    fn line_truncation_keeps_byte_budget_on_char_boundary() {
        let ascii = "x".repeat(100);
        assert_eq!(truncate_line_head(&ascii, 40).len(), 40);
        assert_eq!(truncate_line_head(&ascii, 100), ascii);
        assert_eq!(truncate_line_head(&ascii, 200), ascii, "未超限时原样返回");

        // 每个汉字 3 字节：上限 4 只能落到第 1 个字符（3 字节），不能切出半个字符。
        let zh = "中文测试";
        let kept = truncate_line_head(zh, 4);
        assert_eq!(kept, "中");
        assert_eq!(kept.len(), 3);
        assert_eq!(truncate_line_head(zh, 6), "中文");
    }

    #[test]
    fn tail_truncation_returns_partial_last_line() {
        let text = "short\n0123456789";
        let r = truncate_tail(text, TruncationOptions::new(100, 4));
        assert!(r.truncated);
        assert!(r.last_line_partial);
        assert_eq!(r.content, "6789");
    }
}
