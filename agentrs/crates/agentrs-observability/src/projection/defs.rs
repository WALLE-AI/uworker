//! Phase B 首批投影（架构 §4.4 表格）。
//!
//! 每个投影只回答**一个**排查问题，并且都遵守同一条纪律：
//! `apply` 里没有 `unwrap` 之外的失败路径，未知事件落到 `_ => {}`。
//!
//! | 投影 | 回答的问题 |
//! |---|---|
//! | [`Skeleton`] | 这次 Run 的骨架是什么 |
//! | [`Requests`] | 每次请求花了多少 token |
//! | [`ToolPaths`] | 某次调用为什么被允许/拒绝/挂起，最终做了什么 |
//! | [`Approvals`] | Run 在哪里停下、等了多久 |
//! | [`CacheAndLegalization`] | 命中率多少、事实与实际请求之间做了哪些修复 |
//!
//! 后两个是本轮新增的重点：没有它们，"这次请求为什么和上次不一样"
//! 和"钱花在哪"只能靠猜。

use std::collections::BTreeMap;

use agentrs_contracts::event::{EventPayload, RunEventEnvelope};
use agentrs_contracts::ids::{EventSequence, Timestamp};
use agentrs_contracts::StepOutcome;
use serde::{Deserialize, Serialize};

use super::{ProjectionDefinition, ProjectionKey, ToolPath};

fn outcome_name(o: &StepOutcome) -> &'static str {
    match o {
        StepOutcome::Succeeded => "Succeeded",
        StepOutcome::Failed { .. } => "Failed",
        StepOutcome::Denied { .. } => "Denied",
        StepOutcome::Canceled => "Canceled",
    }
}

// ---------------------------------------------------------------------------
// Run / Turn / Step 骨架
// ---------------------------------------------------------------------------

/// 一个 Turn 及其 Step。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnNode {
    /// Turn 标识；缺失因果字段时为空串。
    pub turn_id: String,
    /// 起止序号。
    pub first_seq: Option<EventSequence>,
    /// 结束序号；`None` 表示尚未闭合。
    pub end_seq: Option<EventSequence>,
    /// 本 Turn 内的 Step 标识，按首次出现顺序。
    pub steps: Vec<String>,
    /// 是否已正常闭合（`TurnEnded`）。
    pub ended: bool,
}

impl TurnNode {
    /// **零 Step 的 Turn**——被拒绝的 claim 也要留痕（架构 §6.0）。
    pub fn is_zero_step(&self) -> bool {
        self.ended && self.steps.is_empty()
    }
}

/// 骨架视图。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkeletonView {
    /// Run 是否已开始。
    pub started: bool,
    /// 终态判别名；`None` 表示尚未终结。
    pub terminal: Option<String>,
    /// Turn 列表，按首次出现顺序。
    pub turns: Vec<TurnNode>,
    /// 零 Step Turn 数量。
    pub zero_step_turns: usize,
}

/// Run/Turn/Step 树。
#[derive(Debug, Default)]
pub struct Skeleton;

/// [`Skeleton`] 的折叠状态。
#[derive(Debug, Default)]
pub struct SkeletonState {
    started: bool,
    terminal: Option<String>,
    order: Vec<String>,
    turns: BTreeMap<String, TurnNode>,
}

impl SkeletonState {
    fn turn_mut(&mut self, e: &RunEventEnvelope) -> &mut TurnNode {
        let id = e
            .causality
            .turn_id
            .as_ref()
            .map(|t| t.to_string())
            .unwrap_or_default();
        if !self.turns.contains_key(&id) {
            self.order.push(id.clone());
            self.turns.insert(
                id.clone(),
                TurnNode {
                    turn_id: id.clone(),
                    first_seq: e.seq,
                    ..Default::default()
                },
            );
        }
        self.turns.get_mut(&id).expect("just inserted")
    }
}

impl ProjectionDefinition for Skeleton {
    type State = SkeletonState;
    type View = SkeletonView;

    fn key(&self) -> ProjectionKey {
        ProjectionKey("skeleton")
    }

    fn state_version(&self) -> u32 {
        1
    }

