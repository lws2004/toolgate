# toolgate 架构独立评审：ADR 0007「source: rerank 专属协议」（2026-11-10）

> **评审对象**：HEAD `157104928543931fa776384e3e53156c0769d62e`（提交信息「ADR 0007 新增 source: rerank 专属协议（架构变更前半段）」），4 个文件、101 行新增 / 14 行删除。
> **评审依据**：仓库根 `AGENTS.md` §2 红线七条、§3.4 架构变更流程、§3.5 ADR、§3.6 评审与闭环、§4 文档规则；`docs/architecture.md` 与 `docs/architecture/` 分册原文（03-slots / 04-tools / 18-deployment / 90-appendices / 95-references / 06-cost / 11-runtime）。
> **评审性质**：独立评审。本报告是历史证据，只作待办来源；修复落在架构正文、ADR 与工作规范，不回写本报告结论。
> **纪律**：本次只写本报告一个文件；未修改架构正文、ADR 或任何代码；未执行 `git commit` / `git add`；未启动或重启任何服务；未对 127.0.0.1:10020 发起探测（避免触发 launchd KeepAlive 拉起服务），故 ADR 中的上游实测仅核对口径标注、未复现。

## 1. 评审对象

| 文件 | 改动 | 内容 |
|---|---|---|
| `docs/decisions/0007-rerank-专属协议.md` | 新增 72 行 | ADR 0007，状态「提议（待独立评审）」 |
| `docs/architecture/18-deployment.md` | +26 / −14 | §18.1 source 表加 `rerank` 行、auto 推断段、能力矩阵加一列、新增「rerank 协议：原生 /v1/rerank 端点」小节 |
| `docs/architecture/03-slots.md` | +1 | §3.10 adapter: auto 规则表加 `source: rerank → 槽位 rerank → native` 一行 |
| `docs/architecture/90-appendices.md` | +1 / −1 | 附录 D `models.<id>.source` 默认值行加 `rerank` |

核对基线：评审开始前 `git status --short` 为空，工作区干净；已跟踪 ADR 为 0001–0007。

## 2. 六项重点核查结论

1. **§2 红线与六个工具**：新增 `source: rerank` **未触及**七条红线中的任何一条，也未改变六个工具的数量、签名、返回形态或错误码语义——补丁未碰 `04-tools.md`，`RerankInput` / `RerankOutput` 形状（`03-slots.md:75-77`）未改，`adapter` 枚举（`03-slots.md:381-386`、附录 D `90-appendices.md:121`）未改。新增的是「模型服务协议」，与 `systemone` 同属 `source` 枚举扩展，不是 agent 框架，也不改 MCP 通信协议。**但**：P1-2 的外部来源/可选层清单漏同步，会削弱红线 3（槽位全空仍能单二进制运行）与红线 4（最少依赖）在文档侧的保障，需修复。
2. **取值清单逐处一致性**：**不一致**。已同步的是 `18-deployment.md:125`（注释 7 值）、`18-deployment.md:108-116`（source 表 7 行）、`90-appendices.md:120`（附录 D 6 值）、`03-slots.md:394`（auto 规则表）、`18-deployment.md:154-160`（能力表新列）；**漏同步**的是 `03-slots.md:339`、`AGENTS.md:93`（source 显式取值清单仍为 4 值），以及 `18-deployment.md:355` / `:989` / `:1003-1004` / `:371`（外部来源、可选层台账、默认构建降级清单）。详见 P1-1、P1-2、P2-7。
3. **ADR 0007 与架构正文**：决策 1–6 与 §18.1 / §3.10 的落点**逐条一致**（新增 source、能力矩阵新列、端点形状、不进 auto、ordered 保留、不新增依赖）。**但实测数字口径不合格**：ADR:18-25、:69、:10 给出 0.641 / 0.453、端口、模型路径与 `block_size=125`，只标了日期与端点名，未给命令、请求体、候选数等条件；`block_size=125` 与 `03-slots.md:147`「参考实现是 125 个」口径混淆（引用写成「本机模型」）；且实测/背景/对比未落 `95-references.md` 附件 E。详见 P2-6。
4. **§18.1 自洽性与 §18.9 四层校验**：**存在冲突**。`base_url` 拼接口径在 source 表与正文之间歧义（P2-3）；`top_n`「按 §3.2 截断」引用错误且截断点未明确（P2-4）；「配错 model 名按 §18.9 fail loud」与 §18.9 四层校验、§18.1「不做启动探测」、§3.11 首次使用校验**直接冲突**（P1-3）。
5. **与 §3.5 / §3.11 / §3.8 的矛盾**：§3.5「rerank 必须独立成槽位」未被改变，新增 `source` 不动槽位契约，一致；§3.8 降级语义（`03-slots.md:209-229`）本身无矛盾，但清单漏同步使 `source: rerank` 在默认构建下的降级无明文（P1-2）；**§3.11「serves 是声明不是承诺」与新增的「serves 只能含 rerank」措辞冲突**（P1-4）。
6. **基线验证**：`./scripts/check-doc.sh` 退出码 0；`cargo test --workspace` 441 passed、退出码 0。基线未破。详见 §4。

