# toolgate 架构评审复核：返回层告知协议修复（2026-11-08 复核）

> 复核对象：docs/architecture-review-2026-11-08.md（首轮评审，27 条）所列 P0/P1 修复及 P2 批量修复后的工作区未提交版本。
> 复核方式：逐条对照原报告 27 条 + 修复后相关章节全文核对（04-tools §4.4–§4.5、§5.4/§5.6；08-isolation §8.7；09-output §9.0/§10.1/§10.2d/§10.4；06-cost §7.2/§7.3；15-risk §15.7/§16.3；18-deployment §18.1/§18.7；11-runtime §13.3；03-slots §3.10；90-appendices 附录 C）+ ./scripts/check-doc.sh 实跑（退出码 0）。
> 结论：**23 条已关闭、3 条部分关闭、1 条未关闭（P0-1）；修复另引入 10 条新问题（P1 1、P2 5、P3 4）。当前残余 P0 1 条、P1 3 条，不能进入实现。**

---

## 0. 结论摘要

修复覆盖了首轮报告的大部分问题：§4.4b 已重写为 11 个 code 的统一枚举，生成拆为 decide_output + finalize_notices 两段，新增 NORMALIZED / UNCALIBRATED / LOSSY_DECODE 并在 §5.4/§3.10/§5.6 建立映射；§7.3/§18.1 的 meta 残留、§10.1 示例、§18.7 govern、§13.3、§15.7、附录 C 均已同步。但仍有 1 条 P0 未关闭，且新增的 finalize_notices 只有一句调用、没有签名与测试。

| 状态 | 数量 | 明细 |
|---|---|---|
| 已关闭 | 23 | P0-2、P0-3；P1-1、P1-2、P1-5、P1-6、P1-7；P2-1–P2-11；P3-1–P3-5 |
| 部分关闭 | 3 | P1-3（枚举值域未闭合）、P1-4（NoticeParams 类型不可直接实现）、P2-12（告知完备性测试未覆盖全部 11 个 code） |
| 未关闭 | 1 | P0-1（模板分隔符与全部示例矛盾） |
| 新引入 | 10 | N-1（P1）、N-2–N-6（P2）、N-7–N-10（P3） |

**残余 P0：1 条（P0-1）。残余 P1：3 条（P1-3、P1-4、N-1）。**

---

## 1. 逐条核对总表（27 条）

