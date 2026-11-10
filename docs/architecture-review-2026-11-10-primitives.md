# toolgate 架构评审：三原语与门控第二维（2026-11-10）

> 评审对象：补齐 TypeSafe 三原语（noul / choice / score）的架构决策——§7.2 分布形状门控、
> §3.10 三原语与槽位归属、§18.1 幂等键口径、附件 E.13 对照实测。
> 依据：[architecture.md](architecture.md) 分册与
> [architecture-review-2026-11-09-typed-decide-align.md](architecture-review-2026-11-09-typed-decide-align.md)
> 第 5 节（A-4 否决与重开条件）。
> 本报告是历史证据，只作待办来源，不回写结论。

## 0. 结论

- **不重开 A-4**：不给 decide 契约加问题类型维度，不新增第 6 个槽位，工具层维持 §5.1 的九问二元分解。
- 三处决策补充全部落地（P1 清零），**无代码改动**。
- §2 七条红线未触及：工具数量、MCP 协议、槽位可空、依赖分层、无状态、隔离强度、错误契约均未变。

## 1. 证据

- **实测**：附件 E.13。d3-nano `:8082`，8 条构造样本，对照九问二元分解与同请求混合
  三类原语；另实测 `score` 形状与混合请求（均为 HTTP 200）。探针
  `scripts/primitive_probe.py` 随仓库保留，命令 `python3 scripts/primitive_probe.py`。
- **A-4 触发条件核对**（typed-decide-align §5.3）：条件 1「classify / rerank 有真实调用方」
  不成立——classify 槽位有适配但工具层无调用方；条件 2 仅**部分**成立：互斥性被证伪
  （5/8 样本 ≥2 个候选 >0.5）、门控恒落低档（6/8 <0.60）有证据，但「判错率」在本次
  样本主要归因于两种方法共用的 `has_error` noul 偏低，且样本为构造而非真实错误样本。
- **官方一手**：docs.typesafe.ai 的 choice / score 原语页（2026-11-10 取），形状见 E.13。

## 2. 发现与处置

| # | 发现 | 级别 | 处置 | 落点 |
|---|---|---|---|---|
| P-1 | `score` 无槽位归属，架构未写决策 | P1 | 补三原语完整映射表：无槽位、不新增槽位，量表判定仍由二元分解承接 | 03-slots.md §3.10 |
| P-2 | 分布形状（`confidence` / `none_probability`）与 p 的关系未定义，存在静默转换风险 | P1 | 定义为独立 fail-safe 维度：不混合、不换算，三档阈值不变 | 06-cost.md §7.2 |
| P-3 | 幂等键指纹未含问题类型与 `criteria` 顺序，混合请求可能命中错误台账 | P1 | 指纹补 type + criteria 顺序 | 18-deployment.md §18.1 |
| P-4 | 对照实测未入附件 E，数字无口径 | P2 | 新增 E.13，探针脚本入 `scripts/` 可复现 | 95-references.md |
| P-5 | `has_error` 的 noul 在 6/8 明显错误输出上 <0.5，先于归并否掉 L1 | P3 | 记录为观察项，需真实生产样本单独验证，不改 §5.1 | 附件 E.13 |

## 3. 边界（未做）

- 不改 §3.2 槽位契约，不给 decide 加类型维度；不改 §5.1 / §5.5 的问题定义与归并规则。
- 不改 §7.2 三档阈值（0.85 / 0.60）与 §7.3 的 confidence 取值口径。
- 不动 §13.4 方案 B「不实现」的结论（跨请求聚合前提与三原语无关）。
- 不用 `choice` 替 rerank（§18.1 能力表已判不可用）；不新增工具。

## 4. 验收

- `./scripts/check-doc.sh`：通过（章节连续性、交叉引用、代码围栏、占位符、内部链接、规模；总行数 6611）。
- 无代码改动，Rust 验证基线不变：`cargo test --workspace` 454 passed、
  `cargo test --workspace --features external-protocols` 448 passed（本轮复跑）。
- `git diff --check`：无空白错误。
- P0 / P1 全部落地；P2 已落；P3 记录为观察项，不阻塞。