## 3. 发现

### P0（阻塞级）——0 条

核查结论：新增 `source: rerank` 未触及 `AGENTS.md:20-28` 七条红线；六个工具数量/签名/返回形态/错误码语义均未改变（`docs/architecture.md:44`、`AGENTS.md:22`、`docs/architecture.md:224-248`）。本次变更属「协议 / 部署形态」类架构变更，按 `AGENTS.md:59` 应先改正文、跑 check-doc.sh、再评审——提交定位为「架构变更前半段」，此流程本身合规。

### P1（清零后才能进入实现阶段）——4 条

**P1-1 `source` 显式取值清单在架构正文与工作规范中漏同步**

- 证据：`docs/architecture/03-slots.md:339`「也可显式 `local` / `openai` / `anthropic` / `responses`（见 §18.1）」；`AGENTS.md:93`「模型来源 `source` = auto / local / openai / anthropic / responses（§18.1）」。两处均缺 `systemone` 与本次新增的 `rerank`。
- 对照：`docs/architecture/18-deployment.md:108-116`（source 表 7 行）、`:125`（注释 7 值）、`docs/architecture/90-appendices.md:120`（附录 D 6 值）已含 `rerank`。
- 依据：`AGENTS.md:78`（单一事实源）、`:83`（文档改动跑检查）；`docs/architecture.md:40`（分册是同一份架构的物理拆分，冲突以总目录为准）。
- 影响：§3.10 是配置解析的实现依据，实现者按 `:339` 会只实现 4 个显式值，`source: rerank` 配置在结构层被判「枚举非法」而启动失败，本次新增功能直接不可用。`systemone` 是 2026-11-09 落地时的遗留漏同步，本次 `rerank` 延续了该缺口。
- 修复方向：把两处清单补齐为与 §18.1 / 附录 D 一致的完整取值；`AGENTS.md` 的修改按 `AGENTS.md:139` 属流程变更，需与架构正文一起评审。

**P1-2 外部来源 / 可选层 / 默认构建降级 / §19.6 台账清单均未纳入 `rerank`**

