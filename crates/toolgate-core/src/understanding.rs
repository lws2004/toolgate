//! 理解链（§4.8）与判定封装（§13.4 方案 A）。
//!
//! 纯逻辑层：固定动作菜单、有界 DAG 校验与确定性分层调度、
//! 命名问题到 d1 输入形状的映射。不发起模型调用、不写文件、
//! 不跨请求保存状态（§22.5）；方案 B 的跨请求聚合留给阶段 1.5/4。

use serde_json::Value;

use crate::slots::{DecideInput, SlotKind};

/// §4.8 七个固定动作。链不能生成菜单外的新动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainAction {
    Recall,
    Decide,
    Summarize,
    Vision,
    Rerank,
    SelfCheck,
    Merge,
}

impl ChainAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            ChainAction::Recall => "recall",
            ChainAction::Decide => "decide",
            ChainAction::Summarize => "summarize",
            ChainAction::Vision => "vision",
            ChainAction::Rerank => "rerank",
            ChainAction::SelfCheck => "self_check",
            ChainAction::Merge => "merge",
        }
    }

    /// 动作归属的模型槽位；recall / merge 是确定性动作，不占槽位（§4.8）。
    pub fn slot(&self) -> Option<SlotKind> {
        match self {
            ChainAction::Recall | ChainAction::Merge => None,
            ChainAction::Decide | ChainAction::SelfCheck => Some(SlotKind::Decide),
            ChainAction::Summarize => Some(SlotKind::Summarize),
            ChainAction::Vision => Some(SlotKind::Vision),
            ChainAction::Rerank => Some(SlotKind::Rerank),
        }
    }
}

/// 链节点：只有数据依赖才连边（§4.8 DAG）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainNode {
    pub id: usize,
    pub action: ChainAction,
    pub depends_on: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainViolation {
    pub code: &'static str,
    pub fact: String,
    pub next_steps: Vec<String>,
}

impl ChainViolation {
    fn new(fact: impl Into<String>) -> Self {
        Self {
            code: "CHAIN_INVALID",
            fact: fact.into(),
            next_steps: vec![
                "把链步数压到 5 步以内，或去掉无依赖的重复节点".to_owned(),
                "检查 depends_on 是否指向已存在的节点且无环".to_owned(),
                "降低链级并发上限后重试".to_owned(),
            ],
        }
    }
}

/// §4.8 有界预算：最多 5 步、并发 ≤ 4（链级上限可配，默认 min(4, CPU/2)）。
pub const MAX_STEPS: usize = 5;
pub const MAX_PARALLEL: usize = 4;

/// 校验有界预算与 DAG 合法性。纯函数，同输入同输出。
pub fn validate_chain(nodes: &[ChainNode], max_parallel: usize) -> Result<(), ChainViolation> {
    if nodes.len() > MAX_STEPS {
        return Err(ChainViolation::new(format!(
            "链有 {} 步，超过上限 {MAX_STEPS}",
            nodes.len()
        )));
    }
    let mut seen = std::collections::BTreeSet::new();
    for n in nodes {
        if !seen.insert(n.id) {
            return Err(ChainViolation::new(format!("节点 id {} 重复", n.id)));
        }
        for dep in &n.depends_on {
            if *dep == n.id {
                return Err(ChainViolation::new(format!("节点 {} 依赖自身", n.id)));
            }
            if !seen.contains(dep) {
                return Err(ChainViolation::new(format!(
                    "节点 {} 的依赖 {dep} 不存在或尚未定义",
                    n.id
                )));
            }
        }
    }
    // 环检测：拓扑分层能否吃掉全部节点。
    let layers = schedule(nodes, max_parallel);
    let scheduled: usize = layers.iter().map(Vec::len).sum();
    if scheduled != nodes.len() {
        return Err(ChainViolation::new("依赖图存在环，无法完成拓扑分层"));
    }
    Ok(())
}

/// 确定性分层调度：每层是可并发的节点，按 id 排序；超过 max_parallel 的节点
/// 拆到下一层排队，不无界展开（§4.8 并发模型）。同输入同输出（§16.3）。
pub fn schedule(nodes: &[ChainNode], max_parallel: usize) -> Vec<Vec<usize>> {
    let limit = max_parallel.max(1);
    let mut done: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
    let mut layers: Vec<Vec<usize>> = Vec::new();
    let mut remaining: Vec<&ChainNode> = nodes.iter().collect();
    while !remaining.is_empty() {
        let mut ready: Vec<usize> = remaining
            .iter()
            .filter(|n| n.depends_on.iter().all(|d| done.contains(d)))
            .map(|n| n.id)
            .collect();
        ready.sort_unstable();
        if ready.is_empty() {
            break; // 存在环，交给 validate_chain 报错
        }
        for chunk in ready.chunks(limit) {
            let layer: Vec<usize> = chunk.to_vec();
            for id in &layer {
                done.insert(*id);
            }
            layers.push(layer);
        }
        remaining.retain(|n| !done.contains(&n.id));
    }
    layers
}

/// §13.4 方案 A：把「state + 一组命名问题」映射成 d1 的输入形状（§3.2 decide 契约）。
/// 每次工具调用单独调一次 d1；不传 turn 字段时永远走这条路径。
pub fn decide_request(
    state: Value,
    questions: Vec<String>,
    images: Option<Vec<Value>>,
) -> DecideInput {
    DecideInput {
        state,
        images,
        questions,
    }
}

