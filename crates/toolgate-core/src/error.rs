//! 错误契约（§1.4.5）：类型层、返回层与测试层。
//!
//! 一次错误只有一个事实源 ErrorView：content 文本与 structuredContent.error
//! 都由它渲染，避免两处各写一份文案漂移（§4.4b 的「一个事实源，两个通道」）。
//! ToolError 的四个变体都带 errorCode 与 next_steps（§2 红线 7）。

use std::io::ErrorKind;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::locale::{Language, t, tf};
use crate::tools::{Content, ToolResponse};

/// §1.4.5 稳定错误码：8 个特定场景 + 4 个通用兜底，封闭 12 个。
/// 与 §4.4b 的 notice code 分属两个命名空间，不做互相映射。
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    PathNotFound,
    EncodingInvalid,
    AnchorMismatch,
    EditAmbiguous,
    Timeout,
    OutputLimit,
    SlotUnavailable,
    IsolationRefused,
    UnknownTool,
    BadArgument,
    IoError,
    Failed,
}

impl ErrorCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ErrorCode::PathNotFound => "PATH_NOT_FOUND",
            ErrorCode::EncodingInvalid => "ENCODING_INVALID",
            ErrorCode::AnchorMismatch => "ANCHOR_MISMATCH",
            ErrorCode::EditAmbiguous => "EDIT_AMBIGUOUS",
            ErrorCode::Timeout => "TIMEOUT",
            ErrorCode::OutputLimit => "OUTPUT_LIMIT",
            ErrorCode::SlotUnavailable => "SLOT_UNAVAILABLE",
            ErrorCode::IsolationRefused => "ISOLATION_REFUSED",
            ErrorCode::UnknownTool => "UNKNOWN_TOOL",
            ErrorCode::BadArgument => "BAD_ARGUMENT",
            ErrorCode::IoError => "IO_ERROR",
            ErrorCode::Failed => "FAILED",
        }
    }
}

/// 按码给出可执行的下一步（§1.4.4）。新增码必须补 match 分支。
fn next_steps_for(code: ErrorCode) -> Vec<String> {
    let steps: &[&str] = match code {
        ErrorCode::PathNotFound => &["确认路径存在且可访问", "用 search 定位文件后重试"],
        ErrorCode::EncodingInvalid => &[
            "用 bash 转码后处理：iconv -f GBK -t UTF-8 原文件 > /tmp/x",
            "或改用 read 查看可读部分",
        ],
        ErrorCode::AnchorMismatch => &["重新 read 获取最新行锚点", "重算锚点后重试"],
        ErrorCode::EditAmbiguous => &["在 edits 里补更多上下文使 oldText 唯一", "改用锚点模式定位"],
        ErrorCode::Timeout => &["缩小命令范围或分批执行", "用 task 后台执行长任务"],
        ErrorCode::OutputLimit => &["用 offset/limit 分段读取", "缩小 search 的 maxResults"],
        ErrorCode::SlotUnavailable => &["检查 models 的 serves 声明", "或让它按 §3.8 走机械路径"],
        ErrorCode::IsolationRefused => &["调整隔离策略", "改用允许的路径"],
        ErrorCode::UnknownTool => &[
            "调用 tools/list 查看可用工具名",
            "改用 read / write / edit / search / bash / task 之一",
        ],
        ErrorCode::BadArgument => &[
            "对照 tools/list 里该工具的 inputSchema 补齐必填参数",
            "参数名区分大小写，检查拼写后重试",
        ],
        ErrorCode::IoError => &["确认路径存在且可访问", "用 bash 检查权限与磁盘状态后重试"],
        ErrorCode::Failed => &[
            "按 fact 里的失败点修正输入后重试",
            "若持续失败，改用更简单的等价调用",
        ],
    };
    steps.iter().map(|s| (*s).to_owned()).collect()
}

const TOOL_NAMES: [&str; 6] = ["read", "write", "edit", "search", "bash", "task"];

impl crate::ToolError {
    pub fn unknown_tool(name: impl Into<String>) -> Self {
        Self::UnknownTool {
            code: ErrorCode::UnknownTool,
            name: name.into(),
            available: TOOL_NAMES.iter().map(|s| (*s).to_owned()).collect(),
            next_steps: next_steps_for(ErrorCode::UnknownTool),
        }
    }

    pub fn bad_argument(message: impl Into<String>) -> Self {
        Self::BadArgument {
            code: ErrorCode::BadArgument,
            message: message.into(),
            next_steps: next_steps_for(ErrorCode::BadArgument),
        }
    }

    pub fn failed(fact: impl Into<String>) -> Self {
        Self::failed_with(ErrorCode::Failed, fact)
    }

    pub fn failed_with(code: ErrorCode, fact: impl Into<String>) -> Self {
        Self::Failed {
            code,
            fact: fact.into(),
            reason: "工具执行未能完成".to_owned(),
            next_steps: next_steps_for(code),
        }
    }
}

