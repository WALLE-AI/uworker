// Ported from aionrs (Apache-2.0), crates/aion-skills.
//   Source: crates/aion-skills/src/frontmatter.rs @ f711174
//   Copied: 2026-09-01   Modified: yes
//   Changes: 去掉 `SkillSource`/`LoadedFrom`/`skill_root` 三个发现相关的参数
//            （发现归 Core）；`tracing::warn!` 改为把解析失败**返回给调用方**
//            而不是记日志后吞掉（内核不持有日志设施，而且"技能没装上"该是
//            调用方能看见的事）；`expand_braces` 的 let-chain 改写为嵌套 if
//            （本仓库是 edition 2021）；花括号展开加了展开数上限。

//! 技能内容包的解析。
//!
//! 一个技能是一份 Markdown：`---` 围起来的 YAML frontmatter，加一段正文。
//! 本模块把那段文本变成 [`SkillMetadata`]。
//!
//! # 边界
//!
//! **只解析，不发现。** 技能文件在哪个目录、哪一层优先、要不要监视文件变化，
//! 全归 Core（见 crate 文档：技能不是权限插件）。本模块收一段 `&str`，
//! 不碰磁盘，也不知道这段文本是从哪儿来的。
//!
//! # 解析失败不 panic，也不静默
//!
//! 用户手写的 YAML 一定会有写坏的时候。上游的做法是记一条 warning 然后返回
//! 空 frontmatter——技能于是"装上了"但什么也不做，而使用者只看到它不生效。
//! 这里改成把失败**返回给调用方**（[`ParseOutcome::Recovered`] /
//! [`ParseOutcome::Failed`]），由 Core 决定是提示用户还是跳过。

/// 接到 `modifier` 的收窄合并上。
pub mod bridge;
/// 按文件路径激活的条件技能。
pub mod conditional;
/// frontmatter 与正文的解析、字段归一化。
pub mod frontmatter;
/// 技能清单的预算内排版。
pub mod listing;
/// 技能的放行判定。
pub mod permissions;
/// 正文里的参数替换。
pub mod substitution;
/// 内容包的字段类型。
pub mod types;

pub use bridge::{overrides_of, SkillOverrides};
pub use conditional::ConditionalSkills;
pub use frontmatter::{parse_pack, parse_skill_fields, ParseOutcome};
pub use listing::{format_skill_entry, format_within_budget};
pub use permissions::{PermissionRule, SkillPermission, SkillPermissionChecker};
pub use substitution::{parse_arguments, substitute, substitute_for};
pub use types::{
    BoolOrString, EffortLevel, ExecutionContext, FrontmatterData, ParsedMarkdown, SkillMetadata,
    StringOrNumber, StringOrVec,
};
