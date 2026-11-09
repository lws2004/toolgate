//! SLM 客户端层：协议适配与槽位调用编排（§18.1 / §3.2 / §3.11 / §7.2）。
//!
//! 纯协议逻辑（请求构造、响应解析、端点拼接）放在默认层，HTTP 发送放在
//! #[cfg(feature = "external-protocols")] 后面：默认构建不含外部协议客户端（§19.6 可选层），
//! 但适配逻辑照样可测。
//!
//! 架构依据：18-deployment.md §18.1（模型来源与协议）、§19.6（依赖分层与可选层）、
//! 03-slots.md §3.2（槽位契约）、§3.11（能力声明与失效状态机）、06-cost.md §7.2（置信度门控）。

pub mod client;
pub mod ordered_protocol;
pub mod systemone;
pub mod text_protocol;
pub mod vision_protocol;

pub use client::{
    DecideOutcome, RerankOutcome, SlotCallError, SlotClient, SummarizeOutcome, VisionOutcome,
};
pub use systemone::{
    SystemOneQuestion, SystemOneRequest, SystemOneResponse, build_request, endpoint,
    parse_decide_output,
};
