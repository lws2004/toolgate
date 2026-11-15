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

/// 头 + 关键行 + 尾采样（§4.0a 治理时间预算）。
///
/// 长文先减输入再谈超时：保留开头、结尾与中间的关键行（错误、路径、异常），
/// 略去其余，使送给槽位的输入落到 `max_bytes` 量级。按行边界切，不产生半行；
///
/// 返回文本自带省略说明，调用方无需再拼提示。输入未超限时原样返回。
pub fn sample_ends(content: &str, max_bytes: usize) -> String {
    if content.len() <= max_bytes {
        return content.to_owned();
    }
    let lines: Vec<&str> = content.split_inclusive('\n').collect();
    let n = lines.len();

    // 头尾各 2/5、关键行 1/5；关键行额度未用尽时回收给头尾（§4.0a：不让预算空转）。
    let head_budget = max_bytes * 2 / 5;
    let tail_budget = max_bytes * 2 / 5;
    let key_budget = max_bytes - head_budget - tail_budget;

    let mut head_cnt = 0usize;
    let mut head_bytes = 0usize;
    for line in &lines {
        if head_bytes + line.len() > head_budget {
            break;
        }
        head_bytes += line.len();
        head_cnt += 1;
    }
    let mut tail_cnt = 0usize;
    let mut tail_bytes = 0usize;
    for line in lines.iter().rev() {
        if tail_cnt + head_cnt >= n || tail_bytes + line.len() > tail_budget {
            break;
        }
        tail_bytes += line.len();
        tail_cnt += 1;
    }
    let mid_start = head_cnt;
    let mid_end = n - tail_cnt;

    let mut key_idx: Vec<usize> = Vec::new();
    let mut key_bytes = 0usize;
    for (i, line) in lines.iter().enumerate().take(mid_end).skip(mid_start) {
        if key_bytes + line.len() > key_budget {
            break;
        }
        if is_key_line(line) {
            key_idx.push(i);
            key_bytes += line.len();
        }
    }

    if head_cnt == 0 && tail_cnt == 0 && key_idx.is_empty() {
        // 首行本身就超预算：按字节切一行，避免返回空内容。
        return truncate_line_head(content, max_bytes).to_owned();
    }

    // 关键行额度剩余时，从中间区两侧补给头/尾（跳过已选关键行，避免重复）。
    // 顺序有意不对称：先让头侧吃满（读者看头优先），再给尾侧；两侧都放不下才停。
    let key_set: std::collections::HashSet<usize> = key_idx.iter().copied().collect();
    let mut leftover = key_budget.saturating_sub(key_bytes);
    let mut extra_head: Vec<usize> = Vec::new();
    let mut extra_tail: Vec<usize> = Vec::new();
    let mut lo = mid_start;
    let mut hi = mid_end;
    while leftover > 0 && lo < hi {
        while lo < hi && key_set.contains(&lo) {
            lo += 1;
        }
        if lo >= hi || lines[lo].len() > leftover {
            break;
        }
        extra_head.push(lo);
        leftover -= lines[lo].len();
        lo += 1;
    }
    while leftover > 0 && lo < hi {
        while hi > lo && key_set.contains(&(hi - 1)) {
            hi -= 1;
        }
        if hi <= lo || lines[hi - 1].len() > leftover {
            break;
        }
        extra_tail.push(hi - 1);
        leftover -= lines[hi - 1].len();
        hi -= 1;
    }

    let mut out = String::new();
    for line in lines.iter().take(head_cnt) {
        out.push_str(line);
    }
    for &i in &extra_head {
        out.push_str(lines[i]);
    }
    if !key_idx.is_empty() {
        out.push_str("\n…（关键行：\n");
        for &i in &key_idx {
            out.push_str(lines[i]);
            if !lines[i].ends_with('\n') {
                out.push('\n');
            }
        }
        out.push('）');
    }
    let kept_lines = head_cnt + extra_head.len() + key_idx.len() + extra_tail.len() + tail_cnt;
    let omitted_lines = n.saturating_sub(kept_lines);
    let omitted_bytes = content.len().saturating_sub(out.len());
    out.push_str(&format!(
        "\n…（已采样：略去 {omitted_lines} 行 / {} 字节，完整原文见指针）\n",
        format_size(omitted_bytes)
    ));
    for &i in extra_tail.iter().rev() {
        out.push_str(lines[i]);
    }
    for line in lines.iter().take(n).skip(mid_end) {
        out.push_str(line);
    }
    out
}

