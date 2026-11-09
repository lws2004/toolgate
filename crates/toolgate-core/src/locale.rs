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
        "调整隔离策略" => "Adjust the isolation policy",
        "改用允许的路径" => "Use an allowed path instead",
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
        "content 模式是正则表达式；files 模式是文件名子串或通配符" => {
            "content mode is a regular expression; files mode is a filename substring or glob"
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
        _ => return None,
    })
}

/// 按语言取文案；未登记的 msgid 回退源语言中文。
pub fn t<'a>(lang: Language, zh: &'a str) -> Cow<'a, str> {
    match lang {
        Language::Zh => Cow::Borrowed(zh),
        Language::En => match en(zh) {
            Some(s) => Cow::Borrowed(s),
            None => Cow::Borrowed(zh),
        },
    }
}

/// 命名占位符模板：英文译文保留同名占位符，顺序无关。
pub fn tf(lang: Language, tmpl: &str, args: &[(&str, String)]) -> String {
    let mut s = t(lang, tmpl).into_owned();
    for (key, value) in args {
        s = s.replace(&format!("{{{key}}}"), value);
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
    fn localize_value_walks_nested_objects_and_keeps_enums() {
        let v = json!({ "description": "目标文件路径", "enum": ["raw", "summary"] });
        let out = localize_value(Language::En, &v);
        assert_eq!(out["description"], json!("Target file path"));
        assert_eq!(out["enum"], json!(["raw", "summary"]));
    }
}
