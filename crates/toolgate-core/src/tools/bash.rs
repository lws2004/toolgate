//! bash：受控的系统命令执行。
//!
//! 刻意**不接受** shell 字符串，只接受 程序名 + 参数数组。理由：
//! 字符串形式必然要经过一层 shell 解析，引号、通配符、变量展开都会
//! 变成注入面；数组形式直接走 execve，没有解析层。
//!
//! 输出治理：尾部截断（错误栈与最终结果在尾部），完整输出落盘。
//!
//! 执行边界（§17.2）：子进程不继承父进程环境，只注入固定白名单；
//! 真实 uid/gid 也在这里记录一次，供 MCP initialize 与 CLI status 上报。

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::process::Command;

use super::{
    ToolContext, ToolResponse, ToolSpec, gate_notices, optional_str, optional_usize, required_str,
};
use crate::ToolError;
use crate::bash_path::{
    BashDecision, BashMode, DecisionFacts, L1_QUESTION_KEYS, L1_QUESTIONS, estimate_tokens,
    intent_tag, merge_decision, plan_bash, resolve_bash_intent, structured_facts,
};
use crate::locale::{t, tformat};
use crate::notice::{Notice, SlotFallback, budgeted_notices};
use crate::slots::{ConfidenceGate, DecideInput, SummarizeInput};
use crate::truncate::{format_size, truncate_tail};

/// 默认超时：5 分钟。
const DEFAULT_TIMEOUT_SECS: u64 = 300;
/// 输出累积上限，超过后停止读取，避免一条命令吃光内存。
const MAX_CAPTURE_BYTES: usize = 8 * 1024 * 1024;

/// 子进程环境变量白名单（§17.2）：固定名单，不做配置。
///
/// 允许配置扩大等于把父进程环境重新泄漏给子进程，与白名单目的相反。
const ENV_ALLOWLIST: &[&str] = &[
    "PATH", "HOME", "LANG", "LC_ALL", "LC_CTYPE", "TERM", "TMPDIR", "USER", "LOGNAME", "SHELL",
    "PWD",
];

/// 给子进程注入白名单环境（§17.2）。
///
/// 先 `env_clear` 清掉继承来的父进程环境，再逐项透传名单内存在的变量。
/// `PWD` 不透传父进程的值：它必须是子进程自己的工作目录，否则子进程看到的是
/// 启动 toolgate 时所在的目录，而不是它真正运行的位置。
pub(crate) fn apply_env_allowlist(cmd: &mut Command, cwd: &Path) {
    cmd.env_clear();
    for key in ENV_ALLOWLIST {
        if *key == "PWD" {
            continue;
        }
        if let Some(value) = std::env::var_os(key) {
            cmd.env(key, value);
        }
    }
    cmd.env("PWD", cwd);
}

/// 进程身份（§17.2 权限越界）：启动时记录的真实 uid/gid。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessIdentity {
    /// 真实 uid；取不到时是 None——不填 0，0 会被读成 root。
    pub uid: Option<u32>,
    /// 真实 gid；取不到时是 None。
    pub gid: Option<u32>,
}

impl ProcessIdentity {
    /// 上报形态：取不到的值给 null，不编造数字（§4.6 诚实）。
    pub fn to_json(self) -> Value {
        json!({ "uid": self.uid, "gid": self.gid })
    }
}

static IDENTITY: OnceLock<ProcessIdentity> = OnceLock::new();

/// 进程级缓存的真实 uid/gid：只探测一次，之后读缓存。
///
/// 零依赖取法：调 `id -u` / `id -g` 子进程。macOS 没有 `/proc/self/status`，
/// 而 `getuid(2)` 需要 libc 依赖（§1.5 准入）。
pub fn identity() -> ProcessIdentity {
    *IDENTITY.get_or_init(|| ProcessIdentity {
        uid: read_id("-u"),
        gid: read_id("-g"),
    })
}

