#!/usr/bin/env python3
"""向 toolgate daemon 的 /govern 接缝发一次请求，打印形状/耗时/queryApplied/前 N 行（重测用）。

用法：
  python3 scripts/bench_govern.py --lines 120 --query "数据库迁移失败的原因"
  python3 scripts/bench_govern.py --file /tmp/out.txt --intent debug
"""
import argparse
import json
import socket
import time

MIGRATION_LINES = {
    7: "crates/db/src/migrate.rs:118: note: migration 0007 needs attention -- column type mismatch on orders table",
    37: "crates/store/src/migrate.rs:42: note: migration 0007 blocked by stale schema, DATABASE_URL points to old snapshot",
    68: "crates/db/src/migrate.rs:203: note: 数据库迁移 0009 需人工核对：表 orders 出现重复列",
    97: "crates/db/src/migrate.rs:260: note: migration lock still held after migration 0007, review DATABASE_URL",
    111: "crates/db/src/migrate.rs:305: note: migration 0011 checksum mismatch, rerun after fixing the migration script",
}


def make_listing(n):
    rows = []
    for i in range(1, n + 1):
        if i in MIGRATION_LINES:
            rows.append(MIGRATION_LINES[i])
        else:
            rows.append("crates/util/src/mod_%02d.rs:%d: warning: unused variable `tmp_%d`" % (i % 40, i, i))
    return "\n".join(rows)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--socket", default=".toolgate/daemon.sock")
    ap.add_argument("--file")
    ap.add_argument("--lines", type=int, default=120)
    ap.add_argument("--query")
    ap.add_argument("--intent", default="debug")
    ap.add_argument("--head", type=int, default=8)
    ap.add_argument("--tail", type=int, default=3)
    a = ap.parse_args()

    text = open(a.file, encoding="utf-8").read() if a.file else make_listing(a.lines)
    req = {
        "source": {"kind": "tool", "tool": "bash"},
        "content": [{"type": "text", "text": text}],
        "intent": a.intent,
        "policy": {"isolation": "none"},
    }
    if a.query:
        req["query"] = a.query
    body = json.dumps(req, ensure_ascii=False).encode("utf-8")

    s = socket.socket(socket.AF_UNIX)
    s.connect(a.socket)
    t0 = time.time()
    s.sendall(
        b"POST /govern HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n"
        b"Content-Length: " + str(len(body)).encode() + b"\r\nConnection: close\r\n\r\n" + body
    )
    chunks = []
    while True:
        b = s.recv(65536)
        if not b:
            break
        chunks.append(b)
    ms = round((time.time() - t0) * 1000)
    raw = b"".join(chunks)
    _, _, payload = raw.partition(b"\r\n\r\n")
    resp = json.loads(payload)
    out = "".join(blk.get("text", "") for blk in resp.get("content", []))
    lines = [l for l in out.split("\n") if l.strip()]
    sc = resp.get("structuredContent") or {}
    meta = resp.get("meta") or {}
    print(json.dumps({
        "inBytes": len(body),
        "outBytes": len(out.encode()),
        "outLines": len(lines),
        "ms": ms,
        "shape": sc.get("shape"),
        "queryApplied": sc.get("queryApplied"),
        "notices": sc.get("notices"),
        "slmStatus": meta.get("slmStatus"),
    }, ensure_ascii=False))
    for l in lines[: a.head]:
        print("  |", l)
    if a.tail and len(lines) > a.head:
        print("  …")
        for l in lines[-a.tail:]:
            print("  |", l)


if __name__ == "__main__":
    main()