impl crate::ToolError {
    /// §18.1b：三段式文案按用户语言本地化；字段里的中文是源语言 msgid。
    pub fn localized(&self, lang: Language) -> Self {
        use crate::ToolError as E;
        let l = |s: &str| t(lang, s).into_owned();
        let steps = |v: &Vec<String>| v.iter().map(|s| l(s)).collect();
        match self {
            E::UnknownTool {
                code,
                name,
                available,
                next_steps,
            } => E::UnknownTool {
                code: *code,
                name: name.clone(),
                available: available.clone(),
                next_steps: steps(next_steps),
            },
            E::BadArgument {
                code,
                message,
                next_steps,
            } => E::BadArgument {
                code: *code,
                message: l(message),
                next_steps: steps(next_steps),
            },
            E::Io {
                code,
                fact,
                reason,
                next_steps,
            } => E::Io {
                code: *code,
                fact: l(fact),
                reason: l(reason),
                next_steps: steps(next_steps),
            },
            E::Failed {
                code,
                fact,
                reason,
                next_steps,
            } => E::Failed {
                code: *code,
                fact: l(fact),
                reason: l(reason),
                next_steps: steps(next_steps),
            },
        }
    }
}

impl From<std::io::Error> for crate::ToolError {
    fn from(e: std::io::Error) -> Self {
        let code = match e.kind() {
            ErrorKind::NotFound => ErrorCode::PathNotFound,
            ErrorKind::InvalidData => ErrorCode::EncodingInvalid,
            ErrorKind::TimedOut => ErrorCode::Timeout,
            _ => ErrorCode::IoError,
        };
        Self::Io {
            code,
            fact: format!("文件系统调用失败：{e}"),
            reason: "工具层无法完成该 IO 操作".to_owned(),
            next_steps: next_steps_for(code),
        }
    }
}

/// 一次错误的唯一事实源。三段式见 §1.4.4：事实 / 原因 / 选项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorView {
    pub code: String,
    pub fact: String,
    pub reason: String,
    pub next_steps: Vec<String>,
}

impl ErrorView {
    pub fn to_content_text(&self) -> String {
        format!(
            "事实：{}\n原因：{}\n可选下一步：{}",
            self.fact,
            self.reason,
            self.next_steps.join("；")
        )
    }

    pub fn to_structured(&self) -> Value {
        json!({
            "code": self.code,
            "fact": self.fact,
            "reason": self.reason,
            "nextSteps": self.next_steps,
        })
    }
}

/// 把 ToolError 映射成三段式 ErrorView（§1.4.5 第二层）。
/// 每个分支都带可执行的下一步；缺一个即为 bug，由测试层锁定。
pub fn govern_error(e: &crate::ToolError) -> ErrorView {
    match e {
        crate::ToolError::UnknownTool {
            code,
            name,
            available,
            next_steps,
        } => ErrorView {
            code: code.as_str().to_owned(),
            fact: format!("请求的工具 {name} 不在工具表内"),
            reason: format!("可用工具：{}", available.join("、")),
            next_steps: next_steps.clone(),
        },
        crate::ToolError::BadArgument {
            code,
            message,
            next_steps,
        } => ErrorView {
            code: code.as_str().to_owned(),
            fact: message.clone(),
            reason: "入参不符合该工具的 inputSchema".to_owned(),
            next_steps: next_steps.clone(),
        },
        crate::ToolError::Io {
            code,
            fact,
            reason,
            next_steps,
        } => ErrorView {
            code: code.as_str().to_owned(),
            fact: fact.clone(),
            reason: reason.clone(),
            next_steps: next_steps.clone(),
        },
        crate::ToolError::Failed {
            code,
            fact,
            reason,
            next_steps,
        } => ErrorView {
            code: code.as_str().to_owned(),
            fact: fact.clone(),
            reason: reason.clone(),
            next_steps: next_steps.clone(),
        },
    }
}

/// 构造一条完整的错误响应，让传输层的 Ok / Err 两条路径共用同一套序列化。
/// 带用户语言的错误响应：content 标签与分隔符同样按语言取（§18.1b）。
pub fn error_response_with_lang(view: &ErrorView, lang: Language) -> ToolResponse {
    let text = tf(
        lang,
        "事实：{fact}\n原因：{reason}\n可选下一步：{next}",
        &[
            ("fact", view.fact.clone()),
            ("reason", view.reason.clone()),
            ("next", view.next_steps.join(t(lang, "；").as_ref())),
        ],
    );
    ToolResponse {
        content: vec![Content::Text(text)],
        is_error: true,
        meta: json!({ "error": view.to_structured() }),
    }
}

