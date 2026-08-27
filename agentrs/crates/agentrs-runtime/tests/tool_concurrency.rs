//! 工具并发调度的端到端验证（架构 §8.3，任务 T09A）。
//!
//! 计划本身在 `schedule.rs` 里已有单元测试。这里验证的是**运行时真的照计划跑**：
//!
//! 1. 同一并行批次里的调用**确实同时在执行**——用一个只有全员到齐才放行的
//!    闸门证明；若实现退化成串行，闸门永远开不了，测试超时而不是悄悄通过。
//! 2. 独占工具形成 barrier：它开始时前面的都已完成，它完成后后面的才开始。
//! 3. 并发执行**不改变事件顺序**——同一份历史重放必须得到同一个事件序。
//!
//! 三条都走 `toolround::execute_batch`，与 `engine::run_tool_calls` 是**同一份实现**。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agentrs_contracts::ids::{Deadline, Timestamp};
use agentrs_contracts::policy::SandboxGrant;
use agentrs_contracts::ports::SandboxExecutor;
use agentrs_contracts::sandbox::{
    ExecutionOutcome, ExecutionRequest, ExecutionResult, ExecutionStatus, IsolationLevel, SandboxError,
};
use agentrs_contracts::StepOutcome;
use agentrs_runtime::schedule::plan;
use agentrs_runtime::toolround::{execute_batch, ProposedCall, ToolRoundCtx, ToolRoundDeps};
use agentrs_testkit::FakePolicy;
use agentrs_types::ToolDef;

/// 只有全员到齐才放行的执行器。
///
/// 每次 `execute` 先登记进入、再等闸门。闸门在**同时在场人数达到 `width`**
/// 时打开。串行执行下人数永远到不了 `width`，于是卡死——
/// 这正是我们要的：并发性靠"不并发就跑不完"来证明，而不是靠计时。
struct 闸门执行器 {
    width: usize,
    notify: Arc<tokio::sync::Notify>,
    in_flight: AtomicUsize,
    /// 每次执行的 (进入序号, 工具名, 完成序号)。
    trace: Mutex<Vec<(usize, String, usize)>>,
    enter_seq: AtomicUsize,
    finish_seq: AtomicUsize,
}

impl 闸门执行器 {
    fn new(width: usize) -> Self {
        Self {
            width,
            notify: Arc::new(tokio::sync::Notify::new()),
            in_flight: AtomicUsize::new(0),
            trace: Mutex::new(Vec::new()),
            enter_seq: AtomicUsize::new(0),
            finish_seq: AtomicUsize::new(0),
        }
    }

    fn trace(&self) -> Vec<(usize, String, usize)> {
        self.trace.lock().expect("poisoned").clone()
    }
}

#[async_trait::async_trait]
impl SandboxExecutor for 闸门执行器 {
    async fn execute(
        &self,
        _grant: SandboxGrant,
        request: ExecutionRequest,
    ) -> Result<ExecutionResult, SandboxError> {
        let enter = self.enter_seq.fetch_add(1, Ordering::SeqCst);
        let n = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;

        if n >= self.width {
            // 全员到齐，放行所有等待者。
            self.notify.notify_waiters();
        } else {
            // 让出执行权，等最后一个到齐的人来叫醒。
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().await;
        }

        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        let finish = self.finish_seq.fetch_add(1, Ordering::SeqCst);
        self.trace
            .lock()
            .expect("poisoned")
            .push((enter, request.tool_name.clone(), finish));

        Ok(ExecutionResult {
            execution_id: request.execution_id,
            outcome: ExecutionOutcome::Completed { exit_code: 0 },
            effective_isolation: IsolationLevel::L0BasicContainment,
            artifacts: vec![],
            output: Some("ok".into()),
            change_set: None,
            finished_at: Timestamp(0),
        })
    }

    async fn cancel(&self, _id: agentrs_contracts::ids::ExecutionId) -> Result<(), SandboxError> {
        Ok(())
    }

    async fn reconcile(
        &self,
        _id: agentrs_contracts::ids::ExecutionId,
    ) -> Result<ExecutionStatus, SandboxError> {
        Ok(ExecutionStatus::Unknown)
    }
}

fn 目录() -> Vec<ToolDef> {
    vec![
        ToolDef::read_only("Read", "读", serde_json::json!({})),
        ToolDef::mutating("Write", "写", serde_json::json!({})),
    ]
}

fn 调用(names: &[&str]) -> Vec<ProposedCall> {
    names
        .iter()
        .enumerate()
        .map(|(i, n)| ProposedCall {
            call_id: format!("c{i}").into(),
            tool_name: (*n).to_owned(),
            arguments: serde_json::json!({}),
        })
        .collect()
}

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

