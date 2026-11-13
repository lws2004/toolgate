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

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Value, json};

use super::{ToolContext, ToolResponse, ToolSpec, optional_str};
use crate::ToolError;
use crate::compat::{CompatConfig, resolve_intent};
use crate::edit_path::{
    APPLY_CONFIDENCE_THRESHOLD, ApplyGate, Candidate, EDIT_QUESTION_KEYS, EDIT_QUESTIONS,
    EditDecider, EditDecision, EditFuture, FailureKind, FileFacts, MatchLevel, PatchConflict,
    SEMANTIC_SIMILARITY_THRESHOLD, SEMANTIC_STATE_CHARS, SEMANTIC_TOP_K, apply_gate, apply_patch,
    change_facts, classify_match, detect_eol, diff_stats, mechanical_attribution,
    merge_edit_decision, nearby_build_files, parse_patch, plan_edit, similar_candidates,
    suggestion_for, unified_diff,
};
use crate::hashline::{line_anchor, parse_anchor};
use crate::locale::t;
use crate::notice::{Notice, budgeted_notices};
use crate::read_path::display_path;
use crate::slots::DecideInput;

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: "edit",
        description: "能做什么：局部修改文件，三种寻址——edits 文本匹配（默认）、anchor 行锚点、patch 扁平 diff（重排无需转义），三选一，多处匹配或 hunk 不匹配即拒，改前留快照。不能做什么：不摘要大块内容、不自动回滚。什么时候用别的工具：重写 write、查找 search、读取 read。",
        description_en: "Can: local edits with three modes — edits text match (default), anchor line anchors, patch flat diff (no escaping on reorder); pick one, reject multiple matches or mismatched hunks, snapshot before writing. Cannot: summarize large content, auto-rollback. Use instead: write for rewrite, search to find, read to inspect.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "目标文件；patch 可推断" },
                "edits": {
                    "type": "array",
                    "description": "基于原文，不得重叠",
                    "items": {
                        "type": "object",
                        "properties": {
                            "oldText": { "type": "string", "description": "原文，需唯一" },
                            "newText": { "type": "string", "description": "新内容，删除传空串" }
                        },
                        "required": ["oldText", "newText"]
                    }
                },
                "anchor": { "type": "string", "description": "行号#哈希，如 12#f8a2" },
                "newText": { "type": "string", "description": "该行新内容" },
                "patch": { "type": "string", "description": "扁平 diff，互斥 edits/anchor" },
                "intent": { "type": "string", "enum": ["raw", "summary", "structured", "visual", "auto"], "description": "输出形态 §4.2；fail loud" }
            },
            "required": []
        }),
    }
}

pub async fn run(args: &Value, ctx: &ToolContext) -> Result<ToolResponse, ToolError> {
    let lang = ctx.language;
    // §13.3 / §18.9：intent 对所有调用统一校验，非法值 fail loud。
    // edit 不产生可协商的大块内容（§4.0），所以只校验、不改输出形态。
    resolve_intent(args, &CompatConfig::default()).map_err(ToolError::bad_argument)?;

    // §5.4：三种寻址模式三选一，多于一个直接拒绝（不猜调用方想用哪个）。
    let mode = select_mode(args)?;
    let patch_text = optional_str(args, "patch");
    // §5.4 模式 C：patch 自带 ---/+++ 文件头时 path 可省略。
    let raw_path = match optional_str(args, "path") {
        Some(path) => path.to_owned(),
        None => match patch_text {
            Some(patch) => patch_target_path(patch)?,
            None => {
                return Err(ToolError::bad_argument(
                    "缺少 path：edits / anchor 模式必须给目标文件路径，patch 模式可从 ---/+++ 文件头推断"
                        .to_owned(),
                ));
            }
        },
    };
    // §17.2：文件路径先过根校验（含 ../ 与符号链接逃逸）。
    let path = ctx.resolve_file(&raw_path)?;
    // §22.3：read 预热过的文件内容直接复用，省一次 IO 与解码；
    // 缓存可丢弃（§22.5），未命中就走正常读取。写前复核仍以磁盘为准（§18.5）。
    let meta = tokio::fs::metadata(&path).await.map_err(|e| {
        ToolError::failed_with(
            crate::error::ErrorCode::PathNotFound,
            format!("无法读取 {}：{e}", path.display()),
        )
    })?;
    let warm_key = crate::warm::FileKey::of(meta.modified().ok(), meta.len());
    let bytes = match ctx.warm.get(&path, warm_key) {
        Some(warm) => warm.bytes,
        None => tokio::fs::read(&path).await.map_err(|e| {
            ToolError::failed_with(
                crate::error::ErrorCode::PathNotFound,
                format!("无法读取 {}：{e}", path.display()),
            )
        })?,
    };
    // §5.4 共同前置：读原文时记版本指纹，写前复核（三种模式同一份）。
    let stamp = FileStamp::capture(&path, &bytes).await?;

    // 读-改-写必须字节保真：按检测出的编码解码，改完再用同一编码写回。
    let encoding = crate::encoding::FileEncoding::detect(&bytes);
    let (original, had_decode_errors) = encoding.decode(&bytes);
    if had_decode_errors {
        return Err(ToolError::failed(format!(
            "{} 按 {} 解码时出现无法解释的字节。继续读写会破坏这些字节，\
             请先用 bash 确认编码或转码（例如 iconv -f GBK -t UTF-8）。",
            path.display(),
            encoding.name()
        )));
    }

    let (updated, notices, decision) = match mode {
        EditMode::Patch => {
            let patch_text = patch_text.expect("select_mode 已确认 patch 存在");
            (apply_patch_mode(&original, patch_text)?, vec![], None)
        }
        EditMode::Anchor => {
            let anchor_raw = optional_str(args, "anchor").expect("select_mode 已确认 anchor 存在");
            (
                apply_anchor_mode(
                    &original,
                    anchor_raw,
                    optional_str(args, "newText").unwrap_or(""),
                )?,
                vec![],
                None,
            )
        }
        EditMode::Text => {
            let edits = args
                .get("edits")
                .and_then(Value::as_array)
                .ok_or_else(|| ToolError::bad_argument("edits 必须是数组".to_owned()))?;
            // 第 4 级判定入口：没有配置模型时为 None，第 4 级按机械归因降级（§3.8）。
            let decider = ctx.slm.as_deref().map(|client| SlotDecider { client });
            let result = apply_text_mode(
                &original,
                edits,
                &display_path(&path, &ctx.cwd),
                decider.as_ref().map(|decider| decider as &dyn EditDecider),
            )
            .await?;
            (result.updated, result.notices, result.decision)
        }
    };

    if updated == original {
        return Ok(ToolResponse::text(t(lang, "内容未变化，未写入。")));
    }

    // 落盘前留一份原文，供回滚。
    let snapshot = ctx.snapshots.snapshot(&path, &original).ok();

    // §5.4 写前复核：指纹与读时不一致即拒绝，不做三方合并（§18.5）。
    stamp.verify(&path).await?;

    // 按原编码写回。目标编码无法表示新内容时明确报错，不静默替换。
    let out_bytes = encoding
        .encode(&updated)
        .map_err(|e| ToolError::failed(format!("{} 写回失败：{e}", path.display())))?;
    tokio::fs::write(&path, &out_bytes).await?;

    let diff = unified_diff(&original, &updated, &display_path(&path, &ctx.cwd));
    let snapshot_meta = snapshot
        .as_ref()
        .map(|s| json!(s.path.display().to_string()))
        .unwrap_or(json!(null));

    // §5.7：只给事实，不给「你该跑什么」的指令。
    let stats = diff_stats(&original, &updated);
    let file_facts = FileFacts {
        lines: updated.lines().count(),
        bytes: out_bytes.len(),
        encoding: encoding.name(),
        eol: detect_eol(&original),
    };
    let nearby = nearby_build_files(path.parent().unwrap_or_else(|| Path::new(".")));
    let facts = change_facts(
        &display_path(&path, &ctx.cwd),
        stats,
        file_facts,
        nearby,
        &diff,
    );
    let mut meta = json!({
        "path": path.display().to_string(),
        "changed": true,
        "bytesBefore": original.len(),
        "bytesAfter": updated.len(),
        "snapshot": snapshot_meta,
        "encoding": encoding.name(),
        "facts": facts,
    });
    // §5.4：第 4 级写回成功时给出判定结论与置信度（§4.4 程序可读通道）。
    if let Some(decision) = &decision {
        meta["decision"] = json!({
            "failureKind": decision.failure_kind,
            "fixable": decision.fixable,
        });
        meta["confidence"] = json!(decision.confidence);
    }
    let mut content = diff;
    if !notices.is_empty() {
        content.push('\n');
        let ch = budgeted_notices(notices.clone());
        content.push_str(&ch.text);
        meta["notices"] = ch.structured;
        meta["noticeOverflow"] = json!(ch.overflow);
    }
    Ok(ToolResponse::text(content).with_meta(meta))
}

