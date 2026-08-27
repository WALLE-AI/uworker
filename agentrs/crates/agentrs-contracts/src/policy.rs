//! 策略裁决、SandboxGrant 与审批（架构 §4.2、§6.2）。
//!
//! **grant 由 `PolicyEnforcer` 签发，内核只搬运不铸造。**四条规则：
//!
//! 1. 一次性消费——一个 grant 只能进入一次 `execute`，重复使用是内部错误而非重试路径；
//! 2. 输入绑定——`bound_input_hash` 覆盖 `工具名 + 规范化参数 + workspace + change_set`，
//!    Sandbox 侧必须**独立复核**（宿主义务 H1）；
//! 3. 过期即失效——恢复时读到过期 grant 不得直接执行，走重新裁决；
//! 4. 不可派生——ChildRun/MemberRun 不复用父 grant。

use serde::{Deserialize, Serialize};

use crate::ids::{
    ApprovalToken, ChangeSetId, Deadline, Digest, MemberId, StepId, TeamId, Timestamp, ToolCallId,
};

/// 工具调用输入的指纹。
///
/// 覆盖 `tool_name + 规范化参数 + workspace_id + change_set_id`——
/// 同一命令在不同 ChangeSet 上是不同的意图，reconcile 时不可混淆（架构 §8.2）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InputHash(pub Digest);

/// 模型提出的工具调用，尚未经过任何裁决。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolProposal {
    /// provider 侧的调用 id。
    pub call_id: ToolCallId,
    /// 工具名。
    pub tool_name: String,
    /// 已通过 schema 校验并规范化的参数。
    pub arguments: serde_json::Value,
    /// 目标工作区。
    pub workspace_id: String,
    /// 目标 ChangeSet。
    pub change_set_id: ChangeSetId,
    /// 输入指纹。
    pub input_hash: InputHash,
}

/// 沙箱执行授权。内核视其载荷为**不透明**，只负责搬运与一次性消费记账。
///
/// 签名与校验方式由 Core + Sandbox 约定（跨团队议题 Q1）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxGrant {
    /// grant 标识，内核以此做一次性消费记账。
    pub grant_id: String,
    /// 不透明载荷（可含签名）。内核不解析。
    pub payload: serde_json::Value,
}

/// 策略裁决结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "decision")]
pub enum PolicyDecision {
    /// 允许，并签发一次性 grant。
    Allow {
        /// 授权凭证。
        grant: SandboxGrant,
        /// grant 绑定的输入指纹，Sandbox 必须独立复核。
        bound_input_hash: InputHash,
        /// 过期时刻。过期未执行则作废，必须重新裁决。
        expires_at: Timestamp,
    },
    /// 需要人工审批。
    RequireApproval(ApprovalRequest),
    /// 拒绝。结构化回灌模型，不是异常。
    Deny {
        /// 稳定拒绝码。
        code: DenyCode,
        /// 已脱敏的说明。
        message: String,
    },
}

/// 稳定的拒绝码。用户可读文案与 i18n 归 Core（架构 §1.1 裁定 5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DenyCode {
    /// 超出 `AuthorityEnvelope`。
    OutOfAuthority,
    /// 当前 `PermissionMode` 不允许。
    PermissionMode,
    /// 被单调 guard 拒绝。
    GuardDenied,
    /// 被 Hook 否决。
    HookBlocked,
    /// 用户在审批中拒绝。
    UserRejected,
    /// 预算耗尽。
    BudgetExhausted,
}

/// 审批请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRequest {
    /// 关联的 Step。
    pub step_id: StepId,
    /// 待批准的提议。
    pub proposal: ToolProposal,
    /// 风险摘要，供 UI 展示。
    pub risk_summary: String,
    /// 发起该请求的团队成员。仅供 UI 归组展示——**裁决方永远是人**。
    pub originating_member: Option<MemberId>,
    /// 所属团队。
    pub team_id: Option<TeamId>,
}

