//! toolgate 核心：工具契约、输出治理与附件流水线。
//!
//! 本 crate 不关心传输层。MCP、CLI 或嵌入式调用都复用同一套
//! 工具实现与治理规则。

pub mod hashline;
pub mod media;
pub mod spill;
pub mod tools;
pub mod truncate;

pub use truncate::{
    DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, TruncatedBy, TruncationOptions, TruncationResult,
    truncate_head, truncate_tail,
};

/// 工具层统一的错误类型。
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("未知工具：{0}")]
    UnknownTool(String),
    #[error("参数错误：{0}")]
    BadArgument(String),
    #[error("文件系统错误：{0}")]
    Io(#[from] std::io::Error),
    #[error("工具执行失败：{0}")]
    Failed(String),
}
