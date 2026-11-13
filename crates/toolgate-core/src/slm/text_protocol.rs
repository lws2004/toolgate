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
/// 无 schema 的摘要指令（§18.1 text 适配器）：不写这一句，模型会把原文当用户消息
/// 继续对话，返回「We need to…」式推理过程而不是摘要。
const SUMMARY_INSTRUCTION: &str =
    "请把下面的内容压缩成简洁摘要：只保留结论、关键数据与待办，不复述推理过程，不使用 Markdown 围栏。";
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
    /// 上游结束原因：`length` 表示被 max_tokens 截断，输出不能当完整摘要（§3.11）。
    #[serde(default)]
    pub finish_reason: Option<String>,
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
    // 有 schema 时把 schema 本身写进 user prompt：只说「输出 JSON」而不给结构，
    // 模型只能猜字段名与类型（§3.2「输出按 schema 校验」）。
    let user = match &input.schema {
        Some(schema) => format!(
            "{}\n\n输出必须符合以下 JSON Schema：\n{}",
            input.text,
            serde_json::to_string_pretty(schema).unwrap_or_else(|_| "{}".to_owned())
        ),
        None => format!("{SUMMARY_INSTRUCTION}\n\n{}", input.text),
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
                content: user,
            },
        ],
        max_tokens: input.max_tokens,
    })
}

/// 用错 chat 协议的特征（§3.1）：模型把摘要请求当对话，先输出「我们要…」式推理。
/// 只挡开头的明显引导词，不做语义判定——这是形状校验，不是内容质量判定。
fn looks_like_reasoning(content: &str) -> bool {
    let head = content.trim_start().to_ascii_lowercase();
    const PREFIXES: &[&str] = &[
        "we need", "we should", "we will", "let me", "i need", "i'll", "i will", "the user",
        "sure,", "okay,", "first, i",
    ];
    PREFIXES.iter().any(|prefix| head.starts_with(prefix))
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
    if choice.finish_reason.as_deref() == Some("length") {
        return Err(violation("chat 响应因 max_tokens 被截断（finish_reason=length）"));
    }
    if input.schema.is_none() && looks_like_reasoning(content) {
        return Err(violation(
            "chat 响应是推理过程而非摘要正文（模型把 prompt 当成了对话）",
        ));
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
        assert!(a.messages[1].content.contains("一段日志"));
        assert!(a.messages[1].content.contains("摘要"));
    }

    #[test]
    fn empty_text_is_rejected_actionably() {
        let input = SummarizeInput {
            text: String::new(),
            schema: None,
            max_tokens: None,
        };
        let err = build_request(&input, "demo").unwrap_err();
        assert!(!err.next_steps.is_empty());
        assert_eq!(err.code, "CAPABILITY_MISMATCH");
    }

    #[test]
    fn plain_response_maps_to_text() {
        let input = SummarizeInput {
            text: "x".to_owned(),
            schema: None,
            max_tokens: None,
        };
        let out = parse_output(&response("  摘要内容  "), &input).unwrap();
        assert_eq!(out.text.as_deref(), Some("摘要内容"));
        assert!(out.data.is_none());
        out.validate(&input).unwrap();
    }

    #[test]
    fn schema_response_maps_to_data_and_schema_reaches_prompt() {
        let input = SummarizeInput {
            text: "x".to_owned(),
            schema: Some(json!({
                "type": "object",
                "properties": { "summary": { "type": "string" } },
                "required": ["summary"]
            })),
            max_tokens: None,
        };
        let request = build_request(&input, "demo").unwrap();
        let prompt = &request.messages[1].content;
        assert!(prompt.contains("JSON Schema"), "{prompt}");
        assert!(prompt.contains("summary"), "{prompt}");
        let out = parse_output(&response(r#"{"summary":"ok"}"#), &input).unwrap();
        assert_eq!(out.data, Some(json!({ "summary": "ok" })));
        assert!(out.text.is_none());
        out.validate(&input).unwrap();
    }

    #[test]
    fn malformed_responses_are_rejected() {
        let input = SummarizeInput {
            text: "x".to_owned(),
            schema: None,
            max_tokens: None,
        };
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

    #[test]
    fn reasoning_output_is_rejected_instead_of_returned_as_summary() {
        let input = SummarizeInput {
            text: "x".to_owned(),
            schema: None,
            max_tokens: None,
        };
        assert!(parse_output(&response("We need to parse the user's input…"), &input).is_err());
        // 形状合法的摘要仍然放行；引导词判定只针对开头。
        assert!(parse_output(&response("结论：构建通过。"), &input).is_ok());
    }

    #[test]
    fn truncated_output_is_rejected() {
        let input = SummarizeInput {
            text: "x".to_owned(),
            schema: None,
            max_tokens: None,
        };
        let truncated: ChatResponse = serde_json::from_value(json!({
            "choices": [{
                "message": { "role": "assistant", "content": "摘要开头" },
                "finish_reason": "length"
            }]
        }))
        .expect("固定形状应可解析");
        assert!(parse_output(&truncated, &input).is_err());
    }
}
