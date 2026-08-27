//! 授权、能力视图与权限模式（架构 §4.1）。
//!
//! 两条轴严格正交，不可互相替代（架构 §5.1）：
//!
//! | 轴 | 管什么 | 载体 |
//! |---|---|---|
//! | Scope | 可见性与生命周期 | [`crate::ids::ScopeId`] + owner |
//! | Authority | 权限 | [`AuthorityEnvelope`] + [`PermissionMode`] + grant |
//!
//! **子 Run 的 scope 更窄不等于它权限更小。**

use serde::{Deserialize, Serialize};

use crate::ids::{AuthorityEnvelopeId, Digest, ModelId, ProviderId};

/// Core 签发的授权上界。**Run 内不可扩大。**
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorityEnvelope {
    /// 信封标识，进入 `OperationView` 供审计。
    pub id: AuthorityEnvelopeId,
    /// 允许的工作区。工作区身份不能在 Run 内原地切换。
    pub workspaces: Vec<String>,
    /// 允许的工具名上界。
    pub tools: Vec<String>,
    /// 允许的 provider 上界。
    pub providers: Vec<ProviderId>,
    /// 允许的模型上界。
    pub models: Vec<ModelId>,
    /// 子运行的最大派生深度。超过即拒绝，不静默截断。
    pub max_depth: u16,
}

impl AuthorityEnvelope {
    /// 判断 `other` 是否为本信封的子集。派生子运行时必须成立。
    pub fn contains(&self, other: &AuthorityEnvelope) -> bool {
        fn subset<T: PartialEq>(inner: &[T], outer: &[T]) -> bool {
            inner.iter().all(|x| outer.contains(x))
        }
        subset(&other.workspaces, &self.workspaces)
            && subset(&other.tools, &self.tools)
            && subset(&other.providers, &self.providers)
            && subset(&other.models, &self.models)
            && other.max_depth <= self.max_depth
    }
}

/// 上界内当前实际可用的能力集合。可以收缩、失效，或在安全边界换代。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityView {
    /// 当前可见的工具名。
    pub tools: Vec<String>,
    /// 当前可用的 provider。
    pub providers: Vec<ProviderId>,
    /// 当前可用的模型。
    pub models: Vec<ModelId>,
}

/// 能力视图的摘要。
///
/// **P0 形态：单一 digest 覆盖 provider + tool catalog + prompt + middleware。**
/// 不变式只有一条：*一次 operation 内 digest 不变*——一条断言即可验证。
///
/// P1 出现真实换代需求时，在保持本字段不变的前提下追加 generation 元组；
/// digest 是向前兼容的收缩投影，旧事件仍可解析（架构 §4.1）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CapabilityViewDigest(pub Digest);

/// 能力的可用状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    /// 可用。
    Available,
    /// 已撤回。in-flight 调用返回 `CapabilityWithdrawn` 并回灌模型。
    Withdrawn,
}

/// Run 级权限模式。**不是提示词约定，也不能由函数式子 Agent 表达**（架构 §4.1.2）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "mode")]
pub enum PermissionMode {
    /// 只读探索：仅 Info 类工具，禁止任何非只读调用。
    Plan,
    /// 常规：按 Policy 逐次裁决。
    Default,
    /// Core 已就本 Run 预授权某类操作，仍受 `AuthorityEnvelope` 与 Sandbox 约束。
    Accepted {
        /// 已预授权的范围。
        scopes: Vec<String>,
    },
}

impl PermissionMode {
    /// 严格度序：`Plan` 最严，`Accepted` 最宽。
    fn strictness(&self) -> u8 {
        match self {
            Self::Plan => 0,
            Self::Default => 1,
            Self::Accepted { .. } => 2,
        }
    }

    /// 判断 `self` 是否不宽于 `other`。派生子运行时必须成立。
    pub fn is_at_least_as_strict_as(&self, other: &Self) -> bool {
        self.strictness() <= other.strictness()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn 信封(tools: &[&str], depth: u16) -> AuthorityEnvelope {
        AuthorityEnvelope {
            id: "env-1".into(),
            workspaces: vec!["ws".into()],
            tools: tools.iter().map(|s| (*s).to_owned()).collect(),
            providers: vec!["p".into()],
            models: vec!["m".into()],
            max_depth: depth,
        }
    }

    #[test]
    fn 子信封必须是子集() {
        let parent = 信封(&["Read", "Write"], 3);
        assert!(parent.contains(&信封(&["Read"], 2)));
        assert!(
            !parent.contains(&信封(&["Read", "Exec"], 2)),
            "多出的工具必须被拒绝"
        );
        assert!(!parent.contains(&信封(&["Read"], 4)), "更深的递归必须被拒绝");
    }

    #[test]
    fn 权限模式只能单调收窄() {
        let plan = PermissionMode::Plan;
        let default = PermissionMode::Default;
        let accepted = PermissionMode::Accepted { scopes: vec![] };

        assert!(plan.is_at_least_as_strict_as(&default));
        assert!(plan.is_at_least_as_strict_as(&accepted));
        assert!(
            !default.is_at_least_as_strict_as(&plan),
            "Default 比 Plan 宽，不能作为其子模式"
        );
        assert!(!accepted.is_at_least_as_strict_as(&default));
    }

    #[test]
    fn scope_不参与授权判定() {
        // 本测试是文档性的：AuthorityEnvelope 上没有任何 ScopeId 字段。
        // 若将来有人试图加，这个断言所在的模块注释会提醒他两轴必须正交。
        let json = serde_json::to_string(&信封(&["Read"], 1)).unwrap();
        assert!(!json.contains("scope"), "Authority 不得携带 scope：{json}");
    }
}
