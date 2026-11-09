//! bash：受控的系统命令执行。
//!
//! 刻意**不接受** shell 字符串，只接受 程序名 + 参数数组。理由：
//! 字符串形式必然要经过一层 shell 解析，引号、通配符、变量展开都会
//! 变成注入面；数组形式直接走 execve，没有解析层。
//!
//! 输出治理：尾部截断（错误栈与最终结果在尾部），完整输出落盘。

use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use super::{ToolContext, ToolResponse, ToolSpec, optional_str, optional_usize, required_str};
use crate::ToolError;
use crate::bash_path::{
    BashMode, estimate_tokens, intent_tag, plan_bash, resolve_bash_intent, structured_facts,
};
use crate::locale::{t, tformat};
use crate::notice::budgeted_notices;
use crate::truncate::{format_size, truncate_tail};

/// 默认超时：5 分钟。
const DEFAULT_TIMEOUT_SECS: u64 = 300;
/// 输出累积上限，超过后停止读取，避免一条命令吃光内存。
const MAX_CAPTURE_BYTES: usize = 8 * 1024 * 1024;

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: "bash",
        description: "能做什么：执行系统命令，只接受程序名 + 参数数组（无 shell 解析层）；输出超过 2000 行 / 50KB 保留尾部并落盘。 \
                      不能做什么：不接受 shell 字符串、管道与重定向；不返回图片；不做长任务托管。 \
                      什么时候用别的工具：长任务用 task；按内容查找用 search；改文件用 edit / write；看图用 read。",
        description_en: "Can: run system commands via program name + args array (no shell parsing); tail-truncates past 2000 lines / 50KB and spills. Cannot: accept shell strings, pipes or redirects, return images, or host long jobs. Use instead: task for long jobs, search to find content, edit / write to modify, read for images.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "binary": { "type": "string", "description": "可执行程序名或绝对路径，例如 git" },
                "args": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "参数数组，例如 [\"status\", \"--short\"]"
                },
                "cwd": { "type": "string", "description": "工作目录，默认用上下文的工作目录" },
                "timeout_secs": {
                    "type": "integer",
                    "description": "超时秒数，默认 300",
                    "minimum": 1
                },
                "intent": {
                    "type": "string",
                    "enum": ["raw", "summary", "structured", "visual", "auto"],
                    "description": "协商层输出形态：raw 截断原文加落盘指针（默认）/ summary 失败摘要 / structured 结构化事实 JSON / visual 不适用（看图请用 read）/ auto 输出小于 500 token 给原文，否则走 summary"
                }
            },
            "required": ["binary"]
        }),
    }
}