    fn init(&self) -> SkeletonState {
        SkeletonState::default()
    }

    fn apply(&self, st: &mut SkeletonState, e: &RunEventEnvelope) {
        match &e.payload {
            EventPayload::RunStarted => st.started = true,
            EventPayload::TurnStarted => {
                st.turn_mut(e);
            }
            EventPayload::TurnEnded => {
                let seq = e.seq;
                let t = st.turn_mut(e);
                t.ended = true;
                t.end_seq = seq;
            }
            EventPayload::StepStarted => {
                let step = e.causality.step_id.as_ref().map(|s| s.to_string());
                let t = st.turn_mut(e);
                if let Some(s) = step {
                    if !t.steps.contains(&s) {
                        t.steps.push(s);
                    }
                }
            }
            EventPayload::RunCompleted => st.terminal = Some("RunCompleted".into()),
            EventPayload::RunFailed => st.terminal = Some("RunFailed".into()),
            EventPayload::RunCanceled => st.terminal = Some("RunCanceled".into()),
            EventPayload::RunNeedsUserAction => st.terminal = Some("RunNeedsUserAction".into()),
            // 未知与无关事件必须可安全忽略。
            _ => {}
        }
    }

    fn view(&self, st: &SkeletonState) -> SkeletonView {
        let turns: Vec<TurnNode> = st
            .order
            .iter()
            .filter_map(|id| st.turns.get(id).cloned())
            .collect();
        SkeletonView {
            started: st.started,
            terminal: st.terminal.clone(),
            zero_step_turns: turns.iter().filter(|t| t.is_zero_step()).count(),
            turns,
        }
    }
}

// ---------------------------------------------------------------------------
// 模型请求与用量
// ---------------------------------------------------------------------------

/// 请求与用量视图。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestsView {
    /// 已装配的请求 id，按顺序。
    pub request_ids: Vec<String>,
    /// 用量更新次数。
    pub usage_updates: usize,
    /// 已开始产出可见输出的次数。**恢复时判断"用户看到过没有"靠它。**
    pub partial_outputs: usize,
    /// 已提交的助手消息数。
    pub assistant_messages: usize,
}

/// 模型请求投影。
#[derive(Debug, Default)]
pub struct Requests;

impl ProjectionDefinition for Requests {
    type State = RequestsView;
    type View = RequestsView;

    fn key(&self) -> ProjectionKey {
        ProjectionKey("requests")
    }

    fn state_version(&self) -> u32 {
        1
    }

    fn init(&self) -> RequestsView {
        RequestsView::default()
    }

    fn apply(&self, st: &mut RequestsView, e: &RunEventEnvelope) {
        match &e.payload {
            EventPayload::ModelRequestPrepared { request_id } => {
                st.request_ids.push(request_id.to_string());
            }
            EventPayload::UsageUpdated => st.usage_updates += 1,
            EventPayload::PartialOutputStarted => st.partial_outputs += 1,
            EventPayload::AssistantMessage => st.assistant_messages += 1,
            _ => {}
        }
    }

    fn view(&self, st: &RequestsView) -> RequestsView {
        st.clone()
    }
}

// ---------------------------------------------------------------------------
// 工具路径
// ---------------------------------------------------------------------------

/// 工具路径视图。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolPathsView {
    /// 每次调用的完整路径，按首次出现顺序。
    pub calls: Vec<ToolPath>,
    /// **有意图无结果**的调用——恢复时必须 `reconcile` 的那一批。
    pub dangling: Vec<String>,
}

/// 工具路径投影：ToolProposed → Hook → StepIntent → Policy/Approval → Sandbox → StepResult。
#[derive(Debug, Default)]
pub struct ToolPaths;

/// [`ToolPaths`] 的折叠状态。
#[derive(Debug, Default)]
pub struct ToolPathsState {
    order: Vec<String>,
    calls: BTreeMap<String, ToolPath>,
}

impl ToolPathsState {
    fn entry(&mut self, call_id: String) -> &mut ToolPath {
        if !self.calls.contains_key(&call_id) {
            self.order.push(call_id.clone());
            self.calls.insert(
                call_id.clone(),
                ToolPath {
                    call_id: call_id.clone(),
                    ..Default::default()
                },
            );
        }
        self.calls.get_mut(&call_id).expect("just inserted")
    }
}

