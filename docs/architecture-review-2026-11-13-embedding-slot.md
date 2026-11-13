# toolgate 架构独立评审：embedding 槽位、search pattern/query 拆分与槽位自动绑定（2026-11-13）

> **评审对象**：工作区未提交改动，基线 HEAD `d91029d`。21 个已跟踪文件修改（`+627 / −169`），另新增未跟踪 `crates/toolgate-core/src/slm/embedding_protocol.rs`（125 行）；`docs/reports/` 下的既有 HTML 报告不入评审范围。评审开始时 `git status --short` 非空，故本报告评审的是工作区，而不是某一个提交。
> **评审依据**：仓库根 `AGENTS.md` §2 红线、§3.4 架构变更、§3.5 决策与 ADR、§3.6 评审与闭环、§4 文档规则、§5 实现规则、§6 验证；`docs/architecture.md` 与 `docs/architecture/` 分册（03-slots / 04-tools / 06-cost / 15-risk / 18-deployment / 20-behavior / 90-appendices / 95-references / 11-runtime）；`docs/upgrade-v1-to-v2.md`；`docs/implementation-status.md`。
> **评审性质**：独立评审。本报告是历史证据，只作待办来源；修复落在架构正文与代码，不回写本报告结论。
> **纪律**：本次只写本报告一个文件；未修改架构正文、ADR 或任何代码；未执行 `git add` / `git commit`；未启动或重启任何服务；未对任何端口发起探测；未下载模型或权重。

## 1. 评审对象

| 文件 | 改动 | 内容 |
|---|---|---|
| `docs/architecture.md` | +1 / −1 | 总目录「能力槽位」由五个改为六个 |
| `docs/architecture/03-slots.md` | +23 / −11 | §3.1 增 embedding 行；§3.2 增 embedding 契约；§3.8 降级表增行；§3.10 概念表 / adapter 表 / 解析规则补 embedding |
| `docs/architecture/04-tools.md` | +39 / −19 | §5.2 重写为「pattern 匹配 + 语义重排」，增 pattern/query 分工、TopK 阈值、embedding 退路与失败告知 |
| `docs/architecture/06-cost.md` | +3 / −3 | `slmStatus` 由五键扩为六键 |
| `docs/architecture/15-risk.md` | +1 / −1 | 契约测试覆盖六槽位 |
| `docs/architecture/18-deployment.md` | +21 / −18 | §18.1 source 表加 embedding、base_url 口径、auto 推断、能力矩阵六源×六槽、部署约束与可选层清单 |
| `docs/architecture/20-behavior.md` | +1 / −1 | 行为快照的槽位计数改为六 |
| `docs/architecture/90-appendices.md` | +1 / −1 | 附录 C「槽位」术语改为六 |
| `docs/architecture/95-references.md` | +1 / −1 | E.3.2 标题由五改六（实测表未同步，见 P1-4） |
| `docs/implementation-status.md` | +5 / −4 | 2026-11-13 状态、embedding 行、基线 462 |
| `docs/upgrade-v1-to-v2.md` | +15 / −1 | 新增 search 输入参数迁移节（query→pattern） |
| `crates/toolgate-core/src/capability.rs` | +29 / −3 | 六源×六槽能力矩阵、embedding 行、`source_slot_usable`、测试扩展 |
| `crates/toolgate-core/src/config.rs` | +37 / −15 | `ModelSource::Embedding`、`SLOT_NAMES` / `SOURCE_VALUES`、adapter 解析、默认 mode、auto 绑定与测试 |
| `crates/toolgate-core/src/search_path.rs` | +83 / −14 | `RerankState::Embedding`、`plan_search` 五参数、`embedding_order` / `cosine` |
| `crates/toolgate-core/src/slm/client.rs` | +169 / −5 | `EmbeddingOutcome`、`SlotClient::embedding`、`embedding_over_http`、失败分类与健康状态 |
| `crates/toolgate-core/src/slm/mod.rs` | +3 / −2 | 导出 embedding 协议模块 |
| `crates/toolgate-core/src/slm/embedding_protocol.rs` | 新增 125 行 | `/v1/embeddings` 请求构造、响应解析、5 个单元测试 |
| `crates/toolgate-core/src/slot_state.rs` | +5 / −4 | 六槽位状态数组与索引 |
| `crates/toolgate-core/src/slots.rs` | +69 / −9 | `SlotKind::Embedding` / `SlotSet`、`EmbeddingInput` / `EmbeddingOutput`、validate |
| `crates/toolgate-core/src/tools/mod.rs` | +1 / −1 | 注册新的模块路径 / 类型 |
| `crates/toolgate-core/src/tools/search.rs` | +117 / −52 | schema 改 required pattern、pattern/query 分离、plan_search 接线、`embedding_rerank`、失败改 Unavailable+notice、meta 加 pattern |
| `crates/toolgate-core/tests/agent_consumption.rs` | +3 / −3 | 契约测试的槽位清单改六 |

