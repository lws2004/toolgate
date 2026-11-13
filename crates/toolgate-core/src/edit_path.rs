//! edit 的智能路径（§5.4 四级匹配 / §5.7 改动后事实返回 / §3.8）。
//!
//! 纯逻辑：匹配级别判定、NORMALIZED 告知、机械失败归因、diff 事实与 nearby 清单。
//! 第 4 级语义对齐需 decide 槽位（可选层，§3.3 未接入），本阶段降级并告知。

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Value, json};
use similar::{ChangeTag, TextDiff};

use crate::notice::{NormalizeKind, NormalizeLevel, Notice, SlotFallback};
use crate::slots::{ConfidenceGate, DecideInput};

/// §5.4 的四级匹配策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchLevel {
    Exact,
    Eol,
    Whitespace,
    /// 机械三级都没唯一命中，需第 4 级 d1 语义对齐或确认不存在。
    Semantic,
}

fn hits(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    haystack.match_indices(needle).count()
}

fn normalize_eol(text: &str) -> String {
    text.lines()
        .map(|l| l.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
}

fn normalize_ws(text: &str) -> String {
    normalize_eol(text)
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 判定 old 在 original 中的匹配级别；只看「唯一命中」，多处由调用方拒绝。
pub fn classify_match(original: &str, old: &str) -> MatchLevel {
    if hits(original, old) == 1 {
        return MatchLevel::Exact;
    }
    if hits(&normalize_eol(original), &normalize_eol(old)) == 1 {
        return MatchLevel::Eol;
    }
    if hits(&normalize_ws(original), &normalize_ws(old)) == 1 {
        return MatchLevel::Whitespace;
    }
    MatchLevel::Semantic
}

/// §4.4b：第 2/3 级归一化命中必须告知。
pub fn normalization_notice(level: MatchLevel) -> Option<Notice> {
    match level {
        MatchLevel::Eol => Some(Notice::Normalized {
            level: NormalizeLevel::L2,
            kind: NormalizeKind::Eol,
        }),
        MatchLevel::Whitespace => Some(Notice::Normalized {
            level: NormalizeLevel::L3,
            kind: NormalizeKind::Whitespace,
        }),
        MatchLevel::Exact | MatchLevel::Semantic => None,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EditPlan {
    pub notices: Vec<Notice>,
}

/// decide_slot_available 本阶段恒 false；第 4 级真实对齐随可选层接入。
pub fn plan_edit(level: MatchLevel, decide_slot_available: bool) -> EditPlan {
    let mut notices = Vec::new();
    if let Some(n) = normalization_notice(level) {
        notices.push(n);
    }
    if level == MatchLevel::Semantic && !decide_slot_available {
        notices.push(Notice::SlotUnavailable {
            slot: "decide".to_owned(),
            fallback: SlotFallback::Mechanical,
            reason: None,
        });
    }
    EditPlan { notices }
}

/// §5.4 失败归因：机械能判的几种，其余留给 d1。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    ContextInsufficient,
    FormatDifference,
    NotFound,
    /// §5.4：模式 C 的 hunk 校验失败（文件已变或 hunk 算错）。
    /// 不是判定类别里的第六类——判定仍在「文件已变」与其它之间区分它。
    PatchConflict,
}

impl FailureKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            FailureKind::ContextInsufficient => "上下文不足",
            FailureKind::FormatDifference => "格式差异",
            FailureKind::NotFound => "根本不存在",
            FailureKind::PatchConflict => "patch 冲突",
        }
    }
}

pub fn mechanical_attribution(original: &str, old: &str) -> FailureKind {
    if old.trim().len() < 8 {
        return FailureKind::ContextInsufficient;
    }
    if hits(&normalize_ws(original), &normalize_ws(old)) > 0 {
        return FailureKind::FormatDifference;
    }
    FailureKind::NotFound
}

// ---------- §5.4 第 4 级：语义对齐（接 decide 槽位） ----------

/// 机械分诊的相似度阈值：低于它直接按「根本不存在」处理，不调用判定。
pub const SEMANTIC_SIMILARITY_THRESHOLD: f32 = 0.6;

/// 候选歧义边界：第 1 名与第 2 名相似度差小于它就算多处相似（§5.4 硬约束 2：不猜）。
pub const SEMANTIC_AMBIGUITY_MARGIN: f32 = 0.05;

/// 候选个数（top-k）：多处相似时由调用方按「绝不猜」拒绝。
pub const SEMANTIC_TOP_K: usize = 3;

/// 判定输入的截断长度（§5.1 有界：oldText 与候选片段各约 2000 字符）。
pub const SEMANTIC_STATE_CHARS: usize = 2000;

/// 判定驱动写操作的置信度门槛：比只读判定更严（写操作不可逆，§5.4）。
pub const APPLY_CONFIDENCE_THRESHOLD: f64 = 0.85;

/// 旧文本在文件里的最相似候选。
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// 候选起始行号（1 起始）。
    pub line: usize,
    /// 相似度（0..1，1 = 完全一致）。
    pub similarity: f32,
    /// 候选原文（该行窗口的真实文本）。
    pub text: String,
}

