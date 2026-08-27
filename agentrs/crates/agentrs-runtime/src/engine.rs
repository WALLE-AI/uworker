//! Turn / Step 状态机（架构 §6.0，任务 T04）。
//!
//! 沿用精确定义，避免"回合"一词指代两件事：
//!
//! ```text
//! Step = 一次模型请求 + 该请求提出的全部工具调用及其结果
//! Turn = 零个或多个 Step
//!        在第一批输入被 claim 之前打开，在"没有任何欠账"之后关闭
//! ```
//!
//! "欠账"指两类：工具还欠模型一次请求，或 inbox 里还有已到达的输入。
//!
//! **一个必须显式支持的边界情况**：被拒绝或被改写为空的首次 claim，
//! 仍然关闭一个花了 0 个 Step 的 durable Turn——日志要记录"这次尝试发生过
//! 但没有产生模型请求"，否则用户的输入会在事实流里凭空消失。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use agentrs_contracts::event::{Causality, Durability, EventPayload, RunEventEnvelope, Visibility};
use agentrs_contracts::ids::{EventId, RunEpoch, RunId, StepId, Timestamp, TurnId};
use agentrs_contracts::ports::{Clock, PersistError, RunPersistence};
use agentrs_types::{LlmEvent, LlmRequest, StopReason};

use crate::composition::ResourceOwner;
use crate::inbox::{Claim, Inbox, PreStepDecision};
use tokio::sync::Mutex;

/// Step 的结果分类（沿用 aionrs 的四态骨架，见架构 §1.5.1）。
#[derive(Debug, Clone, PartialEq)]
pub enum StepOutcome {
    /// 模型给出最终答复。
    Final {
        /// 助手文本。
        text: String,
    },
    /// 模型提出工具调用，欠一次后续请求。
    ToolRound {
        /// 提出的调用数。
        calls: usize,
    },
    /// 触及 `max_tokens` 被截断。
    Truncated {
        /// 已产出的文本。
        text: String,
    },
    /// 既无文本也无工具调用。
    EmptyFinal,
    /// 工具回合中审批超时 —— 整个 Run 转入挂起。
    Suspended {
        /// 恢复令牌。
        token: agentrs_contracts::ids::ApprovalToken,
    },
}

/// Run 的终止原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Termination {
    /// 正常完成。
    Completed,
    /// 被取消。
    Canceled,
    /// 触及回合上限等确定性防护。
    NeedsUserAction {
        /// 稳定原因码。
        reason: String,
    },
    /// 失败。
    Failed {
        /// 稳定原因码。
        code: String,
    },
}

/// 引擎运行结束后的摘要。
#[derive(Debug, Clone, PartialEq)]
pub struct RunSummary {
    /// 终止原因。
    pub termination: Termination,
    /// 打开过的 Turn 数（含 0-Step Turn）。
    pub turns: u32,
    /// 执行过的 Step 数。
    pub steps: u32,
    /// 花了 0 个 Step 的 Turn 数——被拒绝的 claim 会产生它。
    pub zero_step_turns: u32,
    /// 最后一条助手文本。
    ///
    /// **从 Surface 投影而来，不是另存一份**——否则"模型可见即已记录"
    /// 就多了一个不受该不变式约束的旁路。`None` 表示 Run 结束时
    /// 没有任何助手文本（例如被取消，或全程只有工具调用）。
    pub final_text: Option<String>,
}

/// 确定性回合防护。**不依赖模型自觉。**
#[derive(Debug, Clone, Copy)]
pub struct TurnGuards {
    /// 最大 Step 数。
    pub max_steps: u32,
    /// 连续空回复的容忍次数。
    pub max_empty_finals: u32,
}

impl Default for TurnGuards {
    fn default() -> Self {
        Self {
            max_steps: 64,
            max_empty_finals: 1,
        }
    }
}

/// 取消令牌。
#[derive(Debug, Default)]
pub struct CancelToken(AtomicBool);

