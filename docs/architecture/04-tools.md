# toolgate 架构设计 · 三层结构与六个工具（§4–§5）

> 本文是 [architecture.md](../architecture.md) 的分册，章节编号与总目录一致。
> 返回总目录：[architecture.md](../architecture.md)

## 4. 三层结构与 intent 契约

### 4.0 六个工具总览

| 工具 | 职责 | L0 执行 | 本地模型介入 | 协商输出 |
|---|---|---|---|---|
| read | 读文本/图片/URL | 解码、嗅探、截断；URL 走 fetch.provider | decide 判类型 / sensitivity / best_view；summarize 结构化；vision 看图 | raw / summary / structured / visual |
| write | 新建或整体覆盖 | 建父目录、写前快照 | 无 | 原文确认 + 快照路径 |
| edit | 局部修改 | 匹配、锚点校验、快照 | decide 槽位解释失败原因 | diff / 失败归因 |
| search | 内容检索 / 文件枚举 | 遍历、pattern（content 正则；files 子串·通配）、上限 | rerank 或 embedding 槽位（命中 > 20） | 全部 / top-k / 结构化 |
| bash | 一次性命令 | 执行、截断、落盘 | decide 判错；summarize 摘要 | raw / summary / structured |
| task | 后台长任务 | 进程管理、增量读取 | decide 判状态；summarize 摘要 | 原始增量 / 摘要 + 状态 |

**write 为什么不需要本地模型**：它的输入是调用方给的完整内容，输出是
「写成功/失败 + 快照路径」。没有需要理解的中间物，也没有可以压缩的
输出。给它加 SLM 是纯粹的复杂度。

因此 write / edit 的 intent 只做参数校验与 `meta` 标注，不切换输出形态（§4.2）。

这也说明一件事：**不是每个工具都该有智能层**。判断标准是「这个工具
是否产生需要理解的大块内容」——read/search/bash/task 会，write/edit 不会。

**为什么是 6 个：三条约束**

附录 A 说「6 个，理由是三条约束」，正文一直没把约束写出来。补在这里：

| 约束 | 含义 | 反例 |
|---|---|---|
| 缓存前缀稳定 | 工具表在请求里，抖动即失效；工具应覆盖高频动作，低频动作走组合而非新增工具。这是「上下文预算」的具体理由 | 加 glob：search 已能枚举文件；加 cat：read 已能做 |
| 治理规则集中 | 每条治理规则只在一个工具里实现 | grep + find 合并为 search（超限落盘逻辑一样） |
| 语义边界清晰 | 职责重叠时模型会选错；不产生「需要理解的大块内容」的工具不值得存在，write 是唯一例外。这是「智能层门槛」的延伸 | 加一个只做文件枚举的工具：search 已能；加一个只做 OCR 的工具：read 图片已能 |

语义边界清晰解释了为什么 write 在列内却不需要本地模型：它不产生大块内容，缺了它
read/edit 又无处落地。

这三条也是回答「要不要为编排加第 7 个工具」的依据（§21.4）。

**与 ADR 0002 的关系**：本节三条与 ADR 0002 一致（同名）；ADR 0002 是 2026-11 的原始论证，本节是现行表述。原「上下文预算」并入缓存前缀稳定，「智能层门槛」并入语义边界清晰。

### 4.1 每个工具的三层

```text
执行层   → 真正做操作：读文件、跑命令、检索
理解层   → 本地 SLM 介入：判定、摘要、抽取、看图；可组成有界多步链（§4.8）
协商层   → 决定回传什么：原文 / 摘要 / 结构化 / 原图 / 指针
```

协商层是新概念，也是本架构与普通工具层的分水岭。它决定「云端看到什么」。

**理解层不是一条固定流水线**：单步判定与摘要是最小形态；当输出需要「读一段 →
判断够不够 → 再读 → 摘要 → 自检」时，工具层可以编排一条有界多步链（§4.8）。
链路仍在单次工具调用内、动作菜单固定、无状态、无副作用。

### 4.2 intent：把协商权交给调用方

每个工具新增可选参数 intent，取值：

| intent | 含义 | 典型场景 | 对应隔离档位 |
|---|---|---|---|
| raw | 要原文，不要加工 | 改代码前读文件 | raw |
| summary | 要摘要，省 token | 了解构建为什么失败 | summary |
| structured | 要结构化数据 | 程序化处理检索结果 | understand |
| visual | 要原图 | 需要精确视觉推理 | raw |
| auto（可配置；v2 首版默认关闭） | 由 decide 槽位判定 | 大多数情况 | 取 isolation.default |

设计要点：**工具层默认值是 auto，但 v2 首版关闭它**；auto 也不等于「总是摘要」。

**兼容性约束**：v2 的首个正式版默认 `raw`，保持 v1 行为；`auto` 需显式配置开启（`intent.default: raw|auto`）。否则 v1 客户端升级后，同一次调用会从「原文」变成「可能摘要」，是破坏性变更。等客户端显式声明支持 v2 协商语义后，再切默认 auto。

**auto 是工具层的默认值，不是所有形态的最优值。** 客户端形态不同时应当显式
配置：云端程序编排（§21.1）建议 raw——压缩后的结果程序反而难处理；本地 agent
编排（§21.7）也建议 raw——它需要细节来做下一步决策。auto 只适合「结果直接进
模型上下文」的形态（§6.2 的 F1）。

**关键约束：intent 只是「请求」，不是「结果」。** 实际返回什么由
隔离策略决定——intent 请求超过策略允许的上限时，走回退链（先尝试脱敏、
结构化、摘要，最后才降级），并按 §4.4b 的 `ISOLATION_FALLBACK` 明确告知（§8.6、§8.7）。
模型不能通过 intent 绕过外部配置。

**适用范围**：write / edit 不产生需要理解的大块内容（§4.0），它们的 intent 只做统一
校验与 `meta.intent` 标注，不切换输出形态——输出固定为写入事实 / diff 与失败归因。

### 4.3 协商结果必须可预测

云端要能预期「我说 raw 会拿到什么」。所以协商层的行为是确定的：

- intent=raw 且内容未超限 → 一定返回原文，不做任何加工
- intent=summary → 一定返回摘要 + 原始指针
- intent=structured → 一定返回 JSON，schema 在工具描述里写明

这条是缓存友好性的要求：行为不可预测的工具有效地等于缓存不可用。

### 4.4 两条通道：模型可见与程序可读

工具返回有两个消费者：模型与程序。两者的契约不同，不能混用。

| 通道 | 载体 | 放什么 | 放错什么会出事 |
|---|---|---|---|
| 模型可见 | content 文本 | 凡影响下一步判断的事实——至少包括 §4.4b 的 10 个 notice code | 只放程序字段 → 模型以为拿到的是完整原文 |
| 程序可读 | structuredContent / meta | 耗时、路径、行号、字节数、错误码、置信度数值；`notices` 数组（structuredContent）；`noticeOverflow`（meta） | 放自然语言文案 → 程序要解析自然语言 |

`notices` 是结构化数据，不是自然语言文案，因此可同时出现在 structuredContent
（§4.4b 的「一个事实源、两个通道」）。

**为什么不能假设 structuredContent 不进模型上下文**：这取决于客户端。DSH 的 mcp-client 只把 content 投影给模型，但 MCP 协议不保证这一点；pi / Claude Code 等客户端可能序列化整个 result。所以：

> 任何模型必须知道的告知，必须出现在 content 文本里；structuredContent 不承担「必须让模型知道」的告知义务，其中的 `notices` 是结构化数据视图（§4.4b）。

这条是 §8.7「不告知等于欺骗」的落地方式，也解释了 §7.3 的「降级」为什么只能对程序静默、不能对模型静默。

### 4.4b 告知协议：一个事实源，两个通道

§4.4 只规定了「告知必须进 content」，没规定告知长什么样。同一件事各处各写一遍，
会出现两种问题：漏说（模型把降级产物当原文）与漂移（content 说 A、structuredContent 说 B）。
本节把告知收敛成一组**枚举 + 模板**。

**先分三类**——标准不同：

| 类型 | 例子 | 处理 |
|---|---|---|
| 告知性（改变认知） | 下表 11 个 notice code 覆盖的全部场景 | 必须进 content，一条不能漏；用固定模板压成一行 |
| 事实性（下一步依据） | 路径、行号、exitCode、diff、文件级变化、候选位置 | 结构化字段为主，文本只渲染必要项（§5.7） |
| 程序性（逻辑判断） | 耗时、字节数、错误码、置信度数值、spillPath | 默认只进 structuredContent；当它是 notice 模板的必填字段时，作为模板的一部分进 content |

判据：**漏一条告知是决策事故，多一条告知只是几十 token**（§1.4.2 的可恢复性判据）。
所以告知类不能靠「精简文案」省，只能靠「换表达形式」省。

**notice 是唯一事实源。** 一次返回的所有告知先产出中间表示：

```text
notices: [{ code, params }]
```

content 由固定模板渲染，structuredContent 原样携带同一份 `notices`——
「原样」指不另写一份文案，不是绕过脱敏：`rule` 的脱敏发生在 notice 构造时
（见本节「敏感信息」），两个通道拿到的是同一份已脱敏的 notices，
不存在「结构化通道给原始绝对路径」的例外。
**文案是视图，不是事实源**——两处各写一份文案迟早漂移。

**生成分两段，合并成一个数组**：

| 阶段 | 产出 | 输入 |
|---|---|---|
| 决策（`decide_output`，纯函数，§4.5） | `ISOLATION_FALLBACK` / `DEGRADED`（能力/延迟/上游类）/ `SLOT_UNAVAILABLE` | intent、隔离配置、上游能力、槽位可用性 |
| 执行后（`finalize_notices`，纯函数） | `TRUNCATED` / `REDACTED` / `LOW_CONFIDENCE` / `NORMALIZED` / `UNCALIBRATED` / `LOSSY_DECODE` / `SNIFFED` / `DEGRADED`（压缩熔断） | 执行层观测值：截断量、脱敏计数、decide 概率、匹配级别、解码结果、嗅探结果、压缩结果 |

两个函数都是纯函数：同输入同输出。`decide_output` 不读执行结果，`finalize_notices`
不读配置与模型，二者都不产生副作用；对外只有一个 `notices` 数组。

**枚举与模板**（11 个 code；新增 code 算契约变更）。notice code 用全大写蛇形，
与 §1.4.5 的 ErrorCode（PascalCase）分属两个命名空间——同名时以命名风格区分，
不要互相映射：

