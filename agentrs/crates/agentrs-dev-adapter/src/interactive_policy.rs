//! TUI/测试宿主使用的交互式开发 Policy。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agentrs_contracts::ids::{ApprovalToken, Deadline, Timestamp};
use agentrs_contracts::policy::{
    ApprovalDecision, ApprovalOutcome, ApprovalRequest, DecisionSource, DenyCode, PolicyDecision,
    PolicyError, SandboxGrant, ToolProposal,
};
use agentrs_contracts::ports::PolicyEnforcer;
use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};

use crate::LocalFileSandbox;

/// 人工审批回答。开发 TUI 只支持一次性放行或拒绝。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalAnswer {
    /// 仅放行本次提议。
    AllowOnce {
        /// 做出裁决的测试用户标识。
        user_id: String,
    },
    /// 拒绝本次提议。
    Reject {
        /// 做出裁决的测试用户标识。
        user_id: String,
    },
}

/// 交给 TUI 的一次审批请求。
pub struct ApprovalPrompt {
    /// 完整、结构化的审批请求。
    pub request: ApprovalRequest,
    /// 单次回答通道。发送端只能成功回答一次。
    pub respond_to: oneshot::Sender<ApprovalAnswer>,
}

/// 非生产交互式开发策略。
pub struct InteractiveDevPolicy {
    auto_allowed: HashSet<String>,
    approval_required: HashSet<String>,
    sandbox: Arc<LocalFileSandbox>,
    requests: mpsc::Sender<ApprovalPrompt>,
    now: Timestamp,
    wait_limit: Duration,
    next_grant: AtomicU64,
    next_token: AtomicU64,
    pending: Mutex<HashMap<ApprovalToken, ApprovalRequest>>,
}

impl InteractiveDevPolicy {
    /// 构造测试策略。两个工具集合不得重叠；未列出的工具一律拒绝。
    pub fn new(
        sandbox: Arc<LocalFileSandbox>,
        auto_allowed: impl IntoIterator<Item = &'static str>,
        approval_required: impl IntoIterator<Item = &'static str>,
        requests: mpsc::Sender<ApprovalPrompt>,
        now: Timestamp,
        wait_limit: Duration,
    ) -> Result<Self, PolicyError> {
        let auto_allowed = auto_allowed
            .into_iter()
            .map(str::to_string)
            .collect::<HashSet<_>>();
        let approval_required = approval_required
            .into_iter()
            .map(str::to_string)
            .collect::<HashSet<_>>();
        if !auto_allowed.is_disjoint(&approval_required) {
            return Err(PolicyError::Other {
                message: "interactive_policy_overlapping_tool_sets".into(),
            });
        }
        Ok(Self {
            auto_allowed,
            approval_required,
            sandbox,
            requests,
            now,
            wait_limit,
            next_grant: AtomicU64::new(0),
            next_token: AtomicU64::new(0),
            pending: Mutex::new(HashMap::new()),
        })
    }

    fn grant(&self, proposal: &ToolProposal) -> (SandboxGrant, Timestamp) {
        let n = self.next_grant.fetch_add(1, Ordering::SeqCst) + 1;
        let grant_id = format!("dev-tui-grant-{n}");
        let expires_at = Timestamp(self.now.0 + 5 * 60 * 1000);
        self.sandbox
            .issue_grant(&grant_id, proposal.input_hash.clone(), expires_at);
        (
            SandboxGrant {
                grant_id,
                payload: serde_json::json!({"adapter":"dev-tui"}),
            },
            expires_at,
        )
    }

    fn decided(&self, request: &ApprovalRequest, answer: ApprovalAnswer) -> ApprovalOutcome {
        let (allowed, user_id) = match answer {
            ApprovalAnswer::AllowOnce { user_id } => (true, user_id),
            ApprovalAnswer::Reject { user_id } => (false, user_id),
        };
        let grant = allowed.then(|| self.grant(&request.proposal).0);
        ApprovalOutcome::Decided(ApprovalDecision {
            allowed,
            source: DecisionSource::Human { user_id },
            grant,
            decided_at: self.now,
        })
    }

