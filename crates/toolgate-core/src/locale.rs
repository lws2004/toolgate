//! 多语言（§18.1b / §4.7）：源语言中文作 msgid，输出按用户语言翻译。
//!
//! 零新增依赖（§19.6）：译文是静态表，语言由请求级元数据或进程环境决定，
//! 不引入 i18n 框架、不读配置文件、不产生跨请求状态（§22.5）。
//! 中文 msgid 是消息标识，不是写死的输出；未登记的 msgid 回退源语言。

use serde_json::Value;
use std::borrow::Cow;

/// 当前支持的两种语言；扩展新语言只需加变体与一张译文表。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Language {
    /// 源语言，也是无法判定用户语言时的兜底。
    #[default]
    Zh,
    En,
}

impl Language {
    /// BCP-47 前缀匹配：zh / zh-CN → Zh，en / en-US → En。
    pub fn parse(tag: &str) -> Option<Self> {
        let t = tag.to_ascii_lowercase();
        if t.starts_with("zh") {
            Some(Self::Zh)
        } else if t.starts_with("en") {
            Some(Self::En)
        } else {
            None
        }
    }

    /// 默认跟随用户语言：显式环境变量 → LANG / LC_ALL → 源语言兜底。
    pub fn from_env() -> Self {
        for key in ["TOOLGATE_LANG", "LANG", "LC_ALL"] {
            if let Ok(v) = std::env::var(key)
                && let Some(lang) = Self::parse(&v)
            {
                return lang;
            }
        }
        Self::Zh
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Zh => "zh-CN",
            Self::En => "en",
        }
    }
}

