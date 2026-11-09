# toolgate 架构评审：systemone 协议入正文与阶段 1 验收（2026-11-09）

> 评审对象：把 TypeSafe 的 systemone 判定协议写进架构正文（§18.1 的 `source` 枚举与
> 协议能力表、§3.10 的形状适配、§19.6 的可选层 HTTP 客户端），以及阶段 1 验收的
> 首次端到端勾选（§14）。
> 核对方式：逐条比对 §3.10「其他协议按同样方式加 `source` 枚举与可选层适配器」
> 的扩展规则、§1.5 准入四问、§3.2 槽位契约、§7.2 置信度门控、§19.6 依赖分层；
> 本机 127.0.0.1:8090 的 StartLux 服务实跑三组 noul 概率与一组 choice；
> StartLuxLabs/StartLux-Decision 的 docs/api.md 与 README（2026-11-09 取）；
> `cargo tree` 系列命令实跑；`./scripts/check-doc.sh` 实跑。
> 结论：7 项发现（2 项 P1、3 项 P2、2 项 P3），已全部落架构正文；无 P0。

## 0. 结论摘要

| 项 | 分级 | 状态 | 落点 |
|---|---|---|---|
| S-1 systemone 协议缺 `source` 枚举与能力登记 | P1 | 已补 | 18-deployment.md §18.1 |
| S-2 systemone 的形状映射未定义（decide 契约 ↔ 命名问题） | P1 | 已补 | 03-slots.md §3.10 |
| S-3 §18.1 的 auto 推断规则对 systemone 无解 | P2 | 已补 | 18-deployment.md §18.1 |
| S-4 默认构建不含 HTTP 客户端时的行为未定义 | P2 | 已补 | 18-deployment.md §19.6、§18.1 |
| S-5 阶段 1 验收项无实测证据 | P2 | 已补 | 11-runtime.md §14 |
| S-6 协议能力表的三列结论缺实测支撑 | P3 | 已补 | 18-deployment.md §18.1、95-references.md E.10 |
| S-7 jev 类判定模型按 chat 协议调用会退化，误判为「模型坏了」 | P1 | 已补 | 03-slots.md §3.4、18-deployment.md §18.1、95-references.md E.10 |

## 1. S-1：systemone 缺 `source` 枚举与能力登记（P1）

### 1.1 现象

§18.1 的 `source` 枚举只有 `local` / `openai` / `anthropic` / `responses` /
`auto`，协议能力表只有对应的三列。但本机已经跑着 TypeSafe 的 systemone 判定服务
（127.0.0.1:8090，StartLux-Decision-v1.1-4B-AutoThink），它提供的判定形状与
§3.2 的 decide 契约、§7.2 的门控直接对应，却无处登记。

§18.1 结尾已给出扩展规则：「新增协议（如 Gemini 兼容）按同样方式加 `source`
枚举与可选层适配器，属于扩展，不改槽位契约」。systemone 正是这条规则的第一个
使用者，所以本次不是改槽位契约，是按既有扩展规则补登记。

### 1.2 协议形状（官方定义 + 本机实测）

`POST /v1/systemone`（别名 `/v1/decisions`）：

```text
请求: { state: string | object,
        questions: { "<name>": { type: "noul" | "choice" | "score",
                                   instructions: string,
                                   criteria?: object } },
        images?: [base64 / data URI] }        # 每请求至多 8 张
响应: { answers: { "<name>": { type: "noul",  noul: number }
                          | { type: "choice", choice: string, confidence: number,
                              probabilities: { <option>: number }, none_probability: number }
                          | { type: "score",  ... } },
        usage: { input_tokens, output_tokens },
        model: string }
```

官方语义：`noul` 是 yes 的概率；`score` 是量表上的等级期望值；`none_probability`
是「没有选项合适」的告警分（官方建议阈值 0.2）。

### 1.3 本机实测（2026-11-09）

| state | 问题 | noul | 判读 |
|---|---|---|---|
| `这段 Python 代码有语法错误：def f(: pass` | 这段代码有语法错误吗？ | 0.960 | 肯定 |
| `订单号 A-123 已被标记为已发货，运费 0 元。` | 这个订单是已发货状态吗？ | 0.997 | 肯定 |
| `今天天气不错，适合散步。` | 这个订单已经发货了吗？ | 0.126 | 否定（state 未提及） |

另实测一组 `choice`：返回逐选项 `probabilities` 与 `none_probability`
（一次实测 `none_probability` = 0.505）。三组 noul 的形态与 §7.2 门控同向——
肯定接近 1、否定接近 0.13 而非 0，说明是概率不是硬布尔。

### 1.4 协议能力表新增 systemone 列

