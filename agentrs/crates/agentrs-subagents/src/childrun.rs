//! `ChildRun` 派生规则（架构 §11.1、§11.3.1，内核不变量 5）。
//!
//! > 子 Agent 的工具、模型、`PermissionMode` 和通信能力**只能收窄**。
//!
//! ## 这是"只能收紧"原则的第四次出现
//!
//! 前三次分别是单调 guard（只有 `Deny`/`Abstain`）、Hook（只有
//! `Proceed`/`Advise`/`Block`）、技能的 `ContextModifier`（取下确界）。
//! 每一次的落点不同，但判据同一条：**任何派生出来的东西都不该比它的来源更宽**。
//!
//! ## 与 `MemberRun` 分清
//!
//! | | `ChildRun`（本模块） | `MemberRun`（Phase D） |
//! |---|---|---|
//! | 所有权 | parent-owned | 平级，归 Core 编排 |
//! | Epoch | **共享父的** | 自己的 |
//! | 生命周期 | 不超过发起它的 operation | 可比父活得久 |
//! | 通信 | 只回一个 `SubagentSummary` | 双向 `ExternalFact` |
//!
//! 共享 epoch 这一条**只对函数式子 Agent 成立**：它活不过一个 operation，
//! 父被围栏时它跟着倒是对的。协作式成员跨父的多个 turn，
//! 父 Run 结束了它还可能在跑——让它跟着父的 epoch 倒就错了。
//!
//! ## 深度上限拒绝而不截断
//!
//! 超过 `max_depth` 时**直接拒绝**，不是"截到上限继续跑"。
//! 静默截断会让一个写错的递归编排看起来在正常工作，
//! 而它实际上少做了最里面那几层。

use agentrs_contracts::authority::{AuthorityEnvelope, CapabilityView, PermissionMode};
use agentrs_contracts::ids::{OperationId, RunEpoch, RunId};
use agentrs_contracts::spec::{ChildRunSpec, RunSpec};

/// 派生失败。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeriveError {
    /// 超过 `AuthorityEnvelope::max_depth`。
    ///
    /// **拒绝而不是截断**：静默截断会让写错的递归编排看起来在正常工作。
    #[error("child depth {depth} exceeds max_depth {max}")]
    TooDeep {
        /// 请求的深度。
        depth: u16,
        /// 上限。
        max: u16,
    },
    /// 请求的能力超出父的 `CapabilityView`。
    #[error("child requests capabilities the parent does not have: {field}")]
    NotASubset {
        /// 哪一项。
        field: &'static str,
    },
    /// 子运行 id 与父相同。
    #[error("child must have a distinct run id")]
    SameRunId,
}

/// 父 Run 在派生时刻的状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parent {
    /// 父 Run 标识。
    pub run_id: RunId,
    /// 父的 epoch。**子共享它。**
    pub epoch: RunEpoch,
    /// 父的授权上界。
    pub authority: AuthorityEnvelope,
    /// 父**当下**的能力视图。
    ///
    /// 注意是 `CapabilityView` 而不是 `AuthorityEnvelope`——
    /// 子能拿到的上界是父**此刻实际可用的**，不是父被签发时的上界。
    /// 父自己已经收窄过的东西，子不该拿回去。
    pub capabilities: CapabilityView,
    /// 父的权限模式。
    pub permission_mode: PermissionMode,
    /// 父的派生深度。
    pub depth: u16,
}

/// 子运行的请求。每一项都是**收窄意图**，`None` 表示照搬父的。
///
/// **没有 `Default`**：`child_run_id` 与 `operation_id` 都必须显式给出。
/// 给它们一个默认值意味着"忘了填"会静默变成一个空 id 的子运行，
/// 而那在 trajectory 里根本查不出是谁派生的。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildRequest {
    /// 子运行标识。
    pub child_run_id: RunId,
    /// 发起它的 operation。
    pub operation_id: OperationId,
    /// 请求的工具子集。
    pub tools: Option<Vec<String>>,
    /// 请求的模型子集。
    pub models: Option<Vec<agentrs_contracts::ids::ModelId>>,
    /// 请求的权限模式。
    pub permission_mode: Option<PermissionMode>,
}

impl ChildRequest {
    /// 最小请求：只给必填的两项，其余照搬父的。
    pub fn new(child_run_id: impl Into<RunId>, operation_id: impl Into<OperationId>) -> Self {
        Self {
            child_run_id: child_run_id.into(),
            operation_id: operation_id.into(),
            tools: None,
            models: None,
            permission_mode: None,
        }
    }
}