| code | 触发 | params（顺序固定） | 渲染示例 |
|---|---|---|---|
| `TRUNCATED` | 输出被裁短，或摘要替代原文且原文可回溯 | kept:u64 total:u64 unit:lines\|bytes pointer:option<string> | `[TRUNCATED] kept=2000 total=8734 unit=lines pointer=spill://ab12cd34` |
| `DEGRADED` | 非隔离原因的输出形态或质量降级（槽位可用但输出被迫变化，含有损压缩） | requested:enum actual:enum reason:enum slot:option<string> | `[DEGRADED] requested=summary actual=none reason=capability_missing slot=summarize` |
| `LOW_CONFIDENCE` | decide 概率低于门控（§7.2） | p:f32(2) views:both\|local | `[LOW_CONFIDENCE] p=0.62 views=local` |
| `ISOLATION_FALLBACK` | 隔离回退命中（§8.7） | requested:enum actual:enum rule:string reason:enum | `[ISOLATION_FALLBACK] requested=raw actual=structure rule=**/.env* reason=retention_below_threshold` |
| `SLOT_UNAVAILABLE` | 槽位不可用或未配置 | slot:string fallback:enum reason:enum(可选) | `[SLOT_UNAVAILABLE] slot=vision fallback=metadata reason=no_client` |
| `REDACTED` | 内容被脱敏 | count:u32 retention:percent(1) | `[REDACTED] count=3 retention=99.4%` |
| `SENSITIVE` | classify 或机械规则判定输出含密钥/个人信息（只标注，不脱敏） | level:secret\|personal | `[SENSITIVE] level=secret` |
| `NORMALIZED` | edit 按第 2/3 级归一化命中（§5.4） | level:2\|3 kind:eol\|whitespace | `[NORMALIZED] level=2 kind=eol` |
| `UNCALIBRATED` | 概率来自非校准适配器（§3.10） | slot:string adapter:enum | `[UNCALIBRATED] slot=decide adapter=json` |
| `LOSSY_DECODE` | 解码出现替换字符（§5.6） | encoding:string replacements:u32 | `[LOSSY_DECODE] encoding=gbk replacements=3` |
| `SNIFFED` | 嗅探结果与扩展名不一致，或判为二进制（§10.4） | ext:string actual:enum | `[SNIFFED] ext=.txt actual=binary` |

**`TRUNCATED` 的语义**：`kept` 是保留内容的体量——read / search 保留头部（`kept` 行数，read 的续读 offset = kept + 1），bash / task 保留尾部（`kept` 行数）；摘要替代原文时 `unit=bytes`（`kept` 是摘要字节数、`total` 是原文字节数），`pointer` 必须指向原文。

**格式约定**（渲染器统一实现，测试锁定）：

- 浮点固定两位小数（`p=0.62`），百分比固定一位小数（`retention=99.4%`）；`p` 与 `retention` 内部均按 0–1 比例存储，`retention` 渲染时乘 100（两位小数的口径与 structuredContent 同源，见 §4.7 的数值精度）；
- 整数不带千分位；枚举值用 snake_case（`retention_below_threshold`）；
- 列表用 `,` 分隔，每项先 URL 编码（文件名含逗号不会破坏解析）；
- 空值字段直接省略，不渲染 `key=`；
- `reason` 只允许有限枚举，禁止自由文本（见「敏感信息」）。

**枚举值域**：
- `DEGRADED.requested` / `actual`：输出形态 `raw` / `summary` / `structured` / `visual` / `none`；`actual` 另可 `metadata` / `compressed`；
- `ISOLATION_FALLBACK.requested`：隔离档位 `none` / `understand` / `summary` / `raw`；`actual`：回退动作 `redact` / `structure` / `summarize` / `refuse`；
- `unit`：`lines` / `bytes`；`views`：`both` / `local`；`fallback`：`metadata` / `text` / `mechanical` / `loading`；
- `SLOT_UNAVAILABLE.reason`：`unbound`（没有能服务该槽位的模型）/ `source_slot_mismatch`（来源×槽位在能力矩阵里不可用）/ `no_client`（当前构建未含该来源的协议客户端）/ `disabled`（连续失败被禁用）/ `call_failed`（本次调用失败或超时，槽位本身仍可用）。它是**可执行下一步的判据**：前两者改配置，`no_client` 重编译（`--features external-protocols`），`disabled` 等恢复或换模型，`call_failed` 检查模型服务监听与地址后重试；
- `SLOT_UNAVAILABLE` 的 `nextSteps` **不进 content**：渲染模板只带 `reason`，具体命令写在 structuredContent 的 `slmStatusDetail[nextSteps]`（程序字段，§4.7 高密度口径）；
- `adapter`：`native` / `json` / `text` / `ordered`；
- `SNIFFED.actual`：`binary` / `text` / `png` / `jpeg` / `gif` / `webp` / `pdf` / `zip` / `gzip` / `other`；
- `SENSITIVE.level`：`secret`（密钥、令牌、凭据）/ `personal`（个人信息）；多个命中取最高（`secret` > `personal`）。

**code 选择规则**（同一事实只发一条，组合按此裁决）：

1. **槽位不可用只发 `SLOT_UNAVAILABLE`**，不再发 `DEGRADED`；`DEGRADED` 只用于「槽位可用但输出形态或质量被迫变化」（上游能力不支持、有损压缩、压缩熔断）。
2. **`TRUNCATED` 只用于「裁短或摘要替代原文」且原文可回溯**；形态从 A 变 B 但未裁短用 `DEGRADED`。
3. **隔离回退到 redact 发两条**：`ISOLATION_FALLBACK`（原因）与 `REDACTED`（结果）；`ISOLATION_FALLBACK.reason` 只写回退原因，不复制脱敏统计。
4. **判为二进制且只回元信息时只发 `SNIFFED`**，不叠加 `DEGRADED`（没有形态决策）。
5. **`LOW_CONFIDENCE` 与 `UNCALIBRATED` 正交**：前者是概率低，后者是概率性质；p=0.95 但 adapter=json 仍要 `UNCALIBRATED`，可同时出现。
6. **`NORMALIZED` 与 `LOSSY_DECODE` 只在对应动作发生时发**，不影响其他 code。

**渲染顺序**（固定，属于契约）：不同 code 按下行顺序拼接；同 code 多条按固定二级键排序
（`slot` → `pointer` → `failed` 首项 → `level` 的字典序；二级键缺省时按 params 序列化后的字典序），不按并发节点的完成顺序：

```text
ISOLATION_FALLBACK → REDACTED → SENSITIVE → TRUNCATED → SLOT_UNAVAILABLE
→ DEGRADED → LOW_CONFIDENCE → NORMALIZED → UNCALIBRATED → LOSSY_DECODE → SNIFFED
```

模板语法固定为 `[CODE] key1=value1 key2=value2 …`：字段顺序即上表顺序，字段间用单个
空格分隔；单值字段的值内不允许出现空格与 ` · `。列表参数按格式约定用 `,` 分隔，
逗号只出现在列表分隔位置。不放时间戳与随机 ID（§16.3 对策二）。
**模板、字段顺序、渲染顺序都是契约**：改动等同工具描述改动，按 §16.3 对策一
同步 `serverInfo.version`，否则历史消息字节变化会让前缀缓存全部失效。

**信息分级与预算**（本节 G0/G1/G2 是信息等级，与 §2.1 的成本阶梯 L0–L4 无关）：

| 级 | 内容 | 默认 |
|---|---|---|
| G0 必给 | 上表 11 个 code 的全部告知 | 总是 |
| G1 默认给 | 下一步依据：路径、行号、diff、exitCode | 总是（structuredContent 为主，文本只列关键项） |
| G2 按需 | 解释、候选、引用线索 | intent=structured 或配置开启 |

预算（设计目标，非实测）：

- 每条 notice ≤ 1 行；每条 notice 的 params ≤ 4 个；
- 常规返回 ≤ 3 条 notice；出现安全类（`ISOLATION_FALLBACK`、`REDACTED`、`SENSITIVE`）时 ≤ 6 条；
- 全部 notice 合计 ≤ 400 UTF-8 字节（约 100 token，按 4 字节/token 估算）。

**超预算合并算法**（确定性，实现按此顺序）：

1. **同 code 合并**：count 类求和（两处 REDACTED → `count=5`）；列表类合并去重；
   阈值类取最差值。
2. **按类优先级保留**：安全（`ISOLATION_FALLBACK`、`REDACTED`、`SENSITIVE`）> 完整性
   （`TRUNCATED`）> 可用性（`SLOT_UNAVAILABLE`、`DEGRADED`）> 认知
   （`LOW_CONFIDENCE`、`UNCALIBRATED`、`SNIFFED`、`NORMALIZED`、`LOSSY_DECODE`）。
3. **超 6 条时**：从认知类最低优先级开始整条省略；省略数量记入
   `meta.noticeOverflow`（usize，程序字段，不进 content；只在有省略时出现，
   0 值省略）。安全类与
   完整性类永不省略。
4. **仍超 400 字节时**：先缩短 `reason` / `pointer` 等字符串字段（取规则 ID、内容
   哈希前缀），不删 code。

「code 选择规则」保证同一事实不重复触发；最坏组合按上述算法压到 ≤ 6 条。

**敏感信息**：notice 与 content、structuredContent 一样可能被模型看到（§4.4），必须过 §8.11：

- `rule` 渲染脱敏后的 glob：绝对路径转工作区相对或 `<external>`；无法安全渲染时用规则 ID（`rule=rule-07`）；
- `reason` 只允许有限枚举：`retention_below_threshold` / `no_redactable_pattern` / `redact_optimal` / `capability_missing` / `upstream_no_vision` / `lossy_compression` / `compression_exceeded`；
- `REDACTED.count` 只给数量，不给类型与内容；
- 测试：配置绝对路径规则时，content 渲染与 structuredContent.notices 都不得出现用户名与绝对路径前缀。

**不套 notice 的三类**：

- **错误响应**——ToolError 的 fact / reason / nextSteps 三段（§1.4.5）是错误契约，保持原样。
- **操作事实**——快照路径、嗅探的 kind/mime/encoding 字段、edit 第 4 级语义对齐的候选位置走 structuredContent 的专门字段，文本只渲染必要说明。
- **启动与运维输出**——配置校验、`toolgate list` 的槽位状态不是模型返回，不适用 notice。

**适用边界**：凡经治理管线产生的结果都带 `notices`——MCP 工具返回与 §18.7 的 `govern` 响应；`structuredContent.notices` 为数组，空时省略该字段。JSON 字段一律 camelCase（与 `nextSteps` 一致），Rust 内部字段可 snake_case、序列化时转换。

**Rust 类型草案**：

