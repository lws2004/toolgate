//! toolgate 命令行入口。
//!
//! 子命令：
//!   serve              以 MCP stdio server 方式运行（供 agent 接入）
//!   status             打印进程身份 uid/gid（§17.2 权限越界）
//!   list               列出工具及其入参 schema
//!   call <tool> [json] 直接调用一次工具（调试用）
//!   config check       校验配置文件并打印六个槽位的最终绑定（§18.9）
//!   decide             调用 decide 槽位判定一次（调试用）
//!   summarize          调用 summarize 槽位摘要一段文本（调试用）
//!
//! 配置解析与槽位绑定推导在 toolgate-core 的 config 模块（§3.10 / §18.9），
//! CLI 只做参数解析、文案本地化与退出码。

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::json;
use toolgate_core::config::{
    ConfigError, ConfigErrors, ResolvedConfig, default_config_path, load_config,
};
use toolgate_core::locale::{Language, localize_value, tf};
use toolgate_core::slm::{DecideOutcome, SlotCallError, SlotClient, SummarizeOutcome};
use toolgate_core::slots::{
    ConfidenceGate, DecideInput, SlotKind, SummarizeInput, confidence_gate,
};
use toolgate_core::tools::bash::ProcessIdentity;
use toolgate_core::tools::{ToolContext, all_specs, dispatch, round2};
use toolgate_mcp::McpServer;

const USAGE: &str = "\
用法: toolgate <子命令> [参数]

子命令:
  serve                 以 MCP stdio server 方式运行，供 agent 客户端接入
  status                打印进程身份 uid/gid，供无握手客户端核对权限（§17.2）
  list                  列出全部工具与入参 schema
  call <工具名> [JSON]   直接调用一次工具，参数是 JSON 对象（调试用）
  config check          校验配置文件并打印六个槽位的最终绑定（§18.9）
  decide                调用 decide 槽位判定一次并打印概率与门控结论（调试用）
  summarize             调用 summarize 槽位摘要一段文本（调试用）

选项:
  --config <路径>       指定配置文件路径（默认 ~/.toolgate/config.yaml）
  --json                config check / decide / summarize / status 以 JSON 输出结果
  --state <文本>        decide 的待判定状态（必填）
  --question <问题>     decide 的判定问题，可重复（至少一个）
  --text <文本>        summarize 的待摘要文本（必填）

说明:
  serve / list / call 启动时按 --config 加载配置并注入槽位客户端；
  配置文件不存在不算错误，槽位全空（§3.8）。

示例:
  toolgate serve
  toolgate list --config ~/.toolgate/config.yaml
  toolgate config check
  toolgate decide --state \"The sky is blue.\" --question \"Is the sky blue?\"
  toolgate call read '{\"path\":\"Cargo.toml\"}'
";

const USAGE_EN: &str = "\
Usage: toolgate <subcommand> [args]

Subcommands:
  serve                 Run as an MCP stdio server for agent clients
  status                Print the process identity uid/gid for handshake-less clients (§17.2)
  list                  List all tools and their input schemas
  call <tool> [JSON]     Invoke one tool directly (debug only)
  config check          Validate the config file and print the six slot bindings (§18.9)
  decide                Run one decide-slot judgment and print probabilities and the gate
  summarize             Summarize a text with the summarize slot (debug only)

Options:
  --config <path>       Config file path (default ~/.toolgate/config.yaml)
  --json                Print the config check, decide, summarize and status results as JSON
  --state <text>        State to judge for decide (required)
  --question <question> Decision question for decide; repeatable (at least one)
  --text <text>         Text to summarize for the summarize subcommand (required)

Notes:
  serve / list / call load the config at startup and inject the slot client;
  a missing config file is not an error, all slots stay empty (§3.8).

Examples:
  toolgate serve
  toolgate list --config ~/.toolgate/config.yaml
  toolgate config check
  toolgate decide --state \"The sky is blue.\" --question \"Is the sky blue?\"
  toolgate call read '{\"path\":\"Cargo.toml\"}'
";

/// 槽位绑定的一行视图（§3.10 可审计）：模型、来源、模式、适配器、显式/自动。
struct SlotRow {
    slot: String,
    model_id: Option<String>,
    source: Option<String>,
    mode: Option<String>,
    adapter: Option<String>,
    explicit: bool,
}

impl SlotRow {
    fn from_config(cfg: &ResolvedConfig) -> Vec<Self> {
        cfg.bindings
            .iter()
            .map(|b| Self {
                slot: b.slot.as_str().to_owned(),
                model_id: b.model_id.clone(),
                source: b.source.as_ref().map(|s| s.as_str().to_owned()),
                mode: b.mode.as_ref().map(|m| m.as_str().to_owned()),
                adapter: b.adapter.as_ref().map(|a| a.as_str().to_owned()),
                explicit: b.explicit,
            })
            .collect()
    }

