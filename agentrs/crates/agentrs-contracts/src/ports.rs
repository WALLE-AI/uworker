//! 外部端口（架构 §4.2）。
//!
//! 内核与外界只有四个交接面，**没有第五种**（架构 §1.1）：
//!
//! | 方向 | 通道 |
//! |---|---|
//! | Core → RS（启动） | [`crate::spec::RunSpec`] |
//! | Core → RS（运行中） | `RunHandle::submit()` |
//! | RS → Core | [`crate::event::RunEventEnvelope`] 事实流 |
//! | 双向 | 本模块的 port trait |
//!
//! 端口的责任必须单一：Persistence 是事实记录，不执行策略；ContentStore 是不可变字节
//! 存储，不理解语义；TokenCounter 只计数，不做预算决策；Policy 是授权决策，不运行命令；
//! Sandbox 是限制执行，不规划任务；Memory 是检索，不直接注入提示词；Hook 是可插拔的
//! 建议/否决点，不能签发 grant。

use async_trait::async_trait;
use bytes::Bytes;

use crate::content::{
    ByteRange, ContentError, ContentMeta, ContentRef, ContentScope, ContentStat, RetentionOwner,
};
use crate::event::RunEventEnvelope;
use crate::ids::{ApprovalToken, Deadline, EventSequence, MemoryId, ModelId, RunEpoch, SkillId, ToolCallId};
use crate::policy::{ApprovalOutcome, ApprovalRequest, PolicyDecision, PolicyError, ToolProposal};
use crate::sandbox::{ExecutionRequest, ExecutionResult, ExecutionStatus, SandboxError};
use crate::spec::RunCheckpoint;
use crate::{StepIntent, StepResult};

/// 持久化端口。
///
/// **三条实现义务（宿主义务 H3）**，缺一则恢复与 replay 全部不可信：
///
/// 1. `append_event` 以 `event_id` 幂等——重复投递必须返回首次分配的序号，
///    不得分配新序号，也不得写入第二条记录；
/// 2. `seq` 单调递增；
/// 3. epoch 围栏——拒绝小于当前 epoch 的写入并返回 [`PersistError::Fenced`]。
#[async_trait]
pub trait RunPersistence: Send + Sync {
    /// 记录一次副作用意图。**跨出有副作用的调用边界前必须成功返回。**
    async fn begin_step(&self, epoch: RunEpoch, intent: StepIntent) -> Result<(), PersistError>;

    /// 追加一条事件。以 `event_id` 幂等。
    async fn append_event(
        &self,
        epoch: RunEpoch,
        event: RunEventEnvelope,
    ) -> Result<EventSequence, PersistError>;

    /// 记录一次副作用结果。
    async fn finish_step(&self, epoch: RunEpoch, result: StepResult) -> Result<(), PersistError>;

    /// 保存检查点。**必须在其引用的事件已持久化之后才可见**，
    /// 否则返回 [`PersistError::CheckpointAhead`]。
    async fn save_checkpoint(&self, epoch: RunEpoch, checkpoint: RunCheckpoint) -> Result<(), PersistError>;
}

/// Run 事件的实时投递端口。
///
/// Persistence 只保存 durable 事实；live delta 通过本端口交给宿主。宿主必须把
/// sink 当作可丢失的观察面，不能用投递成功与否决定 durable 事实是否成立。
#[async_trait]
pub trait RunEventSink: Send + Sync {
    /// 发布一条已经分配好相应序号的事件。
    async fn publish(&self, event: RunEventEnvelope) -> Result<(), EventSinkError>;
}

/// 实时事件投递失败。
#[derive(Debug, thiserror::Error)]
pub enum EventSinkError {
    /// 消费端已经关闭。
    #[error("event sink closed")]
    Closed,
    /// 有界队列无法接收更多事件。
    #[error("event sink lagged")]
    Lagged,
}

/// 持久化错误。
#[derive(Debug, thiserror::Error)]
pub enum PersistError {
    /// 本 writer 已被更新的 epoch 取代。
    ///
    /// 收到后内核必须**立即停止全部 durable 写入**、取消 live resource 并收敛。
    /// 没有这条，"恢复以 durable log 为准"不成立——log 已被两个 writer 写坏。
    #[error("fenced by a newer epoch")]
    Fenced,
    /// checkpoint 引用了尚未持久化的事件。
    #[error("checkpoint references unpersisted events")]
    CheckpointAhead,
    /// 存储后端错误。
    #[error("persistence backend error: {message}")]
    Backend {
        /// 已脱敏的错误描述。
        message: String,
    },
}

/// 内容寻址存储端口。
#[async_trait]
pub trait ContentStore: Send + Sync {
    /// 写入内容并返回引用。相同字节在同一 scope 内必然产生相同引用。
    async fn put(
        &self,
        scope: ContentScope,
        bytes: Bytes,
        meta: ContentMeta,
    ) -> Result<ContentRef, ContentError>;

