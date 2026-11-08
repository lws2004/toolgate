# toolgate 架构评审终审 2（signoff-2）：P1-4 与 N-1 关闭确认

> 终审对象：docs/architecture-review-2026-11-08-signoff.md 残留的 P1-4（Rust 类型可编译性）与 N-1（ObservedFacts 缺 uncalibrated），以及同轮 C-1~C-6 复查。
> 核对方式：04-tools.md §4.4b Rust 草案逐类型核对 + §4.5 ObservedFacts/finalize_notices 核对 + serde 版本核对（Cargo.lock: serde 1.0.229）+ §15.7 测试表交叉核对 + ./scripts/check-doc.sh 实跑（退出码 0）。
> 结论：**P1-4 已关闭；N-1 已关闭。P0/P1 清零，可进入实现阶段评审。**

---

## 0. 结论摘要

| 项目 | 状态 | 依据 |
|---|---|---|
| P1-4 Rust 类型 | **已关闭** | Notice 11 个变体字段类型全部可解析；13 个值枚举全部有 derive 与 serde 属性；序列化示例与模板一致 |
| N-1 finalize_notices | **已关闭** | ObservedFacts 9 个 Option 覆盖 9 个执行事实类 code；9 个 *Obs 结构全部定义；函数签名与约束完整 |
| C-1 ~ C-6 | **已关闭** | 逐项见 §3 |

**残余 P2/P3：4 条（不阻塞签收，见 §4）。**

---

## 1. P1-4 核对

### 1.1 类型引用全部可解析（无未定义类型）

Notice（04-tools.md:255–267）11 个变体的字段类型逐个核对：

| Notice 变体 | 字段类型 | 定义位置 |
|---|---|---|
| Truncated | TruncUnit | 291 |
| Degraded | OutputShape / DegradedActual / DegradeReason | 295 / 299 / 337 |
| LowConfidence | ConfidenceViews | 323 |
| IsolationFallback | IsolationLevel / FallbackAction / IsolationReason | 303 / 307 / 341 |
| SlotUnavailable | SlotFallback | 311 |
| Redacted | u32 / f32（内置） | — |
| Partial | u32 / Vec<String>（内置） | — |
| Normalized | NormalizeLevel / NormalizeKind | 330 / 327 |
| Uncalibrated | AdapterKind | 315 |
| LossyDecode | String / u32（内置） | — |
| Sniffed | SniffActual | 319 |

13 个值枚举（TruncUnit、OutputShape、DegradedActual、IsolationLevel、FallbackAction、SlotFallback、AdapterKind、SniffActual、ConfidenceViews、NormalizeKind、NormalizeLevel、DegradeReason、IsolationReason）全部有定义；上一轮指出的 ConfidenceViews / DegradeReason / IsolationReason / NormalizeKind 四个缺失类型均已补齐。**无未定义类型。**

### 1.2 serde 属性自洽

- Notice：#[derive(Serialize, Deserialize)] + #[serde(tag = "code", content = "params", rename_all = "SCREAMING_SNAKE_CASE", rename_all_fields = "camelCase")]（04-tools.md:251–255）——外部 tag、code 为变体名、params 为内容，结构与数据结构一致。
- 13 个值枚举全部 #[derive(Serialize, Deserialize)] + #[serde(rename_all = "snake_case")]；NormalizeLevel 用 #[serde(rename = "2")] / #[serde(rename = "3")]（329–333）。
- serde 版本核对：Cargo.lock 为 serde 1.0.229，支持 rename_all_fields（1.0.181+）。
- Notice::code() 为 11 分支穷举 match，返回 &'static str（270–286），无 _ 通配；新增变体会编译失败。
- 序列化示例（344）：{"code":"TRUNCATED","params":{"kept":2000,"total":8734,"unit":"lines","pointer":"spill://ab12cd34"}} —— 与模板示例 153 的 unit=lines、格式约定 168 的「枚举值 snake_case」一致。C-1 关闭。

### 1.3 结论

**P1-4 已关闭。** 类型可解析、serde 属性自洽、序列化与模板一致。

---

## 2. N-1 核对

### 2.1 ObservedFacts 覆盖 9 个执行事实类 code

ObservedFacts（04-tools.md:381–391）9 个 Option 与 finalize_notices 产出的 9 个执行事实类 code 逐一对应：

| 执行事实 code | ObservedFacts 字段 | *Obs 结构定义 |
|---|---|---|
| TRUNCATED | truncation | TruncationObs（393） |
| REDACTED | redaction | RedactionObs（394） |
| LOW_CONFIDENCE | confidence | ConfidenceObs（395） |
| PARTIAL | partial | PartialObs（396） |
| NORMALIZED | normalization | NormalizationObs（397） |
| LOSSY_DECODE | decode | DecodeObs（398） |
| SNIFFED | sniff | SniffObs（399） |
| DEGRADED（压缩熔断） | compression | CompressionObs（400） |
| UNCALIBRATED | uncalibrated | UncalibratedObs（401） |

