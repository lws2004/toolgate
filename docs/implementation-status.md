# toolgate 实现进度表

> **本文件是「实现状态」的唯一事实源**：哪项做了、证据在哪、怎么验证。
> 架构决策仍以 [architecture.md](architecture.md) 与分册为准；本文件只回答「做到哪了」，
> 不复制架构正文，只给指针与证据。
>
> **更新纪律**：每次改动落地后更新对应行（状态 + 证据）。判断下一步只读本文件，
> 不要重新扫描代码与架构正文——那既慢又容易与事实源漂移。
>
> 最后更新：2026-11-15（新增 R12 分段 rerank / 长文先去重再采样 / 按密度选行 / L0 机械字节 clamp（均为默认关的可选增强或确定性改进）；新增 R10 治理时间预算优化（长文采样触发线下调 64→32KiB、取消 summary 放宽、小列表无 query 跳过 rerank）；daemon + govern/pre 新形态；本地 6 槽位接入 govern 降级链并新增 slmPrefer 策略；缺陷 A 修复；边界修复：summarize/rerank 超时放宽、rerank 1000 docs 分片、链接器错误识别、适配器 30s；治理时间预算（长文先采样再摘要、列表超预算机械兜底、错误确定性压缩）+ daemon GET /stats 与结构化治理日志 + 适配器按形状分级超时；长文送模型前先做输入去重（精确行 + 有界窗口 embedding 聚类）与 slmStatus 未参与槽位改报绑定态；列表重排按请求可选 query 排序（三态 queryApplied + 丢弃时 DEGRADED 告知 + query 作为数据字段）；R11 classify 判定体量自洽护栏（修复缺陷 E：60 行 info 日志被 classify 误判 long_text、摘要空 45B）；ADR 0009：P1 rerank 预算加字节项（已实现），P2 daemon 冷启预热已评估否决大样本、保留轻量；配置段接线：limits 段已接线（解析 max_lines/max_bytes 注入 ToolContext.limits）；spill 段已接线（ttl_days/max_bytes 的 TTL 清理与配额淘汰）；snapshot 段已接线（data/min_free_bytes/keep_per_file/min_retention_hours/ttl_days/max_bytes/gc_interval_hours 解析，写入时 TTL/每文件保留 N/每工作区配额淘汰 + statvfs 空闲下限 + `snapshot prune` CLI）；logging 段已接线（CLI 启动期按 `logging.level` 设 EnvFilter 默认、daemon 日志迁 tracing）；isolation 段阶段1+2 已接线（isolation.* 解析注入 + govern_with_policy 按 max_exposure/路径规则算硬顶、none 档拒绝、intent 收紧，默认 max_exposure=raw 向后兼容）；验证基线见文末验证基线表）

## 怎么用

1. 读「当前待办」一节的表，判断下一步做什么；
2. 动手前按「架构依据」列读对应章节（不读整本）；
3. 完成后更新该行状态与证据（提交号 / 测试名 / 验证命令）；
4. 状态只有三种：`完成`（有测试或实测证据）/ `部分`（已接入但验收未过，证据列写缺口）/ `未做`。

## 当前待办（按优先级）