- 证据：`docs/architecture/18-deployment.md:355`「外部来源（`openai` / `anthropic` / `responses` / `systemone` / `auto` 解析为外部协议时）`mode` 只能是 `http`」；`:989`（§19.6 可选层台账「外部模型协议客户端（openai / anthropic / responses / systemone）」与「配置外部 `source`（openai / anthropic / responses / systemone / auto）时启用」）；`:1003-1004`（「配置了外部 `source`（openai / anthropic / responses / systemone）的槽位按 §3.8 降级并告警」）；`:371`（「配置了外部 `source` 的槽位按 §3.8 降级」）。
- 对照：`docs/architecture/18-deployment.md:221`「HTTP 发送在 `external-protocols` feature 后（§19.6）」；`docs/decisions/0007-rerank-专属协议.md:54-55` 决策 6「不新增依赖：HTTP 发送仍在 `external-protocols` feature 后（§19.6）」；`docs/architecture/18-deployment.md:940-941`「新增项走 §1.5 的准入四问，并同时更新本表与附件 E.8」。
- 影响：① `mode: embedded` + `source: rerank` 不落入 `18-deployment.md:783` 语义层的「`mode: embedded` + 外部 `source`」启动失败判据，非法组合不会被 fail loud 拒绝；② §19.6 台账未登记该协议客户端属可选层，违反台账同步纪律；③ 默认构建不含 `external-protocols` 时，`source: rerank` 的槽位是否按 §3.8 降级并告警无明文，可能被误编进默认层，削弱 `AGENTS.md:24` / `:25` 两条红线的文档保障。
- 修复方向：在上述四处清单统一补入 `rerank`，并明确「`source: rerank` 的客户端在 `external-protocols` feature 后、默认构建缺失时按 §3.8 降级并告警」。

**P1-3 「配错 `model` 名按 §18.9 fail loud」与四层校验 / 不做启动探测 / 首次使用校验冲突**

- 证据：`docs/architecture/18-deployment.md:218`「`model` 必须与上游服务启动时的模型名完全一致，不做别名推断（配错按 §18.9 fail loud）」；`docs/decisions/0007-rerank-专属协议.md:62`「配错按 §18.9 fail loud」。
- 依据：`docs/architecture/18-deployment.md:783`（语义层 fail loud 只覆盖配置内部可判定项）、`:784`（资源层「该槽位不可用（§3.8 降级），启动告警，不阻塞」）、`:785`（能力不符层首次校验失败 → 降级/禁用）、`:787`（前三层 fail loud、第四层降级但必须告警）、`:830-831`（`config check` 只做语法/结构/语义三层，「不加载模型、不监听端口」）；`docs/architecture/18-deployment.md:150`「不做启动探测」；`docs/architecture/03-slots.md:484`「首次使用校验：第一次真实调用即视为校验」；`docs/architecture/03-slots.md:503-504`「能力不符是『声明合法但实际不支持』——启动可过，运行期校验后降级 / 禁用」。
- 影响：`model` 名是否与上游启动名一致无法在配置期判定（配置里只有一个字符串），只能由上游在首次调用返回 `model_not_found` 暴露。按 §18.9 分层这属资源/能力层，应「槽位不可用 + 降级并告警」，而非启动失败；且 §18.1 明令不做启动探测。实现者按「fail loud」会在配置阶段无从下手，或违规加入启动探测。
- 修复方向：改为「上游返回 `model_not_found` 时按 §18.9 第四层 / §3.11 首次使用校验降级并告警（§7.3 标注）」，并把「不做别名推断」保留为请求构造约束。

**P1-4 「`source: rerank` 的 `serves` 只能含 `rerank`」与 §3.11「serves 是声明不是承诺」冲突**

- 证据：`docs/architecture/18-deployment.md:221`「`source: rerank` 的 `serves` 只能含 `rerank`（其余槽位在能力表里是「不可用」）」；`docs/decisions/0007-rerank-专属协议.md:41`、`:62` 同口径。
- 依据：`docs/architecture/03-slots.md:476-477`「`serves` 是**声明**，不是承诺……工具层不信任声明，只信任校验」；`:420-422`「声明阶段不因这条判据拒绝配置（前三层的 fail loud 仍按 §18.9）；能力判定是**运行时的前置准入**」；`:503-504`「能力不符是『声明合法但实际不支持』——启动可过，运行期校验后降级 / 禁用」。
- 影响：「只能含」是强约束措辞，若实现成配置层硬拒绝，等于把声明变成承诺、把运行时前置准入提前到启动期，与 §3.11 及 §18.9 的分界相悖；也和 §3.10:398「无法判断 → `json`」的兜底路径不一致（若 `serves` 写了非 rerank 槽位，auto 规则会落到 json 而非报错）。
- 修复方向：改为「能力表中 `rerank` 协议对其余四个槽位判为**不可用**，运行时按 §3.11 前置准入拒绝并降级」；若要保留为配置约束，须显式声明这是对 §3.11 的例外并写进 §18.9 语义层。

