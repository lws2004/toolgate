//! §22 L0 预热 / L1 预判：可丢弃缓存 + 确定性规则表。
//!
//! 缓存不属于语义状态（§22.5 / §11.2）：随时可丢、失效只影响延迟，
//! 不进任何「必须一致」的路径；预判只按固定规则，不用模型采样（§22.4）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use crate::decision::Intent;

/// §22.3 预热上限：超过则退化为按需读。取 §22.3 的示例值，
/// 阈值标定需实测（§15.4），此处不声称已标定。
pub const WARM_MAX_BYTES: u64 = 1024 * 1024;

/// §22.3「预热成本必须可算」：单文件上限之外，整体也要有上限——
/// 否则连续 read 大文件会把内存吃满。超限整体丢弃（缓存可随时丢弃，§22.5）。
pub const WARM_MAX_TOTAL_BYTES: usize = 64 * 1024 * 1024;
/// 预热条目数上限（同样的成本控制口径）。
pub const WARM_MAX_ENTRIES: usize = 32;

/// 文件失效判据：mtime + size（§22.5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileKey {
    pub mtime: Option<SystemTime>,
    pub size: u64,
}

impl FileKey {
    pub fn of(mtime: Option<SystemTime>, size: u64) -> Self {
        Self { mtime, size }
    }
}

/// 是否值得预热：只预热 1MB 以下的文本（§22.3）。
pub fn should_warm(size: u64) -> bool {
    size <= WARM_MAX_BYTES
}

/// §22.4 按 intent 的预判动作。规则确定，同输入同输出。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreheatAction {
    /// 预热完整解码 + 编码 / 行尾 / 行区间（read 之后大概率要 edit 或续读）。
    Full,
    /// 不预热（只要了解、或已有其它路径在做）。
    None,
}

/// §22.4 规则表：read + raw → Full；其余一律不预热。
/// bash + summary 的错误行预解析由 §5.1 的 decide 路径承担，不在此重复。
pub fn predict(tool: &str, intent: Intent) -> PreheatAction {
    match (tool, intent) {
        ("read", Intent::Raw) => PreheatAction::Full,
        _ => PreheatAction::None,
    }
}

/// 预热条目：原始字节 + 解码结果 + 编码名，供命中后跳过 IO 与解码。
#[derive(Debug, Clone, PartialEq)]
pub struct WarmText {
    pub key: FileKey,
    pub bytes: Vec<u8>,
    pub text: String,
    pub encoding_name: &'static str,
    pub had_decode_errors: bool,
}

/// 可丢弃缓存：Arc 让 ToolContext 的 clone 共享同一份，
/// 但不构成语义状态——clear() 后所有调用照常工作。
#[derive(Debug, Clone, Default)]
pub struct WarmCache {
    entries: Arc<Mutex<HashMap<PathBuf, WarmText>>>,
}

impl WarmCache {
    /// 命中条件：存在且 mtime + size 均未变（§22.5）。
    pub fn get(&self, path: &Path, key: FileKey) -> Option<WarmText> {
        let guard = self.entries.lock().ok()?;
        let entry = guard.get(path)?;
        if entry.key == key {
            Some(entry.clone())
        } else {
            None
        }
    }

    pub fn put(&self, path: PathBuf, entry: WarmText) {
        let Ok(mut guard) = self.entries.lock() else {
            return;
        };
        // §22.3：成本可算——总量或条目数超限时整体丢弃，不做复杂的 LRU。
        let total: usize = guard.values().map(|e| e.bytes.len()).sum();
        if guard.len() >= WARM_MAX_ENTRIES || total + entry.bytes.len() > WARM_MAX_TOTAL_BYTES {
            guard.clear();
        }
        guard.insert(path, entry);
    }

    /// §22.5：缓存可随时丢弃。
    pub fn clear(&self) {
        if let Ok(mut guard) = self.entries.lock() {
            guard.clear();
        }
    }

    pub fn len(&self) -> usize {
        self.entries.lock().map(|g| g.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predict_is_deterministic_and_matches_rule_table() {
        assert_eq!(predict("read", Intent::Raw), PreheatAction::Full);
        assert_eq!(predict("read", Intent::Summary), PreheatAction::None);
        assert_eq!(predict("bash", Intent::Summary), PreheatAction::None);
        assert_eq!(predict("edit", Intent::Raw), PreheatAction::None);
        for intent in [
            Intent::Raw,
            Intent::Auto,
            Intent::Visual,
            Intent::Structured,
        ] {
            assert_eq!(predict("read", intent), predict("read", intent));
        }
    }

    #[test]
    fn warm_size_cap_is_one_megabyte() {
        assert!(should_warm(WARM_MAX_BYTES));
        assert!(!should_warm(WARM_MAX_BYTES + 1));
    }

    /// §22.3：条目数超限时整体丢弃，避免连续 read 大文件把内存吃满。
    #[test]
    fn entry_limit_discards_cache_instead_of_growing() {
        let cache = WarmCache::default();
        for index in 0..=WARM_MAX_ENTRIES {
            cache.put(PathBuf::from(format!("/tmp/warm-{index}.txt")), entry(None, 5));
        }
        // 第 WARM_MAX_ENTRIES+1 次写入前触发清空，之后只剩最新一条。
        let last = PathBuf::from(format!("/tmp/warm-{WARM_MAX_ENTRIES}.txt"));
        assert!(cache.get(&last, FileKey::of(None, 5)).is_some());
        let first = PathBuf::from("/tmp/warm-0.txt");
        assert!(cache.get(&first, FileKey::of(None, 5)).is_none());
    }

    fn entry(mtime: Option<SystemTime>, size: u64) -> WarmText {
        WarmText {
            key: FileKey::of(mtime, size),
            bytes: b"hello".to_vec(),
            text: "hello".to_owned(),
            encoding_name: "UTF-8",
            had_decode_errors: false,
        }
    }

    #[test]
    fn cache_hits_only_while_mtime_and_size_match() {
        let cache = WarmCache::default();
        let path = Path::new("/tmp/x");
        let t = SystemTime::now();
        cache.put(path.to_path_buf(), entry(Some(t), 5));
        assert!(cache.get(path, FileKey::of(Some(t), 5)).is_some());
        assert!(cache.get(path, FileKey::of(None, 5)).is_none());
        assert!(cache.get(path, FileKey::of(Some(t), 6)).is_none());
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn cache_can_be_dropped_without_breaking_anything() {
        let cache = WarmCache::default();
        let shared = cache.clone();
        let path = Path::new("/tmp/x");
        cache.put(path.to_path_buf(), entry(None, 5));
        assert_eq!(shared.len(), 1, "clone 应共享同一份缓存");
        cache.clear();
        assert!(shared.is_empty());
        assert!(cache.get(path, FileKey::of(None, 5)).is_none());
    }
}
