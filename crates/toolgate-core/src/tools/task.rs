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
        description: "启动、查看、终止后台长任务。action=start 启动并立即返回 task_id；\
                      action=read 读累积输出（尾部截断）；action=kill 终止；action=list 列出全部任务。\
                      一次性命令用 bash，不要用 task。",
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
                "task_id": { "type": "string", "description": "read / kill 时必填" }
            },
            "required": ["action"]
        }),
    }
}

pub async fn run(args: &Value, ctx: &ToolContext) -> Result<ToolResponse, ToolError> {
    match required_str(args, "action")? {
        "start" => start(args, ctx).await,
        "read" => read_output(args, ctx),
        "kill" => kill(args).await,
        "list" => list(),
        other => Err(ToolError::BadArgument(format!(
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
        .map_err(|e| ToolError::Failed(format!("无法启动 {binary}：{e}")))?;

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
        if let Ok(status) = guard.wait().await {
            if let Ok(mut e) = exit_for_wait.lock() {
                *e = status.code();
            }
        }
    });

    let task_id = format!("task_{pid}_{}", Instant::now().elapsed().as_nanos());
    let label = format!("{binary} {}", argv.join(" ")).trim().to_owned();

    registry()
        .lock()
        .map_err(|_| ToolError::Failed("任务表锁失效".to_owned()))?
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

    Ok(ToolResponse::text(format!(
        "已启动后台任务 {task_id}：{label}（pid {pid}）。用 action=read 查看输出，action=kill 终止。"
    ))
    .with_meta(json!({ "taskId": task_id, "pid": pid, "label": label })))
}

fn read_output(args: &Value, ctx: &ToolContext) -> Result<ToolResponse, ToolError> {
    let task_id = required_str(args, "task_id")?;
    let reg = registry()
        .lock()
        .map_err(|_| ToolError::Failed("任务表锁失效".to_owned()))?;
    let job = reg
        .get(task_id)
        .ok_or_else(|| ToolError::Failed(format!("没有这个任务：{task_id}")))?;

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
        body.push_str(&format!(
            "\n[输出已截断：共 {}，仅展示尾部 {} 行。{}]",
            format_size(truncation.total_bytes),
            truncation.output_lines,
            spill.hint()
        ));
        spill_meta = json!({ "path": spill.path.display().to_string(), "lines": spill.lines });
    }
    if body.trim().is_empty() {
        body.push_str("(暂无输出)");
    }

    Ok(ToolResponse::text(body).with_meta(json!({
        "taskId": task_id,
        "label": job.label,
        "pid": job.pid,
        "running": exit.is_none(),
        "exitCode": exit,
        "elapsedSecs": elapsed,
        "truncated": truncation.truncated,
        "spill": spill_meta,
    })))
}

async fn kill(args: &Value) -> Result<ToolResponse, ToolError> {
    let task_id = required_str(args, "task_id")?;
    let child = {
        let reg = registry()
            .lock()
            .map_err(|_| ToolError::Failed("任务表锁失效".to_owned()))?;
        reg.get(task_id)
            .ok_or_else(|| ToolError::Failed(format!("没有这个任务：{task_id}")))?
            .child
            .clone()
    };
    let mut guard = child.lock().await;
    guard
        .kill()
        .await
        .map_err(|e| ToolError::Failed(format!("终止失败：{e}")))?;
    Ok(ToolResponse::text(format!("已请求终止任务 {task_id}。"))
        .with_meta(json!({ "taskId": task_id })))
}

fn list() -> Result<ToolResponse, ToolError> {
    let reg = registry()
        .lock()
        .map_err(|_| ToolError::Failed("任务表锁失效".to_owned()))?;
    if reg.is_empty() {
        return Ok(ToolResponse::text("当前没有后台任务。"));
    }
    let mut out = String::new();
    let mut items = Vec::new();
    for (id, job) in reg.iter() {
        let exit = job.exit.lock().ok().and_then(|g| *g);
        let state = match exit {
            Some(code) => format!("已退出（{code}）"),
            None => "运行中".to_owned(),
        };
        out.push_str(&format!(
            "{id}  pid={}  {}  {} 已运行 {}s\n",
            job.pid,
            state,
            job.label,
            job.started.elapsed().as_secs()
        ));
        items.push(json!({ "taskId": id, "pid": job.pid, "exitCode": exit, "label": job.label }));
    }
    Ok(ToolResponse::text(out).with_meta(json!({ "tasks": items })))
}

#[cfg(test)]
mod tests {
    use super::*;

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