/// 找最相似的若干行窗口候选：机械分诊，不调用模型。
///
/// 相似度按字符级算（TextDiff::ratio，1 = 完全一致）：只有格式差异的内容
/// （缩进、全角标点、个别字符不同）仍然很相似，而真正缺失的内容很低——
/// 这正是第 4 级要接手的区间（§5.4 的规则优先口径）。
pub fn similar_candidates(original: &str, old: &str, top_k: usize) -> Vec<Candidate> {
    let old_lines: Vec<&str> = old.lines().collect();
    let file_lines: Vec<&str> = original.lines().collect();
    if old_lines.is_empty() || file_lines.is_empty() {
        return vec![];
    }
    let window = old_lines.len().min(file_lines.len());
    let old_owned = old.to_owned();
    let mut candidates: Vec<Candidate> = Vec::new();
    for start in 0..=(file_lines.len() - window) {
        let text = file_lines[start..start + window].join("\n");
        let similarity = TextDiff::from_chars(&text, &old_owned).ratio();
        candidates.push(Candidate {
            line: start + 1,
            similarity,
            text,
        });
    }
    candidates.sort_by(|a, b| {
        b.similarity
            .partial_cmp(&a.similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.line.cmp(&b.line))
    });
    candidates.truncate(top_k);
    candidates
}

/// §5.4 第 4 级的判定问题集：六个二元问题，一次请求读出全部概率。
///
/// 位置不交给判定（systemone 不返回 span）：最像的候选位置由工具层用
/// 机械相似度给出，判定只回答「差异类型」与「能否安全应用」。
pub const EDIT_QUESTIONS: [&str; 6] = [
    "模型给的原文是否只是上下文不足（文件里有相似内容，但缺少更多上下文）？",
    "文件内容是否已经改变（模型记的是旧版本）？",
    "差异是否只是格式层面的（空白、缩进、行尾、引号风格）？",
    "是否存在多处相似片段导致歧义？",
    "目标内容是否根本不存在？",
    "能否通过归一化安全地应用这次修改？",
];

/// 六个问题的稳定 key（与 EDIT_QUESTIONS 一一对应；归并只按 key 取值）。
pub const EDIT_QUESTION_KEYS: [&str; 6] = [
    "kind_missing_context",
    "kind_file_changed",
    "kind_format",
    "kind_conflict",
    "kind_absent",
    "fixable",
];

/// 判定给出的差异类型（五个 kind 问的 argmax）。
pub const EDIT_FAILURE_KINDS: [&str; 5] = [
    "missing_context",
    "file_changed",
    "format",
    "conflict",
    "absent",
];

/// §5.4 第 4 级的判定结论。
#[derive(Debug, Clone, PartialEq)]
pub struct EditDecision {
    /// 差异类型：missing_context / file_changed / format / conflict / absent。
    pub failure_kind: String,
    /// 是否能安全应用（fixable 概率 ≥ 门槛）。
    pub fixable: bool,
    /// 六问中的最小概率：本次结论的置信度。
    pub confidence: f64,
}

/// 归并第 4 级判定（纯函数）：六问概率 → 差异类型 + 能否安全应用。
pub fn merge_edit_decision(probs: &BTreeMap<String, f64>) -> EditDecision {
    let p = |key: &str| probs.get(key).copied().unwrap_or(0.0);
    let mut best_index = 0usize;
    let mut best = p(EDIT_QUESTION_KEYS[0]);
    for (index, key) in EDIT_QUESTION_KEYS[..5].iter().enumerate() {
        let candidate = p(key);
        if candidate > best {
            best = candidate;
            best_index = index;
        }
    }
    EditDecision {
        failure_kind: EDIT_FAILURE_KINDS[best_index].to_owned(),
        fixable: p(EDIT_QUESTION_KEYS[5]) >= APPLY_CONFIDENCE_THRESHOLD,
        // confidence 是「本次结论所依赖的问题概率的最小值」：被选中的类别与 fixable。
        // **不是全部问题的最小值**：多选问题的非答案概率天然就低（它们本来就该低），
        // 取全部最小值会把任何多问题判定压到低置信档，§7.2 的门控等于失效
        // （2026-11-09 本机实测：edit 的 file_changed 0.5191 却因其余问题低而被压成 0.01）。
        confidence: best.min(p(EDIT_QUESTION_KEYS[5])),
    }
}

/// 第 4 级的应用门槛（纯函数）：全部满足才写回，缺一不可。
#[derive(Debug, Clone, PartialEq)]
pub enum ApplyGate {
    /// 允许写回。
    Apply,
    /// 置信度不足：p < 门槛一律不自动写回。
    LowConfidence {
        /// 本次判定的置信度。
        confidence: f64,
    },
    /// 差异类型不是格式差异。
    KindNotFormat {
        /// 判定给出的差异类型。
        failure_kind: String,
    },
    /// 判定认为不能安全应用。
    NotFixable,
    /// 候选不唯一（多处相似）：拒绝并列出候选行号。
    Ambiguous {
        /// 候选起始行号。
        candidate_lines: Vec<usize>,
    },
}

