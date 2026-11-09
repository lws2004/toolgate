//! 错误契约：§1.4.5 三层保证里的「返回层」与「测试层」基础设施。
//!
//! 一次错误只有一个事实源 \`ErrorView\`：content 文本与 structuredContent.error
//! 都由它渲染，避免两处各写一份文案漂移（§4.4b 的「一个事实源，两个通道」）。
//!
//! v1 骨架的 \`ToolError\` 仍是 tuple 变体；\`govern_error\` 把它映射成三段式，
//! 让传输层无需改动各工具即可输出可行动错误。ToolError 自身的 struct 变体
//! 重构（§13.3）随阶段 2 各工具 run() 重写一并完成——见交付报告的遗留问题。

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::tools::{Content, ToolResponse};

/// §1.4.5 的稳定错误码。客户端按它分支；PascalCase 变体名，序列化为全大写蛇形。
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
}

impl ErrorCode {
    /// 稳定字符串，供程序分支与 JSON 的 error.code 使用。
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
        }
    }
}

/// 一次错误的唯一事实源。三段式见 §1.4.4：事实 / 原因 / 选项。
#[derive(Debug, Clone)]
pub struct ErrorView {
    /// 稳定错误码（§1.4.5 第一层）。ErrorCode 的序列化值，或 v1 遗留的通用码。
    pub code: String,
    pub fact: String,
    pub reason: String,
    pub next_steps: Vec<String>,
}

impl ErrorView {
    /// 渲染进 MCP content 的文本块：三段式，给模型读。
    pub fn to_content_text(&self) -> String {
        format!(
            "事实：{}\n原因：{}\n可选下一步：{}",
            self.fact,
            self.reason,
            self.next_steps.join("；")
        )
    }

    /// 渲染进 structuredContent.error：字段 camelCase，给程序判断。
    pub fn to_structured(&self) -> Value {
        json!({
            "code": self.code,
            "fact": self.fact,
            "reason": self.reason,
            "nextSteps": self.next_steps,
        })
    }
}

/// v1 遗留错误的通用码。它们不在 §1.4.5 的 ErrorCode 值域内，
/// 只用于返回层的稳定分支；扩充 ErrorCode 属契约变更（§3.4），不在此擅自改。
const UNKNOWN_TOOL: &str = "UNKNOWN_TOOL";
const BAD_ARGUMENT: &str = "BAD_ARGUMENT";
const IO_ERROR: &str = "IO_ERROR";
const FAILED: &str = "FAILED";

/// 把 v1 的 ToolError 映射成三段式 ErrorView（§1.4.5 第二层）。
/// 每个分支都带可执行的下一步；缺一个即为 bug，由测试层锁定。
pub fn govern_error(e: &crate::ToolError) -> ErrorView {
    match e {
        crate::ToolError::UnknownTool(name) => ErrorView {
            code: UNKNOWN_TOOL.to_owned(),
            fact: format!("请求的工具 {name} 不在工具表内"),
            reason: "工具名必须是 tools/list 返回的六个名字之一".to_owned(),
            next_steps: vec![
                "调用 tools/list 查看可用工具名".to_owned(),
                "改用 read / write / edit / search / bash / task 之一".to_owned(),
            ],
        },
        crate::ToolError::BadArgument(msg) => ErrorView {
            code: BAD_ARGUMENT.to_owned(),
            fact: msg.clone(),
            reason: "入参不符合该工具的 inputSchema".to_owned(),
            next_steps: vec![
                "对照 tools/list 里该工具的 inputSchema 补齐必填参数".to_owned(),
                "参数名区分大小写，检查拼写后重试".to_owned(),
            ],
        },
        crate::ToolError::Io(io) => {
            use std::io::ErrorKind;
            let code = match io.kind() {
                ErrorKind::NotFound => ErrorCode::PathNotFound.as_str(),
                ErrorKind::InvalidData => ErrorCode::EncodingInvalid.as_str(),
                ErrorKind::TimedOut => ErrorCode::Timeout.as_str(),
                _ => IO_ERROR,
            };
            ErrorView {
                code: code.to_owned(),
                fact: format!("文件系统调用失败：{io}"),
                reason: "工具层无法完成该 IO 操作".to_owned(),
                next_steps: vec![
                    "确认路径存在且可访问".to_owned(),
                    "用 bash 检查权限与磁盘状态后重试".to_owned(),
                ],
            }
        }
        crate::ToolError::Failed(msg) => ErrorView {
            code: FAILED.to_owned(),
            fact: msg.clone(),
            reason: "工具执行未能完成".to_owned(),
            next_steps: vec![
                "按 fact 里的失败点修正输入后重试".to_owned(),
                "若持续失败，改用更简单的等价调用".to_owned(),
            ],
        },
    }
}

/// 构造一条完整的错误响应，让传输层的 Ok / Err 两条路径共用同一套序列化。
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

    /// §1.4.5 第三层：所有错误路径的样本。
    fn all_error_paths() -> Vec<crate::ToolError> {
        vec![
            crate::ToolError::UnknownTool("frobnicate".to_owned()),
            crate::ToolError::BadArgument("缺少字符串参数 path".to_owned()),
            crate::ToolError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no such file",
            )),
            crate::ToolError::Failed("任务表锁失效".to_owned()),
        ]
    }

    /// 遍历所有错误路径，断言 nextSteps 非空（§1.4.5 第三层）。
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

    /// 元测试：match 穷举 ToolError 变体，不加 \`_\` 通配——新增变体即编译失败。
    #[test]
    fn all_tool_error_variants_covered() {
        for e in all_error_paths() {
            match &e {
                crate::ToolError::UnknownTool(_) => {}
                crate::ToolError::BadArgument(_) => {}
                crate::ToolError::Io(_) => {}
                crate::ToolError::Failed(_) => {}
            }
        }
    }

    /// 元测试：match 穷举 ErrorCode 变体，并锁定 as_str 与 serde tag 一致。
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
            }
            assert_eq!(serde_json::to_value(c).unwrap(), json!(c.as_str()));
        }
    }

    /// 返回层形状：structuredContent.error 四个字段齐全且用 camelCase。
    #[test]
    fn structured_error_shape() {
        let view = govern_error(&crate::ToolError::BadArgument("缺少 path".to_owned()));
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

    /// content 文本是可行动的三段，而不是一句错误。
    #[test]
    fn content_text_has_three_parts() {
        let view = govern_error(&crate::ToolError::Failed("执行失败".to_owned()));
        let text = view.to_content_text();
        assert!(text.contains("事实："));
        assert!(text.contains("原因："));
        assert!(text.contains("可选下一步："));
        assert!(text.contains("执行失败"));
    }

    /// IO 错误按 ErrorKind 落到 §1.4.5 的稳定码。
    #[test]
    fn io_error_maps_to_stable_code() {
        let e = crate::ToolError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "bad utf-8",
        ));
        assert_eq!(govern_error(&e).code, "ENCODING_INVALID");
    }
}