### P2（文档一致性，可批量修）——7 条

**P2-1 能力表注解与 §3.10 判例未同步新列**
证据：`docs/architecture/18-deployment.md:162-167` 注解只写「每个『不可用』都能归到输入信息不足（systemone 不生成文本、无排序接口）」，未把 `rerank` 列四个「不可用」的理由纳入；`:157-159` 的说明列也未给 rerank 列理由；`docs/architecture/03-slots.md:415-416` 的判例只举 systemone。依据：`AGENTS.md:78`（单一事实源）、`docs/architecture/03-slots.md:415`（表里每个「不可用 / 受限」都必须能归到两条判据之一）。修复：补一句「rerank 协议只返回排序结果，缺文本/概率/图像输出通道」，并把 `rerank × rerank`「可用」也纳入判例。

**P2-2 auto 推断段的理由文字只讲 systemone**
证据：`docs/architecture/18-deployment.md:141-148` 结论句已加「`systemone` 与 `rerank`」，但 `:142-144` 的解释仍是「判断后端是 systemone 还是别的……同一个根地址既能挂 OpenAI 兼容端点，也能挂 systemone 端点」；`:145` 与 `:141-144` 重复「猜错协议比报错更糟」。依据：`AGENTS.md:79`（正文只写决策）、`docs/architecture/18-deployment.md:132-139`（六条推断规则未变）。修复：把解释改为覆盖「systemone / rerank 等独立协议族」，去掉重复措辞。

**P2-3 source 表 `rerank` 行的「同上」与 `base_url` 拼接口径歧义**
证据：`docs/architecture/18-deployment.md:115`（`rerank` 行「字段：同上」「形态：同上」，沿用 `:111` 的 `base_url` 口径）与 `:185-186`（「`POST {base_url}/chat/completions`，`base_url` 为含 `/v1` 的 API 根」）；而 `:148`、`:214` 要求「`base_url` 指向服务**根地址**，客户端自行拼 `/v1/rerank`」。依据：`AGENTS.md:78`（单一事实源）。修复：在 source 表或表注中标明「`systemone` / `rerank` 的 `base_url` 是不含 `/v1` 的服务根」，避免实现者拼出 `/v1/v1/rerank`（`systemone` 行同样存在此既有歧义）。

**P2-4 `top_n`「按 §3.2 截断」交叉引用错误，截断点未明确**
证据：`docs/architecture/18-deployment.md:221`「`RerankInput.top_n` 不透传……由 toolgate 侧在完整 `results` 上按 §3.2 截断」、`:204`（ordered 路径「`top_n` 透传但 results 仍须覆盖全部候选」）、`docs/decisions/0007-rerank-专属协议.md:48`；`docs/architecture/03-slots.md:75-77` 的 rerank 契约只要求「results 覆盖全部输入文档」，**没有截断条款**；`docs/architecture/04-tools.md:815-816` 把截断点定在工具层（「重排命中行后，再做确定性截断」）。依据：`AGENTS.md:46`（实现必须能指到具体章节）、`docs/architecture/04-tools.md:816`。影响：「按 §3.2 截断」指向的章节不存在该规则；若实现者在槽位返回前截断，会破坏 §3.2「覆盖全部输入」。修复：改为「`top_n` 不透传，槽位按 §3.2 返回覆盖全部输入的完整 `results`；截断由工具层按 §5.2 的确定性截断完成」，并与 `:204` 的 ordered 路径做一句对照。

