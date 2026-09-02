//! # AgentRS Context
//!
//! token 账本、预算裁剪、请求装配与缓存分段。
//!
//! ## 当前进度
//!
//! - ✅ [`budget`] —— 保守估算器 + 优先级阶梯裁剪（T11 起步）
//! - ✅ [`cache`] —— 缓存分段布局、前缀摘要、断裂归因（T05C）
//! - ✅ [`assembler`] —— 请求装配 / ModelRequestManifest / ContentRef 降级与 retain
//! - ✅ [`compaction`] —— 四段压缩规划（对话历史）
//! - ✅ [`compact`] —— 工具输出压缩（单次输出，进上下文之前）

#![forbid(unsafe_code)]

pub mod assembler;
pub mod budget;
pub mod cache;
pub mod cache_diagnostics;
pub mod compact;
/// 长对话的压缩策略：估算、状态、自动/微/紧急三档。
pub mod strategy;
pub mod compaction;

pub use compact::{compact_output, compact_output_toon, CompactLevel};

pub use cache::{attribute, CacheLayout, CacheStats, RequestSnapshot, Segment};

pub use budget::{trim_to_budget, ConservativeEstimator, Fragment, Priority, TrimResult};

pub use assembler::{
    assemble, resolve_text_refs, retain_manifest_refs, AssembleError, ContextPlan, ContextPlanInput,
    PlannedMessage, Resolution,
};