/// 读 `id <flag>` 的输出并解析成数字；失败返回 None（诚实上报，不猜）。
fn read_id(flag: &str) -> Option<u32> {
    let out = std::process::Command::new("id").arg(flag).output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()?.trim().parse().ok()
}

pub fn spec() -> ToolSpec {
    ToolSpec {
        name: "bash",
        description: "能做什么：执行命令——单命令 binary+args（无 shell），或 pipeline 线性管道；stdout_to/stderr_to 写文件返回路径；超 2000 行/50KB 落盘。不能做什么：不支持 shell 字符串、&&/||/; 等；不做长任务托管。什么时候用别的工具：task、search、edit/write、read。",
        description_en: "Can: run commands via binary+args (no shell), or a linear pipeline array (== a | b | c, no shell parsing); stdout_to/stderr_to redirect to file. Tail-truncates past 2000 lines/50KB. Cannot: shell strings, &&/||/;, globbing, var expansion, host long jobs. Use instead: task, search, edit/write, read.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "binary": { "type": "string" },
                "args": { "type": "array", "items": { "type": "string" } },
                "pipeline": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "binary": { "type": "string" },
                            "args": { "type": "array", "items": { "type": "string" } }
                        },
                        "required": ["binary"]
                    }
                },
                "stdout_to": { "type": "string" },
                "stderr_to": { "type": "string" },
                "cwd": { "type": "string" },
                "timeout_secs": { "type": "integer", "minimum": 1 },
                "intent": { "type": "string", "enum": ["raw", "summary", "structured", "visual", "auto"] }
            }
        }),
    }
}