**P2-5 04-tools.md §5.2 的分数性质说明未区分原生 ranker**
证据：`docs/architecture/04-tools.md:820`「分数是模型自报值、非校准（§18.1「受限」），只用作顺序，不作为置信度门控输入」——该括注对应能力表 openai/anthropic/responses 三列的「受限」，未覆盖新增「可用（原生排序）」列；`docs/decisions/0007-rerank-专属协议.md:59-60` 声称「不会把通用 LLM 的自报分数与专用 ranker 的相关性分数混为一谈」，但 §18.1 新小节（`:210-221`）未定义 `relevance_score` 的性质、是否标 `UNCALIBRATED`（`docs/architecture/03-slots.md:462-465`）。修复：在 §18.1 新小节或 §5.2 明确两条路径的分数语义与是否只作顺序。

**P2-6 违反 §4「正文只写决策」，实测数字未标口径、未落附件 E**
证据：`docs/architecture/18-deployment.md:212` 把本机实例（`127.0.0.1:10020` 的 rmlx retrieval 服务、`jina-reranker-v3.5-mlx-merged`）写进架构正文；`docs/decisions/0007-rerank-专属协议.md:18-27` 的实测、`:29-33` 的对比与撤回方案、`:7-16` 的背景均落在 ADR，而 `docs/architecture/95-references.md` 未新增附件（HEAD 只改 4 个文件，无 95-references.md）；对照 `docs/architecture/95-references.md:502-525` 的 E.10「systemone 协议的来源与本机实测」有独立条目与逐条实测表。数字口径：`ADR 0007:22-23`（0.641 / 0.453）、`:10`（`block_size=125`）、`:69`（只列端点名与日期）未给命令、请求体与候选数等条件；`block_size=125` 与 `docs/architecture/03-slots.md:147`「参考实现是 125 个」混用，把引用写成「本机模型」。依据：`AGENTS.md:79`（研究、对比、实测、背景一律进 95-references.md 附件 E）、`AGENTS.md:67`（实测给命令与条件、不编造测试数）、`docs/architecture.md:9-11`（本目录正文只写决策）。修复：把实例与实测移入附件 E 新增条目（如 E.11），正文只留指针；ADR 引用该条目并补齐命令与条件。注：ADR `:72`「`./scripts/check-doc.sh` 退出码 0」经本次实跑复核为真（§4）。

**P2-7 术语与附录 D 措辞未随新协议更新**
证据：`docs/architecture/90-appendices.md:126`「`allow_remote`……是否允许非 loopback 的 **OpenAI 兼容地址**」，未覆盖 `rerank` 这类非 OpenAI 兼容端点（`:357` 的「外部协议默认只接受 loopback」是通用规则）；`docs/architecture/03-slots.md:395-397` 的「外部协议」括号限定为 `openai` / `anthropic` / `responses`，而 `docs/architecture/18-deployment.md:355` 的「外部来源」含 `systemone`（应含 `rerank`），两个术语口径不一致。依据：`AGENTS.md:81`（术语按附录 C）、`AGENTS.md:78`（单一事实源）。修复：统一「外部来源 / 外部协议」定义或加注，附录 D 的 `allow_remote` 说明改为「非 loopback 的外部协议地址」。

### P3（结构优化与范围外观察）——4 条

**P3-1 工作规范中的既有清单过期（非本次变更引入）**
证据：`AGENTS.md:65`「编号递增（现有 0001–0005）」，实际已跟踪 0001–0007；`AGENTS.md:106`「当前基线 57 passed」，实测 `cargo test --workspace` 为 441 passed（`docs/implementation-status.md:10` 记「验证基线 441/435」，`README.md:105` 仍写 57 个测试）。按 `AGENTS.md:139`，修改 AGENTS.md 属流程变更，需与架构文档一起评审，本评审只报告不修改。

