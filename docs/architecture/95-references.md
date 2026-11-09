# toolgate 架构附件 E：参考、比较与实证

> 本文件是 [architecture.md](../architecture.md) 的附件。正文只写**决策**；
> 参考、比较、研究、实测与背景记录统一放这里。正文引用格式为「附件 E.x」。

## E.1 已有调研索引

| 文件 | 内容 |
|---|---|
| [research-notes-external.md](../research-notes-external.md) | 外部事实核实：CacheRouter、LFM3 等 |
| [research-notes-dsh.md](../research-notes-dsh.md) | DSH 真实机制与工具集 |
| [research-notes-pi.md](../research-notes-pi.md) | pi 真实机制与原始设想差异 |
| [research-notes-models.md](../research-notes-models.md) | 本地模型层与 jev 族 |
| [architecture-review-2026-11-07.md](../architecture-review-2026-11-07.md) | 2026-11-07 架构评审记录 |
| [architecture-review-comparison-2026-11-07.md](../architecture-review-comparison-2026-11-07.md) | 评审对比记录 |

---

## E.2 槽位参考实现

> 原正文 §3.4。参考实现是选型记录，不是架构约束；替换判据见正文 §3.3。

### 参考实现清单（原 §3.4）

以下是本机实测可用的实现，**作为参考而非架构的一部分**。

| 槽位 | 参考实现 | 规模 | 关键事实 |
|---|---|---|---|
| classify / decide | **StartLux-Decision-v1.1-4B-AutoThink**（本机 MLX 8bit）与 **d3-nano**（本机 GGUF Q8_0） | 4B / 2.21B | 按本机口径只启用这两个 jev 类实现；d1 系列不在本机，见下方 |
| rerank | jina-reranker-v3.5-mlx | 约 570M | listwise，block_size 125；**cc-by-nc-4.0 非商用** |
| summarize | LFM3-1.7B | 1.73B | 256K 上下文；官方明确不适用 agentic coding |
| vision | LFM3-VL-2B | 1.92B | OCR / grounding / 布局标注 |

**关于 jev 类与本机口径**：jev 不是某个模型，是一类输出形态（受限输出、
带概率、一次前向出结果）。按本机口径，classify / decide 只启用两个实现：
**StartLux-Decision-v1.1-4B-AutoThink**（本机 MLX 8bit 权重，常驻 :8090）与
**d3-nano**（2.21B、Q8_0 GGUF，llama.cpp 常驻 :8082）。其余族内成员
（JevK5、openJev Verdict、NeoHorse-Jev、CLM 等）不纳入本机候选。
d3-nano 容易被叫成 d1，两者不是同一个模型，见下表。

**d1 与 d3-nano 不要混**：

| 指代 | 是什么 | 在哪 |
|---|---|---|
| `d3-nano` | Decision 3.0 最小档，2.21B、Q8_0 GGUF，本机权重 `~/models/d3-nano-gguf/` | llama-server :8082；typed-decide 的超时兜底后端，日志 `via: d3` |
| `d1-2B` | LiquidAI 官方规格：1.92B、128K、H100 单次 5.7ms、不生成 token | 本架构 decide 槽位的参考规格；**本机没有它的权重** |
| `DECIDE_BACKEND=d1` | `jev_engine` 的别名，映射到 `liquid` 后端，跑云端 `liquid/d1-ultra`（OpenRouter，版本 ID `liquid/d1-ultra-20261015`） | 本机 typed-decide 服务 :10300，日志里 `via: liquid` |
| 网关判定主后端 | 当前 `DECIDE_BACKEND=typesafe` / `TYPESAFE_MODEL=jev-1.14.0`，上游 `api.typesafe.ai/v1/systemone` | :4000 `/v1/decide` → :10300 |

所以本机两个 jev 类权重是 **StartLux 与 d3-nano**；d1 系列（无论 d1-2B 规格还是
d1-ultra 云端后端）都不在本机。架构里的 5.7ms 是官方 H100 口径，按 §3.1
不能外推到本机。

**关于 classify 与 decide 的合并**：当前 jev 族同时覆盖这两个槽位，
所以参考实现里合并成一行。但契约上是两个槽位——classify 只吃文本、
候选集小；decide 可能吃图、问题集固定。将来如果出现纯文本的极快分类器
（如 151M 的 GLiClass）和一个多模态判定器分别最优，它们就该拆开。

---

## E.3 本机实测

> 原正文 §6.4、§6.5。实测条件、拟合过程与原始读数保留在此；正文只保留由它们得出的决策。

### E.3.1 prefill：本地模型延迟的一阶项（原 §6.4）

§6.1–6.3 的延迟预算全部是**推理/生成口径**：d1 的 5.7ms、LFM3 的数百 ms。
它们漏了一项——把输入 token 编码成 KV 的 prefill。实测表明这一项在长输入上
主导端到端延迟，且有一笔与输入长度无关的固定开销。

#### 实测条件与结果

同一套探针跑两个模型：关闭前缀缓存，7 档 prompt ×3 次，`max_tokens=1`，
token 数以响应 `usage.prompt_tokens` 为准；拟合
`latency_ms = 固定开销 + ms/token × prompt_tokens`。

| 模型 | 架构 / 位宽 | prefill | 固定开销 | 拟合区间 |
|---|---|---|---|---|
| **LFM3-1.7B-MLX-8bit**（§3.4 参考实现） | hybrid 线性注意力 / 8bit | **1703 tok/s** | **36.4 ms** | 98–7449 token |
| LFM2.5-1.2B-Instruct-4bit（对照） | pure attention / 4bit | 2532 tok/s | 38.8 ms | 109–8420 token |

| 项 | 值 |
|---|---|
| 硬件 / 引擎 | Apple M2 Max，96 GiB；rapid-mlx 1.0.0（MLX） |
| 权重 | 两个模型均 2026-11-07 本机拉取（LFM3 1.7 GiB、LFM2.5 632.7 MiB）；此前本机无任何 LFM 副本 |
| 生成侧（LFM2.5 对照） | 364.6 tok/s（独立基准，短输出）；126.9 tok/s（`bench --tier speed`，greedy 119 token） |
| 短输入 TTFT（LFM2.5 对照） | 19.4 ms（独立基准，7–8 token）/ 60 ms（`bench --tier smoke`） |

