//! 宿主义务 H1 / H2 的契约测试（架构 §12.2）。
//!
//! - **H1**：`SandboxExecutor` 独立复核 `bound_input_hash` 与 grant 有效期，不信任调用方。
//! - **H2**：真正实施隔离；`reconcile` 如实报告三态。
//!
//! 违反 H1 → 内核不变量 2 失效；违反 H2 → 不变量 4 失效，恢复会重复副作用。

use agentrs_contracts::ids::{Digest, Timestamp};
use agentrs_contracts::policy::{InputHash, SandboxGrant};
use agentrs_contracts::ports::SandboxExecutor;
use agentrs_contracts::sandbox::{
    ExecutionOutcome, ExecutionRequest, ExecutionStatus, IsolationLevel, RejectReason, SandboxError,
};
use agentrs_testkit::{FakeSandbox, ScriptedExecution};

fn hash(h: &str) -> InputHash {
    InputHash(Digest::from_hex(h))
}

fn grant(id: &str) -> SandboxGrant {
    SandboxGrant {
        grant_id: id.into(),
        payload: serde_json::json!({}),
    }
}

fn 请求(exec: &str, h: &str, iso: IsolationLevel) -> ExecutionRequest {
    ExecutionRequest {
        execution_id: exec.into(),
        tool_name: "Write".into(),
        arguments: serde_json::json!({"path": "a.rs"}),
        change_set_id: "cs1".into(),
        input_hash: hash(h),
        required_isolation: iso,
    }
}

fn 沙箱() -> FakeSandbox {
    let s = FakeSandbox::new();
    s.issue_grant("g1", hash("correct"), Timestamp(1000));
    s
}

#[tokio::test]
async fn h1_1_篡改的_input_hash_被拒绝() {
    // 调用方声称的 hash 与 grant 绑定值不符 —— Sandbox 必须独立发现。
    let s = 沙箱();
    let r = s
        .execute(
            grant("g1"),
            请求("e1", "tampered", IsolationLevel::L0BasicContainment),
        )
        .await
        .unwrap();
    assert_eq!(
        r.outcome,
        ExecutionOutcome::Rejected {
            reason: RejectReason::InputHashMismatch
        }
    );
    assert_eq!(s.execution_count(), 0, "拒绝的请求不得进入执行");
}

#[tokio::test]
async fn h1_2_过期的_grant_被拒绝() {
    let s = 沙箱();
    s.set_now(Timestamp(2000)); // 已过 expires_at=1000
    let r = s
        .execute(
            grant("g1"),
            请求("e1", "correct", IsolationLevel::L0BasicContainment),
        )
        .await
        .unwrap();
    assert_eq!(
        r.outcome,
        ExecutionOutcome::Rejected {
            reason: RejectReason::GrantExpired
        }
    );
}

#[tokio::test]
async fn h1_3_grant_只能消费一次() {
    // 重复使用是内部错误而非重试路径（架构 §4.2 grant 规则 1）。
    let s = 沙箱();
    let ok = s
        .execute(
            grant("g1"),
            请求("e1", "correct", IsolationLevel::L0BasicContainment),
        )
        .await
        .unwrap();
    assert!(matches!(ok.outcome, ExecutionOutcome::Completed { .. }));

    let again = s
        .execute(
            grant("g1"),
            请求("e2", "correct", IsolationLevel::L0BasicContainment),
        )
        .await
        .unwrap();
    assert_eq!(
        again.outcome,
        ExecutionOutcome::Rejected {
            reason: RejectReason::GrantAlreadyConsumed
        }
    );
    assert_eq!(s.execution_count(), 1);
}

#[tokio::test]
async fn h1_4_未登记的_grant_直接失败() {
    let s = 沙箱();
    let e = s
        .execute(
            grant("forged"),
            请求("e1", "correct", IsolationLevel::L0BasicContainment),
        )
        .await;
    assert!(matches!(e, Err(SandboxError::InvalidGrant)));
}

