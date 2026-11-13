//! search：内容检索与文件枚举的聚合入口。
//!
//! 把 grep 与 find 合成一个工具，靠 mode 区分。这样做不是为了省一个
//! 工具名，而是为了让「超过阈值就落盘」这条治理规则只有一处实现。
//!
//! 遍历与匹配都用纯 Rust 库（ignore + regex），不依赖系统是否装了
//! ripgrep 或 fd，因此行为在各平台一致。

use std::path::Path;
use std::time::Instant;

use ignore::WalkBuilder;
use serde_json::{Value, json};

use super::{ToolContext, ToolResponse, ToolSpec, optional_str, optional_usize, required_str};
use crate::ToolError;
use crate::bash_path::intent_tag;
use crate::locale::{Language, tf};
use crate::notice::{Notice, SlotFallback, budgeted_notices};
use crate::search_path::{
    RerankState, SearchPlan, SearchShape, WalkLimits, WalkOutcome, apply_rerank, embedding_order,
    plan_search, resolve_search_intent, structured_facts,
};
use crate::slots::{EmbeddingInput, RerankInput, SlotKind};
use crate::truncate::{MAX_MATCH_LINE_LENGTH, truncate_head};

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: "search",
        description: "能做什么：content 模式用正则检索文件内容，files 模式用文件名子串 / 通配符枚举路径；content 命中超过 20 条且提供 query 时，用 rerank（优先）或 embedding 余弦按语义重排（files 不重排）；结果超过 2000 行 / 50KB 保留头部并落盘。 \
                      不能做什么：不解析网页 JS；不返回图片；两个语义槽位都不可用或未提供 query 时按命中顺序返回并告知（§3.8）。 \
                      什么时候用别的工具：已知路径读全文用 read；修改用 edit / write；跑命令用 bash。",
        description_en: "Can: content mode uses regex over file contents, files mode lists paths by filename substring / glob; for content, over 20 hits with a query reorder by relevance via rerank (preferred) or embedding cosine (files never reorders); head-truncates past 2000 lines / 50KB and spills. Cannot: run page JS or return images; without query or when both semantic slots are unavailable it keeps hit order and says so (§3.8). Use instead: read for a known path, edit / write to modify, bash to run commands.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "mode": {
                    "type": "string",
                    "enum": ["content", "files"],
                    "description": "content 搜内容，files 枚举文件路径"
                },
                "pattern": {
                    "type": "string",
                    "description": "content 模式是正则表达式；files 模式是文件名子串或通配符"
                },
                "query": {
                    "type": "string",
                    "description": "可选的自然语言查询：命中超过 20 条且 rerank / embedding 槽位可用时用于按语义相关度排序"
                },
                "path": {
                    "type": "string",
                    "description": "检索起点目录或文件，默认工作目录"
                },
                "max_results": {
                    "type": "integer",
                    "description": "最多返回多少条结果，默认 200，上限 200（语义重排只作用于这些候选）",
                    "minimum": 1,
                    "maximum": 200
                },
                "intent": {
                    "type": "string",
                    "enum": ["raw", "summary", "structured", "visual", "auto"],
                    "description": "协商层输出形态：raw 全部命中（默认）/ structured 结构化候选列表 / auto 与 summary 仅在命中超过 20 条且提供 query 时尝试语义重排，优先 rerank、其次 embedding，都不成则按命中顺序返回并告知 / visual 不适用"
                }
            },
            "required": ["mode", "pattern"]
        }),
    }
}

pub async fn run(args: &Value, ctx: &ToolContext) -> Result<ToolResponse, ToolError> {
    run_with_limits(args, ctx, WalkLimits::default()).await
}

