# toolgate 架构评审终审（signoff）：返回层告知协议

> 终审对象：docs/architecture-review-2026-11-08-followup.md（第二轮复核）所列残余项及 lead 指定的 11 项修复。
> 核对方式：逐条对照 + 修复后章节交叉一致性核对（04-tools §4.4b/§4.5、08-isolation §8.7、15-risk §15.7、18-deployment §18.7）+ Rust 类型草案引用完整性检查 + ./scripts/check-doc.sh 实跑（退出码 0）。
> 结论：**P0 清零；P1 未清零——P1-4（Rust 类型草案不可编译）与 N-1（ObservedFacts 缺 UNCALIBRATED 输入）仍未关闭。暂不能进入实现阶段评审。**

---

## 0. 结论摘要

| 状态 | 数量 | 明细 |
|---|---|---|
| 已关闭 | 9 | P0-1、P1-3、N-2、N-5/P2-12、N-6、N-7、N-8、N-9、N-10 |
| 未关闭 | 2 | P1-4（类型草案不可编译）、N-1（finalize_notices 契约缺 UNCALIBRATED 观测项） |

**残余 P0：0 条。残余 P1：2 条（P1-4、N-1）。**
**结论：不能写「P0/P1 清零，可进入实现阶段评审」；需先修 P1-4 与 N-1。**

---

## 1. 逐项核对（lead 指定 11 项 + 第二轮残余）

| # | 核对项 | 状态 | 证据 |
|---|---|---|---|
| 1 | P0-1 模板分隔符 | **已关闭** | 04-tools.md:197–199 总则改为「字段间用单个空格分隔；单值字段的值内不允许出现空格与『 · 』；列表参数用『,』分隔」；11 个示例 153–163 全部空格分隔；08-isolation.md:176–177 亦为空格；PARTIAL 的 failed=a.rs,b.rs 属 198–199 的列表例外 |
| 2 | P1-3 枚举值域 | **已关闭** | 04-tools.md:173–178 分条列出 DEGRADED.requested/actual、ISOLATION_FALLBACK.requested/actual、unit/views/fallback/adapter、SNIFFED.actual 的完整值域 |
| 3 | P1-4 Rust 类型 | **未关闭** | 04-tools.md:258–274 已改外部 tag（tag=code、content=params、rename_all=SCREAMING_SNAKE_CASE、rename_all_fields=camelCase）的单一 enum Notice（11 变体），删除 untagged；但嵌套 enum 无 derive/serde 属性，4 个类型未定义，且序列化示例与模板大小写不一致（详见 §2.1） |
| 4 | N-1 finalize_notices | **未关闭** | 04-tools.md:324–344 已补 ObservedFacts（8 个 Option）与函数签名；15-risk.md:137 已加执行事实属性测试；但 ObservedFacts 缺 UNCALIBRATED 所需的 slot/adapter 观测项，而 finalize_notices 被要求产 UNCALIBRATED（详见 §2.2） |
| 5 | N-2 DEGRADED 两阶段 | **已关闭** | 04-tools.md:141–142 生成表已分「DEGRADED（能力/延迟/上游类）」决策阶段与「DEGRADED（压缩熔断）」执行后；§4.5:318–320 明确 decision_notices 不含压缩熔断 |
| 6 | N-5/P2-12 告知测试 | **已关闭** | 15-risk.md:140 告知完备性改为「全部 11 个 code 每个至少一条路径」；141 预算测试改为「11 code 全触发，合并后 ≤6 条、≤400 字节、params ≤4、安全与完整性类均在」 |
| 7 | N-6 合并算法 | **已关闭** | 04-tools.md:217–230 确定性算法：同 code 合并 → 类优先级（安全 > 完整性 > 可用性 > 认知）→ 超 6 条省略认知类并记 structuredContent.meta.noticeOverflow → 超字节缩短字符串；与 214 行的 ≤6 条一致 |
| 8 | N-7 meta 命名 | **已关闭** | 18-deployment.md:442 已改 slmStatus；04-tools.md:245 明确 JSON 字段一律 camelCase |
| 9 | N-8 reason 枚举与 camelCase | **已关闭** | 04-tools.md:235 已删 partial_failure；258–261 增加 rename_all_fields=camelCase |
| 10 | N-9 同 code 排序 | **已关闭** | 04-tools.md:189–190 改为按二级键字典序（slot → pointer → failed 首项 → level），不按并发完成顺序 |
| 11 | N-10 列表逗号例外 | **已关闭** | 04-tools.md:197–199 明确「列表参数按格式约定用『,』分隔，逗号只出现在列表分隔位置」 |

---

## 2. 未关闭项详述

### 2.1 P1-4 Rust 类型草案不可编译，且与模板示例大小写不一致

**位置**：04-tools.md:247–290。

