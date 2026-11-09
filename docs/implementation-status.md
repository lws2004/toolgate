# toolgate 实现进度表

> **本文件是「实现状态」的唯一事实源**：哪项做了、证据在哪、怎么验证。
> 架构决策仍以 [architecture.md](architecture.md) 与分册为准；本文件只回答「做到哪了」，
> 不复制架构正文，只给指针与证据。
>
> **更新纪律**：每次改动落地后更新对应行（状态 + 证据）。判断下一步只读本文件，
> 不要重新扫描代码与架构正文——那既慢又容易与事实源漂移。
>
> 最后更新：2026-11-09（安全底线四项 + 配置层 paths 段落地后）

## 怎么用

1. 读「当前待办」一节的表，判断下一步做什么；
2. 动手前按「架构依据」列读对应章节（不读整本）；
3. 完成后更新该行状态与证据（提交号 / 测试名 / 验证命令）；
4. 状态只有三种：`完成`（有测试或实测证据）/ `部分`（已接入但验收未过，证据列写缺口）/ `未做`。

## 当前待办（按优先级）

| 项 | 状态 | 架构依据 | 缺口 |
|---|---|---|---|
| 阶段 2 整体验收 | 部分 | §14 阶段 2 | read 图片 auto 判定待真实截图样本复核；其余已接入 |
| decide 批处理（方案 B，turn 内聚合） | 未做 | §13.4 | `_meta.toolgate/turn` 载体已解析（compat.rs / understanding.rs），跨请求聚合与丢弃未实现 |
| 理解链并发调度调优 | 部分 | §4.8 | 有 MAX_PARALLEL 上限与确定性调度；链级并发调优未做 |
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

## 阶段 1.5：兼容性

| 项 | 状态 | 架构依据 | 证据 |
|---|---|---|---|
| v2 首版默认 raw | 完成 | §4.2 | `compat.rs` resolve_intent 缺省 raw |
| 无握手 + 旧版 initialize 兼容 | 完成 | §13.2 | `toolgate-mcp/src/lib.rs` SUPPORTED_VERSIONS；`check_protocol_version` |
| `_meta` 解析（含 turn 载体） | 完成 | §13.2 / §13.4 | `compat.rs` parse_request_meta；`understanding.rs` TurnContext |
| 升级指南 | 完成 | §14 | `docs/upgrade-v1-to-v2.md` |

## 阶段 2：单工具接入

| 项 | 状态 | 架构依据 | 证据 |
|---|---|---|---|
| bash：decide 九问 + summarize | 完成 | §5.1 / §7.2 | `bash_path.rs`；`gate_notices` |
| read 图片：vision 理解 + decide 七问 + need_pixels 三档 | 部分 | §5.3 / §10.1 | `read.rs` plan_image；真实截图实测已记 §14，多图复核待补 |
| read 文本：decide 十问 + summarize 结构化 | 完成 | §5.3 | `read_path.rs`（file_kind / sensitivity / best_view） |
| search：rerank 接线 | 完成 | §5.2 | `search_path.rs`；命中 > 20 且槽位可用才调用 |
| edit：四级匹配 + decide 归因 | 完成 | §5.4 | `edit_path.rs`；p ≥ 0.85 才自动写回 |
| task：decide 七问 + summarize | 完成 | §5.5 | `task_path.rs` |
| write：§5.7 改动后事实返回 | 完成 | §5.7 | `write.rs`；与 edit 共用 `unified_diff` |

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
| L0 预热与 L1 预判 | 部分 | §22 | `warm.rs` + read 路径接入；其余工具未接 |
| 质量阶梯标定 | 完成 | §10.2b | `examples/quality_calibration.rs` |
| decide 批处理（方案 B） | 未做 | §13.4 | 见「当前待办」 |
| 理解链并发调度调优 | 部分 | §4.8 | `understanding.rs` MAX_PARALLEL / MAX_STEPS |
| 有损 WebP | 未做（条件触发） | §10.2b | 见「当前待办」 |

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

| 命令 | 结果（2026-11-09） |
|---|---|
| `cargo test --workspace` | 429 passed（安全四项 + 配置层 + edit 模式 C + read 单行落盘后） |
| `cargo clippy --workspace --all-targets` | No issues |
| `cargo build --release` | 零警告 |
| `./scripts/check-doc.sh` | 通过 |

> 基线数字是快照，不是承诺：新增行为必须同步加测试，交付时重跑并把新数字写回本表。
