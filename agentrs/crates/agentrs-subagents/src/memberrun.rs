//! 协作式 `MemberRun` 的单调派生规则。
//!
//! MemberRun 是 Core 编排的平级 Run：不设置 `parent_run_id`，不继承 checkpoint，
//! 也不携带创建者 epoch。宿主启动它时必须分配自己的 epoch。

use std::collections::BTreeSet;

use agentrs_contracts::authority::{CapabilityView, PermissionMode};
use agentrs_contracts::ids::{MemberId, RunId, TeamId};
use agentrs_contracts::spec::{MemberRunSpec, RunSpec};

/// MemberRun 派生失败。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MemberDeriveError {
    /// 派生深度超过授权上限。
    #[error("member depth {depth} exceeds max_depth {max}")]
    TooDeep {
        /// 请求深度。
        depth: u16,
        /// 授权上限。
        max: u16,
    },
    /// 新 Run 与创建者使用了相同 id。
    #[error("member must have a distinct run id")]
    SameRunId,
    /// 请求的某类能力与创建者当前能力没有交集。
    #[error("member requests unavailable capabilities: {field}")]
    NoAvailableCapability {
        /// 能力字段。
        field: &'static str,
    },
}

/// 创建成员时刻的父状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberParent {
    /// 创建者 Run id。
    pub run_id: RunId,
    /// 创建者规格。
    pub spec: RunSpec,
    /// 创建者当前能力，而不是最初授权上界。
    pub capabilities: CapabilityView,
    /// 创建者当前派生深度。
    pub depth: u16,
}

/// Core 发起的成员派生请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberRequest {
    /// 成员 id。
    pub member_id: MemberId,
    /// 团队协调域 id。
    pub team_id: TeamId,
    /// 成员自己的 Run id。
    pub run_id: RunId,
    /// 可选工具子集。
    pub tools: Option<Vec<String>>,
    /// 可选 provider 子集。
    pub providers: Option<Vec<agentrs_contracts::ids::ProviderId>>,
    /// 可选模型子集。
    pub models: Option<Vec<agentrs_contracts::ids::ModelId>>,
    /// 可选的更严格权限模式。
    pub permission_mode: Option<PermissionMode>,
}

impl MemberRequest {
    /// 创建一个默认继承父当前能力的请求。
    pub fn new(member_id: impl Into<MemberId>, team_id: impl Into<TeamId>, run_id: impl Into<RunId>) -> Self {
        Self {
            member_id: member_id.into(),
            team_id: team_id.into(),
            run_id: run_id.into(),
            tools: None,
            providers: None,
            models: None,
            permission_mode: None,
        }
    }
}

/// 派生一个能力只能收窄的平级 MemberRun。
pub fn derive_member(
    parent: &MemberParent,
    request: &MemberRequest,
) -> Result<MemberRunSpec, MemberDeriveError> {
    if request.run_id == parent.run_id {
        return Err(MemberDeriveError::SameRunId);
    }
    let depth = parent.depth.saturating_add(1);
    if depth > parent.spec.authority.max_depth {
        return Err(MemberDeriveError::TooDeep {
            depth,
            max: parent.spec.authority.max_depth,
        });
    }

    let tools = narrow(&parent.capabilities.tools, request.tools.as_deref(), "tools")?;
    let providers = narrow(
        &parent.capabilities.providers,
        request.providers.as_deref(),
        "providers",
    )?;
    let models = narrow(&parent.capabilities.models, request.models.as_deref(), "models")?;
    let permission_mode = match &request.permission_mode {
        Some(mode) if mode.is_at_least_as_strict_as(&parent.spec.permission_mode) => mode.clone(),
        _ => parent.spec.permission_mode.clone(),
    };

    let mut spec = parent.spec.clone();
    spec.run_id = request.run_id.clone();
    // MemberRun 是平级关系；团队关系由 team_id 表达。
    spec.parent_run_id = None;
    spec.checkpoint = None;
    spec.permission_mode = permission_mode;
    spec.authority.tools = tools.clone();
    spec.authority.providers = providers.clone();
    spec.authority.models = models.clone();
    spec.initial_capabilities = CapabilityView {
        tools,
        providers: providers.clone(),
        models: models.clone(),
    };
    spec.model_policy
        .providers
        .retain(|provider| providers.contains(provider));
    spec.model_policy.fallback.retain(|model| models.contains(model));
    spec.model_policy.tiers.retain(|_, model| models.contains(model));

    Ok(MemberRunSpec {
        member_id: request.member_id.clone(),
        team_id: request.team_id.clone(),
        depth,
        spec,
    })
}

