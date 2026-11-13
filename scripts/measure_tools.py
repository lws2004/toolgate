#!/usr/bin/env python3
"""测量 MCP tools/list 的固定开销（六工具 description/inputSchema/outputSchema 字节）。

口径与 docs/architecture/06-cost.md §6.4 一致：六工具字段和与完整 JSON-RPC result
字节数，按 4 B/token 估算常驻 token，红线 6000 B（≈1.5K token）。

用法:
  python3 scripts/measure_tools.py                  # 默认 target/release/toolgate，中文描述
  python3 scripts/measure_tools.py --lang en        # 英文描述（tools/list 带 _meta.toolgate/language=en）
  python3 scripts/measure_tools.py --json           # 机器可读快照
  python3 scripts/measure_tools.py --file out.txt   # 复用已抓取的 serve stdout，不启动进程
  python3 scripts/measure_tools.py --bin ./target/release/toolgate
"""
import argparse
import json
import subprocess
import sys
from pathlib import Path

RED_LINE_BYTES = 6000  # 1.5K token × 4 B/token


def b(x):
    return len(json.dumps(x, ensure_ascii=False, separators=(",", ":")).encode())


def fetch(bin_path, lang):
    init = {
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": "measure_tools", "version": "0"}},
    }
    tools_list = {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}
    if lang == "en":
        tools_list["params"]["_meta"] = {"toolgate/language": "en"}
    payload = json.dumps(init) + "\n" + json.dumps(tools_list) + "\n"
    proc = subprocess.run([bin_path, "serve"], input=payload, capture_output=True,
                          text=True, timeout=30)
    if proc.returncode != 0:
        sys.stderr.write(proc.stderr)
    return proc.stdout


def parse_tools(text):
    for line in text.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            obj = json.loads(line)
        except Exception:
            continue
        if obj.get("id") == 2 and "result" in obj:
            return obj["result"]["tools"], obj["result"]
    return None, None


def main():
    ap = argparse.ArgumentParser(description="测量 MCP tools/list 固定开销")
    ap.add_argument("--bin", default="target/release/toolgate", help="toolgate 二进制路径")
    ap.add_argument("--lang", choices=["zh", "en"], default="zh", help="描述语言")
    ap.add_argument("--file", help="读取已抓取的 serve stdout，不启动进程")
    ap.add_argument("--json", action="store_true", dest="as_json", help="输出 JSON 快照")
    args = ap.parse_args()

    text = (Path(args.file).read_text(encoding="utf-8", errors="replace")
            if args.file else fetch(args.bin, args.lang))
    tools, result = parse_tools(text)
    if tools is None:
        print("未找到 tools/list 响应（id=2）", file=sys.stderr)
        return 1

    rows, total = [], 0
    for t in tools:
        nb, db = b(t["name"]), b(t["description"])
        ib, ob = b(t["inputSchema"]), b(t.get("outputSchema"))
        s = nb + db + ib + ob
        total += s
        rows.append({"name": t["name"], "name_b": nb, "desc_b": db,
                     "in_b": ib, "out_b": ob, "sum_b": s})
    full = b(result)
    snapshot = {"lang": args.lang, "tools": rows, "sum_fields": total,
                "full_result": full, "est_tokens_4b": round(full / 4, 2),
                "red_line_bytes": RED_LINE_BYTES, "ok": full <= RED_LINE_BYTES}

    if args.as_json:
        print(json.dumps(snapshot, ensure_ascii=False, indent=2))
        return 0

    print("tools count", len(rows))
    for r in rows:
        print("%-8s name=%d desc=%d in=%d out=%d sum=%d"
              % (r["name"], r["name_b"], r["desc_b"], r["in_b"], r["out_b"], r["sum_b"]))
    print("sum all fields", total)
    print("full result bytes", full)
    print("≈%.0f token ≈ %.2fK (4 B/token)，红线 %d B：%s"
          % (full / 4, full / 4000, RED_LINE_BYTES, "达标" if full <= RED_LINE_BYTES else "超标"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