/// §5.4 的三种寻址模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditMode {
    Text,
    Anchor,
    Patch,
}

/// §5.4：edits / anchor / patch 三选一，多于一个给可行动错误（不猜）。
fn select_mode(args: &Value) -> Result<EditMode, ToolError> {
    let present: Vec<&str> = [
        ("edits", args.get("edits").is_some()),
        ("anchor", args.get("anchor").is_some()),
        ("patch", args.get("patch").is_some()),
    ]
    .iter()
    .filter(|(_, present)| *present)
    .map(|(name, _)| *name)
    .collect();
    match present.as_slice() {
        [] => Err(ToolError::bad_argument(
            "需要提供 edits 数组（文本匹配模式）、anchor + newText（行锚点模式），或 patch（unified diff 模式）；三选一"
                .to_owned(),
        )),
        [only] => Ok(match *only {
            "anchor" => EditMode::Anchor,
            "patch" => EditMode::Patch,
            _ => EditMode::Text,
        }),
        many => Err(ToolError::bad_argument(format!(
            "edits / anchor / patch 三选一，本次同时收到 {}；删掉多余的模式后重试",
            many.join("、")
        ))),
    }
}

/// §5.4 模式 C：patch 未带 path 时，从 ---/+++ 文件头推断目标路径。
fn patch_target_path(patch: &str) -> Result<String, ToolError> {
    let parsed = parse_patch(patch).map_err(|conflict| patch_conflict_error(&conflict))?;
    for label in [parsed.new_label.as_deref(), parsed.old_label.as_deref()] {
        if let Some(path) = label.and_then(path_from_diff_label) {
            return Ok(path);
        }
    }
    Err(ToolError::bad_argument(
        "patch 模式没有可用的 path：请显式传 path，或在 patch 里给 ---/+++ 文件头（/dev/null 不算）"
            .to_owned(),
    ))
}

/// 从 diff 标签取路径：去时间戳、去 <workspace>/ 与 a/ b/ 前缀。
fn path_from_diff_label(label: &str) -> Option<String> {
    let label = label.split('\t').next()?.trim();
    if label.is_empty() || label == "/dev/null" {
        return None;
    }
    let label = label.strip_prefix("<workspace>/").unwrap_or(label);
    let label = label
        .strip_prefix("a/")
        .or_else(|| label.strip_prefix("b/"))
        .unwrap_or(label);
    Some(label.to_owned())
}

/// §5.4 模式 C：解析 unified diff，按 hunk 校验后应用。
fn apply_patch_mode(original: &str, patch_text: &str) -> Result<String, ToolError> {
    let parsed = parse_patch(patch_text).map_err(|conflict| patch_conflict_error(&conflict))?;
    apply_patch(original, &parsed).map_err(|conflict| patch_conflict_error(&conflict))
}

/// patch 冲突按 §1.4.5 三段式返回：事实里带期望与实际内容，下一步指向重算 hunk。
fn patch_conflict_error(conflict: &PatchConflict) -> ToolError {
    ToolError::Failed {
        code: crate::error::ErrorCode::Failed,
        fact: format!(
            "{}（机械归因：{}）",
            conflict.fact,
            FailureKind::PatchConflict.as_str()
        ),
        reason: "patch 的 hunk 与当前文件对不上：文件已被改过，或 hunk 的行号/行数算错了"
            .to_owned(),
        next_steps: vec![
            "重新 read 当前文件，按最新行号重算 hunk".to_owned(),
            "或改用 edits 文本匹配模式（oldText 需在文件中唯一）".to_owned(),
        ],
    }
}

