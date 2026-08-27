//! 开发用策略：显式 allowlist + 可选交互式审批。
//!
//! **不存储 `allow always`**——授权状态归宿主，内核与本 adapter 都不持久化它
//! （架构 §1.5.4 明确拒绝把 aionrs 的 `confirm.rs` 那套搬进来）。
//!
//! 每次放行都签发一个**新的、一次性的** grant，并把绑定指纹交给 Sandbox
//! 独立持有——这正是 H1 要求的"不信任调用方"。

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use agentrs_contracts::ids::{ApprovalToken, Deadline, Timestamp};
use agentrs_contracts::policy::{
    ApprovalOutcome, ApprovalRequest, DenyCode, PolicyDecision, PolicyError, SandboxGrant, ToolProposal,
};
use agentrs_contracts::ports::PolicyEnforcer;
use async_trait::async_trait;

use crate::sandbox::LocalFileSandbox;

/// 开发用策略。
pub struct DevPolicy {
    /// 允许的工具名。**不在名单内一律拒绝**，而不是反过来。
    allowed: HashSet<String>,
    sandbox: Arc<LocalFileSandbox>,
    next_grant: AtomicU64,
    /// 签发时刻。**由宿主从 Clock port 取得后传入**，内核与 adapter 都不读真实时钟。
    now: Timestamp,
}

impl DevPolicy {
    /// 用显式 allowlist 构造。
    pub fn new(sandbox: Arc<LocalFileSandbox>, allowed: impl IntoIterator<Item = &'static str>) -> Self {
        Self {
            allowed: allowed.into_iter().map(str::to_string).collect(),
            sandbox,
            now: Timestamp(0),
            next_grant: AtomicU64::new(0),
        }
    }
}

/// dev adapter 签发的 grant 有效期。
///
/// **不能是"永不过期"**——那等于一次批准就是永久批准。
/// 具体数值不重要，"有限"这件事才重要。
const GRANT_TTL_MS: i64 = 5 * 60 * 1000;

/// dev adapter 唯一会签发的挂起令牌。
const PENDING_TOKEN: &str = "dev-pending";

#[async_trait]
impl PolicyEnforcer for DevPolicy {
    async fn evaluate(&self, proposal: ToolProposal) -> Result<PolicyDecision, PolicyError> {
        if !self.allowed.contains(&proposal.tool_name) {
            return Ok(PolicyDecision::Deny {
                code: DenyCode::OutOfAuthority,
                message: format!("工具 {} 不在 allowlist 内", proposal.tool_name),
            });
        }

        let n = self.next_grant.fetch_add(1, Ordering::SeqCst) + 1;
        let grant_id = format!("dev-grant-{n}");
        let expires_at = Timestamp(self.now.0 + GRANT_TTL_MS);

        // **把绑定指纹交给 Sandbox 独立持有**——它随后会自行复核（H1）。
        self.sandbox
            .issue_grant(&grant_id, proposal.input_hash.clone(), expires_at);

        Ok(PolicyDecision::Allow {
            grant: SandboxGrant {
                grant_id,
                payload: serde_json::json!({"adapter": "dev"}),
            },
            bound_input_hash: proposal.input_hash,
            expires_at,
        })
    }

    async fn await_approval(
        &self,
        _request: ApprovalRequest,
        _deadline: Deadline,
    ) -> Result<ApprovalOutcome, PolicyError> {
        // 开发 adapter 不做交互式审批：一律挂起。
        // **这是安全默认值**——"默默放行"会让开发期悄悄养成越界习惯。
        Ok(ApprovalOutcome::Pending {
            resume_token: PENDING_TOKEN.into(),
        })
    }

    async fn redeem(&self, token: ApprovalToken) -> Result<ApprovalOutcome, PolicyError> {
        // **未签发过的令牌必须失败，不能报 Pending。**
        // 报 Pending 意味着内核会一直挂着等一个永远不会到来的裁决——
        // 编一个令牌就能让 Run 卡死。这条是 conformance suite 查出来的。
        if token.as_str() != PENDING_TOKEN {
            return Err(PolicyError::Unavailable);
        }
        // dev adapter 不做交互式审批，因此已知令牌也只能继续挂起。
        Ok(ApprovalOutcome::Pending { resume_token: token })
    }
}

#[cfg(test)]
mod tests {
    use agentrs_contracts::ids::Digest;
    use agentrs_contracts::policy::InputHash;

    use super::*;

    fn 提议(tool: &str) -> ToolProposal {
        ToolProposal {
            call_id: "c1".into(),
            tool_name: tool.into(),
            arguments: serde_json::json!({}),
            workspace_id: "ws".into(),
            change_set_id: "cs1".into(),
            input_hash: InputHash(Digest::from_hex("h")),
        }
    }

    fn 策略() -> (DevPolicy, tempdir::TempDir) {
        let dir = tempdir::TempDir::new("agentrs-policy").unwrap();
        let sb = Arc::new(LocalFileSandbox::new(dir.path()).unwrap());
        (DevPolicy::new(sb, ["Read", "Write"]), dir)
    }

    #[tokio::test]
    async fn 不在_allowlist_的工具被拒绝() {
        // 默认拒绝而非默认放行——名单是白名单不是黑名单。
        let (p, _d) = 策略();
        assert!(matches!(
            p.evaluate(提议("ExecCommand")).await.unwrap(),
            PolicyDecision::Deny { .. }
        ));
    }

    #[tokio::test]
    async fn 每次放行签发新的一次性_grant() {
        let (p, _d) = 策略();
        let ids: Vec<String> = {
            let mut v = Vec::new();
            for _ in 0..2 {
                if let PolicyDecision::Allow { grant, .. } = p.evaluate(提议("Read")).await.unwrap() {
                    v.push(grant.grant_id);
                }
            }
            v
        };
        assert_eq!(ids.len(), 2);
        assert_ne!(ids[0], ids[1], "grant 不得复用");
    }

    #[tokio::test]
    async fn 审批默认挂起而非放行() {
        // "默默放行"会让开发期悄悄养成越界习惯。
        let (p, _d) = 策略();
        let req = ApprovalRequest {
            step_id: "s1".into(),
            proposal: 提议("Write"),
            risk_summary: "".into(),
            originating_member: None,
            team_id: None,
        };
        assert!(matches!(
            p.await_approval(req, Deadline(Timestamp(0))).await.unwrap(),
            ApprovalOutcome::Pending { .. }
        ));
    }

    #[tokio::test]
    async fn 不持久化_allow_always() {
        // 结构上就没有存储字段——授权状态归宿主。
        let (p, _d) = 策略();
        let _ = p.evaluate(提议("Read")).await;
        // allowed 是构造时固定的 HashSet，evaluate 不会往里加东西。
        assert_eq!(p.allowed.len(), 2);
    }
}
