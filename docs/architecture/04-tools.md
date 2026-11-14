# toolgate 架构设计 · 三层结构与六类处理器（§4）

> 本文是 [architecture.md](../architecture.md) 的分册，章节编号与总目录一致。
> 返回总目录：[architecture.md](../architecture.md)

## 4. 三层结构与 intent 契约

### 4.0a 六类处理器总览（重新定位后）

对外不再有「六个工具」，改为在宿主接缝拦截内置工具、按**返回形状**分派的**六类处理器**。
三层结构（执行 / 理解 / 协商）保留，但「执行」由宿主内置工具完成，toolgate 只做理解与协商。

| 处理器 | 观察到的形状（确定性 L0 嗅探） | 分派槽位 | 动作 |
|---|---|---|---|
| 视觉 | 含 image block / 图片魔数 | vision | 压缩 + OCR / 描述 + 判定 need_pixels |
| 长文 | 超长文本 / ANSI 日志 / 行缓冲 | summarize | 双阈值截断 + 落盘指针 + 摘要 |
| 错误 | isError / exit≠0 / stack | （L0 + decide） | fact / reason / nextSteps 三段（§1.4.4） |
| 列表 | 大量行列表 / 命中清单 | rerank | 按意图重排 + 截断 |
| 结构化 | 已是带 outputSchema 的 JSON | 归一 | 保留结构，仅补 meta / notices |
| 元信息 | 二进制 / 未知 MIME | （无） | 路径 + 类型 + 大小，不瞎解析 |

**错误处理器保真**：≤32 KiB 的错误原文逐字保留——错误栈、退出码与上下文是模型判断的
一手事实，只取首行或替换成摘要都会丢信息（§4.6「诚实」）。超过 32 KiB 时只做确定性
压缩（保留首尾 + 关键行，标注省略行数），不请求模型；fact / reason / nextSteps 三段
只放 `structuredContent.error` 供程序读取，不改写正文。

**治理时间预算**：治理链的等待上限按返回形状给定，不用统一超时。它与 §3.1 的槽位
预算分工不同：槽位预算是选型与观测基线，不作运行时门控；本节是对一次工具结果的
端到端等待约束。

- **长文**：输入超 64 KiB 时先采样（头 + 尾 + 关键行）到 32 KiB 再送 summarize，
  `intent=summary` 放宽到 120 KiB；完整原文一律落盘并回指针（§9.1）。先减输入再谈
  超时——延迟随输入增长，调超时只会拉长等待。
- **列表**：调用 rerank 前按候选数与实测延迟估算（约 100ms + 15ms/条），估算超 2s 或
  候选 > 1000 时不调模型，直接头尾采样 + `max_lines` 截断（≤2000 行，毫秒级）；语义
  排序只在搜索 / 多文档候选来源开启。不满足条件仍调模型属于「为了治理而治理」。
- **错误**：短错误逐字保真；超 32 KiB 走上面的确定性压缩，不走模型。

**免白名单三条**：① 截获是**全局钩子**（每次工具 dispatch 必过），不需登记装了哪些工具；
② 路由看**返回形状**不看工具名，新工具自动覆盖；③ 形状不识别 → **退回纯 L0 治理**（机械
截断 / 落盘 / 脱敏 / 错误可行动化），不瞎摘要——未知形状不会出错，只是增强得少（§3.8 同构）。
三级信号：确定性命中 → 返回自带 `structuredContent` / `outputSchema` → 仍模糊才交 classify /
decide 按内容判。完整论证见提案 §5。

**视觉处理器约束（OCR / 描述档）**：视觉处理器把图片转成文本或结构时，遵守：

① **先判 need_pixels 再决定要不要像素**：只需文字的场景（文档 / 表单 / 代码截图）优先生成文本，不把原图送模型——原图 base64 与视觉 token 是最大开销（§10.1）。
② **长图必须先切块再识别**：高度超阈值的整图送检会被缩放失真到无法识别，必须按空白带切块、逐块识别、按序去重拼接（阈值与切点算法见 §5.3）。
③ **低置信 / 易混短文本强制第二引擎复核**：短文本含「数字夹字母」「全大写缩写」等易被图标污染的 token 时，即使快引擎置信度高也升专用 OCR 引擎复核，避免「自信读错」。
④ **多引擎降级链**：本地快引擎 → 专用 OCR → 语义模型（描述 / 兜底）→ 会话模型；上一档不可用或形状不符按 §3.8 / §3.11 降级并告警，不整体失败（OCR 无字时自动转描述档）。
⑤ **输出如实映射**：每行 bbox、低置信、交叉复核失败、降级原因都进 `notices` / `structuredContent`，不得把「未复核 / 降级」结果当高置信返回（§4.6「不假装成功」）。
⑥ **实现一律 Rust 原生，不引跨语言运行时**：OCR / 描述档的引擎与图像算法用 Rust crate 原生实现——图像解码 / 切块用 `image`（§19.6 默认层），原生推理倾向 candle（§19.3 评估中）；VLM 经既有 HTTP 协议（`openai` / `anthropic` / `responses`）调用。**不得以 Python / Node 子进程或解释器承载**（§1.5 禁止层已含 GC 运行时，§19.3 单二进制）。本机 `~/.ocr-tool` 的 Python 实现仅作设计参考与算法来源，不进入运行依赖。

⑦ **视觉默认经反向委派，daemon 不持有字节**：判定 `need_pixels=false` 且 daemon 无本地 VLM 时，
视觉描述 / OCR 走 §18.7 反向委派，把**原 image 引用**交给宿主会话模型（DSH 由官方 `readImageRequest`
按目标宽高与 maxBytes 缩放 / 编码 / 缓存）；daemon 只携带 `attachmentId` 与元信息，**绝不 base64
搬运图片字节**。仅当宿主无回调、且部署显式开启 `dataBase64` 降级（≤256 KiB）时才允许内联，并计入
预算与告警。`need_pixels` 判定：intent=`visual`，或来源工具契约本身即「把图返回给模型」（如
`read_image`）→ 保留原图；其余 → 生成文本。

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

- **token 效率**：截断、summary、落盘指针是默认路径（§9）；工具描述控制在一屏内，不写长文档（§4.6 的三段式要短）。六个工具的 `tools/list` 声明（description / inputSchema / outputSchema）合计是每请求常驻固定开销，硬预算 **≤1.5K token**（按 4 B/token ≈ 6000 B）；超预算时压描述与 schema 文本，不加字段、不靠可选层裁剪。
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