    fn duration_until(&self, deadline: Deadline) -> Duration {
        let logical_ms = deadline.0 .0.saturating_sub(self.now.0).max(0) as u64;
        self.wait_limit.min(Duration::from_millis(logical_ms))
    }

    async fn ask(
        &self,
        request: ApprovalRequest,
        wait: Duration,
    ) -> Result<Option<ApprovalOutcome>, PolicyError> {
        let (respond_to, response) = oneshot::channel();
        self.requests
            .send(ApprovalPrompt {
                request: request.clone(),
                respond_to,
            })
            .await
            .map_err(|_| PolicyError::Unavailable)?;
        match tokio::time::timeout(wait, response).await {
            Ok(Ok(answer)) => Ok(Some(self.decided(&request, answer))),
            Ok(Err(_)) => Err(PolicyError::Unavailable),
            Err(_) => Ok(None),
        }
    }

    fn suspend(&self, request: ApprovalRequest, token: Option<ApprovalToken>) -> ApprovalOutcome {
        let token = token.unwrap_or_else(|| {
            let n = self.next_token.fetch_add(1, Ordering::SeqCst) + 1;
            ApprovalToken::new(format!("dev-tui-pending-{n}"))
        });
        self.pending.lock().unwrap().insert(token.clone(), request);
        ApprovalOutcome::Pending { resume_token: token }
    }
}

#[async_trait]
impl PolicyEnforcer for InteractiveDevPolicy {
    async fn evaluate(&self, proposal: ToolProposal) -> Result<PolicyDecision, PolicyError> {
        if proposal.step_id.as_str() == "unknown" {
            return Ok(PolicyDecision::Deny {
                code: DenyCode::OutOfAuthority,
                message: "proposal missing step id".into(),
            });
        }
        if self.auto_allowed.contains(&proposal.tool_name) {
            let (grant, expires_at) = self.grant(&proposal);
            return Ok(PolicyDecision::Allow {
                grant,
                bound_input_hash: proposal.input_hash,
                expires_at,
            });
        }
        if self.approval_required.contains(&proposal.tool_name) {
            return Ok(PolicyDecision::RequireApproval(ApprovalRequest {
                step_id: proposal.step_id.clone(),
                risk_summary: format!("{} requests workspace access", proposal.tool_name),
                proposal,
                originating_member: None,
                team_id: None,
            }));
        }
        Ok(PolicyDecision::Deny {
            code: DenyCode::OutOfAuthority,
            message: "tool is not in the dev TUI allowlist".into(),
        })
    }

    async fn await_approval(
        &self,
        request: ApprovalRequest,
        deadline: Deadline,
    ) -> Result<ApprovalOutcome, PolicyError> {
        match self.ask(request.clone(), self.duration_until(deadline)).await? {
            Some(outcome) => Ok(outcome),
            None => Ok(self.suspend(request, None)),
        }
    }

