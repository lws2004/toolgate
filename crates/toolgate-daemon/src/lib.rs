//! toolgate daemon：常驻进程外的感知治理层（§0–§2、§18.2 三形态之主路径）。
//!
//! 传输层手写极简 HTTP/1.1（禁止层不含 Web 框架，故不引入 axum/hyper）：在 Unix
//! socket 上监听，暴露两个**非 MCP** 接缝——读侧 POST /govern、写侧 POST /pre、
//! 以及 GET /health。薄适配器（独立仓，一期 pi/dsh）把宿主内置工具的
//! 输入输出搬运到这里，再搬回宿主上下文。核心仓零框架依赖由本 crate 承接。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Instant;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

use toolgate_core::govern::{GovernRequest, GovernResponse, LocalSlots, govern_with_slots};

mod delegate;
mod local_slots;

use delegate::HttpDelegate;
use toolgate_core::config::{default_config_path, load_config};
use toolgate_core::precheck::{PreRequest, pre};
use toolgate_core::slm::SlotClient;
use toolgate_core::spill::SpillStore;

/// 服务端共享状态：快照/落盘存储、工作目录与只读聚合计数。
struct ServerCtx {
    spill: SpillStore,
    cwd: PathBuf,
    /// 本地 SLM 槽位后端（§3.8 降级链第一档）；None 时降级反向委派 / L0。
    local: Option<Arc<dyn LocalSlots>>,
    stats: Stats,
}

/// 只读聚合计数（§18.7）：进程内存态、可随时丢弃，不参与治理决策、无会话状态。
#[derive(Default)]
struct Stats {
    inner: Mutex<StatsInner>,
}

#[derive(Default)]
struct StatsInner {
    requests: u64,
    bytes_in: u64,
    bytes_out: u64,
    latency_ms: u64,
    by_shape: BTreeMap<String, ShapeCounts>,
    slots: BTreeMap<String, SlotCounts>,
    degradations: BTreeMap<String, u64>,
}

#[derive(Default, Clone, Copy)]
struct ShapeCounts {
    requests: u64,
    bytes_in: u64,
    bytes_out: u64,
    latency_ms: u64,
}

#[derive(Default, Clone, Copy)]
struct SlotCounts {
    local: u64,
    delegated: u64,
    unavailable: u64,
}

impl Stats {
    /// 记录一次 /govern：聚合计数 + 追加一行结构化日志（日志失败不阻断请求）。
    fn record_govern(
        &self,
        bytes_in: usize,
        bytes_out: usize,
        latency_ms: u64,
        resp: &GovernResponse,
    ) {
        let shape = resp
            .structured_content
            .get("shape")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_owned();
        let mut degraded: Vec<String> = Vec::new();
        {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.requests += 1;
            inner.bytes_in += bytes_in as u64;
            inner.bytes_out += bytes_out as u64;
            inner.latency_ms += latency_ms;

            let counts = inner.by_shape.entry(shape.clone()).or_default();
            counts.requests += 1;
            counts.bytes_in += bytes_in as u64;
            counts.bytes_out += bytes_out as u64;
            counts.latency_ms += latency_ms;

            if let Some(status) = resp.meta.get("slmStatus").and_then(|v| v.as_object()) {
                for (slot, state) in status {
                    let counts = inner.slots.entry(slot.clone()).or_default();
                    match state.as_str() {
                        Some("local") => counts.local += 1,
                        Some("delegated") => counts.delegated += 1,
                        Some("unavailable") => counts.unavailable += 1,
                        _ => {}
                    }
                }
            }

            // 降级按 notice code 与槽位聚合（§18.7）：notice 是「治理做过的妥协」唯一事实源。
            if let Some(notices) = resp
                .structured_content
                .get("notices")
                .and_then(|v| v.as_array())
            {
                for notice in notices {
                    let code = notice
                        .get("code")
                        .and_then(|v| v.as_str())
                        .unwrap_or("UNKNOWN");
                    let slot = notice
                        .get("params")
                        .and_then(|p| p.get("slot"))
                        .and_then(|v| v.as_str());
                    let key = match slot {
                        Some(s) => format!("{code}:{s}"),
                        None => code.to_owned(),
                    };
                    *inner.degradations.entry(key.clone()).or_default() += 1;
                    degraded.push(key);
                }
            }
        }
        eprintln!(
            "[toolgate-daemon] govern {}",
            serde_json::json!({
                "shape": shape,
                "bytesIn": bytes_in,
                "bytesOut": bytes_out,
                "latencyMs": latency_ms,
                "slots": resp.meta.get("slmStatus").cloned().unwrap_or(serde_json::Value::Null),
                "degraded": degraded,
            })
        );
    }

