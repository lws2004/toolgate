//! vision 适配器：通用 chat 端点的多模态请求构造与响应解析（默认层纯逻辑）。
//!
//! §3.10 的 text 适配器在 vision 上扩展为多模态 content part：user 的 content
//! 是「文本指令 part + 图片 part」数组。图片形状由调用方给（§3.2 不预设），
//! 本轮按 chat 端点的 image content part 直接透传。HTTP 发送在 client.rs 的
//! #[cfg(feature = "external-protocols")] 分支（§19.6 可选层）。
//!
//! 架构依据：18-deployment.md §18.1（vision 适配器的协议形状）、03-slots.md §3.2
//! （vision 契约）、§3.11（形状不符即降级）。

use serde::Serialize;
use serde_json::{Value, json};

use super::text_protocol::ChatResponse;
use crate::slots::{ContractViolation, VisionBox, VisionInput, VisionOutput, VisionTask};

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

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct VisionMessage {
    pub role: String,
    /// 多模态 content：文本指令 part + 图片 part（chat 端点形状）。
    pub content: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct VisionRequest {
    pub model: String,
    /// §3.2：temperature = 0，保证同输入同输出。
    pub temperature: f32,
    pub messages: Vec<VisionMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
}

/// 四档固定任务指令（§3.2），不生成自由指令。
fn instruction(task: VisionTask) -> &'static str {
    match task {
        VisionTask::Describe => "描述这张图片的内容。",
        VisionTask::Ocr => "提取图片中的文字与布局，只输出文字内容。",
        VisionTask::Ground => {
            r#"找出图片中的界面元素，只输出一个合法 JSON 对象：{"boxes":[{"label":"...","bbox":[x,y,w,h]}]}；bbox 用原图像素坐标。"#
        }
        VisionTask::Classify => "判断这张图片属于哪一类，只输出类别名。",
    }
}

/// 槽位输入 → chat 多模态请求。
pub fn build_request(input: &VisionInput, model: &str) -> Result<VisionRequest, ContractViolation> {
    if !input.image.is_object() {
        return Err(violation("image 不是对象形态（应为 chat content part）"));
    }
    let mut text = instruction(input.task).to_owned();
    if let Some(query) = &input.query {
        text.push_str(&format!(" 补充要求：{query}"));
    }
    Ok(VisionRequest {
        model: model.to_owned(),
        temperature: 0.0,
        messages: vec![VisionMessage {
            role: "user".to_owned(),
            content: vec![json!({ "type": "text", "text": text }), input.image.clone()],
        }],
        max_tokens: None,
    })
}

/// chat 响应 → 槽位形状。ground 要求 JSON boxes，其余给文本。
pub fn parse_output(
    response: &ChatResponse,
    input: &VisionInput,
) -> Result<VisionOutput, ContractViolation> {
    let choice = response
        .choices
        .first()
        .ok_or_else(|| violation("chat 响应缺 choices[0]"))?;
    let content = choice.message.content.trim();
    if content.is_empty() {
        return Err(violation("chat 响应 choices[0].message.content 为空"));
    }
    if input.task == VisionTask::Ground {
        let value: Value = serde_json::from_str(content)
            .map_err(|_| violation("task=ground 时 content 不是合法 JSON"))?;
        let boxes: Vec<VisionBox> = serde_json::from_value(
            value
                .get("boxes")
                .cloned()
                .ok_or_else(|| violation("task=ground 时 content 缺 boxes 字段"))?,
        )
        .map_err(|_| violation("boxes 不是 VisionBox 形状（label / bbox 字段不符）"))?;
        Ok(VisionOutput {
            text: None,
            boxes: Some(boxes),
        })
    } else {
        Ok(VisionOutput {
            text: Some(content.to_owned()),
            boxes: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn vision_input(task: VisionTask) -> VisionInput {
        VisionInput {
            image: json!({ "type": "image_url", "image_url": { "url": "data:image/png;base64,AA" } }),
            task,
            query: None,
        }
    }

    fn response(content: &str) -> ChatResponse {
        serde_json::from_value(json!({
            "choices": [{ "message": { "role": "assistant", "content": content } }]
        }))
        .expect("固定形状应可解析")
    }

    #[test]
    fn request_has_text_and_image_parts_and_is_deterministic() {
        let a = build_request(&vision_input(VisionTask::Describe), "demo").unwrap();
        let b = build_request(&vision_input(VisionTask::Describe), "demo").unwrap();
        assert_eq!(a, b);
        assert_eq!(a.temperature, 0.0);
        assert_eq!(a.messages[0].content.len(), 2);
        assert_eq!(a.messages[0].content[0]["type"], json!("text"));
        assert_eq!(a.messages[0].content[1]["type"], json!("image_url"));
    }

    #[test]
    fn instruction_varies_by_task_and_query_is_appended() {
        let mut with_query = vision_input(VisionTask::Ocr);
        with_query.query = Some("只要表格".to_owned());
        let req = build_request(&with_query, "demo").unwrap();
        let text = req.messages[0].content[0]["text"].as_str().unwrap();
        assert!(text.contains("文字"));
        assert!(text.contains("只要表格"));
        let ground = build_request(&vision_input(VisionTask::Ground), "demo").unwrap();
        assert!(
            ground.messages[0].content[0]["text"]
                .as_str()
                .unwrap()
                .contains("boxes")
        );
    }

    #[test]
    fn non_object_image_is_rejected_actionably() {
        let mut bad = vision_input(VisionTask::Describe);
        bad.image = json!("not-an-object");
        let err = build_request(&bad, "demo").unwrap_err();
        assert_eq!(err.code, "CAPABILITY_MISMATCH");
        assert!(!err.next_steps.is_empty());
    }

    #[test]
    fn describe_response_maps_to_text() {
        let input = vision_input(VisionTask::Describe);
        let out = parse_output(&response("  一张截图  "), &input).unwrap();
        assert_eq!(out.text.as_deref(), Some("一张截图"));
        assert!(out.boxes.is_none());
        out.validate(&input).unwrap();
    }

    #[test]
    fn ground_response_maps_to_boxes() {
        let input = vision_input(VisionTask::Ground);
        let out = parse_output(
            &response(r#"{"boxes":[{"label":"登录按钮","bbox":[512,620,176,44]}]}"#),
            &input,
        )
        .unwrap();
        assert_eq!(out.boxes.as_ref().unwrap().len(), 1);
        assert_eq!(out.boxes.as_ref().unwrap()[0].label, "登录按钮");
        out.validate(&input).unwrap();
    }

    #[test]
    fn malformed_responses_are_rejected() {
        let input = vision_input(VisionTask::Describe);
        let empty = ChatResponse { choices: vec![] };
        assert!(parse_output(&empty, &input).is_err());
        assert!(parse_output(&response("  "), &input).is_err());
        let ground = vision_input(VisionTask::Ground);
        assert!(parse_output(&response("not json"), &ground).is_err());
        assert!(parse_output(&response(r#"{"foo":1}"#), &ground).is_err());
        // 负坐标由 §3.2 的 validate 拦下。
        let neg = parse_output(
            &response(r#"{"boxes":[{"label":"x","bbox":[-1,0,1,1]}]}"#),
            &ground,
        )
        .unwrap();
        assert!(neg.validate(&ground).is_err());
    }
}