impl CancelToken {
    /// 请求取消。
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// 是否已请求取消。
    pub fn is_canceled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// 模型调用的抽象。
///
/// 引擎不直接依赖 `agentrs-provider`——那会让状态机测试必须构造完整的
/// provider 适配器。这里只要求"给一个请求，回一串事件"。
#[async_trait::async_trait]
pub trait StepDriver: Send + Sync {
    /// 执行一次模型请求。
    async fn call(&self, req: LlmRequest) -> Result<Vec<LlmEvent>, String>;
}

/// PreStep 裁决点。**返回值是权威的。**
pub trait StepAdmission: Send + Sync {
    /// 对一批被 claim 的输入做裁决。
    fn admit(&self, claim: Claim) -> PreStepDecision;
}

/// 默认放行。
pub struct AdmitAll;

impl StepAdmission for AdmitAll {
    fn admit(&self, claim: Claim) -> PreStepDecision {
        PreStepDecision::Enter(claim.inputs)
    }
}

/// 引擎所需的外部依赖。
pub struct EngineDeps {
    /// 事实记录。
    pub persistence: Arc<dyn RunPersistence>,
    /// 逻辑时钟。内核不读真实时钟。
    pub clock: Arc<dyn Clock>,
    /// 模型调用。
    pub driver: Arc<dyn StepDriver>,
    /// PreStep 裁决。
    pub admission: Arc<dyn StepAdmission>,
    /// 工具回合依赖。
    ///
    /// `None` 表示**本 Run 无工具**——此时模型若仍提出调用，
    /// 会得到一条结构化拒绝而不是被静默忽略。
    pub tools: Option<Arc<crate::toolround::ToolRoundDeps>>,
}

impl EngineDeps {
    /// 无工具的最小依赖组合。
    pub fn without_tools(
        persistence: Arc<dyn RunPersistence>,
        clock: Arc<dyn Clock>,
        driver: Arc<dyn StepDriver>,
        admission: Arc<dyn StepAdmission>,
    ) -> Self {
        Self {
            persistence,
            clock,
            driver,
            admission,
            tools: None,
        }
    }
}

/// 一次 Run 的推进计数。
#[derive(Debug, Clone, Copy, Default)]
struct Progress {
    turns: u32,
    steps: u32,
    zero_step_turns: u32,
    empty_finals: u32,
}

/// 单次 claim 的批量上限。
const CLAIM_BATCH: usize = 8;

/// Turn/Step 状态机。
pub struct Engine {
    run_id: RunId,
    epoch: RunEpoch,
    deps: EngineDeps,
    inbox: Arc<Inbox>,
    cancel: Arc<CancelToken>,
    owner: Arc<ResourceOwner>,
    guards: TurnGuards,
    seq: std::sync::atomic::AtomicU64,
    /// 已追加的 Surface 节点。请求**只从这里投影**，
    /// 这样"模型可见即已记录"才是可验证的（见 `invariant`）。
    surface: Mutex<Vec<crate::surface::SurfaceNode>>,
    invariants: crate::invariant::Invariants,
    workspace_id: String,
    change_set_id: agentrs_contracts::ids::ChangeSetId,
    approval_timeout_ms: i64,
    model: agentrs_contracts::ids::ModelId,
    system_prompt: String,
    /// 模型可见的工具目录。**只追加不重排**（缓存前缀 S1 段）。
    tool_catalog: Vec<agentrs_types::ToolDef>,
    required_isolation: agentrs_contracts::sandbox::IsolationLevel,
    /// 当前权限模式。切换只在 Turn 边界发生（§4.1.2 规则 2）。
    permission_mode: agentrs_contracts::authority::PermissionMode,
    /// 由模式派生的单调 guard，每次工具回合都装进 `ToolRoundDeps`。
    mode_guard: Option<Arc<dyn crate::toolround::ToolGuard>>,
    /// 上一次请求的缓存快照，用于归因本次断裂（架构 §9.1.1）。
    prev_cache: Mutex<Option<agentrs_context::cache::RequestSnapshot>>,
}

impl Engine {
    /// 构造引擎。
    pub fn new(
        run_id: RunId,
        epoch: RunEpoch,
        deps: EngineDeps,
        inbox: Arc<Inbox>,
        cancel: Arc<CancelToken>,
        owner: Arc<ResourceOwner>,
        guards: TurnGuards,
    ) -> Self {
        Self {
            run_id,
            epoch,
            deps,
            inbox,
            cancel,
            owner,
            guards,
            seq: std::sync::atomic::AtomicU64::new(0),
            surface: Mutex::new(Vec::new()),
            invariants: crate::invariant::Invariants::default(),
            workspace_id: "default".into(),
            change_set_id: "cs-default".into(),
            approval_timeout_ms: 60_000,
            model: "default".into(),
            system_prompt: String::new(),
            tool_catalog: Vec::new(),
            permission_mode: agentrs_contracts::authority::PermissionMode::Default,
            mode_guard: None,
            prev_cache: Mutex::new(None),
            required_isolation: agentrs_contracts::sandbox::IsolationLevel::L0BasicContainment,
        }
    }

    /// 设置模型与系统段。
    pub fn with_model(
        mut self,
        model: impl Into<agentrs_contracts::ids::ModelId>,
        system_prompt: impl Into<String>,
    ) -> Self {
        self.model = model.into();
        self.system_prompt = system_prompt.into();
        self
    }

    /// 设置模型可见的工具目录。
    pub fn with_tools(mut self, tools: Vec<agentrs_types::ToolDef>) -> Self {
        self.tool_catalog = tools;
        self
    }

    /// 应用 `PermissionMode`（架构 §4.1.2）。
    ///
    /// **一次调用同时决定三件事**——工具目录投影、系统提示分段、固定管线里的
    /// [`ModeGuard`](crate::permission::ModeGuard)。分成三处设置迟早会漏一处，
    /// 而漏掉任何一处的后果都不是"少一层保护"：漏目录，模型提出注定被拒的调用；
    /// 漏提示，模型不知道自己在只读模式；漏 guard，历史里残留的旧 `tool_use`
    /// 可以直接穿过去。
    ///
    /// **必须在 [`with_tools`](Self::with_tools) 之后调用**——它要在完整目录上做投影。
    pub fn with_permission_mode(mut self, mode: agentrs_contracts::authority::PermissionMode) -> Self {
        let full = self.tool_catalog.clone();
        let p = crate::permission::project(&mode, &full);

        self.tool_catalog = p.catalog;
        if !p.system_section.is_empty() {
            if self.system_prompt.is_empty() {
                self.system_prompt = p.system_section;
            } else {
                self.system_prompt = format!("{}\n\n{}", self.system_prompt, p.system_section);
            }
        }
        // guard 拿**完整**目录：它要能区分"这个工具不存在"与
        // "存在但当前模式不允许"，后者才给得出 PermissionMode 拒绝码。
        self.mode_guard = Some(crate::permission::ModeGuard::shared(mode.clone(), full));
        self.permission_mode = mode;
        self
    }

    /// 设置工作区与 ChangeSet（宿主在 `RunSpec` 解析后调用）。
    pub fn with_workspace(
        mut self,
        workspace_id: impl Into<String>,
        change_set_id: impl Into<agentrs_contracts::ids::ChangeSetId>,
    ) -> Self {
        self.workspace_id = workspace_id.into();
        self.change_set_id = change_set_id.into();
        self
    }

    fn next_event_id(&self) -> EventId {
        // 确定性派生：run_id + epoch + 本地计数。这是 durable 写入的幂等键，
        // 重投递必须命中同一条记录（架构 §4.3）。
        let n = self.seq.fetch_add(1, Ordering::SeqCst);
        EventId::new(format!("{}-{}-{}", self.run_id, self.epoch, n))
    }

