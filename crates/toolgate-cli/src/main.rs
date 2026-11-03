//! toolgate 命令行入口。
//!
//! 三个子命令：
//!   serve            以 MCP stdio server 方式运行（供 agent 接入）
//!   list             列出工具及其入参 schema
//!   call <tool> [json] 直接调用一次工具（调试用）

use std::process::ExitCode;

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

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first() else {
        eprintln!("{USAGE}");
        return ExitCode::FAILURE;
    };

    match cmd.as_str() {
        "serve" => match run_serve().await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("MCP server 异常退出：{e}");
                ExitCode::FAILURE
            }
        },
        "list" => {
            for spec in all_specs() {
                println!("{}\n  {}", spec.name, spec.description);
                println!(
                    "  schema: {}",
                    serde_json::to_string(&spec.input_schema).unwrap_or_default()
                );
                println!();
            }
            ExitCode::SUCCESS
        }
        "call" => {
            let Some(tool) = args.get(1) else {
                eprintln!("call 需要工具名。\n\n{USAGE}");
                return ExitCode::FAILURE;
            };
            let raw = args.get(2).map(String::as_str).unwrap_or("{}");
            let parsed: serde_json::Value = match serde_json::from_str(raw) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("参数不是合法 JSON：{e}");
                    return ExitCode::FAILURE;
                }
            };
            match run_call(tool, parsed).await {
                Ok(code) => code,
                Err(e) => {
                    eprintln!("{e}");
                    ExitCode::FAILURE
                }
            }
        }
        "-h" | "--help" | "help" => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("未知子命令：{other}\n\n{USAGE}");
            ExitCode::FAILURE
        }
    }
}

async fn run_serve() -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let ctx = ToolContext::new(cwd)?;
    let mut server = McpServer::new(ctx);
    server.serve_stdio().await
}

async fn run_call(tool: &str, args: serde_json::Value) -> anyhow::Result<ExitCode> {
    let cwd = std::env::current_dir()?;
    let ctx = ToolContext::new(cwd)?;
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
