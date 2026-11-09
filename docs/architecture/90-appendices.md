# toolgate 架构设计 · 附录 A–D

> 本文是 [architecture.md](../architecture.md) 的分册，章节编号与总目录一致。
> 返回总目录：[architecture.md](../architecture.md)

> 参考、比较、研究与实测数据统一放在 [95-references.md](95-references.md)（附件 E）；本文件只保留对照表、术语与配置参考。

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
| 模型适配层 HTTP 解耦 | 进程内/进程外都支持；判定槽位默认进程内、生成与批处理槽位默认进程外，30B 级 agent 模型默认进程外（§18.1） | 兼顾延迟、共享与故障隔离 |
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
| 无（原文未提） | 最少依赖原则（§1.5、§19.6） | 依赖是负债：默认层只留六类，其余进可选层或禁止层 |
| 无（原文未提） | 编码与行尾保真（§5.6） | 读改写不能破坏用户文件 |
| Rust 强约束 | §19.1 说明约束的是四类错误，不是「性能」 | 标题支柱之一，之前未展开 |
| 通用 | §19.5 四个不绑定 + 明确不等于万能 | 标题支柱之一，之前未展开 |
| CacheRouter 支撑端云协同 | 只支撑工具表稳定 | 论文无端云论述 |
| LFM3-VL | LFM3-VL-2B | 名称精确性 |
| 本地模型按毫秒计费（§1.1） | 按 prefill 时间 + 固定开销 + 生成时间计费（§6.4） | 2026-11-07 本机实测（§6.4），架构口径取 LFM3-1.7B-8bit |
| 「256K 上下文能一次读完日志」（§9.3） | 落盘保留全量，喂模型只给有界候选窗口（§6.4、§9.3） | 12.8K token 的 prefill 是秒级；窗口上限不是延迟预算 |
| 无（原文未提） | 各槽位本机实测（§6.5） | 2026-11-07 用本地权重校准 §3.1 预算，H100 口径五档无一成立 |

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
| 本地 agent | 进程外、由 30B 级本地模型驱动的 agent 循环，通过 MCP 消费 6 个工具；不是 toolgate 的组件，也不是槽位 | §21.7 |
| 第一期 | v2 首版（阶段 1–4），平台范围只含 macOS（§19.7） | §19.7 |
| L0–L4 | 成本阶梯：确定性 / 判定 / 生成 / 视觉 / 云端 | §2.1、§6.1 |
| 槽位 | 能力契约，不是具体模型：classify / rerank / decide / summarize / vision | §3.1 |
| embedded / http | 槽位模型的两种部署形态：进程内动态库 / 本机回环 HTTP 服务 | §18.1 |
| intent | 调用方表达的输出偏好：raw / summary / structured / visual / auto | §4.2 |
| spill | 超限输出的落盘副本 + 指针 | §9.1 |
| notice | 成功返回中的结构化告知单元：code + params，content 按固定模板渲染、structuredContent 原样携带 | §4.4b |
| 告知协议 | 10 个 notice code 的枚举、模板、预算与测试约定 | §4.4b |
| G0 / G1 / G2 | 返回信息等级：必给 / 默认给 / 按需；与成本阶梯 L0–L4 无关 | §4.4b |
| snapshot | 写前保存的旧内容副本 | §20 |
| 信任域 | 共享同一套隔离配置的一组客户端；一个 server 实例 = 一个信任域 | §18.2 |
| turn | 客户端的一次任务回合；在无状态协议下由 `_meta.toolgate/turn` 显式传递，可选 | §13.4 |

---

## 附录 D：配置参考

**默认配置：无配置文件也能启动。** 所有字段都有内置默认值；只写你要覆盖的字段，
字段语义见对应章节。

### 最小配置

空文件即可。需要本地模型时，只增加要用的槽位：

```yaml
slots:
  summarize: lfm3-1.7b
```

### 常用覆盖

```yaml
slots:
  summarize: lfm3-1.7b
slm:
  mode: http                 # 多客户端共享模型服务时
```

