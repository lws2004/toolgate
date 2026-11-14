# toolgate 架构设计 · 部署形态与技术选型（§18–§19）

> 本文是 [architecture.md](../architecture.md) 的分册，章节编号与总目录一致。
> 返回总目录：[architecture.md](../architecture.md)

## 18. 部署形态与接口契约

模型适配层与 Task Input / Task Result 的边界决策如下——**它们不是不做，是归属不同**。

### 18.1 模型适配层：进程内还是进程外

模型适配层支持进程内与进程外两种形态，**默认按槽位分档，不是一刀切**：

- **判定类槽位（classify / decide）默认进程内（embedded）**——预算几十毫秒，
  IPC 的 1-3ms 占比不可忽略；
- **生成与批处理槽位（rerank / summarize / vision）默认进程外（http）**——预算
  秒级，IPC 可以忽略，而模型更大、常驻成本更高。

| 形态 | 延迟 | 数据边界 | 适用 |
|---|---|---|---|
| 进程内（embedded） | 最低（无 IPC） | 数据不出进程 | 单实例、单信任域、模型小且常驻 |
| 进程外（http，loopback） | +1-3ms（本机回环） | 数据出进程但不出机器 | 多实例共享、模型大、要故障隔离 |
| 远程 | +网络延迟 | 数据出机器 | 不推荐（违背数据隔离） |

**为什么不是六个槽位一律进程内**：进程内的收益是省掉 IPC 的 1-3ms，这笔收益只在
「端到端预算几十毫秒」的槽位上成立。§3.1 的预算里只有 classify（<20ms）与
decide（<30ms）属于这一档；summarize（<1s）、vision（<2s）、rerank（批处理）、embedding（批处理）
的预算比 IPC 大两三个数量级，进程内省下的时间可以忽略，却要把数 GB 的模型
装进工具进程。

| 槽位 | 默认模式 | 理由 |
|---|---|---|
| classify | embedded | 预算 <20ms；参考实现是 2-4B 的 jev 类，常驻成本可控 |
| decide | embedded | 预算 <30ms；判定在每次工具调用的热路径上 |
| rerank | http | 批处理、N 可变；570M 模型常驻换不来收益 |
| summarize | http | 预算 <1s；生成模型较大，进程内常驻不划算 |
| vision | http | 预算 <2s；多模态后端平台绑定最重 |
| embedding | http | 批处理、N 可变；向量模型常驻换不来收益 |

**30B 级 agent 模型不归本节管**：它是本地 agent（客户端）的模型，由客户端部署
与加载，不需要 toolgate 进程背；同机共存时用进程外服务避免争内存（§21.7.9）。
本节的模型适配层只服务六个槽位（2B 级）。

**进程内默认还有三个前提**，任一不满足就该整机走 http：

1. **单实例、单信任域**：多信任域要多实例（§18.2），每个实例各加载一份进程内
   模型，内存翻倍；共享一个进程外模型服务才是正确形态；
2. **单模型能装进进程**：模型权重加运行时开销超过可用内存时，进程内不可行；
3. **能接受 native 崩溃带走工具进程**：见下面的故障边界，这是 embedded 的固有
   代价，不是实现缺陷。

#### 进程内模式的执行模型

进程内 = 推理引擎以动态库（llama.cpp / MLX）形式加载进 toolgate 主进程，
不是「模型文件放在同一目录」。架构对它提五条约束：

1. **模型实例按模型文件去重，不按槽位编号。** 多个槽位指向同一模型时共享一个
   实例（§3.10 的 serves）；实例是进程级单例，引用计数归零才能卸载。
2. **常驻上限默认 2 个模型实例，热槽位优先。** classify / decide 指向同一模型时
   共用一个实例（参考实现就是这种形态）；指向不同模型时两个都常驻，冷槽位不再
   驻留。冷槽位按 LRU 竞争剩余名额。依据是参考实现的内存账：六个槽位全常驻
   约 11GB（4B + 2.2B + 1.7B + 1.9B + 0.57B rerank + 0.57B embedding 的 8bit 权重，估算值，未含运行时
   开销），普通开发机放不下。上限可配（`slm.embedded.max_resident_models`）；
   启动时按模型文件大小估算常驻内存，估算值超过物理内存 50% 即拒绝启动。
3. **加载时机：热槽位启动时预加载，冷槽位首次调用时懒加载。** 预加载在后台
   线程进行，不阻塞 MCP 启动；未就绪期间该槽位按不可用降级，content 按 §4.4b 的
   `SLOT_UNAVAILABLE` 告知（fallback=loading），`meta.slmStatus` 中该槽位为 `loading`（形状见 §7.3 返回契约）。
4. **并发：一个模型实例一个专用推理线程 + 有界队列，实例内推理串行。**
   native 推理上下文不是线程安全的，不能直接在 tokio task 里跑；队列满时返回
   排队深度或按 §7.3 降级，不无界堆积。不同实例之间可以并行。理解链内的并发
   按 §4.8 调度：无依赖节点并行，同一实例的节点排队。
5. **卸载：冷槽位空闲 TTL（默认 10 分钟）后卸载；配置变更要重启（§8.8）。**
   卸载只丢 KV 缓存，不影响落盘副本与快照。

#### 进程内的故障边界

三类失败必须分开，不能都用「降级」兜：

| 失败 | 能否在进程内处理 | 行为 |
|---|---|---|
| 模型文件缺失 / 加载失败 | 能 | 该槽位置为不可用，按 §3.8 降级，工具继续（§7.3） |
| 推理错误 / 超时 | 能 | 该次调用降级；连续失败 N 次后实例标记 poisoned，后续直接降级 |
| native abort / SIGSEGV | **不能** | 整个 toolgate 进程退出 |

第三类是进程内模式的固有代价：C++ / Apple 框架的崩溃不能靠 Rust 的
`catch_unwind` 兜住。所以进程内模式**不承诺**§7.3 的「本地模型不可用工具必须
仍工作」全覆盖——它只覆盖前两类。要覆盖第三类，必须用进程外模式，让模型服务
自己崩，toolgate 调用失败后降级。

**这也是选择 http 的第二个理由：不是延迟，是故障隔离。**

#### 进程外模式（http）与共享

进程外是本地回环 HTTP：`llama-server` / `rapid-mlx` 独立常驻，toolgate 只做客户端。
它换来三件事：

- **故障隔离**：模型进程崩溃不影响工具进程；
- **共享**：同一台机器上的多个 toolgate 实例（多信任域，§18.2）共用一个模型服务，
  避免重复加载；
- **后端自由**：llama.cpp / MLX / vLLM 都可以，不限 Rust 绑定。

代价是 +1-3ms 回环延迟，以及模型服务自身的生命周期要有人管（谁拉起、谁看护）。
在单机单用户、判定槽位的场景下这两项是纯成本，所以判定类默认不走它。

#### 模型来源：一个 `source` 字段

**模型来源是部署形态，不是能力等级。** 来源类型与协议合成一个字段，避免「external + protocol」两层概念：

| `source` | 来源 | 字段 | 形态 | 典型 |
|---|---|---|---|---|
| `local` | 本地权重 | `path` | embedded 加载，或 toolgate 拉起本地推理服务（http） | llama.cpp / MLX 权重 |
| `openai` | OpenAI Chat Completions | `base_url` + `model`（+ `api_key_env`） | 按协议调用 HTTP 接口；第一期只允许 loopback | llama-server / vLLM / 兼容服务 |
| `anthropic` | Anthropic Messages | 同上 | 同上 | Anthropic 兼容服务 |
| `responses` | OpenAI Responses | 同上 | 同上 | OpenAI Responses 兼容服务 |
| `systemone` | TypeSafe SystemOne（`/v1/systemone`） | 同上 | 同上 | StartLux-Decision 等类型化判定服务 |
| `rerank` | 原生重排序端点（`/v1/rerank`） | 同上 | 同上 | jina-reranker 等专用 listwise reranker |
| `embedding` | 原生向量端点（`/v1/embeddings`） | 同上 | 同上 | bge-m3 等 embedding 服务 |
| `agent` | 反向委派给宿主会话模型 | 无（随 `govern`/`pre` 请求携带） | 不常驻；daemon 把单条内容回投宿主、由 agent 模型执行后回填 | 一期：本地无 SLM 时借用会话模型做 summarize / classify |
| `auto` | 按 URL 推断 | `path` 或 `base_url` + `model` | 推断为上面某一种，规则见下 | 地址已含协议信息时 |

**`agent`（反向委派）不占常驻模型、toolgate 自身不出网**：`govern` / `pre` 需要槽位增强而部署未配本地 SLM 时，daemon 把单条待处理内容回投宿主，由 agent 会话模型完成该次增强后回填（详见 §18.7「反向委派」）。它不触发下面的 loopback 限制——出网的是宿主既有模型通道，不是 toolgate 新开的连接；`openai` / `anthropic` / `responses` 等**由 toolgate 主动发起**的外部调用仍限 loopback（第一期）。

**`base_url` 口径**：`openai` / `anthropic` / `responses` 的 `base_url` 是含 `/v1` 的 API 根（如 `http://127.0.0.1:8080/v1`）；`systemone` / `rerank` / `embedding` 是不含 `/v1` 的**服务根地址**，客户端自行拼 `/v1/systemone` / `/v1/rerank` / `/v1/embeddings`。三者不可与含 `/v1` 的 API 根混用，否则会拼出 `/v1/v1/...`。

```yaml
models:
  jev:
    source: local
    path: ~/.toolgate/models/jev
    serves: [classify, decide]
  lfm3-vl:
    source: auto              # auto（默认）| local | openai | anthropic | responses | systemone | rerank | embedding
    base_url: http://127.0.0.1:8080/v1
    model: lfm3-vl
    api_key_env: TOOLGATE_MODEL_KEY   # 可选；密钥从环境变量读，不落配置
    serves: [summarize, vision]
```

**`source: auto` 的判定顺序**（只做保守推断，不猜）：

1. 配了 `path` → `local`；
2. `base_url` 路径含 `/chat/completions` → `openai`；
3. 含 `/messages` → `anthropic`；
4. 含 `/responses` → `responses`；
5. host 是 `api.openai.com` → `openai`；`api.anthropic.com` → `anthropic`；
6. 都不命中 → **启动失败**，要求显式写 `source`。

