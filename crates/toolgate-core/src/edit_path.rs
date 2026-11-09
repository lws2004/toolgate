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
        });
    }
    EditPlan { notices }
}

/// §5.4 失败归因：机械能判的三种，其余留给 d1。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    ContextInsufficient,
    FormatDifference,
    NotFound,
}

impl FailureKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            FailureKind::ContextInsufficient => "上下文不足",
            FailureKind::FormatDifference => "格式差异",
            FailureKind::NotFound => "根本不存在",
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