核对基线：`git diff --numstat` 汇总 627 插入 / 169 删除；未跟踪的 `embedding_protocol.rs` 与 `docs/reports/` 不在 `git diff` 统计内。

## 2. 重点核查结论

1. **范围与流程**：本次未新增第 7 个工具，未改六个工具的 ToolSpec 名称 / 返回形态 / 错误码语义（search 只改参数 schema 与重排路径），未引入依赖、数据库、Web 框架或 agent 框架；HTTP 仍在 `external-protocols` feature 后（`crates/toolgate-core/src/slm/embedding_protocol.rs:3-4`、`docs/architecture/18-deployment.md:228`），没有把可选层打开成默认。**但**这是「槽位契约 + 工具参数 + 部署协议」的架构变更，按 `AGENTS.md` §3.4 应先改架构正文再实现；当前工作区是「正文与代码同改未提交」，评审通过前不应把代码当作已定契约。另外 `AGENTS.md:24` 红线仍写「五个能力槽位」，与本次六槽位新增不一致（P2-1）。
2. **文档一致性：六槽位大体同步，但残留五槽位与一处反向条款**。已同步：`docs/architecture.md:28`、`03-slots.md:424`、`06-cost.md:286-289`、`15-risk.md:161`、`18-deployment.md:158-165`（六源×六槽矩阵含 embedding 行/列）、`20-behavior.md:207`、`90-appendices.md:67`。未同步：`03-slots.md:10`（正文仍「五个能力槽位」）、`README.md:12` / `:39` / `:41` / `:44-50`（槽位表仅五行，缺 embedding）、`11-runtime.md:97` / `:214` / `:230-231`、`AGENTS.md:24`；以及 CLI / locale / config / capability 的注释与测试文案（P2-1、P2-2）。**反向条款**：`03-slots.md:228` 仍写「embedding 缺失时按命中顺序返回（不降级到向量排序）」，与 `04-tools.md:806-808`、`:832-834`、`18-deployment.md:165` 和实现 `search_path.rs:113-119`、`tools/search.rs:144-153` 直接矛盾，是本次唯一的 P0。
3. **embedding 契约与 §3.2 / §3.10 / §18.1 自洽性**：`03-slots.md:106-112` 的 `{texts: string[]}`（长度 1-32）→ `{vectors: number[][]}`（等长、同序、同维、不改变原文本）与 `slots.rs:250-293` 的字段 / validate 基本一致；`03-slots.md:411` 新增 `source: embedding → 槽位 embedding → native`，`18-deployment.md:152` 的 base_url 口径（不含 `/v1`，客户端拼 `/v1/embeddings`）与 `embedding_protocol.rs:26-28` 一致；`18-deployment.md:145-149` 的三条「不参与 auto 推断」已含 embedding；`capability.rs:170-181` 的 embedding 行只有 `embedding × embedding` 为 Available，其余 Unavailable，与 §18.1 表一致。**不自洽处**：§3.8 降级表反向（P0-1）；`18-deployment.md:129` 的 YAML 注释 source 枚举仍缺 embedding（P2-4）；`03-slots.md:350` / `:420` / `:436` 仍只算 systemone/rerank 两族与四个槽位（P2-3）。另有文档未明说：`source: local` / `auto` 解析到本地的 embedding 在能力矩阵中是 Unspecified，运行时会被 `client.rs:916-990` 的「必须恰好 Available」拒绝；这符合 §3.11，但「本轮只实现 source: embedding，本地向量后端未落」应写一句（P3-2）。
4. **代码与文档契约一致性**：`slots.rs` 六种 `SlotKind` / `SlotSet` / embedding 输入输出已接上；`config.rs` 的 source / slot / adapter / default mode 已加 embedding，`build_bindings` 按「slots 显式覆盖 → serves 含该槽位且 systemone 优先 → models 顺序第一个」实现，与 §3.10 解析规则一致；`capability.rs` 矩阵逐格与 §18.1 一致；`slot_state.rs` 六格状态机一致；`tools/search.rs` 的 schema required 已改为 `["mode","pattern"]`，`pattern` 必填、`query` 可选，优先 rerank、其次 embedding、失败改 Unavailable + notice，meta 增 `pattern`，基础契约正确。**偏差**：files 模式仍走重排（P1-1）；`pattern` 的文档 / 工具描述称 content 也支持通配，而实现 content 只做正则（P1-2）；embedding 退路的候选总量无上限（P1-3）；`embedding_protocol.rs` 不校验 index 唯一/连续（P1-5）。
5. **安全 / 有界性**：新增路径无索引、无持久化、无跨调用状态；任一批失败即整体放弃（`search.rs:254` 的 `ok()?`），调用方把槽位标 Unavailable 并发 notice，不谎报 `applied`（`search.rs:154-166`）；失败错误带 `CAPABILITY_MISMATCH` 与三条可执行 nextSteps（`embedding_protocol.rs:13-23`），符合 §1.4.5 与 §4.6。**不足**：`max_results` 无上限、embedding 批数与单条文本字节数都无总预算，最坏可对 20 万文件发起约 6250 次串行 HTTP（P1-3）；同一响应内重复/越界 index 或跨批维度漂移可静默产出错位向量（P1-5）。边界基于既有 client 的 HTTP / API key / feature 开关，未新开网络面。
6. **测试与验证**：`cargo test --workspace` 退出 0，463 passed（core 437 + 4 + 4 + mcp 18）；`./scripts/check-doc.sh` 退出 0。新增 `embedding_protocol.rs` 5 个单元测试覆盖请求形状、空 texts、按 index 重排、缺 data、endpoint 去重；`search_path.rs` 有 rerank 不可用 / 两者不可用 / 无 query 的 plan 测试。**缺口**：工具级无 files 模式重排、无 no-query、无 embedding 成功/失败、无 >32 批、无 schema required、无 `max_results` 交互测试；`slots.rs` 无 embedding validate 的反例；`degrade_action` 测试只断言非空，未断言 embedding=KeepHitOrder（P2-7）。验证结果见 §4。

