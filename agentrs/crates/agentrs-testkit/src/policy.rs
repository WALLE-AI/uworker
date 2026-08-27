//! 可编排的 fake PolicyEnforcer（任务 T02）。
//!
//! 覆盖三条裁决路径与审批的两种结局，让内核的以下不变式可测：
//!
//! - grant **一次性消费**——重复使用是内部错误而非重试路径；
//! - 审批**有界等待**——超时转挂起，绝不无限阻塞；
//! - **任何 agent 不得成为裁决来源**（内核不变量 15）。

use std::collections::VecDeque;
use std::sync::Mutex;

use agentrs_contracts::ids::{ApprovalToken, Deadline, Timestamp};
use agentrs_contracts::policy::{
    ApprovalDecision, ApprovalOutcome, ApprovalRequest, DecisionSource, InputHash, PolicyDecision,
    PolicyError, SandboxGrant, ToolProposal,
};
use agentrs_contracts::ports::PolicyEnforcer;
use async_trait::async_trait;

/// 预置的裁决脚本。按 FIFO 消费；耗尽后回落到 `default`。
#[derive(Default)]
struct State {
    scripted: VecDeque<PolicyDecision>,
    approvals: VecDeque<ApprovalOutcome>,
    /// 已签发的 grant，用于验证一次性消费。
    issued: Vec<String>,
    /// 收到的提议，供断言。
    seen: Vec<ToolProposal>,
    next_grant: u64,
    /// token -> 兑现结果。
    redeems: std::collections::HashMap<String, ApprovalOutcome>,
}

/// 可编排的策略 fake。
pub struct FakePolicy {
    state: Mutex<State>,
    default_allow: bool,
}

impl FakePolicy {
    /// 默认放行的策略（脚本耗尽后一律 Allow 并签发新 grant）。
    pub fn allow_all() -> Self {
        Self {
            state: Mutex::new(State::default()),
            default_allow: true,
        }
    }

    /// 默认拒绝的策略。
    pub fn deny_all() -> Self {
        Self {
            state: Mutex::new(State::default()),
            default_allow: false,
        }
    }

    /// 追加一条脚本裁决。
    pub fn script(&self, decision: PolicyDecision) -> &Self {
        self.state.lock().unwrap().scripted.push_back(decision);
        self
    }

    /// 追加一条脚本审批结局。
    pub fn script_approval(&self, outcome: ApprovalOutcome) -> &Self {
        self.state.lock().unwrap().approvals.push_back(outcome);
        self
    }

    /// 为一个令牌预置兑现结果。挂起后 `redeem` 会取到它。
    pub fn script_redeem(&self, token: &str, outcome: ApprovalOutcome) -> &Self {
        self.state
            .lock()
            .unwrap()
            .redeems
            .insert(token.to_string(), outcome);
        self
    }

    /// 已签发的 grant id 列表。
    pub fn issued_grants(&self) -> Vec<String> {
        self.state.lock().unwrap().issued.clone()
    }

    /// 收到过的提议。
    pub fn seen_proposals(&self) -> Vec<ToolProposal> {
        self.state.lock().unwrap().seen.clone()
    }

    fn mint_grant(state: &mut State, hash: InputHash) -> PolicyDecision {
        state.next_grant += 1;
        let id = format!("grant-{}", state.next_grant);
        state.issued.push(id.clone());
        PolicyDecision::Allow {
            grant: SandboxGrant {
                grant_id: id,
                payload: serde_json::json!({"fake": true}),
            },
            bound_input_hash: hash,
            // 远期过期；需要测过期路径时用 script 显式给一个过去的时刻。
            expires_at: Timestamp(i64::MAX),
        }
    }
}

#[async_trait]
impl PolicyEnforcer for FakePolicy {
    async fn evaluate(&self, proposal: ToolProposal) -> Result<PolicyDecision, PolicyError> {
        let mut s = self.state.lock().unwrap();
        s.seen.push(proposal.clone());

        if let Some(d) = s.scripted.pop_front() {
            if let PolicyDecision::Allow { grant, .. } = &d {
                s.issued.push(grant.grant_id.clone());
            }
            return Ok(d);
        }

        if self.default_allow {
            Ok(Self::mint_grant(&mut s, proposal.input_hash))
        } else {
            Ok(PolicyDecision::Deny {
                code: agentrs_contracts::policy::DenyCode::OutOfAuthority,
                message: "fake policy denies by default".into(),
            })
        }
    }

    async fn await_approval(
        &self,
        _request: ApprovalRequest,
        _deadline: Deadline,
    ) -> Result<ApprovalOutcome, PolicyError> {
        let mut s = self.state.lock().unwrap();
        if let Some(o) = s.approvals.pop_front() {
            // 内核不变量 15：拒绝任何非人类/组织策略来源的裁决。
            if let ApprovalOutcome::Decided(ApprovalDecision { source, .. }) = &o {
                match source {
                    DecisionSource::Human { .. } | DecisionSource::Policy { .. } => {}
                }
            }
            return Ok(o);
        }
        // 无脚本时默认超时挂起——这是比"默默放行"安全得多的默认值。
        Ok(ApprovalOutcome::Pending {
            resume_token: "fake-token".into(),
        })
    }

    async fn redeem(&self, token: ApprovalToken) -> Result<ApprovalOutcome, PolicyError> {
        let s = self.state.lock().unwrap();
        Ok(s.redeems.get(token.as_str()).cloned().unwrap_or(
            // 未预置 = 人仍未裁决，继续保持挂起。
            ApprovalOutcome::Pending { resume_token: token },
        ))
    }
}
