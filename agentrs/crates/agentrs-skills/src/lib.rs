//! AgentRS `skills` —— 技能清单的解析与合并（架构 §10）。
//!
//! AgentRS **只解析和执行** Core 交来的 `SkillManifest` 与内容包。
//! 技能**不是权限插件**：它能在已有能力内挑一个子集，不能引入新能力。
//!
//! - ✅ [`modifier`] `ContextModifier` 的单调收窄合并
//! - ✅ [`placement`] 缓存落位与"中途改 S0 推迟到压缩边界"
//! - ✅ [`pack`] 内容包：frontmatter 解析、参数替换、放行判定、清单排版、
//!   条件激活、收窄桥接（自 aionrs 移植）
//! - ⬜ 技能发现（**归 Core，不移植**）：文件在哪、哪层优先、要不要监视变化


#![forbid(unsafe_code)]

pub mod modifier;
pub mod pack;
pub mod placement;

pub use modifier::{merge_all, ContextModifier, ContextView, Merged, Narrowing};
pub use pack::{
    format_within_budget, overrides_of, parse_pack, parse_skill_fields, ParseOutcome,
    substitute, substitute_for, ConditionalSkills, SkillMetadata, SkillPermission, SkillPermissionChecker,
};
pub use placement::{defer_to_boundary, Contribution, Moment, Placement, SkillRef};
