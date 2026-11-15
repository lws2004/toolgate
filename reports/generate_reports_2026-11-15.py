#!/usr/bin/env python3
# 生成 2026-11-15 版三份评估报告（数据来自本轮重跑：tg_slot_compare.json + bench_govern_budget + query 三态实测）
import json, html, os

BASE = "/Users/lanws/workspace/toolgate/reports"
CSS = """
:root{--bg:#f6f7f9;--card:#fff;--fg:#1f2937;--muted:#6b7280;--line:#e5e7eb;--accent:#1e40af;--ok:#065f46;--okbg:#d1fae5;--no:#7c2d12;--nobg:#fee2e2;--warn:#b45309;--warnbg:#fef3c7}
@media(prefers-color-scheme:dark){:root{--bg:#0f172a;--card:#111827;--fg:#e5e7eb;--muted:#9ca3af;--line:#374151;--accent:#93c5fd;--ok:#6ee7b7;--okbg:#064e3b;--no:#fca5a5;--nobg:#7f1d1d;--warn:#fcd34d;--warnbg:#78350f}}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--fg);font:15px/1.65 system-ui,-apple-system,"PingFang SC",sans-serif}
.wrap{max-width:1140px;margin:0 auto;padding:36px 22px 70px}
h1{font-size:25px;margin:0 0 6px;letter-spacing:-.01em}h2{font-size:18px;margin:36px 0 10px;padding-left:10px;border-left:4px solid var(--accent)}
h3{font-size:15px;margin:20px 0 6px}.sub{color:var(--muted);font-size:13px;margin-bottom:8px}
.lead{background:var(--card);border:1px solid var(--line);border-left:4px solid var(--accent);border-radius:8px;padding:14px 18px;margin:16px 0}
.kpi{display:flex;gap:12px;flex-wrap:wrap;margin:14px 0}.kpi div{flex:1 1 200px;background:var(--card);border:1px solid var(--line);border-radius:9px;padding:12px 14px}
.kpi b{display:block;font-size:21px;margin-top:2px}.kpi .k{font-size:12px;color:var(--muted)}
table{width:100%;border-collapse:collapse;background:var(--card);border:1px solid var(--line);border-radius:8px;overflow:hidden;font-size:13.5px;margin:10px 0}
th,td{padding:8px 11px;border-bottom:1px solid var(--line);text-align:left;vertical-align:top}
th{background:rgba(127,127,127,.08);font-weight:600;white-space:nowrap}tr:last-child td{border-bottom:none}
td.num{text-align:right;font-variant-numeric:tabular-nums}
.b{display:inline-block;padding:1px 8px;border-radius:999px;font-size:12px;font-weight:600}
.b.ok{color:var(--ok);background:var(--okbg)}.b.no{color:var(--no);background:var(--nobg)}.b.w{color:var(--warn);background:var(--warnbg)}
code{background:rgba(127,127,127,.14);padding:1px 5px;border-radius:4px;font-size:12.5px}
ul{margin:8px 0 0 20px;padding:0}li{margin:5px 0}.note{color:var(--muted);font-size:12.5px}
footer{margin-top:44px;padding-top:14px;border-top:1px solid var(--line);color:var(--muted);font-size:12px}
"""

def page(title, subtitle, body):
    return f"""<!doctype html><html lang="zh-CN"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1"><title>{html.escape(title)}</title>
<style>{CSS}</style></head><body><div class="wrap">
<h1>{html.escape(title)}</h1><div class="sub">{subtitle}</div>
{body}
<footer>toolgate · 2026-11-15 · 数据口径：daemon /govern 实测，UTF-8 字节，墙钟延迟；本报告为单机单次快照</footer>
</div></body></html>"""

def esc(x): return html.escape(str(x))

def load_slots():
    d = json.load(open(os.path.join(BASE, "tg_slot_compare.json")))
    return d["slots"], d["three_state"]

