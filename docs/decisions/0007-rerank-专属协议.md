# ADR 0007：新增 `source: rerank`——rerank 槽位走专属协议

- 状态：已采纳（2026-11-10；独立评审 P0 清零、P1 4 条闭环，复审确认可进入实现阶段）
- 影响面：`source` 枚举新增一值、协议适配层、能力矩阵；不改六个工具、不改槽位契约形状、不改 `adapter` 枚举、不改依赖分层
- 触发：本机 127.0.0.1:10020 常驻服务提供原生 `/v1/rerank`（jina-reranker-v3.5-mlx-merged），而现有 `ordered` 适配器只发 `/v1/chat/completions`；用户 2026-11-10 指出「rerank 槽位本来就是专属的，应该用 rerank 协议。其它槽位若要做它是需要转换的」

## 背景

§3.5 已定「rerank 必须独立成槽位」：输出规模随候选数变化、必须保序且覆盖全部输入，
listwise reranker 一次前向处理整块候选（参考实现的块大小为 125，§3.5 选型记录）。§18.1 的 `source`
是「模型来源/部署形态」，与协议合成一个字段；能力表按 `source × slot` 判定。

现状只有 `ordered` 一条 rerank 路径：query + 编号候选拼进 chat prompt，让通用 LLM
输出排序 JSON，再从 `choices[0].message.content` 解析。§18.1 因此把
`openai` / `anthropic` / `responses` × `rerank` 标「受限」，并写「专用 listwise
reranker 不在本轮实现范围内」。

上游事实（本机原生 `/v1/rerank` 端点）与逐条实测命令、条件、返回形状见
[附件 E.11](../architecture/95-references.md)。要点：该端点只做排序，
`/v1/chat/completions` 返回 `Model not loaded`；`model` 必须写上游启动名，别名会
`model_not_found`；OpenViking 已在消费该端点。

**曾考虑并撤回的方案**：新增 `adapter: rerank` 而把 `source` 保留为 `openai`。
用户指出这把协议层级搞反了——原生 `/v1/rerank` 是独立协议族，不是 OpenAI Chat
Completions 上的一个形状变体；§18.1「其他协议按同样方式加 `source` 枚举与可选层
适配器」才是正解。专用 ranker 输出的就是排序结果，不需要形状转换；只有通用 LLM
承担 rerank 时才需要 `ordered` 转换。

## 决策

1. **新增 `source: rerank`**，与 `systemone` 并列，作为独立协议族；`adapter` 枚举
   不变，专用 ranker 走 `native`。
2. **能力矩阵**（§18.1 新增一列）：`rerank × rerank` = **可用**；其余四个槽位
   = **不可用**（专用协议只做排序，不生成文本、不出校准概率、不传图）。
   `serves` 是声明不是承诺（§3.11）：声明阶段不因这条判据拒绝配置，运行时按 §3.11 前置准入拒绝并降级。
3. **端点与形状**（写进 §18.1）：
   - `base_url` 指向服务根，客户端自行拼 `/v1/rerank`（与 `systemone` 同口径）；
   - 请求 `{"model": <models.<id>.model>, "query": ..., "documents": [...]}`，
     `model` 必须与上游启动名一致，不做别名推断；
   - 响应 `results[].{index, relevance_score}` → `RerankResult{index, score}`，
     `document` 等额外字段丢弃；
   - `top_n` 不透传（未实测上游支持）；槽位按 §3.2 返回覆盖全部输入的完整 `results`，截断由工具层按 §5.2 完成；
   - 校验沿用 §3.2：覆盖全部输入、index 指向原始位置、按 score 降序；不符按 §3.11 降级。
4. **不进 `auto` 推断**：与 `systemone` 同理由（根地址无协议特征，猜错比报错更糟，
   §1.4.4），必须显式写 `source: rerank`。`adapter: auto` 命中时给 `native`。
5. **`ordered` 保留**：它是「通用 LLM 承担 rerank 时的形状转换层」，不被替换；
   两条路径按 `source` 区分，互不干扰。
6. **不新增依赖**：HTTP 发送仍在 `external-protocols` feature 后（§19.6），请求/响应
   解析放默认层纯逻辑。

## 后果

- 正面：协议层级与槽位语义对齐；本机专用 ranker 可被消费，且不会把通用 LLM 的
  自报分数与专用 ranker 的相关性分数混为一谈。
- 代价：`source` 枚举多一值、能力表多一列；`model` 必须写上游启动名（较长绝对路径），
  与上游不符时由首次调用暴露 `model_not_found`、按 §18.9 第四层降级并告警，不在配置期 fail loud；
  `rerank` 协议只服务 rerank 槽位（能力表对其余槽位判「不可用」，§3.11 运行时准入）。
- 不改动：六个工具数量、只依赖 MCP 协议、`RerankInput` / `RerankOutput` 形状、
  `adapter` 枚举、依赖分层、§2 红线七条。

## 依据

- 用户 2026-11-10：「rerank 槽位本来就是专属的，应该用 rerank 协议。其它槽位若要做它是需要转换的」
- 实测：见 [附件 E.11](../architecture/95-references.md)（2026-11-10，含命令与条件）
- 架构：§3.5（rerank 独立成槽位）、§18.1（source 是部署形态+协议、能力表、其他协议
  扩展规则、auto 保守推断）、§3.10（adapter 是形状转换层）、§1.5 / §19.6（准入与依赖分层）
- 校验：`./scripts/check-doc.sh` 退出码 0
