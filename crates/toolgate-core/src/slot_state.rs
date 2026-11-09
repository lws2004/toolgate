//! 槽位失效状态机（§3.11）：declared → verified → degraded → disabled。
//!
//! 状态是进程内可丢弃缓存（§22.5），不持久化；改配置并重启后重新校验。
//! 本模块只维护状态与转移，不发起模型调用、不读配置、不写磁盘。
//!
//! 架构依据：03-slots.md §3.9/§3.11、06-cost.md §7.3、20-behavior.md §22.5。

use serde::{Deserialize, Serialize};

use crate::slots::SlotKind;

/// 连续失败多少次后禁用（§3.11 默认 3）。
pub const DEFAULT_MAX_FAILURES: u32 = 3;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum SlotState {
    /// 配置声明，尚未经首次真实调用校验。
    #[default]
    Declared,
    /// 首次校验通过。
    Verified,
    /// 单次失败，按 §7.3 降级。
    Degraded,
    /// 连续失败或形状完全不符；不再尝试调用。
    Disabled,
}

impl SlotState {
    pub fn as_str(&self) -> &'static str {
        match self {
            SlotState::Declared => "declared",
            SlotState::Verified => "verified",
            SlotState::Degraded => "degraded",
            SlotState::Disabled => "disabled",
        }
    }
}

/// 单个槽位的状态与诊断信息（供 toolgate list 显示原因与失败计数）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SlotHealth {
    pub state: SlotState,
    pub failure_count: u32,
    pub reason: Option<String>,
}

/// 失败类别：形状完全不符直接禁用，其余按连续失败计数处理（§3.11）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    Transient,
    ShapeMismatch,
}

/// 五个槽位的状态表。match 穷举，新增槽位即编译失败。
fn slot_index(kind: SlotKind) -> usize {
    match kind {
        SlotKind::Classify => 0,
        SlotKind::Rerank => 1,
        SlotKind::Decide => 2,
        SlotKind::Summarize => 3,
        SlotKind::Vision => 4,
    }
}

pub struct SlotHealthMap {
    entries: [SlotHealth; 5],
    max_failures: u32,
}

impl Default for SlotHealthMap {
    fn default() -> Self {
        Self::new()
    }
}

impl SlotHealthMap {
    pub fn new() -> Self {
        Self::with_max_failures(DEFAULT_MAX_FAILURES)
    }

    pub fn with_max_failures(max_failures: u32) -> Self {
        Self {
            entries: std::array::from_fn(|_| SlotHealth::default()),
            max_failures,
        }
    }

    pub fn status(&self, slot: SlotKind) -> &SlotHealth {
        &self.entries[slot_index(slot)]
    }

    /// disabled 后不再尝试，避免每次都花一次失败调用（§3.11）。
    pub fn is_usable(&self, slot: SlotKind) -> bool {
        self.status(slot).state != SlotState::Disabled
    }

    /// 首次真实调用成功即视为校验通过；成功重置连续失败计数。
    /// disabled 只能靠改配置并重启（reset）恢复，成功不会让它复活。
    pub fn on_success(&mut self, slot: SlotKind) {
        let h = &mut self.entries[slot_index(slot)];
        h.failure_count = 0;
        h.reason = None;
        match h.state {
            SlotState::Declared | SlotState::Verified | SlotState::Degraded => {
                h.state = SlotState::Verified;
            }
            SlotState::Disabled => {}
        }
    }

    pub fn on_failure(&mut self, slot: SlotKind, reason: impl Into<String>, kind: FailureKind) {
        let h = &mut self.entries[slot_index(slot)];
        h.failure_count += 1;
        h.reason = Some(reason.into());
        match kind {
            FailureKind::ShapeMismatch => h.state = SlotState::Disabled,
            FailureKind::Transient => {
                if h.state != SlotState::Disabled {
                    h.state = if h.failure_count >= self.max_failures {
                        SlotState::Disabled
                    } else {
                        SlotState::Degraded
                    };
                }
            }
        }
    }

    /// 改配置并重启后恢复：全部回到 declared，重新走首次校验。
    pub fn reset(&mut self) {
        self.entries = std::array::from_fn(|_| SlotHealth::default());
    }