/// 审批裁决的来源。
///
/// **内核不变量 15：任何 agent 不得成为 `ApprovalDecision` 的来源。**
/// 成员触发的审批一律归人类裁决，禁止模型给模型批准（架构 §11.3.5）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "source")]
pub enum DecisionSource {
    /// 人类用户。这是唯一合法来源。
    Human {
        /// 用户标识。
        user_id: String,
    },
    /// 组织策略自动裁决（由 Core 代表人类预先配置，非 agent 决定）。
    Policy {
        /// 策略标识。
        policy_id: String,
    },
}

/// 审批裁决。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalDecision {
    /// 是否放行。
    pub allowed: bool,
    /// 裁决来源。内核会拒绝任何非 [`DecisionSource`] 变体的来源。
    pub source: DecisionSource,
    /// 放行时随附的 grant。
    pub grant: Option<SandboxGrant>,
    /// 裁决时刻。
    pub decided_at: Timestamp,
}

/// 有界等待的结果（架构 §6.2）。
///
/// **绝不允许无限期阻塞**：阻塞期间 Run 的全部 live resource 驻留内存，
/// 桌面场景下用户离开一晚等价于句柄与内存泄漏。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum ApprovalOutcome {
    /// 在 deadline 内得到裁决。
    Decided(ApprovalDecision),
    /// 到达 deadline 仍无人裁决 —— 写 checkpoint 并挂起为 `RunNeedsUserAction`。
    Pending {
        /// 恢复令牌。内核视其为不透明值，保管与唤醒归 Core。
        resume_token: ApprovalToken,
    },
}

/// 策略端口的错误。
#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    /// 裁决方不可达。
    #[error("policy enforcer unavailable")]
    Unavailable,
    /// 裁决来源非法——例如来源被标记为 agent。
    #[error("illegal decision source: agents must not authorize")]
    IllegalDecisionSource,
    /// 其他已脱敏错误。
    #[error("policy error: {message}")]
    Other {
        /// 已脱敏的错误描述。
        message: String,
    },
}

/// 审批等待的截止时刻计算入参。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalDeadline {
    /// 实际生效的截止时刻 = min(approval_timeout, 剩余 ExecutionBudget)。
    pub at: Deadline,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 裁决来源只有人类与组织策略两种() {
        // 类型层面就没有 Agent 变体——内核不变量 15 由类型保证，不靠运行时检查。
        let json = serde_json::to_string(&DecisionSource::Human { user_id: "u1".into() }).unwrap();
        assert!(json.contains("\"source\":\"human\""), "{json}");

        let variants = ["human", "policy"];
        for v in variants {
            let probe = format!("{{\"source\":\"{v}\",\"user_id\":\"u\",\"policy_id\":\"p\"}}");
            assert!(
                serde_json::from_str::<DecisionSource>(&probe).is_ok(),
                "已知变体 {v} 应可反序列化"
            );
        }
        let agent = "{\"source\":\"agent\",\"agent_id\":\"a1\"}";
        assert!(
            serde_json::from_str::<DecisionSource>(agent).is_err(),
            "agent 来源必须无法构造"
        );
    }

    #[test]
    fn allow_必须同时携带_grant_与绑定指纹() {
        // 类型上无法构造"只有 grant 没有 hash"的 Allow。
        let d = PolicyDecision::Allow {
            grant: SandboxGrant {
                grant_id: "g1".into(),
                payload: serde_json::json!({}),
            },
            bound_input_hash: InputHash(Digest::from_hex("h")),
            expires_at: Timestamp(0),
        };
        let json = serde_json::to_string(&d).unwrap();
        assert!(json.contains("bound_input_hash"), "{json}");
        assert!(json.contains("expires_at"), "{json}");
    }

    #[test]
    fn pending_携带恢复令牌以便挂起后继续() {
        let o = ApprovalOutcome::Pending {
            resume_token: "tok".into(),
        };
        let json = serde_json::to_string(&o).unwrap();
        assert!(json.contains("resume_token"), "{json}");
    }
}
