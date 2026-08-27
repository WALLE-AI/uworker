//! `PermissionMode` 与 Plan Mode（架构 §4.1.2）。
//!
//! `PermissionMode` 是 **Run 级一等状态**，不是提示词约定。这句话的实际含义是
//! 下面五条规则各自都有代码在执行，而不是写在系统提示里请模型配合。
//!
//! | 规则 | 由谁执行 |
//! |---|---|
//! | 1. 工具目录与系统提示分段**必须一致变化** | [`project`]：两者从同一个 mode 推导，结构上无法分叉 |
//! | 2. 切换只在 turn 边界，写 durable 事件，是缓存前缀断点 | [`propose_transition`] 的 `at_turn_boundary` 判据 |
//! | 3. 模型只能**提议**切换，裁决归 Policy/用户 | [`TransitionSource`] 没有"模型"这个来源 |
//! | 4. `Plan → Default` 需显式审批；`→ Plan` 无条件 | [`propose_transition`] 的放宽/收窄分支 |
//! | 5. ChildRun 只能更严格 | [`inherit`] |
//!
//! ## 为什么规则 1 要靠结构而不是靠纪律
//!
//! 目录里有 `Write`、提示里说"当前只读"，模型会提出注定被拒的调用；
//! 反过来提示里说可以写、目录里没有 `Write`，模型会反复尝试并困惑。
//! 两者都是**投影**，从同一个 mode 一次推出，就不存在"改了一个忘了另一个"。
//!
//! ## 纵深防御
//!
//! [`ModeGuard`] 在固定管线里再挡一次。目录过滤是"模型看不见"，
//! guard 是"看见了也调不动"——**前者防误导，后者防绕过**。
//! 只做前者的话，一次历史重放里残留的旧 `tool_use` 就能穿过去。

use std::sync::Arc;

use agentrs_contracts::authority::PermissionMode;
use agentrs_contracts::policy::{DecisionSource, DenyCode};
use agentrs_types::ToolDef;

use crate::toolround::{GuardVerdict, ProposedCall, ToolGuard};

/// 由 mode 一次推出的两个投影。
#[derive(Debug, Clone, PartialEq)]
pub struct ModeProjection {
    /// 模型可见的工具目录。
    pub catalog: Vec<ToolDef>,
    /// 追加到系统提示的分段。
    pub system_section: String,
}

/// Plan 模式的系统提示分段。
///
/// 正文来自 `agentrs-prompts` 的注册表——**提示词集中在一处**，
/// 才能对它施加版本号、snapshot 与安全 lint。
fn plan_section() -> String {
    agentrs_prompts::registry::PLAN_MODE
        .render_static()
        .expect("PLAN_MODE 无输入")
}

/// `Accepted` 模式的预授权说明。
fn accepted_section(scopes: &[String]) -> String {
    agentrs_prompts::registry::ACCEPTED_MODE
        .render(&[("scopes", scopes.join("、"))].into_iter().collect())
        .expect("ACCEPTED_MODE 的输入声明与模板必须一致（registry 测试保证）")
}

/// 从 mode 推出工具目录与系统提示分段。**两者必须同源。**
pub fn project(mode: &PermissionMode, full_catalog: &[ToolDef]) -> ModeProjection {
    match mode {
        PermissionMode::Plan => ModeProjection {
            // 写类工具不进目录——模型看不见，就不会提出注定被拒的调用。
            catalog: full_catalog
                .iter()
                .filter(|t| t.is_read_only())
                .cloned()
                .collect(),
            system_section: plan_section(),
        },
        PermissionMode::Default => ModeProjection {
            catalog: full_catalog.to_vec(),
            system_section: String::new(),
        },
        PermissionMode::Accepted { scopes } => ModeProjection {
            catalog: full_catalog.to_vec(),
            system_section: if scopes.is_empty() {
                String::new()
            } else {
                accepted_section(scopes)
            },
        },
    }
}