```rust
/// 序列化为 {"code": "TRUNCATED", "params": {...}}；tag 判别式即 notice code。
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(tag = "code", content = "params",
       rename_all = "SCREAMING_SNAKE_CASE",
       rename_all_fields = "camelCase")]
pub enum Notice {
    Truncated { kept: u64, total: u64, unit: TruncUnit, #[serde(default, skip_serializing_if = "Option::is_none")] pointer: Option<String> },
    Degraded { requested: OutputShape, actual: DegradedActual, reason: DegradeReason, #[serde(default, skip_serializing_if = "Option::is_none")] slot: Option<String> },
    LowConfidence { p: f32, views: ConfidenceViews },
    IsolationFallback { requested: IsolationLevel, actual: FallbackAction, rule: String, reason: IsolationReason },
    SlotUnavailable { slot: String, fallback: SlotFallback },
    Redacted { count: u32, retention: f32 },
    Normalized { level: NormalizeLevel, kind: NormalizeKind },
    Uncalibrated { slot: String, adapter: AdapterKind },
    LossyDecode { encoding: String, replacements: u32 },
    Sniffed { ext: String, actual: SniffActual },
}

// 新增变体必须补 match（无 _ 通配）。code() 供程序分支；序列化 tag 由 serde 属性生成。
// code() 的字符串必须与 serde tag 完全一致，由 §15.7 的枚举测试锁定。
// Option 字段已用 skip_serializing_if 省略空值，与「空值字段省略」一致。
impl Notice {
    pub fn code(&self) -> &'static str {
        match self {
            Notice::Truncated { .. } => "TRUNCATED",
            Notice::Degraded { .. } => "DEGRADED",
            Notice::LowConfidence { .. } => "LOW_CONFIDENCE",
            Notice::IsolationFallback { .. } => "ISOLATION_FALLBACK",
            Notice::SlotUnavailable { .. } => "SLOT_UNAVAILABLE",
            Notice::Redacted { .. } => "REDACTED",
            Notice::Normalized { .. } => "NORMALIZED",
            Notice::Uncalibrated { .. } => "UNCALIBRATED",
            Notice::LossyDecode { .. } => "LOSSY_DECODE",
            Notice::Sniffed { .. } => "SNIFFED",
        }
    }
}

// 值枚举一律 snake_case 序列化，与模板渲染一致（unit=lines，不是 LINES）。
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum TruncUnit { Lines, Bytes }

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum OutputShape { Raw, Summary, Structured, Visual, None }

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum DegradedActual { Raw, Summary, Structured, Visual, None, Metadata, Compressed }

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum IsolationLevel { None, Understand, Summary, Raw }

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum FallbackAction { Redact, Structure, Summarize, Refuse }

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum SlotFallback { Metadata, Text, Mechanical, Loading }

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum AdapterKind { Native, Json, Text, Ordered }

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum SniffActual { Binary, Text, Png, Jpeg, Gif, Webp, Pdf, Zip, Gzip, Other }

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum ConfidenceViews { Both, Local }

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum NormalizeKind { Eol, Whitespace }

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum NormalizeLevel {
    #[serde(rename = "2")] L2,
    #[serde(rename = "3")] L3,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum DegradeReason { CapabilityMissing, UpstreamNoVision, LatencyOverBudget, LossyCompression, CompressionExceeded }

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum IsolationReason { RetentionBelowThreshold, NoRedactablePattern, RedactOptimal }
```

序列化示例：`{"code":"TRUNCATED","params":{"kept":2000,"total":8734,"unit":"lines","pointer":"spill://ab12cd34"}}`。

`match` 不写 `_` 通配分支——新增 code 即编译失败（§15.7）。

**测试层**（§15.7）：枚举穷举（新增即编译失败）、渲染快照（每个 code 一组固定
params → 完整期望字节，另加多 notice 组合用例）、告知完备性（10 个 code 每个至少
一条路径断言 content 含对应 code）、预算（10 code 全触发按合并算法 ≤6 条、≤400 字节）、
敏感信息（绝对路径规则下不出现用户名）。

### 4.5 输出形态的单一决策表

输出形态由六处规则共同决定：intent（§4.2）、隔离策略（§8.4）、回退链（§8.6）、置信度门控（§7.2）、上游能力（§18.1b）、槽位可用性（§3.8）。分散在各章靠文字保持一致，改一处容易漏另一处。

v2 把它们收敛成一个纯函数，各章只引用它：

```rust
/// 五档 intent（§4.2）。v2 首版默认 raw，auto 需显式开启。
pub enum Intent { Raw, Summary, Structured, Visual, Auto }

/// 四档隔离强度（§8.1），按「暴露量」递增：None 最严、Raw 最宽。
/// 数值越小暴露越少，min() 即取更严的一档。
pub enum ExposureLevel { None, Understand, Summary, Raw }

/// 四级回退动作（§8.6），信息量从多到少。
pub enum FallbackStep { Redact, Structure, Summarize, Refuse }

pub struct PathRule { pub glob: String, pub level: ExposureLevel }

pub struct ContentRule { pub kind: ContentRuleKind, pub level: FallbackStep }
pub enum ContentRuleKind {
    SecretPattern, SensitivePathMention, PathUsername, HostMetadata, HighEntropy,
}

/// 上游能力（§18.1b）：每次调用传入，不是启动声明。
pub struct UpstreamCapability { pub vision: bool }

pub struct DecisionInput {
    pub intent: Intent,
    pub isolation_default: ExposureLevel,
    pub max_exposure: ExposureLevel,
    /// §8.5 的 fallback_chain；原文字签名漏列，导致 §4.5:414/420 无输入可依。
    pub fallback_chain: Vec<FallbackStep>,
    /// §8.5 的 redact_min_retention，只用于决定是否跳过 redact 级。
    pub redact_min_retention: f32,
    pub path_rule: Option<PathRule>,
    pub content_rules: Vec<ContentRule>,
    pub upstream_capability: UpstreamCapability,
    /// 该输出依赖的槽位是否可用（§3.8）。
    pub slot_available: bool,
}

pub struct DecisionOutput {
    pub output_shape: OutputShape,
    /// 需依次尝试的回退动作，是 input.fallback_chain 的保序子序列；空表示不回退。
    pub fallback_steps: Vec<FallbackStep>,
    /// 只含 ISOLATION_FALLBACK / DEGRADED（能力·上游）/ SLOT_UNAVAILABLE 三类。
    pub decision_notices: Vec<Notice>,
    /// 程序字段：requested_level / effective_level / isolation_refused。
    pub meta: serde_json::Value,
}

pub fn decide_output(input: &DecisionInput) -> DecisionOutput
```

**输入补齐说明**：原文字签名只列 7 项，但 §4.5:414 的决策规则、§4.5:420 的属性测试、
§8.5/§8.6 的四级回退都依赖 `fallback_chain` 与 `redact_min_retention`。二者显式纳入
`DecisionInput`；`redact_min_retention` 只参与决策层「是否跳过 redact 级」，保留率的
实际观测值由执行层按 §8.6 判定，决策函数不读执行结果（§4.4b:144）。

**三段决策判据**（前一段是硬约束，顺序固定）：

1. **能力**：`intent=visual`（或输出含 image block）且 `upstream_capability.vision=false`
   → 降级为文本、产 `DEGRADED`（requested=visual、actual=none、reason=upstream_no_vision、slot=vision）。
2. **隔离**：意图档位按 §4.2 映射（raw/visual→Raw、summary→Summary、structured→Understand、
   auto→isolation_default）；有效上限 = min(max_exposure, path_rule.level)；`content_rules`
   非空或意图档位 > 有效上限 → 按 `fallback_chain` 依次尝试，产 `ISOLATION_FALLBACK`；
   有效上限为 None → `meta.isolation_refused=true`，由调用方返回 `IsolationRefused` 错误（§8.11）。
3. **意图**：在允许范围内取 output_shape；`slot_available=false` → 产 `SLOT_UNAVAILABLE`
   并退回机械路径（§3.8）。`output_shape` 对应档位不得超过 `max_exposure`。

**属性测试**（§15.7）：随机 `DecisionInput`，断言 ① output_shape 不超出 max_exposure；
② `fallback_steps` 每一步都在 `fallback_chain` 中且保序；③ decision_notices 只含三类；
④ 同输入两次调用字节一致（§16.3）。

`decision_notices` 只含本阶段能决定的形态类告知（`ISOLATION_FALLBACK` / `DEGRADED`
（能力/上游）/ `SLOT_UNAVAILABLE`，**不含压缩熔断**）；执行后由
`finalize_notices(decision_notices, observed)` 合并执行事实类告知（截断量、脱敏计数、
decide 概率、压缩熔断等，§4.4b「生成分两段」）。最终 `notices` 是唯一事实源：content 由它按
固定模板渲染，structuredContent 原样携带；`meta` 只放程序字段。

**`finalize_notices` 契约**（与 `decide_output` 一样是纯函数）：

```rust
pub struct ObservedFacts {
    pub truncation: Option<TruncationObs>,      // kept / total / unit / pointer
    pub redaction: Option<RedactionObs>,        // count / retention
    pub confidence: Option<ConfidenceObs>,      // p / views
    pub normalization: Option<NormalizationObs>,// level / kind
    pub decode: Option<DecodeObs>,              // encoding / replacements
    pub sniff: Option<SniffObs>,                // ext / actual
    pub compression: Option<CompressionObs>,    // requested / actual / slot / reason
    pub uncalibrated: Option<UncalibratedObs>,  // slot / adapter
}

pub struct TruncationObs { pub kept: u64, pub total: u64, pub unit: TruncUnit, pub pointer: Option<String> }
pub struct RedactionObs { pub count: u32, pub retention: f32 }
pub struct ConfidenceObs { pub p: f32, pub views: ConfidenceViews }
pub struct NormalizationObs { pub level: NormalizeLevel, pub kind: NormalizeKind }
pub struct DecodeObs { pub encoding: String, pub replacements: u32 }
pub struct SniffObs { pub ext: String, pub actual: SniffActual }
// lossy：requested=visual actual=compressed reason=lossy_compression slot=vision
// cutoff：requested=visual actual=metadata reason=compression_exceeded slot=vision
pub struct CompressionObs { pub requested: OutputShape, pub actual: DegradedActual, pub slot: Option<String>, pub reason: DegradeReason }
pub struct UncalibratedObs { pub slot: String, pub adapter: AdapterKind }

pub fn finalize_notices(decision: Vec<Notice>, observed: &ObservedFacts) -> Vec<Notice>
```

- 只产执行事实类 code（`TRUNCATED` / `REDACTED` / `LOW_CONFIDENCE` /
  `NORMALIZED` / `UNCALIBRATED` / `LOSSY_DECODE` / `SNIFFED` / `DEGRADED` 压缩熔断），
  不产 `ISOLATION_FALLBACK` / `SLOT_UNAVAILABLE`；
- 输出按 §4.4b 的固定渲染顺序排序；同输入同输出。

决策顺序固定为三段，前一段是硬约束：

1. **能力**：上游收不收这种 content block。不收 → 降级为文本并按 §4.4b 的 `DEGRADED` 告知。
2. **隔离**：路径规则与内容级规则取交集，再与 `max_exposure` 取 min。超出 → 按 `fallback_chain` 逐级尝试。
3. **意图**：intent 只在允许范围内表达偏好，不能突破前两段。

**实现要求**：这个函数必须是纯函数，配属性测试：