/// 英文译文表：中文 msgid → 英文文本（占位符保持同名）。
fn en(zh: &str) -> Option<&'static str> {
    Some(match zh {
        // 错误三段式的可执行下一步（§1.4.5）
        "确认路径存在且可访问" => "Check that the path exists and is accessible",
        "用 search 定位文件后重试" => "Use search to locate the file, then retry",
        "用 bash 转码后处理：iconv -f GBK -t UTF-8 原文件 > /tmp/x" => {
            "Transcode with bash first: iconv -f GBK -t UTF-8 file > /tmp/x"
        }
        "或改用 read 查看可读部分" => "Or use read to inspect the readable part",
        "重新 read 获取最新行锚点" => "Re-read to get fresh line anchors",
        "重算锚点后重试" => "Recompute the anchor and retry",
        "在 edits 里补更多上下文使 oldText 唯一" => {
            "Add more context in edits so oldText is unique"
        }
        "改用锚点模式定位" => "Use anchor mode instead",
        "缩小命令范围或分批执行" => "Narrow the command scope or split it into batches",
        "用 task 后台执行长任务" => "Use task to run long jobs in the background",
        "用 offset/limit 分段读取" => "Read in chunks with offset/limit",
        "缩小 search 的 maxResults" => "Lower search maxResults",
        "检查 models 的 serves 声明" => "Check the serves declarations in models",
        "或让它按 §3.8 走机械路径" => "Or let it degrade to the mechanical path per §3.8",
        "把路径改到工作目录或已配置的允许根内" => {
            "Move the path into the working directory or a configured allowed root"
        }
        "或用 paths.allowed_roots 扩大允许根后重启" => {
            "Or widen the allowed roots via paths.allowed_roots and restart"
        }
        "请求的路径 {requested} 不在允许根内" => {
            "Requested path {requested} is outside the allowed roots"
        }
        "允许根：{roots}（§17.2 路径穿越对策）" => {
            "Allowed roots: {roots} (§17.2 path traversal countermeasure)"
        }
        "search 遍历超过单次时限：已用 {} 毫秒，上限 {} 毫秒" => {
            "search walk exceeded the per-call limit: {} ms used, {} ms allowed"
        }
        "遍历上限防止一个 glob 匹配百万文件把时间与内存打满（§17.2）" => {
            "Walk limits keep a glob matching millions of files from saturating time and memory (§17.2)"
        }
        "把 path 缩小到子目录后重试" => "Narrow path to a subdirectory and retry",
        "或用更精确的 query 减少遍历量" => {
            "Or use a more precise query to reduce the walk"
        }
        "\n[遍历已截断：递归深度达 {depth} 层且文件数达 {files} 个，结果只覆盖已遍历部分]" => {
            "\n[Walk truncated: depth limit {depth} and file limit {files} both reached; results cover only the walked portion]"
        }
        "\n[遍历已截断：递归深度达 {depth} 层，结果只覆盖已遍历部分]" => {
            "\n[Walk truncated: depth limit {depth} reached; results cover only the walked portion]"
        }
        "\n[遍历已截断：文件数达 {files} 个，结果只覆盖已遍历部分]" => {
            "\n[Walk truncated: file limit {files} reached; results cover only the walked portion]"
        }
        "调用 tools/list 查看可用工具名" => "Call tools/list for available tool names",
        "改用 read / write / edit / search / bash / task 之一" => {
            "Use one of read / write / edit / search / bash / task"
        }
        "对照 tools/list 里该工具的 inputSchema 补齐必填参数" => {
            "Fill required arguments per the tool's inputSchema in tools/list"
        }
        "参数名区分大小写，检查拼写后重试" => {
            "Argument names are case-sensitive; check spelling and retry"
        }
        "用 bash 检查权限与磁盘状态后重试" => {
            "Check permissions and disk state with bash, then retry"
        }
        "按 fact 里的失败点修正输入后重试" => {
            "Fix the input at the point named in fact, then retry"
        }
        "若持续失败，改用更简单的等价调用" => {
            "If it keeps failing, use a simpler equivalent call"
        }
        // 固定 reason / fact
        "工具执行未能完成" => "Tool execution did not complete",
        "工具层无法完成该 IO 操作" => {
            "The tool layer could not complete this IO operation"
        }
        "入参不符合该工具的 inputSchema" => {
            "Arguments do not match this tool's inputSchema"
        }
        "任务表锁失效" => "Task registry lock is poisoned",
        "参数错误：{message}" => "Bad argument: {message}",
        "未知工具：{name}" => "Unknown tool: {name}",
        "工具执行失败：{fact}" => "Tool failed: {fact}",
        "文件系统错误：{fact}" => "Filesystem error: {fact}",
        "事实：{fact}\n原因：{reason}\n可选下一步：{next}" => {
            "Fact: {fact}\nReason: {reason}\nNext steps: {next}"
        }
        "；" => "; ",
        // 请求级 / 传输层提示
        "JSON 解析失败：{e}" => "JSON parse failed: {e}",
        "不支持的方法：{other}" => "Unsupported method: {other}",
        "参数不是合法 JSON：{e}" => "Arguments are not valid JSON: {e}",
        "MCP server 异常退出：{e}" => "MCP server exited abnormally: {e}",
        // inputSchema 描述
        "目标文件路径" => "Target file path",
        "完整文件内容" => "Full file content",
        "可执行程序名或绝对路径，例如 git" => {
            "Executable name or absolute path, e.g. git"
        }
        "工作目录，默认用上下文的工作目录" => {
            "Working directory; defaults to the context cwd"
        }
        "超时秒数，默认 300" => "Timeout in seconds; defaults to 300",
        "文件路径，相对路径按工作目录解析" => {
            "File path; relative paths resolve against the cwd"
        }
        "起始行号，1 起始。默认 1" => "First line number, 1-based. Defaults to 1",
        "最多读取的行数。默认读到文件末尾" => {
            "Maximum lines to read. Defaults to end of file"
        }
        "content 搜内容，files 枚举文件路径" => {
            "content searches file contents; files enumerates paths"
        }
        "content 模式是行匹配正则；files 模式是文件名子串或通配符" => {
            "content mode matches a line regex; files mode takes a filename substring or glob"
        }
        "检索起点目录或文件，默认工作目录" => {
            "Search root directory or file; defaults to cwd"
        }
        "最多返回多少条结果，默认 200" => "Maximum results; defaults to 200",
        "要执行的动作" => "Action to perform",
        "start 时必填：可执行程序名" => "Required for start: executable name",
        "start 时的参数数组" => "Argument array for start",
        "start 时的工作目录" => "Working directory for start",
        "read / kill 时必填" => "Required for read / kill",
        "文本匹配模式的编辑列表。各项都基于原始文件匹配，不要重叠" => {
            "Edit list for text-match mode; each item matches the original file and must not overlap"
        }
        "要替换的原文，需在文件中唯一" => {
            "Original text to replace; must be unique in the file"
        }
        "替换后的内容，删除时传空串" => {
            "Replacement text; pass an empty string to delete"
        }
        "行锚点模式：read 返回的 行号#哈希，例如 12#f8a2" => {
            "Anchor mode: the line#hash returned by read, e.g. 12#f8a2"
        }
        "行锚点模式下该行的新内容（不含换行）" => {
            "New content for that line in anchor mode (no newline)"
        }
        "是否给每行附加 行号#短哈希 锚点，供 edit 的锚点模式使用。大文件反复编辑时开启可省 token，其余场景建议关闭。默认 false" => {
            "Attach line#hash anchors to each line for edit's anchor mode; saves tokens on large files, otherwise off by default"
        }
        // 工具 content 与落盘提示（§4.7）
        "(无输出)" => "(no output)",
        "(暂无输出)" => "(no output yet)",
        "(无匹配)\n" => "(no matches)\n",
        "已创建" => "Created",
        "已覆盖" => "Overwritten",
        "{} {}（{} 行 / {} 字节）" => "{} {} ({} lines / {} bytes)",
        "\n旧内容已快照：{}（需要回滚时读它再写回）" => {
            "\nOld content snapshotted: {} (read it and write back to roll back)"
        }
        "\n[输出已截断：共 {} 行 / {}，仅展示尾部 {} 行。{}]" => {
            "\n[Output truncated: {} lines / {}, showing last {} lines. {}]"
        }
        "\n[输出已截断：共 {}，仅展示尾部 {} 行。{}]" => {
            "\n[Output truncated: {}, showing last {} lines. {}]"
        }
        "\n[结果已截断：命中 {hits} 处，仅展示前 {shown} 行。{hint}]" => {
            "\n[Results truncated: {hits} hits, showing first {shown} lines. {hint}]"
        }
        "\n[{reason}。用 offset={next} 继续读取，文件共 {total_lines} 行。]" => {
            "\n[{reason}. Continue with offset={next}; file has {total_lines} lines.]"
        }
        "已截断到 {}（字节上限）" => "Truncated to {} (byte limit)",
        "已截断到 {} 行（行数上限）" => "Truncated to {} lines (line limit)",
        "{} 是目录。用 search 的 files 模式列目录内容。" => {
            "{} is a directory. Use search in files mode to list it."
        }
        "{} 是二进制文件（{} 字节），不能按文本读取。若需处理请用 bash 调用专用工具。" => {
            "{} is a binary file ({} bytes) and cannot be read as text. Use bash with a dedicated tool if needed."
        }
        "起始行 {offset} 超过文件总行数 {total_lines}。" => {
            "Start line {offset} exceeds the file's {total_lines} lines."
        }
        "完整输出已落盘：{}（{} 行 / {} 字节），需要时用 read 按段读取。" => {
            "Full output spilled: {} ({} lines / {} bytes); use read with segments if needed."
        }
        "内容未变化，未写入。" => "Content unchanged; nothing written.",
        "需要提供 edits 数组（文本匹配模式），或 anchor + newText（行锚点模式）" => {
            "Provide either an edits array (text-match mode) or anchor + newText (anchor mode)"
        }
        "edits 不能为空" => "edits must not be empty",
        "edits[{i}] 缺少 oldText" => "edits[{i}] is missing oldText",
        "edits[{i}] 缺少 newText" => "edits[{i}] is missing newText",
        "edits 存在重叠区间，请合并成一笔编辑" => {
            "edits contain overlapping ranges; merge them into one edit"
        }
        "oldText 不能为空" => "oldText must not be empty",
        "在文件中匹配到 {n} 处，无法确定改哪一处。请扩大 oldText 的上下文使其唯一。" => {
            "Matched {n} places in the file; cannot decide which to change. Add more context to make oldText unique."
        }
        "归一化匹配到 {n} 处，请扩大 oldText 上下文使其唯一。" => {
            "Normalized matching found {n} places; add more context to make oldText unique."
        }
        "未找到 oldText（机械归因：{}）。原文片段：{}\n请重新 read 确认当前内容。" => {
            "oldText not found (mechanical attribution: {}). Original excerpt: {}\nRe-read to confirm current content."
        }
        // §5.4 模式 C（扁平 patch）：冲突、hunk 校验与模式互斥
        "patch 冲突" => "patch conflict",
        "patch 里没有 @@ hunk 头；模式 C 需要 unified diff 文本" => {
            "The patch has no @@ hunk header; mode C requires unified diff text"
        }
        "patch 的 hunk 区间重叠，请合并成一个 hunk" => {
            "Patch hunks overlap; merge them into one hunk"
        }
        "edits 必须是数组" => "edits must be an array",
        "缺少 path：edits / anchor 模式必须给目标文件路径，patch 模式可从 ---/+++ 文件头推断" => {
            "Missing path: edits / anchor modes need the target file path; patch mode can infer it from the ---/+++ header"
        }
        "patch 模式没有可用的 path：请显式传 path，或在 patch 里给 ---/+++ 文件头（/dev/null 不算）" => {
            "patch mode has no usable path: pass path explicitly, or include a ---/+++ header in the patch (/dev/null does not count)"
        }
        "需要提供 edits 数组（文本匹配模式）、anchor + newText（行锚点模式），或 patch（unified diff 模式）；三选一" => {
            "Provide exactly one of: edits array (text matching), anchor + newText (line anchor), or patch (unified diff)"
        }
        "patch 的 hunk 与当前文件对不上：文件已被改过，或 hunk 的行号/行数算错了" => {
            "The patch hunks do not match the current file: the file changed, or the hunk line numbers/counts are wrong"
        }
        "重新 read 当前文件，按最新行号重算 hunk" => {
            "Re-read the current file and recompute the hunks against fresh line numbers"
        }
        "或改用 edits 文本匹配模式（oldText 需在文件中唯一）" => {
            "Or switch to edits text matching (oldText must be unique in the file)"
        }
        "hunk 里出现无法识别的行前缀：{line}" => {
            "unrecognized line prefix inside a hunk: {line}"
        }
        "hunk 头格式不对：{line}" => "malformed hunk header: {line}",
        "hunk 头缺旧侧范围：{line}" => "hunk header lacks the old-side range: {line}",
        "hunk 头缺新侧范围：{line}" => "hunk header lacks the new-side range: {line}",
        "hunk 头的范围要以 {sign} 开头：{line}" => {
            "hunk header range must start with {sign}: {line}"
        }
        "hunk 头的行号不是数字：{line}" => {
            "hunk header line number is not a number: {line}"
        }
        "hunk {}（@@ -{},{} +{},{}）的行数与内容不符：头声明旧侧 {} 行 / 新侧 {} 行，体里是 {} / {}" => {
            "hunk {} (@@ -{},{} +{},{}): line counts do not match the body — header says {} old / {} new, body has {} / {}"
        }
        "hunk {} 的行号 {} 超出文件范围（文件共 {} 行）" => {
            "hunk {} start line {} is outside the file (file has {} lines)"
        }
        "patch 冲突：hunk {} 第 {} 行（文件第 {} 行）不匹配\n期望：{}\n实际：{}" => {
            "patch conflict: hunk {} line {} (file line {}) does not match\nExpected: {}\nActual: {}"
        }
        "{}（机械归因：{}）" => "{} (mechanical attribution: {})",
        "edits / anchor / patch 三选一，本次同时收到 {}；删掉多余的模式后重试" => {
            "edits / anchor / patch are mutually exclusive; got {} this time — drop the extra modes and retry"
        }
        "{} 在读取后被其他进程修改，已放弃写入（不做三方合并）。请重新 read 后再改。" => {
            "{} changed after it was read; write aborted (no three-way merge). Re-read, then edit again."
        }
        "锚点行号 {} 超出文件行数 {}" => "Anchor line {} exceeds the file's {} lines",
        "当前没有后台任务。" => "No background tasks.",
        "已请求终止任务 {task_id}。" => "Requested termination of task {task_id}.",
        "已退出（{code}）" => "Exited ({code})",
        "运行中" => "Running",
        "{id}  pid={pid}  {state}  {label} 已运行 {secs}s\n" => {
            "{id}  pid={pid}  {state}  {label} running for {secs}s\n"
        }
        "[状态] processState={state} exitCode={code} elapsedSecs={elapsed} outputBytes={bytes} truncated={truncated}" => {
            "[State] processState={state} exitCode={code} elapsedSecs={elapsed} outputBytes={bytes} truncated={truncated}"
        }
        "已启动后台任务 {task_id}：{label}（pid {pid}）。用 action=read 查看输出，action=kill 终止。" => {
            "Started background task {task_id}: {label} (pid {pid}). Use action=read to view output, action=kill to terminate."
        }
        "没有这个任务：{task_id}" => "No such task: {task_id}",
        "终止失败：{e}" => "Failed to terminate: {e}",
        "action 只能是 start / read / kill / list，收到 {other}" => {
            "action must be start / read / kill / list, got {other}"
        }
        "无法启动 {binary}：{e}" => "Cannot start {binary}: {e}",
        "等待进程失败：{e}" => "Failed to wait for process: {e}",
        "命令超时（{} 秒）已被终止：{binary} {}" => {
            "Command timed out ({} s) and was terminated: {binary} {}"
        }
        "正则无效：{e}" => "Invalid regex: {e}",
        "通配符无效：{e}" => "Invalid glob: {e}",
        "mode 只能是 content 或 files，收到 {other}" => {
            "mode must be content or files, got {other}"
        }
        "无法访问 {}：{e}" => "Cannot access {}: {e}",
        "无法读取 {}：{e}" => "Cannot read {}: {e}",
        "{} 写入失败：{e}" => "Failed to write {}: {e}",
        "{} 写回失败：{e}" => "Failed to write back {}: {e}",
        "图片解码失败（{mime}）：{e}" => "Image decode failed ({mime}): {e}",
        // 契约校验 / 理解链 / 失败归因（§3.11 / §4.8 / §5.4）
        "answers 未覆盖全部 questions" => "answers do not cover all questions",
        "index {} 越界或重复" => "index {} is out of range or duplicated",
        "labels 长度 {} 不在 2-50 内" => "labels length {} is outside 2-50",
        "results 未按 score 降序排列" => "results are not sorted by score descending",
        "results 长度 {} 与 documents 长度 {n} 不一致" => {
            "results length {} does not match documents length {n}"
        }
        "score 不是有限数" => "score is not finite",
        "scores 的 key 集合与 labels 不一致" => "score keys do not match labels",
        "task=ground 时 boxes 必填且不能为空" => "task=ground requires non-empty boxes",
        "无 schema 时不应给 data" => "data must not be given without a schema",
        "无 schema 时输出缺 text" => "output is missing text without a schema",
        "有 schema 时不应同时给 text" => "text must not be given alongside a schema",
        "有 schema 时输出缺 data" => "output is missing data with a schema",
        "标签 {label} 的分数 {score} 不在 [0,1]" => {
            "label {label} score {score} is outside [0,1]"
        }
        "问题 {question} 的 probability {} 不在 [0,1]" => {
            "question {question} probability {} is outside [0,1]"
        }
        "换用 native 适配器或原生支持该槽位契约的模型" => {
            "Use a native adapter or a model with native support for this slot contract"
        }
        "把 adapter 改为 json / text / ordered 后重试" => {
            "Set adapter to json / text / ordered and retry"
        }
        "从该模型的 serves 里移除此槽位，让它按 §3.8 降级" => {
            "Remove this slot from the model's serves and let it degrade per §3.8"
        }
        "依赖图存在环，无法完成拓扑分层" => {
            "The dependency graph has a cycle; topological scheduling failed"
        }
        "节点 {} 的依赖 {dep} 不存在或尚未定义" => {
            "node {} depends on {dep}, which does not exist or is not defined yet"
        }
        "链有 {} 步，超过上限 {MAX_STEPS}" => {
            "chain has {} steps, exceeding the limit {MAX_STEPS}"
        }
        "把链步数压到 5 步以内，或去掉无依赖的重复节点" => {
            "Keep the chain within 5 steps or remove duplicate dependency-free nodes"
        }
        "检查 depends_on 是否指向已存在的节点且无环" => {
            "Check that depends_on points to existing nodes and has no cycle"
        }
        "降低链级并发上限后重试" => "Lower the chain-level concurrency limit and retry",
        "上下文不足" => "insufficient context",
        "格式差异" => "format difference",
        "根本不存在" => "does not exist",
        "未检测到构建描述文件" => "no build manifest detected",
        // 入参 / 协议 / 传输层
        "intent 必须是字符串（raw / summary / structured / visual / auto）" => {
            "intent must be a string (raw / summary / structured / visual / auto)"
        }
        "未知 intent {other}；允许：raw / summary / structured / visual / auto" => {
            "unknown intent {other}; allowed: raw / summary / structured / visual / auto"
        }
        "缺少字符串参数 {key}" => "missing string argument {key}",
        "不支持的协议版本 {v}；支持：{}。请改用受支持版本，或去掉请求里的版本字段按旧版无元数据方式调用" => {
            "unsupported protocol version {v}; supported: {}. Use a supported version, or drop the version field to call as a legacy client without metadata"
        }
        "请求的工具 {name} 不在工具表内" => {
            "requested tool {name} is not in the tool table"
        }
        "可用工具：{}" => "available tools: {}",
        "事实：{}\n原因：{}\n可选下一步：{}" => "Fact: {}\nReason: {}\nNext steps: {}",
        "文件系统调用失败：{e}" => "filesystem call failed: {e}",
        "哈希应为 {ANCHOR_HASH_LEN} 位十六进制字符，收到：{s}" => {
            "hash should be {ANCHOR_HASH_LEN} hex characters, got: {s}"
        }
        "行号必须是正整数，收到：{s}" => {
            "line number must be a positive integer, got: {s}"
        }
        "锚点格式应为 行号#哈希，例如 12#f8a2" => {
            "anchor must be line#hash, e.g. 12#f8a2"
        }
        "call 需要工具名。\n\n{USAGE}" => "call requires a tool name.\n\n{USAGE}",
        "未知子命令：{other}\n\n{USAGE}" => "unknown subcommand: {other}\n\n{USAGE}",
        // plan_* 的 visual 拒绝
        "bash 不返回图片，intent=visual 不适用；要精确看图请用 read 工具，或改用 intent=raw" => {
            "bash does not return images; intent=visual does not apply. Use read for precise image viewing, or switch to intent=raw"
        }
        "search 不返回图片，intent=visual 不适用；要看图请用 read 工具，或改用 intent=raw" => {
            "search does not return images; intent=visual does not apply. Use read to view images, or switch to intent=raw"
        }
        "task 不返回图片，intent=visual 不适用；要看图请用 read 工具，或改用 intent=raw" => {
            "task does not return images; intent=visual does not apply. Use read to view images, or switch to intent=raw"
        }
        // schema / outputSchema 描述
        "协商层输出形态：raw 截断原文加落盘指针（默认）/ summary 失败摘要 / structured 结构化事实 JSON / visual 不适用（看图请用 read）/ auto 输出小于 500 token 给原文，否则走 summary" => {
            "Output shape: raw truncated output with spill pointer (default) / summary / structured facts JSON / visual not applicable (use read for images) / auto gives raw under 500 tokens, otherwise summary"
        }
        "structuredContent 是程序可读通道（§4.4）；notices 之外的键为各工具特有的程序事实（路径、行数、退出码等）" => {
            "structuredContent is the program-readable channel (§4.4); keys other than notices are tool-specific facts (path, line count, exit code, ...)"
        }
        "§4.4b 告知数组；无告知时省略该字段。与 content 文本同源" => {
            "§4.4b notices array; omitted when empty. Same source as the content text"
        }
        "标签 {} 的 bbox 含非法像素坐标" => {
            "label {} bbox contains invalid pixel coordinates"
        }
        "内容中有字符无法用 {} 表示。请改用 UTF-8 保存，或删除这些字符。" => {
            "content contains characters not representable in {}. Save as UTF-8 or remove them."
        }
        "节点 id {} 重复" => "duplicate node id {}",
        "节点 {} 依赖自身" => "node {} depends on itself",
        "{notices}\n[图片文件] {file_name} · {mime}{dims} · {size}\n路径：{path}\n\n无法提供图片内容：vision 槽位未配置，上游模型也不接受图片输入。\n可选路径：\n- 若你能通过其他方式查看该文件，用上面的路径\n- 配置 vision 槽位后重试\n- 若只需要文件名与尺寸，以上信息已足够" => {
            "{notices}\n[Image] {file_name} · {mime}{dims} · {size}\nPath: {path}\n\nCannot provide image content: the vision slot is not configured and the upstream model does not accept images.\nOptions:\n- If you can view the file another way, use the path above\n- Configure the vision slot and retry\n- File name and dimensions are enough if that is all you need"
        }
        // CLI 侧（§18.9 配置层）：配置检查与槽位绑定的输出文案
        "未找到配置文件，使用内置默认（六个槽位全空）" => {
            "No config file found; using built-in defaults (all six slots empty)"
        }
        "配置检查通过" => "Config check passed",
        "槽位绑定（§3.10）：" => "Slot bindings (§3.10):",
        "模型={v}" => "model={v}",
        "来源={v}" => "source={v}",
        "模式={v}" => "mode={v}",
        "适配器={v}" => "adapter={v}",
        "绑定={v}" => "binding={v}",
        "未绑定" => "unbound",
        "显式" => "explicit",
        "自动" => "auto",
        "配置有误：" => "Config errors:",
        "存在的配置文件路径" => "path to an existing config file",
        "检查路径或先创建文件" => "Check the path, or create the file first",
        "未知参数：{other}" => "Unknown argument: {other}",
        "--config 需要一个路径参数。" => "--config requires a path argument.",
        "config 需要子命令（当前只有 check）。" => {
            "config requires a subcommand (only check exists)."
        }
        "未知 config 子命令：{other}" => "Unknown config subcommand: {other}",
        "槽位绑定：{summary}" => "Slot bindings: {summary}",
        // CLI 侧（§18.9 配置层 / §14 阶段 1 判定调试面）
        "decide 需要 --state。" => "decide requires --state.",
        "decide 需要至少一个 --question。" => "decide requires at least one --question.",
        "decide 槽位未绑定模型。" => "The decide slot has no model bound.",
        "在配置的 models 里让某个模型的 serves 含 decide" => {
            "Let a model in the config serve decide via serves"
        }
        "当前构建未启用外部协议客户端（external-protocols）。" => {
            "This build has no external protocol client (external-protocols)."
        }
        "用 cargo build --features external-protocols 重新构建后再试" => {
            "Rebuild with cargo build --features external-protocols and retry"
        }
        "判定调用失败：{e}" => "Decide call failed: {e}",
        "判定结果形状不符合槽位契约：{e}" => {
            "Decide result violates the slot contract: {e}"
        }
        "问题：{q}" => "question: {q}",
        "概率：{p}" => "probability: {p}",
        "门控结论：采用" => "gate: adopt",
        "门控结论：低置信" => "gate: adopt with low confidence",
        "门控结论：两种视图" => "gate: both views",
        "整体门控结论：{gate}" => "overall gate: {gate}",
        "判定耗时 {ms} ms" => "decide took {ms} ms",
        "--question 需要一个文本参数。" => "--question requires a text argument.",
        "--state 需要一个文本参数。" => "--state requires a text argument.",
        _ => return None,
    })
}