pub async fn run(args: &Value, ctx: &ToolContext) -> Result<ToolResponse, ToolError> {
    let lang = ctx.language;

    // §5.1：结构化线性 pipeline（可选）或单命令 binary+args。每段仍是 execve，
    // 段间用 OS 管道连接——没有 shell 解析层，也就没有 shell 注入面。
    let segments: Vec<(String, Vec<String>)> = match args.get("pipeline").and_then(Value::as_array)
    {
        Some(p) => {
            if p.is_empty() {
                return Err(ToolError::bad_argument("pipeline 不能为空数组".to_owned()));
            }
            let mut segs = Vec::with_capacity(p.len());
            for (i, seg) in p.iter().enumerate() {
                let bin = seg.get("binary").and_then(Value::as_str).ok_or_else(|| {
                    ToolError::bad_argument(format!("pipeline 第 {} 段缺少 binary", i + 1))
                })?;
                reject_shell(bin)?;
                let av: Vec<String> = seg
                    .get("args")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .map(|v| v.as_str().unwrap_or_default().to_owned())
                            .collect()
                    })
                    .unwrap_or_default();
                segs.push((bin.to_owned(), av));
            }
            segs
        }
        None => {
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
            vec![(binary.to_owned(), argv)]
        }
    };
    // meta 与错误消息仍用第一段标识，单命令形态与旧行为一致。
    let binary = segments[0].0.clone();
    let argv = segments[0].1.clone();

    let cwd = optional_str(args, "cwd")
        .map(|p| ctx.resolve(p))
        .unwrap_or_else(|| ctx.cwd.clone());
    let timeout = Duration::from_secs(
        optional_usize(args, "timeout_secs").unwrap_or(DEFAULT_TIMEOUT_SECS as usize) as u64,
    );
    let stdout_to = optional_str(args, "stdout_to").map(|p| ctx.resolve(p));
    let stderr_to = optional_str(args, "stderr_to").map(|p| ctx.resolve(p));

    let (mut stdout, mut stderr, status) = exec_segments(&segments, &cwd, timeout).await?;

    // §5.1：stdout_to / stderr_to 把最终输出重定向到文件，返回路径而非内容。
    let mut redirected: Vec<Value> = Vec::new();
    if let Some(path) = &stdout_to {
        tokio::fs::write(path, &stdout).await.map_err(|e| {
            ToolError::failed_with(
                crate::error::ErrorCode::IoError,
                format!("写 stdout_to 失败：{e}"),
            )
        })?;
        redirected.push(
            json!({"stream": "stdout", "path": path.display().to_string(), "bytes": stdout.len()}),
        );
        stdout.clear();
    }
    if let Some(path) = &stderr_to {
        tokio::fs::write(path, &stderr).await.map_err(|e| {
            ToolError::failed_with(
                crate::error::ErrorCode::IoError,
                format!("写 stderr_to 失败：{e}"),
            )
        })?;
        redirected.push(
            json!({"stream": "stderr", "path": path.display().to_string(), "bytes": stderr.len()}),
        );
        stderr.clear();
    }
    let combined = merge_output(&stdout, &stderr);
    let truncation = truncate_tail(&combined, ctx.limits);

    let mut body = truncation.content.clone();
    let mut spill_meta = Value::Null;
    // §4.4b：截断告知是一个事实源、两个通道——content 有可读提示，
    // structuredContent 也必须有结构化条目，程序才能按 code 判断。
    let mut truncation_notice: Option<Notice> = None;

    if truncation.truncated {
        let spill = ctx.spill.persist("bash", &combined)?;
        truncation_notice = Some(Notice::Truncated {
            kept: truncation.output_lines as u64,
            total: truncation.total_lines as u64,
            unit: crate::notice::TruncUnit::Lines,
            pointer: Some(spill.path.display().to_string()),
        });
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
    if segments.len() > 1 {
        meta["pipeline"] = json!(
            segments
                .iter()
                .map(|(b, a)| json!({"binary": b, "args": a}))
                .collect::<Vec<_>>()
        );
    }
    if !redirected.is_empty() {
        meta["redirected"] = json!(redirected);
    }
    // §5.1/§9.2：L1 判定只在协商层判定要判时跑（raw 不介入；输出 < 500 token
    // 由 exitCode 机械判断）。判定不可用（未绑定 / feature 未开 / 调用失败）
    // 只降级并告知，工具照常工作（§7.3）。
    let mut notices = plan.notices.clone();
    if let Some(notice) = truncation_notice {
        notices.push(notice);
    }

    // §5.1 L0.5：去重只作用于送给 decide/summarize 的模型输入，content 原文不变。
    let mut model_body = body.clone();
    let dedup = if plan.should_decide || plan.should_summarize {
        crate::semantic_dedup::dedup_tail(ctx, &body).await
    } else {
        None
    };
    if let Some(dedup) = dedup {
        meta["dedup"] = json!({
            "inputLines": dedup.stats.input_lines,
            "outputLines": dedup.stats.output_lines,
            "batches": dedup.stats.batches,
        });
        model_body = dedup.text;
    }

    // §5.1 L1.5：非 raw 且达到 500 token 门槛才标注；classify 不可用回退机械。
    if plan.mode != BashMode::Raw && estimate_tokens(&body) >= 500 {
        let sensitivity = crate::sensitivity::assess(ctx, &body).await;
        meta["sensitivity"] = sensitivity.to_meta();
        if let Some(level) = sensitivity.notice_level() {
            notices.push(Notice::Sensitive { level });
        }
    }

    let mut decision_facts = DecisionFacts::default();
    if plan.should_decide {
        match run_decide(ctx, &model_body).await {
            Some(run) => {
                notices.extend(gate_notices(run.gate, run.decision.confidence));
                decision_facts = DecisionFacts {
                    error_kind: Some(run.decision.error_kind.clone()),
                    severity: Some(run.decision.severity.clone()),
                    // failedItems 本轮留空：§5.1 的 failedItems 语义未定义，不编造。
                    failed_items: Vec::new(),
                    needs_full_context: Some(run.decision.needs_full_context),
                    confidence: Some(run.decision.confidence),
                };
                meta["confidence"] = json!(run.decision.confidence);
                meta["decision"] = json!({
                    "hasError": run.decision.has_error,
                    "errorKind": run.decision.error_kind,
                    "severity": run.decision.severity,
                    "needsFullContext": run.decision.needs_full_context,
                });
            }
            None => {
                notices.push(Notice::SlotUnavailable {
                    slot: "decide".to_owned(),
                    fallback: SlotFallback::Mechanical,
                    reason: ctx
                        .slm
                        .as_ref()
                        .and_then(|c| c.slot_blocker(crate::slots::SlotKind::Decide)),
                });
            }
        }
    }

    // §5.1：L2 摘要只在协商层判定要摘要时跑；不可用只降级并告知，工具照常工作（§7.3）。
    let mut summary_text: Option<String> = None;
    if plan.should_summarize {
        match run_summarize(ctx, &model_body, &decision_facts).await {
            Some(text) => summary_text = Some(text),
            None => notices.push(Notice::SlotUnavailable {
                slot: "summarize".to_owned(),
                fallback: SlotFallback::Mechanical,
                reason: ctx
                    .slm
                    .as_ref()
                    .and_then(|c| c.slot_blocker(crate::slots::SlotKind::Summarize)),
            }),
        }
    }

    let mut content = body;
    if let Some(text) = &summary_text {
        // §5.1：摘要成功时 content 只给摘要，判定事实在 meta.decision、原文在 meta.spill，
        // 不重复上传原文；meta.summary 同时给程序可读通道。
        content = text.clone();
        meta["summary"] = json!(text);
    }
    if !notices.is_empty() {
        let ch = budgeted_notices(notices);
        // structured 形态的 content 是纯事实 JSON，告知只进 structuredContent（§4.4b 两个通道）。
        if plan.mode != BashMode::Structured {
            content.push('\n');
            content.push_str(&ch.text);
        }
        meta["notices"] = ch.structured;
        meta["noticeOverflow"] = json!(ch.overflow);
    }
    if plan.mode == BashMode::Structured {
        let spill_path = meta["spill"]["path"].as_str();
        let facts = structured_facts(code, spill_path, &decision_facts);
        content = facts.to_string();
        meta["facts"] = facts;
    }
    Ok(ToolResponse::text(content).with_meta(meta))
}