| 项 | 状态 | 架构依据 | 缺口 |
|---|---|---|---|
| §15.2 判定日志（decisions.jsonl） | 完成 | §15.2 / §15.6 / §3.11 | `decision_log.rs`（sha256 输入哈希 / RFC3339 时间戳 / JSONL 追加写）+ 五个判定点接线；3 个单测覆盖字段与脱敏、追加写与 null、时间戳基准。五条路径均在归并结论后按 §7.3 口径传入 `confidence`：bash（`min(has_error, 选中 kind)`）、task（`min(选中 state, 选中 progress)`）、edit（`min(选中 kind, fixable)`）与 read 的两条同源；该路径无产出数值时记 null，不编造 |
| decide 批处理（方案 B，turn 内聚合） | 不实现（前提不成立） | §13.4 | 一次 decide 调用只读一个 state，不同工具的输出无法合并；可合并的「同一 state 多问题」在单次调用内已问完。论证与 turn 载体现状见 §13.4「实现状态」 |
| 有损 WebP | 未做（条件触发） | §10.2b | 三条触发判据未出现，image_pipeline 只检测格式 |
| 流式输出 | 不做 | §14 阶段 4 | MCP progress notification 不改变返回形态，已定不做 |
| 立项 P1：rerank 预算按字节估算 | 完成（ADR 0009 P1） | §4.0a / [ADR 0009](decisions/0009-性能优化立项-rerank预算按字节与冷启预热.md) | 预算 `100+15*lines` 只看条数，120 行×39KB 无 query 实测 4952ms 超 2s；目标纳入字节维度，不放松「≤max_lines 且 ≤32KiB 且无 query 跳过」与 1000 候选/2s 默认；已实现：`rerank_estimated_ms` 加 `0.005ms/字节` 项；实测 39KiB/120 行 4952ms→1ms 兜底、120 行短带 query 仍 applied 955ms；测试 `listing_over_32kib_without_query_budget_fallback` + 估算字节断言 |
| 立项 P2：daemon 冷启预热策略 | 已评估（不采纳大样本，ADR 0009 P2） | §18.1 / §3.8 / [ADR 0009](decisions/0009-性能优化立项-rerank预算按字节与冷启预热.md) | 现 `prewarm` 仅送 8-token 小样本，重建后首个 4000 行 157KB 长文首请求 19.4s（热态 946ms）；实测否决大样本预热：1500 行 43KiB 预热后首个 157KiB 请求 38647ms vs 轻量 1498ms（实例内串行竞争，§18.1 第 4 条）；保留轻量预热 |
| 配置段接线：`limits` | 完成 | §9.1 / 附录 D | `config.rs` 解析 `limits.{max_lines,max_bytes}`（默认 2000/51200，未知字段→CONFIG_UNKNOWN_FIELD，0→CONFIG_INVALID_VALUE，非数字→CONFIG_TYPE）；CLI `build_context` 注入 `TruncationOptions` 到 `ToolContext.limits`，bash/read/search/task 共用；5 个单测 |
| 配置段接线：`spill` | 完成 | §22.5 / 附录 D | `config.rs` 解析 `spill.{ttl_days,max_bytes}`（默认 7 天 / 1GiB）；`SpillStore` 增 `set_policy` + 写入前 TTL 清理与每工作区配额淘汰（失败不阻断写入）；`ToolContext::new` 默认启用、CLI `build_context` 按配置覆盖；4 个 config 单测 + 1 个配额淘汰单测 |
| 配置段接线：`snapshot` | 完成 | §20 / 附录 D | `config.rs` 解析 `snapshot.{data,min_free_bytes,keep_per_file,min_retention_hours,ttl_days,max_bytes,gc_interval_hours}`（默认 auto / 512MB / 20 / 24h / 30d / 1GiB / 24h，`data` 只收 auto/reflink/copy）；`SpillStore` 增 `set_snapshot_policy` + 写入前 TTL / 每文件保留 N / 每工作区配额淘汰 + `statvfs` 空闲下限拒创建 + `prune_now`；CLI `snapshot prune`；写入前快照失败即拒绝写入；5 个 config 单测 + 1 个保留份数淘汰单测 |
| 配置段接线：`logging` | 完成 | §19.6 / 附录 D | `config.rs` 解析 `logging.level`（默认 `warn`，只收 trace/debug/info/warn/error/off）；CLI 启动期按 `logging.level` 设 `EnvFilter` 默认（`RUST_LOG` 优先）；daemon 三处 `eprintln!` 迁 `tracing`；3 个 config 单测 |
| 安全类配置段接线：`isolation` / `paths.expose_absolute` / `govern.fail_open` | 部分完成（阶段1–4 已落地；`govern.fail_open` 层位在适配器仓，本仓不接） | §8 / §18.7 / 附录 D | **isolation 阶段1+2 已落地**：`config.rs` 解析 `isolation.{default,max_exposure,rules,content_rules,fallback_chain,redact_min_retention}`（未知字段→CONFIG_UNKNOWN_FIELD、非法档/回退动作/保留率越界→CONFIG_INVALID_VALUE）注入 `IsolationConfig`，daemon `ServerCtx` 启动加载；govern 入口新增 `govern_with_policy`：`effective=min(max_exposure,请求级 isolation,路径规则)` 算输出硬顶、`none` 档拒绝告知、其余收紧 intent（复用 `decide_output`），默认 `max_exposure=raw` 向后兼容，旧 `govern`/`govern_with_slots` 不变；7 config + 3 govern 单测。**阶段3**：`sensitivity.rs` 新增 `redact_secrets`/`redact_identity`，`govern_with_policy` 按 `content_rules` 中 `level=redact` 的规则改写 content 并记 `meta.contentRedacted`；`sensitive_path_mention` 复用 `IsolationConfig::path_rule_for` 判敏感路径提及、`high_entropy` 走 `redact_high_entropy`，五种 `content_rules` 与 `fallback_chain` 降级执行一并在返回前落地（`redact` 仅在保留率 ≥ `redact_min_retention` 时采纳，否则 `structure`/`summarize`/`refuse`，链耗尽 fail closed 拒绝；降级不返回原文：`structure` 产真实骨架、`summarize` 接本地槽位产摘要、`refuse` 只给一行结构化告知，`ISOLATION_FALLBACK` 的 actual/reason 按实际生效级改写）。**阶段4**：`config.rs` 解析 `paths.expose_absolute`（布尔，默认 false，非法值→CONFIG_TYPE）、`ToolContext.expose_absolute` 注入、`read_path.rs` 的 `display_path` 支持绝对路径且过 `path_username` 规则，read/write/edit 共 7 处调用点透传。**`govern.fail_open` 不接**：层位在宿主适配器仓调用 daemon 失败/超时的降级（§18.7），本仓 `govern_with_slots` 无治理失败 Err 分支，配了无消费点会造成配置幻觉（§18.9）。阶段3/4 新增 7 个回归单测（3 sensitivity + 1 govern + 2 config + 1 read_path），内容规则与回退链执行再增 5 个（1 sensitivity + 4 govern），回退链逐级落地再增 2 个 govern 单测。见 [ADR 0010](decisions/0010-安全类配置段接线立项-isolation与路径呈现与治理失败策略.md) |

## 阶段 R：daemon + govern/pre 接缝（2026-11-14 新形态）

