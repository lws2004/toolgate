#!/usr/bin/env python3
# 附件 E.13 的对照实测探针：§5.1 九问二元分解 vs 同请求混合 noul/choice/score 原生归并。
# 命令：python3 scripts/primitive_probe.py
# 依赖：本机 d3-nano :8082 在线；loopback、无鉴权。
# 口径：构造样本（非生产分布），只验形状与归并，不验分类质量。
import json, time, urllib.request

URL = "http://127.0.0.1:8082/v1/systemone"

def ask(questions, state):
    body = json.dumps({"state": state, "questions": questions}, ensure_ascii=False).encode()
    req = urllib.request.Request(URL, data=body, headers={"Content-Type": "application/json"})
    t0 = time.time()
    with urllib.request.urlopen(req, timeout=30) as r:
        out = json.loads(r.read())
    return out["answers"], round((time.time() - t0) * 1000, 1)

KINDS = ["compile", "test", "permission", "network", "resource"]
KIND_CN = {"compile": "编译错误", "test": "测试失败", "permission": "权限问题", "network": "网络问题", "resource": "资源问题"}
LEVELS = ["噪音，可忽略", "警告，功能受损但有绕过办法", "阻断，无绕过办法"]

SAMPLES = [
    ("编译错误", "error[E0463]: can't find crate for `serde`\n  --> src/main.rs:1:1\nerror: aborting due to previous error"),
    ("测试失败", "running 12 tests\ntest auth::login ... FAILED\nthread 'auth::login' panicked at 'assertion failed'\ntest result: FAILED. 11 passed; 1 failed"),
    ("权限问题", "bash: ./deploy.sh: Permission denied\nmake: *** [deploy] Error 126"),
    ("网络问题", "curl: (7) Failed to connect to registry.example.com port 443: Connection refused\nretrying in 2s..."),
    ("资源问题", "write failed: No space left on device\nERROR: cannot create temp file"),
    ("正常完成", "Finished `release` profile [optimized] target(s) in 12.48s\n3 crates compiled"),
    ("仅警告", "warning: unused variable: `x`\n  --> src/lib.rs:9:9\nFinished dev profile"),
    ("进程卡住", "Starting dev server on :5173\n(no further output for 180s)\nwatcher ready"),
]

A_QUESTIONS = {**{"q_has_error": {"type": "noul", "instructions": "这段输出有错误吗？"}},
               **{f"q_kind_{i}": {"type": "noul", "instructions": f"这段输出是不是{KIND_CN[k]}？"} for i, k in enumerate(KINDS)},
               "q_blocking": {"type": "noul", "instructions": "这是阻断性问题吗？"},
               "q_noise": {"type": "noul", "instructions": "这只是噪音吗？"},
               "q_full": {"type": "noul", "instructions": "需要看完整输出吗？"}}

B_QUESTIONS = {
    "q_has_error": {"type": "noul", "instructions": "这段输出有错误吗？"},
    "q_error_kind": {"type": "choice", "instructions": "这段输出属于哪一类错误？", "criteria": {k: KIND_CN[k] for k in KINDS}},
    "q_severity": {"type": "score", "instructions": "这个问题的严重程度如何？", "criteria": LEVELS},
    "q_full": {"type": "noul", "instructions": "需要看完整输出吗？"},
}

def method_a(name, state):
    ans, ms = ask(A_QUESTIONS, state)
    p_has = ans["q_has_error"]["noul"]
    kinds = {k: ans[f"q_kind_{i}"]["noul"] for i, k in enumerate(KINDS)}
    p_block, p_noise = ans["q_blocking"]["noul"], ans["q_noise"]["noul"]
    top = max(kinds, key=kinds.get)
    error_kind = "none" if p_has < 0.5 else (top if kinds[top] >= 0.5 else "unknown")
    severity = "blocking" if p_block >= 0.5 else ("noise" if p_noise >= 0.5 else "warning")
    conf = p_has if error_kind == "none" else min(p_has, kinds[top])
    conflicts = sum(1 for v in kinds.values() if v >= 0.5)
    return {"method": "A", "errorKind": error_kind, "severity": severity,
            "confidence": round(conf, 3), "gate": "Adopt" if conf >= 0.85 else ("LOW" if conf >= 0.60 else "BothViews"),
            "kinds>0.5": conflicts, "lowest_kind": round(min(kinds.values()), 3), "ms": ms,
            "p_has": round(p_has, 3), "p_block": round(p_block, 3), "p_noise": round(p_noise, 3)}

def method_b(name, state):
    ans, ms = ask(B_QUESTIONS, state)
    p_has = ans["q_has_error"]["noul"]
    ch = ans["q_error_kind"]
    probs = ch["probabilities"]
    top = max(probs, key=probs.get)
    error_kind = "none" if p_has < 0.5 else (top if probs[top] >= 0.5 else "unknown")
    sev = ans["q_severity"]
    pos = sev["score"]
    severity = "blocking" if pos >= 1.5 else ("noise" if pos < 0.5 else "warning")
    return {"method": "B", "errorKind": error_kind, "severity": severity,
            "choice_top": round(probs[top], 3), "choice_conf": round(ch.get("confidence", -1), 3),
            "none_prob": ch.get("none_probability"), "score": round(pos, 3),
            "level_probs": [round(sev["probabilities"].get(str(i), 0), 3) for i in range(3)],
            "score_conf": round(sev.get("confidence", -1), 3), "ms": ms, "p_has": round(p_has, 3)}

print("样本\t方法\t结果")
print("-" * 110)
for name, state in SAMPLES:
    a = method_a(name, state)
    b = method_b(name, state)
    print(f"{name}\tA\t{json.dumps(a, ensure_ascii=False)}")
    print(f"{name}\tB\t{json.dumps(b, ensure_ascii=False)}")
