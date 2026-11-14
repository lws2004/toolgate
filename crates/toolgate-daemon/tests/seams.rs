//! daemon 接缝集成测试：走真实 Unix socket + 手写 HTTP，验证 govern / pre / health。

use std::time::Duration;

use std::io::{Read, Write};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

fn socket_path(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("toolgate-daemon-{tag}-{}.sock", std::process::id()))
}

async fn connect(socket: &std::path::Path) -> UnixStream {
    for _ in 0..100 {
        if let Ok(s) = UnixStream::connect(socket).await {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("连接 daemon 超时");
}

async fn request(socket: &std::path::Path, method: &str, path: &str, body: &str) -> String {
    let mut s = connect(socket).await;
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut buf = String::new();
    s.read_to_string(&mut buf).await.unwrap();
    buf
}

fn body_of(resp: &str) -> &str {
    resp.split("\r\n\r\n").nth(1).unwrap_or("")
}

#[tokio::test]
async fn health_govern_and_pre_over_socket() {
    let socket = socket_path("it");
    let _ = std::fs::remove_file(&socket);
    let sock = socket.clone();
    tokio::spawn(async move {
        let _ = toolgate_daemon::serve(&sock).await;
    });

    // health
    let resp = request(&socket, "GET", "/health", "").await;
    assert!(resp.starts_with("HTTP/1.1 200"), "resp={resp}");
    assert!(body_of(&resp).contains("\"ok\":true"));

    // govern: 短文本 → pass_through
    let gov = r#"{"source":{"kind":"tool","tool":"read"},"content":[{"type":"text","text":"hello"}]}"#;
    let resp = request(&socket, "POST", "/govern", gov).await;
    assert!(resp.starts_with("HTTP/1.1 200"), "resp={resp}");
    assert!(body_of(&resp).contains("pass_through"));

    // pre: 新文件无指纹 → allow
    let dir = std::env::temp_dir().join(format!("toolgate-daemon-pre-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let target = dir.join("new.txt");
    let pre_body = format!(
        r#"{{"ops":[{{"op":"write","path":{},"payload":"x"}}]}}"#,
        serde_json::to_string(&target.display().to_string()).unwrap()
    );
    let resp = request(&socket, "POST", "/pre", &pre_body).await;
    assert!(resp.starts_with("HTTP/1.1 200"), "resp={resp}");
    assert!(body_of(&resp).contains("\"decision\":\"allow\""), "resp={resp}");

    let _ = std::fs::remove_file(&socket);
}

/// 假委派服务：收一次 POST，回固定 ok 文本（独立 OS 线程，避免与阻塞客户端争运行时线程）。
fn spawn_fake_delegate(path: std::path::PathBuf) {
    let _ = std::fs::remove_file(&path);
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let mut s = match conn {
                Ok(v) => v,
                // accept 可能被信号中断（EINTR）；不能因此关闭整个假服务，否则客户端会立刻连接失败。
                Err(_) => continue,
            };
            let mut tmp = [0u8; 8192];
            let _ = s.read(&mut tmp);
            let body = br#"{"ok":true,"text":"OCR: hi","model":"m1"}"#;
            let head = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = s.write_all(head.as_bytes());
            let _ = s.write_all(body);
            let _ = s.flush();
        }
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn govern_delegates_vision_over_socket() {
    let del_sock = socket_path("delegate");
    spawn_fake_delegate(del_sock.clone());

    let socket = socket_path("it-del");
    let _ = std::fs::remove_file(&socket);
    let sock = socket.clone();
    tokio::spawn(async move {
        let _ = toolgate_daemon::serve(&sock).await;
    });

    let endpoint = format!("unix:{}", del_sock.display());
    let gov = format!(
        r#"{{"source":{{"kind":"tool","tool":"web_fetch"}},"content":[{{"type":"image","mimeType":"image/png","attachmentId":"abc","bytes":100,"width":10,"height":10}}],"intent":"summary","policy":{{"delegate":{{"endpoint":{},"token":"tok"}}}}}}"#,
        serde_json::to_string(&endpoint).unwrap()
    );
    let resp = request(&socket, "POST", "/govern", &gov).await;
    assert!(resp.starts_with("HTTP/1.1 200"), "resp={resp}");
    let body = body_of(&resp);
    assert!(body.contains("OCR: hi"), "body={body}");
    assert!(body.contains("\"delegated\":true"), "body={body}");
    let _ = std::fs::remove_file(&socket);
    let _ = std::fs::remove_file(&del_sock);
}
