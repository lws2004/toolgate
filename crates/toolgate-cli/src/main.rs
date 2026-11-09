//! toolgate 命令行入口。
//!
//! 四个子命令：
//!   serve              以 MCP stdio server 方式运行（供 agent 接入）
//!   list               列出工具及其入参 schema
//!   call <tool> [json] 直接调用一次工具（调试用）
//!   config check       校验配置文件并打印五个槽位的最终绑定（§18.9）
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
use toolgate_core::slots::SlotKind;
use toolgate_core::tools::{ToolContext, all_specs, dispatch};
use toolgate_mcp::McpServer;

const USAGE: &str = "\
用法: toolgate <子命令> [参数]

子命令:
  serve                 以 MCP stdio server 方式运行，供 agent 客户端接入
  list                  列出全部工具与入参 schema
  call <工具名> [JSON]   直接调用一次工具，参数是 JSON 对象（调试用）
  config check          校验配置文件并打印五个槽位的最终绑定（§18.9）

选项:
  --config <路径>       指定配置文件路径（默认 ~/.toolgate/config.yaml）
  --json                config check 以 JSON 输出结果

示例:
  toolgate serve
  toolgate list --config ~/.toolgate/config.yaml
  toolgate config check
  toolgate call read '{\"path\":\"Cargo.toml\"}'
";

const USAGE_EN: &str = "\
Usage: toolgate <subcommand> [args]

Subcommands:
  serve                 Run as an MCP stdio server for agent clients
  list                  List all tools and their input schemas
  call <tool> [JSON]     Invoke one tool directly (debug only)
  config check          Validate the config file and print the five slot bindings (§18.9)

Options:
  --config <path>       Config file path (default ~/.toolgate/config.yaml)
  --json                Print the config check result as JSON

Examples:
  toolgate serve
  toolgate list --config ~/.toolgate/config.yaml
  toolgate config check
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

    /// 五个槽位全空：无配置文件时的内置默认（§3.8）。
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
            let rows = match load_slots(lang, config_path.as_deref(), false) {
                Ok(rows) => rows,
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
            match run_serve().await {
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
        "list" => {
            let config_path = match parse_config_arg(lang, &args[1..]) {
                Ok(p) => p,
                Err(code) => return code,
            };
            let rows = match load_slots(lang, config_path.as_deref(), false) {
                Ok(rows) => rows,
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
            let Some(tool) = args.get(1) else {
                eprintln!(
                    "{}",
                    tf(
                        lang,
                        "call 需要工具名。\n\n{USAGE}",
                        &[("USAGE", usage.to_owned())]
                    )
                );
                return ExitCode::FAILURE;
            };
            let raw = args.get(2).map(String::as_str).unwrap_or("{}");
            let parsed: serde_json::Value = match serde_json::from_str(raw) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!(
                        "{}",
                        tf(lang, "参数不是合法 JSON：{e}", &[("e", e.to_string())])
                    );
                    return ExitCode::FAILURE;
                }
            };
            match run_call(tool, parsed, lang).await {
                Ok(code) => code,
                Err(e) => {
                    eprintln!("{e}");
                    ExitCode::FAILURE
                }
            }
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

/// 加载配置并取出五个槽位的绑定视图（serve / list 用）。
fn load_slots(
    lang: Language,
    config_path: Option<&Path>,
    json_out: bool,
) -> Result<Vec<SlotRow>, ExitCode> {
    match load_cli_config(lang, config_path) {
        ConfigOutcome::Resolved(cfg) => Ok(SlotRow::from_config(&cfg)),
        ConfigOutcome::Missing => {
            if !json_out {
                eprintln!(
                    "{}",
                    tf(lang, "未找到配置文件，使用内置默认（五个槽位全空）", &[])
                );
            }
            Ok(SlotRow::empty())
        }
        ConfigOutcome::Failed(errors) => {
            report_errors(lang, &errors, json_out);
            Err(ExitCode::FAILURE)
        }
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
                    tf(lang, "未找到配置文件，使用内置默认（五个槽位全空）", &[])
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

/// 打印五个槽位的最终绑定（§3.10 可审计）。
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

async fn run_serve() -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let mut ctx = ToolContext::new(cwd)?;
    ctx.language = Language::from_env();
    let mut server = McpServer::new(ctx);
    server.serve_stdio().await
}

async fn run_call(tool: &str, args: serde_json::Value, lang: Language) -> anyhow::Result<ExitCode> {
    let cwd = std::env::current_dir()?;
    let mut ctx = ToolContext::new(cwd)?;
    ctx.language = lang;
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