**架构口径一律取 LFM3 这一行**：它是 §3.4 的 summarize 参考实现，且 8bit + hybrid
比 4bit 对照慢约 33%。以下换算按 1703 tok/s、36.4 ms 计算。

按 §15.5 标为**实测**：有可复跑的探针与日期；两组的拟合在各自区间内线性良好。
**超过 7449 token 的数字是线性外推**，长上下文的 attention 二次项与 KV 显存压力
会让实际更差，不能当实测用。

用户口径「本地常见 300–1200 tok/s」落在**生成侧**，不是 prefill：本机独立基准
测到 364.6 tok/s，`speed` 档测到 126.9 tok/s，都在这个区间；两个模型的 prefill
实测（1703 / 2532 tok/s）是它的 1.4–8 倍。两者不能混用——架构里把 prefill 速率
与 decode 速率分开记，不写死任何一个数字：模型规模、量化位宽、架构（hybrid 与否）
与上下文长度都会改变它。

#### 三条改判

| 原表述 | 改判 | 依据 |
|---|---|---|
| 「云端按 token 计费，本地按毫秒计费」（§1.1） | 本地按「输入 token / prefill 速率 + 固定开销 + 生成时间」计费 | LFM3 实测拟合 |
| decide 槽位 `< 30ms`（§3.1） | 本机固定开销就 36.4 ms，空输入也超预算；该预算是 H100 短输入口径，不能外推 | LFM3 实测截距 |
| 「LFM3 有 256K 上下文，能一次读完 50KB 日志」（§9.3） | 256K 是窗口上限，不是可用预算；50KB ≈ 12.8K token，实测约 7.6 s | LFM3 实测外推 |

**「prefill 是瓶颈」这个判断仍成立，但理由要换**：不是速率低（1703 tok/s 不慢），
而是**喂给模型的输入规模**与**固定开销**。12.8K token 要 7.6 s，42K 要 24.7 s，
而 §1.1 与 §6.2 的账里写的是「+0.6s」。§3.1 的槽位预算在本机全档不成立。

#### 实测换算

| 输入 | token | 延迟（LFM3 口径） |
|---|---|---|
| 摘要候选片段 | 400 | 0.27 s（实测区间内） |
| 小输出（§6.2 场景 C） | 2 048 | 1.24 s |
| 50 KB 日志 | 12 800 | 7.6 s |
| 4200 行日志（§6.2 场景 A） | 42 000 | 24.7 s（外推） |
| d1 窗口上限 | 131 072 | 77.0 s（外推） |
| LFM3 窗口上限 | 262 144 | 154.0 s（外推） |

反推预算：1 s 内完成 prefill 的输入上限约 `(1000−36.4)/0.5871 ≈ 1641 token`；
2 s 约 3344 token。**所有 >2K 的原文都不该整份喂给槽位模型。**

#### 对策（按实测收益排序）

1. **输入有界化（第一优先，不需要新依赖）**：§9.3 的流水线改为「确定性预筛 →
   候选片段召回（头/尾/错误行/匹配行）→ 拼成 ≤2K 的有界窗口 → 再喂模型」。
   既不漏中间错误，又把 12.8K 的 7.6 s 压到约 1.2 s。比换更快的引擎有效得多。
2. **prefill 估算只用于分块**：调槽位前按上式估算，超预算时改为分块或缩小候选窗口，不跳过槽位调用；估算用
   `usage.prompt_tokens` 的实测值校准，不用字节数猜。
3. **请求级批处理提前（§13.4 方案 B）**：原建议「先做 A，因为 A 的 5.7ms 可忽略」
   在 prefill 口径下反转——批处理的收益从「省几毫秒」变成「把 N 次固定开销与
   N 份 prefill 合成一次」，是数量级的。详见 §13.4。
4. **分块 map-reduce**：确实必须看长文时，按块摘要再归并；每块仍守有界窗口。
5. **前缀缓存：本机尚未验证到收益，不得当既成杠杆**。2026-11-07 用 LFM2.5 与同一份
   ~3941 token 静态前缀连发三次（缓存开启），服务日志 4 次全为 `[cache_fetch] MISS`，
   冷 1316 ms、后续 1281/1281 ms、全新前缀对照 1260 ms，差异在噪声内。原因未定位
   （工具调用语法路径可能每请求注入动态前缀）。在拿到 HIT 证据前，§16 的缓存结论
   只适用于**云端前缀缓存**，不覆盖本地槽位。

#### 影响面（统一在此声明口径，不逐处改文）

- §11.1「IPC 比 5.7ms 推理还大」、§19.2「5.7ms 里塞不进 GC」：这两处数字是
  H100 短输入口径。在 prefill 主导的本机上，IPC 与 GC 相对秒级 prefill 可忽略；
  两条**结论不变**（SLM 默认在工具层内部、Rust 保延迟可预测），但论证锚点应换成
  「固定开销 + 输入有界」，不再是 5.7ms。
- §6.3 硬件表的 Apple Silicon 列目前是估算且只记推理延迟；应补 prefill 速率与
  固定开销两列。
- §13.4 的方案 A/B 建议已按第 3 条改判，见正文。

#### 待测（不阻塞架构定稿）

- `lfm3-vl-2b-8bit`（2.1 GiB）：§3.4 的视觉槽位参考实现，未拉取、未测；
  `lfm3-1.7b-8bit` 已按同法补测（见上表），两个口径并列保留，不互相外推。
- d1-2B「不生成 token」省的是 decode，**不是 prefill**；它的 state 形态与实际输入
  长度未测，§6.1「L1 便宜到可以每个调用都跑」需按此重估（d1 与 d3-nano 的区分见 §3.4）。
