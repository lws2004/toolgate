# toolgate 架构评审：返回层告知协议（§4.4b）（2026-11-08）

> 评审对象：docs/architecture/04-tools.md §4.4b（新增）与 §4.4/§4.5；08-isolation.md §8.7；09-output.md §9.0/§9.1/§10.1/§10.4；06-cost.md §7.2/§7.3；15-risk.md §15.7；90-appendices.md 附录 C。
> 评审方式：全文通读上述章节 + 跨章节逐条核对 + 全文档「告知 / 标注 / 说明 / 警告」扫描（「告知」62 处、「说明|提示|告警」45 处命中）+ ./scripts/check-doc.sh 实跑（退出码 0，130 个交叉引用全部可解析）。
> 结论：**方向成立（一个事实源、两个通道），但模板契约与 notice 生成路径存在 3 条 P0，当前版本不可直接进入实现。**

---

## 0. 结论摘要

本次改动把返回层描述信息从「各章各写文案」收敛为「枚举 + 固定模板 + 预算 + 测试」，方向与上一轮评审 P0-3 的根因一致，§8.7 的四字段表也比旧的自然语言示例更可测。但落地层面存在三类阻塞问题：

1. **模板语法自相矛盾**：§4.4b 总则规定「[CODE] key=value · key=value」，但 8 个 code 的渲染示例没有一个符合该语法；§8.7 的 ISOLATION_FALLBACK 与 §4.4b 的同一 code 渲染字段名也不同。§15.7 要求的「渲染快照断言字节」因此无法编写。
2. **notice 生成路径不成立**：§4.5 把 notices 放进 decide_output（声明为纯函数）的返回值，但截断量、脱敏计数、d1 概率、嗅探结果等 notice 必需参数都不在函数输入里。
3. **枚举不完整**：§5.4 的 edit 归一化告知、§3.10 的非校准概率标注、§5.6 的解码替换字符警告三处已明确要求「告知/标注/警告」，8 个 code 无对应项，「唯一事实源」承诺不成立。

按严重度统计：**P0 3 条、P1 7 条、P2 12 条、P3 5 条，共 27 条。**

| 级别 | 数量 | 主题 |
|---|---|---|
| P0 | 3 | 模板契约、decide_output 数据流、notice 枚举完备性 |
| P1 | 7 | meta 通道残留、code 互斥、字节稳定格式、Rust 类型、敏感参数、预算、§10.1 示例 |
| P2 | 12 | 术语重名、交叉引用、分级表一致性、govern/差距表同步、阈值与压缩告知、测试覆盖 |
| P3 | 5 | 附录术语、命名规范、示例语义、表格风格 |

---

## 1. 评审范围与证据

| 对象 | 读法 | 核对内容 |
|---|---|---|
| 04-tools.md §4.4–§4.5、§5.4、§5.6 | 逐行通读 | 告知协议枚举、模板、预算、decide_output 签名、edit 归一化与编码警告 |
| 08-isolation.md §8.7、§8.10–§8.12 | 逐行通读 | 四字段表、示例、脱敏与路径规则 |
| 09-output.md §9.0、§9.1、§10.1、§10.2d、§10.4 | 逐行通读 | 截断/脱敏/槽位/嗅探告知引用与示例 |
| 06-cost.md §7.2、§7.3 | 逐行通读 | LOW_CONFIDENCE / DEGRADED / SLOT_UNAVAILABLE 引用 |
| 15-risk.md §15.7 | 逐行通读 | 三项告知测试 |
| 90-appendices.md 附录 C | 逐行通读 | notice 术语 |
| architecture.md、03-slots.md、11-runtime.md、18-deployment.md、20-behavior.md | 相关段落核对 | §1.4.5 错误契约、§3.10 概率标注、§13.3 差距表、§18.1/§18.7/§18.9、§20 快照、§21.7.5 partial |
| ./scripts/check-doc.sh | 实跑 | 退出码 0；章节连续性、130 个交叉引用、围栏、链接、规模均通过 |

**本次评审范围排除工作区其他未提交改动。** 工作区同时存在另一条主题的未提交修改：03-slots.md §3.10/§3.11 槽位配置与绑定、04-tools.md §4.8 理解链、11-runtime.md 本地 agent 相关、18-deployment.md §19.7 平台范围与本地 agent、20-behavior.md §21.7 本地 agent、08-isolation.md §8.12 平台范围行、90-appendices.md 附录 D 的 models/slots 配置行，以及 AGENTS.md、README.md、architecture.md 的对应改动。这些不计入本次评审缺陷。

---

## 2. P0：进入实现前必须先定（3 条）

### P0-1 模板语法与 8 个渲染示例全部不符，§8.7 与 §4.4b 同一 code 互相矛盾

**位置**：§4.4b（04-tools.md:134-148「枚举与模板」）、§8.7（08-isolation.md:164-178）。

**现象**：§4.4b 明确写「模板固定为 [CODE] key=value · key=value：字段顺序固定、分隔符固定」，但枚举表 8 个 code 的渲染示例没有一个符合该语法：

