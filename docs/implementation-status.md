# toolgate 实现进度表

> **本文件是「实现状态」的唯一事实源**：哪项做了、证据在哪、怎么验证。
> 架构决策仍以 [architecture.md](architecture.md) 与分册为准；本文件只回答「做到哪了」，
> 不复制架构正文，只给指针与证据。
>
> **更新纪律**：每次改动落地后更新对应行（状态 + 证据）。判断下一步只读本文件，
> 不要重新扫描代码与架构正文——那既慢又容易与事实源漂移。
>
> 最后更新：2026-11-14（daemon + govern/pre 新形态；dsh 薄适配器独立仓并已挂载 web profile；附件引用治理与反向委派落地；默认 cargo test 514 passed、适配器 node --test 30 passed）

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

## 阶段 R：daemon + govern/pre 接缝（2026-11-14 新形态）

| 项 | 状态 | 架构依据 | 证据 |
|---|---|---|---|
| R1 daemon 常驻 + Unix socket 手写 HTTP | 完成 | §18.2 / §18.7 / §1.5 | 新 crate `crates/toolgate-daemon`（`serve()` 监听 Unix socket，手写 HTTP/1.1，不引入 axum/hyper）；路由 POST /govern、POST /pre、GET /health；集成测试 `crates/toolgate-daemon/tests/seams.rs`（真实 socket 往返 health/govern/pre）；CLI 新增 `toolgate daemon [--socket <path>]` |
| R2 govern 六类处理器（按形状路由、免白名单） | 部分 | §4.0a / §4.1 / §5.3 | `crates/toolgate-core/src/govern.rs`：`sniff_shape` 三级信号前两级（确定性命中：image block / data URI / 魔数；结构自证：JSON；错误标记 / 列表 / 长文 / 元信息）+ `govern_with()` 分派六个 processor；12 个单测覆盖视觉/透传/结构化/错误/长文/空输入及委派成功/失败，并含「文档正文出现 error: 字样不被误判为错误」回归。视觉/长文/列表/错误四类在 `policy.delegate` 存在时反向委派给宿主模型（见 R5），无回调时降级 L0 并告知。缺口：第三级 classify（纯语义判形状）未接，形状仍靠前两级信号 |
| R3 pre 写前复核 + 写前快照 | 完成 | §18.6 / §18.7 / §20 | `crates/toolgate-core/src/precheck.rs`：`pre()` 逐条按 (mtime,size,sha256) 复核，不一致返回 PRE_STALE 并 block；一致则 `spill.snapshot` 写前快照；4 个单测（匹配放行+快照 / sha 变化阻断 / 新文件无指纹放行 / 期望存在但文件消失阻断） |
| R4 薄适配器独立仓（pi / dsh） | 完成（dsh；pi 暂不做） | §18.2 / §18.6 | 独立仓 `/Users/lanws/workspace/toolgate-dsh-adapter`：Cordis 插件 `@toolgate/dsh-adapter`，零执行逻辑零工具声明，只挂 `tools/post-execute`（→ daemon `POST /govern`）与 `tools/pre-execute`（`write`/`edit` → `POST /pre`）；图片只传引用+元信息（`attachmentId/mimeType/bytes/width/height/sha256`），不传 base64 字节；写侧期望指纹取「该 agent 上次 read 到的真值」，缓存按 `exec.agent` WeakMap 隔离；`node --test test/*.test.mjs` = 30 passed（含 11 委派协议单测 + 多 agent 缓存隔离）；`check_plugin_spec.py` 0 E / 0 W；真实 DSH headless 实测：读 3000 行文件触发长文治理并成功反向委派 `summarize`（模型 `auto/deepseek-v4.1-flash` 返回摘要）、`edit` 生成写前快照；宿主 web profile `/Users/lanws/.dsh/profiles/web/cordis.patch.yml` 已挂载 `toolgate-adapter`（`socketPath` 指向 `toolgate/.toolgate/daemon.sock`），旧 `mcp:toolgate` 条目已从 `capability-catalog.mcp` 下线；重启后 web PID 84297、`delegate-84297.sock` 活性，长文治理实测返回摘要 + `[TRUNCATED]` 落盘指针 |
| R5 反向委派（无本地 SLM 回投宿主会话模型） | 完成（dsh；pi 暂不做） | §18.7 三约束 | core `govern_with(req,spill,Some(&dyn Delegate))` 在 vision/summarize/rerank/classify 四处调用委派，失败/无回调降级 L0；daemon `delegate.rs` 实现 `HttpDelegate`（`unix:` / `http://127.0.0.1`，超时 8s、输出预算 512 token）；适配器 `createDelegateServer` 起同目录 Unix socket，`handleDelegatePayload` 把单条内容交 `ctx.llm.stream`（`ctx.get('llm')` + `agentDefaultModel.currentSelection()`，不声明硬依赖），token 单次登记不落盘；`crates/toolgate-daemon/tests/seams.rs` 真实 socket 委派往返测试通过；真实 DSH headless 长文委派实测成功 |

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

| 命令 | 结果（2026-11-13 复测） |
|---|---|
| `cargo test --workspace` | 489 passed（默认构建；含 4 个阶段 5 集成用例、1 个 MCP 握手序列用例、3 个判定日志单测、7 个 rerank 协议单测、6 个 classify × systemone 单测、embedding 协议 7、search 语义重排与 files 门 7、自动绑定 1，以及敏感度 7 / 语义去重 3 个新增单测） |
| `cargo test --workspace --features external-protocols` | 482 passed（feature 构建；6 个默认构建专属的降级测试被 cfg 隔离，另含 3 个判定日志单测、7 个 rerank 协议单测、6 个 classify × systemone 单测与敏感度 / 语义去重新增单测） |
| `cargo clippy --workspace --all-targets` | No issues |
| `cargo clippy --workspace --all-targets --features external-protocols` | No issues |
| `cargo build --release` | 零警告 |
| `./scripts/check-doc.sh` | 通过 |

> 基线数字是快照，不是承诺：新增行为必须同步加测试，交付时重跑并把新数字写回本表。