**P3-2 ADR 0007 状态待更新**
证据：`docs/decisions/0007-rerank-专属协议.md:3`「状态：提议（2026-11-10，待独立评审）」；对照 `docs/decisions/0006-实现阶段契约补齐.md:3`「状态：已采纳（2026-11-09）」。本评审即其「待独立评审」，但按 `AGENTS.md:73` 报告不改 ADR；评审通过后由架构正文侧把状态改为「已采纳」。

**P3-3 附录 C 术语表未收协议族相关术语**
证据：`docs/architecture/90-appendices.md:59-76` 未收 `source` / `adapter` / 「独立协议族」等术语；`AGENTS.md:81` 要求「新增术语补进附录 C」。本次新增的「独立协议族 / 原生重排序端点」可考虑补入；既有 `source` / `adapter` 也未收，属结构优化，放最后处理。

**P3-4 check-doc.sh 未覆盖跨文件取值清单一致性**
证据：本次 `./scripts/check-doc.sh` 退出码 0，但 P1-1 / P1-2 的 4 处清单漏同步均未被捕获（脚本只检查章节连续性、交叉引用、链接、围栏、占位符、规模，见 §4 输出）。建议后续增加「`source` 枚举取值清单跨文件一致性」检查；脚本修改属代码改动，本报告不动。

## 4. 验证结果

| 命令 | 结果 | 结论 |
|---|---|---|
| `./scripts/check-doc.sh` | 退出码 0；「§0-22 全部存在」「附录 A–D 存在」「交叉引用全部存在（135 个不同章节号）」「各文件围栏均成对」「无残留占位符」「链接检查通过」「总行数 6398」「文档完整性检查通过」 | 通过 |
| `cargo test --workspace` | 441 passed（6 suites，9.61s），`CARGO_TEST_EXIT=0`，后台任务状态 completed、退出码 0 | 通过，基线未破 |
| `git status --short`（评审前） | 空 | 工作区干净，评审对象仅为 HEAD 提交 |
| `git -c core.quotepath=false show HEAD` | 4 文件、+101 / −14 | 与评审对象一致 |

未执行 `cargo build --release` / `cargo clippy`：本次评审不改代码，用户要求的基线核验为 check-doc.sh 与 cargo test 两项，均已实跑。

## 5. 结论

- **P0 是否清零：是（0 条）**。新增 `source: rerank` 未触及 `AGENTS.md` §2 七条红线，未改变六个工具的数量 / 签名 / 返回形态 / 错误码语义；本次提交定位为「架构变更前半段」（正文 + ADR 待评审），流程合规。
- **P1 是否清零：否（4 条：P1-1 / P1-2 / P1-3 / P1-4 未落地）**。
- **能否进入实现阶段：不能**。依据 `AGENTS.md:72`「P0 与 P1 全部落地后才能进入实现阶段」。4 条 P1 分别会导致：配置解析枚举漏值使新功能不可用（P1-1）、默认构建与可选层边界无明文保障（P1-2）、配置期与运行期错误处理语义冲突（P1-3）、`serves` 声明语义与 §3.11 冲突（P1-4），均会在实现阶段直接引发契约分歧。
- **闭环要求**：4 条 P1 的修复落在架构正文（`03-slots.md` / `18-deployment.md` / `90-appendices.md`）、ADR 0007 与工作规范 `AGENTS.md`，**不回写本报告**；P2 的 7 条可批量修，P3 放最后。修完重跑 `./scripts/check-doc.sh` 与 `cargo test --workspace`，做一次复审 / 确认后再进入实现阶段。本报告作为历史证据保留。
- **范围外观察**：AGENTS.md 的编号清单与测试基线数字过期（P3-1）、`95-references.md` 未新增实测附件（P2-6）、能力表注解与 04-tools.md 分数语义未同步（P2-1 / P2-5）——这些都不改变「本次提交是待评审的前半段」这一事实，但需在闭环时一并处理。
