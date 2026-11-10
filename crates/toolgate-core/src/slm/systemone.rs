//! SystemOne 协议适配（默认层纯逻辑）：请求构造、响应解析、端点拼接。
//!
//! d3-nano 与 StartLux 都是 jev 类 systemOne 模型，只能走 /v1/systemone：按 chat 协议
//! （/v1/chat/completions）调用会输出退化重复（ABAABA…、finish_reason=length），
//! 正确形态是 systemone 的命名问题 → 校准概率。
//!
//! 协议契约以官方 StartLux-Decision 的 docs/api.md 与 TypeSafe
//! docs/typesafe.ai/primitives/choice 为准：decide 用 noul，classify 用 choice
//! （criteria 是选项名→描述的 map，响应给 probabilities / choice / confidence）。
//!
//! 本模块不发起网络请求，只做形状与语义转换；HTTP 发送在 client.rs 的
//! #[cfg(feature = "external-protocols")] 分支（§19.6 可选层）。
//!
//! 架构依据：18-deployment.md §18.1（模型来源与协议形态）、03-slots.md §3.2（槽位契约）、
//! §3.10（systemone 适配器映射）、§3.11（形状不符即 CAPABILITY_MISMATCH）。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::slots::{
    ClassifyInput, ClassifyOutput, ContractViolation, DecideAnswer, DecideInput, DecideOutput,
};

/// 形状不符的错误码（§3.11）。
const CAPABILITY_MISMATCH: &str = "CAPABILITY_MISMATCH";

/// decide 用 noul（P(yes) 原语）。
pub const NOUL_TYPE: &str = "noul";
/// classify 用 choice（逐选项概率）。
pub const CHOICE_TYPE: &str = "choice";
/// classify 问题的固定 id。
pub const CLASSIFY_QUESTION_KEY: &str = "q0";
/// classify 问题的固定措辞（确定性，不随输入变）。
pub const CLASSIFY_INSTRUCTIONS: &str = "从给定标签中为文本选出最合适的标签，并给出每个标签的概率。";
/// none_probability 的降级阈值（官方建议 0.2，附件 E.10）。
pub const NONE_PROBABILITY_THRESHOLD: f64 = 0.2;

