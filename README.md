# toolgate

端云协同的本地工具层。云端模型做规划，本地进程做执行，两者用 MCP 通信。

![Rust](https://img.shields.io/badge/rust-1.85%2B-orange)
![License](https://img.shields.io/badge/license-MIT-blue)

## 它解决什么问题

把编码 agent 的工具执行下沉到本地，会立刻遇到三个问题：

1. **上下文被大输出撑爆**——一次 find / 一次测试，几十万 token 就没了；
2. **图片太大模型吃不下**——原始截图动辄几 MB，base64 后更夸张；
3. **编辑在漂移的文件上失败**——读到的内容和要改的内容对不上。

toolgate 在工具边界上处理这三件事：超限输出落盘只回传指针，图片多级
降级压缩，编辑用精确匹配或行锚点并在失配时拒绝写入。

## 快速开始

```bash
git clone https://github.com/lws2004/toolgate.git
cd toolgate
cargo build --release

# 列出全部工具
./target/release/toolgate list

# 直接调一个工具（调试用）
./target/release/toolgate call read '{"path":"Cargo.toml","limit":5}'

# 以 MCP server 方式运行，供 agent 接入
./target/release/toolgate serve
```

## 接入 MCP 客户端

在客户端的 MCP 配置里加一条 stdio server：

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

## 六个工具

| 工具 | 做什么 | 治理 |
|---|---|---|
| read | 读文本（带行号）或图片 | 超 2000 行 / 50KB 头部截断，给 offset 续读 |
| write | 新建或整体覆盖 | 自动建父目录 |
| edit | 局部修改 | 文本匹配（含模糊容错）或行锚点；失配拒绝写入 |
| search | 内容检索 / 文件枚举 | 超限保留头部，完整结果落盘 |
| bash | 一次性命令 | 拒绝 shell 字符串；尾部截断 + 落盘 |
| task | 后台长任务 | start / read / kill / list |

## 设计文档

完整架构、前提修正与路线图见 **[docs/architecture.md](docs/architecture.md)**。

那份文档里有一节专门列出**原设计里六处站不住的说法**，以及为什么改。
读它比读 README 有用。

## 当前状态

v0.1，核心能力已实现并有 46 个测试覆盖：

- 双阈值截断、按内容位置分治、内容寻址落盘
- 图片压缩流水线（缩放 → PNG/JPEG 质量阶梯 → 尺寸兜底 → 熔断）
- 行锚点（可选模式，默认关闭）
- MCP stdio 传输，兼容 2026-07-28 / 2025-11-25 / 2025-06-18

未实现（见路线图）：有损 WebP、进程组管理、流式输出、HTTP 传输。

## 明确不做

agent 循环、会话存储、检查点、上下文压缩、提示词构造、模型路由。
这些属于调用侧，工具层做不了也不该做。理由见架构文档 §8。

## 许可

MIT
