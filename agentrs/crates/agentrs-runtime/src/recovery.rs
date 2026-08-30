//! RecoveryPlanner：五个 durable 边界（架构 §6.1，任务 T10）。
//!
//! **原则：宁可停下问人，也不重复未知副作用。**
//!
//! 恢复分两步，本模块是第一步：
//!
//! 1. **规划**（本模块）——对 durable log 做**纯折叠**，得出"停在哪个边界"。
//!    纯函数、无 I/O，因此同一前缀必然得出同一结论，可完整离线测试。
//! 2. **执行**——按结论去 `reconcile` 或 `redeem`，再把外部答复折叠成最终决定。
//!
//! ## 只有两种情况允许重试
//!
//! - 模型在**无可见输出**前的瞬时失败；
//! - Sandbox 明确返回 `NotStarted`。
//!
//! 其余一律 `reconcile` 或停在 `NeedsUserAction`。任何"未知副作用、部分写入、
//! 外部系统超时"都不得由内核自行重放。

use agentrs_contracts::event::{EventPayload, RunEventEnvelope};
use agentrs_contracts::ids::{ApprovalToken, EventRange, RequestId, ToolCallId};
use agentrs_contracts::sandbox::ExecutionStatus;
use agentrs_contracts::{StepIntent, StepResult};

/// 折叠 durable log 后得出的恢复计划。
#[derive(Debug, Clone, PartialEq)]
pub enum RecoveryPlan {
    /// 无未完成边界，从新 Turn 开始。
    Fresh,

    /// 停在「模型请求前」，且**尚未产出可见输出**。
    ///
    /// manifest 可由 durable 来源重建，因此**允许重试同一请求**。
    RetryModelRequest {
        /// 待重试的请求。
        request_id: RequestId,
    },

    /// 停在「模型请求前」，但**已产出可见输出**却无 committed assistant 消息。
    ///
    /// **禁止静默重发**——用户已经看到了一部分内容，再发一遍会重复。
    /// 按已记录的 partial-output policy 结束、续接或请求用户确认。
    ResolvePartialOutput {
        /// 相关请求。
        request_id: RequestId,
    },

    /// 停在「工具执行前」：意图已落盘但无结果。
    ///
    /// **必须先 `reconcile`，不得盲目重跑。**
    ReconcileExecution {
        /// 待核对的意图。
        intent: Box<StepIntent>,
    },

    /// 停在「审批已挂起」。凭令牌兑现，**继续同一个意图**。
    RedeemApproval {
        /// 恢复令牌。
        token: ApprovalToken,
        /// 挂起时的意图。
        intent: Box<StepIntent>,
    },

    /// 停在「等待审批」但未记录超时——崩溃发生在等待期间。
    ///
    /// 审批请求本身是 durable 的，但没有令牌可兑现，只能重新发起审批。
    ReissueApproval {
        /// 关联调用。
        call_id: ToolCallId,
        /// 相关意图。
        intent: Box<StepIntent>,
    },

    /// 停在「压缩完成」之后：摘要已存在。**复用它，不重新压缩同一范围。**
    ReuseCompaction {
        /// 已完成摘要的来源区间。
        source_range: EventRange,
    },

    /// Run 已抵达终态，无需恢复。
    AlreadyTerminal,
}