- 对任意输入，`output_shape` 不超出 `max_exposure`；
- 对任意输入，`fallback` 链的每一步都在配置里；
- `decision_notices` 只含 §4.4b 决策阶段的三类 code；
- 同样的输入两次调用得到同样的输出（§16.3 对策二）。

各章不再重复写「什么情况下给什么」，只引用本节。

### 4.6 工具的克制、能力与诚实

工具层的设计标准是三条同时成立：**克制**（能不要的就不要）、**能力**（留下的要够用）、**诚实**（不误导、不编造）。缺任何一条，工具层都会从助力变成负担。

**克制：新增工具是最后手段。**

- 工具表是上下文的一部分：多一个工具，所有会话都多背一条声明，模型的选择面也多一个歧义来源（§4.0 的缓存前缀稳定）。
- 判断顺序：现有工具能否组合完成？→ 能否给现有工具加参数完成？→ 都不行才新增。低频动作不做成工具。
- 已明确不做：批量入口（§21.4）、自动回滚工具（§20.3）、编排入口（§21.7）、插件 SDK（§18.2）。
- 不为单个用例、单个项目、单个模型加工具；不为「以后可能」预留工具。

**能力：克制不等于残缺。**

- 留下的工具必须覆盖高频动作的完整表达：bash 的线性管道与重定向（§5.1）、edit 的三种寻址模式（§5.4）、read 的 offset 续读与 URL 兜底（§5.3）、task 的增量读取（§5.5）。
- 判据：如果模型为了完成一个常见动作必须绕路（多次调用、手工拼装、把中间结果塞回上下文），说明工具能力不足——补能力，不加工具。
- 「能力」是表达力，不是包办：工具给事实与操作，不替模型做判断（§5.7、§21.4）。

**诚实：不编造、不假装、不越界。**

工具的输出只有两种合法来源：**观测到的**（文件内容、命令输出、检索结果）与**可溯源的**（对观测内容的确定性加工，如截断、哈希、diff）。除此之外一律不生成。

| 约束 | 具体要求 | 违反的后果 |
|---|---|---|
| 不编造 | read 读不到就报错；search 无命中就返回空；bash 无输出就返回空。摘要 / OCR / 结构化输出必须能回退到原文（spill 指针、图片路径、行号范围） | 模型基于假内容做决策，且无法发现 |
| 不假装成功 | write 未落盘不报成功；edit 未匹配不猜位置；超时按 `Timeout` 错误码；任何部分完成都明确标注已完成项，不把部分结果当完整 | 调用方以为状态已变，后续全部踩空 |
| 不越界猜测 | edit 多处命中即拒绝（§5.4）；路径不存在不猜相似路径；不自动回滚（§20.3）；不自动装依赖 | 工具替模型做了它没授权的决定 |
| 降级必须告知 | 截断、摘要、脱敏、隔离回退、槽位不可用时，模型可见通道必须有说明（§4.4b） | 模型拿到降级产物却当成原文 |
| 错误归因准确 | `PathNotFound` / `EncodingInvalid` / `AnchorMismatch` / `EditAmbiguous` / `Timeout` / `OutputLimit` / `SlotUnavailable` / `IsolationRefused` 各自对应真实原因，不用 `Failed` 兜底一切（§1.4.5） | 模型按错误码分支时走错路径 |

**边界必须写进工具描述。** 每个 ToolSpec 的 description 固定三段：**能做什么**、**不能做什么**、**什么时候该用别的工具**。模型看不到的边界等于不存在——它会产生「工具应该能做」的幻觉，然后反复试错。

以 read 为例：

```text
description:
  能做什么：读取本地文件内容；支持 offset/limit 分段读取；图片按压缩流水线返回或转译。
  不能做什么：不解析网页 JS（需要 fetch.provider，且默认 none）；不跟随符号链接；不返回未落盘的临时内容。
  什么时候用别的工具：按内容查找用 search；修改用 edit/write；整段重写用 write。
```

### 4.7 面向 agent：高密度、高效

**前提：工具的唯一消费者是 agent（模型与调用它的程序），不是人。** 这条前提决定返回内容与工具描述的形状，也解释了一批「对人不够友好」的设计：

- 不写「成功」「已完成」这类寒暄——成功用状态码与事实表达；
- 不写「请稍后重试」这类给人看的文案——错误给 `code + fact + reason + next_steps`（§1.4.4）；
- 不做对齐、缩进美化、emoji、彩色输出——结构化字段与最短文本；
- 不复述调用参数——模型知道自己传了什么，重复只浪费 token；
- 不把建议包装成指令（§5.7）——给事实与候选，判断留给模型。

**高密度不是把原始内容全塞回去**：恰恰相反，工具的价值就是挡掉不需要上传的东西（§1.3）。高密度指**保留下来的每一条都对下一步有用**。两条通道（§4.4）都按本节风格。

| 要求 | 具体做法 | 反例 |
|---|---|---|
| 每行都有用 | 路径、行号、字节数、错误码、diff 等可执行事实 | 「操作成功」「共处理 3 项」这类摘要性废话 |
| 结构优先 | 能用 JSON 字段表达的不用散文；schema 在 outputSchema 声明 | 一段自然语言描述检索结果 |
| 可核对 | 摘要附原文指针、结构化输出附行号范围、diff 附 hunk 头 | 只有摘要没有原文位置 |

**数值精度**：概率类数值对外只保留两位小数——`0.7084448…` 与 `0.71` 对调用方是同一个信息，多出来的位数只是 token。这条覆盖两条通道：`structuredContent` 的 `confidence`（出口统一收敛）与 notice 文本里的 `p`（渲染口径见 §4.4b 的格式约定）。CLI 调试输出（`toolgate decide` 打印的概率）同样两位小数并共用同一个格式化函数：同一事实不因通道不同而精度不同，`--json` 的数字因此可以直接比对。

**决策已是有限档位时给档位，不给概率**：`need_pixels` 的判定结果是三档之一（§10.1），`structuredContent` 就写 `original_only` / `both` / `structured_only`，不写概率。理由不是省两位小数，而是**概率会诱使调用方自己重新切阈值**，与工具层的决策打架——档位是决策结果，概率只是决策输入，只给输入等于把判断推回去。判定缺位时省略字段，不写 `unknown`：那会让调用方以为存在第四种结果。

**高效的两条要求**：

- **token 效率**：截断、summary、落盘指针是默认路径（§9）；工具描述控制在一屏内，不写长文档（§4.6 的三段式要短）。
- **往返效率**：一次调用能完成的动作不拆成多次；但也不为了少一次调用而造第 7 个工具（§4.6）。

**边界**：CLI（`toolgate list` / `toolgate call`）是调试与脚本入口，输出可以偏人类可读；MCP 工具的返回必须按本节执行，两者不共享输出格式。

**自检**：把返回内容交给一个不了解上下文的模型，它能否直接拿来做下一步？若需要人再读一遍才能理解，就是设计错了。

**多语言（§18.1b）**：模型可见文案不写死中文，按请求级
`_meta.toolgate/language` 或环境语言切换，缺省回退源语言中文。
源语言中文只作为 msgid 与译文表的键；工具描述、入参 schema 描述与错误
三段式按语言返回，程序字段（code、路径、数值）保持语言无关。

### 4.8 理解链路：有界多步与并发

**问题**：§4.1 的理解层原来是一条固定流水线（判定 → 摘要）。本地小模型已经具备多步执行能力（§1.2 的 2026 年事实：端侧 2B 级模型在 agentic 基准上可排同规模前列），固定流水线浪费了「读一段 → 判断够不够 → 再读」和「多个独立片段同时处理」这两类能力。

**决策：理解层升级为「有界多步 DAG」，边界四条**：

| 约束 | 内容 |
|---|---|
| 动作菜单固定 | 只能从下表 8 个动作里选，不生成新动作 |
| 单次调用内 | 不跨 tools/call、不写文件、不产生持久状态 |
| 有界预算 | 最多 5 步、并发 ≤ 4；链级 token / 步数按本节边界，不设延迟门控 |
| 每步可验证 | 每步输出有 schema 或机械校验；不达标降级到机械路径 |

**动作菜单**：

| 动作 | 作用 | 归属 |
|---|---|---|
| `recall` | 从落盘副本召回指定片段（行号 / 正则 / 头部 / 尾部） | 确定性 |
| `decide` | 在命名问题上给概率答案 | decide 槽位 |
| `classify` | 对文本打敏感度 / 类别标签（只产出标签事实，不改变原文） | classify 槽位 |
| `summarize` | 有界文本 → 摘要或 schema 化 | summarize 槽位 |
| `vision` | 图片 → 文字 / 坐标 | vision 槽位 |
| `rerank` | 候选排序 | rerank 槽位 |
| `self_check` | 对前一步输出做达标判定（如「摘要是否覆盖全部失败项」） | decide 槽位 |
| `merge` | 合并并行分支的结果（去重、按 schema 汇总） | 确定性 |

**链是 DAG，不是串行脚本。** 节点之间只有数据依赖才连边；没有依赖的节点并发执行：

```text
bash 失败（输出 8000 行）：
  recall(错误行) ──┬── decide(has_error)
  recall(尾部)   ──┘        │
                            ▼
                    summarize(失败摘要) → self_check(覆盖失败项？)
                            │
              不覆盖：recall(补充片段) → summarize(二次) → merge
              覆盖：返回摘要 + 指针
```

多个召回片段各自 summarize 的分支是并行的；`self_check` 依赖 summarize，只能串行。

**并发模型**（与 §18.1、§18.5 一致）：

- 未依赖的链节点并发提交，调度器是工具层内的 tokio task；
- **同一模型实例内部仍串行**（§18.1 的有界队列），不同模型实例可并行——「decide + vision」能同时跑，「两个 summarize」仍排队；
- 链级并发上限 `chain.max_parallel`（默认 `min(4, CPU/2)`），超过排队，不无界展开；
- **并发不改变结果**：同输入同输出的约束（§16.3）按节点级落实——节点输出确定、DAG 拓扑固定，并发只影响延迟。

**下一步由谁选**：由 `decide` 槽位在菜单内做受限选择（答案空间 = 8 个动作），或由机械条件触发（如「召回窗口含错误行 → 必须 summarize」）。**不做自由生成式规划**——那会跨出 §1.2 的答案空间判据。

**与 §13.4 批量的关系**：§13.4 方案 B 是「一次 decide 槽位调用问多个问题」（模型内批量）；本节是「多个链节点并行提交」（调度层并发）。两者互补：批量减少调用次数，并发缩短关键路径。

**与 §21.7 本地 agent 的区别**（关键边界）：

| 维度 | 理解链（本节） | 本地 agent（§21.7） |
|---|---|---|
| 范围 | 单次工具调用内 | 跨工具调用的任务循环 |
| 目标 | 把本次结果定形 | 完成一个任务 |
| 规划 | 固定动作菜单内的受限选择 | 开放式多步规划 |
| 状态 | 无（链结束即丢弃） | 有（任务状态、checkpoint） |
| 副作用 | 无（只读） | 有（写文件、跑命令） |
| 失败 | 降级到机械路径，按 §4.4b 的 `DEGRADED` 告知 | 任务层 partial / 升级云端 |