## 3. 发现

### P0（阻塞级，契约错误）——1 条

**P0-1 §3.8 降级表仍禁止 embedding 向量退路，与 §5.2、§18.1 和实现相反**

- 证据：`docs/architecture/03-slots.md:228`「embedding | 语义向量重排 | 命中 > 20 且 rerank 不可用时按命中顺序返回（不降级到向量排序） | 是」；`:224` 的 rerank 行也只写「按命中顺序返回（遍历顺序稳定）」。
- 对照：`docs/architecture/04-tools.md:806-808`「否则若 embedding 可用……按余弦相似度排序」「两者都不可用：按命中顺序返回」；`:832-834`「rerank 不可用而 embedding 可用且有 query → 对 query 与每个候选行分别取向量……按余弦相似度降序排序」；`18-deployment.md:165` 能力表 `embedding × embedding` 可用；实现 `crates/toolgate-core/src/search_path.rs:113-119` 在 rerank 不可用、embedding 可用时产出 `RerankState::Embedding` / TopK，`crates/toolgate-core/src/tools/search.rs:144-153` 调 `embedding_rerank`。
- 依据：`AGENTS.md` §3.2「代码与文档冲突时不要自行选边，先报告」；§3.6 评审闭环；`docs/architecture/03-slots.md:219-229` 是槽位缺失的规范降级表，`04-tools.md:832-834` 是工具侧落地口径，两者必须一致。
- 影响：这是规范条款，不是措辞；实现者按 §3.8 会删掉 embedding 退路，按 §5.2 会保留，评审无法给出唯一验收标准。若按现状发版，同一配置在「文档承诺」与「实际行为」之间二选一，属于对外契约错误。
- 修复方向：将 `03-slots.md:224` 的 rerank 行改为「embedding 可用则按余弦重排；否则按命中顺序（遍历顺序稳定）」；将 `:228` 改为「rerank 可用时无影响；rerank 不可用且命中 > 20 时按 embedding 余弦重排；rerank 与 embedding 都不可用时按命中顺序返回」。不要保留「不降级到向量排序」这半句。

### P1（清零后才能进入实现阶段）——5 条

**P1-1 files 模式未被排除在重排之外，直接违反 §5.2**

