//! MCP 传输层：把 toolgate-core 的工具表暴露成 MCP server。
//!
//! 走 stdio + JSON-RPC 2.0，这是 MCP 客户端（pi、DSH、Claude Code 等）
//! 最通用的接入方式。stdout 只写协议消息，日志一律走 stderr——
//! 混在一起会让客户端的 JSON 解析直接失败。

use std::io::Write as _;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use toolgate_core::tools::{ToolContext, all_specs, dispatch};

/// 支持的 MCP 协议版本，按新到旧排列。
const SUPPORTED_VERSIONS: &[&str] = &["2026-07-28", "2025-11-25", "2025-06-18"];

/// 一个运行中的 MCP server 实例。
pub struct McpServer {
    ctx: ToolContext,
}

impl McpServer {
    pub fn new(ctx: ToolContext) -> Self {
        Self { ctx }
    }

    /// 在 stdin/stdout 上跑主循环，直到 stdin 关闭。
    pub async fn serve_stdio(&mut self) -> anyhow::Result<()> {
        let stdin = tokio::io::stdin();
        let mut stdout = tokio::io::stdout();
        let mut reader = BufReader::new(stdin);
        let mut line = String::new();

        loop {
            line.clear();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                break; // 客户端关闭了 stdin
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let request: Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(e) => {
                    let resp = error_response(Value::Null, -32700, &format!("JSON 解析失败：{e}"));
                    write_line(&mut stdout, &resp).await?;
                    continue;
                }
            };

            // 通知类消息（无 id）不回复。
            let Some(id) = request.get("id").cloned() else {
                continue;
            };

            let response = self.handle(&request, id).await;
            write_line(&mut stdout, &response).await?;
        }
        Ok(())
    }

    async fn handle(&mut self, request: &Value, id: Value) -> Value {
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        match method {
            "initialize" => {
                let requested = request
                    .pointer("/params/protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let version = SUPPORTED_VERSIONS
                    .iter()
                    .find(|v| **v == requested)
                    .copied()
                    .unwrap_or(SUPPORTED_VERSIONS[0]);
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "protocolVersion": version,
                        "capabilities": { "tools": { "listChanged": false } },
                        "serverInfo": {
                            "name": "toolgate",
                            "version": env!("CARGO_PKG_VERSION"),
                        }
                    }
                })
            }
            "ping" => json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
            "tools/list" => {
                let tools: Vec<Value> = all_specs()
                    .into_iter()
                    .map(|s| {
                        json!({
                            "name": s.name,
                            "description": s.description,
                            "inputSchema": s.input_schema,
                        })
                    })
                    .collect();
                json!({ "jsonrpc": "2.0", "id": id, "result": { "tools": tools } })
            }
            "tools/call" => {
                let name = request
                    .pointer("/params/name")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let args = request
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));

                match dispatch(name, &args, &self.ctx).await {
                    Ok(resp) => {
                        let content: Vec<Value> = resp.content.iter().map(|c| c.to_mcp()).collect();
                        json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "content": content,
                                "isError": resp.is_error,
                                "structuredContent": resp.meta,
                            }
                        })
                    }
                    Err(e) => {
                        // 工具级错误走 isError，而不是 JSON-RPC 错误——
                        // 这样模型能看到错误文本并自行修正。
                        json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "content": [{ "type": "text", "text": e.to_string() }],
                                "isError": true,
                            }
                        })
                    }
                }
            }
            other => error_response(id, -32601, &format!("不支持的方法：{other}")),
        }
    }
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    })
}

async fn write_line<W: AsyncWriteExt + Unpin>(w: &mut W, v: &Value) -> std::io::Result<()> {
    let mut s = serde_json::to_string(v).map_err(std::io::Error::other)?;
    s.push('\n');
    w.write_all(s.as_bytes()).await?;
    w.flush().await
}

/// 便于调试：把一条消息同步写到 stderr，不污染协议通道。
pub fn log_stderr(msg: &str) {
    let _ = writeln!(std::io::stderr(), "{msg}");
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn call(server: &mut McpServer, req: Value) -> Value {
        let id = req.get("id").cloned().unwrap_or(json!(1));
        server.handle(&req, id).await
    }

    fn test_server() -> McpServer {
        let dir = std::env::temp_dir().join(format!("tg-mcp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        McpServer::new(ToolContext::new(&dir).expect("ctx"))
    }

    #[tokio::test]
    async fn initialize_negotiates_version() {
        let mut s = test_server();
        let resp = call(&mut s, json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": "2026-07-28" }
        })).await;
        assert_eq!(resp["result"]["protocolVersion"], json!("2026-07-28"));
        assert_eq!(resp["result"]["serverInfo"]["name"], json!("toolgate"));
    }

    #[tokio::test]
    async fn initialize_falls_back_on_unknown_version() {
        let mut s = test_server();
        let resp = call(&mut s, json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": "1999-01-01" }
        })).await;
        assert_eq!(resp["result"]["protocolVersion"], json!("2026-07-28"));
    }

    #[tokio::test]
    async fn tools_list_returns_six_tools() {
        let mut s = test_server();
        let resp = call(&mut s, json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" })).await;
        let tools = resp["result"]["tools"].as_array().expect("tools");
        assert_eq!(tools.len(), 6);
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert_eq!(names, vec!["read", "write", "edit", "search", "bash", "task"]);
    }

    #[tokio::test]
    async fn unknown_tool_returns_is_error() {
        let mut s = test_server();
        let resp = call(&mut s, json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": { "name": "nope", "arguments": {} }
        })).await;
        assert_eq!(resp["result"]["isError"], json!(true));
    }

    #[tokio::test]
    async fn unknown_method_returns_jsonrpc_error() {
        let mut s = test_server();
        let resp = call(&mut s, json!({ "jsonrpc": "2.0", "id": 4, "method": "bogus" })).await;
        assert_eq!(resp["error"]["code"], json!(-32601));
    }
}
