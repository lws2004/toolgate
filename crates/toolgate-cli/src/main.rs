//! toolgate 命令行入口。
//!
//! 三个子命令：
//!   serve            以 MCP stdio server 方式运行（供 agent 接入）
//!   list             列出工具及其入参 schema
//!   call <tool> [json] 直接调用一次工具（调试用）

use std::process::ExitCode;

use toolgate_core::locale::{Language, localize_value, tf};
use toolgate_core::tools::{ToolContext, all_specs, dispatch};
use toolgate_mcp::McpServer;

const USAGE: &str = "\
用法: toolgate <子命令> [参数]

子命令:
  serve                 以 MCP stdio server 方式运行，供 agent 客户端接入
  list                  列出全部工具与入参 schema
  call <工具名> [JSON]   直接调用一次工具，参数是 JSON 对象（调试用）

示例:
  toolgate serve
  toolgate call read '{\"path\":\"Cargo.toml\"}'
";

const USAGE_EN: &str = "\
Usage: toolgate <subcommand> [args]

Subcommands:
  serve                 Run as an MCP stdio server for agent clients
  list                  List all tools and their input schemas
  call <tool> [JSON]     Invoke one tool directly (debug only)

Examples:
  toolgate serve
  toolgate call read '{\"path\":\"Cargo.toml\"}'
";

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
        "serve" => match run_serve().await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!(
                    "{}",
                    tf(lang, "MCP server 异常退出：{e}", &[("e", e.to_string())])
                );
                ExitCode::FAILURE
            }
        },
        "list" => {
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