    /// 六个槽位全空：无配置文件时的内置默认（§3.8）。
    fn empty() -> Vec<Self> {
        SlotKind::ALL
            .iter()
            .map(|k| Self {
                slot: k.as_str().to_owned(),
                model_id: None,
                source: None,
                mode: None,
                adapter: None,
                explicit: false,
            })
            .collect()
    }

    fn json(&self) -> serde_json::Value {
        json!({
            "slot": self.slot,
            "model_id": self.model_id,
            "source": self.source,
            "mode": self.mode,
            "adapter": self.adapter,
            "explicit": self.explicit,
        })
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let lang = Language::from_env();
    let usage = match lang {
        Language::En => USAGE_EN,
        Language::Zh => USAGE,
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first() else {
        eprintln!("{usage}");
        return ExitCode::FAILURE;
    };

    match cmd.as_str() {
        "serve" => {
            let config_path = match parse_config_arg(lang, &args[1..]) {
                Ok(p) => p,
                Err(code) => return code,
            };
            let (ctx, rows) = match build_context(lang, config_path.as_deref()) {
                Ok(v) => v,
                Err(code) => return code,
            };
            tracing::warn!(
                "{}",
                tf(
                    lang,
                    "槽位绑定：{summary}",
                    &[("summary", slot_summary(lang, &rows))]
                )
            );
            tracing::warn!(
                "{}",
                tf(
                    lang,
                    "判定能力：{state}",
                    &[("state", decide_state(lang, &rows))]
                )
            );
            match run_serve(ctx).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!(
                        "{}",
                        tf(lang, "MCP server 异常退出：{e}", &[("e", e.to_string())])
                    );
                    ExitCode::FAILURE
                }
            }
        }
        "status" => {
            let json_out = match parse_status_args(lang, &args[1..]) {
                Ok(v) => v,
                Err(code) => return code,
            };
            run_status(lang, json_out)
        }
        "list" => {
            let config_path = match parse_config_arg(lang, &args[1..]) {
                Ok(p) => p,
                Err(code) => return code,
            };
            let (_ctx, rows) = match build_context(lang, config_path.as_deref()) {
                Ok(v) => v,
                Err(code) => return code,
            };
            for spec in all_specs() {
                let description = match lang {
                    Language::En => spec.description_en,
                    Language::Zh => spec.description,
                };
                println!("{}\n  {}", spec.name, description);
                println!(
                    "  schema: {}",
                    serde_json::to_string(&localize_value(lang, &spec.input_schema))
                        .unwrap_or_default()
                );
                println!(
                    "  output: {}",
                    serde_json::to_string(&localize_value(lang, &spec.output_schema()))
                        .unwrap_or_default()
                );
                println!();
            }
            print_slot_rows(lang, &rows);
            ExitCode::SUCCESS
        }
        "call" => {
            let call_args = match parse_call_args(lang, &args[1..]) {
                Ok(v) => v,
                Err(code) => return code,
            };
            let (ctx, _rows) = match build_context(lang, call_args.config_path.as_deref()) {
                Ok(v) => v,
                Err(code) => return code,
            };
            match run_call(&call_args.tool, call_args.args, ctx).await {
                Ok(code) => code,
                Err(e) => {
                    eprintln!("{e}");
                    ExitCode::FAILURE
                }
            }
        }
        "decide" => {
            let decide_args = match parse_decide_args(lang, &args[1..]) {
                Ok(v) => v,
                Err(code) => return code,
            };
            run_decide(lang, decide_args).await
        }
        "summarize" => {
            let summarize_args = match parse_summarize_args(lang, &args[1..]) {
                Ok(v) => v,
                Err(code) => return code,
            };
            run_summarize(lang, summarize_args).await
        }
        "config" => match args.get(1).map(String::as_str) {
            Some("check") => {
                let (config_path, json_out) = match parse_config_check_args(lang, &args[2..]) {
                    Ok(v) => v,
                    Err(code) => return code,
                };
                run_config_check(lang, config_path.as_deref(), json_out)
            }
            Some(other) => {
                eprintln!(
                    "{}",
                    tf(
                        lang,
                        "未知 config 子命令：{other}",
                        &[("other", other.to_owned())]
                    )
                );
                ExitCode::FAILURE
            }
            None => {
                eprintln!("{}", tf(lang, "config 需要子命令（当前只有 check）。", &[]));
                ExitCode::FAILURE
            }
        },
        "-h" | "--help" | "help" => {
            println!("{usage}");
            ExitCode::SUCCESS
        }
        other => {
            eprintln!(
                "{}",
                tf(
                    lang,
                    "未知子命令：{other}\n\n{USAGE}",
                    &[("other", other.to_owned()), ("USAGE", usage.to_owned())]
                )
            );
            ExitCode::FAILURE
        }
    }
}

