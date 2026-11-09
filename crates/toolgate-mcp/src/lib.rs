//! MCP 传输层：把 toolgate-core 的工具表暴露成 MCP server。
//!
//! 走 stdio + JSON-RPC 2.0，这是 MCP 客户端（pi、DSH、Claude Code 等）
//! 最通用的接入方式。stdout 只写协议消息，日志一律走 stderr——
//! 混在一起会让客户端的 JSON 解析直接失败。

use std::io::Write as _;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use toolgate_core::compat::{
    CompatConfig, LATEST_VERSION, SUPPORTED_VERSIONS, check_protocol_version,
    default_decision_input, parse_request_meta, resolve_intent,
};
use toolgate_core::decision::{Intent, decide_output};
use toolgate_core::error::{error_response_with_lang, govern_error};
use toolgate_core::locale::{Language, localize_value, t};
use toolgate_core::notice::budgeted_notices;
use toolgate_core::tools::{ToolContext, all_specs, dispatch};

/// 一个运行中的 MCP server 实例。
pub struct McpServer {
    ctx: ToolContext,
    /// v2 首版默认 intent=raw（§4.2 兼容性约束）。
    compat: CompatConfig,
}

impl McpServer {
    pub fn new(ctx: ToolContext) -> Self {
        Self {
            ctx,
            compat: CompatConfig::default(),
        }
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
                    let resp = error_response(
                        Value::Null,
                        -32700,
                        &t(Language::from_env(), &format!("JSON 解析失败：{e}")),
                    );
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

        // 2026-07-28 无握手：每个请求独立校验版本；initialize 走旧版协商。
        if let Err(message) = check_protocol_version(method, request) {
            return error_response(id, -32600, &t(Language::from_env(), &message));
        }
        let params = request.get("params").cloned().unwrap_or(Value::Null);
        let meta = parse_request_meta(&params);
        let lang = meta.effective_language();

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
                    .unwrap_or(LATEST_VERSION);
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
                        let description = match lang {
                            Language::En => s.description_en,
                            Language::Zh => s.description,
                        };
                        json!({
                            "name": s.name,
                            "description": description,
                            "inputSchema": localize_value(lang, &s.input_schema),
                            "outputSchema": localize_value(lang, &s.output_schema()),
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

                // 阶段 1.5：intent 缺省 raw，v1 客户端不改参数行为不变。
                let intent = match resolve_intent(&args, &self.compat) {
                    Ok(intent) => intent,
                    Err(message) => return error_response(id, -32602, &t(lang, &message)),
                };

                // 工具级错误走 isError，而不是 JSON-RPC 错误——模型据此自行修正。
                // Ok 与 Err 都收敛成 ToolResponse，序列化只有一处。
                // 把 _meta 的上游视觉能力注入本次调用上下文（§18.1b）。
                let mut call_ctx = self.ctx.clone();
                call_ctx.upstream_vision = meta.upstream_capability().vision;
                call_ctx.language = lang;
                let resp = match dispatch(name, &args, &call_ctx).await {
                    Ok(resp) => resp,
                    Err(e) => error_response_with_lang(&govern_error(&e), lang),
                };
                let content: Vec<Value> = resp.content.iter().map(|c| c.to_mcp()).collect();
                let mut result = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": content,
                        "isError": resp.is_error,
                        "structuredContent": resp.meta,
                    }
                });