| 槽位 | openai | anthropic | responses | systemone | 说明 |
|---|---|---|---|---|---|
| summarize | 可用 | 可用 | 可用 | **不可用** | systemone 只做判定，不生成文本 |
| vision | 可用 | 可用 | 可用 | 可用 | `images` 需后端模型带 vision tower（本机 4B MLX 构建带） |
| classify | 可用（JSON schema） | 可用（JSON / tool use） | 可用（structured outputs） | **受限** | `choice` 有逐选项概率，但 §3.10 适配器本轮只映射 `noul`，走 `choice` 的 classify 映射未落 |
| decide | **受限** | **受限** | **受限** | **可用（给校准概率）** | `noul` = P(yes)，直接满足 §3.2 decide 契约与 §7.2 门控 |
| rerank | **受限** | **受限** | **受限** | **不可用** | 无排序接口 |

### 1.5 为什么值得单列，而不是硬套 openai

openai 协议的 decide 标「受限」：实测 llama.cpp 的 `/v1/chat/completions` 虽带
`logprobs`（token 级概率），但那是「下一个 token 的分布」，要映射到「每个问题
一个概率」还需额外约定（多 token 答案如何聚合、答案在序列中的位置如何定位），
本轮不落。systemone 把这层约定做进了协议本身，直接给出命名问题 → 校准概率。

**它就是 §13.4 所说的 d1 形态的原生实现**：「一个 state + 任意数量命名问题 →
带概率的答案集合」逐字对应。§13.4 的方案 A / B 因此可以直接作用在 systemone 上，
不需要为它另设一套调用形态。

### 1.6 落点

18-deployment.md §18.1：`source` 表新增 `systemone` 一行（字段同其他外部协议：
`base_url` + `model` + `api_key_env`；只允许 loopback；`mode` 只能是 `http`）；
协议能力表新增 systemone 列；新增「systemone：TypeSafe 判定协议」小节（协议形状、
官方语义、为什么单列、本机实测表）。

## 2. S-2：systemone 的形状映射未定义（P1）

### 2.1 现象

§3.2 的 decide 契约是「每个问题一个 probability」，槽位输入是 `questions`（字符串
数组）+ `state`；systemone 的输入是「命名问题对象」（键 → 问题定义）。两者的
形状转换规则没写，实现时会各写各的。

### 2.2 映射规则

| 方向 | 映射 |
|---|---|
| 槽位 decide → 协议 | `questions`（字符串数组）按序编号成命名问题 `q0` / `q1` / …；每个 `type` 固定 `noul`，`instructions` 写问题原文；`state` 写槽位输入的文本 / 对象 |
| 协议 → 槽位 | 每个 `noul` 答案的数值 → 对应问题的 `probability`；`q0` / `q1` 按序还原回槽位问题的顺序 |
| 概率语义 | 「该问题的肯定回答为真的概率」，与 §7.2 门控同向（≥0.85 采用 / 0.60–0.85 标 LOW_CONFIDENCE / <0.60 两视图都给） |

**只接受 `noul` 类型**：§3.2 的槽位契约**没有类型维度**——槽位只问「这个问题的
概率是多少」，不表达「从哪几个选项里选」。上游返回 `choice` / `score` 时按
§3.11 的「形状不符」处理（该次降级、连续失败后禁用该槽位并告警），**不做静默
转换**：把 `choice` 的 `confidence` 当 `probability` 会把「选项间的相对置信」与
「二元肯定概率」混为一谈。

**adapter 判定**：systemone 走 `native` 而非 `json`——它直接给校准概率，不需要
「要求模型输出 JSON schema 再解析」这层转换，也不触发 §3.10 的 `UNCALIBRATED`
标注。§3.10 的 auto 规则表新增 `source: systemone` → 槽位 `decide` → `native`。

### 2.3 落点

03-slots.md §3.10：auto 规则表新增 systemone 行；新增「systemone 适配器：槽位形状
↔ 命名问题」小节（映射表、只接受 noul 的理由与边界、与 `adapter: json` 的差别）。

## 3. S-3：auto 推断规则对 systemone 无解（P2）

### 3.1 现象

§18.1 的 `source: auto` 判定顺序是六条（`path` → `local`；URL 含
`/chat/completions` → `openai`；含 `/messages` → `anthropic`；含 `/responses`
→ `responses`；host 判定；都不命中 → 启动失败）。systemone 的 `base_url` 写的是
服务根地址（`http://127.0.0.1:8090`），根地址里没有任何协议特征可据此判断后端
是 systemone 还是 OpenAI 兼容端点——同一个根地址两种都能挂。

### 3.2 处理

**这六条顺序不变，`systemone` 不进自动推断，必须显式写 `source: systemone`。**
理由是 §18.1 的保守推断原则：猜错协议比报错更糟（§1.4.4 的可行动失败）。显式写
时 `base_url` 指向服务根地址，客户端自行拼 `/v1/systemone` 路径。

