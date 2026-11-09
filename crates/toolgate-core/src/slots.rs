//! 槽位契约层（§3.2 / §3.8 / §3.11）与置信度门控（§7.2）。
//!
//! 本模块只定义五个能力槽位的输入输出形状、形状校验，以及「槽位全空仍能跑」
//! 的可用性与降级决策。它不含模型后端、不做配置解析、不发起调用：
//! 模型后端属可选层（§19.6），配置解析与工具接入属后续阶段。
//!
//! 架构依据：03-slots.md §3.1/§3.2/§3.8/§3.10/§3.11、06-cost.md §7.2/§7.3。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------- 槽位词汇与可用性 ----------

/// 五个能力槽位（§3.1）。架构只定义槽位契约，不绑定具体模型。
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum SlotKind {
    Classify,
    Rerank,
    Decide,
    Summarize,
    Vision,
}

impl SlotKind {
    pub const ALL: [SlotKind; 5] = [
        SlotKind::Classify,
        SlotKind::Rerank,
        SlotKind::Decide,
        SlotKind::Summarize,
        SlotKind::Vision,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            SlotKind::Classify => "classify",
            SlotKind::Rerank => "rerank",
            SlotKind::Decide => "decide",
            SlotKind::Summarize => "summarize",
            SlotKind::Vision => "vision",
        }
    }
}

/// 槽位可用性（§3.8）。Default 即「一个都不配」——全空仍是可用产品。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SlotSet {
    pub classify: bool,
    pub rerank: bool,
    pub decide: bool,
    pub summarize: bool,
    pub vision: bool,
}

impl SlotSet {
    pub fn none() -> Self {
        Self::default()
    }

    pub fn all() -> Self {
        Self {
            classify: true,
            rerank: true,
            decide: true,
            summarize: true,
            vision: true,
        }
    }

    /// 按 §3.10 的 serves 声明推导：声明里出现即视为该槽位已绑定。
    pub fn from_serves(serves: &[SlotKind]) -> Self {
        let mut set = Self::none();
        for kind in serves {
            match kind {
                SlotKind::Classify => set.classify = true,
                SlotKind::Rerank => set.rerank = true,
                SlotKind::Decide => set.decide = true,
                SlotKind::Summarize => set.summarize = true,
                SlotKind::Vision => set.vision = true,
            }
        }
        set
    }

    pub fn is_available(&self, kind: SlotKind) -> bool {
        match kind {
            SlotKind::Classify => self.classify,
            SlotKind::Rerank => self.rerank,
            SlotKind::Decide => self.decide,
            SlotKind::Summarize => self.summarize,
            SlotKind::Vision => self.vision,
        }
    }

    pub fn all_empty(&self) -> bool {
        !self.classify && !self.rerank && !self.decide && !self.summarize && !self.vision
    }

    /// §7.3：降级要标注哪些槽位不可用。
    pub fn empty_slots(&self) -> Vec<SlotKind> {
        SlotKind::ALL
            .into_iter()
            .filter(|kind| !self.is_available(*kind))
            .collect()
    }
}

// ---------- 形状校验失败（§3.11 第一层） ----------

/// 形状校验失败。与 §3.11 的 CAPABILITY_MISMATCH 同源：fact 说看到了什么，
/// next_steps 给可行动下一步（§1.4.4）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractViolation {
    pub code: &'static str,
    pub fact: String,
    pub next_steps: Vec<String>,
}

const CAPABILITY_STEPS: &[&str] = &[
    "换用 native 适配器或原生支持该槽位契约的模型",
    "把 adapter 改为 json / text / ordered 后重试",
    "从该模型的 serves 里移除此槽位，让它按 §3.8 降级",
];

impl ContractViolation {
    fn new(fact: impl Into<String>) -> Self {
        Self {
            code: "CAPABILITY_MISMATCH",
            fact: fact.into(),
            next_steps: CAPABILITY_STEPS.iter().map(|s| (*s).to_owned()).collect(),
        }
    }
}

fn finite_unit(p: f64) -> bool {
    p.is_finite() && (0.0..=1.0).contains(&p)
}

