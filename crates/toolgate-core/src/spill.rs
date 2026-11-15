//! 超限输出的落盘与指针回传。
//!
//! 治理策略是「截断 + 落盘 + 给指针」，而不是把完整内容塞进上下文。
//! 文件名由内容哈希决定，因此相同输出重复落盘不会产生多份副本，
//! 也不会因为重跑同一条命令而让磁盘膨胀。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// 落盘内容的引用，随工具结果一起回传给调用方。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpillRef {
    /// 落盘文件的绝对路径。
    pub path: PathBuf,
    /// 内容字节数。
    pub bytes: usize,
    /// 内容行数。
    pub lines: usize,
}

impl SpillRef {
    /// 给模型看的一行指针说明（§18.1b 按用户语言）。
    pub fn hint(&self, lang: crate::locale::Language) -> String {
        crate::locale::tformat(
            lang,
            "完整输出已落盘：{}（{} 行 / {} 字节），需要时用 read 按段读取。",
            &[
                self.path.display().to_string(),
                self.lines.to_string(),
                crate::truncate::format_size(self.bytes),
            ],
        )
    }
}

/// 计算内容哈希的十六进制前缀，供落盘命名与行锚点复用。
pub(crate) fn sha256_short(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    hex::encode(digest)
}

/// 从快照文件名 `snapshot-<target>-<16hex>.bak` 解出目标文件名。
fn snapshot_target_of(name: &str) -> Option<String> {
    let rest = name.strip_prefix("snapshot-")?;
    let rest = rest.strip_suffix(".bak")?;
    let (target, short) = rest.rsplit_once('-')?;
    if short.len() == 16 && short.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(target.to_owned())
    } else {
        None
    }
}

/// 指定目录所在文件系统的可用字节数（statvfs）；查询失败返回 None（不阻断）。
#[cfg(unix)]
fn available_bytes(dir: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c = CString::new(dir.as_os_str().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(c.as_ptr(), &mut st) };
    if rc != 0 {
        return None;
    }
    Some((st.f_bavail as u64).saturating_mul(st.f_frsize as u64))
}

#[cfg(not(unix))]
fn available_bytes(_dir: &Path) -> Option<u64> {
    None
}

/// 修改时间落在保护期内则为 true（不参与淘汰）。
fn is_protected(
    now: std::time::SystemTime,
    modified: std::time::SystemTime,
    retention: Option<std::time::Duration>,
) -> bool {
    match retention {
        Some(r) => now.duration_since(modified).map(|age| age < r).unwrap_or(false),
        None => false,
    }
}

/// 落盘副本默认存活天数（附录 D spill.ttl_days）。
pub const DEFAULT_SPILL_TTL_DAYS: u64 = 7;
/// 每工作区落盘默认总字节上限（附录 D spill.max_bytes）。
pub const DEFAULT_SPILL_MAX_BYTES: u64 = 1024 * 1024 * 1024;

/// 快照每文件默认保留份数（附录 D snapshot.keep_per_file）。
pub const DEFAULT_SNAPSHOT_KEEP_PER_FILE: usize = 20;
/// 快照默认最短保护期，小时（附录 D snapshot.min_retention_hours）。
pub const DEFAULT_SNAPSHOT_MIN_RETENTION_HOURS: u64 = 24;
/// 快照默认存活天数（附录 D snapshot.ttl_days）。
pub const DEFAULT_SNAPSHOT_TTL_DAYS: u64 = 30;
/// 每工作区快照默认总字节上限（附录 D snapshot.max_bytes）。
pub const DEFAULT_SNAPSHOT_MAX_BYTES: u64 = 1024 * 1024 * 1024;
/// 创建快照前默认空闲空间下限（附录 D snapshot.min_free_bytes）。
pub const DEFAULT_SNAPSHOT_MIN_FREE_BYTES: u64 = 512 * 1024 * 1024;

/// 落盘存储，绑定一个目录；可选 TTL 与每工作区配额（附录 D spill 段）。
#[derive(Debug, Clone)]
pub struct SpillStore {
    dir: PathBuf,
    ttl_days: Option<u64>,
    max_bytes: Option<u64>,
    keep_per_file: Option<usize>,
    min_retention_hours: Option<u64>,
    min_free_bytes: Option<u64>,
}

