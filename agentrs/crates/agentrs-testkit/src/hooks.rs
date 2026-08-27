//! 可编排的 Hook 求值器。
//!
//! 用于验证一条纪律：**Hook 只能收紧**。因此这个 fake 刻意做成
//! "只能返回 `HookOutcome` 的三个变体"——想让它放行一个被 Policy 拒绝的调用，
//! 在类型上就写不出来。

use std::sync::Mutex;

use agentrs_contracts::ports::{HookError, HookEvaluator, HookOutcome, HookPayload, HookPoint};
use async_trait::async_trait;

/// 按挂载点编排结论的 Hook 求值器。
#[derive(Default)]
pub struct FakeHooks {
    pre: Mutex<Option<HookOutcome>>,
    post: Mutex<Option<HookOutcome>>,
    /// 求值时是否直接报错（模拟宿主 hook 故障）。
    fail: Mutex<bool>,
    calls: Mutex<Vec<HookPoint>>,
}

impl FakeHooks {
    /// 全部挂载点返回 `Proceed`。
    pub fn proceed() -> Self {
        Self::default()
    }

    /// 编排 `PreToolUse` 的结论。
    pub fn with_pre(self, outcome: HookOutcome) -> Self {
        *self.pre.lock().expect("poisoned") = Some(outcome);
        self
    }

    /// 编排 `PostToolUse` 的结论。
    pub fn with_post(self, outcome: HookOutcome) -> Self {
        *self.post.lock().expect("poisoned") = Some(outcome);
        self
    }

    /// 让求值直接失败。内核必须按 `Proceed` 处理，而不是卡死或放大成 Run 失败。
    pub fn failing(self) -> Self {
        *self.fail.lock().expect("poisoned") = true;
        self
    }

    /// 被求值过的挂载点，按顺序。
    pub fn calls(&self) -> Vec<HookPoint> {
        self.calls.lock().expect("poisoned").clone()
    }
}

#[async_trait]
impl HookEvaluator for FakeHooks {
    async fn evaluate(&self, point: HookPoint, _payload: HookPayload) -> Result<HookOutcome, HookError> {
        self.calls.lock().expect("poisoned").push(point);
        if *self.fail.lock().expect("poisoned") {
            return Err(HookError::TimedOut);
        }
        let slot = match point {
            HookPoint::PreToolUse => &self.pre,
            HookPoint::PostToolUse => &self.post,
            _ => return Ok(HookOutcome::Proceed),
        };
        Ok(slot
            .lock()
            .expect("poisoned")
            .clone()
            .unwrap_or(HookOutcome::Proceed))
    }
}