/// 解析 serve / list 的 --config 参数。
fn parse_config_arg(lang: Language, args: &[String]) -> Result<Option<PathBuf>, ExitCode> {
    let mut path: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--config" => {
                let Some(value) = args.get(i + 1) else {
                    eprintln!("{}", tf(lang, "--config 需要一个路径参数。", &[]));
                    return Err(ExitCode::FAILURE);
                };
                path = Some(PathBuf::from(value));
                i += 2;
            }
            other => {
                eprintln!(
                    "{}",
                    tf(lang, "未知参数：{other}", &[("other", other.to_owned())])
                );
                return Err(ExitCode::FAILURE);
            }
        }
    }
    Ok(path)
}

/// 解析 config check 的 --config / --json 参数。
fn parse_config_check_args(
    lang: Language,
    args: &[String],
) -> Result<(Option<PathBuf>, bool), ExitCode> {
    let mut path: Option<PathBuf> = None;
    let mut json_out = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--config" => {
                let Some(value) = args.get(i + 1) else {
                    eprintln!("{}", tf(lang, "--config 需要一个路径参数。", &[]));
                    return Err(ExitCode::FAILURE);
                };
                path = Some(PathBuf::from(value));
                i += 2;
            }
            "--json" => {
                json_out = true;
                i += 1;
            }
            other => {
                eprintln!(
                    "{}",
                    tf(lang, "未知参数：{other}", &[("other", other.to_owned())])
                );
                return Err(ExitCode::FAILURE);
            }
        }
    }
    Ok((path, json_out))
}

/// summarize 子命令的参数。
struct SummarizeArgs {
    config_path: Option<PathBuf>,
    text: Option<String>,
    json_out: bool,
}

/// 解析 summarize 的 --config / --text / --json 参数。
fn parse_summarize_args(lang: Language, args: &[String]) -> Result<SummarizeArgs, ExitCode> {
    let mut config_path: Option<PathBuf> = None;
    let mut text: Option<String> = None;
    let mut json_out = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--config" => {
                let Some(value) = args.get(i + 1) else {
                    eprintln!("{}", tf(lang, "--config 需要一个路径参数。", &[]));
                    return Err(ExitCode::FAILURE);
                };
                config_path = Some(PathBuf::from(value));
                i += 2;
            }
            "--text" => {
                let Some(value) = args.get(i + 1) else {
                    eprintln!("{}", tf(lang, "--text 需要一个文本参数。", &[]));
                    return Err(ExitCode::FAILURE);
                };
                text = Some(value.clone());
                i += 2;
            }
            "--json" => {
                json_out = true;
                i += 1;
            }
            other => {
                eprintln!("{}", tf(lang, "未知参数：{other}", &[("other", other.to_owned())]));
                return Err(ExitCode::FAILURE);
            }
        }
    }
    Ok(SummarizeArgs { config_path, text, json_out })
}

/// decide 子命令的参数。
struct DecideArgs {
    config_path: Option<PathBuf>,
    state: Option<String>,
    questions: Vec<String>,
    json_out: bool,
}

/// 解析 decide 的 --config / --state / --question / --json 参数。
fn parse_decide_args(lang: Language, args: &[String]) -> Result<DecideArgs, ExitCode> {
    let mut config_path: Option<PathBuf> = None;
    let mut state: Option<String> = None;
    let mut questions: Vec<String> = Vec::new();
    let mut json_out = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--config" => {
                let Some(value) = args.get(i + 1) else {
                    eprintln!("{}", tf(lang, "--config 需要一个路径参数。", &[]));
                    return Err(ExitCode::FAILURE);
                };
                config_path = Some(PathBuf::from(value));
                i += 2;
            }
            "--state" => {
                let Some(value) = args.get(i + 1) else {
                    eprintln!("{}", tf(lang, "--state 需要一个文本参数。", &[]));
                    return Err(ExitCode::FAILURE);
                };
                state = Some(value.clone());
                i += 2;
            }
            "--question" => {
                let Some(value) = args.get(i + 1) else {
                    eprintln!("{}", tf(lang, "--question 需要一个文本参数。", &[]));
                    return Err(ExitCode::FAILURE);
                };
                questions.push(value.clone());
                i += 2;
            }
            "--json" => {
                json_out = true;
                i += 1;
            }
            other => {
                eprintln!(
                    "{}",
                    tf(lang, "未知参数：{other}", &[("other", other.to_owned())])
                );
                return Err(ExitCode::FAILURE);
            }
        }
    }
    Ok(DecideArgs {
        config_path,
        state,
        questions,
        json_out,
    })
}