    async fn redeem(&self, token: ApprovalToken) -> Result<ApprovalOutcome, PolicyError> {
        let request = self
            .pending
            .lock()
            .unwrap()
            .remove(&token)
            .ok_or(PolicyError::Unavailable)?;
        match self.ask(request.clone(), self.wait_limit).await? {
            Some(outcome) => Ok(outcome),
            None => Ok(self.suspend(request, Some(token))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentrs_contracts::ids::Digest;
    use agentrs_contracts::policy::InputHash;

    fn proposal(tool: &str) -> ToolProposal {
        ToolProposal {
            step_id: "s1".into(),
            call_id: "c1".into(),
            tool_name: tool.into(),
            arguments: serde_json::json!({"path":"a.txt"}),
            workspace_id: "ws".into(),
            change_set_id: "cs1".into(),
            input_hash: InputHash(Digest::from_hex("hash")),
        }
    }

    fn setup(
        wait: Duration,
    ) -> (
        Arc<InteractiveDevPolicy>,
        mpsc::Receiver<ApprovalPrompt>,
        tempdir::TempDir,
    ) {
        let dir = tempdir::TempDir::new("agentrs-interactive-policy").unwrap();
        let sandbox = Arc::new(LocalFileSandbox::new(dir.path()).unwrap());
        let (tx, rx) = mpsc::channel(4);
        let policy = Arc::new(
            InteractiveDevPolicy::new(sandbox, ["Read"], ["Write"], tx, Timestamp(0), wait).unwrap(),
        );
        (policy, rx, dir)
    }

    #[tokio::test]
    async fn 写工具产生带真实_step_的审批() {
        let (policy, _rx, _dir) = setup(Duration::from_secs(1));
        let decision = policy.evaluate(proposal("Write")).await.unwrap();
        let PolicyDecision::RequireApproval(request) = decision else {
            panic!("Write must require approval");
        };
        assert_eq!(request.step_id.as_str(), "s1");
    }

    #[tokio::test]
    async fn allow_once_最后时刻签发一次性_grant() {
        let (policy, mut rx, _dir) = setup(Duration::from_secs(1));
        let request = match policy.evaluate(proposal("Write")).await.unwrap() {
            PolicyDecision::RequireApproval(request) => request,
            _ => panic!("Write must require approval"),
        };
        let waiter = {
            let policy = policy.clone();
            tokio::spawn(async move {
                policy
                    .await_approval(request, Deadline(Timestamp(1_000)))
                    .await
                    .unwrap()
            })
        };
        let prompt = rx.recv().await.unwrap();
        prompt
            .respond_to
            .send(ApprovalAnswer::AllowOnce {
                user_id: "tester".into(),
            })
            .unwrap();
        let ApprovalOutcome::Decided(decision) = waiter.await.unwrap() else {
            panic!("approval should be decided");
        };
        assert!(decision.allowed);
        assert!(decision.grant.is_some());
    }

    #[tokio::test]
    async fn 超时挂起且_redeem_重新询问同一请求() {
        let (policy, mut rx, _dir) = setup(Duration::from_millis(1));
        let request = match policy.evaluate(proposal("Write")).await.unwrap() {
            PolicyDecision::RequireApproval(request) => request,
            _ => panic!("Write must require approval"),
        };
        let policy_for_wait = policy.clone();
        let first = tokio::spawn(async move {
            policy_for_wait
                .await_approval(request, Deadline(Timestamp(1)))
                .await
                .unwrap()
        });
        let _unanswered = rx.recv().await.unwrap();
        let ApprovalOutcome::Pending { resume_token } = first.await.unwrap() else {
            panic!("timeout should suspend");
        };

        let policy_for_redeem = policy.clone();
        let redeem = tokio::spawn(async move { policy_for_redeem.redeem(resume_token).await.unwrap() });
        let prompt = rx.recv().await.unwrap();
        assert_eq!(prompt.request.proposal.call_id.as_str(), "c1");
        prompt
            .respond_to
            .send(ApprovalAnswer::Reject {
                user_id: "tester".into(),
            })
            .unwrap();
        let ApprovalOutcome::Decided(decision) = redeem.await.unwrap() else {
            panic!("redeem should resolve");
        };
        assert!(!decision.allowed);
        assert!(decision.grant.is_none());
    }

    #[tokio::test]
    async fn 关闭请求通道_fail_closed() {
        let (policy, rx, _dir) = setup(Duration::from_secs(1));
        drop(rx);
        let request = match policy.evaluate(proposal("Write")).await.unwrap() {
            PolicyDecision::RequireApproval(request) => request,
            _ => panic!("Write must require approval"),
        };
        assert!(matches!(
            policy.await_approval(request, Deadline(Timestamp(1_000))).await,
            Err(PolicyError::Unavailable)
        ));
    }
}
