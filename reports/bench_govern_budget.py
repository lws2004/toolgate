#!/usr/bin/env python3
"""实测 govern 治理时间预算（报告 P0-1/P0-2/P1-2 与 /stats 观测）。

用法：python3 reports/bench_govern_budget.py <socket>
只读：发 /govern 与 /stats，不改任何状态。
"""

import http.client
import json
import socket
import sys
import time


def call(sock_path, method, path, payload=None):
    body = b"" if payload is None else json.dumps(payload, ensure_ascii=False).encode()
    conn = http.client.HTTPConnection("localhost")
    conn.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    conn.sock.connect(sock_path)
    started = time.perf_counter()
    conn.request(method, path, body=body, headers={"content-type": "application/json"})
    resp = conn.getresponse()
    text = resp.read().decode()
    elapsed = time.perf_counter() - started
    conn.close()
    return resp.status, text, elapsed


def govern(sock_path, tool, text, intent="raw"):
    req = {
        "source": {"kind": "tool", "tool": tool},
        "content": [{"type": "text", "text": text}],
        "intent": intent,
        "policy": {"isolation": "summary"},
    }
    status, text, elapsed = call(sock_path, "POST", "/govern", req)
    if status != 200:
        return {"status": status, "error": text[:300], "elapsedMs": round(elapsed * 1000)}
    obj = json.loads(text)
    sc = obj.get("structuredContent") or {}
    meta = obj.get("meta") or {}
    out_text = "".join(b.get("text", "") for b in obj.get("content") or [] if b.get("type") == "text")
    return {
        "status": status,
        "elapsedMs": round(elapsed * 1000),
        "shape": sc.get("shape"),
        "sampled": sc.get("sampled"),
        "slmStatus": meta.get("slmStatus"),
        "notices": [n.get("code") for n in sc.get("notices") or []],
        "bytesOut": len(out_text.encode()),
        "outLines": out_text.count("\n") + 1 if out_text else 0,
    }


def make_long_text(lines, line_bytes):
    out = []
    for i in range(lines):
        row = f"2026-11-14 12:{i % 60:02d}:00 INFO worker id={i} handled request in {i % 97} ms"
        pad = max(0, line_bytes - len(row))
        out.append(row + "x" * pad)
    return "\n".join(out)


def main():
    sock_path = sys.argv[1]
    cases = []
    for intent in ("raw", "summary"):
        cases.append((f"长文2200行/{intent}", "read", make_long_text(2200, 84), intent))
    for intent in ("raw", "summary"):
        cases.append((f"长文600行/{intent}", "read", make_long_text(600, 84), intent))
    listing = "\n".join(f"src/app.rs:{i}:1: fn handler_{i}()" for i in range(3000))
    cases.append(("列表3000行", "search", listing, "raw"))
    small = "\n".join(f"line {i}: value = {i}" for i in range(60))
    cases.append(("短文本60行", "bash", small, "raw"))
    err = "\n".join(f"Traceback (most recent call last): frame {i} at src/x.rs:{i}:1" for i in range(1600))
    cases.append(("错误40KB", "bash", err, "raw"))

    print(f"# socket={sock_path}")
    print("| 形态 | 输入字节 | 输出字节 | 输出行 | 耗时ms | shape | sampled | notices | slmStatus |")
    print("|---|---|---|---|---|---|---|---|---|")
    for name, tool, text, intent in cases:
        r = govern(sock_path, tool, text, intent)
        if r.get("status") != 200:
            print(f"| {name} | {len(text.encode())} | - | - | {r['elapsedMs']} | ERR | - | - | {r['error']} |")
            continue
        print(
            f"| {name} | {len(text.encode())} | {r['bytesOut']} | {r['outLines']} | {r['elapsedMs']} "
            f"| {r['shape']} | {r['sampled']} | {','.join(n for n in r['notices'] if n)} | {json.dumps(r['slmStatus'], ensure_ascii=False)} |"
        )

    status, text, elapsed = call(sock_path, "GET", "/stats")
    stats = json.loads(text) if status == 200 else {"error": text[:300]}
    print(f"\n## /stats（{round(elapsed * 1000)}ms）\n")
    print("```json")
    print(json.dumps(stats, ensure_ascii=False, indent=2))
    print("```")


if __name__ == "__main__":
    main()