/// 执行（可多段）线性管道并返回最后一段的 stdout/stderr 与退出状态（§5.1）。
///
/// 段间用 OS 管道连接：第一段 `process_group(0)` 建立独立进程组，后续段继承该组，
/// 超时按组终止即可整链回收（§14 阶段 4）。每段仍是 execve，没有 shell 解析层。
/// 管道用 std::process 构建（其 ChildStdout 可直接转 Stdio），整体放在阻塞线程里跑。
async fn exec_segments(
    segments: &[(String, Vec<String>)],
    cwd: &Path,
    timeout: Duration,
) -> Result<(Vec<u8>, Vec<u8>, std::process::ExitStatus), ToolError> {
    let segs = segments.to_vec();
    let cwd = cwd.to_path_buf();
    let label = segments
        .iter()
        .map(|(b, a)| {
            if a.is_empty() {
                b.clone()
            } else {
                format!("{b} {}", a.join(" "))
            }
        })
        .collect::<Vec<_>>()
        .join(" | ");
    let pid_shared = Arc::new(AtomicU32::new(0));
    let pid_for_thread = pid_shared.clone();
    let join =
        tokio::task::spawn_blocking(move || run_pipeline_blocking(&segs, &cwd, &pid_for_thread));
    match tokio::time::timeout(timeout, join).await {
        Ok(Ok(result)) => result,
        Ok(Err(e)) => Err(ToolError::failed_with(
            crate::error::ErrorCode::IoError,
            format!("执行线程失败：{e}"),
        )),
        Err(_) => {
            let pid = pid_shared.load(Ordering::SeqCst);
            super::kill_process_group(pid).await;
            Err(ToolError::failed_with(
                crate::error::ErrorCode::Timeout,
                format!("命令超时（{} 秒）已被终止：{label}", timeout.as_secs()),
            ))
        }
    }
}

