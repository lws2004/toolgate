//! search：内容检索与文件枚举的聚合入口。
//!
//! 把 grep 与 find 合成一个工具，靠 mode 区分。这样做不是为了省一个
//! 工具名，而是为了让「超过阈值就落盘」这条治理规则只有一处实现。
//!
//! 遍历与匹配都用纯 Rust 库（ignore + regex），不依赖系统是否装了
//! ripgrep 或 fd，因此行为在各平台一致。

use std::path::Path;

use ignore::WalkBuilder;
use serde_json::{Value, json};

use super::{ToolContext, ToolResponse, ToolSpec, optional_str, optional_usize, required_str};
use crate::ToolError;
use crate::truncate::{MAX_MATCH_LINE_LENGTH, truncate_head};

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: "search",
        description: "检索文件内容或枚举文件路径。mode=content 时按正则搜内容（等价 grep -rn），\
                      mode=files 时按通配符枚举文件名（等价 fd）。结果超过 2000 行 / 50KB 时\
                      保留头部、完整结果落盘并返回路径。",
        input_schema: json!({
            "type": "object",
            "properties": {
                "mode": {
                    "type": "string",
                    "enum": ["content", "files"],
                    "description": "content 搜内容，files 枚举文件路径"
                },
                "query": {
                    "type": "string",
                    "description": "content 模式是正则表达式；files 模式是文件名子串或通配符"
                },
                "path": {
                    "type": "string",
                    "description": "检索起点目录或文件，默认工作目录"
                },
                "max_results": {
                    "type": "integer",
                    "description": "最多返回多少条结果，默认 200",
                    "minimum": 1
                }
            },
            "required": ["mode", "query"]
        }),
    }
}

pub async fn run(args: &Value, ctx: &ToolContext) -> Result<ToolResponse, ToolError> {
    let mode = required_str(args, "mode")?;
    let query = required_str(args, "query")?;
    let root = ctx.resolve(optional_str(args, "path").unwrap_or("."));
    let max_results = optional_usize(args, "max_results").unwrap_or(200);

    let (raw, hit_count) = match mode {
        "content" => search_content(&root, query, max_results)?,
        "files" => list_files(&root, query, max_results)?,
        other => {
            return Err(ToolError::BadArgument(format!(
                "mode 只能是 content 或 files，收到 {other}"
            )));
        }
    };

    let truncation = truncate_head(&raw, ctx.limits);
    let mut body = truncation.content.clone();
    let mut spilled = Value::Null;

    if truncation.truncated {
        let spill = ctx.spill.persist(&format!("search-{mode}"), &raw)?;
        body.push_str(&format!(
            "\n[结果已截断：命中 {hit_count} 处，仅展示前 {} 行。{}]",
            truncation.output_lines,
            spill.hint()
        ));
        spilled = json!({
            "path": spill.path.display().to_string(),
            "bytes": spill.bytes,
            "lines": spill.lines,
        });
    }

    Ok(ToolResponse::text(body).with_meta(json!({
        "mode": mode,
        "query": query,
        "hits": hit_count,
        "truncated": truncation.truncated,
        "spill": spilled,
    })))
}

/// 内容检索：正则逐行匹配，返回 路径:行号:内容。
fn search_content(root: &Path, pattern: &str, max_results: usize) -> Result<(String, usize), ToolError> {
    let re = regex::Regex::new(pattern)
        .map_err(|e| ToolError::BadArgument(format!("正则无效：{e}")))?;

    let mut out = String::new();
    let mut hits = 0usize;

    for entry in walker(root) {
        if hits >= max_results {
            break;
        }
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        let Ok(content) = std::fs::read_to_string(path) else {
            continue; // 二进制或不可读文件跳过
        };
        for (idx, line) in content.lines().enumerate() {
            if hits >= max_results {
                break;
            }
            if re.is_match(line) {
                hits += 1;
                let shown = truncate_line(line);
                out.push_str(&format!("{}:{}:{}\n", path.display(), idx + 1, shown));
            }
        }
    }

    if out.is_empty() {
        out.push_str("(无匹配)\n");
    }
    Ok((out, hits))
}

/// 文件枚举：按子串或通配符匹配文件名。
fn list_files(root: &Path, pattern: &str, max_results: usize) -> Result<(String, usize), ToolError> {
    let glob = glob_to_regex(pattern);
    let re = regex::Regex::new(&glob)
        .map_err(|e| ToolError::BadArgument(format!("通配符无效：{e}")))?;

    let mut out = String::new();
    let mut hits = 0usize;

    for entry in walker(root) {
        if hits >= max_results {
            break;
        }
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if re.is_match(name) {
            hits += 1;
            out.push_str(&format!("{}\n", path.display()));
        }
    }

    if out.is_empty() {
        out.push_str("(无匹配)\n");
    }
    Ok((out, hits))
}

/// 用 ignore 的遍历器，自动遵守 .gitignore 并跳过隐藏目录。
fn walker(root: &Path) -> ignore::Walk {
    WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(false)
        .build()
}

/// 单行过长时从中间省略，避免一行吃掉整个预算。
fn truncate_line(line: &str) -> String {
    if line.len() <= MAX_MATCH_LINE_LENGTH {
        return line.to_owned();
    }
    let head: String = line.chars().take(MAX_MATCH_LINE_LENGTH / 2).collect();
    let tail: String = line
        .chars()
        .rev()
        .take(MAX_MATCH_LINE_LENGTH / 2)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{head}...{tail}")
}

/// 把 * 与 ? 通配符转成锚定正则；不含通配符时按子串匹配。
fn glob_to_regex(pattern: &str) -> String {
    if !pattern.contains('*') && !pattern.contains('?') {
        return format!("(?i){}", regex::escape(pattern));
    }
    let mut out = String::from("(?i)^");
    for ch in pattern.chars() {
        match ch {
            '*' => out.push_str(".*"),
            '?' => out.push('.'),
            other => out.push_str(&regex::escape(&other.to_string())),
        }
    }
    out.push('$');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_without_wildcard_is_substring() {
        assert_eq!(glob_to_regex("main"), "(?i)main");
    }

    #[test]
    fn glob_with_wildcard_is_anchored() {
        assert_eq!(glob_to_regex("*.rs"), "(?i)^.*\\.rs$");
    }

    #[test]
    fn long_line_is_collapsed() {
        let line = "a".repeat(2000);
        let out = truncate_line(&line);
        assert!(out.len() < 600);
        assert!(out.contains("..."));
    }
}
