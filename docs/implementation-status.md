# toolgate 实现进度表

> **本文件是「实现状态」的唯一事实源**：哪项做了、证据在哪、怎么验证。
> 架构决策仍以 [architecture.md](architecture.md) 与分册为准；本文件只回答「做到哪了」，
> 不复制架构正文，只给指针与证据。
>
> **更新纪律**：每次改动落地后更新对应行（状态 + 证据）。判断下一步只读本文件，
> 不要重新扫描代码与架构正文——那既慢又容易与事实源漂移。
>
> 最后更新：2026-11-10（三原语架构决策补齐：门控第二维 / score 归属 / 幂等键口径；验证基线 454/448）

## 怎么用

1. 读「当前待办」一节的表，判断下一步做什么；
2. 动手前按「架构依据」列读对应章节（不读整本）；
3. 完成后更新该行状态与证据（提交号 / 测试名 / 验证命令）；
4. 状态只有三种：`完成`（有测试或实测证据）/ `部分`（已接入但验收未过，证据列写缺口）/ `未做`。

## 当前待办（按优先级）

| 项 | 状态 | 架构依据 | 缺口 |
|---|---|---|---|
| §15.2 判定日志（decisions.jsonl） | 完成 | §15.2 / §15.6 / §3.11 | `decision_log.rs`（sha256 输入哈希 / RFC3339 时间戳 / JSONL 追加写）+ 五个判定点接线；3 个单测覆盖字段与脱敏、追加写与 null、时间戳基准。bash/edit/task 的归并函数未暴露 §7.3 数值，这三条路径 `confidence` 记 null（不编造） |
| decide 批处理（方案 B，turn 内聚合） | 不实现（前提不成立） | §13.4 | 一次 decide 调用只读一个 state，不同工具的输出无法合并；可合并的「同一 state 多问题」在单次调用内已问完。论证与 turn 载体现状见 §13.4「实现状态」 |
| 有损 WebP | 未做（条件触发） | §10.2b | 三条触发判据未出现，image_pipeline 只检测格式 |
| 流式输出 | 不做 | §14 阶段 4 | MCP progress notification 不改变返回形态，已定不做 |

## 阶段 1：SLM 接入层

| 项 | 状态 | 架构依据 | 证据 |
|---|---|---|---|
| 五槽位客户端适配 | 完成 | §3.2 / §3.10 / §18.1 | `crates/toolgate-core/src/slm/`（systemone / text / ordered / vision / json）；`toolgate decide` 端到端实测见 §14 |
| 判定封装（命名问题 → 带概率答案） | 完成 | §3.2 / §7.2 | `decision.rs`、`slm/systemone.rs` |
| 置信度门控 | 完成 | §7.2 | `slots.rs` ConfidenceGate（Adopt / AdoptWithLowConfidence / BothViews） |
| 降级路径（槽位不可用回退 L0） | 完成 | §3.8 | 各工具 `unavailable` 分支 + `SLOT_UNAVAILABLE` 告知 |
| 错误自检三层 + ErrorCode 12 值域 | 完成 | §1.4.5 | `error.rs`；测试 `every_error_carries_next_steps` |
| slmStatus 状态真实性（构建可用性） | 完成 | §7.3 / §19.6 | `capability::client_available` / `source_slot_usable`：默认构建下外部协议来源标 `unavailable`，不再谎报 `ready`；测试按 feature 分支 |
| source: rerank 专属协议（ADR 0007） | 完成 | §18.1 / §3.5 / ADR 0007 | `config.rs` 的 `ModelSource::Rerank`、`capability.rs` 能力矩阵新列、`slm/rerank_protocol.rs`（原生 `/v1/rerank` 请求/响应）、client 按 source 分流的 HTTP 路径；7 个新单测；`toolgate config check --config` 实测 rerank 绑定 `native`；feature 构建真机 `call search`（25 命中）实测 `meta.rerank=applied`、`slmStatus.rerank=ready`（附件 E.11） |
| classify × systemone 原生路径（choice → 逐标签分数） | 完成 | §3.10 / §18.1 | `slm/systemone.rs` 的 `build_classify_request` / `parse_classify_output`（criteria 选项 map、probabilities → scores、none_probability ≥ 0.2 降级，5 个协议单测）；`capability.rs` 该格由 Limited 改 Available；`slm/client.rs` 按 source 分流到 `/v1/systemone`（`calibrated: true`）；`config.rs` 多模型同槽位优先绑 systemone 源（1 个新单测）；默认 / feature 双构建测试通过；d3-nano（:8082）真机端到端实测见附件 E.12 |

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
| read 图片：vision 理解 + decide 七问 + need_pixels 三档 | 完成 | §5.3 / §10.1 | `read.rs` plan_image；真实 UI 截图复核通过（imageKind=ui_screenshot、hasText=true、need_pixels 概率 0.90），vision 端到端待本机视觉模型 |
| read 文本：decide 十问 + summarize 结构化 | 完成 | §5.3 | `read_path.rs`（file_kind / sensitivity / best_view） |
| search：rerank 接线 | 完成 | §5.2 | `search_path.rs`；命中 > 20 且槽位可用才调用 |
| edit：四级匹配 + decide 归因 | 完成 | §5.4 | `edit_path.rs`；p ≥ 0.85 才自动写回 |
| task：decide 七问 + summarize | 完成 | §5.5 | `task_path.rs` |
| write：§5.7 改动后事实返回 | 完成 | §5.7 | `write.rs`；与 edit 共用 `unified_diff` |
| **阶段 2 整体验收** | 完成 | §14 阶段 2 | 四工具 raw / 协商双路径；read 图片真实截图复核通过；classify 无真实分类模型不影响本阶段 |

## 阶段 3：协商层与 intent

| 项 | 状态 | 架构依据 | 证据 |
|---|---|---|---|
| 六工具 intent 参数（raw / summary / structured / visual / auto） | 完成 | §4.2 | 测试 `all_specs_declare_intent_output_schema_and_three_part_description` |
| 结构化输出 schema | 完成 | §4.3 | `ToolSpec::output_schema`（notices 契约 + additionalProperties） |
| confidence / slmStatus / intent 进 meta | 完成 | §7.3 | `tools/mod.rs` `negotiation_meta` + `inject_negotiation_meta` |

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

| 命令 | 结果（2026-11-10 复测） |
|---|---|
| `cargo test --workspace` | 454 passed（默认构建；含 4 个阶段 5 集成用例、1 个 MCP 握手序列用例、3 个判定日志单测、7 个 rerank 协议单测与 6 个 classify × systemone 新增单测） |
| `cargo test --workspace --features external-protocols` | 448 passed（feature 构建；6 个默认构建专属的降级测试被 cfg 隔离，另含 3 个判定日志单测、7 个 rerank 协议单测与 6 个 classify × systemone 新增单测） |
| `cargo clippy --workspace --all-targets` | No issues |
| `cargo clippy --workspace --all-targets --features external-protocols` | No issues |
| `cargo build --release` | 零警告 |
| `./scripts/check-doc.sh` | 通过 |

> 基线数字是快照，不是承诺：新增行为必须同步加测试，交付时重跑并把新数字写回本表。