/// 阻塞版：构建并运行线性管道，返回最后一段 stdout/stderr 与退出状态。
fn run_pipeline_blocking(
    segments: &[(String, Vec<String>)],
    cwd: &Path,
    pid_shared: &AtomicU32,
) -> Result<(Vec<u8>, Vec<u8>, std::process::ExitStatus), ToolError> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command as StdCommand, Stdio as Std};

    let mut children = Vec::with_capacity(segments.len());
    let mut prev_stdout: Option<Std> = None;
    let mut last_stdout: Option<std::process::ChildStdout> = None;
    let mut stderr_threads = Vec::with_capacity(segments.len());

    for (i, (bin, av)) in segments.iter().enumerate() {
        let mut cmd = StdCommand::new(bin);
        cmd.args(av).current_dir(cwd).stderr(Std::piped());
        if i == 0 {
            cmd.stdin(Std::null()).stdout(Std::piped()).process_group(0);
        } else {
            cmd.stdin(prev_stdout.take().expect("上一段 stdout"))
                .stdout(Std::piped());
        }
        apply_env_allowlist_std(&mut cmd, cwd);
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let pid = pid_shared.load(Ordering::SeqCst);
                if pid != 0 {
                    let _ = StdCommand::new("kill")
                        .arg("-KILL")
                        .arg(format!("-{pid}"))
                        .stdin(Std::null())
                        .stdout(Std::null())
                        .stderr(Std::null())
                        .status();
                }
                return Err(ToolError::failed_with(
                    crate::error::ErrorCode::IoError,
                    format!("无法启动 {bin}：{e}"),
                ));
            }
        };
        if i == 0 {
            pid_shared.store(child.id(), Ordering::SeqCst);
        }
        let out = child.stdout.take().expect("stdout piped");
        if i + 1 == segments.len() {
            last_stdout = Some(out);
        } else {
            prev_stdout = Some(Std::from(out));
        }
        let err = child.stderr.take().expect("stderr piped");
        stderr_threads.push(std::thread::spawn(move || {
            use std::io::Read;
            let mut r = err;
            let mut b = Vec::new();
            let _ = r.read_to_end(&mut b);
            b
        }));
        children.push(child);
    }

    let mut out = Vec::new();
    if let Some(mut h) = last_stdout {
        use std::io::Read;
        let _ = h.read_to_end(&mut out);
    }
    let mut err = Vec::new();
    for t in stderr_threads {
        if let Ok(b) = t.join() {
            err.extend_from_slice(&b);
        }
    }
    let mut status: Option<std::process::ExitStatus> = None;
    for ch in &mut children {
        match ch.wait() {
            Ok(st) => status = Some(st),
            Err(e) => {
                return Err(ToolError::failed_with(
                    crate::error::ErrorCode::IoError,
                    format!("等待进程失败：{e}"),
                ));
            }
        }
    }
    Ok((out, err, status.expect("至少一个进程")))
}

/// std::process::Command 版环境白名单（§17.2），与 tokio 版同一份名单。
fn apply_env_allowlist_std(cmd: &mut std::process::Command, cwd: &Path) {
    cmd.env_clear();
    for key in ENV_ALLOWLIST {
        if *key == "PWD" {
            continue;
        }
        if let Some(value) = std::env::var_os(key) {
            cmd.env(key, value);
        }
    }
    cmd.env("PWD", cwd);
}

/// 判定状态窗口：命令输出尾部 8000 字符（§6.4 prefill 预算，§5.1 有界候选）。
const DECISION_STATE_CHARS: usize = 8000;

/// 一次判定的结果：归并后的结论与 §7.2 门控档位。
struct DecideRun {
    decision: BashDecision,
    gate: ConfidenceGate,
}

