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

/// 落盘存储，绑定一个目录。
#[derive(Debug, Clone)]
pub struct SpillStore {
    dir: PathBuf,
}

impl SpillStore {
    /// 用指定目录创建存储；目录不存在时自动创建。
    pub fn new(dir: impl Into<PathBuf>) -> io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        Ok(Self { dir })
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

    /// 在覆盖写之前保存旧内容，返回可回滚的快照指针。
    ///
    /// 与 persist 的区别是意图：persist 处理「输出太大放不下」，
    /// snapshot 处理「改之前留一份」。两者都走内容寻址，所以对
    /// 同一份旧内容反复快照不会产生多份副本。
    ///
    /// 回滚方式：用 read 读快照，再用 write 写回原路径。刻意不做
    /// 自动回滚工具——回滚是低频动作，为它增加第 7 个工具位不值。
    pub fn snapshot(&self, target: &Path, content: &str) -> io::Result<SpillRef> {
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
    fn different_content_gets_different_files() {
        let dir = std::env::temp_dir().join(format!("toolgate-test2-{}", std::process::id()));
        let store = SpillStore::new(&dir).expect("create store");
        let a = store.persist("search", "one").expect("persist a");
        let b = store.persist("search", "two").expect("persist b");
        assert_ne!(a.path, b.path);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