- 证据：`crates/toolgate-core/src/tools/search.rs:89-97` 按 `mode` 取 `raw`（content 走 `search_content`、files 走 `list_files`），但 `:99-167` 的 `plan_search` + TopK / rerank / embedding 分支完全不判断 `mode`；files 命中是文件路径，仍会送 rerank 或 embedding。
- 对照：`docs/architecture/04-tools.md:821-822`「files 模式不重排：命中是文件路径，语义相似度对路径几乎没有意义；files 模式只用 pattern 做文件名子串 / 通配匹配」。
- 影响：语义模型对路径打分既无意义又增加延迟与外部请求；`meta.rerank` 可能标 `applied` / `embedding`，但文档承诺 files 永不重排；与 P1-3 的请求量叠加时放大成本。
- 修复方向：给 `plan_search` 增 `mode` 参数，或在 `search.rs` 的重排入口加 `if mode == "content"` 门；files 分支只保留 pattern 匹配与确定性截断。补回归测试 `files_mode_never_reranks`（构造 >20 个路径且 mock rerank 可用，断言不发调用、meta 不为 applied）。

**P1-2 pattern 契约过度承诺：文档称 content 支持通配 / 子串，实现只做正则**

- 证据：`crates/toolgate-core/src/tools/search.rs:30`（顶层 description「pattern（正则 / 通配 / 子串）」）、`:33`（英文同）；`:42-45` 的 schema 是正确的「content 正则；files 文件名子串或通配」；content 实现 `:273-274` 只做 `regex::Regex::new(pattern)`，错误文案「正则无效」；files 实现 `:431-446` 才用 `glob_to_regex`。
- 对照：`docs/architecture/04-tools.md:801`「pattern：正则 / 通配符 / 文件名子串」；`:814-815`「pattern 决定哪些行算命中（正则 / 通配 / 子串）」；`docs/upgrade-v1-to-v2.md:73`「匹配表达式（正则 / 通配 / 子串），content 与 files 都必填」；`:76-77`「只做纯匹配……行为不变」。
- 影响：旧调用方若按文档在 content 写 `*.rs` / `foo`，升级后得到 `INVALID_INPUT`（「正则无效」）而不是可执行迁移；工具描述还会让 agent 生成不可用的 pattern。文档与 schema 已经互相矛盾，属于契约层问题，不是实现细节。
- 修复方向：二选一，建议收敛文档而不是扩实现（正则与通配的混合语义难界定）：(a) 统一为「content 只接受正则，files 接受子串 / 通配」，同步 `search.rs:30/33`、`04-tools.md:801/815`、`upgrade-v1-to-v2.md:73/77`，并在迁移节写明「content 旧 query 中的通配写法会失败，需改写为正则或改用 files」；(b) 若坚持 content 支持通配，则内容检索先做 glob 判定再回退正则，并定义 `*` / `?` 的转义与大小写规则。选择后跑 check-doc，并补 `pattern_contract` 测试（content `*.rs` 按选定语义成功或给出明确错误）。

**P1-3 embedding 退路的候选总量与单条文本无界，文档「有界 / 一次批量调用」不实**

- 证据：`crates/toolgate-core/src/tools/search.rs:87` `max_results` 默认 200，schema `:54-58` 只有 `minimum: 1`，无 maximum，运行期也不 clamp；`:250-256` 以 `texts.chunks(32)` 串行循环，每批一次 `client.embedding(...).await`；`search_path.rs:236-238` 的 `WalkLimits::default()` 允许 `max_files = 200_000`（`search_path.rs:415` 的测试断言），因此最坏候选约 20 万条、约 6250 批；embedding 文本是 `raw.lines()` 的整行，无单条字节上限。
- 对照：`docs/architecture/04-tools.md:807`「有界：只算本次命中行、无索引、无持久状态」；`:833`「一次批量调用，§3.2 embedding 契约」；`docs/architecture/03-slots.md:109` 的上限是「每批 1-32 条」，不是总条数 / 总时长。
- 依据：`AGENTS.md` §4.8 要求工具内多步理解「有界步数与并发」；§17.2 的遍历上限保护的是遍历，不是 embedding 调用；§1.4.5 要求错误可行动，超时 / 限流不能只表现为「慢」。
- 影响：一次 `search` 可对本地或外部 embedding 端点发起数千次串行请求，占用工具线程与上游配额，延迟无上界；「一次批量调用」的文档会让调用方以为只有一次请求；若单行是超大文本，单批请求体也可达 MB 级。该路径在无查询、files 模式（P1-1）时同样可能触发。
- 修复方向：在 schema 给 `max_results` 加 `maximum`（建议 200）并在运行期 clamp；给 embedding 候选设独立硬上限（例如最多 200 条或 1000 条，按 §3.1 的批量预算），超出时只对前 N 条重排、其余保持命中顺序并在 meta / notice 里说明；给每条参与 embedding 的文本设字节上限（截断必须在送槽位前完成并记录，不能静默）；给总时长设预算，超时按失败降级为命中顺序。文档改「按 32 条一批、最多 N 批，总量受 max_results 与候选上限约束」。补 >32 条分多批、第二批失败整体放弃、超大行截断的测试。