- 长上下文（>8K）是否偏离线性、前缀缓存为何全 MISS（该对照用 LFM2.5 测）。

### E.3.2 各槽位本机实测（原 §6.5）

五个槽位按 §3.1 的契约各测一次典型负载，用来校准 §3.1 的延迟预算表。
硬件与口径同 §6.4（Apple M2 Max / 96 GiB / rapid-mlx 1.0.0）；除特别标注外
其余槽位都用本机已有权重（StartLux / d3-nano / jina / PaddleOCR-VL），**没有再为槽位测量下载新模型**；§6.4 的两个 LFM 权重是为 prefill 实测拉取的。

| 槽位 | 本机实现 | 典型负载 | 实测延迟 | §3.1 预算（H100 / Apple Silicon） |
|---|---|---|---|---|
| classify / decide | StartLux（MLX 8bit，:8090）/ d3-nano（GGUF Q8_0，:8082） | 50 / 200 / 800 词 state，2 个类型化问题 | StartLux 350 / 708 / 2745 ms；d3-nano 137 / 231 / 644 ms | <20 / <100 ms；<30 / <100 ms |
| rerank | `jina-reranker-v3.5-mlx`（本地，listwise，block 125） | N=10 / 125 / 300 文档 | 94 / 1818 / 5223 ms | <100 ms 每批；N=300 <300 ms / <1.2 s |
| summarize | `LFM3-1.7B-MLX-8bit`（本地） | prefill 拟合 + 生成 | 1703 tok/s prefill + 36.4 ms 固定开销 | <1 s / <2 s |
| vision | `PaddleOCR-VL-1.6-8bit`（本地） | 65 KB / 191 token 图；233 KB / 2831 token 图 | 264 ms / 5165 ms | <2 s / <5 s |

**读法与边界**

- **rerank 差得最远**：一块 125 文档实测 1.8 s，是「<100 ms 每批」的 18 倍；
  N=300 的 5.2 s 是「<300 ms」的 17 倍。§3.5 说「listwise 300 条只需 3 次
  前向」仍然成立，但**前向次数少不等于延迟低**——每块前向本身就要 1.8 s。
  所以本机档位上 rerank 的延迟只作观测与选型参考（可换更快实现），不因超预算降级或默认关闭；是否触发按 §9.2 的内容判据决定。
- **vision 的延迟由图像 token 数主导**，不是固定「约 1 s」：小图 0.26 s 达标，
  2 倍放大的中文截图（2831 token）5.2 s 已超 Apple Silicon 档 5 s 上限。
  §3.1 给 vision 的预算应改成按图像 token 数分档，与 §6.4 给 summarize 的
  输入有界同理。
- **classify / decide 用本机两个 jev 类模型真实判定，不再是代理**：StartLux
  （MLX 8bit，:8090，`--think-mode never`）与 d3-nano（GGUF Q8_0，:8082）
  都按官方 `/v1/systemone` 格式作答，输出带概率，正是 §3.1 要的受限输出形态。
  StartLux 50 词 state 要 350 ms、800 词 2.7 s；d3-nano 同负载快 2.5–4 倍，
  但它的定位是判定服务的**超时兜底**，不是主判定（§3.4）。两者都**受输入长度
  支配**，与 §6.4 的生成侧结论一致。d1-2B 本机无权重、未测；网关 `d1` 后端是
  云端 d1-ultra，与本机 d3-nano 不是同一个模型，见 §3.4。
- summarize 的生成侧 364.6 / 126.9 tok/s 来自不同采样口径（独立基准短输出 /
  `bench --tier speed` greedy），按 §15.5 都标实测但不可互换。

**对 §3.1 预算表的结论**：H100 口径的五个数字在本机**没有一个成立**。预算必须
按硬件档位重标，并把「输入规模」写进契约——rerank 是候选数、vision 是图像
token 数、summarize 与 decide 是 prompt token 数。§3.3 的替换判据「在本机所在
档位上实测」由此从建议变成必须。

预算只作观测与选型基线，不作运行时门控、不触发降级（§7.3）。§3.1 三档表已按本节实测重标。

---

## E.4 强模型场景的价值分析

> 原正文 §6.6。价值项与模型档位的对比分析保留在此；正文只保留决策。

### 分析正文（原 §6.6）

**先给结论：换强模型不是全面增益，它改变的是价值结构。** 这套架构的底价值
不来自「模型小」，而来自三件事——数据在离开机器前被理解、大对象变成可判定的
结构化结果、机械治理。强模型能把第一件做得更深，但不会自动做好后两件。

#### 价值项 × 模型档位

| 价值项 | 小模型槽位（2B/4B） | 强模型本地（30B 级） | 强模型云端 |
|---|---|---|---|
| 上下文压缩（F1） | 强：成本低、回本快（§6.2） | 中：质量更高，但每步秒级、显存 24G+，要摊薄才回本 | 弱：窗口更大、单价更低，省 token 的空间被压缩 |
| 数据隔离 | 中：understand/summary 两档可用，但只能做粗理解 | **强**：能理解更复杂内容，敏感判定更准 | **无**：数据出机器，§8 的隔离归零 |
| 结构化与确定性 | 强 | 强 | 强（但数据已出机器） |
| 机械治理（截断/落盘/快照） | 强，且是底价 | 强；强模型上下文更贵，治理更值钱 | 强 |
| 延迟/成本 | 最优 | 差：秒级/步（§6.3） | 最差：网络 + token |
| 部署门槛 | 低：单机可跑 | 高：单卡 24G 或统一内存 64G | 低，但依赖网络与外部服务 |

#### 强模型的优势是「理解质量」，不是「压缩」

把强模型用于压缩，是用最贵的能力做最便宜的活。真正值得用强模型的位置是
**高价值、低频率、错一次代价大**的理解任务：复杂代码摘要、长文结构化、敏感
信息判定、视觉 grounding。热路径的 classify / decide 仍应由小模型承担
（§3.1 的延迟预算不因模型变强而放宽）。

#### 三个能把价值做高的方向