// ---------------------------------------------------------------------------
// 模式切换
// ---------------------------------------------------------------------------

/// 谁在要求切换。
///
/// **没有"模型"这个来源。** 模型只能通过 `ExitPlanMode` 之类的工具*提议*，
/// 那条提议走完整的固定管线，最终由 Policy 裁出一个 [`TransitionSource::User`]
/// 或 [`TransitionSource::Core`]。若这里允许模型直接切模式，
/// Plan 模式就退化成一句提示词请求。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionSource {
    /// 用户显式裁决。
    User,
    /// Core 依策略切换。
    Core,
}

impl TransitionSource {
    /// 从 Policy 的裁决来源转换。
    pub fn from_decision(src: &DecisionSource) -> Self {
        match src {
            DecisionSource::Human { .. } => Self::User,
            DecisionSource::Policy { .. } => Self::Core,
        }
    }
}

/// 切换请求的裁定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransitionVerdict {
    /// 生效。调用方必须写 `PermissionModeChanged` 并把它计为缓存前缀断点。
    Accepted,
    /// 拒绝。
    Rejected(TransitionReject),
}

/// 拒绝原因。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransitionReject {
    /// 不在 turn 边界上。
    #[error("mode may only change at a turn boundary")]
    NotAtTurnBoundary,
    /// 放宽权限但没有用户裁决。
    #[error("widening requires an explicit user decision")]
    WideningNeedsUser,
    /// 目标模式超出 `AuthorityEnvelope` 允许的上界。
    #[error("target mode exceeds the run ceiling")]
    ExceedsCeiling,
    /// 与当前模式相同。
    #[error("mode unchanged")]
    NoChange,
}

/// 裁定一次模式切换。
///
/// `ceiling` 是本 Run 允许的最宽模式（由 `AuthorityEnvelope` 决定）。
/// 切换**永远不能突破它**——否则模式就成了绕过信封的后门。
pub fn propose_transition(
    from: &PermissionMode,
    to: &PermissionMode,
    source: TransitionSource,
    at_turn_boundary: bool,
    ceiling: &PermissionMode,
) -> TransitionVerdict {
    use TransitionReject as R;

    if from == to {
        return TransitionVerdict::Rejected(R::NoChange);
    }

    // 规则 2：Step 中途换模式意味着同一个 Turn 里前后两次请求的
    // 系统提示与工具目录不一致，缓存前缀断在中间，历史也自相矛盾。
    if !at_turn_boundary {
        return TransitionVerdict::Rejected(R::NotAtTurnBoundary);
    }

    // 上界优先于一切——包括用户当场的裁决。
    // 信封是 Core 签发的、Run 内不可扩大（架构 §4.1）。
    if !to.is_at_least_as_strict_as(ceiling) {
        return TransitionVerdict::Rejected(R::ExceedsCeiling);
    }

    // 规则 4：收窄无条件放行，放宽必须有用户。
    let 收窄 = to.is_at_least_as_strict_as(from);
    if 收窄 {
        return TransitionVerdict::Accepted;
    }
    match source {
        TransitionSource::User => TransitionVerdict::Accepted,
        // Core 可以自行收窄，但**不能自行放宽**——那等于策略给自己提权。
        TransitionSource::Core => TransitionVerdict::Rejected(R::WideningNeedsUser),
    }
}

/// 规则 5：ChildRun 继承父 Run 的模式且只能更严格。
///
/// `requested` 为 `None` 表示直接继承。
pub fn inherit(parent: &PermissionMode, requested: Option<&PermissionMode>) -> PermissionMode {
    match requested {
        None => parent.clone(),
        Some(r) if r.is_at_least_as_strict_as(parent) => r.clone(),
        // 请求更宽 —— **收窄到父模式而不是报错**：子运行拿到的权限
        // 少于它要的，这是安全的降级；报错会让一个写错的编排整体失败。
        Some(_) => parent.clone(),
    }
}

