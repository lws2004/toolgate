//! text 适配器：通用 chat 端点的请求构造与响应解析（默认层纯逻辑）。
//!
//! §3.10 的 text 适配器把 summarize 槽位请求转成 prompt + 纯文本 / JSON 输出，
//! 再把响应转回 §3.2 的 SummarizeOutput 形状。HTTP 发送在 client.rs 的
//! #[cfg(feature = "external-protocols")] 分支（§19.6 可选层）。
//!
//! 架构依据：18-deployment.md §18.1（text 适配器的协议形状）、03-slots.md §3.2
//! （summarize 契约）、§3.11（形状不符即降级）。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::slots::{ContractViolation, SummarizeInput, SummarizeOutput};

const SYSTEM_PROMPT: &str =
    "你是本地文本处理组件。只输出用户要求的结果，不要解释、不要寒暄、不要 Markdown 围栏。";
const JSON_SYSTEM_PROMPT: &str =
    "你是本地文本处理组件。只输出一个合法的 JSON 对象，不要解释、不要 Markdown 围栏。";

fn violation(fact: impl Into<String>) -> ContractViolation {
    ContractViolation {
        code: "CAPABILITY_MISMATCH",
        fact: fact.into(),
        next_steps: vec![
            "确认上游返回 chat 形状的 choices[0].message.content".to_owned(),
            "把 adapter 改为 text 后重试".to_owned(),
            "从该模型的 serves 里移除此槽位，让它按 §3.8 降级".to_owned(),
        ],
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ChatRequest {
    pub model: String,
    /// §3.2：temperature = 0，保证同输入同输出。
    pub temperature: f32,
    pub messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ChatChoice {
    pub message: ChatMessage,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ChatResponse {
    pub choices: Vec<ChatChoice>,
}

/// 端点：base_url 是 API 根（含 /v1），拼 /chat/completions。
pub fn endpoint(base_url: &str) -> String {
    format!("{}/chat/completions", base_url.trim_end_matches('/'))
}

/// 槽位输入 → chat 请求。有 schema 时要求模型只输出 JSON 对象。
pub fn build_request(
    input: &SummarizeInput,
    model: &str,
) -> Result<ChatRequest, ContractViolation> {
    if input.text.is_empty() {
        return Err(violation("summarize 的 text 为空"));
    }
    let system = if input.schema.is_some() {
        JSON_SYSTEM_PROMPT
    } else {
        SYSTEM_PROMPT
    };
    Ok(ChatRequest {
        model: model.to_owned(),
        temperature: 0.0,
        messages: vec![
            ChatMessage {
                role: "system".to_owned(),
                content: system.to_owned(),
            },
            ChatMessage {
                role: "user".to_owned(),
                content: input.text.clone(),
            },
        ],
        max_tokens: input.max_tokens,
    })
}

/// chat 响应 → 槽位形状。有 schema 时把 content 解析成 JSON 给 data，否则给 text。
pub fn parse_output(
    response: &ChatResponse,
    input: &SummarizeInput,
) -> Result<SummarizeOutput, ContractViolation> {
    let choice = response
        .choices
        .first()
        .ok_or_else(|| violation("chat 响应缺 choices[0]"))?;
    let content = choice.message.content.trim();
    if content.is_empty() {
        return Err(violation("chat 响应 choices[0].message.content 为空"));
    }
    if input.schema.is_some() {
        let data: Value = serde_json::from_str(content)
            .map_err(|_| violation("有 schema 时 content 不是合法 JSON"))?;
        Ok(SummarizeOutput {
            text: None,
            data: Some(data),
        })
    } else {
        Ok(SummarizeOutput {
            text: Some(content.to_owned()),
            data: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn response(content: &str) -> ChatResponse {
        serde_json::from_value(json!({
            "choices": [{ "message": { "role": "assistant", "content": content } }]
        }))
        .expect("固定形状应可解析")
    }

    #[test]
    fn endpoint_appends_chat_completions() {
        assert_eq!(
            endpoint("http://127.0.0.1:8080/v1/"),
            "http://127.0.0.1:8080/v1/chat/completions"
        );
    }

    #[test]
    fn request_is_deterministic_and_uses_temperature_zero() {
        let input = SummarizeInput {
            text: "一段日志".to_owned(),
            schema: None,
            max_tokens: Some(200),
        };
        let a = build_request(&input, "demo").unwrap();
        let b = build_request(&input, "demo").unwrap();
        assert_eq!(a, b);
        assert_eq!(a.temperature, 0.0);
        assert_eq!(a.model, "demo");
        assert_eq!(a.max_tokens, Some(200));
        assert_eq!(a.messages.len(), 2);
        assert_eq!(a.messages[1].content, "一段日志");
    }

    #[test]
    fn empty_text_is_rejected_actionably() {
        let input = SummarizeInput { text: String::new(), schema: None, max_tokens: None };
        let err = build_request(&input, "demo").unwrap_err();
        assert!(!err.next_steps.is_empty());
        assert_eq!(err.code, "CAPABILITY_MISMATCH");
    }

    #[test]
    fn plain_response_maps_to_text() {
        let input = SummarizeInput { text: "x".to_owned(), schema: None, max_tokens: None };
        let out = parse_output(&response("  摘要内容  "), &input).unwrap();
        assert_eq!(out.text.as_deref(), Some("摘要内容"));
        assert!(out.data.is_none());
        out.validate(&input).unwrap();
    }

    #[test]
    fn schema_response_maps_to_data() {
        let input = SummarizeInput {
            text: "x".to_owned(),
            schema: Some(json!({ "type": "object" })),
            max_tokens: None,
        };
        let out = parse_output(&response(r#"{"a":1}"#), &input).unwrap();
        assert_eq!(out.data, Some(json!({ "a": 1 })));
        assert!(out.text.is_none());
        out.validate(&input).unwrap();
    }

    #[test]
    fn malformed_responses_are_rejected() {
        let input = SummarizeInput { text: "x".to_owned(), schema: None, max_tokens: None };
        let empty = ChatResponse { choices: vec![] };
        assert!(parse_output(&empty, &input).is_err());
        assert!(parse_output(&response("  "), &input).is_err());
        let json_input = SummarizeInput {
            text: "x".to_owned(),
            schema: Some(json!({ "type": "object" })),
            max_tokens: None,
        };
        assert!(parse_output(&response("not json"), &json_input).is_err());
    }
}