/// decide：读配置 → 调 decide 槽位 → 打印每问概率与 §7.2 门控结论。
async fn run_decide(lang: Language, args: DecideArgs) -> ExitCode {
    let Some(state) = args.state else {
        eprintln!("{}", tf(lang, "decide 需要 --state。", &[]));
        return ExitCode::FAILURE;
    };
    if args.questions.is_empty() {
        eprintln!("{}", tf(lang, "decide 需要至少一个 --question。", &[]));
        return ExitCode::FAILURE;
    }

    let cfg = match load_cli_config(lang, args.config_path.as_deref()) {
        ConfigOutcome::Resolved(cfg) => cfg,
        ConfigOutcome::Missing => {
            report_errors(
                lang,
                &ConfigErrors {
                    items: vec![ConfigError {
                        code: "CONFIG_FILE_NOT_FOUND".to_owned(),
                        field: default_config_path().to_string_lossy().into_owned(),
                        value: None,
                        expected: tf(lang, "存在的配置文件路径", &[]),
                        fix: tf(lang, "检查路径或先创建文件", &[]),
                    }],
                },
                args.json_out,
            );
            return ExitCode::FAILURE;
        }
        ConfigOutcome::Failed(errors) => {
            report_errors(lang, &errors, args.json_out);
            return ExitCode::FAILURE;
        }
    };

    // decide 槽位必须绑定模型，否则无法判定（§3.8 空槽位降级到机械路径，不做语义判定）。
    let bound = cfg
        .bindings
        .iter()
        .find(|b| b.slot == SlotKind::Decide && b.model_id.is_some());
    if bound.is_none() {
        eprintln!("{}", tf(lang, "decide 槽位未绑定模型。", &[]));
        eprintln!(
            "{}",
            tf(lang, "在配置的 models 里让某个模型的 serves 含 decide", &[])
        );
        return ExitCode::FAILURE;
    }

    let input = DecideInput {
        state: json!(state),
        images: None,
        questions: args.questions.clone(),
    };

    let client = SlotClient::new(cfg);
    let outcome = match client.decide(&input).await {
        Ok(outcome) => outcome,
        Err(err) => {
            report_slot_error(lang, &err);
            return ExitCode::FAILURE;
        }
    };

    print_decide_outcome(lang, &outcome, args.json_out)
}

/// summarize：读配置 → 调 summarize 槽位 → 打印摘要文本。
async fn run_summarize(lang: Language, args: SummarizeArgs) -> ExitCode {
    let Some(text) = args.text else {
        eprintln!("{}", tf(lang, "summarize 需要 --text。", &[]));
        return ExitCode::FAILURE;
    };

    let cfg = match load_cli_config(lang, args.config_path.as_deref()) {
        ConfigOutcome::Resolved(cfg) => cfg,
        ConfigOutcome::Missing => {
            report_errors(
                lang,
                &ConfigErrors {
                    items: vec![ConfigError {
                        code: "CONFIG_FILE_NOT_FOUND".to_owned(),
                        field: default_config_path().to_string_lossy().into_owned(),
                        value: None,
                        expected: tf(lang, "存在的配置文件路径", &[]),
                        fix: tf(lang, "检查路径或先创建文件", &[]),
                    }],
                },
                args.json_out,
            );
            return ExitCode::FAILURE;
        }
        ConfigOutcome::Failed(errors) => {
            report_errors(lang, &errors, args.json_out);
            return ExitCode::FAILURE;
        }
    };

    let bound = cfg
        .bindings
        .iter()
        .find(|b| b.slot == SlotKind::Summarize && b.model_id.is_some());
    if bound.is_none() {
        eprintln!("{}", tf(lang, "summarize 槽位未绑定模型。", &[]));
        eprintln!("{}", tf(lang, "在配置的 models 里让某个模型的 serves 含 summarize", &[]));
        return ExitCode::FAILURE;
    }

    let input = SummarizeInput {
        text,
        schema: None,
        max_tokens: None,
    };
    let client = SlotClient::new(cfg);
    match client.summarize(&input).await {
        Ok(outcome) => print_summarize_outcome(lang, &outcome, args.json_out),
        Err(err) => {
            report_slot_error(lang, &err);
            ExitCode::FAILURE
        }
    }
}

fn print_summarize_outcome(lang: Language, outcome: &SummarizeOutcome, json_out: bool) -> ExitCode {
    let text = outcome.output.text.clone().unwrap_or_default();
    if json_out {
        let payload = json!({
            "ok": true,
            "text": text,
            "latencyMs": outcome.latency_ms,
        });
        println!("{}", pretty_json(lang, &payload));
        return ExitCode::SUCCESS;
    }
    println!("{text}");
    println!("{}", tf(lang, "摘要耗时 {ms} ms", &[("ms", outcome.latency_ms.to_string())]));
    ExitCode::SUCCESS
}