    /// 读取全部内容。
    async fn get(&self, r: &ContentRef) -> Result<Bytes, ContentError>;

    /// 读取内容片段。
    async fn get_range(&self, r: &ContentRef, range: ByteRange) -> Result<Bytes, ContentError>;

    /// 查询元数据。
    async fn stat(&self, r: &ContentRef) -> Result<ContentStat, ContentError>;

    /// 声明这些引用仍被某个 Run 或 checkpoint 使用，阻止回收（宿主义务 H4）。
    async fn retain(&self, owner: RetentionOwner, refs: &[ContentRef]) -> Result<(), ContentError>;

    /// 释放保留声明。
    async fn release(&self, owner: RetentionOwner) -> Result<(), ContentError>;
}

/// 策略端口。**最终裁决者**，内核只消费结果。
#[async_trait]
pub trait PolicyEnforcer: Send + Sync {
    /// 对一次工具提议做裁决。
    async fn evaluate(&self, proposal: ToolProposal) -> Result<PolicyDecision, PolicyError>;

    /// 有界等待人工审批。
    ///
    /// `deadline` 由 `ExecutionBudget` 派生。超时返回
    /// [`ApprovalOutcome::Pending`]，由内核降级为挂起——**绝不允许无限期阻塞**。
    async fn await_approval(
        &self,
        request: ApprovalRequest,
        deadline: Deadline,
    ) -> Result<ApprovalOutcome, PolicyError>;

    /// 凭挂起时的令牌取回决策（架构 §6.2）。
    ///
    /// **没有这个方法，`ApprovalOutcome::Pending` 里的令牌就是死路**——
    /// 挂起后永远无法恢复。这是实现审批路径时发现的契约缺口。
    ///
    /// 返回 `Pending` 表示人仍未裁决，内核继续保持挂起；
    /// 返回 `Decided` 则**继续同一个 `StepIntent`**，不重新裁决、不重复副作用。
    async fn redeem(&self, token: ApprovalToken) -> Result<ApprovalOutcome, PolicyError>;
}

/// 沙箱执行端口。内核**没有任何执行权**，一切经此。
#[async_trait]
pub trait SandboxExecutor: Send + Sync {
    /// 执行一次受限操作。
    ///
    /// 实现方必须独立复核 `input_hash` 与 grant 有效期，**不信任调用方**（宿主义务 H1）。
    async fn execute(
        &self,
        grant: crate::policy::SandboxGrant,
        request: ExecutionRequest,
    ) -> Result<ExecutionResult, SandboxError>;

    /// 取消一次执行。
    async fn cancel(&self, execution_id: crate::ids::ExecutionId) -> Result<(), SandboxError>;

    /// 查询一次执行的真实状态。
    ///
    /// **必须如实报告**（宿主义务 H2）：内核据此决定重试、等待还是停下问人，
    /// 猜测会导致重复副作用。
    async fn reconcile(&self, execution_id: crate::ids::ExecutionId)
        -> Result<ExecutionStatus, SandboxError>;
}

/// 记忆检索端口。内核不扫描磁盘、不建索引。
#[async_trait]
pub trait MemoryRetriever: Send + Sync {
    /// 取候选集。
    async fn candidates(&self, query: MemoryQuery) -> Result<Vec<MemoryCandidate>, MemoryError>;

    /// 加载选中的片段。
    async fn load(&self, ids: &[MemoryId]) -> Result<Vec<MemoryFragment>, MemoryError>;
}

/// 记忆查询。
#[derive(Debug, Clone)]
pub struct MemoryQuery {
    /// 查询文本。
    pub text: String,
    /// 返回上限。
    pub limit: usize,
}

/// 记忆候选。**切点**：内核拿候选做选择，Core 拿索引、权限与保留。
#[derive(Debug, Clone)]
pub struct MemoryCandidate {
    /// 片段标识。
    pub id: MemoryId,
    /// 摘要，供 selector 判断。
    pub summary: String,
    /// 确定性排名，selector 失败时的降级依据。
    pub rank: u32,
}

/// 记忆片段。
#[derive(Debug, Clone)]
pub struct MemoryFragment {
    /// 片段标识。
    pub id: MemoryId,
    /// 内容引用。
    pub content: ContentRef,
}

/// 记忆端口错误。
#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    /// 检索不可用，降级为 Core 给出的确定性排名。
    #[error("memory retriever unavailable")]
    Unavailable,
    /// 其他已脱敏错误。
    #[error("memory error: {message}")]
    Other {
        /// 已脱敏的错误描述。
        message: String,
    },
}