fn narrow<T>(parent: &[T], requested: Option<&[T]>, field: &'static str) -> Result<Vec<T>, MemberDeriveError>
where
    T: Clone + Ord,
{
    let Some(requested) = requested else {
        return Ok(parent.to_vec());
    };
    let available: BTreeSet<&T> = parent.iter().collect();
    let narrowed: Vec<T> = requested
        .iter()
        .filter(|item| available.contains(item))
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if !requested.is_empty() && narrowed.is_empty() {
        Err(MemberDeriveError::NoAvailableCapability { field })
    } else {
        Ok(narrowed)
    }
}

#[cfg(test)]
mod tests {
    use agentrs_contracts::authority::AuthorityEnvelope;
    use agentrs_contracts::spec::{
        ContextBudget, ConversationSnapshot, ExecutionBudget, ModelPolicy, SystemContext,
    };
    use agentrs_contracts::version::SpecVersion;

    use super::*;

    fn parent() -> MemberParent {
        let capabilities = CapabilityView {
            tools: vec!["Read".into(), "Write".into()],
            providers: vec!["p".into()],
            models: vec!["m".into()],
        };
        MemberParent {
            run_id: "parent".into(),
            capabilities: capabilities.clone(),
            depth: 0,
            spec: RunSpec {
                run_id: "parent".into(),
                parent_run_id: Some("irrelevant".into()),
                conversation: ConversationSnapshot::default(),
                system_context: SystemContext::default(),
                authority: AuthorityEnvelope {
                    id: "a".into(),
                    workspaces: vec!["w".into()],
                    tools: capabilities.tools.clone(),
                    providers: capabilities.providers.clone(),
                    models: capabilities.models.clone(),
                    max_depth: 2,
                },
                initial_capabilities: capabilities,
                permission_mode: PermissionMode::Default,
                model_policy: ModelPolicy {
                    tiers: [(agentrs_contracts::spec::ModelTier::Default, "m".into())]
                        .into_iter()
                        .collect(),
                    fallback: vec![],
                    providers: vec!["p".into()],
                    max_retries: 1,
                    allow_attachments: false,
                },
                context_budget: ContextBudget {
                    max_input_tokens: 100,
                    reserved_output_tokens: 10,
                    compaction_threshold_pct: 80,
                },
                execution_budget: ExecutionBudget::default(),
                change_set_id: None,
                checkpoint: Some(agentrs_contracts::spec::RunCheckpoint {
                    spec_version: SpecVersion(1),
                    up_to_seq: agentrs_contracts::ids::EventSequence(1),
                    pending_approval: None,
                }),
                spec_version: SpecVersion(1),
                config: Default::default(),
            },
        }
    }

    #[test]
    fn member_是平级新_run_且不继承_checkpoint() {
        let member = derive_member(&parent(), &MemberRequest::new("member", "team", "child")).unwrap();
        assert_eq!(member.spec.run_id, "child".into());
        assert_eq!(member.spec.parent_run_id, None);
        assert_eq!(member.spec.checkpoint, None);
        assert_eq!(member.depth, 1);
    }

    #[test]
    fn 请求能力只能与父当前视图取交集() {
        let mut request = MemberRequest::new("member", "team", "child");
        request.tools = Some(vec!["Write".into(), "Shell".into()]);
        let member = derive_member(&parent(), &request).unwrap();
        assert_eq!(member.spec.initial_capabilities.tools, ["Write"]);
        assert_eq!(member.spec.authority.tools, ["Write"]);
    }

    #[test]
    fn 完全不可用的能力与超深度都拒绝() {
        let mut request = MemberRequest::new("member", "team", "child");
        request.models = Some(vec!["unknown".into()]);
        assert!(matches!(
            derive_member(&parent(), &request),
            Err(MemberDeriveError::NoAvailableCapability { field: "models" })
        ));
        let mut too_deep = parent();
        too_deep.depth = 2;
        assert!(matches!(
            derive_member(&too_deep, &MemberRequest::new("member", "team", "child")),
            Err(MemberDeriveError::TooDeep { .. })
        ));
    }

    #[test]
    fn 更宽权限会降级为父权限() {
        let mut p = parent();
        p.spec.permission_mode = PermissionMode::Plan;
        let mut request = MemberRequest::new("member", "team", "child");
        request.permission_mode = Some(PermissionMode::Default);
        let member = derive_member(&p, &request).unwrap();
        assert_eq!(member.spec.permission_mode, PermissionMode::Plan);
    }
}