**为什么不是「内置 agent 循环」**：链的目标不是完成任务，是「这次工具结果怎么给」；动作全只读、菜单固定、单次调用内、无状态。§21.7.3 拒绝的是有目标、有状态、有副作用的 agent 循环，不是单次调用内的多步理解。

**什么时候进链**：输出 > 500 token、图片、或失败归因需要多步；小输出单步判定即可（§7.1）。超预算按本节边界收束（给已完成步骤 + 机械结果），不中途扩展预算。

---

## 5. 各工具的智能路径

每个工具写清楚：L0 做什么、本地模型在哪一步介入、协商层给什么。
**不是所有工具都需要全部三级**——不需要的就不写，这是「不越需求」。
本节描述**最小路径**；实际执行可按 §4.8 组成有界多步链（补读、自检、并行召回），
链不改变这里的分层职责。

### 5.1 bash —— 收益最大的一个

```text
L0 执行 → 拿 stdout/stderr
L0 双阈值截断（2000 行 / 50KB）+ 完整输出落盘
L0.5 语义去重（embedding；只作用于 decide/summarize 的模型输入）
L1 decide 判定（一次前向，多个问题）：
     has_error?          概率
     error_kind?         编译 / 测试 / 权限 / 网络 / 资源 / 未知
     severity?           阻断 / 警告 / 噪音
     needs_full_context? 是/否
L1.5 敏感度标注（classify；只发 SENSITIVE 告知，不改原文）
L2 summarize 摘要（仅当 decide 判定 has_error 且 severity=阻断）：
     失败根因 + 涉及的文件与测试 + 建议排查方向
L0 协商：
     raw        → 截断后的原文 + 落盘指针
     summary    → decide 判定 + summarize 摘要 + 落盘指针
     structured → JSON：{exitCode, errorKind, severity, failedItems[], spillPath}
     auto       → 输出 < 500 token 给原文；否则走 summary
```

为什么 bash 收益最大：一次失败的测试套件输出常在 3000-8000 行，
其中真正有信息量的是最后 50 行加上中间散落的断言失败——这与模型需要的信息分布一致：关键是尾部与散落的失败点。

**decide 的判定问题一次问完**，这是它「任意数量命名问题」特性的直接用法。
（上面图里画的是四个**语义问题**，落到槽位契约上会分解成九个**二元问题**，见下。）

#### L1 判定的落地口径：多值语义分解成二元问题

上面图里的四个问题是**多值语义**（error_kind 六选一、severity 三选一），但 §3.2 的
decide 槽位契约是**二元形状**——「每个问题一个概率」，没有类型维度。systemone 的
`choice` 类型映射到 classify 槽位（§3.10），decide 路径仍只用 `noul`。
所以实现上把多值语义**分解成若干二元问题**，一次请求问完：

| 组 | 问题 | 数 |
|---|---|---|
| 是否有错 | `has_error` | 1 |
| 错误种类 | 编译 / 测试 / 权限 / 网络 / 资源，各问「是不是这类？」 | 5 |
| 严重程度 | 「阻断？」、「噪音？」 | 2 |
| 是否需要全文 | `needs_full_context` | 1 |
| | **合计** | **9** |

九问一次调用，不是九次——这正是 §13.4 方案 A 里「systemone 一次前向读出全部问题
的概率」的能力（每个问题一个独立概率，共享同一次 state 读取）。

**归并规则**（把九个二元概率合回多值字段）：

| 字段 | 规则 |
|---|---|
| `errorKind` | `has_error` < 0.5 → `"none"`；否则取五个候选里概率最大者，最大者 < 0.5 → `"unknown"` |
| `severity` | 阻断概率 ≥ 0.5 → `"blocking"`；否则噪音概率 ≥ 0.5 → `"noise"`；否则 `"warning"` |
| `needsFullContext` | 该问题概率 ≥ 0.5 为真 |
| `failedItems` | **本轮留空**（语义未定义，见下） |

**本次结论的置信度**：min(`has_error` 的概率, **被选中** kind 类别概率)；判定为「无错」
（`errorKind` = `"none"`）时取 `has_error` 概率本身。「被选中」指归并规则命中的那一类
（argmax）。为什么不是所有问题的最小值——多选问题的非答案概率天然低，取全量最小值
会把任何判定压到低置信档，门控形同虚设（§7.3 的 confidence 来源）。

**为什么 `failedItems` 留空而不是猜**：它要表达「哪些具体条目失败了」，需要从输出
里定位到条目级标识（测试名、错误码、行号），这依赖命令的输出格式，跨命令没有统一
的抽法。**语义未定义就留空，不编造**——填一个看似合理但无定义的数组，比留空更糟：
调用方会以为它有含义。

**判定的 state 是有界窗口，不是全文**：命令输出经 L0 双阈值截断后，取**最后 8000
字符**作为判定 state（§6.4 的 prefill 预算口径，不是整份输出）。取尾部是因为错误
信息通常在末尾；但这是有界窗口，**错误若在被截掉的前部就判不出来**——此时的表现
是概率偏低，不是假阳性。

**触发条件**：

| intent | 是否判定 |
|---|---|
| `raw` | **不判定**（§7.1 硬性不介入：调用方明确要原文，加工即违背契约） |
| `summary` / `structured` / `auto` | 输出 **≥ 500 token** 才判定（§9.2 的触发门槛；低于门槛由 `exitCode` 机械判断） |

**判定不可用时工具照常工作**：槽位未绑定、`external-protocols` feature 未开启、
或调用失败 → 该次判定缺失，工具按 L0 结果继续工作，按 §3.8 降级并按 §4.4b 发
`SLOT_UNAVAILABLE` 告知（`reason=call_failed` 或槽位层原因），`meta.confidence` 为 `null`。**不判错、不判类型**——
把「判定不了」记成「判定为无错」是错的。

#### L2 摘要的落地口径

`intent=summary` 与 `auto`（输出 ≥ 500 token）在 L1 判定之后尝试 L2 摘要：

- 摘要输入是**有界窗口**：输出尾部 8000 字符（与判定 state 同源，§6.4）加上
  L1 判定事实（`errorKind` / `severity`），不把整份输出喂给生成模型；
- 摘要成功时 `content` 只给摘要文本，判定事实在 `meta.decision`、完整输出在
  `meta.spill`，**不重复上传原文**；`meta.summary` 同时给程序可读通道；
- 摘要不可用（槽位未绑定 / feature 未开 / 调用失败）按 §3.8 降级：给原文 +
  `SLOT_UNAVAILABLE` 告知（调用失败时 `reason=call_failed`），工具照常工作（§7.3），**不编造摘要**。

上图中「has_error 且 severity=阻断」是 §9.2 对 L2 节点**自动展开**的触发判据；
本轮的协商层接线按 intent 进入摘要路径，理解链的自动调度（§4.8）仍待落地。

#### L1.5 输出敏感度标注与模型输入去重

- **敏感度标注**：`intent != raw` 且 body ≥ 500 token 时，classify 与机械正则
  （私钥块、API key 赋值、邮箱、手机号、身份证号）同时判定，取更高等级
  （secret > personal > normal）；classify 判 `normal` 不覆盖机械命中的
  `secret` / `personal`——机械是下限，防模型漏判。最终等级命中前两者发
  `SENSITIVE`，`normal` 不发。**只标注不脱敏**——脱敏仍由 §8 的规则决定，不改变
  返回内容与隔离档位。`meta.sensitivity.source` 记最终来源（classify / mechanical）。
  输入取头尾各 2000 字符（合计 ≤ 4000），不喂整份输出。
- **失败降级**：classify 未绑定或调用失败时只用机械正则，
  `meta.sensitivity.source=mechanical`；**不发
  `SLOT_UNAVAILABLE`**——这是附加标签，不改变返回形态，不构成 §4.4b 意义上的降级
  （显式例外，避免每次 bash 都多一条噪声）。
- **模型输入去重**：送给 decide / summarize 的窗口（≤ 8000 字符、50–200 行）先做
  精确行去重，再用 embedding 余弦聚类（阈值 0.92），每簇保留首次出现的行；
  **只作用于模型输入，不回写 content 原文**。窗口 > 200 行或 embedding 不可用则
  跳过。`meta.dedup` 给 `inputLines` / `outputLines` / `batches`（程序字段）。
  只读、单次调用内、无索引、无持久状态（§4.8 边界）。
- 两个动作与 L1 无数据依赖，可并发提交；同模型实例内部仍按 §18.1 有界队列串行。

**结构化字段在判定不可用时的取值**：`errorKind` = `"unknown"`、`severity` =
`"unknown"`、`failedItems` = 空数组（`exitCode` 与 `spillPath` 照常给真实值，
它们不依赖模型）。**这与「判定为无错」是两回事**：`errorKind` 的 `"none"` 是
判定**得出**「没有错误」，`"unknown"` 是**没有判定**。

**管道与重定向：保留安全，消除摩擦**

只接受 binary + args 消除了 shell 注入面，但也带来一个摩擦：管道要拆成
多次调用，中间结果要么落盘要么进上下文。

解法是**结构化地表达管道**，每段仍然是 execve，没有 shell 解析层：

```json
{
  "pipeline": [
    { "binary": "rg", "args": ["TODO", "src/"] },
    { "binary": "head", "args": ["-n", "20"] }
  ],
  "stdout_to": "/tmp/todos.txt"
}
```

- pipeline 的每一段用 OS 管道连接，等价于 rg ... | head -n 20；
- stdout_to / stderr_to 把最终输出重定向到文件，返回文件路径而不占上下文；
- 单命令仍可用 binary + args 简写；
- **只支持线性管道**（a | b | c），不支持 && / || / ; 这类顺序控制——
  那等于引入一门小语言，而拆成多次调用反而让每步的输出都能被模型看到。

**这比放开 shell 字符串安全**：每段的参数是独立数组，没有词分割、没有
通配符展开、没有变量替换。攻击面与单命令相同。

**也比拆多次调用高效**：一次往返完成，中间结果不经过上下文。

**第三种形态：脚本**

前两种都是「调用已有程序」。但云端模型最强的能力之一是**写程序一次完成复杂
任务**——批量重命名、格式转换、条件分支、数据提取。这类任务单命令表达不了，
拆成多次工具调用又浪费往返。

现状：bash 拒绝 shell 解释器（§17.2），所以「bash -c "..."」走不通。模型要做
这件事，得走「write 写脚本 → bash 用解释器执行」两步。

这个摩擦是真的，但换来的东西也是真的：