| 方向 | 做法 | 为什么提高价值 |
|---|---|---|
| 质量分档 | 槽位按任务价值/敏感度选择「快 / 均衡 / 深」三档模型，热路径永远走快档 | 不为每次调用付强模型的延迟，只在值得的调用上升级 |
| 重槽位独立 | summarize / vision 的强模型实现走进程外、按任务触发、可批处理（§18.1） | 强模型的秒级延迟不污染工具热路径，显存由多实例共享 |
| 离线蒸馏 | 强模型离线生成判定/摘要样本，蒸馏到小模型；运行期仍用小模型 | 唯一不增加运行成本的质量提升路径 |

#### 三个不能提高价值的方向

- **用强模型替换全部槽位**：§3.1 的 decide <30ms、classify <20ms 直接崩塌，
  每个工具调用都摊秒级推理，热路径不可用。槽位是低延迟专用件，不是通用
  模型接口。
- **用云端强模型做槽位**：数据出机器，隔离价值归零；压缩价值又被云端大窗口
  削弱，是两头不靠。
- **强模型进进程内**：30B 级模型进 toolgate 进程会拖垮内存与启动，native
  崩溃风险更高（§18.1）。强模型应走进程外共享服务。

**一句话**：架构的价值上限不取决于槽位模型有多强，而取决于「数据在离开之前
被处理了多少」与「工具返回有多确定」。强模型让前者更深，后者仍要靠工具层的
机械治理。

---

## E.5 dsh-ledger-compact 参照

> 原正文 §11.3。对照表与六条借鉴保留在此；正文只保留 toolgate 的分工决策。

### 对照与借鉴（原 §11.3）

DSH 生态里的 **dsh-ledger-compact** 是会话层压缩插件：它不碰工具执行，只在
「结果进模型前」做定形与历史折页。它和 toolgate 相邻但不同层：

| 层 | 谁做 | 何时 | 作用对象 |
|---|---|---|---|
| 工具层定形 | toolgate | 工具返回前 | 单次 tool result |
| 会话层折页 | dsh-ledger-compact | 发送前（pre-step） | 已产生的历史消息 |
| 官方 LLM compact | DSH 引擎 | 压力触发 | 整段历史 |

**边界结论不变：跨轮压缩归会话层（§11.2），toolgate 不替代它，也不接受
「先发送、后回写」的压缩方式。** 工具层只在结果产生时定形；历史怎么折叠
是会话层的事。**对定位的含义**：客户端已有这类会话压缩时，toolgate 的
压缩价值进一步缩小——历史折页不用它做，它更应聚焦单次结果定形、隔离与
确定性（与 §21.5 的 PTC 削弱同理）。

#### 值得吸收的六条

1. **缓存契约是结构性约束，不是幂等性的结果。** dsh-ledger-compact 只处理
   「刚结束那一步、尚未发送」的 tool result；step - 1 的限制让「不改写已
   发送前缀」成为结构性质，而不是靠实现恰好幂等。toolgate 的对应约束是：
   每次返回的结果一旦发出即不可再改；跨轮变更只能折叠成新卡，不能回写旧
   结果（§16.3 对策五）。
2. **机械基线优先，模型是可选增强。** 它的默认路径是零模型调用的机械折页，
   LLM compact 只是可替换引擎，replaceDefault 默认关。toolgate 的 §3.8
   已有「槽位全空仍可用」，但产品形态要写清：每个槽位的机械降级结果必须
   自己可读、可行动，模型是优化项，不是可用性的前提。
3. **结构化账卡，而不是自由文本摘要。** 折页卡固定七段：STATE / FILES /
   INTENTS / TOOLS / COMMANDS / ERRORS / EXCERPT，文件带「×N」编辑次数与
   「L100-149」读取窗口。它比一段模型写的摘要更可审计、更省 token、更稳定。
   toolgate 的 summarize 槽位应吸收为：**账卡 schema 是摘要输出的最低结构**，
   有模型时生成语义摘要，无模型时生成机械账卡，对外形态一致。
4. **省略必须附带可执行的恢复路径。** dsh-ledger-compact 只在 read 这类
   能按 offset/limit 重读的工具上承诺 re-read；stdout 这类不可寻址的结果
   先落盘再省略，notice 里给路径。对 toolgate 的意义：任何截断/摘要都要
   同时给出 caller 真能执行的恢复动作；假承诺比不压缩更糟。
5. **判定式折叠的四象限与「缺失不等于 0」。** 它用两个问题（还需要吗 /
   重跑能拿回来吗）把候选分成 keep / truncate / drop 三态：只有「不需要
   且可重跑」才允许 drop，且要求两个 margin 都过阈值；缺失的答案一律当
   无判定回退机械卡。toolgate 的 decide 槽位应吸收：drop 类动作的准入最严，
   probability=null 不得解析成 0，低 margin 一律回退保守路径（§7.2）。
6. **降级可观测，静默回退是 bug。** 它的状态命令报告 shaped / snapped /
   excerpted / tokensSaved / failed / lastError，并显示哪些压缩引擎会
   「静默退回模型调用」。toolgate 的观测性（§15.2）应同样把「哪个槽位降级、
   为什么」做成可查询的计数，而不是只写日志。

#### 不吸收的三条

- **不在工具层做历史折页**：那是会话层的职责（§11.2）。
- **不以机械摘要替代语义摘要**：dsh-ledger-compact 自己也承认机械折页只留
  路径、意图、工具次数和短摘录，语义比官方 checkpoint 粗。toolgate 的
  summarize 槽位仍以语义摘要为目标，机械账卡只是降级形态。
- **不复制它的宿主绑定**：它是 DSH 插件，toolgate 是 MCP server，跨客户端。

**一句话**：它的价值在于证明「压缩可以不调模型、可以结构化、可以把恢复
路径写进合同」；toolgate 应吸收这三条，而不是把会话层职责搬进工具层。

---

## E.6 前提修正

> 原正文 §12。完整校正记录也见 [ADR 0005](../decisions/0005-前提修正.md)。