| # | 原级别 | 原发现摘要 | 状态 | 证据（修复后位置） |
|---|---|---|---|---|
| P0-1 | P0 | 模板语法与 8 个示例全部不符，§8.7 与 §4.4b 矛盾 | **未关闭** | 04-tools.md:189 总则「分隔符固定为『 · 』」；示例 153–163 全部空格分隔；08-isolation.md:176–177 亦为空格 |
| P0-2 | P0 | decide_output 纯函数无法生成 notices | 已关闭 | 04-tools.md:137–145 两段生成；264–286 decision_notices + finalize_notices |
| P0-3 | P0 | notice 枚举遗漏三处告知要求 | 已关闭 | 04-tools.md:160–162 新增 NORMALIZED/UNCALIBRATED/LOSSY_DECODE；658–659、766；03-slots.md:383–385 |
| P1-1 | P1 | §7.3 第 2 条 / §18.1 第 3 条 meta 残留 | 已关闭 | 06-cost.md:194 content 模板 + slmStatus；18-deployment.md:64–66 SLOT_UNAVAILABLE + slmStatus |
| P1-2 | P1 | code 互斥性不足 | 已关闭 | 04-tools.md:173–180 六条 code 选择规则 |
| P1-3 | P1 | 模板数值/枚举格式未定义 | **部分关闭** | 04-tools.md:165–171 已加格式约定；但 requested/actual/fallback/adapter 的枚举值域未定义（见 N-3） |
| P1-4 | P1 | Rust 侧 NoticeCode / params 类型未定义 | **部分关闭** | 04-tools.md:228–257 已有类型草案；但 NoticeParams 用 untagged 无法可靠反序列化，且字段类型与表不一致（见 N-4） |
| P1-5 | P1 | rule/reason 泄漏敏感信息 | 已关闭 | 04-tools.md:213–218 敏感信息段；08-isolation.md:172–173 reason 枚举 |
| P1-6 | P1 | 预算与不删类矛盾、token 口径不可测 | 已关闭 | 04-tools.md:202–211 ≤3/≤5 条、≤400 UTF-8 字节、合并规则 |
| P1-7 | P1 | §10.1 SLOT_UNAVAILABLE 示例未按模板、绝对路径 | 已关闭 | 09-output.md:149 模板 + <workspace>/... 相对路径；153–154 structuredContent 字段 |
| P2-1 | P2 | L0/L1/L2 与成本阶梯重名 | 已关闭 | 04-tools.md:194 G0/G1/G2 与 L0–L4 无关；90-appendices.md:73 |
| P2-2 | P2 | §9.0 REDACTED 引用 §8.7 错误 | 已关闭 | 09-output.md:24 改为「脱敏规则见 §8.10/§8.11」 |
| P2-3 | P2 | §4.4 与 §4.4b 类别/通道不一致 | 已关闭 | 04-tools.md:99–103「至少包括 11 个 notice code」+ notices 结构化说明 |
| P2-4 | P2 | L0 清单与 8 code 不对应 | 已关闭 | 04-tools.md:198 G0 = 11 code 全部；208–211 四类 |
| P2-5 | P2 | 程序性表述与模板渲染矛盾 | 已关闭 | 04-tools.md:123 改为「默认只进 structuredContent；当它是 notice 模板的必填字段时作为模板一部分进 content」 |
| P2-6 | P2 | 多条 notice 顺序未定义 | 已关闭 | 04-tools.md:182–187 固定渲染顺序 |
| P2-7 | P2 | §18.7 govern 响应无 notices | 已关闭 | 18-deployment.md:442 structuredContent 加 notices，meta 为派生视图 |
| P2-8 | P2 | §13.3 ToolResponse 差距表缺 notices | 已关闭 | 11-runtime.md:108 已加 notices |
| P2-9 | P2 | LOW_CONFIDENCE 0.85 与 need_pixels 0.2/0.8 关系未说明 | 已关闭 | 06-cost.md:176–179 两条轴说明 |
| P2-10 | P2 | §10.2d 压缩熔断告知未定义 | 已关闭 | 09-output.md:283–286 lossy_compression / compression_exceeded |
| P2-11 | P2 | 模板改动未提 serverInfo.version 同步 | 已关闭 | 04-tools.md:191–192；15-risk.md:177 |
| P2-12 | P2 | §15.7 告知测试未覆盖预算与既有告知点 | **部分关闭** | 15-risk.md:138–141 已加预算/敏感/组合快照；但告知完备性只列 7 个路径，缺 REDACTED/PARTIAL/SLOT_UNAVAILABLE/LOW_CONFIDENCE（见 N-5） |
| P3-1 | P3 | 附录 C notice 定义与信息分级术语不完整 | 已关闭 | 90-appendices.md:71–74 notice/告知协议/G0–G2/PARTIAL |
| P3-2 | P3 | PARTIAL 与 Task Result partial 同名不同层 | 已关闭 | 90-appendices.md:74 |
| P3-3 | P3 | notice code 与 ErrorCode 命名不一致 | 已关闭 | 04-tools.md:147–149 明确两个命名空间 |
| P3-4 | P3 | §8.7 示例 reason 语义不符 | 已关闭 | 08-isolation.md:172–173、176–177 reason 枚举 |
| P3-5 | P3 | §8.7 字段表与 §4.4b 风格不统一 | 已关闭 | 08-isolation.md:170–173 代码体字段名；04-tools.md:151–163 params 带类型 |

---

## 2. 未关闭项（P0）

### P0-1 模板总则的分隔符仍与全部 11 个示例矛盾

**位置**：04-tools.md:189–190（总则）、153–163（11 个渲染示例）、08-isolation.md:176–177（§8.7 示例）。

**现象**：修复把值统一成了 key=value，§8.7 也统一为 requested/actual/rule/reason，这部分已修好。但总则仍写：

> 模板语法固定为 [CODE] key=value · key=value：字段顺序即上表顺序，分隔符固定为「 · 」，值内不允许出现分隔符与空格。

