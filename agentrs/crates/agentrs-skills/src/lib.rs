//! AgentRS `skills` —— 技能清单的解析与合并（架构 §10）。
//!
//! AgentRS **只解析和执行** Core 交来的 `SkillManifest` 与内容包。
//! 技能**不是权限插件**：它能在已有能力内挑一个子集，不能引入新能力。
//!
//! - ✅ [`modifier`] `ContextModifier` 的单调收窄合并
//! - ✅ [`placement`] 缓存落位与"中途改 S0 推迟到压缩边界"
//! - ⬜ B 类移植：技能发现与内容包解析（归 Core 的部分不移植）

#![forbid(unsafe_code)]

pub mod modifier;
pub mod placement;

pub use modifier::{merge_all, ContextModifier, ContextView, Merged, Narrowing};
pub use placement::{defer_to_boundary, Contribution, Moment, Placement, SkillRef};