| 项 | 状态 | 架构依据 | 证据 |
|---|---|---|---|
| R1 daemon 常驻 + Unix socket 手写 HTTP | 完成 | §18.2 / §18.7 / §1.5 | 新 crate `crates/toolgate-daemon`（`serve()` 监听 Unix socket，手写 HTTP/1.1，不引入 axum/hyper）；路由 POST /govern、POST /pre、GET /health；集成测试 `crates/toolgate-daemon/tests/seams.rs`（真实 socket 往返 health/govern/pre）；CLI 新增 `toolgate daemon [--socket <path>]` |
| R2 govern 六类处理器（按形状路由、免白名单） | 完成 | §4.0a / §4.1 / §5.3 | `crates/toolgate-core/src/govern.rs`：`sniff_shape` 确定性信号（image block / data URI / 魔数；JSON；错误标记 / 列表 / 长文 / 元信息）+ `govern_with()` 分派六个 processor；14 个单测覆盖视觉/透传/结构化/错误/长文/空输入、委派成功/失败，并含「文档正文出现 error: 字样不被误判为错误」与第三级 classify 回归。视觉/长文/列表/错误四类在 `policy.delegate` 存在时反向委派给宿主模型（见 R5），无回调时降级 L0 并告知。第三级 classify 已接：`intent != raw` 且确定性嗅探无结论时，经反向委派 classify 任务在 long_text/listing/error/pass_through 中判定，结果写入 `structuredContent.classifiedShape`；raw 意图不额外调用模型 |
| R3 pre 写前复核 + 写前快照 | 完成 | §18.6 / §18.7 / §20 | `crates/toolgate-core/src/precheck.rs`：`pre()` 逐条按 (mtime,size,sha256) 复核，不一致返回 PRE_STALE 并 block；一致则 `spill.snapshot` 写前快照；4 个单测（匹配放行+快照 / sha 变化阻断 / 新文件无指纹放行 / 期望存在但文件消失阻断） |
| R4 薄适配器独立仓（pi / dsh） | 完成（dsh；pi 暂不做） | §18.2 / §18.6 | 独立仓 `/Users/lanws/workspace/toolgate-dsh-adapter`：Cordis 插件 `@toolgate/dsh-adapter`，零执行逻辑零工具声明，只挂 `tools/post-execute`（→ daemon `POST /govern`）与 `tools/pre-execute`（`write`/`edit` → `POST /pre`）；图片只传引用+元信息（`attachmentId/mimeType/bytes/width/height/sha256`），不传 base64 字节；写侧期望指纹取「该 agent 上次 read 到的真值」，缓存按 `exec.agent` WeakMap 隔离；PTC 嵌套工具（`exec.parent` 非空）跳过 `/govern`（其结果只回外层程序、不进模型上下文），只治理最终进上下文的外层结果；`node --test test/*.test.mjs` = 31 passed（含 11 委派协议单测 + 多 agent 缓存隔离）；`check_plugin_spec.py` 0 E / 0 W；真实 DSH headless 实测：读 3000 行文件触发长文治理并成功反向委派 `summarize`（模型 `auto/deepseek-v4.1-flash` 返回摘要）、`edit` 生成写前快照；宿主 web profile `/Users/lanws/.dsh/profiles/web/cordis.patch.yml` 已挂载 `toolgate-adapter`（`socketPath` 指向 `toolgate/.toolgate/daemon.sock`），旧 `mcp:toolgate` 条目已从 `capability-catalog.mcp` 下线；重启后 web PID 84297、`delegate-84297.sock` 活性，长文治理实测返回摘要 + `[TRUNCATED]` 落盘指针 |
| R5 反向委派（无本地 SLM 回投宿主会话模型） | 完成（dsh；pi 暂不做） | §18.7 三约束 | core `govern_with(req,spill,Some(&dyn Delegate))` 在 vision/summarize/rerank/classify 四处调用委派，失败/无回调降级 L0；daemon `delegate.rs` 实现 `HttpDelegate`（`unix:` / `http://127.0.0.1`，超时 8s、输出预算 512 token）；适配器 `createDelegateServer` 起同目录 Unix socket，`handleDelegatePayload` 把单条内容交 `ctx.llm.stream`（`ctx.get('llm')` + `agentDefaultModel.currentSelection()`，不声明硬依赖），token 单次登记不落盘；`crates/toolgate-daemon/tests/seams.rs` 真实 socket 委派往返测试通过；真实 DSH headless 长文委派实测成功 |
| R6 govern 本地 SLM 槽位接线 + slmPrefer 策略 | 完成 | §3.8 / §3.10 / §4.2 | `crates/toolgate-core/src/govern.rs`：`Policy.slmPrefer`（camelCase，auto/local/delegate，缺省 auto=本地优先）；新增 `LocalSlots` trait 与 `govern_with_slots()`；vision/summarize/rerank/classify 四处先本地后委派、皆失降 L0；`slmStatus` 标 `local`/`delegated`；缺陷 A 修复 `looks_like_error` 识别 `error[CODE]`。`crates/toolgate-daemon/src/local_slots.rs`：`LocalSlotBackend` 把 `SlotClient` 包成 `LocalSlots`（独立线程 + 专属 Runtime）；`lib.rs` 注入 `ServerCtx.local` 并在 `/govern` 传 `govern_with_slots`。单测覆盖本地优先 / delegate 优先 / 本地不可用降委派 / 全不可用降 L0 / classify 标 local / rustc error[CODE] / 链接器错误 / 分片合并排序。实测（2026-11-14，`reports/tg_slot_compare.py`）：正式 daemon（HOME 正常，6 槽位绑定）下 summarize/rerank/classify 分别报 `local` 并治理输出，同一二进制以 `HOME=/tmp/tg_nohome`（无 config→本地槽位 None）起第二实例时三者降 L0；vision 本地未接（`process_vision` 只走委派/元信息，`vision_text` 未被调用）。边界修复：summarize 单请求超时 120s、rerank 180s（classify/decide 仍 30s）；daemon 启动后台预热 summarize；rerank 按 1000 docs 分片并全局合并（3000 行 19.5s→2000 行，`slmStatus.rerank=local`）；`looks_like_error` 增链接器前缀；适配器 `timeoutMs` 30000（旧 8000 会 failOpen 透传）。复测 2200 行 / 约 183KB 长文 17.4s、摘要 335B、`slmStatus.summarize=local` |
| R7 治理时间预算 + /stats 可观测性 | 完成 | §4.0a（治理时间预算）/ §18.7（可观测性） | core `truncate::sample_ends`（≤上限原样；否则头 2/5 + 关键行 1/5 + 尾 2/5，关键行扫 error/failed/panic/traceback/错误/失败/异常/路径:行号）；`govern.rs` 常量 长文采样触发线/目标与 summary 放宽（已由 R10 调整：触发线 32KiB、取消 summary 放宽，见 R10 行与 §4.0a）`LONG_TEXT_SAMPLE_TARGET_BYTES=32KiB`、`ERROR_FIDELITY_LIMIT_BYTES=32KiB`、`ERROR_COMPRESS_TARGET_BYTES=16KiB`、`RERANK_CANDIDATE_LIMIT=1000`、`RERANK_BUDGET_MS=2000`，估算式 `rerank_estimated_ms = 100 + 15*lines`；`process_long_text` 触发后采样 32KiB 再 truncate_tail，`process_listing` 超预算一律 `SlotUnavailable{rerank,Mechanical}` 且 >2000 行用 `deterministic_listing` 头尾截断，`process_error` >32KiB 采样 16KiB 并跳过 classify 委派。daemon `Stats`（totals/byShape/slots/degradations）+ 每次 govern 一行结构化日志 + `GET /stats`。适配器 `governTimeoutMs` 分级（短 5s / 列表 2s / 长文 cfg.timeoutMs / summary 120s），超时 failOpen 附「（toolgate 治理超时 Nms，已原样返回）」。实测（2026-11-14，`-p toolgate-cli --features external-protocols` release + launchd daemon，`reports/bench_govern_budget.py`）：长文 2200 行/186,999B→469B/7 行（热态 1281ms，冷启 14651ms）；长文 600 行/50,999B summary→286B/3 行（热态 1851–2135ms，冷启 29327ms）；列表 3000 行/108,779B→2004 行/71,927B 2ms（`SLOT_UNAVAILABLE:rerank` 机械兜底）；错误 40KB/101,779B→16,397B/263 行 3ms（sampled=true，不调模型）；短文本 1179B pass_through 1ms；`/stats` totals.requests=7、avgLatencyMs=6465、byShape 四类、slots 五槽位计数、degradations `SLOT_UNAVAILABLE:rerank`+TRUNCATED。测试：cargo test 534 passed、feature 527 passed、daemon seams 5 passed（含 stats 用例）、适配器 `node --test` 33 passed（注：其中长文触发线与 summary 放宽已由 R10 调整，见 R10 行） |
| R8 embedding 接入长文输入去重 + slmStatus 报绑定态 | 完成 | §4.0a（长文去重）/ §7.3（状态真实性）/ §3.10 | `crates/toolgate-core/src/semantic_dedup.rs` 抽出 `unique_lines`（精确行去重、保首现序）与 `cluster_representatives`（余弦聚类、阈值 0.92）纯函数，原 `dedup_lines` 改为调它们 + `client.embedding`；`govern.rs` 的 `LocalSlots` 增 `embed` / `slot_status`，`process_long_text` 在送 summarize 前先精确行去重、再在 ≤800 行 / ≤32 KiB 窗口内按 ≤32 分批 embedding + 余弦聚类，统计写 `structuredContent.dedup`（inputLines / uniqueLines / outputLines / batches），只作用于送模型文本、L0 兜底仍返回原文；`finish` 经 `apply_slm_base` 把未参与槽位由占位 `unavailable` 改为绑定态（ready / degraded / unavailable），六键含 `embedding`。`crates/toolgate-daemon/src/local_slots.rs` 实现 `embed`（32 分批、维度不齐返回 None）与 `slot_status`（`SlotBlocker`→四态）。测试 +8（core 8 个新单测；daemon `seams.rs` 的 stats 断言随状态真实性调整）。实测（2026-11-14，`reports/bench_govern_budget.py`，输出存 `reports/bench-retest-2026-11-14.txt`）：重复行长文 2100 行 14,699B → `dedup{inputLines:2000,uniqueLines:3,outputLines:3,batches:1}`、`slmStatus.summarize/embedding=local`、925ms；长文 2200 行 186,999B → 782B/13 行 3596ms；长文 600 行 50,999B summary → 294B/3 行 2465ms（`classify`+`summarize` local）；短文本 pass_through 六槽位全 `ready`（改前恒 `unavailable`）；`/stats` slots.embedding.local=3、summarize.local=4 |
| R9 列表重排按请求 query 排序（三态 + 告知 + 数据/指令隔离） | 完成 | §18.7 / §4.0a / §4.4b / §15.1 / §22.5 | `govern.rs`：`GovernRequest.query`（可选、serde default）、`DEFAULT_RERANK_QUERY`、`natural_query()`（trim 后 2..=200 字符、拒换行与 `^ $ \\ | ( ) [ ] { } * + ? / < >`）、`process_listing` 三态 `structuredContent.queryApplied`（applied / rejected_non_natural / not_provided）、query 被丢弃时 `DEGRADED`（requested=none actual=generic_intent reason=non_natural_query slot=rerank）进 content、委派 input 改 `relevance_query` 独立数据字段（不插值指令句）。`notice.rs`：`DegradedActual::GenericIntent`、`DegradeReason::NonNaturalQuery`。适配器 `naturalQueryOf`（优先宿主 `naturalQuery`，否则仅白名单搜索类工具取 `arguments.query`，拒正则/路径/超长）。测试 +4（core 三个 listing 场景 + 委派数据字段隔离；适配器 +2）。文档：§18.7 请求/响应契约、§4.0a、§4.2、§4.4b 值域、§3.8 降级链（不降 embedding）同步。实测（2026-11-14，`reports/bench-rerank-query-2026-11-14.txt`，daemon 带 external-protocols）：120 行列表带 query「数据库迁移失败的原因」→ 5 条 migration 行排前 5、`queryApplied=applied`、`rerank=local`、905ms；不带 query → 相关行散在中后、`not_provided`；query=「^fn .*handler」→ `rejected_non_natural`、notices 含 DEGRADED（actual=generic_intent reason=non_natural_query）、content 末行 `[DEGRADED] requested=none actual=generic_intent reason=non_natural_query slot=rerank` |
| R10 治理时间预算优化：长文采样触发线下调 + 小列表跳过重排 | 完成 | §4.0a（治理时间预算）/ §18.7（query 三态）/ §3.8（消费点） | core `govern.rs`：`LONG_TEXT_SAMPLE_TRIGGER_BYTES` 64KiB→32KiB，删除 `SUMMARY_INTENT_MAX_BYTES`（120KiB）与 `process_long_text` 的 summary 放宽分支（统一 `sampled = total_bytes > 32KiB`，目标 `LONG_TEXT_SAMPLE_TARGET_BYTES=32KiB`）；新增列表正文 32KiB 无 query 跳过（该常量后由 R12 收尾合并为 `LISTING_BODY_MAX_BYTES`，见 R12 行），`process_listing` 在 `query_state=="not_provided" && total<=max_lines && joined.len()<=32KiB` 时提前返回（不调 rerank、无 queryApplied、无 SLOT_UNAVAILABLE）。测试：`listing_small_without_query_skips_rerank`（跳过侧）、`listing_over_32kib_without_query_still_reranks`（不跳过侧，120 行长行 39KB 仍 rerank）；`long_text_under_trigger_keeps_full_input` 改用含 ANSI 的短文本（≤32KiB 但判 long_text）。文档 §4.0a/§3.8/§18.7 同步。实测（2026-11-15，`reports/bench-r10-2026-11-15.txt`，daemon 带 external-protocols）：长文 600 行 63046B→385B/2 行 2507ms（sampled=true，R10 前 40385ms）；1000 行 105376B→394B/2 行 2233ms（前 42070ms）；列表 60 行 3916B 无 query→原样返回 1ms（前 448ms）；120 行带 query→applied/rerank=local/1198ms；3000 行无 query→兜底 3ms；120 行 39KB 长行无 query→not_provided/rerank=local/4952ms（超 32KiB 仍排，估算按行数未计字节为已知边界）。测试基线 cargo test 549 / feature 542。 |
| R11 classify 判定体量自洽护栏（修复缺陷 E） | 完成 | §4.0a（三级信号）/ §4.2 | core `govern.rs` 新增 `classify_plausible(shape, joined)`：classify 判定须与体量自洽——`long_text` 要求超 50KiB / 2000 行或含 ANSI（`pass_through` 下必为误判，丢弃）、`listing` 要求 ≥20 非空行、`error` 放行；`govern_with_slots` 采纳 classify 前 `.filter(|(s,_)| classify_plausible(*s,&joined))`。文档 §4.0a 补体量自洽条款。测试 +1 `classify_long_text_on_small_input_is_discarded`（60 行 info 被 classify 判 long_text → 保持 pass_through、无 classifiedShape）。实测（2026-11-15，launchd daemon，缺陷 E 原场景）：60 行 info `intent=debug` 改前 long_text 空摘要 45B → 改后 pass_through 5709B 原样 468ms；`intent=summary` 同 364ms；回归：600 行 summary long_text sampled=true 331B 2394ms、120 行列表无 query skip 1ms、带 query applied 892ms。基线指针见下方验证基线表（R11 时点为 cargo test 549 / feature 543、check-doc 7067 行，已随 R12 更新）。 |
| R12 分段 rerank（默认关）+ 长文先去重再采样 + 按密度选行（默认关）+ L0/机械字节 clamp | 完成 | §4.0a（治理时间预算）/ §18.7（policy）/ §3.8（消费点） | core `truncate::sample_ends` 重写（头 2/5 + 关键行 1/5 + 尾 2/5，关键行额度未用尽时从中间区两侧回收给头尾）；新增 `truncate::clamp_head_bytes`（行边界保留开头 + 略去说明，全篇截断返回 Some(0)）；`govern.rs` Policy 增 `rerankChunk{enabled, maxTotalMs}` 与 `sampleEmbedding`（camelCase，均默认关）、常量 `LISTING_BODY_MAX_BYTES=32KiB`；`process_listing`：`est_ms=rerank_estimated_ms(total)`、`within_budget=total<=1000 && est_ms<=2000`、`chunked=rerankChunk.enabled && !within_budget && maxTotalMs.is_none_or(|m| est_ms<=m)`，`within_budget||chunked` 才调 rerank，rerank 成功与兜底都 `clamp_head_bytes(...,32KiB)` 并命中记 `Truncated{Bytes}`；`process_long_text` 改「先 `unique_lines` 去重、再按去重后体量判 `sampled`」，`sampled && policy.sampleEmbedding==Some(true)` 时走 `sample_by_density`（≤800 行按 ≤32 批 embed + `cluster_representatives` 选代表行）否则 `sample_ends`；`dedup_model_input_with(slots,text,input_lines)` 传原始行数，使 `stats.inputLines` 反映真实重复度。新增测试 6 个（core 单 crate 单测总数 527，详见验证基线表）。实测（2026-11-15，daemon external-protocols）：600 行日志 summary 63KB→352B/2626ms（embedding 聚类 312→3 行）；4000 行唯一长文 157KB→428B/热态 946ms（精确去重 4000→853，首请求冷启 19.4s）；2100 行重复长文 63KB→93B/752ms（先去重压成 1 行不再采样）；3000 行列表无 query 123KB→32969B/2ms（字节 clamp，改前 71927B）；1500 行列表 + rerankChunk 96KB→32846B/chunked/applied/13941ms；60 行列表无 query 2ms、带 query 364ms/applied。基线见下方验证基线表（R12 收尾已合并 32KiB 常量并补 2 个单测；clippy 0、build 0 警告）。 |

