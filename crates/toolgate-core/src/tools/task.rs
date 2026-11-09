//! task：后台长任务的启动、查看与终止。
//!
//! 与 bash 的分工：bash 是一次性、等结果的命令；task 用于开发服务器、
//! 完整测试套件这类跑起来就不回头的进程。
//!
//! # 关于 checkpoint
//!
//! 断点续传不在这层实现。工具层只保证「进程可寻址、输出可增量读取」，
//! 检查点属于会话层（记录已完成的步骤与未完成的意图）。把两者混在
//! 一起会让工具依赖会话状态，破坏可测试性。

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};

use super::{ToolContext, ToolResponse, ToolSpec, optional_str, required_str};
use crate::ToolError;
use crate::bash_path::{estimate_tokens, intent_tag};
use crate::decision::Intent;
use crate::locale::{Language, t, tf, tformat};
use crate::notice::budgeted_notices;
use crate::task_path::{TaskMode, plan_task, resolve_task_intent, structured_facts};
use crate::truncate::{format_size, truncate_tail};

/// 单任务累积输出上限，超出后丢弃最早的内容。
const MAX_BUFFER_BYTES: usize = 4 * 1024 * 1024;

struct Job {
    pid: u32,
    label: String,
    started: Instant,
    output: std::sync::Arc<Mutex<Vec<u8>>>,
    exit: std::sync::Arc<Mutex<Option<i32>>>,
    child: std::sync::Arc<tokio::sync::Mutex<Child>>,
}

fn registry() -> &'static Mutex<HashMap<String, Job>> {
    static REG: OnceLock<Mutex<HashMap<String, Job>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: "task",
        description: "能做什么：启动、查看、终止后台长任务；action=read 返回累积输出（尾部截断并落盘）与进程状态，intent 协商仅作用于 read。 \
                      不能做什么：不做任务级断点与完成度判断（归调用方）；不判定卡住/崩溃/报错循环（decide 槽位属可选层，未接入时按 §3.8 降级并告知）；不返回图片。 \
                      什么时候用别的工具：一次性等结果的命令用 bash；按内容查找用 search；精确看图用 read。",
        description_en: "Can: start, inspect and kill background long jobs; action=read returns accumulated output (tail-truncated and spilled) plus process state; intent applies only to read. Cannot: decide task-level checkpoints or completion (caller's job), judge stuck / crash / error loops (decide slot is optional, unavailable degrades per §3.8), or return images. Use instead: bash for one-shot commands, search to find content, read for precise images.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["start", "read", "kill", "list"],
                    "description": "要执行的动作"
                },
                "binary": { "type": "string", "description": "start 时必填：可执行程序名" },
                "args": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "start 时的参数数组"
                },
                "cwd": { "type": "string", "description": "start 时的工作目录" },
                "task_id": { "type": "string", "description": "read / kill 时必填" },
                "intent": {
                    "type": "string",
                    "enum": ["raw", "summary", "structured", "visual", "auto"],
                    "description": "协商层输出形态，仅作用于 action=read：raw 累积输出加落盘指针（默认）/ summary 状态与增量摘要 / structured 结构化事实 JSON / auto 小于 500 token 给原文，500–2000 token 只判状态，超过 2000 token 再做增量摘要 / visual 不适用"
                }
            },
            "required": ["action"]
        }),
    }
}

pub async fn run(args: &Value, ctx: &ToolContext) -> Result<ToolResponse, ToolError> {
    // intent 对所有 action 统一校验（§18.9 结构错误 fail loud）；
    // 协商只在 action=read 生效（§5.5 L2）。
    let intent = resolve_task_intent(args).map_err(ToolError::bad_argument)?;
    let lang = ctx.language;
    match required_str(args, "action")? {
        "start" => start(args, ctx).await,
        "read" => read_output(args, ctx, intent),
        "kill" => kill(args, lang).await,
        "list" => list(lang),
        other => Err(ToolError::bad_argument(format!(
            "action 只能是 start / read / kill / list，收到 {other}"
        ))),
    }
}

