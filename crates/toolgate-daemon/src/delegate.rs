//! 反向委派客户端（§18.7）：把单条内容回投宿主会话模型。
//!
//! core 只定义 `Delegate` trait，本文件给出 daemon 侧实现：支持
//! `unix:/abs.sock` 与 `http://127.0.0.1:port` 两种回环端点；同步阻塞 + 超时，
//! 失败即返回 Err，让 core 降级到 L0（§3.8 / §4.6）。

use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use serde_json::{Value, json};
use toolgate_core::govern::Delegate;

/// 单次委派的超时与输出预算（由 daemon 决定，宿主不得无上限）。
const TIMEOUT: Duration = Duration::from_secs(8);
const MAX_OUTPUT_TOKENS: u32 = 512;

/// 默认委派实现：按 `policy.delegate.endpoint` 建立回环连接，POST /delegate。
pub struct HttpDelegate;

impl Delegate for HttpDelegate {
    fn call(
        &self,
        endpoint: &str,
        task: &str,
        token: Option<&str>,
        input: Value,
    ) -> Result<Value, String> {
        let payload = json!({
            "task": task,
            "token": token,
            "input": input,
            "bounds": { "maxOutputTokens": MAX_OUTPUT_TOKENS, "timeoutMs": TIMEOUT.as_millis() as u64 },
        });
        let body = serde_json::to_vec(&payload).map_err(|e| e.to_string())?;
        let response = post(endpoint, "/delegate", &body)?;
        serde_json::from_slice::<Value>(&response).map_err(|e| format!("委派响应非 JSON：{e}"))
    }
}

/// 发送一个极简 HTTP/1.1 POST，返回响应体。
fn post(endpoint: &str, path: &str, body: &[u8]) -> Result<Vec<u8>, String> {
    let head = format!(
        "POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    if let Some(sock) = endpoint.strip_prefix("unix:") {
        let mut stream =
            UnixStream::connect(sock).map_err(|e| format!("连接委派 socket 失败：{e}"))?;
        stream.set_read_timeout(Some(TIMEOUT)).ok();
        stream.set_write_timeout(Some(TIMEOUT)).ok();
        exchange(&mut stream, &head, body)
    } else if let Some(rest) = endpoint.strip_prefix("http://") {
        let authority = rest.split('/').next().unwrap_or(rest);
        let mut stream =
            TcpStream::connect(authority).map_err(|e| format!("连接委派端口失败：{e}"))?;
        stream.set_read_timeout(Some(TIMEOUT)).ok();
        stream.set_write_timeout(Some(TIMEOUT)).ok();
        exchange(&mut stream, &head, body)
    } else {
        Err(format!("不支持的委派端点：{endpoint}"))
    }
}

fn exchange<S: Read + Write>(stream: &mut S, head: &str, body: &[u8]) -> Result<Vec<u8>, String> {
    stream
        .write_all(head.as_bytes())
        .map_err(|e| format!("写委派请求头失败：{e}"))?;
    stream
        .write_all(body)
        .map_err(|e| format!("写委派请求体失败：{e}"))?;
    stream.flush().ok();
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| format!("读委派响应失败：{e}"))?;
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| "委派响应缺少头体分隔".to_owned())?;
    Ok(raw[split + 4..].to_vec())
}
