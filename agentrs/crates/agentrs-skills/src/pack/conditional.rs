// Ported from aionrs (Apache-2.0), crates/aion-skills.
//   Source: crates/aion-skills/src/conditional.rs @ f711174
//   Copied: 2026-09-01   Modified: yes
//   Changes: 用本仓库的 `agentrs_types::glob` 而不是 `glob` crate——两份 glob
//            语义迟早会分叉，而用户会问"为什么 paths: 和 Glob 匹配得不一样"；
//            去掉 `relativize` 与 `cwd` 参数（**收相对路径**：绝对路径怎么变
//            相对是宿主的事，内核不知道 cwd）；`HashMap` 改 `BTreeMap`，
//            于是激活顺序确定——上游明说"顺序不保证"，而不确定的顺序会让
//            trajectory 不可重放；去掉 tracing。

//! 条件技能：按文件路径激活。
//!
//! 一个带 `paths:` 的技能默认**休眠**——它不进目录、不占提示词预算。等模型真的
//! 碰到匹配的文件时才被激活。
//!
//! 这是技能数量增长之后唯一撑得住的形态：装了三十个技能，其中二十个是针对特定
//! 文件类型的，全量塞进系统提示是纯浪费。
//!
//! # 收相对路径
//!
//! 上游收绝对路径外加一个 `cwd`，自己做相对化。内核不知道 cwd 是什么
//! （那是宿主义务），所以这里直接收**工作区相对路径**——正是
//! `WorkspaceIo::list` 交出来的那种。

use std::collections::{BTreeMap, BTreeSet};

use agentrs_types::glob::glob_match;

use super::types::SkillMetadata;

/// 一个休眠的条件技能。
#[derive(Debug, Clone)]
struct Dormant {
    skill: SkillMetadata,
    patterns: Vec<String>,
}

/// 条件技能的休眠/激活状态。
///
/// **不含任何 I/O**：谁碰了哪些文件由调用方告知。
#[derive(Debug, Clone, Default)]
pub struct ConditionalSkills {
    dormant: BTreeMap<String, Dormant>,
    activated: BTreeMap<String, SkillMetadata>,
    /// 已激活过的名字。**跨 [`Self::clear_dormant`] 保留**——
    /// 重新加载一次技能目录不该让已经激活的技能退回休眠，
    /// 那会让模型在同一段会话里看着一个技能忽有忽无。
    activated_names: BTreeSet<String>,
}

impl ConditionalSkills {
    /// 空状态。
    pub fn new() -> Self {
        Self::default()
    }

    /// 把一批技能分成"无条件"与"休眠"两堆，返回前者。
    ///
    /// 已经激活过的按无条件处理——见 [`Self::activated_names`] 的理由。
    pub fn partition(&mut self, skills: Vec<SkillMetadata>) -> Vec<SkillMetadata> {
        let mut unconditional = Vec::new();
        for skill in skills {
            if skill.paths.is_empty() || self.activated_names.contains(&skill.name) {
                unconditional.push(skill);
            } else {
                let patterns = skill.paths.clone();
                self.dormant
                    .insert(skill.name.clone(), Dormant { skill, patterns });
            }
        }
        unconditional
    }

    /// 拿一批**工作区相对路径**去唤醒匹配的技能。
    ///
    /// 返回这次新激活的名字，**按名字排序**——上游用 `HashMap` 且明说顺序
    /// 不保证，而不确定的顺序会让同一段历史两次重放得到不同的 trajectory。
    pub fn activate_for_paths(&mut self, paths: &[&str]) -> Vec<String> {
        if self.dormant.is_empty() {
            return Vec::new();
        }
        let 命中: Vec<String> = self
            .dormant
            .iter()
            .filter(|(_, d)| {
                paths
                    .iter()
                    .any(|p| d.patterns.iter().any(|pat| glob_match(pat, p)))
            })
            .map(|(name, _)| name.clone())
            .collect();

        for name in &命中 {
            if let Some(d) = self.dormant.remove(name) {
                self.activated_names.insert(name.clone());
                self.activated.insert(name.clone(), d.skill);
            }
        }
        命中
    }

    /// 取一个已激活的技能。
    pub fn activated(&self, name: &str) -> Option<&SkillMetadata> {
        self.activated.get(name)
    }

    /// 全部已激活的技能，按名字排序。
    pub fn all_activated(&self) -> Vec<&SkillMetadata> {
        self.activated.values().collect()
    }