/// 请求里的单个问题。
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SystemOneQuestion {
    /// 问题类型：decide 用 noul，classify 用 choice。
    #[serde(rename = "type")]
    pub kind: String,
    /// 问题原文。
    pub instructions: String,
    /// choice 的选项 map（键为选项名、值为描述）；noul 不带该字段。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub criteria: Option<BTreeMap<String, Value>>,
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
    /// 答案类型；decide 只接受 noul，classify 只接受 choice。
    #[serde(rename = "type")]
    pub kind: String,
    /// noul 的 P(yes)。
    #[serde(default)]
    pub noul: Option<f64>,
    /// choice 的逐选项概率分布。
    #[serde(default)]
    pub probabilities: Option<BTreeMap<String, f64>>,
    /// choice 的「无合适选项」告警分（官方建议阈值 0.2）。
    #[serde(default)]
    pub none_probability: Option<f64>,
    /// choice 选中的项与分布集中度；两者都不参与槽位映射。
    #[serde(default)]
    pub choice: Option<String>,
    #[serde(default)]
    pub confidence: Option<f64>,
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
                criteria: None,
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

/// classify 槽位 → systemone 的 choice 请求（§3.10）。
///
/// `criteria` 用 `{<标签>: <标签>}` 的 map：官方要求键为选项名、值为描述，槽位只有
/// 标签名，用标签名本身，不注入额外语义。`state` 写待分类文本。
pub fn build_classify_request(input: &ClassifyInput) -> Result<SystemOneRequest, ContractViolation> {
    if input.text.is_empty() {
        return Err(classify_violation("classify 的 text 为空"));
    }
    if !(2..=50).contains(&input.labels.len()) {
        return Err(classify_violation(format!(
            "labels 长度 {} 不在 2-50 内",
            input.labels.len()
        )));
    }
    let mut criteria = BTreeMap::new();
    for label in &input.labels {
        if criteria
            .insert(label.clone(), Value::String(label.clone()))
            .is_some()
        {
            return Err(classify_violation(format!("labels 有重复项：{label}")));
        }
    }
    let mut questions = BTreeMap::new();
    questions.insert(
        CLASSIFY_QUESTION_KEY.to_owned(),
        SystemOneQuestion {
            kind: CHOICE_TYPE.to_owned(),
            instructions: CLASSIFY_INSTRUCTIONS.to_owned(),
            criteria: Some(criteria),
        },
    );
    Ok(SystemOneRequest {
        state: Value::String(input.text.clone()),
        questions,
        images: Vec::new(),
    })
}

/// systemone 的 choice 响应 → classify 槽位形状（§3.10）。
///
/// 只取 `probabilities` 按标签名映射成 `scores`；`choice` / `confidence` 不参与
/// （把 `confidence` 当 `probability` 属静默转换，§3.10 禁止）。`none_probability`
/// ≥ 0.2 表示「无合适选项」，本次按 §3.11 形状不符降级。
pub fn parse_classify_output(
    response: &SystemOneResponse,
    input: &ClassifyInput,
) -> Result<ClassifyOutput, ContractViolation> {
    let answer = response
        .answers
        .get(CLASSIFY_QUESTION_KEY)
        .ok_or_else(|| classify_violation(format!("响应缺少问题 {CLASSIFY_QUESTION_KEY} 的答案")))?;
    if answer.kind != CHOICE_TYPE {
        return Err(classify_violation(format!(
            "问题 {CLASSIFY_QUESTION_KEY} 的答案类型是 {}，只接受 {CHOICE_TYPE}",
            answer.kind
        )));
    }
    if let Some(none) = answer.none_probability
        && (!none.is_finite() || none >= NONE_PROBABILITY_THRESHOLD)
    {
        return Err(classify_violation(format!(
            "none_probability={none} 达到阈值 {NONE_PROBABILITY_THRESHOLD}，模型自认无合适选项"
        )));
    }
    let probabilities = answer
        .probabilities
        .as_ref()
        .ok_or_else(|| classify_violation("choice 答案缺 probabilities 字段"))?;
    let output = ClassifyOutput {
        scores: probabilities.clone(),
    };
    // 复用 §3.2 契约校验：key 集合等于 labels、每个分数在 [0,1]。
    output.validate(input)?;
    Ok(output)
}

fn classify_violation(fact: impl Into<String>) -> ContractViolation {
    ContractViolation {
        code: CAPABILITY_MISMATCH,
        fact: fact.into(),
        next_steps: classify_next_steps(),
    }
}

/// classify 形状不符时的可执行下一步（§1.4.5）。
fn classify_next_steps() -> Vec<String> {
    vec![
        "确认上游服务支持 systemone 的 choice 原语且返回 probabilities".to_owned(),
        "检查 criteria 的选项名与 classify 的 labels 是否一致".to_owned(),
        "从该模型的 serves 里移除 classify 槽位，让它按 §3.8 降级".to_owned(),
    ]
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

    fn classify_input() -> ClassifyInput {
        ClassifyInput {
            text: "一段构建日志".to_owned(),
            labels: vec!["has_error".to_owned(), "is_binary".to_owned()],
        }
    }

    fn choice_response(extra: Value) -> SystemOneResponse {
        let mut answer = json!({
            "type": "choice",
            "choice": "has_error",
            "confidence": 0.7,
            "probabilities": {"has_error": 0.9, "is_binary": 0.1},
        });
        if let Some(obj) = extra.as_object() {
            for (k, v) in obj {
                answer[k.as_str()] = v.clone();
            }
        }
        serde_json::from_value(json!({"answers": {"q0": answer}})).expect("应当能解析")
    }

    /// 请求构造：type=choice，criteria 是标签名 map，state 是待分类文本。
    #[test]
    fn classify_request_uses_choice_criteria_map() {
        let request = build_classify_request(&classify_input()).expect("应当构造成功");
        assert_eq!(request.questions.len(), 1);
        let body = serde_json::to_value(&request).expect("应当能序列化");
        assert_eq!(body["questions"]["q0"]["type"], json!("choice"));
        assert_eq!(
            body["questions"]["q0"]["criteria"],
            json!({"has_error": "has_error", "is_binary": "is_binary"})
        );
        assert_eq!(body["state"], json!("一段构建日志"));
        assert!(body.get("images").is_none());
    }

    /// 请求构造：空 text、labels 越界或重复都按形状不符拒。
    #[test]
    fn classify_request_rejects_bad_input() {
        let mut input = classify_input();
        input.text = String::new();
        assert_eq!(
            build_classify_request(&input).expect_err("空 text 应被拒").code,
            "CAPABILITY_MISMATCH"
        );
        input = classify_input();
        input.labels = vec!["only".to_owned()];
        assert!(build_classify_request(&input).is_err());
        input = classify_input();
        input.labels = vec!["dup".to_owned(), "dup".to_owned()];
        assert!(build_classify_request(&input).is_err());
    }

    /// 响应解析：probabilities → scores，confidence / choice 不参与。
    #[test]
    fn classify_response_maps_probabilities_to_scores() {
        let response = choice_response(json!({}));
        let output = parse_classify_output(&response, &classify_input()).expect("应当解析成功");
        assert_eq!(output.scores["has_error"], 0.9);
        assert_eq!(output.scores["is_binary"], 0.1);
        output
            .validate(&classify_input())
            .expect("应符合 §3.2 契约");
    }

    /// 响应解析：类型不符、缺 probabilities、键不符、概率越界都按形状不符拒。
    #[test]
    fn classify_response_shape_violations_are_rejected() {
        let wrong_type: SystemOneResponse = serde_json::from_value(json!({
            "answers": {"q0": {"type": "noul", "noul": 0.5}}
        }))
        .expect("应当能解析");
        assert_eq!(
            parse_classify_output(&wrong_type, &classify_input())
                .expect_err("非 choice 应被拒")
                .code,
            "CAPABILITY_MISMATCH"
        );

        let missing_probs: SystemOneResponse = serde_json::from_value(json!({
            "answers": {"q0": {"type": "choice"}}
        }))
        .expect("应当能解析");
        assert!(parse_classify_output(&missing_probs, &classify_input()).is_err());

        let missing_label = choice_response(json!({"probabilities": {"has_error": 0.9}}));
        assert!(parse_classify_output(&missing_label, &classify_input()).is_err());

        let out_of_range =
            choice_response(json!({"probabilities": {"has_error": 1.4, "is_binary": 0.1}}));
        assert!(parse_classify_output(&out_of_range, &classify_input()).is_err());

        let missing_answer = SystemOneResponse {
            answers: BTreeMap::new(),
            usage: None,
            model: None,
        };
        assert!(parse_classify_output(&missing_answer, &classify_input()).is_err());
    }

    /// none_probability 达到 0.2 阈值 → 降级，不返回分数；低于阈值照常映射。
    #[test]
    fn classify_response_none_probability_gate() {
        let high = choice_response(json!({"none_probability": 0.505}));
        let violation =
            parse_classify_output(&high, &classify_input()).expect_err("应触发降级");
        assert!(violation.fact.contains("none_probability"), "{}", violation.fact);
        assert!(!violation.next_steps.is_empty());

        let low = choice_response(json!({"none_probability": 0.19}));
        assert!(parse_classify_output(&low, &classify_input()).is_ok());
    }
}
