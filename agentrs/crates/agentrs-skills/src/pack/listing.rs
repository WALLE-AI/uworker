// Ported from aionrs (Apache-2.0), crates/aion-skills.
//   Source: crates/aion-skills/src/prompt.rs @ f711174
//   Copied: 2026-09-01   Modified: yes
//   Changes: `SkillSource::Bundled` 换成调用方传入的 `pinned` 名字集——
//            "哪些技能优先"是优先级，不是发现来源，而发现归 Core；
//            预算改为显式传入字符数，不再从 context_window 反算（token 账本
//            在 agentrs-context，这里不该再有第二套换算）；截断统一走一个
//            函数，上游两处各写了一遍且 `>=` / `-1` 的边界不一致。

//! 把技能列表排进一份预算之内。
//!
//! 技能清单要进系统提示，而它会随装的技能数线性增长。不设上限的话，装了三十个
//! 技能的人会发现每一轮都先烧掉几千 token 才开始干活。
//!
//! 三级降级，**优先保住 `pinned` 那批**：
//!
//! 1. 全量：所有技能带完整描述；
//! 2. 截断：`pinned` 保持完整，其余的描述按剩余预算均分后截断；
//! 3. 极简：`pinned` 保持完整，其余**只留名字**。
//!
//! 名字永远不截——一个被截断的名字调不出来，那比没有它更糟。

use std::collections::BTreeSet;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::types::SkillMetadata;

/// 单条描述的宽度上限。
pub const MAX_LISTING_DESC: usize = 250;
/// 描述短于这个宽度就不值得留了，直接退到极简模式。
const MIN_DESC_WIDTH: usize = 20;
/// 没给预算时用这个。约等于 200k 上下文的 1%，按 4 字符 1 token 折算。
pub const DEFAULT_CHAR_BUDGET: usize = 8_000;

