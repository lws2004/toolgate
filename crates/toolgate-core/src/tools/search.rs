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
use crate::bash_path::intent_tag;
use crate::locale::tf;
use crate::notice::{Notice, SlotFallback, budgeted_notices};
use crate::search_path::{
    RerankState, SearchShape, apply_rerank, plan_search, resolve_search_intent, structured_facts,
};
use crate::slots::{RerankInput, SlotKind};
use crate::truncate::{MAX_MATCH_LINE_LENGTH, truncate_head};

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: "search",
        description: "能做什么：按正则检索文件内容或按通配符枚举文件路径；结果超过 2000 行 / 50KB 保留头部并落盘。 \
                      不能做什么：不解析网页 JS；不返回图片；rerank 槽位可用时按相关度重排，不可用时按命中顺序返回并告知（§3.8）。 \
                      什么时候用别的工具：已知路径读全文用 read；修改用 edit / write；跑命令用 bash。",
        description_en: "Can: regex content search or glob file enumeration; head-truncates past 2000 lines / 50KB and spills; reorders hits by relevance when the rerank slot is available. Cannot: run page JS or return images; when rerank is unavailable it keeps hit order and says so (§3.8). Use instead: read for a known path, edit / write to modify, bash to run commands.",
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
                },
                "intent": {
                    "type": "string",
                    "enum": ["raw", "summary", "structured", "visual", "auto"],
                    "description": "协商层输出形态：raw 全部命中（默认）/ structured 结构化候选列表 / auto 与 summary 仅在命中超过 20 条时尝试 rerank，槽位可用则按相关度重排、不可用则按命中顺序返回并告知 / visual 不适用"
                }
            },
            "required": ["mode", "query"]
        }),
    }
}

pub async fn run(args: &Value, ctx: &ToolContext) -> Result<ToolResponse, ToolError> {
    let lang = ctx.language;
    let mode = required_str(args, "mode")?;
    let query = required_str(args, "query")?;
    let root = ctx.resolve(optional_str(args, "path").unwrap_or("."));
    let max_results = optional_usize(args, "max_results").unwrap_or(200);

    let (raw, hit_count) = match mode {
        "content" => search_content(&root, query, max_results)?,
        "files" => list_files(&root, query, max_results)?,
        other => {
            return Err(ToolError::bad_argument(format!(
                "mode 只能是 content 或 files，收到 {other}"
            )));
        }
    };

    let intent = resolve_search_intent(args).map_err(ToolError::bad_argument)?;
    // §3.10：槽位可用性按绑定 + 能力矩阵真实判定，不硬编码（§7.3 状态要真实）。
    let rerank_available = ctx.slm.as_ref().is_some_and(|client| {
        client.is_bound(SlotKind::Rerank)
            && client
                .source_for_slot(SlotKind::Rerank)
                .map(|source| {
                    crate::capability::source_slot_capability(source, SlotKind::Rerank).grade
                        != crate::capability::CapabilityGrade::Unavailable
                })
                .unwrap_or(true)
    });
    let mut plan =
        plan_search(intent, hit_count, rerank_available).map_err(ToolError::bad_argument)?;

    // §5.2：只有 TopK 路径真实调用 rerank；失败按 §3.8 退到命中顺序并告知，
    // 绝不谎报 applied（§4.6）。
    let mut rerank_order: Option<Vec<usize>> = None;
    if plan.shape == SearchShape::TopK {
        let documents: Vec<String> = raw.lines().map(str::to_owned).collect();
        let input = RerankInput {
            query: query.to_owned(),
            documents,
            top_n: None,
        };
        let attempted = match ctx.slm.as_ref() {
            Some(client) => client.rerank(&input).await.ok(),
            None => None,
        };
        match attempted {
            Some(outcome) => {
                rerank_order = Some(outcome.output.results.iter().map(|r| r.index).collect());
            }
            None => {
                plan.rerank = RerankState::Unavailable;
                plan.shape = SearchShape::All;
                plan.notices.push(Notice::SlotUnavailable {
                    slot: "rerank".to_owned(),
                    fallback: SlotFallback::Mechanical,
                });
            }
        }
    }

    // 重排命中后再做确定性截断：返回、落盘与结构化事实共用同一份顺序。
    let raw = match &rerank_order {
        Some(order) => apply_rerank(&raw, order),
        None => raw,
    };

    let truncation = truncate_head(&raw, ctx.limits);
    let mut body = truncation.content.clone();
    let mut spilled = Value::Null;

    if truncation.truncated {
        let spill = ctx.spill.persist(&format!("search-{mode}"), &raw)?;
        body.push_str(&tf(
            lang,
            "\n[结果已截断：命中 {hits} 处，仅展示前 {shown} 行。{hint}]",
            &[
                ("hits", hit_count.to_string()),
                ("shown", truncation.output_lines.to_string()),
                ("hint", spill.hint(lang)),
            ],
        ));
        spilled = json!({
            "path": spill.path.display().to_string(),
            "bytes": spill.bytes,
            "lines": spill.lines,
        });
    }

    let mut meta = json!({
        "mode": mode,
        "query": query,
        "hits": hit_count,
        "truncated": truncation.truncated,
        "spill": spilled,
        "intent": intent_tag(intent),
        "rerank": plan.rerank.as_str(),
    });
    let mut content = body;
    if !plan.notices.is_empty() {
        content.push('\n');
        let ch = budgeted_notices(plan.notices.clone());
        content.push_str(&ch.text);
        meta["notices"] = ch.structured;
        meta["noticeOverflow"] = json!(ch.overflow);
    }
    if plan.shape == SearchShape::Structured {
        let candidates: Vec<&str> = raw.lines().collect();
        let spill_path = meta["spill"]["path"].as_str();
        let facts = structured_facts(hit_count, &candidates, spill_path, plan.rerank);
        content = facts.to_string();
        meta["facts"] = facts;
    }
    Ok(ToolResponse::text(content).with_meta(meta))
}