| 方案 | 往返次数 | 注入面 | 可审计 |
|---|---|---|---|
| 放开 shell 字符串入口 | 1 | 大：引号、通配符、变量展开都是注入点 | 差：脚本在请求里，不留痕 |
| write + 执行 | 2 | 小：脚本是文件，内容确定 | 好：文件在磁盘上，可重跑、可进版本控制 |
| script 入参（落临时文件再执行） | 1 | 同 write + 执行 | 好 |

**倾向第三种，但现在不做**：它与「write + 执行」的实现完全等价，只是把两步
合并省一次往返；而它引入一个新的入参形态（script + interpreter），这个形态与
「解释器 + -c」的能力完全重叠。等真实调用里「写脚本」占比够高再加。

**所以当下的正路是**：write 写脚本 → bash 用解释器执行。两步，但脚本留在磁盘
上可审计、可重跑、可进版本控制——这比把一段脚本塞进请求里更好，不只是「更安全」。

**一个必须写明的现状**：reject_shell 只拒 shell 解释器，不拒 python3 / node。
所以「python3 -c "..."」现在就能跑任意脚本，风险与 shell 字符串同级。这不是
漏洞，是「解释器白名单没做」的结果——要么补白名单（但那会堵掉正经的脚本用法），
要么承认它是有意为之：脚本由模型自己写，风险由 §17 的边界承担。这一条列进 §15.4。

### 5.2 search —— pattern 匹配 + 语义重排（rerank 优先，embedding 退路）

```text
L0 匹配（content 用正则；files 用文件名子串 / 通配符），拿全部命中
L0 若命中数 ≤ 20，直接返回（没必要语义重排）
L1 语义重排（仅当命中数 > 20、提供 query、且 content 模式）：
     候选 = pattern 命中的行；语义查询 = 调用方给的 query（自然语言）
     优先 rerank 槽位：按「候选与 query 意图的相关性」打分，返回 top-k 顺序
     否则若 embedding 可用：对 query 与每个候选行各取向量，按余弦相似度排序
       （有界：只算本次命中行、无索引、无持久状态；候选 ≤200、单条 ≤4096 字符、
        每批 ≤32 条，批间维度不一致即整体放弃）
     两者都不可用：按命中顺序返回
L0 协商：按 intent 返回全部 / top-k / 结构化列表
```

设计要点：

- **`pattern` 与 `query` 分工，不复用同一字段**。`pattern` 决定「哪些行算命中」：
  content 模式是正则表达式（不做通配或子串推断），files 模式是文件名子串 /
  通配符（纯机械 L0）；`query` 决定「命中后怎么排」（自然语言的语义查询，喂给
  重排槽位）。旧口径让 `query` 同时充当正则与语义查询，既不能写通配，又会在
  关键词查询时把关键词本身当成「语义」喂给 ranker，产生与预期反向的排序
  （实测见附件 E）。本次修正为两字段分离：`pattern` 必填、`query` 可选（仅在
  需要语义重排时提供）。旧输入里的 `query` 正则写法要迁到 `pattern`，否则会按
  字面正则失败（迁移说明见 [upgrade-v1-to-v2.md](../upgrade-v1-to-v2.md)）。
- **只在候选多时才重排**。20 条以内的命中，重排延迟反而超过收益。阈值作用于
  本次返回的候选数（受 `max_results` 上限 200 约束），不是遍历到的全部匹配数。
- **files 模式不重排**：命中是文件路径，语义相似度对路径几乎没有意义；
  files 模式只用 `pattern` 做文件名子串 / 通配匹配。

#### 语义重排的接线落地口径

工具层按「绑定 + 能力矩阵非 Unavailable」判定槽位是否可用，不硬编码；
重排优先级是 rerank > embedding > 命中顺序：

- 命中数 > 20、content 模式、rerank 可用且有 `query` → 真实调用 rerank 槽位，按
  `results`（已按 score 降序、覆盖全部命中、index 指向原始位置，§3.2）重排命中
  行后，再做确定性截断；返回、落盘与结构化事实共用同一份顺序；
- rerank 不可用而 embedding 可用且有 `query` → 对 `query` 与候选行分批取向量
  （每批 ≤32 条、候选 ≤200、单条 ≤4096 字符，§3.2 embedding 契约；批间维度
  不一致即整体放弃），按余弦相似度降序排序后截断；`meta.rerank` 标 `embedding`
  以示区分（不是 rerank 的 `applied`），`meta.embedding` 给出
  `candidates` / `textsTruncated` / `batches` 统计；
- 两个槽位都不可用、或调用失败 → 降级为命中顺序返回，按 §4.4b 发
  `SLOT_UNAVAILABLE` 告知（调用失败时 `reason=call_failed`），**不谎报 applied**（§4.6）；
- 分数只用作顺序，不作为置信度门控输入。通用 LLM 走 `ordered` 时是自报值、
  非校准（§18.1「受限」，按 §3.10 标 `UNCALIBRATED`）；`source: rerank` 的
  `relevance_score` 是专用 ranker 的相关性分数、embedding 的余弦是向量相似度，
  三者都是非校准量，但语义与告知口径不同（§18.1）。

#### 输出敏感度标注

content 模式、body ≥ 500 token 时对命中内容做 classify 敏感度标签（labels
`secret` / `personal` / `normal`）并与机械正则取更高等级（§5.1 同口径，机械为
下限），命中前两者发 `SENSITIVE`。与重排无数据依赖，可并发提交；classify 不可用时
只用机械正则并记 `meta.sensitivity.source=mechanical`，**不发
`SLOT_UNAVAILABLE`**（附加标签，不改变返回形态，与 §5.1 同口径）。只标注不脱敏；
`files` 模式只有路径，不做标注。

### 5.3 read —— 文本与图片两条路

**文本路径**

```text
L0 读文件、嗅探类型、截断
L1 decide 判定（样本有界：头 2000 + 尾 200 字符；问题集按 intent 分级）：
     file_kind?   代码 / 配置 / 日志 / 文档 / 数据        （恒问）
     sensitivity? 含密钥 / 含个人信息 / 普通              （恒问）
     best_view?   全文 / 结构 / 片段                       （仅 auto 问）
L1.5 摘要输入去重（embedding；只作用于 L2 输入，不回写原文）
L2 summarize 结构化（仅当 intent=structured 或 auto 且 best_view=结构）：
     代码 → 函数签名列表、TODO、导入
     配置 → 键值树
     日志 → 错误行与时间线
协商：raw 给原文；structured 给提取结果 + 行号范围
```

read 的 `sensitivity` 由 L1 decide 给出，**不再用 classify 重复判定**（同一事实
只发一条，§4.4b）；classify 在 read 的落点仅限 §5.1/§5.2/§5.5 的输出标注。
L2 摘要前对 `bounded`（≤ 8000 字符）先做与 §5.1 同口径的语义去重，失败用原窗口；
`meta.dedup` 统计，不改变 `content` 的原文与行号。

**判定样本与问题集都按 §6.4 有界化**。decide 的成本与「状态长度 × 问题数」成正比
（systemone 服务端逐问串行、不并发，附件 E.3 实测），因此：判定样本取「头 2000 +
尾 200 字符」而非整份（日志的失败信息常在尾部），中段以省略标记代替；问题集按 intent
分级——summary / structured 的输出形态已由 intent 定死，只问 file_kind + sensitivity
七问；只有 auto 需要 best_view 决定协商形态，问满十问。best_view 未判定时
`meta.bestView` 字段省略，不写 `full` 冒充（§4.6「不假装成功」）。摘要输入 `bounded`
（≤ 8000 字符）与判定样本是两回事，仍按去重后的有界窗口送 summarize。

**图片路径**

```text
L0 嗅探 MIME、解码
L1 decide 判定：
     image_kind?  UI 截图 / 文档照片 / 图表 / 照片 / 代码截图
     has_text?    是/否
     need_pixels? 概率 p（≥0.8 只给原图 / ≤0.2 只给结构化 / 中间两种都给，§10.1）
L2/L3 按 image_kind 分流：
     UI 截图  → VL 做 grounding，返回元素列表 + 坐标
     文档照片 → VL 做 OCR，返回文字 + 布局
     图表     → VL 描述趋势 + 关键数值
     代码截图 → VL 做 OCR，返回代码文本
协商：
     visual     → 压缩后的原图（走 v1 的压缩流水线）
     structured → 元素列表 / OCR 文本 / 描述
     auto       → 按 need_pixels 三档：只给原图 / 只给结构化 / 两种都给（§10.1）
```

**Web 路径（可选 provider，兜底）**

```text
L0 read({url}) 调 fetch.provider 取回：
     provider 负责 JS / 登录 / fake-IP / SSRF / 反爬 / 正文提取
     返回 content_type + body；按文本或图片路径继续
L1/L2/协商：与上面的文件路径完全同一套治理（截断、脱敏、spill、SLM、vision）
```

**主路径不是 read({url})**：客户端已有钩子时，agent 自己 fetch 后把结果交给
`govern`（§18.7）；`read({url})` 只是无钩子客户端的兜底，未配置
`fetch.provider` 时返回 `WEB_FETCH_UNAVAILABLE`，不会在后台偷偷联网。

**这一条是本架构最有价值的设计**：一张 UI 截图 800KB，base64 后约 1.1MB，
约 1500 token（视觉 token 另算）。而 VL 的 grounding 结果可能是
「登录按钮 (120,340,86,32)、用户名输入框 (40,200,320,40)…」约 300 token。

对「帮我改这个页面的按钮颜色」这类任务，坐标比原图更有用——它直接告诉
云端该改哪一行 CSS。

### 5.4 edit —— 三种寻址模式 + decide 槽位解释失败

**三种模式，按场景选**

| 模式 | 入参 | 适用 | 成本 |
|---|---|---|---|
| A 文本匹配（默认） | edits:[{oldText,newText}] | 改几行，原文短 | 回传原文，转义风险 |
| B 行锚点 | anchor:"12#f8a2" + newText | 大文件反复改少量行 | read 时多 7 字节/行 |
| C 扁平 patch | patch:"@@ -12,3 +12,5 @@ ..." | 整段重排、缩进调整 | 无需转义原文 |

**三种模式各自消掉一个具体故障**

| 故障 | 表现 | 消掉它的机制 |
|---|---|---|
| 转义摩擦 | 用 JSON 字符串传原文，引号、反斜杠、换行都要转义，一处错整条失败 | 模式 C：unified diff 用行前缀表达，原文不转义 |
| 行号漂移 | 文件被改过之后，模型记的行号与文件实际内容错位，改到错的行 | 模式 B：行号带内容哈希，不符即拒绝；模式 C：hunk 上下文逐行比对 |
| 上下文爆炸 | 大文件全文进上下文，或一次失败输出几千行 | §9：双阈值截断 + 落盘指针 + 语义压缩 |

三者不是同一个机制的不同说法：转义摩擦发生在**请求构造**阶段，行号漂移
发生在**定位**阶段，上下文爆炸发生在**结果回传**阶段。所以它们落在不同的
层——请求形态（模式 C）、定位校验（模式 B/C）、输出治理（§9）。用一个机制
去解三个问题，必然有一个解不动。