| code | 文档渲染 | 与 key=value · key=value 的偏差 |
|---|---|---|
| TRUNCATED | [TRUNCATED] 2000/8734 lines · full: spill://ab12cd34 | 首字段不是 key=value；full: 用冒号 |
| DEGRADED | [DEGRADED] summary→none slot=summarize · mechanical window | 首字段是箭头；末字段无 key |
| LOW_CONFIDENCE | [LOW_CONFIDENCE] p=0.62 · local only | local only 无 key |
| ISOLATION_FALLBACK | [ISOLATION_FALLBACK] raw→structure rule=**/.env* reason=retention 8%<50% | requested/actual 合并为箭头；reason 值含空格 |
| SLOT_UNAVAILABLE | [SLOT_UNAVAILABLE] vision · meta returned | 两个字段都无 key=value |
| REDACTED | [REDACTED] 3 secrets masked · retention 99.4% | count 无 key |
| PARTIAL | [PARTIAL] 7/9 items · failed: a.rs, b.rs | done/total 无 key；failed 用冒号 |
| SNIFFED | [SNIFFED] ext=.txt actual=binary | 字段间用空格，不是 · |

同时 §8.7 对同一个 ISOLATION_FALLBACK 给出的是「[ISOLATION_FALLBACK] requested=raw actual=structure rule=**/.env* reason=retention 8%<50%」，字段名与 §4.4b 的 raw→structure 不同。两章都是本次改动新增的「固定模板」，读者无法判断哪一个才是权威。

**为什么是 P0**：模板字节稳定是本次改动的核心交付物之一。总则与全部示例矛盾，意味着实现者无法确定渲染器语法；§15.7 的「渲染快照（模板字节锁定）」和 §4.4b 的「模板改动等同工具描述改动，属于契约变更」都建立在一个不存在的固定语法上。两章各写一套，必然产生两套实现和不可比对的快照。

**修复建议**：
1. 二选一并全文档统一：**(a)** 严格结构化——所有 code 统一为 [CODE] key=value · key=value，逐 code 给出唯一字段名、顺序、值域与格式；或 **(b)** 承认模板是「[CODE] 固定摘要句」的自然语言形态，放弃「程序可解析」承诺，只保留快照测试。
2. 推荐 (a)，因为 §4.4b 同时要求「structuredContent 原样携带同一份 notices」，结构化字段本来就有；模板只是 content 视图，不应该再承担解析职责。
3. 以 §8.7 的四字段 requested/actual/rule/reason 为 ISOLATION_FALLBACK 的权威字段名，更新 §4.4b 的渲染列；删除或改写 §8.7 的第二个示例使其与 §4.4b 完全一致。
4. 给 §15.7 的渲染快照测试补一份「固定 params → 完整期望字节」的样例表，作为模板变更的验收入口。

---

### P0-2 §4.5 的 decide_output 是纯函数，但返回值 notices 所需的输入不在签名里

**位置**：§4.5（04-tools.md:180-194）、§4.4b（04-tools.md:125-132）。

**现象**：§4.5 把返回值改为 (output_shape, fallback, notices, meta)，并规定「这个函数必须是纯函数，配属性测试：同样的输入两次调用得到同样的输出」。但签名输入只有：

~~~text
intent, isolation_default, max_exposure, path_rule, content_rules,
upstream_capability, slot_available, latency_budget
~~~

而 notice 的必填参数分别来自执行期或模型：
- TRUNCATED 的 kept / total 取决于实际输出行数与截断阈值（执行后才知道）；
- REDACTED 的 count / retention 取决于脱敏正则的命中统计（执行后才知道）；
- LOW_CONFIDENCE 的 p 来自 d1 判定（模型推理结果）；
- PARTIAL 的 done / total 取决于任务实际完成情况；
- SNIFFED 的 ext / actual 来自类型嗅探（执行前流水线）；
- DEGRADED / SLOT_UNAVAILABLE 的 reason / fallback 才可能由形态决策得到。

纯函数若内部调用 d1 或读取执行结果，就不再是纯函数；若不调用，notices 只能生成一部分，与「notice 是唯一事实源」「一次返回的所有告知」冲突。§4.5 的「告知与输出形态在同一处决定」把决策阶段与执行后的事实混为一谈。

**为什么是 P0**：这是本次改动对 ToolResponse 的核心结构改动，数据流不成立会导致实现时返工：要么把 notices 拆成两处生成（破坏唯一事实源），要么把执行结果塞进 decide_output 的输入（破坏「输出形态决策」的定位）。两种结果都会让 §15.7 的「同输入同输出」属性测试与告知完备性测试互相打架。

**修复建议**（推荐方案 B）：
- **方案 A**：把 decide_output 明确定义为执行后的纯函数，输入增加执行事实：truncation: {kept, total}、redaction: {count, retention}、d1: {p, whether_both}、sniff: {ext, actual}、partial: {done, total, failed}；并在 §4.5 说明这些输入由执行层填充、函数本身仍不产生副作用。
- **方案 B**：拆成两阶段——decide_output 只产出「形态决策类 notice」（ISOLATION_FALLBACK / DEGRADED / SLOT_UNAVAILABLE）；执行后由 finalize_notices 合并「执行事实类 notice」（TRUNCATED / REDACTED / PARTIAL / SNIFFED / LOW_CONFIDENCE）。两阶段写入同一 notices 数组，对外仍是一个事实源。
- 无论选哪个，§4.5 的「同样输入两次调用得到同样输出」需要明确是「同一阶段内」，且 §15.7 的告知完备性测试要覆盖两阶段。

---

### P0-3 notice 枚举遗漏已有明确告知要求，「唯一事实源」不成立

**位置**：§4.4b 枚举表（04-tools.md:134-145）对照 §5.4（04-tools.md:565-566）、§3.10（03-slots.md:383-384）、§5.6（04-tools.md:673）。

