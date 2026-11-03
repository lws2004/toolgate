# 调研笔记：外部事实核实

本文件记录对外部事实的联网核实结果，用于校正架构文档的前提。
核实时间：2026-11。

## 一、CacheRouter：真实存在，但被误用

- 出处：arXiv:2608.22708，2026-08-24，作者 Donghui Zha 等
- 核心思想：主模型只暴露固定少量核心工具以保持请求前缀不变（从而命中
  prompt cache），长尾工具交给独立路由子模型检索-选择-执行-回传
- 实测：token 级缓存命中率 90.99% / 95.2%，输入成本降到基线的 12.0% / 8.0%
- 证据等级：官方预印本（单篇论文，非项目/产品）

**重要修正**：论文通篇**没有**端云分工，只讲「工具集固定以保 prefix cache」。
把它当作「端云协同」的先例是引申，不成立。它真正支撑的是：
工具声明必须稳定，否则缓存前缀失效。

## 二、LiquidAI LFM3：真实存在，名称需精确

- 发布：2026-10-27 官方博客，开放权重
- 首代 7 个模型，均约 2B 参数：LFM3-1.7B-Base(1.73B)、LFM3-1.7B、
  **LFM3-VL-2B(1.92B)**、LFM3-Audio-2B、LFM3-Omni-2B、d1-2B、d1-Omni-2B
- 许可证：**LFM Open License v1.0**（不是 Apache/MIT，商用前需读 LICENSE）
- 本地跑：官方提供 GGUF / MLX / ONNX / FP8 / NVFP4 变体，llama.cpp 与 MLX 均可行
- 架构：22T tokens 预训练，256K 上下文，24 层 tri-hybrid

**名称修正**：设计文档写「LFM3-VL」不精确，应写 **LFM3-VL-2B**。
「快思考路由 d1」对应 d1-2B（1.92B）。

## 三、pi_agent_rust：项目真实存在且活跃

本机 rpi 是 bash 脚本，注释写明「pi_agent_rust installer managed alias」，
末行 exec ~/.local/bin/pi-rust，而该二进制不存在。
**结论：本机安装残缺，不是项目不存在。**

两个易混淆项目，必须按 URL 区分：

| 项目 | 仓库 | 特征 |
|---|---|---|
| pi_agent_rust | Dicklesworthstone/pi_agent_rust | 1959★，MIT+Rider，零 unsafe，v0.8.0，二进制名 pi-rust |
| pi-rust | bigfish1913/pi-rust | 198★，MIT，v0.16.1-preview，**二进制名就叫 rpi**，提供 rpi-plugin-sdk |

设计文档说的「rpi-plugin-sdk」对应**第二个**项目（bigfish1913/pi-rust）。

## 四、MCP Content Blocks：字段名完全正确

最新稳定 schema：**2026-07-28**。

ImageContent 定义（schema.ts L2340）：

    { type: "image", data: string /* base64 */, mimeType: string,
      annotations?: Annotations, _meta?: MetaObject }

ContentBlock 联合（L2305）：TextContent | ImageContent | AudioContent |
ResourceLink | EmbeddedResource

CallToolResult（L1809）：{ content: ContentBlock[], structuredContent?: unknown,
isError?: boolean }

ResourceLink（L1719）：extends Resource { type: "resource_link" }

**结论**：原始设计文档里 image block 的字段名 {type, data, mimeType} 正确，
但多出的 imageHash 字段不属于 MCP 规范，应放在 _meta 里或 structuredContent。

## 五、hashline 真实存在，但不是 pi 核心特性

- 机制：每行分配 3 字符哈希（A-Za-z0-9），模型用「行号+哈希」锚定要改的行；
  文件变了锚点对不上，补丁在落盘前被拒
- 起源：pi 官方 discussion #5474 原话「I saw this concept implemented in
  the oh-my-pi fork」——pi 核心当时未内置
- 生态实现：pi-hashline-edit-pro、@piex-dev/hashline、DSH 的 dsh-better-edit、
  hermes-agent issue #358
- 反证：anthropics/claude-code issue #25775，state=CLOSED，
  stateReason=**NOT_PLANNED**

**结论**：hashline 是真实存在的生态实践，但把它说成「pi 特性」不准确。
本项目的 hashline 模块属独立实现，与上述生态做法一致。

## 六、工具集没有收敛趋势

- **Claude Code**：官方 tools-reference 列出 **46 个**内置工具，并且专门设了
  **ToolSearch** 工具——用「可发现」而非「少而精」解决工具膨胀
- **Gemini CLI**：约 20 个核心工具
- **Codex CLI**：约 16 个 handler

**结论**：不存在统一收敛趋势。真实趋势是**分层**：内置工具稳定甚至扩张，
外部/MCP 工具走按需加载与工具搜索。

这对本架构的含义：「6 大厚工具」不是业界共识，而是一个**针对端云场景的
取舍**——云端模型的工具声明要尽量稳定以保缓存（CacheRouter 论据），
因此本地网关暴露的工具面要窄且稳定，长尾能力藏在工具内部（bash/task）。

## 七、来源清单

- CacheRouter: https://arxiv.org/abs/2608.22708
- LFM3: https://www.liquid.ai/blog/lfm3
- LFM3 HF 合集: https://huggingface.co/collections/LiquidAI/lfm3
- pi_agent_rust: https://github.com/Dicklesworthstone/pi_agent_rust
- pi-rust: https://github.com/bigfish1913/pi-rust
- MCP spec: https://modelcontextprotocol.io/specification/2026-07-28/server/tools
- pi hashline discussion: https://github.com/earendil-works/pi/discussions/5474
- Claude Code 拒绝 hashline: https://github.com/anthropics/claude-code/issues/25775
- Claude Code 工具表: https://code.claude.com/docs/en/tools-reference