/// 打印判定调用失败：事实 + 可执行的下一步（§1.4.5）。
fn report_slot_error(lang: Language, err: &SlotCallError) {
    match err {
        SlotCallError::NotBound { slot } => {
            eprintln!(
                "{}",
                tf(
                    lang,
                    "槽位 {slot} 未绑定模型。",
                    &[("slot", slot.as_str().to_owned())]
                )
            );
            eprintln!(
                "{}",
                tf(
                    lang,
                    "在配置的 models 里让某个模型的 serves 含 {slot}",
                    &[("slot", slot.as_str().to_owned())]
                )
            );
        }
        SlotCallError::ClientUnavailable { slot, reason } => {
            eprintln!(
                "{}",
                tf(
                    lang,
                    "槽位 {slot} 不可用：{reason}",
                    &[
                        ("slot", slot.as_str().to_owned()),
                        ("reason", reason.clone())
                    ]
                )
            );
            eprintln!(
                "{}",
                tf(
                    lang,
                    "用 cargo build --features external-protocols 重新构建后再试",
                    &[]
                )
            );
        }
        SlotCallError::Transport { slot, reason } => {
            eprintln!(
                "{}",
                tf(
                    lang,
                    "槽位调用失败：{slot} {reason}",
                    &[
                        ("slot", slot.as_str().to_owned()),
                        ("reason", reason.clone())
                    ]
                )
            );
            eprintln!(
                "{}",
                tf(
                    lang,
                    "确认判定服务已启动且 base_url 与端口可达，再重试",
                    &[]
                )
            );
        }
        SlotCallError::Contract { slot, reason } => {
            eprintln!(
                "{}",
                tf(
                    lang,
                    "结果形状不符合槽位契约：{slot} {reason}",
                    &[
                        ("slot", slot.as_str().to_owned()),
                        ("reason", reason.clone())
                    ]
                )
            );
            eprintln!(
                "{}",
                tf(lang, "确认上游形状符合槽位契约（decide 为 noul 概率，summarize 为 chat content）", &[])
            );
        }
    }
}

/// 打印判定结果：每问一行概率与门控结论，最后一行给整体门控结论。
fn print_decide_outcome(lang: Language, outcome: &DecideOutcome, json_out: bool) -> ExitCode {
    // answers 的键是问题原文（core 解析时已从 qN 回映），值带概率。
    // §4.7 数值精度：概率只保留两位小数，CLI 与 MCP 返回同一口径。
    let mut answers: Vec<(String, f64)> = outcome
        .output
        .answers
        .iter()
        .map(|(question, answer)| (question.clone(), round2(answer.probability)))
        .collect();
    answers.sort_by(|a, b| a.0.cmp(&b.0));

    if json_out {
        let payload = json!({
            "ok": true,
            "answers": answers
                .iter()
                .map(|(question, probability)| json!({
                    "question": question,
                    "probability": probability,
                    "gate": gate_label(confidence_gate(*probability)),
                }))
                .collect::<Vec<_>>(),
            "gate": gate_label(outcome.gate),
            "latencyMs": outcome.latency_ms,
        });
        println!("{}", pretty_json(lang, &payload));
        return ExitCode::SUCCESS;
    }

    for (question, probability) in &answers {
        println!(
            "{}",
            tf(lang, "问题：{q}", &[("q", question.as_str().to_owned())])
        );
        println!(
            "  {}",
            tf(lang, "概率：{p}", &[("p", format!("{probability:.2}"))])
        );
        println!("  {}", gate_text(lang, confidence_gate(*probability)));
    }
    println!(
        "{}",
        tf(
            lang,
            "整体门控结论：{gate}",
            &[("gate", gate_label(outcome.gate).to_owned())]
        )
    );
    println!(
        "{}",
        tf(
            lang,
            "判定耗时 {ms} ms",
            &[("ms", outcome.latency_ms.to_string())]
        )
    );
    ExitCode::SUCCESS
}

/// 门控三档的机器可读标签（§7.2）：采用 / 低置信 / 两种视图。
fn gate_label(gate: ConfidenceGate) -> &'static str {
    gate.as_str()
}

/// 门控三档的人读结论。
fn gate_text(lang: Language, gate: ConfidenceGate) -> String {
    match gate {
        ConfidenceGate::Adopt => tf(lang, "门控结论：采用", &[]),
        ConfidenceGate::AdoptWithLowConfidence => tf(lang, "门控结论：低置信", &[]),
        ConfidenceGate::BothViews => tf(lang, "门控结论：两种视图", &[]),
    }
}

/// call 子命令的参数：工具名、JSON 入参、配置文件路径。
struct CallArgs {
    tool: String,
    args: serde_json::Value,
    config_path: Option<PathBuf>,
}

/// 解析 call 的位置参数与 --config。
///
/// 位置参数保持既有语义：第 1 个是工具名，第 2 个是 JSON 入参；
/// --config 可放在任意位置。
fn parse_call_args(lang: Language, args: &[String]) -> Result<CallArgs, ExitCode> {
    let mut positional: Vec<&str> = Vec::new();
    let mut config_path: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--config" => {
                let Some(value) = args.get(i + 1) else {
                    eprintln!("{}", tf(lang, "--config 需要一个路径参数。", &[]));
                    return Err(ExitCode::FAILURE);
                };
                config_path = Some(PathBuf::from(value));
                i += 2;
            }
            other => {
                positional.push(other);
                i += 1;
            }
        }
    }

    let Some((tool, raw)) = positional.split_first() else {
        eprintln!(
            "{}",
            tf(
                lang,
                "call 需要工具名。\n\n{USAGE}",
                &[("USAGE", usage_text(lang))]
            )
        );
        return Err(ExitCode::FAILURE);
    };
    let raw = raw.first().copied().unwrap_or("{}");
    let parsed: serde_json::Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "{}",
                tf(lang, "参数不是合法 JSON：{e}", &[("e", e.to_string())])
            );
            return Err(ExitCode::FAILURE);
        }
    };

    Ok(CallArgs {
        tool: (*tool).to_owned(),
        args: parsed,
        config_path,
    })
}

