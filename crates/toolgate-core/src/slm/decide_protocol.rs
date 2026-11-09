//! decide 的 json 适配器：通用 chat 端点上的命名问题判定（默认层纯逻辑）。
//!
//! §3.10 的 json 适配器把 decide 槽位的 state + questions 转成 prompt，要求模型
//! 输出 {answers: {q0: {probability}}}，再转回 §3.2 的 DecideOutput 形状。
//! HTTP 发送在 client.rs 的 #[cfg(feature = "external-protocols")] 分支（§19.6 可选层）。
//!
//! 注意：json 适配器产出的是模型自报概率、非校准（§3.10）；高风险判定只能用
//! systemone 的 native 路径，或由云端复核（§7.2）。
//!
//! 架构依据：18-deployment.md §18.1（json 适配器的 decide 形状）、03-slots.md §3.2
//! （decide 契约）、§3.11（形状不符即降级）。

use std::collections::BTreeMap;

use serde_json::Value;

use super::text_protocol::{ChatMessage, ChatRequest, ChatResponse};
use crate::slots::{ContractViolation, DecideAnswer, DecideInput, DecideOutput};

const SYSTEM_PROMPT: &str =
    "你是本地判定组件。只输出一个合法 JSON 对象，不要解释、不要寒暄、不要 Markdown 围栏。";

fn violation(fact: impl Into<String>) -> ContractViolation {
    ContractViolation {
        code: "CAPABILITY_MISMATCH",
        fact: fact.into(),
        next_steps: vec![
            "确认上游返回 chat 形状的 choices[0].message.content".to_owned(),
            "把 adapter 改为 json 后重试".to_owned(),
            "需要校准概率时改用 source: systemone".to_owned(),
        ],
    }
}

/// 槽位输入 → chat 请求：state 与编号问题放进 user prompt。
pub fn build_request(input: &DecideInput, model: &str) -> Result<ChatRequest, ContractViolation> {
    if let Some(images) = input.images.as_deref()
        && !images.is_empty()
    {
        return Err(violation(
            "json 适配器的 decide 不接收 images；带图判定请用 source: systemone",
        ));
    }
    let state = serde_json::to_string(&input.state)
        .map_err(|err| violation(format!("state 无法序列化为 JSON：{err}")))?;
    let mut questions = String::new();
    for (index, question) in input.questions.iter().enumerate() {
        questions.push_str(&format!("q{index}: {question}\n"));
    }
    let prompt = format!(
        "对待判定的 state 与一组命名问题逐个给出「肯定回答为真的概率」。\n\
         state：\n{state}\n\n\
         问题：\n{questions}\n\
         只输出一个 JSON 对象：{{\"answers\": {{\"q0\": {{\"probability\": <0 到 1 的数>}}, ...}}}}；\
         answers 的键必须恰好覆盖全部问题，不要输出其它字段。"
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

/// chat 响应 → 槽位形状：q0… 按序还原为输入问题原文的键。
pub fn parse_output(
    response: &ChatResponse,
    input: &DecideInput,
) -> Result<DecideOutput, ContractViolation> {
    let choice = response
        .choices
        .first()
        .ok_or_else(|| violation("chat 响应缺 choices[0]"))?;
    let content = strip_fences(choice.message.content.trim());
    if content.is_empty() {
        return Err(violation("chat 响应 choices[0].message.content 为空"));
    }
    let value: Value =
        serde_json::from_str(&content).map_err(|_| violation("decide 的 content 不是合法 JSON"))?;
    let answers_value = value
        .get("answers")
        .cloned()
        .unwrap_or_else(|| value.clone());
    let object = answers_value
        .as_object()
        .ok_or_else(|| violation("answers 不是对象"))?;
    let mut answers = BTreeMap::new();
    for (index, question) in input.questions.iter().enumerate() {
        let key = format!("q{index}");
        let entry = object
            .get(&key)
            .or_else(|| object.get(question))
            .ok_or_else(|| violation(format!("answers 缺问题 {key} 的概率")))?;
        let probability = entry
            .get("probability")
            .and_then(Value::as_f64)
            .or_else(|| entry.as_f64())
            .ok_or_else(|| violation(format!("问题 {key} 缺 probability 字段")))?;
        answers.insert(
            question.clone(),
            DecideAnswer {
                probability,
                span: None,
            },
        );
    }
    Ok(DecideOutput { answers })
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

    fn input() -> DecideInput {
        DecideInput {
            state: json!({ "lines": 8000, "exitCode": 1 }),
            images: None,
            questions: vec!["有错吗".to_owned(), "需要摘要吗".to_owned()],
        }
    }

    fn response(content: &str) -> ChatResponse {
        serde_json::from_value(json!({
            "choices": [{ "message": { "role": "assistant", "content": content } }]
        }))
        .expect("固定形状应可解析")
    }

    #[test]
    fn request_maps_state_and_numbered_questions() {
        let request = build_request(&input(), "demo").unwrap();
        assert_eq!(request.temperature, 0.0);
        assert_eq!(request.messages.len(), 2);
        let prompt = &request.messages[1].content;
        assert!(prompt.contains("q0: 有错吗"));
        assert!(prompt.contains("q1: 需要摘要吗"));
        assert!(prompt.contains("8000"));
        assert!(prompt.contains("answers"));
    }

    #[test]
    fn request_rejects_images_actionably() {
        let mut with_image = input();
        with_image.images = Some(vec![json!("data:image/png;base64,AA")]);
        let err = build_request(&with_image, "demo").unwrap_err();
        assert_eq!(err.code, "CAPABILITY_MISMATCH");
        assert!(err.next_steps.iter().any(|step| step.contains("systemone")));
    }

    #[test]
    fn parses_numbered_answers_and_restores_question_keys() {
        let output = parse_output(
            &response(r#"{"answers":{"q0":{"probability":0.97},"q1":{"probability":0.12}}}"#),
            &input(),
        )
        .unwrap();
        assert_eq!(output.answers["有错吗"].probability, 0.97);
        assert_eq!(output.answers["需要摘要吗"].probability, 0.12);
        output.validate(&input()).unwrap();
    }

    #[test]
    fn parses_bare_and_fenced_variants() {
        let bare = parse_output(
            &response(r#"{"q0":{"probability":0.8},"q1":{"probability":0.2}}"#),
            &input(),
        )
        .unwrap();
        assert_eq!(bare.answers["有错吗"].probability, 0.8);

        let fenced = parse_output(
            &response("```json\n{\"answers\":{\"q0\":{\"probability\":0.7},\"q1\":{\"probability\":0.3}}}\n```"),
            &input(),
        )
        .unwrap();
        assert_eq!(fenced.answers["有错吗"].probability, 0.7);
    }

    #[test]
    fn malformed_outputs_are_rejected_actionably() {
        let empty = ChatResponse { choices: vec![] };
        assert!(parse_output(&empty, &input()).is_err());
        assert!(parse_output(&response("  "), &input()).is_err());
        assert!(parse_output(&response("not json"), &input()).is_err());
        // 缺一个问题概率 → 形状不符。
        assert!(
            parse_output(
                &response(r#"{"answers":{"q0":{"probability":0.9}}}"#),
                &input()
            )
            .is_err()
        );
        // 概率越界由 §3.2 的 validate 拦下。
        let bad = parse_output(
            &response(r#"{"answers":{"q0":{"probability":1.7},"q1":{"probability":0.2}}}"#),
            &input(),
        )
        .unwrap();
        assert!(bad.validate(&input()).is_err());
    }
}