impl SpillStore {
    /// 用指定目录创建存储；目录不存在时自动创建。
    pub fn new(dir: impl Into<PathBuf>) -> io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        Ok(Self {
            dir,
            ttl_days: None,
            max_bytes: None,
            keep_per_file: None,
            min_retention_hours: None,
            min_free_bytes: None,
        })
    }

    /// 设置存活与配额策略（附录 D）。仅对输出落盘目录启用；写前快照目录不设，
    /// 以免把用户待回滚的快照按 spill 的短 TTL 清掉。
    pub fn set_policy(&mut self, ttl_days: u64, max_bytes: u64) {
        self.ttl_days = Some(ttl_days);
        self.max_bytes = Some(max_bytes);
    }

    /// 设置快照生命周期策略（附录 D snapshot 段）：TTL、每工作区上限、
    /// 每文件保留份数与最短保护期。仅对写前快照目录启用。
    pub fn set_snapshot_policy(
        &mut self,
        ttl_days: u64,
        max_bytes: u64,
        keep_per_file: usize,
        min_retention_hours: u64,
        min_free_bytes: u64,
    ) {
        self.ttl_days = Some(ttl_days);
        self.max_bytes = Some(max_bytes);
        self.keep_per_file = Some(keep_per_file);
        self.min_retention_hours = Some(min_retention_hours);
        self.min_free_bytes = Some(min_free_bytes);
    }

    /// 默认落盘目录：系统临时目录下的 toolgate-spill。
    pub fn with_default_dir() -> io::Result<Self> {
        Self::new(std::env::temp_dir().join("toolgate-spill"))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// 落盘一段文本，返回指针。文件名由 kind 与内容哈希共同决定，
    /// 因此相同内容只会写一次。
    pub fn persist(&self, kind: &str, content: &str) -> io::Result<SpillRef> {
        self.enforce_policy(content.len());
        let digest = Sha256::digest(content.as_bytes());
        let short = hex::encode(&digest[..8]);
        let safe_kind: String = kind
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        let path = self.dir.join(format!("{safe_kind}-{short}.log"));
        if !path.exists() {
            fs::write(&path, content)?;
        }
        Ok(SpillRef {
            path,
            bytes: content.len(),
            lines: content.lines().count(),
        })
    }

    /// 写入前按策略清理：先删超过 TTL 的旧副本，再按 keep_per_file 淘汰同一目标文件
    /// 的旧快照，最后按每工作区配额删最旧直到放得下。保护期内与删除失败都不阻断。
    /// 任何一步失败都不阻断写入——落盘副本本就随时可丢弃（§22.5 同源）。
    fn enforce_policy(&self, incoming: usize) {
        let now = std::time::SystemTime::now();
        let retention = self
            .min_retention_hours
            .map(|h| std::time::Duration::from_secs(h.saturating_mul(3600)));
        if let Some(ttl_days) = self.ttl_days {
            let ttl = std::time::Duration::from_secs(ttl_days.saturating_mul(24 * 3600));
            if let Ok(entries) = self.list_entries() {
                for (path, _, modified) in entries {
                    if let Ok(age) = now.duration_since(modified)
                        && age > ttl
                    {
                        let _ = fs::remove_file(&path);
                    }
                }
            }
        }
        if let Some(keep) = self.keep_per_file
            && keep > 0
            && let Ok(entries) = self.list_entries()
        {
            let mut groups: std::collections::BTreeMap<
                String,
                Vec<(PathBuf, std::time::SystemTime)>,
            > = std::collections::BTreeMap::new();
            for (path, _, modified) in entries {
                if let Some(name) = path.file_name().and_then(|s| s.to_str())
                    && let Some(target) = snapshot_target_of(name)
                {
                    groups.entry(target).or_default().push((path, modified));
                }
            }
            for (_target, mut list) in groups {
                list.sort_by_key(|(_, m)| *m);
                let total = list.len();
                if total > keep {
                    for (path, modified) in list.into_iter().take(total - keep) {
                        if is_protected(now, modified, retention) {
                            continue;
                        }
                        let _ = fs::remove_file(&path);
                    }
                }
            }
        }
        if let Some(max_bytes) = self.max_bytes {
            let mut alive = self
                .list_entries()
                .unwrap_or_default()
                .into_iter()
                .filter(|(p, _, _)| p.exists())
                .collect::<Vec<(PathBuf, u64, std::time::SystemTime)>>();
            let mut total: u64 = alive.iter().map(|(_, size, _)| *size).sum();
            let incoming = incoming as u64;
            alive.sort_by_key(|(_, _, mtime)| *mtime);
            for (path, size, mtime) in alive {
                if total.saturating_add(incoming) <= max_bytes {
                    break;
                }
                if is_protected(now, mtime, retention) {
                    continue;
                }
                if fs::remove_file(&path).is_ok() {
                    total = total.saturating_sub(size);
                }
            }
        }
    }

    /// 立即按当前策略清理一次（供 CLI `snapshot prune` 手动巡检），返回清理前后字节数。
    pub fn prune_now(&self) -> io::Result<(u64, u64)> {
        let before = self.total_bytes()?;
        self.enforce_policy(0);
        let after = self.total_bytes()?;
        Ok((before, after))
    }

    /// 目录内普通文件总字节数。
    fn total_bytes(&self) -> io::Result<u64> {
        Ok(self.list_entries()?.iter().map(|(_, size, _)| *size).sum())
    }

    /// 列出目录下普通文件及其大小与修改时间。
    fn list_entries(&self) -> io::Result<Vec<(PathBuf, u64, std::time::SystemTime)>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if !meta.is_file() {
                continue;
            }
            let modified = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            out.push((entry.path(), meta.len(), modified));
        }
        Ok(out)
    }

    /// 在覆盖写之前保存旧内容，返回可回滚的快照指针。
    ///
    /// 与 persist 的区别是意图：persist 处理「输出太大放不下」，
    /// snapshot 处理「改之前留一份」。两者都走内容寻址，所以对
    /// 同一份旧内容反复快照不会产生多份副本。
    ///
    /// 回滚方式：用 read 读快照，再用 write 写回原路径。刻意不做
    /// 自动回滚工具——回滚是低频动作，为它增加第 7 个工具位不值。
    pub fn snapshot(&self, target: &Path, content: &str) -> io::Result<SpillRef> {
        self.enforce_policy(content.len());
        // §8.12：创建前做一次 O(1) 空闲空间检查，低于 min_free_bytes 则拒绝创建。
        if let Some(min_free) = self.min_free_bytes
            && let Some(avail) = available_bytes(&self.dir)
            && avail < min_free
        {
            return Err(io::Error::other(format!(
                "SNAPSHOT_QUOTA_EXCEEDED: 可用空间 {avail} 字节低于下限 {min_free} 字节"
            )));
        }
        let digest = Sha256::digest(content.as_bytes());
        let short = hex::encode(&digest[..8]);
        let name: String = target
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("file")
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let path = self.dir.join(format!("snapshot-{name}-{short}.bak"));
        if !path.exists() {
            fs::write(&path, content)?;
        }
        // 写后按 keep_per_file / 配额再收敛一次，使目录稳定在策略上限内。
        self.enforce_policy(0);
        Ok(SpillRef {
            path,
            bytes: content.len(),
            lines: content.lines().count(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_once_for_identical_content() {
        let dir = std::env::temp_dir().join(format!("toolgate-test-{}", std::process::id()));
        let store = SpillStore::new(&dir).expect("create store");
        let a = store.persist("bash", "hello\nworld\n").expect("persist a");
        let b = store.persist("bash", "hello\nworld\n").expect("persist b");
        assert_eq!(a.path, b.path, "相同内容应复用同一文件");
        assert_eq!(a.lines, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_keeps_old_content_restorable() {
        let dir = std::env::temp_dir().join(format!("toolgate-snap-{}", std::process::id()));
        let store = SpillStore::new(&dir).expect("create store");
        let target = Path::new("/tmp/src/main.rs");
        let snap = store
            .snapshot(
                target,
                "fn old() {}
",
            )
            .expect("snapshot");
        assert!(snap.path.to_string_lossy().contains("snapshot-main.rs"));
        let restored = std::fs::read_to_string(&snap.path).expect("read back");
        assert_eq!(
            restored,
            "fn old() {}
"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_keep_per_file_evicts_oldest() {
        let dir = std::env::temp_dir().join(format!("toolgate-snap-keep-{}", std::process::id()));
        let mut store = SpillStore::new(&dir).expect("create store");
        store.set_snapshot_policy(30, 1024 * 1024 * 1024, 2, 0, 0);
        let target = Path::new("/tmp/app.rs");
        for i in 0..3 {
            store
                .snapshot(target, &format!("version-{i}\n"))
                .expect("snapshot");
        }
        let count = std::fs::read_dir(&dir)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .map(|x| x == "bak")
                    .unwrap_or(false)
            })
            .count();
        assert_eq!(count, 2, "keep_per_file=2 应只留 2 份");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn quota_evicts_oldest_before_write() {
        let dir = std::env::temp_dir().join(format!("toolgate-spill-quota-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = SpillStore::new(&dir).expect("create store");
        store.set_policy(DEFAULT_SPILL_TTL_DAYS, 100);
        let a = "x".repeat(60);
        let b = "y".repeat(60);
        store.persist("bash", &a).expect("persist a");
        store.persist("bash", &b).expect("persist b");
        let count = std::fs::read_dir(&dir).map(|it| it.count()).unwrap_or(0);
        assert_eq!(count, 1, "配额 100 字节：写第二个 60 字节时应淘汰第一个");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn different_content_gets_different_files() {
        let dir = std::env::temp_dir().join(format!("toolgate-test2-{}", std::process::id()));
        let store = SpillStore::new(&dir).expect("create store");
        let a = store.persist("search", "one").expect("persist a");
        let b = store.persist("search", "two").expect("persist b");
        assert_ne!(a.path, b.path);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