**P1-4 E.3.2 改称「六个槽位各测一次」，实测表只有四类，属未测却写实测**

- 证据：`docs/architecture/95-references.md:165`「六个槽位按 §3.1 的契约各测一次典型负载」；同节 `:169-174` 的实测表只有 classify / decide、rerank、summarize、vision 四行，无 embedding；`docs/architecture/03-slots.md:46` 自述「六个槽位中五个在本机有实测值（embedding 未测，见 §6.5）」。两处直接矛盾。
- 依据：`AGENTS.md` §3.5「数字必须标口径：实测给命令与条件，估算明确写估算，不编造测试数、行数、延迟」；§4「研究、对比、实测、背景一律进 95-references」。
- 影响：embedding 的 `<50ms / 批 N≤32` 延迟预算没有实测依据，却被标题写成「六槽位各测一次」；后续按 §3.1 做容量规划会把未验证数字当基线。评审若放过，等于承认附件 E 的实测表可以覆盖未测项。
- 修复方向：把 `:165` 改为「已测四个槽位（classify/decide、rerank、summarize、vision）；embedding 未测」；或在表中增加真实 embedding 行，附命令、模型、N、批量、硬件与日期（不得用估算冒充实测）。在 §3.1 的 embedding 预算旁加「未实测」标注，直到有数据。

**P1-5 embedding 响应不校验 index 唯一/连续，跨批也不校验维度，可能静默错位**

- 证据：`crates/toolgate-core/src/slm/embedding_protocol.rs:45-72` 的 `parse_output` 把可选 `data[].index` 排序后直接返回 vectors，未拒绝重复 index、缺号、越界；重复 index 时两条向量会按同一 index 排序，vectors 顺序与 texts 不再一一对应，但长度仍相等，`slots.rs:264-293` 的 validate 只查「长度相等 / 每条同维 / 分量有限」，会放行。`crates/toolgate-core/src/tools/search.rs:250-263` 的各批独立取回，不比较批间维度；维度不同时 `search_path.rs:187-201` 的 cosine 以 NEG_INFINITY 处理，结果静默排到末尾。
- 对照：`docs/architecture/03-slots.md:110-111`「与 texts 等长、同序；每条等维（维度一致）」「同一批内维度一致」；§3.11 形状不符应拒绝并降级，而不是产出顺序可疑的结果。
- 影响：上游返回 JSON 的 index 乱序 / 重复 / 越界时，工具不会报错，只会给出错误的相关性排序；跨批模型切换或代理返回不同维度时同样静默。错误不可见，比直接失败更糟。
- 修复方向：让 `parse_output` 接收期望条数（或由调用方传入 `texts.len()`），校验 `data.len() == expected`、每个 index 在 `0..expected ` 且唯一、连续；无 index 时按出现顺序但同样要求条数；不合法就返回 `ContractViolation`（CAPABILITY_MISMATCH），由 client 走失败降级。`embedding_rerank` 在拼接各批 vectors 后校验所有向量维度相同；不同则整体返回 `None`，由调用方按命中顺序降级并 notice。补重复 index、越界 index、跨批维度不一致三个反例。

### P2（文档一致性，可批量修）——7 条

**P2-1 架构正文、README、工作规范与运行时分册仍残留「五个槽位」**

