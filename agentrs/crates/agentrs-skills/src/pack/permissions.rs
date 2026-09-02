// Ported from aionrs (Apache-2.0), crates/aion-skills.
//   Source: crates/aion-skills/src/permissions.rs @ f711174
//   Copied: 2026-09-01   Modified: yes
//   Changes: `Prefix` 规则改为**要求前缀以分隔符结尾**（上游把 `db*` 也存成
//            `Prefix("db")`，于是它会命中 `database`——文档说要防的正是这件事，
//            实现却没防住）；理由文本改中文；`auto_approve` 改名 `assume_yes`
//            并在文档里写清它绕不过 deny。

//! 技能的放行判定。
//!
//! 一个技能可以声明 `allowed-tools` 与 `hooks`——**前者挑工具子集，后者能跑
//! 任意 shell**。所以"这个技能能不能直接跑"不是一个可以默认为是的问题。
//!
//! 判定链五步，顺序即优先级：
//!
//! 1. `deny` 命中 → [`SkillPermission::Deny`]，**`assume_yes` 也绕不过**；
//! 2. `allow` 命中 → 放行；
//! 3. 既没有 hooks 也没有 allowed-tools → 放行（它什么额外权限也没要）；
//! 4. `assume_yes` → 放行；
//! 5. 其余 → [`SkillPermission::Ask`]，带上**为什么**要问。

use super::types::SkillMetadata;

/// 一条技能名匹配规则。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionRule {
    /// 精确匹配：`commit` 只命中 `commit`。
    Exact(String),
    /// 前缀匹配：`db:*` 存成 `Prefix("db:")`。
    ///
    /// **冒号留着**，否则 `db:*` 会命中 `database`——一条本想放行某个命名空间的
    /// 规则，顺手放行了一个毫不相干的技能。
    Prefix(String),
}

/// 前缀规则要求前缀以其中之一结尾，`*` 才当通配符用。
const 分隔符: [char; 3] = [':', '/', '-'];

impl PermissionRule {
    /// 解析一条规则。
    ///
    /// `db:*` → 前缀；`commit` → 精确。**`db*` 也当精确**：
    /// 一个不带分隔符的裸前缀命中面太宽，而写规则的人多半只是想匹配一个名字。
    pub fn parse(rule: &str) -> Self {
        match rule.strip_suffix('*') {
            Some(prefix) if prefix.ends_with(分隔符) => Self::Prefix(prefix.to_string()),
            _ => Self::Exact(rule.to_string()),
        }
    }

    /// 这条规则命中这个技能名吗？
    pub fn matches(&self, name: &str) -> bool {
        match self {
            Self::Exact(s) => s == name,
            Self::Prefix(p) => name.starts_with(p.as_str()),
        }
    }
}

/// 一次判定的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillPermission {
    /// 放行。
    Allow,
    /// 被配置拒绝。**`assume_yes` 也绕不过。**
    Deny,
    /// 需要人点头，并说明为什么。
    Ask {
        /// 给人看的理由。
        reason: String,
    },
}

/// 技能放行判定器。
#[derive(Debug, Clone)]
pub struct SkillPermissionChecker {
    deny_rules: Vec<PermissionRule>,
    allow_rules: Vec<PermissionRule>,
    assume_yes: bool,
}

impl SkillPermissionChecker {
    /// 从 deny/allow 两张表构造。
    ///
    /// `assume_yes` 把第 5 步的 `Ask` 变成 `Allow`，**但绕不过第 1 步的
    /// `Deny`**——否则一个"图省事"的开关就能关掉唯一的硬拦截。
    pub fn new(deny: Vec<String>, allow: Vec<String>, assume_yes: bool) -> Self {
        Self {
            deny_rules: deny.iter().map(|s| PermissionRule::parse(s)).collect(),
            allow_rules: allow.iter().map(|s| PermissionRule::parse(s)).collect(),
            assume_yes,
        }
    }

    /// 跑五步判定链。
    pub fn check(&self, skill: &SkillMetadata) -> SkillPermission {
        let name = &skill.name;

        if self.deny_rules.iter().any(|r| r.matches(name)) {
            return SkillPermission::Deny;
        }
        if self.allow_rules.iter().any(|r| r.matches(name)) {
            return SkillPermission::Allow;
        }
        // 什么额外权限也没要的技能不必问。
        if skill.hooks_raw.is_none() && skill.allowed_tools.is_empty() {
            return SkillPermission::Allow;
        }
        if self.assume_yes {
            return SkillPermission::Allow;
        }
        SkillPermission::Ask {
            reason: ask_reason(skill),
        }
    }
}

