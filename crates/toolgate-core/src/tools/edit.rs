//! edit：局部修改，两种寻址模式。
//!
//! # 模式 A：文本匹配（默认）
//!
//! 传 edits 数组，每项是 oldText / newText。匹配顺序是：
//! 精确匹配 → 归一化匹配（智能引号、Unicode 破折号、行尾空白）。
//! 匹配到多处时报错，要求扩大 oldText 上下文，绝不猜。
//!
//! # 模式 B：行锚点
//!
//! 传 anchor（形如 12#f8a2）与 newText。锚点哈希与当前行不符时拒绝
//! 写入——这正是它存在的意义：文件被别人改过，就宁可失败也不误改。
//!
//! 两种模式都基于**原始文件**计算，多笔编辑互不累积偏移。

use serde_json::{Value, json};

use super::{ToolContext, ToolResponse, ToolSpec, optional_str, required_str};
use crate::ToolError;
use crate::hashline::{line_anchor, parse_anchor};

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: "edit",
        description: "局部修改文件。两种用法：\
                      (1) 传 edits:[{oldText,newText}]，按原文精确匹配替换，匹配多处会报错；\
                      (2) 传 anchor:\"行号#哈希\" 与 newText，按 read 返回的锚点定位单行，\
                      文件已变动时拒绝写入。局部修改一律用 edit，不要用 write 覆盖。",
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "目标文件路径" },
                "edits": {
                    "type": "array",
                    "description": "文本匹配模式的编辑列表。各项都基于原始文件匹配，不要重叠",
                    "items": {
                        "type": "object",
                        "properties": {
                            "oldText": { "type": "string", "description": "要替换的原文，需在文件中唯一" },
                            "newText": { "type": "string", "description": "替换后的内容，删除时传空串" }
                        },
                        "required": ["oldText", "newText"]
                    }
                },
                "anchor": {
                    "type": "string",
                    "description": "行锚点模式：read 返回的 行号#哈希，例如 12#f8a2"
                },
                "newText": {
                    "type": "string",
                    "description": "行锚点模式下该行的新内容（不含换行）"
                }
            },
            "required": ["path"]
        }),
    }
}

pub async fn run(args: &Value, ctx: &ToolContext) -> Result<ToolResponse, ToolError> {
    let path = ctx.resolve(required_str(args, "path")?);
    let original = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| ToolError::Failed(format!("无法读取 {}：{e}", path.display())))?;

    let updated = if let Some(anchor_raw) = optional_str(args, "anchor") {
        apply_anchor_mode(&original, anchor_raw, optional_str(args, "newText").unwrap_or(""))?
    } else if let Some(edits) = args.get("edits").and_then(Value::as_array) {
        apply_text_mode(&original, edits)?
    } else {
        return Err(ToolError::BadArgument(
            "需要提供 edits 数组（文本匹配模式），或 anchor + newText（行锚点模式）".to_owned(),
        ));
    };

    if updated == original {
        return Ok(ToolResponse::text("内容未变化，未写入。"));
    }

    // 落盘前留一份原文，供回滚。
    let snapshot = ctx.snapshots.snapshot(&path, &original).ok();

    tokio::fs::write(&path, &updated).await?;

    let diff = unified_diff(&original, &updated, &path.display().to_string());
    let snapshot_meta = snapshot
        .as_ref()
        .map(|s| json!(s.path.display().to_string()))
        .unwrap_or(json!(null));

    Ok(ToolResponse::text(diff).with_meta(json!({
        "path": path.display().to_string(),
        "changed": true,
        "bytesBefore": original.len(),
        "bytesAfter": updated.len(),
        "snapshot": snapshot_meta,
    })))
}