pub async fn run(args: &Value, ctx: &ToolContext) -> Result<ToolResponse, ToolError> {
    let lang = ctx.language;
    let binary = required_str(args, "binary")?;
    reject_shell(binary)?;

    let argv: Vec<String> = args
        .get("args")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|v| v.as_str().unwrap_or_default().to_owned())
                .collect()
        })
        .unwrap_or_default();

    let cwd = optional_str(args, "cwd")
        .map(|p| ctx.resolve(p))
        .unwrap_or_else(|| ctx.cwd.clone());
    let timeout = Duration::from_secs(
        optional_usize(args, "timeout_secs").unwrap_or(DEFAULT_TIMEOUT_SECS as usize) as u64,
    );

    let mut child = Command::new(binary)
        .args(&argv)
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| ToolError::failed(format!("无法启动 {binary}：{e}")))?;

    let mut stdout_pipe = child.stdout.take().expect("stdout piped");
    let mut stderr_pipe = child.stderr.take().expect("stderr piped");

    let collect = async {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut out).await;
        let _ = stderr_pipe.read_to_end(&mut err).await;
        (out, err)
    };

    let status = match tokio::time::timeout(timeout, async {
        let (out, err) = collect.await;
        let status = child.wait().await;
        (out, err, status)
    })
    .await
    {
        Ok((out, err, status)) => {
            let status = status.map_err(|e| ToolError::failed(format!("等待进程失败：{e}")))?;
            (out, err, status)
        }
        Err(_) => {
            let _ = child.kill().await;
            return Err(ToolError::failed(format!(
                "命令超时（{} 秒）已被终止：{binary} {}",
                timeout.as_secs(),
                argv.join(" ")
            )));
        }
    };

    let (stdout, stderr, status) = status;
    let combined = merge_output(&stdout, &stderr);
    let truncation = truncate_tail(&combined, ctx.limits);

    let mut body = truncation.content.clone();
    let mut spill_meta = Value::Null;

    if truncation.truncated {
        let spill = ctx.spill.persist("bash", &combined)?;
        body.push_str(&tformat(
            lang,
            "\n[输出已截断：共 {} 行 / {}，仅展示尾部 {} 行。{}]",
            &[
                truncation.total_lines.to_string(),
                format_size(truncation.total_bytes),
                truncation.output_lines.to_string(),
                spill.hint(lang),
            ],
        ));
        spill_meta = json!({
            "path": spill.path.display().to_string(),
            "bytes": spill.bytes,
            "lines": spill.lines,
        });
    }

    if body.trim().is_empty() {
        body.push_str(&t(lang, "(无输出)"));
    }

    let code = status.code();
    let intent = resolve_bash_intent(args).map_err(ToolError::bad_argument)?;
    let plan = plan_bash(intent, estimate_tokens(&body)).map_err(ToolError::bad_argument)?;

    let mut meta = json!({
        "binary": binary,
        "args": argv,
        "cwd": cwd.display().to_string(),
        "exitCode": code,
        "success": status.success(),
        "truncated": truncation.truncated,
        "spill": spill_meta,
        "intent": intent_tag(intent),
    });
    let mut content = body;
    if !plan.notices.is_empty() {
        content.push('\n');
        let ch = budgeted_notices(plan.notices.clone());
        content.push_str(&ch.text);
        meta["notices"] = ch.structured;
        meta["noticeOverflow"] = json!(ch.overflow);
    }
    if plan.mode == BashMode::Structured {
        let spill_path = meta["spill"]["path"].as_str();
        let facts = structured_facts(code, spill_path);
        content = facts.to_string();
        meta["facts"] = facts;
    }
    Ok(ToolResponse::text(content).with_meta(meta))
}

/// 拒绝把 shell 解释器当作命令入口。
fn reject_shell(binary: &str) -> Result<(), ToolError> {
    let base = std::path::Path::new(binary)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(binary);
    const SHELLS: &[&str] = &["sh", "bash", "zsh", "fish", "dash", "ksh", "csh", "tcsh"];
    if SHELLS.contains(&base) {
        return Err(ToolError::bad_argument(format!(
            "不接受通过 {base} 执行命令字符串。请直接把要运行的程序名放进 binary，\
             参数放进 args 数组——这样没有 shell 解析层，也就没有注入面。"
        )));
    }
    Ok(())
}

