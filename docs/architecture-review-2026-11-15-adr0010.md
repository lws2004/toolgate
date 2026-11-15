# 架构评审：ADR 0010 安全类配置段接线（isolation / paths.expose_absolute / govern.fail_open）

- 日期：2026-11-15
- 评审对象：[ADR 0010](decisions/0010-安全类配置段接线立项-isolation与路径呈现与治理失败策略.md)
- 范围：把 ADR 0010 立项的三段落到实处前的可行性评审（是否可直接实现、落点在哪、风险与分期）
- 结论：**方向正确，但「配置接线」的定位低估了实现尺度。** 核查发现治理主路径完全没有隔离引擎（P0），三段不能按「解析 + 注入」实现，须在 govern 主流程织入 §8 能力并明确 fail_open 层位。按下列分期实现，先做阶段 0/1（文档前置）与阶段 2（隔离决策织入，默认值向后兼容）。

## 一、核查方法与证据

以 `crates` 检索为准（证据等级：源码实测），逐段核对 ADR 0010 的「已有 / 缺」判断：

- `rg -n 'ExposureLevel|isolation|decide_output|DecisionInput'` 全仓；
- 逐文件读 `crates/toolgate-core/src/decision.rs`（732 行）、`crates/toolgate-core/src/govern.rs`（2561 行）、`crates/toolgate-core/src/compat.rs`、`crates/toolgate-mcp/src/lib.rs`、`crates/toolgate-daemon/src/lib.rs`。

## 二、关键发现

### P0-1 治理主路径（govern.rs）没有任何隔离引擎

`crates/toolgate-core/src/govern.rs` 是 daemon `/govern` 的实际治理入口（`govern_with_slots`，govern.rs:323）。它按 `req.intent`（`Option<String>`，默认 `"raw"`，govern.rs:329）+ `sniff_shape`（govern.rs:258）分类后分派到六个 process_* 处理器，**全程不做 ExposureLevel 档位计算、不做脱敏、不做路径相对化、不执行 content_rules、不执行 fallback_chain**。隔离四档（§8.1）在治理主流程里根本不存在。

### P0-2 `decision.rs` 的隔离判定只服务旧 MCP 路径、且只产出告知

`crates/toolgate-core/src/decision.rs` 的 `decide_output`（decision.rs:495）确实实现了「有效上限 = min(max_exposure, path_rule.level)」（507-513）、`isolation_refused`（529）、回退链取首步（535）。但它的唯一生产调用点是 `crates/toolgate-mcp/src/lib.rs:191`，且仅在 `intent==auto` 时用来生成 notices（`toolgate-mcp/src/lib.rs:174-205`），**不改动返回内容**。daemon / govern 路径不复用它。

→ 合起来：ADR 0010 说「isolation 决策层已有、缺注入」不够准确——**决策结构有，但不在治理主路径上；把它接到 govern 主流程，等价于在 2561 行的核心里新织入 §8 的档位/回退/脱敏能力**，属重大架构实现，不是配置接线。

### P0-3 `govern.fail_open` 的落点层级未定义且与现状语义有歧义

- daemon `/govern`（`crates/toolgate-daemon/src/lib.rs:271-286`）：解析 `GovernRequest`（解析失败→400）→ 调 `govern_with_slots` → 直接返回 `GovernResponse`。`govern_with_slots` 是纯函数、永远返回响应（槽位不可用走 L0 降级），**没有「治理失败」的 Err 分支**。
- 因此 `govern.fail_open` 在 daemon 内部几乎没有触发点；真正的「治理不可用」表现为**宿主适配器调用 daemon 超时/连接失败**。R7 记录适配器已实现「超时 failOpen 原样返回 + 文案」（[implementation-status.md](implementation-status.md) R7 行），这与 §18.7「`govern.fail_open` 默认 `false`，不静默放行敏感内容」存在层位不一致。

→ 实现前必须先定义：fail_open 属于 daemon 内部错误、还是宿主适配器的降级策略；默认 `false` 在两层分别意味着什么。属 §3.4 的架构变更，须先改 §18.7 正文。

### P1-1 paths.expose_absolute 依赖档位且路径呈现未实现

`crates` 内无任何绝对/相对路径呈现逻辑；`crates/toolgate-core/src/config.rs:289` 注释「只接线 allowed_roots；expose_absolute 等字段尚未接线」，`config.rs:1200` 对其报错、`config.rs:2487-2489` 有拒绝测试。语义是「仅 raw 档且显式 true 才绝对，且过 path_username 规则」（§8.11、09-output §163）——必须等 P0-1 的档位成立后才可接，不能单独落。

### P2-1 双轨判定（信息项）

`decision.rs`（§4.5 第一段纯函数，MCP 旧路径）与 `govern.rs`（§18.7 daemon 路径）对 intent/shape 的判定不共享。后续维护需知晓：只接 decision 路径不会影响 govern 的实际治理效果。

## 三、实现设计与分期

| 阶段 | 内容 | 风险 | 验收 |
|---|---|---|---|
| 0（文档前置） | §8 增「工具层实现状态」小节；§18.7 定义 fail_open 层位与默认；附录 D 状态标注 | 无 | check-doc 通过 |
| 1（配置解析） | `config.rs` 解析 `isolation.*` / `paths.expose_absolute` / `govern.fail_open`（默认值取附录 D，未知字段/非法值拒绝），先不接执行 | 低 | 单测：默认值、覆盖、拒非法 |
| 2（隔离决策织入 govern） | 由配置构造 `IsolationPolicy`；govern 入口算 `effective=min(intent, max_exposure, path_rule)`；`none` 档返回 `IsolationRefused`、降档作用于 shape。**默认 `max_exposure=raw` 时行为不变（向后兼容）** | 中（触核心） | 回归全绿 + none/降档用例 |
| 3（脱敏与内容规则） | 复用 `sensitivity.rs` 的 `secret_pattern`；补 `path_username`/`host_metadata` 替换、`content_rules`、`fallback_chain` 执行 | 高 | §8.11 行为用例 |
| 4（路径呈现 + fail_open） | `paths.expose_absolute` 呈现；按阶段 0 定义落 fail_open | 中 | 绝对/相对路径用例、fail-open/closed 两路 |

**阶段 1 与阶段 2 必须同一交付**：只解析不织入会造成「配了不生效」的配置幻觉（§18.9），比继续拒绝更糟——二者合并为一次实现与验证。

## 四、结论与准入

- ADR 0010 方向成立；**P0-1/P0-2/P0-3 必须先由 §8/§18.7 正文澄清，再进入阶段 1+2 实现**（§3.4 架构变更：先文档→评审→实现）。
- 建议实现从「阶段 0 文档 + 阶段 1+2（隔离决策织入 govern，默认向后兼容）」开始；阶段 3/4 拆成后续可评审步骤，不一次性重写多个安全层。
- 分期与本文档作为 ADR 0010 的实现依据；实现后回归 `cargo test --workspace` 全绿、`clippy` 0、`build` 0 警告、`check-doc` 通过。
