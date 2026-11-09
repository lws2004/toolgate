//! 原生 `/v1/rerank` 端点的请求构造与响应解析（默认层纯逻辑，ADR 0007）。
//!
//! `source: rerank` 走独立协议族，不复用 `ordered` 的 chat 形状（§18.1）。
//! HTTP 发送在 client.rs 的 `#[cfg(feature = "external-protocols")]` 分支（§19.6）。
//!
//! 架构依据：18-deployment.md §18.1（原生 rerank 端点）、03-slots.md §3.2
//! （rerank 契约）、§3.11（形状不符即降级）、ADR 0007。

use serde_json::{Value, json};

use crate::slots::{ContractViolation, RerankInput, RerankOutput, RerankResult};

fn violation(fact: impl Into<String>) -> ContractViolation {
    ContractViolation {
        code: "CAPABILITY_MISMATCH",
        fact: fact.into(),
        next_steps: vec![
            "确认上游返回原生 /v1/rerank 形状的 results 数组".to_owned(),
            "把 source 改为 rerank 并把 adapter 设为 native".to_owned(),
            "从该模型的 serves 里移除此槽位，让它按 §3.8 降级".to_owned(),
        ],
    }
}

/// 服务根地址 → 端点（`base_url` 不含 `/v1`，§18.1）。
pub fn endpoint(base_url: &str) -> String {
    format!("{}/v1/rerank", base_url.trim_end_matches('/'))
}

/// 槽位输入 → 原生端点请求体；`model` 必须与上游启动名一致，不做别名推断。
pub fn build_request(input: &RerankInput, model: &str) -> Result<Value, ContractViolation> {
    if input.documents.is_empty() {
        return Err(violation("rerank 的 documents 为空"));
    }
    Ok(json!({
        "model": model,
        "query": input.query,
        "documents": input.documents,
    }))
}

/// 原生端点响应 → 槽位形状；§3.2 的覆盖 / 保序校验由调用方执行。
pub fn parse_output(value: &Value) -> Result<RerankOutput, ContractViolation> {
    let results = value
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| violation("原生 rerank 响应缺 results 数组"))?;
    let mut parsed = Vec::with_capacity(results.len());
    for item in results {
        let index = item
            .get("index")
            .and_then(Value::as_u64)
            .ok_or_else(|| violation("results[].index 缺失或非整数"))?;
        let score = item
            .get("relevance_score")
            .and_then(Value::as_f64)
            .ok_or_else(|| violation("results[].relevance_score 缺失或非数值"))?;
        parsed.push(RerankResult {
            index: index as usize,
            score,
        });
    }
    Ok(RerankOutput { results: parsed })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> RerankInput {
        RerankInput {
            query: "apple".to_owned(),
            documents: vec!["apple fruit".to_owned(), "a car".to_owned()],
            top_n: Some(1),
        }
    }

    #[test]
    fn request_is_native_shape_and_drops_top_n() {
        let value = build_request(&input(), "/models/jina-reranker").unwrap();
        assert_eq!(value["model"], "/models/jina-reranker");
        assert_eq!(value["query"], "apple");
        assert_eq!(value["documents"][1], "a car");
        assert!(value.get("top_n").is_none(), "top_n 不透传");
        assert!(value.get("messages").is_none(), "不是 chat 形状");
    }

    #[test]
    fn empty_documents_fails_with_actionable_steps() {
        let mut empty = input();
        empty.documents.clear();
        let violation = build_request(&empty, "m").unwrap_err();
        assert!(!violation.fact.is_empty());
        assert!(!violation.next_steps.is_empty(), "错误必须带可行动下一步");
    }

    #[test]
    fn response_maps_relevance_score_and_drops_document() {
        let value = json!({
            "id": "rerank-1",
            "results": [
                {"index": 0, "relevance_score": 0.641, "document": {"text": "apple fruit"}},
                {"index": 1, "relevance_score": 0.453, "document": {"text": "a car"}}
            ]
        });
        let output = parse_output(&value).unwrap();
        assert_eq!(output.results.len(), 2);
        assert_eq!(output.results[0].index, 0);
        assert_eq!(output.results[0].score, 0.641);
        assert_eq!(output.results[1].score, 0.453);
        assert!(
            output.validate(&input()).is_ok(),
            "覆盖全部输入且按 score 降序"
        );
    }

    #[test]
    fn malformed_response_fails_with_actionable_steps() {
        let violation = parse_output(&json!({"results": [{"index": 0}]})).unwrap_err();
        assert!(!violation.fact.is_empty());
        assert!(!violation.next_steps.is_empty());
    }

    #[test]
    fn endpoint_appends_v1_rerank_once() {
        assert_eq!(
            endpoint("http://127.0.0.1:10020"),
            "http://127.0.0.1:10020/v1/rerank"
        );
        assert_eq!(
            endpoint("http://127.0.0.1:10020/"),
            "http://127.0.0.1:10020/v1/rerank"
        );
    }
}