/// 跑一次 L1 判定；不可用（未绑定 / feature 未开 / 调用失败）时返回 None，由调用方降级（§7.3）。
async fn run_decide(ctx: &ToolContext, body: &str) -> Option<DecideRun> {
    let client = ctx.slm.as_ref()?;
    // 状态文本取命令输出的尾部窗口（§6.4 有界候选）。
    let state: String = body
        .chars()
        .rev()
        .take(DECISION_STATE_CHARS)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let input = DecideInput {
        state: json!(state),
        images: None,
        questions: L1_QUESTIONS.iter().map(|q| (*q).to_owned()).collect(),
    };
    let outcome = client.decide(&input).await.ok()?;
    // 按固定 key 归并：answers 的键是问题原文，先按顺序映射到稳定 key。
    let probs: BTreeMap<String, f64> = L1_QUESTION_KEYS
        .iter()
        .zip(L1_QUESTIONS.iter())
        .map(|(key, question)| {
            let probability = outcome
                .output
                .answers
                .get(*question)
                .map(|answer| answer.probability)
                .unwrap_or(0.0);
            ((*key).to_owned(), probability)
        })
        .collect();
    let decision = merge_decision(&probs);
    // §15.2 判定日志：confidence 按 §7.3 口径在归并结论后传入——它是「被采纳结论
    // 所依赖的问题概率最小值」，不是全部问题的最小值。写失败不影响工具结果
    // （观测性不进主路径）。
    let _ = crate::decision_log::append(
        "bash",
        Some(decision.confidence),
        client.decide_model_name().as_deref(),
        &input,
        &outcome,
    )
    .await;
    Some(DecideRun {
        decision,
        gate: outcome.gate,
    })
}

/// 跑一次 L2 摘要；槽位不可用（未绑定 / feature 未开 / 调用失败）时返回 None，
/// 由调用方降级（§7.3）。
async fn run_summarize(ctx: &ToolContext, body: &str, facts: &DecisionFacts) -> Option<String> {
    let client = ctx.slm.as_ref()?;
    let input = build_summarize_input(body, facts);
    let outcome = client.summarize(&input).await.ok()?;
    outcome.output.text
}