/// 按字节上限保留文本开头（列表读者看头更合理）：超限时在行边界截断并附略去说明。
/// 返回 (正文, Some(保留行数))/未截断时 (原文, None)。与行数阈值互补，防「行数合规但字节超标」。
pub fn clamp_head_bytes(content: &str, max_bytes: usize) -> (String, Option<usize>) {
    if content.len() <= max_bytes {
        return (content.to_owned(), None);
    }
    let mut out = String::new();
    let mut bytes = 0usize;
    let mut kept = 0usize;
    for line in content.split_inclusive('\n') {
        if bytes + line.len() > max_bytes {
            break;
        }
        out.push_str(line);
        bytes += line.len();
        kept += 1;
    }
    if kept == 0 {
        return (truncate_line_head(content, max_bytes).to_owned(), Some(0));
    }
    let total_lines = content.lines().count();
    out.push_str(&format!(
        "\n…（已按 {} 字节上限截断：保留前 {kept} 行 / 共 {total_lines} 行，原 {}）",
        format_size(max_bytes),
        format_size(content.len())
    ));
    (out, Some(kept))
}

/// 关键行判据：错误、异常、路径与退出线索。大小写不敏感，命中即保留。
fn is_key_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    [
        "error",
        "failed",
        "failure",
        "panic",
        "exception",
        "traceback",
        "errno",
        "错误",
        "失败",
        "异常",
        "/users/",
        "/usr/",
        "/var/",
        ".rs:",
        ".ts:",
        ".py:",
        ".js:",
    ]
    .iter()
    .any(|k| lower.contains(k))
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

    #[test]
    fn sample_ends_keeps_head_tail_and_within_budget() {
        let mut text = String::new();
        for i in 0..4000 {
            text.push_str(&format!("line {i} padding padding padding padding\n"));
        }
        let r = sample_ends(&text, 32 * 1024);
        assert!(
            r.len() <= 32 * 1024 + 512,
            "采样不得明显超出预算：{}",
            r.len()
        );
        assert!(r.contains("已采样"));
        assert!(r.starts_with("line 0 "));
        assert!(r.ends_with("line 3999 padding padding padding padding\n"));
    }

    #[test]
    fn sample_ends_reclaims_unused_key_budget() {
        // 无关键行时 1/5 的关键行额度应回收给头尾，输出接近满额而非只剩 4/5。
        let mut text = String::new();
        for i in 0..4000 {
            text.push_str(&format!("line {i} padding padding padding padding\n"));
        }
        let r = sample_ends(&text, 32 * 1024);
        assert!(r.len() >= 31 * 1024, "关键行额度应回收：{}", r.len());
        assert!(r.len() <= 32 * 1024 + 512);
        assert!(r.starts_with("line 0 "));
        assert!(r.ends_with("line 3999 padding padding padding padding\n"));
    }

    #[test]
    fn sample_ends_keeps_error_lines_from_middle() {
        let mut text = String::new();
        for i in 0..3000 {
            if i == 1500 {
                text.push_str("ERROR: connection refused\n");
            } else {
                text.push_str(&format!("filler line {i}\n"));
            }
        }
        let r = sample_ends(&text, 8 * 1024);
        assert!(r.contains("ERROR: connection refused"));
        assert!(r.contains("已采样"));
    }

    #[test]
    fn sample_ends_returns_short_content_unchanged() {
        let text = "a\nb\nc";
        assert_eq!(sample_ends(text, 1024), text);
    }

    #[test]
    fn clamp_head_bytes_returns_zero_when_first_line_exceeds() {
        // 首行本身就超上限：无整行可留，回退按字节切一行并报 kept=0（提示调用方「保留 0 行」）。
        let text = "x".repeat(200);
        let (out, kept) = clamp_head_bytes(&text, 16);
        assert_eq!(kept, Some(0));
        assert_eq!(out.len(), 16);
        assert!(out.bytes().all(|b| b == b'x'));
    }
}