/// §5.4 共同前置：读时的版本指纹（mtime / size / sha256），写前复核。
struct FileStamp {
    modified: Option<std::time::SystemTime>,
    size: usize,
    sha256: String,
}

impl FileStamp {
    async fn capture(path: &Path, bytes: &[u8]) -> Result<Self, ToolError> {
        let meta = tokio::fs::metadata(path).await?;
        Ok(Self {
            modified: meta.modified().ok(),
            size: bytes.len(),
            sha256: crate::spill::sha256_short(bytes),
        })
    }

    /// 写前复核：与读时不一致即拒绝，不做三方合并（§18.5）。
    async fn verify(&self, path: &Path) -> Result<(), ToolError> {
        let meta = tokio::fs::metadata(path).await?;
        let bytes = tokio::fs::read(path).await?;
        let unchanged = meta.modified().ok() == self.modified
            && bytes.len() == self.size
            && crate::spill::sha256_short(&bytes) == self.sha256;
        if unchanged {
            return Ok(());
        }
        Err(ToolError::failed_with(
            crate::error::ErrorCode::Failed,
            format!(
                "{} 在读取后被其他进程修改，已放弃写入（不做三方合并）。请重新 read 后再改。",
                path.display()
            ),
        ))
    }
}

/// 行尾风格。读改写必须保留它，否则 CRLF 文件会被整体转成 LF，
/// git diff 会显示全文件修改。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineEnding {
    Lf,
    Crlf,
}

impl LineEnding {
    /// 看第一个换行符前面是不是 \r。
    fn detect(text: &str) -> Self {
        match text.find('\n') {
            Some(i) if i > 0 && text.as_bytes()[i - 1] == b'\r' => Self::Crlf,
            _ => Self::Lf,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::Crlf => "\r\n",
        }
    }
}

/// 每一行在**原始字节**里的 (start, end)，end 不含 \r 与 \n。
///
/// 不能用 lines().map(|l| l.len() + 1) 累加来算偏移——CRLF 文件每行是
/// len + 2 字节，那样算出来的位置会一路偏小，替换到错误的地方。
fn line_spans(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut spans = Vec::new();
    let mut start = 0usize;
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'\n' {
            let mut end = i;
            if end > start && bytes[end - 1] == b'\r' {
                end -= 1;
            }
            spans.push((start, end));
            start = i + 1;
        }
    }
    if start < bytes.len() {
        spans.push((start, bytes.len()));
    }
    spans
}

/// 把行内容按原始字节切出来，供匹配与锚点使用。
fn split_lines(text: &str) -> Vec<&str> {
    line_spans(text)
        .into_iter()
        .map(|(s, e)| &text[s..e])
        .collect()
}

/// 行锚点模式：校验哈希后替换单行。
fn apply_anchor_mode(
    original: &str,
    anchor_raw: &str,
    new_text: &str,
) -> Result<String, ToolError> {
    let anchor = parse_anchor(anchor_raw).map_err(|e| ToolError::bad_argument(e.to_string()))?;
    let ending = LineEnding::detect(original);
    let mut lines = split_lines(original);
    if anchor.line > lines.len() {
        return Err(ToolError::failed(format!(
            "锚点行号 {} 超出文件行数 {}",
            anchor.line,
            lines.len()
        )));
    }
    let idx = anchor.line - 1;
    let current = lines[idx];
    if line_anchor(current) != anchor.hash {
        return Err(ToolError::failed(format!(
            "锚点失配：第 {} 行当前哈希为 {}，与传入的 {} 不符。\
             文件可能已被改动，请重新 read 后再编辑。",
            anchor.line,
            line_anchor(current),
            anchor.hash
        )));
    }
    lines[idx] = new_text;

    let mut out = lines.join(ending.as_str());
    if original.ends_with('\n') {
        out.push_str(ending.as_str());
    }
    Ok(out)
}

/// 文本匹配模式：逐笔在原始文件上定位并替换。
///
/// 第 4 级（语义对齐）在三级都没命中时接管：机械分诊 → 判定 → 应用门槛，
/// 判定不可用时保持机械归因（§3.8 降级）。
async fn apply_text_mode(
    original: &str,
    edits: &[Value],
    path_label: &str,
    decider: Option<&dyn EditDecider>,
) -> Result<TextModeResult, ToolError> {
    if edits.is_empty() {
        return Err(ToolError::bad_argument("edits 不能为空".to_owned()));
    }

    // 先收集所有替换区间，再统一应用，避免前一笔改动影响后一笔的匹配位置。
    let mut spans: Vec<(usize, usize, String)> = Vec::with_capacity(edits.len());
    let mut notices: Vec<Notice> = Vec::new();
    let mut decision: Option<EditDecision> = None;
    for (i, e) in edits.iter().enumerate() {
        let old = e
            .get("oldText")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::bad_argument(format!("edits[{i}] 缺少 oldText")))?;
        let new = e
            .get("newText")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::bad_argument(format!("edits[{i}] 缺少 newText")))?;

        let (start, len, level, edit_decision) = resolve_span(original, old, path_label, decider)
            .await
            .map_err(|failure| ToolError::Failed {
                code: crate::error::ErrorCode::Failed,
                fact: format!("edits[{i}] {}", failure.fact),
                reason: "工具执行未能完成".to_owned(),
                next_steps: failure.next_steps,
            })?;
        notices.extend(plan_edit(level, decider.is_some()).notices);
        if edit_decision.is_some() {
            decision = edit_decision;
        }
        spans.push((start, len, new.to_owned()));
    }

    // 检查重叠：区间排序后相邻不得相交。
    let mut sorted = spans.clone();
    sorted.sort_by_key(|(s, _, _)| *s);
    for w in sorted.windows(2) {
        let (s1, l1, _) = &w[0];
        let (s2, _, _) = &w[1];
        if s1 + l1 > *s2 {
            return Err(ToolError::bad_argument(
                "edits 存在重叠区间，请合并成一笔编辑".to_owned(),
            ));
        }
    }

    // 从后往前替换，保证前面的偏移不受影响。
    let mut out = original.to_owned();
    for (start, len, new) in sorted.iter().rev() {
        out.replace_range(*start..(*start + *len), new);
    }
    Ok(TextModeResult {
        updated: out,
        notices,
        decision,
    })
}