#[tokio::test]
async fn h2_1_隔离级别不足时失败而非静默降级() {
    // L0 的执行器收到要求 L1 的请求 —— 必须失败。
    // 静默降级等于对外宣称"已隔离"却没有，是最危险的一类失败。
    let s = 沙箱();
    s.set_isolation(IsolationLevel::L0BasicContainment);
    let r = s
        .execute(
            grant("g1"),
            请求("e1", "correct", IsolationLevel::L1RealIsolation),
        )
        .await
        .unwrap();
    assert_eq!(
        r.outcome,
        ExecutionOutcome::Rejected {
            reason: RejectReason::IsolationUnavailable
        }
    );
}

#[tokio::test]
async fn h2_2_结果如实报告实际生效的隔离级别() {
    let s = 沙箱();
    s.set_isolation(IsolationLevel::L0BasicContainment);
    let r = s
        .execute(
            grant("g1"),
            请求("e1", "correct", IsolationLevel::L0BasicContainment),
        )
        .await
        .unwrap();
    assert_eq!(r.effective_isolation, IsolationLevel::L0BasicContainment);
    // 内核据此在 trajectory 标注，用户能看到"这条命令是在 L0 下执行的"。
}

#[tokio::test]
async fn h2_3_未执行过的请求_reconcile_为_not_started() {
    // 这是**唯一允许重试**的情形之一。
    let s = 沙箱();
    assert_eq!(
        s.reconcile("never-ran".into()).await.unwrap(),
        ExecutionStatus::NotStarted
    );
}

#[tokio::test]
async fn h2_4_崩溃后_reconcile_报告_unknown_内核必须停下问人() {
    // "执行了但没返回"——最危险的情形。猜测会导致重复的外部副作用。
    let s = 沙箱();
    s.script(ScriptedExecution::CrashThen(ExecutionStatus::Unknown));

    let e = s
        .execute(
            grant("g1"),
            请求("e1", "correct", IsolationLevel::L0BasicContainment),
        )
        .await;
    assert!(e.is_err(), "崩溃时不返回结果");
    assert_eq!(s.execution_count(), 1, "但它确实执行过");

    assert_eq!(
        s.reconcile("e1".into()).await.unwrap(),
        ExecutionStatus::Unknown,
        "必须如实报告未知，内核据此停在 RunNeedsUserAction"
    );
}

#[tokio::test]
async fn h2_5_崩溃后可如实报告已完成() {
    let s = 沙箱();
    let done = ExecutionStatus::Finished(Box::new(agentrs_contracts::sandbox::ExecutionResult {
        execution_id: "e1".into(),
        outcome: ExecutionOutcome::Completed { exit_code: 0 },
        effective_isolation: IsolationLevel::L0BasicContainment,
        artifacts: vec![],
        output: None,
        change_set: Some("cs1".into()),
        finished_at: Timestamp(0),
    }));
    s.script(ScriptedExecution::CrashThen(done.clone()));

    let _ = s
        .execute(
            grant("g1"),
            请求("e1", "correct", IsolationLevel::L0BasicContainment),
        )
        .await;

    // 内核据此直接回灌结果，**绝不重执行**（内核不变量 4）。
    assert_eq!(s.reconcile("e1".into()).await.unwrap(), done);
    assert_eq!(s.execution_count(), 1, "恢复不产生第二次执行");
}

#[tokio::test]
async fn 执行请求必须携带_change_set_且被记录() {
    // change_set_id 进入 input_hash：同一命令在不同 ChangeSet 上是不同意图。
    let s = 沙箱();
    s.execute(
        grant("g1"),
        请求("e1", "correct", IsolationLevel::L0BasicContainment),
    )
    .await
    .unwrap();
    let ex = s.executed();
    assert_eq!(ex[0].change_set_id.as_str(), "cs1");
}