**现象**：
1. **嵌套 enum 无 derive/serde 属性**：Notice（258–262）derive Serialize/Deserialize，其字段使用了 TruncUnit、OutputShape、IsolationLevel、FallbackAction、SlotFallback、AdapterKind、SniffActual；但 281–287 行这些 enum 均没有 derive(Serialize, Deserialize)，也没有 serde 的 rename_all 属性。按 Rust 语义，Notice 的 derive 会因字段类型不满足 trait bound 而编译失败（E0277）。
2. **4 个类型完全没有定义**：ConfidenceViews（265）、DegradeReason（264）、IsolationReason（266）、NormalizeKind（270）在全文只出现在使用处，没有任何 enum 定义（grep 确认）。编译会报 E0412（未定义类型）。
3. **序列化示例与模板示例不一致**：290 行序列化示例写 unit 为 "LINES"，而 153 行模板示例与 168 行格式约定写 unit=lines（snake_case）。同一参数的枚举值出现两套大小写；实现者无法确定 TruncUnit 的 serde rename 应写 SCREAMING_SNAKE_CASE 还是 snake_case。
4. **OutputShape 含 Metadata 但 DEGRADED.requested 值域不含**：174 行说 metadata 只出现在 actual；而 282 行 OutputShape 含 None、Metadata，会让 requested 也能取 Metadata，类型层不约束。

**为什么是 P1**：P1-4 的验收标准是「Rust 侧 NoticeCode / params 类型足够明确、可实现」。当前草案照抄会编译失败；§15.7 的「枚举穷举 match 无通配」和渲染快照测试都无法建立。

**修复建议**：
1. 为 281–287 的所有 enum 增加 derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq) 与 serde rename_all（与模板一致的 snake_case），并在 290 行示例中统一显示。
2. 补定义 ConfidenceViews（Both、Local）、DegradeReason、IsolationReason、NormalizeKind（Eol、Whitespace），并把 235 行的 reason 枚举分到对应类型。
3. 把 OutputShape 的 Metadata 拆到 actual 专用类型（如 DegradedActual），或修改 174 行值域说明。

### 2.2 N-1 ObservedFacts 缺 UNCALIBRATED 的观测项，finalize_notices 契约不完整

**位置**：04-tools.md:324–344（§4.5）、137–145（§4.4b 两段生成）。

**现象**：
- ObservedFacts（327–336）的 8 个 Option 是 truncation / redaction / confidence / partial / normalization / decode / sniff / compression。
- finalize_notices 的产出清单（341–342）明确包含 UNCALIBRATED；§4.4b:142 也把 UNCALIBRATED 列为执行后阶段。
- UNCALIBRATED 的 params 是 slot / adapter（04-tools.md:161、271）；ConfidenceObs 的注释只有 p / views，ObservedFacts 没有 adapter/slot 的观测项。因此 finalize_notices 无法生成 UNCALIBRATED，契约闭合不了。
- 另：ObservedFacts 的 8 个 Obs 类型（TruncationObs / RedactionObs / ConfidenceObs / PartialObs / NormalizeObs / DecodeObs / SniffObs / CompressionObs）也都没有结构定义，只有注释——这是 P3 级补充。

**为什么是 P1**：N-1 的验收标准是「finalize_notices 的输入/输出约束完整」。缺 UNCALIBRATED 的输入意味着 9 个执行事实类 code 只有 8 个能生成，实现者必须猜或把 UNCALIBRATED 挪到别的阶段。

**修复建议**：在 ObservedFacts 增加 uncalibrated: Option<UncalibratedObs>（slot、adapter），或明确把 UNCALIBRATED 改为决策阶段/配置阶段生成并从 §4.5:341–342 与 §4.4b:142 的 finalize 清单移除；同时给 8 个 Obs 类型补结构定义（或直接复用 Notice 变体的字段结构）。

---

## 3. 新发现的交叉矛盾（P2/P3）

### C-1（P2）序列化示例与模板示例的枚举值大小写不一致

**位置**：04-tools.md:153（unit=lines）、168（枚举值 snake_case）、290（"unit":"LINES"）。

**现象**：同一 TruncUnit 在模板示例中是小写 lines，在序列化示例中是大写 LINES；views=local 与 ConfidenceViews::Local 若按 SCREAMING_SNAKE_CASE 序列化也会变成 LOCAL。模板渲染与 JSON 序列化没有统一的枚举映射规则。

**修复建议**：明确「模板渲染用 snake_case 字符串、JSON 序列化用同一套字符串」或两处都改同一大小写；在 §4.4b 的格式约定里加一句「模板与 JSON 使用同一枚举字符串」。

### C-2（P3）structuredContent.meta.noticeOverflow 未在响应契约定义

