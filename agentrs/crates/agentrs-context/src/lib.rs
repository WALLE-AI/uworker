//! # AgentRS Context
//!
//! token 账本、预算裁剪、请求装配与缓存分段。
//!
//! ## 当前进度
//!
//! - ✅ [`budget`] —— 保守估算器 + 优先级阶梯裁剪（T11 起步）
//! - ✅ [`cache`] —— 缓存分段布局、前缀摘要、断裂归因（T05C）
//! - ⬜ 请求装配 / ModelRequestManifest / 四段压缩

#![forbid(unsafe_code)]

pub mod budget;
pub mod cache;
pub mod cache_diagnostics;
pub mod compaction;

pub use cache::{attribute, CacheLayout, CacheStats, RequestSnapshot, Segment};

pub use budget::{trim_to_budget, ConservativeEstimator, Fragment, Priority, TrimResult};