## 阶段 1：SLM 接入层

| 项 | 状态 | 架构依据 | 证据 |
|---|---|---|---|
| 六槽位客户端适配 | 完成 | §3.2 / §3.9 / §3.10 / §18.1 | `crates/toolgate-core/src/slm/`（systemone / text / embedding / ordered / vision / json）；`toolgate decide` 端到端实测见 §14 |
| 判定封装（命名问题 → 带概率答案） | 完成 | §3.2 / §7.2 | `decision.rs`、`slm/systemone.rs` |
| 置信度门控 | 完成 | §7.2 | `slots.rs` ConfidenceGate（Adopt / AdoptWithLowConfidence / BothViews） |
| 降级路径（槽位不可用回退 L0） | 完成 | §3.8 | 各工具 `unavailable` 分支 + `SLOT_UNAVAILABLE` 告知 |
| 错误自检三层 + ErrorCode 12 值域 | 完成 | §1.4.5 | `error.rs`；测试 `every_error_carries_next_steps` |
| slmStatus 状态真实性（构建可用性） | 完成 | §7.3 / §19.6 | `capability::client_available` / `source_slot_usable`：默认构建下外部协议来源标 `unavailable`，不再谎报 `ready`；测试按 feature 分支 |
| 槽位不可用原因与下一步（slmStatusDetail） | 完成 | §4.4b / §6.1 / §18.9 | `capability.rs` 的 `SlotBlocker`（unbound / source_slot_mismatch / no_client / disabled）+ `SlotClient::slot_blocker` 单一判定；notice `reason` 与 `structuredContent.slmStatusDetail{reason,nextSteps}` 同源；`toolgate list` 槽位行标原因、serve 启动按原因聚合告警；默认 / feature 双构建测试 |
| source: rerank 专属协议（ADR 0007） | 完成 | §18.1 / §3.5 / ADR 0007 | `config.rs` 的 `ModelSource::Rerank`、`capability.rs` 能力矩阵新列、`slm/rerank_protocol.rs`（原生 `/v1/rerank` 请求/响应）、client 按 source 分流的 HTTP 路径；7 个新单测；`toolgate config check --config` 实测 rerank 绑定 `native`；feature 构建真机 `call search`（25 命中）实测 `meta.rerank=applied`、`slmStatus.rerank=ready`（附件 E.11） |
| embedding 槽位（OpenAI 兼容 `/v1/embeddings`） | 完成 | §3.9 / §18.1 | `slm/embedding_protocol.rs`（请求 `{model,input:texts}`、响应按 `data[].index` 升序还原向量、5 个协议单测）+ `client.embedding` HTTP 路径；`capability.rs` 六格矩阵该格 Available；`search_path.rs` 余弦重排退路与 `tools/search.rs` 分批（≤32 条）接线；`semantic_dedup.rs` 对 decide/summarize 输入做精确行 + 余弦聚类去重（§5.1 / §5.3 / §5.5），不可用时回退原窗口；`config check` 实测 bge-m3 绑定 `native` |
| classify × systemone 原生路径（choice → 逐标签分数） | 完成 | §3.10 / §18.1 | `slm/systemone.rs` 的 `build_classify_request` / `parse_classify_output`（criteria 选项 map、probabilities → scores、none_probability ≥ 0.2 降级，5 个协议单测）；`capability.rs` 该格由 Limited 改 Available；`slm/client.rs` 按 source 分流到 `/v1/systemone`（`calibrated: true`）；`config.rs` 多模型同槽位优先绑 systemone 源（1 个新单测）；默认 / feature 双构建测试通过；d3-nano（:8082）真机端到端实测见附件 E.12；`sensitivity.rs` 用 classify 给 bash/search/task 输出打敏感度标签（与机械正则取高、机械保底，只标注不脱敏） |