    async fn emit(
        &self,
        payload: EventPayload,
        durability: Durability,
        causality: Causality,
    ) -> Result<(), PersistError> {
        let ev = RunEventEnvelope {
            run_id: self.run_id.clone(),
            epoch: self.epoch,
            event_id: self.next_event_id(),
            seq: None,
            live_seq: None,
            at: self.deps.clock.now(),
            durability,
            visibility: Visibility::User,
            causality,
            surface: None,
            payload,
        };
        self.deps
            .persistence
            .append_event(self.epoch, ev)
            .await
            .map(|_| ())
    }

    async fn emit_fact(&self, payload: EventPayload, c: Causality) -> Result<(), PersistError> {
        self.emit(payload, Durability::DurableFact, c).await
    }

    /// 把一条消息追加进 Surface。
    ///
    /// **这是模型可见内容进入系统的唯一入口**——请求装配只从 Surface 投影，
    /// 因此绕过这里的内容会被运行时不变式捕获。
    async fn append_surface(
        &self,
        kind: agentrs_contracts::surface::SurfaceEventKind,
        message: agentrs_types::Message,
    ) {
        let mut s = self.surface.lock().await;
        let seq = agentrs_contracts::ids::EventSequence(s.len() as u64 + 1);
        s.push(crate::surface::SurfaceNode {
            seq,
            kind,
            op: agentrs_contracts::surface::SurfaceOp::Append,
            message,
        });
    }

    /// 从 Surface 投影出即将发送的历史。
    async fn project_history(&self) -> Vec<agentrs_types::Message> {
        crate::surface::derive_messages(&self.surface.lock().await)
    }

    /// 驱动到终态。
    ///
    /// **两层循环**：外层是 Turn，内层是 Step。
    ///
    /// 这个结构不是可选的——`ToolRound` 欠的是"一次后续请求"，那是**同一个 Turn
    /// 内的下一个 Step**，不是新 Turn。把每个 Step 都开成新 Turn 会让工具链路在
    /// 第一次回合后就因"inbox 为空"而提前收敛。
    pub async fn run(&self) -> RunSummary {
        let mut st = Progress::default();

        if let Err(e) = self
            .emit_fact(EventPayload::RunStarted, Causality::default())
            .await
        {
            return self.abort(e, st).await;
        }

        loop {
            if self.cancel.is_canceled() {
                return self.finish(Termination::Canceled, st).await;
            }

            st.turns += 1;
            let turn_id = TurnId::new(format!("{}-t{}", self.run_id, st.turns));
            let turn_c = Causality {
                turn_id: Some(turn_id.clone()),
                ..Default::default()
            };

            if let Err(e) = self.emit_fact(EventPayload::TurnStarted, turn_c.clone()).await {
                return self.abort(e, st).await;
            }

            match self.run_turn(&turn_id, &turn_c, &mut st).await {
                Ok(Some(term)) => return self.finish(term, st).await,
                Ok(None) => {}
                Err(e) => return self.abort(e, st).await,
            }

            if let Err(e) = self.emit_fact(EventPayload::TurnEnded, turn_c).await {
                return self.abort(e, st).await;
            }

            // Turn 只在"没有任何欠账"后关闭。此处还有输入 = 期间又有人 submit。
            if self.inbox.pending().await == 0 {
                break;
            }
        }

        self.finish(Termination::Completed, st).await
    }

    /// 跑完一个 Turn。返回 `Some(termination)` 表示整个 Run 应就此终止。
    async fn run_turn(
        &self,
        turn_id: &TurnId,
        turn_c: &Causality,
        st: &mut Progress,
    ) -> Result<Option<Termination>, PersistError> {
        let claim = self.inbox.claim(CLAIM_BATCH).await;
        self.emit_fact(EventPayload::UserInputClaimed, turn_c.clone())
            .await?;

        let mut pending = match self.deps.admission.admit(claim) {
            PreStepDecision::Reject { .. } => {
                // 0-Step Turn：留痕，输入不得凭空消失。
                st.zero_step_turns += 1;
                return Ok(None);
            }
            PreStepDecision::Enter(inputs) => inputs,
        };

        // 空 claim 且已跑过 Step —— 同样是 0-Step Turn。
        if pending.is_empty() && st.steps > 0 {
            st.zero_step_turns += 1;
            return Ok(None);
        }

        loop {
            if self.cancel.is_canceled() {
                return Ok(Some(Termination::Canceled));
            }

            if st.steps >= self.guards.max_steps {
                return Ok(Some(Termination::NeedsUserAction {
                    reason: "max_steps".into(),
                }));
            }

            st.steps += 1;
            let step_c = Causality {
                turn_id: Some(turn_id.clone()),
                step_id: Some(StepId::new(format!("{}-s{}", self.run_id, st.steps))),
                ..Default::default()
            };

            self.emit_fact(EventPayload::StepStarted, step_c.clone()).await?;
            for input in &pending {
                match input {
                    crate::inbox::UserInput::Message(_) => {
                        self.emit_fact(EventPayload::UserInputSubmitted, step_c.clone())
                            .await?;
                    }
                    crate::inbox::UserInput::External(fact) => {
                        // 跨 Run 事实单独留痕，并携带**源 Run 的确切位置**——
                        // 这是 Trajectory 能回答"这个成员为什么这么做"的依据。
                        let mut c = step_c.clone();
                        c.cross_run = fact.causality.clone();
                        self.emit_fact(EventPayload::ExternalFactReceived, c).await?;
                    }
                }
                // 输入在此进入 Surface —— 之后请求只从投影装配，
                // 绕过这里的内容会被运行时不变式捕获。
                let msg = match input {
                    crate::inbox::UserInput::Message(blocks) => {
                        agentrs_types::Message::new(agentrs_types::Role::User, blocks.clone())
                    }
                    crate::inbox::UserInput::External(fact) => agentrs_types::Message::new(
                        agentrs_types::Role::User,
                        vec![agentrs_types::ContentBlock::text(match &fact.content {
                            agentrs_contracts::external::ExternalContent::Inline { text } => text.clone(),
                            agentrs_contracts::external::ExternalContent::Ref { .. } => {
                                "[外部内容引用]".to_string()
                            }
                        })],
                    ),
                };
                self.append_surface(agentrs_contracts::surface::SurfaceEventKind::UserMessage, msg)
                    .await;
            }

            let outcome = self.execute_step(&step_c).await?;
            self.emit_fact(EventPayload::StepEnded, step_c).await?;

            match outcome {
                // 工具欠模型一次请求 —— 同一 Turn 内继续下一个 Step。
                StepOutcome::ToolRound { .. } => {
                    pending = Vec::new();
                }
                StepOutcome::EmptyFinal => {
                    st.empty_finals += 1;
                    if st.empty_finals > self.guards.max_empty_finals {
                        return Ok(Some(Termination::NeedsUserAction {
                            reason: "empty_final".into(),
                        }));
                    }
                    pending = Vec::new();
                }
                // 审批超时 —— 整个 Run 转挂起，由宿主凭令牌 resume。
                StepOutcome::Suspended { .. } => {
                    return Ok(Some(Termination::NeedsUserAction {
                        reason: "approval_suspended".into(),
                    }))
                }
                StepOutcome::Final { .. } | StepOutcome::Truncated { .. } => {
                    // 无工具欠账。若 next-step 输入已到达，同一 Turn 内继续认领。
                    let next = self.inbox.claim(CLAIM_BATCH).await;
                    if next.is_empty() {
                        return Ok(None);
                    }
                    self.emit_fact(EventPayload::UserInputClaimed, turn_c.clone())
                        .await?;
                    match self.deps.admission.admit(next) {
                        PreStepDecision::Reject { .. } => return Ok(None),
                        PreStepDecision::Enter(inputs) => pending = inputs,
                    }
                }
            }
        }
    }