/// 按用户语言取用法文本。
fn usage_text(lang: Language) -> String {
    match lang {
        Language::En => USAGE_EN.to_owned(),
        Language::Zh => USAGE.to_owned(),
    }
}

/// 构造工具上下文：按配置加载槽位客户端并注入（§18.1）。
///
/// 配置文件不存在不是错误：槽位全空（slm = None），工具照常降级（§3.8）。
/// 配置有误是错误：沿用四段式报错格式并以非 0 退出。
fn build_context(
    lang: Language,
    config_path: Option<&Path>,
) -> Result<(ToolContext, Vec<SlotRow>), ExitCode> {
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(e) => {
            eprintln!(
                "{}",
                tf(lang, "无法确定当前工作目录：{e}", &[("e", e.to_string())])
            );
            return Err(ExitCode::FAILURE);
        }
    };
    let mut ctx = match ToolContext::new(cwd) {
        Ok(ctx) => ctx,
        Err(e) => {
            eprintln!(
                "{}",
                tf(lang, "无法创建工具上下文：{e}", &[("e", e.to_string())])
            );
            return Err(ExitCode::FAILURE);
        }
    };
    ctx.language = lang;

    let (slm, rows, extra_roots) = match load_cli_config(lang, config_path) {
        ConfigOutcome::Resolved(cfg) => {
            let rows = SlotRow::from_config(&cfg);
            let roots = cfg.paths.allowed_roots.clone();
            (Some(std::sync::Arc::new(SlotClient::new(cfg))), rows, roots)
        }
        ConfigOutcome::Missing => (None, SlotRow::empty(), Vec::new()),
        ConfigOutcome::Failed(errors) => {
            report_errors(lang, &errors, false);
            return Err(ExitCode::FAILURE);
        }
    };
    ctx.slm = slm;
    // §17.2：配置的允许根是在「启动时工作目录」之上的追加，不是替换。
    if !extra_roots.is_empty() {
        ctx.set_allowed_roots(merge_allowed_roots(&ctx.allowed_roots, &extra_roots));
    }

    Ok((ctx, rows))
}

/// §17.2：配置的允许根追加到默认根上；空清单保持默认。
///
/// 不能直接 set_allowed_roots(extra)：那会把「启动时工作目录」这个默认根丢掉，
/// 而 set_allowed_roots(vec![]) 等于拒绝一切文件路径。
fn merge_allowed_roots(defaults: &[PathBuf], extra: &[String]) -> Vec<PathBuf> {
    let mut roots = defaults.to_vec();
    roots.extend(extra.iter().map(PathBuf::from));
    roots
}

/// 加载配置的四种结局：解析成功 / 文件缺失 / 校验失败。
enum ConfigOutcome {
    Resolved(ResolvedConfig),
    Missing,
    Failed(ConfigErrors),
}

/// 加载配置并区分「文件不存在」的两种情形（§18.9）。
///
/// 默认路径不存在用内置默认；显式 --config 指定但不存在是错误。
/// 配置错误一律 fail loud。
fn load_cli_config(lang: Language, config_path: Option<&Path>) -> ConfigOutcome {
    let (path, explicit) = match config_path {
        Some(p) => (p.to_path_buf(), true),
        None => (default_config_path(), false),
    };

    match load_config(&path) {
        Ok(Some(cfg)) => ConfigOutcome::Resolved(cfg),
        Ok(None) if explicit => ConfigOutcome::Failed(ConfigErrors {
            items: vec![ConfigError {
                code: "CONFIG_FILE_NOT_FOUND".to_owned(),
                field: path.to_string_lossy().into_owned(),
                value: None,
                expected: tf(lang, "存在的配置文件路径", &[]),
                fix: tf(lang, "检查路径或先创建文件", &[]),
            }],
        }),
        Ok(None) => ConfigOutcome::Missing,
        Err(errors) => ConfigOutcome::Failed(errors),
    }
}

