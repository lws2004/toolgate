//! search 的智能路径（§5.2 / §4.8 / §3.8 / §7.3）。
//!
//! 纯逻辑：命中数阈值、intent 协商、理解链 DAG、rerank 槽位全空时的降级告知。
//! 不含模型后端（可选层，§19.6）；rerank 缺失时按 §3.8 保持命中顺序。

use std::time::Duration;

use serde_json::{Value, json};

use crate::compat::{CompatConfig, resolve_intent};
use crate::decision::Intent;
use crate::notice::{Notice, SlotFallback};
use crate::understanding::{ChainAction, ChainNode};

/// §5.2：20 条以内的命中，rerank 的延迟反而超过收益。
pub const RERANK_THRESHOLD: usize = 20;

/// rerank 状态，写进结构化事实便于程序判断（不进模型可见文本）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RerankState {
    /// 命中数未过阈值，无需 rerank。
    Skipped,
    /// 需要 rerank 但槽位不可用，按命中顺序降级返回。
    Unavailable,
    /// 槽位可用，按 top-k 顺序返回。
    Applied,
}

impl RerankState {
    pub fn as_str(&self) -> &'static str {
        match self {
            RerankState::Skipped => "skipped",
            RerankState::Unavailable => "unavailable",
            RerankState::Applied => "applied",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchShape {
    /// 全部命中（v1 行为 / raw）。
    All,
    /// rerank 后的 top-k 顺序。
    TopK,
    /// 结构化候选列表。
    Structured,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchPlan {
    pub shape: SearchShape,
    pub rerank: RerankState,
    pub notices: Vec<Notice>,
}

pub fn needs_rerank(hits: usize) -> bool {
    hits > RERANK_THRESHOLD
}

pub fn resolve_search_intent(args: &Value) -> Result<Intent, String> {
    resolve_intent(args, &CompatConfig::default())
}

fn slot_unavailable() -> Notice {
    Notice::SlotUnavailable {
        slot: "rerank".to_owned(),
        fallback: SlotFallback::Mechanical,
    }
}

/// §5.2 的协商层。rerank_slot_available 本阶段恒 false，但可用分支先落地。
pub fn plan_search(
    intent: Intent,
    hits: usize,
    rerank_slot_available: bool,
) -> Result<SearchPlan, String> {
    let plan = match intent {
        Intent::Raw => SearchPlan {
            shape: SearchShape::All,
            rerank: RerankState::Skipped,
            notices: vec![],
        },
        Intent::Visual => {
            return Err(
                "search 不返回图片，intent=visual 不适用；要看图请用 read 工具，或改用 intent=raw"
                    .to_owned(),
            );
        }
        Intent::Structured => SearchPlan {
            shape: SearchShape::Structured,
            rerank: RerankState::Skipped,
            notices: vec![],
        },
        Intent::Auto | Intent::Summary if !needs_rerank(hits) => SearchPlan {
            shape: SearchShape::All,
            rerank: RerankState::Skipped,
            notices: vec![],
        },
        Intent::Auto | Intent::Summary => {
            if rerank_slot_available {
                SearchPlan {
                    shape: SearchShape::TopK,
                    rerank: RerankState::Applied,
                    notices: vec![],
                }
            } else {
                // §3.8：按命中顺序返回；§7.3：降级必须显式告知。
                SearchPlan {
                    shape: SearchShape::All,
                    rerank: RerankState::Unavailable,
                    notices: vec![slot_unavailable()],
                }
            }
        }
    };
    Ok(plan)
}

/// §5.2 的理解链 DAG：召回全部命中 → rerank 打分 → 自检覆盖 → merge。
/// 4 步、单链，确定性调度；并发行为已在 bash / read 图片链覆盖。
pub fn search_chain() -> Vec<ChainNode> {
    vec![
        ChainNode {
            id: 0,
            action: ChainAction::Recall,
            depends_on: vec![],
        },
        ChainNode {
            id: 1,
            action: ChainAction::Rerank,
            depends_on: vec![0],
        },
        ChainNode {
            id: 2,
            action: ChainAction::SelfCheck,
            depends_on: vec![1],
        },
        ChainNode {
            id: 3,
            action: ChainAction::Merge,
            depends_on: vec![2],
        },
    ]
}

/// §5.2：按 rerank 结果重排命中行。order 已按 score 降序、index 指向原始位置；
/// 越界 index 忽略（形状由 RerankOutput::validate 校验，这里只做兜底）。
pub fn apply_rerank(raw: &str, order: &[usize]) -> String {
    let lines: Vec<&str> = raw.lines().collect();
    let mut out = String::new();
    for index in order {
        if let Some(line) = lines.get(*index) {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// §5.2 structured 路径的结构化事实。
pub fn structured_facts(
    hits: usize,
    candidates: &[&str],
    spill_path: Option<&str>,
    rerank: RerankState,
) -> Value {
    json!({
        "hits": hits,
        "candidates": candidates,
        "spillPath": spill_path,
        "rerank": rerank.as_str(),
    })
}

/// §17.2 资源耗尽：search 遍历的递归深度上限（起点目录算第 0 层）。
pub const MAX_WALK_DEPTH: usize = 32;
/// §17.2 资源耗尽：search 单次遍历的文件数上限。
pub const MAX_WALK_FILES: usize = 200_000;
/// §17.2 资源耗尽：search 单次遍历的时限，超时返回 TIMEOUT。
pub const MAX_WALK_DURATION: Duration = Duration::from_secs(10);

/// 遍历上限。默认值即 §17.2 的口径；测试注入更小的值验证截断与超时。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalkLimits {
    pub max_depth: usize,
    pub max_files: usize,
    pub max_duration: Duration,
}

impl Default for WalkLimits {
    fn default() -> Self {
        Self {
            max_depth: MAX_WALK_DEPTH,
            max_files: MAX_WALK_FILES,
            max_duration: MAX_WALK_DURATION,
        }
    }
}

impl WalkLimits {
    /// 纯函数：到达时限即超时。超时是错误（TIMEOUT），与截断部分结果不同。
    pub fn timed_out(&self, elapsed: Duration) -> bool {
        elapsed >= self.max_duration
    }
}

/// 一次遍历的收尾事实：处理了多少文件、是否因上限被截断。
///
/// 截断不是错误：返回已遍历的部分，并让调用方在 content 与 meta 里明确告知。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WalkOutcome {
    pub files: usize,
    pub depth_truncated: bool,
    pub files_truncated: bool,
}

impl WalkOutcome {
    /// 目录到达深度上限：其子项不再遍历，记深度截断。
    pub fn observe_dir(&mut self, depth: usize, limits: WalkLimits) {
        if depth >= limits.max_depth {
            self.depth_truncated = true;
        }
    }

    /// 文件计数；已达文件数上限时记截断并返回 true（调用方应停止遍历）。
    pub fn observe_file(&mut self, limits: WalkLimits) -> bool {
        if self.files >= limits.max_files {
            self.files_truncated = true;
            return true;
        }
        self.files += 1;
        false
    }

    /// 是否发生了截断（深度或文件数）。
    pub fn truncated(&self) -> bool {
        self.depth_truncated || self.files_truncated
    }

    /// 截断原因；未截断时为 None（写进 meta 供程序判断）。
    pub fn truncated_by(&self) -> Option<&'static str> {
        match (self.depth_truncated, self.files_truncated) {
            (false, false) => None,
            (true, false) => Some("depth"),
            (false, true) => Some("files"),
            (true, true) => Some("depth+files"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notice::render_notices;
    use crate::understanding::{MAX_PARALLEL, MAX_STEPS, schedule, validate_chain};

    /// §5.2：按 rerank 结果重排，越界 index 忽略。
    #[test]
    fn apply_rerank_follows_order_and_ignores_out_of_range() {
        let raw = "a\nb\nc\n";
        assert_eq!(apply_rerank(raw, &[2, 0, 1]), "c\na\nb\n");
        assert_eq!(apply_rerank(raw, &[2, 99]), "c\n");
        assert_eq!(apply_rerank(raw, &[]), "");
    }

    #[test]
    fn threshold_is_twenty() {
        assert!(!needs_rerank(RERANK_THRESHOLD));
        assert!(needs_rerank(RERANK_THRESHOLD + 1));
        assert_eq!(RERANK_THRESHOLD, 20);
    }

    #[test]
    fn raw_and_small_auto_need_no_rerank() {
        let raw = plan_search(Intent::Raw, 100, false).unwrap();
        assert_eq!(raw.shape, SearchShape::All);
        assert_eq!(raw.rerank, RerankState::Skipped);
        assert!(raw.notices.is_empty());

        let small = plan_search(Intent::Auto, 20, false).unwrap();
        assert_eq!(small.shape, SearchShape::All);
        assert_eq!(small.rerank, RerankState::Skipped);
        assert!(small.notices.is_empty());
    }

    #[test]
    fn many_hits_without_slot_degrades_with_notice() {
        let plan = plan_search(Intent::Auto, 25, false).unwrap();
        assert_eq!(plan.shape, SearchShape::All);
        assert_eq!(plan.rerank, RerankState::Unavailable);
        assert_eq!(plan.notices.len(), 1);
        let rendered = render_notices(&plan.notices);
        assert!(rendered.contains("slot=rerank"));
        assert!(rendered.contains("fallback=mechanical"));
    }

    #[test]
    fn many_hits_with_slot_uses_topk() {
        let plan = plan_search(Intent::Summary, 25, true).unwrap();
        assert_eq!(plan.shape, SearchShape::TopK);
        assert_eq!(plan.rerank, RerankState::Applied);
        assert!(plan.notices.is_empty());
    }

    #[test]
    fn structured_shape_has_no_notices() {
        let plan = plan_search(Intent::Structured, 25, false).unwrap();
        assert_eq!(plan.shape, SearchShape::Structured);
        assert!(plan.notices.is_empty());
    }

    #[test]
    fn visual_is_rejected_actionably() {
        let err = plan_search(Intent::Visual, 1, false).unwrap_err();
        assert!(err.contains("read"));
        assert!(err.contains("raw"));
    }

    #[test]
    fn intent_parsing_and_bad_values() {
        assert_eq!(resolve_search_intent(&json!({})).unwrap(), Intent::Raw);
        assert_eq!(
            resolve_search_intent(&json!({ "intent": "structured" })).unwrap(),
            Intent::Structured
        );
        assert!(resolve_search_intent(&json!({ "intent": "bogus" })).is_err());
    }

    #[test]
    fn chain_is_valid_and_deterministic() {
        let chain = search_chain();
        assert!(chain.len() <= MAX_STEPS);
        validate_chain(&chain, MAX_PARALLEL).unwrap();
        assert_eq!(
            schedule(&chain, MAX_PARALLEL),
            schedule(&chain, MAX_PARALLEL)
        );
        assert_eq!(schedule(&chain, MAX_PARALLEL)[0], vec![0]);
    }

    /// §17.2：遍历上限的默认值与文档口径一致（32 层 / 20 万文件 / 10 秒）。
    #[test]
    fn walk_limits_match_documented_defaults() {
        let limits = WalkLimits::default();
        assert_eq!(limits.max_depth, 32);
        assert_eq!(limits.max_files, 200_000);
        assert_eq!(limits.max_duration, Duration::from_secs(10));
    }

    /// §17.2：超时判定是纯函数，到达时限即超时。
    #[test]
    fn timed_out_is_a_pure_predicate() {
        let limits = WalkLimits::default();
        assert!(!limits.timed_out(Duration::from_secs(9)));
        assert!(limits.timed_out(Duration::from_secs(10)));
        assert!(limits.timed_out(Duration::from_secs(11)));
    }

    /// §17.2：深度与文件数分别记截断，两者都触发时原因合并。
    #[test]
    fn walk_outcome_records_depth_and_file_truncation() {
        let limits = WalkLimits {
            max_depth: 2,
            max_files: 2,
            max_duration: Duration::from_secs(10),
        };
        let mut walk = WalkOutcome::default();
        walk.observe_dir(1, limits);
        assert!(!walk.truncated(), "未到深度上限不应记截断");
        walk.observe_dir(2, limits);
        assert_eq!(walk.truncated_by(), Some("depth"));
        assert!(!walk.observe_file(limits));
        assert!(!walk.observe_file(limits));
        assert_eq!(walk.files, 2);
        assert!(walk.observe_file(limits), "文件数到上限应请求停止");
        assert_eq!(walk.files, 2, "请求停止后不再计数");
        assert_eq!(walk.truncated_by(), Some("depth+files"));
    }

    #[test]
    fn structured_facts_shape() {
        let facts = structured_facts(
            2,
            &["a.rs:1:hit", "b.rs:3:hit"],
            Some("spill://ab12cd34"),
            RerankState::Skipped,
        );
        assert_eq!(facts["hits"], json!(2));
        assert_eq!(facts["candidates"].as_array().unwrap().len(), 2);
        assert_eq!(facts["spillPath"], json!("spill://ab12cd34"));
        assert_eq!(facts["rerank"], json!("skipped"));
    }
}
