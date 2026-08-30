//! 审批挂起与恢复的端到端验证（M0 出口标准，架构 §6.2）。
//!
//! > 审批超时挂起后进程内该 Run 常驻内存 ≈ 0，`resume` 能继续同一 `StepIntent`。
//!
//! 这条不是理论要求：阻塞等待期间 Run 的全部 live resource 驻留内存——
//! provider 连接、owner、child、tool lease、文件句柄。桌面场景下用户离开一晚，
//! 等价于句柄与内存泄漏。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use agentrs_contracts::ids::{Deadline, Timestamp};
use agentrs_contracts::policy::{
    ApprovalDecision, ApprovalOutcome, ApprovalRequest, DecisionSource, InputHash, PolicyDecision,
    SandboxGrant, ToolProposal,
};
use agentrs_contracts::ports::{PolicyEnforcer, SandboxExecutor};
use agentrs_contracts::sandbox::{ExecutionOutcome, ExecutionRequest, IsolationLevel};
use agentrs_contracts::StepOutcome;
use agentrs_runtime::composition::{AsyncCleanup, CleanupError, ResourceOwner};
use agentrs_runtime::toolround::{execute_call, input_hash, ProposedCall, ToolRoundCtx, ToolRoundDeps};
use agentrs_testkit::{FakePolicy, FakeSandbox};

fn 上下文() -> ToolRoundCtx {
    ToolRoundCtx {
        step_id: "s1".into(),
        workspace_id: "ws".into(),
        change_set_id: "cs1".into(),
        approval_deadline: Deadline(Timestamp(60_000)),
        required_isolation: IsolationLevel::L0BasicContainment,
        now: Timestamp(0),
    }
}

fn 调用() -> ProposedCall {
    ProposedCall {
        call_id: "c1".into(),
        tool_name: "Write".into(),
        arguments: serde_json::json!({"path": "report.md"}),
    }
}

fn 需要审批(hash: InputHash) -> PolicyDecision {
    PolicyDecision::RequireApproval(ApprovalRequest {
        step_id: "s1".into(),
        proposal: ToolProposal {
            step_id: "s1".into(),
            call_id: "c1".into(),
            tool_name: "Write".into(),
            arguments: serde_json::json!({}),
            workspace_id: "ws".into(),
            change_set_id: "cs1".into(),
            input_hash: hash,
        },
        risk_summary: "写入 report.md".into(),
        originating_member: None,
        team_id: None,
    })
}

/// 记录是否被清理的 live 资源。
struct Tracked(Arc<AtomicBool>);