**现象**：文档有三处已经明确要求告知，但 8 个 code 无对应项，且都不属于「错误响应」（不能走 §1.4.5 的 ToolError）：

| 位置 | 原文要求 | 为什么不是现有 8 类 |
|---|---|---|
| §5.4 第 2 级 | 行尾归一化命中「应用，并告知『按行尾归一化命中』」 | 不是截断/降级/低置信/隔离回退/槽位/脱敏/部分成功/嗅探；输出形态没变，只是匹配策略 |
| §5.4 第 3 级 | 空白归一化命中「应用，并告知」 | 同上 |
| §3.10 | 「非校准概率必须标注：返回的 structuredContent 带 calibrated: false，content 里说明（§4.4）」 | LOW_CONFIDENCE 是 p<0.85 的置信门控；非校准是概率性质问题，p=0.95 仍非校准，两者正交 |
| §5.6 | 解码出现替换字符「read 正常返回但**明确警告**」 | 既非截断也非嗅探；是输入编码有损，模型看到的文本不可靠 |

**为什么是 P0**：§4.4b 自己给出判据「漏一条告知是决策事故，多一条告知只是几十 token」。如果枚举不覆盖这些已有要求，实现只有两条路：绕过 notice 直接写文案（破坏「唯一事实源」），或者把它们硬塞进不匹配的 code（破坏语义）。无论哪条，§15.7 的「告知完备性」测试都会漏路径，且「新增 code 编译失败」的反向保护无从谈起。

**修复建议**：
1. 新增三个 code，并补齐参数与模板：NORMALIZED（params: level、kind；渲染如 [NORMALIZED] level=2 · kind=eol）、UNCALIBRATED（params: slot、adapter；渲染如 [UNCALIBRATED] slot=decide · adapter=json）、LOSSY_DECODE（params: encoding、replacement_count；渲染如 [LOSSY_DECODE] encoding=gbk · replacements=3）。
2. 若决定不新增，必须在 §4.4b 写明每个场景归入哪个 code 的哪个参数，并同步修改 §5.4/§3.10/§5.6 的措辞指向该 code；不能保留「必须告知」但不给映射。
3. 在 §4.4b 增加「明确排除」小节：错误响应（ToolError 的 fact/reason/nextSteps，§1.4.5）、配置校验（§18.9 启动失败/告警）、快照（§20.3 路径属事实字段；SNAPSHOT_EXPIRED/SNAPSHOT_QUOTA_EXCEEDED 走错误码）、本地副本（§8.12 不进入返回）、运维告警（启动日志/toolgate list）。把这些写成清单，避免下一次再被问「为什么快照没有 notice」。
4. §15.7 的「告知完备性」测试从「截断/降级/回退」扩展到上述路径 + 新增 code 对应路径。

---

## 3. P1：进入实现前应修（7 条）

### P1-1 §7.3 第 2 条与 §18.1 第 3 条仍把告知放在 meta，和 §4.4b 直接冲突

**位置**：§7.3（06-cost.md:185-196，尤其第 2 条 189 行）、§18.1 进程内模式第 3 条（18-deployment.md:64-66）、§4.4b。

**现象**：§7.3 第 1 条刚写完「必须按 §4.4b 的 DEGRADED / LOW_CONFIDENCE / SLOT_UNAVAILABLE 写进 content 文本，不能只放 meta」，第 2 条紧接着说「降级要标注：在 meta 里写清哪个槽位不可用，供调用方判断」；§18.1 第 3 条说「未就绪期间该槽位按不可用降级，并在 meta 里标注『模型加载中』」。同一文档对同一事实给了两个互斥的通道要求。

**为什么是问题**：这正是上一轮评审 P0-3「meta 承载告知」的同类问题。本次 §4.4b 的整个目的就是消除它；残留的两句会让实现者按字面只写 meta，模型看不到槽位不可用/模型加载中，继续把降级产物当正常结果。

**修复建议**：§7.3 第 2 条改为「同时在 structuredContent/meta 提供结构化字段（slm_status、slot），content 按 §4.4b 的 SLOT_UNAVAILABLE 渲染」；§18.1 第 3 条改为「按 §4.4b 的 SLOT_UNAVAILABLE 告知（slot=… · fallback=loading），meta 附状态字段」。全文再扫一遍「在 meta 里标注/写清」的表述，统一改为「content 告知 + meta 结构化字段」。

---

### P1-2 notice code 互斥性不足，三个边界没有裁决规则

**位置**：§4.4b 枚举表（04-tools.md:134-145）、§7.3 第 1 条、§8.7 示例。

**现象**：
- DEGRADED 定义为「非隔离原因的输出形态降级（槽位/延迟/上游能力）」，其中「槽位」与 SLOT_UNAVAILABLE「槽位不可用或未配置」重叠；
- TRUNCATED「输出被截断或摘要替代」与 DEGRADED「输出形态降级」在「摘要替代」上重叠（摘要槽位不可用时摘要变原文是 DEGRADED，输出过长时摘要替代原文是 TRUNCATED，但两者都叫「摘要」）；
- REDACTED 与 ISOLATION_FALLBACK 在回退到 redact 时同时发生：§8.7 例 1 直接把「keys 3」塞进 ISOLATION_FALLBACK.reason，既没有另发 REDACTED，也没有说明是否要发；
- SNIFFED 判为二进制后若只返回元信息，是否同时 DEGRADED，没有说明。