**模式 C 的判据**：支持传入扁平文本 Diff，彻底消灭 JSON 引号转义。它解决的具体问题是：当改动涉及一整段结构化内容（重排 YAML 键序、
调整 JSON 缩进、批量改 import 顺序），模式 A 要求模型精确复制原文的每个
字符——JSON 字符串里的引号、反斜杠、换行都要正确转义，这是模型出错的高发区。

扁平 patch 用 unified diff 格式，原文以行前缀（空格/+/-）表达，不需要转义。

**模式 B 的机制与启用判据**

锚点形如「12#f8a2」：行号 + 该行内容的 4 位十六进制哈希，共约 7 个字符。
edit 拿到锚点后重算该行哈希，**不符即拒绝写入**，并把该行的实际内容回传。
这把「行号漂移」从一次静默的错误修改变成一次可读的失败。

默认关闭。启用判据是「省下的原文回传量 > 锚点带来的开销」：

- 单文件超过约 500 行，且预计反复改其中少量行；
- 同一文件在同一会话被读多次，oldText 回传成本累计超过锚点开销。

小文件、一次性编辑、整段重写用模式 A 更省，所以哈希锚点默认关闭（附录 A）。

**三种模式的共同前置**

**「不用先读」的准确含义**：省掉的是模型把全文读进上下文、再回传 `oldText` / patch 里的原文；**工具层每次写之前必须读原文并记版本指纹**，这不是可关闭的优化，是写侧安全的前置条件（§18.5）。三种模式只是寻址方式不同，共同前置完全相同。

```text
L0 读原文（按检测出的编码解码）→ 记录 (mtime, size, sha256) → 写前快照（§20）
L0 写前复核：指纹与读时不一致即拒绝，不做三方合并（§18.5）
L0 按模式定位与替换（偏移基于原始字节，不基于解码后的字符）
L0 写回：按原编码编码；无法无损表示时报错
L0 校验：patch 模式额外校验 hunk 行号与内容匹配
```

复核失败按 §1.4.5 的 ToolError 三段式返回，`nextSteps` 给出「重新 read / 重算锚点」，不把旧内容静默改写。`read(intent=raw)` 的产物是行锚点与版本指纹，不是供模型回传的全文；编排里中间结果不进模型历史（§21.3、§22.4）。

**匹配的四级策略：让失败在本地消化掉**

编辑失败的绝大多数原因不是「模型想错了」，而是「模型回传的原文与文件里的
字节不完全一致」：文件是 CRLF 而模型写的是 LF、行尾多了空格、缩进是 tab 而
模型写成空格、全角空格混进来。这些差异**不改变语义**，却让精确匹配失败，然后
云端要重新 read、重新生成、再来一次。

这类失败应该在本地消化，不该退回云端。四级策略：

| 级 | 策略 | 谁做 | 命中后 |
|---|---|---|---|
| 1 | 精确字节匹配 | 机械 | 直接应用 |
| 2 | 行尾归一化（CRLF/LF、尾随空白） | 机械 | 应用，按 §4.4b 的 `NORMALIZED` 告知（level=2 kind=eol） |
| 3 | 空白归一化（行首缩进、行内多空格、全角空格 / NBSP） | 机械 | 应用，按 §4.4b 的 `NORMALIZED` 告知（level=3 kind=whitespace） |
| 4 | 语义对齐（内容被重排、模型记的是旧版本） | decide 槽位判定 | 给出「最像的位置 + 差异类型」，能安全改就改，不能才报错 |

**两条硬约束**

1. **归一化只用于匹配，不用于写回。** 命中的位置按原文件的实际文本写回
   （行尾、缩进、编码都保留原样，§5.6）。否则「改一行」会变成「改了整个文件
   的空白」——这是比编辑失败更糟的结果。
2. **命中多处即拒绝。** 归一化放宽了匹配条件，同一片段可能在文件里出现多次。
   这时不猜：报「歧义 + 所有命中位置」，让调用方补上下文。猜错一次的代价远高于
   报错一次。

**第 4 级是「智能」真正介入的地方**

| 现象 | decide 槽位的判断 | 动作 |
|---|---|---|
| 找到高度相似但不完全一致的片段 | 空白 / 缩进差异 | 归一化后应用，返回 diff 供核对 |
| 相似片段有多处 | 歧义 | 拒绝，列出候选行号 |
| 内容真的变了（模型记的是旧版本） | 文件已变 | 报错 + 指出差异在哪一行 |
| 完全找不到 | 根本不存在 | 报错 + 建议扩大上下文或重新 read |

**这一级的价值不是「更宽容」，是把往返次数从 2 降到 1**：一次失败意味着云端
重新 read（几千 token）再重试；而本地做这个判断只要数毫秒。

**为什么不是「全部交给模型」**：机械能解决的（行尾、缩进）用规则解决，快且
确定；模型只处理规则覆盖不到的对齐（内容被重排、记的是旧版本）。把规则能做的
事也交给模型，等于用数百毫秒换 0ms 就能做对的事，还引入了不确定性——同一个
edit 两次可能给出不同结果，缓存与预期一起失效（§16.3 对策二）。

**失败时的 decide 槽位介入：先对齐，再归因**

```text
L1 归一化对齐（第 4 级）：在文件里找最相似的候选片段
L1 decide 判定（仅当归一化也没命中）：
     failure_kind?  上下文不足 / 文件已变 / 格式差异 / 根本不存在 / patch 冲突
     fixable?       能安全归一化应用 / 需要调用方补上下文 / 必须重新 read
     suggestion?    扩大上下文 / 重新 read / 确认路径 / 重算 hunk
L0 返回：能修就直接修（附 diff）；不能修才报错 + 归因 + 建议
```

edit 本身不产生浪费，但**失败很贵**——一次失败意味着云端要重新 read
（几千 token）再重试。而失败的原因里，**格式差异占绝大多数，且本地就能消除**。
所以顺序是「先尝试修好，修不了才报错」，不是「报错让调用方去修」。

#### 第 4 级的落地口径：机械分诊在前，判定只答规则答不了的

**机械分诊在前。** 第 1、2、3 级（精确 / 行尾 / 空白）都没命中时，先用相似度算出
最像的候选。**相似度低于阈值就直接按「根本不存在」走机械归因，不调用判定**——
理由与本节「为什么不是全部交给模型」一致：机械能解决的用规则解决，模型只处理
规则覆盖不到的（§5.4 末尾那段）。用模型回答机械能答的问题是拿数百毫秒换 0ms
就能做对的事，还引入不确定性（同一个 edit 两次可能给出不同结果，§16.3 对策二）。

**判定只答规则答不了的**，一次调用问六个二元问题：

| 组 | 问题 | 数 |
|---|---|---|
| 差异类型 | 上下文不足 / 文件已变 / 格式差异 / 歧义 / 根本不存在，各问「是不是这类？」 | 5 |
| 能否安全应用 | 「能否安全应用？」 | 1 |
| | **合计** | **6** |

`failure_kind` 取差异类型里概率最大者（差异类型之间互斥，取最大即可）。

**本次结论的置信度**：min(**被选中** `failure_kind` 的概率, `fixable` 的概率)。
「被选中类别」指 argmax 命中的那一类——**不是所有五类概率的最小值**。多选问题的
非答案概率天然极低（非答案本来就该低概率），取全量最小值会让任何判定都掉进低
置信档：实测 edit 场景 `file_changed` 问 0.5191（argmax 正确、归因也对），但
`format` 问只有 0.0096，取最小值得 0.01——判定明明是对的，却因为「不是答案的
那一类概率低」而被门控否掉。置信度该衡量「这条结论有多可信」，不是「所有问题
都多确定」（§7.3 的 confidence 来源）。

**与上面伪代码段的对应关系**：伪代码里列的「patch 冲突」不是独立的第六类差异
类型——patch 模式的 hunk 校验失败（§5.4 下方「为什么 patch 模式也要有 hunk
校验」）要么是文件变了、要么是模型算错了 hunk，判定在「文件已变」与其它之间区分
它。伪代码是路径示意，本小节是精确口径，两者以本小节为准。

**建议由 `failure_kind` 机械映射，不由判定生成。** §3.2 的 decide 槽位契约**没有自由
文本输出**——它只给每个问题一个概率。判定生成「建议扩大上下文」这类句子，等于让
模型在契约之外说话：措辞不受控、无法测试、还多一次生成开销。所以「失败原因 →
建议」是工具层按 `failure_kind` 查表得到的确定性映射（与 §5.1 的归并规则同理）。

**位置不交给判定。** 最像候选的位置由工具层用**机械相似度**给出——systemone 不返回
span（§3.2 的 span 仅在「要求定位时返回」），而且位置本来就是机械相似度的产物，
判定给不出更准的。**判定只回答差异类型与能否安全应用**，位置与建议都归工具层。

**state 有界。** `oldText` 与候选片段各截断到约 2000 字符，再带上路径与候选行号
（§6.4 的有界窗口口径，不是全文）。

**命中多处即拒绝的口径**（承接本节硬约束 2）：相似度算出多处候选时**不猜**——先由
判定确认「是不是歧义」，是则列出全部候选行号并拒绝，等调用方补上下文。判定的作用
是「确认歧义」而不是「挑一个」。

**为什么 patch 模式也要有 hunk 校验**

unified diff 的 hunk 头带行号（@@ -12,3 +12,5 @@）。如果文件已经被改过，
行号会错位。校验规则是：按 hunk 头定位，逐行比对上下文行（空格前缀的行），
全部匹配才应用。不匹配就报「patch 冲突」并给出实际内容，让 decide 槽位判断是
「文件已变」还是「模型算错了 hunk」。

### 5.5 task —— 状态判断 + 增量摘要

```text
L0 启动 / 读累积输出 / 终止
L0.5 语义去重（embedding；只作用于 decide/summarize 的模型输入）
L1 decide 判定：
     state?      运行中 / 已完成 / 卡住 / 已崩溃
     progress?   有进展 / 无变化 / 报错循环
L1.5 敏感度标注（classify；只发 SENSITIVE 告知，不改原文）
L2 summarize 增量摘要（仅当 action=read 且累积输出 > 2000 token）
协商：原始增量 / 摘要 + 状态
```

「卡住」的判定值得单说：开发服务器启动后输出不再变化，可能是正常等待，
也可能是死锁。decide 看最后几行 + 时间间隔能给出概率判断。

#### L1 判定的落地口径：多值语义分解成二元问题

上面图里的 `state?`（四选一）与 `progress?`（三选一）是**多值语义**，但 §3.2 的
decide 槽位契约是二元形状——「每个问题一个概率」，没有类型维度；systemone 的
`choice` 类型映射到 classify 槽位（§3.10），decide 契约本身仍无类型维度。所以与 §5.1 同构，把多值语义**分解成七个二元
问题**，一次请求问完（§13.4 方案 A）：