// ---------- classify（§3.2） ----------

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ClassifyInput {
    pub text: String,
    pub labels: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ClassifyOutput {
    pub scores: BTreeMap<String, f64>,
}

impl ClassifyOutput {
    /// §3.2：labels 长度 2-50；scores 的 key 集合必须等于 labels；分数在 [0,1]。
    pub fn validate(&self, input: &ClassifyInput) -> Result<(), ContractViolation> {
        if !(2..=50).contains(&input.labels.len()) {
            return Err(ContractViolation::new(format!(
                "labels 长度 {} 不在 2-50 内",
                input.labels.len()
            )));
        }
        let mut expected: Vec<&str> = input.labels.iter().map(String::as_str).collect();
        let mut actual: Vec<&str> = self.scores.keys().map(String::as_str).collect();
        expected.sort_unstable();
        actual.sort_unstable();
        expected.dedup();
        actual.dedup();
        if expected != actual {
            return Err(ContractViolation::new("scores 的 key 集合与 labels 不一致"));
        }
        for (label, score) in &self.scores {
            if !finite_unit(*score) {
                return Err(ContractViolation::new(format!(
                    "标签 {label} 的分数 {score} 不在 [0,1]"
                )));
            }
        }
        Ok(())
    }
}

// ---------- rerank（§3.2） ----------

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RerankInput {
    pub query: String,
    pub documents: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_n: Option<usize>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RerankResult {
    pub index: usize,
    pub score: f64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RerankOutput {
    pub results: Vec<RerankResult>,
}

impl RerankOutput {
    /// §3.2：results 覆盖全部输入文档、index 指向原始位置、按 score 降序。
    pub fn validate(&self, input: &RerankInput) -> Result<(), ContractViolation> {
        let n = input.documents.len();
        if self.results.len() != n {
            return Err(ContractViolation::new(format!(
                "results 长度 {} 与 documents 长度 {n} 不一致",
                self.results.len()
            )));
        }
        let mut seen = vec![false; n];
        let mut prev = f64::INFINITY;
        for r in &self.results {
            if !r.score.is_finite() {
                return Err(ContractViolation::new("score 不是有限数"));
            }
            if r.score > prev {
                return Err(ContractViolation::new("results 未按 score 降序排列"));
            }
            if r.index >= n || seen[r.index] {
                return Err(ContractViolation::new(format!(
                    "index {} 越界或重复",
                    r.index
                )));
            }
            seen[r.index] = true;
            prev = r.score;
        }
        Ok(())
    }
}

// ---------- decide（§3.2） ----------

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DecideInput {
    /// state 可以是字符串或对象（§3.2）；Image 的形状由调用方给，不预设。
    pub state: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<Value>>,
    pub questions: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DecideAnswer {
    pub probability: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<[usize; 2]>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DecideOutput {
    pub answers: BTreeMap<String, DecideAnswer>,
}

impl DecideOutput {
    /// §3.2：每个问题一个概率答案；概率在 [0,1]。
    pub fn validate(&self, input: &DecideInput) -> Result<(), ContractViolation> {
        let mut expected: Vec<&str> = input.questions.iter().map(String::as_str).collect();
        let mut actual: Vec<&str> = self.answers.keys().map(String::as_str).collect();
        expected.sort_unstable();
        actual.sort_unstable();
        expected.dedup();
        actual.dedup();
        if expected != actual {
            return Err(ContractViolation::new("answers 未覆盖全部 questions"));
        }
        for (question, answer) in &self.answers {
            if !finite_unit(answer.probability) {
                return Err(ContractViolation::new(format!(
                    "问题 {question} 的 probability {} 不在 [0,1]",
                    answer.probability
                )));
            }
        }
        Ok(())
    }
}

// ---------- summarize（§3.2） ----------

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SummarizeInput {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SummarizeOutput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl SummarizeOutput {
    /// §3.2：无 schema 给 text，有 schema 给 data，二者互斥。
    /// 「输出按 schema 校验」需要 JSON schema 校验器，属可选层，见交付报告遗留。
    pub fn validate(&self, input: &SummarizeInput) -> Result<(), ContractViolation> {
        if input.schema.is_some() {
            if self.data.is_none() {
                return Err(ContractViolation::new("有 schema 时输出缺 data"));
            }
            if self.text.is_some() {
                return Err(ContractViolation::new("有 schema 时不应同时给 text"));
            }
        } else {
            if self.text.is_none() {
                return Err(ContractViolation::new("无 schema 时输出缺 text"));
            }
            if self.data.is_some() {
                return Err(ContractViolation::new("无 schema 时不应给 data"));
            }
        }
        Ok(())
    }
}

// ---------- vision（§3.2） ----------

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum VisionTask {
    Describe,
    Ocr,
    Ground,
    Classify,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VisionInput {
    /// Image 的形状由调用方给，不预设（同 DecideInput.images）。
    pub image: Value,
    pub task: VisionTask,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VisionBox {
    pub label: String,
    /// bbox 用原图像素坐标 [x, y, w, h]（§3.2）。
    pub bbox: [f64; 4],
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VisionOutput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boxes: Option<Vec<VisionBox>>,
}

impl VisionOutput {
    /// §3.2：task=ground 时 boxes 必填；bbox 用原图像素坐标（非负值）。
    pub fn validate(&self, input: &VisionInput) -> Result<(), ContractViolation> {
        if input.task == VisionTask::Ground && self.boxes.as_ref().is_none_or(|b| b.is_empty()) {
            return Err(ContractViolation::new(
                "task=ground 时 boxes 必填且不能为空",
            ));
        }
        if let Some(boxes) = &self.boxes {
            for b in boxes {
                if b.bbox.iter().any(|v| !v.is_finite() || *v < 0.0) {
                    return Err(ContractViolation::new(format!(
                        "标签 {} 的 bbox 含非法像素坐标",
                        b.label
                    )));
                }
            }
        }
        Ok(())
    }
}

// ---------- 降级决策（§3.8 / §7.3） ----------

/// 逐槽位缺失时的降级行为（§3.8 表）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DegradeAction {
    MechanicalSniff,
    KeepHitOrder,
    RawPath,
    TruncatedWithPointer,
    OriginalImageOrMetadata,
}

impl DegradeAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            DegradeAction::MechanicalSniff => "mechanical_sniff",
            DegradeAction::KeepHitOrder => "keep_hit_order",
            DegradeAction::RawPath => "raw_path",
            DegradeAction::TruncatedWithPointer => "truncated_with_pointer",
            DegradeAction::OriginalImageOrMetadata => "original_image_or_metadata",
        }
    }
}

/// 槽位为空时走哪条机械路径（§3.8）。match 穷举，新增槽位即编译失败。
pub fn degrade_action(slot: SlotKind) -> DegradeAction {
    match slot {
        SlotKind::Classify => DegradeAction::MechanicalSniff,
        SlotKind::Rerank => DegradeAction::KeepHitOrder,
        SlotKind::Decide => DegradeAction::RawPath,
        SlotKind::Summarize => DegradeAction::TruncatedWithPointer,
        SlotKind::Vision => DegradeAction::OriginalImageOrMetadata,
    }
}

// ---------- 置信度门控（§7.2） ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfidenceGate {
    /// p ≥ 0.85：采用本地判定。
    Adopt,
    /// 0.60 ≤ p < 0.85：采用，按 LOW_CONFIDENCE 标注。
    AdoptWithLowConfidence,
    /// p < 0.60：本地判定与原始内容同时给（both views）。
    BothViews,
}

/// §7.2 的三档门控。纯函数，同输入同输出（§16.3）。
pub fn confidence_gate(p: f64) -> ConfidenceGate {
    if p >= 0.85 {
        ConfidenceGate::Adopt
    } else if p >= 0.60 {
        ConfidenceGate::AdoptWithLowConfidence
    } else {
        ConfidenceGate::BothViews
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn classify_pair() -> (ClassifyInput, ClassifyOutput) {
        let input = ClassifyInput {
            text: "一段构建日志".to_owned(),
            labels: vec!["has_error".to_owned(), "is_binary".to_owned()],
        };
        let mut scores = BTreeMap::new();
        scores.insert("has_error".to_owned(), 0.93);
        scores.insert("is_binary".to_owned(), 0.04);
        (input, ClassifyOutput { scores })
    }

    fn rerank_pair() -> (RerankInput, RerankOutput) {
        let input = RerankInput {
            query: "timeout".to_owned(),
            documents: vec!["a".to_owned(), "b".to_owned(), "c".to_owned()],
            top_n: None,
        };
        let output = RerankOutput {
            results: vec![
                RerankResult {
                    index: 2,
                    score: 0.9,
                },
                RerankResult {
                    index: 0,
                    score: 0.5,
                },
                RerankResult {
                    index: 1,
                    score: 0.5,
                },
            ],
        };
        (input, output)
    }

    fn decide_pair() -> (DecideInput, DecideOutput) {
        let input = DecideInput {
            state: json!({ "lines": 8000 }),
            images: None,
            questions: vec!["有错吗".to_owned(), "要摘要吗".to_owned()],
        };
        let mut answers = BTreeMap::new();
        answers.insert(
            "有错吗".to_owned(),
            DecideAnswer {
                probability: 0.97,
                span: None,
            },
        );
        answers.insert(
            "要摘要吗".to_owned(),
            DecideAnswer {
                probability: 0.88,
                span: Some([3, 12]),
            },
        );
        (input, DecideOutput { answers })
    }

    fn vision_pair() -> (VisionInput, VisionOutput) {
        let input = VisionInput {
            image: json!({ "path": "/tmp/shot.png" }),
            task: VisionTask::Ground,
            query: None,
        };
        let output = VisionOutput {
            text: Some("按钮在左上".to_owned()),
            boxes: Some(vec![VisionBox {
                label: "按钮".to_owned(),
                bbox: [10.0, 20.0, 80.0, 30.0],
            }]),
        };
        (input, output)
    }

    /// §15.7 契约测试：五个槽位各一组固定样本，断言输出形态与类型。
    #[test]
    fn all_slot_contracts_roundtrip_and_validate() {
        let (ci, co) = classify_pair();
        assert_eq!(
            serde_json::to_value(&co).unwrap()["scores"]["has_error"],
            json!(0.93)
        );
        co.validate(&ci).unwrap();

        let (ri, ro) = rerank_pair();
        let rv = serde_json::to_value(&ro).unwrap();
        assert_eq!(rv["results"][0]["index"], json!(2));
        ro.validate(&ri).unwrap();

        let (di, dout) = decide_pair();
        let dv = serde_json::to_value(&dout).unwrap();
        assert_eq!(dv["answers"]["有错吗"]["probability"], json!(0.97));
        dout.validate(&di).unwrap();

        let si = SummarizeInput {
            text: "长日志".to_owned(),
            schema: None,
            max_tokens: Some(200),
        };
        let sout = SummarizeOutput {
            text: Some("摘要".to_owned()),
            data: None,
        };
        let sv = serde_json::to_value(&si).unwrap();
        assert_eq!(sv["maxTokens"], json!(200));
        sout.validate(&si).unwrap();

        let (vi, vo) = vision_pair();
        let vv = serde_json::to_value(&vo).unwrap();
        assert_eq!(vv["boxes"][0]["bbox"], json!([10.0, 20.0, 80.0, 30.0]));
        vo.validate(&vi).unwrap();
    }

    /// 每个槽位至少一条违规路径，且违规都可行动（§1.4.4 / §3.11）。
    #[test]
    fn every_contract_violation_is_actionable() {
        let (ci, mut co) = classify_pair();
        co.scores.insert("extra".to_owned(), 0.5);
        let mut violations = vec![co.validate(&ci).unwrap_err()];

        let (ri, mut ro) = rerank_pair();
        ro.results.reverse();
        violations.push(ro.validate(&ri).unwrap_err());

        let (di, mut dout) = decide_pair();
        dout.answers.get_mut("有错吗").unwrap().probability = 1.7;
        violations.push(dout.validate(&di).unwrap_err());

        let si = SummarizeInput {
            text: "x".to_owned(),
            schema: Some(json!({})),
            max_tokens: None,
        };
        violations.push(
            SummarizeOutput {
                text: Some("t".to_owned()),
                data: None,
            }
            .validate(&si)
            .unwrap_err(),
        );

        let (vi, vo) = vision_pair();
        violations.push(
            VisionOutput {
                text: vo.text.clone(),
                boxes: None,
            }
            .validate(&vi)
            .unwrap_err(),
        );

        for v in violations {
            assert_eq!(v.code, "CAPABILITY_MISMATCH");
            assert!(!v.fact.is_empty());
            assert!(
                !v.next_steps.is_empty(),
                "形状校验失败缺少可执行下一步：{v:?}"
            );
        }
    }

    #[test]
    fn classify_rejects_bad_label_range_and_score() {
        let input = ClassifyInput {
            text: "x".to_owned(),
            labels: vec!["only".to_owned()],
        };
        let output = ClassifyOutput {
            scores: BTreeMap::new(),
        };
        assert!(output.validate(&input).is_err());
        let (ci, mut co) = classify_pair();
        *co.scores.get_mut("has_error").unwrap() = f64::NAN;
        assert!(co.validate(&ci).is_err());
    }

    #[test]
    fn rerank_rejects_missing_or_duplicate_index() {
        let (ri, mut ro) = rerank_pair();
        ro.results[1].index = 2;
        assert!(ro.validate(&ri).is_err());
        let (ri2, mut ro2) = rerank_pair();
        ro2.results.pop();
        assert!(ro2.validate(&ri2).is_err());
    }

    #[test]
    fn summarize_requires_data_when_schema_present() {
        let si = SummarizeInput {
            text: "x".to_owned(),
            schema: Some(json!({"type": "object"})),
            max_tokens: None,
        };
        assert!(
            SummarizeOutput {
                text: None,
                data: Some(json!({"k": 1}))
            }
            .validate(&si)
            .is_ok()
        );
        assert!(
            SummarizeOutput {
                text: None,
                data: None
            }
            .validate(&si)
            .is_err()
        );
    }

    #[test]
    fn slot_kind_all_covered_and_serde_stable() {
        assert_eq!(SlotKind::ALL.len(), 5);
        for kind in SlotKind::ALL {
            match kind {
                SlotKind::Classify => {}
                SlotKind::Rerank => {}
                SlotKind::Decide => {}
                SlotKind::Summarize => {}
                SlotKind::Vision => {}
            }
            assert_eq!(serde_json::to_value(kind).unwrap(), json!(kind.as_str()));
        }
    }

    /// §3.8 核心保证：一个都不配也能跑。
    #[test]
    fn slot_set_default_is_all_empty() {
        let set = SlotSet::none();
        assert!(set.all_empty());
        assert_eq!(set.empty_slots().len(), 5);
        assert!(!SlotSet::all().all_empty());
        assert!(SlotSet::all().empty_slots().is_empty());
    }

    /// §3.10 示例：两个模型的 serves 覆盖四个槽位，rerank 为空。
    #[test]
    fn slot_set_from_serves_matches_declaration() {
        let set = SlotSet::from_serves(&[
            SlotKind::Classify,
            SlotKind::Decide,
            SlotKind::Summarize,
            SlotKind::Vision,
        ]);
        assert!(!set.all_empty());
        assert!(!set.is_available(SlotKind::Rerank));
        assert_eq!(set.empty_slots(), vec![SlotKind::Rerank]);
        assert!(set.is_available(SlotKind::Classify));
        assert!(set.is_available(SlotKind::Vision));
    }

    #[test]
    fn degrade_action_covers_all_slots() {
        for kind in SlotKind::ALL {
            let action = degrade_action(kind);
            assert!(!action.as_str().is_empty());
        }
        assert_eq!(
            degrade_action(SlotKind::Classify),
            DegradeAction::MechanicalSniff
        );
        assert_eq!(
            degrade_action(SlotKind::Rerank),
            DegradeAction::KeepHitOrder
        );
        assert_eq!(degrade_action(SlotKind::Decide), DegradeAction::RawPath);
        assert_eq!(
            degrade_action(SlotKind::Summarize),
            DegradeAction::TruncatedWithPointer
        );
        assert_eq!(
            degrade_action(SlotKind::Vision),
            DegradeAction::OriginalImageOrMetadata
        );
    }

    /// §7.2 三档边界。
    #[test]
    fn confidence_gate_boundaries() {
        assert_eq!(confidence_gate(1.0), ConfidenceGate::Adopt);
        assert_eq!(confidence_gate(0.85), ConfidenceGate::Adopt);
        assert_eq!(
            confidence_gate(0.849),
            ConfidenceGate::AdoptWithLowConfidence
        );
        assert_eq!(
            confidence_gate(0.60),
            ConfidenceGate::AdoptWithLowConfidence
        );
        assert_eq!(confidence_gate(0.599), ConfidenceGate::BothViews);
        assert_eq!(confidence_gate(0.0), ConfidenceGate::BothViews);
        assert_eq!(confidence_gate(f64::NAN), ConfidenceGate::BothViews);
    }
}