/// 应用门槛判定（纯函数）：置信度够、判定认为可安全应用、候选唯一、
/// 差异类型是格式差异——四条缺一不可（§5.4 硬约束）。
///
/// 候选唯一：只剩一个候选，或第 1 名与第 2 名的相似度差足够大；
/// 判定给出 conflict 也按歧义处理（多处相似就列候选行号，绝不猜）。
pub fn apply_gate(
    decision: &EditDecision,
    gate: ConfidenceGate,
    candidates: &[Candidate],
) -> ApplyGate {
    if gate != ConfidenceGate::Adopt || decision.confidence < APPLY_CONFIDENCE_THRESHOLD {
        return ApplyGate::LowConfidence {
            confidence: decision.confidence,
        };
    }
    if !decision.fixable {
        return ApplyGate::NotFixable;
    }
    let candidate_lines: Vec<usize> = candidates.iter().map(|candidate| candidate.line).collect();
    let ambiguous = decision.failure_kind == "conflict"
        || (candidates.len() > 1
            && (candidates[0].similarity - candidates[1].similarity) < SEMANTIC_AMBIGUITY_MARGIN);
    if ambiguous {
        return ApplyGate::Ambiguous { candidate_lines };
    }
    if decision.failure_kind != "format" {
        return ApplyGate::KindNotFormat {
            failure_kind: decision.failure_kind.clone(),
        };
    }
    ApplyGate::Apply
}

/// 差异类型 → 建议（机械映射；判定不生成自由文本，§5.4）。
pub fn suggestion_for(failure_kind: &str) -> &'static str {
    match failure_kind {
        "missing_context" => "扩大 oldText 的上下文后重试",
        "file_changed" => "重新 read 当前文件后再改",
        "format" => "补更多上下文，或改用 write 重写这一段",
        "conflict" => "补上下文或改用行锚点模式（行号#哈希）",
        "absent" => "确认文件路径是否正确，或重新 read 当前内容",
        _ => "重新 read 当前文件后再改",
    }
}

/// 判定的异步返回类型（不用 async-trait，避免新增依赖，§19.6）。
pub type EditFuture<'a> = std::pin::Pin<
    Box<dyn std::future::Future<Output = Option<(EditDecision, ConfidenceGate)>> + Send + 'a>,
>;

/// 判定入口：可替换实现，便于测试注入桩而不依赖真实模型服务。
pub trait EditDecider {
    /// 发起一次判定；不可用时返回 None，调用方按机械路径降级（§3.8）。
    fn decide<'a>(&'a self, input: &'a DecideInput) -> EditFuture<'a>;
}

/// §5.7 的 diff 事实：增删行数与行号范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffStats {
    pub added: usize,
    pub removed: usize,
    /// 第一个与最后一个变化行（1 起始）；无变化为 [0, 0]。
    pub lines: [usize; 2],
}

pub fn diff_stats(before: &str, after: &str) -> DiffStats {
    let diff = TextDiff::from_lines(before, after);
    let mut added = 0usize;
    let mut removed = 0usize;
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => added += 1,
            ChangeTag::Delete => removed += 1,
            ChangeTag::Equal => {}
        }
    }
    let mut first = 0usize;
    let mut last = 0usize;
    for (idx, change) in diff.iter_all_changes().enumerate() {
        if change.tag() != ChangeTag::Equal {
            if first == 0 {
                first = idx + 1;
            }
            last = idx + 1;
        }
    }
    DiffStats {
        added,
        removed,
        lines: [first, last],
    }
}

pub fn detect_eol(text: &str) -> &'static str {
    match text.find('\n') {
        Some(i) if i > 0 && text.as_bytes()[i - 1] == b'\r' => "crlf",
        _ => "lf",
    }
}

/// §5.7 第 1 级：只列已知构建描述文件名清单，不列目录全部内容。
const BUILD_FILES: &[&str] = &[
    "Cargo.toml",
    "package.json",
    "Makefile",
    "pyproject.toml",
    "build.gradle",
    "build.gradle.kts",
    "go.mod",
    "pom.xml",
    "CMakeLists.txt",
    "justfile",
];

pub fn nearby_build_files(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut found: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| BUILD_FILES.contains(&name.as_str()))
        .collect();
    found.sort_unstable();
    found.dedup();
    found
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileFacts {
    pub lines: usize,
    pub bytes: usize,
    pub encoding: &'static str,
    pub eol: &'static str,
}