### 修正清单（原 §12）

这些是 2026-11 的事实核查结果，与架构选择无关。完整记录见 [ADR 0005](../decisions/0005-前提修正.md)。

| 原说法 | 修正 |
|---|---|
| 基于 rpi 构建 | 半错：本机安装残缺，项目真实存在（pi_agent_rust / pi-rust） |
| 哈希锚点是 Pi 特性 | 错：hashline 来自 oh-my-pi fork，Claude Code 明确 NOT_PLANNED |
| Apply Patch 是 Pi 风格 | 归因错，但设计保留为 edit 模式 C |
| 图片 1568/WebP/1.2MB 是 Pi 取值 | 自选参数；WebP 是条件触发的优化（§10.2b） |
| 多级压缩继承自 pi | 归因不成立；toolgate 自己的三级见 §9 |
| CacheRouter 支撑端云协同 | 误用：论文只支撑「工具表字节稳定」（§16） |
| LFM3-VL | 正确名称 LFM3-VL-2B（1.92B），LFM Open License v1.0 |
| v1 把 SLM 移出工具层 | v1 的判断错误，v2 已修正（§3、§11.1） |

## E.7 敏感数据分类来源

用于 §8.10 的敏感数据分类与 §8.11 的路径/元数据规则。法规定义不直接等同
技术分类；同一数据可能同时落入多类，实际按项目所在司法辖区取最严要求。