## 阶段 1.5：兼容性

| 项 | 状态 | 架构依据 | 证据 |
|---|---|---|---|
| v2 首版默认 raw | 完成 | §4.2 | `compat.rs` resolve_intent 缺省 raw |
| 无握手 + 标准 initialize 握手 | 完成 | §13.2 | `SUPPORTED_VERSIONS`（2026-07-28 / 2025-11-25 / 2025-06-18）；`check_protocol_version`；测试 `legacy_handshake_sequence_reaches_tools` 覆盖 initialize → tools/list → tools/call（官方 SDK 客户端走这条路径） |
| `_meta` 解析（含 turn 载体） | 完成 | §13.2 / §13.4 | `compat.rs` parse_request_meta；`understanding.rs` TurnContext |
| 升级指南 | 完成 | §14 | `docs/upgrade-v1-to-v2.md` |

## 阶段 2：单工具接入

| 项 | 状态 | 架构依据 | 证据 |
|---|---|---|---|
| bash：decide 九问 + summarize | 完成 | §5.1 / §7.2 | `bash_path.rs`；`gate_notices` |
| bash：结构化线性 pipeline + stdout_to/stderr_to | 完成 | §5.1 | `bash.rs` `exec_segments`/`run_pipeline_blocking`（straight-line 管道、OS 管道连接、超时按组终止）；测试 `pipeline_joins_segments_by_os_pipe` / `pipeline_rejects_shell_segment` / `pipeline_empty_is_rejected` / `stdout_to_redirects_to_file`；tools/list 实测 6000 B/1500 tok 仍达标 |
| read 图片：vision 理解 + decide 七问 + need_pixels 三档 | 完成 | §5.3 / §10.1 | `read.rs` plan_image；真实 UI 截图复核通过（imageKind=ui_screenshot、hasText=true、need_pixels 概率 0.90），vision 端到端待本机视觉模型 |
| read 文本：decide 分级判定 + summarize 结构化 | 完成 | §5.3 / §6.4 | `read_path.rs`：`text_decide_questions_for(intent)`（auto 十问；summary/structured 七问，不问 best_view）+ `bounded_ruling_sample`（判定样本头 2000 + 尾 200 字符）；`merge_text_answers` 未判定 best_view 时置 None、`meta.bestView` 省略；测试 `text_decide_questions_summary_and_structured_skip_best_view`、`text_answers_merge_without_best_view_keeps_none` |
| search：语义重排接线 | 完成 | §5.2 | `search_path.rs`；query 必填主体由 pattern 承担、可选自然语言 query 决定排序：命中 > 20 且 query 非空时 rerank 优先，rerank 槽位不可用而 embedding 可用则按余弦重排；两者都不可用保持命中顺序并告知 |
| edit：四级匹配 + decide 归因 | 完成 | §5.4 | `edit_path.rs`；p ≥ 0.85 才自动写回 |
| task：decide 七问 + summarize | 完成 | §5.5 | `task_path.rs` |
| write：§5.7 改动后事实返回 | 完成 | §5.7 | `write.rs`；与 edit 共用 `unified_diff` |
| **阶段 2 整体验收** | 完成 | §14 阶段 2 | 四工具 raw / 协商双路径；read 图片真实截图复核通过；classify 已接入输出敏感度标注（机械正则保底） |

