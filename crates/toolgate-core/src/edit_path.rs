//! edit 的智能路径（§5.4 四级匹配 / §5.7 改动后事实返回 / §3.8）。
//!
//! 纯逻辑：匹配级别判定、NORMALIZED 告知、机械失败归因、diff 事实与 nearby 清单。
//! 第 4 级语义对齐需 decide 槽位（可选层，§3.3 未接入），本阶段降级并告知。

use std::path::Path;

use serde_json::{Value, json};
use similar::{ChangeTag, TextDiff};

use crate::notice::{NormalizeKind, NormalizeLevel, Notice, SlotFallback};

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
