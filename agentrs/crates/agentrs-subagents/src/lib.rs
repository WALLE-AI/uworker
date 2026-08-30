//! AgentRS `subagents` —— 函数式子 Agent（架构 §11.1）。
//!
//! ```text
//! 输入 → 独立受限上下文 → schema 化结果
//! ```
//!
//! **没有持久身份、默认零工具、不能发消息、不能建命令。**
//! 主 Agent 只拿结论，过程推理与原始上下文不自动回灌。
//!
//! - ✅ [`functional`] 通用骨架
//! - ✅ [`compact`] 压缩摘要（§9.3 第 3 段）
//! - ✅ [`childrun`] ChildRun 派生规则（§11.1，内核不变量 5）
//! - ✅ Explore / Plan / ToolSearch / contextSummary
//! - ✅ memorySelector（由 `agentrs-memory` 实现）

#![forbid(unsafe_code)]

pub mod childrun;
pub mod compact;
pub mod functional;
pub mod memberrun;
pub mod structured;

pub use childrun::{derive, ChildRequest, DeriveError, Parent};
pub use functional::{build_request, collect, SubagentError, SubagentInput, SubagentOutput};
pub use memberrun::{derive_member, MemberDeriveError, MemberParent, MemberRequest};
pub use structured::{
    parse_structured, parse_tool_search, to_summary, FunctionalKind, StructuredConclusion, ToolSearchDecision,
};
