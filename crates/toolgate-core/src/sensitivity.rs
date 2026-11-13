//! 输出敏感度标注（§5.1/§5.2/§5.5）：classify 槽位优先，机械正则兜底。
//!
//! 只产出标签事实，不改变原文；classify 不可用时回退机械判定并标
//! `source=mechanical`，不发 `SLOT_UNAVAILABLE`（附加标签不改变返回形态）。

use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::notice::SensitivityLevel;
use crate::slots::ClassifyInput;
use crate::tools::ToolContext;

const LABELS: [&str; 3] = ["secret", "personal", "normal"];
const HEAD_TAIL_CHARS: usize = 2000;
const SENSITIVE_THRESHOLD: f64 = 0.5;

/// 标注级别；`Normal` 只出现在 `meta.sensitivity`，不会进 notice。
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Normal,
    Personal,
    Secret,
}

impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Level::Normal => "normal",
            Level::Personal => "personal",
            Level::Secret => "secret",
        }
    }

    fn notice_level(self) -> Option<SensitivityLevel> {
        match self {
            Level::Normal => None,
            Level::Personal => Some(SensitivityLevel::Personal),
            Level::Secret => Some(SensitivityLevel::Secret),
        }
    }
}

/// 判定来源：classify 槽位或机械正则。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    Classify,
    Mechanical,
}

impl Source {
    fn as_str(self) -> &'static str {
        match self {
            Source::Classify => "classify",
            Source::Mechanical => "mechanical",
        }
    }
}

/// 一次输出标注的结果。
#[derive(Clone, Debug)]
pub struct Sensitivity {
    pub level: Level,
    pub score: Option<f64>,
    pub source: Source,
    pub calibrated: bool,
}

impl Sensitivity {
    /// 命中 `secret` / `personal` 时需要发 `SENSITIVE`。
    pub fn notice_level(&self) -> Option<SensitivityLevel> {
        self.level.notice_level()
    }

    /// `meta.sensitivity` 的形状。
    pub fn to_meta(&self) -> Value {
        let mut value = json!({
            "level": self.level.as_str(),
            "source": self.source.as_str(),
        });
        if let Some(score) = self.score {
            value["score"] = json!((score * 100.0).round() / 100.0);
        }
        if self.source == Source::Classify {
            value["calibrated"] = json!(self.calibrated);
        }
        value
    }
}

/// 对文本打敏感度标签。输入取头尾各 2000 字符（≤4000），classify 失败回退机械。
pub async fn assess(ctx: &ToolContext, text: &str) -> Sensitivity {
    let window = head_tail(text);
    let mechanical_result = mechanical(&window);
    if let Some(client) = ctx.slm.as_ref() {
        let input = ClassifyInput {
            text: window.clone(),
            labels: LABELS.iter().map(|label| (*label).to_owned()).collect(),
        };
        if let Ok(outcome) = client.classify(&input).await {
            let score = |label: &str| outcome.output.scores.get(label).copied().unwrap_or(0.0);
            let secret = score("secret");
            let personal = score("personal");
            let normal = score("normal");
            let (level, value) = if secret >= SENSITIVE_THRESHOLD {
                (Level::Secret, secret)
            } else if personal >= SENSITIVE_THRESHOLD {
                (Level::Personal, personal)
            } else {
                (Level::Normal, normal)
            };
            return stronger(
                mechanical_result,
                Sensitivity {
                    level,
                    score: Some(value),
                    source: Source::Classify,
                    calibrated: outcome.calibrated,
                },
            );
        }
    }
    mechanical_result
}

/// 机械命中是下限：classify 判得更低时不覆盖（防模型漏判）。
fn stronger(mechanical: Sensitivity, classify: Sensitivity) -> Sensitivity {
    if level_rank(mechanical.level) > level_rank(classify.level) {
        mechanical
    } else {
        classify
    }
}

fn level_rank(level: Level) -> u8 {
    match level {
        Level::Normal => 0,
        Level::Personal => 1,
        Level::Secret => 2,
    }
}