### 3.3 落点

18-deployment.md §18.1「`source: auto` 的判定顺序」：六条之后补一段说明
（为何不进推断、显式写法）。附录 D 的 `models.<id>.source` 默认值行同步注明
「`systemone` 必须显式写，不参与 auto 推断」。

## 4. S-4：默认构建不含 HTTP 客户端时的行为未定义（P2）

### 4.1 现象

§19.6 的可选层有「外部模型协议客户端」一行（feature `external-protocols`，默认
关闭），但**默认构建不含该 feature 时，配置了外部 `source` 的槽位会怎样**，
原文没写。这直接影响 §2 红线 3（槽位是增强不是依赖）：如果配置了外部源就启动
失败，等于让一个可选项成为硬依赖。

### 4.2 约定

**按 §3.8 降级并告警，归类为资源层失败**（§18.9 四层校验的第四层），**不是配置
错误**——配置本身合法，缺的是这个构建里的后端能力。判定发生在启动时发现「该
feature 未编入」，不在配置解析阶段。

### 4.3 落点

18-deployment.md §18.1「依赖」段与 §19.6 可选层：两处都写明这个降级口径；
§19.6 另补「可选层的 HTTP 客户端选型」段（选 `reqwest` 的理由：异步、纯 Rust
`rustls` 后端、不引入 OpenSSL 之类系统级 C 依赖 → 纯 L0 版仍是单二进制）。

## 5. S-5：阶段 1 验收项无实测证据（P2）

### 5.1 现象

§14 阶段 1 的验收写「能对一段固定文本问『有错吗』，拿到带概率的答案」，但没有
实测记录。§14 是路线图，勾选验收项属**实现状态记录**，不是架构决策——本次勾选
不改动任何架构决策，只补首次端到端证据。

### 5.2 验收形态

`toolgate decide` 子命令 + 本机 StartLux 服务（127.0.0.1:8090，StartLux-Decision-
v1.1-4B-AutoThink）端到端，走 `source: systemone`。实测三组 noul 概率：
肯定错误 0.960、state 内事实 0.997、state 未提及 0.126。

**同时写明未覆盖的部分**（不影响本条验收，但阶段 1 未完）：五个槽位的客户端适配
只落了 decide 一条；§13.4 的 turn 聚合钩子随阶段 1.5 与阶段 4 落地；
`external-protocols` feature 默认关闭，默认构建下这条路径不可用（§19.6）。

### 5.3 落点

11-runtime.md §14 阶段 1「验收」：补验收形态、实测数字、「这是实现状态记录不是
架构决策」的说明与未覆盖部分。

## 6. S-6：协议能力表的原结论缺实测支撑（P3）

### 6.1 现象

§18.1 的协议能力表把 openai / anthropic / responses 的 decide 一律标「受限」，
理由写「无校准概率 → §7.2 置信度门控不可用；需实测各协议 logprobs 能力」。
「需实测」是待办不是结论，而这张表是选型的依据。

### 6.2 补齐

本轮实测了可测的一半：llama.cpp 的 `/v1/chat/completions` **确实带 `logprobs`**
（token 级概率），但它是「下一个 token 的分布」，映射到「每个问题一个概率」还需
额外约定（多 token 答案如何聚合、答案在序列中的位置如何定位）——所以「受限」的
结论成立，理由从「需实测」换成实测后的具体依据。anthropic / responses 的
logprobs 能力本轮未实测，表中保持「受限」，不写成实测结论。

### 6.3 落点

18-deployment.md §18.1「为什么 systemone 值得单列」段：写明 openai 的 logprobs
实测结论与映射缺口；95-references.md 附件 E.10 记录 systemone 协议来源与本机实测。

## 7. S-7：jev 类判定模型按 chat 协议调用会退化（P1）

### 7.1 现象

§18.1 的协议能力表把 openai / anthropic / responses 的 decide 标「受限」，理由
写「需实测各协议 logprobs 能力」。但**参考判定实现（StartLux / d3-nano，§3.4）
根本不是「精度差一点」，而是协议层就不对**。

本机实测（2026-11-09）：d3-nano 经 llama-server 的 `/v1/chat/completions` 调用，
返回 `finish_reason=length`，内容是重复 token 序列（形如 `IAHO-YA-IH` 循环，无语义），
`usage.completion_tokens` 撞 40 的上限。**这是「用错协议」的特征，不是「模型加载
失败」或「权重损坏」**——但两者的输出表面相似，极易误判：看到「模型输出乱码」就
去换模型、换量化、重装 llama.cpp，方向全错。

### 7.2 补齐

三处都写上，避免下一个使用者重踩：