/// 合并 stdout 与 stderr，保持到达顺序无关的稳定结构：先 stdout 后 stderr。
fn merge_output(stdout: &[u8], stderr: &[u8]) -> String {
    let mut s = String::from_utf8_lossy(stdout).into_owned();
    if !stderr.is_empty() {
        if !s.is_empty() && !s.ends_with('\n') {
            s.push('\n');
        }
        s.push_str(&String::from_utf8_lossy(stderr));
    }
    if s.len() > MAX_CAPTURE_BYTES {
        s.truncate(MAX_CAPTURE_BYTES);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_shell_entrypoints() {
        assert!(reject_shell("bash").is_err());
        assert!(reject_shell("/bin/sh").is_err());
        assert!(reject_shell("git").is_ok());
        assert!(reject_shell("/usr/bin/env").is_ok());
    }

    #[tokio::test]
    async fn runs_echo_and_captures_stdout() {
        let dir = std::env::temp_dir().join(format!("tg-bash-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let ctx = ToolContext::new(&dir).expect("ctx");
        let resp = run(&json!({ "binary": "echo", "args": ["hello"] }), &ctx)
            .await
            .expect("run ok");
        let crate::tools::Content::Text(body) = &resp.content[0] else {
            panic!("应为文本")
        };
        assert!(body.contains("hello"));
        assert_eq!(resp.meta["success"], json!(true));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn bash_ctx(suffix: &str) -> (ToolContext, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("tg-bash-{}-{suffix}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let ctx = ToolContext::new(&dir).expect("ctx");
        (ctx, dir)
    }

    #[tokio::test]
    async fn raw_intent_matches_v1_behavior() {
        let (ctx, dir) = bash_ctx("raw");
        let resp = run(
            &json!({ "binary": "echo", "args": ["hello"], "intent": "raw" }),
            &ctx,
        )
        .await
        .expect("run ok");
        let crate::tools::Content::Text(body) = &resp.content[0] else {
            panic!("应为文本")
        };
        assert!(body.contains("hello"));
        assert!(!body.contains("[SLOT_UNAVAILABLE]"));
        assert!(resp.meta.get("notices").is_none());
        assert_eq!(resp.meta["intent"], json!("raw"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn summary_intent_degrades_with_slot_unavailable() {
        let (ctx, dir) = bash_ctx("summary");
        let resp = run(
            &json!({ "binary": "echo", "args": ["hello"], "intent": "summary" }),
            &ctx,
        )
        .await
        .expect("run ok");
        let crate::tools::Content::Text(body) = &resp.content[0] else {
            panic!("应为文本")
        };
        assert!(body.contains("[SLOT_UNAVAILABLE]"));
        assert!(body.contains("slot=decide"));
        assert!(body.contains("slot=summarize"));
        assert_eq!(resp.meta["intent"], json!("summary"));
        assert_eq!(resp.meta["notices"].as_array().unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn auto_small_output_stays_raw() {
        let (ctx, dir) = bash_ctx("auto-small");
        let resp = run(
            &json!({ "binary": "echo", "args": ["hello"], "intent": "auto" }),
            &ctx,
        )
        .await
        .expect("run ok");
        let crate::tools::Content::Text(body) = &resp.content[0] else {
            panic!("应为文本")
        };
        assert!(body.contains("hello"));
        assert!(!body.contains("[SLOT_UNAVAILABLE]"));
        assert!(resp.meta.get("notices").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn auto_large_output_degrades() {
        let (ctx, dir) = bash_ctx("auto-large");
        let long = "x".repeat(2400);
        let resp = run(
            &json!({ "binary": "echo", "args": [long], "intent": "auto" }),
            &ctx,
        )
        .await
        .expect("run ok");
        let crate::tools::Content::Text(body) = &resp.content[0] else {
            panic!("应为文本")
        };
        assert!(body.contains("[SLOT_UNAVAILABLE]"));
        assert_eq!(resp.meta["intent"], json!("auto"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn structured_intent_returns_facts_json() {
        let (ctx, dir) = bash_ctx("structured");
        let resp = run(
            &json!({ "binary": "echo", "args": ["hi"], "intent": "structured" }),
            &ctx,
        )
        .await
        .expect("run ok");
        let crate::tools::Content::Text(body) = &resp.content[0] else {
            panic!("应为文本")
        };
        let facts: Value = serde_json::from_str(body).expect("content 应是 JSON");
        assert_eq!(facts["exitCode"], json!(0));
        assert_eq!(facts["errorKind"], json!("unknown"));
        assert_eq!(facts["failedItems"], json!([]));
        assert_eq!(resp.meta["facts"]["errorKind"], json!("unknown"));
        assert_eq!(resp.meta["intent"], json!("structured"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn visual_intent_is_rejected_actionably() {
        let (ctx, dir) = bash_ctx("visual");
        let err = run(
            &json!({ "binary": "echo", "args": ["hi"], "intent": "visual" }),
            &ctx,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("read"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
