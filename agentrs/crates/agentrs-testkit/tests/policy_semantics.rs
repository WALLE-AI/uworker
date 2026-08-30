//! 策略与审批语义的契约测试（架构 §4.2、§6.2、§11.3.5）。
//!
//! 三条不变式：grant 一次性消费、审批有界等待、**任何 agent 不得授权**。

use agentrs_contracts::ids::{Deadline, Digest, Timestamp};
use agentrs_contracts::policy::{
    ApprovalDecision, ApprovalOutcome, ApprovalRequest, DecisionSource, InputHash, PolicyDecision,
    ToolProposal,
};
use agentrs_contracts::ports::PolicyEnforcer;
use agentrs_testkit::FakePolicy;

fn 提议(name: &str) -> ToolProposal {
    ToolProposal {
        step_id: "s1".into(),
        call_id: "c1".into(),
        tool_name: name.into(),
        arguments: serde_json::json!({}),
        workspace_id: "ws".into(),
        change_set_id: "cs1".into(),
        input_hash: InputHash(Digest::from_hex("h")),
    }
}

fn 审批请求() -> ApprovalRequest {
    ApprovalRequest {
        step_id: "s1".into(),
        proposal: 提议("Write"),
        risk_summary: "写入文件".into(),
        originating_member: None,
        team_id: None,
    }
}

#[tokio::test]
async fn 每次放行签发不同的_grant() {
    // grant 一次性消费：两次裁决不能拿到同一个 grant，否则重放防护失效。
    let p = FakePolicy::allow_all();
    p.evaluate(提议("Read")).await.unwrap();
    p.evaluate(提议("Read")).await.unwrap();
    let g = p.issued_grants();
    assert_eq!(g.len(), 2);
    assert_ne!(g[0], g[1], "每次放行必须签发新 grant");
}

#[tokio::test]
async fn allow_必须绑定提议的_input_hash() {
    let p = FakePolicy::allow_all();
    match p.evaluate(提议("Read")).await.unwrap() {
        PolicyDecision::Allow { bound_input_hash, .. } => {
            assert_eq!(bound_input_hash, InputHash(Digest::from_hex("h")))
        }
        other => panic!("期望 Allow，得到 {other:?}"),
    }
}

#[tokio::test]
async fn 默认拒绝的策略产生结构化_deny() {
    // Deny 是结构化结果，会被回灌模型，不是异常。
    let p = FakePolicy::deny_all();
    match p.evaluate(提议("Exec")).await.unwrap() {
        PolicyDecision::Deny { code, .. } => {
            assert_eq!(code, agentrs_contracts::policy::DenyCode::OutOfAuthority);
        }
        other => panic!("期望 Deny，得到 {other:?}"),
    }
}

#[tokio::test]
async fn 无人裁决时默认挂起而非放行() {
    // 这是安全默认值：超时转挂起，绝不"默默放行"。
    let p = FakePolicy::allow_all();
    let out = p
        .await_approval(审批请求(), Deadline(Timestamp(0)))
        .await
        .unwrap();
    match out {
        ApprovalOutcome::Pending { resume_token } => {
            assert_eq!(resume_token.as_str(), "fake-token");
        }
        other => panic!("默认必须挂起，得到 {other:?}"),
    }
}

#[tokio::test]
async fn 人类裁决可放行并随附_grant() {
    let p = FakePolicy::allow_all();
    p.script_approval(ApprovalOutcome::Decided(ApprovalDecision {
        allowed: true,
        source: DecisionSource::Human { user_id: "u1".into() },
        grant: Some(agentrs_contracts::policy::SandboxGrant {
            grant_id: "g-human".into(),
            payload: serde_json::json!({}),
        }),
        decided_at: Timestamp(1),
    }));

    match p
        .await_approval(审批请求(), Deadline(Timestamp(0)))
        .await
        .unwrap()
    {
        ApprovalOutcome::Decided(d) => {
            assert!(d.allowed);
            assert!(matches!(d.source, DecisionSource::Human { .. }));
            assert!(d.grant.is_some());
        }
        other => panic!("期望 Decided，得到 {other:?}"),
    }
}

#[tokio::test]
async fn agent_无法成为裁决来源() {
    // 内核不变量 15 由类型保证：DecisionSource 没有 Agent 变体。
    // 反序列化一个伪造来源必然失败——这是"模型给模型批准"的防线。
    let 伪造 = r#"{"source":"agent","agent_id":"a1"}"#;
    assert!(
        serde_json::from_str::<DecisionSource>(伪造).is_err(),
        "agent 来源必须无法构造"
    );

    // 合法来源只有两种。
    for ok in [
        r#"{"source":"human","user_id":"u"}"#,
        r#"{"source":"policy","policy_id":"p"}"#,
    ] {
        assert!(serde_json::from_str::<DecisionSource>(ok).is_ok(), "{ok}");
    }
}

#[tokio::test]
async fn 提议被完整记录供审计() {
    let p = FakePolicy::allow_all();
    p.evaluate(提议("Read")).await.unwrap();
    p.evaluate(提议("Write")).await.unwrap();
    let seen = p.seen_proposals();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].tool_name, "Read");
    assert_eq!(seen[1].change_set_id.as_str(), "cs1", "change_set 参与身份");
}