def pct(out, raw):
    return "-" if not raw else f"{out*100.0/raw:.1f}%"

def build_slot():
    slots, three = load_slots()
    rows = []
    for r in slots:
        a, b = r["A_local_on"], r["B_local_off"]
        am = a["model"] or "L0/机械"
        bm = b["model"] or "L0/机械"
        rows.append(f"<tr><td>{esc(r['case'])}</td><td class='num'>{r['rawBytes']}</td>"
                    f"<td>{esc(am)}<div class='note'>shape={esc(a['shape'])}</div></td>"
                    f"<td class='num'>{a['outBytes']} ({pct(a['outBytes'],r['rawBytes'])})</td><td class='num'>{a['latencyMs']}ms</td>"
                    f"<td>{esc(bm)}<div class='note'>shape={esc(b['shape'])}</div></td>"
                    f"<td class='num'>{b['outBytes']} ({pct(b['outBytes'],r['rawBytes'])})</td><td class='num'>{b['latencyMs']}ms</td></tr>")
    trows = []
    for t in three:
        trows.append(f"<tr><td>{esc(t['state'])}</td><td class='num'>{t['rawBytes']}</td>"
                     f"<td class='num'>{t.get('outBytes')} ({pct(t.get('outBytes',0),t['rawBytes'])})</td>"
                     f"<td>{esc(t.get('model') or '—')}</td><td class='num'>{t.get('latencyMs')}ms</td></tr>")
    qrows = [
        ("120 行 + query=「数据库迁移失败的原因」", "listing", "applied", "rerank=local", "1198ms", "3 条 migration 行排最前（query 生效）"),
        ("120 行 + query=「^fn .*handler」", "listing", "rejected_non_natural", "rerank=local", "1574ms", "notices 含 DEGRADED(actual=generic_intent reason=non_natural_query)，content 末行同步"),
        ("60 行（无 query，R10 跳过）", "listing", "无（跳过不重排）", "未调用", "1ms", "小列表无排序诉求，原样返回、不带 queryApplied"),
        ("120 行 39KB 长行（无 query）", "listing", "not_provided", "rerank=local", "4952ms", "超 32KiB 仍走 rerank（已知边界：估算按行数未计字节）"),
        ("1200 行（>1000 候选，超预算）", "listing", "缺失（兜底不重排）", "未调用", "3ms", "只发 SLOT_UNAVAILABLE；无 DEGRADED"),
        ("1500 行 + rerankChunk 开（R12）", "listing", "applied", "rerank=local（分批）", "13941ms", "chunked=true；1500 docs 分 2 批 → 32KiB clamp"),
    ]
    qhtml = "".join(f"<tr><td>{esc(a)}</td><td>{esc(b)}</td><td>{esc(c)}</td><td>{esc(d)}</td><td class='num'>{esc(e)}</td><td>{esc(f)}</td></tr>" for a,b,c,d,e,f in qrows)
    body = f"""
<div class="lead"><b>结论</b>：本地 <b>4 个槽位真实进入 daemon 的 govern 降级链</b>——summarize / rerank 在正式 daemon 下 <code>meta.slmStatus</code> 报 <code>local</code> 且实际改写输出；embedding 用于长文输入去重（R8）；classify 可服务，但确定性嗅探优先命中时不再调用（本轮 error-like 由链接器前缀直接判 error，0.7ms）。vision 本地仍未接（图片只传引用、不传字节）；decide 不在 govern 链。R9 列表按请求 query 排序的三态已落地（第三节）。</div>
<div class="kpi">
<div><div class="k">长文 28.8KB → 本地摘要</div><b>101 B <span class="note">(0.4%)</span></b><div class="note">2528.7ms（热态）</div></div>
<div><div class="k">三态②默认本地优先</div><b>101 B <span class="note">/ 1984.6ms</span></b></div>
<div><div class="k">小列表无 query（R10）</div><b>1 ms</b><div class="note">跳过 rerank 原样返回</div></div>
<div><div class="k">验证基线</div><b>554 / 547</b><div class="note">cargo test / feature 构建</div></div>
</div>

<h2>一、每个槽位：有配置（A） vs 无配置（B）</h2>
<table><tr><th>场景</th><th>原始字节</th><th>A 有配置（命中）</th><th>输出字节</th><th>延迟</th><th>B 无配置（命中）</th><th>输出字节</th><th>延迟</th></tr>
{''.join(rows)}
</table>
<p class="note">A = 正式 daemon（~/.toolgate/config.yaml，6 槽位全部绑定本地服务）；B = 同一 release 二进制以 HOME=/tmp/tg_nohome 启动（无 config → 本地槽位 None）。两实例走同一 /govern 路径。</p>

<h2>二、三态对比（同一段长文，28.8KB）</h2>
<table><tr><th>形态</th><th>原始字节</th><th>输出字节</th><th>model / 来源</th><th>延迟</th></tr>
{''.join(trows)}
</table>
<p class="note">① 无包装=不经 daemon；② 默认=policy 缺省（本地优先）；③ slmPrefer=delegate 指向宿主 stub（链路验证，非真实宿主延迟）；④ 无本地无委派=B 实例 L0 机械兜底。</p>

<h2>三、R9：列表按请求 query 排序的三态（新增）</h2>
<table><tr><th>场景</th><th>shape</th><th>queryApplied</th><th>槽位</th><th>耗时</th><th>观察</th></tr>{qhtml}</table>
<p class="note">口径：query 只影响 <code>shape=listing</code> 且候选在预算内的 rerank 排序；非自然语言（含正则元字符/换行/超长）退回固定通用意图串并按 §4.4b 发 DEGRADED；兜底路径（超预算或槽位不可用）不重排、不带 queryApplied，只发 SLOT_UNAVAILABLE。</p>

<h2>四、发现与边界</h2>
<ul>
<li><b>长文收益最高、代价也最高</b>：28.8KB→101B（0.4%）用 2.5s；R10 把触发线下调到 32KiB 后，600 行 40.4s→2.5s、1000 行 42.1s→2.2s（此前 600 行 35.3s、1000 行 42.1s 全量送模型）。延迟随输入增长，所以「先减输入」比「调超时」有效。</li>
<li><b>列表超预算走毫秒级兜底</b>：3000 行 108.8KB → 71927B/2004 行仅 2ms（TRUNCATED + SLOT_UNAVAILABLE）；候选 ≤1000 且 ≤2s 才真实调用 rerank；R10 起无 query 且 ≤32KiB 的小列表直接跳过 rerank（60 行 448ms→1ms）。</li>
<li><b>错误保真分档</b>：短错误逐字 100% 保留；超 32KiB 走确定性压缩（40KB 样本 101779B→16397B，263 行），不入模型。</li>
<li><b>确定性嗅探优先</b>：/usr/bin/ld、undefined reference to、collect2、error[CODE] 等前缀已识别为 error，不再依赖 classify；因此本轮 error-like 的 A 态与 B 态一致（均 0.7ms L0）。</li>
<li><b>vision 本地未接、decide 不在链</b>：前者受「图片只传引用」限制，后者属各工具判定用途；均按红线 #3 保持「增强不是依赖」。</li>
</ul>

<h2>五、方法与复现</h2>
<ul>
<li>脚本：<code>reports/tg_slot_compare.py</code>（Python 标准库）：Unix socket 手写 HTTP/1.1 调 POST /govern；内置 127.0.0.1:18080 委派 stub；以 HOME=/tmp/tg_nohome 起第二 daemon 作 B 态。</li>
<li>数据：<code>reports/tg_slot_compare.json</code>（本轮重跑；旧版备份 <code>tg_slot_compare-2026-11-14.json</code>）；query 三态数据见 <code>reports/bench-rerank-query-fallback-2026-11-14.txt</code>、<code>bench-rerank-query-2026-11-14.txt</code>。</li>
<li>验证基线（2026-11-15）：<code>cargo test --workspace</code> 554 passed；<code>--features external-protocols</code> 547 passed；clippy 无告警；<code>./scripts/check-doc.sh</code> 通过（7081 行）；适配器 <code>node --test</code> 35 passed。</li>
</ul>"""
    return page("toolgate 槽位「有配置 / 无配置」与三态对比（2026-11-15）",
                "实测 2026-11-15 · 仓库 /Users/lanws/workspace/toolgate · 数据源 reports/tg_slot_compare.json（本轮重跑）", body)

