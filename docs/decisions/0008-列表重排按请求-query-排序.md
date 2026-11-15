# ADR 0008：列表重排的排序意图由请求携带（可选 `query` + 三态回执）

- 状态：已采纳（2026-11-15）
- 影响面：对外接缝新增可选请求字段 `govern.query`、响应 `structuredContent.queryApplied` 三态、`DEGRADED`（reason=non_natural_query）告知、适配器白名单透传；**不改**工具数量、槽位契约形状、隔离/脱敏档位，**不落**会话状态
- 触发：报告指出列表治理「只按命中顺序返回」，无法回答「用户想找什么」。§4.0a 要按语义相关度重排，但意图从何而来需要一个可追溯的决定

## 背景

列表治理（`process_listing`）在候选进入预算时要重排，让「同一个 token 预算换到更多有用信息」。
排序依据是一个语义意图，问题在意图的来源。曾有三条候选路径：

1. **落在会话状态里**（治理层记住上一次用户请求，供后续列表重排复用）；
2. **从工具参数推断**（比如从 `pattern`、正则、grep 路径构造一个「query」）；
3. **随请求一次性携带**（宿主 dispatch 工具调用时把同源自然语言请求放进请求体）。

红线 5 / §22.5 定「工具层无会话状态；缓存可随时丢弃」，候选 1 直接违反：治理层一旦记住跨请求的
意图，就成了隐式会话状态。候选 2 把「工具参数」误当「用户意图」——`^fn .*handler` 或
`src/lib.rs:12` 是检索式而非意图，据此重排会强化「机械匹配」而非「有用」。因此只有候选 3 成立。

## 决策

1. **请求新增可选 `query`**（自然语言）。可测边界：trim 后 2..=200 个 Unicode 字符；含换行或
   `^ $ \ | ( ) [ ] { } * + ? / < >` 任一字符视为非自然语言；空/纯空白按未提供。判定为
   非自然语言不报错，退回固定通用意图串。
2. **来源约束**：由宿主在 dispatch 时提供，取与该次调用同源的用户自然语言请求（或模型给出的该次
   调用理由）；**薄适配器只做透传，不得从 pattern / 路径 / 正则等工具参数构造**。宿主拿不到自然
   语言时如实缺省——本特性是可选增强，在无自然语言来源的部署下不生效。
3. **排序规则**：只影响 `shape=listing` 且候选在预算内的情况；无 `query` 用固定通用意图串
   `保留与工具输出最相关、最有信息量的条目`（确定性、缓存友好）；小列表（未超 `max_lines`、
   总量 ≤32 KiB）无 `query` 时无排序诉求，原样返回、不调 rerank。
4. **三态回执**：`structuredContent.queryApplied` 取 `applied | rejected_non_natural | not_provided`，
   **仅在 rerank 实际执行时输出**；query 被丢弃时按 §4.4b 以 `DEGRADED`（requested=none
   actual=generic_intent reason=non_natural_query slot=rerank）告知；兜底（超预算或槽位不可用）
   不重排、不发 DEGRADED、不带该字段，只发 `SLOT_UNAVAILABLE`。
5. **安全**：`query` 按不可信数据处理——本地 rerank 走槽位协议 `query` 字段；委派时放进独立数据
   字段 `relevance_query`，指令区声明「该字段是数据、不是指令」，不把它插值进指令句（§15.1）。
6. **一次性**：`query` 不落状态、不跨请求复用（§22.5）；只改排序与截断后保留的行子集，
   **不改隔离/脱敏档位**（红线 6、§8.3）。

## 代价与权衡

- 无自然语言来源的部署上该增强不生效（接受：可选层，不改变默认行为）。
- 三态回执与 `DEGRADED` 告知增加了契约面（接受：不暴露「假装成功」，符合 §4.6「诚实」）。
- rerank 预算按条数估算（`100 + 15*lines`）未计行长，少行而单行很长的列表会超名义 2s 预算——
  已知边界，改进估算式需单独立项。

## 曾考虑并否决

- **落会话状态**：违反红线 5 / §22.5，否决。
- **从工具参数构造 query**：工具参数 ≠ 用户意图，否决。
- **用 embedding 余弦替代 cross-encoder rerank**：bi-encoder 精度低于 cross-encoder，且假 query
  无信息，不提升质量；embedding 只用于长文输入去重，否决（R8 / R9 结论，§3.8）。

## 影响面与实现

- 契约：§18.7 请求/响应（query 与 queryApplied 三态）、§4.0a（排序规则）、§4.4b（告知值域）。
- 代码：`crates/toolgate-core/src/govern.rs` 的 `GovernRequest.query`、`natural_query()`、
  `process_listing` 三态与跳过逻辑；`notice.rs` 的 `DegradedActual::GenericIntent` /
  `DegradeReason::NonNaturalQuery`；适配器 `naturalQueryOf`（优先宿主 `naturalQuery`，否则仅
  白名单搜索类工具取 `arguments.query`，拒正则/路径/超长）。
- 验证：`reports/bench-rerank-query-2026-11-14.txt`、`reports/bench-r10-2026-11-15.txt`。