- 证据：`docs/architecture/03-slots.md:10`「它定义的是五个能力槽位」；`README.md:12`「decide / classify / summarize / rerank / vision 五槽位适配」、`:39`「五个能力槽位」、`:41`「它定义五个槽位」、`:44-50` 的槽位表只有五行（缺 embedding）；`docs/architecture/11-runtime.md:97`、`:214`、`:230-231` 仍写五槽位客户端适配 / 五条全落；`AGENTS.md:24` 红线 3「五个能力槽位是增强不是依赖」。
- 影响：README 是入口文档，读者拿不到 embedding 的输入输出与延迟预算；AGENTS 是工作规范，写五个会让实现者按旧红线判断「第六个槽位是否越界」。`AGENTS.md` 的修改属流程变更（`AGENTS.md` §7），需与架构正文一起评审，不在本报告内直接改。
- 修复：全部改「六个」；README 槽位表补 embedding 行（`文本 / 文本批 → 定长向量`，预算 `<50ms / 批 N≤32` 并标「未实测」直到 P1-4 有数据）；11-runtime 的差距表补 embedding 一行；AGENTS 红线改成「能力槽位（当前六个）是增强不是依赖」。

**P2-2 代码注释、错误文案与测试仍按五槽位写，且缺 embedding 断言**

- 证据：`crates/toolgate-core/src/config.rs:1`、`:314`、`:325`、`:344` 注释；`:422-427`、`:874`、`:905` 的错误 fix 文案「五个合法槽位」；`:1403` 测试「五个槽位都应有绑定」。`crates/toolgate-core/src/capability.rs:140` 外部来源注释漏 embedding；`:249` 测试注释「6 来源 × 5 槽位」；`:272` 测试注释「其余四个槽位」；`:274-286` 的 rerank_row 只列四个槽位、漏 embedding；`:290-308` 的 llm_rows 未断言 embedding 不可用；`:262-270` 的 systemone_row 未断言 embedding。`crates/toolgate-cli/src/main.rs:8`、`:39`、`:118`、`:959`、`:1061` 以及 `crates/toolgate-core/src/locale.rs:445-446` 仍写五槽位。
- 影响：错误文案会把「六个合法槽位」误报成五个，用户按提示改配置仍会被拒；测试漏断言意味着矩阵改动不会被回归捕获。
- 修复：注释 / 文案统一六；capability 测试补 embedding 行专项（embedding × embedding Available、其余五源 Unavailable）、llm_rows 断言 embedding、systemone_row 断言 embedding；config 测试补「六个槽位都应有绑定」并验证 slots 显式覆盖。

**P2-3 03-slots 的配置示例、非 auto 清单、外部协议族与「其余四个槽位」仍按五槽位口径**

- 证据：`docs/architecture/03-slots.md:348`「六个槽位不写全……rerank 为空」只提 rerank；`:350`「其中 systemone / rerank 不参与 auto 推断」漏 embedding；`:420`「另含 systemone / rerank 两个独立协议族」应为三个；`:436`「rerank 协议对其余四个槽位标不可用」应为其余五个。
- 影响：读者会以为 embedding 参与 auto 推断、或以为 embedding 可以被 rerank 协议服务；与 §18.1 的三条不参与 auto、矩阵的五格 Unavailable 不一致。
- 修复：:348 改为「rerank / embedding 为空」；:350 补 embedding；:420 改「systemone / rerank / embedding 三个独立协议族」；:436 改「其余五个槽位」。

**P2-4 18-deployment 的 source 枚举注释、接口计数、矩阵注解与可选层清单未全部同步**

- 证据：`docs/architecture/18-deployment.md:129` 的 YAML 注释仍是 `auto（默认）| local | openai | anthropic | responses | systemone | rerank`，缺 embedding；`:156`「五类接口是传输形态」但 `:158` 表已有六列；`:167-169` 的「不可用」注解只归因 systemone / rerank，未提 embedding 的「无向量输出通道」；`:228`「rerank 协议对其余四个槽位」应为五个；`:1017` 可选层清单与 `:1031-1032` 默认构建降级清单仍是 openai / anthropic / responses / systemone / rerank，缺 embedding。
- 影响：配置示例的注释抄错会导致 embedding 被当成未知 source；可选层台账漏 embedding 会削弱 §19.6 与红线 4 的文档保障，默认构建是否降级无明文。
- 修复：:129 注释补 embedding；:156 改「六类接口」；:168 补「embedding 只返回向量，不生成文本 / 排序 / 图像」；:228 改「其余五个槽位」；:1017 与 :1031-1032 两处清单补 embedding。

**P2-5 04-tools §5.2 的伪代码与实现 / 正文不一致：漏 query 条件、把多批写成一次批量**