/// 含占位符的中文模板（§18.1b）：精确 msgid 匹配不上时按模板反查。
/// 每条都必须在 en() 里有译文；匿名 {} 与命名 {name} 可混用。
const TEMPLATES: &[&str] = &[
    "参数错误：{message}",
    "未知工具：{name}",
    "工具执行失败：{fact}",
    "文件系统错误：{fact}",
    "事实：{fact}\n原因：{reason}\n可选下一步：{next}",
    "事实：{}\n原因：{}\n可选下一步：{}",
    "JSON 解析失败：{e}",
    "不支持的方法：{other}",
    "参数不是合法 JSON：{e}",
    "MCP server 异常退出：{e}",
    "无法启动 {binary}：{e}",
    "等待进程失败：{e}",
    "终止失败：{e}",
    "正则无效：{e}",
    "通配符无效：{e}",
    "图片解码失败（{mime}）：{e}",
    "没有这个任务：{task_id}",
    "已请求终止任务 {task_id}。",
    "已启动后台任务 {task_id}：{label}（pid {pid}）。用 action=read 查看输出，action=kill 终止。",
    "已退出（{code}）",
    "action 只能是 start / read / kill / list，收到 {other}",
    "mode 只能是 content 或 files，收到 {other}",
    "未知 intent {other}；允许：raw / summary / structured / visual / auto",
    "缺少字符串参数 {key}",
    "请求的工具 {name} 不在工具表内",
    "可用工具：{}",
    "文件系统调用失败：{e}",
    "标签 {label} 的分数 {score} 不在 [0,1]",
    "问题 {question} 的 probability {} 不在 [0,1]",
    "标签 {} 的 bbox 含非法像素坐标",
    "哈希应为 {ANCHOR_HASH_LEN} 位十六进制字符，收到：{s}",
    "行号必须是正整数，收到：{s}",
    "call 需要工具名。\n\n{USAGE}",
    "未知子命令：{other}\n\n{USAGE}",
    "锚点行号 {} 超出文件行数 {}",
    "在文件中匹配到 {n} 处，无法确定改哪一处。请扩大 oldText 的上下文使其唯一。",
    "归一化匹配到 {n} 处，请扩大 oldText 上下文使其唯一。",
    "未找到 oldText（机械归因：{}）。原文片段：{}\n请重新 read 确认当前内容。",
    "hunk 里出现无法识别的行前缀：{line}",
    "hunk 头格式不对：{line}",
    "hunk 头缺旧侧范围：{line}",
    "hunk 头缺新侧范围：{line}",
    "hunk 头的范围要以 {sign} 开头：{line}",
    "hunk 头的行号不是数字：{line}",
    "hunk {}（@@ -{},{} +{},{}）的行数与内容不符：头声明旧侧 {} 行 / 新侧 {} 行，体里是 {} / {}",
    "hunk {} 的行号 {} 超出文件范围（文件共 {} 行）",
    "patch 冲突：hunk {} 第 {} 行（文件第 {} 行）不匹配\n期望：{}\n实际：{}",
    "{}（机械归因：{}）",
    "edits / anchor / patch 三选一，本次同时收到 {}；删掉多余的模式后重试",
    "{} 在读取后被其他进程修改，已放弃写入（不做三方合并）。请重新 read 后再改。",
    "edits[{i}] 缺少 oldText",
    "edits[{i}] 缺少 newText",
    "不支持的协议版本 {v}；支持：{}。请改用受支持版本，或去掉请求里的版本字段按旧版无元数据方式调用",
    "内容中有字符无法用 {} 表示。请改用 UTF-8 保存，或删除这些字符。",
    "节点 {} 的依赖 {dep} 不存在或尚未定义",
    "results 长度 {} 与 documents 长度 {n} 不一致",
    "index {} 越界或重复",
    "labels 长度 {} 不在 2-50 内",
    "节点 id {} 重复",
    "请求的路径 {requested} 不在允许根内",
    "允许根：{roots}（§17.2 路径穿越对策）",
    "search 遍历超过单次时限：已用 {} 毫秒，上限 {} 毫秒",
    "\n[遍历已截断：递归深度达 {depth} 层且文件数达 {files} 个，结果只覆盖已遍历部分]",
    "\n[遍历已截断：递归深度达 {depth} 层，结果只覆盖已遍历部分]",
    "\n[遍历已截断：文件数达 {files} 个，结果只覆盖已遍历部分]",
    "节点 {} 依赖自身",
    "链有 {} 步，超过上限 {MAX_STEPS}",
    "命令超时（{} 秒）已被终止：{binary} {}",
    "无法访问 {}：{e}",
    "无法读取 {}：{e}",
    "{} 写入失败：{e}",
    "{} 写回失败：{e}",
    "起始行 {offset} 超过文件总行数 {total_lines}。",
    "\n[{reason}。用 offset={next} 继续读取，文件共 {total_lines} 行。]",
    "[状态] processState={state} exitCode={code} elapsedSecs={elapsed} outputBytes={bytes} truncated={truncated}",
    "{id}  pid={pid}  {state}  {label} 已运行 {secs}s\n",
];