/// 折叠 durable log，得出恢复计划。
///
/// **纯函数**：只读事件、不做 I/O、不调用端口。相同前缀必然得出相同结论。
///
/// `events` 应为按 `seq` 升序的 durable 事件。live 事件（`TextDelta` 等）
/// 即便存在也会被忽略——它们不参与恢复。
pub fn plan(events: &[RunEventEnvelope]) -> RecoveryPlan {
    let mut pending_request: Option<RequestId> = None;
    let mut visible_output = false;
    let mut pending_intent: Option<Box<StepIntent>> = None;
    let mut awaiting_approval: Option<ToolCallId> = None;
    let mut suspended: Option<(ApprovalToken, ToolCallId)> = None;
    let mut last_compaction: Option<EventRange> = None;
    let mut compaction_open = false;
    let mut terminal = false;

    for e in events.iter().filter(|e| e.is_durable()) {
        match &e.payload {
            EventPayload::ModelRequestPrepared { request_id } => {
                pending_request = Some(request_id.clone());
                visible_output = false;
            }
            EventPayload::PartialOutputStarted => visible_output = true,
            EventPayload::AssistantMessage => {
                pending_request = None;
                visible_output = false;
            }

            EventPayload::StepIntentRecorded { intent } => {
                pending_intent = Some(intent.clone());
            }
            EventPayload::StepResultRecorded { .. } => {
                // 结果已提交 —— 该意图彻底了结，绝不再次执行。
                pending_intent = None;
                awaiting_approval = None;
                suspended = None;
            }

            EventPayload::ApprovalRequested { call_id } => {
                awaiting_approval = Some(call_id.clone());
            }
            EventPayload::ApprovalTimedOut { token, call_id } => {
                suspended = Some((token.clone(), call_id.clone()));
                awaiting_approval = None;
            }

            EventPayload::CompactionStarted => compaction_open = true,
            EventPayload::CompactionCompleted { source_range } => {
                last_compaction = Some(*source_range);
                compaction_open = false;
            }

            EventPayload::RunCompleted
            | EventPayload::RunFailed
            | EventPayload::RunCanceled
            | EventPayload::RunNeedsUserAction => terminal = true,

            // 其余事件不影响恢复判定。未知事件同样安全忽略。
            _ => {}
        }
    }

    if terminal {
        return RecoveryPlan::AlreadyTerminal;
    }

    // 优先级：越靠近"已经动过外部世界"的边界越优先处理。
    if let (Some((token, _)), Some(intent)) = (suspended.clone(), pending_intent.clone()) {
        return RecoveryPlan::RedeemApproval { token, intent };
    }
    if let (Some(call_id), Some(intent)) = (awaiting_approval, pending_intent.clone()) {
        return RecoveryPlan::ReissueApproval { call_id, intent };
    }
    if let Some(intent) = pending_intent {
        return RecoveryPlan::ReconcileExecution { intent };
    }
    if let Some(request_id) = pending_request {
        return if visible_output {
            RecoveryPlan::ResolvePartialOutput { request_id }
        } else {
            RecoveryPlan::RetryModelRequest { request_id }
        };
    }
    // 压缩开了头但没完成 —— 摘要不可信，当作没做过。
    if !compaction_open {
        if let Some(source_range) = last_compaction {
            return RecoveryPlan::ReuseCompaction { source_range };
        }
    }
    RecoveryPlan::Fresh
}

/// 拿到 `reconcile` 答复后的最终决定。
#[derive(Debug, Clone, PartialEq)]
pub enum ReconcileDecision {
    /// Sandbox 明确未开始 —— **唯一允许重执行的情形之一**。
    SafeToRetry {
        /// 待重试的意图。
        intent: Box<StepIntent>,
    },
    /// 仍在执行，继续等待 settlement。
    KeepWaiting,
    /// 已完成，直接回灌结果，**绝不再次执行**。
    ReplayResult {
        /// 已有结果。
        result: Box<StepResult>,
    },
    /// 状态未知 —— **必须停下问人**。猜测会造成重复的外部副作用。
    NeedsUserAction {
        /// 稳定原因码。
        reason: &'static str,
    },
}

/// 把 `reconcile` 的三态折叠为决定。
pub fn decide(intent: Box<StepIntent>, status: ExecutionStatus) -> ReconcileDecision {
    match status {
        ExecutionStatus::NotStarted => ReconcileDecision::SafeToRetry { intent },
        ExecutionStatus::Running => ReconcileDecision::KeepWaiting,
        ExecutionStatus::Finished(result) => {
            // 用 Sandbox 报告的真实结果重建 StepResult 的语义外壳。
            ReconcileDecision::ReplayResult {
                result: Box::new(StepResult {
                    step_id: intent.step_id.clone(),
                    call_id: intent.call_id.clone(),
                    outcome: match result.outcome {
                        agentrs_contracts::sandbox::ExecutionOutcome::Completed { exit_code: 0 } => {
                            agentrs_contracts::StepOutcome::Succeeded
                        }
                        agentrs_contracts::sandbox::ExecutionOutcome::Canceled => {
                            agentrs_contracts::StepOutcome::Canceled
                        }
                        other => agentrs_contracts::StepOutcome::Failed {
                            message: format!("{other:?}"),
                        },
                    },
                    effective_isolation: Some(result.effective_isolation),
                    artifacts: result.artifacts,
                    output: None,
                    at: result.finished_at,
                }),
            }
        }
        ExecutionStatus::Unknown => ReconcileDecision::NeedsUserAction {
            reason: "execution_status_unknown",
        },
    }
}

#[cfg(test)]
mod tests {
    use agentrs_contracts::event::{Causality, Durability, Visibility};
    use agentrs_contracts::ids::{Digest, EventSequence, RunEpoch, Timestamp};
    use agentrs_contracts::policy::InputHash;
    use agentrs_contracts::sandbox::{ExecutionOutcome, ExecutionResult, IsolationLevel};
    use agentrs_contracts::StepOutcome;

    use super::*;

    fn 事件(n: u64, payload: EventPayload, d: Durability) -> RunEventEnvelope {
        RunEventEnvelope {
            run_id: "r1".into(),
            epoch: RunEpoch(1),
            event_id: format!("e{n}").into(),
            seq: Some(EventSequence(n)),
            live_seq: None,
            at: Timestamp(0),
            durability: d,
            visibility: Visibility::User,
            causality: Causality::default(),
            surface: None,
            payload,
        }
    }

