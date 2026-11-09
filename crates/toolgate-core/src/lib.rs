//! toolgate 核心：工具契约、输出治理与附件流水线。
//!
//! 本 crate 不关心传输层。MCP、CLI 或嵌入式调用都复用同一套
//! 工具实现与治理规则。

pub mod bash_path;
pub mod compat;
pub mod config;
pub mod decision;
pub mod edit_path;
pub mod encoding;
pub mod error;
pub use error::ErrorCode;
pub mod hashline;
pub mod locale;
pub mod media;
pub mod notice;
pub mod read_path;
pub mod search_path;
pub mod slot_state;
pub mod slots;
pub mod spill;
pub mod task_path;
pub mod tools;
pub mod truncate;
pub mod understanding;
pub mod warm;

pub use truncate::{
    DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, TruncatedBy, TruncationOptions, TruncationResult,
    truncate_head, truncate_tail,
};

/// 工具层统一的错误类型（§1.4.5 第一层：四个变体都带 errorCode 与 next_steps）。
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("未知工具：{name}")]
    UnknownTool {
        code: ErrorCode,
        name: String,
        available: Vec<String>,
        next_steps: Vec<String>,
    },
    #[error("参数错误：{message}")]
    BadArgument {
        code: ErrorCode,
        message: String,
        next_steps: Vec<String>,
    },
    #[error("文件系统错误：{fact}")]
    Io {
        code: ErrorCode,
        fact: String,
        reason: String,
        next_steps: Vec<String>,
    },
    #[error("工具执行失败：{fact}")]
    Failed {
        code: ErrorCode,
        fact: String,
        reason: String,
        next_steps: Vec<String>,
    },
}
