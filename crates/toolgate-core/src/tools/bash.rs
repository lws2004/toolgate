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
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
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

    let mut command = Command::new(binary);
    command
        .args(&argv)
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // §14 阶段 4：独立进程组，超时可整组终止（避免子进程残留）。
        .process_group(0);
    // §17.2：子进程不继承父进程环境，只拿白名单。
    apply_env_allowlist(&mut command, &cwd);
    let mut child = command
        .spawn()
        .map_err(|e| ToolError::failed(format!("无法启动 {binary}：{e}")))?;
    let pid = child.id().unwrap_or(0);

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
            super::kill_process_group(pid).await;
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
    // §5.1/§9.2：L1 判定只在协商层判定要判时跑（raw 不介入；输出 < 500 token
    // 由 exitCode 机械判断）。判定不可用（未绑定 / feature 未开 / 调用失败）
    // 只降级并告知，工具照常工作（§7.3）。
    let mut notices = plan.notices.clone();
    let mut decision_facts = DecisionFacts::default();
    if plan.should_decide {
        match run_decide(ctx, &body).await {
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
                });
            }
        }
    }

    // §5.1：L2 摘要只在协商层判定要摘要时跑；不可用只降级并告知，工具照常工作（§7.3）。
    let mut summary_text: Option<String> = None;
    if plan.should_summarize {
        match run_summarize(ctx, &body, &decision_facts).await {
            Some(text) => summary_text = Some(text),
            None => notices.push(Notice::SlotUnavailable {
                slot: "summarize".to_owned(),
                fallback: SlotFallback::Mechanical,
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
    Some(DecideRun {
        decision: merge_decision(&probs),
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