## 阶段 3：协商层与 intent

| 项 | 状态 | 架构依据 | 证据 |
|---|---|---|---|
| 六工具 intent 参数（raw / summary / structured / visual / auto） | 完成 | §4.2 | 测试 `all_specs_declare_intent_output_schema_and_three_part_description` |
| 结构化输出 schema | 完成 | §4.3 | `ToolSpec::output_schema`（notices 契约 + additionalProperties） |
| confidence / slmStatus / intent 进 meta | 完成 | §7.3 | `tools/mod.rs` `negotiation_meta` + `inject_negotiation_meta` |
| tools/list 固定开销 ≤1.5K token | 完成 | §4.7 / §6.4 | 六工具 description+inputSchema+outputSchema 合计实测中文 5999 B≈1.50K、英文 5724 B≈1.43K（4 B/token，2026-11-14 release）；原 2.0K 起只压描述与 schema 文本，字段结构不变 |

## 阶段 4：优化

| 项 | 状态 | 架构依据 | 证据 |
|---|---|---|---|
| 进程组管理（超时整组终止） | 完成 | §14 阶段 4 | `kill_process_group`；bash/task 均接 |
| L0 预热与 L1 预判 | 完成 | §22 | read 文本预热（`WarmText`）；edit 复用预热内容；read 图片单次读取（判定/组装/压缩共用字节与尺寸）；预热缓存有总容量（64MB）与条目数（32）上限 |
| 质量阶梯标定 | 完成 | §10.2b | `examples/quality_calibration.rs` |
| decide 批处理（方案 B） | 不实现（前提不成立） | §13.4 | 见「当前待办」 |
| 理解链并发调度调优 | 完成 | §4.8 | read 图片 `Both` 并发（`tokio::join!`）；`understanding.rs` 的分层调度仍用于校验与确定性 |
| 有损 WebP | 未做（条件触发） | §10.2b | 见「当前待办」 |

