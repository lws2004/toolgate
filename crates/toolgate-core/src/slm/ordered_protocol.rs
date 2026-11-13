//! ordered 适配器：通用 chat 端点的 rerank 请求构造与响应解析（默认层纯逻辑）。
//!
//! §3.10 的 ordered 适配器把 rerank 槽位请求转成「query + 编号候选列表 → 排序 JSON」，
//! 再把响应转回 §3.2 的 RerankOutput 形状。与 text 适配器共用同一个 chat 端点与
//! 请求/响应结构，差别只在 prompt 与输出形状；HTTP 发送在 client.rs 的
//! #[cfg(feature = "external-protocols")] 分支（§19.6 可选层）。
//!
//! 架构依据：18-deployment.md §18.1（ordered 适配器的协议形状）、03-slots.md §3.2
//! （rerank 契约）、§3.5（rerank 必须独立成槽位）、§3.11（形状不符即降级）。

use serde_json::Value;

use super::text_protocol::{ChatMessage, ChatRequest, ChatResponse};
use crate::slots::{ContractViolation, RerankInput, RerankOutput};

const SYSTEM_PROMPT: &str = r#"你是本地重排序组件。只输出一个合法的 JSON 对象，形如 {"results":[{"index":0,"score":0.9}]}；index 是候选的原始编号，score 在 [0,1]，results 必须覆盖全部候选且按 score 降序。不要解释、不要 Markdown 围栏。"#;

fn violation(fact: impl Into<String>) -> ContractViolation {
    ContractViolation {
        code: "CAPABILITY_MISMATCH",
        fact: fact.into(),
        next_steps: vec![
            "确认上游返回 chat 形状的 choices[0].message.content 且为排序 JSON".to_owned(),
            "把 adapter 改为 ordered 后重试".to_owned(),
            "从该模型的 serves 里移除此槽位，让它按 §3.8 降级".to_owned(),
        ],
    }
}

/// 槽位输入 → chat 请求。候选按编号列全，要求模型只输出排序 JSON。
pub fn build_request(input: &RerankInput, model: &str) -> Result<ChatRequest, ContractViolation> {
    if input.documents.is_empty() {
        return Err(violation("rerank 的 documents 为空"));
    }
    let mut user = format!("查询：{}\n候选：\n", input.query);
    for (index, doc) in input.documents.iter().enumerate() {
        user.push_str(&format!("[{index}] {doc}\n"));
    }
    if let Some(top_n) = input.top_n {
        user.push_str(&format!(
            "重点关注前 {top_n} 个候选，但 results 仍须覆盖全部候选。\n"
        ));
    }
    Ok(ChatRequest {
        model: model.to_owned(),
        temperature: 0.0,
        messages: vec![
            ChatMessage {
                role: "system".to_owned(),
                content: SYSTEM_PROMPT.to_owned(),
            },
            ChatMessage {
                role: "user".to_owned(),
                content: user,
            },
        ],
        max_tokens: None,
    })
}

/// chat 响应 → 槽位形状；§3.2 的形状校验（覆盖、保序）由调用方执行。
pub fn parse_output(response: &ChatResponse) -> Result<RerankOutput, ContractViolation> {
    let choice = response
        .choices
        .first()
        .ok_or_else(|| violation("chat 响应缺 choices[0]"))?;
    let content = choice.message.content.trim();
    if content.is_empty() {
        return Err(violation("chat 响应 choices[0].message.content 为空"));
    }
    let value: Value =
        serde_json::from_str(content).map_err(|_| violation("content 不是合法 JSON"))?;
    serde_json::from_value::<RerankOutput>(value)
        .map_err(|_| violation("content 不是 RerankOutput 形状（缺 results 或字段类型不符）"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn input() -> RerankInput {
        RerankInput {
            query: "超时".to_owned(),
            documents: vec!["a.rs:1:timeout".to_owned(), "b.rs:3:error".to_owned()],
            top_n: Some(1),
        }
    }

    fn response(content: &str) -> ChatResponse {
        serde_json::from_value(json!({
            "choices": [{ "message": { "role": "assistant", "content": content } }]
        }))
        .expect("固定形状应可解析")
    }

    #[test]
    fn request_is_deterministic_and_lists_all_candidates() {
        let a = build_request(&input(), "demo").unwrap();
        let b = build_request(&input(), "demo").unwrap();
        assert_eq!(a, b);
        assert_eq!(a.temperature, 0.0);
        assert_eq!(a.model, "demo");
        let user = &a.messages[1].content;
        assert!(user.contains("查询：超时"));
        assert!(user.contains("[0] a.rs:1:timeout"));
        assert!(user.contains("[1] b.rs:3:error"));
        assert!(user.contains("重点关注前 1 个"));
    }

    #[test]
    fn empty_documents_is_rejected_actionably() {
        let mut empty = input();
        empty.documents.clear();
        let err = build_request(&empty, "demo").unwrap_err();
        assert_eq!(err.code, "CAPABILITY_MISMATCH");
        assert!(!err.next_steps.is_empty());
    }

    #[test]
    fn valid_response_maps_to_rerank_output() {
        let out = parse_output(&response(
            r#"{"results":[{"index":1,"score":0.9},{"index":0,"score":0.2}]}"#,
        ))
        .unwrap();
        assert_eq!(out.results.len(), 2);
        assert_eq!(out.results[0].index, 1);
        out.validate(&input()).unwrap();
    }

    #[test]
    fn malformed_responses_are_rejected() {
        let empty = ChatResponse { choices: vec![] };
        assert!(parse_output(&empty).is_err());
        assert!(parse_output(&response("  ")).is_err());
        assert!(parse_output(&response("not json")).is_err());
        assert!(parse_output(&response(r#"{"foo":1}"#)).is_err());
    }
}