    async fn execute_step(&self, c: &Causality) -> Result<StepOutcome, PersistError> {
        let request_id: agentrs_contracts::ids::RequestId = format!("{}-req", self.run_id).as_str().into();
        self.emit_fact(
            EventPayload::ModelRequestPrepared {
                request_id: request_id.clone(),
            },
            c.clone(),
        )
        .await?;

        // Surface 投影 → （预算裁剪）→ ★不变式断言★ → legalization → 发送。
        let history = self.project_history().await;
        {
            let surface = self.surface.lock().await;
            if let Err(v) = self.invariants.check_model_visible_is_logged(&history, &surface) {
                // 宁可失败也不发出不可重建的请求。
                let _ = v;
                self.emit_fact(EventPayload::RunFailed, c.clone()).await?;
                return Ok(StepOutcome::EmptyFinal);
            }
        }

        // 缓存前缀：算出本次的稳定段摘要，与上一次比对并归因。
        // **归因必须在发请求之前做**——请求发出后前缀就变成"上一次"了。
        let snapshot = self.cache_snapshot(&history).await;
        let cause = {
            let mut prev = self.prev_cache.lock().await;
            let c = agentrs_context::cache::attribute(prev.as_ref(), &snapshot);
            *prev = Some(snapshot.clone());
            c
        };
        if let Some(cause) = cause {
            // 每次 miss 都要落到一个具体原因上，否则"命中率低"无从优化。
            let _ = cause;
            self.emit_fact(EventPayload::CacheBreakObserved, c.clone())
                .await?;
        }

        let req = LlmRequest {
            request_id,
            model: self.model.clone(),
            system: self.system_prompt.clone(),
            messages: history,
            tools: self.tool_catalog.clone(),
            max_tokens: None,
            thinking: None,
            reasoning_effort: None,
            cache_prefix_digest: Some(snapshot.prefix_digest.clone()),
        };

        let events = match self.deps.driver.call(req).await {
            Ok(e) => e,
            Err(_) => {
                self.emit_fact(EventPayload::RunFailed, c.clone()).await?;
                return Ok(StepOutcome::EmptyFinal);
            }
        };

        let mut text = String::new();
        let mut tool_calls = 0usize;
        let mut stop = None;
        let mut partial_recorded = false;
        let mut proposed: Vec<crate::toolround::ProposedCall> = Vec::new();

        for e in &events {
            match e {
                LlmEvent::TextDelta(t) => {
                    // 首个可见增量处写一条 durable 事实。
                    // TextDelta 本身是 live 可丢的，恢复时无法据它判断
                    // "崩溃前用户看到过东西没有"——那正是本事件存在的理由。
                    if !partial_recorded && !t.is_empty() {
                        partial_recorded = true;
                        self.emit_fact(EventPayload::PartialOutputStarted, c.clone())
                            .await?;
                    }
                    text.push_str(t);
                    // live 事件：可采样或丢失，不参与恢复。
                    self.emit(EventPayload::TextDelta, Durability::LiveStream, c.clone())
                        .await?;
                }
                LlmEvent::ToolUse { id, name, input, .. } => {
                    tool_calls += 1;
                    proposed.push(crate::toolround::ProposedCall {
                        call_id: id.clone(),
                        tool_name: name.clone(),
                        arguments: input.clone(),
                    });
                }
                LlmEvent::Done { stop_reason, .. } => stop = Some(*stop_reason),
                _ => {}
            }
        }

        if !text.is_empty() || !proposed.is_empty() {
            let mut blocks: Vec<agentrs_types::ContentBlock> = Vec::new();
            if !text.is_empty() {
                blocks.push(agentrs_types::ContentBlock::text(&text));
            }
            for p in &proposed {
                blocks.push(agentrs_types::ContentBlock::ToolUse {
                    id: p.call_id.clone(),
                    name: p.tool_name.clone(),
                    input: p.arguments.clone(),
                    extra: None,
                });
            }
            self.append_surface(
                agentrs_contracts::surface::SurfaceEventKind::AssistantMessage,
                agentrs_types::Message::new(agentrs_types::Role::Assistant, blocks),
            )
            .await;
        }
        self.emit_fact(EventPayload::AssistantMessage, c.clone()).await?;

        // ---- 工具回合 ----
        if !proposed.is_empty() {
            return self.run_tool_calls(proposed, c).await;
        }

        Ok(match (tool_calls, stop, text.is_empty()) {
            (n, _, _) if n > 0 => StepOutcome::ToolRound { calls: n },
            (_, Some(StopReason::MaxTokens), _) => StepOutcome::Truncated { text },
            (_, _, true) => StepOutcome::EmptyFinal,
            _ => StepOutcome::Final { text },
        })
    }