/// 与 run 同一份实现；limits 只供测试注入更小的遍历上限（§17.2）。
async fn run_with_limits(
    args: &Value,
    ctx: &ToolContext,
    limits: WalkLimits,
) -> Result<ToolResponse, ToolError> {
    let lang = ctx.language;
    let mode = required_str(args, "mode")?;
    let pattern = required_str(args, "pattern")?;
    // query 是可选的语义查询：只有它参与重排；pattern 只决定命中。
    let query = optional_str(args, "query");
    // §17.2：检索起点同样过根校验，默认 "." 解析后必须落在允许根内。
    let root = ctx.resolve_file(optional_str(args, "path").unwrap_or("."))?;
    // §5.2：返回候选决定重排规模，运行期同样限幅，防止绕过 schema 的调用。
    let max_results = optional_usize(args, "max_results").unwrap_or(200).clamp(1, 200);

    let (raw, hit_count, walk) = match mode {
        "content" => search_content(&root, pattern, max_results, limits)?,
        "files" => list_files(&root, pattern, max_results, limits)?,
        other => {
            return Err(ToolError::bad_argument(format!(
                "mode 只能是 content 或 files，收到 {other}"
            )));
        }
    };

    let intent = resolve_search_intent(args).map_err(ToolError::bad_argument)?;
    // §3.10：槽位可用性按绑定 + 能力矩阵真实判定，不硬编码（§7.3 状态要真实）。
    let slot_available = |slot: SlotKind| {
        ctx.slm.as_ref().is_some_and(|client| {
            client.is_bound(slot)
                && client
                    .source_for_slot(slot)
                    .map(|source| crate::capability::source_slot_usable(source, slot))
                    .unwrap_or(true)
        })
    };
    let rerank_available = slot_available(SlotKind::Rerank);
    let embedding_available = slot_available(SlotKind::Embedding);
    let has_query = query.is_some_and(|text| !text.is_empty());
    let mut plan = plan_search(
        intent,
        hit_count,
        rerank_available,
        embedding_available,
        has_query,
    )
    .map_err(ToolError::bad_argument)?;
    // §5.2：files 模式只枚举路径，不参与语义重排（重排只对 content 命中行有意义）。
    confine_rerank_to_content(mode, &mut plan);

    // §5.2：TopK 路径先试 rerank，失败再退 embedding 余弦；都不成按命中顺序并告知，
    // 绝不谎报 applied（§4.6）。
    let mut rerank_order: Option<Vec<usize>> = None;
    let mut embedding_meta: Option<Value> = None;
    if plan.shape == SearchShape::TopK {
        let documents: Vec<String> = raw.lines().map(str::to_owned).collect();
        let semantic_query = query.unwrap_or_default();
        let mut applied = false;
        if plan.rerank == RerankState::Applied {
            let input = RerankInput {
                query: semantic_query.to_owned(),
                documents: documents.clone(),
                top_n: None,
            };
            let attempted = match ctx.slm.as_ref() {
                Some(client) => client.rerank(&input).await.ok(),
                None => None,
            };
            if let Some(outcome) = attempted {
                rerank_order = Some(outcome.output.results.iter().map(|r| r.index).collect());
                applied = true;
            }
        }
        let embedding_attempt = if !applied && embedding_available && has_query {
            embedding_rerank(ctx, semantic_query, &documents).await
        } else {
            None
        };
        if let Some(attempt) = embedding_attempt {
            rerank_order = Some(attempt.order);
            plan.rerank = RerankState::Embedding;
            embedding_meta = Some(json!({
                "candidates": attempt.candidates,
                "textsTruncated": attempt.truncated_texts,
                "batches": attempt.batches,
            }));
            applied = true;
        }
        if !applied {
            let failed_slot = if plan.rerank == RerankState::Embedding {
                "embedding"
            } else {
                "rerank"
            };
            plan.rerank = RerankState::Unavailable;
            plan.shape = SearchShape::All;
            plan.notices.push(Notice::SlotUnavailable {
                slot: failed_slot.to_owned(),
                fallback: SlotFallback::Mechanical,
            });
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

    // §17.2：达到深度或文件数上限时明确告知截断，不静默返回不完整结果。
    if let Some(note) = walk_note(lang, &walk, limits) {
        body.push_str(&note);
    }

    let mut meta = json!({
        "mode": mode,
        "pattern": pattern,
        "query": query,
        "hits": hit_count,
        "truncated": truncation.truncated,
        "spill": spilled,
        "intent": intent_tag(intent),
        "rerank": plan.rerank.as_str(),
        "embedding": embedding_meta,
        "walk": {
            "files": walk.files,
            "truncated": walk.truncated(),
            "truncatedBy": walk.truncated_by(),
        },
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
        let mut facts = structured_facts(hit_count, &candidates, spill_path, plan.rerank);
        // §17.2：structured 的 content 就是 facts，截断事实一并放进来。
        facts["walk"] = meta["walk"].clone();
        content = facts.to_string();
        meta["facts"] = facts;
    }
    Ok(ToolResponse::text(content).with_meta(meta))
}

/// §5.2：语义重排只对 content 命中行有意义；files 只枚举路径——不重排、不协商槽位，
/// 因此也不应报 SLOT_UNAVAILABLE（files 的 `meta.rerank` 恒为 skipped）。
fn confine_rerank_to_content(mode: &str, plan: &mut SearchPlan) {
    if mode == "content" {
        return;
    }
    if plan.shape == SearchShape::TopK {
        plan.shape = SearchShape::All;
    }
    plan.rerank = RerankState::Skipped;
    plan.notices.clear();
}

/// §5.2 / §3.8：embedding 退路的单次尝试结果；统计进 meta 便于核对有界性。
struct EmbeddingAttempt {
    order: Vec<usize>,
    candidates: usize,
    truncated_texts: usize,
    batches: usize,
}

/// §3.2：单条文本只取前 4096 字符，避免一行吃掉整段预算。
const MAX_EMBED_CHARS: usize = 4096;
/// §3.2：候选行硬上限；返回候选已由 max_results 限幅，这里再兜底。
const MAX_EMBED_DOCUMENTS: usize = 200;
/// §3.2：每批条数上限。
const EMBED_BATCH_SIZE: usize = 32;

fn clip_text(text: &str) -> (String, bool) {
    if text.chars().count() <= MAX_EMBED_CHARS {
        (text.to_owned(), false)
    } else {
        (text.chars().take(MAX_EMBED_CHARS).collect(), true)
    }
}

/// §3.2：按候选上限与单条上限组装待取向量的文本（query 在首位）。
fn embedding_texts(query: &str, documents: &[String]) -> (Vec<String>, usize) {
    let candidates = documents.len().min(MAX_EMBED_DOCUMENTS);
    let mut texts = Vec::with_capacity(candidates + 1);
    let mut truncated = 0usize;
    let (query_text, was_truncated) = clip_text(query);
    truncated += usize::from(was_truncated);
    texts.push(query_text);
    for document in documents.iter().take(candidates) {
        let (text, was_truncated) = clip_text(document);
        truncated += usize::from(was_truncated);
        texts.push(text);
    }
    (texts, truncated)
}

/// §3.2：拼接各批向量后校验条数与跨批维度；任何不符即整体放弃，不猜语义。
/// 单批内的同维由 EmbeddingOutput::validate 保证，这里只保证批间一致与条数对齐。
fn assemble_embedding_vectors(
    vectors: Vec<Vec<f64>>,
    expected: usize,
) -> Option<(Vec<f64>, Vec<Vec<f64>>)> {
    if vectors.len() != expected {
        return None;
    }
    let dim = vectors.first()?.len();
    if vectors.iter().any(|vector| vector.len() != dim) {
        return None;
    }
    let mut iter = vectors.into_iter();
    let query_vec = iter.next()?;
    let document_vecs: Vec<Vec<f64>> = iter.collect();
    Some((query_vec, document_vecs))
}

/// §5.2 / §3.8：embedding 退路。把 query 与候选行按 ≤32 条分批送 /v1/embeddings，
/// 取回同序、同维向量后按余弦相似度返回行号。任何一批失败或跨批维度不一致即整体
/// 放弃，由调用方按命中顺序降级——无索引、无状态、有界（候选 ≤200、单条 ≤4096 字符）。
async fn embedding_rerank(
    ctx: &ToolContext,
    query: &str,
    documents: &[String],
) -> Option<EmbeddingAttempt> {
    let client = ctx.slm.as_ref()?;
    let (texts, truncated_texts) = embedding_texts(query, documents);
    let mut vectors: Vec<Vec<f64>> = Vec::with_capacity(texts.len());
    let mut batches = 0usize;
    for chunk in texts.chunks(EMBED_BATCH_SIZE) {
        batches += 1;
        let input = EmbeddingInput {
            texts: chunk.to_vec(),
        };
        // 任何一批失败（超时、形状不符、槽位禁用）都整体放弃，由调用方按命中顺序降级。
        let outcome = client.embedding(&input).await.ok()?;
        vectors.extend(outcome.output.vectors);
    }
    let (query_vec, document_vecs) = assemble_embedding_vectors(vectors, texts.len())?;
    Some(EmbeddingAttempt {
        order: embedding_order(&query_vec, &document_vecs),
        candidates: document_vecs.len(),
        truncated_texts,
        batches,
    })
}

/// 内容检索：正则逐行匹配，返回 路径:行号:内容。
fn search_content(
    root: &Path,
    pattern: &str,
    max_results: usize,
    limits: WalkLimits,
) -> Result<(String, usize, WalkOutcome), ToolError> {
    let re = regex::Regex::new(pattern)
        .map_err(|e| ToolError::bad_argument(format!("正则无效：{e}")))?;

    let mut out = String::new();
    let mut hits = 0usize;
    let mut walk = WalkOutcome::default();
    let started = Instant::now();

    for entry in walker(root, limits) {
        if hits >= max_results {
            break;
        }
        if limits.timed_out(started.elapsed()) {
            return Err(ToolError::search_walk_timeout(
                started.elapsed(),
                limits.max_duration,
            ));
        }
        let Ok(entry) = entry else { continue };
        let file_type = entry.file_type();
        if file_type.is_some_and(|t| t.is_dir()) {
            walk.observe_dir(entry.depth(), limits);
            continue;
        }
        if !file_type.is_some_and(|t| t.is_file()) {
            continue;
        }
        if walk.observe_file(limits) {
            break;
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
    Ok((out, hits, walk))
}

/// 文件枚举：按子串或通配符匹配文件名。
fn list_files(
    root: &Path,
    pattern: &str,
    max_results: usize,
    limits: WalkLimits,
) -> Result<(String, usize, WalkOutcome), ToolError> {
    let glob = glob_to_regex(pattern);
    let re = regex::Regex::new(&glob)
        .map_err(|e| ToolError::bad_argument(format!("通配符无效：{e}")))?;

    let mut out = String::new();
    let mut hits = 0usize;
    let mut walk = WalkOutcome::default();
    let started = Instant::now();

    for entry in walker(root, limits) {
        if hits >= max_results {
            break;
        }
        if limits.timed_out(started.elapsed()) {
            return Err(ToolError::search_walk_timeout(
                started.elapsed(),
                limits.max_duration,
            ));
        }
        let Ok(entry) = entry else { continue };
        let file_type = entry.file_type();
        if file_type.is_some_and(|t| t.is_dir()) {
            walk.observe_dir(entry.depth(), limits);
            continue;
        }
        if !file_type.is_some_and(|t| t.is_file()) {
            continue;
        }
        if walk.observe_file(limits) {
            break;
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
    Ok((out, hits, walk))
}

/// 用 ignore 的遍历器，自动遵守 .gitignore 并跳过隐藏目录；
/// §17.2 递归深度上限交给 walker：超过上限的目录不再深入。
fn walker(root: &Path, limits: WalkLimits) -> ignore::Walk {
    WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(false)
        .max_depth(Some(limits.max_depth))
        .build()
}

/// §17.2：把遍历截断转成模型可见的告知；未截断时返回 None。
fn walk_note(lang: Language, walk: &WalkOutcome, limits: WalkLimits) -> Option<String> {
    let note = match (walk.depth_truncated, walk.files_truncated) {
        (true, true) => tf(
            lang,
            "\n[遍历已截断：递归深度达 {depth} 层且文件数达 {files} 个，结果只覆盖已遍历部分]",
            &[
                ("depth", limits.max_depth.to_string()),
                ("files", limits.max_files.to_string()),
            ],
        ),
        (true, false) => tf(
            lang,
            "\n[遍历已截断：递归深度达 {depth} 层，结果只覆盖已遍历部分]",
            &[("depth", limits.max_depth.to_string())],
        ),
        (false, true) => tf(
            lang,
            "\n[遍历已截断：文件数达 {files} 个，结果只覆盖已遍历部分]",
            &[("files", limits.max_files.to_string())],
        ),
        (false, false) => return None,
    };
    Some(note)
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
            &json!({ "mode": "content", "pattern": "hit", "intent": "raw" }),
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
            &json!({ "mode": "content", "pattern": "hit", "query": "hit line", "intent": "auto" }),
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
            &json!({ "mode": "content", "pattern": "hit", "query": "hit line", "intent": "auto" }),
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
            &json!({ "mode": "content", "pattern": "hit", "query": "hit line", "intent": "auto" }),
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
            &json!({ "mode": "content", "pattern": "hit", "intent": "structured" }),
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
            &json!({ "mode": "content", "pattern": "hit", "intent": "visual" }),
            &ctx,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("read"));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// §17.2：递归深度到上限时返回已遍历部分，并在 content 与 meta 告知截断。
    #[tokio::test]
    async fn depth_limit_truncates_with_notice() {
        let dir = std::env::temp_dir().join(format!("tg-search-depth-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let deep = dir.join("d1/d2/d3/d4");
        std::fs::create_dir_all(&deep).expect("mkdir");
        std::fs::write(dir.join("d1/shallow.txt"), "hit\n").expect("write");
        std::fs::write(deep.join("deep.txt"), "hit\n").expect("write");
        let ctx = ToolContext::new(&dir).expect("ctx");
        let limits = WalkLimits {
            max_depth: 2,
            ..WalkLimits::default()
        };
        let resp = run_with_limits(&json!({ "mode": "content", "pattern": "hit" }), &ctx, limits)
            .await
            .expect("run ok");
        let crate::tools::Content::Text(body) = &resp.content[0] else {
            panic!("应为文本")
        };
        assert!(body.contains("shallow.txt"), "深度内的命中应返回：{body}");
        assert!(
            !body.contains("deep.txt"),
            "超过深度上限的文件不应出现：{body}"
        );
        assert!(body.contains("遍历已截断"), "content 应告知截断：{body}");
        assert_eq!(resp.meta["walk"]["truncated"], json!(true));
        assert_eq!(resp.meta["walk"]["truncatedBy"], json!("depth"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// §17.2：文件数到上限时返回已遍历部分并告知，不静默返回不完整结果。
    #[tokio::test]
    async fn file_limit_truncates_with_notice() {
        let (ctx, dir) = ctx_with_hits("file-limit", 5);
        let limits = WalkLimits {
            max_files: 2,
            ..WalkLimits::default()
        };
        let resp = run_with_limits(&json!({ "mode": "files", "pattern": "*.txt" }), &ctx, limits)
            .await
            .expect("run ok");
        let crate::tools::Content::Text(body) = &resp.content[0] else {
            panic!("应为文本")
        };
        assert!(body.contains("遍历已截断"), "content 应告知截断：{body}");
        assert_eq!(resp.meta["walk"]["files"], json!(2));
        assert_eq!(resp.meta["walk"]["truncatedBy"], json!("files"));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// §17.2：遍历超时返回 TIMEOUT 错误码，而不是静默的部分结果。
    #[tokio::test]
    async fn walk_timeout_returns_timeout_error() {
        let (ctx, dir) = ctx_with_hits("timeout", 1);
        let limits = WalkLimits {
            max_duration: std::time::Duration::ZERO,
            ..WalkLimits::default()
        };
        let err = run_with_limits(&json!({ "mode": "content", "pattern": "hit" }), &ctx, limits)
            .await
            .unwrap_err();
        let view = crate::error::govern_error(&err);
        assert_eq!(view.code, "TIMEOUT");
        assert!(!view.next_steps.is_empty(), "超时也要给可执行下一步");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// §17.2：远未到上限时不得出现截断告知（不误报）。
    #[tokio::test]
    async fn small_tree_is_not_marked_truncated() {
        let (ctx, dir) = ctx_with_hits("no-trunc", 3);
        let resp = run_with_limits(
            &json!({ "mode": "files", "pattern": "*.txt" }),
            &ctx,
            WalkLimits::default(),
        )
        .await
        .expect("run ok");
        assert_eq!(resp.meta["walk"]["truncated"], json!(false));
        assert!(resp.meta["walk"]["truncatedBy"].is_null());
        let crate::tools::Content::Text(body) = &resp.content[0] else {
            panic!("应为文本")
        };
        assert!(!body.contains("遍历已截断"), "未截断不应告知：{body}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// §17.2：search 的根目录同样校验，根外 path 直接拒绝。
    #[tokio::test]
    async fn root_outside_allowed_roots_is_refused() {
        let (ctx, dir) = ctx_with_hits("root-guard", 1);
        let err = run(
            &json!({ "mode": "files", "pattern": "*.txt", "path": "/etc" }),
            &ctx,
        )
        .await
        .unwrap_err();
        assert_eq!(crate::error::govern_error(&err).code, "ISOLATION_REFUSED");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// §5.2：files 模式不参与语义重排——TopK 计划在 files 下被降回命中顺序。
    #[test]
    fn files_mode_downgrades_topk_plan() {
        let mut plan = SearchPlan {
            shape: SearchShape::TopK,
            rerank: RerankState::Applied,
            notices: vec![],
        };
        confine_rerank_to_content("files", &mut plan);
        assert_eq!(plan.shape, SearchShape::All);
        assert_eq!(plan.rerank, RerankState::Skipped);

        // content 模式不动 TopK 计划。
        let mut plan = SearchPlan {
            shape: SearchShape::TopK,
            rerank: RerankState::Embedding,
            notices: vec![],
        };
        confine_rerank_to_content("content", &mut plan);
        assert_eq!(plan.shape, SearchShape::TopK);
        assert_eq!(plan.rerank, RerankState::Embedding);

        // 两槽位都不可用的计划（shape 已是 All）在 files 下也不能报 unavailable。
        let intent = resolve_search_intent(&json!({ "intent": "auto" })).expect("intent");
        let mut plan = plan_search(intent, 25, false, false, true).expect("plan");
        assert_eq!(plan.rerank, RerankState::Unavailable);
        confine_rerank_to_content("files", &mut plan);
        assert_eq!(plan.shape, SearchShape::All);
        assert_eq!(plan.rerank, RerankState::Skipped);
        assert!(plan.notices.is_empty(), "files 不协商槽位，不应报 SLOT_UNAVAILABLE");
    }

    /// §4.2：schema 契约——pattern 必填、query 可选、max_results 有上限。
    #[test]
    fn spec_requires_pattern_and_caps_results() {
        let spec = spec();
        let schema = &spec.input_schema;
        assert_eq!(schema["required"], json!(["mode", "pattern"]));
        assert_eq!(schema["properties"]["max_results"]["maximum"], json!(200));
        assert!(schema["properties"]["query"].get("required").is_none());
    }

    /// §3.2：单条文本截断到 4096 字符，并返回截断标记供 meta 统计。
    #[test]
    fn clip_text_caps_long_lines() {
        let long = "a".repeat(MAX_EMBED_CHARS + 10);
        let (text, truncated) = clip_text(&long);
        assert_eq!(text.chars().count(), MAX_EMBED_CHARS);
        assert!(truncated);
        let (text, truncated) = clip_text("short");
        assert_eq!(text, "short");
        assert!(!truncated);
    }

    /// §3.2：候选超 200 条只取前 200（query 占首位），截断计数按实际条数累计。
    #[test]
    fn embedding_texts_caps_candidates_and_counts_truncation() {
        let documents: Vec<String> = (0..250)
            .map(|i| {
                if i % 50 == 0 {
                    "b".repeat(MAX_EMBED_CHARS + 1)
                } else {
                    "line".to_string()
                }
            })
            .collect();
        let (texts, truncated) = embedding_texts("query", &documents);
        assert_eq!(texts.len(), MAX_EMBED_DOCUMENTS + 1, "query 占首位");
        assert_eq!(truncated, 4, "0/50/100/150 四条超长，i=200 已被剔除");
        assert_eq!(texts[0], "query");
        assert!(texts.iter().all(|t| t.chars().count() <= MAX_EMBED_CHARS));
    }

    /// §3.2：跨批向量条数不符或维度不一致时整体放弃，不返回可疑排序。
    #[test]
    fn assemble_embedding_vectors_rejects_short_or_ragged_batches() {
        let good = vec![vec![0.1, 0.2], vec![0.3, 0.4], vec![0.5, 0.6]];
        let (query, docs) = assemble_embedding_vectors(good, 3).expect("形状一致应通过");
        assert_eq!(query, vec![0.1, 0.2]);
        assert_eq!(docs.len(), 2);

        // 期望 4 条、实到 3 条：少一条（第二批缺项）。
        assert!(assemble_embedding_vectors(vec![vec![0.1, 0.2]; 3], 4).is_none());
        // 跨批维度不一致（第二批 3 维）。
        assert!(assemble_embedding_vectors(vec![vec![0.1, 0.2], vec![0.3, 0.4, 0.5]], 2).is_none());
        // 连 query 向量都没有。
        assert!(assemble_embedding_vectors(Vec::new(), 0).is_none());
    }
}
