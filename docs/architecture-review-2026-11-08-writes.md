# 架构评审：写侧契约与 sidecar 能力边界（2026-11-08）

- 范围：一次改动只解决一个主题 —— 钉清「读」的归属、写前复核的默认强制、sidecar 不覆盖客户端内置工具的写。
- 依据：附件 E.9 的跨项目对照；结论来自 §5.4 / §18.5 / §18.6 / §20.4 既有决策的一致性修订。
- 边界：**未新增工具、未改 ToolSpec / ToolResponse / ToolError 结构、未触及 §2 七条红线、未重编号章节**。

## 评审结论

| 级别 | 条数 | 说明 |
|---|---|---|
| P0 | 0 | 无对外契约变更，无安全边界放宽 |
| P1 | 0 | 无架构章节增删与重编号 |
| P2 | 0 | 交叉引用与链接由 `check-doc.sh` 校验 |
| P3 | 1 | 附件 E.9 属背景记录，正文不重复其措辞 |

## 改动清单

| 文件 | 改动 |
|---|---|
| `docs/architecture/04-tools.md` | §5.4 共同前置补「读的归属」与写前复核行 |
| `docs/architecture/18-deployment.md` | §18.5 补默认强制与进程内锁边界；§18.6 补写侧能力边界表 |
| `docs/architecture/20-behavior.md` | §20.4 补两行覆盖不到的场景，段落指向 §18.6 |
| `docs/architecture/95-references.md` | 新增 E.9 对照与证据等级 |
| `docs/architecture-review-2026-11-08-writes.md` | 本记录 |

## 验证

- `bash scripts/check-doc.sh` → 退出码 0（章节连续性、附录、交叉引用、围栏、占位符、内部链接、规模）。
- `cargo test --workspace` → 57 passed（纯文档改动，代码未动，用于确证无回归）。
- `cargo build --release` → 退出码 0，零警告。
- `cargo clippy --workspace --all-targets` → 退出码 0，无问题。

## 遗留问题

- 不改 §13 差距表：本次是决策澄清，不是功能新增，v1 代码状态未变。
- 不改 README：架构正文是唯一权威，概览层不重复维护同一条说明。
- toolgate v2 实现仍按 2026-11-03 约定暂不启动。

---

*本记录是历史证据，只作待办来源，不重写既有评审结论。*