/// 定位结果：三级命中，或第 4 级需要的语义候选。
enum Locate {
    /// 三级命中：唯一位置与级别。
    Hit {
        /// 起始字节偏移。
        start: usize,
        /// 替换长度（字节）。
        len: usize,
        /// 命中的级别。
        level: MatchLevel,
    },
    /// 第 4 级：给出最相似的候选片段。
    Semantic {
        /// 最相似的候选（按相似度降序）。
        candidates: Vec<Candidate>,
    },
}

/// 第 4 级失败的事实与下一步（交给调用方包成 ToolError）。
struct EditFailure {
    fact: String,
    next_steps: Vec<String>,
}

/// 文本匹配模式的结果。
#[derive(Debug)]
struct TextModeResult {
    updated: String,
    notices: Vec<Notice>,
    /// 第 4 级判定结论；机械三级命中时为 None。
    decision: Option<EditDecision>,
}

/// 真实判定入口：走 decide 槽位（systemone）。
struct SlotDecider<'a> {
    client: &'a crate::slm::SlotClient,
}

impl EditDecider for SlotDecider<'_> {
    fn decide<'a>(&'a self, input: &'a DecideInput) -> EditFuture<'a> {
        Box::pin(async move {
            let outcome = self.client.decide(input).await.ok()?;
            let probs: BTreeMap<String, f64> = EDIT_QUESTION_KEYS
                .iter()
                .zip(EDIT_QUESTIONS.iter())
                .map(|(key, question)| {
                    let probability = outcome
                        .output
                        .answers
                        .get(*question)
                        .map(|answer| answer.probability)
                        .unwrap_or(0.0);
                    ((*key).to_owned(), probability)
                })
                .collect();
            let decision = merge_edit_decision(&probs);
            // §15.2 判定日志：confidence 按 §7.3 口径在归并结论后传入——它是「被采纳
            // 结论所依赖的问题概率最小值」，不是全部问题的最小值。写失败不影响
            // 工具结果（观测性不进主路径）。
            let _ = crate::decision_log::append(
                "edit",
                Some(decision.confidence),
                self.client.decide_model_name().as_deref(),
                input,
                &outcome,
            )
            .await;
            Some((decision, outcome.gate))
        })
    }
}

/// 机械归因失败：与现有文案一致（第 4 级不可用时的降级路径）。
fn mechanical_failure(original: &str, old: &str) -> EditFailure {
    EditFailure {
        fact: format!(
            "未找到 oldText（机械归因：{}）。原文片段：{}\n请重新 read 确认当前内容。",
            mechanical_attribution(original, old).as_str(),
            snippet(original)
        ),
        next_steps: vec![
            "重新 read 当前文件后再改".to_owned(),
            "扩大 oldText 的上下文后重试".to_owned(),
        ],
    }
}

/// 判定归因失败：写清来源是判定，并给出机械映射的建议（§5.4）。
fn decision_failure(decision: &EditDecision, reason: String) -> EditFailure {
    let suggestion = suggestion_for(&decision.failure_kind);
    EditFailure {
        fact: format!(
            "判定归因：{}（来源：第 4 级语义对齐判定）。{reason}。建议：{suggestion}",
            decision.failure_kind
        ),
        next_steps: vec![suggestion.to_owned(), "重新 read 当前文件后再改".to_owned()],
    }
}

/// 构造判定输入：路径、候选行号、oldText 与最像候选片段（各有界，§5.1）。
fn build_decide_input(path_label: &str, old: &str, candidates: &[&Candidate]) -> DecideInput {
    let lines: Vec<usize> = candidates.iter().map(|candidate| candidate.line).collect();
    let candidate_text = candidates
        .first()
        .map(|candidate| truncate_chars(&candidate.text, SEMANTIC_STATE_CHARS))
        .unwrap_or_default();
    DecideInput {
        state: json!({
            "path": path_label,
            "candidateLines": lines,
            "oldText": truncate_chars(old, SEMANTIC_STATE_CHARS),
            "candidateText": candidate_text,
        }),
        images: None,
        questions: EDIT_QUESTIONS.iter().map(|q| (*q).to_owned()).collect(),
    }
}

/// 取字符上限内的文本（§5.1 有界）。
fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    text.chars().take(max).collect()
}

/// 取从 start_line（1 起始）起 old 对应行数的字节区间；写回按原文件实际文本。
fn line_window(text: &str, start_line: usize, old: &str) -> (usize, usize) {
    let spans = line_spans(text);
    if spans.is_empty() {
        return (0, 0);
    }
    let count = old.lines().count().max(1);
    let start = start_line.saturating_sub(1).min(spans.len() - 1);
    let end = (start + count).min(spans.len());
    let byte_start = spans[start].0;
    let mut byte_len = if end > start {
        spans[end - 1].1 - byte_start
    } else {
        0
    };
    // oldText 以换行结尾时，把该行后的分隔符也纳入替换区间。
    if old.ends_with('\n') && end < spans.len() {
        byte_len = spans[end].0 - byte_start;
    }
    (byte_start, byte_len)
}