async fn start(args: &Value, ctx: &ToolContext) -> Result<ToolResponse, ToolError> {
    let binary = required_str(args, "binary")?;
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

    let mut child = Command::new(binary)
        .args(&argv)
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(false)
        .spawn()
        .map_err(|e| ToolError::failed(format!("无法启动 {binary}：{e}")))?;

    let pid = child.id().unwrap_or(0);
    let output = std::sync::Arc::new(Mutex::new(Vec::<u8>::new()));
    let exit = std::sync::Arc::new(Mutex::new(None::<i32>));

    if let Some(mut stdout) = child.stdout.take() {
        let buf = output.clone();
        tokio::spawn(async move {
            let mut chunk = [0u8; 8192];
            while let Ok(n) = stdout.read(&mut chunk).await {
                if n == 0 {
                    break;
                }
                if let Ok(mut g) = buf.lock() {
                    g.extend_from_slice(&chunk[..n]);
                    if g.len() > MAX_BUFFER_BYTES {
                        let drop_n = g.len() - MAX_BUFFER_BYTES;
                        g.drain(..drop_n);
                    }
                }
            }
        });
    }
    if let Some(mut stderr) = child.stderr.take() {
        let buf = output.clone();
        tokio::spawn(async move {
            let mut chunk = [0u8; 8192];
            while let Ok(n) = stderr.read(&mut chunk).await {
                if n == 0 {
                    break;
                }
                if let Ok(mut g) = buf.lock() {
                    g.extend_from_slice(&chunk[..n]);
                }
            }
        });
    }

    let child = std::sync::Arc::new(tokio::sync::Mutex::new(child));
    let child_for_wait = child.clone();
    let exit_for_wait = exit.clone();
    tokio::spawn(async move {
        let mut guard = child_for_wait.lock().await;
        if let Ok(status) = guard.wait().await
            && let Ok(mut e) = exit_for_wait.lock()
        {
            *e = status.code();
        }
    });

    let task_id = format!("task_{pid}_{}", Instant::now().elapsed().as_nanos());
    let label = format!("{binary} {}", argv.join(" ")).trim().to_owned();

    registry()
        .lock()
        .map_err(|_| ToolError::failed("任务表锁失效".to_owned()))?
        .insert(
            task_id.clone(),
            Job {
                pid,
                label: label.clone(),
                started: Instant::now(),
                output,
                exit,
                child,
            },
        );

    Ok(ToolResponse::text(tf(
        ctx.language,
        "已启动后台任务 {task_id}：{label}（pid {pid}）。用 action=read 查看输出，action=kill 终止。",
        &[
            ("task_id", task_id.clone()),
            ("label", label.clone()),
            ("pid", pid.to_string()),
        ],
    ))
    .with_meta(json!({ "taskId": task_id, "pid": pid, "label": label })))
}

fn read_output(args: &Value, ctx: &ToolContext, intent: Intent) -> Result<ToolResponse, ToolError> {
    let task_id = required_str(args, "task_id")?;
    let reg = registry()
        .lock()
        .map_err(|_| ToolError::failed("任务表锁失效".to_owned()))?;
    let job = reg
        .get(task_id)
        .ok_or_else(|| ToolError::failed(format!("没有这个任务：{task_id}")))?;

    let raw = job
        .output
        .lock()
        .map(|g| String::from_utf8_lossy(&g).into_owned())
        .unwrap_or_default();
    let exit = job.exit.lock().ok().and_then(|g| *g);
    let elapsed = job.started.elapsed().as_secs();

    let truncation = truncate_tail(&raw, ctx.limits);
    let mut body = truncation.content.clone();
    let mut spill_meta = Value::Null;
    if truncation.truncated {
        let spill = ctx.spill.persist("task", &raw)?;
        body.push_str(&tformat(
            ctx.language,
            "\n[输出已截断：共 {}，仅展示尾部 {} 行。{}]",
            &[
                format_size(truncation.total_bytes),
                truncation.output_lines.to_string(),
                spill.hint(ctx.language),
            ],
        ));
        spill_meta = json!({ "path": spill.path.display().to_string(), "lines": spill.lines });
    }
    if body.trim().is_empty() {
        body.push_str(&t(ctx.language, "(暂无输出)"));
    }

    let spill_path = spill_meta["path"].as_str().map(str::to_owned);
    let plan = plan_task(intent, estimate_tokens(&body)).map_err(ToolError::bad_argument)?;
    let mut meta = json!({
        "taskId": task_id,
        "label": job.label,
        "pid": job.pid,
        "running": exit.is_none(),
        "exitCode": exit,
        "elapsedSecs": elapsed,
        "outputBytes": raw.len(),
        "truncated": truncation.truncated,
        "spill": spill_meta,
        "intent": intent_tag(intent),
    });
    let mut content = body;
    match plan.mode {
        TaskMode::Raw => {}
        TaskMode::StateDegraded | TaskMode::SummaryDegraded => {
            content.push('\n');
            content.push_str(&state_line(
                ctx.language,
                exit,
                elapsed,
                raw.len(),
                truncation.truncated,
            ));
            content.push('\n');
            let ch = budgeted_notices(plan.notices.clone());
            content.push_str(&ch.text);
            meta["notices"] = ch.structured;
            meta["noticeOverflow"] = json!(ch.overflow);
        }
        TaskMode::Structured => {
            let facts = structured_facts(
                exit,
                elapsed,
                raw.len(),
                truncation.truncated,
                spill_path.as_deref(),
            );
            content = facts.to_string();
            meta["facts"] = facts;
        }
    }
    Ok(ToolResponse::text(content).with_meta(meta))
}

