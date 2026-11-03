# toolgate

围绕本地小模型构建的工具层。云端做规划，本地做理解与执行。

![Rust](https://img.shields.io/badge/rust-1.85%2B-orange)
![License](https://img.shields.io/badge/license-MIT-blue)
![Status](https://img.shields.io/badge/architectural--review-v2-yellow)

> **状态**：架构已迭代到 v2（围绕本地小模型），**等待审阅**。
> 仓库里的代码是 v1 骨架（无本地模型），与 v2 的差距见
> [架构文档 §13](docs/architecture.md)。

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

## 本地模型的分工

| 模型 | 参数 | 上下文 | 延迟 | 干什么 |
|---|---|---|---|---|
| d1-2B | 1.92B | 128K | **5.7ms/决策** | 分类、路由、门控、rerank、抽取 |
| LFM3-1.7B | 1.73B | 256K | 数百 ms | 摘要、结构化、压缩 |
| LFM3-VL-2B | 1.92B | — | 约 1s | OCR、grounding、视觉理解 |

判据是**答案空间大小**：几选一 → d1；一段话 → LFM3；要看图 → VL；
要规划或写代码 → 云端。

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
| [架构文档](docs/architecture.md) | 完整设计、成本模型、介入判据、路线图 |
| [调研：pi](docs/research-notes-pi.md) | pi 源码实测，含四处与原设计的差异 |
| [调研：DSH](docs/research-notes-dsh.md) | PTC、两层治理、checkpoint 机制 |
| [调研：外部事实](docs/research-notes-external.md) | CacheRouter、MCP 规范、hashline 溯源 |
| [调研：本地模型](docs/research-notes-models.md) | d1 / LFM3 / VL 的官方规格 |
| [决策记录](docs/decisions/) | 4 份 ADR，含一次判断翻转 |

## 当前代码状态

v1 骨架，47 个测试通过，release 构建零警告：

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