// ---------------------------------------------------------------------------
// 固定管线里的 guard
// ---------------------------------------------------------------------------

/// 按 `PermissionMode` 收紧的单调 guard。
///
/// 它只会 `Deny`/`Abstain`——**放行从来不是 guard 的决定**（内核不变量 15）。
pub struct ModeGuard {
    mode: PermissionMode,
    catalog: Vec<ToolDef>,
}

impl ModeGuard {
    /// 用当前模式与**完整**工具目录构造。
    ///
    /// 传完整目录而不是投影后的目录：guard 要能识别"这个工具存在但当前模式
    /// 不允许"，从而给出 `PermissionMode` 而不是含糊的"未知工具"。
    pub fn new(mode: PermissionMode, catalog: Vec<ToolDef>) -> Self {
        Self { mode, catalog }
    }

    /// 包成可直接放进 `ToolRoundDeps::guards` 的形态。
    pub fn shared(mode: PermissionMode, catalog: Vec<ToolDef>) -> Arc<dyn ToolGuard> {
        Arc::new(Self::new(mode, catalog))
    }
}

impl ToolGuard for ModeGuard {
    fn name(&self) -> &str {
        "permission-mode"
    }

    fn check(&self, call: &ProposedCall) -> GuardVerdict {
        let PermissionMode::Plan = self.mode else {
            // Default / Accepted 下模式本身不额外收紧；放行与否由 Policy 决定。
            return GuardVerdict::Abstain;
        };

        let 只读 = self
            .catalog
            .iter()
            .find(|t| t.name == call.tool_name)
            .map(|t| t.is_read_only())
            // 目录里没有 —— fail-closed，当作会改东西。
            .unwrap_or(false);

        if 只读 {
            GuardVerdict::Abstain
        } else {
            GuardVerdict::Deny {
                code: DenyCode::PermissionMode,
                message: format!("{} 在只读探索模式下不可用", call.tool_name),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn 目录() -> Vec<ToolDef> {
        vec![
            ToolDef::read_only("Read", "读", json!({})),
            ToolDef::read_only("Grep", "搜", json!({})),
            ToolDef::mutating("Write", "写", json!({})),
            ToolDef::mutating("Bash", "跑命令", json!({})),
        ]
    }

    fn 调用(name: &str) -> ProposedCall {
        ProposedCall {
            call_id: "c1".into(),
            tool_name: name.to_owned(),
            arguments: json!({}),
        }
    }

    fn 已接受(scopes: &[&str]) -> PermissionMode {
        PermissionMode::Accepted {
            scopes: scopes.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    // ---- 规则 1：两个投影同源 ----

    #[test]
    fn plan_模式过滤掉写类工具() {
        let p = project(&PermissionMode::Plan, &目录());
        let names: Vec<&str> = p.catalog.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["Read", "Grep"]);
    }

    #[test]
    fn plan_模式的提示与目录一致() {
        // 目录里没有写类工具，提示就必须说明这一点——否则模型会反复尝试并困惑。
        let p = project(&PermissionMode::Plan, &目录());
        assert!(!p.system_section.is_empty(), "Plan 模式必须有对应的提示分段");
        assert!(p.catalog.iter().all(|t| t.is_read_only()));
    }

    #[test]
    fn 目录与提示不可能各自为政() {
        // 遍历三种模式：**只要目录被裁剪过，提示就必须非空**。
        // 这条断言就是"两者必须一致变化"的可执行形式。
        let full = 目录();
        for mode in [
            PermissionMode::Plan,
            PermissionMode::Default,
            已接受(&["写工作区"]),
        ] {
            let p = project(&mode, &full);
            let 被裁剪 = p.catalog.len() < full.len();
            if 被裁剪 {
                assert!(
                    !p.system_section.is_empty(),
                    "{mode:?}: 目录裁剪了却没有对应提示，模型会反复尝试拿不到的工具"
                );
            }
        }
    }

    #[test]
    fn default_模式不裁剪目录也不加提示() {
        let p = project(&PermissionMode::Default, &目录());
        assert_eq!(p.catalog.len(), 4);
        assert!(p.system_section.is_empty());
    }

    #[test]
    fn accepted_模式在提示里列出预授权范围() {
        let p = project(&已接受(&["写工作区", "跑测试"]), &目录());
        assert_eq!(p.catalog.len(), 4, "预授权不改变目录，只改变审批频次");
        assert!(p.system_section.contains("写工作区"));
        assert!(p.system_section.contains("跑测试"));
    }

    #[test]
    fn 未声明_effect_的工具在_plan_下被过滤() {
        // fail-closed：第三方注册没写 effect，不能因此在只读模式下畅通无阻。
        let 未声明: ToolDef =
            serde_json::from_str(r#"{"name":"X","description":"d","parameters":{}}"#).unwrap();
        let p = project(&PermissionMode::Plan, &[未声明]);
        assert!(p.catalog.is_empty());
    }

    // ---- 规则 3 与 4：切换裁定 ----

    #[test]
    fn 收窄无条件放行() {
        // Default → Plan，任何来源都行。
        for src in [TransitionSource::User, TransitionSource::Core] {
            assert_eq!(
                propose_transition(
                    &PermissionMode::Default,
                    &PermissionMode::Plan,
                    src,
                    true,
                    &已接受(&[])
                ),
                TransitionVerdict::Accepted,
                "{src:?} 应能无条件收窄"
            );
        }
    }

    #[test]
    fn 放宽必须有用户裁决() {
        // Plan → Default 是 Plan Mode 的收敛点，必须是一次显式审批。
        assert_eq!(
            propose_transition(
                &PermissionMode::Plan,
                &PermissionMode::Default,
                TransitionSource::User,
                true,
                &已接受(&[])
            ),
            TransitionVerdict::Accepted
        );
        assert_eq!(
            propose_transition(
                &PermissionMode::Plan,
                &PermissionMode::Default,
                TransitionSource::Core,
                true,
                &已接受(&[])
            ),
            TransitionVerdict::Rejected(TransitionReject::WideningNeedsUser),
            "策略不能给自己提权"
        );
    }

    #[test]
    fn 切换来源里没有模型这一项() {
        // 这条是类型层面的：DecisionSource 只有 Human/Policy，
        // TransitionSource 只有 User/Core。模型只能提议，提议走固定管线。
        assert_eq!(
            TransitionSource::from_decision(&DecisionSource::Human { user_id: "u1".into() }),
            TransitionSource::User
        );
        assert_eq!(
            TransitionSource::from_decision(&DecisionSource::Policy {
                policy_id: "p1".into()
            }),
            TransitionSource::Core
        );
    }

    #[test]
    fn turn_中途不得切换() {
        // Step 中途换模式 = 同一 Turn 内前后两次请求的提示与目录不一致，
        // 缓存前缀断在中间，历史也自相矛盾。
        assert_eq!(
            propose_transition(
                &PermissionMode::Default,
                &PermissionMode::Plan,
                TransitionSource::User,
                false,
                &已接受(&[])
            ),
            TransitionVerdict::Rejected(TransitionReject::NotAtTurnBoundary)
        );
    }

    #[test]
    fn 上界优先于用户当场的裁决() {
        // 信封是 Core 签发、Run 内不可扩大的。用户在 Run 内说了也不算，
        // 否则模式就成了绕过信封的后门。
        assert_eq!(
            propose_transition(
                &PermissionMode::Plan,
                &已接受(&["一切"]),
                TransitionSource::User,
                true,
                // 本 Run 最宽只到 Default
                &PermissionMode::Default,
            ),
            TransitionVerdict::Rejected(TransitionReject::ExceedsCeiling)
        );
    }

    #[test]
    fn 上界之内的放宽仍需用户() {
        // 两条判据独立：在上界内 ≠ 可以自动放宽。
        assert_eq!(
            propose_transition(
                &PermissionMode::Plan,
                &PermissionMode::Default,
                TransitionSource::Core,
                true,
                &PermissionMode::Default,
            ),
            TransitionVerdict::Rejected(TransitionReject::WideningNeedsUser)
        );
    }

    #[test]
    fn 同模式切换报无变化而不是默默通过() {
        // 默默通过会写出一条无意义的 PermissionModeChanged，
        // 而那条事件是缓存前缀断点——白白丢一次缓存。
        assert_eq!(
            propose_transition(
                &PermissionMode::Default,
                &PermissionMode::Default,
                TransitionSource::User,
                true,
                &已接受(&[])
            ),
            TransitionVerdict::Rejected(TransitionReject::NoChange)
        );
    }

    // ---- 规则 5：子运行继承 ----

    #[test]
    fn 子运行默认继承父模式() {
        assert_eq!(inherit(&PermissionMode::Plan, None), PermissionMode::Plan);
    }

    #[test]
    fn 子运行可以更严格() {
        assert_eq!(
            inherit(&PermissionMode::Default, Some(&PermissionMode::Plan)),
            PermissionMode::Plan
        );
    }

    #[test]
    fn 子运行请求更宽时降级为父模式() {
        // 降级而不是报错：子运行拿到的权限少于它要的，这是安全方向；
        // 报错会让一个写错的编排整体失败。
        assert_eq!(
            inherit(&PermissionMode::Plan, Some(&PermissionMode::Default)),
            PermissionMode::Plan
        );
        assert_eq!(
            inherit(&PermissionMode::Default, Some(&已接受(&["一切"]))),
            PermissionMode::Default
        );
    }

    // ---- 纵深防御：guard ----

    #[test]
    fn guard_在_plan_下拒绝写类工具() {
        let g = ModeGuard::new(PermissionMode::Plan, 目录());
        match g.check(&调用("Write")) {
            GuardVerdict::Deny { code, .. } => assert_eq!(code, DenyCode::PermissionMode),
            other => panic!("期望 Deny，得到 {other:?}"),
        }
    }

    #[test]
    fn guard_在_plan_下放过只读工具() {
        let g = ModeGuard::new(PermissionMode::Plan, 目录());
        assert_eq!(g.check(&调用("Read")), GuardVerdict::Abstain);
    }

    #[test]
    fn guard_挡住目录之外的工具() {
        // 历史重放里残留的旧 tool_use、或已被撤销的工具。
        let g = ModeGuard::new(PermissionMode::Plan, 目录());
        assert!(matches!(g.check(&调用("Mystery")), GuardVerdict::Deny { .. }));
    }

    #[test]
    fn guard_在非_plan_模式下一律弃权() {
        // 弃权 ≠ 放行。是否放行由 Policy 决定——guard 只负责收紧。
        for mode in [PermissionMode::Default, 已接受(&["一切"])] {
            let g = ModeGuard::new(mode, 目录());
            assert_eq!(g.check(&调用("Write")), GuardVerdict::Abstain);
            assert_eq!(g.check(&调用("Mystery")), GuardVerdict::Abstain);
        }
    }

    #[test]
    fn 目录过滤与_guard_是两道独立防线() {
        // 目录过滤防"模型被误导"，guard 防"绕过目录"。
        // 只做前者的话，历史里残留的旧 tool_use 就能穿过去。
        let 投影 = project(&PermissionMode::Plan, &目录());
        assert!(!投影.catalog.iter().any(|t| t.name == "Write"));

        let g = ModeGuard::new(PermissionMode::Plan, 目录());
        assert!(matches!(g.check(&调用("Write")), GuardVerdict::Deny { .. }));
    }
}