## 阶段 5（可选）：本地 agent 消费形态验证

| 项 | 状态 | 架构依据 | 证据 |
|---|---|---|---|
| 四项行为校验（raw 不调模型 / 错误 code+nextSteps / 结构化字段稳定 / 告知两通道） | 完成 | §14 阶段 5、§21.7 | `crates/toolgate-core/tests/agent_consumption.rs`（4 个用例）；期间修掉 bash 截断告知只进 content 的缺陷 |

## §17.2 安全底线（v2 必须补）

| 项 | 状态 | 架构依据 | 证据 |
|---|---|---|---|
| 路径根校验（`paths.allowed_roots`） | 完成 | §17.2 | `ToolContext::resolve_file`；`ISOLATION_REFUSED`；config `paths` 段（绝对路径校验）；CLI 注入为追加 |
| env 白名单（bash / task） | 完成 | §17.2 | `apply_env_allowlist`（11 项固定名单，`env_clear` 后注入，PWD = 子进程 cwd） |
| search 遍历上限（32 层 / 20 万文件 / 10 秒） | 完成 | §17.2 | `WalkLimits`；超时返回 `TIMEOUT`；深度/文件数截断在 content 与 meta 告知 |
| uid/gid 上报 | 完成 | §17.2 | MCP `initialize` → `serverInfo.identity`；`toolgate status`（文本 + `--json`） |