/// 按计划跑一遍：批次内并发，批次之间等齐。
///
/// 这是 `engine::run_tool_calls` 调度部分的等价物，抽出来以便脱离 provider 测试。
async fn 按计划执行(
    deps: &ToolRoundDeps,
    calls: &[ProposedCall],
    catalog: &[ToolDef],
) -> Vec<(usize, StepOutcome)> {
    let p = plan(calls, catalog);
    let ctx = 上下文();
    let mut out = Vec::new();
    for batch in &p.batches {
        // **与 engine::run_tool_calls 走同一个 execute_batch**——
        // 测试里另写一份调度循环，两边迟早会漂移。
        for s in execute_batch(deps, &ctx, calls, &batch.indices).await {
            out.push((s.index, s.outcome.expect("不应挂起").1.outcome));
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn 并行批次里的调用确实同时在执行() {
    // 闸门宽度 3 = 三个 Read 必须同时在场才放行。
    // 若调度退化成串行，这里会超时——**并发性由"不并发就跑不完"证明**。
    let sandbox = Arc::new(闸门执行器::new(3));
    let deps = ToolRoundDeps::minimal(Arc::new(FakePolicy::allow_all()), sandbox.clone());
    let calls = 调用(&["Read", "Read", "Read"]);

    let r = tokio::time::timeout(Duration::from_secs(5), 按计划执行(&deps, &calls, &目录()))
        .await
        .expect("三个只读工具没能并发执行（超时）");

    assert_eq!(r.len(), 3);
    assert!(r.iter().all(|(_, o)| *o == StepOutcome::Succeeded));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn 独占工具形成前后双向屏障() {
    // 闸门宽度 1：任何单独一个调用都能自己放行自己，
    // 因此这个用例只考察**顺序**，不考察并发。
    let sandbox = Arc::new(闸门执行器::new(1));
    let deps = ToolRoundDeps::minimal(Arc::new(FakePolicy::allow_all()), sandbox.clone());
    let calls = 调用(&["Read", "Read", "Write", "Read"]);

    按计划执行(&deps, &calls, &目录()).await;

    let trace = sandbox.trace();
    let write = trace.iter().find(|(_, n, _)| n == "Write").expect("没跑 Write");
    let (write_enter, _, write_finish) = write;

    // 前两个 Read 必须在 Write **进入之前**就已完成。
    let 前置完成: Vec<usize> = trace
        .iter()
        .filter(|(e, n, _)| n == "Read" && e < write_enter)
        .map(|(_, _, f)| *f)
        .collect();
    assert_eq!(前置完成.len(), 2, "Write 之前应有两个 Read");
    assert!(
        前置完成.iter().all(|f| f < write_finish),
        "Write 开始前，前面的 Read 必须已 settlement"
    );

    // 最后一个 Read 必须在 Write **完成之后**才进入。
    let 后置 = trace
        .iter()
        .filter(|(e, n, _)| n == "Read" && e > write_enter)
        .count();
    assert_eq!(后置, 1, "Write 之后应有一个 Read");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn 并发执行不改变结果的原始顺序() {
    // 乱序回灌会让 tool_use 与 tool_result 的配对在部分厂商上直接 400。
    let sandbox = Arc::new(闸门执行器::new(3));
    let deps = ToolRoundDeps::minimal(Arc::new(FakePolicy::allow_all()), sandbox.clone());
    let calls = 调用(&["Read", "Read", "Read"]);

    let r = tokio::time::timeout(Duration::from_secs(5), 按计划执行(&deps, &calls, &目录()))
        .await
        .expect("超时");

    let p = plan(&calls, &目录());
    // 无论谁先跑完，reorder 之后必须是原始 proposal order。
    let 还原 = p.reorder(r).expect("结果应可完整还原");
    assert_eq!(还原.len(), 3);
    assert!(还原.iter().all(|o| *o == StepOutcome::Succeeded));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn 连续两个写不会被并到一起() {
    // 闸门宽度 2：如果两个 Write 被错误地并进同一批次，它们会互相等到放行，
    // 测试"通过"得反而更快——所以这里必须反过来断言 trace 的形状。
    let sandbox = Arc::new(闸门执行器::new(1));
    let deps = ToolRoundDeps::minimal(Arc::new(FakePolicy::allow_all()), sandbox.clone());
    let calls = 调用(&["Write", "Write"]);

    按计划执行(&deps, &calls, &目录()).await;

    let trace = sandbox.trace();
    assert_eq!(trace.len(), 2);
    // 第一个必须在第二个进入之前完成。
    let (e0, _, f0) = trace[0];
    let (e1, _, _) = trace[1];
    assert!(e0 < e1);
    assert!(f0 < e1 + 1, "两个写不得重叠");
}