/// 定位并决定替换区间：三级命中直接返回；第 4 级走判定，判定不可用时保持机械归因（§3.8）。
async fn resolve_span(
    original: &str,
    old: &str,
    path_label: &str,
    decider: Option<&dyn EditDecider>,
) -> Result<(usize, usize, MatchLevel, Option<EditDecision>), EditFailure> {
    let located = locate(original, old).map_err(|msg| EditFailure {
        fact: msg,
        next_steps: vec!["重新 read 当前文件后再改".to_owned()],
    })?;
    match located {
        Locate::Hit { start, len, level } => Ok((start, len, level, None)),
        Locate::Semantic { candidates } => {
            // 机械分诊：连最像的片段都不够像 → 根本不存在，不调用判定（省一次模型调用）。
            let viable: Vec<Candidate> = candidates
                .iter()
                .filter(|candidate| candidate.similarity >= SEMANTIC_SIMILARITY_THRESHOLD)
                .cloned()
                .collect();
            let Some(decider) = decider.filter(|_| !viable.is_empty()) else {
                return Err(mechanical_failure(original, old));
            };

            let candidate_refs: Vec<&Candidate> = viable.iter().collect();
            let input = build_decide_input(path_label, old, &candidate_refs);
            let Some((decision, gate)) = decider.decide(&input).await else {
                return Err(mechanical_failure(original, old));
            };

            match apply_gate(&decision, gate, &viable) {
                ApplyGate::Apply => {
                    let (start, len) = line_window(original, viable[0].line, old);
                    Ok((start, len, MatchLevel::Semantic, Some(decision)))
                }
                ApplyGate::LowConfidence { confidence } => Err(decision_failure(
                    &decision,
                    format!(
                        "判定置信度 {confidence:.2} 低于门槛 {APPLY_CONFIDENCE_THRESHOLD:.2}，不自动写回"
                    ),
                )),
                ApplyGate::KindNotFormat { failure_kind } => Err(decision_failure(
                    &decision,
                    format!("差异类型是 {failure_kind}，不是可安全归一化的格式差异"),
                )),
                ApplyGate::NotFixable => Err(decision_failure(
                    &decision,
                    "判定认为这次修改无法安全应用".to_owned(),
                )),
                ApplyGate::Ambiguous { candidate_lines } => Err(decision_failure(
                    &decision,
                    format!(
                        "候选不唯一（行 {}），不自动写回",
                        candidate_lines
                            .iter()
                            .map(|line| line.to_string())
                            .collect::<Vec<_>>()
                            .join("、")
                    ),
                )),
            }
        }
    }
}