9 个 *Obs 结构全部定义，字段与对应 Notice 变体一致（kept/total/unit/pointer、count/retention、p/views、done/total/failed、level/kind、encoding/replacements、ext/actual、reason、slot/adapter）。

### 2.2 函数签名与约束

- pub fn finalize_notices(decision: Vec<Notice>, observed: &ObservedFacts) -> Vec<Notice>（403）。
- 只产执行事实类 code（含 DEGRADED 压缩熔断），不产 ISOLATION_FALLBACK / SLOT_UNAVAILABLE（406–408）。
- 输出按 §4.4b 固定渲染顺序排序、同输入同输出（409）。
- §15.7:137 已加「执行事实属性测试」，随机 ObservedFacts，断言输出只含执行事实类 code、渲染顺序固定、同输入同输出。

### 2.3 结论

**N-1 已关闭。** 9 个执行事实类 code 全部有输入来源，9 个 *Obs 结构齐全，函数签名与约束完整。

---

## 3. C-1 ~ C-6 复查

| 项 | 状态 | 依据 |
|---|---|---|
| C-1 序列化/模板枚举大小写 | **已关闭** | 344 行 "unit":"lines" 与 153 行 unit=lines、168 行 snake_case 一致 |
| C-2 meta.noticeOverflow | **已关闭** | 18-deployment.md:442 meta 已加 noticeOverflow；11-runtime.md:108 已同步「含 meta.noticeOverflow」 |
| C-3 Notice/NoticeCode 双 enum | **已关闭** | 独立 NoticeCode 已删除；Notice 为唯一 enum，code() 返回 &'static str |
| C-4 NormalizeLevel / Metadata 归属 | **已关闭** | NormalizeLevel（329–333）用 rename 2/3；OutputShape 不再含 Metadata，DegradedActual 专含 Metadata（299） |
| C-5 Obs 结构未定义 | **已关闭** | 9 个 *Obs 结构全部定义（393–401） |
| C-6 二级键缺省排序 | **已关闭** | 04-tools.md:190 已加「二级键缺省时按 params 序列化后的字典序」 |

---

## 4. 残余 P2/P3（不阻塞签收）

### C-7（P2）CompressionObs 只有 reason，构造 Degraded 变体所需的 requested/actual/slot 未明示

**位置**：04-tools.md:400（CompressionObs）、257（Degraded 变体）、406–408（finalize_notices 产出）。

**现象**：CompressionObs 只有 reason；Degraded 变体需要 requested: OutputShape、actual: DegradedActual、reason: DegradeReason、slot: Option<String>。压缩熔断场景下 requested/actual/slot 没有来源。实现者需自行决定固定值（如 requested=raw、actual=metadata、slot=None）。

**建议**：在 CompressionObs 补 requested/actual/slot，或在 §4.5 写死压缩熔断的固定取值。

### C-8（P3）Notice 的 Option 字段未声明 skip_serializing_if

**位置**：04-tools.md:256、257。

**现象**：pointer / slot 是 Option<String>；serde 默认序列化为 null，而 §4.4b:170 的格式约定「空值字段直接省略，不渲染 key=」适用于模板；structuredContent 的 notices JSON 可能输出 "pointer":null。

**建议**：给 Option 字段加 #[serde(skip_serializing_if = "Option::is_none", default)]，或在 §4.4b 明确「模板省略、JSON 保留 null」的差异。

### C-9（P3）Notice::code() 与 serde tag 两处手写同一组 code 字符串

**位置**：04-tools.md:270–286、251–255。

**现象**：code() 的 11 个返回字符串与 serde rename_all 生成的 tag 值各自维护；改一处忘另一处会漂移。

**建议**：给 NoticeCode 加一个 From/const 映射并由 code() 复用，或加测试断言 code() == serde 序列化的 code 字段。

### C-10（P3）Notice 未 derive Clone / PartialEq / Debug

**位置**：04-tools.md:251。

**现象**：§15.7 的渲染快照、属性测试需要比较 Notice 或打印失败信息；当前只有 Serialize/Deserialize。

**建议**：补 #[derive(Clone, PartialEq, Debug)]（Debug 用于测试断言输出）。

---

## 5. 结论

**P1-4 已关闭；N-1 已关闭。P0/P1 清零，可进入实现阶段评审。**

剩余 C-7~C-10 为 P2/P3 文档补强项，不阻塞实现阶段准入；建议在实现前顺手补 C-7（CompressionObs 字段）与 C-8（Option 序列化），C-9/C-10 可随测试代码一起处理。

---

*本报告只记录终审结论，不修改架构正文；修复落在 docs/architecture/ 对应章节。*
