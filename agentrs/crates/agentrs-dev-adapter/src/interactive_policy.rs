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

use crate::LocalDevSandbox;

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
    sandbox: Arc<LocalDevSandbox>,
    requests: mpsc::Sender<ApprovalPrompt>,
    now: Timestamp,
    wait_limit: Duration,
    next_grant: AtomicU64,
    next_token: AtomicU64,
    /// 本进程内该策略实例的序号，用来给 grant id 加前缀。
    ///
    /// 一个 Sandbox 会被不止一个策略实例用到——多轮对话里每一轮是一个新 Run，
    /// 新 Run 配新策略，而 ChangeSet 与 overlay 必须跨轮留在同一个 Sandbox 上。
    /// 光靠实例内计数器，第二轮的第一个 grant 又叫 `-1`，撞上第一轮已经消费掉的
    /// 那个，H1 的一次性台账会把它判成 `GrantAlreadyConsumed`——**拒绝是对的**，
    /// 错的是发号的人重复了号。
    instance: u64,
    pending: Mutex<HashMap<ApprovalToken, ApprovalRequest>>,
}

/// 进程内策略实例计数器。
static NEXT_INSTANCE: AtomicU64 = AtomicU64::new(0);

/// 这次批准，人实际上是在为什么负责。
///
/// 从前这里是一句写死的 "{tool} requests workspace access"。加进命令与网络之后
/// 它开始骗人：`Bash` 要的不是"工作区访问"，是**任意执行**；`WebFetch` 一个
/// 字节也不碰工作区，它要的是**把一个地址发出去**。审批面板上那一行是人唯一
/// 会读的东西，它说错了，人点头时同意的就不是他以为的那件事。
fn 风险(tool: &str) -> String {
    match tool {
        "Bash" => "执行任意命令。副作用**直接落盘**，不进 ChangeSet，事后无法回滚".into(),
        "WebFetch" => "向外发起一次 HTTP 请求。请求发出去就收不回来，对方的日志里会有它".into(),
        "WebSearch" => "把这段检索词发给外部搜索端点".into(),
        "Delete" => "删除工作区文件，改动先进 ChangeSet，提交前可以反悔".into(),
        "Write" | "Edit" => "修改工作区文件，改动先进 ChangeSet，提交前可以反悔".into(),
        other => format!("{other} 请求超出自动放行范围的权限"),
    }
}

impl InteractiveDevPolicy {
    /// 切分一份目录：自动放行的一张表，逐次审批的一张表。
    ///
    /// 判据与工具目录同源（[`agentrs_tools::builtin::split_for_approval`]），
    /// 宿主别再手抄工具名：目录里新增一个工具而策略的两张表没跟上，那个工具就
    /// **永远调不动**——症状是模型反复提出一个被判 `OutOfAuthority` 的调用，
    /// 而目录里明明有它，于是它开始换着法子绕，把一整个 Run 耗在猜上。
    pub fn split_by_effect(catalog: &[agentrs_types::ToolDef]) -> (Vec<String>, Vec<String>) {
        agentrs_tools::builtin::split_for_approval(catalog)
    }

    /// 构造测试策略。两个工具集合不得重叠；未列出的工具一律拒绝。
    pub fn new(
        sandbox: Arc<LocalDevSandbox>,
        auto_allowed: impl IntoIterator<Item = impl Into<String>>,
        approval_required: impl IntoIterator<Item = impl Into<String>>,
        requests: mpsc::Sender<ApprovalPrompt>,
        now: Timestamp,
        wait_limit: Duration,
    ) -> Result<Self, PolicyError> {
        let auto_allowed = auto_allowed.into_iter().map(Into::into).collect::<HashSet<_>>();
        let approval_required = approval_required
            .into_iter()
            .map(Into::into)
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
            instance: NEXT_INSTANCE.fetch_add(1, Ordering::SeqCst),
            pending: Mutex::new(HashMap::new()),
        })
    }

    fn grant(&self, proposal: &ToolProposal) -> (SandboxGrant, Timestamp) {
        let n = self.next_grant.fetch_add(1, Ordering::SeqCst) + 1;
        let grant_id = format!("dev-tui-grant-{}-{n}", self.instance);
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
                risk_summary: 风险(&proposal.tool_name),
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
        let sandbox = Arc::new(LocalDevSandbox::new(dir.path()).unwrap());
        let (tx, rx) = mpsc::channel(4);
        let policy = Arc::new(
            InteractiveDevPolicy::new(sandbox, ["Read"], ["Write"], tx, Timestamp(0), wait).unwrap(),
        );
        (policy, rx, dir)
    }

    #[test]
    fn 切分覆盖目录里的每一个工具() {
        // 这条测试存在的理由是它抓到过一次真的：目录里加了 Glob、两张表没跟上，
        // 于是 Glob 每次调用都被判 OutOfAuthority，而目录里明明有它。
        let catalog = agentrs_tools::builtin::catalog(agentrs_tools::builtin::Capabilities::ALL);
        let (auto, approval) = InteractiveDevPolicy::split_by_effect(&catalog);
        assert_eq!(
            auto.len() + approval.len(),
            catalog.len(),
            "每个工具都要落进其中一张表"
        );
        assert!(auto.contains(&"Glob".to_string()));
        assert!(auto.contains(&"Read".to_string()));
        assert!(approval.contains(&"Write".to_string()));
        // 会改东西的绝不能落进"自动放行"。
        for name in &auto {
            let tool = catalog.iter().find(|t| &t.name == name).unwrap();
            assert!(tool.is_read_only(), "{name} 会改东西却被自动放行");
        }
        // 反过来不成立：只读的也可能要审批。`WebFetch` 不改工作区，
        // 但它把一个模型选定的地址发了出去，那件事收不回来。
        assert!(approval.contains(&"Bash".to_string()));
        assert!(approval.contains(&"WebFetch".to_string()));
        assert!(approval.contains(&"WebSearch".to_string()));
    }

    #[tokio::test]
    async fn 共用一个_sandbox_的两个策略不会发出同一个_grant_id() {
        // 多轮对话就是这个形状：每一轮一个新 Run、一个新策略，而 ChangeSet 与
        // overlay 必须留在同一个 Sandbox 上。号重了，第二轮的第一次写会被 H1
        // 的一次性台账判成 GrantAlreadyConsumed。
        let dir = tempdir::TempDir::new("agentrs-grant-ids").unwrap();
        let sandbox = Arc::new(LocalDevSandbox::new(dir.path()).unwrap());
        let mut ids = std::collections::HashSet::new();
        for _ in 0..3 {
            let (tx, _rx) = mpsc::channel(4);
            let policy = InteractiveDevPolicy::new(
                sandbox.clone(),
                ["Read"],
                ["Write"],
                tx,
                Timestamp(0),
                Duration::from_secs(1),
            )
            .unwrap();
            for _ in 0..2 {
                let (grant, _) = policy.grant(&proposal("Write"));
                assert!(ids.insert(grant.grant_id.clone()), "重复的 grant id: {}", grant.grant_id);
            }
        }
        assert_eq!(ids.len(), 6);
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