`slots.<name>` 的值是模型 id 字符串（§3.10 的显式覆盖），不是 `{model: ...}` 对象。
`fetch.provider` 等段当前尚未接线，配了会被拒绝（见下节）。

### 默认值

| 字段 | 默认值 | 说明 |
|---|---|---|
| `intent.default` | `raw` | v2 首版保持 v1 行为；`auto` 需显式开启（§4.2） |
| `isolation.default` | `summary` | 默认隔离档 |
| `isolation.max_exposure` | `raw` | 外部策略的硬上限（§8.3） |
| `isolation.rules` | `.env*` / `secrets/**` → `none`；`*.log` → `summary` | 内置路径规则，可追加覆盖 |
| `isolation.content_rules` | `secret_pattern`、`path_username`、`host_metadata` → `redact` | 覆盖 bash/search 输出（§8.11） |
| `isolation.fallback_chain` | `[redact, structure, summarize, refuse]` | 脱敏无意义时的降级顺序 |
| `isolation.redact_min_retention` | `0.5` | 脱敏后保留率下限 |
| `paths.expose_absolute` | `false` | 模型可见路径默认工作区相对；raw 显式开启才绝对 |
| `paths.allowed_roots` | `[启动时工作目录]` | 文件访问允许根；read / write / edit / search 的路径必须在其中（§17.2） |
| `models` | `{}` | 模型清单；每个模型声明 `serves`（能服务哪些槽位，§3.10） |
| `models.<id>.source` | `auto` | `auto`（按 path / base_url 推断）；也可显式 `local` / `openai` / `anthropic` / `responses` / `systemone` / `rerank`（§18.1；`systemone` 与 `rerank` 必须显式写，不参与 auto 推断） |
| `models.<id>.adapter` | `auto` | 形状适配器：`auto` / `native` / `json` / `text` / `ordered`（§3.10） |
| `models.<id>.path` | 空 | `source: local`（或 auto 推断为 local）的权重路径 |
| `models.<id>.base_url` | 空 | 外部来源的接口地址（默认仅 loopback） |
| `models.<id>.model` | 空 | 外部来源的上游模型名 |
| `models.<id>.api_key_env` | 空 | 外部来源的密钥环境变量名，不落配置 |
| `models.<id>.allow_remote` | `false` | 是否允许非 loopback 的外部协议地址（含 `rerank` 等非 OpenAI 兼容端点；不推荐，§18.1） |
| `slots` | 空 | 显式覆盖槽位绑定；默认空 = 按 serves 自动解析（§3.10） |
| `slots.<name>` | 自动 | 槽位 → 模型 id 的显式覆盖（§3.10） |
| `slm.mode` | `auto` | 按槽位分档选择 embedded/http（§18.1） |
| `slm.probe` | `first_use` | 能力校验时机：first_use / startup / off（§3.11） |
| `slm.disable_after_failures` | `3` | 连续失败多少次禁用槽位（§3.11） |
| `slm.embedded.backend` | `macOS mlx`（第一期） | 第一期只实现 macOS/MLX；Linux / Windows 见 §19.7 |
| `slm.embedded.model_dir` | `~/.toolgate/models` | 模型权重目录 |
| `slm.embedded.max_resident_models` | `2` | 热槽位 + 一个冷槽位（§18.1） |
| `slm.embedded.idle_ttl_seconds` | `600` | 冷槽位空闲卸载 |
| `slm.http.base_url` | `http://127.0.0.1:8080` | 进程外模型服务地址 |
| `limits.max_lines` | `2000` | 双阈值截断 |
| `limits.max_bytes` | `51200` | 双阈值截断 |
| `spill.ttl_days` | `7` | 落盘副本 TTL |
| `spill.max_bytes` | `1073741824` | 每工作区上限 |
| `snapshot.data` | `auto` | reflink / copy，见 §20.2b |
| `snapshot.min_free_bytes` | `536870912` | 低于此值拒绝创建快照，不等待 GC |
| `snapshot.keep_per_file` | `20` | 每文件保留最近 N 份 |
| `snapshot.min_retention_hours` | `24` | 保护期 |
| `snapshot.ttl_days` | `30` | 过期时间 |
| `snapshot.max_bytes` | `1073741824` | 每工作区后台 GC 目标 |
| `snapshot.gc_interval_hours` | `24` | 后台巡检间隔 |
| `cache.max_bytes` | `536870912` | 摘要/图片缓存上限 |
| `logging.level` | `warn` | 日志级别 |
| `fetch.provider` | `none` | `read({url})` 的取数适配器；可选 dsh-web-fetch / pi / opencli / moli / command |
| `fetch.transport` | `stdio` | `stdio`（NDJSON）或 `http` |
| `fetch.command` | `[]` | provider=command/dsh-web-fetch 时的命令数组 |
| `fetch.endpoint` | 空 | transport=http 时的 loopback URL |
| `fetch.timeout_ms` | `30000` | provider 单次超时 |
| `fetch.max_bytes` | `10485760` | 返回内容上限，超出由 provider 或 toolgate 截断 |
| `fetch.max_concurrency` | `8` | URL 取数并发上限（受全局 max_concurrency 约束） |
| `fetch.per_host_concurrency` | `2` | 同一 host 并发上限 |
| `govern.endpoint` | `http://127.0.0.1:8099` | DSH/pi 钩子调用治理入口 |
| `govern.timeout_ms` | `30000` | 治理请求超时 |
| `govern.max_concurrency` | `min(CPU×2, 16)` | 治理请求并发上限 |
| `govern.fail_open` | `false` | 治理不可用时默认拒绝，不静默放行 |

