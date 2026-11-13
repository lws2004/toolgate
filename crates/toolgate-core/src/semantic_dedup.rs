//! 单次调用内的模型输入语义去重（§5.1 L0.5 / §5.3 L1.5）。
//!
//! 只作用于送给 decide/summarize 的输入，不回写 content；先精确行去重，
//! 再用 embedding 余弦聚类（阈值 0.92，每簇保留首行）。embedding 不可用
//! 或行数超出窗口时返回 `None`，调用方用原窗口。

use std::collections::HashSet;

use crate::slots::EmbeddingInput;
use crate::tools::ToolContext;

const MIN_LINES: usize = 50;
const MAX_LINES: usize = 200;
const MAX_CHARS: usize = 8000;
const BATCH: usize = 32;
const THRESHOLD: f64 = 0.92;

#[derive(Clone, Copy, Debug)]
pub struct Stats {
    pub input_lines: usize,
    pub output_lines: usize,
    pub batches: usize,
}

#[derive(Clone, Debug)]
pub struct Outcome {
    pub text: String,
    pub stats: Stats,
}

/// 取正文尾部 8000 字符窗口后去重（bash/task 状态尾部）。
pub async fn dedup_tail(ctx: &ToolContext, body: &str) -> Option<Outcome> {
    let window: String = body
        .chars()
        .rev()
        .take(MAX_CHARS)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    dedup_lines(ctx, &window).await
}

/// 对整段文本去重（read 的 8000 字符 bounded）。
pub async fn dedup_all(ctx: &ToolContext, text: &str) -> Option<Outcome> {
    dedup_lines(ctx, text).await
}

async fn dedup_lines(ctx: &ToolContext, text: &str) -> Option<Outcome> {
    let lines: Vec<&str> = text.lines().collect();
    if !(MIN_LINES..=MAX_LINES).contains(&lines.len()) {
        return None;
    }
    let mut seen = HashSet::new();
    let mut unique: Vec<&str> = Vec::new();
    for line in &lines {
        if seen.insert(*line) {
            unique.push(*line);
        }
    }
    if unique.len() < 2 {
        return None;
    }

    let client = ctx.slm.as_ref()?;
    let mut vectors: Vec<Vec<f64>> = Vec::new();
    let mut batches = 0_usize;
    for chunk in unique.chunks(BATCH) {
        let input = EmbeddingInput {
            texts: chunk.iter().map(|line| (*line).to_owned()).collect(),
        };
        let outcome = client.embedding(&input).await.ok()?;
        vectors.extend(outcome.output.vectors);
        batches += 1;
    }
    if vectors.len() != unique.len() {
        return None;
    }

    let mut representatives: Vec<(Vec<f64>, &str)> = Vec::new();
    for (line, vector) in unique.iter().zip(vectors.iter()) {
        let near_duplicate = representatives
            .iter()
            .any(|(representative, _)| cosine(vector, representative) >= THRESHOLD);
        if !near_duplicate {
            representatives.push((vector.clone(), line));
        }
    }

    let text = representatives
        .iter()
        .map(|(_, line)| *line)
        .collect::<Vec<_>>()
        .join("\n");
    Some(Outcome {
        text,
        stats: Stats {
            input_lines: lines.len(),
            output_lines: representatives.len(),
            batches,
        },
    })
}

fn cosine(left: &[f64], right: &[f64]) -> f64 {
    if left.len() != right.len() || left.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0;
    let mut left_norm = 0.0;
    let mut right_norm = 0.0;
    for (a, b) in left.iter().zip(right.iter()) {
        dot += a * b;
        left_norm += a * a;
        right_norm += b * b;
    }
    if left_norm == 0.0 || right_norm == 0.0 {
        return 0.0;
    }
    dot / (left_norm.sqrt() * right_norm.sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_vectors_have_cosine_one() {
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn orthogonal_vectors_have_cosine_zero() {
        assert_eq!(cosine(&[1.0, 0.0], &[0.0, 1.0]), 0.0);
    }

    #[test]
    fn mismatched_dimensions_are_zero() {
        assert_eq!(cosine(&[1.0], &[1.0, 2.0]), 0.0);
    }
}
