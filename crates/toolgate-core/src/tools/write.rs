//! write：全量写入器。
//!
//! 只做新建或整体覆盖。局部修改请用 edit——write 覆盖一个已有文件
//! 时会把整份内容记进上下文，是 token 上的浪费。

use std::path::Path;

use serde_json::{Value, json};

use super::{ToolContext, ToolResponse, ToolSpec, required_str};
use crate::ToolError;
use crate::compat::{CompatConfig, resolve_intent};
use crate::edit_path::{
    FileFacts, change_facts, detect_eol, diff_stats, nearby_build_files, unified_diff,
};
use crate::locale::{t, tformat};
use crate::read_path::display_path;
use crate::truncate::{TruncationOptions, truncate_head};

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: "write",
        description: "能做什么：新建或整体覆盖文件，自动建父目录，覆盖前留快照并沿用原编码。不能做什么：不摘要 / 判定大块内容（§4.0）、不做局部替换、不返回图片。什么时候用别的工具：局部改用 edit、查找用 search、读取用 read。",
        description_en: "Can: create or fully overwrite a file, creating parent dirs, snapshot before and keep encoding. Cannot: summarize/judge large content (§4.0), partial replace, return images. Use instead: edit for local changes, search to find, read to inspect.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "目标文件路径" },
                "content": { "type": "string", "description": "完整文件内容" },
                "intent": { "type": "string", "enum": ["raw", "summary", "structured", "visual", "auto"], "description": "输出形态 §4.2；fail loud" }
            },
            "required": ["path", "content"]
        }),
    }
}