- 证据：`docs/architecture/04-tools.md:803`「仅当命中数 > 20，且 content 模式」未写「且 query 非空」；`:820`「20 条以内的命中」只讲数量；`:833`「一次批量调用」；实现 `crates/toolgate-core/src/tools/search.rs:112` 用 `has_query` 参与判定，`:250-256` 按 32 条 `chunks` 串行多批。
- 影响：调用方按伪代码会认为不传 query 也会重排；按「一次批量」会低估延迟与失败面；与 §4.7 面向 agent 的高密度、准确返回冲突。
- 修复：:803 改为「命中数 > 20、content 模式、且 query 非空」；:820 补「且提供 query」；:833 改为「按 §3.2 的 32 条一批分多次调用，任一批失败整体放弃」；与 P1-3 的总上限表述统一。

**P2-6 TopK 阈值作用在已被 max_results 截断的命中数上，与「命中数 >20」的语义不一致**

- 证据：`crates/toolgate-core/src/tools/search.rs:87` 取 `max_results`（默认 200，无上限）；`:282`、`:308`、`:342` 的检索循环在 `hits >= max_results` 时停止；`:99-110` 的 `plan_search` 用 `hits.len()` 判断是否 > `RERANK_THRESHOLD`（`search_path.rs:16`）。因此调用方设 `max_results: 10` 时，即使目录里有 100 条命中，也只会看到 10 条且永不重排；设 `max_results: 20` 时阈值恰好不触发。
- 影响：文档说「命中数 > 20 才重排」，实现实际是「返回的候选数 > 20 才重排」；调用方用较小 max_results 做快速预览时，排序质量静默退化，且没有任何 notice。TopK 与返回上限的交互没有定义。
- 修复：在检索阶段单独计数总命中（或至少记录是否被 max_results 截断），plan_search 用「总命中数」判定；若为性能不愿多遍历，就在 §5.2 明确「阈值作用于返回候选数」，并在 max_results ≤20 时输出 notice「未重排：候选不足 20」；schema 给 max_results 设合理上限（与 P1-3 一起）。

**P2-7 新增行为的回归测试不足，若按现状进入实现阶段无法防回退**

- 证据：`crates/toolgate-core/src/tools/search.rs:452-692` 的测试只覆盖 glob、raw、small、bound rerank failure、many hits degrade、structured、visual、walk 截断；没有 files 模式不重排、工具级 no-query、embedding 成功重排、embedding 失败、>32 条分多批、schema required、`max_results` 与阈值交互。`crates/toolgate-core/src/slots.rs:704-712` 只测 rerank index，无 embedding validate；`:785-807` 的 `degrade_action` 循环只断言非空，未断言 `KeepHitOrder`。`embedding_protocol.rs` 5 个测试没有重复 / 越界 index。
- 影响：P0-1、P1-1、P1-2、P1-3、P1-5 都是可回归的行为；当前测试全绿不代表契约正确。
- 修复：至少补 P1-1 的 files 用例、P1-2 的 pattern 契约用例、P1-3 的 >32 条与总上限用例、P1-5 的 index / 跨批维度反例；再补 tool-level no-query（断言不调用槽位、meta 不为 applied）、embedding 成功（断言 meta.rerank=embedding）、embedding 失败（Unavailable+notice，applied=false）、schema required、`max_results=10` 与总命中 >20 的阈值用例。

### P3（结构优化与范围外观察）——4 条

**P3-1 实现状态基线 462 与本次实测 463 差 1**

- 证据：`docs/implementation-status.md:10` 写「验证基线 462」；本次 `cargo test --workspace` 实跑 463 passed。可能是新增 `embedding_protocol.rs` 的 5 个测试只部分计入，或状态文档未跟上工作区。
- 修复：在本次改动闭环时用实际命令重算并更新为 463（附日期与 suite 拆分），不要手填未跑的数字。

**P3-2 本地 / auto 的 embedding 能力未在文档明说，容易被误读为「serves 声明即绑定即用」**

