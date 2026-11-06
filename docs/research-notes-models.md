# 调研笔记：本地模型层

本地模型层按「答案形状」分四类，不是按「模型大小」分。

## 一、jev 类（类型化判定）

**jev 不是某个模型，是一类输出形态**：受限输出、带概率/margin、一次前向出结果。

d1-2B 属于这个族。族内已知成员：

| 实现 | 规模 | 基座 / 读法 | 中文 | 多模态 | 备注 |
|---|---|---|---|---|---|
| openJev Verdict v1.4 | 151M | GLiClass | 失效，需微调 | 否 | 最小 |
| JevK5 | — | 官方 GGUF | — | 否 | 本机可跑 |
| StartLux-Decision | — | 官方 MLX | — | 否 | Apple Silicon |
| NeoHorse-Jev-4B | 4B | llama.cpp Metal | — | 否 | 更大更强 |
| CLM-v0.1-8B | 8B | 官方 head | 塌缩，需微调 | 否 | |
| **d1-2B** | 1.92B | LFM3-VL-2B | 开箱可用 | **是** | 族内唯一多模态 |

### d1-2B 官方规格

| 项 | 值 |
|---|---|
| 参数 | 1.92B |
| 上下文 | 128K |
| 输入 | state（文本或 JSON）+ 图片/视频 + 任意数量命名问题 |
| 输出 | 带概率的类型化答案，不生成 token |
| 延迟 | H100 5.7ms / MI325X 3.8ms |
| 语言 | 17 种含中文 |
| 许可证 | LFM Open License v1.0 |
| 额外能力 | 抽取文本/数字/日期及 span；图中定位对象；估计数量及区间 |

来源：https://huggingface.co/LiquidAI/d1-2B

## 二、listwise reranker（候选排序）

### 本机已装

路径：~/.models/jina-reranker-v3.5-mlx-merged

| 项 | 值 |
|---|---|
| 架构 | JinaForRanking（MLX 移植） |
| 参数 | 约 570M（1.1GB bf16，hidden_size 1024） |
| 打分 | embed_match（投影隐状态余弦相似度） |
| 处理 | **listwise**，block_size = 125 文档/次前向 |
| 多块融合 | 按每块最高分给 query embedding 加权后统一打分 |
| 输出 | relevance_score 降序 + index，可选 512 维 embedding |
| 依赖 | mlx + mlx-lm + tokenizers（不需要 transformers） |
| **许可证** | **cc-by-nc-4.0（非商用）** |
| base_model | jinaai/jina-reranker-v3.5-exps |

**关键：listwise 不是 pairwise**。pairwise cross-encoder 一次处理一个
(query, doc) 对，N 个候选 = N 次前向；listwise 一次处理一整块候选。
300 条命中时：pairwise 300 次前向，listwise 3 次。

**许可证是硬约束**：cc-by-nc-4.0 不允许商用。商用需换 Apache/MIT 的
reranker（如 bge-reranker-v2-m3，约 568M）。

## 三、LFM3-1.7B（生成式）

| 项 | 值 |
|---|---|
| 参数 | 1.73B |
| 上下文 | 256K |
| 语言 | 17 种含中文 |
| 许可证 | LFM Open License v1.0 |
| 官方定位 | 本地 agent、工具调用、结构化输出、领域微调 |
| **明确不适用** | agentic coding、复杂多步推理 |

官方原文："not recommended for agentic coding or complex multi-step
reasoning due to its small size"。

**这是 1.7B 的边界，不是「本地模型」的边界。** 2026 年 30B 级开源模型在
工具调用基准上已进入与云端前沿同一档，架构文档 §1.2 已按此修正论证方式。

来源：https://huggingface.co/LiquidAI/LFM3-1.7B

## 四、LFM3-VL-2B（视觉生成）

| 项 | 值 |
|---|---|
| 参数 | 1.92B |
| 输入 | 文本 + 图像 + 视频，支持多图 |
| 能力 | OCR、布局标注、pointing、grounding、视觉引导工具调用 |
| 许可证 | LFM Open License v1.0 |

grounding 是最有价值的能力：把「登录按钮」映射到图片具体坐标。

来源：https://huggingface.co/LiquidAI/LFM3-VL-2B

## 五、为什么 d1 和 VL 都保留

d1 的基座就是 LFM3-VL-2B，但输出形态不同：

- VL 做**生成**：描述图里有什么、OCR 出文字、给坐标。输出是自由文本。
- d1 做**判定**：在给定问题集上给概率答案。输出受限。

要生成内容用 VL，要判定用 d1。

## 六、选型判据

| 要什么 | 用什么 |
|---|---|
| 几选一的答案，纯文本 | jev 类（族内按规模/中文选） |
| 几选一的答案，要看图 | d1-2B |
| 给一批候选排序 | listwise reranker |
| 一段生成的文字 / JSON | LFM3-1.7B |
| 图里有什么 / 在哪 / 什么字 | LFM3-VL-2B |
| 短程多步工具调用（3-5 步、工具集受限、每步可验证） | 30B 级本地模型（2026 年已可用，见架构 §1.2） |
| 长程规划、跨文件推理、写代码 | 云端 |

## 七、部署变体

Liquid AI 官方提供 GGUF / MLX / ONNX / FP8 / NVFP4 变体：

- GGUF → llama.cpp（CPU / Apple Silicon）
- MLX → Apple Silicon 原生
- FP8 / NVFP4 → vLLM / SGLang（NVIDIA GPU）

jina-reranker-v3.5-mlx 是社区 MLX 移植，直接可用。