/// 在原文中定位 oldText：三级命中给位置与级别；都没命中时给第 4 级的候选。
/// 精确匹配优先，失败后尝试归一化匹配。
fn locate(original: &str, old: &str) -> Result<Locate, String> {
    if old.is_empty() {
        return Err("oldText 不能为空".to_owned());
    }

    match original.match_indices(old).count() {
        1 => {
            let start = original.find(old).expect("already counted");
            return Ok(Locate::Hit {
                start,
                len: old.len(),
                level: MatchLevel::Exact,
            });
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
    let wide_old = normalize_wide(old);
    let spans = line_spans(original);
    let orig_lines: Vec<&str> = spans.iter().map(|(s, e)| &original[*s..*e]).collect();
    let old_lines: Vec<&str> = old.lines().collect();

    let mut hits: Vec<usize> = Vec::new();
    if old_lines.len() <= orig_lines.len() {
        for start in 0..=(orig_lines.len() - old_lines.len()) {
            let window = &orig_lines[start..start + old_lines.len()];
            let window_text = window.join("\n");
            if normalize(&window_text) == norm_old || normalize_wide(&window_text) == wide_old {
                hits.push(start);
            }
        }
    }

    match hits.len() {
        1 => {
            let line_start = hits[0];
            // 偏移直接从原始字节的行边界取，不做 len+1 累加——
            // CRLF 文件每行是 len+2 字节，累加会一路偏小。
            let (byte_start, _) = spans[line_start];
            let (_, last_end) = spans[line_start + old_lines.len() - 1];
            let mut byte_len = last_end - byte_start;

            // oldText 以换行结尾时，把该行后的分隔符也纳入替换区间。
            if old.ends_with('\n') {
                let bytes = original.as_bytes();
                let mut nl = last_end;
                if bytes.get(nl) == Some(&b'\r') {
                    nl += 1;
                }
                if bytes.get(nl) == Some(&b'\n') {
                    nl += 1;
                }
                byte_len = nl - byte_start;
            }
            // 机械三级都没唯一判出时，locate 的更宽归一化仍命中，按第 3 级告知。
            let level = match classify_match(original, old) {
                MatchLevel::Semantic => MatchLevel::Whitespace,
                other => other,
            };
            Ok(Locate::Hit {
                start: byte_start,
                len: byte_len,
                level,
            })
        }
        // 三级都没命中：交给第 4 级（机械分诊 → 判定）。
        0 => Ok(Locate::Semantic {
            candidates: similar_candidates(original, old, SEMANTIC_TOP_K),
        }),
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
                    '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}' => {
                        '-'
                    }
                    '\u{00A0}' | '\u{2007}' | '\u{202F}' => ' ',
                    other => other,
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 第 3 级归一化：在 normalize 基础上折行首缩进与行内多空格（§5.4）。
fn normalize_wide(text: &str) -> String {
    normalize(text)
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slots::ConfidenceGate;

    #[tokio::test]
    async fn text_mode_replaces_single_occurrence() {
        let src = "fn a() {}\nfn b() {}\n";
        let edits = vec![json!({ "oldText": "fn b() {}", "newText": "fn c() {}" })];
        let out = apply_text_mode(src, &edits, "f.txt", None)
            .await
            .expect("ok")
            .updated;
        assert_eq!(out, "fn a() {}\nfn c() {}\n");
    }

    #[tokio::test]
    async fn text_mode_rejects_ambiguous_match() {
        let src = "x\nx\n";
        let edits = vec![json!({ "oldText": "x", "newText": "y" })];
        let err = apply_text_mode(src, &edits, "f.txt", None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("2 处"));
    }

    #[tokio::test]
    async fn text_mode_handles_multiple_disjoint_edits() {
        let src = "one\ntwo\nthree\n";
        let edits = vec![
            json!({ "oldText": "one", "newText": "1" }),
            json!({ "oldText": "three", "newText": "3" }),
        ];
        let out = apply_text_mode(src, &edits, "f.txt", None)
            .await
            .expect("ok")
            .updated;
        assert_eq!(out, "1\ntwo\n3\n");
    }

    #[tokio::test]
    async fn text_mode_tolerates_smart_quotes() {
        let src = "say \"hi\"\n";
        let edits = vec![json!({ "oldText": "say \u{201C}hi\u{201D}", "newText": "say bye" })];
        let out = apply_text_mode(src, &edits, "f.txt", None)
            .await
            .expect("ok")
            .updated;
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
    fn anchor_mode_preserves_crlf() {
        // CRLF 文件必须保持 CRLF，否则 git diff 会显示全文件修改。
        let src = "alpha\r\nbeta\r\n";
        let hash = line_anchor("beta");
        let out = apply_anchor_mode(src, &format!("2#{hash}"), "BETA").expect("ok");
        assert_eq!(out, "alpha\r\nBETA\r\n");
        assert_eq!(out.matches("\r\n").count(), 2, "两处 CRLF 都要在");
    }

    #[test]
    fn anchor_mode_preserves_lf() {
        let src = "alpha\nbeta\n";
        let hash = line_anchor("beta");
        let out = apply_anchor_mode(src, &format!("2#{hash}"), "BETA").expect("ok");
        assert_eq!(out, "alpha\nBETA\n");
        assert!(!out.contains('\r'));
    }

    #[test]
    fn anchor_mode_keeps_missing_trailing_newline() {
        let src = "alpha\nbeta";
        let hash = line_anchor("beta");
        let out = apply_anchor_mode(src, &format!("2#{hash}"), "BETA").expect("ok");
        assert_eq!(out, "alpha\nBETA", "原文没有末尾换行，结果也不该有");
    }

    #[tokio::test]
    async fn normalized_match_offset_is_correct_with_crlf() {
        // CRLF 文件每行是 len+2 字节。若按 len+1 累加偏移，
        // 替换会落到错误位置。这里用 LF 形式的 oldText 强制走归一化路径。
        let src = "one\r\ntwo\r\nthree\r\n";
        let edits = vec![json!({ "oldText": "one\ntwo", "newText": "1\n2" })];
        let out = apply_text_mode(src, &edits, "f.txt", None)
            .await
            .expect("ok")
            .updated;
        assert_eq!(out, "1\n2\r\nthree\r\n");
        assert!(out.ends_with("three\r\n"), "第三行不能被破坏");
    }

    #[tokio::test]
    async fn exact_match_works_on_crlf_without_touching_other_lines() {
        let src = "one\r\ntwo\r\nthree\r\n";
        let edits = vec![json!({ "oldText": "two", "newText": "TWO" })];
        let out = apply_text_mode(src, &edits, "f.txt", None)
            .await
            .expect("ok")
            .updated;
        assert_eq!(out, "one\r\nTWO\r\nthree\r\n");
    }

    #[tokio::test]
    async fn normalized_eol_match_emits_l2_notice() {
        let src = "one\r\ntwo\r\nthree\r\n";
        let edits = vec![json!({ "oldText": "one\ntwo", "newText": "1\n2" })];
        let result = apply_text_mode(src, &edits, "f.txt", None)
            .await
            .expect("ok");
        let out = result.updated;
        let notices = result.notices;
        assert!(out.ends_with("three\r\n"));
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].render(), "[NORMALIZED] level=2 kind=eol");
    }

    #[tokio::test]
    async fn normalized_whitespace_match_emits_l3_notice() {
        let src = "  fn   a() {}\n";
        let edits = vec![json!({ "oldText": "fn a() {}", "newText": "fn b() {}" })];
        let result = apply_text_mode(src, &edits, "f.txt", None)
            .await
            .expect("ok");
        let out = result.updated;
        let notices = result.notices;
        assert!(out.contains("fn b() {}"));
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].render(), "[NORMALIZED] level=3 kind=whitespace");
    }

    async fn ctx_with_file(name: &str) -> (ToolContext, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("tg-edit-run-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let file = dir.join("f.txt");
        std::fs::write(&file, "alpha\nbeta\n").expect("write");
        (ToolContext::new(&dir).expect("ctx"), dir)
    }

    /// §22.3：read 预热后的文件内容被 edit 复用——命中缓存不改变写回结果，
    /// 写前复核仍以磁盘为准（§18.5）。
    #[tokio::test]
    async fn edit_reuses_warmed_content_and_writes_correctly() {
        let (ctx, dir) = ctx_with_file("warm-edit").await;
        // read（intent 缺省 raw）触发 §22.3 的 Full 预热。
        let _ = crate::tools::read::run(&json!({ "path": "f.txt" }), &ctx)
            .await
            .expect("read 预热");
        let resp = run(
            &json!({ "path": "f.txt", "edits": [{ "oldText": "beta", "newText": "BETA" }] }),
            &ctx,
        )
        .await
        .expect("edit 成功");
        assert_eq!(resp.meta["changed"], json!(true));
        assert_eq!(
            std::fs::read_to_string(dir.join("f.txt")).expect("读回"),
            "alpha\nBETA\n"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// §17.2：edit 的路径参数走根校验，../ 逃逸返回 ISOLATION_REFUSED。
    #[tokio::test]
    async fn path_outside_allowed_roots_is_refused() {
        let (ctx, dir) = ctx_with_file("guard").await;
        let err = run(
            &json!({
                "path": "../escape.txt",
                "edits": [{ "oldText": "a", "newText": "b" }]
            }),
            &ctx,
        )
        .await
        .unwrap_err();
        assert_eq!(crate::error::govern_error(&err).code, "ISOLATION_REFUSED");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn run_reports_change_facts() {
        let (ctx, dir) = ctx_with_file("facts").await;
        let file = dir.join("f.txt");
        let resp = run(
            &json!({
                "path": file.to_str().unwrap(),
                "edits": [{ "oldText": "beta", "newText": "BETA" }]
            }),
            &ctx,
        )
        .await
        .expect("run ok");
        let facts = &resp.meta["facts"];
        assert_eq!(facts["changed"]["added"], json!(1));
        assert_eq!(facts["changed"]["removed"], json!(1));
        assert_eq!(facts["file"]["lines"], json!(2));
        assert_eq!(facts["file"]["eol"], json!("lf"));
        assert_eq!(facts["nearby"], json!("未检测到构建描述文件"));
        assert!(facts["diff"].as_str().unwrap().contains("+BETA"));
        assert!(
            facts["changed"]["path"]
                .as_str()
                .unwrap()
                .starts_with("<workspace>")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn run_error_includes_mechanical_attribution() {
        let (ctx, dir) = ctx_with_file("error").await;
        let file = dir.join("f.txt");
        let err = run(
            &json!({
                "path": file.to_str().unwrap(),
                "edits": [{ "oldText": "completely missing line", "newText": "x" }]
            }),
            &ctx,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("机械归因"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 指定内容的测试文件上下文。
    fn ctx_with_content(name: &str, content: &str) -> (ToolContext, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("tg-edit-patch-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("f.txt"), content).expect("write");
        (ToolContext::new(&dir).expect("ctx"), dir)
    }

    /// §5.4 模式 C：单 hunk 按行号定位并应用，输出与现有 edit 一致。
    #[tokio::test]
    async fn patch_mode_applies_single_hunk() {
        let (ctx, dir) = ctx_with_content("single", "alpha\nbeta\ngamma\n");
        let file = dir.join("f.txt");
        let resp = run(
            &json!({
                "path": file.to_str().unwrap(),
                "patch": "@@ -1,3 +1,3 @@\n alpha\n-beta\n+BETA\n gamma\n"
            }),
            &ctx,
        )
        .await
        .expect("run ok");
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "alpha\nBETA\ngamma\n"
        );
        assert_eq!(resp.meta["changed"], json!(true));
        assert_eq!(resp.meta["facts"]["changed"]["added"], json!(1));
        assert_eq!(resp.meta["facts"]["changed"]["removed"], json!(1));
        assert!(
            resp.meta["facts"]["diff"]
                .as_str()
                .unwrap()
                .contains("+BETA")
        );
        assert!(resp.meta["encoding"].is_string());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// §5.4 模式 C：多个 hunk 都基于原始行号，互不影响。
    #[tokio::test]
    async fn patch_mode_applies_multiple_hunks() {
        let (ctx, dir) = ctx_with_content("multi", "a\nb\nc\nd\ne\n");
        let file = dir.join("f.txt");
        run(
            &json!({
                "path": file.to_str().unwrap(),
                "patch": "@@ -1,1 +1,1 @@\n-a\n+A\n@@ -5,1 +5,1 @@\n-e\n+E\n"
            }),
            &ctx,
        )
        .await
        .expect("run ok");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "A\nb\nc\nd\nE\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// §5.4：hunk 上下文不匹配报 patch 冲突，给出实际内容且不写盘。
    #[tokio::test]
    async fn patch_mode_conflict_reports_actual_content() {
        let (ctx, dir) = ctx_with_content("conflict", "alpha\nbeta\ngamma\n");
        let file = dir.join("f.txt");
        let err = run(
            &json!({
                "path": file.to_str().unwrap(),
                "patch": "@@ -1,3 +1,3 @@\n alpha\n-WRONG\n+BETA\n gamma\n"
            }),
            &ctx,
        )
        .await
        .unwrap_err();
        let message = format!("{err:?}");
        assert!(message.contains("patch 冲突"), "{message}");
        assert!(message.contains("WRONG"), "缺期望内容：{message}");
        assert!(message.contains("beta"), "缺实际内容：{message}");
        assert!(message.contains("next_steps"), "缺可执行下一步：{message}");
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "alpha\nbeta\ngamma\n",
            "冲突时不得写盘"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// §5.4：CRLF 文件按原 EOL 写回，不被 patch 的 LF 行尾带跑。
    #[tokio::test]
    async fn patch_mode_preserves_crlf() {
        let (ctx, dir) = ctx_with_content("crlf", "alpha\r\nbeta\r\ngamma\r\n");
        let file = dir.join("f.txt");
        run(
            &json!({
                "path": file.to_str().unwrap(),
                "patch": "@@ -1,3 +1,3 @@\n alpha\n-beta\n+BETA\n gamma\n"
            }),
            &ctx,
        )
        .await
        .expect("run ok");
        let after = std::fs::read_to_string(&file).unwrap();
        assert_eq!(after, "alpha\r\nBETA\r\ngamma\r\n");
        assert_eq!(after.matches("\r\n").count(), 3, "三处 CRLF 都要在");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// §5.4 共同前置：写前快照保留改前内容。
    #[tokio::test]
    async fn patch_mode_creates_snapshot() {
        let (ctx, dir) = ctx_with_content("snapshot", "alpha\nbeta\n");
        let file = dir.join("f.txt");
        let resp = run(
            &json!({
                "path": file.to_str().unwrap(),
                "patch": "@@ -1,2 +1,2 @@\n alpha\n-beta\n+BETA\n"
            }),
            &ctx,
        )
        .await
        .expect("run ok");
        let snapshot = resp.meta["snapshot"].as_str().expect("应有快照路径");
        assert_eq!(std::fs::read_to_string(snapshot).unwrap(), "alpha\nbeta\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// §5.4：三种模式互斥，多于一个或一个都没有都要给可行动错误。
    #[tokio::test]
    async fn modes_are_mutually_exclusive() {
        let (ctx, dir) = ctx_with_file("exclusive").await;
        let file = dir.join("f.txt");
        let err = run(
            &json!({
                "path": file.to_str().unwrap(),
                "edits": [{ "oldText": "beta", "newText": "BETA" }],
                "patch": "@@ -1,1 +1,1 @@\n-alpha\n+A\n"
            }),
            &ctx,
        )
        .await
        .unwrap_err();
        let message = format!("{err:?}");
        assert!(message.contains("三选一"), "{message}");
        assert!(
            message.contains("edits") && message.contains("patch"),
            "{message}"
        );

        let err = run(&json!({ "path": file.to_str().unwrap() }), &ctx)
            .await
            .unwrap_err();
        let message = format!("{err:?}");
        assert!(
            message.contains("edits") && message.contains("anchor") && message.contains("patch"),
            "{message}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// §5.4：patch 模式未给 path 时从 ---/+++ 文件头推断。
    #[tokio::test]
    async fn patch_mode_infers_path_from_header() {
        let (ctx, dir) = ctx_with_content("infer", "alpha\nbeta\n");
        let resp = run(
            &json!({
                "patch": "--- a/f.txt\n+++ b/f.txt\n@@ -1,2 +1,2 @@\n alpha\n-beta\n+BETA\n"
            }),
            &ctx,
        )
        .await
        .expect("run ok");
        assert_eq!(
            std::fs::read_to_string(dir.join("f.txt")).unwrap(),
            "alpha\nBETA\n"
        );
        assert_eq!(resp.meta["changed"], json!(true));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// §5.4：既没 path 也没有可用文件头时明确要求补 path。
    #[tokio::test]
    async fn patch_mode_without_path_or_header_is_rejected() {
        let (ctx, dir) = ctx_with_file("no-path").await;
        let err = run(&json!({ "patch": "@@ -1,1 +1,1 @@\n-alpha\n+A\n" }), &ctx)
            .await
            .unwrap_err();
        assert!(format!("{err:?}").contains("path"), "{err:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// §5.4 / §18.5：写前复核指纹——读后被改动的文件一律拒绝写回。
    #[tokio::test]
    async fn stale_file_is_refused_before_write() {
        let dir = std::env::temp_dir().join(format!("tg-edit-stale-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let file = dir.join("f.txt");
        std::fs::write(&file, "alpha\n").expect("write");
        let stamp = FileStamp::capture(&file, b"alpha\n")
            .await
            .expect("capture");
        // 读后另一个进程改了文件。
        std::fs::write(&file, "ALPHA\n").expect("rewrite");
        let err = stamp.verify(&file).await.unwrap_err();
        assert!(format!("{err:?}").contains("读取后被"), "{err:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 测试桩：固定判定结论与门控，不依赖真实模型服务。
    struct StubDecider {
        decision: EditDecision,
        gate: ConfidenceGate,
    }

    impl EditDecider for StubDecider {
        fn decide<'a>(&'a self, _input: &'a DecideInput) -> EditFuture<'a> {
            let decision = self.decision.clone();
            let gate = self.gate;
            Box::pin(async move { Some((decision, gate)) })
        }
    }

    fn format_decision(confidence: f64) -> EditDecision {
        EditDecision {
            failure_kind: "format".to_owned(),
            fixable: true,
            confidence,
        }
    }

    /// §5.4：判定成功、高置信、唯一命中、格式差异 → 写回并给出判定元信息。
    #[tokio::test]
    async fn semantic_level_applies_when_confident_unique_and_fixable() {
        // 全角括号：三级归一化都不覆盖，正好落在第 4 级。
        let original = "let greeting = fn（a, b）;\n";
        let old = "let greeting = fn(a, b);\n";
        let decider = StubDecider {
            decision: format_decision(0.93),
            gate: ConfidenceGate::Adopt,
        };
        let result = apply_text_mode(
            original,
            &[json!({ "oldText": old, "newText": "let greeting = fn(a, b);" })],
            "f.rs",
            Some(&decider),
        )
        .await
        .expect("应当写回");
        // 写回按原文件实际文本（候选行），不是 oldText 本身。
        assert_eq!(result.updated, "let greeting = fn(a, b);\n");
        let decision = result.decision.expect("应当有判定结论");
        assert_eq!(decision.failure_kind, "format");
        assert!(decision.fixable);
    }

    /// §5.4：判定置信度不足 → 不自动写回（写操作比只读判定更严）。
    #[tokio::test]
    async fn semantic_level_rejects_low_confidence() {
        let original = "let greeting = fn（a, b）;\n";
        let old = "let greeting = fn(a, b);\n";
        let decider = StubDecider {
            decision: format_decision(0.6),
            gate: ConfidenceGate::AdoptWithLowConfidence,
        };
        let err = apply_text_mode(
            original,
            &[json!({ "oldText": old, "newText": "x" })],
            "f.rs",
            Some(&decider),
        )
        .await
        .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("判定归因"), "{message}");
        assert!(message.contains("不自动写回"), "{message}");
    }

    /// §5.4：差异类型不是格式差异 → 不写回。
    #[tokio::test]
    async fn semantic_level_rejects_non_format_kind() {
        let original = "let greeting = fn（a, b）;\n";
        let old = "let greeting = fn(a, b);\n";
        let decider = StubDecider {
            decision: EditDecision {
                failure_kind: "file_changed".to_owned(),
                fixable: true,
                confidence: 0.95,
            },
            gate: ConfidenceGate::Adopt,
        };
        let err = apply_text_mode(
            original,
            &[json!({ "oldText": old, "newText": "x" })],
            "f.rs",
            Some(&decider),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("file_changed"), "{err}");
    }

    /// §5.4：候选不唯一 → 拒绝并列出候选行号（绝不猜）。
    #[tokio::test]
    async fn semantic_level_rejects_ambiguous_candidates() {
        let original = "let x = fn（a, b）;\nlet x = fn（a, b）;\n";
        let old = "let x = fn(a, b);\n";
        let decider = StubDecider {
            decision: format_decision(0.95),
            gate: ConfidenceGate::Adopt,
        };
        let err = apply_text_mode(
            original,
            &[json!({ "oldText": old, "newText": "x" })],
            "f.rs",
            Some(&decider),
        )
        .await
        .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("候选不唯一"), "{message}");
        assert!(message.contains('1'), "{message}");
    }

    /// §3.8：判定不可用 → 保持现有机械归因路径，工具照常报错。
    #[tokio::test]
    async fn semantic_level_without_decider_falls_back_to_mechanical() {
        let original = "let greeting = fn（a, b）;\n";
        let old = "let greeting = fn(a, b);\n";
        let err = apply_text_mode(
            original,
            &[json!({ "oldText": old, "newText": "x" })],
            "f.rs",
            None,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("机械归因"), "{err}");
    }

    #[test]
    fn anchor_mode_rejects_stale_hash() {
        let src = "alpha\nbeta\n";
        let err = apply_anchor_mode(src, "2#0000", "X").unwrap_err();
        assert!(err.to_string().contains("锚点失配"));
    }
}