/// 行锚点模式：校验哈希后替换单行。
fn apply_anchor_mode(original: &str, anchor_raw: &str, new_text: &str) -> Result<String, ToolError> {
    let anchor = parse_anchor(anchor_raw).map_err(|e| ToolError::BadArgument(e.to_string()))?;
    let mut lines: Vec<&str> = original.lines().collect();
    if anchor.line > lines.len() {
        return Err(ToolError::Failed(format!(
            "锚点行号 {} 超出文件行数 {}",
            anchor.line,
            lines.len()
        )));
    }
    let idx = anchor.line - 1;
    let current = lines[idx];
    if line_anchor(current) != anchor.hash {
        return Err(ToolError::Failed(format!(
            "锚点失配：第 {} 行当前哈希为 {}，与传入的 {} 不符。\
             文件可能已被改动，请重新 read 后再编辑。",
            anchor.line,
            line_anchor(current),
            anchor.hash
        )));
    }
    lines[idx] = new_text;

    let mut out = lines.join("\n");
    if original.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

/// 文本匹配模式：逐笔在原始文件上定位并替换。
fn apply_text_mode(original: &str, edits: &[Value]) -> Result<String, ToolError> {
    if edits.is_empty() {
        return Err(ToolError::BadArgument("edits 不能为空".to_owned()));
    }

    // 先收集所有替换区间，再统一应用，避免前一笔改动影响后一笔的匹配位置。
    let mut spans: Vec<(usize, usize, String)> = Vec::with_capacity(edits.len());
    for (i, e) in edits.iter().enumerate() {
        let old = e
            .get("oldText")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::BadArgument(format!("edits[{i}] 缺少 oldText")))?;
        let new = e
            .get("newText")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::BadArgument(format!("edits[{i}] 缺少 newText")))?;

        let (start, len) = locate(original, old).map_err(|msg| {
            ToolError::Failed(format!("edits[{i}] {msg}"))
        })?;
        spans.push((start, len, new.to_owned()));
    }

    // 检查重叠：区间排序后相邻不得相交。
    let mut sorted = spans.clone();
    sorted.sort_by_key(|(s, _, _)| *s);
    for w in sorted.windows(2) {
        let (s1, l1, _) = &w[0];
        let (s2, _, _) = &w[1];
        if s1 + l1 > *s2 {
            return Err(ToolError::BadArgument(
                "edits 存在重叠区间，请合并成一笔编辑".to_owned(),
            ));
        }
    }

    // 从后往前替换，保证前面的偏移不受影响。
    let mut out = original.to_owned();
    for (start, len, new) in sorted.iter().rev() {
        out.replace_range(*start..(*start + *len), new);
    }
    Ok(out)
}

/// 在原文中定位 oldText，返回 (起始字节偏移, 长度)。
/// 精确匹配优先，失败后尝试归一化匹配。
fn locate(original: &str, old: &str) -> Result<(usize, usize), String> {
    if old.is_empty() {
        return Err("oldText 不能为空".to_owned());
    }

    match original.match_indices(old).count() {
        1 => {
            let start = original.find(old).expect("already counted");
            return Ok((start, old.len()));
        }
        n if n > 1 => {
            return Err(format!(
                "在文件中匹配到 {n} 处，无法确定改哪一处。请扩大 oldText 的上下文使其唯一。"
            ));
        }
        _ => {}
    }

    // 归一化匹配：逐行比对，容忍引号/破折号/空白差异。
    let norm_old = normalize(old);
    let orig_lines: Vec<&str> = original.lines().collect();
    let old_lines: Vec<&str> = old.lines().collect();

    let mut hits: Vec<usize> = Vec::new();
    if old_lines.len() <= orig_lines.len() {
        for start in 0..=(orig_lines.len() - old_lines.len()) {
            let window = &orig_lines[start..start + old_lines.len()];
            if normalize(&window.join("\n")) == norm_old {
                hits.push(start);
            }
        }
    }

    match hits.len() {
        1 => {
            let line_start = hits[0];
            // 换算成字节偏移。
            let byte_start: usize = orig_lines[..line_start]
                .iter()
                .map(|l| l.len() + 1)
                .sum();
            // 区间长度要区分末行：oldText 不以换行结尾时，不能把原文的
            // 行尾换行一并吞掉，否则会把相邻两行粘成一行。
            let window = &orig_lines[line_start..line_start + old_lines.len()];
            let mut byte_len = 0usize;
            for (i, line) in window.iter().enumerate() {
                byte_len += line.len();
                let is_last = i + 1 == window.len();
                if !is_last || old.ends_with('\n') {
                    byte_len += 1;
                }
            }
            let byte_len = byte_len.min(original.len() - byte_start);
            Ok((byte_start, byte_len))
        }
        0 => Err(format!(
            "未找到 oldText。原文片段：{}\n请重新 read 确认当前内容。",
            snippet(original)
        )),
        n => Err(format!(
            "归一化匹配到 {n} 处，请扩大 oldText 上下文使其唯一。"
        )),
    }
}