**为什么是问题**：code 之间不互斥，实现就无法确定同一事实该发哪条；同一输入不同实现可能发 1 条或 2 条，与「模板字节锁定」「同输入同输出」直接冲突。

**修复建议**：在 §4.4b 增加「code 选择规则」小节，至少写清：
1. 槽位不可用只发 SLOT_UNAVAILABLE，不再发 DEGRADED；DEGRADED 只用于「槽位可用但输出形态被迫变化」（延迟/上游能力/压缩熔断）；
2. TRUNCATED 只用于「输出被裁短或摘要替代原文且原文可回溯」；DEGRADED 用于「形态从 A 变 B 但未裁短」；
3. 隔离回退到 redact 时同时发 ISOLATION_FALLBACK（原因）与 REDACTED（结果），ISOLATION_FALLBACK.reason 只放回退原因（如 retention_below_threshold），不复制脱敏统计；
4. 判为二进制且只回元信息时发 SNIFFED + DEGRADED 两条，或合并为一条并写明理由。

---

### P1-3 模板数值/枚举格式未定义，字节级稳定不可测

**位置**：§4.4b「模板固定为…」「硬预算」、§15.7「告知渲染快照」。

**现象**：以下格式均未定义：p=0.62 的小数位（0.62 / 0.620 / 62%）、retention 99.4% 的精度与单位、kept / total 的单位（行/字节/token）、done / total、failed 列表的分隔与转义（文件名含逗号怎么办）、reason 是否允许空格与逗号、whether_both 的布尔渲染、slot/fallback 的值域。同一输入不同实现可以渲染出不同字节，§15.7 的「断言渲染字节」没有可写死的期望值。

**为什么是问题**：§16.3 对策二要求输出确定性，§15.7 要求渲染快照。没有格式规范，「模板字节稳定」只是口号。

**修复建议**：给每个 code 一张参数表（类型、精度、单位、枚举值、空值策略），并规定统一格式化函数：浮点固定两位小数、百分比固定一位小数、列表固定「, 」分隔且值内不允许分隔符（或使用引号转义）、布尔渲染为固定词（both/local）。§15.7 的渲染快照用同一组固定 params 断言完整字节串。

---

### P1-4 Rust 侧 NoticeCode / params 类型未定义

**位置**：§4.4b（notices 中间表示）、§4.5（返回值）、§15.7（枚举穷举测试）。

**现象**：全文只有伪代码 notices: [{ code, params }]，没有：
- pub enum NoticeCode 的定义（8 个变体名、大小写、serde 序列化名）；
- Notice / NoticeParams 的类型（每个 code 的 params 是独立 struct，还是一个 map；如果是 map，键名与类型如何约束）；
- 与 structuredContent.notices 的序列化契约（字段名、数组顺序、空数组是否省略）；
- 与 §15.7「match 无 _ 通配分支；新增 code 编译失败」对应的可编译枚举。

**为什么是问题**：文档是唯一权威，实现者无法据此写出可编译的类型；「新增 code 编译失败」的保护没有载体，模板渲染也无法做类型检查。

**修复建议**：在 §4.4b 增加一段 Rust 类型草案，例如：

~~~rust
pub enum NoticeCode { Truncated, Degraded, LowConfidence, IsolationFallback, SlotUnavailable, Redacted, Partial, Sniffed }

pub struct Notice { pub code: NoticeCode, pub params: NoticeParams }

pub enum NoticeParams {
    Truncated { kept: u64, total: u64, pointer: Option<String> },
    Degraded { requested: String, actual: String, reason: DegradeReason },
    // ...每个 code 一组
}
~~~

并明确 structuredContent.notices 使用 camelCase 还是 snake_case、空数组是否省略。§15.7 的枚举穷举测试以这个类型为编译目标。

---

### P1-5 ISOLATION_FALLBACK 的 rule / reason 可能泄漏敏感信息，未纳入 §8.11 路径脱敏

**位置**：§4.4b ISOLATION_FALLBACK 渲染、§8.7 示例（08-isolation.md:176-177）、§8.11 路径规则（08-isolation.md:363-371）、§4.4（structuredContent 可能进模型上下文）。