/// §5.7 的事实结构：事实齐全、不含「你该跑什么」的指令。
pub fn change_facts(
    path: &str,
    stats: DiffStats,
    file: FileFacts,
    nearby: Vec<String>,
    diff: &str,
) -> Value {
    let nearby_value = if nearby.is_empty() {
        json!("未检测到构建描述文件")
    } else {
        json!(nearby)
    };
    json!({
        "changed": {
            "path": path,
            "lines": [stats.lines[0], stats.lines[1]],
            "added": stats.added,
            "removed": stats.removed,
        },
        "file": {
            "lines": file.lines,
            "bytes": file.bytes,
            "encoding": file.encoding,
            "eol": file.eol,
        },
        "nearby": nearby_value,
        "diff": diff,
    })
}

/// 生成 unified diff（§5.7 的 facts 共用；edit / write 同一实现）。
pub fn unified_diff(before: &str, after: &str, label: &str) -> String {
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

// ---------- §5.4 模式 C：扁平 unified diff ----------

/// 一个 hunk：头里的行号与行数，体里的原始行（含前缀字符）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchHunk {
    /// 旧侧起始行号（1 起始；0 表示空文件）。
    pub old_start: usize,
    /// 旧侧行数（上下文 + 删除）。
    pub old_count: usize,
    /// 新侧起始行号。
    pub new_start: usize,
    /// 新侧行数（上下文 + 新增）。
    pub new_count: usize,
    /// hunk 体：前缀（' ' / '-' / '+'）与行内容（不含行尾）。
    pub lines: Vec<(char, String)>,
}

/// 解析后的 patch：文件头标签与全部 hunk。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedPatch {
    /// --- 行的标签（可能带 a/ 前缀或时间戳）。
    pub old_label: Option<String>,
    /// +++ 行的标签。
    pub new_label: Option<String>,
    pub hunks: Vec<PatchHunk>,
}

/// patch 解析或应用失败：写清哪一行、期望什么、实际是什么（§5.4）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchConflict {
    pub fact: String,
}

impl PatchConflict {
    fn new(fact: impl Into<String>) -> Self {
        Self { fact: fact.into() }
    }
}

/// 解析 unified diff（§5.4 模式 C）。
///
/// 只认 --- / +++ 文件头与 @@ hunk；diff --git、index、mode 等元信息跳过，
/// 这样模型可以直接粘 git diff 的输出。hunk 头声明的行数不在这里校验，
/// 由 apply_patch 对照体里实际行数判定（那才是「行数与内容不符」的判据）。
pub fn parse_patch(patch: &str) -> Result<ParsedPatch, PatchConflict> {
    let mut old_label = None;
    let mut new_label = None;
    let mut hunks: Vec<PatchHunk> = Vec::new();
    let mut current: Option<PatchHunk> = None;

    for line in patch.lines() {
        // 文件头只在 hunk 之外识别：hunk 体里以 "-" 开头的删除行可能正好是
        // "--- xxx"，进 hunk 之后再匹配会把删除行当成文件头吃掉。
        if current.is_none() {
            if let Some(rest) = line.strip_prefix("--- ") {
                old_label = Some(rest.trim().to_owned());
                continue;
            }
            if let Some(rest) = line.strip_prefix("+++ ") {
                new_label = Some(rest.trim().to_owned());
                continue;
            }
        }
        if line.starts_with("@@") {
            if let Some(hunk) = current.take() {
                hunks.push(hunk);
            }
            current = Some(parse_hunk_header(line)?);
            continue;
        }
        // "\ No newline at end of file"：末尾换行标记，行内容不受影响
        // （写回时末尾换行按原文件保持）。
        if line.starts_with('\\') {
            continue;
        }
        let Some(hunk) = current.as_mut() else {
            // hunk 之外的元信息（diff --git / index / mode）忽略。
            continue;
        };
        let (prefix, body) = match line.chars().next() {
            Some(c @ (' ' | '-' | '+')) => (c, &line[c.len_utf8()..]),
            // 部分工具把上下文空行写成完全空行，按上下文空行处理。
            None => (' ', ""),
            Some(_) => {
                return Err(PatchConflict::new(format!(
                    "hunk 里出现无法识别的行前缀：{line}"
                )));
            }
        };
        hunk.lines.push((prefix, body.to_owned()));
    }
    if let Some(hunk) = current.take() {
        hunks.push(hunk);
    }

    if hunks.is_empty() {
        return Err(PatchConflict::new(
            "patch 里没有 @@ hunk 头；模式 C 需要 unified diff 文本".to_owned(),
        ));
    }
    Ok(ParsedPatch {
        old_label,
        new_label,
        hunks,
    })
}

/// 解析 @@ -12,3 +12,5 @@ 头；行数省略时按 1 计。
fn parse_hunk_header(line: &str) -> Result<PatchHunk, PatchConflict> {
    let body = line
        .strip_prefix("@@")
        .and_then(|rest| rest.split("@@").next())
        .map(str::trim)
        .ok_or_else(|| PatchConflict::new(format!("hunk 头格式不对：{line}")))?;
    let mut parts = body.split_whitespace();
    let old = parts
        .next()
        .ok_or_else(|| PatchConflict::new(format!("hunk 头缺旧侧范围：{line}")))?;
    let new = parts
        .next()
        .ok_or_else(|| PatchConflict::new(format!("hunk 头缺新侧范围：{line}")))?;
    let (old_start, old_count) = parse_range(old, '-', line)?;
    let (new_start, new_count) = parse_range(new, '+', line)?;
    Ok(PatchHunk {
        old_start,
        old_count,
        new_start,
        new_count,
        lines: Vec::new(),
    })
}

