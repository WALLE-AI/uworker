//! # AgentRS Types
//!
//! provider 无关的消息、内容块与用量模型。
//!
//! 本 crate 大部分自 aionrs 移植（A 类）——五个厂商的 wire format 是外部事实，
//! 与本架构无关，重写只会引入新 bug。移植清单见 `THIRD-PARTY-NOTICES.md`。

#![forbid(unsafe_code)]

pub mod llm;
pub mod message;
pub mod tool;

pub use llm::{LlmEvent, LlmRequest, ThinkingConfig};
pub use message::{
    extension_to_image_media_type, ContentBlock, ImageInputCapability, ImageUrl, Message, Role, StopReason,
    TokenUsage, SUPPORTED_IMAGE_MEDIA_TYPES,
};
pub use tool::ToolDef;