- 证据：`docs/architecture/03-slots.md:350` 说明 auto 推断，但未说本地没有 embedding 后端；`18-deployment.md:165` 的矩阵只有 `embedding × embedding` 可用，`capability.rs` 对 local / auto 的 embedding 是 Unspecified；`crates/toolgate-core/src/slm/client.rs:916-990` 要求恰好 Available，故本地 `serves: [embedding]` 会在首次调用被拒并按 §3.8 降级。
- 影响：用户会以为给本地模型加 `serves: [embedding]` 就能自动获得向量重排，实际永远不可用；这不是 bug（§3.11 声明不是承诺），但缺少一句显式说明。
- 修复：在 §3.10 或 §18.1 embedding 段加「本轮只实现 `source: embedding` 的原生端点；local / auto 的 embedding 声明在能力矩阵中为 Unspecified，按 §3.11 前置准入拒绝并降级」，并与 §3.8 降级表呼应。

**P3-3 check-doc.sh 不检查「槽位计数 / 实测表覆盖」这类跨文件语义**

- 证据：本次 `./scripts/check-doc.sh` 退出 0，但 P2-1～P2-4 的五槽位残留与 P1-4 的「六槽位标题、四行实测」均未被捕获；脚本只查章节连续性、交叉引用、链接、围栏、占位符、规模（本次输出）。
- 修复：后续可在脚本中加两条低成本检查：全库中「五个能力槽位 / 五槽位」的黑名单（历史评审 / 冻结文档除外），以及 E.3.2 标题声明的槽位数与实测表行数一致性。修改脚本属代码改动，本报告不动。

**P3-4 历史 ADR 与冻结文档中的「五个槽位」是有意的历史记录，不应回写**

- 证据：`docs/decisions/0006` / `0007` 与 `docs/requirements-coverage.md:177` 仍写五槽位；`AGENTS.md` §4 与 `docs/requirements-coverage.md` 顶部冻结说明要求不改历史 / 冻结文档。
- 处理：不列为缺陷，不修改；只在 P2-1 的现值文档中改成六，避免读者把历史当现状。

## 4. 验证结果

| 命令 | 结果 | 结论 |
|---|---|---|
| `./scripts/check-doc.sh` | 退出码 0；「§0-22 全部存在」「附录 A–D 存在」「交叉引用全部存在（135 个不同章节号）」「各文件围栏均成对」「无残留占位符」「链接检查通过」「总行数 6646」「文档完整性检查通过」 | 通过；但未捕获 P2-1～P2-4 与 P1-4，见 P3-3 |
| `cargo test --workspace` | 退出码 0，463 passed（core 437 + 4 + 4 + mcp 18） | 通过；与 implementation-status 的 462 差 1，见 P3-1 |
| `git status --short`（评审前） | 21 个已跟踪文件 `M`、1 个未跟踪新文件 `?? crates/toolgate-core/src/slm/embedding_protocol.rs`、`?? docs/reports/` | 评审对象为工作区；未跟踪目录不入范围 |
| `git diff --numstat` | 627 插入 / 169 删除 | 与评审对象统计一致；新增 125 行未计入 |

未执行 `cargo build --release` / `cargo clippy`：本次评审不改代码，用户要求的评审取证为文档检查与测试基线，均已实跑；未启动服务、未探测 127.0.0.1:10020，故本报告不对上游 embedding 端点的真实行为作断言。

## 5. 结论

- **P0 是否清零：否（1 条：P0-1 §3.8 降级表与 §5.2 / §18.1 / 实现相反）**。
- **P1 是否清零：否（5 条：P1-1 files 重排、P1-2 pattern 契约、P1-3 无界批次、P1-4 未测却称六槽位实测、P1-5 index / 维度校验缺口）**。
- **能否进入实现阶段：不能**。依据 `AGENTS.md` §3.6「P0 与 P1 全部落地后才能进入实现阶段」。当前 P0 会让 §3.8 与 §5.2 各自给出相反的 embedding 退路语义；P1 会让 files 模式做无意义重排、让 content 的通配承诺失败、让一次 search 产生数千次串行 HTTP、让未测延迟被当成实测、让错位向量静默返回。五条都必须先修正文 / 代码。
- **闭环要求**：P0-1 修 `03-slots.md:224` / `:228`；P1-1～P1-3、P1-5 修代码与文档并补回归；P1-4 修 `95-references.md:165` / 表；P2 的 7 条可批量修，P3 放最后。修完重跑 `./scripts/check-doc.sh` 与 `cargo test --workspace`，复审通过后再进入实现阶段。本报告作为历史证据保留，不回写结论。
- **范围外观察**：`AGENTS.md` 本身仍写五个槽位（P2-1），其修改属流程变更，需与架构正文一起评审；`docs/reports/toolgate-vs-builtin-2026-11-13.html` 是旧产物，不在本次架构契约范围。
