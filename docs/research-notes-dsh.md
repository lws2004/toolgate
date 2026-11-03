# 调研笔记：DSH 真实机制

对 DeepSeek Harness（@deepseek-ai/dsh-*）的实测结论。源码 checkout 在
~/workspace/deepseek-harness/，编译产物在 ~/.bun/install/global/node_modules/@deepseek-ai/。

## 一、工具集

工具以 cordis 插件注册，装配清单 dsh-base/cordis.patch.yml。

| 包 | 工具名 |
|---|---|
| dsh-tool-fs | read / write / edit / read_image |
| dsh-tool-fs-search | glob / grep |
| dsh-tool-bash | bash |
| dsh-tool-jobs | job_list / job_output / job_kill |
| dsh-tool-web | web_fetch / web_search |
| dsh-tool-subagent | subagent / subagents / list_subagent_models |
| dsh-tool-subagent-control | send_message / interrupt_agent |
| dsh-tool-todo / goal / schedule | todo_write / create_goal 等 |
| dsh-tool-present / skill / ask-user | present / skill / ask_user_question |
| dsh-tool-working-directory | working_directory |
| dsh-tool-session-query | session_search / session_trace / session_event_* |

**关键设计**：每个工具同时声明**入参 schema 与 output schema**
（packages/core/tool-fs/src/index.ts）。output schema 是 PTC 类型投影的
数据源。

## 二、PTC 模式

run_code 是唯一入口工具，模型写 TS 程序，程序内 await tools.read({...})
调用其他工具（packages/core/tool-executor-ptc/src/ptc.ts）。

**类型注入是代码生成**：ts-types.ts 的 renderToolsSdk() 把每个工具的
parameters 生成 ToolArgsMap、output 生成 ToolOutputMap，合成：

    declare const tools: { [K in ToolName]: (args: ToolArgsMap[K])
      => Promise<ToolOutputMap[K]>; }

工具按名字典序输出以保证字节稳定。

**并发**：单车道，子调用分 exclusive / parallel，上限 maxParallelSubCalls
默认 10。

**解决的问题**：① 上下文——子调用记录进 session log 但不进模型历史；
② 并发——一次程序并发多调用；③ 批量——循环与数据变换在程序内完成。

## 三、输出治理：两层串联

| 层 | 包 | 阈值 | 手段 |
|---|---|---|---|
| 一 | dsh-spill-policy | maxInlineTokens 12500 | 全文落盘 + 头尾各半保留 + notice |
| 二 | dsh-compaction-tool-result-pruner | thresholdChars 8192（头 4096 / 尾 1024）| 硬截断 + marker |

落盘路径：mkdtemp(join(tmpdir(),"dsh-spill-")) → session-<sha256前16hex> →
<12hex>-<name>，权限 0600。

## 四、会话与检查点

- **存储**：~/.dsh/sessions/<cwd编码>/session-<uuid>/session.jsonl.zstd，
  权限 0600，格式 v4，有 v0→v4 逐级迁移包。
- **checkpoint = flush，不是快照**：session-checkpoint-policy 在
  llm/stream、tools/execute（仅顶层）、agent/pre-step 三处调 flush，
  **fail-closed**。嵌套子调用不重复 checkpoint。
- **fork = 前缀继承 + 边界闭合**：buildForkSeed(events, boundary) 复制
  包含式前缀（保留源对象引用），inheritedEventCount 记切点，为未闭合
  step/turn 追加合成 closer。

## 五、图片处理

dsh-image-text 是**第三方 link 插件**（非官方包）。

判定（lib/index.js:746-758）：请求发出前调 llm.resolveModelInfo(provider,
model) 读 info.inputModalities；includes("image") → 视觉路由放行；
否则（含 catch 保守降级）→ 转 OCR 文本。

视觉路由下仅当 visionRouteMode === "auto"（**默认 off**）才前置转译，
且需同时满足意图判定「只要文字」与尺寸判据（4MP / 3000px）。

只处理**用户消息顶层**的图，注释原文：「tool-result 里的图是 agent 主动
要看的，不能动」。

## 六、统一中间层

存在，位于 tools/execute 之后的统一管线（spill → pruner → image-offload），
不依赖各工具自觉。

PTC 另有第三重隔离：每次子调用 session.append("tool/ptc-dispatch") 完整
进会话日志，但只有外层 run_code 的裁剪结果进模型历史。注释原文：
"each sub-dispatch is logged for reconstruction, while only the outer
curated result enters model history."

## 七、对 toolgate 的三条启示

1. **output schema 值得学**：声明了输出结构，调用方才能程序化判断，
   而不是靠解析文本。toolgate 的 ToolResponse.meta 就是这件事的轻量版。

2. **治理要放在数据离开本地之前**：DSH 的两层治理在云端侧，
   toolgate 把同一件事提前到工具内部，下游各层就不会触发。

3. **checkpoint 属于会话层**：DSH 用三个边界 + fail-closed 实现，
   证明这件事需要看到 agent 循环，工具层做不了。
