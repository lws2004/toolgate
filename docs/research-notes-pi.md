# 调研笔记：pi 真实机制（含与原始设想的差异）

本文件记录对 pi（@earendil-works/pi-coding-agent）源码的实测结论，
用于校正架构文档中的事实性描述。行号基于 2026-11 本机安装版本。

## 一、工具集实况

pi 内置 8 个工具，全部平铺暴露（dist/core/tools/index.js:19-28）：

    read / bash / powershell / edit / write / grep / find / ls

**没有** search 聚合工具，**没有** task 后台任务工具，**没有** hashline 编辑模式。

## 二、与原始设计文档的四处事实差异

### 差异 1：图片最长边不是 1568，且没有 WebP

实际默认值（dist/utils/image-resize-core.js:8-15）：

    maxWidth: 2000, maxHeight: 2000,
    maxBytes: 4.5 * 1024 * 1024,   // base64 文本长度
    jpegQuality: 80

- 质量阶梯：new Set([jpegQuality, 85, 70, 55, 40]) → 默认 [80, 85, 70, 55, 40]
- 输出格式：**PNG 或 JPEG**，webp 仅作为输入被识别，不产出
- 熔断阈值量的是 **base64 编码后文本长度 < 4.5 MiB**，不是原始字节
- 兜底：尺寸按 0.75 递减至 1x1，仍超限则整图丢弃并提示
- 缩放参数是**模型级配置**（ctx.model.inputLimits.images.resize），不是全局常量

### 差异 2：read 截断不落盘

双阈值（truncate.js:10-11）：2000 行 / 50KB，先到先切。

但只有 **bash 工具**把完整输出落盘（output-accumulator.js:198-212）。
read 工具截断后只追加一行提示 [Showing lines A-B of N. Use offset=M to continue.]，
全程不创建临时文件。单行超 50KB 时给的是 bash 兜底命令，不是文件指针。

落盘路径规则（utils/output-files.js:14-17）：

    <OS tmpdir>/<prefix>-<16位hex>.log，权限 0o600，打开标志 wx

### 差异 3：compaction 不是多级压缩

docs/compaction.md:16-23 明确是**两种并列机制**：阈值压缩 + 分支摘要，
不是多级流水线。触发条件：

    contextTokens > contextWindow - reserveTokens    // reserveTokens 默认 16384
    keepRecentTokens 默认 20000

压缩是**追加**一条 CompactionEntry，不是替换（compaction.md:83）：

> Omitted raw entries remain stored but do not affect cut selection,
> summaries, checkpoints, or token estimates.

摘要前序列化会丢弃 thinking block，tool result 截断到 2000 字符。

### 差异 4：MCP 图片不降级为文本

pi 作为 MCP client 时，image block 原样透传（extensions/mcp/tools.js:152）：

    content: [{ type: "text", text }, ...content.filter(b => b.type === "image")]

文本超 20KB（MCP_OUTPUT_MAX_BYTES）时挖掉中间、全文落 .txt，
但 image block 被单独过滤出来**全部保留**。

## 三、pi 真正值得借鉴的三个机制

### 1. 双阈值截断（truncate.ts）

2000 行 / 50KB 先到先切；头部截断用于读文件，尾部截断用于命令输出；
永不返回半行（尾截断末行超限时例外，标记 lastLinePartial）。

### 2. 流式输出累积 + 落盘（output-accumulator.ts）

增量解码、只保留解码尾部做展示、超限时开临时文件保留全文，
snapshot() 返回 fullOutputPath。落盘文件名含随机 hex，避免冲突。

### 3. 工具声明的 token 预算（codemode）

工具声明共享 3000 估算 token 预算，超出部分靠 searchTools()/describeTool()
运行时发现。这是「工具集收敛」的另一种答案：不是减少工具数量，
而是减少**默认暴露**的声明体积。

## 四、按需加载的真实做法

- skill：系统提示只放 name + description + path，不放全文（docs/skills.md:43-45）
- 工具：promptSnippet 一行进系统提示，完整 schema 走 tool 声明
- MCP：四档 exposure（codemode / deferred / direct / hidden）

## 五、明确不存在

| 机制 | 结论 | 证据 |
|---|---|---|
| LINE#HASH 行锚点 | 不存在 | 全 dist 检索无实现 |
| 扁平 patch 文本输入 | 不存在 | edit 输入是 {path, edits:[{oldText,newText}]}，patch 只是输出 |
| task 后台任务工具 | 不存在 | 仅 8 个工具，无 run_in_background |
| 多级压缩 | 不存在 | 只有阈值压缩与分支摘要两种并列机制 |