    /// 按调度计划执行工具调用（架构 §8.3）。
    ///
    /// [`crate::schedule::plan`] 把提议切成批次：批次内可并行，批次之间是
    /// **ordering barrier**。这里目前**在批次内也按下标顺序推进**——
    /// 真正的并发执行等 SandboxRS 就绪后接入。即便如此，计划本身已经在
    /// 起作用：它保证了独占工具前后的调用不会被重排，因此接入并发时
    /// 语义不变、只是变快。
    ///
    /// 结果一律按**原始 proposal order** 回灌（[`crate::schedule::Plan::reorder`]）——
    /// 乱序回灌会让 `tool_use` 与 `tool_result` 的配对在部分厂商上直接 400。
    async fn run_tool_calls(
        &self,
        calls: Vec<crate::toolround::ProposedCall>,
        c: &Causality,
    ) -> Result<StepOutcome, PersistError> {
        let Some(host_tools) = self.deps.tools.clone() else {
            // 无工具的 Run 收到调用提议 —— 结构化拒绝，不静默忽略。
            for call in &calls {
                self.emit_fact(
                    EventPayload::ToolProposed {
                        call_id: call.call_id.clone(),
                    },
                    c.clone(),
                )
                .await?;
            }
            return Ok(StepOutcome::ToolRound { calls: calls.len() });
        };

        // 把模式 guard 叠加到宿主提供的 guard 之上。
        // **叠加而不是替换**——宿主的 guard 与模式 guard 都只能收紧，
        // 两者取并集仍然是收紧，顺序不影响结论。
        let tools = match &self.mode_guard {
            None => host_tools,
            Some(g) => {
                let mut guards = host_tools.guards.clone();
                guards.push(g.clone());
                Arc::new(crate::toolround::ToolRoundDeps {
                    policy: host_tools.policy.clone(),
                    sandbox: host_tools.sandbox.clone(),
                    guards,
                    hooks: host_tools.hooks.clone(),
                })
            }
        };

        let ctx = crate::toolround::ToolRoundCtx {
            step_id: c
                .step_id
                .clone()
                .unwrap_or_else(|| StepId::new(format!("{}-s?", self.run_id))),
            workspace_id: self.workspace_id.clone(),
            change_set_id: self.change_set_id.clone(),
            approval_deadline: agentrs_contracts::ids::Deadline(Timestamp(
                self.deps.clock.now().0 + self.approval_timeout_ms,
            )),
            required_isolation: self.required_isolation,
            now: self.deps.clock.now(),
        };

        let plan = crate::schedule::plan(&calls, &self.tool_catalog);
        // 借用而非移动：下面的 async move 只需要 &ProposedCall。
        let calls = &calls;

        for batch in &plan.batches {
            // 批次内并发。每个调用把自己的事件收进独立的 Vec，
            // 之后**按批次内下标顺序**写入事实流——并发执行不等于事件乱序，
            // 否则同一份历史重放会得到不同的事件序，恢复的确定性就没了。
            let settled = crate::toolround::execute_batch(&tools, &ctx, calls, &batch.indices).await;

            for crate::toolround::SettledCall {
                index: i,
                events,
                outcome,
            } in settled
            {
                for e in events {
                    self.emit_fact(e, c.clone()).await?;
                }

                match outcome {
                    Err((token, _intent)) => {
                        // 审批超时 —— 整个 Run 转挂起，不继续后续调用。
                        return Ok(StepOutcome::Suspended { token });
                    }
                    Ok((_intent, result)) => {
                        // **结果必须回灌**——否则模型看不到工具做了什么，
                        // 只能凭空编造后续内容。回灌同样走 Surface，
                        // 因此它也受"模型可见即已记录"约束。
                        let (text, is_error) = match &result.outcome {
                            agentrs_contracts::StepOutcome::Succeeded => {
                                (result.output.clone().unwrap_or_else(|| "ok".into()), false)
                            }
                            agentrs_contracts::StepOutcome::Failed { message } => (message.clone(), true),
                            agentrs_contracts::StepOutcome::Denied { code, message } => {
                                (format!("[{code:?}] {message}"), true)
                            }
                            agentrs_contracts::StepOutcome::Canceled => ("已取消".into(), true),
                        };
                        self.append_surface(
                            agentrs_contracts::surface::SurfaceEventKind::ToolResult,
                            agentrs_types::Message::new(
                                agentrs_types::Role::User,
                                vec![agentrs_types::ContentBlock::ToolResult {
                                    tool_use_id: calls[i].call_id.clone(),
                                    content: text,
                                    is_error,
                                }],
                            ),
                        )
                        .await;
                    }
                }
            }
        }

        Ok(StepOutcome::ToolRound { calls: calls.len() })
    }

