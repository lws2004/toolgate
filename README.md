# toolgate

本地常驻的感知增强层：拦截 agent（pi / dsh）内置工具的输出，在结果进入 agent 上下文之前理解、治理，压成可决策的形状。增强模型可为本地 SLM，也可反向委派给 agent 会话模型。

![Rust](https://img.shields.io/badge/rust-1.85%2B-orange)
![License](https://img.shields.io/badge/license-MIT-blue)
![Status](https://img.shields.io/badge/architectural--review-v2-yellow)

> **状态**：架构 v2（拦截内置工具、进上下文前增强）已定稿；实现按 §14 路线图推进，
> 阶段 0–5 与 §17.2 安全底线已落地，验证基线见
> [实现进度表](docs/implementation-status.md)。
> 本地模型接入层已落地（decide / classify / summarize / rerank / embedding / vision 六槽位适配）；
> `embedded` 推理后端仍属可选层（§19.6），未接入。差距表见
> [架构文档 §13](docs/architecture/11-runtime.md#13-现有实现与新架构的差距)，
> 升级说明见 [v1 → v2 升级指南](docs/upgrade-v1-to-v2.md)。

## 它要回答的问题

不是「怎么执行工具」，而是：

> **进上下文之前，这条结果该被压成什么形状？**

一份 5000 行的失败日志、一张 800KB 的 UI 截图、一次命中 300 条的检索——
这些内容的绝大部分对下一步决策没有用。toolgate 在结果进入 agent 上下文之前，
用增强模型把它们压成「可决策的形状」（本地 SLM 或委派给会话模型皆可）。

## 三层结构

每条返回的增强都是这三层：

```text
执行层   → 读文件、跑命令、检索
理解层   → 本地小模型介入：判定、摘要、抽取、看图
协商层   → 决定回传什么：原文 / 摘要 / 结构化 / 原图 / 指针
```

协商层由新增的 intent 参数控制：raw / summary / structured / visual / auto。

## 增强模型层：六个能力槽位

**架构不绑定任何具体模型**。它定义六个槽位，每个槽位有确定的输入输出
契约与延迟预算；具体模型是可替换的实现。

| 槽位 | 输入 → 输出 | 延迟预算 |
|---|---|---|
| classify | 文本 + 候选标签 → 每标签分数 | < 20ms |
| rerank | query + N 候选 → N 个分数 | < 100ms（N=300） |
| embedding | 文本列表 → 向量列表 | < 100ms / 批（N≤32；实测 71ms） |
| decide | state（可含图）+ 命名问题 → 每问题概率 | < 30ms |
| summarize | 文本 → 文本或 JSON | < 1s |
| vision | 图片 + 任务 → 文本 / 坐标 / OCR | < 2s |

当前参考实现见 [附件 E.2](docs/architecture/95-references.md#e2-槽位参考实现)（正文 §3.4 只讲选型原则）。换模型的条件是
同时满足：输出形态匹配、延迟在预算内（本机实测）、许可证允许、中文达标。

## 六类处理器（按返回形状路由）

| 返回形状 | 增强 |
|---|---|
| 大段命令输出（bash） | 几千行 → 错误摘要 + 落盘指针 |
| 海量检索命中 | 几百条 → rerank 后的 top-k |
| 整文件读 | 结构化提取或相关片段 |
| 图片 | 几 MB 截图 → 元素坐标 / OCR 文本 |
| 写前请求（pre） | 写前复核 + 快照，可 block |

## 快速开始（默认构建，不含外部协议客户端）

> 平台范围：第一期只支持 macOS（Apple Silicon 优先）；Windows / Linux 为后续规划（架构 §19.7）。

```bash
cargo build --release
./target/release/toolgate list
./target/release/toolgate call read '{"path":"Cargo.toml","limit":5}'
./target/release/toolgate serve   # MCP stdio server（仅调试；主路径为接缝适配器）
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

默认构建的测试与验证基线以 [实现进度表](docs/implementation-status.md) 为准，release 构建零警告：

- 双阈值截断（2000 行 / 50KB）
- 按内容位置分治的落盘（read 不落盘，bash/search 落盘）
- 写前事务快照（项目内 .toolgate/snapshots）
- 图片压缩流水线（缩放 → 质量阶梯 → 尺寸兜底 → 熔断）
- 行锚点（可选模式）
- MCP stdio 传输（三版本协商；仅 serve 调试通道）

**剩余项与边界**：可选层（`embedded` 推理后端）与条件触发项（有损 WebP、流式输出）
按 §19.6 / §14 的触发条件处理；逐项状态见 [实现进度表](docs/implementation-status.md)。

## 许可

MIT