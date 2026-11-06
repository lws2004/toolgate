# toolgate 架构设计 · 附录 A–D

> 本文是 [architecture.md](../architecture.md) 的分册，章节编号与总目录一致。
> 返回总目录：[architecture.md](../architecture.md)

## 附录 A：与原设计文档的逐条对照

| 原设计 | 本文档 | 原因 |
|---|---|---|
| 基于 rpi 构建 | 不基于任何框架 | 事实修正 + 中立目标 |
| 哈希锚点是 Pi 特性 | 自选设计，可选模式（§5.4 模式 B） | 事实修正 |
| Apply Patch 扁平文本 | **保留为模式 C**（§5.4） | 归因错（pi 的 patch 是输出），但设计有价值 |
| 图片 1568/WebP/1.2MB 是 Pi 取值 | 是自选参数，保留；WebP 降级为条件触发的优化（§10.2b） | 事实修正，设计保留但不再当目标 |
| 统一落盘 | 按内容位置分治 | read 落盘是浪费（§9.1 说明判据） |
| 超长文本折叠为临时文件 | 能 offset 续读就不落盘，单行超长才落盘 | §9.1 |
| 6 大厚工具 | 6 个，理由是三条约束 | 行业无收敛趋势 |
| **本地 LFM3 摘要** | **升级为核心，且不绑定具体模型**（§3） | v1 移出是错的；现已抽象为槽位 |
| **快思考路由 d1** | **归入 decide 槽位**（jev 类，§3.4） | d1 属于 jev 类，不是与 jev 并列的另一类 |
| 模型适配层 HTTP 解耦 | 进程内/进程外都支持；槽位模型默认进程内，30B 级 agent 模型默认进程外（§18.1） | 兼顾延迟与共享需求 |
| 执行引擎安全底线 | 独立成章（§17） | 原文有列但未展开 |
| checkpoint 在引擎 | 归调用方，契约在 §18.4 定义 | 工具层无状态；只读工具可重放，写工具需调用方 checkpoint + operationId（§18.4） |
| 事务回滚（写前快照） | 已实现，机制见 §20 | v1 曾误判归会话层 |
| Task Input 宏观协议 | **调用方契约，本文档定义**（§18.3） | 不是不做，是归属不同 |
| Task Result 契约 | 同上（§18.3） | |
| 多模态「能力感知」 | **运行时按请求协商**（§18.1b） | 工具层可感知，不必推给调用方 |
| 缓存命中 | 独立成章（§16） | 原文列为核心目标但未展开 |
| 1+1>2 | §2.1 说明乘法效应的确切含义 | 原文列为目标但未展开 |
| 插件 SDK 形态 | 不做（§18.2） | 与常驻、跨 agent 复用目标冲突 |
| 无（原文未提） | 能力槽位抽象（§3） | 架构不绑定模型，换模型不动架构 |
| 无（原文未提） | 隔离强度外部可配（§8.3-8.7） | 被约束方不能决定约束 |
| 无（原文未提） | 编排归属与编排下沉（§21、§21.7） | 审阅时补：一次调用里用多个工具，以及本地模型会多步之后角色怎么变 |
| 无（原文未提） | 编码与行尾保真（§5.6） | 读改写不能破坏用户文件 |
| Rust 强约束 | §19.1 说明约束的是四类错误，不是「性能」 | 标题支柱之一，之前未展开 |
| 通用 | §19.5 四个不绑定 + 明确不等于万能 | 标题支柱之一，之前未展开 |
| CacheRouter 支撑端云协同 | 只支撑工具表稳定 | 论文无端云论述 |
| LFM3-VL | LFM3-VL-2B | 名称精确性 |

---

## 附录 B：与 pi / DSH 的对照

| 能力 | pi | DSH | toolgate v2 |
|---|---|---|---|
| 工具数 | 8 | 20+ | 6 |
| 输出截断 | 双阈值 | 两层（token + 字符） | 双阈值 + 语义层 |
| 落盘 | 仅 bash | 有 spill | 按内容位置分治 |
| 图片 | 压缩 | 调用侧插件处理 | 压缩 + 本地理解 |
| 本地 SLM | 无 | 无 | **核心** |
| 会话 | JSONL 树 | JSONL.zstd + fork | 不做 |

---

## 附录 C：术语表

| 术语 | 含义 | 首次定义 |
|---|---|---|
| F1 / F2 / F3 | 三种使用形态：直连云端 agent / PTC 云端 agent / 本地 agent | §6.2 |
| L0–L4 | 成本阶梯：确定性 / 判定 / 生成 / 视觉 / 云端 | §2.1、§6.1 |
| 槽位 | 能力契约，不是具体模型：classify / rerank / decide / summarize / vision | §3.1 |
| intent | 调用方表达的输出偏好：raw / summary / structured / visual / auto | §4.2 |
| spill | 超限输出的落盘副本 + 指针 | §9.1 |
| snapshot | 写前保存的旧内容副本 | §20 |
| 信任域 | 共享同一套隔离配置的一组客户端；一个 server 实例 = 一个信任域 | §18.2 |
| turn | 客户端的一次任务回合；在无状态协议下由 `_meta.toolgate/turn` 显式传递，可选 | §13.4 |

---

## 附录 D：配置参考

一份完整配置示例（字段名与 §3.8 / §8.5 / §8.11 / §8.12 / §18.1 一致）：

```yaml
intent:
  default: raw            # v2 首版 raw；auto 需显式开启（§4.2）

isolation:
  default: summary
  max_exposure: raw
  rules:
    - glob: "**/.env*"
      level: none
    - glob: "**/secrets/**"
      level: none
    - glob: "**/*.log"
      level: summary
  content_rules:          # §8.11，覆盖 bash/search 的输出
    - kind: secret_pattern
      level: redact
  fallback_chain: [redact, structure, summarize, refuse]
  redact_min_retention: 0.5

slots:                    # 不写就是没有（§3.8）
  classify:  { model: d1-2B, provides: [probability] }
  rerank:    { model: jina-reranker-v3.5-mlx }
  decide:    { model: d1-2B, provides: [probability, span, multimodal] }
  summarize: { model: lfm3-1.7b, provides: [text, structured, long_context] }
  vision:    { model: lfm3-vl-2b }

slm:                      # §18.1
  mode: embedded          # embedded | http
  embedded:
    backend: llama-cpp    # llama-cpp | mlx
    model_dir: ~/.toolgate/models
  http:
    base_url: http://127.0.0.1:8080

limits:                   # §9.1
  max_lines: 2000
  max_bytes: 51200

spill:                    # §8.12
  ttl_days: 7
  max_bytes: 1073741824

snapshot:                 # §20
  keep: 20

cache:                    # §16.3
  max_bytes: 536870912

logging:                  # §15.2
  level: warn
```

**加载顺序**：CLI 参数 > 环境变量 `TOOLGATE_*` > 配置文件（`--config`，默认 `~/.toolgate/config.yaml`）> 内置默认。
**校验**：配置错误启动即失败（fail loud），不静默降级。
**热加载**：不支持；改配置需重启（§8.8）。