/// 按显示宽度截断，末尾补省略号。
///
/// 一个函数而不是两处各写一遍：上游那两处的 `>=` 与 `-1` 边界不一致，
/// 于是同一条描述在两种降级模式下会被截在不同的位置。
fn truncate_to_width(text: &str, limit: usize) -> String {
    if UnicodeWidthStr::width(text) <= limit {
        return text.to_string();
    }
    // 给省略号留一格。
    let 可用 = limit.saturating_sub(1);
    let mut out = String::new();
    let mut w = 0usize;
    for ch in text.chars() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw > 可用 {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('…');
    out
}

/// 一个技能在清单里的描述：`description`，有 `when-to-use` 时接在后面。
pub fn format_skill_description(skill: &SkillMetadata) -> String {
    let desc = match &skill.when_to_use {
        Some(w) if !w.is_empty() => format!("{} - {}", skill.description, w),
        _ => skill.description.clone(),
    };
    truncate_to_width(&desc, MAX_LISTING_DESC)
}

/// 一条清单项：`- name: description`。
pub fn format_skill_entry(skill: &SkillMetadata) -> String {
    format!("- {}: {}", skill.name, format_skill_description(skill))
}

/// 把技能列表排进 `budget` 个字符宽度之内。
///
/// `pinned` 里的名字保持完整描述——它们通常是内置技能，模型总该看得见。
pub fn format_within_budget(
    skills: &[SkillMetadata],
    pinned: &BTreeSet<String>,
    budget: usize,
) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let 全量: Vec<String> = skills.iter().map(format_skill_entry).collect();
    let 宽度 = |s: &str| UnicodeWidthStr::width(s);
    // N 条之间有 N-1 个换行。
    let 全量宽度: usize =
        全量.iter().map(|e| 宽度(e)).sum::<usize>() + 全量.len().saturating_sub(1);

    // ---- 一级：全放得下 ----
    if 全量宽度 <= budget {
        return 全量.join("\n");
    }

    let 要保的: Vec<usize> = (0..skills.len())
        .filter(|&i| pinned.contains(&skills[i].name))
        .collect();
    let 可截的: Vec<usize> = (0..skills.len())
        .filter(|&i| !pinned.contains(&skills[i].name))
        .collect();

    // 全都是要保的，那就没有可截的余地——超预算也照给。
    // 硬截 pinned 会让内置技能的描述残缺，那比超一点预算糟。
    if 可截的.is_empty() {
        return 全量.join("\n");
    }

    let 保住的宽度: usize = 要保的.iter().map(|&i| 宽度(&全量[i]) + 1).sum();
    let 剩余 = budget.saturating_sub(保住的宽度);
    // 名字与 `- ` `: ` 四个字符是省不掉的开销。
    let 名字开销: usize = 可截的
        .iter()
        .map(|&i| 宽度(&skills[i].name) + 4)
        .sum::<usize>()
        + 可截的.len().saturating_sub(1);
    let 每条描述 = 剩余.saturating_sub(名字开销) / 可截的.len();

    // ---- 三级：连 20 格都分不到，只留名字 ----
    if 每条描述 < MIN_DESC_WIDTH {
        return skills
            .iter()
            .enumerate()
            .map(|(i, s)| {
                if pinned.contains(&s.name) {
                    全量[i].clone()
                } else {
                    // 名字永远不截：截断的名字调不出来。
                    format!("- {}", s.name)
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
    }

    // ---- 二级：按均分的额度截描述 ----
    skills
        .iter()
        .enumerate()
        .map(|(i, s)| {
            if pinned.contains(&s.name) {
                return 全量[i].clone();
            }
            let desc = truncate_to_width(&format_skill_description(s), 每条描述);
            format!("- {}: {}", s.name, desc)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn 技能(name: &str, desc: &str) -> SkillMetadata {
        let (p, _) = super::super::parse_pack(&format!("---\ndescription: {desc}\n---\n正文"));
        super::super::parse_skill_fields(&p.frontmatter, &p.content, name)
    }

    fn 无() -> BTreeSet<String> {
        BTreeSet::new()
    }

    #[test]
    fn 放得下就全量给出() {
        let list = [技能("a", "做甲事"), 技能("b", "做乙事")];
        assert_eq!(
            format_within_budget(&list, &无(), 1000),
            "- a: 做甲事\n- b: 做乙事"
        );
    }

    #[test]
    fn 空列表给空串() {
        assert_eq!(format_within_budget(&[], &无(), 100), "");
    }

    #[test]
    fn when_to_use_接在描述后面() {
        let (p, _) = super::super::parse_pack(
            "---\ndescription: 做甲事\nwhen-to-use: 需要甲的时候\n---\n",
        );
        let s = super::super::parse_skill_fields(&p.frontmatter, &p.content, "a");
        assert_eq!(format_skill_entry(&s), "- a: 做甲事 - 需要甲的时候");
    }

    #[test]
    fn 预算紧张时截描述但名字完整() {
        let list: Vec<SkillMetadata> = (0..6)
            .map(|n| 技能(&format!("skill{n}"), &"很长的描述".repeat(20)))
            .collect();
        let out = format_within_budget(&list, &无(), 400);
        for n in 0..6 {
            // 名字永远不截：截断的名字调不出来，那比没有它更糟。
            assert!(out.contains(&format!("skill{n}")), "名字丢了：{out}");
        }
        assert!(out.contains('…'), "该截的没截：{out}");
    }

    #[test]
    fn 预算极紧时退到只剩名字() {
        let list: Vec<SkillMetadata> = (0..20)
            .map(|n| 技能(&format!("skill{n}"), &"描述".repeat(50)))
            .collect();
        let out = format_within_budget(&list, &无(), 200);
        assert!(out.starts_with("- skill0\n- skill1"), "{out}");
        assert!(!out.contains(':'), "极简模式不该有描述：{out}");
    }

    #[test]
    fn pinned_的技能在降级时保持完整() {
        // 内置技能模型总该看得见，不该和第三十个技能一起被截。
        let list: Vec<SkillMetadata> = (0..20)
            .map(|n| 技能(&format!("skill{n}"), &"描述".repeat(50)))
            .collect();
        let mut pinned = BTreeSet::new();
        pinned.insert("skill0".to_string());
        let out = format_within_budget(&list, &pinned, 200);
        let 第一行 = out.lines().next().unwrap();
        assert!(第一行.starts_with("- skill0: 描述描述"), "pinned 被截了：{第一行}");
        assert!(out.contains("\n- skill5\n"), "其余该退到只剩名字：{out}");
    }

    #[test]
    fn 全是_pinned_时超预算也照给() {
        // 硬截 pinned 会让内置技能的描述残缺，那比超一点预算糟。
        let list = [技能("a", &"长".repeat(200)), 技能("b", &"长".repeat(200))];
        let pinned: BTreeSet<String> = ["a", "b"].iter().map(|s| s.to_string()).collect();
        let out = format_within_budget(&list, &pinned, 10);
        assert!(out.contains("- a: 长长长"), "{}", &out[..40]);
        assert!(out.contains("- b: 长长长"));
    }

    #[test]
    fn 描述超过单条上限时被截() {
        let s = 技能("a", &"字".repeat(400));
        let d = format_skill_description(&s);
        assert!(UnicodeWidthStr::width(d.as_str()) <= MAX_LISTING_DESC, "{}", d.len());
        assert!(d.ends_with('…'));
    }

    #[test]
    fn 截断按显示宽度算_中文占两格() {
        // 按字符数算的话，一行中文的实际宽度是预算的两倍，清单会撑出边界。
        assert_eq!(truncate_to_width("中文中文", 5), "中文…");
        assert_eq!(truncate_to_width("abcd", 10), "abcd");
    }

    #[test]
    fn 截断在任何上限下都不会崩() {
        for limit in 0..6 {
            let _ = truncate_to_width("中文abc", limit);
        }
    }
}