/// 从父派生一个 `ChildRunSpec`。
///
/// **全部收窄在这里发生**，调用方拿到的 spec 已经不可能比父宽。
pub fn derive(
    parent: &Parent,
    parent_spec: &RunSpec,
    req: &ChildRequest,
) -> Result<ChildRunSpec, DeriveError> {
    if req.child_run_id == parent.run_id {
        return Err(DeriveError::SameRunId);
    }

    // 深度：父 + 1，超上界直接拒。
    let depth = parent.depth.saturating_add(1);
    if depth > parent.authority.max_depth {
        return Err(DeriveError::TooDeep {
            depth,
            max: parent.authority.max_depth,
        });
    }

    // 工具 / 模型：取交集。请求里父没有的部分**不是报错而是被过滤掉**——
    // 与技能的 `ContextModifier` 同一取法。但若过滤后为空而请求非空，
    // 那说明子想要的全都拿不到，此时报错更有用：静默给一个零工具的子运行，
    // 它会在第一次尝试调用时才失败，而那时已经花了一次模型请求。
    let tools = narrow(&parent.capabilities.tools, req.tools.as_deref(), "tools")?;
    let models = narrow_models(&parent.capabilities.models, req.models.as_deref())?;

    // 权限模式：只能更严。请求更宽时**降级到父模式**而不是报错——
    // 与 `permission::inherit` 同一处理：子拿到的权限少于它要的，
    // 这是安全方向；报错会让一个写错的编排整体失败。
    let mode = match &req.permission_mode {
        Some(m) if m.is_at_least_as_strict_as(&parent.permission_mode) => m.clone(),
        _ => parent.permission_mode.clone(),
    };

    // 子的授权上界 = 父视图收窄后的结果，**不是父的信封**。
    let authority = AuthorityEnvelope {
        id: parent.authority.id.clone(),
        workspaces: parent.authority.workspaces.clone(),
        tools: tools.clone(),
        providers: parent.capabilities.providers.clone(),
        models: models.clone(),
        // 剩余深度：子还能再派生几层。
        max_depth: parent.authority.max_depth,
    };

    let mut spec = parent_spec.clone();
    spec.run_id = req.child_run_id.clone();
    spec.parent_run_id = Some(parent.run_id.clone());
    spec.authority = authority;
    spec.initial_capabilities = CapabilityView {
        tools,
        providers: parent.capabilities.providers.clone(),
        models,
    };
    spec.permission_mode = mode;
    // **不继承 checkpoint**：子运行是新的，父的挂起状态与它无关。
    spec.checkpoint = None;

    Ok(ChildRunSpec {
        child_run_id: req.child_run_id.clone(),
        parent_run_id: parent.run_id.clone(),
        // **共享父的 epoch**：父被围栏时子一并收敛。
        epoch: parent.epoch,
        operation_id: req.operation_id.clone(),
        depth,
        spec,
    })
}

fn narrow(
    parent: &[String],
    want: Option<&[String]>,
    field: &'static str,
) -> Result<Vec<String>, DeriveError> {
    let Some(want) = want else {
        return Ok(parent.to_vec());
    };
    let out: Vec<String> = parent.iter().filter(|t| want.contains(t)).cloned().collect();
    if out.is_empty() && !want.is_empty() {
        return Err(DeriveError::NotASubset { field });
    }
    Ok(out)
}

fn narrow_models(
    parent: &[agentrs_contracts::ids::ModelId],
    want: Option<&[agentrs_contracts::ids::ModelId]>,
) -> Result<Vec<agentrs_contracts::ids::ModelId>, DeriveError> {
    let Some(want) = want else {
        return Ok(parent.to_vec());
    };
    let out: Vec<_> = parent.iter().filter(|m| want.contains(m)).cloned().collect();
    if out.is_empty() && !want.is_empty() {
        return Err(DeriveError::NotASubset { field: "models" });
    }
    Ok(out)
}

/// 判断 `child` 是否确实不比 `parent` 宽。
///
/// **派生的全部承诺就是这一条**，供测试与调试断言使用。
pub fn is_no_wider(child: &ChildRunSpec, parent: &Parent) -> bool {
    let c = &child.spec.initial_capabilities;
    c.tools.iter().all(|t| parent.capabilities.tools.contains(t))
        && c.models.iter().all(|m| parent.capabilities.models.contains(m))
        && c.providers
            .iter()
            .all(|p| parent.capabilities.providers.contains(p))
        && child
            .spec
            .permission_mode
            .is_at_least_as_strict_as(&parent.permission_mode)
        && child.depth > parent.depth
}

#[cfg(test)]
mod tests;