def build_govern():
    bench = [
        ("长文 2200 行 / raw", 186999, 819, 13, 4208, "long_text", "TRUNCATED", "先采样 32KiB 再摘要", "ok"),
        ("长文 2200 行 / summary", 186999, 819, 13, 3414, "long_text", "TRUNCATED", "同上", "ok"),
        ("长文 600 行 / raw", 50999, 50999, 600, 2, "pass_through", "", "raw intent 不摘要，正确透传", "ok"),
        ("长文 600 行 / summary（R10）", 63046, 385, 2, 2507, "long_text", "TRUNCATED", "R10 采样 32KiB 后摘要（R10 前同量级 35.3s）", "ok"),
        ("长文 1000 行 / summary（R10）", 105376, 394, 2, 2233, "long_text", "TRUNCATED", "R10 采样，2233ms（R10 前 42.1s）", "ok"),
        ("列表 3000 行", 108779, 71927, 2004, 2, "listing", "TRUNCATED,SLOT_UNAVAILABLE", "超 1000 候选 → 毫秒级兜底", "ok"),
        ("列表 60 行 / 无 query（R10）", 3916, 3916, 60, 1, "listing", "", "R10 跳过 rerank，原样返回（R10 前 448ms）", "ok"),
        ("长文 600 行日志 / summary（R12 聚类）", 63046, 352, 2, 2626, "long_text", "TRUNCATED", "R12 先去重再采样 + embedding 聚类 312→3 行", "ok"),
        ("长文 4000 行唯一 / summary（R12）", 157300, 428, 2, 946, "long_text", "TRUNCATED", "R12 精确去重 4000→853，热态 946ms", "ok"),
        ("列表 3000 行 / 无 query（R12 字节 clamp）", 123779, 32969, 2000, 2, "listing", "TRUNCATED,SLOT_UNAVAILABLE", "R12 正文 32KiB clamp（改前 71927B）", "ok"),
        ("列表 1500 行 / rerankChunk 开（R12）", 96440, 32846, 2000, 13941, "listing", "TRUNCATED", "R12 分段重排 opt-in，chunked=true", "w"),
        ("短文本 60 行", 1179, 1179, 60, 1, "pass_through", "", "设计如此，原样透传", "ok"),
        ("错误 40KB（>32KiB）", 101779, 16397, 263, 1, "error", "sampled", "确定性压缩，不入模型", "ok"),
    ]
    brows = "".join(
        f"<tr><td>{esc(n)}</td><td class='num'>{i}</td><td class='num'>{o}</td><td class='num'>{ol}</td>"
        f"<td class='num'>{ms}</td><td>{esc(sh)}</td><td>{esc(nn) or '—'}</td><td>{esc(cmt)} <span class='b {'ok' if tag=='ok' else 'w'}'>{'达成' if tag=='ok' else '代价高'}</span></td></tr>"
        for n,i,o,ol,ms,sh,nn,cmt,tag in bench)
    body = f"""
<div class="lead"><b>一句话结论</b>：把 release 构建从「默认（无 HTTP 客户端）」换成 <code>--features external-protocols</code> 后，6 个本地模型槽位从 <code>no_client</code> 全部转为 <code>ready</code>，并且（R8 之后）<b>已真正接入 daemon 的 govern 降级链</b>：summarize / rerank 实测 local 命中并改写输出；无委派时 L0 机械兜底仍可跑通。治理效果按形状分层：长文强（28.8KB→101B）、列表超 1000 行毫秒级兜底（省时）、短错误 100% 保真、超 32KiB 错误确定性压缩。R9 起列表还可按请求 query 排序；R10 把 600 行摘要由 40.4s 降到 2.5s、60 行列表由 448ms 降到 1ms（长文触发线下调 + 小列表跳过重排）。</div>
<div class="kpi">
<div><div class="k">本地槽位状态</div><b>6 / 6 ready</b></div>
<div><div class="k">长文 28.8KB 治理后</div><b>101 B <span class="note">(0.4%)</span></b></div>
<div><div class="k">列表 3000 行兜底</div><b>2 ms</b><div class="note">71927B / 2004 行</div></div>
<div><div class="k">验证基线</div><b>554 / 547</b><div class="note">cargo test / feature</div></div>
</div>

<h2>1 · 配置修复：默认构建 → 带 HTTP 客户端</h2>
<table><tr><th>项</th><th>修复前</th><th>修复后</th></tr>
<tr><td>构建</td><td>cargo build --release（默认，无 reqwest）</td><td>... --features external-protocols（编入 reqwest）</td></tr>
<tr><td>toolgate list 槽位</td><td>6 个全部「原因=no_client」</td><td>6 个「绑定=自动，适配器 native/text」</td></tr>
<tr><td>govern 内 slmStatus</td><td>全 unavailable</td><td>参与槽位报 local/ready，未参与报绑定态</td></tr>
<tr><td>本地模型真实调用</td><td>空转（客户端未编入）</td><td>summarize/rerank/embedding 实测命中（见下）</td></tr>
</table>
<p class="note">槽位绑定（§3.10）：classify/decide=d3-nano(systemone:8082) · summarize=qwen36-a3b(openai:10010) · vision=minicpm-v(openai:10014) · rerank/embedding=jina/bge-m3(rerank/embedding:10020)。</p>

<h2>2 · 治理效果：按形状实测（本轮重跑）</h2>
<table><tr><th>样本</th><th>输入字节</th><th>输出字节</th><th>输出行</th><th>耗时ms</th><th>shape</th><th>notices</th><th>评价</th></tr>
{brows}
</table>
<p class="note">口径：直连 daemon /govern（<code>reports/bench_govern_budget.py</code>），<code>policy.isolation=summary</code>。raw intent 的长文/短文本按设计 pass_through；summary intent 才送 summarize。</p>

<h2>3 · 关键架构事实（相对 11-14 版已变化）</h2>
<ul>
<li><b>daemon govern 已用本地槽位</b>：R8 起 <code>crates/toolgate-daemon/src/local_slots.rs</code> 实现 summarize / classify_shape / rerank / embed，并由 <code>govern</code> 通过 <code>LocalSlots</code> 注入；slmPrefer 决定本地优先还是委派优先。</li>
<li><b>委派仍是一等通道</b>：宿主提供 delegate 端点时可把内容回投会话模型（slmPrefer=delegate），三态对比里 28.8KB→144B 即此路径（stub 验证）。</li>
<li><b>L0 兜底始终可用</b>：无本地、无委派时仍以确定性机械治理跑通（头尾采样、落盘、脱敏、错误三段式），符合红线 #3。</li>
<li>推论：旧版「daemon 的 govern 不用本地 SLM、配好本地模型对 govern 无增益」的结论<b>已不成立</b>——本地槽位现已进链。</li>
</ul>

<h2>4 · 缺陷状态（相对 11-14 版）</h2>
<h3>缺陷 A（曾严重）：Rust 标准编译错误不被识别 —— <span class="b ok">已修复</span></h3>
<p><code>looks_like_error</code> 已扩 <code>/usr/bin/ld:</code>、<code>ld:</code>、<code>undefined reference to</code>、<code>collect2:</code> 及 <code>error[CODE]</code> 前缀；本轮 error-like 由确定性嗅探直接命中（0.7ms）。</p>
<h3>缺陷 B：govern 与本地 SLM 解耦 —— <span class="b ok">已解决</span></h3>
<p>R8 后本地槽位已接入 govern 降级链（见 §3），本地模型对长文/列表/错误治理已有真实增益。</p>
<h3>缺陷 C：L0 机械兜底质量仍偏低 —— <span class="b w">仍存在</span></h3>
<p>无本地无委派时长文只做头尾采样硬截断、错误模板信息增益有限；这是刻意保底的代价，报告保留提示。</p>
<h3>缺陷 D：委派若用弱模型摘要不可靠 —— <span class="b w">提示</span></h3>
<p>生产中应回投宿主强模型，不用本地小模型充当委派。</p>
<h3>R10 已解决的短板：长文摘要延迟 + 小列表无谓重排 —— <span class="b ok">已落地</span></h3>
<p>长文采样触发线由 64KiB 降到 32KiB、取消 summary 放宽 120KiB：600 行 40.4s→2.5s、1000 行 42.1s→2.2s；无 query 且 ≤32KiB 的小列表不再调 rerank（60 行 448ms→1ms）。&gt;1000 行列表分段 rerank 成本约 35–40s，明确不做，维持毫秒级兜底。</p>
<h3>R12 可选增强与确定性改进 —— <span class="b ok">已落地</span></h3>
<p>默认关闭：<code>policy.rerankChunk</code>（超预算分批重排，1500 行 13.9s、默认关因十几秒级等待）与 <code>policy.sampleEmbedding</code>（采样前 embedding 选代表行）。默认生效的确定性改进：长文「先精确行去重再采样」（4000 行唯一 157KB→热态 946ms、600 行日志聚类 312→3 行）；L0/机械路径正文 32KiB 头截断（3000 行列表 71927B→32969B）。</p>

<h2>5 · 复现命令</h2>
<ul>
<li>构建 + 重启：<code>cargo build --release --features external-protocols</code> → <code>launchctl kickstart -k gui/$(id -u)/com.toolgate.daemon</code> → <code>./target/release/toolgate config check</code></li>
<li>槽位 A/B 与三态：<code>python3 reports/tg_slot_compare.py</code></li>
<li>预算与形状：<code>python3 reports/bench_govern_budget.py .toolgate/daemon.sock</code></li>
<li>query 三态：<code>python3 scripts/bench_govern.py --lines 120 --query '数据库迁移失败的原因'</code></li>
<li>R10 重测：见 <code>reports/bench-r10-2026-11-15.txt</code>（长文 600/1000 行、列表 60/120/3000 行）。</li>
</ul>"""
    return page("toolgate 配置修复与治理效果对比 · 2026-11-15",
                "生成时间 2026-11-15 · 工作目录 /Users/lanws/workspace/toolgate · 口径：daemon govern 实测", body)