    /// 还有几个在休眠。
    pub fn dormant_count(&self) -> usize {
        self.dormant.len()
    }

    /// 清空休眠表（重新加载技能目录时用）。
    ///
    /// **已激活的名字保留**：否则重新加载一次，模型就看着一个技能凭空消失了。
    pub fn clear_dormant(&mut self) {
        self.dormant.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn 技能(name: &str, paths: &str) -> SkillMetadata {
        let fm = if paths.is_empty() {
            String::new()
        } else {
            format!("paths: \"{paths}\"\n")
        };
        let (p, _) = super::super::parse_pack(&format!("---\n{fm}---\n正文"));
        super::super::parse_skill_fields(&p.frontmatter, &p.content, name)
    }

    #[test]
    fn 没有_paths_的技能是无条件的() {
        let mut c = ConditionalSkills::new();
        let out = c.partition(vec![技能("普通", "")]);
        assert_eq!(out.len(), 1);
        assert_eq!(c.dormant_count(), 0);
    }

    #[test]
    fn 有_paths_的技能先休眠() {
        // 装了三十个技能、其中二十个针对特定文件类型时，全量塞进系统提示
        // 是纯浪费。
        let mut c = ConditionalSkills::new();
        let out = c.partition(vec![技能("rust 专用", "*.rs")]);
        assert!(out.is_empty(), "不该进无条件那堆");
        assert_eq!(c.dormant_count(), 1);
        assert!(c.all_activated().is_empty());
    }

    #[test]
    fn 碰到匹配的文件才激活() {
        let mut c = ConditionalSkills::new();
        c.partition(vec![技能("rust 专用", "**/*.rs")]);

        assert!(c.activate_for_paths(&["README.md"]).is_empty(), "不该被无关文件唤醒");
        assert_eq!(c.activate_for_paths(&["src/main.rs"]), ["rust 专用"]);
        assert_eq!(c.dormant_count(), 0);
        assert!(c.activated("rust 专用").is_some());
    }

    #[test]
    fn 花括号展开过的多条_paths_任一命中即可() {
        let mut c = ConditionalSkills::new();
        c.partition(vec![技能("前端", "**/*.{ts,tsx}")]);
        assert_eq!(c.activate_for_paths(&["app/x.tsx"]), ["前端"]);
    }

    #[test]
    fn 激活顺序确定() {
        // 上游用 HashMap 且明说顺序不保证。不确定的顺序会让同一段历史
        // 两次重放得到不同的 trajectory。
        let mut c = ConditionalSkills::new();
        c.partition(vec![
            技能("z", "*.rs"),
            技能("a", "*.rs"),
            技能("m", "*.rs"),
        ]);
        assert_eq!(c.activate_for_paths(&["x.rs"]), ["a", "m", "z"]);
    }

    #[test]
    fn 已激活的技能跨重载保留() {
        // 否则重新加载一次技能目录，模型就看着一个技能凭空消失了。
        let mut c = ConditionalSkills::new();
        c.partition(vec![技能("s", "*.rs")]);
        c.activate_for_paths(&["a.rs"]);

        c.clear_dormant();
        let out = c.partition(vec![技能("s", "*.rs")]);
        assert_eq!(out.len(), 1, "重载后它该按无条件处理，而不是退回休眠");
        assert_eq!(c.dormant_count(), 0);
    }

    #[test]
    fn 激活过一次就不再重复报() {
        let mut c = ConditionalSkills::new();
        c.partition(vec![技能("s", "*.rs")]);
        assert_eq!(c.activate_for_paths(&["a.rs"]), ["s"]);
        // 第二次同样的路径不该再报一次——调用方会据此发通知。
        assert!(c.activate_for_paths(&["a.rs"]).is_empty());
    }

    #[test]
    fn 没有休眠技能时是空操作() {
        let mut c = ConditionalSkills::new();
        assert!(c.activate_for_paths(&["a.rs", "b.rs"]).is_empty());
    }

    #[test]
    fn 与_glob_工具用同一套匹配语义() {
        // 两份实现迟早会在 `*` 跨不跨 `/` 这类地方分叉，而那时用户会问
        // "为什么 paths: 和 Glob 匹配得不一样"。
        let mut c = ConditionalSkills::new();
        c.partition(vec![技能("s", "src/*.rs")]);
        // 单星不跨目录——与 Glob 工具一致。
        assert!(c.activate_for_paths(&["src/deep/x.rs"]).is_empty());
        assert_eq!(c.activate_for_paths(&["src/x.rs"]), ["s"]);
    }
}