/// config check：只做语法 / 结构 / 语义三层校验，不加载模型（§18.9）。
fn run_config_check(lang: Language, config_path: Option<&Path>, json_out: bool) -> ExitCode {
    match load_cli_config(lang, config_path) {
        ConfigOutcome::Resolved(cfg) => {
            let rows = SlotRow::from_config(&cfg);
            if json_out {
                println!("{}", pretty_json(lang, &slots_json(&rows)));
            } else {
                println!("{}", tf(lang, "配置检查通过", &[]));
                print_slot_rows(lang, &rows);
            }
            ExitCode::SUCCESS
        }
        ConfigOutcome::Missing => {
            let rows = SlotRow::empty();
            if json_out {
                println!("{}", pretty_json(lang, &slots_json(&rows)));
            } else {
                println!(
                    "{}",
                    tf(lang, "未找到配置文件，使用内置默认（六个槽位全空）", &[])
                );
                print_slot_rows(lang, &rows);
            }
            ExitCode::SUCCESS
        }
        ConfigOutcome::Failed(errors) => {
            report_errors(lang, &errors, json_out);
            ExitCode::FAILURE
        }
    }
}

/// status：打印进程身份（§17.2 权限越界）。
///
/// 2026-07-28 无握手客户端不调 initialize 时，身份只能从命令行查看（§17.2）。
fn run_status(lang: Language, json_out: bool) -> ExitCode {
    let identity = toolgate_core::tools::bash::identity();
    if json_out {
        println!("{}", pretty_json(lang, &status_json(identity)));
    } else {
        println!("{}", identity_line(lang, identity));
    }
    ExitCode::SUCCESS
}

/// status 的 JSON 形态：与 MCP serverInfo.identity 同一形状（一个事实源）。
fn status_json(identity: ProcessIdentity) -> serde_json::Value {
    json!({ "ok": true, "identity": identity.to_json() })
}

/// status 的文本形态。新增文案没有登记进 locale.rs（不在本次改动范围），
/// 故按语言二选一——与 USAGE / USAGE_EN 同一处理方式。
fn identity_line(lang: Language, identity: ProcessIdentity) -> String {
    let uid = id_text(identity.uid, lang);
    let gid = id_text(identity.gid, lang);
    match lang {
        Language::En => format!("Identity: uid={uid} gid={gid}"),
        Language::Zh => format!("身份：uid={uid} gid={gid}"),
    }
}

/// 取不到身份时给「未知」而不是 0——0 会被读成 root（§4.6 诚实）。
fn id_text(value: Option<u32>, lang: Language) -> String {
    match value {
        Some(n) => n.to_string(),
        None => match lang {
            Language::En => "unknown".to_owned(),
            Language::Zh => "未知".to_owned(),
        },
    }
}

/// 解析 status 的 --json 开关。
fn parse_status_args(lang: Language, args: &[String]) -> Result<bool, ExitCode> {
    let mut json_out = false;
    for arg in args {
        match arg.as_str() {
            "--json" => json_out = true,
            other => {
                eprintln!(
                    "{}",
                    tf(lang, "未知参数：{other}", &[("other", other.to_owned())])
                );
                return Err(ExitCode::FAILURE);
            }
        }
    }
    Ok(json_out)
}

/// 打印配置错误：多错误一次列全，每条四段齐全（code / field / value / expected / fix）。
fn report_errors(lang: Language, errors: &ConfigErrors, json_out: bool) {
    // §18.1b：expected / fix 是自然语言，按用户语言取；code / field / value 不译。
    let errors = errors.localized(lang);
    if json_out {
        let mut payload = errors.to_json();
        if let Some(obj) = payload.as_object_mut() {
            obj.insert("ok".to_owned(), json!(false));
        }
        println!("{}", pretty_json(lang, &payload));
        return;
    }

    eprintln!("{}", tf(lang, "配置有误：", &[]));
    let total = errors.items.len();
    for (i, issue) in errors.items.iter().enumerate() {
        eprintln!(
            "  [{}/{}] code={} field={}",
            i + 1,
            total,
            issue.code,
            issue.field
        );
        if let Some(value) = &issue.value {
            eprintln!("      value={value}");
        }
        eprintln!("      expected={}", issue.expected);
        eprintln!("      fix={}", issue.fix);
    }
}

/// 打印六个槽位的最终绑定（§3.10 可审计）。
fn print_slot_rows(lang: Language, rows: &[SlotRow]) {
    println!("{}", tf(lang, "槽位绑定（§3.10）：", &[]));
    for row in rows {
        let model = row
            .model_id
            .clone()
            .unwrap_or_else(|| tf(lang, "未绑定", &[]));
        let source = row.source.clone().unwrap_or_else(|| "-".to_owned());
        let mode = row.mode.clone().unwrap_or_else(|| "-".to_owned());
        let adapter = row.adapter.clone().unwrap_or_else(|| "-".to_owned());
        let binding = if row.explicit {
            tf(lang, "显式", &[])
        } else {
            tf(lang, "自动", &[])
        };
        println!(
            "  {:<9} {}  {}  {}  {}  {}",
            row.slot,
            tf(lang, "模型={v}", &[("v", model)]),
            tf(lang, "来源={v}", &[("v", source)]),
            tf(lang, "模式={v}", &[("v", mode)]),
            tf(lang, "适配器={v}", &[("v", adapter)]),
            tf(lang, "绑定={v}", &[("v", binding)]),
        );
    }
}