fn mechanical(text: &str) -> Sensitivity {
    if secret_patterns().iter().any(|pattern| pattern.is_match(text)) {
        return Sensitivity {
            level: Level::Secret,
            score: None,
            source: Source::Mechanical,
            calibrated: false,
        };
    }
    if personal_patterns().iter().any(|pattern| pattern.is_match(text)) {
        return Sensitivity {
            level: Level::Personal,
            score: None,
            source: Source::Mechanical,
            calibrated: false,
        };
    }
    Sensitivity {
        level: Level::Normal,
        score: None,
        source: Source::Mechanical,
        calibrated: false,
    }
}

fn secret_patterns() -> &'static [Regex] {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        vec![
            Regex::new(r"-----BEGIN [A-Z ]*PRIVATE KEY-----").unwrap(),
            Regex::new(
                r#"(?i)(api[_-]?key|access[_-]?token|secret|password|passwd|credential)\s*[:=]\s*['\"]?[A-Za-z0-9_\-]{8,}"#,
            )
            .unwrap(),
            Regex::new(r"(?i)\b(sk-[A-Za-z0-9]{20,}|ghp_[A-Za-z0-9]{20,}|xox[baprs]-[A-Za-z0-9-]{10,})")
                .unwrap(),
        ]
    })
}

fn personal_patterns() -> &'static [Regex] {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        vec![
            Regex::new(r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b").unwrap(),
            Regex::new(r"\b1[3-9]\d{9}\b").unwrap(),
            Regex::new(r"\b\d{17}[\dXx]\b").unwrap(),
        ]
    })
}

/// 头尾各 2000 字符；不截断时原样返回。
fn head_tail(text: &str) -> String {
    if text.chars().count() <= HEAD_TAIL_CHARS * 2 {
        return text.to_owned();
    }
    let head: String = text.chars().take(HEAD_TAIL_CHARS).collect();
    let tail: String = text
        .chars()
        .rev()
        .take(HEAD_TAIL_CHARS)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{head}\n…\n{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_key_is_secret() {
        let result = mechanical("-----BEGIN RSA PRIVATE KEY-----\nabc\n");
        assert_eq!(result.level, Level::Secret);
        assert_eq!(result.source, Source::Mechanical);
        assert!(result.score.is_none());
    }

    #[test]
    fn access_token_is_secret() {
        let result = mechanical("api_key = abcdefgh12345678");
        assert_eq!(result.level, Level::Secret);
    }

    #[test]
    fn email_is_personal() {
        let result = mechanical("联系 user@example.com");
        assert_eq!(result.level, Level::Personal);
    }

    #[test]
    fn plain_text_is_normal() {
        assert_eq!(mechanical("hello world").level, Level::Normal);
    }

    #[test]
    fn head_tail_keeps_both_ends() {
        let text = format!("{}{}{}", "A".repeat(3000), "B".repeat(5000), "C".repeat(3000));
        let window = head_tail(&text);
        assert!(window.starts_with('A'));
        assert!(window.ends_with('C'));
        assert!(window.contains('…'));
    }

    #[test]
    fn mechanical_secret_beats_classify_normal() {
        let mechanical_result = Sensitivity {
            level: Level::Secret,
            score: None,
            source: Source::Mechanical,
            calibrated: false,
        };
        let classify = Sensitivity {
            level: Level::Normal,
            score: Some(0.95),
            source: Source::Classify,
            calibrated: true,
        };
        let merged = stronger(mechanical_result, classify);
        assert_eq!(merged.level, Level::Secret);
        assert_eq!(merged.source, Source::Mechanical);
        assert!(merged.score.is_none());
    }

    #[test]
    fn classify_wins_when_not_lower() {
        let mechanical_result = Sensitivity {
            level: Level::Normal,
            score: None,
            source: Source::Mechanical,
            calibrated: false,
        };
        let classify = Sensitivity {
            level: Level::Personal,
            score: Some(0.87),
            source: Source::Classify,
            calibrated: true,
        };
        assert_eq!(stronger(mechanical_result, classify).source, Source::Classify);
    }

    #[test]
    fn meta_carries_source_and_rounds_score() {
        let result = Sensitivity {
            level: Level::Secret,
            score: Some(0.876),
            source: Source::Classify,
            calibrated: true,
        };
        let meta = result.to_meta();
        assert_eq!(meta["level"], "secret");
        assert_eq!(meta["source"], "classify");
        assert_eq!(meta["score"], serde_json::json!(0.88));
        assert_eq!(meta["calibrated"], true);
    }
}