/// 把模板拆成字面片段与占位符名（匿名 {} 的名字为空串）。
fn template_parts(tmpl: &str) -> (Vec<&str>, Vec<&str>) {
    let mut lits = Vec::new();
    let mut names = Vec::new();
    let mut rest = tmpl;
    while let Some(i) = rest.find('{') {
        lits.push(&rest[..i]);
        let Some(j) = rest[i..].find('}') else {
            lits.push(&rest[i..]);
            return (lits, names);
        };
        names.push(&rest[i + 1..i + j]);
        rest = &rest[i + j + 1..];
    }
    lits.push(rest);
    (lits, names)
}

/// 在已格式化的字符串里按模板字面片段顺序定位，反查各占位符的值。
fn match_template(tmpl: &str, input: &str) -> Option<Vec<(String, String)>> {
    let (lits, names) = template_parts(tmpl);
    if names.is_empty() || !input.starts_with(lits[0]) {
        return None;
    }
    let mut pos = lits[0].len();
    let mut args = Vec::new();
    let mut anon = 0usize;
    for (k, name) in names.iter().enumerate() {
        let next = lits[k + 1];
        let end = if next.is_empty() {
            input.len()
        } else {
            input[pos..].find(next)? + pos
        };
        let key = if name.is_empty() {
            anon += 1;
            format!("__anon{anon}")
        } else {
            (*name).to_owned()
        };
        args.push((key, input[pos..end].to_owned()));
        pos = end + next.len();
    }
    if pos != input.len() {
        return None;
    }
    Some(args)
}