**这六条顺序不变，`systemone`、`rerank` 与 `embedding` 都不进自动推断。** `base_url` 写的是服务根地址
时，根地址里没有任何协议特征可据此判断后端属于哪个独立协议族——同一个根地址
既能挂 OpenAI 兼容端点，也能挂 systemone、rerank 等独立协议端点。按本节的保守
推断原则（§1.4.4 的可行动失败：猜错协议比报错更糟），
**`systemone`、`rerank` 与 `embedding` 都必须显式写 `source`**。

显式写 `source: systemone` 时，`base_url` 指向服务根地址，客户端自行拼
`/v1/systemone` 路径；`source: rerank` 同口径，客户端拼 `/v1/rerank`；`source: embedding` 拼 `/v1/embeddings`（形状见 §3.2）。

不做启动探测：探测要发真实请求，且错误响应形状不可靠；猜错协议比报错更糟（§1.4.4 的可行动失败）。

**协议能力不是能力等价**：六类接口是「传输形态」，各槽位能否用要看接口能力（适配器见 §3.10）：

| 槽位 | openai | anthropic | responses | systemone | rerank | embedding | 说明 |
|---|---|---|---|---|---|---|---|
| summarize | 可用 | 可用 | 可用 | **不可用** | **不可用** | **不可用** | systemone 只做判定，不生成文本；rerank / embedding 只做排序 / 向量，不生成文本 |
| vision | 可用 | 可用 | 可用 | 可用 | **不可用** | **不可用** | systemone 的 `images` 需后端模型带 vision tower（本机 4B MLX 构建带） |
| classify | 可用（JSON schema） | 可用（JSON / tool use） | 可用（structured outputs） | **可用（choice → 逐标签分数）** | **不可用** | **不可用** | systemone 的 `choice` 返回逐选项校准概率，按 §3.10 映射成每个标签的分数；`choice` / `confidence` 字段不参与映射 |
| decide | **受限** | **受限** | **受限** | **可用（给校准概率）** | **不可用** | **不可用** | systemone 的 `noul` = P(yes)，直接满足 §3.2 decide 契约与 §7.2 门控 |
| rerank | **受限** | **受限** | **受限** | **不可用** | **可用（原生排序）** | **不可用** | systemone 无排序接口；`source: rerank` 走原生 `/v1/rerank`；embedding 只给向量，排序由工具层余弦实现（§5.2），不在槽位内 |
| embedding | **不可用** | **不可用** | **不可用** | **不可用** | **不可用** | **可用（原生向量）** | 只有 `source: embedding` 的 `/v1/embeddings` 满足 §3.2 embedding 契约；其余协议无向量输出通道 |

**这张表是 §3.10「信息充分 ∧ 形状可转换」的判例**：每个「不可用」都能归到输入
信息不足（systemone 不生成文本、无排序 / 向量接口；rerank 协议只返回排序结果，
embedding 协议只返回向量，两者都缺文本 / 概率 / 图像输出通道），每个「受限」归到
输出形状可转但概率非校准或适配未落；
`rerank × rerank` 的「可用」是信息充分且形状可转的原生端点；`classify × systemone`
的「可用」是形状可转换且映射已落（§3.10）。代码侧由 `capability` 模块按本表固化
`source × slot` 判定，作为运行时前置准入的唯一可执行落点——文档表是权威，代码判定
与它逐格一致。不因配置里写了 `serves` 就认为能力成立（§3.11）。

#### json 适配器：通用 chat 端点的 classify / decide 形状

`classify` 与 `decide` 走 §3.10 的 `json` 适配器，与 text 适配器共用同一个 chat 端点与请求/响应结构，差别在 prompt 与输出形状：

| 方向 | 形状 |
|---|---|
| classify：槽位 → 协议 | `temperature = 0`；system 要求只输出一个 JSON 对象；user 给出标签集与待分类文本，要求输出 `{"scores": {"<label>": <0 到 1 的分数>, ...}}` |
| classify：协议 → 槽位 | `choices[0].message.content` 解析成 `ClassifyOutput.scores`，再按 §3.2 校验：scores 的 key 集合等于 labels、每个分数在 [0,1]；兼容纯对象与 Markdown 围栏两种常见外壳 |
| decide：槽位 → 协议 | `temperature = 0`；user 放 state 的 JSON 与编号问题（q0…qN）；system 要求只输出 JSON；`images` 非空时拒绝（带图判定走 systemone） |
| decide：协议 → 槽位 | `choices[0].message.content` 解析成 `{"answers":{"q0":{"probability":...}}}`，按 q0… 还原为输入问题原文的键，再按 §3.2 校验 |

两类分数都是模型自报值、非校准（§3.10 的 json 适配器行）；返回需标 `calibrated: false`，
高风险场景用原生实现或云端复核（§7.2）。形状不符按 §3.11 降级。
`classify` / `decide` 走 systemone 的原生路径见下节，不走本节 json 适配器。

#### text 适配器：通用 chat 端点的协议形状

`summarize` 走 §3.10 的 `text` 适配器。请求是 OpenAI 兼容 chat 端点的通用形状
（`POST {base_url}/chat/completions`，`base_url` 为含 `/v1` 的 API 根）：

| 方向 | 形状 |
|---|---|
| 槽位 → 协议 | `model` 取配置的上游模型名；`temperature = 0`（§3.2 同输入同输出）；`messages` 为 system + user 两条，user 放摘要要求 + `text`（无 `schema` 时明确要求只输出摘要正文、不复述推理过程）；有 `schema` 时把 schema 的 JSON 一并写入 user prompt；`max_tokens` 透传 |
| 协议 → 槽位 | 取 `choices[0].message.content`；无 `schema` 时作为 `SummarizeOutput.text`，有 `schema` 时解析成 JSON 给 `data` |

响应形状不符（缺 `choices[0]`、空 content、有 schema 却不是合法 JSON、`finish_reason=length`、无 schema 时 content 是推理过程而非摘要正文）按 §3.11
形状不符处理：该次降级、连续失败后禁用并告警。**不假设统一包装**（§18.1「响应
解包」）。`schema` 的 JSON Schema 校验需要校验器，属可选层，本轮只做 JSON 形状解析。

#### ordered 适配器：通用 chat 端点的 rerank 形状

`rerank` 走 §3.10 的 `ordered` 适配器，与 text 适配器共用同一个 chat 端点与
请求/响应结构，差别只在 prompt 与输出形状：

| 方向 | 形状 |
|---|---|
| 槽位 → 协议 | `temperature = 0`；user 放 `query` + 编号后的全部候选（`documents`），system 要求只输出排序 JSON；`top_n` 透传但 results 仍须覆盖全部候选 |
| 协议 → 槽位 | `choices[0].message.content` 解析成 `RerankOutput.results`，再按 §3.2 校验：覆盖全部输入、index 指向原始位置、按 score 降序 |

协议无原生排序接口，分数是模型自报值、非校准（§18.1 能力表标「受限」）；
形状不符按 §3.11 降级。这是「通用 LLM 承担 rerank」的转换层；专用 listwise reranker 走下节 `source: rerank` 的原生协议，不复用本形状（§3.5）。

#### rerank 协议：原生 `/v1/rerank` 端点

`source: rerank` 用于上游直接提供排序接口的场景（实例与逐条实测见附件 E.11）。它是独立协议族，不是 `openai` 上的形状变体（ADR 0007）；`adapter: auto` 给它 `native`。

`base_url` 指向服务根地址，客户端自行拼 `/v1/rerank`（与 `systemone` 拼 `/v1/systemone` 同口径）：

| 方向 | 形状 |
|---|---|
| 槽位 → 协议 | `POST /v1/rerank`，`{"model": <models.<id>.model>, "query": ..., "documents": [...]}`；`model` 不做别名推断。与上游启动名是否一致无法在配置期判定（§18.1 不做启动探测），上游返回 `model_not_found` 时按 §18.9 第四层 / §3.11 首次使用校验降级并告警（§7.3），不在配置期 fail loud |
| 协议 → 槽位 | `results[].{index, relevance_score}` → `RerankResult{index, score}`；`document` 等额外字段丢弃；再按 §3.2 校验覆盖全部输入、index 指向原始位置、按 score 降序 |

`RerankInput.top_n` 不透传（上游是否支持本轮未实测）；槽位按 §3.2 返回覆盖全部输入的完整 `results`，截断由工具层按 §5.2 的确定性截断完成（与 `ordered` 路径「`top_n` 透传但 results 仍须覆盖全部候选」对照）。形状不符按 §3.11 降级。能力表中 `rerank` 与 `embedding` 协议对各自其余五个槽位判为「不可用」；`serves` 是声明不是承诺（§3.11），声明阶段不因此拒绝配置，运行时按 §3.11 前置准入拒绝并降级。`relevance_score` 是相关性分数、非校准概率，只用作顺序、不作置信度门控输入（§5.2、§18.1）；`UNCALIBRATED` 告知只适用于自报概率的 `ordered` 路径。HTTP 发送在 `external-protocols` feature 后（§19.6）。

#### vision 适配器的协议形状

`vision` 走 §3.10 的 `text` 适配器，但请求需要多模态 content part：

| 方向 | 形状 |
|---|---|
| 槽位 → 协议 | `POST {base_url}/chat/completions`，`temperature = 0`；user 的 `content` 是数组：一个 `{"type":"text","text":<任务指令 + query>}` 加一个图片 part。`image` 的形状由调用方给（§3.2 不预设），本轮按 chat 端点的 image content part 直接透传 |
| 协议 → 槽位 | `choices[0].message.content` 作为 `VisionOutput.text`；`task=ground` 时要求内容是 `{"boxes":[{"label":...,"bbox":[x,y,w,h]}]}`，解析成 `VisionOutput.boxes`，再按 §3.2 校验 bbox 有限且非负 |

任务指令按 `VisionTask` 固定四档（describe / ocr / ground / classify），不生成自由
指令；形状不符按 §3.11 降级。

**openai / anthropic / responses 是通用 LLM 的协议形态，jev 类判定模型不按 chat
协议工作。** §3.4 的参考判定实现（StartLux / d3-nano）属 systemone 协议族，
对它们只能走 `systemone`；上表 openai 列的 decide 标「受限」，对这类模型是
**不可用**（不是「精度差一点」，是协议层就不对）。判据见 §3.4。

#### systemone：TypeSafe 判定协议

**协议形状**（`POST /v1/systemone`，别名 `/v1/decisions`；官方定义见附件 E.10）：

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

