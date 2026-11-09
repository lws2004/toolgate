# toolgate 架构评审：实现阶段契约空白补齐（2026-11-09）

> 评审对象：实现阶段（§14 阶段 1）连续五轮暴露的 4 处契约空白。
> 核对方式：逐处比对正文、§2 红线、§15.7 测试口径；./scripts/check-doc.sh 实跑。
> 确认：用户 2026-11-09 明确授权「过程发现架构契约缺失则补后执行」，视为 §3.4 评审确认已满足。
> 结论：4 项均为 P1，已全部补齐，实现阶段准入通过。

## 0. 结论摘要

| 项 | 分级 | 状态 | 落点 |
|---|---|---|---|
| C-1 ErrorCode 值域不足、UnknownTool 缺 code | P1 | 已补齐 | architecture.md §1.4.5 |
| C-2 decide_output 签名漏列 fallback_chain、输出类型缺失 | P1 | 已补齐 | 04-tools.md §4.5 |
| C-3 structuredContent 脱敏张力 | P1 | 已补齐 | 04-tools.md §4.4b |
| C-4 §13.4 d1 调用形态仍写「需要实测选定」 | P1 | 已补齐 | 11-runtime.md §13.4 |

## 1. C-1：错误契约

**现象**：§1.4.5 草案的 ErrorCode 只有 8 个特定场景码，v1 骨架 40 处错误构造点大多落不进去；
UnknownTool 变体不带 code 字段，与 §2 红线 7「每个错误必须带 errorCode + 可执行下一步」冲突。

**补齐**：ErrorCode 扩为封闭 12 个（8 特定 + 4 通用兜底 UNKNOWN_TOOL / BAD_ARGUMENT /
IO_ERROR / FAILED）；UnknownTool 补 code 与 next_steps；写明序列化为 SCREAMING_SNAKE_CASE、
与 notice code 不互相映射。

**依据**：§2 红线 7、§4.6「错误归因准确」、§15.7 元测试穷举要求。

## 2. C-2：决策表

**现象**：§4.5 的文字签名只有 7 个参数，没有 fallback_chain / redact_min_retention，
但 §4.5:414 的决策规则、§4.5:420 的属性测试、§8.5/§8.6 的四级回退都依赖它们；
输出 (output_shape, fallback, decision_notices, meta) 无 Rust 类型草案。

**补齐**：写全 DecisionInput / DecisionOutput / decide_output 的 Rust 契约，
显式纳入 fallback_chain 与 redact_min_retention；把三段决策判据与 intent→档位映射写成可执行规则；
明确 fallback_steps 是配置链的保序子序列；属性测试口径落到 §15.7。

**依据**：§4.2、§8.1/§8.4/§8.5/§8.6/§8.11、§18.1b、§3.8、§16.3。

## 3. C-3：两个通道的脱敏口径

**现象**：§4.4b:134 要求 structuredContent「原样携带」notices，§4.4b:233 又要求
notice 与 content、structuredContent 一样必须过 §8.11；绝对路径 rule 是否也脱敏存在张力。

**补齐**：明确「原样」指不另写文案，不是绕过脱敏；rule 脱敏发生在 notice 构造时，
两通道拿到同一份已脱敏 notices；测试要求改为 content 与 structuredContent 同时断言。

**依据**：§4.4、§8.11、§4.4b 敏感信息段。

## 4. C-4：d1 调用形态

**现象**：§13.4 开头仍写「解法有两条，需要实测选定」，与同节末尾「已按 §6.4 实测改判
B 优先级上调」自相矛盾，实现阶段无从下手。

**补齐**：开头改为「取舍已按 §6.4 实测定案」；补「实现阶段的落点」——阶段 1 先实现方案 A
为默认并预留 turn 聚合钩子，方案 B 随阶段 1.5 兼容层与阶段 4 落地，不传 turn 字段永远退化 A。

**依据**：§6.4、§13.4、§14 阶段 1/1.5/4、§22.5。

## 5. 结论

4 项 P1 全部落在架构正文；无新增章节、无冻结文档或评审历史改动；
./scripts/check-doc.sh 退出码 0。按 §3.6，本报告是历史证据，只作待办来源，不回写结论。
