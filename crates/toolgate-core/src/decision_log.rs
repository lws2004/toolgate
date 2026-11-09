//! §15.2 判定日志：每次 decide 槽位判定落一行 JSONL 到本地。
//!
//! 只存输入哈希、问题、答案与置信度，不存 state 原文；不上传。
//! 写入失败由调用方显式丢弃（观测性不进主路径，不改变工具返回）。
//!
//! 架构依据：15-risk.md §15.2（观测性与落地口径）、06-cost.md §7.3
//! （confidence 口径）、18-deployment.md §19.6（默认构建不含外部协议客户端）。

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::slm::DecideOutcome;
use crate::slots::DecideInput;

/// 默认落点：`~/.toolgate/logs/decisions.jsonl`（§15.2）。
pub fn default_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".toolgate/logs/decisions.jsonl"))
}

/// 追加一条判定记录（§15.2 落地口径）。
///
/// `confidence` 按 §7.3 的「被采纳结论所依赖的问题概率最小值」由调用方在归并
/// 结论后传入；该路径没有产出数值时传 `None`，日志记为 `null`，不编造。
/// `model_version` 取绑定模型的 `model` 名，未声明时用模型 id。
pub async fn append(
    tool: &str,
    confidence: Option<f64>,
    model_version: Option<&str>,
    input: &DecideInput,
    outcome: &DecideOutcome,
) -> std::io::Result<()> {
    let path = default_path().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "HOME 未设置，无法确定判定日志落点",
        )
    })?;
    append_to(&path, tool, confidence, model_version, input, outcome).await
}

/// 指定落点的追加写；单测走这里，不依赖 HOME，也不依赖真实模型（§15.2 落地口径）。
async fn append_to(
    path: &Path,
    tool: &str,
    confidence: Option<f64>,
    model_version: Option<&str>,
    input: &DecideInput,
    outcome: &DecideOutcome,
) -> std::io::Result<()> {
    let state_bytes = serde_json::to_vec(&input.state)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let input_hash = format!("sha256:{}", hex::encode(Sha256::digest(&state_bytes)));

    let answers: serde_json::Map<String, Value> = outcome
        .output
        .answers
        .iter()
        .map(|(question, answer)| (question.clone(), json!(answer.probability)))
        .collect();

    let record = json!({
        "ts": rfc3339_now(),
        "tool": tool,
        "slot": "decide",
        "inputHash": input_hash,
        "questions": &input.questions,
        "answers": answers,
        "confidence": confidence,
        "modelVersion": model_version,
    });
    let mut payload = serde_json::to_string(&record)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?
        .into_bytes();
    payload.push(10);

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    file.write_all(&payload).await?;
    Ok(())
}

fn rfc3339_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    rfc3339(secs)
}

/// UNIX 秒 → UTC RFC3339。只用标准库整数运算，避免新增时间依赖（§19.6）。
fn rfc3339(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    // civil_from_days 整数算法：把 epoch 天数换成公历年月日。
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    let year = y + if m <= 2 { 1 } else { 0 };
    format!(
        "{year:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        sod / 3_600,
        (sod % 3_600) / 60,
        sod % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slots::{ConfidenceGate, DecideAnswer, DecideOutput};
    use std::collections::BTreeMap;

    fn fixture() -> (DecideInput, DecideOutcome) {
        let input = DecideInput {
            state: json!("SECRET_STATE_ORIGINAL_TEXT"),
            images: None,
            questions: vec!["has_error".to_owned(), "error_kind".to_owned()],
        };
        let mut answers = BTreeMap::new();
        answers.insert(
            "has_error".to_owned(),
            DecideAnswer {
                probability: 0.92,
                span: None,
            },
        );
        answers.insert(
            "error_kind".to_owned(),
            DecideAnswer {
                probability: 0.71,
                span: None,
            },
        );
        let outcome = DecideOutcome {
            output: DecideOutput { answers },
            gate: ConfidenceGate::Adopt,
            latency_ms: 12,
            calibrated: true,
        };
        (input, outcome)
    }

    fn tmp_path(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "toolgate-decision-log-{}-{tag}-{nanos}.jsonl",
            std::process::id()
        ))
    }

    #[tokio::test]
    async fn writes_fixed_fields_and_never_the_state_original() {
        let path = tmp_path("fields");
        let (input, outcome) = fixture();
        append_to(
            &path,
            "bash",
            Some(0.71),
            Some("d1-2b@q4"),
            &input,
            &outcome,
        )
        .await
        .unwrap();
        let body = tokio::fs::read_to_string(&path).await.unwrap();
        let record: Value = serde_json::from_str(body.trim()).unwrap();
        assert_eq!(record["tool"], "bash");
        assert_eq!(record["slot"], "decide");
        assert_eq!(record["confidence"], 0.71);
        assert_eq!(record["modelVersion"], "d1-2b@q4");
        assert_eq!(record["answers"]["has_error"], 0.92);
        assert_eq!(record["questions"][0], "has_error");
        let hash = record["inputHash"].as_str().unwrap();
        assert!(hash.starts_with("sha256:"), "{hash}");
        assert_eq!(hash.len(), 7 + 64);
        assert_eq!(record["ts"].as_str().unwrap().len(), 20);
        assert!(!body.contains("SECRET_STATE_ORIGINAL_TEXT"));
        let _ = tokio::fs::remove_file(&path).await;
    }

    #[tokio::test]
    async fn appends_instead_of_overwriting_and_allows_null_fields() {
        let path = tmp_path("append");
        let (input, outcome) = fixture();
        append_to(&path, "task", None, None, &input, &outcome)
            .await
            .unwrap();
        append_to(&path, "task", None, None, &input, &outcome)
            .await
            .unwrap();
        let body = tokio::fs::read_to_string(&path).await.unwrap();
        assert_eq!(body.lines().count(), 2);
        let record: Value = serde_json::from_str(body.lines().next().unwrap()).unwrap();
        assert!(record["confidence"].is_null());
        assert!(record["modelVersion"].is_null());
        let _ = tokio::fs::remove_file(&path).await;
    }

    #[test]
    fn rfc3339_matches_known_utc_instants() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(86_400), "1970-01-02T00:00:00Z");
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339(1_794_268_800), "2026-11-10T00:00:00Z");
    }
}
