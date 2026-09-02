// Ported from aionrs (Apache-2.0), crates/aion-agent.
//   Source: crates/aion-agent/src/compact/mod.rs @ f711174
//   Copied: 2026-09-01   Modified: yes
//   Changes: 并入 `context_usage.rs`（原在 aion-agent 根）与 `aion-config` 的
//            `compact.rs`——三者是同一件事的配置、状态与策略，本仓库不设
//            config crate，所以配置随策略走。

// 逐字移植进来的一批文件，公开项尚未逐个补 Rust 文档。
// 本仓库 `missing_docs = warn` + `-D warnings`，所以先在模块级放开，
// **待二次优化时逐项补齐并去掉这一行**。
#![allow(missing_docs, reason = "aionrs 逐字移植，文档待二次优化补齐")]

//! 长对话的压缩守卫，由轻到重：
//! - **microcompact**：清掉旧的工具结果正文
//! - **autocompact**：按上下文阈值触发的 LLM 摘要
//! - **emergency**：逼近上下文窗口上限时挡住请求

/// 按上下文阈值触发的 LLM 摘要压缩。
pub mod auto;
/// 压缩相关的配置项。
pub mod config;
/// 逼近上下文窗口上限时的硬拦截。
pub mod emergency;
/// 工具结果与图像的 token 估算。
pub mod estimate;
/// 清掉旧工具结果正文的轻量压缩。
pub mod micro;
/// 压缩用的提示词。
pub mod prompt;
/// 压缩状态与断路器。
pub mod state;
/// 上下文用量的跟踪与呈现。
pub mod usage;

pub use auto::{autocompact, SummarizeError, Summarizer};
pub use config::CompactConfig;
pub use state::CompactState;
pub use usage::{ContextState, ContextStatus};