| 组 | 问题 | 稳定 key | 数 |
|---|---|---|---|
| state | 仍在正常运行（正常等待也算） | `state_running` | 1 |
| state | 已正常完成并退出 | `state_completed` | 1 |
| state | 输出长时间不再变化、卡住了 | `state_stuck` | 1 |
| state | 已崩溃或异常退出 | `state_crashed` | 1 |
| progress | 有新进展 | `progress_made` | 1 |
| progress | 长时间没有变化 | `progress_no_change` | 1 |
| progress | 重复同样的报错（报错循环） | `progress_error_loop` | 1 |
| | **合计** | | **7** |

**归并规则**（把七个二元概率合回多值字段）：

| 字段 | 规则 |
|---|---|
| `state` | 四个 state 候选取概率最大者；最大者 < 0.5 → `"unknown"`；否则 `running` / `completed` / `stuck` / `crashed` |
| `progress` | 三个 progress 候选取概率最大者；最大者 < 0.5 → `"unknown"`；否则 `made` / `no_change` / `error_loop` |
| `confidence` | min(被选中的 state 类别概率, 被选中的 progress 类别概率)，口径见 §7.3 |

**机械事实与模型判定分工**：`processState` / `exitCode` 由进程表直接给出
（§5.5「工具层只知道进程还在不在」），不依赖模型；模型给的是输出语义层面的状态
判断。两者分列在返回里，不互相覆盖。

**判定的 state 是有界窗口**：取累积输出**最后 8000 字符**（§6.4 prefill 预算，与
§5.1 同源），加上 `exitCode` / `elapsedSecs` 等机械观测——「decide 看最后几行 +
时间间隔」即落在这里。取尾部是因为卡住与报错循环看最近的输出最准。

**触发条件**沿用 §9.2 与 §5.1：`raw` 不判定；`summary` / `structured` / `auto`
输出 **≥ 500 token** 才判定，低于门槛由 `exitCode` 机械判断。判定不可用时工具照常
工作，`state` / `progress` 给 `"unknown"`、`meta.confidence` 为 `null`，按 §3.8
降级并按 §4.4b 发 `SLOT_UNAVAILABLE` 告知——**不编造状态**（§4.6）。

#### L1.5 敏感度标注与模型输入去重

与 §5.1 同口径：`intent != raw` 且 body ≥ 500 token 时用 classify 打
`secret` / `personal` / `normal` 标签并与机械正则取更高等级（机械为下限），
命中前两者发 `SENSITIVE`；classify 不可用时只用机械正则，不发 `SLOT_UNAVAILABLE`。送给 decide / summarize 的窗口先做精确行去重与 embedding
余弦聚类（50–200 行、阈值 0.92），只作用于模型输入，不回写 content；`meta.dedup`
与 `meta.sensitivity` 给程序字段。

#### L2 增量摘要的落地口径

`intent=summary` 与 `auto`（累积输出 > 2000 token）在 L1 判定之后尝试 L2 摘要：

- 摘要输入是**有界窗口**：累积输出尾部 8000 字符（与判定 state 同源，§6.4）加上
  `exitCode` / `elapsedSecs` 机械观测与 L1 判定事实（`state` / `progress`），
  不把整份累积输出喂给生成模型；
- 摘要成功时 `content` 只给摘要 + 状态行，判定事实在 `meta.decision`、完整输出在
  `meta.spill`，**不重复上传原文**；`meta.summary` 同时给程序可读通道；
- 摘要不可用（槽位未绑定 / feature 未开 / 调用失败）按 §3.8 降级：给原文 +
  `SLOT_UNAVAILABLE` 告知（调用失败时 `reason=call_failed`），工具照常工作（§7.3），**不编造摘要**。

显式 `intent=summary` 是调用方明确要加工，不受上图中 > 2000 token 的自动门槛限制；
该门槛只约束 `auto` 路径里 L2 节点的自动展开。

**task 怎么支持「断点续传」**

**决策**：task 只保证进程可寻址与输出增量读取；任务级断点归调用方（§18.4）。
两者分工如下：

| 能力 | 谁提供 | 怎么实现 |
|---|---|---|
| 进程可寻址、输出可增量读取 | **task 工具** | task_id 长期有效，read 返回增量 |
| 任务级断点（跑到第几步、还差什么） | 调用方 | checkpoint_id + status=partial（§18.4） |

工具层能保证的是：只要进程还活着，用 task_id 就能继续拿到输出；
进程已退出，read 会返回 exitCode 和完整累积输出。

**它不产生 partial 状态**，因为「任务完成了没有」需要知道目标是什么——
那是调用方的判断。工具层只知道「进程还在不在」。

这两者组合起来才构成完整的续传能力：调用方记「已做到哪一步」，
工具层保证「那一步的产物还在」。

### 5.6 编码与行尾：读改写的保真要求

**工具不能假设文件是 UTF-8。** 真实世界里存在 GBK 的旧代码、Shift-JIS
的配置文件、带 BOM 的 UTF-8、UTF-16 的 Windows 导出。

更麻烦的是**行尾**：CRLF 文件如果被读成 LF 再写回，git 会显示全文件修改。

#### 编码：检测 → 解码 → 处理 → 按原编码写回

检测顺序（强信号优先）：

```text
1. BOM：UTF-8 (EF BB BF) / UTF-16 LE (FF FE) / UTF-16 BE (FE FF)
2. 严格 UTF-8 能通过 → 判为 UTF-8
3. 统计检测（chardetng，Firefox 用的同一套）
4. 回退
```

处理规则：

| 场景 | 行为 |
|---|---|
| 能无损解码 | 正常处理，写回时用原编码 |
| 解码出现替换字符 | read 正常返回但按 §4.4b 的 `LOSSY_DECODE` 明确警告（encoding / replacements）；edit 拒绝（继续会破坏字节） |
| 写回时目标编码无法表示 | **报错**，不静默替换成问号 |
| UTF-16 | 不当二进制（它天然含 NUL 字节），按文本处理 |

**为什么不能「不是 UTF-8 就拒绝」**：那等于把这个工具限定在 UTF-8 世界里，
而它要处理的是真实文件系统。正确做法是能读能写，只在真正无法保真时才拒绝。

#### 行尾：保留原有风格

读入时检测第一个换行是 \r\n 还是 \n，写回时按原样输出。

三种模式都必须遵守：

- 文本匹配：只替换匹配到的字节区间，不碰其他行的分隔符；
- 行锚点：整行替换后按原行尾风格重新拼接；
- 扁平 patch：hunk 应用后保持原文行尾。

#### 匹配用归一化文本，写回用原始文本

这两件事必须分开，否则会互相破坏：

- **匹配时**把 CRLF、尾随空白、缩进差异抹平，才能容忍模型回传的轻微不一致
  （§5.4 的四级策略）；
- **写回时**用原文件的实际字节（行尾、缩进、编码），否则「改一行」会变成
  「改整个文件的空白」——这比编辑失败更糟。

判据一句话：**归一化只影响「找到哪里」，不影响「写回什么」**。

实现上有个坑：归一化后的文本长度与原文本不同（CRLF 少一个字节、tab 换空格后
长度也变），所以**不能拿归一化文本的偏移去写原始字节**。正确顺序是
「归一化定位到行区间 → 用原始字节的 line_spans 换算偏移 → 写回」。

v1 已实现其中一半：normalize 做的是「每行去尾随空白」，顺带覆盖了 CRLF
（\r 属于尾随空白）与行尾空格，并有 CRLF 回归测试。行首缩进与行内空白的
归一化还没做，属于 §5.4 的第 3 级。

#### 偏移必须基于原始字节

一个容易写错的地方：按行计算替换位置时，用「行长度 + 1」累加。
这在 LF 文件里对，在 CRLF 文件里每行少算 1 字节，累加后偏移一路偏小，
**替换会落到错误的位置**。

正确做法是先扫描出每行在原始字节里的 (start, end)，再做区间运算。
这一点在实现里由 line_spans() 保证，并有针对 CRLF 的回归测试。

#### 与 intent 的关系

编码与行尾的保真对所有 intent 都成立——它是「不破坏用户文件」的底线，
不是可选的加工策略。即便是 intent=summary，写操作也必须保真。

### 5.7 改动之后：给事实，不给指令

edit / write 成功之后，返回里带一组**事实**，不带任何「你该跑什么」的指令。

**为什么不做指令**：要给出「跑 cargo test」，工具得先猜这个项目用什么构建系统。
猜错很常见——workspace 根目录的 Cargo.toml 与子 crate 的测试命令不同，monorepo
里同一目录可能有多种构建方式。而一条错的指令比没有指令更糟：模型会照着跑，
然后对着一个不相干的失败困惑。

**给什么事实（按成本分级）**

| 级 | 事实 | 成本 | 默认 |
|---|---|---|---|
| 0 | diff：增删行数与行号范围 | 零（编辑时已知） | 给 |
| 0 | 文件级变化：行数、字节数、编码、行尾是否改变 | 零 | 给 |
| 1 | 同目录存在的构建 / 测试描述文件 | 一次目录列表 | 给 |
| 2 | 是否在 git 工作区、是否有未提交改动 | 一次 git 调用 | 配置开启 |
| 3 | 改动位置被哪些文件引用 | 一次搜索 | 按需（intent=structured） |

**第 1 级是关键**：它给的是「这个目录里有什么」，不是「你该跑什么」。模型看到
Cargo.toml 与 Makefile 同时存在时，自己知道该选哪个，或者知道要先看 Makefile
里写了什么。工具不替它决定。

**第 1 级要限范围**：只匹配一份已知的构建描述文件名清单（Cargo.toml、
package.json、Makefile、pyproject.toml、build.gradle、go.mod 等），不列出目录
全部内容——否则一个 node_modules 旁的大目录会把返回撑爆。清单为空时明确写
「未检测到构建描述文件」，而不是给一个空数组：空数组分不清「没有」和「没查」。

**为什么这比「自动跑测试」好**：自动跑测试意味着工具要理解项目结构、要处理失败、
要判断「这个失败是不是我改出来的」。那是 agent 的活，不是工具层的活（§11.2）。
工具层能给的是**准确、便宜、不含判断的事实**，判断留给模型——这正是 §1.2 的分工。

**一个具体的返回**

```json
{
  "changed": { "path": "src/auth.rs", "lines": [40, 44], "added": 3, "removed": 1 },
  "file": { "lines": 512, "bytes": 18342, "encoding": "utf-8", "eol": "lf" },
  "nearby": ["Cargo.toml", "Makefile"],
  "diff": "@@ -40,4 +40,6 @@ ..."
}
```

模型拿到这些，自己决定下一步。工具不越界——这条边界与 §21.4「不做批量入口」是
同一条：**能做判断的地方不做判断，能省的信息不省。**

---