    /// 供 toolgate list 打印，顺序与 SlotKind::ALL 一致。
    pub fn all(&self) -> Vec<(SlotKind, SlotHealth)> {
        SlotKind::ALL
            .iter()
            .map(|kind| (*kind, self.status(*kind).clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn health() -> SlotHealthMap {
        SlotHealthMap::new()
    }

    #[test]
    fn initial_all_declared_and_usable() {
        let m = health();
        for k in SlotKind::ALL {
            let h = m.status(k);
            assert_eq!(h.state, SlotState::Declared);
            assert_eq!(h.failure_count, 0);
            assert!(h.reason.is_none());
            assert!(m.is_usable(k));
        }
    }

    #[test]
    fn first_success_verifies_only_that_slot() {
        let mut m = health();
        m.on_success(SlotKind::Decide);
        assert_eq!(m.status(SlotKind::Decide).state, SlotState::Verified);
        assert_eq!(m.status(SlotKind::Classify).state, SlotState::Declared);
    }

    #[test]
    fn single_failure_degrades_and_records_reason() {
        let mut m = health();
        m.on_success(SlotKind::Decide);
        m.on_failure(SlotKind::Decide, "调用超时", FailureKind::Transient);
        let h = m.status(SlotKind::Decide);
        assert_eq!(h.state, SlotState::Degraded);
        assert_eq!(h.failure_count, 1);
        assert_eq!(h.reason.as_deref(), Some("调用超时"));
        assert!(m.is_usable(SlotKind::Decide));
    }

    #[test]
    fn three_consecutive_failures_disable() {
        let mut m = health();
        for i in 1..=3 {
            m.on_failure(
                SlotKind::Vision,
                format!("失败 {i}"),
                FailureKind::Transient,
            );
        }
        assert_eq!(m.status(SlotKind::Vision).state, SlotState::Disabled);
        assert_eq!(m.status(SlotKind::Vision).failure_count, 3);
        assert!(!m.is_usable(SlotKind::Vision));
    }

    #[test]
    fn success_resets_count_and_recovers_from_degraded() {
        let mut m = health();
        m.on_failure(SlotKind::Rerank, "超时", FailureKind::Transient);
        m.on_failure(SlotKind::Rerank, "超时", FailureKind::Transient);
        assert_eq!(m.status(SlotKind::Rerank).state, SlotState::Degraded);
        m.on_success(SlotKind::Rerank);
        let h = m.status(SlotKind::Rerank);
        assert_eq!(h.state, SlotState::Verified);
        assert_eq!(h.failure_count, 0);
        assert!(h.reason.is_none());
        m.on_failure(SlotKind::Rerank, "又失败", FailureKind::Transient);
        assert_eq!(m.status(SlotKind::Rerank).failure_count, 1);
        assert_eq!(m.status(SlotKind::Rerank).state, SlotState::Degraded);
    }

    #[test]
    fn shape_mismatch_disables_immediately() {
        let mut m = health();
        m.on_success(SlotKind::Decide);
        m.on_failure(
            SlotKind::Decide,
            "输出缺 probability",
            FailureKind::ShapeMismatch,
        );
        assert_eq!(m.status(SlotKind::Decide).state, SlotState::Disabled);
        assert_eq!(m.status(SlotKind::Decide).failure_count, 1);
    }

    #[test]
    fn disabled_slot_stays_disabled_on_success() {
        let mut m = health();
        for _ in 0..3 {
            m.on_failure(SlotKind::Summarize, "fail", FailureKind::Transient);
        }
        assert_eq!(m.status(SlotKind::Summarize).state, SlotState::Disabled);
        m.on_success(SlotKind::Summarize);
        assert_eq!(m.status(SlotKind::Summarize).state, SlotState::Disabled);
    }

    #[test]
    fn reset_after_config_change_restores_all() {
        let mut m = health();
        for _ in 0..3 {
            m.on_failure(SlotKind::Classify, "fail", FailureKind::Transient);
        }
        assert!(!m.is_usable(SlotKind::Classify));
        m.reset();
        for k in SlotKind::ALL {
            assert_eq!(m.status(k).state, SlotState::Declared);
            assert_eq!(m.status(k).failure_count, 0);
        }
    }

    #[test]
    fn custom_max_failures() {
        let mut m = SlotHealthMap::with_max_failures(2);
        m.on_failure(SlotKind::Vision, "1", FailureKind::Transient);
        assert_eq!(m.status(SlotKind::Vision).state, SlotState::Degraded);
        m.on_failure(SlotKind::Vision, "2", FailureKind::Transient);
        assert_eq!(m.status(SlotKind::Vision).state, SlotState::Disabled);
    }

    #[test]
    fn state_serde_stable_and_exhaustive() {
        let all = [
            SlotState::Declared,
            SlotState::Verified,
            SlotState::Degraded,
            SlotState::Disabled,
        ];
        for s in all {
            match s {
                SlotState::Declared => {}
                SlotState::Verified => {}
                SlotState::Degraded => {}
                SlotState::Disabled => {}
            }
            assert_eq!(serde_json::to_value(s).unwrap(), json!(s.as_str()));
        }
        assert_eq!(DEFAULT_MAX_FAILURES, 3);
    }

    /// §22.5：状态是可丢弃缓存，实例间互不共享。
    #[test]
    fn state_is_in_memory_cache_only() {
        let mut a = health();
        a.on_failure(SlotKind::Decide, "x", FailureKind::ShapeMismatch);
        let b = health();
        assert_eq!(b.status(SlotKind::Decide).state, SlotState::Declared);
    }

    #[test]
    fn all_reports_five_slots_in_fixed_order() {
        let m = health();
        let rows = m.all();
        assert_eq!(rows.len(), 5);
        let kinds: Vec<SlotKind> = rows.iter().map(|(k, _)| *k).collect();
        assert_eq!(kinds, SlotKind::ALL.to_vec());
    }
}