fn snippet(text: &str) -> String {
    let head: String = text.chars().take(120).collect();
    if text.chars().count() > 120 {
        format!("{head}...")
    } else {
        head
    }
}

/// 归一化：智能引号、Unicode 破折号、特殊空格折成 ASCII，并去掉每行行尾空白。
fn normalize(text: &str) -> String {
    text.lines()
        .map(|line| {
            line.trim_end()
                .chars()
                .map(|c| match c {
                    '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
                    '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
                    '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}' => '-',
                    '\u{00A0}' | '\u{2007}' | '\u{202F}' => ' ',
                    other => other,
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 生成 unified diff 供模型确认改动。
fn unified_diff(before: &str, after: &str, label: &str) -> String {
    use similar::{ChangeTag, TextDiff};
    let diff = TextDiff::from_lines(before, after);
    let mut out = String::new();
    out.push_str(&format!("--- {label}\n+++ {label}\n"));
    for group in diff.grouped_ops(3) {
        for op in group {
            for change in diff.iter_changes(&op) {
                let sign = match change.tag() {
                    ChangeTag::Delete => '-',
                    ChangeTag::Insert => '+',
                    ChangeTag::Equal => ' ',
                };
                out.push(sign);
                out.push_str(change.value().trim_end_matches('\n'));
                out.push('\n');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_mode_replaces_single_occurrence() {
        let src = "fn a() {}\nfn b() {}\n";
        let edits = vec![json!({ "oldText": "fn b() {}", "newText": "fn c() {}" })];
        let out = apply_text_mode(src, &edits).expect("ok");
        assert_eq!(out, "fn a() {}\nfn c() {}\n");
    }

    #[test]
    fn text_mode_rejects_ambiguous_match() {
        let src = "x\nx\n";
        let edits = vec![json!({ "oldText": "x", "newText": "y" })];
        let err = apply_text_mode(src, &edits).unwrap_err();
        assert!(err.to_string().contains("2 处"));
    }

    #[test]
    fn text_mode_handles_multiple_disjoint_edits() {
        let src = "one\ntwo\nthree\n";
        let edits = vec![
            json!({ "oldText": "one", "newText": "1" }),
            json!({ "oldText": "three", "newText": "3" }),
        ];
        let out = apply_text_mode(src, &edits).expect("ok");
        assert_eq!(out, "1\ntwo\n3\n");
    }

    #[test]
    fn text_mode_tolerates_smart_quotes() {
        let src = "say \"hi\"\n";
        let edits = vec![json!({ "oldText": "say \u{201C}hi\u{201D}", "newText": "say bye" })];
        let out = apply_text_mode(src, &edits).expect("ok");
        assert_eq!(out, "say bye\n");
    }

    #[test]
    fn anchor_mode_replaces_matching_line() {
        let src = "alpha\nbeta\n";
        let hash = line_anchor("beta");
        let out = apply_anchor_mode(src, &format!("2#{hash}"), "BETA").expect("ok");
        assert_eq!(out, "alpha\nBETA\n");
    }

    #[test]
    fn anchor_mode_rejects_stale_hash() {
        let src = "alpha\nbeta\n";
        let err = apply_anchor_mode(src, "2#0000", "X").unwrap_err();
        assert!(err.to_string().contains("锚点失配"));
    }
}