pub async fn run(args: &Value, ctx: &ToolContext) -> Result<ToolResponse, ToolError> {
    // §13.3 / §18.9：intent 对所有调用统一校验，非法值 fail loud。
    // write 不产生可协商的大块内容（§4.0），所以只校验、不改输出形态。
    resolve_intent(args, &CompatConfig::default()).map_err(ToolError::bad_argument)?;
    let lang = ctx.language;
    // §17.2：文件路径先过根校验；新建时父目录在根外同样被拒。
    let path = ctx.resolve_file(required_str(args, "path")?)?;
    let content = required_str(args, "content")?;

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        tokio::fs::create_dir_all(parent).await?;
    }

    let existed = path.exists();

    // 覆盖已有文件时沿用它的编码；新建文件用 UTF-8。
    // 这样 write 一个 GBK 文件不会把它悄悄转成 UTF-8。
    let (encoding, snapshot, original) = if existed {
        match tokio::fs::read(&path).await {
            Ok(old_bytes) => {
                let enc = crate::encoding::FileEncoding::detect(&old_bytes);
                let (old_text, _) = enc.decode(&old_bytes);
                (enc, ctx.snapshots.snapshot(&path, &old_text).ok(), old_text)
            }
            Err(_) => (
                crate::encoding::FileEncoding::default(),
                None,
                String::new(),
            ),
        }
    } else {
        (
            crate::encoding::FileEncoding::default(),
            None,
            String::new(),
        )
    };

    // 目标编码无法表示新内容时明确报错，不静默替换成问号。
    let out_bytes = encoding
        .encode(content)
        .map_err(|e| ToolError::failed(format!("{} 写入失败：{e}", path.display())))?;
    tokio::fs::write(&path, &out_bytes).await?;

    let lines = content.lines().count();
    // §5.7：改动后的事实，不含「你该跑什么」的指令；diff 超长时截断。
    let diff = unified_diff(&original, content, &display_path(&path, &ctx.cwd));
    let diff_truncated = truncate_head(&diff, TruncationOptions::new(200, 40_000));
    let stats = diff_stats(&original, content);
    let file_facts = FileFacts {
        lines,
        bytes: out_bytes.len(),
        encoding: encoding.name(),
        eol: detect_eol(&original),
    };
    let nearby = nearby_build_files(path.parent().unwrap_or_else(|| Path::new(".")));
    let facts = change_facts(
        &display_path(&path, &ctx.cwd),
        stats,
        file_facts,
        nearby,
        &diff_truncated.content,
    );
    let snapshot_meta = snapshot
        .as_ref()
        .map(|s| json!(s.path.display().to_string()))
        .unwrap_or(json!(null));

    let verb = t(lang, if existed { "已覆盖" } else { "已创建" });
    let mut body = tformat(
        lang,
        "{} {}（{} 行 / {} 字节）",
        &[
            verb.into_owned(),
            path.display().to_string(),
            lines.to_string(),
            content.len().to_string(),
        ],
    );
    if let Some(s) = &snapshot {
        body.push_str(&tformat(
            lang,
            "\n旧内容已快照：{}（需要回滚时读它再写回）",
            &[s.path.display().to_string()],
        ));
    }

    Ok(ToolResponse::text(body).with_meta(json!({
        "path": path.display().to_string(),
        "bytes": content.len(),
        "lines": lines,
        "created": !existed,
        "snapshot": snapshot_meta,
        "encoding": encoding.name(),
        "facts": facts,
        "diffTruncated": diff_truncated.truncated,
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
        assert_eq!(resp.meta["facts"]["changed"]["added"], json!(1));
        assert_eq!(resp.meta["facts"]["file"]["lines"], json!(1));
        let read_back = std::fs::read_to_string(dir.join("a/b/c.txt")).expect("read back");
        assert_eq!(read_back, "hello\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// §5.7：新建文件的 facts 含 diff 统计、file 事实与同目录构建描述文件。
    #[tokio::test]
    async fn write_facts_include_diff_file_and_nearby_build_files() {
        let dir = std::env::temp_dir().join(format!("tg-write-facts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"x\"\n").expect("write");
        let ctx = ToolContext::new(&dir).expect("ctx");
        let resp = run(
            &json!({ "path": "a.txt", "content": "hello\nworld\n" }),
            &ctx,
        )
        .await
        .expect("write ok");
        let facts = &resp.meta["facts"];
        assert_eq!(facts["changed"]["path"], json!("<workspace>/a.txt"));
        assert_eq!(facts["changed"]["added"], json!(2));
        assert_eq!(facts["changed"]["removed"], json!(0));
        assert_eq!(facts["file"]["lines"], json!(2));
        assert_eq!(facts["nearby"], json!(["Cargo.toml"]));
        let diff = facts["diff"].as_str().unwrap();
        assert!(diff.contains("+hello"));
        assert!(
            !diff.contains("/Users/"),
            "diff label 不应暴露绝对路径：{diff}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// §5.7：覆盖已有文件时 changed 同时给出 added / removed。
    #[tokio::test]
    async fn overwrite_reports_added_and_removed() {
        let dir = std::env::temp_dir().join(format!("tg-write-overwrite-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let ctx = ToolContext::new(&dir).expect("ctx");
        run(&json!({ "path": "a.txt", "content": "a\nb\n" }), &ctx)
            .await
            .expect("first write");
        let resp = run(&json!({ "path": "a.txt", "content": "a\nc\n" }), &ctx)
            .await
            .expect("second write");
        assert_eq!(resp.meta["created"], json!(false));
        assert_eq!(resp.meta["facts"]["changed"]["added"], json!(1));
        assert_eq!(resp.meta["facts"]["changed"]["removed"], json!(1));
        assert_eq!(resp.meta["facts"]["file"]["lines"], json!(2));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn raw_intent_keeps_v1_body_unchanged() {
        let dir = std::env::temp_dir().join(format!("tg-write-intent-{}", std::process::id()));
        let ctx = ToolContext::new(&dir).expect("ctx");
        let resp = run(
            &json!({ "path": "a.txt", "content": "hi", "intent": "raw" }),
            &ctx,
        )
        .await
        .expect("write ok");
        let crate::tools::Content::Text(body) = &resp.content[0] else {
            panic!("应为文本")
        };
        assert!(body.contains("a.txt"));
        assert_eq!(resp.meta["created"], json!(true));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// §17.2：write 新建时校验父目录，根外路径返回 ISOLATION_REFUSED 且不落盘。
    #[tokio::test]
    async fn path_outside_allowed_roots_is_refused() {
        let dir = std::env::temp_dir().join(format!("tg-write-guard-{}", std::process::id()));
        let ctx = ToolContext::new(&dir).expect("ctx");
        let outside = dir
            .parent()
            .expect("parent")
            .join(format!("tg-write-escape-{}.txt", std::process::id()));
        let err = run(
            &json!({ "path": outside.to_str().unwrap(), "content": "x" }),
            &ctx,
        )
        .await
        .unwrap_err();
        assert_eq!(crate::error::govern_error(&err).code, "ISOLATION_REFUSED");
        assert!(!outside.exists(), "根外文件不得被创建");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn unknown_intent_fails_loud_with_next_steps() {
        let dir = std::env::temp_dir().join(format!("tg-write-bad-{}", std::process::id()));
        let ctx = ToolContext::new(&dir).expect("ctx");
        let err = run(
            &json!({ "path": "a.txt", "content": "hi", "intent": "bogus" }),
            &ctx,
        )
        .await
        .unwrap_err();
        let dbg = format!("{err:?}");
        assert!(dbg.contains("next_steps"));
        assert!(!dbg.contains("next_steps: []"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
