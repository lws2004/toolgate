//! SystemOne 协议适配（默认层纯逻辑）：请求构造、响应解析、端点拼接。
//!
//! d3-nano 与 StartLux 都是 jev 类 systemOne 模型，只能走 /v1/systemone：按 chat 协议
//! （/v1/chat/completions）调用会输出退化重复（ABAABA…、finish_reason=length），
//! 正确形态是 systemone 的命名问题 → 校准概率。
//!
//! 协议契约以官方 StartLux-Decision 的 docs/api.md 为准：
//! 端点 POST {base}/v1/systemone，请求
//! {"state": <string|object>, "questions": {"q0": {"type":"noul","instructions":"<问题>"}}}，
//! 响应 {"answers": {"q0": {"type":"noul","noul": 0.0373}}}；noul 即 P(yes)。
//!
//! 本模块不发起网络请求，只做形状与语义转换；HTTP 发送在 client.rs 的
//! #[cfg(feature = "external-protocols")] 分支（§19.6 可选层）。
//!
//! 架构依据：18-deployment.md §18.1（模型来源与协议形态）、03-slots.md §3.2（decide 槽位契约）、
//! §3.11（形状不符即 CAPABILITY_MISMATCH）。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::slots::{ContractViolation, DecideAnswer, DecideInput, DecideOutput};

/// 形状不符的错误码（§3.11）。
const CAPABILITY_MISMATCH: &str = "CAPABILITY_MISMATCH";

/// systemone 的问题类型：只用 noul（P(yes) 原语）。槽位契约没有类型维度，槽位侧一律发 noul。
pub const NOUL_TYPE: &str = "noul";

/// 请求里的单个问题。
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SystemOneQuestion {
    /// 问题类型；固定 noul。
    #[serde(rename = "type")]
    pub kind: String,
    /// 问题原文。
    pub instructions: String,
}

/// systemone 请求体。
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SystemOneRequest {
    /// 待判断的事实（字符串或对象）。
    pub state: Value,
    /// 问题集合：键为 q0、q1…，与输入问题顺序一一对应（保序、可回映）。
    pub questions: BTreeMap<String, SystemOneQuestion>,
    /// 图片输入；只支持字符串形态（base64 或 data URI）。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
}

/// 响应里的单个答案。
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct SystemOneAnswer {
    /// 答案类型；只接受 noul。
    #[serde(rename = "type")]
    pub kind: String,
    /// P(yes)。
    #[serde(default)]
    pub noul: Option<f64>,
}

/// systemone 响应体。
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct SystemOneResponse {
    /// 各问题的答案。
    pub answers: BTreeMap<String, SystemOneAnswer>,
    /// 用量信息；不参与判定。
    #[serde(default)]
    pub usage: Option<Value>,
    /// 上游模型名；不参与判定。
    #[serde(default)]
    pub model: Option<String>,
}

/// 把槽位输入翻译成 systemone 请求：问题按顺序映射到 q0、q1…（保序、可回映）。
///
/// images 只接受字符串形态（base64 或 data URI）；其他形态按形状不符处理（§3.11）。
pub fn build_request(input: &DecideInput) -> Result<SystemOneRequest, ContractViolation> {
    let mut questions = BTreeMap::new();
    for (index, question) in input.questions.iter().enumerate() {
        questions.insert(
            format!("q{index}"),
            SystemOneQuestion {
                kind: NOUL_TYPE.to_owned(),
                instructions: question.clone(),
            },
        );
    }

    let mut images = Vec::new();
    if let Some(items) = input.images.as_deref() {
        for (index, image) in items.iter().enumerate() {
            match image {
                Value::String(text) => images.push(text.clone()),
                other => {
                    return Err(ContractViolation {
                        code: CAPABILITY_MISMATCH,
                        fact: format!(
                            "images[{index}] 不是字符串形态（收到 {}）",
                            value_type_name(other)
                        ),
                        next_steps: next_steps(),
                    });
                }
            }
        }
    }

    Ok(SystemOneRequest {
        state: input.state.clone(),
        questions,
        images,
    })
}

