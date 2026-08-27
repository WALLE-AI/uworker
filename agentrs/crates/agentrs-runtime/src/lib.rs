//! # AgentRS Runtime
//!
//! 内核状态机：RuntimeHost、AgentEngine、ModelSurface 投影、生命周期与恢复。
//!
//! ## 当前进度
//!
//! - ✅ [`surface`] —— ModelSurface 投影（T22，平面 ②）
//! - ✅ [`composition`] —— scope / owner / 有序清理（T03A，平面 ③）
//! - ✅ [`inbox`] —— inbox / claim / steering（T04A）
//! - ✅ [`engine`] —— Turn / Step 状态机（T04）
//! - ✅ [`host`] —— RuntimeHost / RunHandle / epoch 分配（T04）
//! - ✅ [`toolround`] —— 固定管线 + 审批有界等待与挂起（T04B）
//! - ✅ [`recovery`] —— RecoveryPlanner，六个 durable 边界（T10）
//! - ✅ [`invariant`] —— 运行时不变式：模型可见即已记录（T22A）
//! - ✅ [`fork`] —— 对话分叉（T23）
//! - ✅ [`schedule`] —— 工具并发调度与 ordering barrier（T09A）
//! - ✅ [`permission`] —— PermissionMode 与 Plan Mode（§4.1.2）
//! - ✅ 压缩落地 —— `Engine::apply_compaction` 追加 `Replace` 节点（§9.3）
//! - ⬜ 并发调度 / 压缩 / Projection Registry

#![forbid(unsafe_code)]

pub mod composition;
pub mod engine;
pub mod fork;
pub mod host;
pub mod inbox;
pub mod invariant;
pub mod permission;
pub mod recovery;
pub mod schedule;
pub mod surface;
pub mod toolround;

pub use fork::{fork, ForkError, Forked};

pub use invariant::{InvariantViolation, Invariants};

pub use recovery::{decide, plan, ReconcileDecision, RecoveryPlan};

pub use toolround::{execute_call, input_hash, ProposedCall, ToolRoundCtx, ToolRoundDeps, ToolRoundOutcome};

pub use host::{CancelReason, RunHandle, RuntimeHost, StartError, StartedRun};

pub use engine::{
    AdmitAll, CancelToken, Engine, EngineDeps, FixedClock, RunSummary, StepAdmission, StepDriver,
    StepOutcome, Termination, TurnGuards,
};
pub use inbox::{Claim, Inbox, InputAccepted, PreStepDecision, SubmitError, UserInput};

pub use composition::{
    AsyncCleanup, CleanupError, CleanupOutcome, LifecycleState, RegisterError, ResourceOwner,
    SetupTransaction, ShutdownReport,
};

pub use surface::{cache_invalidation_point, derive_messages, derive_transcript, SurfaceNode};