    /// 从 Surface 取最后一条助手文本。
    /// 施加一次压缩：追加一个 `Replace` 节点遮蔽计划中的区间（架构 §9.3）。
    ///
    /// **log 从不被改写**——压缩只是往 Surface 上再追加一个节点。
    /// 人类 transcript 用 append-origin 投影，因此用户已经看到的对话不受影响。
    ///
    /// 返回本次推进到的 [`SurfaceGeneration`]。溢出触发的重试只有在代际
    /// **确实前进**时才允许开启（§9.3 防重试循环）。
    pub async fn apply_compaction(
        &self,
        plan: &agentrs_context::compaction::CompactionPlan,
        summary: agentrs_types::Message,
        c: &Causality,
    ) -> Result<agentrs_contracts::surface::SurfaceGeneration, PersistError> {
        use agentrs_contracts::surface::{SurfaceGeneration, SurfaceOp};

        self.emit_fact(EventPayload::CompactionStarted, c.clone()).await?;

        let generation = {
            let mut s = self.surface.lock().await;
            // 代际取当前最大值 + 1：它是"这次压缩到底有没有效果"的客观依据。
            let next = s
                .iter()
                .filter_map(|n| match n.op {
                    SurfaceOp::Replace { generation, .. } => Some(generation),
                    SurfaceOp::Append => None,
                })
                .max()
                .unwrap_or(SurfaceGeneration(0))
                .advance();

            let seq = agentrs_contracts::ids::EventSequence(s.len() as u64 + 1);
            s.push(crate::surface::SurfaceNode {
                seq,
                kind: agentrs_contracts::surface::SurfaceEventKind::AssistantMessage,
                op: SurfaceOp::Replace {
                    range: plan.range,
                    generation: next,
                },
                message: summary,
            });
            next
        };

        // 记录 source_range：**复用摘要，不重复压同一段**（§9.3 末段）。
        self.emit_fact(
            EventPayload::CompactionCompleted {
                source_range: agentrs_contracts::ids::EventRange {
                    start: plan.range.start,
                    end: plan.range.end,
                },
            },
            c.clone(),
        )
        .await?;

        Ok(generation)
    }

    /// 计算本次请求的缓存快照（架构 §9.1.1）。
    ///
    /// | 段 | 内容 | 何时变 |
    /// |---|---|---|
    /// | S0 | 系统规则 + PermissionMode | 模式切换、宿主改提示 |
    /// | S1 | 工具目录 | 工具注册/撤回、模式过滤 |
    /// | S2 | Surface 的**失效点** | 只有 `Replace`（压缩）才变 |
    ///
    /// ## S2 为什么不是"历史的摘要"
    ///
    /// 第一版把 S2 算成了历史内容的摘要（去掉末尾一条）。跑起来才发现
    /// **每轮请求的前缀摘要都不同**——因为历史每轮增长两条（助手回复 +
    /// 新的用户输入），"去掉末尾一条"剩下的部分仍在变大。
    ///
    /// 根子上是概念错了：稳定前缀**本来就是会增长的**——每轮把断点往后挪，
    /// 让 provider 缓存更长的一段。增长不是断裂。会让已缓存部分失效的
    /// 只有一件事：`Replace` 把某个区间遮蔽掉（压缩）。
    ///
    /// 所以 S2 摘要的是**失效点本身**，不是历史内容：没发生过压缩时它恒定，
    /// 压缩一次就变一次。
    ///
    /// 那"有人偷偷改写了早先的历史"谁来抓？——`Invariants::check_model_visible_is_logged`。
    /// 它逐条比对派生历史与 Surface，比摘要精确得多，也能指出**哪一条**对不上。
    /// 缓存层不重复做它的工作。
    async fn cache_snapshot(
        &self,
        history: &[agentrs_types::Message],
    ) -> agentrs_context::cache::RequestSnapshot {
        use agentrs_context::cache::{CacheLayout, RequestSnapshot, Segment};
        use agentrs_contracts::manifest::CacheSegment;

        fn 摘要(parts: impl IntoIterator<Item = String>) -> agentrs_contracts::ids::Digest {
            let mut h: u64 = 0xcbf2_9ce4_8422_2325;
            for p in parts {
                for b in p.as_bytes() {
                    h ^= *b as u64;
                    h = h.wrapping_mul(0x1000_0000_01b3);
                }
                h ^= 0xff;
                h = h.wrapping_mul(0x1000_0000_01b3);
            }
            agentrs_contracts::ids::Digest::from_hex(format!("{h:016x}"))
        }

        let s0 = 摘要([self.system_prompt.clone(), format!("{:?}", self.permission_mode)]);
        // 工具目录**只追加不重排**，因此按声明顺序摘要即可。
        let s1 = 摘要(
            self.tool_catalog
                .iter()
                .map(|t| format!("{}\u{0}{}\u{0}{}", t.name, t.description, t.parameters)),
        );
        let invalidation = crate::surface::cache_invalidation_point(&self.surface.lock().await);
        // 只摘要失效点，不摘要历史内容——理由见本函数文档。
        let s2 = 摘要([match invalidation {
            None => "no-replace".to_string(),
            Some(p) => format!("replace-from-{}", p.0),
        }]);
        let _ = history;

        let layout = CacheLayout {
            segments: vec![
                Segment {
                    kind: CacheSegment::S0SystemRules,
                    digest: s0.clone(),
                },
                Segment {
                    kind: CacheSegment::S1ToolCatalog,
                    digest: s1.clone(),
                },
                Segment {
                    kind: CacheSegment::S2Surface,
                    digest: s2.clone(),
                },
            ],
            surface_invalidation: crate::surface::cache_invalidation_point(&self.surface.lock().await),
        };

        RequestSnapshot {
            prefix_digest: layout.prefix_digest(),
            stable: layout.segments.clone(),
            surface_invalidation: layout.surface_invalidation,
            provider: self.model.to_string(),
            permission_mode: format!("{:?}", self.permission_mode),
            steering_injected: false,
        }
    }