/// 把 systemone 响应翻译成槽位形状的 decide 输出。
///
/// 只接受 type=noul；缺答案、缺 noul、概率越界都是形状不符（§3.11）。
pub fn parse_decide_output(
    response: &SystemOneResponse,
    questions: &[String],
) -> Result<DecideOutput, ContractViolation> {
    let mut answers = BTreeMap::new();
    for (index, question) in questions.iter().enumerate() {
        let key = format!("q{index}");
        let answer = response
            .answers
            .get(&key)
            .ok_or_else(|| ContractViolation {
                code: CAPABILITY_MISMATCH,
                fact: format!("响应缺少问题 {key} 的答案"),
                next_steps: next_steps(),
            })?;
        if answer.kind != NOUL_TYPE {
            return Err(ContractViolation {
                code: CAPABILITY_MISMATCH,
                fact: format!(
                    "问题 {key} 的答案类型是 {}，只接受 {NOUL_TYPE}",
                    answer.kind
                ),
                next_steps: next_steps(),
            });
        }
        let probability = answer.noul.ok_or_else(|| ContractViolation {
            code: CAPABILITY_MISMATCH,
            fact: format!("问题 {key} 的答案缺 noul 字段"),
            next_steps: next_steps(),
        })?;
        if !(0.0..=1.0).contains(&probability) {
            return Err(ContractViolation {
                code: CAPABILITY_MISMATCH,
                fact: format!("问题 {key} 的概率 {probability} 不在 [0,1]"),
                next_steps: next_steps(),
            });
        }
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

/// 拼接 systemone 端点：base_url 去尾部斜杠；已带 /v1 就不重复加。
pub fn endpoint(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with("/v1") {
        format!("{trimmed}/systemone")
    } else {
        format!("{trimmed}/v1/systemone")
    }
}

/// 形状不符时的可执行下一步（§1.4.5）。
fn next_steps() -> Vec<String> {
    vec![
        "确认上游服务支持 systemone 的 noul 原语".to_owned(),
        "改用 source: local 的本地权重模型，或换支持该原语的模型".to_owned(),
        "从该模型的 serves 里移除 decide 槽位，让它按 §3.8 降级".to_owned(),
    ]
}

/// 值的类型名，用于错误回显。
fn value_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "布尔值",
        Value::Number(_) => "数字",
        Value::String(_) => "字符串",
        Value::Array(_) => "数组",
        Value::Object(_) => "对象",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn input(questions: &[&str]) -> DecideInput {
        DecideInput {
            state: json!("一段构建日志"),
            images: None,
            questions: questions.iter().map(|q| (*q).to_owned()).collect(),
        }
    }

    /// 请求构造：问题顺序 → q0/q1…，instructions 原文，type=noul。
    #[test]
    fn request_maps_questions_in_order() {
        let request = build_request(&input(&["有错吗", "需要看图吗"])).expect("应当构造成功");
        assert_eq!(request.questions.len(), 2);
        let q0 = request.questions.get("q0").expect("应当有 q0");
        let q1 = request.questions.get("q1").expect("应当有 q1");
        assert_eq!(q0.instructions, "有错吗");
        assert_eq!(q1.instructions, "需要看图吗");
        assert_eq!(q0.kind, "noul");
        assert_eq!(q1.kind, "noul");
        assert_eq!(request.state, json!("一段构建日志"));
        assert!(request.images.is_empty());

        let body = serde_json::to_value(&request).expect("应当能序列化");
        assert_eq!(body["questions"]["q0"]["type"], json!("noul"));
        assert_eq!(body["questions"]["q0"]["instructions"], json!("有错吗"));
        assert_eq!(body["questions"]["q1"]["instructions"], json!("需要看图吗"));
        // 没有图片时不带 images 字段。
        assert!(body.get("images").is_none());
    }

    /// 请求构造：images 只接受字符串形态。
    #[test]
    fn request_accepts_string_images_only() {
        let mut decide = input(&["有错吗"]);
        decide.images = Some(vec![json!("data:image/png;base64,AAAA")]);
        let request = build_request(&decide).expect("字符串图片应当构造成功");
        assert_eq!(
            request.images,
            vec!["data:image/png;base64,AAAA".to_owned()]
        );

        decide.images = Some(vec![json!({"path": "/tmp/a.png"})]);
        let violation = build_request(&decide).expect_err("对象形态图片应当被拒");
        assert_eq!(violation.code, "CAPABILITY_MISMATCH");
        assert!(!violation.next_steps.is_empty());
    }

    /// 响应解析：正常路径 noul → probability。
    #[test]
    fn response_parses_noul_into_probability() {
        let response: SystemOneResponse = serde_json::from_value(json!({
            "answers": {
                "q0": {"type": "noul", "noul": 0.6849},
                "q1": {"type": "noul", "noul": 0.0373},
            },
            "usage": {"tokens": 12},
            "model": "startlux-decision",
        }))
        .expect("应当能解析");
        let output = parse_decide_output(&response, &["有错吗".to_owned(), "要摘要吗".to_owned()])
            .expect("应当解析成功");
        assert_eq!(output.answers["有错吗"].probability, 0.6849);
        assert_eq!(output.answers["要摘要吗"].probability, 0.0373);
        assert!(output.answers["有错吗"].span.is_none());
    }

    /// 响应解析：缺答案、类型不对、缺 noul、概率越界都是形状不符。
    #[test]
    fn response_shape_violations_are_rejected() {
        let missing = SystemOneResponse {
            answers: BTreeMap::new(),
            usage: None,
            model: None,
        };
        let violation =
            parse_decide_output(&missing, &["有错吗".to_owned()]).expect_err("缺答案应被拒");
        assert_eq!(violation.code, "CAPABILITY_MISMATCH");
        assert!(violation.fact.contains("q0"));
        assert!(!violation.next_steps.is_empty());

        let wrong_type: SystemOneResponse = serde_json::from_value(json!({
            "answers": {"q0": {"type": "choice", "noul": 0.5}},
        }))
        .expect("应当能解析");
        let violation =
            parse_decide_output(&wrong_type, &["有错吗".to_owned()]).expect_err("非 noul 应被拒");
        assert_eq!(violation.code, "CAPABILITY_MISMATCH");

        let missing_noul: SystemOneResponse = serde_json::from_value(json!({
            "answers": {"q0": {"type": "noul"}},
        }))
        .expect("应当能解析");
        parse_decide_output(&missing_noul, &["有错吗".to_owned()]).expect_err("缺 noul 应被拒");

        let out_of_range: SystemOneResponse = serde_json::from_value(json!({
            "answers": {"q0": {"type": "noul", "noul": 1.4}},
        }))
        .expect("应当能解析");
        let violation =
            parse_decide_output(&out_of_range, &["有错吗".to_owned()]).expect_err("越界应被拒");
        assert!(violation.fact.contains("[0,1]"));

        let key_mismatch: SystemOneResponse = serde_json::from_value(json!({
            "answers": {"q1": {"type": "noul", "noul": 0.5}},
        }))
        .expect("应当能解析");
        parse_decide_output(&key_mismatch, &["有错吗".to_owned()]).expect_err("键不符应被拒");
    }

    /// 端点拼接：带 /v1、不带 /v1、带尾部斜杠。
    #[test]
    fn endpoint_appends_systemone_path() {
        assert_eq!(
            endpoint("http://127.0.0.1:8090"),
            "http://127.0.0.1:8090/v1/systemone"
        );
        assert_eq!(
            endpoint("http://127.0.0.1:8090/"),
            "http://127.0.0.1:8090/v1/systemone"
        );
        assert_eq!(
            endpoint("http://127.0.0.1:8090/v1"),
            "http://127.0.0.1:8090/v1/systemone"
        );
        assert_eq!(
            endpoint("http://127.0.0.1:8090/v1/"),
            "http://127.0.0.1:8090/v1/systemone"
        );
    }
}
