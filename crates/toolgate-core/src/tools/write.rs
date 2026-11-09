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

    // 覆盖已有文件时沿用它的编码；新建文件用 UTF-8。
    // 这样 write 一个 GBK 文件不会把它悄悄转成 UTF-8。
    let (encoding, snapshot) = if existed {
        match tokio::fs::read(&path).await {
            Ok(old_bytes) => {
                let enc = crate::encoding::FileEncoding::detect(&old_bytes);
                let (old_text, _) = enc.decode(&old_bytes);
                (enc, ctx.snapshots.snapshot(&path, &old_text).ok())
            }
            Err(_) => (crate::encoding::FileEncoding::default(), None),
        }
    } else {
        (crate::encoding::FileEncoding::default(), None)
    };

    // 目标编码无法表示新内容时明确报错，不静默替换成问号。
    let out_bytes = encoding
        .encode(content)
        .map_err(|e| ToolError::Failed(format!("{} 写入失败：{e}", path.display())))?;
    tokio::fs::write(&path, &out_bytes).await?;

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
        body.push_str(&format!(
            "\n旧内容已快照：{}（需要回滚时读它再写回）",
            s.path.display()
        ));
    }

    Ok(ToolResponse::text(body).with_meta(json!({
        "path": path.display().to_string(),
        "bytes": content.len(),
        "lines": lines,
        "created": !existed,
        "snapshot": snapshot_meta,
        "encoding": encoding.name(),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn creates_file_and_parents() {
        let dir = std::env::temp_dir().join(format!("tg-write-{}", std::process::id()));
        let ctx = ToolContext::new(&dir).expect("ctx");
        let resp = run(&json!({ "path": "a/b/c.txt", "content": "hello\n" }), &ctx)
            .await
            .expect("write ok");
        assert_eq!(resp.meta["created"], json!(true));
        let read_back = std::fs::read_to_string(dir.join("a/b/c.txt")).expect("read back");
        assert_eq!(read_back, "hello\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