    fn 事实(n: u64, p: EventPayload) -> RunEventEnvelope {
        事件(n, p, Durability::DurableFact)
    }

    fn 意图() -> Box<StepIntent> {
        Box::new(StepIntent {
            step_id: "s1".into(),
            call_id: "c1".into(),
            tool_name: "Write".into(),
            input_hash: InputHash(Digest::from_hex("h")),
            change_set_id: "cs1".into(),
            execution_id: "e1".into(),
            at: Timestamp(0),
        })
    }

    fn 结果() -> Box<StepResult> {
        Box::new(StepResult {
            step_id: "s1".into(),
            call_id: "c1".into(),
            outcome: StepOutcome::Succeeded,
            effective_isolation: Some(IsolationLevel::L0BasicContainment),
            artifacts: vec![],
            output: None,
            at: Timestamp(0),
        })
    }

    // ---------- 边界 1：模型请求前 ----------

    #[test]
    fn 边界1_请求已装配但无可见输出_可重试() {
        let ev = vec![
            事实(1, EventPayload::RunStarted),
            事实(2, EventPayload::TurnStarted),
            事实(
                3,
                EventPayload::ModelRequestPrepared {
                    request_id: "req1".into(),
                },
            ),
        ];
        assert_eq!(
            plan(&ev),
            RecoveryPlan::RetryModelRequest {
                request_id: "req1".into()
            }
        );
    }

    #[test]
    fn 边界1_已产出可见输出则禁止静默重发() {
        // 用户已经看到了一部分内容，再发一遍会重复。
        let ev = vec![
            事实(
                1,
                EventPayload::ModelRequestPrepared {
                    request_id: "req1".into(),
                },
            ),
            事实(2, EventPayload::PartialOutputStarted),
        ];
        assert_eq!(
            plan(&ev),
            RecoveryPlan::ResolvePartialOutput {
                request_id: "req1".into()
            }
        );
    }

    #[test]
    fn live_delta_不影响判定() {
        // TextDelta 可丢失，因此不能用它回答"有没有可见输出"。
        // 只有 durable 的 PartialOutputStarted 才算数。
        let ev = vec![
            事实(
                1,
                EventPayload::ModelRequestPrepared {
                    request_id: "req1".into(),
                },
            ),
            事件(
                2,
                EventPayload::TextDelta { text: "x".into() },
                Durability::LiveStream,
            ),
        ];
        assert_eq!(
            plan(&ev),
            RecoveryPlan::RetryModelRequest {
                request_id: "req1".into()
            },
            "live delta 不是可见输出的凭据"
        );
    }

    #[test]
    fn assistant_消息落盘后请求边界关闭() {
        let ev = vec![
            事实(
                1,
                EventPayload::ModelRequestPrepared {
                    request_id: "req1".into(),
                },
            ),
            事实(2, EventPayload::PartialOutputStarted),
            事实(3, EventPayload::AssistantMessage),
        ];
        assert_eq!(plan(&ev), RecoveryPlan::Fresh);
    }

    // ---------- 边界 2：工具执行前 ----------

    #[test]
    fn 边界2_意图已落盘无结果_必须先_reconcile() {
        let ev = vec![
            事实(1, EventPayload::AssistantMessage),
            事实(2, EventPayload::StepIntentRecorded { intent: 意图() }),
        ];
        assert_eq!(plan(&ev), RecoveryPlan::ReconcileExecution { intent: 意图() });
    }

    // ---------- 边界 3：等待审批 ----------

    #[test]
    fn 边界3_崩溃于审批等待期间_重新发起审批() {
        let ev = vec![
            事实(1, EventPayload::StepIntentRecorded { intent: 意图() }),
            事实(2, EventPayload::ApprovalRequested { call_id: "c1".into() }),
        ];
        assert_eq!(
            plan(&ev),
            RecoveryPlan::ReissueApproval {
                call_id: "c1".into(),
                intent: 意图()
            }
        );
    }

    // ---------- 边界 4：审批已挂起 ----------

    #[test]
    fn 边界4_已挂起_凭令牌兑现并继续同一意图() {
        let ev = vec![
            事实(1, EventPayload::StepIntentRecorded { intent: 意图() }),
            事实(2, EventPayload::ApprovalRequested { call_id: "c1".into() }),
            事实(
                3,
                EventPayload::ApprovalTimedOut {
                    token: "tok".into(),
                    call_id: "c1".into(),
                },
            ),
        ];
        assert_eq!(
            plan(&ev),
            RecoveryPlan::RedeemApproval {
                token: "tok".into(),
                intent: 意图()
            }
        );
    }

    // ---------- 边界 5：执行完成 ----------