/// 为什么要问。
///
/// 说清是哪一样触发的：人据此决定要不要看一眼那个技能文件。
/// 一句"需要确认"给不出这个判断。
fn ask_reason(skill: &SkillMetadata) -> String {
    match (skill.hooks_raw.is_some(), !skill.allowed_tools.is_empty()) {
        (true, true) => format!(
            "技能 `{}` 同时声明了 hooks 与 allowed-tools：前者能跑任意 shell 命令，\
             后者会改变可用工具集",
            skill.name
        ),
        (true, false) => format!("技能 `{}` 声明了 hooks，它能跑任意 shell 命令", skill.name),
        (false, true) => format!(
            "技能 `{}` 声明了 allowed-tools（{}），会改变可用工具集",
            skill.name,
            skill.allowed_tools.join("、")
        ),
        // 走不到：第 3 步已经放行了。留着是为了这个函数在任何输入下都有答案。
        (false, false) => format!("技能 `{}` 需要确认", skill.name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn 技能(name: &str, tools: &[&str], hooks: bool) -> SkillMetadata {
        let (parsed, _) = super::super::parse_pack("---\n---\n正文");
        let mut m = super::super::parse_skill_fields(&parsed.frontmatter, &parsed.content, name);
        m.allowed_tools = tools.iter().map(|s| s.to_string()).collect();
        m.hooks_raw = hooks.then(|| serde_json::json!({"pre": []}));
        m
    }

    #[test]
    fn 精确与前缀规则() {
        assert_eq!(PermissionRule::parse("commit"), PermissionRule::Exact("commit".into()));
        assert_eq!(PermissionRule::parse("db:*"), PermissionRule::Prefix("db:".into()));
        assert!(PermissionRule::parse("db:*").matches("db:migrate"));
        assert!(!PermissionRule::parse("db:*").matches("db"));
    }

    #[test]
    fn 前缀规则不会漏到隔壁命名空间() {
        // 这是这条规则存在的全部理由。上游把 `db*` 也存成 `Prefix("db")`，
        // 于是一条本想放行 `db:` 命名空间的规则顺手放行了 `database`。
        assert!(!PermissionRule::parse("db:*").matches("database"));
        // 不带分隔符的裸前缀当精确匹配——命中面太宽的规则多半是笔误。
        assert_eq!(PermissionRule::parse("db*"), PermissionRule::Exact("db*".into()));
        assert!(!PermissionRule::parse("db*").matches("database"));
    }

    #[test]
    fn deny_最先且_assume_yes_绕不过() {
        // 否则一个"图省事"的开关就能关掉唯一的硬拦截。
        let c = SkillPermissionChecker::new(vec!["危险".into()], vec![], true);
        assert_eq!(c.check(&技能("危险", &["Bash"], true)), SkillPermission::Deny);
        // deny 与 allow 同时命中时，deny 赢。
        let c = SkillPermissionChecker::new(vec!["x".into()], vec!["x".into()], false);
        assert_eq!(c.check(&技能("x", &[], false)), SkillPermission::Deny);
    }

    #[test]
    fn 什么权限也没要的技能不必问() {
        let c = SkillPermissionChecker::new(vec![], vec![], false);
        assert_eq!(c.check(&技能("无害", &[], false)), SkillPermission::Allow);
    }

    #[test]
    fn 声明了_hooks_或工具就要问_并说清是哪一样() {
        // 人据此决定要不要看一眼那个技能文件。一句"需要确认"给不出这个判断。
        let c = SkillPermissionChecker::new(vec![], vec![], false);

        let SkillPermission::Ask { reason } = c.check(&技能("h", &[], true)) else {
            panic!("声明了 hooks 就该问");
        };
        assert!(reason.contains("hooks"), "{reason}");
        assert!(reason.contains("shell"), "要说清 hooks 危险在哪：{reason}");

        let SkillPermission::Ask { reason } = c.check(&技能("t", &["Bash", "Write"], false)) else {
            panic!("声明了 allowed-tools 就该问");
        };
        assert!(reason.contains("Bash、Write"), "要列出是哪些工具：{reason}");

        let SkillPermission::Ask { reason } = c.check(&技能("both", &["Bash"], true)) else {
            panic!("两样都有更该问");
        };
        assert!(reason.contains("hooks") && reason.contains("allowed-tools"), "{reason}");
    }

    #[test]
    fn 显式_allow_跳过后面的判定() {
        let c = SkillPermissionChecker::new(vec![], vec!["信任的".into()], false);
        assert_eq!(c.check(&技能("信任的", &["Bash"], true)), SkillPermission::Allow);
    }

    #[test]
    fn assume_yes_把问变成放行() {
        let c = SkillPermissionChecker::new(vec![], vec![], true);
        assert_eq!(c.check(&技能("要权限的", &["Bash"], true)), SkillPermission::Allow);
    }
}
