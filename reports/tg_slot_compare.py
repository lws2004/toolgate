#!/usr/bin/env python3
# 槽位「有配置/无配置」与三态对比 harness（经 daemon /govern）
import json, os, socket, subprocess, sys, threading, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT = "/Users/lanws/workspace/toolgate"
BIN = ROOT + "/target/release/toolgate"
SOCK_A = ROOT + "/.toolgate/daemon.sock"
TMP = "/tmp/tg_nohome"
SOCK_B = TMP + "/daemon.sock"
DELEG_PORT = 18080

def http_unix(sock_path, method, path, body=None, timeout=180):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(timeout)
    s.connect(sock_path)
    raw = b"" if body is None else json.dumps(body, ensure_ascii=False).encode()
    head = f"{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {len(raw)}\r\nConnection: close\r\n\r\n".encode()
    s.sendall(head + raw)
    data = b""
    while True:
        try:
            chunk = s.recv(65536)
        except socket.timeout:
            break
        if not chunk:
            break
        data += chunk
    s.close()
    idx = data.find(b"\r\n\r\n")
    return data[idx+4:] if idx >= 0 else data

def govern(sock_path, content, intent, policy=None):
    body = {"source": {}, "content": content, "intent": intent}
    if policy is not None:
        body["policy"] = policy
    t0 = time.time()
    raw = http_unix(sock_path, "POST", "/govern", body)
    dt = (time.time() - t0) * 1000
    try:
        resp = json.loads(raw)
    except Exception:
        resp = {"_raw": raw.decode(errors="replace")}
    return resp, dt

# ---------- 委派替身（会话模型） ----------
class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_POST(self):
        n = int(self.headers.get("Content-Length", 0))
        payload = json.loads(self.rfile.read(n) or b"{}")
        task = payload.get("task")
        inp = payload.get("input", {})
        if task == "summarize":
            out = {"ok": True, "model": "session/deepseek-v4.1-flash",
                   "text": "【会话模型摘要】收到长文 " + str(inp.get("totalLines")) + " 行；关键线索：worker 批处理正常，末段有 1 条超时告警；已保留路径与数字。"}
        elif task == "rerank":
            text = inp.get("text", "")
            limit = int(inp.get("limit", 2000))
            lines = text.splitlines()[:limit]
            out = {"ok": True, "model": "session/deepseek-v4.1-flash", "text": "\n".join(lines)}
        elif task == "classify":
            out = {"ok": True, "model": "session/deepseek-v4.1-flash", "text": json.dumps({"shape": "long_text"})}
        elif task == "vision":
            out = {"ok": True, "model": "session/deepseek-v4.1-flash", "text": "【会话模型识图】一张 1x1 占位 PNG，无文字。"}
        else:
            out = {"ok": False, "error": "unknown task"}
        b = json.dumps(out, ensure_ascii=False).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(b)))
        self.end_headers()
        self.wfile.write(b)

def start_delegate():
    srv = ThreadingHTTPServer(("127.0.0.1", DELEG_PORT), H)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv

def start_daemon_b():
    os.makedirs(TMP, exist_ok=True)
    env = dict(os.environ)
    env["HOME"] = TMP  # 无 ~/.toolgate/config.yaml → 本地槽位不可用
    log = open(TMP + "/daemon.log", "w")
    p = subprocess.Popen([BIN, "daemon", "--socket", SOCK_B], env=env, stdout=log, stderr=subprocess.STDOUT)
    for _ in range(100):
        if os.path.exists(SOCK_B):
            try:
                r = http_unix(SOCK_B, "GET", "/health", None, timeout=3)
                if b'"ok":true' in r:
                    return p
            except Exception:
                pass
        time.sleep(0.1)
    raise RuntimeError("daemon B 未就绪")

def text_block(t): return {"type": "text", "text": t}

