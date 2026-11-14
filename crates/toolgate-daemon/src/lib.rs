//! toolgate daemon：常驻进程外的感知治理层（§0–§2、§18.2 三形态之主路径）。
//!
//! 传输层手写极简 HTTP/1.1（禁止层不含 Web 框架，故不引入 axum/hyper）：在 Unix
//! socket 上监听，暴露两个**非 MCP** 接缝——读侧 POST /govern、写侧 POST /pre、
//! 以及 GET /health。薄适配器（独立仓，一期 pi/dsh）把宿主内置工具的
//! 输入输出搬运到这里，再搬回宿主上下文。核心仓零框架依赖由本 crate 承接。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

use toolgate_core::govern::{GovernRequest, govern_with};

mod delegate;

use delegate::HttpDelegate;
use toolgate_core::precheck::{PreRequest, pre};
use toolgate_core::spill::SpillStore;

/// 服务端共享状态：快照/落盘存储与工作目录。
struct ServerCtx {
    spill: SpillStore,
    cwd: PathBuf,
}

/// 启动 daemon，监听 Unix socket 直至进程被终止。
pub async fn serve(socket_path: impl AsRef<Path>) -> anyhow::Result<()> {
    let socket_path = socket_path.as_ref().to_path_buf();
    // 清理陈旧 socket，避免重新绑定时 EADDRINUSE。
    if socket_path.exists() {
        let _ = std::fs::remove_file(&socket_path);
    }
    if let Some(parent) = socket_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let listener = UnixListener::bind(&socket_path)?;

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let spill = SpillStore::with_default_dir()?;
    let ctx = Arc::new(ServerCtx { spill, cwd });

    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let ctx = Arc::clone(&ctx);
                tokio::spawn(async move {
                    if let Err(err) = handle_conn(stream, ctx).await {
                        eprintln!("[toolgate-daemon] 连接处理失败：{err}");
                    }
                });
            }
            Err(err) => {
                eprintln!("[toolgate-daemon] accept 失败：{err}");
            }
        }
    }
}

async fn handle_conn(mut stream: UnixStream, ctx: Arc<ServerCtx>) -> anyhow::Result<()> {
    let (method, path, body) = read_request(&mut stream).await?;

    let (status, payload) = route(&method, &path, &body, &ctx);

    write_response(&mut stream, status, &payload).await?;
    Ok(())
}

fn route(method: &str, path: &str, body: &[u8], ctx: &ServerCtx) -> (u16, serde_json::Value) {
    if method == "GET" && (path == "/health" || path == "/") {
        return (200, serde_json::json!({ "ok": true, "service": "toolgate-daemon" }));
    }
    if method != "POST" {
        return (
            405,
            serde_json::json!({ "error": { "code": "METHOD_NOT_ALLOWED", "message": "仅支持 POST /govern 与 POST /pre" } }),
        );
    }
    match path {
        "/govern" => match serde_json::from_slice::<GovernRequest>(body) {
            Ok(req) => {
                let resp = govern_with(&req, &ctx.spill, Some(&HttpDelegate));
                (200, serde_json::to_value(resp).unwrap_or_else(|e| bad_req(&e.to_string())))
            }
            Err(e) => (400, bad_req(&format!("govern 请求解析失败：{e}"))),
        },
        "/pre" => match serde_json::from_slice::<PreRequest>(body) {
            Ok(req) => {
                let resp = pre(&req, &ctx.spill, &ctx.cwd);
                (200, serde_json::to_value(resp).unwrap_or_else(|e| bad_req(&e.to_string())))
            }
            Err(e) => (400, bad_req(&format!("pre 请求解析失败：{e}"))),
        },
        _ => (
            404,
            serde_json::json!({ "error": { "code": "NOT_FOUND", "message": format!("未知路径：{path}") } }),
        ),
    }
}

fn bad_req(msg: &str) -> serde_json::Value {
    serde_json::json!({ "error": { "code": "BAD_REQUEST", "message": msg } })
}

/// 读一个 HTTP 请求：解析起始行 + Content-Length，读满 body。
async fn read_request(stream: &mut UnixStream) -> anyhow::Result<(String, String, Vec<u8>)> {
    let mut buf: Vec<u8> = Vec::new();
    let mut tmp = [0u8; 8192];
    loop {
        if let Some(header_end) = find_header_end(&buf) {
            let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
            let mut lines = head.lines();
            let request_line = lines.next().unwrap_or("");
            let mut parts = request_line.split_whitespace();
            let method = parts.next().unwrap_or("").to_owned();
            let path = parts.next().unwrap_or("/").to_owned();
            let content_length = parse_content_length(&head);
            let body_start = header_end + 4;
            while buf.len() < body_start + content_length {
                let n = stream.read(&mut tmp).await?;
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
            }
            let body = buf[body_start..].to_vec();
            return Ok((method, path, body));
        }
        if buf.len() > 64 * 1024 * 1024 {
            anyhow::bail!("请求头过大");
        }
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            anyhow::bail!("连接在请求头读全前关闭");
        }
        buf.extend_from_slice(&tmp[..n]);
    }
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn parse_content_length(head: &str) -> usize {
    for line in head.lines() {
        let lower = line.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("content-length:")
            && let Ok(v) = rest.trim().parse::<usize>()
        {
            return v;
        }
    }
    0
}

async fn write_response(
    stream: &mut UnixStream,
    status: u16,
    payload: &serde_json::Value,
) -> anyhow::Result<()> {
    let body = serde_json::to_vec(payload)?;
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "OK",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(&body).await?;
    stream.flush().await?;
    Ok(())
}