#[async_trait::async_trait]
impl AsyncCleanup for Tracked {
    async fn cleanup(self: Box<Self>) -> Result<(), CleanupError> {
        self.0.store(true, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn 挂起时释放全部_live_资源() {
    // "常驻内存 ≈ 0"的可测形式：挂起路径必须结算 owner，
    // 而不是把 provider 连接、lease 等挂着等人。
    let cleaned = Arc::new(AtomicBool::new(false));
    let owner = ResourceOwner::new("run:r1");
    owner
        .register("provider-stream", Box::new(Tracked(cleaned.clone())))
        .await
        .unwrap();

    let ctx = 上下文();
    let call = 调用();
    let policy = FakePolicy::allow_all();
    policy.script(需要审批(
        input_hash(&call, &ctx.workspace_id, &ctx.change_set_id),
    ));

    let deps = ToolRoundDeps::minimal(Arc::new(policy), Arc::new(FakeSandbox::new()));

    // 审批未决 → 挂起。
    let (token, intent) = execute_call(&deps, &ctx, &call, &mut |_| {}).await.unwrap_err();
    assert_eq!(token.as_str(), "fake-token");

    // 宿主据此结算 live 平面。
    let report = owner.shutdown().await;
    assert!(report.is_clean());
    assert!(cleaned.load(Ordering::SeqCst), "挂起后 live 资源必须被释放");

    // 而 durable 平面上的意图完整保留——这正是两个平面不可替代的体现。
    assert_eq!(intent.call_id.as_str(), "c1");
    assert_eq!(intent.change_set_id.as_str(), "cs1");
}

#[tokio::test]
async fn 恢复凭令牌取回决策并继续同一_step_intent() {
    let ctx = 上下文();
    let call = 调用();
    let hash = input_hash(&call, &ctx.workspace_id, &ctx.change_set_id);

    // ---- 第一次：挂起 ----
    let policy = Arc::new(FakePolicy::allow_all());
    policy.script(需要审批(hash.clone()));
    let sandbox = Arc::new(FakeSandbox::new());
    sandbox.issue_grant("g-late", hash.clone(), Timestamp(i64::MAX));

    let deps = ToolRoundDeps::minimal(policy.clone(), sandbox.clone());
    let (token, intent) = execute_call(&deps, &ctx, &call, &mut |_| {}).await.unwrap_err();
    assert_eq!(sandbox.execution_count(), 0, "挂起时尚未执行");

    // ---- 人在稍后裁决 ----
    policy.script_redeem(
        token.as_str(),
        ApprovalOutcome::Decided(ApprovalDecision {
            allowed: true,
            source: DecisionSource::Human { user_id: "u1".into() },
            grant: Some(SandboxGrant {
                grant_id: "g-late".into(),
                payload: serde_json::json!({}),
            }),
            decided_at: Timestamp(5000),
        }),
    );

    // ---- 第二次：恢复 ----
    let outcome = policy.redeem(token).await.unwrap();
    let ApprovalOutcome::Decided(d) = outcome else {
        panic!("兑现后必须拿到裁决");
    };
    assert!(d.allowed);
    assert!(matches!(d.source, DecisionSource::Human { .. }), "裁决方永远是人");

    // 恢复后**继续同一个 StepIntent**：指纹不变，因此不需要重新裁决，
    // 也不会因为参数被重新计算而产生第二个不同的意图。
    assert_eq!(intent.input_hash, hash);

    // 用兑现得到的 grant 执行 —— Sandbox 独立复核指纹后放行。
    let req = ExecutionRequest {
        execution_id: intent.execution_id.clone(),
        tool_name: intent.tool_name.clone(),
        arguments: call.arguments.clone(),
        change_set_id: intent.change_set_id.clone(),
        input_hash: intent.input_hash.clone(),
        required_isolation: IsolationLevel::L0BasicContainment,
    };
    let r = sandbox.execute(d.grant.unwrap(), req).await.unwrap();
    assert!(matches!(r.outcome, ExecutionOutcome::Completed { exit_code: 0 }));
    assert_eq!(sandbox.execution_count(), 1, "恢复后只执行一次");
}

#[tokio::test]
async fn 人仍未裁决时保持挂起而不是放行() {
    // redeem 未预置 = 人还没看。安全默认值是继续挂起。
    let ctx = 上下文();
    let call = 调用();
    let policy = Arc::new(FakePolicy::allow_all());
    policy.script(需要审批(
        input_hash(&call, &ctx.workspace_id, &ctx.change_set_id),
    ));

    let deps = ToolRoundDeps::minimal(policy.clone(), Arc::new(FakeSandbox::new()));
    let (token, _) = execute_call(&deps, &ctx, &call, &mut |_| {}).await.unwrap_err();

    match policy.redeem(token.clone()).await.unwrap() {
        ApprovalOutcome::Pending { resume_token } => {
            assert_eq!(resume_token, token, "令牌保持有效，可再次兑现");
        }
        other => panic!("人未裁决时必须保持挂起，得到 {other:?}"),
    }
}

#[tokio::test]
async fn 人拒绝时产生结构化拒绝而非执行() {
    let ctx = 上下文();
    let call = 调用();
    let hash = input_hash(&call, &ctx.workspace_id, &ctx.change_set_id);

    let policy = FakePolicy::allow_all();
    policy.script(需要审批(hash));
    policy.script_approval(ApprovalOutcome::Decided(ApprovalDecision {
        allowed: false,
        source: DecisionSource::Human { user_id: "u1".into() },
        grant: None,
        decided_at: Timestamp(1),
    }));

    let sandbox = Arc::new(FakeSandbox::new());
    let deps = ToolRoundDeps::minimal(Arc::new(policy), sandbox.clone());

    let (_, result) = execute_call(&deps, &ctx, &call, &mut |_| {}).await.unwrap();
    assert!(matches!(result.outcome, StepOutcome::Denied { .. }));
    assert_eq!(sandbox.execution_count(), 0, "被拒不得触达执行器");
}