def payload_long():
    lines = []
    for i in range(300):
        lines.append(f"2026-11-14T12:{i%60:02d}:00Z INFO worker[{i%16}] processed batch id={i:05d} region=cn-east took={i%400}ms rows={i*3}")
    return [text_block("\x1b[31m" + "\n".join(lines))]

def payload_listing():
    lines = []
    for i in range(60):
        lines.append(f"src/crates/toolgate-core/src/module_{i:04d}.rs:{i+1}: let handler_{i} = process(item_{i});")
    return [text_block("\n".join(lines))]

def payload_ambiguous():
    lines = ["构建在链接阶段终止，链接器报告若干未解析符号。"]
    for i in range(30):
        lines.append(f"undefined reference to process_item_{i} in module_{i} during final link step")
    lines.append("collect2: ld returned 1 exit status")
    return [text_block("\n".join(lines))]

def payload_vision():
    return [{"type": "image", "mimeType": "image/png", "dataBase64": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==", "bytes": 70, "width": 1, "height": 1}]

CASES = [
    ("summarize/long_text", payload_long, "summary"),
    ("rerank/listing", payload_listing, "auto"),
    ("classify/error-like", payload_ambiguous, "summary"),
    ("vision/image", payload_vision, "summary"),
]

def summarize_resp(resp, dt):
    content = resp.get("content", [])
    out_text = ""
    for b in content:
        if b.get("type") == "text":
            out_text += b.get("text", "")
    st = resp.get("structuredContent", {})
    slm = resp.get("meta", {}).get("slmStatus", {})
    active = {k: v for k, v in slm.items() if v not in ("unavailable", "not_required")}
    return {
        "shape": st.get("shape"),
        "classified": st.get("classified", False),
        "classifiedShape": st.get("classifiedShape"),
        "model": st.get("model"),
        "slmActive": active,
        "outBytes": len(out_text.encode()),
        "isError": resp.get("isError"),
        "latencyMs": round(dt, 1),
        "preview": out_text[:120].replace("\n", " "),
    }

def main():
    srv = start_delegate()
    pB = start_daemon_b()
    results = {"slots": [], "three_state": []}
    try:
        for name, fn, intent in CASES:
            content = fn()
            raw_bytes = sum(len(b.get("text", "").encode()) for b in content)
            ra, ta = govern(SOCK_A, content, intent)
            rb, tb = govern(SOCK_B, content, intent)
            results["slots"].append({
                "case": name, "rawBytes": raw_bytes,
                "A_local_on": summarize_resp(ra, ta),
                "B_local_off": summarize_resp(rb, tb),
            })
        # 三态：同一长文
        content = payload_long()
        raw_bytes = sum(len(b.get("text", "").encode()) for b in content)
        r_def, t_def = govern(SOCK_A, content, "summary")
        pol = {"delegate": {"endpoint": f"http://127.0.0.1:{DELEG_PORT}"}, "slmPrefer": "delegate"}
        r_del, t_del = govern(SOCK_A, content, "summary", pol)
        r_none, t_none = govern(SOCK_B, content, "summary")
        results["three_state"] = [
            {"state": "① 无包装(raw)", "rawBytes": raw_bytes, "outBytes": raw_bytes},
            {"state": "② 有包装/默认(本地优先)", "rawBytes": raw_bytes, **summarize_resp(r_def, t_def)},
            {"state": "③ 有包装/会话模型(slmPrefer=delegate)", "rawBytes": raw_bytes, **summarize_resp(r_del, t_del)},
            {"state": "④ 有包装/无本地无委派(L0)", "rawBytes": raw_bytes, **summarize_resp(r_none, t_none)},
        ]
    finally:
        try: pB.terminate(); pB.wait(timeout=5)
        except Exception: pass
        srv.shutdown()
    os.makedirs(ROOT + "/reports", exist_ok=True)
    with open(ROOT + "/reports/tg_slot_compare.json", "w") as f:
        json.dump(results, f, ensure_ascii=False, indent=2)
    print(json.dumps(results, ensure_ascii=False, indent=2))

if __name__ == "__main__":
    main()