pub fn error_response(view: &ErrorView) -> ToolResponse {
    ToolResponse {
        content: vec![Content::Text(view.to_content_text())],
        is_error: true,
        meta: json!({ "error": view.to_structured() }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_error_paths() -> Vec<crate::ToolError> {
        vec![
            crate::ToolError::unknown_tool("frobnicate"),
            crate::ToolError::bad_argument("缺少字符串参数 path"),
            std::io::Error::new(ErrorKind::NotFound, "no such file").into(),
            crate::ToolError::failed("任务表锁失效"),
        ]
    }

    /// §1.4.5 第三层：遍历所有错误路径，断言 nextSteps 非空。
    #[test]
    fn every_error_carries_next_steps() {
        for e in all_error_paths() {
            let view = govern_error(&e);
            assert!(!view.code.is_empty(), "错误缺少 errorCode：{e:?}");
            assert!(!view.fact.is_empty(), "错误缺少事实：{e:?}");
            assert!(!view.reason.is_empty(), "错误缺少原因：{e:?}");
            assert!(!view.next_steps.is_empty(), "错误缺少可执行下一步：{e:?}");
            let structured = view.to_structured();
            let steps = structured["nextSteps"].as_array().unwrap();
            assert!(
                !steps.is_empty(),
                "structuredContent.error.nextSteps 为空：{e:?}"
            );
        }
    }

    /// 元测试：match 穷举 ToolError 变体，不加 _ 通配——新增变体即编译失败。
    #[test]
    fn all_tool_error_variants_covered() {
        for e in all_error_paths() {
            match &e {
                crate::ToolError::UnknownTool { .. } => {}
                crate::ToolError::BadArgument { .. } => {}
                crate::ToolError::Io { .. } => {}
                crate::ToolError::Failed { .. } => {}
            }
        }
    }

    /// 元测试：match 穷举 ErrorCode 12 个变体，并锁定 as_str 与 serde tag 一致。
    #[test]
    fn all_error_codes_covered_and_stable() {
        let codes = [
            ErrorCode::PathNotFound,
            ErrorCode::EncodingInvalid,
            ErrorCode::AnchorMismatch,
            ErrorCode::EditAmbiguous,
            ErrorCode::Timeout,
            ErrorCode::OutputLimit,
            ErrorCode::SlotUnavailable,
            ErrorCode::IsolationRefused,
            ErrorCode::UnknownTool,
            ErrorCode::BadArgument,
            ErrorCode::IoError,
            ErrorCode::Failed,
        ];
        for c in codes {
            match c {
                ErrorCode::PathNotFound => {}
                ErrorCode::EncodingInvalid => {}
                ErrorCode::AnchorMismatch => {}
                ErrorCode::EditAmbiguous => {}
                ErrorCode::Timeout => {}
                ErrorCode::OutputLimit => {}
                ErrorCode::SlotUnavailable => {}
                ErrorCode::IsolationRefused => {}
                ErrorCode::UnknownTool => {}
                ErrorCode::BadArgument => {}
                ErrorCode::IoError => {}
                ErrorCode::Failed => {}
            }
            assert_eq!(serde_json::to_value(c).unwrap(), json!(c.as_str()));
        }
        assert_eq!(codes.len(), 12);
    }

    #[test]
    fn structured_error_shape() {
        let view = govern_error(&crate::ToolError::bad_argument("缺少 path"));
        let v = view.to_structured();
        assert_eq!(v["code"], json!("BAD_ARGUMENT"));
        assert!(v["fact"].is_string());
        assert!(v["reason"].is_string());
        assert!(v["nextSteps"].is_array());
        assert!(v.get("next_steps").is_none());
        assert_eq!(
            error_response(&view).meta["error"]["nextSteps"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn content_text_has_three_parts() {
        let view = govern_error(&crate::ToolError::failed("执行失败"));
        let text = view.to_content_text();
        assert!(text.contains("事实："));
        assert!(text.contains("原因："));
        assert!(text.contains("可选下一步："));
        assert!(text.contains("执行失败"));
    }

    #[test]
    fn io_error_maps_to_stable_code() {
        let e: crate::ToolError = std::io::Error::new(ErrorKind::InvalidData, "bad utf-8").into();
        assert_eq!(govern_error(&e).code, "ENCODING_INVALID");
        assert_eq!(govern_error(&e).next_steps.len(), 2);
    }

    #[test]
    fn failed_with_allows_precise_attribution() {
        let e = crate::ToolError::failed_with(ErrorCode::PathNotFound, "无法访问 /tmp/x");
        assert_eq!(govern_error(&e).code, "PATH_NOT_FOUND");
        assert!(govern_error(&e).next_steps[0].contains("路径"));
    }

    #[test]
    fn unknown_tool_lists_available_tools() {
        let view = govern_error(&crate::ToolError::unknown_tool("nope"));
        assert_eq!(view.code, "UNKNOWN_TOOL");
        assert!(view.reason.contains("read"));
        assert!(!view.next_steps.is_empty());
    }
}