/// 内容检索：正则逐行匹配，返回 路径:行号:内容。
fn search_content(
    root: &Path,
    pattern: &str,
    max_results: usize,
) -> Result<(String, usize), ToolError> {
    let re = regex::Regex::new(pattern)
        .map_err(|e| ToolError::bad_argument(format!("正则无效：{e}")))?;

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
fn list_files(
    root: &Path,
    pattern: &str,
    max_results: usize,
) -> Result<(String, usize), ToolError> {
    let glob = glob_to_regex(pattern);
    let re = regex::Regex::new(&glob)
        .map_err(|e| ToolError::bad_argument(format!("通配符无效：{e}")))?;

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

    fn ctx_with_hits(name: &str, files: usize) -> (ToolContext, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("tg-search-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        for i in 0..files {
            std::fs::write(dir.join(format!("f{i}.txt")), "hit\n").expect("write");
        }
        (ToolContext::new(&dir).expect("ctx"), dir)
    }

    #[tokio::test]
    async fn raw_intent_matches_v1_behavior() {
        let (ctx, dir) = ctx_with_hits("raw", 3);
        let resp = run(
            &json!({ "mode": "content", "query": "hit", "intent": "raw" }),
            &ctx,
        )
        .await
        .expect("run ok");
        let crate::tools::Content::Text(body) = &resp.content[0] else {
            panic!("应为文本")
        };
        assert!(body.contains("hit"));
        assert_eq!(resp.meta["hits"], json!(3));
        assert_eq!(resp.meta["intent"], json!("raw"));
        assert_eq!(resp.meta["rerank"], json!("skipped"));
        assert!(resp.meta.get("notices").is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn small_hits_auto_stays_raw() {
        let (ctx, dir) = ctx_with_hits("small", 5);
        let resp = run(
            &json!({ "mode": "content", "query": "hit", "intent": "auto" }),
            &ctx,
        )
        .await
        .expect("run ok");
        assert_eq!(resp.meta["rerank"], json!("skipped"));
        assert!(resp.meta.get("notices").is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// §5.2：槽位绑定但调用失败时降级为 unavailable + 告知，不谎报 applied（§4.6）。
    #[tokio::test]
    async fn bound_rerank_failure_degrades_without_faking_applied() {
        let (mut ctx, dir) = ctx_with_hits("rerank-bound", 25);
        let cfg = crate::config::parse_config(
            "models:
  llm:
    source: openai
    base_url: http://127.0.0.1:8090/v1
    model: demo
    serves: [rerank]
",
            "test.yaml",
        )
        .expect("配置应当解析成功");
        ctx.slm = Some(std::sync::Arc::new(crate::slm::SlotClient::new(cfg)));
        let resp = run(
            &json!({ "mode": "content", "query": "hit", "intent": "auto" }),
            &ctx,
        )
        .await
        .expect("run ok");
        assert_eq!(resp.meta["rerank"], json!("unavailable"));
        assert_eq!(resp.meta["notices"].as_array().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn many_hits_auto_degrades_with_slot_unavailable() {
        let (ctx, dir) = ctx_with_hits("many", 25);
        let resp = run(
            &json!({ "mode": "content", "query": "hit", "intent": "auto" }),
            &ctx,
        )
        .await
        .expect("run ok");
        let crate::tools::Content::Text(body) = &resp.content[0] else {
            panic!("应为文本")
        };
        assert!(body.contains("[SLOT_UNAVAILABLE]"));
        assert_eq!(resp.meta["hits"], json!(25));
        assert_eq!(resp.meta["rerank"], json!("unavailable"));
        assert_eq!(resp.meta["notices"].as_array().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn structured_intent_returns_candidate_json() {
        let (ctx, dir) = ctx_with_hits("structured", 3);
        let resp = run(
            &json!({ "mode": "content", "query": "hit", "intent": "structured" }),
            &ctx,
        )
        .await
        .expect("run ok");
        let crate::tools::Content::Text(body) = &resp.content[0] else {
            panic!("应为文本")
        };
        let facts: Value = serde_json::from_str(body).expect("content 应是 JSON");
        assert_eq!(facts["hits"], json!(3));
        assert_eq!(facts["candidates"].as_array().unwrap().len(), 3);
        assert_eq!(resp.meta["facts"]["rerank"], json!("skipped"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn visual_intent_is_rejected_actionably() {
        let (ctx, dir) = ctx_with_hits("visual", 1);
        let err = run(
            &json!({ "mode": "content", "query": "hit", "intent": "visual" }),
            &ctx,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("read"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
