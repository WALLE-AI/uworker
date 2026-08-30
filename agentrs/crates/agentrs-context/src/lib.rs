//! # AgentRS Context
//!
//! token 账本、预算裁剪、请求装配与缓存分段。
//!
//! ## 当前进度
//!
//! - ✅ [`budget`] —— 保守估算器 + 优先级阶梯裁剪（T11 起步）
//! - ✅ [`cache`] —— 缓存分段布局、前缀摘要、断裂归因（T05C）
//! - ✅ [`assembler`] —— 请求装配 / ModelRequestManifest / ContentRef 降级与 retain
//! - ✅ [`compaction`] —— 四段压缩规划

#![forbid(unsafe_code)]

pub mod assembler;
pub mod budget;
pub mod cache;
pub mod cache_diagnostics;
pub mod compaction;

pub use cache::{attribute, CacheLayout, CacheStats, RequestSnapshot, Segment};

pub use budget::{trim_to_budget, ConservativeEstimator, Fragment, Priority, TrimResult};

pub use assembler::{
    assemble, resolve_text_refs, retain_manifest_refs, AssembleError, ContextPlan, ContextPlanInput,
    PlannedMessage, Resolution,
};