/// 机械可观测的进程状态行（§4.7：只给事实，不给判断）。
fn state_line(
    lang: Language,
    exit: Option<i32>,
    elapsed: u64,
    bytes: usize,
    truncated: bool,
) -> String {
    let state = if exit.is_some() { "exited" } else { "running" };
    let code = exit
        .map(|c| c.to_string())
        .unwrap_or_else(|| "null".to_owned());
    tf(
        lang,
        "[状态] processState={state} exitCode={code} elapsedSecs={elapsed} outputBytes={bytes} truncated={truncated}",
        &[
            ("state", state.to_owned()),
            ("code", code),
            ("elapsed", elapsed.to_string()),
            ("bytes", bytes.to_string()),
            ("truncated", truncated.to_string()),
        ],
    )
}

async fn kill(args: &Value, lang: Language) -> Result<ToolResponse, ToolError> {
    let task_id = required_str(args, "task_id")?;
    let child = {
        let reg = registry()
            .lock()
            .map_err(|_| ToolError::failed("任务表锁失效".to_owned()))?;
        reg.get(task_id)
            .ok_or_else(|| ToolError::failed(format!("没有这个任务：{task_id}")))?
            .child
            .clone()
    };
    let mut guard = child.lock().await;
    guard
        .kill()
        .await
        .map_err(|e| ToolError::failed(format!("终止失败：{e}")))?;
    Ok(ToolResponse::text(tf(
        lang,
        "已请求终止任务 {task_id}。",
        &[("task_id", task_id.to_owned())],
    ))
    .with_meta(json!({ "taskId": task_id })))
}