    fn snapshot(&self) -> serde_json::Value {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut by_shape = serde_json::Map::new();
        for (shape, c) in &inner.by_shape {
            by_shape.insert(
                shape.clone(),
                serde_json::json!({
                    "requests": c.requests,
                    "bytesIn": c.bytes_in,
                    "bytesOut": c.bytes_out,
                    "latencyMs": c.latency_ms,
                }),
            );
        }
        let mut slots = serde_json::Map::new();
        for (slot, c) in &inner.slots {
            slots.insert(
                slot.clone(),
                serde_json::json!({
                    "local": c.local,
                    "delegated": c.delegated,
                    "unavailable": c.unavailable,
                }),
            );
        }
        let avg = inner.latency_ms.checked_div(inner.requests).unwrap_or(0);
        serde_json::json!({
            "ok": true,
            "totals": {
                "requests": inner.requests,
                "bytesIn": inner.bytes_in,
                "bytesOut": inner.bytes_out,
                "latencyMs": inner.latency_ms,
                "avgLatencyMs": avg,
            },
            "byShape": by_shape,
            "slots": slots,
            "degradations": inner.degradations,
        })
    }
}

/// 构建本地 SLM 槽位后端（§3.8）。配置缺失或构建未编入 HTTP 客户端时返回 None，
/// 让 govern 自动降级到反向委派 / L0。
fn build_local_slots() -> Option<Arc<dyn LocalSlots>> {
    let path = default_config_path();
    match load_config(&path) {
        Ok(Some(cfg)) => {
            let client = Arc::new(SlotClient::new(cfg));
            local_slots::LocalSlotBackend::new(client).map(|backend| {
                // 冷启可能 20s 级：后台预热 summarize，不阻塞 daemon 开始监听。
                backend.prewarm();
                Arc::new(backend) as Arc<dyn LocalSlots>
            })
        }
        _ => None,
    }
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
    let local = build_local_slots();
    let ctx = Arc::new(ServerCtx {
        spill,
        cwd,
        local,
        stats: Stats::default(),
    });

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
        return (
            200,
            serde_json::json!({ "ok": true, "service": "toolgate-daemon" }),
        );
    }
    if method == "GET" && path == "/stats" {
        return (200, ctx.stats.snapshot());
    }
    if method != "POST" {
        return (
            405,
            serde_json::json!({ "error": { "code": "METHOD_NOT_ALLOWED", "message": "仅支持 POST /govern、POST /pre 与 GET /stats" } }),
        );
    }
    match path {
        "/govern" => match serde_json::from_slice::<GovernRequest>(body) {
            Ok(req) => {
                let started = Instant::now();
                let resp =
                    govern_with_slots(&req, &ctx.spill, ctx.local.as_deref(), Some(&HttpDelegate));
                let payload = serde_json::to_value(&resp).unwrap_or_else(|e| bad_req(&e.to_string()));
                let out_bytes = serde_json::to_vec(&payload).map(|v| v.len()).unwrap_or(0);
                ctx.stats.record_govern(
                    body.len(),
                    out_bytes,
                    started.elapsed().as_millis() as u64,
                    &resp,
                );
                (200, payload)
            }
            Err(e) => (400, bad_req(&format!("govern 请求解析失败：{e}"))),
        },
        "/pre" => match serde_json::from_slice::<PreRequest>(body) {
            Ok(req) => {
                let resp = pre(&req, &ctx.spill, &ctx.cwd);
                (
                    200,
                    serde_json::to_value(resp).unwrap_or_else(|e| bad_req(&e.to_string())),
                )
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