**三种问题类型的官方语义**：`noul` 是「yes 的概率」；`score` 是量表上的等级期望值；
`none_probability` 是「没有选项合适」的告警分（官方建议阈值 0.2）。

**为什么值得单列，而不是硬套 openai**：它直接提供「命名问题 → 校准概率」，
这正是 §3.2 的 decide 契约（每个问题一个 probability）与 §7.2 的置信度门控需要的
形状。openai 协议的 decide 在上表里标「受限」——实测 llama.cpp 的
`/v1/chat/completions` 虽带 `logprobs`（token 级概率），但那是「下一个 token 的
分布」，要映射到「每个问题一个概率」还需要额外约定（多 token 答案如何聚合、
答案在序列中的位置如何定位），本轮不落。systemone 把这层约定做进了协议本身。

**更根本的一条：systemone 是 jev 类判定模型的原生协议。** §3.4 的参考判定实现
（StartLux / d3-nano）按 systemone 工作，对它们走 chat 协议不是「精度差一点」，
是协议层就不对（§3.4 的实测判据）。所以 systemone 值得单列一个 `source`，
而不是硬套进 openai 那一列。

**它就是 §13.4 所说的 decide 槽位形态的原生实现**：「一个 state + 任意数量命名问题 →
带概率的答案集合」逐字对应。§13.4 讨论的方案 A / B（每工具单独调 / turn 级
批处理）因此可以直接作用在 systemone 上，不需要为它再设计一套调用形态。

#### classify：choice → 逐标签分数

`classify` 槽位走 `choice`，映射规则与请求/响应形状见 §3.10「systemone 适配器」。
本节只记协议侧事实：`choice` 问题的 `criteria` 是 **map**（键为选项名、值为该选项的
描述，来源见附件 E.10）；响应 `probabilities` 是全选项的校准概率分布（和为 1），
另给 `choice`（最高概率项）与 `confidence`（分布集中度）。toolgate 只取
`probabilities` 按标签名映射成 §3.2 的 `scores`，**不读 `choice` / `confidence`**
（把 `confidence` 当 `probability` 属静默转换，§3.10 禁止）。`none_probability`
≥ 0.2（官方建议阈值，附件 E.10）表示「无合适选项」，本次按 §3.11 形状不符降级，
不返回分数。真机端到端实测见附件 E.12（d3-nano :8082）；三类原语的混合请求与
二元分解对照实测见附件 E.13。

**本机实测**（2026-11-09，127.0.0.1:8090 的 StartLux-Decision-v1.1-4B-AutoThink）：

| # | state | 问题（措辞） | noul | 含义 |
|---|---|---|---|---|
| 1 | `这段 Python 代码有语法错误：def f(: pass` | 这段代码**有语法错误**吗？ | 0.960 | P(有错) |
| 2 | 同一句 | 这段代码**语法正确**吗？ | 0.017 | P(正确) |
| 3 | `订单号 A-123 已被标记为已发货，运费 0 元。` | 这个订单是**已发货**状态吗？ | 0.997 | P(是) |
| 4 | `今天天气不错，适合散步。` | 这个订单**已经发货**了吗？ | 0.126 | P(是)，state 未提及 |

**第 1、2 行是同一句 state 的互补两面**：0.960 与 0.017 合起来接近 1，因为 `noul`
的语义是 **P(肯定回答为真)**（官方定义「noul is the probability of yes」）。
概率相对 state 成立、方向明确——所以门控按 P(是) 走，与 §7.2 同向：
`p ≥ 0.85` 采用、`0.60 ≤ p < 0.85` 标 LOW_CONFIDENCE、`p < 0.60` 两视图都给。

第 4 行尤其关键：state 里没有订单信息，模型给的是「低概率但非零」而不是编造。
`choice` 类型实测也返回逐选项 `probabilities` 与 `none_probability`（本机一次 choice
实测 `none_probability` = 0.505）。

#### 响应解包：按各自协议形状解析，不假设统一包装

**不同判定服务的响应形状不同，适配器按各自协议解析，不假设存在统一包装。** 本机
两种实测形状：

| 服务 | 端点 | 响应形状 |
|---|---|---|
| 本机 startlux-decide | `POST /v1/systemone` | `{answers: {qid: {...}}, usage: {...}, model: "..."}`——**没有 `ok` / `result` 包装** |
| 本机 typed-decide 网关 | `POST /v1/decide/{noul,choice,score,batch}` | `{ok: true, result: {...}, latency_ms: N}`——**字段在 `result` 内层** |

**遇到带 `ok` / `result` 的形状必须取 `result` 内层**。按顶层直接取
`value` / `probability` 只会拿到空对象——typed-decide 的实测记录里明确记了这个坑
（「本次即因此白跑一轮」，来源见附件 E.10）。这不是理论风险：包装层看起来像
「多此一举」，实现者很容易直接按顶层取字段，而空对象不会报错，只会让概率静默
变成缺失值。

**`latency_ms` 在台账命中时为 0**：所以它度量的是「本次实际推理耗时」，命中缓存
的判定耗时记 0，读它作延迟指标时要知道这一点。

**归一到槽位形状这层不变**：解包只是取到该协议的结果字段，之后仍然按 §3.10 的
规则转成 §3.2 的槽位形状——解包与形状归一是两件事，别混在一起做。

#### 协议选择原则：判定类槽位该用哪个协议

**先分清两件事**：

| | 是什么 | 与协议的关系 |
|---|---|---|
| decide / classify | **槽位契约**（§3.1–§3.2）：输入输出形状与质量要求 | **与协议无关**。§3.0 的「架构定义槽位，不定义模型」在这里同样成立——槽位不绑定协议 |
| `source` | **模型来源**（部署形态） | 是协议的选择，不是能力等级（§18.1 的总纲：模型来源是部署形态，不是能力等级） |

所以问题不是「decide 只能用 systemone」，而是「**能用 systemone 的模型就用它**」。
这条优先级落在配置与绑定：多模型服务同一槽位时优先绑 `source: systemone`（§3.10），
调用层按绑定来源分流——systemone 走原生原语，其余走 `json` 适配器；**不做同一次
调用的跨协议重试**（§3.8 / §3.11）。判定类槽位可以用任何协议——差别在
**概率的性质**，不在能不能做：

| 模型类型 | 协议 | 适配器 | 概率性质 | §7.2 门控 |
|---|---|---|---|---|
| jev 类判定模型 | `systemone` | `native` | **校准概率**（后端按 read-out 概率 + 校准训练得到） | **可用**：直接套 0.85 / 0.60 阈值 |
| 通用 LLM | 自身协议（`openai` / `anthropic` / `responses`） | `json` | **自报值，非校准**（§3.10） | **不可直接套用**：走最保守档 + `UNCALIBRATED` 标注（§7.2） |

**为什么优先 systemone**：它是 jev 类判定模型的原生协议（命名问题 → 校准概率），
走 `native` 适配器直接满足 §3.2 的槽位契约，概率是校准的，§7.2 的置信度门控因此
可用。通用 LLM 也能做 decide / classify，但走 `json` 适配器，概率是模型自报值，
必须标 `calibrated: false` 与 `UNCALIBRATED`（§3.10），门控也只能走保守档。

**不要把通用模型包装成 systemone 形状**，三条理由：

1. **适配器的职责本就是直接归一**：§3.10 的适配器把槽位请求转成该协议的原生形状、
   再把输出转回 §3.2 槽位形状。openai 的原生形状是「prompt + JSON schema 输出
   （或 logprobs）」，不是 systemone 的「命名问题 + noul / choice / score」。强行先转
   systemone 形状再转回槽位形状，只是多一层映射、多一处真相、多一处可能出错；
2. **包装不会凭空产生校准性**：systemone 的价值在于后端是按 read-out 概率加校准
   训练做的判定模型；套一个协议外壳不改变概率性质，对 decide 的核心价值（门控
   可用）没有帮助；
3. **要一致的是语义，不是协议形状**：同一个 decide 请求无论走哪个协议，问的都是
   同样的语义问题（§5.1 的 `has_error` / 错误种类 / `severity` /
   `needs_full_context`），载体不同语义一致——这由**槽位契约与工具层的问题定义**
   保证，不由协议形状保证。

**边界**：本节只管协议选择，不改 §3.2 的槽位契约、也不改 §5.1 的问题定义。

**部署约束**：外部来源（`openai` / `anthropic` / `responses` / `systemone` / `rerank` / `embedding` / `auto` 解析为外部协议时）`mode` 只能是 `http`；`embedded` 只适用于 `source: local`。非法组合启动即失败（fail loud）。`base_url` 不写时用 `slm.http.base_url`。

**数据边界**：外部协议默认只接受 loopback（127.0.0.1 / localhost）。远程 endpoint 会让数据出机器，违背 §8 的隔离目标——必须显式配置 `allow_remote: true` 并在启动时告警；第一期不实现远程来源。

**远程判定源必须带幂等键**：本机服务不计费、重复判定没有副作用，所以本机链路
看不出问题。但配置了远程判定源（`allow_remote: true`）后，同一判定重复发出会
**重复计费**（云端按 token 计价）并可能重复触发上游副作用。

口径：**每一次判定请求都必须带幂等键**，键从 turn 上下文派生——§13.4 的
`_meta.toolgate/turn` 的 `turn_id` + 请求指纹（state 与问题的哈希）。同一判定在
同一 turn 内重试时用同一个键，跨 turn 不复用（台账不跨 turn，避免把上一轮的判定
当成这一轮的答案）。

**请求指纹必须包含问题类型与 `criteria` 顺序**：同一请求可混合 `noul` / `choice` /
`score`（附件 E.13 实测），仅哈希 state 与问题文字不足以区分「同一句话问二元」与
「同一句话问多选」。`score` 的等级编号按 `criteria` 数组顺序、`choice` 存在选项顺序
翻转（本机既有基准：d3-nano 0.56% / StartLux 1.12%），顺序变了答案就可能变，
必须计入指纹；否则台账会把不同形状的请求当成同一判定命中。

**这条现在就写明，是因为它防的是「接云端时漏掉」**：本机链路加不加幂等键都跑得
通，所以它不会以故障的形式暴露出来，只会以账单的形式。