fn slots_json(rows: &[SlotRow]) -> serde_json::Value {
    let items: Vec<serde_json::Value> = rows.iter().map(|r| r.json()).collect();
    json!({"ok": true, "slots": items})
}

/// JSON 输出按用户语言本地化其中的文案（§18.1b）。
fn pretty_json(lang: Language, value: &serde_json::Value) -> String {
    serde_json::to_string_pretty(&localize_value(lang, value)).unwrap_or_default()
}

/// 一行摘要，用于启动日志（§18.9：槽位绑定写进启动日志）。
fn slot_summary(lang: Language, rows: &[SlotRow]) -> String {
    rows.iter()
        .map(|row| {
            let model = row
                .model_id
                .clone()
                .unwrap_or_else(|| tf(lang, "未绑定", &[]));
            match &row.source {
                Some(source) => format!("{}=[{}]({})", row.slot, model, source),
                None => format!("{}=[{}]", row.slot, model),
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// 判定能力状态：decide 槽位是否绑定了模型（§18.1）。
fn decide_state(lang: Language, rows: &[SlotRow]) -> String {
    let bound = rows
        .iter()
        .find(|row| row.slot == SlotKind::Decide.as_str())
        .and_then(|row| row.model_id.clone());
    match bound {
        Some(model) => tf(lang, "decide 已绑定模型 {model}", &[("model", model)]),
        None => tf(lang, "decide 未绑定模型（工具按 §3.8 走机械路径）", &[]),
    }
}

async fn run_serve(ctx: ToolContext) -> anyhow::Result<()> {
    let mut server = McpServer::new(ctx);
    server.serve_stdio().await
}

async fn run_call(
    tool: &str,
    args: serde_json::Value,
    ctx: ToolContext,
) -> anyhow::Result<ExitCode> {
    let resp = dispatch(tool, &args, &ctx).await;
    match resp {
        Ok(r) => {
            for block in &r.content {
                match block {
                    toolgate_core::tools::Content::Text(t) => println!("{t}"),
                    toolgate_core::tools::Content::Image { mime_type, .. } => {
                        println!("[image block: {mime_type}]")
                    }
                }
            }
            if !r.meta.is_null() {
                eprintln!("meta: {}", serde_json::to_string_pretty(&r.meta)?);
            }
            Ok(if r.is_error {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            })
        }
        Err(e) => {
            eprintln!("{e}");
            Ok(ExitCode::FAILURE)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// §17.2：status 的 JSON 形态与 MCP serverInfo.identity 同形；
    /// 取不到身份时给 null 而不是 0（0 会被读成 root）。
    #[test]
    fn status_json_reports_integer_identity() {
        let payload = status_json(ProcessIdentity {
            uid: Some(501),
            gid: Some(20),
        });
        assert_eq!(payload["ok"].as_bool(), Some(true));
        assert_eq!(payload["identity"]["uid"].as_u64(), Some(501));
        assert_eq!(payload["identity"]["gid"].as_u64(), Some(20));

        let unknown = status_json(ProcessIdentity {
            uid: None,
            gid: None,
        });
        assert!(
            unknown["identity"]["uid"].is_null(),
            "取不到身份不应编造 0：{unknown}"
        );
    }

    /// §17.2：本机探测拿到的是真实 uid/gid，不是占位值。
    #[test]
    fn status_reports_probed_identity() {
        let identity = toolgate_core::tools::bash::identity();
        assert!(identity.uid.is_some(), "uid 探测失败");
        assert!(identity.gid.is_some(), "gid 探测失败");
    }

    /// 文本形态按语言二选一，取不到身份给「未知 / unknown」。
    #[test]
    fn status_line_follows_language() {
        let identity = ProcessIdentity {
            uid: Some(501),
            gid: Some(20),
        };
        assert!(
            identity_line(Language::Zh, identity).starts_with("身份：uid=501"),
            "中文文案不对"
        );
        assert!(
            identity_line(Language::En, identity).starts_with("Identity: uid=501"),
            "英文文案不对"
        );
        let unknown = ProcessIdentity {
            uid: None,
            gid: None,
        };
        assert!(identity_line(Language::Zh, unknown).contains("未知"));
        assert!(identity_line(Language::En, unknown).contains("unknown"));
    }

    /// §17.2：配置的允许根是追加到默认根上，不是替换；空清单保持默认。
    #[test]
    fn allowed_roots_append_to_defaults() {
        let defaults = vec![PathBuf::from("/work")];
        assert_eq!(merge_allowed_roots(&defaults, &[]), defaults);
        assert_eq!(
            merge_allowed_roots(&defaults, &["/data".to_owned()]),
            vec![PathBuf::from("/work"), PathBuf::from("/data")]
        );
    }
}
