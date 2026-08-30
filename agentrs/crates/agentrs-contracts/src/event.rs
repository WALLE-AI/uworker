//! RunEvent：AgentRS 的权威 durable facts 流与唯一对外语义输出（架构 §4.3）。
//!
//! 事件分三类，保留策略不同：
//!
//! | 类别 | 示例 | 规则 |
//! |---|---|---|
//! | Durable facts | StepIntent、审批、StepResult、终态 | 必须追加持久化，是恢复与回放依据 |
//! | Durable content refs | assistant 消息、工具结果、摘要 | 保存 hash 与来源范围，正文可加密存储 |
//! | Live stream | TextDelta、frame timing | 可采样或丢失 |

use serde::{Deserialize, Serialize};

use crate::ids::{
    ApprovalToken, EventId, EventRange, EventSequence, LiveSequence, OperationId, RequestId, RunEpoch, RunId,
    ScopeId, StepId, Timestamp, ToolCallId, TurnId,
};
use crate::surface::SurfaceMarker;
use crate::{StepIntent, StepResult};

/// 事件的持久性类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Durability {
    /// 必须持久化，参与恢复与回放。
    DurableFact,
    /// 持久化引用，正文可由 Core 另行存储。
    DurableContentRef,
    /// 进程内流，可采样或丢失。
    LiveStream,
}

/// 事件的可见性。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    /// 可展示给用户。
    User,
    /// 仅诊断。
    Diagnostic,
    /// 内部。
    Internal,
}

/// 因果关联字段。需要建立因果关系的事件携带它们。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Causality {
    /// 追踪标识。
    pub trace_id: Option<String>,
    /// 父事件。live 事件靠它挂到最近的 durable 锚点。
    pub parent_event_id: Option<EventId>,
    /// 所属 Turn。
    pub turn_id: Option<TurnId>,
    /// 所属 Step。
    pub step_id: Option<StepId>,
    /// 所属 operation。
    pub operation_id: Option<OperationId>,
    /// 所属 scope。
    pub scope_id: Option<ScopeId>,
    /// **跨 Run 因果**：指向源 Run 的确切位置。
    ///
    /// 有了它，Trajectory 才能回答团队场景里最重要的那个问题——
    /// "这个成员为什么这么做"（架构 §11.3.3）。
    pub cross_run: Option<crate::external::ExternalCausality>,
}

/// 事件信封。
///
/// `event_id` 是 **durable 写入的幂等键**；`seq` 由存储侧分配。
/// 消费端去重键是 `(run_id, event_id)`——用 `seq` 去重会在重试分配新序号时失效。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunEventEnvelope {
    /// 所属 Run。
    pub run_id: RunId,
    /// 写者围栏。存储侧拒绝旧 epoch。
    pub epoch: RunEpoch,
    /// 幂等键。重复投递必须命中同一条记录。
    pub event_id: EventId,
    /// durable 全序序号。live 事件为 `None`。
    pub seq: Option<EventSequence>,
    /// live 局部序号。durable 事件为 `None`。
    pub live_seq: Option<LiveSequence>,
    /// 逻辑时刻。
    pub at: Timestamp,
    /// 持久性类别。
    pub durability: Durability,
    /// 可见性。
    pub visibility: Visibility,
    /// 因果字段。
    #[serde(default)]
    pub causality: Causality,
    /// Surface 标记。仅当该事件进入模型可见平面时存在。
    pub surface: Option<SurfaceMarker>,
    /// 事件载荷。**未知类型必须可安全忽略。**
    pub payload: EventPayload,
}

impl RunEventEnvelope {
    /// 是否为 durable 事实（参与恢复）。
    pub fn is_durable(&self) -> bool {
        matches!(
            self.durability,
            Durability::DurableFact | Durability::DurableContentRef
        )
    }

    /// 是否进入模型可见 Surface。
    pub fn is_surface(&self) -> bool {
        self.surface.is_some()
    }
}