**依赖**：外部协议客户端（HTTP 客户端）属于可选层（Cargo feature `external-protocols`），默认构建不含；纯本地路径不需要它（§19.6）。**默认构建不含该 feature 时，配置了外部 `source` 的槽位按 §3.8 降级**（资源层失败，不是配置错误）：配置本身合法，后端能力缺失属运行期资源问题（§18.9 四层校验的第四层）。`source: rerank` 的客户端同属该 feature 后的可选层；默认构建缺失时按 §3.8 降级并告警（§7.3）。

**启动告警与 `toolgate list` 的展示口径**：降级不能只出现在某一次工具返回里——`serve` 启动时把 `unavailable` / `degraded` 的槽位按原因聚合，打印一条启动告警（含 `reason` 与最少可执行的下一步），满足 §18.9 第四层「降级但告警」；`toolgate list` 的槽位行除状态外同样标注 `reason`。三处（notice 的 `reason=`、工具返回的 `slmStatusDetail`、启动告警与 `list`）读的是**同一份判定**，不允许各自再算一遍。

**实例去重**：本地来源按模型文件路径去重；外部来源按 `base_url + source + model` 去重（§18.1 的实例管理）。

**其他协议**：新增协议（如 Gemini 兼容）按同样方式加 `source` 枚举与可选层适配器，属于扩展，不改槽位契约。

#### 配置形态

```yaml
slm:
  mode: auto                   # auto（默认）| embedded | http
  embedded:
    backend: llama-cpp | mlx   # 槽位未覆盖时使用
    model_dir: ~/.toolgate/models
    max_resident_models: 2
    idle_ttl_seconds: 600
  http:
    base_url: http://127.0.0.1:8080

models:
  d3-nano:   { path: ~/.toolgate/models/d3-nano, mode: embedded, serves: [classify, decide] }
  lfm3-1.7b: { path: ~/.toolgate/models/lfm3-1.7b, mode: http, serves: [summarize] }
# slots 可省略：槽位按 serves 自动绑定（§3.10）
```

`mode` 是模型级字段：不写时继承 `slm.mode`；`auto` 按上面的分档表展开，显式写
`embedded` / `http` 则覆盖默认。`backend` 同理（模型级可选）。

**配置只写模型清单**：槽位按 §3.10 的 `serves` 自动绑定；上例两个模型覆盖三个
槽位（classify 与 decide 共用 d3-nano）。

**配置错误与资源失败要分开**：未知 backend、未知槽位名、模型常驻内存估算超过
物理内存 50% → 启动即失败（fail loud）；模型文件缺失、显存不足 → 该槽位不可用
（§3.8），不阻塞启动。

**参考实现的部署形态**：§3.4 的参考模型当前都以进程外服务验证（MLX :8090、
llama.cpp :8082），embedded 后端尚未实现（§13.1）。这是实现差距，不是架构
默认被否定——进程内后端落地前，`mode` 的实际取值只能是 `http`。

**配置层接受 `mode: embedded`，但运行期不因此报错。** `embedded` 是合法的配置值
（不违反 §18.9 的语义校验），配置层不因「后端未实现」而拒绝合法配置——那是实现
状态，不是配置错误。运行期该槽位按 §3.8 降级并告警（原因写明 embedded 后端
尚未实现），归类为**资源层失败**（§18.9 四层校验的第四层），不是配置错误。
对应到 §19.6：embedded 后端属可选层（feature `slm-embedded`，待实现），默认构建
里没有它，因此默认构建下 `mode: embedded` 一律走降级路径。

### 18.1b 上游模型能力：运行时协商，不是启动声明

**决策：上游模型不支持 Vision 时自动走本地降维；能力判断必须发生在每次调用，
不是启动声明。** toolgate 服务的是 DSH、pi 这类 agent，它们的**会话模型可以
在会话中途切换**。启动时声明的能力在切换后就失效，产生两种错配：

| 配置 | 实际发生 | 后果 |
|---|---|---|
| vision: true | 用户切到纯文本模型 | 返回的 image block 被拒（DSH 会抛 UNSUPPORTED_CONTENT） |
| vision: false | 用户切到视觉模型 | 白白走 OCR，浪费视觉能力 |

**能力判断必须发生在每次调用，而不是每次启动。**

#### MCP 2026-07-28 提供了正确的通道

这个版本的规范取消了 initialize 握手：

> There is no negotiation handshake. Every request carries its protocol
> version, and the server accepts or rejects each request independently.

**版本、身份与能力作为每个请求的元数据传递**（per-request metadata，
放在 _meta 字段）。所以「当前会话用什么模型」可以随每次 tools/call 一起过来，
天然支持中途切换。

#### 但协议没有「模型能力」这个维度

需要说清楚：MCP 规范的 capabilities 指的是**协议特性支持**
（sampling、roots、elicitation 等），**不包含「上游模型是否支持视觉」**。

这是协议层的空白，需要 toolgate 自定义扩展字段。所以下面是本项目的
约定，不是标准。

#### 用户语言（每次调用重新判定）

**决策**：工具层的模型可见文案不绑定中文。语言按
`_meta.toolgate/language`（BCP-47 前缀，如 `zh-CN` / `en`）→ 环境变量
`TOOLGATE_LANG` / `LANG` → 源语言中文兜底 的顺序判定，与上游视觉能力一样
**每次调用重新判定**，不写进启动配置（客户端与会话模型可中途切换）。
中文原文是源语言 msgid，译文表集中在 `locale.rs`，零新增依赖（§19.6）；
未登记的 msgid 回退源语言，不编造。`tools/list` 的 description / inputSchema /
outputSchema 与错误三段式（§1.4.5）都按该语言返回；程序字段（code、路径、数值）不翻译。

#### 三层优先级

```text
1. 请求级 _meta（最高）——客户端每次调用时告知当前模型能力
2. intent 参数         ——客户端显式表达「我要原图」
3. 启动配置            ——默认值，供不支持 _meta 的客户端兜底
4. 保守默认            ——都没有时，不给 image block
```

请求形态：

```json
{
  "method": "tools/call",
  "params": {
    "name": "read",
    "arguments": { "path": "screenshot.png", "intent": "auto" },
    "_meta": {
      "toolgate/upstream": {
        "vision": false,
        "maxImageEdge": 1568
      },
      "toolgate/session": { "session_id": "sess_01" },
      "toolgate/turn": { "turn_id": "turn_01", "seq": 3, "budget": { "max_judgments": 8 } }
    }
  }
}
```

`toolgate/session` 用于 task 表分区（§18.5）；`toolgate/turn` 用于 decide 判定聚合（§13.4），两者都是**可选**字段：缺省时服务端退化为无会话隔离与每工具单独判定，不报错。

#### 保守默认：不确定就不给

客户端不传 _meta、启动配置也没设时，默认 **vision: false**，即不返回
image block，只返回文本理解结果。

理由：**给错比给少更糟**。给了不支持的内容，客户端可能直接报错中断；
给少了，模型还能显式用 intent=visual 再要一次。

#### 这与 §8.6 的「信息量优先」不矛盾

两处的「保守」方向相反，因为场景不同：

| 场景 | 不确定性 | 处理 | 理由 |
|---|---|---|---|
| 隔离回退（§8.6） | 内容敏感度已知，只是给不给 | **尽量多给** | 少给会导致模型瞎猜，猜错成本高 |
| 能力协商（本节） | 对方能不能收未知 | **宁可少给** | 给错会导致硬失败，中断整轮 |

判据是「不确定的是什么」：不确定「该不该给」就多给（有策略兜底），
不确定「能不能收」就少给（有重试兜底）。

#### 客户端侧的实现建议

DSH 已经有 llm.resolveModelInfo() 能拿到 inputModalities，取值填进
_meta 即可。pi 侧对应 model.inputLimits.images。

不实现也可以——toolgate 会退到启动配置，再退到保守默认，只是多一轮往返。

### 18.2 通信网关的三种形态

重新定位后，对外接入收口为「一个常驻 daemon + 各宿主薄适配器」：

| 形态 | 状态 | 用途 |
|---|---|---|
| 常驻 daemon（HTTP / Unix socket，提供 `govern` + `pre`） | 目标形态 | 主路径，多 agent 共享一个实例 |
| 宿主薄适配器（独立仓，一期 pi / dsh） | 目标形态 | 挂宿主接缝 → 转 `govern`/`pre` → 写回；零执行逻辑 |
| MCP Server（stdio，六个工具） | **下线**（可保留 `serve` 仅作调试通道，见 §8） | 不再作为对外集成主路径 |
| 插件 SDK（在宿主进程内跑工具） | **不做** | 见下 |

**为什么不做「插件 SDK」（原判断仍成立）**：插件形态意味着工具跑在宿主进程内、共享宿主
生命周期与故障面；本架构要的是**进程外、可独立升级、多宿主共享**的增强服务，故一律走
「daemon + 薄适配器」，适配器只搬运、不含工具实现（决策 2：核心仓零框架依赖）。

**一个 server 实例 = 一个信任域。** 隔离配置（§8.5）是进程级、改配置要重启（§8.8），所以同一个实例不能同时服务「需要 none 档」与「需要 raw 档」的两个 agent。多信任域的正确做法是**多实例**，各自持有自己的配置；多个实例可以共享同一个进程外 SLM 服务（§18.1），避免重复加载模型。这条同时解释了为什么 §18.2 的「跨 agent 复用」指的是同一信任域内的复用。

**为什么不做插件 SDK**：插件形态意味着工具跑在宿主进程内，共享宿主的
生命周期与内存。这与「本地常驻、跨 agent 复用」的目标冲突，且要为每个
宿主维护一套 ABI。MCP 已经解决了跨 agent 复用的问题。

### 18.3 Task Input / Task Result：这是调用方的契约

原文 §六 定义了两个宏观边界。v2 明确：**它们由调用方实现，但契约在
本文档定义**——因为工具层的行为要与之对齐。

**Task Input**（调用方 → 其 agent 循环）

```json
{
  "task_id": "req_01",
  "goal": "分析截图 design.png，并修复 src/auth.rs 中的空指针",
  "acceptance": ["cargo test 全绿", "不改动 src/ 之外的文件"],
  "budget": { "max_iterations": 5, "max_seconds": 600 },
  "attachments": [{ "type": "local_path", "path": "/tmp/design.png" }]
}
```

**Task Result**（agent 循环 → 调用方）

```json
{
  "task_id": "req_01",
  "status": "partial",
  "checkpoint_id": "ckpt_002",
  "content": [
    { "type": "text", "text": "已修复 auth.rs。日志已折叠，请按需读取 /tmp/bash_out_001.log" },
    { "type": "image", "mimeType": "image/png", "data": "iVBORw0..." }
  ]
}
```