/// §5.1：摘要输入是有界窗口（输出尾部 8000 字符，§6.4）+ L1 判定事实，
/// 不把整份输出喂给生成模型。
fn build_summarize_input(body: &str, facts: &DecisionFacts) -> SummarizeInput {
    let tail: String = body
        .chars()
        .rev()
        .take(DECISION_STATE_CHARS)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let text = format!(
        "判定事实：errorKind={} severity={}\n输出尾部：\n{}",
        facts.error_kind.as_deref().unwrap_or("unknown"),
        facts.severity.as_deref().unwrap_or("unknown"),
        tail,
    );
    SummarizeInput {
        text,
        schema: None,
        max_tokens: None,
    }
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

    /// §17.2：白名单外的父进程变量不进子进程。
    #[tokio::test]
    async fn env_allowlist_hides_unlisted_parent_vars() {
        // SAFETY: edition 2024 把 set_var 标为 unsafe（多线程下改环境不安全）。
        // 这里用本次测试独占的变量名，断言的是子进程看不到它，测完即删。
        unsafe { std::env::set_var("TOOLGATE_TEST_LEAK", "1") };
        let (ctx, dir) = bash_ctx("env-leak");
        let resp = run(&json!({ "binary": "env", "args": [] }), &ctx)
            .await
            .expect("run ok");
        let crate::tools::Content::Text(body) = &resp.content[0] else {
            panic!("应为文本")
        };
        assert!(
            !body.contains("TOOLGATE_TEST_LEAK"),
            "子进程看到了白名单外的变量：{body}"
        );
        unsafe { std::env::remove_var("TOOLGATE_TEST_LEAK") };
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// §17.2：白名单内的变量透传，PWD 是子进程自己的工作目录。
    #[tokio::test]
    async fn env_allowlist_keeps_listed_vars_and_sets_pwd() {
        let (ctx, dir) = bash_ctx("env-keep");
        let resp = run(&json!({ "binary": "env", "args": [] }), &ctx)
            .await
            .expect("run ok");
        let crate::tools::Content::Text(body) = &resp.content[0] else {
            panic!("应为文本")
        };
        assert!(body.contains("PATH="), "PATH 未透传：{body}");
        assert!(body.contains("HOME="), "HOME 未透传：{body}");
        // PWD 必须是子进程的 cwd，而不是 toolgate 进程的 cwd。
        let expected = format!("PWD={}", ctx.cwd.display());
        assert!(
            body.contains(&expected),
            "PWD 不是子进程 cwd：期望 {expected}（dir={}）\n{body}",
            dir.display()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// §17.2：身份探测给出整数 uid/gid（本机 id 命令可用）。
    #[test]
    fn identity_probe_returns_integers() {
        let id = identity();
        assert!(id.uid.is_some(), "uid 探测失败");
        assert!(id.gid.is_some(), "gid 探测失败");
        let payload = id.to_json();
        assert!(payload["uid"].as_u64().is_some());
        assert!(payload["gid"].as_u64().is_some());
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

    /// §5.1：pipeline 段间用 OS 管道连接，等价 a | b | c。
    #[tokio::test]
    async fn pipeline_joins_segments_by_os_pipe() {
        let (ctx, dir) = bash_ctx("pipeline-pipe");
        let resp = run(
            &json!({
                "pipeline": [
                    { "binary": "printf", "args": ["a\nb\nc\n"] },
                    { "binary": "head", "args": ["-n", "2"] }
                ],
                "intent": "raw"
            }),
            &ctx,
        )
        .await
        .expect("pipeline ok");
        let crate::tools::Content::Text(body) = &resp.content[0] else {
            panic!("应为文本")
        };
        assert!(body.contains("a"), "缺 a：{body}");
        assert!(body.contains("b"), "缺 b：{body}");
        assert!(!body.contains("c"), "head -n 2 不应含 c：{body}");
        assert_eq!(resp.meta["pipeline"].as_array().unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// §5.1 / §17.2：pipeline 每段仍拒绝 shell 解释器。
    #[tokio::test]
    async fn pipeline_rejects_shell_segment() {
        let (ctx, dir) = bash_ctx("pipeline-shell");
        let err = run(
            &json!({ "pipeline": [ { "binary": "bash", "args": ["-c", "echo hi"] } ] }),
            &ctx,
        )
        .await
        .expect_err("应拒绝 shell 段");
        assert!(
            format!("{err:?}").contains("不接受"),
            "应拒绝 shell 入口：{err:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// §5.1：pipeline 为空数组是参数错误。
    #[tokio::test]
    async fn pipeline_empty_is_rejected() {
        let (ctx, dir) = bash_ctx("pipeline-empty");
        assert!(run(&json!({ "pipeline": [] }), &ctx).await.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// §5.1：stdout_to 把输出写文件并返回路径，不占上下文。
    #[tokio::test]
    async fn stdout_to_redirects_to_file() {
        let (ctx, dir) = bash_ctx("stdout-to");
        let target = dir.join("out.txt");
        let resp = run(
            &json!({ "binary": "echo", "args": ["hi"], "stdout_to": target.display().to_string(), "intent": "raw" }),
            &ctx,
        )
        .await
        .expect("run ok");
        let content = std::fs::read_to_string(&target).expect("文件应存在");
        assert!(content.contains("hi"), "文件内容：{content}");
        assert_eq!(resp.meta["redirected"][0]["stream"], json!("stdout"));
        let crate::tools::Content::Text(body) = &resp.content[0] else {
            panic!("应为文本")
        };
        assert!(
            body.contains("无输出"),
            "stdout 已被重定向，content 应为空提示：{body}"
        );
        let _ = std::fs::remove_dir_all(&dir);
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
        // 到达 500 token 门槛才进理解链；低门槛走机械路径（见 summary_small_output_stays_raw）。
        let long = "x".repeat(2400);
        let resp = run(
            &json!({ "binary": "echo", "args": [long], "intent": "summary" }),
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
        assert_eq!(facts["severity"], json!("unknown"));
        assert_eq!(facts["failedItems"], json!([]));
        // 判定不可用：needsFullContext 与 confidence 都是 null，不编造（§3.8）。
        assert!(facts["needsFullContext"].is_null());
        assert!(facts["confidence"].is_null());
        assert_eq!(resp.meta["facts"]["errorKind"], json!("unknown"));
        // 低于门槛不触发判定，因此没有 decide 降级告知（§5.1/§9.2）。
        assert!(resp.meta.get("notices").is_none());
        assert_eq!(resp.meta["intent"], json!("structured"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// §7.2：门控三档在 bash 路径上的告知行为。
    #[test]
    fn gate_notices_cover_three_tiers() {
        assert!(gate_notices(ConfidenceGate::Adopt, 0.95).is_empty());
        let low = gate_notices(ConfidenceGate::AdoptWithLowConfidence, 0.7);
        assert_eq!(low.len(), 1);
        assert_eq!(low[0].code(), "LOW_CONFIDENCE");
        assert!(low[0].render().contains("views=local"));
        let both = gate_notices(ConfidenceGate::BothViews, 0.3);
        assert_eq!(both.len(), 1);
        assert_eq!(both[0].code(), "LOW_CONFIDENCE");
        assert!(both[0].render().contains("views=both"));
    }

    /// §7.3：判定不可用时降级并告知，工具照常成功（不因判定失败而失败）。
    #[tokio::test]
    async fn decide_unavailable_degrades_without_failing() {
        let (ctx, dir) = bash_ctx("no-decide");
        assert!(ctx.slm.is_none(), "默认构造应当没有槽位客户端");
        // 到达门槛才进理解链，否则本测试验证不到「判定不可用」的分支。
        let long = "x".repeat(2400);
        let resp = run(
            &json!({ "binary": "echo", "args": [long], "intent": "summary" }),
            &ctx,
        )
        .await
        .expect("run ok");
        let crate::tools::Content::Text(body) = &resp.content[0] else {
            panic!("应为文本")
        };
        assert!(body.contains("slot=decide"));
        assert!(body.contains("slot=summarize"));
        // 判定不可用：confidence 与 decision 都不写（§4.6 诚实，不填 0）。
        assert!(resp.meta.get("confidence").is_none());
        assert!(resp.meta.get("decision").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// §5.1/§9.2：低于 500 token 不进理解链，给原文且无降级告知。
    #[tokio::test]
    async fn summary_small_output_stays_raw() {
        let (ctx, dir) = bash_ctx("summary-small");
        let resp = run(
            &json!({ "binary": "echo", "args": ["hello"], "intent": "summary" }),
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
        assert!(resp.meta.get("confidence").is_none());
        assert_eq!(resp.meta["intent"], json!("summary"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// §5.1：摘要输入有界，且带上 L1 判定事实。
    #[test]
    fn summarize_input_is_bounded_and_carries_decision() {
        let body = "头部".to_owned() + &"x".repeat(DECISION_STATE_CHARS + 100) + "尾部";
        let facts = DecisionFacts {
            error_kind: Some("compile".to_owned()),
            severity: Some("blocking".to_owned()),
            ..Default::default()
        };
        let input = build_summarize_input(&body, &facts);
        assert!(input.text.contains("errorKind=compile"));
        assert!(input.text.contains("severity=blocking"));
        assert!(input.text.contains("尾部"));
        assert!(!input.text.contains("头部"));
        assert!(input.schema.is_none());
        assert!(input.max_tokens.is_none());
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
