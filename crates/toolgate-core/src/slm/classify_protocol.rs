//! classify 适配器：通用 chat 端点的 JSON 打分请求构造与响应解析（默认层纯逻辑）。
//!
//! §3.10 的 json 适配器把 classify 槽位请求转成 prompt + JSON schema 要求，
//! 再把响应转回 §3.2 的 ClassifyOutput 形状。HTTP 发送在 client.rs 的
//! #[cfg(feature = "external-protocols")] 分支（§19.6 可选层）。
//!
//! 架构依据：18-deployment.md §18.1（classify 的 JSON schema 形状）、03-slots.md §3.2
//! （classify 契约）、§3.11（形状不符即降级）。

use serde_json::Value;

use super::text_protocol::{ChatMessage, ChatRequest, ChatResponse};
use crate::slots::{ClassifyInput, ClassifyOutput, ContractViolation};

const SYSTEM_PROMPT: &str =
    "你是本地分类组件。只输出一个合法 JSON 对象，不要解释、不要 Markdown 围栏。";

fn violation(fact: impl Into<String>) -> ContractViolation {
    ContractViolation {
        code: "CAPABILITY_MISMATCH",
        fact: fact.into(),
        next_steps: vec![
            "确认上游返回 chat 形状的 choices[0].message.content".to_owned(),
            "把 adapter 改为 json 后重试".to_owned(),
            "从该模型的 serves 里移除此槽位，让它按 §3.8 降级".to_owned(),
        ],
    }
}

/// 槽位输入 → chat 请求：system 要求只输出 JSON，user 给出标签集与待分类文本。
pub fn build_request(input: &ClassifyInput, model: &str) -> Result<ChatRequest, ContractViolation> {
    if input.text.is_empty() {
        return Err(violation("classify 的 text 为空"));
    }
    if !(2..=50).contains(&input.labels.len()) {
        return Err(violation(format!(
            "labels 长度 {} 不在 2-50 内",
            input.labels.len()
        )));
    }
    let mut seen = std::collections::BTreeSet::new();
    for label in &input.labels {
        if !seen.insert(label.as_str()) {
            return Err(violation(format!("labels 有重复项：{label}")));
        }
    }
    let labels = input
        .labels
        .iter()
        .map(|label| format!("\"{label}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let prompt = format!(
        "对下面的文本按给定标签逐个打分。标签列表：[{labels}]。\n\
         只输出一个 JSON 对象：{{\"scores\": {{\"<标签>\": <0 到 1 的分数>, ...}}}}；\
         scores 的键必须恰好是标签列表里的每一项，不要多写标签，不要输出其它字段。\n\
         文本：\n{}",
        input.text
    );
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
                content: prompt,
            },
        ],
        max_tokens: None,
    })
}

/// chat 响应 → 槽位形状。接受 {"scores": {...}} 或直接的 {标签: 分数} 两种形状。
pub fn parse_output(
    response: &ChatResponse,
    _input: &ClassifyInput,
) -> Result<ClassifyOutput, ContractViolation> {
    let choice = response
        .choices
        .first()
        .ok_or_else(|| violation("chat 响应缺 choices[0]"))?;
    let content = strip_fences(choice.message.content.trim());
    if content.is_empty() {
        return Err(violation("chat 响应 choices[0].message.content 为空"));
    }
    let value: Value = serde_json::from_str(&content)
        .map_err(|_| violation("classify 的 content 不是合法 JSON"))?;
    let scores = value
        .get("scores")
        .cloned()
        .unwrap_or_else(|| value.clone());
    let output: ClassifyOutput = serde_json::from_value(serde_json::json!({ "scores": scores }))
        .map_err(|_| violation("scores 不是 {标签: 分数} 形状"))?;
    Ok(output)
}

/// 宽容剥离常见 Markdown 围栏；其余内容原样保留。
fn strip_fences(content: &str) -> String {
    let text = content.trim();
    if let Some(rest) = text.strip_prefix("```") {
        let without_lang = rest.split_once('\n').map(|(_, body)| body).unwrap_or(rest);
        if let Some(body) = without_lang.trim_end().strip_suffix("```") {
            return body.trim().to_owned();
        }
    }
    text.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn input() -> ClassifyInput {
        ClassifyInput {
            text: "一段构建日志".to_owned(),
            labels: vec!["has_error".to_owned(), "is_binary".to_owned()],
        }
    }

    fn response(content: &str) -> ChatResponse {
        serde_json::from_value(json!({
            "choices": [{ "message": { "role": "assistant", "content": content } }]
        }))
        .expect("固定形状应可解析")
    }

    #[test]
    fn request_carries_labels_and_is_deterministic() {
        let a = build_request(&input(), "demo").unwrap();
        let b = build_request(&input(), "demo").unwrap();
        assert_eq!(a, b);
        assert_eq!(a.temperature, 0.0);
        assert_eq!(a.messages.len(), 2);
        assert!(a.messages[1].content.contains("has_error"));
        assert!(a.messages[1].content.contains("is_binary"));
        assert!(a.messages[1].content.contains("scores"));
    }

    #[test]
    fn rejects_empty_text_short_labels_and_duplicates() {
        let mut bad = input();
        bad.text = String::new();
        assert_eq!(
            build_request(&bad, "demo").unwrap_err().code,
            "CAPABILITY_MISMATCH"
        );

        let mut one = input();
        one.labels = vec!["only".to_owned()];
        assert!(build_request(&one, "demo").is_err());

        let mut dup = input();
        dup.labels = vec!["a".to_owned(), "a".to_owned()];
        assert!(build_request(&dup, "demo").is_err());
    }

    #[test]
    fn parses_scores_object_and_validates() {
        let output = parse_output(
            &response(r#"{"scores":{"has_error":0.93,"is_binary":0.04}}"#),
            &input(),
        )
        .unwrap();
        assert_eq!(output.scores["has_error"], 0.93);
        output.validate(&input()).unwrap();
    }

    #[test]
    fn parses_bare_scores_and_markdown_fences() {
        let bare =
            parse_output(&response(r#"{"has_error":0.8,"is_binary":0.1}"#), &input()).unwrap();
        assert_eq!(bare.scores["has_error"], 0.8);
        let fenced = parse_output(
            &response("```json\n{\"scores\":{\"has_error\":0.7,\"is_binary\":0.2}}\n```"),
            &input(),
        )
        .unwrap();
        assert_eq!(fenced.scores["has_error"], 0.7);
    }

    #[test]
    fn malformed_outputs_are_rejected_actionably() {
        let empty = ChatResponse { choices: vec![] };
        assert!(parse_output(&empty, &input()).is_err());
        assert!(parse_output(&response("  "), &input()).is_err());
        assert!(parse_output(&response("not json"), &input()).is_err());
        // key 集合不符由 §3.2 的 validate 拦下。
        let missing = parse_output(&response(r#"{"scores":{"has_error":0.9}}"#), &input()).unwrap();
        assert!(missing.validate(&input()).is_err());
    }
}