1. **§18.1 协议能力表下方注解**：`openai` / `anthropic` / `responses` 是**通用
   LLM 的协议形态**，jev 类判定模型不按 chat 协议工作，对它们只能走
   `systemone`；openai 列的 decide 标「受限」，对这类模型是**不可用**（不是
   精度差一点）；
2. **§3.4 参考实现**：补本机实测结论——参考判定模型属 systemone 协议族；用通用
   chat 协议调用会得到退化输出，**判据是 `finish_reason=length` + 重复 token
   序列**，别误判成「模型坏了」；正确处理是改用 systemone 协议（`source:
   systemone`），不是去换模型；
3. **§18.1 的 systemone 段**：写明它是 jev 类判定模型的**原生**协议——这是它
   值得单列一个 `source` 而不是硬套 openai 的更根本理由（原先只写了「直接提供
   命名问题 → 校准概率」这一层）；附件 E.10 记录实测证据。

### 7.3 与 S-6 的区别

S-6 的「受限」是**能调通、映射缺约定**（openai 的 logprobs 是 token 级概率，
映射到「每个问题一个概率」还需聚合约定）；S-7 的「退化」是**协议层就不对**。
两者不是同一个问题，附件 E.10 里也分开写了。

### 7.4 落点

03-slots.md §3.4；18-deployment.md §18.1（能力表注解 + systemone 段）；
95-references.md E.10（实测证据与两者的区别）。

## 8. 结论

- 7 项发现全部落架构正文；无新增编号章节（在既有小节内补内容，新增两处不编号的
  小节标题：§18.1 的「systemone：TypeSafe 判定协议」、§3.10 的「systemone 适配器：
  槽位形状 ↔ 命名问题」）；
- 无冻结文档（`requirements-coverage.md` / `value-audit.md`）改动，无既有评审
  报告改动；
- 协议数字（noul 概率）全部本机实跑取得，官方语义标注来源为 StartLuxLabs/
  StartLux-Decision 的 docs/api.md 与 README（2026-11-09 取）；
- §2 七条红线未被触及：工具数、协议（MCP 不变，systemone 是模型服务协议不是
  MCP 协议）、槽位可空、默认层依赖分层（HTTP 客户端仍在可选层）、工具层无状态、
  隔离强度（systemone 同样只允许 loopback）、错误契约均未变；
- `./scripts/check-doc.sh` 退出码 0。

按 §3.6，本报告是历史证据，只作待办来源，不回写结论。

## 9. 遗留

1. **`reqwest` 的实测依赖数字（已补）**：§19.6 可选层写明选型（`reqwest`，
   `default-features = false` + `features = ["json"]`，不带 TLS 后端——§18.1 第一期
   只允许 loopback 的 http，远程不实现，带 rustls 会多出 rustls / ring 整棵子树）。
   附件 E.8 的数字已实跑补齐：`reqwest` 子树 **79** 个唯一包、默认构建规模
   **114** 行、`Cargo.lock` **164** 个包、`--duplicates` **2** 组。**79 是唯一一棵
   超过 10 的子树，而它不在默认层**（默认构建的 `cargo tree` 里 `reqwest` 为 0 条）。
2. **systemone 的 classify 路径未落**：协议有 `choice` 类型（逐选项概率），
   但 §3.10 的适配器只映射 `noul`。走 `choice` 的 classify 需要额外约定
   （逐选项概率如何对齐槽位的「每个标签的分数」契约、`none_probability` 如何
   参与降级），本轮不落，表中标「受限」。
3. **`external-protocols` 默认关闭**：默认构建不含 HTTP 客户端，配置了外部
   `source` 的槽位按 §3.8 降级（§19.6）。要让 systemone 路径在默认构建可用，
   需另行决策是否把它转成默认层——那是 §1.5 的准入问题，不在本轮范围。
4. **anthropic / responses 的 logprobs 能力未实测**：能力表中这两列的「受限」
   保持原状，未做实测改写（§6.2）。
5. **仓库 MSRV 与依赖版本（已解决，非遗留）**：2026-11-09 按用户要求把
   workspace `rust-version` 从 1.85 抬到 **1.89**（noyalib 0.0.58 的 MSRV，未抬到
   本机工具链版本），按 §19.6 的「`cargo update` 单独提交」纪律把 `noyalib`
   从 0.0.15 升到 **0.0.58**（新增传递依赖 `libm` 0.2.16），随后做了一次全量
   `cargo update`（21 个包升到 Rust 1.89 兼容的最高版本）。**依赖取最新版**这条
   纪律已合并进 §19.6 的既有约束条（新增依赖取注册表当前最新版；MSRV 挡在旧版时
   抬 MSRV 而非将就；版本更新单独提交）。版本口径与连带影响见附件 E.8。