/// 事件载荷。
///
/// 使用 `#[serde(other)]` 兜底变体，保证**未知事件类型不破坏投影**——
/// 这是版本兼容的基础（T01 验收标准）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum EventPayload {
    // ---- 生命周期 ----
    /// Run 开始。
    RunStarted,
    /// Turn 开始。
    TurnStarted,
    /// Turn 关闭。**没有任何欠账**（工具不欠请求、inbox 无待认领输入）之后发出。
    /// 花 0 个 Step 的 Turn 同样有它——被拒绝的 claim 必须留痕。
    TurnEnded,
    /// Step 开始。
    StepStarted,
    /// Step 结束。
    StepEnded,
    /// checkpoint 已保存。
    Checkpointed,

    // ---- 输入与模式 ----
    /// 从 inbox 认领了输入。**被拒绝的 claim 也要写，输入不得凭空消失。**
    UserInputClaimed,
    /// 认领的输入已进入对话。
    UserInputSubmitted,
    /// 权限模式变更。同时是缓存前缀断点。
    PermissionModeChanged,

    // ---- 模型 ----
    /// 模型请求已装配。
    ModelRequestPrepared {
        /// 与 `ModelRequestManifest` 一一对应。
        request_id: RequestId,
    },
    /// 请求构成已经完整记录。必须先于 [`Self::ModelRequestPrepared`] 持久化。
    ModelRequestManifestRecorded {
        /// 可离线重建同一请求的清单。
        manifest: Box<crate::manifest::ModelRequestManifest>,
    },
    /// **已开始产出可见输出**。
    ///
    /// `TextDelta` 是 live 的、可丢失的，因此"崩溃前有没有给用户看到过东西"
    /// 这个问题**无法**从 delta 的有无来回答。恢复时若不能回答它，就只能在
    /// "静默重发"（可能重复内容）与"一律放弃"（可能丢失工作）之间二选一。
    ///
    /// 因此在**首个可见增量**处写这一条 durable 事实，每个 Step 至多一条。
    /// 架构 §4.3 所说的"明确 partial record"就是它。
    PartialOutputStarted,
    /// 助手消息已提交。
    AssistantMessage,
    /// 一条可离线重建的 Surface 消息。
    ///
    /// 其 append/replace 语义与种类由事件信封的 `surface` marker 携带。
    SurfaceMessageRecorded {
        /// 模型可见消息本体的结构化表示。
        message: serde_json::Value,
    },
    /// 文本增量（live，可丢）。
    TextDelta {
        /// 可直接展示的文本片段。旧版本 unit 事件反序列化为空片段。
        #[serde(default)]
        text: String,
    },
    /// 思考增量（live，可丢）。
    ThinkingDelta,
    /// 用量更新。
    UsageUpdated,

    // ---- 工具 ----
    /// 模型提出工具调用。
    ToolProposed {
        /// 提出的调用。
        ///
        /// **必须带上**：`Causality` 只到 Step 一级，而一个 Step 里可以有
        /// 多次调用。没有它，投影只能拿 step_id 兜底，于是同一次调用被拆成
        /// 两条记录——一条"有提议无结果"、一条"有结果无提议"，
        /// 前者会被误报成待 reconcile 的悬挂意图。
        call_id: ToolCallId,
    },
    /// Hook 结论已记录。
    HookOutcomeRecorded {
        /// 相关调用。
        call_id: ToolCallId,
    },
    /// 副作用意图已落盘。**跨出有副作用边界前必须有它。**
    StepIntentRecorded {
        /// 意图本体。恢复时凭 `execution_id` 向 Sandbox `reconcile`。
        intent: Box<StepIntent>,
    },
    /// 请求审批。
    ApprovalRequested {
        /// 关联的调用。
        call_id: ToolCallId,
    },
    /// 审批超时，转入挂起。
    ApprovalTimedOut {
        /// 恢复令牌。凭它继续同一个 `StepIntent`。
        token: ApprovalToken,
        /// 关联的调用。
        call_id: ToolCallId,
    },
    /// 工具开始执行。
    ToolStarted {
        /// 开始执行的调用。
        call_id: ToolCallId,
    },
    /// 工具执行结果已提交。**写入后绝不再次执行。**
    StepResultRecorded {
        /// 结果本体。恢复时直接回灌。
        result: Box<StepResult>,
    },
    /// 产生了可用的 ChangeSet。
    ChangeSetAvailable,
    /// 产出物已创建。
    ArtifactCreated,

    // ---- 上下文 ----
    /// 上下文来源已选定。
    ContextSelected,
    /// 成功解引用并进入模型上下文的内容引用。
    ContextContentAttached {
        /// 仅包含成功解析的引用；Forbidden / NotFound 不得进入。
        refs: Vec<crate::content::ContentRef>,
    },
    /// 内容引用解析失败，已降级。
    ContentRefUnresolved,
    /// 历史合法化已执行。
    HistoryLegalized,
    /// 观测到缓存断裂。
    CacheBreakObserved,
    /// 压缩开始。
    CompactionStarted,
    /// 压缩完成。
    CompactionCompleted {
        /// 被摘要的来源区间。**复用摘要，不重新压缩同一范围。**
        source_range: EventRange,
    },

    // ---- 跨 Run ----
    /// 收到来自本 Run 之外的事实。
    ExternalFactReceived,
    /// 任务板快照已附着。
    BoardSnapshotAttached,
    /// 子运行摘要。
    SubagentSummary,

    // ---- 恢复与终态 ----
    /// checkpoint 已迁移。
    CheckpointMigrated,
    /// Run 正常完成。
    RunCompleted,
    /// Run 失败。
    RunFailed,
    /// Run 被取消。
    RunCanceled,
    /// Run 需要用户介入（审批挂起、未知副作用、预算耗尽）。
    RunNeedsUserAction,

    /// 未知事件。**必须可安全忽略**，不得破坏投影。
    #[serde(other)]
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn 信封(durability: Durability, payload: EventPayload) -> RunEventEnvelope {
        RunEventEnvelope {
            run_id: "r1".into(),
            epoch: RunEpoch(1),
            event_id: "e1".into(),
            seq: Some(EventSequence(1)),
            live_seq: None,
            at: Timestamp(0),
            durability,
            visibility: Visibility::User,
            causality: Causality::default(),
            surface: None,
            payload,
        }
    }

    #[test]
    fn 未知事件类型可安全降级() {
        let json = r#"{"type":"some_future_event","extra":123}"#;
        let payload: EventPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload, EventPayload::Unknown, "未知类型必须降级而非报错");
    }

    #[test]
    fn live_事件不参与恢复() {
        let e = 信封(
            Durability::LiveStream,
            EventPayload::TextDelta { text: "x".into() },
        );
        assert!(!e.is_durable());
    }

    #[test]
    fn durable_与_live_序号互斥填充() {
        let mut e = 信封(Durability::DurableFact, EventPayload::RunStarted);
        assert!(e.seq.is_some() && e.live_seq.is_none());

        e.durability = Durability::LiveStream;
        e.seq = None;
        e.live_seq = Some(LiveSequence(1));
        assert!(e.seq.is_none() && e.live_seq.is_some());
    }

    #[test]
    fn 事件信封可_round_trip() {
        let e = 信封(Durability::DurableFact, EventPayload::PartialOutputStarted);
        let json = serde_json::to_string(&e).unwrap();
        let back: RunEventEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(back, e);
    }

    #[test]
    fn 幂等键是_event_id_不是_seq() {
        let a = 信封(Durability::DurableFact, EventPayload::RunStarted);
        let mut b = a.clone();
        b.seq = Some(EventSequence(99)); // 重试导致存储侧分配了新序号
        assert_eq!(a.event_id, b.event_id, "去重必须靠 event_id，否则重试会写入两条");
    }

    #[test]
    fn 旧版无正文_text_delta_仍可读取() {
        let payload: EventPayload = serde_json::from_str(r#"{"type":"text_delta"}"#).unwrap();
        assert_eq!(payload, EventPayload::TextDelta { text: String::new() });
    }
}
