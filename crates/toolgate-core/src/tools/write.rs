//! write：全量写入器。
//!
//! 只做新建或整体覆盖。局部修改请用 edit——write 覆盖一个已有文件
//! 时会把整份内容记进上下文，是 token 上的浪费。

use serde_json::{Value, json};

use super::{ToolContext, ToolResponse, ToolSpec, required_str};
use crate::ToolError;

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: "write",
        description: "新建文件或整体覆盖已有文件。父目录不存在时自动创建。\
                      局部修改请用 edit，不要用 write 覆盖大文件。",
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "目标文件路径" },
                "content": { "type": "string", "description": "完整文件内容" }
            },
            "required": ["path", "content"]
        }),
    }
}

pub async fn run(args: &Value, ctx: &ToolContext) -> Result<ToolResponse, ToolError> {
    let path = ctx.resolve(required_str(args, "path")?);
    let content = required_str(args, "content")?;

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent).await?;
        }
    }

    let existed = path.exists();

    // 覆盖前留一份旧内容。新建文件没有旧内容，跳过。
    let snapshot = if existed {
        match tokio::fs::read_to_string(&path).await {
            Ok(old) => ctx.snapshots.snapshot(&path, &old).ok(),
            Err(_) => None, // 原文件不是合法 UTF-8，无法做文本快照
        }
    } else {
        None
    };

    tokio::fs::write(&path, content).await?;

    let lines = content.lines().count();
    let snapshot_meta = snapshot
        .as_ref()
        .map(|s| json!(s.path.display().to_string()))
        .unwrap_or(json!(null));

    let mut body = format!(
        "{} {}（{} 行 / {} 字节）",
        if existed { "已覆盖" } else { "已创建" },
        path.display(),
        lines,
        content.len()
    );
    if let Some(s) = &snapshot {
        body.push_str(&format!("\n旧内容已快照：{}（需要回滚时读它再写回）", s.path.display()));
    }

    Ok(ToolResponse::text(body).with_meta(json!({
        "path": path.display().to_string(),
        "bytes": content.len(),
        "lines": lines,
        "created": !existed,
        "snapshot": snapshot_meta,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn creates_file_and_parents() {
        let dir = std::env::temp_dir().join(format!("tg-write-{}", std::process::id()));
        let ctx = ToolContext::new(&dir).expect("ctx");
        let resp = run(
            &json!({ "path": "a/b/c.txt", "content": "hello\n" }),
            &ctx,
        )
        .await
        .expect("write ok");
        assert_eq!(resp.meta["created"], json!(true));
        let read_back = std::fs::read_to_string(dir.join("a/b/c.txt")).expect("read back");
        assert_eq!(read_back, "hello\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