/// 用反查出的参数回填英文模板：命名占位符按名，匿名 {} 按序。
fn fill_template(en_tmpl: &str, args: &[(String, String)]) -> String {
    let mut s = en_tmpl.to_owned();
    for (name, value) in args {
        if !name.starts_with("__anon") {
            s = s.replace(&format!("{{{name}}}"), value);
        }
    }
    for (name, value) in args {
        if name.starts_with("__anon") && s.contains("{}") {
            s = s.replacen("{}", value, 1);
        }
    }
    s
}

/// 按语言取文案；未登记的 msgid 回退源语言中文。
/// 含插值的 msgid 先试模板反查，故已格式化的 fact / message 也能翻译。
pub fn t<'a>(lang: Language, zh: &'a str) -> Cow<'a, str> {
    if lang == Language::Zh {
        return Cow::Borrowed(zh);
    }
    if let Some(s) = en(zh) {
        return Cow::Borrowed(s);
    }
    for tmpl in TEMPLATES {
        if let Some(args) = match_template(tmpl, zh)
            && let Some(en_tmpl) = en(tmpl)
        {
            return Cow::Owned(fill_template(en_tmpl, &args));
        }
    }
    Cow::Borrowed(zh)
}

/// 命名占位符模板：英文译文保留同名占位符，顺序无关。
pub fn tf(lang: Language, tmpl: &str, args: &[(&str, String)]) -> String {
    let mut s = t(lang, tmpl).into_owned();
    for (key, value) in args {
        s = s.replace(&format!("{{{key}}}"), value);
    }
    s
}