**现象**：rule=**/config/**、**/.env* 通常安全，但 rule 来自用户配置，可能是含绝对路径/用户名/项目名的模式（如 /Users/alice/work/secret-project/**）。§8.11 明确列出「路径规则同时作用于 read/write/edit 的返回 meta、bash/search/task 的输出文本和错误 message」，没有列 notice 的 rule 字段。reason 是自由文本（例 keys 3, retention 99.4%），可能携带密钥数量、类型甚至片段；§4.4 已承认 structuredContent 不保证不进模型上下文，所以两个通道都应视为模型可见。

**为什么是问题**：隔离告知本身是安全边界的一部分。如果告知把被保护对象的用户名、项目名、密钥数量带出来，等于用「告知」绕过了「隔离」。任务评审维度 4 点名了 rule 路径与 reason retention。

**修复建议**：
1. rule 只渲染「脱敏后的 glob 模式」：过 §8.11 的 path_username / host_metadata 规则；绝对路径一律转工作区相对或 <external>；无法安全渲染时用规则 ID（如 rule=rule-07）。
2. reason 限定为有限枚举（retention_below_threshold / no_redactable_pattern / capability_missing / compression_budget 等），禁止自由文本拼接；REDACTED 的 count 只给数量，不给类型/内容。
3. 在 §8.11 的「路径规则作用范围」清单中加入「§4.4b 的 notice params（尤其 rule、pointer、failed 列表）」。
4. 给测试加一条：配置绝对路径规则时，notice 渲染不得出现用户名/绝对路径前缀。

---

### P1-6 预算（≤3 条 / ≤100 token / params≤4）与「不删类」自相矛盾，token 口径不可测

**位置**：§4.4b「信息分级与预算」「超预算先合并，不删类」。

**现象**：
- 「单次返回 ≤ 3 条 notice」与「三类不能省」冲突：安全相关有 ISOLATION_FALLBACK + REDACTED 两条，完整性相关 TRUNCATED + PARTIAL 两条，可用性 SLOT_UNAVAILABLE 一条，最坏 5 条。文档没有说「合并后允许超 3 条」还是「必须二选一」。
- 「合计 ≤ 100 token」没有 tokenizer 口径（不同模型 tokenizer 不同），也没有说按字节/字符近似；无法写测试。
- 「每条 notice 的 params ≤ 4 个」与 DEGRADED 渲染里出现 slot=（必填列只列 requested/actual/reason）不一致，实际渲染是 4 个字段，刚好卡线但没有说明。
- 「超预算先合并，不删类」只给了 REDACTED 合并例子；TRUNCATED、PARTIAL 如何合并（kept/total 怎么合并？failed 列表怎么合并？）没有定义。

**为什么是问题**：预算自称「可测」，但口径缺失导致测试写不出来；预算与不删类冲突会让实现者自行取舍，产生不一致的裁剪策略。

**修复建议**：
1. 把「≤3 条」改为「常规路径 ≤3 条；安全类可突破到 5 条」或定义优先级（安全 > 完整性 > 可用性）。
2. token 预算改为「按 UTF-8 字节数 ≤ 400 字节（约 100 token，按每 token 4 字节估算）」或删除 token 数字，只保留行数与 params 数。
3. 明确 DEGRADED 的 params 为 requested/actual/reason/slot 四个，或在必填列补 slot。
4. 给同类合并写规则：count 类求和、列表类合并去重、阈值类取最差值；不可合并时保留最严重一条并计数。

---

### P1-7 §10.1 的 SLOT_UNAVAILABLE 示例未按模板渲染，且示例含绝对路径违反 §8.11

**位置**：§10.1（09-output.md:141-151）。

**现象**：正文说「告知按 §4.4b 的 SLOT_UNAVAILABLE 渲染，元信息本身走结构化字段」，但下面的 JSON 示例是：

~~~json
{ "type": "text", "text": "[图片文件] screenshot.png
PNG，1920x1080，840KB
绝对路径：/Users/x/project/screenshot.png

无法提供图片内容：vision 槽位未配置，上游模型也不接受图片输入。
可选路径：…" }
~~~

这段文本没有 [SLOT_UNAVAILABLE] 前缀、没有 slot= / fallback= 字段，是自然语言长文；并且绝对路径 /Users/x/... 直接暴露用户名，与 §8.11「模型可见路径默认工作区相对」及附录 D paths.expose_absolute: false 冲突。

**为什么是问题**：这是文档中唯一的 SLOT_UNAVAILABLE 具体返回示例，实现者会照抄。它同时违反本次新增的模板契约和既有路径脱敏规则，等于用一个示例抵消两条规范。

**修复建议**：把示例改为两段——content 按模板渲染（如 [SLOT_UNAVAILABLE] slot=vision · fallback=meta 加必要的文件元信息），元信息放 structuredContent 字段；路径改为 <workspace>/project/screenshot.png 或 spill://<hash>，并注明仅 raw + paths.expose_absolute: true 才返回绝对路径。

---

## 4. P2：文档一致性问题，可批量修（12 条）

### P2-1 §4.4b 的 L0/L1/L2 信息分级与 §2.1/§6.1 的 L0–L4 成本阶梯术语重名

**位置**：§4.4b「信息分级与预算」（04-tools.md:150-156）、§2.1 成本阶梯（architecture.md:383-431）、§6.1（06-cost.md:8-30）、附录 C。

**现象**：同一组 L0/L1/L2 在同一架构中表示两种完全不同的东西：§2.1/§6.1 是「确定性/判定/生成/视觉/云端」成本阶梯；§4.4b 是「必给/默认给/按需」的信息等级。附录 C 只定义 L0–L4 成本阶梯，没有定义信息分级。读者会把「notice 只覆盖 L0」误解为「notice 只在确定性层生成」。

**为什么是问题**：术语冲突会让实现者把 notice 与成本层混淆；§4.4b 的 L0 表又含「失败」但错误响应不走 notice，进一步加深误解。

**修复建议**：把 §4.4b 的信息分级改名为 G0/G1/G2（告知级）或 N0/N1/N2，并在附录 C 增加定义；或至少在 §4.4b 明确写「本节 L0/L1/L2 是信息等级，与 §2.1 的成本阶梯 L0–L4 无关」。

---

### P2-2 §9.0 的 REDACTED 引用指向 §8.7，但 §8.7 没有定义 REDACTED

**位置**：§9.0（09-output.md:24）、§8.7（08-isolation.md:164-185）。

**现象**：「被 [REDACTED] 的片段不能从 spill 取回原文——这是隔离的代价，必须按 §4.4b 的 REDACTED 告知（§8.7）」。§8.7 只定义 ISOLATION_FALLBACK 四字段，没有任何 REDACTED 的定义或示例；脱敏在 §8.10/§8.11。

**为什么是问题**：交叉引用误导，读者点 §8.7 找不到 REDACTED 的触发条件与参数。

**修复建议**：改为「必须按 §4.4b 的 REDACTED 告知（脱敏规则见 §8.10/§8.11）」，或把 §8.7 扩展为同时覆盖 REDACTED（不推荐，会与 §8.10 重复）。

---

### P2-3 §4.4 与 §4.4b 的告知类别、两通道承诺不一致

**位置**：§4.4（04-tools.md:93-106）、§4.4b（04-tools.md:114-132）。

**现象**：§4.4 表格列 5 类（截断、降级、低置信、隔离回退、槽位不可用），§4.4b 列 8 类（增加脱敏、部分成功、嗅探结果）；§4.4 说「structuredContent 只承担程序判断」，§4.4b 说「structuredContent 原样携带同一份 notices」（告知性数据）。两节没有互相引用类别差异。

**为什么是问题**：同一节组内对「告知」的范围和通道职责有两种表述，实现者不知道以哪份为准。

**修复建议**：§4.4 的表格改为「至少包括 §4.4b 的 8 个 code」并加一句「notices 是结构化数据，不是自然语言文案，因此可同时出现在 structuredContent」；§4.4b 引用 §4.4 的「不能假设 structuredContent 不进模型上下文」作为前提。

---

### P2-4 L0 必给清单与「三类不能省」清单未覆盖全部 8 个 code

**位置**：§4.4b L0 表（04-tools.md:152-156）、「超预算先合并」（04-tools.md:162-164）。

**现象**：L0 表写「截断、降级、失败、脱敏、部分成功、隔离回退」6 类，缺 LOW_CONFIDENCE、SLOT_UNAVAILABLE、SNIFFED；且「失败」不是 8 个 code 中的任何一个（PARTIAL 是部分成功，错误响应不走 notice）。「三类不能省」只列安全（隔离回退、脱敏）、完整性（截断、部分成功）、可用性（槽位不可用），缺 DEGRADED、LOW_CONFIDENCE、SNIFFED。

**为什么是问题**：分级表是预算裁剪的依据，漏项会让实现者在预算超限时优先删掉未被列入「不能省」的 code。

**修复建议**：L0 表改为「8 个 code 全部必给」或明确列出 8 个；「失败」改为「部分成功（PARTIAL）」；「三类不能省」补全为：安全（ISOLATION_FALLBACK、REDACTED）、完整性（TRUNCATED、PARTIAL）、可用性（SLOT_UNAVAILABLE、DEGRADED）、认知（LOW_CONFIDENCE、SNIFFED）。

---

### P2-5 §4.4b「程序性（只进 structuredContent）」与模板渲染自相矛盾

**位置**：§4.4b 三类表（04-tools.md:120）、TRUNCATED/LOW_CONFIDENCE/REDACTED 模板、§9.1。

**现象**：三类表把「耗时、字节数、错误码、置信度数值、spillPath」列为程序性，「只进 structuredContent，不占模型 token」；但 TRUNCATED 模板含 full: spill://ab12cd34（pointer 进 content）、LOW_CONFIDENCE 模板含 p=0.62（置信度数值进 content）、REDACTED 模板含 retention 99.4%（数值进 content）。§9.1 也明确说「截断与落盘的告知按 §4.4b 的 TRUNCATED 渲染（kept / total + pointer）」。

**为什么是问题**：同一字段被同时归为「只进 structuredContent」和「必须进 content」，实现者会二选一，而两种选择都违背另一条规范。

**修复建议**：把「程序性」表改为「默认只进 structuredContent；当该数值是告知模板的必填字段时，按 §4.4b 模板进 content」，或把模板中的数值改为摘要化表述（如 p=low、retention=high）而把精确值只放 structuredContent。推荐前者，并明确「模板中的数值字段是告知的一部分，不适用程序性归类」。

---

### P2-6 多条 notice 的渲染顺序与合并规则未定义

**位置**：§4.4b「超预算先合并」、§15.7 渲染快照。

**现象**：一条返回可能同时有 TRUNCATED + DEGRADED + SLOT_UNAVAILABLE 等多条 notice；文档没有规定它们在 content 中的排列顺序（按 code 枚举顺序？按严重度？按产生阶段？），也没有规定合并后的 params 形态。不同实现顺序不同，字节不稳定；§15.7 的快照只按单个 code 测，测不出组合顺序。

**为什么是问题**：§16.3 的缓存保护要求输出字节稳定；多条 notice 是常见场景（截断 + 降级），顺序不定等于字节不定。

**修复建议**：规定固定顺序（如 ISOLATION_FALLBACK → REDACTED → TRUNCATED → PARTIAL → SLOT_UNAVAILABLE → DEGRADED → LOW_CONFIDENCE → SNIFFED，或按严重度降序），并在 §15.7 增加「多 notice 组合快照」。

---

### P2-7 §18.7 的 govern 接口响应没有 notices 字段

**位置**：§18.7（18-deployment.md:428-444）、§4.4b。

**现象**：govern 响应为 {content, structuredContent, meta:{slm_status, confidence, redacted, truncated, spillPointer}, isError}，没有 notices；而 §18.7 说「处理链与 read 相同」，§4.4b 说「一次返回的所有告知先产出一个中间表示」。如果 govern 也走治理管线，它应带 notices；如果不带，客户端拿不到告知。meta 里的 redacted/truncated 布尔是旧的程序字段，与 notices 可能双源。

**为什么是问题**：govern 是 DSH/pi 接入的主治理入口（§18.7），如果它不遵守 §4.4b，告知协议在最常用的路径上失效。

**修复建议**：在 §4.4b 明确适用范围「凡经治理管线产生的结果（含 MCP 工具返回与 §18.7 govern 响应）都带 notices」；同步 §18.7 响应契约，并说明 meta 的 redacted/truncated 是 notices 的派生视图还是废弃字段。

---

### P2-8 §13.3 的 ToolResponse 差距表未同步 notices 字段

**位置**：§13.3（11-runtime.md:108）、§4.5、§4.4b。

**现象**：§13.3「ToolResponse | 增加 confidence、slm_status、intent 字段；content 与 structuredContent 按 §4.4 分流」——没有 notices。本次改动把 notices 写入 §4.5 返回值和 §4.4b 的 structuredContent，但实现差距表没列。

**为什么是问题**：§13 是「从 v1 到 v2 要改什么」的路线图，缺 notices 会让实现阶段漏掉契约字段。

**修复建议**：§13.3 的 ToolResponse 行补「notices（§4.4b 的 code+params，content 与 structuredContent 同源）」；若 ToolResponse 类型在代码中已有定义，同步列出字段变更。

---

### P2-9 §4.4b 的 LOW_CONFIDENCE 阈值 0.85 与 §10.1 的 need_pixels 0.2/0.8 关系未说明，§10.3 又禁止第二套阈值

**位置**：§4.4b LOW_CONFIDENCE 行、§7.2（06-cost.md:164-181）、§10.1（09-output.md:117-127）、§10.3（09-output.md:292）。

**现象**：LOW_CONFIDENCE 写「d1 判定 p < 0.85（§7.2）」；§10.1 的 need_pixels 用 p≥0.8 / p≤0.2 / 中间三种处理；§10.3 说「不要另设第二套阈值——同一个判定出现两套阈值，缓存与预期都会失效」。若 need_pixels p=0.75，按 §10.1 是「两种都给」，按 §4.4b 是「LOW_CONFIDENCE」；是否要同时发两条、还是 0.2/0.8 是另一条轴，文档没有说明。

**为什么是问题**：同一 d1 概率字段在两个章节有不同阈值，且 §10.3 明确禁止第二套阈值；实现者无法确定何时发 LOW_CONFIDENCE。

**修复建议**：在 §4.4b 或 §7.2 加一句「LOW_CONFIDENCE 是通用置信度门控（p<0.85），与 §10.1 的 need_pixels 输出形态阈值（0.2/0.8）是两条独立的轴，可同时触发：p=0.75 的图片会同时得到 LOW_CONFIDENCE 和两种都给」；或统一阈值并修改 §10.3。

---

### P2-10 §10.2d 图片压缩熔断的告知归属未定义

**位置**：§10.2d（09-output.md:257-282）、§4.4b。

**现象**：档 4 熔断「返回文件元信息 + 路径（§10.1）」——这是「压缩流水线无法产出可接受图片」的降级，按 §4.4b 应属 DEGRADED（非隔离原因的输出形态降级），但文档没有说是否/如何告知；档 2/3 有损压缩（质量 85→70→55→40）后返回图片，模型也不知道图片被有损处理过。

**为什么是问题**：图片质量降级可能影响视觉判断（文字模糊、细节丢失），模型不知道就会基于低质量图做决策；这是告知完备性缺口。

**修复建议**：在 §10.2d 明确：档 4 熔断发 DEGRADED（requested=original · actual=metadata · reason=compression_exceeded）；档 2/3 有损压缩是否告知按「是否影响可读性」判定，若影响则发 DEGRADED（reason=lossy_compression），否则明确写「不告知」并说明理由（不改变语义）。

---

### P2-11 模板改动未提 serverInfo.version 同步

**位置**：§4.4b「模板改动等同工具描述改动，属于契约变更」、§16.3 对策一（15-risk.md:170-174）。

**现象**：§16.3 对策一要求「描述文字改动视为破坏性变更，必须同步改 serverInfo.version」；§4.4b 只说「属于契约变更」，没有引用 §16.3 对策一，也没说是否需要 bump 版本。

**为什么是问题**：模板是进入 content 的字节，改动会破坏客户端缓存前缀（§16.3 对策二），必须与工具描述一样走版本同步；漏掉会让升级静默破坏缓存。

**修复建议**：§4.4b 的「模板改动」句补「并按 §16.3 对策一同步 serverInfo.version」；§16.3 对策一的清单加入「notice 模板」。

---

### P2-12 §15.7 的告知测试未覆盖预算与新增/既有告知点

**位置**：§15.7（15-risk.md:131-142）。

**现象**：三项测试是「枚举穷举、渲染快照、告知完备性（截断/降级/回退路径）」。但：
- 没有「预算」测试（≤3 条、≤100 token、params≤4）；
- 告知完备性只列截断/降级/回退，没有覆盖 §5.4 归一化、§3.10 非校准、§5.6 解码警告（即使 P0-3 新增 code，测试也要跟上）；
- 渲染快照每个 code 一组固定 params，但没有组合场景（多条 notice 顺序）。

**为什么是问题**：§4.4b 把「预算」写成硬目标、把「测试层」列为三项，但测试清单缺预算与组合，无法验证本次改动的核心承诺。

**修复建议**：§15.7 增加「告知预算 | §4.4b 的 ≤3 条/≤100 token/params≤4 | 构造最坏组合输入，断言条数、字节上限、params 数」，并把告知完备性扩展到 §5.4/§3.10/§5.6 路径；渲染快照增加多 notice 组合用例。

---

## 5. P3：结构优化与措辞（5 条）

### P3-1 附录 C 的 notice 定义与信息分级术语不完整

**位置**：附录 C（90-appendices.md:59-74）。

**现象**：notice | 成功返回中的结构化告知单元：code + params，由 content 按固定模板渲染 | §4.4b。定义写「成功返回中」，但 §4.4b 明确 notice 也用于「部分成功返回」（PARTIAL）；同时附录 C 没有「告知」「信息分级 G0/G1/G2」「notice code」等本次新增术语。

**为什么是问题**：术语表是首次出现定义的权威，定义不准确会让读者以为 PARTIAL 不属于 notice。

**修复建议**：改为「成功或部分成功返回中的结构化告知单元——code + params，content 按固定模板渲染、structuredContent 原样携带」；补「告知」「notice code」与信息分级术语（若采纳 P2-1 的重命名）。

---

### P3-2 PARTIAL 与 Task Result 的 partial 同名不同层

**位置**：§4.4b PARTIAL、§18.3 status 取值（18-deployment.md:335-337）、§21.7.5（20-behavior.md:330-347）。

**现象**：PARTIAL 是「工具返回部分成功」的 notice code；partial 是 Task Result 的 status（有进展但未完成）；§21.7.5 的「返回 partial」是任务层。三处同名不同层，且附录 C 没有区分。

**为什么是问题**：实现者可能把 Task Result 的 partial 误当成 notice PARTIAL，或在工具返回里错误复用任务状态枚举。

**修复建议**：在附录 C 或 §4.4b 加一句「PARTIAL（notice code）只描述一次工具调用的部分成功，与 §18.3 Task Result 的 status: partial、§21.7.5 的预算耗尽 partial 是不同层」。

---

### P3-3 notice code 命名规范与 ErrorCode 不一致

**位置**：§4.4b 枚举表、§1.4.5 ErrorCode（architecture.md:231-235）。

**现象**：notice code 全大写蛇形（TRUNCATED、SLOT_UNAVAILABLE），ErrorCode 是 PascalCase（PathNotFound、SlotUnavailable）；两者都有 SLOT_UNAVAILABLE/SlotUnavailable 同名不同层，但没有任何说明。

**为什么是问题**：同名不同层容易让程序把 notice 当成错误码分支；命名风格不一致增加记忆负担。

**修复建议**：在 §4.4b 加「命名规范：notice code 全大写蛇形，与 §1.4.5 的 ErrorCode（PascalCase）分属两个命名空间；同名时以 all-caps vs PascalCase 区分」；或把 notice 的 SLOT_UNAVAILABLE 改为 SLOT_MISSING 避免同名。

---

### P3-4 §8.7 示例 reason 字段的语义与示例不符

**位置**：§8.7（08-isolation.md:166-178）。

**现象**：四字段表说 reason 是「为什么没用更高一级」，示例却写 reason=keys 3, retention 99.4%——这是脱敏细节，不是「为什么没用更高一级」；第二个示例 reason=retention 8%<50% 才是正确语义。两个示例的 reason 语义不一致。

**为什么是问题**：示例是模板的唯一具体参考，语义不一致会让实现者把脱敏统计塞进 reason。

**修复建议**：固定 reason 为回退原因枚举（如 retention_below_threshold / no_redactable_pattern / capability_missing）；例 1 改为 reason=redact_optimal 或类似，脱敏统计放 REDACTED 的 count/retention。

---

### P3-5 §8.7 字段表格式与 §4.4b code 表风格不统一

**位置**：§8.7（08-isolation.md:168-173）、§4.4b（04-tools.md:136-145）。

**现象**：§8.7 的字段名 requested/actual/rule/reason 未用代码体；§4.4b 的 code 用代码体。§4.4b 的「必填 params」列没有给出每个 params 的类型（string/int/float/enum），而 §8.7 的「例」列给了值示例。

**为什么是问题**：格式不统一降低可读性；参数类型缺失影响 P1-4 的类型落地。

**修复建议**：统一用代码体标注字段名；§4.4b 的 params 列改为「名称: 类型」格式，例如 kept: u64, total: u64, pointer: string。

---

## 6. 方向性结论（不属于缺陷）

1. **「一个事实源、两个通道」的方向正确**。§4.4b 把告知从「各章文案」收敛为 code + params 的中间表示，直接回应了上一轮评审 P0-3 的根因；content 由模板渲染、structuredContent 原样携带的设计，如果模板语法与类型定义补齐（P0-1/P1-3/P1-4），可以实现字节稳定与双通道同源。
2. **§8.7 的四字段表优于旧的自然语言示例**。requested/actual/rule/reason 把「为什么回退」变成可测试字段，是本次改动中最扎实的一处；只要与 §4.4b 的模板统一（P0-1），即可作为实现依据。
3. **§15.7 的三项测试方向正确**。枚举穷举、渲染快照、告知完备性覆盖了「类型—字节—路径」三个层面；按 P2-12 补齐预算与既有告知路径后，可以作为验收基线。

---

*本报告只记录评审结论，不修改架构正文；修复落在 docs/architecture/ 对应章节。*
