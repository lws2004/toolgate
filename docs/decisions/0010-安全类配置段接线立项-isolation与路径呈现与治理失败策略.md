# ADR 0010：安全类配置段的接线立项（isolation / paths.expose_absolute / govern.fail_open）

- 状态：立项，未实现（2026-11-15）。本轮只接线 limits / spill / snapshot / logging 四个「配置解析 + 消费点注入」可安全直落的段；本 ADR 覆盖的三个段三触安全边界，需先评审、后实现。
- 影响面：未来改 `crates/toolgate-core/src/decision.rs` 的隔离决策输入、`crates/toolgate-core/src/config.rs` 的段解析、`crates/toolgate-cli/src/main.rs` 的注入、daemon `crates/toolgate-daemon/src/lib.rs` 的治理失败路径；**当前不改**任何对外契约、默认行为与 §2 红线。
- 相关红线：§2 第 6 条「隔离强度由外部配置决定，模型不能通过 intent 绕过」（§8.3、§8.4）。
- 触发：配置段接线推进时，四段（limits/spill/snapshot/logging）已可直接接；剩下三段不是同一类问题，逐段核查后单独立项。

## 背景：三段现状不同，不能像四段那样「一接了之」

配置段接线分两类：**配置解析 + 注入已有消费点**（limits/spill/snapshot/logging 属此类，本轮已接）与**依赖尚未落地的执行引擎**（本 ADR 三段属此类）。逐段核查（以 `crates` 检索与 `config.rs` 注释为据）：

### isolation（附录 D 默认：default=summary / max_exposure=raw / rules / content_rules / fallback_chain / redact_min_retention=0.5）

- **已有**：`crates/toolgate-core/src/decision.rs` 的 `DecisionInput` 已含 `isolation_default` / `max_exposure` / `path_rule` / `content_rules` / `fallback_chain` / `redact_min_retention` 字段（decision.rs:410-415）；`decide_output`（decision.rs:495）已实现「有效上限 = min(max_exposure, path_rule.level)」（decision.rs:507-513）、`isolation_refused = cap==None`（decision.rs:529）、回退链取首步（decision.rs:535）；`crates/toolgate-core/src/sensitivity.rs` 已有 `secret_patterns()` 脱敏正则（sensitivity.rs:147/177）。
- **缺**：① 从 `config.yaml` 解析 `isolation.*` 并注入决策输入（现默认值硬编码在 `crates/toolgate-core/src/compat.rs:189/197`）；② §8.5 的 `rules` **glob 路径规则**解析与匹配（crates 内无 glob 依赖、无匹配器）；③ §8.11 的 `path_username` / `host_metadata` **用户名与主机名替换**（crates 内无 `[USER]`/`[HOST]` 处理）；④ `content_rules` 的**执行链**（decision 只消费结构，实际脱敏/裁决未串起）；⑤ `fallback_chain` 的**实际执行**（decision.rs:8 注释已注明「需先与架构对齐后实现」）。

### paths.expose_absolute（附录 D 默认：false）

- **现状**：`config.rs:289` 注释明确「只接线 allowed_roots；expose_absolute 等字段尚未接线」，且 `config.rs:1200` 对写入该字段报错、`config.rs:2487-2489` 有拒绝测试。crates 内无任何绝对路径呈现逻辑。
- **依赖**：语义是「仅 raw 档且显式 true 才给绝对路径，且仍过 path_username 规则」（§8.11、09-output §163）。它必须建立在 isolation 引擎「档位」成立之后，无法单独接线。

### govern.fail_open（附录 D 默认：false）

- **现状**：crates 内 `fail_open`/`failOpen` 零命中——daemon 治理失败路径当前未区分 fail-open / fail-closed。
- **语义**：默认 `false`＝治理不可用时**默认拒绝、不静默放行敏感内容**（§18.7）。这是安全默认，接线必须保证默认路径不改变「拒绝」行为。

## 决策

1. **isolation 段**：立项为独立实现阶段，分两步——(a) 补 `config` 解析 + 注入 `DecisionInput`（默认值取附录 D，`isolation.default=summary` 等），(b) 补缺失执行部件（glob 路径规则、path_username/host_metadata 替换、content_rules 执行链、fallback_chain 执行）。第 (b) 步触及 §8 安全边界，**必须先改 `docs/architecture/08-isolation.md` 正文并评审**（§3.4 架构变更）。
2. **paths.expose_absolute**：随 isolation 的路径呈现一起接（同一实现单元），不入独立 ADR。
3. **govern.fail_open**：作为 daemon 失败路径的独立小项，接线时**默认值必须保持 `false` 且 fail-closed 行为不变**，仅当显式配 `true` 才放行并记结构化告警。
4. 三段接线前均先补 `docs/architecture` 对应正文（§8 / §18.7）并跑 `check-doc.sh`，再实现、再补回归测试。

## 约束与验收

- **红线**：任何实现不得让模型经 `intent` 突破 `max_exposure`（§8.3/§8.4、§2-6）；`govern.fail_open` 默认 `false` 时不得静默放行敏感内容。
- **isolation 验收**：`isolation.*` 配置解析（未知字段拒绝、非法档位拒绝）；默认档 `summary`、`max_exposure=raw` 与现有 decision 默认一致；`.env*`/`secrets/**`→`none` 路径规则命中；`path_username` 把 `/Users/<name>` 替换为 `[USER]`；`cargo test --workspace` 全绿且属性测试「任一 intensity ≤ max_exposure」仍成立。
- **fail_open 验收**：治理不可用时默认返回结构化错误（不返回原文）；显式 `true` 时原样放行并记 `DEGRADED` 告警；测试覆盖两条路径。
- **为何现在不做**：三段都改安全默认或安全边界语义；isolate 还缺多个执行部件。强行只接配置会造成「配了不生效」的配置幻觉（§18.9），或半套隔离给出错误安全感。按 §3.4 属架构变更，须先文档 + 评审，非「实现细节」可直接落。

## 与已接线四段的边界

limits / spill / snapshot / logging 是「解析 + 注入已有消费点」，不改安全边界语义，属实现细节（§3.4），本轮已接并验证（`cargo test --workspace` 575 passed / feature 568 / clippy 0 / build 0 警告 / check-doc 7094 行）。本 ADR 与它们无共享改动；三段实现时不回改四段。