impl ProjectionDefinition for ToolPaths {
    type State = ToolPathsState;
    type View = ToolPathsView;

    fn key(&self) -> ProjectionKey {
        ProjectionKey("tool_paths")
    }

    fn state_version(&self) -> u32 {
        1
    }

    fn init(&self) -> ToolPathsState {
        ToolPathsState::default()
    }

    fn apply(&self, st: &mut ToolPathsState, e: &RunEventEnvelope) {
        match &e.payload {
            EventPayload::ToolProposed { call_id } => st.entry(call_id.to_string()).proposed = true,
            EventPayload::HookOutcomeRecorded { call_id } => st.entry(call_id.to_string()).hooked = true,
            EventPayload::StepIntentRecorded { intent } => {
                let p = st.entry(intent.call_id.to_string());
                p.intent_recorded = true;
                p.tool_name = Some(intent.tool_name.clone());
            }
            EventPayload::ApprovalRequested { call_id } => {
                st.entry(call_id.to_string()).approval_requested = true;
            }
            EventPayload::ApprovalTimedOut { call_id, .. } => {
                st.entry(call_id.to_string()).approval_timed_out = true;
            }
            EventPayload::ToolStarted { call_id } => st.entry(call_id.to_string()).started = true,
            EventPayload::StepResultRecorded { result } => {
                let p = st.entry(result.call_id.to_string());
                p.outcome = Some(outcome_name(&result.outcome).to_owned());
                p.effective_isolation = result.effective_isolation.map(|i| format!("{i:?}"));
            }
            _ => {}
        }
    }

    fn view(&self, st: &ToolPathsState) -> ToolPathsView {
        let calls: Vec<ToolPath> = st
            .order
            .iter()
            .filter_map(|id| st.calls.get(id).cloned())
            .collect();
        ToolPathsView {
            dangling: calls
                .iter()
                .filter(|c| c.is_dangling())
                .map(|c| c.call_id.clone())
                .collect(),
            calls,
        }
    }
}

// ---------------------------------------------------------------------------
// 审批与挂起
// ---------------------------------------------------------------------------

/// 一次审批等待。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalSpan {
    /// 关联调用。
    pub call_id: String,
    /// 请求时刻。
    pub requested_at: Timestamp,
    /// 超时时刻；`None` 表示未超时（在窗口内被裁决或仍在等）。
    pub timed_out_at: Option<Timestamp>,
    /// 挂起令牌；只有超时才有。
    pub resume_token: Option<String>,
}

impl ApprovalSpan {
    /// 等了多久（逻辑毫秒）。`None` 表示尚未闭合。
    pub fn waited_ms(&self) -> Option<i64> {
        self.timed_out_at.map(|t| t.0.saturating_sub(self.requested_at.0))
    }
}

/// 审批视图。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalsView {
    /// 全部审批等待。
    pub spans: Vec<ApprovalSpan>,
    /// Run 是否因此停在等待用户。
    pub needs_user_action: bool,
}

/// 审批与挂起投影。
#[derive(Debug, Default)]
pub struct Approvals;

impl ProjectionDefinition for Approvals {
    type State = ApprovalsView;
    type View = ApprovalsView;

    fn key(&self) -> ProjectionKey {
        ProjectionKey("approvals")
    }

    fn state_version(&self) -> u32 {
        1
    }

    fn init(&self) -> ApprovalsView {
        ApprovalsView::default()
    }

    fn apply(&self, st: &mut ApprovalsView, e: &RunEventEnvelope) {
        match &e.payload {
            EventPayload::ApprovalRequested { call_id } => st.spans.push(ApprovalSpan {
                call_id: call_id.to_string(),
                requested_at: e.at,
                timed_out_at: None,
                resume_token: None,
            }),
            EventPayload::ApprovalTimedOut { token, call_id } => {
                let id = call_id.to_string();
                // 只闭合**最后一条尚未闭合**的同 call 记录——同一次调用可以
                // 被 redeem 后再次请求审批。
                if let Some(s) = st
                    .spans
                    .iter_mut()
                    .rev()
                    .find(|s| s.call_id == id && s.timed_out_at.is_none())
                {
                    s.timed_out_at = Some(e.at);
                    s.resume_token = Some(token.to_string());
                }
            }
            EventPayload::RunNeedsUserAction => st.needs_user_action = true,
            _ => {}
        }
    }

