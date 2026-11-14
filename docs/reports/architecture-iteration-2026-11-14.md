# 架构迭代方案：task 状态 / 无 shell / 大文件多次续读

> 日期：2026-11-14　状态：提案（评审输入）
> 范围：把三类问题按各自性质分流——① 直接修的实现缺陷、② 需评审的架构变更、③ 已判定的权衡。
> 权威口径以 `docs/architecture.md` 及其分册为准；本文只作评审输入，不改正文。

## 0. 结论摘要

| 问题 | 性质 | 决策 | 落地方式 | 依据 |
|---|---|---|---|---|
| task kill 状态不同步 | 实现缺陷（非架构） | 修 | 内部结束状态用 `ExitOutcome` 区分退出码/信号，`running` 不再靠 `exit.is_none()` | §5.5 |
| 无 shell | 已实现（结构化 pipeline） | 已落地结构化线性 pipeline；解释器白名单定案不做 | 新增 `pipeline`/`stdout_to`/`stderr_to` 入参；过渡期 write→解释器 | §5.1、§15.4、§17.2 |
| 大文件多次续读 | 已判定权衡（非缺陷） | 不改续读机制 | 提升 structured/summary 提取与 §22 L1 预判，实现「少读多定位」 | §9.1、§1.4.3、§5.3 |

要点：这三件事不是同一类，不能都当「缺陷修复」。①是缺陷；②是架构既有设计缺实现，动它要过 §3.4 评审；③架构已论证 offset 续读是最优，属已识别摩擦。

## 1. task kill 状态不同步（本轮已实现修复）

**现象**：`task kill` 后进程约 0.5s 内已死（pid 消失），但 `task list` / `read` 仍报「运行中」。

**根因**：kill 走 SIGKILL（`kill_process_group` 用 `kill -KILL -<pid>` 整组 + `Child::kill()`）。被信号终止的进程，`std::process::ExitStatus::code()` 返回 `None`（信号终止没有退出码）。原判定 `"running": exit.is_none()` 把「被杀死（None）」与「仍在运行（None）」混为一谈，永远显示运行中。

**改动**（`crates/toolgate-core/src/tools/task.rs`、`crates/toolgate-core/src/task_path.rs`）：
- 新增内部枚举 `ExitOutcome { Code(i32) | Signal(i32) }`；`Job.exit` 由 `Option<i32>` 改为 `Option<ExitOutcome>`。
- wait 线程（start 内）用 `status.code()` / `status.signal()` 写入 `Some(Code/Signal)`；`running` 改由「是否已结束」判定。
- `read` 的 `meta.running`、`list` 的展示、`state_line`、`structured_facts` 统一按「已结束」判定；`list` 对被信号终止显示「已终止（信号 N）」。
- **对外契约不变**：`exitCode` 语义保持（正常退出给码、信号终止为 null），只修正 `running`。

**验证**：新增回归 `task::tests::killed_job_reports_not_running`（start sleep 30 → kill → read/list 均须 `running=false`），task 模块 15 passed 连跑两次稳定。

## 2. 无 shell（已按结构化线性 pipeline 落地，2026-11-14）

> 状态：2026-11-14 已实现 pipeline/stdout_to/stderr_to（见 docs/implementation-status.md 阶段 2）；§15.4 解释器白名单定案不做。下列为落地设计记录。

**现状**：`bash` 只接受 `binary+args`，`reject_shell`（bash.rs:126/460）拦 shell 解释器；但 `python3`/`node` 仍可跑任意脚本，风险同级——已列 §15.4 未决。

**架构既有设计（§5.1「管道与重定向」）**：全仓无 `pipeline`/`stdout_to` 实现，属「已建模、未落地」。三档路径：
1. **结构化线性管道（推荐落地）**：入参 `{"pipeline":[{"binary":"rg","args":[...]},{"binary":"head","args":["-n","20"]}],"stdout_to":"/tmp/out"}`。每段 execve、OS 管道连接，等价 `rg|head`；只支持线性管道，不支持 `&&`/`||`/`;`。安全：无词分割/通配/变量替换，攻击面≈单命令；收益：一次往返、中间结果不占上下文。
2. **过渡期正路（现在可用）**：`write` 写脚本 → `bash` 用解释器执行；两步、脚本落盘可审计可重跑。
3. **script 入参**：落临时文件再执行，架构倾向但「现在不做」，等「写脚本」占比够高再加。

**影响面与流程**：
- 新增 `pipeline` 等入参 = 对外契约变更 + 安全面变更 → 按 §3.4：先改 `04-tools.md` §5.1 正文 → `./scripts/check-doc.sh` → 评审确认 → 再实现。
- 需同时拍板 §15.4「解释器白名单要不要做」：若只拦 shell 而放开 python3/node，「无 shell」名不副实。

**测试计划**：管道两段等价性、非法段拒绝（非 linear）、`stdout_to` 落盘与 pointer、超限截断、与 `reject_shell` 组合无绕过。

## 3. 大文件多次续读（已判定权衡，不改机制）

**判定**：§9.1 决策「超长文本不折叠落盘，给 offset 续读提示」；§1.4.3 明确列「大文件仍要多次 read」为**已识别未消除的摩擦**（权衡非缺陷）。理由：文件已在磁盘复制是浪费、给指针仍要 read 一跳、offset 比指针更直接。

**现有能力已覆盖**：`read` 的 `offset/limit` + 「用 offset=N 继续」提示（read.rs:249-257/322）；`structured` intent 只给相关段（代码→签名/TODO/导入、配置→键值树、日志→错误行时间线，§5.3）；单行超长才落盘。

**迭代方向（增强存量、不动 offset）**：
- 提升 `structured` 提取的覆盖与质量，让「一次读对」；
- `search` 先定位再定点 `read`；
- §22 L1 预判提前读下一段；
- 可选：先给全文骨架/TOC 再按需读段（仍属 structured 增强）。

**不做什么**：不改成自动折叠落盘、不引入分页缓存——会与 §9.1 的存储与跳数论证冲突。

## 4. 实施顺序与验收

1. ① 已随本轮提交：`cargo test --workspace` / `build --release` / `clippy` 全绿。
2. ② 先出 §5.1 正文修订稿 + 解释器白名单决策，过评审后再实现；实现后补管道测试。
3. ③ 归入 structured/summary 能力增强，低优先，按真实调用采样驱动（§15.6）。

## 5. 需拍板的点

- §15.4：解释器白名单做不做、白名单范围（直接决定「无 shell」的边界）。
- ② 是否本轮就动架构正文（需评审），还是先并行只做①并排期评审。