/// 匿名占位符模板：按出现顺序替换 {}；英文译文保留相同数量的 {}。
/// 用于含插值的工具 content（Rust 的 format! 要求编译期格式串，故在返回层替换）。
pub fn tformat(lang: Language, tmpl: &str, args: &[String]) -> String {
    let mut s = t(lang, tmpl).into_owned();
    for a in args {
        s = s.replacen("{}", a.as_str(), 1);
    }
    s
}

/// 递归本地化 JSON 值里的字符串（tools/list 的 inputSchema 描述）。
pub fn localize_value(lang: Language, v: &Value) -> Value {
    match v {
        Value::String(s) => Value::String(t(lang, s).into_owned()),
        Value::Array(a) => Value::Array(a.iter().map(|x| localize_value(lang, x)).collect()),
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, x)| (k.clone(), localize_value(lang, x)))
                .collect(),
        ),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn language_parses_bcp47_prefixes() {
        assert_eq!(Language::parse("zh-CN"), Some(Language::Zh));
        assert_eq!(Language::parse("EN_us"), Some(Language::En));
        assert_eq!(Language::parse("fr"), None);
        assert_eq!(Language::Zh.as_str(), "zh-CN");
    }

    #[test]
    fn t_translates_and_falls_back_to_source_language() {
        assert_eq!(t(Language::En, "目标文件路径"), "Target file path");
        assert_eq!(t(Language::Zh, "目标文件路径"), "目标文件路径");
        assert_eq!(t(Language::En, "未登记的消息"), "未登记的消息");
    }

    #[test]
    fn tf_replaces_named_placeholders_in_both_languages() {
        let args = [("e", "boom".to_owned())];
        assert_eq!(
            tf(Language::En, "JSON 解析失败：{e}", &args),
            "JSON parse failed: boom"
        );
        assert_eq!(
            tf(Language::Zh, "JSON 解析失败：{e}", &args),
            "JSON 解析失败：boom"
        );
    }

    #[test]
    fn t_matches_interpolated_error_messages() {
        assert_eq!(
            t(Language::En, "无法启动 git：boom"),
            "Cannot start git: boom"
        );
        assert_eq!(
            t(
                Language::En,
                "未知 intent bogus；允许：raw / summary / structured / visual / auto"
            ),
            "unknown intent bogus; allowed: raw / summary / structured / visual / auto"
        );
        assert_eq!(t(Language::Zh, "无法启动 git：boom"), "无法启动 git：boom");
        assert_eq!(t(Language::En, "完全无关的消息"), "完全无关的消息");
    }

    #[test]
    fn template_parts_and_en_entries_stay_in_sync() {
        for tmpl in TEMPLATES {
            assert!(en(tmpl).is_some(), "模板缺少英文译文: {tmpl}");
        }
    }

    #[test]
    fn tformat_replaces_anonymous_placeholders_in_order() {
        assert_eq!(
            tformat(
                Language::En,
                "{} 是目录。用 search 的 files 模式列目录内容。",
                &["/tmp/a".to_owned()]
            ),
            "/tmp/a is a directory. Use search in files mode to list it."
        );
        assert_eq!(
            tformat(
                Language::Zh,
                "已截断到 {} 行（行数上限）",
                &["5".to_owned()]
            ),
            "已截断到 5 行（行数上限）"
        );
        assert_eq!(
            tformat(
                Language::En,
                "已截断到 {} 行（行数上限）",
                &["5".to_owned()]
            ),
            "Truncated to 5 lines (line limit)"
        );
    }

    #[test]
    fn localize_value_walks_nested_objects_and_keeps_enums() {
        let v = json!({ "description": "目标文件路径", "enum": ["raw", "summary"] });
        let out = localize_value(Language::En, &v);
        assert_eq!(out["description"], json!("Target file path"));
        assert_eq!(out["enum"], json!(["raw", "summary"]));
    }

    /// §17.2 / §18.1b：安全底线新增文案在英文下必须整句英文，不留中文。
    #[test]
    fn security_messages_are_fully_english() {
        fn contains_han(text: &str) -> bool {
            text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
        }
        let refusal = crate::ToolError::isolation_refused(
            "/etc/passwd",
            &[std::path::PathBuf::from("/work")],
        );
        let view = crate::error::govern_error(&refusal.localized(Language::En));
        for text in [view.fact, view.reason, view.next_steps.join("；")] {
            assert!(!contains_han(&text), "英文输出残留中文：{text}");
        }
        let timeout = crate::ToolError::search_walk_timeout(
            std::time::Duration::from_millis(11_000),
            std::time::Duration::from_millis(10_000),
        );
        let view = crate::error::govern_error(&timeout.localized(Language::En));
        for text in [view.fact, view.reason, view.next_steps.join("；")] {
            assert!(!contains_han(&text), "英文输出残留中文：{text}");
        }
        for zh in [
            "\n[遍历已截断：递归深度达 32 层，结果只覆盖已遍历部分]",
            "\n[遍历已截断：文件数达 200000 个，结果只覆盖已遍历部分]",
            "\n[遍历已截断：递归深度达 32 层且文件数达 200000 个，结果只覆盖已遍历部分]",
        ] {
            let en = t(Language::En, zh);
            assert!(!contains_han(&en), "截断告知残留中文：{en}");
        }
    }
}
