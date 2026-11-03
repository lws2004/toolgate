# 调研笔记：本地小模型规格

三个模型的官方规格（来源：HuggingFace 模型卡与 Liquid AI 官方文档，
核实时间 2026-11）。

## d1-2B（决策模型）

| 项 | 值 |
|---|---|
| 参数 | 1.92B |
| 基座 | LFM3-VL-2B |
| 上下文 | 128K tokens |
| 词表 | 128,000 |
| 输入 | 一个 state（文本或 JSON）+ 图片/视频 + 任意数量命名问题 |
| 输出 | 带概率的类型化答案，**不生成 token** |
| 延迟 | H100 5.7ms / MI325X 3.8ms 每决策 |
| 语言 | 17 种（含中文） |
| 许可证 | LFM Open License v1.0 |
| 官方定位 | 路由与分诊、审核与护栏、意图分类、文档抽取、判定与 rerank、视觉检查 |

**超出分类的能力**：抽取文本/数字/日期及其 span、在图片中定位对象、
估计数量及区间。

**架构含义**：
1. 不生成 token → 输出受限，不会跑偏，无需解析文本
2. 一次调用可问任意数量问题 → 一个 state 上批量判定
3. 带概率 → 可做置信度门控

来源：https://huggingface.co/LiquidAI/d1-2B

## LFM3-1.7B（文本生成）

| 项 | 值 |
|---|---|
| 参数 | 1.73B |
| 上下文 | 256K tokens（输入 + 输出） |
| 词表 | 128,000 |
| 语言 | 17 种（含中文） |
| 许可证 | LFM Open License v1.0 |
| 官方定位 | 本地 agent、工具调用、结构化输出、领域微调 |
| **明确不适用** | agentic coding、复杂多步推理 |

**架构含义**：不要让 LFM3 写代码或做规划。它做「把长变短、把乱变结构化」。

来源：https://huggingface.co/LiquidAI/LFM3-1.7B

## LFM3-VL-2B（视觉语言）

| 项 | 值 |
|---|---|
| 参数 | 1.92B |
| 输入 | 文本 + 图像 + 视频，支持多图 |
| 能力 | OCR、布局标注、pointing、grounding、视觉引导工具调用 |
| 许可证 | LFM Open License v1.0 |

**grounding 是最有价值的能力**：把「登录按钮」映射到图片里的具体坐标。
这让「看截图改 UI」不必上传原图。

来源：https://huggingface.co/LiquidAI/LFM3-VL-2B

## 同系列其他模型（备选）

| 模型 | 参数 | 备注 |
|---|---|---|
| d1-3B | 3B | 更大的决策模型，d1-2B 在其三分之二体积下领先 |
| d1-Omni-2B | 2.17B | 增加音频输入 |
| LFM3-1.7B-Base | 1.73B | 基座，供微调 |
| LFM3-Audio-2B | 2.00B | 音频 |
| LFM3-Omni-2B | 2.17B | 全模态 |

## 部署变体

官方提供 GGUF / MLX / ONNX / FP8 / NVFP4 变体：

- GGUF → llama.cpp（CPU / Apple Silicon）
- MLX → Apple Silicon 原生
- FP8 / NVFP4 → vLLM / SGLang（NVIDIA GPU）

这决定了 §6.3 的硬件前提表。