fn parse_range(raw: &str, sign: char, line: &str) -> Result<(usize, usize), PatchConflict> {
    let body = raw
        .strip_prefix(sign)
        .ok_or_else(|| PatchConflict::new(format!("hunk 头的范围要以 {sign} 开头：{line}")))?;
    let (start, count) = match body.split_once(',') {
        Some((start, count)) => (start, count),
        None => (body, "1"),
    };
    let parse = |text: &str| {
        text.parse::<usize>()
            .map_err(|_| PatchConflict::new(format!("hunk 头的行号不是数字：{line}")))
    };
    Ok((parse(start)?, parse(count)?))
}

/// 应用 patch（§5.4 模式 C）。
///
/// 按 hunk 头的旧行号定位，逐行比对旧侧（上下文行与删除行），全部匹配才应用；
/// 任一不匹配即报冲突并给出期望/实际内容。所有 hunk 都基于原始行号，
/// 校验通过后从后往前应用，前一笔不会影响后一笔的位置。
///
/// 行尾与末尾换行保持原文件形态：比对按行内容（不含行尾），写回用原文 EOL。
pub fn apply_patch(original: &str, patch: &ParsedPatch) -> Result<String, PatchConflict> {
    let lines: Vec<&str> = original.lines().collect();
    let mut edits: Vec<(usize, usize, Vec<String>)> = Vec::with_capacity(patch.hunks.len());

    for (index, hunk) in patch.hunks.iter().enumerate() {
        let old_side: Vec<&str> = hunk
            .lines
            .iter()
            .filter(|(prefix, _)| *prefix != '+')
            .map(|(_, body)| body.as_str())
            .collect();
        let new_side: Vec<String> = hunk
            .lines
            .iter()
            .filter(|(prefix, _)| *prefix != '-')
            .map(|(_, body)| body.clone())
            .collect();

        // hunk 头声明的行数必须与体里实际行数一致，否则 patch 本身是坏的。
        if old_side.len() != hunk.old_count || new_side.len() != hunk.new_count {
            return Err(PatchConflict::new(format!(
                "hunk {}（@@ -{},{} +{},{}）的行数与内容不符：头声明旧侧 {} 行 / 新侧 {} 行，体里是 {} / {}",
                index + 1,
                hunk.old_start,
                hunk.old_count,
                hunk.new_start,
                hunk.new_count,
                hunk.old_count,
                hunk.new_count,
                old_side.len(),
                new_side.len()
            )));
        }

        // old_count 为 0 表示纯插入：插入点在 old_start 之后。
        let start = if hunk.old_count == 0 {
            hunk.old_start
        } else {
            hunk.old_start.saturating_sub(1)
        };
        // 行数来自外部输入：饱和相加兜住 usize 溢出，超大值按越界拒绝。
        let end = start.saturating_add(hunk.old_count);
        if end > lines.len() {
            return Err(PatchConflict::new(format!(
                "hunk {} 的行号 {} 超出文件范围（文件共 {} 行）",
                index + 1,
                hunk.old_start,
                lines.len()
            )));
        }

        for (offset, expected) in old_side.iter().enumerate() {
            let actual = lines[start + offset];
            if actual != *expected {
                return Err(PatchConflict::new(format!(
                    "patch 冲突：hunk {} 第 {} 行（文件第 {} 行）不匹配\n期望：{}\n实际：{}",
                    index + 1,
                    offset + 1,
                    start + offset + 1,
                    expected,
                    actual
                )));
            }
        }
        edits.push((start, hunk.old_count, new_side));
    }

    edits.sort_by_key(|(start, _, _)| *start);
    for window in edits.windows(2) {
        let (start, count, _) = &window[0];
        let (next_start, _, _) = &window[1];
        if start + count > *next_start {
            return Err(PatchConflict::new(
                "patch 的 hunk 区间重叠，请合并成一个 hunk".to_owned(),
            ));
        }
    }

    let mut out_lines: Vec<String> = lines.iter().map(|line| (*line).to_owned()).collect();
    for (start, count, new_side) in edits.iter().rev() {
        out_lines.splice(*start..(*start + *count), new_side.iter().cloned());
    }

    let ending = if detect_eol(original) == "crlf" {
        "\r\n"
    } else {
        "\n"
    };
    let mut out = out_lines.join(ending);
    if original.ends_with('\n') {
        out.push_str(ending);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notice::render_notices;

    #[test]
    fn classifies_four_levels() {
        assert_eq!(classify_match("fn a() {}\n", "fn a()"), MatchLevel::Exact);
        assert_eq!(
            classify_match("fn a() {}\r\nfn b() {}\r\n", "fn a() {}\nfn b() {}"),
            MatchLevel::Eol
        );
        assert_eq!(
            classify_match("  fn   a() {}\n", "fn a() {}"),
            MatchLevel::Whitespace
        );
        assert_eq!(
            classify_match("unrelated\n", "fn missing()"),
            MatchLevel::Semantic
        );
    }

    #[test]
    fn normalization_notices_have_contract_shape() {
        let l2 = normalization_notice(MatchLevel::Eol).unwrap();
        assert_eq!(l2.render(), "[NORMALIZED] level=2 kind=eol");
        let l3 = normalization_notice(MatchLevel::Whitespace).unwrap();
        assert_eq!(l3.render(), "[NORMALIZED] level=3 kind=whitespace");
        assert!(normalization_notice(MatchLevel::Exact).is_none());
    }

    #[test]
    fn plan_edit_levels_and_semantic_degradation() {
        assert!(plan_edit(MatchLevel::Exact, false).notices.is_empty());
        assert_eq!(plan_edit(MatchLevel::Eol, false).notices.len(), 1);
        let plan = plan_edit(MatchLevel::Semantic, false);
        assert_eq!(plan.notices.len(), 1);
        assert!(render_notices(&plan.notices).contains("slot=decide"));
        assert!(plan_edit(MatchLevel::Semantic, true).notices.is_empty());
    }

    #[test]
    fn mechanical_attribution_covers_three_kinds() {
        assert_eq!(
            mechanical_attribution("abc\ndef\n", "x"),
            FailureKind::ContextInsufficient
        );
        assert_eq!(
            mechanical_attribution("fn  a() {}\n", "fn a() {}"),
            FailureKind::FormatDifference
        );
        assert_eq!(
            mechanical_attribution("abc\ndef\n", "totally absent line"),
            FailureKind::NotFound
        );
        assert_eq!(FailureKind::NotFound.as_str(), "根本不存在");
    }

    /// §5.4：判定归并——failure_kind 取 argmax，fixable 走 0.85 门槛，confidence 取最小。
    #[test]
    fn merge_edit_decision_takes_argmax_and_threshold() {
        let probs = BTreeMap::from([
            ("kind_missing_context".to_owned(), 0.2),
            ("kind_file_changed".to_owned(), 0.9),
            ("kind_format".to_owned(), 0.3),
            ("kind_conflict".to_owned(), 0.1),
            ("kind_absent".to_owned(), 0.05),
            ("fixable".to_owned(), 0.9),
        ]);
        let decision = merge_edit_decision(&probs);
        assert_eq!(decision.failure_kind, "file_changed");
        assert!(decision.fixable);
        // confidence = 结论所依赖的问题的最小值：被选中的类别（0.9）与 fixable（0.9）→ 0.9。
        // 不是六问的最小值（0.05）——非答案的问题概率天然低，用它会把门控压死。
        assert!((decision.confidence - 0.9).abs() < 1e-9);

        // fixable 未过门槛 → 不可安全应用；confidence 取 min(类别, fixable)。
        let mut low = probs.clone();
        low.insert("fixable".to_owned(), 0.5);
        let low_decision = merge_edit_decision(&low);
        assert!(!low_decision.fixable);
        assert!((low_decision.confidence - 0.5).abs() < 1e-9);
    }

    /// §5.4：建议是机械映射，判定不生成自由文本。
    #[test]
    fn suggestions_are_mapped_mechanically() {
        assert_eq!(
            suggestion_for("missing_context"),
            "扩大 oldText 的上下文后重试"
        );
        assert_eq!(suggestion_for("file_changed"), "重新 read 当前文件后再改");
        assert_eq!(
            suggestion_for("format"),
            "补更多上下文，或改用 write 重写这一段"
        );
        assert_eq!(
            suggestion_for("conflict"),
            "补上下文或改用行锚点模式（行号#哈希）"
        );
        assert_eq!(
            suggestion_for("absent"),
            "确认文件路径是否正确，或重新 read 当前内容"
        );
    }

    /// §5.4：应用门槛四条缺一不可，另加「判定说冲突也算歧义」。
    #[test]
    fn apply_gate_requires_all_four_conditions() {
        let decision = EditDecision {
            failure_kind: "format".to_owned(),
            fixable: true,
            confidence: 0.93,
        };
        let candidates = vec![Candidate {
            line: 3,
            similarity: 0.9,
            text: "x".to_owned(),
        }];
        assert_eq!(
            apply_gate(&decision, ConfidenceGate::Adopt, &candidates),
            ApplyGate::Apply
        );

        // 置信度不足。
        let low = EditDecision {
            confidence: 0.6,
            ..decision.clone()
        };
        assert!(matches!(
            apply_gate(&low, ConfidenceGate::Adopt, &candidates),
            ApplyGate::LowConfidence { .. }
        ));
        // 门控不是 Adopt。
        assert!(matches!(
            apply_gate(&decision, ConfidenceGate::BothViews, &candidates),
            ApplyGate::LowConfidence { .. }
        ));
        // 判定认为不能安全应用。
        let not_fixable = EditDecision {
            fixable: false,
            ..decision.clone()
        };
        assert_eq!(
            apply_gate(&not_fixable, ConfidenceGate::Adopt, &candidates),
            ApplyGate::NotFixable
        );
        // 差异类型不是格式差异。
        let other = EditDecision {
            failure_kind: "file_changed".to_owned(),
            ..decision.clone()
        };
        assert!(matches!(
            apply_gate(&other, ConfidenceGate::Adopt, &candidates),
            ApplyGate::KindNotFormat { .. }
        ));
        // 候选不唯一（第 1 名与第 2 名相似度接近）。
        let two = vec![
            Candidate {
                line: 3,
                similarity: 0.90,
                text: "x".to_owned(),
            },
            Candidate {
                line: 9,
                similarity: 0.88,
                text: "y".to_owned(),
            },
        ];
        assert!(matches!(
            apply_gate(&decision, ConfidenceGate::Adopt, &two),
            ApplyGate::Ambiguous { .. }
        ));
        // 判定说冲突也算歧义。
        let conflict = EditDecision {
            failure_kind: "conflict".to_owned(),
            ..decision.clone()
        };
        assert!(matches!(
            apply_gate(&conflict, ConfidenceGate::Adopt, &candidates),
            ApplyGate::Ambiguous { .. }
        ));
    }

    /// §5.4：机械分诊——只有格式差异时很相似，无关内容很低。
    #[test]
    fn similar_candidates_separates_format_difference_from_absent() {
        let original = "let greeting = fn（a, b）;\n";
        let old = "let greeting = fn(a, b);\n";
        let candidates = similar_candidates(original, old, SEMANTIC_TOP_K);
        assert_eq!(candidates.len(), 1);
        assert!(
            candidates[0].similarity >= SEMANTIC_SIMILARITY_THRESHOLD,
            "格式差异应当足够相似：{candidates:?}"
        );

        let unrelated = similar_candidates(
            original,
            "completely different statement here",
            SEMANTIC_TOP_K,
        );
        assert!(
            unrelated[0].similarity < SEMANTIC_SIMILARITY_THRESHOLD,
            "无关内容应当低于阈值：{unrelated:?}"
        );
    }

    #[test]
    fn diff_stats_counts_added_removed_and_range() {
        let stats = diff_stats("a\nb\nc\n", "a\nB\nc\nd\n");
        assert_eq!(stats.added, 2);
        assert_eq!(stats.removed, 1);
        assert!(stats.lines[0] >= 1 && stats.lines[1] >= stats.lines[0]);
        let none = diff_stats("a\n", "a\n");
        assert_eq!(none.lines, [0, 0]);
    }

    #[test]
    fn detects_eol() {
        assert_eq!(detect_eol("a\r\nb"), "crlf");
        assert_eq!(detect_eol("a\nb"), "lf");
    }

    fn apply_text(original: &str, patch: &str) -> Result<String, PatchConflict> {
        let parsed = parse_patch(patch).expect("patch 应能解析");
        apply_patch(original, &parsed)
    }

    /// §5.4：hunk 头解析——行号、行数与体里的前缀行。
    #[test]
    fn parses_unified_diff_header_and_hunks() {
        let parsed = parse_patch(
            "diff --git a/f.txt b/f.txt\n--- a/f.txt\n+++ b/f.txt\n@@ -12,3 +12,4 @@ fn foo()\n ctx\n-old\n+new1\n+new2\n ctx2\n",
        )
        .expect("应能解析");
        assert_eq!(parsed.old_label.as_deref(), Some("a/f.txt"));
        assert_eq!(parsed.new_label.as_deref(), Some("b/f.txt"));
        assert_eq!(parsed.hunks.len(), 1);
        let hunk = &parsed.hunks[0];
        assert_eq!(
            (
                hunk.old_start,
                hunk.old_count,
                hunk.new_start,
                hunk.new_count
            ),
            (12, 3, 12, 4)
        );
        assert_eq!(hunk.lines.len(), 5);
        assert_eq!(hunk.lines[0], (' ', "ctx".to_owned()));
        assert_eq!(hunk.lines[1], ('-', "old".to_owned()));
        assert_eq!(hunk.lines[2], ('+', "new1".to_owned()));
    }

    /// §5.4：没有 @@ hunk 头就不是 patch。
    #[test]
    fn parse_patch_requires_a_hunk() {
        let err = parse_patch("--- a/f.txt\n+++ b/f.txt\n").unwrap_err();
        assert!(err.fact.contains("hunk"), "{err:?}");
    }

    /// §5.4：单 hunk 按行号定位、比对上下文后应用。
    #[test]
    fn apply_patch_single_hunk() {
        let out = apply_text(
            "alpha\nbeta\ngamma\n",
            "@@ -1,3 +1,3 @@\n alpha\n-beta\n+BETA\n gamma\n",
        )
        .expect("应能应用");
        assert_eq!(out, "alpha\nBETA\ngamma\n");
    }

    /// §5.4：多个 hunk 都基于原始行号，互不影响。
    #[test]
    fn apply_patch_multiple_hunks() {
        let out = apply_text(
            "a\nb\nc\nd\ne\n",
            "@@ -1,1 +1,1 @@\n-a\n+A\n@@ -5,1 +5,1 @@\n-e\n+E\n",
        )
        .expect("应能应用");
        assert_eq!(out, "A\nb\nc\nd\nE\n");
    }

    /// §5.4：上下文行不匹配即报 patch 冲突，并给出期望与实际内容。
    #[test]
    fn apply_patch_reports_context_mismatch_with_actual_content() {
        let err = apply_text(
            "alpha\nbeta\ngamma\n",
            "@@ -1,3 +1,3 @@\n alpha\n-WRONG\n+BETA\n gamma\n",
        )
        .unwrap_err();
        assert!(err.fact.contains("patch 冲突"), "{err:?}");
        assert!(err.fact.contains("WRONG"), "缺期望内容：{err:?}");
        assert!(err.fact.contains("beta"), "缺实际内容：{err:?}");
        assert!(err.fact.contains("文件第 2 行"), "缺定位行号：{err:?}");
    }

    /// §5.4：hunk 头声明的行数与体里实际行数不符时直接拒绝。
    #[test]
    fn apply_patch_rejects_hunk_count_mismatch() {
        let err = apply_text(
            "alpha\nbeta\ngamma\n",
            "@@ -1,3 +1,3 @@\n alpha\n-beta\n+BETA\n",
        )
        .unwrap_err();
        assert!(err.fact.contains("行数与内容不符"), "{err:?}");
    }

    /// §5.4：hunk 行号超出文件范围时拒绝，不越界应用。
    #[test]
    fn apply_patch_rejects_out_of_range_hunk() {
        let err = apply_text("a\n", "@@ -9,1 +9,1 @@\n-a\n+A\n").unwrap_err();
        assert!(err.fact.contains("超出文件范围"), "{err:?}");
    }

    /// §5.4：CRLF 文件按原 EOL 写回，不被 patch 的 LF 行尾带跑。
    #[test]
    fn apply_patch_preserves_crlf() {
        let out = apply_text(
            "alpha\r\nbeta\r\ngamma\r\n",
            "@@ -1,3 +1,3 @@\n alpha\n-beta\n+BETA\n gamma\n",
        )
        .expect("应能应用");
        assert_eq!(out, "alpha\r\nBETA\r\ngamma\r\n");
        assert_eq!(out.matches("\r\n").count(), 3, "三处 CRLF 都要在");
    }

    /// §5.4：old_count 为 0 的纯插入 hunk 落在 old_start 之后。
    #[test]
    fn apply_patch_inserts_with_zero_old_count() {
        let out = apply_text("a\nb\n", "@@ -1,0 +2,1 @@\n+NEW\n").expect("应能应用");
        assert_eq!(out, "a\nNEW\nb\n");
    }

    /// §5.4：原文没有末尾换行时，结果也不补。
    #[test]
    fn apply_patch_keeps_missing_trailing_newline() {
        let out = apply_text("a\nb", "@@ -1,2 +1,2 @@\n a\n-b\n+B\n").expect("应能应用");
        assert_eq!(out, "a\nB");
    }

    /// §5.4：patch 冲突是机械归因的一类，与判定的五类差异类型分开。
    #[test]
    fn patch_conflict_is_a_mechanical_failure_kind() {
        assert_eq!(FailureKind::PatchConflict.as_str(), "patch 冲突");
    }

    #[test]
    fn nearby_only_known_build_files() {
        let dir = std::env::temp_dir().join(format!("tg-edit-nearby-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("Cargo.toml"), "").expect("write");
        std::fs::write(dir.join("random.txt"), "").expect("write");
        let found = nearby_build_files(&dir);
        assert_eq!(found, vec!["Cargo.toml".to_owned()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn change_facts_shape_and_empty_nearby_note() {
        let stats = DiffStats {
            added: 3,
            removed: 1,
            lines: [40, 44],
        };
        let file = FileFacts {
            lines: 512,
            bytes: 18342,
            encoding: "utf-8",
            eol: "lf",
        };
        let facts = change_facts(
            "<workspace>/src/auth.rs",
            stats,
            file,
            vec![],
            "@@ -40,4 +40,6 @@",
        );
        assert_eq!(facts["changed"]["added"], json!(3));
        assert_eq!(facts["changed"]["lines"], json!([40, 44]));
        assert_eq!(facts["file"]["encoding"], json!("utf-8"));
        assert_eq!(facts["nearby"], json!("未检测到构建描述文件"));
        assert!(facts["diff"].as_str().unwrap().contains("@@"));

        let with = change_facts(
            "p",
            stats,
            file,
            vec!["Cargo.toml".to_owned(), "Makefile".to_owned()],
            "d",
        );
        assert_eq!(with["nearby"].as_array().unwrap().len(), 2);
    }
}