                // 新客户端 intent=auto：协商告知必须同时进 content 与
                // structuredContent（§4.4 / §4.4b 一个事实源、两个通道）。
                if intent == Intent::Auto {
                    let decision = decide_output(&default_decision_input(intent, &meta));
                    if !decision.decision_notices.is_empty() {
                        let ch = budgeted_notices(decision.decision_notices);
                        result["result"]["content"]
                            .as_array_mut()
                            .expect("content 是数组")
                            .push(json!({ "type": "text", "text": ch.text }));
                        let sc = &mut result["result"]["structuredContent"];
                        if !sc.is_object() {
                            *sc = json!({});
                        }
                        sc["notices"] = ch.structured;
                        sc["noticeOverflow"] = json!(ch.overflow);
                    }
                }
                result
            }
            other => error_response(id, -32601, &t(lang, &format!("不支持的方法：{other}"))),
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
        let resp = call(
            &mut s,
            json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": { "protocolVersion": "2026-07-28" }
            }),
        )
        .await;
        assert_eq!(resp["result"]["protocolVersion"], json!("2026-07-28"));
        assert_eq!(resp["result"]["serverInfo"]["name"], json!("toolgate"));
    }

    #[tokio::test]
    async fn initialize_falls_back_on_unknown_version() {
        let mut s = test_server();
        let resp = call(
            &mut s,
            json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": { "protocolVersion": "1999-01-01" }
            }),
        )
        .await;
        assert_eq!(resp["result"]["protocolVersion"], json!("2026-07-28"));
    }

    #[tokio::test]
    async fn tools_list_returns_six_tools() {
        let mut s = test_server();
        let resp = call(
            &mut s,
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
        )
        .await;
        let tools = resp["result"]["tools"].as_array().expect("tools");
        assert_eq!(tools.len(), 6);
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert_eq!(
            names,
            vec!["read", "write", "edit", "search", "bash", "task"]
        );
    }

    #[tokio::test]
    async fn tools_list_declares_output_schema_and_intent() {
        let mut s = test_server();
        let resp = call(
            &mut s,
            json!({ "jsonrpc": "2.0", "id": 6, "method": "tools/list" }),
        )
        .await;
        for tool in resp["result"]["tools"].as_array().expect("tools") {
            assert_eq!(
                tool["outputSchema"]["type"],
                json!("object"),
                "{} 缺 outputSchema",
                tool["name"]
            );
            assert_eq!(
                tool["inputSchema"]["properties"]["intent"]["type"],
                json!("string"),
                "{} 缺 intent 参数",
                tool["name"]
            );
        }
    }

    #[tokio::test]
    async fn tools_call_carries_unified_negotiation_meta() {
        let mut s = test_server();
        let resp = call(
            &mut s,
            json!({
                "jsonrpc": "2.0", "id": 7, "method": "tools/call",
                "params": {
                    "name": "write",
                    "arguments": { "path": "neg-meta.txt", "content": "hi" }
                }
            }),
        )
        .await;
        let sc = &resp["result"]["structuredContent"];
        assert_eq!(sc["intent"], json!("raw"));
        assert_eq!(sc["slmStatus"]["decide"], json!("unavailable"));
        assert!(sc["confidence"].is_null());
    }

    #[tokio::test]
    async fn auto_notices_reach_both_channels() {
        let mut s = test_server();
        let resp = call(
            &mut s,
            json!({
                "jsonrpc": "2.0", "id": 8, "method": "tools/call",
                "params": {
                    "name": "write",
                    "arguments": {
                        "path": "auto-channels.txt",
                        "content": "hi",
                        "intent": "auto"
                    }
                }
            }),
        )
        .await;
        let blocks = resp["result"]["content"]
            .as_array()
            .expect("content 是数组");
        let joined: String = blocks
            .iter()
            .map(|c| c["text"].as_str().unwrap_or(""))
            .collect();
        assert!(
            joined.contains("SLOT_UNAVAILABLE"),
            "content 通道缺少协商告知"
        );
        assert_eq!(
            resp["result"]["structuredContent"]["noticeOverflow"],
            json!(0)
        );
    }

    #[tokio::test]
    async fn user_language_controls_specs_and_errors() {
        let mut s = test_server();
        let list = call(
            &mut s,
            json!({
                "jsonrpc": "2.0", "id": 9, "method": "tools/list",
                "params": { "_meta": { "toolgate/language": "en" } }
            }),
        )
        .await;
        let tools = list["result"]["tools"].as_array().expect("tools");
        let write_spec = tools.iter().find(|t| t["name"] == "write").expect("write");
        assert!(
            write_spec["description"]
                .as_str()
                .unwrap()
                .starts_with("Can:"),
            "工具描述未按用户语言切换"
        );
        assert_eq!(
            write_spec["inputSchema"]["properties"]["content"]["description"],
            json!("Full file content")
        );

        let err = call(
            &mut s,
            json!({
                "jsonrpc": "2.0", "id": 10, "method": "tools/call",
                "params": {
                    "name": "nope", "arguments": {},
                    "_meta": { "toolgate/language": "en" }
                }
            }),
        )
        .await;
        let text = err["result"]["content"][0]["text"].as_str().unwrap_or("");
        assert!(text.starts_with("Fact:"), "错误三段式标签未本地化: {text}");
        assert!(
            text.contains("Call tools/list"),
            "nextSteps 未本地化: {text}"
        );
    }

    #[tokio::test]
    async fn user_language_controls_tool_content() {
        let mut s = test_server();
        let resp = call(
            &mut s,
            json!({
                "jsonrpc": "2.0", "id": 11, "method": "tools/call",
                "params": {
                    "name": "write",
                    "arguments": { "path": "i18n.txt", "content": "hi" },
                    "_meta": { "toolgate/language": "en" }
                }
            }),
        )
        .await;
        let text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
        assert!(
            text.starts_with("Created"),
            "工具 content 未按用户语言切换: {text}"
        );
    }

    #[tokio::test]
    async fn user_language_controls_interpolated_errors() {
        let mut s = test_server();
        let resp = call(
            &mut s,
            json!({
                "jsonrpc": "2.0", "id": 12, "method": "tools/call",
                "params": {
                    "name": "bash",
                    "arguments": { "binary": "definitely-not-a-real-binary", "intent": "raw" },
                    "_meta": { "toolgate/language": "en" }
                }
            }),
        )
        .await;
        let fact = resp["result"]["structuredContent"]["error"]["fact"]
            .as_str()
            .unwrap_or("");
        assert!(
            fact.starts_with("Cannot start"),
            "含插值 fact 未按用户语言翻译: {fact}"
        );
    }

    #[tokio::test]
    async fn unknown_tool_returns_is_error() {
        let mut s = test_server();
        let resp = call(
            &mut s,
            json!({
                "jsonrpc": "2.0", "id": 3, "method": "tools/call",
                "params": { "name": "nope", "arguments": {} }
            }),
        )
        .await;
        assert_eq!(resp["result"]["isError"], json!(true));
    }

    #[tokio::test]
    async fn unknown_tool_error_carries_next_steps() {
        let mut s = test_server();
        let resp = call(
            &mut s,
            json!({
                "jsonrpc": "2.0", "id": 5, "method": "tools/call",
                "params": { "name": "nope", "arguments": {} }
            }),
        )
        .await;
        let err = &resp["result"]["structuredContent"]["error"];
        assert_eq!(err["code"], json!("UNKNOWN_TOOL"));
        assert!(err["fact"].is_string());
        assert!(err["reason"].is_string());
        assert!(err["nextSteps"].as_array().is_some_and(|a| !a.is_empty()));
        assert!(
            !resp["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn v1_call_without_intent_keeps_v1_shape() {
        let mut s = test_server();
        let resp = call(
            &mut s,
            json!({
                "jsonrpc": "2.0", "id": 6, "method": "tools/call",
                "params": { "name": "nope", "arguments": {} }
            }),
        )
        .await;
        assert_eq!(resp["result"]["isError"], json!(true));
        assert!(resp["result"]["structuredContent"].get("notices").is_none());
    }

    #[tokio::test]
    async fn new_client_meta_with_auto_negotiates() {
        let mut s = test_server();
        let resp = call(
            &mut s,
            json!({
                "jsonrpc": "2.0", "id": 7, "method": "tools/call",
                "params": {
                    "name": "nope",
                    "arguments": { "intent": "auto" },
                    "_meta": {
                        "protocolVersion": "2026-07-28",
                        "toolgate/upstream": { "vision": false },
                        "toolgate/turn": {
                            "turn_id": "turn_01", "seq": 1,
                            "budget": { "max_judgments": 8 }
                        }
                    }
                }
            }),
        )
        .await;
        assert_eq!(resp["result"]["isError"], json!(true));
        let notices = resp["result"]["structuredContent"]["notices"]
            .as_array()
            .expect("auto 应产出协商告知");
        assert!(!notices.is_empty());
        assert!(resp["error"].is_null());
    }

    #[tokio::test]
    async fn unknown_protocol_version_is_rejected() {
        let mut s = test_server();
        let resp = call(
            &mut s,
            json!({
                "jsonrpc": "2.0", "id": 8, "method": "tools/call",
                "params": {
                    "name": "nope", "arguments": {},
                    "protocolVersion": "1999-01-01"
                }
            }),
        )
        .await;
        assert_eq!(resp["error"]["code"], json!(-32600));
        assert!(
            resp["error"]["message"]
                .as_str()
                .unwrap()
                .contains("2026-07-28")
        );
    }

    #[tokio::test]
    async fn unknown_intent_fails_loud() {
        let mut s = test_server();
        let resp = call(
            &mut s,
            json!({
                "jsonrpc": "2.0", "id": 9, "method": "tools/call",
                "params": { "name": "nope", "arguments": { "intent": "bogus" } }
            }),
        )
        .await;
        assert_eq!(resp["error"]["code"], json!(-32602));
        assert!(resp["error"]["message"].as_str().unwrap().contains("raw"));
    }

    #[tokio::test]
    async fn unknown_method_returns_jsonrpc_error() {
        let mut s = test_server();
        let resp = call(
            &mut s,
            json!({ "jsonrpc": "2.0", "id": 4, "method": "bogus" }),
        )
        .await;
        assert_eq!(resp["error"]["code"], json!(-32601));
    }
}