**acceptance 是本地执行的前提**（§21.7.1）：没有它，执行方不知道「尽力完成」
做到哪算完。§18.3 之前只给了 goal，那是云端 agent 自己规划时的写法；交给本地执行
时必须补上验收标准与预算。

**status 的取值**：done（完成且自检通过）/ partial（有进展但未完成）/ failed（尝试后
失败）/ needs_escalation（需要人介入）。后两个的区别是：failed 说明这条路走不通，
needs_escalation 说明需要人做决定。

**工具层在其中的位置**：Task 内部的每一次工具调用是 tools/call，
是 Task 的原子操作。工具层不接收 Task Input，也不产生 Task Result。

这样既保留契约定义，又不与「只有 6 个工具」矛盾。

### 18.4 断点续传的契约

**决策：达到步数上限未完成时，保存 checkpoint_id 并返回 partial。**
工具层不实现，但契约要定义清楚：

| 字段 | 谁产生 | 含义 |
|---|---|---|
| acceptance | 调用方 | 验收标准。本地执行据此判断「做到哪算完」（§21.7.1） |
| budget.max_iterations | 调用方 | 本次任务最多几轮 |
| budget.max_seconds | 调用方 | 本次任务最长多久 |
| checkpoint_id | 调用方（会话层） | 断点标识，可据此恢复 |
| status | 调用方 | done / partial / failed / needs_escalation（取值含义见 §18.3）；**预算耗尽一律 partial**（§21.7.5） |

**工具层能提供什么**：分两类看，不能一概而论。

| 工具 | 可重放？ | 原因 | 调用方要做什么 |
|---|---|---|---|
| read / search | 是 | 输入确定、输出确定、无副作用 | 记录调用即可重建 |
| write / edit / bash（有副作用） | 否 | 重放会重复写、重复执行 | 用自己的 checkpoint 保证「已执行的调用不再执行」；工具层提供 operationId 做幂等键 |

**可重放 ≠ 确定性**：确定性是「同输入同输出」，幂等是「重复执行不重复副作用」。§16.3 只能保证前者。

写工具的返回带 `operationId`：相同 operationId 的重复调用返回首次结果，不重复执行。这是调用方做幂等重试的基础。

### 18.5 执行模型：并发、取消、生命周期

§21.2 要求工具「可编排」，但没说请求怎么并发执行。这一节补上，因为它是 F2（PTC）形态的前提。

#### 并发

- **v1 的串行是过渡实现，不是架构约束。** `McpServer::serve_stdio` 逐请求 `await`，在 PTC 里 `Promise.all` 会退化成排队。
- v2 目标：每请求一个 tokio task，全局并发上限 `max_concurrency`（默认 `min(CPU×2, 16)`）；超出排队，返回 `queueDepth`。请求内的理解链并发（§4.8）属于该请求的子任务，不额外占用全局并发上限。
- 只读工具（read/search）可并行；写工具（write/edit）对同一路径加进程内锁，后写者等待而不是覆盖。冲突检测用「读时记录 (mtime, size, sha256) → 写前复核 → 不一致即拒绝」，不做三方合并（工具层不猜）。
- 写前复核是**默认且不可关闭**的行为：不提供跳过选项，也不因 intent 或隔离档位放宽。不一致即拒绝，按 §1.4.5 返回当前内容与 `nextSteps`（共同前置见 §5.4）。
- 路径锁是**进程内锁**：只串行化本实例内的写。不同 toolgate 实例、客户端内置工具、外部进程同时写同一路径，不在该锁仲裁范围内（§18.6）。

#### 多客户端隔离

- `task` 表按会话分区：从请求 `_meta.toolgate/session` 取会话标识；无标识时按连接隔离。
- 一个 server 实例 = 一个信任域（§18.2）；多信任域用多实例。
- 客户端 A 不能 `kill` 客户端 B 的任务。

#### 取消

- 响应 MCP `notifications/cancelled`：杀掉进程组（不是只杀父进程），释放输出缓冲，把 task 标记为 cancelled。第一期用 POSIX 进程组（macOS）；Windows Job Object 属后续（§19.7）。
- 正在执行的写操作不中途放弃：等当前这一步完成再停（与 §21.7.5「不中途打断」同一条）。

#### 生命周期

- task 退出后保留 30 分钟或最多 100 条，先到先回收；
- task_id 用会话内单调计数器（`t1`, `t2`…），不用时间戳——时间戳会破坏 §16.3 的输出确定性，且当前实现 `Instant::now().elapsed()` 几乎恒为 0，会撞 ID；
- 进程退出时杀掉所有子进程组。

#### 原子写

spill 与 snapshot 用「临时文件 + rename」：否则并发读可能读到半截文件，§21.2 的「并发安全」只是空话。

### 18.6 接入 DSH / pi：拦截内置工具结果，不暴露同名工具（一期仅 pi / dsh）

**决策：toolgate 不提供同名工具、不替换执行；它在宿主接缝处拦截内置工具的结果与写前动作，
就地增强。** 这消除了旧 sidecar 方案的两个病根：同名工具制造选择歧义（§18.6 旧文自陈）、
工具声明常驻请求前缀导致缓存失效。

| 客户端 | 内置工具 | toolgate 的增量（接缝侧） |
|---|---|---|
| DSH | 十几个内置工具（read/bash/edit/write/glob/grep/job_*/web_*/read_image…）+ PTC + output schema | 返回侧成形（截断/摘要/结构化/错误可行动）、写前指纹复核 + 快照、槽位理解 |
| pi | 8 个平铺工具（read/bash/powershell/edit/write/grep/find/ls） | 返回侧成形、写前复核；无 search 聚合与 task，按形状路由照样接住命中清单 |

**两个接缝（已取证，非设想）**：

| 宿主 | 读侧（可改返回） | 写侧（可 block） |
|---|---|---|
| DSH | `tools/post-execute`（accept / replace / enrich / block） | `tools/pre-execute`（block / ask） |
| pi | `afterToolCall`（整段替换 content / structuredContent / details / isError） | `beforeToolCall`（block） |

**免白名单**：钩子是全局的（每次工具 dispatch 必过），处理器按**返回形状**路由而非工具名，
新工具自动覆盖——详情见 §4.0a 与提案 §5。

**逐宿主接入要点**：

1. **适配器独立仓**（决策 2）：薄适配器挂上述接缝，把「工具返回 / 写前请求」转成
   `govern` / `pre` 调用，再把结果写回宿主；**核心仓零框架依赖、零宿主 SDK 代码**。
2. **DSH 配置**：装 `tools/post-execute` 与 `tools/pre-execute` 钩子（注册在宿主侧），
   钩子回调指向常驻 daemon 的本地回环地址；不注册 MCP server、无工具声明。
3. **pi 配置**：装 `afterToolCall` / `beforeToolCall` 扩展，回调同上。
4. **多宿主共享**：pi、dsh 共用同一个常驻 daemon（§18.1 的外部模型来源或反向委派），
   不各自加载模型。
5. **一期范围**：只做 pi 与 dsh；其他宿主按同一接缝模型扩展，不在本期。pi 内置工具无 `task` / `search` 聚合，一期**不覆盖**这两类，只增强 read / bash / edit / write / grep / find / ls 的返回。
6. **嵌套结果不治理**：宿主若给工具调用带嵌套标记（如 DSH PTC 的 `exec.parent` 非空），其结果只回外层程序、不进模型上下文，因此不请求 `govern`；只治理最终进上下文的外层结果。这是适配器契约，不得当作漏治理「修复」——双层治理会让一次调用耗时翻倍，且治理对象错误。

**价值定位**：读侧卖「进上下文前的成形」（高信噪比 / 结构化 / 错误可行动 / 持续处理能力，
§1.6）；写侧卖「内置写也不破坏文件、可回退」。省 token 是读侧成形的**结果之一**（§1.1、
§6.2），不是唯一卖点。

**写侧能力边界：拦截即覆盖内置写（重新定位后的变化）**

| 决策 | 内容 |
|---|---|
| 保护范围 | 经 `pre` 钩子拦截的宿主内置 `write` / `edit`：写前取真值做 (mtime,size,sha256) 复核 + 写前快照，可 block |
| 不覆盖 | 经 `bash` / 脚本 / 格式化器 / 外部进程的写（命令执行前不可知它会写什么，与 §20.4 同口径）；未装适配器的宿主 |
| 原因 | 写前钩子能在**执行前**拿到参数与当前文件真值；命令级副作用无法在执行前枚举 |
| 调用方责任 | 备份/回滚点仅覆盖经 `pre` 的写；命令执行的副作用仍由调用方负责 |
| 失败模式 | 把写前快照当成「一切写都可回退」，误以为 `bash` 副作用也在保护内 |

**与 DSH「先读再改」的关系**：那是会话层兜底（覆盖所有写通道含 `bash`）；本节的写前复核
覆盖文件写工具。两者互补，不因接入 toolgate 而放宽客户端侧约束。证据等级见附件 E.9。

**缓存边界（重新定位后更有利）**：因不再暴露 MCP 工具、不改写工具声明，`govern` 只影响
**工具返回的字节**；只要返回稳定（§16.3 确定性、摘要缓存、图片复用），就不额外制造前缀 miss。
旧 sidecar 的「工具声明进前缀、增删工具即失效」问题随之消失。

### 18.7 统一治理入口：`govern`（读侧）/ `pre`（写侧）/ 反向委派

**读侧主路径（`govern`）：默认不要求 toolgate 取数。** DSH / pi 的 agent 自行选择原生 `web_fetch`、浏览器、`curl`、`lark-cli`、`opencli` 或任何 MCP 工具；结果在进入模型上下文之前，统一送到 toolgate 的 `govern` 接口。这比按来源做 provider 更简单：只有一条治理路径、一个内容契约、一套脱敏/截断/摘要规则；fetch 方式仍由客户端决定，toolgate 不绑定 web 技术栈。

**三块协议合起来就是「统一治理入口」**：读侧 `govern`（结果进上下文前成形）、写侧 `pre`（内置写执行前复核 / 快照 / 可 block）、反向委派（`govern` 需要模型增强、本地又无合适 SLM 时，把单条内容回投宿主会话模型执行）。三者都走本地回环、**都不是 MCP 工具**，因此不产生工具声明、不带工具选择歧义。