而 11 个渲染示例全部是空格分隔，没有「 · 」。例如：
- 04-tools.md:153 [TRUNCATED] kept=2000 total=8734 unit=lines pointer=spill://ab12cd34
- 04-tools.md:156 [ISOLATION_FALLBACK] requested=raw actual=structure rule=**/.env* reason=retention_below_threshold
- 08-isolation.md:176 [ISOLATION_FALLBACK] requested=raw actual=redact rule=**/config/** reason=redact_optimal

**为什么仍是 P0**：§15.7 的「告知渲染快照（每个 code 一组固定 params，断言完整字节）」要求模板有一个确定的字节形态；总则与全部示例不一致，实现者只能二选一，任选一个都会有另一处规范被违反。这是首轮 P0-1 的核心问题，修复只完成了「key=value」，没有完成「分隔符统一」。

**修复建议**：把总则的分隔符改为「字段间用单个空格分隔」，与 11 个示例和 §8.7 一致（最小改动）；若坚持「 · 」，则 11 个示例 + §8.7 两个示例都要改，且 §15.7 的期望字节需重写。建议前者。

---

## 3. 部分关闭项

### P1-3 模板数值格式已定义，但枚举值域未闭合

**位置**：04-tools.md:165–171（格式约定）、151–163（params 列）。

**现象**：浮点两位、百分比一位、整数无千分位、枚举 snake_case、列表逗号+URL 编码、空值省略、reason 有限枚举——这些已修好。但以下 params 只写 enum，没有列出合法值：
- DEGRADED 的 requested / actual；
- ISOLATION_FALLBACK 的 requested / actual；
- SLOT_UNAVAILABLE 的 fallback（示例只出现 metadata，18-deployment.md:66 又出现 loading）；
- SNIFFED 的 actual；
- UNCALIBRATED 的 adapter。

**为什么是问题**：字节稳定测试需要固定值域；不同实现可能给 summary vs summary_only、loading vs model_loading，快照无法断言。仍是原 P1-3「枚举格式未定义」的一部分。

**修复建议**：为每个 enum 参数列出合法值（如 DEGRADED.requested ∈ {raw, summary, structured, visual, auto}；DEGRADED.actual ∈ {raw, summary, structured, none, metadata}；ISOLATION_FALLBACK.requested ∈ {none, understand, summary, raw}；actual ∈ {redact, structure, summarize, refuse}；SLOT_UNAVAILABLE.fallback ∈ {metadata, loading, mechanical}；SNIFFED.actual ∈ {binary, jpeg, png, text, …}；UNCALIBRATED.adapter ∈ {json, text, ordered}）。

### P1-4 Rust 类型草案存在，但 NoticeParams 的 untagged 形式不可直接实现

**位置**：04-tools.md:240–254。

**现象**：NoticeParams 标注 #[serde(untagged)]。反序列化时 serde 按变体声明顺序尝试匹配字段：Degraded { requested, actual, reason, slot: Option<String> } 在前，IsolationFallback { requested, actual, rule, reason } 在后。ISOLATION_FALLBACK 的 JSON 没有 slot（Option 可缺省），但多出 rule；Degraded 变体会忽略未知字段 rule 并成功匹配（若 serde 未配置 deny_unknown_fields），导致 ISOLATION_FALLBACK 被反序列化成 Degraded。序列化方向也不稳定：同一结构有两个可匹配变体。此外草案里 requested: String / actual: String 与表里的 enum 不一致，Sniffed.actual: String 与 actual:enum 不一致。

**为什么是问题**：类型草案是 §15.7「match 无 _ 通配分支」的编译目标；untagged 使反序列化歧义，测试可能误判通过。

**修复建议**：改为外部 tagged——Notice { code: NoticeCode, params: NoticeParams } 配合手动按 code 匹配，或 #[serde(tag = "code", content = "params")] 的单一 enum；把 requested/actual/fallback/adapter 定义为独立 enum；明确每个 params struct 的字段命名（camelCase，与 §4.4b 的 structuredContent 声明一致）。

### P2-12 §15.7 的告知完备性测试仍未覆盖全部 11 个 code

**位置**：15-risk.md:139–141、04-tools.md:259–262。

**现象**：告知完备性写「截断 / 降级 / 回退 / 归一化 / 非校准 / 有损解码 / 嗅探」——覆盖 TRUNCATED、DEGRADED、ISOLATION_FALLBACK、NORMALIZED、UNCALIBRATED、LOSSY_DECODE、SNIFFED，缺 REDACTED、PARTIAL、SLOT_UNAVAILABLE、LOW_CONFIDENCE。预算测试只构造「安全类最坏组合（5 条）」，没有覆盖 11 个 code 全触发后的合并结果。

**为什么是问题**：§4.4b 自己说「G0 必给：11 个 code 的全部告知」「漏一条告知是决策事故」；测试只覆盖 7 条路径，另外 4 个 code 的回归保护缺失。

**修复建议**：告知完备性改为「逐个 code 至少一条路径断言 content 含对应 code」；预算测试补「11 个 code 全触发 + 合并后 ≤5 条且 ≤400 字节」的用例。

---

## 4. 新引入问题

### N-1（P1）finalize_notices 只有一句调用，缺少签名、输入类型与属性测试

**位置**：04-tools.md:283–286（§4.5）、137–145（§4.4b 两段生成）、15-risk.md:136（决策表属性测试）。

**现象**：§4.5 只写「执行后由 finalize_notices(decision_notices, observed) 合并执行事实类告知（截断量、脱敏计数、d1 概率等，§4.4b「生成分两段」）」。没有：
- finalize_notices 的输入/输出类型（observed 是 struct 还是 map、包含哪些字段、每个字段的来源）；
- 纯函数要求的具体内容（同输入同输出、notice 顺序按 §4.4b 的固定顺序）；
- §15.7 的属性测试只写「决策表属性测试 | §4.5 的纯函数 | 随机输入，断言不越 max_exposure、fallback 链合法、同输入同输出」——只覆盖 decide_output，不覆盖 finalize_notices。

**为什么是 P1**：这是本次修复为了关闭 P0-2 而新增的核心函数，承担 8 个执行事实类 code 的生成；没有契约，实现者无法写，也无法测试。P0-2 的修复方案 B 只完成了「有这个名字」，没有完成「有契约」。

**修复建议**：在 §4.5 补 finalize_notices 的完整签名与输入类型（struct Observed { truncation: Option<TruncationObs>, redaction: Option<RedactionObs>, d1: Option<D1Obs>, match_level: Option<NormalizeObs>, decode: Option<DecodeObs>, sniff: Option<SniffObs>, compression: Option<CompressionObs>, partial: Option<PartialObs> }），并在 §15.7 增加 finalize_notices 的属性测试（同输入同输出、输出 code 只来自执行事实类、渲染顺序固定）。

### N-2（P2）DEGRADED 同时出现在决策阶段与执行后阶段，归属矛盾

**位置**：04-tools.md:141–142（两段生成表）、264–286（§4.5）。

**现象**：§4.4b 的「决策（decide_output）」行把 DEGRADED 列为产出一；「执行后（finalize_notices）」行又列「DEGRADED（压缩熔断）」。§4.5 说「decision_notices 只含本阶段能决定的形态类告知（ISOLATION_FALLBACK / DEGRADED / SLOT_UNAVAILABLE）」——两个阶段都可能产 DEGRADED，且 §4.5 没有说明压缩熔断的 DEGRADED 属于哪个阶段。

**为什么是问题**：实现者无法确定 DEGRADED 的生成位置；同一 code 在两处生成会导致去重/顺序问题（§4.4b「同 code 多条按产生顺序」）。

**修复建议**：明确拆分 DEGRADED 的触发源——决策阶段只产「延迟超预算/上游能力不支持」的 DEGRADED；执行后阶段产「压缩熔断」的 DEGRADED；在 §4.4b 表加「阶段」列或在 §4.5 说明 decision_notices 不含压缩熔断类。或者把压缩熔断单独定义为一个 code（如 COMPRESSION_FALLBACK），避免同一 code 跨阶段。

### N-3（P2）enum 参数的合法值域未定义

**位置**：04-tools.md:151–163。

**现象**：同 P1-3 的展开（DEGRADED/ISOLATION_FALLBACK 的 requested/actual、SLOT_UNAVAILABLE 的 fallback、SNIFFED 的 actual、UNCALIBRATED 的 adapter）。这是 P1-3 的残余，单列为新问题以便实现者抓。

**修复建议**：同 P1-3。

### N-4（P2）NoticeParams 的 untagged 反序列化歧义

**位置**：04-tools.md:240–254。

**现象**：同 P1-4 的展开（Degraded 与 IsolationFallback 结构相近，untagged 会按声明顺序误匹配；requested: String vs 表 enum）。

**修复建议**：同 P1-4。

### N-5（P2）§15.7 告知完备性/预算测试覆盖不全

**位置**：15-risk.md:139–141。

**现象**：同 P2-12 的展开（缺 REDACTED/PARTIAL/SLOT_UNAVAILABLE/LOW_CONFIDENCE；预算只测安全类 5 条）。

**修复建议**：同 P2-12。

### N-6（P2）11 个 code 全触发时「四类不能整类删除」与 ≤5 条上限的合并算法未验证

**位置**：04-tools.md:202–211。

**现象**：预算写「常规 ≤3 条；安全类（ISOLATION_FALLBACK、REDACTED）时允许到 5 条；超预算先合并，不删类；四类不能整类删除」。11 个 code 分属四类，其中认知类有 5 个（LOW_CONFIDENCE、UNCALIBRATED、SNIFFED、NORMALIZED、LOSSY_DECODE）。若全部触发，合并到 ≤5 条需要明确的合并算法（哪些可合并、哪些必须保留），文档只给了 count/列表/阈值三类例子，没有给出完整算法；「四类不能整类删除」+「≤5 条」在最坏情况下是否可同时满足没有证明。

**为什么是问题**：预算从「设计目标」变成可测试要求后，实现者需要一个确定性算法；没有算法，§15.7 的预算测试无法构造期望结果。

**修复建议**：给出完整的合并算法（按类优先级 + 同类合并规则 + 跨类保留规则），并用一个 11 code 全触发的例子验证合并后 ≤5 条、≤400 字节。

### N-7（P3）§18.7 的 meta 字段命名与 §4.4b 的 camelCase 声明不一致

**位置**：18-deployment.md:442、04-tools.md:226、06-cost.md:194。

**现象**：§4.4b 说「structuredContent.notices 为数组，空时省略该字段，字段名 camelCase（与 nextSteps 一致）」；§18.7 的 meta 仍是 slm_status、spillPointer 混用 snake_case/camelCase；§7.3 用 slmStatus。同一返回对象出现两套命名。

**修复建议**：统一 meta/structuredContent 字段命名为 camelCase（slmStatus、spillPointer），或在 §4.4b 说明「camelCase 只约束 notices 内部，不约束 meta」。

### N-8（P3）reason 枚举含未使用的 partial_failure，NoticeParams 字段命名未声明

**位置**：04-tools.md:216、240–254。

**现象**：reason 枚举含 partial_failure，但 PARTIAL 没有 reason 参数，其他 code 也用不到；NoticeParams 的字段（如 replacement_count、retention）没有说明是否 camelCase，而 §4.4b 的表用 replacements。

**修复建议**：删除未使用的枚举项，或说明保留理由；给 NoticeParams 加 #[serde(rename_all = "camelCase")] 并在文档列出最终字段名。

### N-9（P3）「同 code 多条按产生顺序」在并发链下可能不稳定

**位置**：04-tools.md:182–187、§4.8（理解链并发）。

**现象**：渲染顺序规定「同 code 多条按产生顺序」。§4.8 的理解链允许无依赖节点并发执行；若两个并发节点都产生同一 code（如两个 NORMALIZED），完成顺序取决于调度，会导致 notice 顺序不稳定、字节不稳定。

**修复建议**：规定「同 code 多条按固定子键排序」（如按 level、路径、slot 字典序），或明确「notice 仅在主线程按固定顺序汇总，不按节点完成顺序」。

### N-10（P3）「值内不允许出现分隔符与空格」与列表参数的逗号分隔未说明例外

**位置**：04-tools.md:189–190、169。

**现象**：总则说「值内不允许出现分隔符与空格」，但 PARTIAL 的 failed:list(csv) 示例 failed=a.rs,b.rs 值内出现逗号（列表分隔符）。如果「分隔符」包含逗号，则模板自相矛盾；如果不包含，需要说明列表参数是例外。

**修复建议**：把总则改为「值内不允许出现『 · 』与空格；列表参数按 §4.4b 的 URL 编码规则处理，逗号仅在列表分隔位置出现」。

---

## 5. 已关闭项确认（23 条）

以下条目已按原报告修复，核对通过：P0-2（两段生成，04-tools.md:137–145）、P0-3（新增 3 个 code 并映射，04-tools.md:160–162、658–659、766、03-slots.md:383–385）、P1-1、P1-2、P1-5、P1-6、P1-7、P2-1–P2-11、P3-1–P3-5。逐条证据见第 1 节总表。

**遗留注意**：P1-6 的 token 口径已改为 UTF-8 字节（04-tools.md:203），可测；P1-1 的 §7.3 第 2 条与 §18.1 第 3 条均已改为「content 模板 + structuredContent 字段」，首轮 P0-3 的 meta 双通道问题已彻底清除。

---

## 6. 结论

本轮修复的主体方向正确：11 个 code 的统一枚举、两段生成、格式约定、敏感信息处理、适用边界与测试层都已落地，首轮 27 条中 23 条关闭。但 **P0-1 的分隔符矛盾仍在**，且 **finalize_notices 的契约缺失**（N-1）使 P0-2 的修复只完成了一半。建议先修这两条（P0-1、N-1），再修 P1-3/P1-4 的枚举值域与类型问题；其余 P2/P3 可批量处理。

---

*本报告只记录复核结论，不修改架构正文；修复落在 docs/architecture/ 对应章节。*
