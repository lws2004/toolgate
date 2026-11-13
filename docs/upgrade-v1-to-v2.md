# 从 v1 升级到 v2（阶段 1.5 兼容性）

> 本文是客户端迁移说明，不是架构正文；架构决策以 [architecture.md](architecture.md)
> 与 docs/architecture/ 分册为准。

## 一句话结论

v1 客户端不需要改任何参数：v2 首版默认 intent=raw（§4.2），工具结果与 v1 一致。
新客户端可以额外使用 intent=auto 与每请求 _meta。

## v1 客户端：不改参数

- 照旧发 initialize 握手；服务端保留旧版协商逻辑，未知版本回退到最新受支持版本。
- tools/call 不传 intent、不传 _meta：服务端按 intent=raw 执行，不触发协商，
  结构化通道不出现 notices 字段。
- 工具级错误仍是 isError=true 加 content 文本；v2 额外在 structuredContent.error
  提供 code / fact / reason / nextSteps 四段（§1.4.5），content 文本改为
  「事实 / 原因 / 可选下一步」三段式。只读 content 与 isError 的客户端不受影响；
  按错误码分支的客户端应改读 structuredContent.error.code。
- 六个工具名与数量不变（§2 红线 1）；`search` 的输入参数有一处破坏性变更，见下节。

## 新客户端：2026-07-28 无握手

协议取消 initialize 握手后，版本、能力、会话与 turn 元数据随每个请求传递：

```json
{
  "method": "tools/call",
  "params": {
    "name": "read",
    "arguments": { "path": "shot.png", "intent": "auto" },
    "_meta": {
      "protocolVersion": "2026-07-28",
      "toolgate/upstream": { "vision": false, "maxImageEdge": 1568 },
      "toolgate/session": { "session_id": "sess_01" },
      "toolgate/turn": {
        "turn_id": "turn_01", "seq": 3,
        "budget": { "max_judgments": 8 }
      }
    }
  }
}
```

- 三个 toolgate 扩展字段都是可选；缺省时退化为无会话隔离、每工具单独判定，不报错。
- 不传 _meta 且启动配置也没设时，vision 默认 false（§18.1b 保守默认）。
- 请求带不支持的 protocolVersion 会收到 JSON-RPC -32600，错误信息列出受支持版本；
  请求不带版本则按旧版无元数据方式放行。

## intent 参数

| 取值 | 含义 |
|---|---|
| raw | 原文（v2 首版默认） |
| summary | 摘要 |
| structured | 结构化数据 |
| visual | 原图 |
| auto | 由协商层判定 |

- intent 缺省取配置 intent.default，v2 首版为 raw；要启用 auto 必须显式配置。
- 非法 intent（未知值或非字符串）会收到 JSON-RPC -32602，错误信息列出允许值。
- intent=auto 时，协商结果以 §4.4b 的 notices 数组出现在 structuredContent；
  本阶段只做解析与协商，工具结果形态仍保持 raw，单工具接入在阶段 2。

## search 输入参数变更（2026-11-13）

`search` 旧版把 `query` 同时当作「匹配正则 / 文件名子串」与「语义查询」喂给重排，两个用途
塞进一个字段：既不能写通配，又会在关键词查询时把关键词本身当成语义信号（排序可能与
预期反向）。本次拆成两个字段（§5.2）：

| 旧 | 新 | 说明 |
|---|---|---|
| `query: "<正则>"` | `pattern: "<正则>"` | 匹配表达式，content 与 files 都必填：content 只按正则解释，files 按文件名子串 / 通配符解释，决定「哪些行算命中」 |
| （无） | `query: "<自然语言>"` | 可选；语义查询，仅在命中 > 20 且 rerank / embedding 可用时用于重排顺序 |

**迁移**：把旧的 `query: "<正则或文件名>"` 全部改名为 `pattern`；如需语义重排，另加
`query` 写自然语言。`mode` 仍必填。注意：content 模式的 `pattern` 只按正则解释，
旧调用若是把 `*.rs` 这类通配 / 子串写法当 `query` 用，改到 `pattern` 后不会命中，
需改写成等效正则（如 `.*\.rs$`）或改用 `mode: files`；纯匹配、不关心排序的调用
只需改字段名，行为不变。

## 校验口径

- 请求级 _meta 是宽容读取：未知字段忽略、类型不符按缺省处理。
  §18.9 的「未知字段一律拒绝」针对启动配置，不适用于请求元数据。
- 启动配置的语法 / 结构 / 语义错误仍 fail loud，资源失败按 §3.8 降级并告警。

## 尚未提供（按 §14 路线图）

- 阶段 2：bash / read 图片 / search / edit / task 接入理解链；产生大块内容的工具有 raw 与 summary 两条路径。write 按 §4.0 不接理解层。
- 阶段 3：intent 全面接入、置信度与 slmStatus 进 meta。
- 阶段 4：d1 批处理（方案 B）、L0 预热、质量阶梯标定。
- 本地模型后端属可选层（§19.6），当前默认构建不含，槽位全空仍能运行（§3.8）。