#### `govern` 接口

- 传输：本地回环 HTTP 或 Unix socket；**不是 MCP 工具**，避免新增工具声明和工具选择歧义；
- 请求：

```json
{
  "source": { "kind": "web|file|tool", "tool": "web_fetch", "url": "...", "path": "..." },
  "content": [
    { "type": "text", "text": "..." },
    { "type": "image", "mimeType": "image/png", "bytes": 184320,
      "width": 1280, "height": 720, "sha256": "...", "attachmentId": "opaque-host-handle" }
  ],
  "intent": "raw|summary|structured|visual|auto",
  "policy": {
    "isolation": "summary",
    "slmPrefer": "auto",
    "delegate": { "endpoint": "unix:/abs/path/delegate.sock", "token": "opaque" }
  }
}
```

- **图片只传引用与元信息，不传字节**：`attachmentId` 是宿主的**不透明句柄**（DSH 为附件 sha256），
  daemon 不解释、不取字节；`bytes/width/height/sha256` 供预算与去重。`dataBase64` 字段仅作
  **无附件服务宿主的有界降级**（≤256 KiB，显式配置开启），dsh / pi 不用——base64 会体积膨胀约
  1/3、重复编码、绕开宿主图片预算与版本缓存（§10.1、§4.0a ⑥）。
- `policy.slmPrefer` 取 `auto|local|delegate`（缺省 `auto`=本地优先，`delegate`=宿主优先），
  决定本地 SLM 与反向委派谁先试；两档都不可用或失败才降 L0（§3.8），结果来源见 `meta.slmStatus`；
- 视觉分支响应：`structuredContent` 带 `{shape:"vision", needPixels, images:[{attachmentId,...}]}`；
  `needPixels=true` 时原图块原样保留，`false` 时以委派得到的文本替换图块。

- 响应：`{content, structuredContent:{..., notices}, meta:{intent, slmStatus, confidence, redacted, truncated, spillPointer, noticeOverflow}, isError}`；`notices` 与 MCP 工具返回同源（§4.4b）；`meta` 的 redacted/truncated 是派生视图，不单独承载告知；
- 处理链与 `read` 相同：类型/大小检查 → 截断/落盘 → 脱敏（含路径用户名）→ 本地 SLM → 协商；web 内容按不可信来源处理（§15.1）；
- 失败返回结构化 error + next；`govern.fail_open` 默认 `false`，不静默放行敏感内容。

#### 可观测性（`GET /stats`）

daemon 暴露只读聚合计数，用于回答「真实会话里命中多少、降级多少、平均省多少字节」（§15.2）：

- `totals`：请求数、输入 / 输出字节、累计与平均耗时；
- `byShape`：按 `structuredContent.shape` 分桶的请求数、字节与耗时；
- `slots`：每个槽位 `local` / `delegated` / `unavailable` 次数；
- `degradations`：按 notice code（如 `SLOT_UNAVAILABLE`）与槽位聚合的降级次数。

每次 `/govern` 完成追加一行结构化日志（`shape` / `bytesIn` / `bytesOut` / `latencyMs` /
`slots` / `degraded`）。计数只存内存、可随时丢弃，不是会话状态（§22.5）；它不参与治理
决策，也不回写模型上下文。

#### `pre` 接口（写侧）

- 触发：宿主内置 `write` / `edit` **执行前**（dsh `tools/pre-execute`、pi `beforeToolCall`）经薄适配器调用；
- 请求：`{source:{tool,path}, ops:[{op:"write|edit", path, before:{mtime,size,sha256}, payload}], policy:{snapshot:true, verify:true}}`；
- 处理：取当前文件真值做 (mtime,size,sha256) 复核 → 不一致返回 `PRE_STALE` 并 block（next 指示重读）→ 写前快照（§20）→ 一致则返回 `{decision:"allow", snapshotPointer}`；
- 边界：只覆盖经 `pre` 的文件写；`bash` / 脚本副作用不可在执行前枚举，不在保护内（§18.6「写侧能力边界」）。

#### 反向委派（槽位借 agent 会话模型）

`govern` / `pre` 的处理链需要 summarize / classify 等槽位，而部署未配本地 SLM 时，daemon **不自行出网**：它把**单条待处理内容**回投给宿主，由 agent 的会话模型完成该次增强后回填。约束三条：① 只投单条、不带会话历史（隔离微调用，§6.2）；② 出网走宿主既有模型通道，不经 toolgate 新开连接（§18.1 `agent` 来源）；③ 宿主不提供回调时按 §3.8 降级到 L0 机械治理。

协议（wire）：

```text
POST <endpoint>/delegate
请求 { "task": "vision|summarize|rerank|classify", "token": "...", "input": {...},
       "bounds": { "maxOutputTokens": 512, "timeoutMs": 8000 } }
响应 { "ok": true, "text": "...", "model": "..." }
     | { "ok": false, "code": "...", "message": "..." }
```

- `endpoint` 取 `policy.delegate.endpoint`（`unix:/abs.sock` 或 `http://127.0.0.1:port`）；
- `input` 对 vision 只携带 `attachmentId` 与元信息；宿主用**原 image block + 会话模型**作答，
  **不回传字节**；其余 task 的 `input` 是待处理的单条文本/列表；
- `task` 限定为槽位枚举里的增强动作，不接受任意 prompt；`bounds` 由 daemon 给定，宿主不得无上限；
- 失败 / 超时 / 无回调 → L0 降级，`meta.slmStatus` 明确标记（如
  `SLOT_UNAVAILABLE vision fallback=mechanical`）；`token` 仅单次调用内存持有，不落盘不记录。

#### 客户端钩子

| 客户端 | 读侧钩子 | 写侧钩子 |
|---|---|---|
| DSH | `tools/post-execute`（`accept` 替换 content、或 `block`；无 replace/enrich 变体） | `tools/pre-execute`（可 block / ask） |
| pi | `afterToolCall`（可整段替换 content / structuredContent / details / isError） | `beforeToolCall`（可 block） |
| 无钩子的客户端 | 使用 `read({url})` + `fetch.provider` 作为兜底（仅 serve 调试通道） | 无 |

#### `read({url})` 的位置

`read({url})` 保留为**无钩子 / 仅 serve 调试通道**的兜底：它内部调用 `fetch.provider`，拿到内容后仍走同一治理管线。它不是主路径；有客户端钩子时，agent 自己 fetch 后交给 `govern` 即可，不需要 toolgate 再取一次。

### 18.8 fetch.provider 协议（仅 serve 调试通道 / 无钩子客户端兜底，可选）

`fetch.provider` 只在客户端没有 `govern` 钩子、由 `read({url})` 取数时使用。协议版本 1；未知主版本直接 fail loud。

#### 传输

- `stdio`（默认）：toolgate 启动 provider 进程；stdin/stdout 各一行一个 JSON 对象（NDJSON）；stderr 只写日志；一次请求后进程退出。
- `http`：POST 到配置的 loopback endpoint，body 为请求对象，返回响应对象；超时由 `fetch.timeout_ms` 控制。
- 不流式；一次请求只取一个 URL；多 URL 由 toolgate 并发调度（见下面「并发与取消」）。

#### 请求

```json
{
  "protocolVersion": 1,
  "id": "req_uuid",
  "method": "fetch",
  "params": {
    "url": "https://...",
    "headers": { "Accept": "text/html" },
    "timeoutMs": 30000,
    "maxBytes": 10485760,
    "mode": "auto|text|raw|browser"
  }
}
```

`headers` 不应包含模型生成的凭据；认证凭据由 provider 配置持有。

#### 响应

```json
{
  "protocolVersion": 1,
  "id": "req_uuid",
  "ok": true,
  "result": {
    "status": 200,
    "finalUrl": "https://...",
    "contentType": "text/html; charset=utf-8",
    "content": [{ "type": "text", "text": "..." }],
    "bytes": 12345,
    "truncated": false,
    "cache": { "etag": "...", "lastModified": "..." }
  }
}
```

- 二进制/图片用 `{"type":"image","data":"base64","mimeType":"image/png"}`；`bytes` 是解码后大小。
- 失败返回 `{ok:false,error:{code,message,retryable,next}}`；错误码：`FETCH_INVALID_URL` / `FETCH_BLOCKED_URL` / `FETCH_TIMEOUT` / `FETCH_HTTP_ERROR` / `FETCH_TOO_LARGE` / `FETCH_UNSUPPORTED_CONTENT` / `FETCH_PROVIDER_UNAVAILABLE` / `FETCH_PROTOCOL_ERROR`。
- provider 负责 URL 校验、DNS/redirect/SSRF、认证、JS/browser、反爬、重试与正文提取；`finalUrl` 必须是实际落点。
- provider 必须先执行 `maxBytes`；超过时返回 `truncated: true` 或 `FETCH_TOO_LARGE`，toolgate 再复核一次。
- 可选 `method:"describe"` 返回 `{name, version, modes, maxBytes, supportsImages}`，供启动自检；不实现也不影响 `fetch`。
- provider 是独立进程/信任域；toolgate 不把 headers、cookies、URL query 中的凭据写入日志；返回内容一律按不可信来源处理（§15.1）。

#### 并发与取消

- `read({url})` 的多个 URL 请求按 `fetch.max_concurrency`（默认 8）并发；全局仍受 §18.5 的 `max_concurrency` 约束，取两者较小值。
- `fetch.per_host_concurrency` 默认 2：同一 host 限流，不同 host 可并行；provider 若声明更小值，按声明执行。
- 单 URL 失败只影响该 URL；批量结果按**请求顺序**返回（不是完成顺序），失败项带 `error` / `next`，不丢弃其他结果。
- 取消：MCP `notifications/cancelled` 或客户端 abort → 杀掉 provider 进程组 / 取消 HTTP；排队请求直接丢弃。
- `stdio` provider 默认一请求一进程；`http` provider 可长驻并天然并发。provider 不得把多个 URL 串成一个串行循环；toolgate 按 URL 拆分并发。
- `govern` 侧同理：多个 tool result 可并发调用，`govern.max_concurrency` 默认 `min(CPU×2,16)`；本地 SLM 按模型实例有界队列串行（§18.1），慢摘要只阻塞自己的队列，不阻塞其他结果。