/// 方案 B 的 turn 载体（§13.4）：本轮只定义形状，跨请求聚合判定留给阶段 1.5/4。
/// 状态只在 turn 内存在、可随时丢弃，不得跨 turn 复用（§21.2、§22.5）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnContext {
    pub turn_id: String,
    pub seq: u32,
    pub budget: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn simple_chain() -> Vec<ChainNode> {
        vec![
            ChainNode {
                id: 0,
                action: ChainAction::Recall,
                depends_on: vec![],
            },
            ChainNode {
                id: 1,
                action: ChainAction::Recall,
                depends_on: vec![],
            },
            ChainNode {
                id: 2,
                action: ChainAction::Decide,
                depends_on: vec![0, 1],
            },
            ChainNode {
                id: 3,
                action: ChainAction::Summarize,
                depends_on: vec![2],
            },
            ChainNode {
                id: 4,
                action: ChainAction::Merge,
                depends_on: vec![3],
            },
        ]
    }

    #[test]
    fn action_menu_has_seven_fixed_actions() {
        let all = [
            ChainAction::Recall,
            ChainAction::Decide,
            ChainAction::Summarize,
            ChainAction::Vision,
            ChainAction::Rerank,
            ChainAction::SelfCheck,
            ChainAction::Merge,
        ];
        for a in all {
            match a {
                ChainAction::Recall => {}
                ChainAction::Decide => {}
                ChainAction::Summarize => {}
                ChainAction::Vision => {}
                ChainAction::Rerank => {}
                ChainAction::SelfCheck => {}
                ChainAction::Merge => {}
            }
            assert!(!a.as_str().is_empty());
        }
        assert_eq!(all.len(), 7);
    }

    #[test]
    fn deterministic_actions_have_no_slot() {
        assert_eq!(ChainAction::Recall.slot(), None);
        assert_eq!(ChainAction::Merge.slot(), None);
        assert_eq!(ChainAction::Vision.slot(), Some(SlotKind::Vision));
        assert_eq!(ChainAction::SelfCheck.slot(), Some(SlotKind::Decide));
    }

    #[test]
    fn valid_chain_passes_and_schedules_deterministically() {
        let nodes = simple_chain();
        validate_chain(&nodes, MAX_PARALLEL).unwrap();
        let a = schedule(&nodes, MAX_PARALLEL);
        let b = schedule(&nodes, MAX_PARALLEL);
        assert_eq!(a, b);
        assert_eq!(a[0], vec![0, 1]);
        assert_eq!(a[1], vec![2]);
        assert_eq!(a[2], vec![3]);
        assert_eq!(a[3], vec![4]);
        assert_eq!(a.iter().map(Vec::len).sum::<usize>(), nodes.len());
    }

    #[test]
    fn parallel_limit_splits_layers() {
        let nodes: Vec<ChainNode> = (0..4)
            .map(|id| ChainNode {
                id,
                action: ChainAction::Recall,
                depends_on: vec![],
            })
            .collect();
        let layers = schedule(&nodes, 2);
        assert_eq!(layers, vec![vec![0, 1], vec![2, 3]]);
        validate_chain(&nodes, 2).unwrap();
    }

    #[test]
    fn chain_budget_rejects_more_than_five_steps() {
        let nodes: Vec<ChainNode> = (0..6)
            .map(|id| ChainNode {
                id,
                action: ChainAction::Recall,
                depends_on: vec![],
            })
            .collect();
        let err = validate_chain(&nodes, MAX_PARALLEL).unwrap_err();
        assert_eq!(err.code, "CHAIN_INVALID");
        assert!(!err.next_steps.is_empty());
    }

    #[test]
    fn chain_rejects_cycles_and_unknown_dependencies() {
        let cycle = vec![
            ChainNode {
                id: 0,
                action: ChainAction::Decide,
                depends_on: vec![1],
            },
            ChainNode {
                id: 1,
                action: ChainAction::Recall,
                depends_on: vec![0],
            },
        ];
        assert!(validate_chain(&cycle, MAX_PARALLEL).is_err());
        let unknown = vec![ChainNode {
            id: 0,
            action: ChainAction::Recall,
            depends_on: vec![9],
        }];
        assert!(validate_chain(&unknown, MAX_PARALLEL).is_err());
        let self_dep = vec![ChainNode {
            id: 0,
            action: ChainAction::Recall,
            depends_on: vec![0],
        }];
        assert!(validate_chain(&self_dep, MAX_PARALLEL).is_err());
    }

    #[test]
    fn chain_rejects_duplicate_ids() {
        let nodes = vec![
            ChainNode {
                id: 0,
                action: ChainAction::Recall,
                depends_on: vec![],
            },
            ChainNode {
                id: 0,
                action: ChainAction::Merge,
                depends_on: vec![],
            },
        ];
        assert!(validate_chain(&nodes, MAX_PARALLEL).is_err());
    }

    #[test]
    fn decide_request_maps_named_questions_to_d1_shape() {
        let req = decide_request(
            json!({ "lines": 8000 }),
            vec!["有错吗".to_owned(), "要摘要吗".to_owned()],
            None,
        );
        assert_eq!(req.questions.len(), 2);
        assert_eq!(req.state["lines"], json!(8000));
        assert!(req.images.is_none());
    }

    #[test]
    fn turn_context_shape_is_defined() {
        let ctx = TurnContext {
            turn_id: "t-1".to_owned(),
            seq: 2,
            budget: 8,
        };
        assert_eq!(ctx.turn_id, "t-1");
        assert_eq!(ctx.seq, 2);
        assert_eq!(ctx.budget, 8);
    }
}