### 本版本已实现的配置段

上表是**完整配置面**（架构的最终形态）；但**当前版本只实现并消费其中三段**。
分段状态如下：

| 配置段 | 状态 | 说明 |
|---|---|---|
| `models` | **已实现** | 模型清单解析：`source` / `path` / `base_url` / `model` / `api_key_env` / `allow_remote` / `serves` / `adapter`（§18.1、§3.10） |
| `slots` | **已实现** | 槽位绑定解析：显式覆盖优先，否则按 `serves` 推导（§3.10） |
| `slm` | **已实现** | 槽位运行参数：`mode` / `probe` / `disable_after_failures` / `embedded.*` / `http.base_url`（§18.1、§3.11） |
| `paths.allowed_roots` | **已实现** | 文件访问允许根（§17.2）：默认 `[启动时工作目录]`，可追加扩大 |
| `intent` / `isolation` / `paths.expose_absolute` / `limits` / `spill` / `snapshot` / `cache` / `logging` / `fetch` / `govern` | **配置参考，尚未接线** | 这些段与字段目前只有架构定义，还没有被运行时消费 |

**未接线段配了会被明确拒绝**：写入上表标记为「尚未接线」的段，配置校验按
§18.9 的结构层失败处理，报出该段名并说明「当前版本未实现该配置段」的可行动
错误，**不静默忽略**。理由与 §18.9 的未知字段拒绝同源：静默忽略会让用户以为配
了会生效——配置幻觉（§18.9）。用户要这些能力时，先不写该段；等该段接线后自然
生效，期间用架构正文记录的目标行为（默认值表）理解其语义。

**加载顺序**：CLI 参数 > 环境变量 `TOOLGATE_*` > 配置文件（`--config`，默认 `~/.toolgate/config.yaml`）> 内置默认。
**缺省**：配置文件不存在时全部使用内置默认，不视为错误；存在但字段非法才 fail loud。
**校验**：语法 / 结构 / 语义错误启动即失败（fail loud），多错误聚合、未知字段拒绝、错误带字段路径与修法（§18.9）；资源失败按 §3.8 降级但告警。改前先跑 `toolgate config check`。
**热加载**：不支持；改配置需重启（§8.8）。