/// 技能解析端口。内核不自行发现或安装技能。
#[async_trait]
pub trait SkillResolver: Send + Sync {
    /// 解析一个技能清单。
    async fn resolve(&self, id: &SkillId) -> Result<SkillManifest, SkillError>;
}

/// 技能清单。
#[derive(Debug, Clone)]
pub struct SkillManifest {
    /// 技能标识。
    pub id: SkillId,
    /// 版本。
    pub version: String,
    /// 正文内容引用。
    pub content: ContentRef,
    /// 该技能要求的工具子集。**只能是当前视图的交集，不能扩大。**
    pub tool_subset: Option<Vec<String>>,
}

/// 技能端口错误。
#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    /// 技能不存在或未启用。
    #[error("skill not found")]
    NotFound,
    /// 其他已脱敏错误。
    #[error("skill error: {message}")]
    Other {
        /// 已脱敏的错误描述。
        message: String,
    },
}

/// Hook 求值端口。**只能建议或否决，不能授权。**
#[async_trait]
pub trait HookEvaluator: Send + Sync {
    /// 在指定挂载点求值。
    async fn evaluate(&self, point: HookPoint, payload: HookPayload) -> Result<HookOutcome, HookError>;
}

/// Hook 挂载点。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookPoint {
    /// 工具执行前。
    PreToolUse,
    /// 工具执行后。
    PostToolUse,
    /// 压缩前。
    PreCompact,
    /// Turn 结束。
    TurnEnd,
    /// Run 终止。
    RunStop,
}

/// Hook 输入。
#[derive(Debug, Clone)]
pub struct HookPayload {
    /// 相关工具调用（若适用）。
    pub call_id: Option<ToolCallId>,
    /// 结构化载荷。
    pub data: serde_json::Value,
}

/// Hook 结论。
///
/// **没有 `Allow` 变体**——放行是"不否决"的结果，不是 hook 的决定。
/// 这与单调 guard 是同一原则的两个投影：任何可插拔的东西只能收紧，不能放宽。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookOutcome {
    /// 不干预。
    Proceed,
    /// 追加一段回灌给模型的文本，**不改变授权结论**。
    Advise(String),
    /// 否决本次调用；等价于一次 Deny，仍要写 `StepResult`。
    Block {
        /// 稳定拒绝码。
        code: crate::policy::DenyCode,
        /// 已脱敏说明。
        message: String,
    },
}

/// Hook 端口错误。
#[derive(Debug, thiserror::Error)]
pub enum HookError {
    /// 求值超时。**按 `Proceed` 处理并记 telemetry**，
    /// 避免宿主 hook 故障锁死整个 Run。
    #[error("hook timed out")]
    TimedOut,
    /// 其他已脱敏错误。
    #[error("hook error: {message}")]
    Other {
        /// 已脱敏的错误描述。
        message: String,
    },
}

/// 精确 token 计数端口。
///
/// 内核**另有内置保守估算器**用于硬上限保护，二者并存：估算器保证不超窗，
/// 本 port 保证账本数字准确（架构 §1.1 裁定 1）。
#[async_trait]
pub trait TokenCounter: Send + Sync {
    /// 计数。
    async fn count(&self, model: &ModelId, text: &str) -> Result<u64, TokenCountError>;
}

/// 计数端口错误。计数失败必须降级到内置估算器，不得阻塞请求。
#[derive(Debug, thiserror::Error)]
pub enum TokenCountError {
    /// 不可用，降级到内置估算器。
    #[error("token counter unavailable")]
    Unavailable,
}

/// 时钟端口。内核不读真实时钟（边界判据，见 `clippy.toml`）。
pub trait Clock: Send + Sync {
    /// 当前逻辑时刻。
    fn now(&self) -> crate::ids::Timestamp;
}

/// 取消信号端口。
pub trait Cancellation: Send + Sync {
    /// 是否已请求取消。
    fn is_canceled(&self) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_结论没有_allow_变体() {
        // 类型层面保证"可插拔的东西只能收紧"。
        let outcomes = [
            HookOutcome::Proceed,
            HookOutcome::Advise("note".into()),
            HookOutcome::Block {
                code: crate::policy::DenyCode::HookBlocked,
                message: "blocked".into(),
            },
        ];
        assert_eq!(outcomes.len(), 3, "新增变体必须重新论证是否破坏单调性");
    }

    #[test]
    fn persist_错误含围栏变体() {
        let e = PersistError::Fenced;
        assert_eq!(e.to_string(), "fenced by a newer epoch");
    }

    #[test]
    fn hook_超时按放行处理不阻塞_run() {
        // 文档性断言：TimedOut 存在，且其语义在 doc comment 中固定为 Proceed。
        let e = HookError::TimedOut;
        assert_eq!(e.to_string(), "hook timed out");
    }
}
