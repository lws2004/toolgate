//! 写侧统一治理入口 pre 的写前复核与写前快照（§18.6 写侧能力边界 / §18.7 / §20）。
//!
//! 宿主内置 write / edit 执行**前**，适配器把目标路径、期望指纹 (mtime,size,sha256)
//! 送进来：与当前真值一致则写前快照并放行；不一致返回 PRE_STALE 并 block，
//! 让 agent 重新 read 后再写。只覆盖经 pre 的文件写；bash / 脚本副作用不在保护内
//! （命令执行前不可枚举，§18.6）。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::ErrorView;
use crate::spill::SpillStore;

/// 期望的文件指纹（调用方上次 read 到的真值）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fingerprint {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtime: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

impl Fingerprint {
    fn is_empty(&self) -> bool {
        self.mtime.is_none() && self.size.is_none() && self.sha256.is_none()
    }
}

/// pre 来源：宿主工具名 + 主路径（可选，用于整体标注）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreSource {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// 单条写操作：op=write|edit，path，期望指纹，以及（可选）即将写入的 payload。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreOp {
    pub op: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<Fingerprint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrePolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify: Option<bool>,
}

/// pre 请求（§18.7）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<PreSource>,
    #[serde(default)]
    pub ops: Vec<PreOp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<PrePolicy>,
}

/// 单条写操作的结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreOpResult {
    pub op: String,
    pub path: String,
    /// allow | block
    pub decision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_pointer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
}

/// pre 响应（§18.7）：整体 decision + 快照指针 + 逐条明细。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreResponse {
    /// allow | block
    pub decision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_pointer: Option<String>,
    pub ops: Vec<PreOpResult>,
}

struct CurrentFile {
    size: u64,
    sha256: String,
    mtime: i64,
}

impl CurrentFile {
    fn to_fingerprint(&self) -> Fingerprint {
        Fingerprint {
            mtime: Some(self.mtime),
            size: Some(self.size),
            sha256: Some(self.sha256.clone()),
        }
    }
}

/// 写前复核入口：逐条复核 + 快照，任一不一致即整体 block。
pub fn pre(req: &PreRequest, spill: &SpillStore, cwd: &Path) -> PreResponse {
    let policy = req.policy.clone().unwrap_or_default();
    let verify = policy.verify.unwrap_or(true);
    let snapshot = policy.snapshot.unwrap_or(true);

    let mut results: Vec<PreOpResult> = Vec::new();
    let mut any_block = false;
    let mut last_pointer: Option<String> = None;

    for op in &req.ops {
        let resolved = resolve_path(&op.path, cwd);
        let current = read_current(&resolved);

        // 复核：期望指纹与当前真值比较。
        let mismatch = if verify {
            op.before
                .as_ref()
                .filter(|b| !b.is_empty())
                .and_then(|before| compare(&current, before, &op.path))
        } else {
            None
        };

        if let Some(reason) = mismatch {
            any_block = true;
            results.push(PreOpResult {
                op: op.op.clone(),
                path: op.path.clone(),
                decision: "block".to_owned(),
                snapshot_pointer: None,
                error: Some(pre_stale(&op.path, &reason)),
            });
            continue;
        }

        // 一致（或无期望）：写前快照（新文件无需快照）。
        let mut pointer = None;
        if snapshot
            && current.is_some()
            && let Ok(content) = fs::read_to_string(&resolved)
            && let Ok(sp) = spill.snapshot(&resolved, &content)
        {
            let p = sp.path.display().to_string();
            last_pointer = Some(p.clone());
            pointer = Some(p);
        }

        results.push(PreOpResult {
            op: op.op.clone(),
            path: op.path.clone(),
            decision: "allow".to_owned(),
            snapshot_pointer: pointer,
            error: None,
        });
    }

    PreResponse {
        decision: if any_block { "block".to_owned() } else { "allow".to_owned() },
        snapshot_pointer: last_pointer,
        ops: results,
    }
}

fn compare(current: &Option<CurrentFile>, before: &Fingerprint, path: &str) -> Option<String> {
    match current {
        None => {
            // 期望存在但当前已消失。
            if before.size.is_some() || before.sha256.is_some() {
                Some(format!("{path} 已不存在（期望存在）"))
            } else {
                None
            }
        }
        Some(cur) => {
            if let Some(size) = before.size
                && size != cur.size
            {
                return Some(format!("大小变化：期望 {size} 字节，当前 {} 字节", cur.size));
            }
            if let Some(sha) = &before.sha256
                && !sha.eq_ignore_ascii_case(&cur.sha256)
            {
                return Some(format!(
                    "内容变化：sha256 期望 {}…，当前 {}…",
                    short(sha),
                    short(&cur.sha256)
                ));
            }
            if let Some(mtime) = before.mtime
                && mtime != cur.mtime
            {
                return Some(format!("mtime 变化：期望 {mtime}，当前 {}", cur.mtime));
            }
            None
        }
    }
}