**位置**：04-tools.md:224–226；18-deployment.md:442；11-runtime.md:108。

**现象**：合并算法说「省略数量记入 structuredContent.meta.noticeOverflow（程序字段，不进 content）」，但 §18.7 的响应结构与 §13.3 的 ToolResponse 字段清单都没有 meta.noticeOverflow。实现者不知道这个字段挂在哪一层（structuredContent.meta 还是顶层 meta）。

**修复建议**：在 §18.7 与 §13.3 明确 noticeOverflow 的层级与类型（u32），或在 §4.4b 写「顶层 meta.noticeOverflow」并统一。

### C-3（P3）Notice 与 NoticeCode 双 enum 11 变体重复

**位置**：04-tools.md:252–255、262–274。

**现象**：NoticeCode 与 Notice 各自维护 11 个变体，Notice::code() 再映射一次；新增 code 要改三处（NoticeCode、Notice、code()），与「新增 code 编译失败」的初衷部分冲突（漏改 NoticeCode 可能仍能编译）。

**修复建议**：二选一——保留 NoticeCode 作为独立 code 枚举但 Notice 改为 struct { code: NoticeCode, params: NoticeParams }（回归结构体），或删除 NoticeCode 直接用 Notice 的变体判别；避免两套 11 变体。

### C-4（P3）Normalized.level 用 u8 不约束 2|3；OutputShape 的 Metadata 归属

**位置**：04-tools.md:270、282；174、160。

**现象**：level 是 u8，可接受任意值，而值域是 2/3；OutputShape 含 Metadata 但 DEGRADED.requested 值域不含。类型层没有把值域收紧。

**修复建议**：level 用 NormalizeLevel（Level2、Level3）；把 Metadata 拆到 DegradedActual 专用 enum。

### C-5（P3）ObservedFacts 的 8 个 Obs 结构未定义

**位置**：04-tools.md:327–336。

**现象**：TruncationObs 等 8 个类型只有行内注释（kept / total / unit / pointer 等），没有结构定义；实现者无法直接按草案写。

**修复建议**：给 8 个 Obs 各写一个 struct（字段与对应 Notice 变体一致），或直接复用 Notice 变体字段。

### C-6（P3）同 code 二级键在字段缺失时的排序规则未说明

**位置**：04-tools.md:189–190。

**现象**：二级键是 slot → pointer → failed 首项 → level；不同 code 只有其中一部分（如 NORMALIZED 只有 level，TRUNCATED 只有 pointer）。字段缺失时如何比较（空串、跳过、固定序）没有说明。

**修复建议**：明确「缺失键视为空字符串，按剩余键继续比较」，或给每个 code 固定一个排序键。

---

## 4. 已关闭项确认

- **P0-1**：04-tools.md:197–199 总则与 153–163 的 11 个示例、08-isolation.md:176–177 的 §8.7 示例完全一致（字段间单个空格；单值字段禁空格与「 · 」；列表用逗号并说明例外）。
- **P1-3**：04-tools.md:173–178 枚举值域完整，覆盖 DEGRADED、ISOLATION_FALLBACK、unit/views/fallback/adapter、SNIFFED.actual。
- **N-2**：决策/执行后阶段的 DEGRADED 已分工，§4.5 明确 decision_notices 不含压缩熔断。
- **N-5/P2-12**：§15.7 告知完备性覆盖全部 11 个 code；预算测试改为 11 code 全触发 ≤6 条、≤400 字节。
- **N-6**：合并算法确定性，含同 code 合并、类优先级、超 6 条省略认知类并记 noticeOverflow、超字节缩短字符串。
- **N-7**：§18.7 meta.slmStatus 与 §4.4b 的 camelCase 一致。
- **N-8**：partial_failure 已删，rename_all_fields=camelCase 已加。
- **N-9**：同 code 多条按二级键字典序，不按并发完成顺序。
- **N-10**：列表参数逗号例外已写清。

---

## 5. 结论与建议

**P0 清零（1/1），P1 未清零（P1-4、N-1 两条）。** 本轮不能写「P0/P1 清零，可进入实现阶段评审」。

建议：
1. 先修 P1-4：给 281–287 的嵌套 enum 加 derive/rename，补 ConfidenceViews / DegradeReason / IsolationReason / NormalizeKind，统一模板与 JSON 的枚举大小写。
2. 再修 N-1：给 ObservedFacts 补 uncalibrated（slot/adapter）或调整 UNCALIBRATED 的生成阶段。
3. C-1 至 C-6 为 P2/P3，可批量修；其中 C-1（枚举大小写）建议与 P1-4 一起修。
4. 修完上述两项后，可再次终审；其余已关闭项无需重开。

---

*本报告只记录终审结论，不修改架构正文；修复落在 docs/architecture/ 对应章节。*