    fn view(&self, st: &ApprovalsView) -> ApprovalsView {
        st.clone()
    }
}

// ---------------------------------------------------------------------------
// 缓存与历史合法化
// ---------------------------------------------------------------------------

/// 缓存与合法化视图。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheView {
    /// 观测到的缓存断裂次数。
    pub cache_breaks: usize,
    /// 装配过的请求数。
    pub requests: usize,
    /// 历史合法化执行次数。
    pub legalizations: usize,
    /// 内容引用解析失败次数。
    pub unresolved_refs: usize,
    /// 已完成压缩的来源区间。**复用摘要靠它，不重复压同一段。**
    pub compacted_ranges: Vec<(EventSequence, EventSequence)>,
}

impl CacheView {
    /// **前缀未断裂的比例**——不是 token 命中率。
    ///
    /// 这两者常被混为一谈，但它们回答的是不同的问题：
    ///
    /// | 指标 | 数据来源 | 回答 |
    /// |---|---|---|
    /// | 本方法 | **请求侧**（我们自己算的前缀摘要） | 我发出去的前缀有几次是稳定的 |
    /// | token 命中率 | **响应侧**（端点报告的 `cache_read_tokens`） | 端点那边实际命中了多少 |
    ///
    /// 一个从不启用缓存的端点，本方法照样是 100%——**因为前缀确实没断**，
    /// 只是没人拿它去命中任何东西。把它当 token 命中率读，
    /// 会得出"缓存工作良好"的错误结论。真正的收益要看
    /// [`crate::projection`] 之外的 `agentrs-context::cache_diagnostics`。
    ///
    /// 没有请求时为 `None`——**不要报 0%，那会被读成"全 miss"**。
    pub fn unbroken_rate(&self) -> Option<f64> {
        if self.requests == 0 {
            return None;
        }
        let 未断裂 = self.requests.saturating_sub(self.cache_breaks);
        Some(未断裂 as f64 / self.requests as f64)
    }

    /// 是否观测到过任何缓存断裂。
    ///
    /// 全程为 `false` 有两种可能，**本投影分不出来**：前缀始终稳定，
    /// 或者根本没人在观测。区分它们需要响应侧数据。
    pub fn observed_any_break(&self) -> bool {
        self.cache_breaks > 0
    }
}

/// 缓存与历史合法化投影。
#[derive(Debug, Default)]
pub struct CacheAndLegalization;

impl ProjectionDefinition for CacheAndLegalization {
    type State = CacheView;
    type View = CacheView;

    fn key(&self) -> ProjectionKey {
        ProjectionKey("cache")
    }

    fn state_version(&self) -> u32 {
        1
    }

    fn init(&self) -> CacheView {
        CacheView::default()
    }

    fn apply(&self, st: &mut CacheView, e: &RunEventEnvelope) {
        match &e.payload {
            EventPayload::CacheBreakObserved => st.cache_breaks += 1,
            EventPayload::ModelRequestPrepared { .. } => st.requests += 1,
            EventPayload::HistoryLegalized => st.legalizations += 1,
            EventPayload::ContentRefUnresolved => st.unresolved_refs += 1,
            EventPayload::CompactionCompleted { source_range } => {
                st.compacted_ranges.push((source_range.start, source_range.end));
            }
            _ => {}
        }
    }

    fn view(&self, st: &CacheView) -> CacheView {
        st.clone()
    }
}

/// 注册 Phase B 首批投影。
pub fn register_default(reg: &mut super::ProjectionRegistry) -> Result<(), super::ProjectionError> {
    reg.register(Skeleton)?;
    reg.register(Requests)?;
    reg.register(ToolPaths)?;
    reg.register(Approvals)?;
    reg.register(CacheAndLegalization)?;
    Ok(())
}