## §13.1 剩余 L0 差距

| 项 | 状态 | 架构依据 | 证据 |
|---|---|---|---|
| edit 模式 C（扁平 patch） | 完成 | §5.4 | `parse_patch` / `apply_patch`（hunk 头定位 + 上下文逐行比对 + 冲突报期望与实际内容）；三模式互斥；写前 (mtime,size,sha256) 复核；20 个测试 |
| read 单行超长落盘 | 完成 | §9.1 | `truncate_line_head` + read 输出定形分支（整行落盘 + pointer）；4 个测试 |

## 已知边界与遗留（不阻塞当前阶段，改动前先读）

| 项 | 现状 | 处理建议 |
|---|---|---|
| 单行长度恰好等于 `limits.max_bytes` | `truncate_head` 因 `len+1 > max_bytes` 返回空内容，续读提示是原地 `offset=offset` | 属既有边界，不在「单行超长」定义内；要收就单独一条（判据改 `>=` 并让整行恰好放得下走普通路径） |
| 多文件 patch | 不支持：第二个 `---` 文件头会被当成 hunk 体行，报「行数与内容不符」 | 单文件场景够用；要支持需扩展 `parse_patch` 并按文件分组应用 |
| patch 冲突的判定层归类 | 走机械归因 `FailureKind::PatchConflict`，不进判定五类 | §5.4 明确说 patch 冲突不是独立第六类；要改需同步 `EDIT_QUESTIONS` 与文档 |
| `edit` 的 schema `required` | 从 `["path"]` 改为 `[]`：edits/anchor 缺 path 由运行时 `bad_argument` 拦截 | 客户端若拿 `required` 做前置校验，需知道 patch 模式可从 `---/+++` 推断路径 |
| `\ No newline at end of file` | 只做忽略：末尾换行按原文件保持 | 需要时再扩展 |

## 验证基线

| 命令 | 结果（2026-11-15 复测） |
|---|---|
| `cargo test --workspace` | 599 passed（默认构建；含 isolation 段接线（isolation.* 解析 + govern_with_policy 硬顶/none 拒绝/intent 收紧）、本机槽位接线、长文输入去重、状态真实性、列表 query 重排三态、R10 小列表跳过/大列表仍排、R11 classify 体量护栏、R12 分段 rerank/先去重再采样/密度选行/字节 clamp、R12 收尾（32KiB 常量合并 + clamp Some(0)/chunked maxTotalMs 边界单测）、配置段接线（limits/spill/snapshot/logging 解析与淘汰/GC/日志级别单测）、阶段3 脱敏执行链、阶段4 paths.expose_absolute 路径呈现、内容规则五种 kind 与 fallback_chain 降级执行等新单测） |
| `cargo test --workspace --features external-protocols` | 592 passed（feature 构建） |
| `cargo clippy --workspace --all-targets` | No issues |
| `cargo clippy --workspace --all-targets --features external-protocols` | No issues |
| `cargo build --release` | 零警告 |
| `./scripts/check-doc.sh` | 通过（7111 行） |

> 基线数字是快照，不是承诺：新增行为必须同步加测试，交付时重跑并把新数字写回本表。