fn pre_stale(path: &str, reason: &str) -> Value {
    ErrorView {
        code: "PRE_STALE".to_owned(),
        fact: format!("写前复核失败：{path} 自上次读取后已变更"),
        reason: reason.to_owned(),
        next_steps: vec![
            "重新 read 目标文件获取最新 (mtime,size,sha256) 后再写".to_owned(),
            "确认没有其他进程并发修改该文件".to_owned(),
        ],
    }
    .to_structured()
}

fn short(sha: &str) -> String {
    sha.chars().take(8).collect()
}

fn read_current(path: &Path) -> Option<CurrentFile> {
    let bytes = fs::read(path).ok()?;
    let meta = fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Some(CurrentFile {
        size: bytes.len() as u64,
        sha256: hex::encode(hasher.finalize()),
        mtime,
    })
}

fn resolve_path(raw: &str, cwd: &Path) -> PathBuf {
    let p = Path::new(raw);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    }
}

/// 便捷：把当前文件真值编码成 Fingerprint（供客户端在 read 时记录，pre 时回传）。
pub fn fingerprint_of(path: &Path) -> Option<Fingerprint> {
    read_current(path).map(|c| c.to_fingerprint())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("toolgate-pre-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn store(tag: &str) -> SpillStore {
        SpillStore::new(tmpdir(&format!("{tag}-snap"))).unwrap()
    }

    #[test]
    fn matching_fingerprint_allows_and_snapshots() {
        let dir = tmpdir("ok");
        let file = dir.join("a.txt");
        fs::write(&file, b"hello").unwrap();
        let before = fingerprint_of(&file).unwrap();
        let req = PreRequest {
            source: None,
            ops: vec![PreOp {
                op: "write".to_owned(),
                path: file.display().to_string(),
                before: Some(before),
                payload: None,
            }],
            policy: Some(PrePolicy { snapshot: Some(true), verify: Some(true) }),
        };
        let resp = pre(&req, &store("ok"), &dir);
        assert_eq!(resp.decision, "allow");
        assert!(resp.snapshot_pointer.is_some());
        assert_eq!(resp.ops[0].decision, "allow");
    }

    #[test]
    fn stale_sha_blocks_with_pre_stale() {
        let dir = tmpdir("stale");
        let file = dir.join("b.txt");
        fs::write(&file, b"v1").unwrap();
        let before = fingerprint_of(&file).unwrap();
        // 并发修改后内容变化。
        let mut f = fs::OpenOptions::new().write(true).truncate(true).open(&file).unwrap();
        f.write_all(b"v2-changed").unwrap();
        drop(f);
        let req = PreRequest {
            source: None,
            ops: vec![PreOp {
                op: "edit".to_owned(),
                path: file.display().to_string(),
                before: Some(before),
                payload: None,
            }],
            policy: None,
        };
        let resp = pre(&req, &store("stale"), &dir);
        assert_eq!(resp.decision, "block");
        assert_eq!(resp.ops[0].error.as_ref().unwrap()["code"], json!("PRE_STALE"));
        assert!(resp.snapshot_pointer.is_none());
    }

    #[test]
    fn new_file_without_before_allows_no_snapshot() {
        let dir = tmpdir("new");
        let file = dir.join("new.txt");
        let req = PreRequest {
            source: None,
            ops: vec![PreOp {
                op: "write".to_owned(),
                path: file.display().to_string(),
                before: None,
                payload: Some(json!("x")),
            }],
            policy: None,
        };
        let resp = pre(&req, &store("new"), &dir);
        assert_eq!(resp.decision, "allow");
        assert!(resp.snapshot_pointer.is_none());
    }

    #[test]
    fn missing_file_expected_present_blocks() {
        let dir = tmpdir("missing");
        let file = dir.join("gone.txt");
        let req = PreRequest {
            source: None,
            ops: vec![PreOp {
                op: "write".to_owned(),
                path: file.display().to_string(),
                before: Some(Fingerprint {
                    mtime: None,
                    size: Some(10),
                    sha256: None,
                }),
                payload: None,
            }],
            policy: None,
        };
        let resp = pre(&req, &store("missing"), &dir);
        assert_eq!(resp.decision, "block");
        assert_eq!(resp.ops[0].error.as_ref().unwrap()["code"], json!("PRE_STALE"));
    }
}
