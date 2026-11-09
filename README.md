# toolgate

围绕本地小模型构建的工具层。云端做规划，本地做理解与执行。

![Rust](https://img.shields.io/badge/rust-1.85%2B-orange)
![License](https://img.shields.io/badge/license-MIT-blue)
![Status](https://img.shields.io/badge/architectural--review-v2-yellow)

> **状态**：架构已迭代到 v2（围绕本地小模型）并已审阅，阶段 0（架构定稿）完成；
> 实现阶段自 2026-11-09 起按 §14 路线图推进，阶段 1 与阶段 1.5 已落地。
> 代码不再是纯 v1 骨架：已落地错误契约、槽位契约、告知协议、决策表、理解链与兼容层；
> 本地模型后端仍属可选层（§19.6），尚未接入。差距表见
> [架构文档 §13](docs/architecture/11-runtime.md#13-现有实现与新架构的差距)，
> 升级说明见 [v1 → v2 升级指南](docs/upgrade-v1-to-v2.md)。

## 它要回答的问题

不是「怎么执行工具」，而是：

> **哪些东西根本不需要上传到云端？**

一份 5000 行的失败日志、一张 800KB 的 UI 截图、一次命中 300 条的检索——
这些内容的绝大部分对下一步决策没有用。toolgate 在数据离开本地之前，
用本地小模型把它们压成「够用的形状」。

## 三层结构

每个工具都是这三层：

```text
执行层   → 读文件、跑命令、检索
理解层   → 本地小模型介入：判定、摘要、抽取、看图
协商层   → 决定回传什么：原文 / 摘要 / 结构化 / 原图 / 指针
```

协商层由新增的 intent 参数控制：raw / summary / structured / visual / auto。

## 本地模型层：五个能力槽位

**架构不绑定任何具体模型**。它定义五个槽位，每个槽位有确定的输入输出
契约与延迟预算；具体模型是可替换的实现。

| 槽位 | 输入 → 输出 | 延迟预算 |
|---|---|---|
| classify | 文本 + 候选标签 → 每标签分数 | < 20ms |
| rerank | query + N 候选 → N 个分数 | < 100ms（N=300） |
| decide | state（可含图）+ 命名问题 → 每问题概率 | < 30ms |
| summarize | 文本 → 文本或 JSON | < 1s |
| vision | 图片 + 任务 → 文本 / 坐标 / OCR | < 2s |

当前参考实现见 [附件 E.2](docs/architecture/95-references.md#e2-槽位参考实现)（正文 §3.4 只讲选型原则）。换模型的条件是
同时满足：输出形态匹配、延迟在预算内（本机实测）、许可证允许、中文达标。

## 六个工具

| 工具 | 本地模型挡住什么 |
|---|---|
| bash | 几千行日志 → 错误摘要 + 落盘指针 |
| search | 几百条命中 → rerank 后的 top-k |
| read（文本） | 整文件 → 结构化提取或相关片段 |
| read（图片） | 几 MB 截图 → 元素坐标 / OCR 文本 |
| task | 累积输出 → 状态判断 + 增量摘要 |
| write / edit | 挡不住什么，但前四个要有输出对象 |

## 快速开始（v1 骨架，无本地模型）

> 平台范围：第一期只支持 macOS（Apple Silicon 优先）；Windows / Linux 为后续规划（架构 §19.7）。

```bash
cargo build --release
./target/release/toolgate list
./target/release/toolgate call read '{"path":"Cargo.toml","limit":5}'
./target/release/toolgate serve   # MCP stdio server
```

接入 MCP 客户端：

```json
{
  "mcpServers": {
    "toolgate": {
      "command": "/绝对路径/target/release/toolgate",
      "args": ["serve"]
    }
  }
}
```

## 文档

| 文档 | 内容 |
|---|---|
| [架构文档](docs/architecture.md) | 总目录与核心命题（§0–§2）；分册清单见总目录的[阅读地图](docs/architecture.md#阅读地图) |
| [AGENTS.md](AGENTS.md) | 参与架构迭代 / 实现 / 评审的 agent 工作规范：红线、流程、验证清单 |
| [调研：pi](docs/research-notes-pi.md) | pi 源码实测，含四处与原设计的差异 |
| [调研：DSH](docs/research-notes-dsh.md) | PTC、两层治理、checkpoint 机制 |
| [调研：外部事实](docs/research-notes-external.md) | CacheRouter、MCP 规范、hashline 溯源 |
| [调研：本地模型](docs/research-notes-models.md) | d1 / LFM3 / VL 的官方规格 |
| [决策记录](docs/decisions/) | 5 份 ADR，含一次判断翻转 |

> 路径基准：上表相对链接以仓库根为基准；分册文件位于 `docs/architecture/`。

## 当前代码状态

v1 骨架，57 个测试通过（以 `cargo test` 输出为准），release 构建零警告：

- 双阈值截断（2000 行 / 50KB）
- 按内容位置分治的落盘（read 不落盘，bash/search 落盘）
- 写前事务快照（项目内 .toolgate/snapshots）
- 图片压缩流水线（缩放 → 质量阶梯 → 尺寸兜底 → 熔断）
- 行锚点（可选模式）
- MCP stdio 传输（三版本协商）

**v2 需要的本地模型接入层尚未实现**——按「先定架构，后写实现」的
顺序，等架构审阅通过再动手。

## 许可

MIT