    #[test]
    fn 边界5_结果已提交_边界关闭不再执行() {
        let ev = vec![
            事实(1, EventPayload::StepIntentRecorded { intent: 意图() }),
            事实(2, EventPayload::StepResultRecorded { result: 结果() }),
        ];
        assert_eq!(plan(&ev), RecoveryPlan::Fresh, "结果已提交，绝不再次执行");
    }

    #[test]
    fn 结果提交同时清除审批挂起状态() {
        let ev = vec![
            事实(1, EventPayload::StepIntentRecorded { intent: 意图() }),
            事实(
                2,
                EventPayload::ApprovalTimedOut {
                    token: "tok".into(),
                    call_id: "c1".into(),
                },
            ),
            事实(3, EventPayload::StepResultRecorded { result: 结果() }),
        ];
        assert_eq!(plan(&ev), RecoveryPlan::Fresh);
    }

    // ---------- 边界 6：压缩完成 ----------

    #[test]
    fn 边界6_压缩已完成_复用摘要不重压() {
        let r = EventRange {
            start: EventSequence(1),
            end: EventSequence(10),
        };
        let ev = vec![
            事实(1, EventPayload::CompactionStarted),
            事实(2, EventPayload::CompactionCompleted { source_range: r }),
        ];
        assert_eq!(plan(&ev), RecoveryPlan::ReuseCompaction { source_range: r });
    }

    #[test]
    fn 压缩开了头没完成则当作没做过() {
        // 半成品摘要不可信——用它会造成信息漂移。
        let ev = vec![事实(1, EventPayload::CompactionStarted)];
        assert_eq!(plan(&ev), RecoveryPlan::Fresh);
    }

    // ---------- 通用性质 ----------

    #[test]
    fn 终态后无需恢复() {
        for t in [
            EventPayload::RunCompleted,
            EventPayload::RunFailed,
            EventPayload::RunCanceled,
            EventPayload::RunNeedsUserAction,
        ] {
            let ev = vec![
                事实(1, EventPayload::StepIntentRecorded { intent: 意图() }),
                事实(2, t),
            ];
            assert_eq!(plan(&ev), RecoveryPlan::AlreadyTerminal);
        }
    }

    #[test]
    fn 未知事件安全忽略不破坏判定() {
        let ev = vec![
            事实(1, EventPayload::Unknown),
            事实(
                2,
                EventPayload::ModelRequestPrepared {
                    request_id: "req1".into(),
                },
            ),
            事实(3, EventPayload::Unknown),
        ];
        assert_eq!(
            plan(&ev),
            RecoveryPlan::RetryModelRequest {
                request_id: "req1".into()
            }
        );
    }

    #[test]
    fn 相同前缀必然得出相同计划() {
        // 纯折叠：这是恢复可预测的前提。
        let ev = vec![
            事实(1, EventPayload::StepIntentRecorded { intent: 意图() }),
            事实(2, EventPayload::ApprovalRequested { call_id: "c1".into() }),
        ];
        let first = plan(&ev);
        for _ in 0..8 {
            assert_eq!(plan(&ev), first);
        }
    }

    #[test]
    fn 空日志从新开始() {
        assert_eq!(plan(&[]), RecoveryPlan::Fresh);
    }

    // ---------- reconcile 三态折叠 ----------

    #[test]
    fn not_started_是唯一允许重执行的情形() {
        assert_eq!(
            decide(意图(), ExecutionStatus::NotStarted),
            ReconcileDecision::SafeToRetry { intent: 意图() }
        );
    }

    #[test]
    fn running_继续等待() {
        assert_eq!(
            decide(意图(), ExecutionStatus::Running),
            ReconcileDecision::KeepWaiting
        );
    }

    #[test]
    fn finished_直接回灌不再执行() {
        let r = ExecutionResult {
            execution_id: "e1".into(),
            outcome: ExecutionOutcome::Completed { exit_code: 0 },
            effective_isolation: IsolationLevel::L0BasicContainment,
            artifacts: vec![],
            output: None,
            change_set: None,
            finished_at: Timestamp(7),
        };
        match decide(意图(), ExecutionStatus::Finished(Box::new(r))) {
            ReconcileDecision::ReplayResult { result } => {
                assert_eq!(result.outcome, StepOutcome::Succeeded);
                assert_eq!(result.at, Timestamp(7));
            }
            other => panic!("期望 ReplayResult，得到 {other:?}"),
        }
    }

    #[test]
    fn unknown_必须停下问人绝不重试() {
        // 最危险的情形：执行了但状态未知。猜测会造成重复的外部副作用。
        assert_eq!(
            decide(意图(), ExecutionStatus::Unknown),
            ReconcileDecision::NeedsUserAction {
                reason: "execution_status_unknown"
            }
        );
    }
}