    async fn final_text(&self) -> Option<String> {
        let text = self
            .project_history()
            .await
            .iter()
            .rev()
            .find(|m| m.role == agentrs_types::Role::Assistant)
            .map(|m| {
                m.content
                    .iter()
                    .filter_map(|b| match b {
                        agentrs_types::ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("")
            })?;
        (!text.is_empty()).then_some(text)
    }

    async fn finish(&self, termination: Termination, st: Progress) -> RunSummary {
        let payload = match &termination {
            Termination::Completed => EventPayload::RunCompleted,
            Termination::Canceled => EventPayload::RunCanceled,
            Termination::NeedsUserAction { .. } => EventPayload::RunNeedsUserAction,
            Termination::Failed { .. } => EventPayload::RunFailed,
        };
        let _ = self.emit_fact(payload, Causality::default()).await;

        // 先取文本再结算——shutdown 之后 Surface 仍在，但把顺序写死
        // 可以避免以后有人把 Surface 也纳入结算时出现空结果。
        let final_text = self.final_text().await;

        // 终态后拒绝新输入，并结算全部 live 资源。
        self.inbox.mark_terminal();
        let _ = self.owner.shutdown().await;

        RunSummary {
            termination,
            turns: st.turns,
            steps: st.steps,
            zero_step_turns: st.zero_step_turns,
            final_text,
        }
    }

    /// 持久化失败时的收敛。
    ///
    /// `Fenced` 意味着本 writer 已被更新的 epoch 取代——**必须立即停止全部
    /// durable 写入**并收敛，不能再尝试写终态事件（那同样会被拒绝）。
    async fn abort(&self, err: PersistError, st: Progress) -> RunSummary {
        let code = match err {
            PersistError::Fenced => "fenced",
            PersistError::CheckpointAhead => "checkpoint_ahead",
            PersistError::Backend { .. } => "persist_backend",
        };

        self.inbox.mark_terminal();
        let _ = self.owner.shutdown().await;

        RunSummary {
            termination: Termination::Failed { code: code.into() },
            turns: st.turns,
            steps: st.steps,
            zero_step_turns: st.zero_step_turns,
            // 事实流已经写不进去了。此时报告"助手说过什么"没有意义——
            // 那段文本能否算数取决于它有没有落盘，而这正是失败的原因。
            final_text: None,
        }
    }
}

/// 便捷构造：从时刻构造一个固定时钟（测试与简单宿主用）。
pub struct FixedClock(pub Timestamp);

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use crate::inbox::UserInput;
    use agentrs_contracts::event::EventPayload;
    use agentrs_contracts::ids::Digest;
    use agentrs_types::TokenUsage;

    use super::*;

    /// 按脚本回放模型事件的 driver。
    struct ScriptedDriver {
        script: Mutex<Vec<Vec<LlmEvent>>>,
    }

    impl ScriptedDriver {
        fn new(script: Vec<Vec<LlmEvent>>) -> Arc<Self> {
            Arc::new(Self {
                script: Mutex::new(script),
            })
        }
    }

    #[async_trait::async_trait]
    impl StepDriver for ScriptedDriver {
        async fn call(&self, _req: LlmRequest) -> Result<Vec<LlmEvent>, String> {
            let mut s = self.script.lock().unwrap();
            if s.is_empty() {
                // 脚本耗尽 → 给一个 Final 收尾，避免测试无限循环。
                return Ok(vec![
                    LlmEvent::TextDelta("done".into()),
                    LlmEvent::Done {
                        stop_reason: StopReason::EndTurn,
                        usage: TokenUsage::default(),
                    },
                ]);
            }
            Ok(s.remove(0))
        }
    }

    struct RejectAll;
    impl StepAdmission for RejectAll {
        fn admit(&self, _c: Claim) -> PreStepDecision {
            PreStepDecision::Reject {
                reason: "test".into(),
            }
        }
    }

    fn 文本_final(t: &str) -> Vec<LlmEvent> {
        vec![
            LlmEvent::TextDelta(t.into()),
            LlmEvent::Done {
                stop_reason: StopReason::EndTurn,
                usage: TokenUsage::default(),
            },
        ]
    }

    fn 工具回合() -> Vec<LlmEvent> {
        vec![
            LlmEvent::ToolUse {
                id: "tc1".into(),
                name: "Read".into(),
                input: serde_json::json!({}),
                extra: None,
            },
            LlmEvent::Done {
                stop_reason: StopReason::ToolUse,
                usage: TokenUsage::default(),
            },
        ]
    }

    struct Fixture {
        engine: Engine,
        persistence: Arc<agentrs_testkit::FakePersistence>,
        inbox: Arc<Inbox>,
        cancel: Arc<CancelToken>,
    }

    fn 装配(script: Vec<Vec<LlmEvent>>, admission: Arc<dyn StepAdmission>, guards: TurnGuards) -> Fixture {
        let persistence = Arc::new(agentrs_testkit::FakePersistence::new());
        let inbox = Arc::new(Inbox::new(16));
        let cancel = Arc::new(CancelToken::default());
        let owner = ResourceOwner::new("run");
        let engine = Engine::new(
            "r1".into(),
            RunEpoch(1),
            EngineDeps {
                persistence: persistence.clone(),
                clock: Arc::new(FixedClock(Timestamp(0))),
                driver: ScriptedDriver::new(script),
                admission,
                tools: None,
            },
            inbox.clone(),
            cancel.clone(),
            owner,
            guards,
        );
        Fixture {
            engine,
            persistence,
            inbox,
            cancel,
        }
    }

    fn 事件类型(p: &Arc<agentrs_testkit::FakePersistence>) -> Vec<String> {
        p.events().iter().map(|e| format!("{:?}", e.payload)).collect()
    }

    #[tokio::test]
    async fn 单个文本_final_产生一个_turn_一个_step() {
        let f = 装配(vec![文本_final("hi")], Arc::new(AdmitAll), TurnGuards::default());
        let s = f.engine.run().await;

        assert_eq!(s.termination, Termination::Completed);
        assert_eq!(s.turns, 1);
        assert_eq!(s.steps, 1);
        assert_eq!(s.zero_step_turns, 0);

        let types = 事件类型(&f.persistence);
        assert!(types.contains(&"RunStarted".to_string()));
        assert!(types.contains(&"TurnStarted".to_string()));
        assert!(types.contains(&"StepStarted".to_string()));
        assert!(types.contains(&"RunCompleted".to_string()));
    }

    #[tokio::test]
    async fn 工具回合欠一次请求因此继续下一个_step() {
        let f = 装配(
            vec![工具回合(), 文本_final("done")],
            Arc::new(AdmitAll),
            TurnGuards::default(),
        );
        let s = f.engine.run().await;
        assert_eq!(s.termination, Termination::Completed);
        assert_eq!(s.steps, 2, "ToolRound 之后必须再来一个 Step");
    }

    #[tokio::test]
    async fn 被拒绝的_claim_仍关闭一个零_step_的_turn() {
        // 这是本状态机最容易被漏掉的边界：输入不得在事实流里凭空消失。
        let f = 装配(vec![], Arc::new(RejectAll), TurnGuards::default());
        f.inbox
            .submit(UserInput::Message(vec![agentrs_types::ContentBlock::text("x")]))
            .await
            .unwrap();

        let s = f.engine.run().await;
        assert_eq!(s.zero_step_turns, 1);
        assert_eq!(s.steps, 0, "被拒绝的 claim 不产生模型请求");
        assert!(s.turns >= 1, "但 Turn 必须被打开并关闭");

        let types = 事件类型(&f.persistence);
        assert!(
            types.contains(&"UserInputClaimed".to_string()),
            "认领事实必须留痕：{types:?}"
        );
    }

    #[tokio::test]
    async fn live_文本增量不进入_durable_事实() {
        let f = 装配(
            vec![文本_final("hello")],
            Arc::new(AdmitAll),
            TurnGuards::default(),
        );
        f.engine.run().await;

        let deltas = f
            .persistence
            .events()
            .iter()
            .filter(|e| matches!(e.payload, EventPayload::TextDelta))
            .count();
        // fake 持久化会记录所有 append，但 durability 标记必须是 LiveStream。
        let live = f.persistence.events().iter().filter(|e| !e.is_durable()).count();
        assert_eq!(deltas, live, "TextDelta 必须标记为 live，不参与恢复");
    }

    #[tokio::test]
    async fn 取消在下一个_turn_边界收敛() {
        let f = 装配(
            vec![工具回合(), 工具回合(), 文本_final("x")],
            Arc::new(AdmitAll),
            TurnGuards::default(),
        );
        f.cancel.cancel();
        let s = f.engine.run().await;
        assert_eq!(s.termination, Termination::Canceled);
        assert_eq!(s.steps, 0, "取消后不再发起新请求");
    }

    #[tokio::test]
    async fn 超过_step_上限时停在_needs_user_action() {
        // 确定性防护，不依赖模型自觉。
        let script = vec![工具回合(); 10];
        let f = 装配(
            script,
            Arc::new(AdmitAll),
            TurnGuards {
                max_steps: 3,
                ..Default::default()
            },
        );
        let s = f.engine.run().await;
        assert_eq!(
            s.termination,
            Termination::NeedsUserAction {
                reason: "max_steps".into()
            }
        );
        assert_eq!(s.steps, 3, "不得无限循环");
    }

    #[tokio::test]
    async fn 终态后拒绝新输入并结算资源() {
        let f = 装配(vec![文本_final("ok")], Arc::new(AdmitAll), TurnGuards::default());
        f.engine.run().await;
        assert!(f.inbox.is_terminal());
        assert!(f.inbox.submit(UserInput::Message(vec![])).await.is_err());
    }

    #[tokio::test]
    async fn 被围栏时立即收敛不再尝试写终态() {
        // 旧 writer 复活的场景：epoch 1 的引擎遇到已推进到 epoch 2 的存储。
        let persistence = Arc::new(agentrs_testkit::FakePersistence::new());
        // 先用 epoch 2 占位，使 epoch 1 的写入全部被 Fenced。
        {
            use agentrs_contracts::ports::RunPersistence as _;
            let ev = RunEventEnvelope {
                run_id: "r1".into(),
                epoch: RunEpoch(2),
                event_id: "seed".into(),
                seq: None,
                live_seq: None,
                at: Timestamp(0),
                durability: Durability::DurableFact,
                visibility: Visibility::User,
                causality: Causality::default(),
                surface: None,
                payload: EventPayload::RunStarted,
            };
            persistence.append_event(RunEpoch(2), ev).await.unwrap();
        }

        let inbox = Arc::new(Inbox::new(8));
        let engine = Engine::new(
            "r1".into(),
            RunEpoch(1),
            EngineDeps {
                persistence: persistence.clone(),
                clock: Arc::new(FixedClock(Timestamp(0))),
                driver: ScriptedDriver::new(vec![文本_final("x")]),
                admission: Arc::new(AdmitAll),
                tools: None,
            },
            inbox.clone(),
            Arc::new(CancelToken::default()),
            ResourceOwner::new("run"),
            TurnGuards::default(),
        );

        let s = engine.run().await;
        assert_eq!(
            s.termination,
            Termination::Failed {
                code: "fenced".into()
            }
        );
        assert_eq!(
            persistence.event_count(),
            1,
            "被围栏后不得再写入任何事件，包括终态"
        );
        assert!(inbox.is_terminal(), "仍须收敛：拒绝新输入");
    }

    #[tokio::test]
    async fn 事件_id_确定性派生可作幂等键() {
        let f = 装配(vec![文本_final("x")], Arc::new(AdmitAll), TurnGuards::default());
        f.engine.run().await;
        let ids: Vec<String> = f
            .persistence
            .events()
            .iter()
            .map(|e| e.event_id.to_string())
            .collect();
        assert!(ids.iter().all(|i| i.starts_with("r1-1-")), "{ids:?}");
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len(), "同一 Run 内 event_id 不得重复");
        let _ = Digest::from_hex("x");
    }
}
