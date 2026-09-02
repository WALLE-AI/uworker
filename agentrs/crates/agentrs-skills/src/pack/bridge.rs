// Ported from aionrs (Apache-2.0), crates/aion-skills.
//   Source: crates/aion-skills/src/context_modifier.rs @ f711174
//   Copied: 2026-09-01   Modified: yes
//   Changes: 本仓库的 `ContextModifier` 形状不同——它只表达**收窄**
//            （tool_subset / max_input_tokens / permission_mode / max_depth），
//            没有 model / effort 字段。所以 `allowed_tools` 映到 `tool_subset`，
//            而 model / effort 单独返回：它们是模型选择，归 `ModelPolicy`，
//            混进收窄合并里会让"技能只能收窄不能提权"这条不再成立。

//! 把解析好的技能接到 [`crate::modifier`] 的收窄合并上。
//!
//! 这是 [`pack`](super) 与本 crate 原有部分之间唯一的接缝。
//!
//! # 为什么 model / effort 不进 `ContextModifier`
//!
//! `ContextModifier` 的每一个字段都是**单调收窄**的：工具取交集、预算取最小、
//! 权限模式只在更严时生效。合并算法依赖这条性质——它是"技能不是权限插件"
//! 的机器化表达。
//!
//! 而 `model: claude-opus-4` 不是收窄，它是**换一个**。把它塞进同一个结构里，
//! 合并语义立刻含糊：两个技能各指定一个模型，取哪个？"更严格"对模型没有定义。
//! 所以它单独出来，由调用方按 `ModelPolicy` 的规则处置。

use std::collections::BTreeSet;

use super::types::{EffortLevel, SkillMetadata};
use crate::modifier::ContextModifier;

/// 一个技能想改变什么。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkillOverrides {
    /// 能力收窄。`None` 表示这个技能不收窄任何东西。
    pub narrowing: Option<ContextModifier>,
    /// 想换的模型。**不是收窄**，见模块文档。
    pub model: Option<String>,
    /// 想要的推理强度。同样不是收窄。
    pub effort: Option<EffortLevel>,
}

impl SkillOverrides {
    /// 这个技能什么也不想改吗？
    pub fn is_empty(&self) -> bool {
        self.narrowing.is_none() && self.model.is_none() && self.effort.is_none()
    }
}

/// 从技能元数据里读出它想改变的东西。
pub fn overrides_of(skill: &SkillMetadata) -> SkillOverrides {
    // 空的 allowed-tools 表示"不限制"，不是"一个工具也不给"。
    // 反过来理解的话，一个只想换模型的技能会把工具集清空，
    // 然后模型在那一轮什么也做不了。
    let narrowing = (!skill.allowed_tools.is_empty()).then(|| ContextModifier {
        tool_subset: Some(skill.allowed_tools.iter().cloned().collect::<BTreeSet<_>>()),
        max_input_tokens: None,
        permission_mode: None,
        max_depth: None,
    });

    SkillOverrides {
        narrowing,
        model: skill.model.clone(),
        effort: skill.effort,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::{parse_pack, parse_skill_fields};

    fn 技能(frontmatter: &str) -> SkillMetadata {
        let (p, _) = parse_pack(&format!("---\n{frontmatter}\n---\n正文"));
        parse_skill_fields(&p.frontmatter, &p.content, "s")
    }

    #[test]
    fn 什么也没声明的技能不改变任何东西() {
        let o = overrides_of(&技能("description: 只是一段提示"));
        assert!(o.is_empty(), "{o:?}");
    }

    #[test]
    fn allowed_tools_变成工具子集() {
        let o = overrides_of(&技能("allowed-tools: [Read, Grep]"));
        let n = o.narrowing.expect("该有收窄");
        assert_eq!(
            n.tool_subset,
            Some(["Read", "Grep"].iter().map(|s| s.to_string()).collect())
        );
        // 只收窄工具，其余三项不表态——不表态与"设成 0"是两回事。
        assert_eq!(n.max_input_tokens, None);
        assert_eq!(n.permission_mode, None);
        assert_eq!(n.max_depth, None);
    }

    #[test]
    fn 空的_allowed_tools_表示不限制而不是一个都不给() {
        // 反过来理解的话，一个只想换模型的技能会把工具集清空，
        // 然后模型在那一轮什么也做不了。
        let o = overrides_of(&技能("model: some-model"));
        assert!(o.narrowing.is_none(), "{o:?}");
        assert_eq!(o.model.as_deref(), Some("some-model"));
    }

    #[test]
    fn model_与_effort_不进收窄结构() {
        // 它们不是收窄，是"换一个"。混进去的话，两个技能各指定一个模型时，
        // "更严格"对模型没有定义，合并语义就含糊了。
        let o = overrides_of(&技能("model: m\neffort: high\nallowed-tools: Read"));
        assert_eq!(o.model.as_deref(), Some("m"));
        assert_eq!(o.effort, Some(EffortLevel::High));
        let n = o.narrowing.unwrap();
        assert_eq!(n.tool_subset.unwrap().len(), 1);
    }

    #[test]
    fn inherit_的模型不算一次覆盖() {
        let o = overrides_of(&技能("model: inherit"));
        assert!(o.is_empty(), "inherit 的意思就是别改：{o:?}");
    }

    #[test]
    fn 收窄结果能直接喂给合并() {
        // 这是这个桥存在的理由：解析出来的东西要能接上原有的合并算法。
        let o = overrides_of(&技能("allowed-tools: [Read]"));
        let view = crate::modifier::ContextView {
            tools: ["Read", "Write"].iter().map(|s| s.to_string()).collect(),
            max_input_tokens: 1000,
            permission_mode: agentrs_contracts::authority::PermissionMode::Default,
            max_depth: 2,
        };
        let merged = crate::modifier::merge_all(&view, &[o.narrowing.unwrap()]);
        assert_eq!(merged.view.tools, ["Read"].iter().map(|s| s.to_string()).collect());
        assert!(!merged.narrowings.is_empty(), "收窄要留痕");
    }
}
