//! # AgentRS Types
//!
//! provider 无关的消息、内容块与用量模型。
//!
//! 本 crate 大部分自 aionrs 移植（A 类）——五个厂商的 wire format 是外部事实，
//! 与本架构无关，重写只会引入新 bug。移植清单见 `THIRD-PARTY-NOTICES.md`。

#![forbid(unsafe_code)]

/// 压缩边界的元数据与触发方式。
pub mod compact;
/// 模型读过的文件状态（读去重用）。
pub mod file_state;
/// 技能相关的共享类型。
pub mod skill_types;
pub mod glob;
pub mod llm;
pub mod message;
pub mod schema;
pub mod tool;

pub use glob::{glob_match, leaf, under};
pub use llm::{LlmEvent, LlmRequest, ThinkingConfig};
pub use message::{
    extension_to_image_media_type, ContentBlock, ImageInputCapability, ImageUrl, Message, Role, StopReason,
    TokenUsage, SUPPORTED_IMAGE_MEDIA_TYPES,
};
pub use schema::{describe_all, validate, Violation};
pub use tool::ToolDef;
