//! 可编排的 fake SandboxExecutor（任务 T02）。
//!
//! 它是**宿主义务 H1/H2 的被测对象**：
//!
//! - H1：独立复核 `bound_input_hash` 与 grant 有效期，**不信任调用方**；
//! - H2：`reconcile` 如实报告三态。
//!
//! 三态里 `Unknown` 最关键——内核据此**停在 `RunNeedsUserAction`**，
//! 而不是重试。猜测会导致重复的外部副作用。

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use agentrs_contracts::ids::{ExecutionId, Timestamp};
use agentrs_contracts::policy::{InputHash, SandboxGrant};
use agentrs_contracts::ports::SandboxExecutor;
use agentrs_contracts::sandbox::{
    ExecutionOutcome, ExecutionRequest, ExecutionResult, ExecutionStatus, IsolationLevel, RejectReason,
    SandboxError,
};
use async_trait::async_trait;

/// 一次执行在 fake 中的编排结果。
#[derive(Debug, Clone)]
pub enum ScriptedExecution {
    /// 正常完成。
    Succeed {
        /// 退出码。
        exit_code: i32,
    },
    /// 执行到一半"崩溃"——不返回结果，后续 `reconcile` 报告指定状态。
    ///
    /// 这是崩溃注入的核心：内核必须**先 reconcile 再决定**，不能盲目重跑。
    CrashThen(ExecutionStatus),
    /// 超时。
    TimeOut,
}

#[derive(Default)]
struct State {
    /// grant_id -> 绑定的 input_hash 与过期时刻。
    grants: HashMap<String, (InputHash, Timestamp)>,
    /// 已被消费的 grant——一次性消费的记账（H1）。
    consumed: HashSet<String>,
    /// 按 FIFO 消费的执行脚本。
    script: Vec<ScriptedExecution>,
    /// 崩溃后的 reconcile 状态。
    crashed: HashMap<ExecutionId, ExecutionStatus>,
    /// 实际执行过的请求，供断言。
    executed: Vec<ExecutionRequest>,
    /// 当前可提供的隔离级别。
    isolation: IsolationLevel,
    /// 当前逻辑时刻，用于判定 grant 过期。
    now: Timestamp,
}

/// 沙箱执行器 fake。
pub struct FakeSandbox {
    state: Mutex<State>,
}

impl Default for FakeSandbox {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeSandbox {
    /// 新建，默认提供 L0 基础围栏。
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State {
                isolation: IsolationLevel::L0BasicContainment,
                ..Default::default()
            }),
        }
    }

    /// 登记一个 grant 及其绑定值。**Sandbox 侧独立持有绑定值**——
    /// 这正是 H1 要求的"不信任调用方"。
    pub fn issue_grant(&self, grant_id: &str, bound: InputHash, expires_at: Timestamp) -> &Self {
        self.state
            .lock()
            .unwrap()
            .grants
            .insert(grant_id.to_string(), (bound, expires_at));
        self
    }

    /// 追加一条执行脚本。
    pub fn script(&self, e: ScriptedExecution) -> &Self {
        self.state.lock().unwrap().script.push(e);
        self
    }

    /// 设置可提供的隔离级别。低于请求要求时执行必须失败，**不得静默降级**。
    pub fn set_isolation(&self, level: IsolationLevel) -> &Self {
        self.state.lock().unwrap().isolation = level;
        self
    }

    /// 推进逻辑时刻，用于测试 grant 过期。
    pub fn set_now(&self, now: Timestamp) -> &Self {
        self.state.lock().unwrap().now = now;
        self
    }

    /// 实际执行过的请求。
    pub fn executed(&self) -> Vec<ExecutionRequest> {
        self.state.lock().unwrap().executed.clone()
    }

    /// 实际执行次数——验证"崩溃恢复后不重复副作用"的关键计数。
    pub fn execution_count(&self) -> usize {
        self.state.lock().unwrap().executed.len()
    }

    fn reject(id: ExecutionId, reason: RejectReason, isolation: IsolationLevel) -> ExecutionResult {
        ExecutionResult {
            execution_id: id,
            outcome: ExecutionOutcome::Rejected { reason },
            effective_isolation: isolation,
            artifacts: vec![],
            output: None,
            change_set: None,
            finished_at: Timestamp(0),
        }
    }
}

#[async_trait]
impl SandboxExecutor for FakeSandbox {
    async fn execute(
        &self,
        grant: SandboxGrant,
        request: ExecutionRequest,
    ) -> Result<ExecutionResult, SandboxError> {
        let mut s = self.state.lock().unwrap();
        let iso = s.isolation;

        // ---- H1：独立复核，不信任调用方 ----
        let Some((bound, expires_at)) = s.grants.get(&grant.grant_id).cloned() else {
            return Err(SandboxError::InvalidGrant);
        };
        if bound != request.input_hash {
            return Ok(Self::reject(
                request.execution_id,
                RejectReason::InputHashMismatch,
                iso,
            ));
        }
        if s.now > expires_at {
            return Ok(Self::reject(
                request.execution_id,
                RejectReason::GrantExpired,
                iso,
            ));
        }
        if !s.consumed.insert(grant.grant_id.clone()) {
            return Ok(Self::reject(
                request.execution_id,
                RejectReason::GrantAlreadyConsumed,
                iso,
            ));
        }

        // 隔离级别达不到要求时失败，**不得静默降级**。
        if iso < request.required_isolation {
            return Ok(Self::reject(
                request.execution_id,
                RejectReason::IsolationUnavailable,
                iso,
            ));
        }

        s.executed.push(request.clone());

        let scripted = if s.script.is_empty() {
            ScriptedExecution::Succeed { exit_code: 0 }
        } else {
            s.script.remove(0)
        };

        match scripted {
            ScriptedExecution::Succeed { exit_code } => Ok(ExecutionResult {
                execution_id: request.execution_id,
                outcome: ExecutionOutcome::Completed { exit_code },
                effective_isolation: iso,
                artifacts: vec![],
                output: None,
                change_set: Some(request.change_set_id),
                finished_at: Timestamp(0),
            }),
            ScriptedExecution::TimeOut => Ok(ExecutionResult {
                execution_id: request.execution_id,
                outcome: ExecutionOutcome::TimedOut,
                effective_isolation: iso,
                artifacts: vec![],
                output: None,
                change_set: None,
                finished_at: Timestamp(0),
            }),
            ScriptedExecution::CrashThen(status) => {
                // 模拟"执行了但没返回"——内核只能靠 reconcile 弄清真相。
                s.crashed.insert(request.execution_id.clone(), status);
                Err(SandboxError::Unavailable)
            }
        }
    }

    async fn cancel(&self, _execution_id: ExecutionId) -> Result<(), SandboxError> {
        Ok(())
    }

    async fn reconcile(&self, execution_id: ExecutionId) -> Result<ExecutionStatus, SandboxError> {
        let s = self.state.lock().unwrap();
        if let Some(st) = s.crashed.get(&execution_id) {
            return Ok(st.clone());
        }
        // 从未见过的执行 = 确实没开始。这是唯一允许重试的情形之一。
        if !s.executed.iter().any(|r| r.execution_id == execution_id) {
            return Ok(ExecutionStatus::NotStarted);
        }
        Ok(ExecutionStatus::Unknown)
    }
}