fn list(lang: Language) -> Result<ToolResponse, ToolError> {
    let reg = registry()
        .lock()
        .map_err(|_| ToolError::failed("任务表锁失效".to_owned()))?;
    if reg.is_empty() {
        return Ok(ToolResponse::text(t(lang, "当前没有后台任务。")));
    }
    let mut out = String::new();
    let mut items = Vec::new();
    for (id, job) in reg.iter() {
        let exit = job.exit.lock().ok().and_then(|g| *g);
        let state = match exit {
            Some(code) => tf(lang, "已退出（{code}）", &[("code", code.to_string())]),
            None => t(lang, "运行中").into_owned(),
        };
        out.push_str(&tf(
            lang,
            "{id}  pid={pid}  {state}  {label} 已运行 {secs}s\n",
            &[
                ("id", id.to_owned()),
                ("pid", job.pid.to_string()),
                ("state", state),
                ("label", job.label.clone()),
                ("secs", job.started.elapsed().as_secs().to_string()),
            ],
        ));
        items.push(json!({ "taskId": id, "pid": job.pid, "exitCode": exit, "label": job.label }));
    }
    Ok(ToolResponse::text(out).with_meta(json!({ "tasks": items })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task_ctx(suffix: &str) -> (ToolContext, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("tg-task-{}-{suffix}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let ctx = ToolContext::new(&dir).expect("ctx");
        (ctx, dir)
    }

    async fn seed_output(ctx: &ToolContext, bytes: usize) -> String {
        let long = "x".repeat(bytes);
        let resp = run(
            &json!({ "action": "start", "binary": "echo", "args": [long] }),
            ctx,
        )
        .await
        .expect("start ok");
        let id = resp.meta["taskId"].as_str().expect("taskId").to_owned();
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        id
    }

    fn body_of(resp: &ToolResponse) -> &str {
        let crate::tools::Content::Text(body) = &resp.content[0] else {
            panic!("应为文本")
        };
        body
    }

    #[tokio::test]
    async fn raw_intent_keeps_v1_behavior() {
        let (ctx, dir) = task_ctx("raw");
        let id = seed_output(&ctx, 16).await;
        let resp = run(
            &json!({ "action": "read", "task_id": id, "intent": "raw" }),
            &ctx,
        )
        .await
        .expect("read ok");
        assert!(body_of(&resp).contains("xxxx"));
        assert!(!body_of(&resp).contains("[SLOT_UNAVAILABLE]"));
        assert_eq!(resp.meta["intent"], json!("raw"));
        assert!(resp.meta.get("notices").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn summary_intent_degrades_with_two_notices() {
        let (ctx, dir) = task_ctx("summary");
        let id = seed_output(&ctx, 16).await;
        let resp = run(
            &json!({ "action": "read", "task_id": id, "intent": "summary" }),
            &ctx,
        )
        .await
        .expect("read ok");
        let body = body_of(&resp);
        assert!(body.contains("[SLOT_UNAVAILABLE]"));
        assert!(body.contains("slot=decide"));
        assert!(body.contains("slot=summarize"));
        assert!(body.contains("[状态]"));
        assert_eq!(resp.meta["notices"].as_array().unwrap().len(), 2);
        assert_eq!(resp.meta["intent"], json!("summary"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn auto_small_output_stays_raw() {
        let (ctx, dir) = task_ctx("auto-small");
        let id = seed_output(&ctx, 16).await;
        let resp = run(
            &json!({ "action": "read", "task_id": id, "intent": "auto" }),
            &ctx,
        )
        .await
        .expect("read ok");
        assert!(body_of(&resp).contains("xxxx"));
        assert!(!body_of(&resp).contains("[SLOT_UNAVAILABLE]"));
        assert!(resp.meta.get("notices").is_none());
        assert_eq!(resp.meta["intent"], json!("auto"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn auto_mid_output_reports_only_decide() {
        let (ctx, dir) = task_ctx("auto-mid");
        let id = seed_output(&ctx, 3000).await;
        let resp = run(
            &json!({ "action": "read", "task_id": id, "intent": "auto" }),
            &ctx,
        )
        .await
        .expect("read ok");
        let body = body_of(&resp);
        assert!(body.contains("slot=decide"));
        assert!(!body.contains("slot=summarize"));
        assert_eq!(resp.meta["notices"].as_array().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn auto_large_output_reports_both_slots() {
        let (ctx, dir) = task_ctx("auto-large");
        let id = seed_output(&ctx, 9001).await;
        let resp = run(
            &json!({ "action": "read", "task_id": id, "intent": "auto" }),
            &ctx,
        )
        .await
        .expect("read ok");
        let body = body_of(&resp);
        assert!(body.contains("slot=decide"));
        assert!(body.contains("slot=summarize"));
        assert_eq!(resp.meta["notices"].as_array().unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn structured_intent_returns_facts_json() {
        let (ctx, dir) = task_ctx("structured");
        let id = seed_output(&ctx, 16).await;
        let resp = run(
            &json!({ "action": "read", "task_id": id, "intent": "structured" }),
            &ctx,
        )
        .await
        .expect("read ok");
        let facts: Value = serde_json::from_str(body_of(&resp)).expect("content 应是 JSON");
        assert_eq!(facts["state"], json!("unknown"));
        assert_eq!(facts["progress"], json!("unknown"));
        assert!(facts["summary"].is_null());
        assert_eq!(resp.meta["facts"]["state"], json!("unknown"));
        assert_eq!(resp.meta["intent"], json!("structured"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn visual_intent_is_rejected_actionably() {
        let (ctx, dir) = task_ctx("visual");
        let id = seed_output(&ctx, 16).await;
        let err = run(
            &json!({ "action": "read", "task_id": id, "intent": "visual" }),
            &ctx,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("read"));
        assert!(err.to_string().contains("raw"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn invalid_intent_fails_loud_on_any_action() {
        let (ctx, dir) = task_ctx("bad-intent");
        let err = run(&json!({ "action": "list", "intent": "bogus" }), &ctx)
            .await
            .unwrap_err();
        let dbg = format!("{err:?}");
        assert!(dbg.contains("next_steps"));
        assert!(!dbg.contains("next_steps: []"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn starts_lists_and_kills_a_job() {
        let dir = std::env::temp_dir().join(format!("tg-task-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let ctx = ToolContext::new(&dir).expect("ctx");

        let resp = run(
            &json!({ "action": "start", "binary": "sleep", "args": ["30"] }),
            &ctx,
        )
        .await
        .expect("start ok");
        let id = resp.meta["taskId"].as_str().expect("taskId").to_owned();

        let listed = run(&json!({ "action": "list" }), &ctx)
            .await
            .expect("list ok");
        assert!(
            listed
                .content
                .iter()
                .any(|c| matches!(c, crate::tools::Content::Text(t) if t.contains(&id)))
        );

        run(&json!({ "action": "kill", "task_id": id }), &ctx)
            .await
            .expect("kill ok");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn unknown_task_is_error() {
        let dir = std::env::temp_dir().join(format!("tg-task2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let ctx = ToolContext::new(&dir).expect("ctx");
        let err = run(&json!({ "action": "read", "task_id": "nope" }), &ctx).await;
        assert!(err.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