`dsh-web-fetch` 适配器直接包装其 `scripts/fetch-auto.py --json`：把单 URL 结果映射成上述 `result`，把 `ok:false` 映射为对应 error code。它的内部多 URL 循环是串行的，所以 toolgate 按 URL 拆成多个进程/请求并行，不把整批交给单个进程。

### 18.9 配置校验与错误处理

配置是用户最容易配错的地方。原则一句话：**配错必须显式失败，不能静默猜；能降级的只在运行时资源层降级，不掩盖配置错误。**

#### 四层校验

| 层 | 例子 | 处理 | 时机 |
|---|---|---|---|
| 语法 | YAML 缩进错、冒号缺失 | 启动失败；报文件:行:列 | 加载时 |
| 结构 | 未知字段（拼写错）、类型错、枚举非法、必填缺失 | 启动失败；**一次报全部错误**，不是只报第一个 | 加载时 |
| 语义 | `slots.<name>` 指向不存在的模型、`serves` 含未知槽位、`mode: embedded` + 外部 `source`、`api_key_env` 未设置 | 启动失败；报字段路径与期望 | 加载时 |
| 资源 | 模型文件缺失、显存不足、端口占用、鉴权失败 | **该槽位不可用**（§3.8 降级），启动告警，不阻塞 | 启动 / 首次调用 |
| 能力不符 | 声明 `serves` 但输出不符合槽位契约 | 首次校验失败 → 槽位不可用 / 禁用（§3.11），首次调用告警 | 首次调用 |

前三层是「配置错误」，必须 fail loud；第四层是「资源失败」，降级但**必须告警**（§7.3 的降级要标注）。分界：配置错误改一行就能修，资源失败要装模型 / 加显存 / 改服务。

**三层之间是短路关系，不是并行收集**：

| 层 | 短路点 |
|---|---|
| 语法 | 解析失败即返回**一条**错误，带文件路径与行列位置；不进入结构层 |
| 结构 | 结构层错误一次列全后**立即返回**，不跑语义层——结构不完整时语义校验只会产生连锁噪音（缺 `models` 定义的 `slots` 引用会报一堆无意义的「模型不存在」） |
| 语义 | 语义层错误一次列全 |

短路不是「少报错误」：每层内部仍然一次列全，只是**下游层不基于不完整结构继续推断**。语法错只报一条是例外——文件都解析不出来，不存在「多个语法错误」的稳定切分。

**错误码**：语法层 `CONFIG_SYNTAX`；结构层与语义层共用 `CONFIG_UNKNOWN_FIELD` 等语义化错误码，每个错误带 `code` / `field` / `value` / `expected` / `fix`（见下节）。

#### 错误格式：可行动

每个配置错误至少四段（与 §1.4.4 同一口径）：

```json
{
  "code": "CONFIG_UNKNOWN_FIELD",
  "field": "models.lfm3-vl.serve",
  "value": "summarize",
  "expected": "serves（槽位名列表）",
  "fix": "把 serve 改成 serves；或检查是否漏写 s"
}
```

- **字段路径必须到叶子**（`models.<id>.serves[0]`），不是「配置有误」；
- 多错误**聚合返回**，一次列全，避免用户改一个重启一次；
- 错误信息**不回显密钥值**（`api_key_env` 只报环境变量名）；
- 退出码非 0（`serve` 与 `config check` 一致）。

#### 未知字段一律拒绝

配置 schema 使用「拒绝未知字段」策略（Rust serde 的 `deny_unknown_fields`）。理由：拼写错的字段如果被静默忽略，用户会以为配置生效了——这是最典型的「配置幻觉」。拒绝未知字段让拼写错误在启动时暴露。

#### 启动前自检：`toolgate config check`

```bash
toolgate config check --config ~/.toolgate/config.yaml
```

- 只做语法 / 结构 / 语义三层校验，不加载模型、不监听端口；
- 输出与启动时一致的可行动错误；
- 用于改配置后、重启前验证，避免「重启—报错—再改」循环。

**「文件不存在」分两种，处理不同**：

| 情形 | 判定 | 理由 |
|---|---|---|
| 默认路径（`~/.toolgate/config.yaml`）不存在 | **不是错误**：全部使用内置默认，退出码 0 | 「无配置文件也能启动」是默认形态（附录 D）；把它判成错会让第一次跑 `config check` 的人以为装坏了 |
| 显式 `--config <path>` 指定但文件不存在 | **是错误**：报可行动错误，退出码非 0 | 用户点名了这个路径，路径写错是最可能的解释；静默通过等于让用户以为检查过的是那份配置 |

**错误聚合**：多错误一次列全（与本节「错误格式：可行动」的多错误聚合同一口径），
不逐个改逐个跑；**退出码**：有错误时非 0，与 `serve` 的退出码语义一致
（本节「错误格式：可行动」：退出码非 0，`serve` 与 `config check` 一致）。

#### 资源失败的降级与告知

- 槽位不可用 → 该能力降级（§3.8），工具继续工作；
- 降级必须写进启动日志与 `toolgate list` 的槽位状态（模型、来源、可用性）；
- **不因资源失败而放宽配置校验**：配置错误（如 mode/source 冲突）不会因为「模型反正没装」而放过。

#### 热加载

不支持（§8.8）：改配置需重启。`config check` 是改前验证的正路；运行期不接受配置文件变更（避免「改了一半」的状态）。

---

## 19. 技术选型：Rust 强约束与「通用」

原文档标题写着「基于 CacheRouter 理念、Pi 附件机制与 **Rust 强约束**的**通用**
本地超级工具」。前两项在 §16 与 §10 展开，这两项补在这里。

### 19.1 「强约束」约束的是什么

不是「Rust 很快」，而是**四类错误从运行时前移到编译期**：

| 约束 | 编译期拦住什么 | 对应的运行时事故 |
|---|---|---|
| 所有权与借用 | 并发读写同一份状态 | 任务表被两个线程同时改，状态错乱 |
| Send / Sync | 把非线程安全的东西跨线程共享 | 异步任务里共享裸指针，数据竞争 |
| Result 强制处理 | 忽略 IO 失败 | 写文件失败但返回「成功」 |
| 无空指针 | 空引用解引用 | 段错误 |

第一条和第三条与本架构的核心命题直接对应：

- 工具执行天然是并发 + IO 密集（§5.5 的 task 表被多个调用共享），
  所有权约束让「谁在什么时候能改它」成为编译期事实；
- 命题三要求「不确定性必须显式失败」，而 Result 类型让「忘记处理错误」
  根本编译不过——这不是靠自律，是靠类型系统。

### 19.2 无 GC 与延迟预算

本地工具层的卖点是低延迟。decide 槽位的 5.7ms 预算里塞不进一次 GC 停顿。（这条只对
槽位模型成立。30B 级 agent 模型由客户端部署、秒级推理，GC 停顿相对可忽略；
它的内存开销属于客户端部署，不在工具层（§21.7.9）。）

更实际的是**延迟可预测性**：工具调用的延迟应该由「命令本身跑多久」决定，
而不是「恰好在哪个时刻触发了 GC」。用带 GC 的运行时写工具层，
延迟分布会有长尾。

### 19.3 单二进制分发

```bash
cargo build --release    # 产出一个可执行文件，无运行时依赖
```

对比：Node 工具要 node + node_modules，Python 工具要解释器 + venv。
「本地常驻工具」的分发成本直接决定它会不会被真的装上去。

**默认构建就是纯 L0 版**（§1.5 的默认层，依赖清单见 §19.6）。**这个卖点只对它成立。** 带 SLM 的发行版需要模型后端：llama.cpp 是 C++ 动态库，MLX 是 Apple 框架，二者都会引入平台绑定。所以发行形态分两类：

| 形态 | 内容 | 单二进制 | 适用 |
|---|---|---|---|
| 纯 L0 版 | 工具执行 + 机械治理 | 是 | 第一期 macOS；不需要本地模型的部署 |
| 带 SLM 版 | 主程序 + 模型后端 + 模型文件 | 否（两件套） | 需要理解与压缩的部署 |

评估 Rust 原生推理（candle）作为第三条路径，若成熟可恢复单二进制。

### 19.4 代价：诚实地说

| 代价 | 具体表现 |
|---|---|
| 开发速度 | 同样的逻辑，Rust 代码量约为 Python 的 1.5-2 倍 |
| 编译时间 | 全量构建分钟级，增量秒级（本仓库 release 约 20s） |
| 异步复杂度 | tokio 的生命周期标注在复杂场景下很难写 |
| 生态 | 图片处理不如 Python（有损 WebP 的编码器就是例子，见 §10.2b） |
| 模型后端 | llama.cpp/MLX 引入 C++/Apple 框架依赖；Rust 绑定成熟度需验证（§18.1、§19.3） |

**这些代价是真实的，不是「习惯了就好」。** 选 Rust 的理由是分发与延迟，
不是「性能」——工具层的瓶颈在模型推理和 IO，不在 CPU。

### 19.5 「通用」的边界

原文档说「通用"本地超级工具"」。「通用」指四个不绑定：

| 不绑定 | 靠什么实现 |
|---|---|
| agent 框架 | 只依赖 MCP 协议（§18.2） |
| 云端模型 | 工具层不知道对面是谁（§11） |
| 本地模型 | 六个槽位，参考实现是五个模型族（jev / jina-reranker / bge-m3 / LFM3 / LFM3-VL），全部可替换（§3、§18.1） |
| 操作系统 | 第一期 macOS；长期目标是不绑定（§19.7） |

**但「通用」不等于「万能」**。§15.3 列了六类不适用场景：
一次性脚本、极短会话、内容即答案、需精确全文、纯 CPU 老机器、数据本不敏感。

一个诚实的架构应该同时说清「它能做什么」和「它不该被用在哪」。

### 19.6 依赖分层与清单

§1.5 的「最少依赖」原则落到技术选型的台账。**默认层就是发行契约**：§19.3 的
纯 L0 版只能包含下表「默认层」的依赖；新增项走 §1.5 的准入四问，并同时更新
本表与附件 E.8。

**默认层：默认构建必需**