| 来源 | 采用内容 |
|---|---|
| [NIST SP 800-122](https://csrc.nist.gov/pubs/sp/800/122/final) | PII 定义：可区分或追踪个人身份的信息，以及可与个人关联的信息 |
| [GDPR Art. 4](https://gdpr-info.eu/art-4-gdpr/) / [Art. 9](https://gdpr-info.eu/art-9-gdpr/) | personal data 与 special categories（种族、政治、宗教、工会、基因、生物特征、健康、性取向） |
| [OWASP Top 10 A3:2017](https://owasp.org/www-project-top-ten/2017/A3_2017-Sensitive_Data_Exposure) / [OWASP ASVS V8](https://github.com/OWASP/ASVS/blob/master/4.0/en/0x16-V8-Data-Protection.md) | 密码、卡号、健康记录、个人信息、商业机密等敏感数据保护 |
| [CWE-200](https://cwe.mitre.org/data/definitions/200.html) / [CWE-359](https://cwe.mitre.org/data/definitions/359.html) | 敏感信息暴露与个人隐私信息暴露 |
| [PIPL（2021）](https://flk.npc.gov.cn/detail?id=ff8081817b6472a3017b656cc2040044) | 敏感个人信息：生物识别、宗教信仰、特定身份、医疗健康、金融账户、行踪轨迹、14 岁以下未成年人 |
| [PCI DSS Glossary](https://www.pcisecuritystandards.org/glossary/) | 持卡人数据（PAN 等）与敏感认证数据（CVV/PIN 等） |
| [HHS HIPAA De-identification](https://www.hhs.gov/hipaa/for-professionals/privacy/guidance/de-identification/index.html) | PHI 的 18 类标识符 |

## E.8 依赖树实测与审计方法

> 正文 §19.6 的台账依据。数字随依赖版本变化，只作量级参考；审计时以现场
> `cargo tree` 输出为准。

**基线（2026-11-09，配置层接入 `noyalib` + CLI 重新声明 `tracing` + 抬 MSRV
至 1.89 + 全量 `cargo update` 后）**：`Cargo.lock` 共 164 个包（含 3 个 workspace
crate），`[workspace.dependencies]` 直接依赖 18 个；默认构建依赖规模
（`cargo tree --prefix none | sort -u | wc -l`）为 114 行。

**`reqwest` 不在默认构建里**：它是 `toolgate-core` 的 optional 依赖，由
`external-protocols` feature 控制（§19.6 可选层）。默认构建的 `cargo tree` 输出
里 `reqwest` 为 0 条；开全部 feature 后规模为 185 行。

**两个数字不同源，别混用**：164 是 `Cargo.lock` 的包条目数；114 是上面那条
`cargo tree` 命令的去重行数——`cargo tree` 会给重复展开的子树打 `(*)` 标记，
这些标记行也占行数，所以 114 与 164 不是同一个口径的两个版本，比较时不要拿
它们互相验证。

**各直接依赖的传递依赖数**（`cargo tree -p <crate> --prefix none | sort -u | wc -l`，含自身）：

> **`reqwest` 一行的口径说明**：`reqwest` 是 `toolgate-core` 的 optional 依赖，
> `cargo tree -p reqwest` 对它不适用（package ID 匹配不到——它不是 workspace
> 成员且默认未启用）。上表的 79 是从 `cargo tree --features external-protocols`
> 的带缩进输出里解析出的 **reqwest 子树唯一包数**（含自身），与左列的
> `sort -u | wc -l` 是同一口径（唯一包名计数）。该数含跨目标包（本机不编译的
> `icu_*` / windows 相关包），因此偏大。

| 依赖 | 数目 | 依赖 | 数目 |
|---|---|---|---|
| serde | 9 | base64 | 1 |
| serde_json | 5 | encoding_rs | 6 |
| tokio | 14 | chardetng | 8 |
| anyhow | 1 | similar | 1 |
| thiserror | 8 | ignore | 15 |
| tracing-subscriber | 27 | regex | 6 |
| tracing | 11 | image | 23 |
| sha2 | 11 | noyalib | 12 |
| hex | 1 | reqwest | **79**（可选层） |

**审计命令**：

```bash
cargo tree --duplicates                       # 同名多版本
cargo tree --prefix none | sort -u | wc -l    # 默认构建依赖规模
cargo tree -p <crate> --prefix none | sort -u | wc -l   # 单个依赖的子树
```

**2026-11-09 现状**：`--duplicates` 有两组同名多版本：

| 组 | 版本 | 来源 |
|---|---|---|
| `miniz_oxide` | 0.8.9 / 0.9.1 | png 解码链的内部选择，不在直接控制范围；2026-11-08 基线即存在，未新增 |
| `syn` | 2.0.119 / 3.0.10 | 2.0.119 由 `tracing-attributes`（`tracing` 的 proc-macro 依赖）引入；3.0.10 由 `serde_derive` / `thiserror-impl` / `tokio-macros` 引入 |

`syn` 这组是 2026-11-09 新增的：CLI 重新声明 `tracing` 后，`tracing-attributes`
首次进入锁文件并拉入 `syn` 2.x，而既有的 serde / thiserror / tokio 宏链用的是
`syn` 3.x。两条链的 proc-macro 依赖不兼容，`cargo` 不做版本合并——这是「重新
声明一个传递依赖」的代价，如实记录在此。

**结论**：`image`（23）与 `tracing-subscriber`（27）是两棵最大的**默认层**子树——
前者是图片能力的必要成本，后者只允许出现在 CLI crate；`ignore`（15）是搜索语义
的必要成本；`noyalib`（12）是配置解析的必要成本；`tracing`（11）是日志调用门面，
其子树与 `tracing-subscriber` 的子树高度重叠（两者共享 `proc-macro2` / `quote` /
`unicode-ident`），不是一棵独立的新成本。其余默认层直接依赖的子树均 ≤ 14，
没有可轻易替换的大依赖。

**`reqwest`（79）是唯一一棵超过 10 的子树，且它不在默认层**——它是
`external-protocols` feature 下的可选层依赖，默认构建的 `cargo tree` 里为 0 条。
这个数字正是 §1.5 准入四问第 3 问要的答案：「说明为什么不能做成可选层」在这里
反过来了——**它已经被做成可选层**，所以 79 棵子树不进纯 L0 版单二进制（§19.3）。
若将来有人提议把 HTTP 客户端转进默认层，79 这个数字就是必须重新论证的依据
（§19.6 的「默认层出现新的直接依赖而本表未登记，视为架构变更」）。

### YAML 解析库的选型证据

配置层需要解析 `~/.toolgate/config.yaml`（§18.9、附录 D）。三个候选的
crates.io 元数据（2026-11-09 查 crates.io API 与 `cargo info`）：

| 候选 | crates.io 最新版 | updated_at | 维护状态 |
|---|---|---|---|
| `serde_yaml` | 0.9.34+deprecated | 2024-03-25 | 官方仓库已归档停更；版本号本身已带 `+deprecated` 后缀 |
| `serde_yml` | 0.0.13 | 2026-05-27 | crates.io 描述首行即「DEPRECATED — `serde_yml` is unmaintained」，并说明它是转发到 `noyalib` 的兼容壳 |
| `noyalib` | 0.0.58 | 2026-10-15 | 活跃维护；YAML 1.2；纯 Rust、`#![forbid(unsafe_code)]`、serde 集成完整 |

**结论**：默认层用 `noyalib`，不用 `serde_yaml` / `serde_yml`。前者已停更，后者在
自己的描述里声明自己是转发到 `noyalib` 的兼容壳——用后者的路径最终也是
`noyalib`，不如直接依赖它。

**子树的实际构成**（`cargo tree -p noyalib --prefix none | sort -u | wc -l` 实测
19 个）：`serde` / `serde_core` / `serde_derive`（proc-macro）、`serde_ignored`、
`indexmap` / `hashbrown` / `equivalent`、`smallvec`、`itoa` / `ryu`、`memchr`、
`rustc-hash`、`proc-macro2` / `quote` / `syn` / `unicode-ident`。多数（serde 系与
proc-macro 系）已是默认层既有依赖，新增成本集中在 YAML 解析器自身与
`indexmap` / `smallvec` / `itoa` / `ryu` 这几个小工具库。

**版本口径**：`Cargo.lock` 当前锁 `noyalib` 0.0.58（crates.io 最新版），新增传递
依赖 `libm` 0.2.16。

**此前锁的是 0.0.15，不是漏升级，是 MSRV 约束**：0.0.58 的 `rust-version` 是
1.89.0，0.0.15 的是 1.85.0，而当时仓库 `rust-version = "1.85"`，cargo 的 MSRV
感知解析按仓库声明选到 0.0.15。**2026-11-09 按用户要求抬 MSRV 至 1.89**
（noyalib 0.0.58 的 MSRV；不抬到本机工具链版本，避免无谓抬高门槛），并按
§19.6 的「`cargo update` 单独提交，不与功能改动混在一起」纪律升版到 0.0.58。

**抬 MSRV 的连带影响**：noyalib 0.0.58 的子树从 19 降到 12（实现换了），
`tracing-subscriber` 的子树从 20 升到 27。**随后的全量 `cargo update`**（把 21 个
包升到 Rust 1.89 兼容的最高版本，含 `image` 0.25.9→0.25.10、`encoding_rs`
0.8.35→0.8.44、`ignore` 0.4.30→0.4.34）让默认构建规模从 109 升到 114 行。
仓库 MSRV 的口径说明见 §19.6。

**选型落点**：§19.6 默认层台账的一行，准入四问逐条回答见
[architecture-review-2026-11-09-config-layer.md](../architecture-review-2026-11-09-config-layer.md)。

---

## E.9 写侧安全：与 DSH 的对照（2026-11-08）

> 本节是**比较与研究记录**，不是架构约束；写侧决策落在 §5.4、§18.5、§18.6、§20.4。
> 发起原因：结论「修改可以不用先读」在跨项目讨论中被读成「工具层不读文件」，需要钉清「读」的归属。

### 对照表

| 维度 | toolgate | DSH（本机运行时） |
|---|---|---|
| 读的归属 | 工具层 L0 必读原文并记指纹；模型侧 `read(intent=raw)` 取锚点，不回传全文 | agent 侧强制先读，据此拼 `old_string` |
| 脏写防护 | 写前复核 `(mtime, size, sha256)`，不一致即拒绝（§18.5） | 版本守卫 `FsWriteIntent` 是可选参数，省略即无条件覆盖 |
| 失败后 | 返回当前内容 + `nextSteps`（重新 read / 重算锚点） | `FS_STALE_VERSION` → 重读 → rebase → 重试 |
| 可恢复 | 字节级写前快照 + 临时文件/rename 原子写 + 快照路径（§20） | git 与会话日志，无内置文件级快照 |
| 写通道 | 6 个工具是唯一入口；bash 副作用明确不覆盖（§20.4） | 文件工具 / bash / 格式化器 / 脚本 / 多 teammate 都能写 |
| 并发 | 同路径进程内锁（§18.5），跨实例无仲裁 | 会话层 + 外部编辑无 watcher，read 触发刷新 |

### 证据等级

- **架构文档（一手）**：`04-tools.md` §5.4 共同前置、`18-deployment.md` §18.5 / §18.6、`20-behavior.md` §20.4。
- **源码（一手）**：DSH checkout `packages/fs/fs/src/types.ts:117`（`FsWriteIntent` 注释「省略 intent 即无条件 create-or-overwrite」）、`src/index.ts:290`（`editText` 的 `expected?` 可选）、`packages/extensions/tool-cordis/src/api-catalog.ts:1587`（对外描述 omission allows unconditional overwrite）。
- **运行时注入的硬约束（官方指令）**：DSH 提示层要求写前先读、edit 前先读，并声明 bash / 格式化器 / 脚本不完全受版本守卫保护。
- **实测判定**：typed-decide Choice（jev-1.14.0），候选 `keep` / `adopt` / `hybrid`，结果 `hybrid 0.97`、`confidence 0.96`、`margin 0.95`；自检 34 PASS / 0 FAIL。该判定只用于「DSH 运行环境下三选一」，不作跨架构优劣的裁决。

### 结论与落地

1. 「不用先读」不成立：省的是**模型侧的全文回传**，不是工具层的读。toolgate 的读被下沉到 L0，且写前复核默认强制。
2. 机制下沉（指纹复核 / 行锚点 / 字节快照 / 原子写）在设计上更可靠，但成立前提是「所有写都经由受控工具层」。DSH 不满足该前提，故不能照搬。
3. 本次修订据此把边界写成决策：§18.6 明确 sidecar 不覆盖客户端内置工具的写，§20.4 补两行覆盖不到的场景；客户端侧「先读」约束不因接入 toolgate 而放宽。

---

## E.10 systemone 协议的来源与本机实测（2026-11-09）

> 本节是**协议来源与实测记录**，不是架构约束；协议决策落在 §18.1，形状映射落在
> §3.10，验收形态落在 §14 阶段 1。

### 协议来源

- **官方文档（一手）**：StartLuxLabs/StartLux-Decision 的 `docs/api.md` 与 README，
  2026-11-09 取。定义了 `POST /v1/systemone`（别名 `/v1/decisions`）的请求与
  响应形状、`noul` / `choice` / `score` 三种问题类型，以及 `none_probability`
  的告警语义（官方建议阈值 0.2）。
- **官方定义（一手）**：`noul` 是 yes 的概率；`score` 是量表上的等级期望值。

### 本机实测

本机 127.0.0.1:8090 运行 `startlux-decide serve`，模型
StartLux-Decision-v1.1-4B-AutoThink。端点 `POST /v1/systemone` 实跑记录：

| # | state | 问题（措辞） | 类型 | 实测值 |
|---|---|---|---|---|
| 1 | `这段 Python 代码有语法错误：def f(: pass` | 这段代码**有语法错误**吗？ | noul | 0.960（P(有错)） |
| 2 | 同一句 | 这段代码**语法正确**吗？ | noul | 0.017（P(正确)） |
| 3 | `订单号 A-123 已被标记为已发货，运费 0 元。` | 这个订单是**已发货**状态吗？ | noul | 0.997（P(是)） |
| 4 | `今天天气不错，适合散步。` | 这个订单**已经发货**了吗？ | noul | 0.126（P(是)，state 未提及） |
| 5 | `这是一段 Python 代码：def f(: pass` | 这段代码的问题是什么？（criteria：语法错误 / 运行时错误 / 逻辑错误） | choice | `probabilities` = 0.637 / 0.120 / 0.243，`confidence` = 0.455，`none_probability` = 0.505 |

**怎么读第 1、2 行**：同一句 state，把问题方向反过来，概率从 0.960 掉到 0.017，
两者互补接近 1——因为 `noul` 的语义是 **P(肯定回答为真)**（官方定义「noul is the
probability of yes」）。**概率相对 state 成立、方向明确**，所以门控可以直接按
P(是) 走，与 §7.2 同向（≥0.85 采用 / 0.60–0.85 标 LOW_CONFIDENCE / <0.60
两视图都给）。

第 4 行尤其关键：state 里没有订单信息，模型给的是「低概率但非零」（0.126）而不是
编造——概率的连续性正是门控可用的前提。

第 5 行的 `none_probability` = 0.505 说明模型自认「三个选项都不够合适」——这正是
该字段存在的理由（告警而非硬选）。但本轮 §3.10 的 systemone 适配器只映射
`noul`，classify 走 `choice` 的映射未落（§18.1 能力表标「受限」）。

### 对照：jev 类判定模型走 chat 协议会退化

同机实测（2026-11-09）：d3-nano 经 llama-server 的 `/v1/chat/completions` 调用，
返回 `finish_reason=length`，内容是重复 token 序列（形如 `IAHO-YA-IH` 循环，无语义），
`usage.completion_tokens` = 40（撞上限）。**这不是「模型坏了」**，是
「用错协议」的特征——d3-nano 与 StartLux 都是 jev 类 systemone 模型，正确形态是
`/v1/systemone`（命名问题 → 校准概率）。判据与处理见 §3.4。

**第 4 组的意义**：`choice` 类型的 `none_probability` 达 0.505，说明模型自认
「三个选项都不够合适」——这正是该字段存在的理由（告警而非硬选）。但本轮 §3.10 的
systemone 适配器只映射 `noul`，classify 走 `choice` 的映射未落（§18.1 能力表
标「受限」）。

### 对照：openai 协议的 logprobs

实测 llama.cpp 的 `/v1/chat/completions` **带 `logprobs`**（token 级概率），
但那是「下一个 token 的分布」，映射到 §3.2 decide 契约的「每个问题一个概率」还需
额外约定（多 token 答案如何聚合、答案在序列中的位置如何定位）。所以 §18.1 能力表
里 openai 的 decide 标「受限」的结论成立，理由是实测后的具体依据而非待办。
anthropic / responses 的 logprobs 能力本轮未实测，表中保持原状。

**注意与上一节的区别**：logprobs 的「受限」是**能调通、映射缺约定**；jev 类模型走
chat 协议的「退化」是**协议层就不对**。两者不是同一个问题。

### 对照：本机 typed-decide 网关的判定接口

> 来源：本机 `~/.agents/skills/typed-decide/references/gateway-and-backends.md` 与
> `typesafe-official-skill.md`（2026-11-09 取）；官方文档 docs.typesafe.ai。
> 本节是**接口事实记录**，对齐口径落在 §18.1 与 §13.4。

**端点与形状**（网关 `:4000/v1/decide/*`，Bearer master key）：

| 端点 | 入参 | 出参（`result` 内层字段） |
|---|---|---|
| `POST /v1/decide/noul` | `state, question, criteria?` | `probability, value, margin` |
| `POST /v1/decide/choice` | `state, question, options[]` | `value, probability, margin, distribution, confidence` |
| `POST /v1/decide/score` | `state, question, levels[]` | `value, level, margin, distribution, confidence, top_probability` |
| `POST /v1/decide/batch` | `state, questions{qid:{type,instructions,criteria}}` | `{qid: 与单问同构的结果}` |

**响应是三层包装**：三个 POST 判定端点统一返回 `{ok: true, result: {…}, latency_ms: N}`，
字段在 `result` 内层。**按顶层直接取 `value` / `probability` 只会拿到空对象**——
来源文件明确记了这个坑（「本次即因此白跑一轮」）。`GET /v1/decide/health` 是例外：
字段平铺在同一层，没有 `result` 包装。`latency_ms` **在台账命中时为 0**。

**幂等台账**：判定统一走网关 client，带幂等键；台账按
`sha256(primitive + model + state + question + 选项)` 落盘，同一输入永远同一决策，
命中时零上游调用、延迟降到 0ms（返回 `cached:true`）。这是本机链路「重复判定没有
成本」的原因，也是 §18.1 要求**远程判定源必须带幂等键**的参照。

**TypeSafe 三原语**：`noul`（yes/no 概率）、`choice`（候选择一并给逐候选分布）、
`score`（有序量表，给逐级分布与期望等级）。本机 `:8090` 的 startlux-decision 原生
提供 `/v1/systemone`（命名问题 + noul/choice/score），与上面的 batch 形态同源——
toolgate 上一轮已接它（§18.1）。

**与 toolgate 的关系**：判定服务侧的三原语比 §3.2 的 decide 契约**更宽**（契约只有
「每个问题一个概率」，没有类型维度）。这个差距本轮评估过「要不要给契约加类型
维度」，结论是**不采纳**（理由与重新评估的触发条件见
[architecture-review-2026-11-09-typed-decide-align.md](../architecture-review-2026-11-09-typed-decide-align.md)
第 5 节）——**§3.2 正文保持不动**.
## E.11 原生 rerank 端点实测（2026-11-10）

用途：ADR 0007 新增 `source: rerank` 的上游事实依据。按 §4「正文只写决策」，
实测 / 背景 / 实例一律放附件 E，架构正文只留指针。

**服务**：`com.lanws.rmlx-retrieval`（launchd `RunAtLoad` + `KeepAlive`）常驻
127.0.0.1:10020；启动脚本 `~/workspace/tools/retrieval/scripts/rmlx_retrieval_service.py`
带 `--rerank-model /Users/lanws/.models/jina-reranker-v3.5-mlx-merged`、
`--embedding-model /Users/lanws/.omlx/models/mlx-community_bge-m3-mlx-8bit`、
`--rerank-model-family jina3 --port 10020`。

**实测命令与条件**（本机 macOS、2026-11-10、loopback、无鉴权；以下数字均为**实测**）：

| 命令 | 条件 | 结果 |
|---|---|---|
| `curl -sS http://127.0.0.1:10020/v1/models` | GET | 列出 `mlx-community_bge-m3-mlx-8bit` |
| `curl -sS -X POST http://127.0.0.1:10020/v1/rerank -H 'Content-Type: application/json' -d '{"model":"/Users/lanws/.models/jina-reranker-v3.5-mlx-merged","query":"apple","documents":["apple fruit","a car"]}'` | 2 个候选 | `results[0].relevance_score=0.641352117061615`（index 0）、`results[1].relevance_score=0.4527008831501007`（index 1），按分数降序；`usage.prompt_tokens=166` |
| 同上，`model` 改为 `bge-m3` | 2 个候选 | `model_not_found`，提示只能用启动时的 `--rerank-model` 路径 |
| `curl -sS -X POST http://127.0.0.1:10020/v1/chat/completions -H 'Content-Type: application/json' -d '{"model":"bge-m3","messages":[{"role":"user","content":"hi"}],"max_tokens":5}'` | 最小 chat 请求 | `Model not loaded`（该服务未加载 chat 模型） |
| `curl -sS http://127.0.0.1:10020/openapi.json` | GET | 注册路径含 `/v1/rerank`、`/v1/embeddings` |

**口径**：上表只用于说明端点形状，不作延迟 / 质量基准。`jina-reranker-v3.5-mlx-merged`
的 listwise 块大小 125 出自 §3.5 的参考实现选型记录（**引用**，非本机实测），
不在本表内。OpenViking 已在消费该端点（用户 2026-11-10 告知）。
