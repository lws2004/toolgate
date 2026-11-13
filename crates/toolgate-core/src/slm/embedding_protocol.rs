//! 原生 `/v1/embeddings` 端点的请求构造与响应解析（§18.1）。
//!
//! `source: embedding` 走原生向量协议族；HTTP 发送在 client.rs 的
//! `#[cfg(feature = "external-protocols")]` 分支（§19.6）。
//!
//! 架构依据：18-deployment.md §18.1（原生向量端点）、03-slots.md §3.2
//! （embedding 契约）、§3.11（形状不符即降级）。

use serde_json::{Value, json};

use crate::slots::{ContractViolation, EmbeddingInput, EmbeddingOutput};

fn violation(fact: impl Into<String>) -> ContractViolation {
    ContractViolation {
        code: "CAPABILITY_MISMATCH",
        fact: fact.into(),
        next_steps: vec![
            "确认上游返回 OpenAI 兼容 /v1/embeddings 形状的 data[].embedding".to_owned(),
            "把 source 改为 embedding 并把 adapter 设为 native".to_owned(),
            "从该模型的 serves 里移除 embedding 槽位，让它按 §3.8 降级".to_owned(),
        ],
    }
}

/// 服务根地址 → 端点（`base_url` 不含 `/v1`，§18.1）。
pub fn endpoint(base_url: &str) -> String {
    format!("{}/v1/embeddings", base_url.trim_end_matches('/'))
}

/// 槽位输入 → 原生端点请求体；`model` 必须与上游启动名一致，不做别名推断。
pub fn build_request(input: &EmbeddingInput, model: &str) -> Result<Value, ContractViolation> {
    if input.texts.is_empty() {
        return Err(violation("embedding 的 texts 为空"));
    }
    Ok(json!({
        "model": model,
        "input": input.texts,
    }))
}

/// 原生端点响应 → 槽位形状。`expected` 是本次请求的条目数，用于拒绝缺条 /
/// 重复 index / 缺号（§3.2 等长、同序、同维；§3.11 形状不符即拒绝并降级）。
///
/// 兼容 OpenAI 形状 `data[].embedding` + 可选 `data[].index`：全部带 index 时按
/// index 升序重建顺序并校验 `0..expected` 唯一连续；无 index 时按出现顺序；
/// 部分带 index 视为形状不符。
pub fn parse_output(value: &Value, expected: usize) -> Result<EmbeddingOutput, ContractViolation> {
    let data = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| violation("原生 embedding 响应缺 data 数组"))?;
    if data.len() != expected {
        return Err(violation(format!(
            "响应条数 {} 与请求条数 expected={expected} 不一致",
            data.len()
        )));
    }
    let mut indexed: Vec<(usize, Vec<f64>)> = Vec::with_capacity(data.len());
    let mut any_index = false;
    let mut missing_index = false;
    let mut seen = vec![false; expected];
    for (pos, item) in data.iter().enumerate() {
        let vector = item
            .get("embedding")
            .and_then(Value::as_array)
            .ok_or_else(|| violation("data[].embedding 缺失或非数组"))?
            .iter()
            .map(|x| {
                x.as_f64()
                    .ok_or_else(|| violation("data[].embedding 含非数值分量"))
            })
            .collect::<Result<Vec<f64>, ContractViolation>>()?;
        match item.get("index").and_then(Value::as_u64) {
            Some(n) => {
                any_index = true;
                let index = n as usize;
                if index >= expected || seen[index] {
                    return Err(violation(format!(
                        "data[].index={index} 越界或重复（expected={expected}）"
                    )));
                }
                seen[index] = true;
                indexed.push((index, vector));
            }
            None => {
                missing_index = true;
                indexed.push((pos, vector));
            }
        }
    }
    if any_index && missing_index {
        return Err(violation("部分 data 项带 index、部分缺失，无法保证同序"));
    }
    if any_index {
        // 长度已等于 expected，seen 全真即 index 唯一且连续。
        if let Some(gap) = seen.iter().position(|hit| !hit) {
            return Err(violation(format!("data[].index 缺号：缺少 {gap}")));
        }
        indexed.sort_by_key(|(index, _)| *index);
    }
    let vectors = indexed.into_iter().map(|(_, vector)| vector).collect();
    Ok(EmbeddingOutput { vectors })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> EmbeddingInput {
        EmbeddingInput {
            texts: vec!["a".to_owned(), "b".to_owned()],
        }
    }

    #[test]
    fn request_is_native_shape_and_keeps_texts() {
        let req = build_request(&input(), "bge-m3").unwrap();
        assert_eq!(req["model"], json!("bge-m3"));
        assert_eq!(req["input"], json!(["a", "b"]));
    }

    #[test]
    fn empty_texts_fails_with_actionable_steps() {
        let input = EmbeddingInput { texts: vec![] };
        let err = build_request(&input, "m").unwrap_err();
        assert_eq!(err.code, "CAPABILITY_MISMATCH");
        assert!(!err.next_steps.is_empty());
    }

    #[test]
    fn response_reorders_by_index() {
        let value = json!({
            "data": [
                {"index": 1, "embedding": [0.3, 0.4]},
                {"index": 0, "embedding": [0.1, 0.2]}
            ]
        });
        let out = parse_output(&value, 2).unwrap();
        assert_eq!(out.vectors, vec![vec![0.1, 0.2], vec![0.3, 0.4]]);
    }

    #[test]
    fn duplicate_or_missing_index_is_rejected() {
        let dup = json!({
            "data": [
                {"index": 0, "embedding": [0.1]},
                {"index": 0, "embedding": [0.2]}
            ]
        });
        assert!(parse_output(&dup, 2).is_err());
        let gap = json!({
            "data": [
                {"index": 0, "embedding": [0.1]},
                {"index": 2, "embedding": [0.3]}
            ]
        });
        assert!(parse_output(&gap, 2).is_err());
        let short = json!({"data": [{"index": 0, "embedding": [0.1]}]});
        assert!(parse_output(&short, 2).is_err());
        let mixed = json!({
            "data": [
                {"index": 0, "embedding": [0.1]},
                {"embedding": [0.2]}
            ]
        });
        assert!(parse_output(&mixed, 2).is_err());
    }

    #[test]
    fn response_without_index_keeps_order() {
        let value = json!({"data": [{"embedding": [1.0]}, {"embedding": [2.0]}]});
        let out = parse_output(&value, 2).unwrap();
        assert_eq!(out.vectors, vec![vec![1.0], vec![2.0]]);
    }

    #[test]
    fn malformed_response_fails_with_actionable_steps() {
        let err = parse_output(&json!({"foo": 1}), 2).unwrap_err();
        assert_eq!(err.code, "CAPABILITY_MISMATCH");
        assert!(!err.next_steps.is_empty());
    }

    #[test]
    fn endpoint_appends_v1_embeddings_once() {
        assert_eq!(
            endpoint("http://127.0.0.1:10020/"),
            "http://127.0.0.1:10020/v1/embeddings"
        );
    }
}