| 依赖 | 用途 | 为什么值得这个依赖 |
|---|---|---|
| serde / serde_json | MCP 协议与结构化数据编解码 | 手写 JSON 解析器的边界条件（转义、Unicode、数字精度）远多于依赖成本 |
| tokio | 异步 IO、进程管理、任务调度 | 工具层是 IO 密集的并发模型（§18.5），标准库没有异步运行时 |
| thiserror | 库层错误枚举与 From/Display | 手写样板会在枚举变更时漏实现，而错误契约是 §1.4.5 的硬要求 |
| anyhow | 边界层错误包装（CLI / MCP 传输） | core 对外错误用 thiserror 定义，anyhow 只用于边界层 |
| tracing-subscriber | 运行日志与日志级别 | 可观测性最低要求；只进 CLI |
| tracing | CLI 侧日志调用门面（宏） | 日志实现是 tracing-subscriber，tracing 只是调用门面；重新声明不新增子树规模（tracing 的子树与 tracing-subscriber 的子树高度重叠），但会让 `tracing-attributes` 与 `syn` 2.x 首次进入锁文件（附件 E.8）；core 库不依赖它，日志仍只进 CLI（见下方依赖方向约束） |
| sha2 / hex | 快照与锚点的哈希 | 密码学哈希不能自写 |
| base64 | MCP 图片 content block | 编解码边界条件（padding、变体）多 |
| encoding_rs / chardetng | 编码检测与转换（§5.6） | 编码表与检测统计无法自维护 |
| regex | 机械脱敏（§8.11） | 正则引擎是安全底线，不自写 |
| similar | edit 的 diff 事实返回（§5.7） | 行级 diff 的正确性成本高于依赖 |
| ignore | search 的 .gitignore 语义（§5.2） | 嵌套 .gitignore、全局排除等遍历语义复杂 |
| image | 图片解码与压缩（§10.2） | 格式解码器不能自写；feature 只开 png/jpeg/webp |
| noyalib | 配置文件 YAML 解析（§18.9 / 附录 D） | YAML 1.2 的标量类型推断、缩进、锚点边界条件多，自写成本高于依赖；纯 Rust 零 unsafe，不破坏纯 L0 版单二进制（§19.3）；serde_yaml 与 serde_yml 均已停止维护（附件 E.8） |

四条依赖方向约束：

- **core 库不依赖 anyhow / tracing-subscriber / tracing**：core 的对外错误用
  thiserror 定义枚举；anyhow 只允许边界层（CLI / MCP 传输）；日志实现只进 CLI。
  CLI 可直接依赖 `tracing`（日志调用门面，宏形态）——它只是 `tracing-subscriber`
  的门面，与日志实现同在边界层；但 **core 库不得依赖 `tracing` 或
  `tracing-subscriber`**，边界不变。
- **不用就不声明**：Cargo.toml 的每个依赖必须有源码引用；v1 骨架遗留的未使用
  声明（core 的 `anyhow` / `tracing`、mcp 的 `tracing`）已于 2026-11-08 清退。
  重新声明是这条原则的正常应用：CLI 接入配置层后要写启动日志（§18.9），
  `tracing::warn!` 这类宏需要直接依赖 `tracing`（它是 `tracing-subscriber` 的传递
  依赖，但宏调用必须显式声明才能编译）。
- **禁止版本通配，用主版本约束（`"1"`），精确版本交给 `Cargo.lock`；
  版本取注册表当前最新版，不沿用陈旧版本**。新增依赖直接取注册表当前最新版；
  若因 MSRV 约束被 cargo 的感知解析挡在旧版，**应抬 MSRV 而不是将就旧版**——
  2026-11-09 即如此：`noyalib` 原被解析到 0.0.15（仓库 `rust-version` 1.85 的
  上限），抬到 1.89 后升到 0.0.58。**版本更新单独提交，不与功能改动混在一起。**
- **配置解析与协议编解码同类，归默认层**：解析 `~/.toolgate/config.yaml`
  的 YAML 库与解析 MCP 的 JSON 库是同一类需求（外部格式的编解码），不因「只读
  一个配置文件」而降级为可选层；否则纯 L0 版要么不带配置能力，要么让配置解析
  与协议解析走两套路径。

**可选层：默认构建不含**

| 能力 | 形态 | 隔离方式 | 为什么不做进默认层 |
|---|---|---|---|
| 本地模型推理（embedded） | llama.cpp / MLX 的 Rust 绑定或动态库 | Cargo feature `slm-embedded`（待实现） | C++ / Apple 框架绑定破坏单二进制，native 崩溃会带走整个进程（§18.1） |
| 外部模型协议客户端（openai / anthropic / responses / systemone / rerank / embedding） | HTTP 客户端库 `reqwest`（Cargo feature `external-protocols`） | 默认关闭；配置外部 `source`（openai / anthropic / responses / systemone / rerank / embedding / auto）时启用 | 默认层不引入 HTTP 客户端；纯本地路径不需要（§18.1） |
| WebP 编码 | ffmpeg / cwebp 子进程 | 运行时检测命令是否存在（§10.2b） | 为约 3.5% 的体积收益引入 C 依赖不值得（§10.2b 的三条判据） |
| fetch provider | 外部子进程（dsh-web-fetch / pi / opencli / moli / command） | `fetch.provider` 配置，默认 `none` | 取数能力由客户端生态提供，不内建网络栈 |
| 视觉 / OCR 原生推理 | candle（纯 Rust 推理框架）/ ort（ONNX Runtime 绑定） | Cargo feature `vision-native`（待实现，默认关闭） | 模型后端默认不进默认构建（§19.3 单二进制）；ort 带 C/C++ 动态库且 2.0 仍 RC，稳定性未定（§4.0a 视觉约束⑥） |

**禁止层**：agent 框架（ADR 0001）、数据库 / ORM、Web 框架、模板引擎、
GC 运行时、日志聚合客户端。它们会把「本地常驻小工具」变成「需要运维的服务」，
与分发目标冲突。

**可选层的 HTTP 客户端选型**：`reqwest`（异步、纯 Rust 默认后端 `rustls`、
支持 `json` / `multipart` / `stream` / `gzip` feature）。选它的理由是本项目的形态
匹配：模型服务调用是低并发、长等待的 HTTP 请求，`reqwest` 的异步接口能直接接进
§18.5 的 tokio 运行时，且不引入 OpenSSL 之类的系统级 C 依赖——纯 L0 版仍是
单二进制（§19.3）。默认关闭该 feature 时，`reqwest` 完全不进默认构建。

**可选层原生推理选型（视觉 / OCR）**：两个候选，默认关闭、不影响纯 L0 版（§19.3）。

- `candle-core` **0.11.1**（2026-11-14 查询 crates.io）：纯 Rust、默认 CPU 后端不引 C/C++ 绑定，能保住单二进制（§19.3），首选；但对 ONNX 格式支持有限，更适合 Transformer / safetensors 类模型。
- `ort` **2.0.0-rc.15**（2026-11-14 查询 crates.io）：ONNX Runtime 的 Rust 绑定，能直接跑 RapidOCR 这类 ONNX 模型；但 `ort-sys` 会引入 C/C++ 动态库（破坏单二进制），且当前最新仍是 2.0 RC、稳定性未定，只能进可选层并标注风险。

二者均按 §19.6 的版本纪律取注册表最新版、`cargo update` 单独提交；这是「参考本机 `~/.ocr-tool`」（§4.0a 视觉约束⑥）的 Rust 原生落点。

**默认构建不含 HTTP 客户端时的行为**：配置了外部 `source`（`openai` /
`anthropic` / `responses` / `systemone` / `rerank` / `embedding`）的槽位**按 §3.8 降级并告警**，归类为
资源层失败（§18.9 四层校验的第四层），**不是配置错误**——配置本身合法，缺的是
这个构建里的后端能力。判定发生在启动时发现「该 feature 未编入」，不是配置解析
阶段。

**审计**：发布前跑 `cargo tree --duplicates` 与附件 E.8 对账；默认层出现新的
直接依赖而本表未登记，视为架构变更而不是实现细节。

**仓库 MSRV**：以 workspace `Cargo.toml` 的 `rust-version` 声明为准（当前 1.89，
为默认层依赖实际需要的下限，不抬到本机工具链版本）。抬 MSRV 会改变依赖可解析的
版本范围（cargo 的 MSRV 感知解析会据此选版本），属架构层取舍，改动单独提交，
不与功能改动混在一起（§19.6 的「`cargo update` 单独提交」）。

### 19.7 平台支持：第一期只实现 macOS

**决策：第一期（v2 首版，阶段 1–4）只实现 macOS；Windows 与 Linux 列入后续规划（阶段 6+）。**

依据不是「mac 用户多」，而是平台差异面的成本：把一件事在三个平台同时做对，
等于把进程管理、快照数据面、权限模型、模型后端四套差异一起调；先把 macOS 的
工具 + 槽位 + 治理闭环验证完，再移植，返工面最小。

**平台矩阵**：

| 平台 | 第一期 | 后续规划 |
|---|---|---|
| macOS（Apple Silicon，macOS 14+） | **默认目标**：纯 L0 版与带 SLM 版都支持 | — |
| macOS（Intel） | 不阻塞：纯 L0 版可用，带 SLM 版按 CPU 档降级（§6.3） | 按需优化 |
| Windows | 不做 | 阶段 6+：Job Object、ReFS / CopyFile、ACL、盘符路径 |
| Linux | 不做 | 阶段 6+：FICLONE、POSIX 权限、进程组 |

**第一期与后续的平台差异面**：

| 差异面 | macOS（第一期实现） | Windows / Linux（后续） |
|---|---|---|
| 进程管理 | POSIX 进程组（setpgid / kill） | Windows Job Object；Linux 进程组 |
| 快照数据面 | APFS `clonefile` → `copyfile` 兜底（§20.2b） | Linux `FICLONE`；Windows ReFS / `CopyFile` |
| 模型后端 | MLX（embedded，§18.1）；llama.cpp（http） | llama.cpp / 其他 |
| 路径与权限 | POSIX 路径；文件 0600、目录 0700 | 盘符 / 反斜杠；ACL |
| 依赖分发 | 单二进制 arm64（纯 L0 版） | 后续打包与签名 |

**代码约束**：

- 平台特定代码集中在 `platform/` 模块，用 `#[cfg(target_os = "macos")]` 隔离；通用逻辑不写 macOS 假设（不硬编码 `/tmp`、不假设路径分隔符）。
- **不提前实现 Windows / Linux 分支，不为跨平台预留抽象**（§1.5 最少依赖；不越需求原则见 §4.6）。移植时再补。
- 平台专属依赖（MLX 绑定）只能进可选层（§19.6）。
- 第一期 CI 只在 macOS 跑测试；Windows / Linux 编译检查不作为门禁。

---