def build_final():
    quadrant = [
        ("高收益 / 可接受成本", "ok", [
            "长文本（约 8–32KB）：本地摘要 0.4–0.7% 输出、2–3s 级延迟，是稳定的正收益场景。",
            "需要语义排序的多文档候选：rerank 有价值；R9 起可按请求 query 定向保留，候选 ≤1000。",
            "PTC 嵌套跳过：38ms，只在结果真正进模型时治理一次。",
        ]),
        ("高收益 / 高成本", "w", [
            "超大长文（180KB 级）：先采样 32KiB 再摘要，2200 行 4.2s；不采样则更慢。",
            "600 行 summary：R10 采样后仅 2.5s（原 35.3s）——已转为可接受成本。",
        ]),
        ("低收益 / 高成本", "w", [
            "短错误：分类命中但正文保真、0% 压缩；价值在可归因不在瘦身。",
            "60 行短列表：R10 已改为无 query 时跳过 rerank（1ms）。",
        ]),
        ("低收益 / 低成本", "ok", [
            "结构化 JSON：只补 meta/data，≈1ms。",
            "列表超 1000 行：毫秒级兜底（2ms），不请求模型。",
        ]),
    ]
    qhtml = "".join(
        f"<h3>{esc(t)} <span class='b {cls}'>{'正收益' if cls=='ok' else '注意'}</span></h3><ul>{''.join(f'<li>{esc(i)}</li>' for i in items)}</ul>"
        for t, cls, items in quadrant)
    body = f"""
<div class="lead"><b>结论</b>：机制已闭环，作用按形状分层。相对 2026-11-14 版，11-14 提出的三个 P0 建议（列表 2s 预算、长文先采样再摘要、/stats 可观测性）已全部落地；本地槽位也已真正接入 govern 降级链（R8），列表可按请求 query 排序（R9）。R10 已把长文摘要延迟降到 2–3s（600 行 40.4s→2.5s）；&gt;1000 行列表分段 rerank 成本 35–40s 远高于 2s 预算，明确不做，维持毫秒级兜底。R12 增两个默认关可选增强（分段 rerank、embedding 选行）与 L0 字节 clamp；3000 行列表 71927B→32969B。</div>
<div class="kpi">
<div><div class="k">长文 28.8KB → </div><b>101 B <span class="note">(0.4%, 2.5s)</span></b></div>
<div><div class="k">列表 3000 行 兜底</div><b>2 ms</b><div class="note">71927B / 2004 行</div></div>
<div><div class="k">R9 query 三态</div><b>已落地</b><div class="note">applied / rejected / not_provided</div></div>
<div><div class="k">验证基线</div><b>554 / 547</b><div class="note">cargo test / feature</div></div>
</div>

<h2>01 机制：成立，且已在真实链路闭环</h2>
<ul>
<li>治理对象是 <code>content</code>（模型可见投影），不是 PTC 内层 <code>value</code>；适配器挂 post-execute 治理前者。</li>
<li>嵌套调用跳过 /govern（内层 38ms，旧行为约 10.7s）。</li>
<li>降级链 local → delegated → L0 三档，<code>policy.slmPrefer</code> 可切；本地槽位现已在这条链上。</li>
<li>错误转发保真：fact/reason/nextSteps 只进 <code>structuredContent.error</code>。</li>
</ul>

<h2>02 作用评估：四个象限（本轮实测口径）</h2>
{qhtml}

<h2>03 三态：默认本地优先，会话模型是可选策略</h2>
<table><tr><th>形态</th><th>原始</th><th>输出</th><th>模型 / 来源</th><th>延迟</th></tr>
<tr><td>① 无包装 raw</td><td class="num">28,766 B</td><td class="num">28,766 B (100%)</td><td>—</td><td>—</td></tr>
<tr><td>② 默认本地优先</td><td class="num">28,766 B</td><td class="num">101 B (0.4%)</td><td>local:summarize</td><td class="num">1984.6ms</td></tr>
<tr><td>③ slmPrefer=delegate + stub</td><td class="num">28,766 B</td><td class="num">144 B (0.5%)</td><td>session / stub</td><td class="num">1341.3ms</td></tr>
<tr><td>④ 无本地无委派 L0</td><td class="num">28,766 B</td><td class="num">28,766 B (100%)</td><td>L0/机械</td><td class="num">1.5ms</td></tr>
</table>
<p class="note">③ 用 stub 验证链路可达，不代表真实宿主延迟；本机 summarize 放本地 qwen3.6-35B-A3B，换隐私与零外部成本，代价是延迟。</p>

<h2>04 已完成 / 待办</h2>
<h3><span class="b ok">已完成</span> 11-14 版 P0 建议全部落地</h3>
<ul>
<li>列表治理 2s 预算 + 确定性兜底（超 1000 候选毫秒级，不再白等模型）。</li>
<li>长文先采样再摘要（&gt;64KB 先采样到 32KiB；intent=summary 放宽 120KiB；完整原文落盘回指针）。</li>
<li><code>/stats</code> 可观测性（按 shape 的请求数/字节/耗时）与结构化治理日志。</li>
<li>额外：本地槽位入链（R8，含 embedding 长文去重）、列表按请求 query 排序（R9，三态 + DEGRADED 告知 + query 作为数据字段）。</li>
</ul>
<h3><span class="b ok">已完成</span> R10 治理时间预算优化</h3>
<ul>
<li><b>长文摘要延迟</b>：采样触发线 64KiB→32KiB、取消 summary 放宽 120KiB；600 行 40.4s→2.5s、1000 行 42.1s→2.2s。</li>
<li><b>小列表无谓重排</b>：无 query 且 ≤32KiB 的小列表跳过 rerank（60 行 448ms→1ms）。</li>
</ul>
<h3><span class="b w">待办</span> 剩余短板</h3>
<ul>
<li><b>vision 本地未接</b>：受「图片只传引用」限制，除非宿主提供像素或本地解码，否则维持 L0/委派。</li>
<li><b>第三级 classify 偶发误判</b>：实测 60 行 info 日志在 intent=debug/summary 时被判 long_text 并摘要为空（45B），raw 正常透传；需加确定性护栏，列为下一个独立主题。</li>
</ul>
<h3><span class="b no">不做</span> 明确不在本期</h3>
<ul>
<li>&gt;1000 行列表分段 rerank：成本约 35–40s（1000 docs≈11–13.6s×3）远高于 2s 预算，维持毫秒级兜底不排序。</li>
<li>不为「槽位齐全」而接 decide / vision；decide 属工具判定用途，不在 govern 链。</li>
<li>不为 query 单独写 ADR（向后兼容的可选字段，§18.7 已记录契约）。</li>
</ul>

<h2>05 边界与风险</h2>
<ul>
<li>冷启：本地 summarize 首次曾 23.5s；daemon 启动后台预热已缓解，重启后首个大请求仍可能长等待。</li>
<li>长文硬上限：超大输入必须先减输入，只调超时会拉长等待；R10 采样固定 32KiB（头+关键行+尾），完整内容走落盘指针。</li>
<li>rerank 服务端 1000 docs 上限：daemon 按 1000 分片串行合并分数。</li>
<li>短错误不省字节、结构化 JSON 不压缩：均为有意的正确性/契约选择。</li>
<li>单机单次实测，非吞吐/并发结论；平台仍为 macOS 第一期。</li>
</ul>

<h2>06 方法与复现</h2>
<ul>
<li>槽位 A/B 与三态：<code>reports/tg_slot_compare.py</code>（A=正式 config，B=HOME=/tmp/tg_nohome）。</li>
<li>预算与形状：<code>reports/bench_govern_budget.py .toolgate/daemon.sock</code>。</li>
<li>query 三态：<code>scripts/bench_govern.py --lines 120/1200 --query ...</code>，输出存 <code>reports/bench-rerank-query-*-2026-11-14.txt</code>。</li>
<li>验证：cargo test --workspace 548；--features external-protocols 541；clippy 无告警；check-doc.sh 通过（7081 行）；适配器 node --test 35。</li>
</ul>"""
    return page("toolgate 最终评估：作用、边界与下一步（2026-11-15）",
                "toolgate · final assessment · 2026-11-15 · 单机实测，字节 UTF-8，延迟墙钟", body)

def main():
    outs = {
        "toolgate-slot-comparison-2026-11-15.html": build_slot(),
        "toolgate-govern-comparison-2026-11-15.html": build_govern(),
        "toolgate-final-assessment-2026-11-15.html": build_final(),
    }
    for name, content in outs.items():
        p = os.path.join(BASE, name)
        open(p, "w", encoding="utf-8").write(content)
        print(name, len(content), "bytes")

main()
